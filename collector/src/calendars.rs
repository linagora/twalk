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
    pub async fn free_busy(
        &self,
        owner_id: &str,
        credential: &crate::side::Credential,
        window: &Window,
    ) -> Result<Vec<Busy>, SideError> {
        let mut answers = Vec::new();
        for calendar in self.side.calendars(owner_id, credential).await? {
            let collection = collection_path(owner_id, &calendar.id);
            answers.push(self.side.free_busy(&collection, window, credential).await?);
        }
        Ok(crate::freebusy::merge_answers(answers, window))
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
