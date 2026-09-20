//! Free/busy, the one governed pull (issue #281, ADR 0032's founding
//! example: a message that needs a calendar looked at before three times are
//! proposed). The pure half: the window a read is allowed to ask for, the
//! `REPORT free-busy-query` (RFC 4791 §7.10) that asks the side service for
//! it, and the VFREEBUSY that comes back read as **busy intervals and
//! nothing else** — no title, no participant, no location ever leaves this
//! module, because none is read: a free-busy report carries none.
//!
//! The window is capped at fourteen days on this side as well as on the
//! Companion Gateway's: the Gateway refuses a wider one before it relays,
//! and this refuses it again because a relay that trusted its caller with
//! the size of a read of the owner's agenda would be a hole one
//! misconfiguration wide. The I/O — every calendar of the owner asked, the
//! answers merged — is `Side::free_busy` in `calendars.rs` and the route in
//! `http.rs`.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDateTime, Utc};
use serde::Serialize;

/// The widest window a read may ask for.
pub const MAX_WINDOW: ChronoDuration = ChronoDuration::days(14);

/// One busy interval, as the answer carries it: RFC 3339 in UTC, the end
/// exclusive, clipped to the window asked for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Busy {
    pub start: String,
    pub end: String,
}

/// The window a read asks for, checked: `from` before `to`, at most
/// [`MAX_WINDOW`] wide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// Why a window is refused, in the words the caller is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowError {
    /// `from` or `to` is not an RFC 3339 instant.
    NotAnInstant(&'static str),
    /// `to` is not after `from`.
    Empty,
    /// Wider than [`MAX_WINDOW`].
    TooWide,
}

impl WindowError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotAnInstant(_) | Self::Empty => "invalid_window",
            Self::TooWide => "window_too_wide",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::NotAnInstant(which) => {
                format!("`{which}` is not an RFC 3339 instant (2026-09-24T08:00:00Z)")
            }
            Self::Empty => "`to` must be after `from`".to_owned(),
            Self::TooWide => format!(
                "the window is wider than the {} days a free/busy read may ask for",
                MAX_WINDOW.num_days()
            ),
        }
    }
}

impl Window {
    pub fn parse(from: &str, to: &str) -> Result<Self, WindowError> {
        let from = DateTime::parse_from_rfc3339(from.trim())
            .map_err(|_| WindowError::NotAnInstant("from"))?
            .with_timezone(&Utc);
        let to = DateTime::parse_from_rfc3339(to.trim())
            .map_err(|_| WindowError::NotAnInstant("to"))?
            .with_timezone(&Utc);
        if to <= from {
            return Err(WindowError::Empty);
        }
        if to - from > MAX_WINDOW {
            return Err(WindowError::TooWide);
        }
        Ok(Self { from, to })
    }

    /// The `REPORT free-busy-query` body for this window (RFC 4791 §7.10):
    /// the time range and nothing else, since the report's answer is fixed
    /// by the specification — periods, no component data.
    pub fn report_body(&self) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<C:free-busy-query xmlns:C=\"urn:ietf:params:xml:ns:caldav\">\n  <C:time-range start=\"{}\" end=\"{}\"/>\n</C:free-busy-query>\n",
            ical_utc(&self.from),
            ical_utc(&self.to)
        )
    }
}

fn ical_utc(at: &DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

fn rfc3339(at: &DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The busy periods of one VFREEBUSY (RFC 5545 §3.6.4), read from the
/// iCalendar text a `free-busy-query` answers: every `FREEBUSY` line whose
/// `FBTYPE` is not `FREE` — `BUSY`, `BUSY-UNAVAILABLE` and `BUSY-TENTATIVE`
/// alike, since a slot the owner may be in is not one to propose — each
/// period `start/end` or `start/duration`, in UTC as the specification
/// requires of a VFREEBUSY. Clipped to the window and merged, so the
/// answer says when the owner is busy and not how many things they have.
pub fn parse_free_busy(ics: &str, window: &Window) -> Result<Vec<Busy>> {
    let mut periods: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new();
    for line in crate::caldav::unfold(ics) {
        let Some((head, value)) = line.split_once(':') else {
            continue;
        };
        let mut parts = head.split(';');
        if !parts
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("FREEBUSY"))
        {
            continue;
        }
        let free = parts.any(|param| {
            param.split_once('=').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("FBTYPE") && value.eq_ignore_ascii_case("FREE")
            })
        });
        if free {
            continue;
        }
        for period in value.split(',') {
            let (start, end) = period
                .split_once('/')
                .with_context(|| format!("a FREEBUSY period is start/end: {period:?}"))?;
            let start = utc_instant(start)?;
            let end = if end.trim().starts_with('P') || end.trim().starts_with("-P") {
                start + crate::caldav::parse_duration(end)?
            } else {
                utc_instant(end)?
            };
            periods.push((start, end));
        }
    }
    Ok(merge(periods, window))
}

fn utc_instant(value: &str) -> Result<DateTime<Utc>> {
    let value = value.trim();
    let bare = value
        .strip_suffix('Z')
        .with_context(|| format!("a VFREEBUSY instant is UTC: {value:?}"))?;
    let naive = NaiveDateTime::parse_from_str(bare, "%Y%m%dT%H%M%S")
        .with_context(|| format!("not a DATE-TIME: {value:?}"))?;
    Ok(DateTime::from_naive_utc_and_offset(naive, Utc))
}

/// Periods from any number of calendars, clipped to the window, sorted and
/// merged where they touch or overlap.
pub fn merge(periods: Vec<(DateTime<Utc>, DateTime<Utc>)>, window: &Window) -> Vec<Busy> {
    let mut clipped: Vec<(DateTime<Utc>, DateTime<Utc>)> = periods
        .into_iter()
        .map(|(start, end)| (start.max(window.from), end.min(window.to)))
        .filter(|(start, end)| end > start)
        .collect();
    clipped.sort();
    let mut merged: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new();
    for (start, end) in clipped {
        match merged.last_mut() {
            Some((_, last_end)) if start <= *last_end => {
                if end > *last_end {
                    *last_end = end;
                }
            }
            _ => merged.push((start, end)),
        }
    }
    merged
        .into_iter()
        .map(|(start, end)| Busy {
            start: rfc3339(&start),
            end: rfc3339(&end),
        })
        .collect()
}

/// Busy intervals from several calendars, as one answer: re-merged, since
/// two calendars may say the owner is busy at once.
pub fn merge_answers(answers: Vec<Vec<Busy>>, window: &Window) -> Vec<Busy> {
    let periods = answers
        .into_iter()
        .flatten()
        .filter_map(|busy| {
            Some((
                DateTime::parse_from_rfc3339(&busy.start)
                    .ok()?
                    .with_timezone(&Utc),
                DateTime::parse_from_rfc3339(&busy.end)
                    .ok()?
                    .with_timezone(&Utc),
            ))
        })
        .collect();
    merge(periods, window)
}

#[cfg(test)]
mod tests {
    use super::{merge_answers, parse_free_busy, Busy, Window, WindowError};

    fn window() -> Window {
        Window::parse("2026-09-24T00:00:00Z", "2026-09-25T00:00:00Z").unwrap()
    }

    #[test]
    fn a_window_is_two_instants_in_order_and_at_most_fourteen_days() {
        let window = Window::parse("2026-09-24T08:00:00+02:00", "2026-09-30T18:00:00Z").unwrap();
        assert_eq!(
            window.report_body(),
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<C:free-busy-query xmlns:C=\"urn:ietf:params:xml:ns:caldav\">\n  <C:time-range start=\"20260924T060000Z\" end=\"20260930T180000Z\"/>\n</C:free-busy-query>\n"
        );
        assert_eq!(
            Window::parse("2026-09-24T08:00:00Z", "2026-10-08T08:00:00Z")
                .map(|_| ())
                .unwrap(),
            (),
            "fourteen days exactly is allowed"
        );
        assert_eq!(
            Window::parse("2026-09-24T08:00:00Z", "2026-10-08T08:00:01Z"),
            Err(WindowError::TooWide)
        );
        assert_eq!(
            Window::parse("2026-09-24T08:00:00Z", "2026-09-24T08:00:00Z"),
            Err(WindowError::Empty)
        );
        assert_eq!(
            Window::parse("jeudi", "2026-09-24T08:00:00Z"),
            Err(WindowError::NotAnInstant("from"))
        );
        assert_eq!(WindowError::TooWide.code(), "window_too_wide");
        assert_eq!(WindowError::Empty.code(), "invalid_window");
    }

    #[test]
    fn a_vfreebusy_is_read_as_busy_periods_clipped_merged_and_nothing_else() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Sabre//Sabre VObject 4.5.3//EN\r\nBEGIN:VFREEBUSY\r\nDTSTART:20260924T000000Z\r\nDTEND:20260925T000000Z\r\nDTSTAMP:20260920T160000Z\r\nFREEBUSY;FBTYPE=BUSY:20260924T080000Z/20260924T090000Z,20260924T083000Z/PT1H\r\nFREEBUSY;FBTYPE=BUSY-TENTATIVE:20260924T140000Z/20260924T150000Z\r\nFREEBUSY;FBTYPE=FREE:20260924T160000Z/20260924T170000Z\r\nFREEBUSY:20260923T230000Z/20260924T003000Z\r\nFREEBUSY:20260924T150000Z/20260924T153000Z\r\nEND:VFREEBUSY\r\nEND:VCALENDAR\r\n";
        let busy = parse_free_busy(ics, &window()).unwrap();
        assert_eq!(
            busy,
            vec![
                Busy {
                    start: "2026-09-24T00:00:00Z".to_owned(),
                    end: "2026-09-24T00:30:00Z".to_owned()
                },
                Busy {
                    start: "2026-09-24T08:00:00Z".to_owned(),
                    end: "2026-09-24T09:30:00Z".to_owned()
                },
                Busy {
                    start: "2026-09-24T14:00:00Z".to_owned(),
                    end: "2026-09-24T15:30:00Z".to_owned()
                },
            ]
        );
        assert!(
            parse_free_busy("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n", &window())
                .unwrap()
                .is_empty()
        );
        assert!(
            parse_free_busy("FREEBUSY:20260924T080000/20260924T090000\r\n", &window()).is_err()
        );
    }

    #[test]
    fn two_calendars_answers_are_one_when_they_overlap() {
        let one = vec![Busy {
            start: "2026-09-24T08:00:00Z".to_owned(),
            end: "2026-09-24T09:00:00Z".to_owned(),
        }];
        let two = vec![
            Busy {
                start: "2026-09-24T09:00:00Z".to_owned(),
                end: "2026-09-24T10:00:00Z".to_owned(),
            },
            Busy {
                start: "2026-09-24T12:00:00Z".to_owned(),
                end: "2026-09-24T12:30:00Z".to_owned(),
            },
        ];
        assert_eq!(
            merge_answers(vec![one, two], &window()),
            vec![
                Busy {
                    start: "2026-09-24T08:00:00Z".to_owned(),
                    end: "2026-09-24T10:00:00Z".to_owned()
                },
                Busy {
                    start: "2026-09-24T12:00:00Z".to_owned(),
                    end: "2026-09-24T12:30:00Z".to_owned()
                },
            ]
        );
    }
}
