//! The owner's calendars, polled (issue #280): the I/O the pure `caldav`
//! module is wrapped in — the three requests to the side service, the
//! cursor per calendar read from and written to the state directory, the
//! consent decision read on the mail connection — kept apart from
//! publishing so the cursor moves only after the bus took the events.
//!
//! No backfill (#251): the first poll of a calendar takes it as it stands
//! and publishes nothing of it; the cursor is filled so that a later change
//! to an existing meeting is a `changed` with its `changed_fields`. After
//! that, a poll whose CTag did not move asks nothing further — and the HAL
//! list carries the CTag, so a calendar that did not move is not even
//! listed.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tracing::{info, warn};
use twalk_consent_cache::{Consent, ConsentCache};

pub use crate::side::SideError;

use crate::caldav::{
    self, calendars_in, collection_path, multiget_body, parse_listing, parse_multiget, Calendar,
    Changes, Cursor, Envelopes, Known, Listing, Resource,
};
use crate::freebusy::{parse_free_busy, Busy, Window};

/// The calendar connection this process holds, and what publishing about it
/// needs.
pub struct Calendars {
    pub connection: String,
    pub owner_email: String,
    /// The mail connection of the same account, whose decisions govern the
    /// participants; `None` on a deployment with no mail connection, where
    /// nobody is ever withheld because nobody was ever decided about.
    pub mail_connection: Option<String>,
    pub side: Side,
    pub state_dir: PathBuf,
    pub consent: ConsentCache,
    /// Whether a published event may carry where the meeting is (#354):
    /// the owner's switch, read from the Companion Gateway and shared with
    /// the run loop that refreshes it. Off until something says otherwise —
    /// a deployment with no Gateway configured has no decision to read, and
    /// no decision is not permission.
    ///
    /// Named for the decision and not for the field: `self.location` would
    /// read as a meeting's own place.
    pub locations_may_travel: SharedSwitch,
}

/// A switch the run loop refreshes and the poll reads. An atomic rather
/// than a lock: one bool, read on every resource of every round, written
/// once a round at most.
pub type SharedSwitch = Arc<AtomicBool>;


/// What one poll found: the envelopes to publish, in order, and the cursors
/// to write once they are on the bus.
#[derive(Debug, Default)]
pub struct Poll {
    pub envelopes: Vec<Value>,
    pub cursors: Vec<(String, Cursor)>,
}

/// The zone the owner's agenda is written in, and where that was read.
///
/// The source travels with the name because "Europe/Paris" alone cannot be
/// checked by the person it is about: a zone the *calendar service* declares
/// is a fact about the owner's own configuration, and one inferred some other
/// way is a guess of a different rank. The owner sees which one answered
/// (#369).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    /// An IANA name — `Europe/Paris`, `America/New_York`.
    pub name: String,
    /// `calendar`: the collection declares it (`CALDAV:calendar-timezone`).
    pub source: &'static str,
}

impl Calendars {

    fn cursor_path(&self, calendar_id: &str) -> PathBuf {
        self.state_dir
            .join("caldav")
            .join(&self.connection)
            .join(format!("{calendar_id}.json"))
    }

    fn read_cursor(&self, calendar_id: &str) -> Result<Cursor> {
        let path = self.cursor_path(calendar_id);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).with_context(|| {
                format!("{} is not a cursor this collector wrote", path.display())
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Cursor::default()),
            Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    /// Writes the cursors a poll handed back, after its envelopes were
    /// published: by rename, so a crash leaves the previous cursor whole and
    /// the next poll republishes what the bus already deduplicates.
    pub fn commit(&self, cursors: &[(String, Cursor)]) -> Result<()> {
        for (calendar_id, cursor) in cursors {
            crate::fs::write_json_private(&self.cursor_path(calendar_id), cursor)?;
        }
        Ok(())
    }

    /// The owner's busy intervals in a window (#281), across the calendars
    /// the poll reads — the HAL list of their own and the shares they
    /// accepted, the same set whose changes are published — one
    /// `free-busy-query` each, merged. No cursor moves and nothing is
    /// published: a read.
    ///
    /// It carries **the zone the agenda is written in** with it (#369). A
    /// free/busy answer is a list of UTC instants and every sentence a human
    /// reads is in local time, so something converts; before this, the thing
    /// converting was a model with nothing to convert *to*, and it guessed.
    /// Measured on 2026-09-24: a draft searched the machine for a timezone,
    /// found none, wrote "(Heures de Paris.)" and happened to be right.
    pub async fn free_busy(
        &self,
        owner_id: &str,
        credential: &crate::side::Credential,
        window: &Window,
    ) -> Result<(Vec<Busy>, Option<Zone>), SideError> {
        let mut answers = Vec::new();
        let mut zone = None;
        for calendar in self.side.calendars(owner_id, credential).await? {
            let collection = collection_path(owner_id, &calendar.id);
            answers.push(self.side.free_busy(&collection, window, credential).await?);
            // The first calendar that declares one wins, and the owner's own
            // comes first in the list the poll reads. A share they accepted
            // may be written in somebody else's zone, and that is not where
            // the owner is.
            if zone.is_none() {
                zone = self
                    .side
                    .calendar_timezone(&collection, credential)
                    .await?
                    .map(|name| Zone {
                        name,
                        source: "calendar",
                    });
            }
        }
        // And when no collection declares one — which is the reference
        // deployment's case, measured on 2026-09-26 — the zone the owner's
        // own events are written in. A weaker source than a declaration, so
        // it is asked second and says which it is.
        let zone = match zone {
            Some(zone) => Some(zone),
            None => self.zone_of_their_events(owner_id, credential).await?,
        };
        Ok((crate::freebusy::merge_answers(answers, window), zone))
    }

    /// The zone most of the owner's events are written in (#369), read from
    /// the cursors this collector already wrote.
    ///
    /// The cursor keeps every event of the window **as published**, and a
    /// published event carries its `timezone` — so the zone their agenda is
    /// written in is already on this deployment's own disk, and nothing new
    /// has to be remembered for it. The first version of this counted zones
    /// as the poll read events, which was wrong in a way only a restart
    /// showed: a round reads the resources whose etag moved, so after a
    /// restart the tally stayed empty until somebody edited a meeting.
    ///
    /// The mode, not the latest: one invitation authored in Tokyo must not
    /// move the owner to Tokyo. Ties go to the name, so a calendar genuinely
    /// split between two zones answers the same thing on every round — an
    /// unstable answer here would make every draft's hours unstable, which
    /// is worse than either zone.
    async fn zone_of_their_events(
        &self,
        owner_id: &str,
        credential: &crate::side::Credential,
    ) -> Result<Option<Zone>, SideError> {
        let mut seen: BTreeMap<String, u64> = BTreeMap::new();
        for calendar in self.side.calendars(owner_id, credential).await? {
            let Ok(cursor) = self.read_cursor(&calendar.id) else {
                continue;
            };
            for known in cursor.known.values() {
                if let Some(zone) = known.published.get("timezone").and_then(Value::as_str) {
                    *seen.entry(zone.to_owned()).or_insert(0) += 1;
                }
            }
        }
        let Some((name, count)) = modal_zone(&seen) else {
            return Ok(None);
        };
        info!(
            zone = name,
            events = count,
            zones = seen.len(),
            "no calendar declares a zone; the one the owner's events are written in is the answer"
        );
        Ok(Some(Zone {
            name: name.clone(),
            source: "events",
        }))
    }

    /// What one of the owner's events carries, without its words (#355):
    /// the conference link when a property declares one, how long the
    /// description is, how many attachments there are.
    ///
    /// The event is named by the `uid` a persona already has from
    /// `calendar.event.*`, and found through the cursors this collector
    /// wrote — so an event it never published is an event it will not
    /// answer about, which is the same perimeter the published events had.
    /// The resource is read again rather than remembered: the cursor keeps
    /// the event **as published**, which is the reduced one, and a copy of
    /// the description kept on disk to answer questions about it would be
    /// the thing this whole ticket refuses.
    pub async fn facts_about(
        &self,
        uid: &str,
        owner_id: &str,
        credential: &crate::side::Credential,
    ) -> Result<Option<caldav::Facts>, SideError> {
        for calendar in self.side.calendars(owner_id, credential).await? {
            let cursor = match self.read_cursor(&calendar.id) {
                Ok(cursor) => cursor,
                Err(error) => {
                    warn!(calendar = %calendar.id, error = %format!("{error:#}"), "a cursor could not be read while answering about an event");
                    continue;
                }
            };
            let Some(href) = cursor.known.iter().find_map(|(href, known)| {
                (known.published.get("uid").and_then(Value::as_str) == Some(uid))
                    .then(|| href.clone())
            }) else {
                continue;
            };
            // The file name is the href's last segment; the collection is
            // this collector's own, built the way every other request builds
            // it. See [`Side::resource`] for why not the href itself.
            let Some(file_name) = href.rsplit('/').find(|part| !part.is_empty()) else {
                warn!(href = %href, "a cursor holds a resource with no file name; no facts answered");
                continue;
            };
            let collection = collection_path(owner_id, &calendar.id);
            let ics = self
                .side
                .resource(&collection, file_name, credential)
                .await?;
            return match caldav::facts_of(&ics) {
                Ok(facts) => Ok(Some(facts)),
                Err(error) => {
                    warn!(href = %href, error = %format!("{error:#}"), "the resource is not a VEVENT this collector reads; no facts answered");
                    Ok(None)
                }
            };
        }
        Ok(None)
    }

    /// The decision about a `mailto:` on the mail connection, or `Pending`
    /// — labelled by nothing, reduced by nothing — when there is none.
    fn decide(&self, identity: &str) -> Consent {
        match &self.mail_connection {
            Some(mail) => self.consent.state(identity, mail),
            None => Consent::Pending,
        }
    }

    /// One poll: every calendar of the owner's, its CTag against the cursor,
    /// and for the ones that moved the listing, the diff and the reads.
    pub async fn poll(
        &self,
        owner_id: &str,
        credential: &crate::side::Credential,
        window: &crate::freebusy::Window,
        now: &str,
    ) -> Result<Poll, SideError> {
        let mut poll = Poll::default();
        for calendar in self.side.calendars(owner_id, credential).await? {
            let cursor = match self.read_cursor(&calendar.id) {
                Ok(cursor) => cursor,
                Err(error) => {
                    // A cursor this collector cannot read is not one to
                    // guess past: said, and the calendar skipped this poll.
                    warn!(calendar = %calendar.id, error = %format!("{error:#}"), "the calendar's cursor cannot be read; skipping it");
                    continue;
                }
            };
            if calendar.ctag.is_some() && calendar.ctag == cursor.ctag {
                continue;
            }
            let collection = caldav::collection_path(owner_id, &calendar.id);
            let mut listing = self.side.listing(&collection, window, credential).await?;
            // A `calendar-query` answers about the events, not about the
            // collection, so it carries no CTag: the one the HAL list gave
            // is what the next cursor remembers (#348).
            listing.ctag = calendar.ctag.clone().or(listing.ctag);
            if listing.ctag.is_some() && listing.ctag == cursor.ctag {
                continue;
            }
            let changes = caldav::diff(&cursor, &listing);
            let resources = self
                .side
                .read(&collection, &changes.to_read(), credential)
                .await?;
            let first_poll = cursor.ctag.is_none() && cursor.known.is_empty();
            let (envelopes, next) = self.reconcile(
                &calendar,
                &collection,
                &cursor,
                &listing,
                &changes,
                &resources,
                now,
            );
            if first_poll {
                // No backfill: what the calendar already holds is taken as
                // the state, published as nothing.
                info!(calendar = %calendar.id, resources = next.known.len(), "calendar taken as it stands; nothing of it published");
            } else {
                poll.envelopes.extend(envelopes);
            }
            poll.cursors.push((calendar.id.clone(), next));
        }
        Ok(poll)
    }

    /// The pure step of one calendar's poll: what the listing and the reads
    /// say, as envelopes and as the cursor that follows.
    #[allow(clippy::too_many_arguments)]
    fn reconcile(
        &self,
        calendar: &Calendar,
        collection: &str,
        cursor: &Cursor,
        listing: &Listing,
        changes: &Changes,
        resources: &[Resource],
        now: &str,
    ) -> (Vec<Value>, Cursor) {
        let envelopes = Envelopes::new(
            &self.connection,
            &self.owner_email,
            &self.side.host(),
            collection,
        );
        let by_href: BTreeMap<&str, &Resource> = resources
            .iter()
            .map(|resource| (resource.href.as_str(), resource))
            .collect();
        let mut out = Vec::new();
        let mut next = Cursor {
            ctag: listing.ctag.clone(),
            known: cursor.known.clone(),
        };
        for href in changes.created.iter().chain(&changes.changed) {
            // Gone between the listing and the read: the next listing
            // reports the removal.
            let Some(resource) = by_href.get(href.as_str()) else {
                continue;
            };
            let before = cursor
                .known
                .get(href)
                .map(|known| &known.published)
                .filter(|published| !published.is_null());
            let published = self.publishable(resource);
            match (&published, before) {
                (Some(published), Some(before)) => {
                    let fields = caldav::changed_fields(before, published);
                    if fields.is_empty() {
                        info!(calendar = %calendar.id, href, "the resource changed in nothing the contract publishes");
                    } else {
                        out.push(envelopes.changed(href, &resource.etag, published, &fields, now));
                    }
                }
                (Some(published), None) => {
                    out.push(envelopes.created(href, &resource.etag, published, now));
                }
                (None, _) => {}
            }
            // Remembered either way — an unreadable resource as `null`, so
            // it is not re-read at every poll and is new when it becomes
            // readable.
            next.known.insert(
                href.clone(),
                Known {
                    etag: resource.etag.clone(),
                    published: published.unwrap_or(Value::Null),
                },
            );
        }
        for href in &changes.removed {
            if let Some(known) = cursor.known.get(href) {
                if let (Some(uid), Some(title)) = (
                    known.published.get("uid").and_then(Value::as_str),
                    known.published.get("title").and_then(Value::as_str),
                ) {
                    out.push(envelopes.removed(
                        href,
                        listing.ctag.as_deref().unwrap_or_default(),
                        uid,
                        title,
                        now,
                    ));
                }
            }
            next.known.remove(href);
        }
        (out, next)
    }

    /// The resource as the contract publishes it, or `None` — said — for
    /// one that is not a VEVENT this collector can read.
    fn publishable(&self, resource: &Resource) -> Option<Value> {
        match caldav::parse_vevent(&resource.ics) {
            Ok(event) => Some(caldav::reduce(
                &event,
                &self.owner_email,
                if self.locations_may_travel.load(Ordering::Relaxed) {
                    caldav::Location::Carried
                } else {
                    caldav::Location::Withheld
                },
                |identity| self.decide(identity),
            )),
            Err(error) => {
                warn!(href = %resource.href, error = %format!("{error:#}"), "the resource is not a VEVENT this collector reads; nothing published");
                None
            }
        }
    }
}

/// The side service, over HTTP: three requests and nothing else.
#[derive(Debug, Clone)]
pub struct Side {
    http: reqwest::Client,
    /// The root, without a trailing slash.
    base: String,
}

impl Side {
    pub fn new(caldav_url: &str) -> Result<Self> {
        Ok(Self {
            http: crate::side::client()?,
            base: caldav_url.trim_end_matches('/').to_owned(),
        })
    }

    /// The side service's host, for `source`.
    pub fn host(&self) -> String {
        crate::side::host_of(&self.base)
    }

    pub async fn calendars(
        &self,
        owner_id: &str,
        credential: &crate::side::Credential,
    ) -> Result<Vec<Calendar>, SideError> {
        let url = format!(
            "{}/dav/calendars/{owner_id}.json?personal=true&sharedDelegationStatus=accepted",
            self.base
        );
        let body = self
            .send(
                self.http.get(&url).header("accept", "application/json"),
                credential,
            )
            .await?;
        let document: Value =
            serde_json::from_str(&body).map_err(|error| SideError::Unreachable {
                detail: format!("the calendar list at {url} is not JSON: {error}"),
            })?;
        Ok(calendars_in(&document))
    }

    /// The etags of the events of a collection **inside a window** (#348).
    ///
    /// `PROPFIND Depth: 1` was the obvious question and the wrong one: it
    /// asks for every resource a calendar has ever held, which on a real
    /// one does not answer inside the client's timeout. A
    /// `REPORT calendar-query` bounded by a time range answers in the same
    /// second on the same server — which is the difference between a
    /// history and what is happening around now.
    /// One resource, as iCalendar text: a plain `GET` (RFC 4791 — a
    /// calendar object resource is an ordinary HTTP resource). Used to
    /// answer facts about one event (#355), where a windowed `REPORT`
    /// would fetch a calendar to read one meeting.
    ///
    /// Addressed by its **collection and its file name**, not by the href
    /// the listing gave. The reference deployment answers hrefs relative to
    /// sabre's own root — `/calendars/<owner>/<calendar>/<uid>.ics` — while
    /// the collection this collector asks about is `/dav/calendars/…`,
    /// because the ESN relays `/dav/` to a sabre mounted at `/`. Pasting
    /// the href onto the base gave a 404 on the live service and passed
    /// against a fake whose hrefs happened to match, which is what running
    /// it in production found. Building the URL the way every other request
    /// here does is what keeps the two from drifting again.
    pub async fn resource(
        &self,
        collection: &str,
        file_name: &str,
        credential: &crate::side::Credential,
    ) -> Result<String, SideError> {
        let url = format!(
            "{}{}/{file_name}",
            self.base,
            collection.trim_end_matches('/')
        );
        let response = crate::side::send(self.http.get(&url), credential, "caldav").await?;
        response
            .text()
            .await
            .map_err(|error| SideError::Unreachable {
                detail: format!(
                    "caldav gave an unreadable resource: {}",
                    crate::side::because(&error)
                ),
            })
    }

    pub async fn listing(
        &self,
        collection: &str,
        window: &crate::freebusy::Window,
        credential: &crate::side::Credential,
    ) -> Result<Listing, SideError> {
        let method = reqwest::Method::from_bytes(b"REPORT").expect("a method name");
        let body = self
            .send(
                self.http
                    .request(method, format!("{}{collection}", self.base))
                    .header("depth", "1")
                    .header("content-type", "application/xml; charset=utf-8")
                    .body(caldav::calendar_query_body(
                        &window.from.to_rfc3339(),
                        &window.to.to_rfc3339(),
                    )),
                credential,
            )
            .await?;
        parse_listing(&body, collection).map_err(|error| SideError::Unreachable {
            detail: format!("{error:#}"),
        })
    }

    pub async fn read(
        &self,
        collection: &str,
        hrefs: &[String],
        credential: &crate::side::Credential,
    ) -> Result<Vec<Resource>, SideError> {
        if hrefs.is_empty() {
            return Ok(Vec::new());
        }
        let method = reqwest::Method::from_bytes(b"REPORT").expect("a method name");
        let body = self
            .send(
                self.http
                    .request(method, format!("{}{collection}", self.base))
                    .header("depth", "1")
                    .header("content-type", "application/xml; charset=utf-8")
                    .body(multiget_body(hrefs)),
                credential,
            )
            .await?;
        parse_multiget(&body).map_err(|error| SideError::Unreachable {
            detail: format!("{error:#}"),
        })
    }

    /// `REPORT free-busy-query` on one collection (#281, RFC 4791 §7.10):
    /// the busy periods in the window, read as intervals and nothing else.
    pub async fn free_busy(
        &self,
        collection: &str,
        window: &Window,
        credential: &crate::side::Credential,
    ) -> Result<Vec<Busy>, SideError> {
        let method = reqwest::Method::from_bytes(b"REPORT").expect("a method name");
        let body = self
            .send(
                self.http
                    .request(method, format!("{}{collection}", self.base))
                    .header("depth", "0")
                    .header("content-type", "application/xml; charset=utf-8")
                    .body(window.report_body()),
                credential,
            )
            .await?;
        parse_free_busy(&body, window).map_err(|error| SideError::Unreachable {
            detail: format!("the free-busy answer for {collection} could not be read: {error:#}"),
        })
    }

    /// The zone a collection declares (`CALDAV:calendar-timezone`,
    /// RFC 4791 §5.2.2), or `None` when it declares none (#369).
    ///
    /// The property holds a whole `VTIMEZONE` — rules, offsets, the lot — of
    /// which the only part anybody here needs is the `TZID` inside it. A
    /// service that has never been told a zone answers `404 Not Found` for
    /// the property inside a `207`, which is not an error and must not read
    /// as one: a calendar with no declared zone is the ordinary case on a
    /// server whose clients never set it, and the answer to it is to say so
    /// rather than to invent a zone or to fail the read.
    pub async fn calendar_timezone(
        &self,
        collection: &str,
        credential: &crate::side::Credential,
    ) -> Result<Option<String>, SideError> {
        let method = reqwest::Method::from_bytes(b"PROPFIND").expect("a method name");
        let body = self
            .send(
                self.http
                    .request(method, format!("{}{collection}", self.base))
                    .header("depth", "0")
                    .header("content-type", "application/xml; charset=utf-8")
                    .body(
                        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
                         <d:propfind xmlns:d=\"DAV:\" xmlns:cal=\"urn:ietf:params:xml:ns:caldav\">\n\
                         \u{20}\u{20}<d:prop><cal:calendar-timezone/></d:prop>\n\
                         </d:propfind>\n",
                    ),
                credential,
            )
            .await?;
        Ok(tzid_in(&body))
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        credential: &crate::side::Credential,
    ) -> Result<String, SideError> {
        crate::side::send(request, credential, "caldav")
            .await?
            .text()
            .await
            .map_err(|error| SideError::Unreachable {
                detail: format!("caldav's answer could not be read: {error}"),
            })
    }
}

/// The zone a tally was seen on most, and how often.
///
/// A free function so that the test exercises the selection itself rather
/// than a copy of it: a test that recomputes the answer the way the code does
/// passes by construction and can never disagree with it.
///
/// Ties go to the name, so a calendar genuinely split between two zones
/// answers the same thing on every round. An unstable answer here would make
/// every draft's hours unstable, which is worse than either zone.
fn modal_zone(seen: &BTreeMap<String, u64>) -> Option<(&String, &u64)> {
    seen.iter()
        .max_by(|left, right| left.1.cmp(right.1).then(right.0.cmp(left.0)))
}

/// The `TZID` of the first `VTIMEZONE` in a `calendar-timezone` answer.
///
/// Read out of the XML as text rather than parsed as iCalendar, because the
/// property's whole value is a calendar object whose only interesting line is
/// this one, and because it arrives escaped in several shapes: a real
/// newline, `&#13;`, `&#xD;`, or the line folding iCalendar allows. What is
/// wanted is one name, and a name has no spaces.
fn tzid_in(body: &str) -> Option<String> {
    let start = body.find("TZID:")? + "TZID:".len();
    let rest = &body[start..];
    let end = rest
        .find(|c: char| c == '\r' || c == '\n' || c == '<' || c == '&')
        .unwrap_or(rest.len());
    let name = rest[..end].trim();
    // A zone is an IANA name, and this is the whole check: anything with a
    // space or a slash-less shape is something else that happened to follow
    // the letters TZID — say nothing rather than hand a model a word.
    (!name.is_empty() && !name.contains(' ') && name.len() < 64).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::tzid_in;
    use std::collections::BTreeMap;

    fn seen(counts: &[(&str, u64)]) -> BTreeMap<String, u64> {
        counts
            .iter()
            .map(|(zone, count)| ((*zone).to_owned(), *count))
            .collect()
    }

    /// The mode, and the same mode every time (#369).
    #[test]
    fn the_zone_of_their_events_is_the_one_most_of_them_are_written_in() {
        // One invitation authored in Tokyo must not move the owner to Tokyo.
        let tally = seen(&[("Europe/Paris", 194), ("Asia/Tokyo", 1)]);
        assert_eq!(pick(&tally).as_deref(), Some("Europe/Paris"));

        // A tie answers the same thing on every round: an unstable answer
        // here would make every draft's hours unstable, which is worse than
        // either zone.
        let split = seen(&[("Europe/Paris", 7), ("America/New_York", 7)]);
        let first = pick(&split);
        assert!(first.is_some());
        for _ in 0..5 {
            assert_eq!(pick(&split), first);
        }

        // And nothing at all before a round has read anything.
        assert_eq!(pick(&seen(&[])), None);
    }

    /// The selection itself, on the tally it takes — not a copy of it.
    fn pick(tally: &BTreeMap<String, u64>) -> Option<String> {
        super::modal_zone(tally).map(|(name, _)| name.clone())
    }

    #[test]
    fn a_zone_is_read_out_of_the_property_however_it_is_escaped() {
        // sabre answers the whole VTIMEZONE, XML-escaped, with its line
        // breaks in whichever shape the server chose.
        assert_eq!(
            tzid_in("<cal:calendar-timezone>BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Paris\r\nEND:VTIMEZONE\r\n</cal:calendar-timezone>").as_deref(),
            Some("Europe/Paris")
        );
        assert_eq!(
            tzid_in("BEGIN:VTIMEZONE&#13;\nTZID:America/New_York&#13;\nEND:VTIMEZONE").as_deref(),
            Some("America/New_York")
        );
        assert_eq!(tzid_in("TZID:Asia/Tokyo</x>").as_deref(), Some("Asia/Tokyo"));

        // A collection that declares none answers the property `404` inside
        // the `207`, and there is no TZID anywhere in it.
        assert_eq!(tzid_in("<d:prop><cal:calendar-timezone/></d:prop>"), None);

        // And a word that follows the letters TZID without being a zone is
        // not handed to a model to put in a sentence: Windows spells its
        // zones with spaces, and no converter this side knows them.
        assert_eq!(tzid_in("TZID: Romance Standard Time\r\n"), None);
        assert_eq!(tzid_in("TZID:\r\n"), None);
    }
}
