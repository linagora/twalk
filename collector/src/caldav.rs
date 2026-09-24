//! The owner's calendars, read through the side service (issue #280, ADR
//! 0033): the pure half — what a listing says, what changed since the last
//! one, what a VEVENT is worth publishing, who in it may be named — and the
//! three envelopes. No I/O: the three requests live in `calendars.rs`,
//! each answered by one of the parsers here.
//!
//! The cursor is the **CTag per calendar** and, under it, the ETag of every
//! resource the collector has published (`Cursor`). A poll whose CTag did
//! not move reads nothing; one whose CTag moved lists the collection
//! (`PROPFIND`, depth 1), diffs the ETags against the cursor, and reads
//! only what is new or changed (`REPORT calendar-multiget`). No
//! `sync-collection`: twaky measured that the proxy does not serve it, and
//! the CTag is what every CalDAV server has. No backfill (#251): a cursor
//! that starts empty takes the calendar as it stands and publishes nothing
//! of it — what the owner's agenda already holds is the past, and only what
//! changes from then on is an event; the first poll's reads fill the cursor
//! so that a later change to an existing meeting is a `changed` with its
//! `changed_fields`, not a `created` out of nowhere.
//!
//! What is published is the event as the owner's agenda shows it
//! (`definitions/calendar-event.schema.json`): never the description, never
//! an attachment. The participants are third-party data inside an event
//! about the owner, and [`reduce`] withholds each one whose consent on the
//! **mail connection of the same account** is `revoked` — the same
//! `mailto:` string, carried by the same grant, so this is structural and
//! not an inferred merge. The owner is never withheld: they have no consent
//! state (ADR 0021).

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, NaiveDate, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use twalk_consent_cache::Consent;

use crate::status::sha256_hex;

pub const CREATED_TYPE: &str = "fr.linagora.twalk.calendar.event.created.v1";
pub const CHANGED_TYPE: &str = "fr.linagora.twalk.calendar.event.changed.v1";
pub const REMOVED_TYPE: &str = "fr.linagora.twalk.calendar.event.removed.v1";

const SCHEMA_BASE: &str = "https://schemas.twalk.dev/cloudevents/v1/";

/// One of the owner's calendars on the side service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    /// The calendar's id: the last segment of its HAL self link.
    pub id: String,
    pub name: String,
    /// The CTag the list itself carried (`calendarserver:ctag`), when it
    /// did: a calendar whose CTag the cursor already holds is not listed.
    pub ctag: Option<String>,
}

/// The owner's calendars, off the HAL document
/// `GET /dav/calendars/<owner id>.json?personal=true&sharedDelegationStatus=accepted`
/// (`_embedded["dav:calendar"][]`, each with `_links.self.href` ending in
/// `<id>.json`). A calendar whose link cannot be read is skipped, since
/// nothing could be asked of it anyway.
pub fn calendars_in(document: &Value) -> Vec<Calendar> {
    document
        .pointer("/_embedded/dav:calendar")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let href = entry.pointer("/_links/self/href")?.as_str()?;
                    let id = href
                        .trim_end_matches('/')
                        .rsplit('/')
                        .next()?
                        .strip_suffix(".json")?
                        .to_owned();
                    let name = entry
                        .get("dav:name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let ctag = entry
                        .get("calendarserver:ctag")
                        .and_then(Value::as_str)
                        .filter(|ctag| !ctag.is_empty())
                        .map(str::to_owned);
                    Some(Calendar { id, name, ctag })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The path of a calendar collection on the side service.
pub fn collection_path(owner_id: &str, calendar_id: &str) -> String {
    format!("/dav/calendars/{owner_id}/{calendar_id}/")
}

/// The `PROPFIND` body: the collection's CTag, every resource's ETag.
/// The `REPORT calendar-query` body listing the etags of the events in a
/// window (RFC 4791 §7.8).
///
/// This replaces the `PROPFIND Depth: 1` this collector used to send
/// (#348). That question — *every resource of this collection and its
/// etag* — is unbounded: on the reference deployment's real calendar it
/// did not answer inside thirty seconds, while a bounded question on the
/// same server, on the same credential, answered in the same second. A
/// calendar is a history; what a persona needs is what is happening
/// around now.
///
/// The window is the collector's perimeter, and it has a consequence
/// worth stating rather than discovering: an event **moved out** of the
/// window reads as removed, because from inside the perimeter it is gone.
pub fn calendar_query_body(from: &str, to: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><d:getetag/></d:prop>
  <c:filter>
    <c:comp-filter name="VCALENDAR">
      <c:comp-filter name="VEVENT">
        <c:time-range start="{}" end="{}"/>
      </c:comp-filter>
    </c:comp-filter>
  </c:filter>
</c:calendar-query>
"#,
        caldav_instant(from),
        caldav_instant(to)
    )
}

/// An RFC 3339 instant as CalDAV writes one: `20260924T050000Z`.
fn caldav_instant(rfc3339: &str) -> String {
    // The fraction goes first: filtering digits out of `…:00.123Z` would
    // otherwise fold the milliseconds into the seconds.
    let without_fraction = rfc3339.split('.').next().unwrap_or_default();
    let digits: String = without_fraction
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == 'T')
        .collect();
    format!("{digits}Z")
}

/// The `REPORT calendar-multiget` body for the resources named.
pub fn multiget_body(hrefs: &[String]) -> String {
    let mut body = String::from(
        r#"<?xml version="1.0" encoding="utf-8"?>
<c:calendar-multiget xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><d:getetag/><c:calendar-data/></d:prop>
"#,
    );
    for href in hrefs {
        body.push_str("  <d:href>");
        body.push_str(&xml_escape(href));
        body.push_str("</d:href>\n");
    }
    body.push_str("</c:calendar-multiget>\n");
    body
}

/// What a `PROPFIND` on a collection says: its CTag and each resource's
/// ETag, by href.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    pub ctag: Option<String>,
    pub etags: BTreeMap<String, String>,
}

/// Reads a multistatus: the response whose href is the collection itself
/// carries the CTag; every other one carries a resource's ETag. A response
/// with no ETag (a sub-collection, a 404 propstat) is not a resource.
pub fn parse_listing(xml: &str, collection: &str) -> Result<Listing> {
    let document = roxmltree::Document::parse(xml).context("the PROPFIND answer is not XML")?;
    let mut listing = Listing::default();
    for response in document
        .descendants()
        .filter(|node| node.has_tag_name((DAV, "response")))
    {
        let Some(href) = text_of(&response, DAV, "href") else {
            continue;
        };
        let ctag = text_of(&response, CALENDARSERVER, "getctag");
        let etag = text_of(&response, DAV, "getetag");
        if href.trim_end_matches('/') == collection.trim_end_matches('/') {
            listing.ctag = ctag;
        } else if let Some(etag) = etag {
            listing.etags.insert(href, etag);
        }
    }
    Ok(listing)
}

/// One resource a `calendar-multiget` answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub href: String,
    pub etag: String,
    pub ics: String,
}

/// Reads a multiget's multistatus: every response with calendar data. One
/// answered 404 (deleted between the listing and the read) is left out, and
/// the next poll's listing reports the removal.
pub fn parse_multiget(xml: &str) -> Result<Vec<Resource>> {
    let document = roxmltree::Document::parse(xml).context("the REPORT answer is not XML")?;
    let mut resources = Vec::new();
    for response in document
        .descendants()
        .filter(|node| node.has_tag_name((DAV, "response")))
    {
        let (Some(href), Some(etag), Some(ics)) = (
            text_of(&response, DAV, "href"),
            text_of(&response, DAV, "getetag"),
            text_of(&response, CALDAV, "calendar-data"),
        ) else {
            continue;
        };
        resources.push(Resource { href, etag, ics });
    }
    Ok(resources)
}

const DAV: &str = "DAV:";
const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
const CALENDARSERVER: &str = "http://calendarserver.org/ns/";

fn text_of(node: &roxmltree::Node, namespace: &str, name: &str) -> Option<String> {
    node.descendants()
        .find(|child| child.has_tag_name((namespace, name)))
        .and_then(|child| child.text())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A person on an event: `mailto:` and the CN, when the source gave one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub identity: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
    pub identity: String,
    pub name: Option<String>,
    pub role: String,
    pub partstat: String,
}

/// A VEVENT, reduced to what the contract publishes — before the consent
/// rule is applied to the people in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vevent {
    pub uid: String,
    pub title: String,
    /// The LOCATION, as somebody typed it, or `None` when the event has
    /// none. Read here and published only when the owner's switch is open
    /// (#354): parsing it is not the decision, [`reduce`] is.
    pub location: Option<String>,
    pub start: String,
    pub end: String,
    pub all_day: bool,
    pub timezone: Option<String>,
    pub status: String,
    pub recurrence: Option<String>,
    pub organizer: Option<Person>,
    pub participants: Vec<Participant>,
}

/// Reads the master VEVENT of an iCalendar text (RFC 5545): the one without
/// a RECURRENCE-ID, or the first one when every VEVENT is an exception.
///
/// The LOCATION is read, because the owner may decide it travels (#354).
/// The DESCRIPTION, the alarms and the attachments are **not read at all**,
/// which is stronger than not publishing them: what a process never holds
/// it cannot leak by a later mistake (ADR 0012, ADR 0028).
pub fn parse_vevent(ics: &str) -> Result<Vevent> {
    let lines = unfold(ics);
    let mut events: Vec<Vec<ContentLine>> = Vec::new();
    let mut current: Option<Vec<ContentLine>> = None;
    for line in lines {
        let Some(parsed) = ContentLine::parse(&line) else {
            continue;
        };
        match (parsed.name.as_str(), parsed.value.as_str()) {
            ("BEGIN", "VEVENT") => current = Some(Vec::new()),
            ("END", "VEVENT") => {
                if let Some(event) = current.take() {
                    events.push(event);
                }
            }
            _ => {
                if let Some(event) = current.as_mut() {
                    event.push(parsed);
                }
            }
        }
    }
    let master = events
        .iter()
        .find(|event| !event.iter().any(|line| line.name == "RECURRENCE-ID"))
        .or_else(|| events.first())
        .context("the resource holds no VEVENT")?;
    let property = |name: &str| master.iter().find(|line| line.name == name);
    let uid = property("UID")
        .map(|line| line.value.clone())
        .filter(|uid| !uid.is_empty())
        .context("the VEVENT has no UID")?;
    let title = property("SUMMARY")
        .map(|line| unescape_text(&line.value))
        .unwrap_or_default();
    let location = property("LOCATION")
        .map(|line| unescape_text(&line.value))
        .map(|location| location.trim().to_owned())
        .filter(|location| !location.is_empty());
    let dtstart = property("DTSTART").context("the VEVENT has no DTSTART")?;
    let all_day = dtstart.is_date();
    let timezone = dtstart.param("TZID").filter(|_| !all_day);
    let start = Moment::parse(&dtstart.value, timezone.as_deref(), all_day)
        .with_context(|| format!("DTSTART {:?} cannot be read", dtstart.value))?;
    let end = match (property("DTEND"), property("DURATION")) {
        (Some(dtend), _) => Moment::parse(
            &dtend.value,
            dtend.param("TZID").or_else(|| timezone.clone()).as_deref(),
            dtend.is_date(),
        )
        .with_context(|| format!("DTEND {:?} cannot be read", dtend.value))?,
        (None, Some(duration)) => start.plus(
            parse_duration(&duration.value)
                .with_context(|| format!("DURATION {:?} cannot be read", duration.value))?,
        ),
        (None, None) if all_day => start.plus(ChronoDuration::days(1)),
        (None, None) => start.clone(),
    };
    let status = match property("STATUS")
        .map(|line| line.value.to_ascii_uppercase())
        .as_deref()
    {
        Some("TENTATIVE") => "tentative",
        Some("CANCELLED") => "cancelled",
        _ => "confirmed",
    }
    .to_owned();
    let recurrence = property("RRULE")
        .map(|line| line.value.clone())
        .filter(|rule| !rule.is_empty());
    let organizer = property("ORGANIZER").and_then(|line| {
        Some(Person {
            identity: mailto_of(&line.value)?,
            name: line.param("CN").map(|name| unescape_text(&name)),
        })
    });
    let participants = master
        .iter()
        .filter(|line| line.name == "ATTENDEE")
        .filter_map(|line| {
            // An attendee that is not a person by email (a room, a
            // `urn:uuid:` resource) has no `mailto:` and is not a participant
            // the contract can name; it is not a head to count either.
            let identity = mailto_of(&line.value)?;
            Some(Participant {
                identity,
                name: line.param("CN").map(|name| unescape_text(&name)),
                role: match line
                    .param("ROLE")
                    .map(|role| role.to_ascii_uppercase())
                    .as_deref()
                {
                    Some("CHAIR") => "chair",
                    Some("OPT-PARTICIPANT") => "optional",
                    Some("NON-PARTICIPANT") => "non-participant",
                    _ => "required",
                }
                .to_owned(),
                partstat: match line
                    .param("PARTSTAT")
                    .map(|status| status.to_ascii_uppercase())
                    .as_deref()
                {
                    Some("ACCEPTED") => "accepted",
                    Some("DECLINED") => "declined",
                    Some("TENTATIVE") => "tentative",
                    Some("DELEGATED") => "delegated",
                    _ => "needs-action",
                }
                .to_owned(),
            })
        })
        .collect();
    Ok(Vevent {
        uid,
        title,
        location,
        start: start.render(),
        end: end.render(),
        all_day,
        timezone,
        status,
        recurrence,
        organizer,
        participants,
    })
}

pub use crate::side::owner_mailto;

/// A `mailto:` value, lower-cased — one string for one person across the
/// mail and calendar connections — or `None` for any other URI.
fn mailto_of(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.len() > 7 && trimmed[..7].eq_ignore_ascii_case("mailto:") {
        let address = trimmed[7..].trim();
        if address.contains('@') {
            return Some(format!("mailto:{}", address.to_ascii_lowercase()));
        }
    }
    None
}

/// RFC 5545 §3.3.1: joins a folded line onto the one before it.
pub(crate) fn unfold(ics: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in ics.split('\n') {
        let raw = raw.trim_end_matches('\r');
        if let Some(rest) = raw.strip_prefix(' ').or_else(|| raw.strip_prefix('\t')) {
            if let Some(last) = lines.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        lines.push(raw.to_owned());
    }
    lines
}

/// RFC 5545 §3.3.11: `\,` `\;` `\n` `\\` back to their characters.
fn unescape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// One content line: `NAME;PARAM=value;PARAM="quoted":value`.
struct ContentLine {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl ContentLine {
    fn parse(line: &str) -> Option<Self> {
        // The first ':' outside quotes separates the name and parameters
        // from the value.
        let mut in_quotes = false;
        let mut split = None;
        for (index, c) in line.char_indices() {
            match c {
                '"' => in_quotes = !in_quotes,
                ':' if !in_quotes => {
                    split = Some(index);
                    break;
                }
                _ => {}
            }
        }
        let (head, value) = line.split_at(split?);
        let value = value[1..].to_owned();
        let mut parts = split_unquoted(head, ';').into_iter();
        let name = parts.next()?.to_ascii_uppercase();
        let params = parts
            .filter_map(|param| {
                let (name, value) = param.split_once('=')?;
                Some((
                    name.to_ascii_uppercase(),
                    value.trim_matches('"').to_owned(),
                ))
            })
            .collect();
        Some(Self {
            name,
            params,
            value,
        })
    }

    /// A DATE rather than a DATE-TIME: said by `VALUE=DATE`, or by the
    /// eight-digit shape a server writes it in without saying.
    fn is_date(&self) -> bool {
        self.param("VALUE").as_deref() == Some("DATE") || self.value.len() == 8
    }

    fn param(&self, name: &str) -> Option<String> {
        self.params
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.clone())
    }
}

fn split_unquoted(text: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for c in text.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            c if c == separator && !in_quotes => parts.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    parts.push(current);
    parts
}

/// A DATE or a DATE-TIME, with the zone it was written in, rendered the way
/// the contract wants it: `YYYY-MM-DD` for a date, RFC 3339 with the zone's
/// offset for an instant.
#[derive(Debug, Clone)]
enum Moment {
    Date(NaiveDate),
    /// A local time in a named zone.
    Zoned(NaiveDateTime, chrono_tz::Tz),
    /// A UTC time (`Z`), or a floating one — which has no zone at all and is
    /// rendered as UTC, the one honest reading of a time that belongs
    /// nowhere in particular.
    Utc(NaiveDateTime),
}

impl Moment {
    fn parse(value: &str, tzid: Option<&str>, all_day: bool) -> Result<Self> {
        let value = value.trim();
        if all_day {
            return Ok(Self::Date(
                NaiveDate::parse_from_str(value, "%Y%m%d").context("not a DATE")?,
            ));
        }
        if let Some(utc) = value.strip_suffix('Z') {
            return Ok(Self::Utc(
                NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").context("not a DATE-TIME")?,
            ));
        }
        let local =
            NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").context("not a DATE-TIME")?;
        match tzid {
            Some(tzid) => {
                let zone: chrono_tz::Tz = tzid
                    .parse()
                    .map_err(|_| anyhow::anyhow!("TZID {tzid:?} is not an IANA zone"))?;
                Ok(Self::Zoned(local, zone))
            }
            None => Ok(Self::Utc(local)),
        }
    }

    fn plus(&self, duration: ChronoDuration) -> Self {
        match self {
            Self::Date(date) => Self::Date(*date + duration),
            Self::Zoned(local, zone) => Self::Zoned(*local + duration, *zone),
            Self::Utc(local) => Self::Utc(*local + duration),
        }
    }

    fn render(&self) -> String {
        match self {
            Self::Date(date) => date.format("%Y-%m-%d").to_string(),
            Self::Zoned(local, zone) => zone
                .from_local_datetime(local)
                .earliest()
                .map(|instant| instant.format("%Y-%m-%dT%H:%M:%S%:z").to_string())
                // A local time that does not exist in the zone (a spring
                // forward): rendered as UTC rather than invented.
                .unwrap_or_else(|| format!("{}Z", local.format("%Y-%m-%dT%H:%M:%S"))),
            Self::Utc(local) => format!("{}Z", local.format("%Y-%m-%dT%H:%M:%S")),
        }
    }
}

/// RFC 5545 §3.3.6: `P1DT2H30M`, `PT15M`, `P2W`, with an optional sign.
pub(crate) fn parse_duration(value: &str) -> Result<ChronoDuration> {
    let value = value.trim();
    let (negative, rest) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let rest = rest.strip_prefix('P').context("a DURATION starts with P")?;
    let mut total = ChronoDuration::zero();
    let mut number = String::new();
    let mut in_time = false;
    for c in rest.chars() {
        match c {
            '0'..='9' => number.push(c),
            'T' => in_time = true,
            unit => {
                let count: i64 = number.parse().context("a DURATION unit without a count")?;
                number.clear();
                total += match (unit, in_time) {
                    ('W', false) => ChronoDuration::weeks(count),
                    ('D', false) => ChronoDuration::days(count),
                    ('H', true) => ChronoDuration::hours(count),
                    ('M', true) => ChronoDuration::minutes(count),
                    ('S', true) => ChronoDuration::seconds(count),
                    _ => anyhow::bail!("unknown DURATION unit {unit:?}"),
                };
            }
        }
    }
    anyhow::ensure!(number.is_empty(), "a DURATION ending in a bare number");
    Ok(if negative { -total } else { total })
}

/// What an agent may learn **about** one event without being given its
/// words (#355, #351).
///
/// The description is the field #351 refused to widen, and the reason does
/// not go away: it is a free-text box written by whoever created the
/// meeting, which on an invitation is a third party who decided nothing
/// about this deployment. But the proposals the owner wants do not need the
/// text. "This meeting has a video link, shall I join it?" needs to know
/// there is one; "there is an agenda to read" needs to know there is one,
/// not what it says.
///
/// So: counts, flags, and exactly one string — the conference URL, which is
/// a URL and not somebody's prose.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Facts {
    /// The join link, when the event says so in a property whose **meaning**
    /// is "this is the join link": `CONFERENCE` (RFC 7986) or the
    /// `X-GOOGLE-CONFERENCE` its exporters write.
    ///
    /// Deliberately **not** scraped out of the DESCRIPTION, which is where
    /// many calendars put it. Pulling a URL out of prose means deciding
    /// which of its URLs is the meeting — and an intranet link, a document,
    /// a one-time token in a path are all URLs too. A persona that learns
    /// "there is a description of 340 characters" and leaves the owner to
    /// open it is less useful and is not wrong.
    pub conference: Option<String>,
    /// How long the DESCRIPTION is, in characters, or `None` when there is
    /// none. The length and not the text: enough to say "there is an agenda
    /// to read" and to tell it from a one-line note.
    pub description_characters: Option<u64>,
    /// How many ATTACH properties the event carries. Their names are not
    /// read either: a file's name is a sentence somebody wrote.
    pub attachments: u64,
}

/// Reads [`Facts`] from an iCalendar resource — the master VEVENT, as
/// [`parse_vevent`] picks it.
///
/// This function is the only place in the collector that looks at a
/// DESCRIPTION at all, and what it takes from it is its length. Everything
/// it returns is a count, a flag, or a URL a property declared to be one.
pub fn facts_of(ics: &str) -> Result<Facts> {
    let lines = unfold(ics);
    let mut inside = false;
    let mut master: Vec<ContentLine> = Vec::new();
    let mut current: Vec<ContentLine> = Vec::new();
    for line in lines {
        let Some(parsed) = ContentLine::parse(&line) else {
            continue;
        };
        match (parsed.name.as_str(), parsed.value.as_str()) {
            ("BEGIN", "VEVENT") => {
                inside = true;
                current = Vec::new();
            }
            ("END", "VEVENT") => {
                inside = false;
                // The master is the one without a RECURRENCE-ID, as
                // `parse_vevent` decides it; the first VEVENT stands in
                // when every one of them is an exception.
                let is_master = !current.iter().any(|line| line.name == "RECURRENCE-ID");
                if master.is_empty() || is_master {
                    master = std::mem::take(&mut current);
                }
            }
            _ if inside => current.push(parsed),
            _ => {}
        }
    }
    anyhow::ensure!(!master.is_empty(), "the resource holds no VEVENT");
    let property = |name: &str| master.iter().find(|line| line.name == name);
    let conference = property("CONFERENCE")
        .or_else(|| property("X-GOOGLE-CONFERENCE"))
        .map(|line| unescape_text(&line.value))
        .map(|url| url.trim().to_owned())
        .filter(|url| is_http_url(url));
    let description_characters = property("DESCRIPTION")
        .map(|line| unescape_text(&line.value))
        .map(|text| text.trim().chars().count() as u64)
        .filter(|count| *count > 0);
    let attachments = master
        .iter()
        .filter(|line| line.name == "ATTACH")
        .count()
        .try_into()
        .unwrap_or(u64::MAX);
    Ok(Facts {
        conference,
        description_characters,
        attachments,
    })
}

/// Whether a `CONFERENCE` value is a URL this collector will pass on. A
/// property may hold `tel:` or anything else a client wrote; only `http`
/// and `https` are handed to an agent, because those are the ones a
/// proposal can act on and the ones a reader can judge.
fn is_http_url(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && !value.contains(char::is_whitespace)
}

/// Whether a published event may carry where the meeting is (#354): the
/// owner's decision, as [`reduce`] is told it. A named pair rather than a
/// `bool`, because `reduce(&event, owner, false, decide)` at a call site
/// says nothing about what the `false` refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// The owner opened the switch: a location travels, unless its
    /// organizer was withheld.
    Carried,
    /// The switch is shut, or could not be read. No location travels.
    Withheld,
}

/// The event as the contract publishes it — `definitions/calendar-event` —
/// after the consent rule: the organizer and each participant whose
/// decision on the mail connection is `revoked` are withheld whole and
/// counted, the owner never. `decide` is the cache's answer for a `mailto:`
/// on the mail connection; a deployment with no mail connection decides
/// nothing about anybody, and `Consent::Pending` — labelled by nothing,
/// reduced by nothing — is what such a closure answers.
pub fn reduce(
    event: &Vevent,
    owner: &str,
    location: Location,
    decide: impl Fn(&str) -> Consent,
) -> Value {
    let owner = owner_mailto(owner);
    let mut withheld = 0u64;
    let mut withhold = |identity: &str| -> bool {
        if identity == owner {
            return false;
        }
        let withheld_one = decide(identity).reduces_publication();
        if withheld_one {
            withheld += 1;
        }
        withheld_one
    };
    let participants: Vec<Value> = event
        .participants
        .iter()
        .filter(|participant| !withhold(&participant.identity))
        .map(|participant| {
            json!({
                "identity": participant.identity,
                "name": participant.name,
                "role": participant.role,
                "partstat": participant.partstat,
            })
        })
        .collect();
    // A withheld organizer's title goes with them: the title of an
    // invitation is the organizer's words on the owner's agenda.
    let organizer_withheld = event
        .organizer
        .as_ref()
        .is_some_and(|person| withhold(&person.identity));
    let (organizer, title) = match (&event.organizer, organizer_withheld) {
        (Some(_), true) => (Value::Null, String::new()),
        (Some(person), false) => (
            json!({ "identity": person.identity, "name": person.name }),
            event.title.clone(),
        ),
        (None, _) => (Value::Null, event.title.clone()),
    };
    // Where the meeting is, on two conditions and never on one (#354).
    // `carry_location` is the owner's switch, off as a deployment ships;
    // and a withheld organizer takes the location with them for the reason
    // they take the title, because the location of an invitation is their
    // words about a place and not the owner's. Asked of the organizer and
    // not of the title, because an event with no SUMMARY has an empty title
    // and nothing was withheld from it.
    let location = match (location, organizer_withheld) {
        (Location::Carried, false) => event
            .location
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
        _ => Value::Null,
    };
    json!({
        "uid": event.uid,
        "title": title,
        "location": location,
        "start": event.start,
        "end": event.end,
        "all_day": event.all_day,
        "timezone": event.timezone,
        "status": event.status,
        "recurrence": event.recurrence,
        "organizer": organizer,
        "participants": participants,
        "participants_withheld": withheld,
    })
}

/// What the collector holds about one calendar between polls, persisted
/// beside the grant: the CTag it last saw, and for every resource it has
/// published, the ETag and the event as published — what `changed_fields`
/// and a removal's title are computed against.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Cursor {
    pub ctag: Option<String>,
    pub known: BTreeMap<String, Known>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Known {
    pub etag: String,
    pub published: Value,
}

/// What a listing says changed since the cursor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Changes {
    pub created: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }

    /// The hrefs to read: created and changed.
    pub fn to_read(&self) -> Vec<String> {
        self.created.iter().chain(&self.changed).cloned().collect()
    }
}

/// The ETag diff: a href the cursor does not hold is created, one whose
/// ETag differs is changed, one the listing no longer has is removed.
pub fn diff(cursor: &Cursor, listing: &Listing) -> Changes {
    let mut changes = Changes::default();
    for (href, etag) in &listing.etags {
        match cursor.known.get(href) {
            None => changes.created.push(href.clone()),
            Some(known) if &known.etag != etag => changes.changed.push(href.clone()),
            Some(_) => {}
        }
    }
    for href in cursor.known.keys() {
        if !listing.etags.contains_key(href) {
            changes.removed.push(href.clone());
        }
    }
    changes
}

/// The published fields that differ between two publications of one event,
/// by the contract's names. `participants_withheld` is not one of them: a
/// withheld participant's answer is not the consumer's to know.
pub fn changed_fields(before: &Value, after: &Value) -> Vec<&'static str> {
    [
        "title",
        "location",
        "start",
        "end",
        "all_day",
        "timezone",
        "status",
        "recurrence",
        "organizer",
        "participants",
    ]
    .into_iter()
    .filter(|field| before.get(field) != after.get(field))
    .collect()
}

/// The three envelopes, for one calendar of one connection.
#[derive(Debug, Clone)]
pub struct Envelopes {
    pub connection: String,
    /// The owner as `mailto:`, the subject of every event.
    pub owner: String,
    /// `caldav://<side service host>/dav/calendars/<owner id>/<calendar>/`.
    pub source: String,
}

impl Envelopes {
    pub fn new(connection: &str, owner_email: &str, side_host: &str, collection: &str) -> Self {
        Self {
            connection: connection.to_owned(),
            owner: owner_mailto(owner_email),
            source: format!("caldav://{side_host}{collection}"),
        }
    }

    pub fn created(&self, href: &str, etag: &str, published: &Value, time: &str) -> Value {
        self.envelope(
            CREATED_TYPE,
            "calendar.event.created",
            sha256_hex(&format!("caldav:{href}:{etag}")),
            time,
            published.clone(),
        )
    }

    pub fn changed(
        &self,
        href: &str,
        etag: &str,
        published: &Value,
        fields: &[&str],
        time: &str,
    ) -> Value {
        self.envelope(
            CHANGED_TYPE,
            "calendar.event.changed",
            sha256_hex(&format!("caldav:{href}:{etag}")),
            time,
            json!({ "event": published, "changed_fields": fields }),
        )
    }

    pub fn removed(&self, href: &str, ctag: &str, uid: &str, title: &str, time: &str) -> Value {
        self.envelope(
            REMOVED_TYPE,
            "calendar.event.removed",
            sha256_hex(&format!("caldav:{href}:removed:{ctag}")),
            time,
            json!({ "uid": uid, "title": title }),
        )
    }

    fn envelope(
        &self,
        event_type: &str,
        schema: &str,
        id: String,
        time: &str,
        data: Value,
    ) -> Value {
        json!({
            "specversion": "1.0",
            "id": id,
            "source": self.source,
            "type": event_type,
            "time": time,
            "subject": self.owner,
            "datacontenttype": "application/json",
            "dataschema": format!("{SCHEMA_BASE}{schema}.schema.json"),
            "connection": self.connection,
            "data": data,
        })
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;

    /// #348: the question the collector asks a calendar is bounded, and
    /// the instants it writes are CalDAV's, not RFC 3339's.
    #[test]
    fn a_calendar_query_names_a_window_in_the_form_caldav_reads() {
        assert_eq!(caldav_instant("2026-09-24T05:00:00Z"), "20260924T050000Z");
        assert_eq!(
            caldav_instant("2026-09-24T05:00:00.123Z"),
            "20260924T050000Z",
            "a fraction is dropped, never folded into the seconds"
        );
        let body = calendar_query_body("2026-09-17T00:00:00Z", "2027-01-15T00:00:00Z");
        assert!(body.contains("<c:comp-filter name=\"VEVENT\">"), "{body}");
        assert!(
            body.contains("start=\"20260917T000000Z\" end=\"20270115T000000Z\""),
            "{body}"
        );
        assert!(
            body.contains("<d:getetag/>") && !body.contains("calendar-data"),
            "the listing asks for etags and never for a word of an event: {body}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEEKLY: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Paris\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:8f3a2b1c-4d5e-6f70-8192-a3b4c5d6e7f8\r\nSUMMARY:Weekly sync\\, with commas\\; and more\r\nDESCRIPTION:Pasted notes nobody decided to share\r\nLOCATION:Salle B\\, 4e étage\r\nDTSTART;TZID=Europe/Paris:20261005T090000\r\nDTEND;TZID=Europe/Paris:20261005T093000\r\nRRULE:FREQ=WEEKLY;BYDAY=MO\r\nORGANIZER;CN=Michel Maudet:mailto:Michel@Example.com\r\nATTENDEE;CN=Michel Maudet;ROLE=CHAIR;PARTSTAT=ACCEPTED:mailto:michel@example.com\r\nATTENDEE;CN=\"Martin, Alice\";ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:alice@example.org\r\nATTENDEE;ROLE=OPT-PARTICIPANT;PARTSTAT=NEEDS-ACTION:mailto:bob@example.org\r\nATTENDEE;CUTYPE=ROOM;CN=Salle B:urn:uuid:room-b\r\nATTACH:https://files.example/secret.pdf\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn a_vevent_is_read_as_the_contract_publishes_it_and_nothing_else() {
        let event = parse_vevent(WEEKLY).unwrap();
        assert_eq!(event.uid, "8f3a2b1c-4d5e-6f70-8192-a3b4c5d6e7f8");
        assert_eq!(event.title, "Weekly sync, with commas; and more");
        assert_eq!(event.start, "2026-10-05T09:00:00+02:00");
        assert_eq!(event.end, "2026-10-05T09:30:00+02:00");
        assert!(!event.all_day);
        assert_eq!(event.timezone.as_deref(), Some("Europe/Paris"));
        assert_eq!(event.status, "confirmed");
        assert_eq!(event.recurrence.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO"));
        let organizer = event.organizer.as_ref().unwrap();
        assert_eq!(
            organizer.identity, "mailto:michel@example.com",
            "lower-cased"
        );
        assert_eq!(organizer.name.as_deref(), Some("Michel Maudet"));
        assert_eq!(event.participants.len(), 3, "the room is not a person");
        assert_eq!(event.participants[1].name.as_deref(), Some("Martin, Alice"));
        assert_eq!(event.participants[1].role, "required");
        assert_eq!(event.participants[2].partstat, "needs-action");
        assert_eq!(event.participants[2].role, "optional");
        let text = serde_json::to_string(&event).unwrap();
        assert!(!text.contains("Pasted notes"), "{text}");
        assert!(!text.contains("secret.pdf"), "{text}");
    }

    #[test]
    fn an_all_day_event_a_duration_and_a_winter_time_read_as_dates_and_offsets() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:d1\r\nSUMMARY:Off\r\nDTSTART;VALUE=DATE:20261003\r\nSTATUS:TENTATIVE\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let event = parse_vevent(ics).unwrap();
        assert!(event.all_day);
        assert_eq!(
            (event.start.as_str(), event.end.as_str()),
            ("2026-10-03", "2026-10-04")
        );
        assert_eq!(event.timezone, None);
        assert_eq!(event.status, "tentative");
        assert_eq!(event.organizer, None);

        let ics = "BEGIN:VEVENT\r\nUID:d2\r\nDTSTART;TZID=Europe/Paris:20261207T140000\r\nDURATION:PT1H30M\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\n";
        let event = parse_vevent(ics).unwrap();
        assert_eq!(event.start, "2026-12-07T14:00:00+01:00", "winter: +01:00");
        assert_eq!(event.end, "2026-12-07T15:30:00+01:00");
        assert_eq!(event.status, "cancelled");
        assert_eq!(event.title, "");

        let ics = "BEGIN:VEVENT\r\nUID:d3\r\nDTSTART:20261207T130000Z\r\nEND:VEVENT\r\n";
        let event = parse_vevent(ics).unwrap();
        assert_eq!(event.start, "2026-12-07T13:00:00Z");
        assert_eq!(
            event.end, event.start,
            "no end and no duration: no time occupied"
        );
    }

    #[test]
    fn a_folded_line_and_a_recurrence_exception_are_read_as_one_master_event() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nRECURRENCE-ID;TZID=Europe/Paris:20261012T090000\r\nSUMMARY:Moved once\r\nDTSTART;TZID=Europe/Paris:20261012T100000\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:x\r\nSUMMARY:A title long enough to be\r\n  folded by the server onto a second line\r\nDTSTART;TZID=Europe/Paris:20261005T090000\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let event = parse_vevent(ics).unwrap();
        assert_eq!(
            event.title,
            "A title long enough to be folded by the server onto a second line"
        );
        assert_eq!(
            event.start, "2026-10-05T09:00:00+02:00",
            "the master, not the exception"
        );
        assert!(parse_vevent("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").is_err());
    }

    #[test]
    fn a_listing_is_the_ctag_and_the_etags_and_a_diff_is_three_lists() {
        let collection = "/dav/calendars/o/c/";
        let xml = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/">
<d:response><d:href>/dav/calendars/o/c/</d:href><d:propstat><d:prop><cs:getctag>http://sabre.io/ns/sync/7</cs:getctag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/dav/calendars/o/c/a.ics</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/dav/calendars/o/c/b.ics</d:href><d:propstat><d:prop><d:getetag>"5"</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#;
        let listing = parse_listing(xml, collection).unwrap();
        assert_eq!(listing.ctag.as_deref(), Some("http://sabre.io/ns/sync/7"));
        assert_eq!(listing.etags.len(), 2);
        let mut cursor = Cursor::default();
        cursor.known.insert(
            "/dav/calendars/o/c/b.ics".to_owned(),
            Known {
                etag: "\"4\"".to_owned(),
                published: json!({}),
            },
        );
        cursor.known.insert(
            "/dav/calendars/o/c/gone.ics".to_owned(),
            Known {
                etag: "\"2\"".to_owned(),
                published: json!({}),
            },
        );
        let changes = diff(&cursor, &listing);
        assert_eq!(changes.created, ["/dav/calendars/o/c/a.ics"]);
        assert_eq!(changes.changed, ["/dav/calendars/o/c/b.ics"]);
        assert_eq!(changes.removed, ["/dav/calendars/o/c/gone.ics"]);
        assert_eq!(changes.to_read().len(), 2);
        assert!(diff(&Cursor::default(), &Listing::default()).is_empty());
    }

    #[test]
    fn a_multiget_answer_is_the_resources_with_data_and_the_hal_list_is_the_calendars() {
        let xml = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:cal="urn:ietf:params:xml:ns:caldav">
<d:response><d:href>/dav/calendars/o/c/a.ics</d:href><d:propstat><d:prop><d:getetag>"1"</d:getetag><cal:calendar-data>BEGIN:VEVENT
UID:a
DTSTART:20261001T100000Z
END:VEVENT</cal:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/dav/calendars/o/c/gone.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>
</d:multistatus>"#;
        let resources = parse_multiget(xml).unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].etag, "\"1\"");
        assert!(resources[0].ics.contains("UID:a"));
        let body = multiget_body(&["/dav/calendars/o/c/a&b.ics".to_owned()]);
        assert!(body.contains("<d:href>/dav/calendars/o/c/a&amp;b.ics</d:href>"));

        let hal = json!({
            "_embedded": { "dav:calendar": [
                { "_links": { "self": { "href": "/dav/calendars/o/o.json" } }, "dav:name": "Mine" },
                { "_links": { "self": { "href": "/dav/calendars/o/shared-1.json" } }, "calendarserver:ctag": "http://sabre.io/ns/sync/9" },
                { "dav:name": "no link" }
            ] }
        });
        let calendars = calendars_in(&hal);
        assert_eq!(calendars.len(), 2);
        assert_eq!(
            calendars[0],
            Calendar {
                id: "o".to_owned(),
                name: "Mine".to_owned(),
                ctag: None
            }
        );
        assert_eq!(calendars[1].id, "shared-1");
        assert_eq!(
            calendars[1].ctag.as_deref(),
            Some("http://sabre.io/ns/sync/9")
        );
        assert_eq!(
            collection_path("o", "shared-1"),
            "/dav/calendars/o/shared-1/"
        );
    }

    #[test]
    fn a_revoked_participant_is_withheld_whole_and_counted_and_the_owner_never_is() {
        let event = parse_vevent(WEEKLY).unwrap();
        let decide = |identity: &str| match identity {
            "mailto:bob@example.org" => Consent::Revoked,
            "mailto:alice@example.org" => Consent::Granted,
            // The owner's own address, were it ever asked about: the closure
            // is never asked, and this answer would be wrong to use.
            _ => Consent::Revoked,
        };
        let published = reduce(&event, "Michel@example.com", Location::Withheld, decide);
        let participants = published["participants"].as_array().unwrap();
        assert_eq!(participants.len(), 2);
        assert!(participants
            .iter()
            .all(|p| p["identity"] != "mailto:bob@example.org"));
        assert_eq!(
            participants[0]["identity"], "mailto:michel@example.com",
            "the owner stays"
        );
        assert_eq!(
            published["organizer"]["identity"],
            "mailto:michel@example.com"
        );
        assert_eq!(published["participants_withheld"], 1);
        let text = published.to_string();
        assert!(!text.contains("bob"), "{text}");

        // Nobody decided: nobody withheld, pending is not a reduction.
        let untouched = reduce(&event, "michel@example.com", Location::Withheld, |_| {
            Consent::Pending
        });
        assert_eq!(untouched["participants"].as_array().unwrap().len(), 3);
        assert_eq!(untouched["participants_withheld"], 0);
        // A revoked organizer who is not the owner: null, and counted.
        let mut foreign = event.clone();
        foreign.organizer = Some(Person {
            identity: "mailto:carol@example.org".to_owned(),
            name: None,
        });
        let reduced = reduce(&foreign, "michel@example.com", Location::Withheld, |_| {
            Consent::Revoked
        });
        assert!(reduced["organizer"].is_null());
        assert_eq!(
            reduced["participants_withheld"], 3,
            "two attendees and the organizer"
        );
    }

    #[test]
    fn facts_say_what_an_event_carries_and_never_what_it_says() {
        // #355: the fixture has a DESCRIPTION, an ATTACH and a LOCATION.
        // What comes back is a length, a count, and no prose.
        let facts = facts_of(WEEKLY).unwrap();
        assert_eq!(
            facts.description_characters,
            Some("Pasted notes nobody decided to share".chars().count() as u64)
        );
        assert_eq!(facts.attachments, 1);
        assert_eq!(facts.conference, None, "the fixture declares no join link");
        let said = format!("{facts:?}");
        assert!(!said.contains("Pasted notes"), "no prose: {said}");
        assert!(!said.contains("secret.pdf"), "no attachment name: {said}");

        // A join link is read from the property whose meaning is "this is
        // the join link", and handed on as it stands.
        let with_link = WEEKLY.replace(
            "ATTACH:https://files.example/secret.pdf\r\n",
            "CONFERENCE;VALUE=URI;FEATURE=VIDEO:https://meet.example/abc-def\r\n",
        );
        let facts = facts_of(&with_link).unwrap();
        assert_eq!(
            facts.conference.as_deref(),
            Some("https://meet.example/abc-def")
        );
        assert_eq!(facts.attachments, 0);

        // And never scraped out of the description, which is where many
        // calendars put it: a URL in prose may be an intranet page, a
        // document, or a token in a path, and choosing among them means
        // reading the words this refuses to read.
        let in_prose = WEEKLY.replace(
            "DESCRIPTION:Pasted notes nobody decided to share",
            "DESCRIPTION:Join at https://meet.example/xyz and read the plan",
        );
        let facts = facts_of(&in_prose).unwrap();
        assert_eq!(facts.conference, None);
        assert!(facts.description_characters.unwrap() > 0);

        // A CONFERENCE that is not an http(s) URL is not passed on.
        let telephone = WEEKLY.replace(
            "ATTACH:https://files.example/secret.pdf\r\n",
            "CONFERENCE;VALUE=URI:tel:+33-1-23-45-67-89\r\n",
        );
        assert_eq!(facts_of(&telephone).unwrap().conference, None);

        // An event with neither says so with an absence, not a zero-length
        // string: "there is no agenda" and "there is an empty one" are the
        // same thing to a reader and should be the same answer.
        let bare = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:bare\r\nSUMMARY:Bare\r\nDTSTART:20261005T090000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let facts = facts_of(bare).unwrap();
        assert_eq!(facts.description_characters, None);
        assert_eq!(facts.attachments, 0);
        assert_eq!(facts.conference, None);

        // Not a VEVENT at all is an error, not empty facts: "this resource
        // holds nothing I can read" and "this meeting has nothing" are
        // different answers.
        assert!(facts_of("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").is_err());
    }

    #[test]
    fn the_location_travels_only_when_the_owner_has_opened_the_switch() {
        // #354: the field the owner asked for, and the two conditions on
        // it. The ICS fixture has a LOCATION, a DESCRIPTION and an ATTACH;
        // only the first is ever readable, and only then.
        let event = parse_vevent(WEEKLY).unwrap();
        assert_eq!(event.location.as_deref(), Some("Salle B, 4e étage"));

        let shut = reduce(&event, "michel@example.com", Location::Withheld, |_| {
            Consent::Granted
        });
        assert_eq!(
            shut["location"],
            Value::Null,
            "a deployment ships with the switch off, and an off switch sends no place"
        );
        let open = reduce(&event, "michel@example.com", Location::Carried, |_| {
            Consent::Granted
        });
        assert_eq!(
            open["location"], "Salle B, 4e étage",
            "opened, it carries the place as the calendar holds it — unescaped, not parsed"
        );

        // The organizer's rule reaches it: the location of an invitation is
        // their words about a place. A revoked organizer takes it with the
        // title, switch or no switch.
        let foreign =
            parse_vevent(&WEEKLY.replace("mailto:Michel@Example.com", "mailto:zoe@example.org"))
                .unwrap();
        let withheld = reduce(&foreign, "michel@example.com", Location::Carried, |_| {
            Consent::Revoked
        });
        assert_eq!(withheld["title"], "");
        assert_eq!(
            withheld["location"],
            Value::Null,
            "the place goes with the words of whoever wrote them"
        );

        // And what is never read is never published, at any setting.
        for published in [&shut, &open, &withheld] {
            let text = serde_json::to_string(published).unwrap();
            assert!(!text.contains("Pasted notes"), "no DESCRIPTION: {text}");
            assert!(!text.contains("secret.pdf"), "no ATTACH: {text}");
            assert!(
                published.get("description").is_none() && published.get("attachments").is_none(),
                "and no member for either: {published}"
            );
        }

        // A location that moved is a change with a name (#354), so an owner
        // reading their journal sees which field moved rather than "the
        // event changed".
        let mut moved = event.clone();
        moved.location = Some("Salle A".to_owned());
        let after = reduce(&moved, "michel@example.com", Location::Carried, |_| {
            Consent::Granted
        });
        assert_eq!(changed_fields(&open, &after), vec!["location"]);
        // With the switch shut, nothing moved, because nothing was said.
        let after_shut = reduce(&moved, "michel@example.com", Location::Withheld, |_| {
            Consent::Granted
        });
        assert!(changed_fields(&shut, &after_shut).is_empty());
    }

    #[test]
    fn changed_fields_name_what_moved_and_the_envelopes_carry_the_contracts_ids() {
        let event = parse_vevent(WEEKLY).unwrap();
        let before = reduce(&event, "michel@example.com", Location::Withheld, |_| {
            Consent::Pending
        });
        let mut moved = event.clone();
        moved.start = "2026-10-05T10:00:00+02:00".to_owned();
        moved.end = "2026-10-05T10:30:00+02:00".to_owned();
        let after = reduce(&moved, "michel@example.com", Location::Withheld, |_| {
            Consent::Pending
        });
        assert_eq!(changed_fields(&before, &after), ["start", "end"]);
        assert!(changed_fields(&before, &before).is_empty());
        // A participant newly revoked moves the count and nothing named.
        let fewer = reduce(&event, "michel@example.com", Location::Withheld, |id| {
            if id == "mailto:bob@example.org" {
                Consent::Revoked
            } else {
                Consent::Pending
            }
        });
        assert_eq!(changed_fields(&before, &fewer), ["participants"]);

        let envelopes = Envelopes::new(
            "agenda-linagora",
            "Michel@example.com",
            "calendar.example.com",
            "/dav/calendars/64f1c0a2e9b1d3f4a5b6c7d8/64f1c0a2e9b1d3f4a5b6c7d8/",
        );
        let href = "/dav/calendars/64f1c0a2e9b1d3f4a5b6c7d8/64f1c0a2e9b1d3f4a5b6c7d8/8f3a2b1c-weekly-sync.ics";
        let created = envelopes.created(href, "\"a1b2c3-1\"", &before, "2026-10-01T08:00:12Z");
        let fixture: Value = serde_json::from_str(include_str!(
            "../../contracts/cloudevents/v1/fixtures/calendar.event.created.json"
        ))
        .unwrap();
        assert_eq!(
            created["id"], fixture["id"],
            "the fixture's id, by the schema's recipe"
        );
        assert_eq!(created["source"], fixture["source"]);
        assert_eq!(created["subject"], "mailto:michel@example.com");
        assert_eq!(created["connection"], "agenda-linagora");
        let removed = envelopes.removed(
            href,
            "http://sabre.io/ns/sync/1042",
            &event.uid,
            &event.title,
            "2026-10-09T07:12:40Z",
        );
        let fixture: Value = serde_json::from_str(include_str!(
            "../../contracts/cloudevents/v1/fixtures/calendar.event.removed.json"
        ))
        .unwrap();
        assert_eq!(removed["id"], fixture["id"]);
        assert_eq!(removed["data"]["uid"], fixture["data"]["uid"]);
        let changed = envelopes.changed(
            href,
            "\"a1b2c3-2\"",
            &after,
            &["start", "end"],
            "2026-10-02T14:31:05Z",
        );
        assert_eq!(changed["data"]["changed_fields"], json!(["start", "end"]));
        assert_eq!(changed["type"], CHANGED_TYPE);
    }

    #[test]
    fn a_duration_reads_every_unit_and_refuses_a_bare_number() {
        assert_eq!(
            parse_duration("P1DT2H30M").unwrap(),
            ChronoDuration::minutes(24 * 60 + 150)
        );
        assert_eq!(parse_duration("P2W").unwrap(), ChronoDuration::weeks(2));
        assert_eq!(
            parse_duration("-PT15M").unwrap(),
            ChronoDuration::minutes(-15)
        );
        assert!(parse_duration("P15").is_err());
        assert!(parse_duration("15M").is_err());
    }
}
