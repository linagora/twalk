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
    /// The window a calendar poll asks about (#348): so many days behind
    /// an instant and so many ahead. Not a free/busy window — that one is
    /// the contact's question and is capped at fourteen days; this one is
    /// the perimeter the collector watches, and the deployment sets it.
    pub fn around(at: std::time::SystemTime, back_days: i64, ahead_days: i64) -> Self {
        let at: DateTime<Utc> = at.into();
        Self {
            from: at - chrono::Duration::days(back_days),
            to: at + chrono::Duration::days(ahead_days),
        }
    }

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
        // Never the period's text in an error: an error is a log line and a
        // 502's detail, and a period is a piece of the owner's agenda.
        for period in value.split(',') {
            let (start, end) = period
                .split_once('/')
                .context("a FREEBUSY period is start/end or start/duration")?;
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
        .context("a VFREEBUSY instant is UTC (RFC 5545 §3.8.2.6), and this one is not")?;
    let naive = NaiveDateTime::parse_from_str(bare, "%Y%m%dT%H%M%S")
        .context("a VFREEBUSY period holds DATE-TIMEs, and this one does not")?;
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

/// One stretch of the window nothing occupies, in UTC and in the owner's own
/// time (#379).
///
/// The UTC pair is the fact; the local pair is the same fact in the form a
/// sentence needs. Both, because the arithmetic between them is what a
/// drafting agent was measured getting wrong in silence: on 2026-09-26 one
/// read three windows, was told `Europe/Paris`, and proposed two times that
/// overlapped a meeting — it had found the gaps in the UTC intervals and
/// written them as if they were Paris hours. An agent that copies cannot
/// make that mistake; an agent that converts already has.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Free {
    pub start: String,
    pub end: String,
    /// The same instants in the owner's zone, absent together when no zone
    /// is known — never guessed, for the reason the zone itself is not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_local: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_local: Option<String>,
    /// How long it is. A four-minute gap is visibly not a meeting slot, and
    /// an agent can prefer a long one without measuring it.
    pub minutes: i64,
}

/// The owner's working day, as the Companion Gateway answers it (#381): the
/// ISO weekdays they accept meetings on, and the wall clock their day runs
/// between — `09:00` meaning nine in the morning where they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingDay {
    pub days: Vec<u8>,
    pub starts_at: String,
    pub ends_at: String,
}

impl WorkingDay {
    /// The minutes since midnight a wall clock names, or `None` for a
    /// spelling this does not accept — in which case the caller offers every
    /// gap rather than a window it guessed.
    fn minutes(value: &str) -> Option<u32> {
        let (hours, minutes) = value.split_once(':')?;
        let (hours, minutes): (u32, u32) = (hours.parse().ok()?, minutes.parse().ok()?);
        (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
    }
}

/// The stretches of the window the busy intervals leave, in order (#379).
///
/// The complement of what [`merge_answers`] produced, which is why it takes
/// that output rather than recomputing it: the intervals are already merged,
/// clipped to the window and in order, so the gaps are what lies between
/// them, plus the ends of the window.
///
/// `zone` spells each gap in the owner's time when the deployment knows it.
/// Nothing is dropped for being short or nocturnal: which gap is worth
/// offering a person is the drafting agent's judgement, guided by its skill,
/// and a collector that hid the night would be making a decision about
/// somebody's working hours that nobody told it.
pub fn free_between(
    busy: &[Busy],
    window: &Window,
    zone: Option<&str>,
    working_day: Option<&WorkingDay>,
) -> Vec<Free> {
    let zone = zone.and_then(|name| name.parse::<chrono_tz::Tz>().ok());
    let local = |at: &DateTime<Utc>| {
        zone.map(|zone| {
            at.with_timezone(&zone)
                .format("%Y-%m-%dT%H:%M:%S%:z")
                .to_string()
        })
    };
    let mut gaps = Vec::new();
    let mut at = window.from;
    let push = |from: DateTime<Utc>, to: DateTime<Utc>, gaps: &mut Vec<Free>| {
        let minutes = (to - from).num_minutes();
        if minutes <= 0 {
            return;
        }
        gaps.push(Free {
            start: from.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            end: to.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            start_local: local(&from),
            end_local: local(&to),
            minutes,
        });
    };
    for interval in busy {
        let (Ok(start), Ok(end)) = (
            DateTime::parse_from_rfc3339(&interval.start),
            DateTime::parse_from_rfc3339(&interval.end),
        ) else {
            // An interval this cannot read is left in place rather than
            // skipped: pretending it is not there would answer a gap the
            // owner is busy in, which is the whole error this exists against.
            return Vec::new();
        };
        let (start, end) = (start.with_timezone(&Utc), end.with_timezone(&Utc));
        push(at, start, &mut gaps);
        at = at.max(end);
    }
    push(at, window.to, &mut gaps);
    // And the owner's working day, when they have said what it is (#381).
    // Applied to the **gaps** and never to the busy intervals: "you are
    // busy" is a fact and "I would rather not be offered that" is a
    // preference, and a read that trimmed an occupation would be lying about
    // a calendar.
    //
    // Nothing is filtered without a zone: an amplitude is a wall clock, and
    // clipping by one without knowing where the owner is would cut the wrong
    // hours. Every gap is offered instead, which is what happens for a
    // deployment that never said either.
    match (working_day, zone) {
        (Some(day), Some(zone)) => within(gaps, day, zone),
        _ => gaps,
    }
}

/// The parts of each gap that fall inside the owner's working day (#381).
///
/// A gap is walked day by day in the owner's own zone, because that is what
/// the amplitude is expressed in: a gap from Friday 18:00 to Monday 10:00
/// contributes nothing on Saturday and Sunday when those are not accepted
/// days, and on Monday only from 09:00. A gap that straddles an edge is
/// **clipped, not dropped** — 07:00–09:30 against a day starting at 09:00 is
/// a half-hour offer, and half an hour is a meeting.
fn within(gaps: Vec<Free>, day: &WorkingDay, zone: chrono_tz::Tz) -> Vec<Free> {
    use chrono::{Datelike, NaiveTime, TimeZone};
    let (Some(from_minutes), Some(to_minutes)) = (
        WorkingDay::minutes(&day.starts_at),
        WorkingDay::minutes(&day.ends_at),
    ) else {
        // A spelling this cannot read: every gap, rather than a window
        // invented out of half a value.
        return gaps;
    };
    let mut kept = Vec::new();
    for gap in &gaps {
        let (Ok(start), Ok(end)) = (
            DateTime::parse_from_rfc3339(&gap.start),
            DateTime::parse_from_rfc3339(&gap.end),
        ) else {
            kept.push(gap.clone());
            continue;
        };
        let (start, end) = (start.with_timezone(&Utc), end.with_timezone(&Utc));
        let mut date = start.with_timezone(&zone).date_naive();
        let last = end.with_timezone(&zone).date_naive();
        while date <= last {
            if day
                .days
                .contains(&(date.weekday().number_from_monday() as u8))
            {
                let at = |minutes: u32| {
                    let time = NaiveTime::from_num_seconds_from_midnight_opt(minutes * 60, 0)?;
                    zone.from_local_datetime(&date.and_time(time))
                        .earliest()
                        .map(|at| at.with_timezone(&Utc))
                };
                if let (Some(opens), Some(closes)) = (at(from_minutes), at(to_minutes)) {
                    let (from, to) = (start.max(opens), end.min(closes));
                    let minutes = (to - from).num_minutes();
                    if minutes > 0 {
                        kept.push(Free {
                            start: from.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                            end: to.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                            start_local: Some(
                                from.with_timezone(&zone)
                                    .format("%Y-%m-%dT%H:%M:%S%:z")
                                    .to_string(),
                            ),
                            end_local: Some(
                                to.with_timezone(&zone)
                                    .format("%Y-%m-%dT%H:%M:%S%:z")
                                    .to_string(),
                            ),
                            minutes,
                        });
                    }
                }
            }
            let Some(next) = date.succ_opt() else { break };
            date = next;
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::{
        free_between, merge_answers, parse_free_busy, Busy, Free, Window, WindowError, WorkingDay,
    };

    fn window() -> Window {
        Window::parse("2026-09-24T00:00:00Z", "2026-09-25T00:00:00Z").unwrap()
    }

    /// #379: the gaps, and the arithmetic an agent must not be asked to do.
    #[test]
    fn the_free_gaps_are_the_windows_complement_in_the_owners_own_time() {
        let window = Window::parse("2026-10-12T06:00:00Z", "2026-10-12T18:00:00Z").unwrap();
        let busy = vec![
            Busy {
                start: "2026-10-12T07:30:00Z".to_owned(),
                end: "2026-10-12T10:00:00Z".to_owned(),
            },
            Busy {
                start: "2026-10-12T12:00:00Z".to_owned(),
                end: "2026-10-12T13:00:00Z".to_owned(),
            },
        ];
        let free = free_between(&busy, &window, Some("Europe/Paris"), None);
        assert_eq!(
            free.iter().map(|gap| gap.start.as_str()).collect::<Vec<_>>(),
            vec![
                "2026-10-12T06:00:00Z",
                "2026-10-12T10:00:00Z",
                "2026-10-12T13:00:00Z"
            ],
            "the gaps are what the intervals leave, including both ends of the window"
        );

        // The pair that stops the error this ticket comes from: 10:00 UTC is
        // 12:00 in Paris, and an agent that copies the local pair cannot
        // write "10h30" for a moment the owner is in a meeting.
        let midday = &free[1];
        assert_eq!(midday.start_local.as_deref(), Some("2026-10-12T12:00:00+02:00"));
        assert_eq!(midday.end_local.as_deref(), Some("2026-10-12T14:00:00+02:00"));
        assert_eq!(midday.minutes, 120);

        // With no zone known, the local pair is absent rather than guessed.
        let without = free_between(&busy, &window, None, None);
        assert!(without.iter().all(|gap| gap.start_local.is_none()));
        assert_eq!(without.len(), free.len());

        // A window entirely taken answers no gap at all, and one entirely
        // free answers itself.
        let whole = vec![Busy {
            start: "2026-10-12T06:00:00Z".to_owned(),
            end: "2026-10-12T18:00:00Z".to_owned(),
        }];
        assert!(free_between(&whole, &window, None, None).is_empty());
        assert_eq!(
            free_between(&[], &window, None, None),
            vec![Free {
                start: "2026-10-12T06:00:00Z".to_owned(),
                end: "2026-10-12T18:00:00Z".to_owned(),
                start_local: None,
                end_local: None,
                minutes: 720,
            }]
        );

        // An interval this cannot read answers **no** gaps rather than gaps
        // that ignore it: a gap the owner is busy in is the one answer worse
        // than no answer.
        let unreadable = vec![Busy {
            start: "not an instant".to_owned(),
            end: "2026-10-12T10:00:00Z".to_owned(),
        }];
        assert!(free_between(&unreadable, &window, None, None).is_empty());
    }

    /// #381: the owner's working day makes a free gap an offer.
    #[test]
    fn the_working_day_clips_the_gaps_and_never_the_occupations() {
        // Friday 18:00 Paris to Monday 10:00 Paris, in one gap — the shape
        // that made a draft offer a Friday at 19:30.
        let window = Window::parse("2026-10-16T12:00:00Z", "2026-10-19T12:00:00Z").unwrap();
        let weekdays = WorkingDay {
            days: vec![1, 2, 3, 4, 5],
            starts_at: "09:00".to_owned(),
            ends_at: "18:00".to_owned(),
        };
        let free = free_between(&[], &window, Some("Europe/Paris"), Some(&weekdays));
        assert_eq!(
            free.iter()
                .map(|gap| (gap.start_local.clone().unwrap(), gap.minutes))
                .collect::<Vec<_>>(),
            vec![
                // Friday, clipped at both ends: the window opens at 14:00
                // local and the day closes at 18:00.
                ("2026-10-16T14:00:00+02:00".to_owned(), 240),
                // Saturday and Sunday are not accepted days and contribute
                // nothing at all.
                ("2026-10-19T09:00:00+02:00".to_owned(), 300),
            ],
            "the weekend is gone and the weekdays are clipped to the amplitude"
        );

        // A gap that straddles an edge is clipped, not dropped: half an hour
        // is a meeting.
        let morning = Window::parse("2026-10-19T05:00:00Z", "2026-10-19T07:30:00Z").unwrap();
        let free = free_between(&[], &morning, Some("Europe/Paris"), Some(&weekdays));
        assert_eq!(free.len(), 1, "{free:?}");
        assert_eq!(free[0].start_local.as_deref(), Some("2026-10-19T09:00:00+02:00"));
        assert_eq!(free[0].minutes, 30);

        // The busy intervals are untouched: "you are busy" is a fact, and a
        // read that trimmed one would be lying about a calendar. The gap
        // around a meeting is what narrows.
        let busy = vec![Busy {
            start: "2026-10-19T09:00:00Z".to_owned(),
            end: "2026-10-19T10:00:00Z".to_owned(),
        }];
        let monday = Window::parse("2026-10-19T05:00:00Z", "2026-10-19T17:00:00Z").unwrap();
        let free = free_between(&busy, &monday, Some("Europe/Paris"), Some(&weekdays));
        assert!(
            free.iter().all(|gap| {
                gap.start.as_str() >= "2026-10-19T07:00:00Z" && gap.end.as_str() <= "2026-10-19T16:00:00Z"
            }),
            "a gap outside the amplitude was offered: {free:?}"
        );
        assert!(
            free.iter().all(|gap| !(gap.start.as_str() < "2026-10-19T10:00:00Z"
                && gap.end.as_str() > "2026-10-19T09:00:00Z")),
            "a gap overlapping the meeting was offered: {free:?}"
        );

        // With no zone, nothing is clipped: an amplitude is a wall clock, and
        // clipping by one without knowing where the owner is would cut the
        // wrong hours.
        assert_eq!(
            free_between(&[], &monday, None, Some(&weekdays)).len(),
            1,
            "a gap was clipped by an amplitude with no zone to read it in"
        );

        // And a spelling the amplitude cannot be read from offers everything
        // rather than a window invented out of half a value.
        let broken = WorkingDay {
            days: vec![1],
            starts_at: "nine".to_owned(),
            ends_at: "18:00".to_owned(),
        };
        assert_eq!(
            free_between(&[], &monday, Some("Europe/Paris"), Some(&broken)).len(),
            1
        );
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
