//! The owner's working day: the days they accept meetings, and how wide those
//! days are (#381).
//!
//! It is a decision of the same family as the two switches in
//! [`crate::switch`] — one for the deployment, taken by the owner, recorded
//! with who and when, answered as "the last row or the default" — and it is
//! not a switch, because what it holds is a span and a set of days rather
//! than a bool. So it has its own module and its own journal, and borrows the
//! reasoning rather than the type.
//!
//! **Why the deployment needs to be told at all.** A free/busy read hands a
//! drafting agent every gap of the window (#379), because hiding the night
//! would be deciding somebody's hours without being told them. Measured on
//! the reference deployment on 2026-09-26, the longest gaps of a week were
//! all nights, and the draft duly offered a Friday at 19:30 — free, correct,
//! and not a time a person offers a colleague. The skill has always said
//! "inside working hours". Nobody had ever said what they are.
//!
//! **An amplitude, not a set of ranges**, in the owner's own words: a start,
//! an end, and which days. Somebody who takes meetings 09:00–12:00 and
//! 14:00–18:00 is describing their lunch, and their calendar already says
//! when they eat.
//!
//! **And one amplitude for the whole week is often false**, which is what #386
//! adds: a short Wednesday, a Friday that ends at 16:00, a Monday that starts
//! late. Before it, the owner's only way to say so was to make the whole week
//! as narrow as its narrowest day, which hides real availability from every
//! draft. So the amplitude above is the **default**, and a weekday may carry an
//! exception of its own.
//!
//! Not a table of seven amplitudes, which was the other shape: it makes the
//! common case seven decisions instead of one, gives a screen seven rows where
//! one sentence would do, and costs the relayed member its one-line reading —
//! a deployment with no exception at all would answer a new shape to say the
//! same thing. Default-plus-exceptions is additive: every reader of the three
//! members above keeps its meaning, and `exceptions` is a member an older
//! reader ignores.
//!
//! A day in `days` with no exception runs the default. **Absence means "as
//! usual", never "no meetings"** — `days` is already where a day is refused,
//! and one member saying two things is how a screen starts lying.
//!
//! **Wall-clock times, never instants.** `09:00` means nine in the morning
//! where the owner is, in summer and in winter both. The zone is the
//! calendar's own (#369), read where the gaps are computed, which is the
//! collector — this module holds no zone and does no arithmetic.
//!
//! **Shipped unset**, which means the deployment has no opinion and every gap
//! is answered exactly as before this existed. There is no default working
//! day, because a default would be a decision about somebody's life taken by
//! whoever wrote the migration.

use std::collections::BTreeMap;

use serde_json::Value;

/// The most days a week has, and the numbers they are named by: ISO weekday,
/// `1` for Monday, `7` for Sunday.
pub const MONDAY: u8 = 1;
pub const SUNDAY: u8 = 7;

/// One day's hours: the amplitude a weekday runs, when it is not the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// `HH:MM`, the local wall clock the day starts at.
    pub starts_at: String,
    /// `HH:MM`, the local wall clock it ends at, strictly after the start.
    pub ends_at: String,
}

/// The owner's working day, as they set it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingDay {
    /// The ISO weekdays they accept meetings on, ascending and without
    /// repeats — kept sorted so that two equal sets are equal values, and a
    /// screen renders them in the order a week runs.
    pub days: Vec<u8>,
    /// `HH:MM`, the local wall clock their day starts at.
    pub starts_at: String,
    /// `HH:MM`, the local wall clock it ends at, strictly after the start.
    pub ends_at: String,
    /// The weekdays that run something other than the default, by ISO number
    /// (#386). Sorted, because a `BTreeMap` is — which is how a screen renders
    /// them in the order a week runs, and how two equal sets are equal values.
    ///
    /// Empty on a deployment that has one amplitude, which is most of them, and
    /// then every answer this module gives is the answer it gave before
    /// exceptions existed.
    ///
    /// Every key is one of [`WorkingDay::days`]: an exception for a day the
    /// owner does not accept meetings on is two statements that contradict each
    /// other, and it is refused by name rather than resolved by a parser.
    pub exceptions: BTreeMap<u8, Span>,
}

impl WorkingDay {
    /// The hours a weekday runs: its own exception, or the default.
    ///
    /// This side's one answer to that question, so that the journal, the two
    /// routes and the screen cannot read one state three ways. The collector
    /// has a function of the same name over its own type — the two crates
    /// share no types by design (ADR 0033: it reads the owner's accounts and
    /// this one holds their decisions) — and what keeps them agreeing is the
    /// shape of the document between them, `exceptions` keyed by weekday, plus
    /// the test on each side.
    pub fn span(&self, weekday: u8) -> (&str, &str) {
        match self.exceptions.get(&weekday) {
            Some(span) => (span.starts_at.as_str(), span.ends_at.as_str()),
            None => (self.starts_at.as_str(), self.ends_at.as_str()),
        }
    }
}

/// The exceptions as one column of the journal: `3=09:00-12:30,5=09:00-16:00`.
///
/// Here rather than in [`crate::store`] because it is a format of this type,
/// and a format written in one module and read in another is a format with two
/// homes and no round trip. `None` when there are none, so a deployment with
/// one amplitude writes the row it wrote before #386 and a reader has nothing
/// to tell apart.
pub fn exceptions_column(exceptions: &BTreeMap<u8, Span>) -> Option<String> {
    (!exceptions.is_empty()).then(|| {
        exceptions
            .iter()
            .map(|(weekday, span)| format!("{weekday}={}-{}", span.starts_at, span.ends_at))
            .collect::<Vec<_>>()
            .join(",")
    })
}

/// That column, read back. An entry this cannot read is **dropped**, and the
/// day it named then runs the default.
///
/// That is the least-wrong of three answers and it is worth saying why:
/// refusing the whole row would answer "no working day at all", which offers
/// the owner's nights; inventing hours for the day is not available; so the day
/// falls back to the amplitude the owner did set, which is the narrowest honest
/// reading of a state this build wrote and can no longer parse.
///
/// "Cannot read" is every rule the write path enforces, and deliberately not a
/// subset of them: a weekday outside `1..=7` (the route answers keys matching
/// `^[1-7]$`), a clock that is not `HH:MM`, and an end that is not after its
/// start. The last one is the reason this list is explicit — an inverted span
/// is two valid clocks, so a reader that checked only their spelling would pass
/// it on, and the day would then be clipped to nothing rather than to the
/// default. A day silently offering no hour at all is exactly the failure the
/// fallback exists to avoid.
pub fn exceptions_from_column(column: Option<&str>) -> BTreeMap<u8, Span> {
    let Some(column) = column else {
        return BTreeMap::new();
    };
    column
        .split(',')
        .filter_map(|entry| {
            let (weekday, hours) = entry.trim().split_once('=')?;
            let weekday: u8 = weekday
                .trim()
                .parse()
                .ok()
                .filter(|day| (MONDAY..=SUNDAY).contains(day))?;
            let (starts_at, ends_at) = hours.trim().split_once('-')?;
            (is_wall_clock(starts_at) && is_wall_clock(ends_at) && ends_at > starts_at).then(|| {
                (
                    weekday,
                    Span {
                        starts_at: starts_at.to_owned(),
                        ends_at: ends_at.to_owned(),
                    },
                )
            })
        })
        .collect()
}

/// The working day as its journal answers it: the last decision, or nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    /// `None` when the owner never said, or said and then cleared it. Both
    /// mean the same to a read — every gap is answered — and they are told
    /// apart by `since`, which is set for a clearing and not for silence.
    pub day: Option<WorkingDay>,
    pub since: Option<String>,
    pub actor: Option<String>,
    pub reason: Option<String>,
}

/// What a `PUT` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// These days, this wide.
    Set {
        day: WorkingDay,
        reason: Option<String>,
    },
    /// No opinion again: every gap is answered, as it shipped. Recorded as a
    /// decision rather than a delete, because "I no longer want to say" is
    /// something the owner did on a day.
    Clear { reason: Option<String> },
}

/// Why a `PUT` was refused, in the vocabulary its answer uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    /// Not a JSON object at all.
    Unreadable(String),
    /// `days` missing, empty, or holding something that is not a weekday.
    Days(String),
    /// A time that is not `HH:MM` on a 24-hour clock.
    Time(String),
    /// The end is not after the start. An amplitude that wraps midnight is a
    /// different thing from a working day and is refused by name rather than
    /// quietly reinterpreted.
    Order(String),
    /// An `exceptions` that is not a map of weekday to hours, or that names a
    /// day the owner does not accept meetings on (#386).
    Exception(String),
    /// A `reason` longer than a journal should carry.
    Reason(usize),
}

impl Invalid {
    pub fn code(&self) -> &'static str {
        match self {
            Invalid::Unreadable(_) => "invalid_request",
            Invalid::Days(_) => "working_day_days_invalid",
            Invalid::Time(_) => "working_day_time_invalid",
            Invalid::Order(_) => "working_day_order_invalid",
            Invalid::Exception(_) => "working_day_exception_invalid",
            Invalid::Reason(_) => "working_day_reason_too_long",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Invalid::Unreadable(detail)
            | Invalid::Days(detail)
            | Invalid::Time(detail)
            | Invalid::Exception(detail) => detail.clone(),
            Invalid::Order(detail) => detail.clone(),
            Invalid::Reason(length) => format!(
                "the reason is {length} characters and the limit is {}",
                crate::switch::MAX_REASON_CHARS
            ),
        }
    }
}

/// `{"days": [1,2,3,4,5], "starts_at": "09:00", "ends_at": "18:00"}`, or
/// `{"days": null}` to say nothing again.
///
/// Refused rather than repaired, every time. A working day silently widened
/// because `18:0` looked close enough to `18:00` is an hour of somebody's
/// evening given away by a parser.
pub fn parse_update(body: &str) -> Result<Update, Invalid> {
    let value: Value = serde_json::from_str(body).map_err(|error| {
        Invalid::Unreadable(format!(
            "the body is not the JSON object this route asks for \
             ({{\"days\", \"starts_at\", \"ends_at\"}}): {error}"
        ))
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| Invalid::Unreadable("the body is JSON but not an object".to_owned()))?;
    let reason = match object.get("reason") {
        None | Some(Value::Null) => None,
        Some(Value::String(reason)) => {
            let reason = reason.trim();
            if reason.chars().count() > crate::switch::MAX_REASON_CHARS {
                return Err(Invalid::Reason(reason.chars().count()));
            }
            (!reason.is_empty()).then(|| reason.to_owned())
        }
        Some(_) => {
            return Err(Invalid::Unreadable(
                "the reason is a string when it is there".to_owned(),
            ))
        }
    };

    // Clearing is `days: null` — the member that says what the decision is
    // about, said to be absent, rather than a second verb in the body.
    if matches!(object.get("days"), Some(Value::Null)) {
        return Ok(Update::Clear { reason });
    }

    let days = object
        .get("days")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Invalid::Days(
                "`days` is the ISO weekdays you accept meetings on — 1 for Monday, 7 for \
                 Sunday — or null to say nothing"
                    .to_owned(),
            )
        })?;
    let mut parsed: Vec<u8> = Vec::new();
    for day in days {
        let day = day
            .as_u64()
            .filter(|day| (u64::from(MONDAY)..=u64::from(SUNDAY)).contains(day))
            .ok_or_else(|| {
                Invalid::Days(format!(
                    "{day} is not a weekday: they run from {MONDAY} (Monday) to {SUNDAY} (Sunday)"
                ))
            })? as u8;
        if !parsed.contains(&day) {
            parsed.push(day);
        }
    }
    if parsed.is_empty() {
        return Err(Invalid::Days(
            "`days` is empty: a week with no day in it accepts no meeting, which is what \
             `days: null` says without pretending to be a working day"
                .to_owned(),
        ));
    }
    parsed.sort_unstable();

    let time = |name: &str| -> Result<String, Invalid> {
        let value = object
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .ok_or_else(|| Invalid::Time(format!("`{name}` is a time of day, as `HH:MM`")))?;
        if !is_wall_clock(value) {
            return Err(Invalid::Time(format!(
                "`{name}` is {value:?}, which is not `HH:MM` on a 24-hour clock"
            )));
        }
        Ok(value.to_owned())
    };
    let starts_at = time("starts_at")?;
    let ends_at = time("ends_at")?;
    // Lexicographic on `HH:MM` is chronological, which is the one thing the
    // format is for — used here and for every exception below.
    let ordered = |starts_at: &str, ends_at: &str, whose: &str| -> Result<(), Invalid> {
        if ends_at <= starts_at {
            return Err(Invalid::Order(format!(
                "{whose} ends at {ends_at} and starts at {starts_at}: an amplitude that ends \
                 before it begins would be a night, and a working day that wraps midnight is a \
                 different thing this route does not hold"
            )));
        }
        Ok(())
    };
    ordered(&starts_at, &ends_at, "the day")?;

    // The days that differ (#386), keyed by ISO weekday because that is what
    // `days` is keyed by and a screen must not have to map one to the other.
    let mut exceptions: BTreeMap<u8, Span> = BTreeMap::new();
    match object.get("exceptions") {
        None | Some(Value::Null) => {}
        Some(Value::Object(given)) => {
            for (weekday, hours) in given {
                let weekday: u8 = weekday
                    .trim()
                    .parse()
                    .ok()
                    .filter(|day| (MONDAY..=SUNDAY).contains(day))
                    .ok_or_else(|| {
                        Invalid::Exception(format!(
                            "`exceptions` is keyed by ISO weekday — \"1\" for Monday, \"7\" for \
                             Sunday — and {weekday:?} is not one"
                        ))
                    })?;
                if !parsed.contains(&weekday) {
                    return Err(Invalid::Exception(format!(
                        "day {weekday} has hours of its own and is not among the days you accept \
                         meetings on: the two say opposite things, and which one you meant is not \
                         this route's to decide. Add it to `days`, or drop the exception."
                    )));
                }
                let hours = hours.as_object().ok_or_else(|| {
                    Invalid::Exception(format!(
                        "day {weekday}'s hours are an object with `starts_at` and `ends_at`, as \
                         the day itself is"
                    ))
                })?;
                let time = |name: &str| -> Result<String, Invalid> {
                    let value = hours
                        .get(name)
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .ok_or_else(|| {
                            Invalid::Time(format!("day {weekday}'s `{name}` is a time of day, as `HH:MM`"))
                        })?;
                    if !is_wall_clock(value) {
                        return Err(Invalid::Time(format!(
                            "day {weekday}'s `{name}` is {value:?}, which is not `HH:MM` on a \
                             24-hour clock"
                        )));
                    }
                    Ok(value.to_owned())
                };
                let span = Span {
                    starts_at: time("starts_at")?,
                    ends_at: time("ends_at")?,
                };
                ordered(&span.starts_at, &span.ends_at, &format!("day {weekday}"))?;
                exceptions.insert(weekday, span);
            }
        }
        Some(_) => {
            return Err(Invalid::Exception(
                "`exceptions` is an object keyed by ISO weekday, or null when every day runs the \
                 same hours"
                    .to_owned(),
            ))
        }
    }
    Ok(Update::Set {
        day: WorkingDay {
            days: parsed,
            starts_at,
            ends_at,
            exceptions,
        },
        reason,
    })
}

/// `HH:MM`, 24-hour, both parts two digits: the only spelling this accepts,
/// so that comparing two of them is comparing two strings.
pub fn is_wall_clock(value: &str) -> bool {
    let Some((hours, minutes)) = value.split_once(':') else {
        return false;
    };
    let two_digits = |part: &str| part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_digit());
    if !two_digits(hours) || !two_digits(minutes) {
        return false;
    }
    matches!(
        (hours.parse::<u8>(), minutes.parse::<u8>()),
        (Ok(0..=23), Ok(0..=59))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_working_day_is_days_and_an_amplitude_and_refused_otherwise() {
        let Ok(Update::Set { day, reason }) = parse_update(
            r#"{"days": [5, 1, 2, 3, 4, 1], "starts_at": "09:00", "ends_at": "18:30",
                "reason": "mes horaires"}"#,
        ) else {
            panic!("a working day was not read");
        };
        assert_eq!(day.days, vec![1, 2, 3, 4, 5], "sorted, and without repeats");
        assert_eq!(day.starts_at, "09:00");
        assert_eq!(day.ends_at, "18:30");
        assert_eq!(reason.as_deref(), Some("mes horaires"));

        assert_eq!(
            parse_update(r#"{"days": null}"#),
            Ok(Update::Clear { reason: None }),
            "saying nothing again is `days: null`, recorded as the decision it is"
        );
    }

    #[test]
    fn nothing_is_repaired_on_the_way_in() {
        // An hour of somebody's evening must not be given away by a parser
        // being helpful.
        for (body, code) in [
            (r#"{"days": [], "starts_at": "09:00", "ends_at": "18:00"}"#, "working_day_days_invalid"),
            (r#"{"days": [0], "starts_at": "09:00", "ends_at": "18:00"}"#, "working_day_days_invalid"),
            (r#"{"days": [8], "starts_at": "09:00", "ends_at": "18:00"}"#, "working_day_days_invalid"),
            (r#"{"days": [1], "starts_at": "9:00", "ends_at": "18:00"}"#, "working_day_time_invalid"),
            (r#"{"days": [1], "starts_at": "18:0", "ends_at": "19:00"}"#, "working_day_time_invalid"),
            (r#"{"days": [1], "starts_at": "24:00", "ends_at": "25:00"}"#, "working_day_time_invalid"),
            (r#"{"days": [1], "starts_at": "09:60", "ends_at": "18:00"}"#, "working_day_time_invalid"),
            (r#"{"days": [1], "starts_at": "09:00"}"#, "working_day_time_invalid"),
            (r#"{"days": [1], "starts_at": "18:00", "ends_at": "09:00"}"#, "working_day_order_invalid"),
            (r#"{"days": [1], "starts_at": "09:00", "ends_at": "09:00"}"#, "working_day_order_invalid"),
            ("not json", "invalid_request"),
            ("[1, 2]", "invalid_request"),
        ] {
            assert_eq!(
                parse_update(body).map(|_| ()).unwrap_err().code(),
                code,
                "{body}"
            );
        }
    }

    #[test]
    fn a_day_may_run_other_hours_and_the_rest_run_the_default() {
        // #386, in the owner's own words: a short Wednesday, said without
        // making the whole week as narrow as it.
        let Ok(Update::Set { day, .. }) = parse_update(
            r#"{"days": [1, 2, 3, 4, 5], "starts_at": "09:00", "ends_at": "18:30",
                "exceptions": {"3": {"starts_at": "09:00", "ends_at": "12:30"}}}"#,
        ) else {
            panic!("a working day with an exception was not read");
        };
        assert_eq!(day.span(3), ("09:00", "12:30"), "the day that differs");
        for usual in [1, 2, 4, 5] {
            assert_eq!(
                day.span(usual),
                ("09:00", "18:30"),
                "day {usual} runs the default: absence means as usual, never no meetings"
            );
        }
        assert_eq!(day.exceptions.len(), 1);

        // A deployment with one amplitude is the same value it was before
        // exceptions existed.
        let Ok(Update::Set { day, .. }) = parse_update(
            r#"{"days": [1, 5], "starts_at": "09:00", "ends_at": "18:30"}"#,
        ) else {
            panic!("a working day was not read");
        };
        assert!(day.exceptions.is_empty());
        assert_eq!(day.span(5), ("09:00", "18:30"));
    }

    #[test]
    fn an_exception_is_refused_rather_than_resolved_when_it_contradicts_the_days() {
        for (body, code) in [
            // A day with hours of its own that the owner does not accept
            // meetings on: two statements that contradict each other, and
            // which one they meant is not this route's to decide.
            (
                r#"{"days": [1, 2], "starts_at": "09:00", "ends_at": "18:00",
                    "exceptions": {"6": {"starts_at": "10:00", "ends_at": "12:00"}}}"#,
                "working_day_exception_invalid",
            ),
            (
                r#"{"days": [1], "starts_at": "09:00", "ends_at": "18:00",
                    "exceptions": {"lundi": {"starts_at": "10:00", "ends_at": "12:00"}}}"#,
                "working_day_exception_invalid",
            ),
            (
                r#"{"days": [1], "starts_at": "09:00", "ends_at": "18:00",
                    "exceptions": {"0": {"starts_at": "10:00", "ends_at": "12:00"}}}"#,
                "working_day_exception_invalid",
            ),
            (
                r#"{"days": [1], "starts_at": "09:00", "ends_at": "18:00",
                    "exceptions": {"1": "09:00-12:00"}}"#,
                "working_day_exception_invalid",
            ),
            (
                r#"{"days": [1], "starts_at": "09:00", "ends_at": "18:00", "exceptions": []}"#,
                "working_day_exception_invalid",
            ),
            // And an exception is held to the same two rules the day is: a
            // wall clock, and an end after its start.
            (
                r#"{"days": [1], "starts_at": "09:00", "ends_at": "18:00",
                    "exceptions": {"1": {"starts_at": "9:00", "ends_at": "12:00"}}}"#,
                "working_day_time_invalid",
            ),
            (
                r#"{"days": [1], "starts_at": "09:00", "ends_at": "18:00",
                    "exceptions": {"1": {"starts_at": "12:00"}}}"#,
                "working_day_time_invalid",
            ),
            (
                r#"{"days": [1], "starts_at": "09:00", "ends_at": "18:00",
                    "exceptions": {"1": {"starts_at": "12:00", "ends_at": "09:00"}}}"#,
                "working_day_order_invalid",
            ),
            (
                r#"{"days": [1], "starts_at": "09:00", "ends_at": "18:00",
                    "exceptions": {"1": {"starts_at": "12:00", "ends_at": "12:00"}}}"#,
                "working_day_order_invalid",
            ),
        ] {
            assert_eq!(
                parse_update(body).map(|_| ()).unwrap_err().code(),
                code,
                "{body}"
            );
        }

        // Saying nothing again clears the exceptions with the day: they are
        // one decision, and the journal holds it whole.
        assert_eq!(
            parse_update(r#"{"days": null, "exceptions": {"3": {"starts_at": "09:00", "ends_at": "12:30"}}}"#),
            Ok(Update::Clear { reason: None }),
            "clearing is clearing, and does not keep half of what was cleared"
        );

        // An exception equal to the default is allowed: the owner may state a
        // day's hours explicitly, and refusing it would be this route having
        // an opinion about how somebody describes their own week.
        assert!(matches!(
            parse_update(
                r#"{"days": [1], "starts_at": "09:00", "ends_at": "18:00",
                    "exceptions": {"1": {"starts_at": "09:00", "ends_at": "18:00"}}}"#
            ),
            Ok(Update::Set { .. })
        ));
    }

    #[test]
    fn the_column_round_trips_and_drops_what_the_write_path_would_have_refused() {
        let day = |exceptions: BTreeMap<u8, Span>| WorkingDay {
            days: vec![1, 2, 3, 4, 5],
            starts_at: "09:00".to_owned(),
            ends_at: "18:30".to_owned(),
            exceptions,
        };
        let span = |starts_at: &str, ends_at: &str| Span {
            starts_at: starts_at.to_owned(),
            ends_at: ends_at.to_owned(),
        };
        let exceptions: BTreeMap<u8, Span> = [(3, span("09:00", "12:30")), (5, span("09:00", "16:00"))]
            .into_iter()
            .collect();
        let column = exceptions_column(&exceptions).expect("a column");
        assert_eq!(column, "3=09:00-12:30,5=09:00-16:00");
        assert_eq!(exceptions_from_column(Some(&column)), exceptions);

        // None, not an empty column: the row a deployment with one amplitude
        // writes is the row it wrote before #386.
        assert_eq!(exceptions_column(&BTreeMap::new()), None);
        assert!(exceptions_from_column(None).is_empty());
        assert!(exceptions_from_column(Some("")).is_empty());

        // And every rule the write path enforces is enforced here too — a
        // reader that checked fewer of them would pass on a span the route
        // would have refused. The inverted one is the reason this is a list:
        // two valid clocks, and a day clipped to nothing instead of to the
        // default.
        let read = exceptions_from_column(Some(
            "3=09:00-12:30,5=bananas,x=09:00-10:00,7=09:00,0=09:00-10:00,9=09:00-10:00,\
             2=18:00-09:00,4=12:00-12:00",
        ));
        assert_eq!(
            read.keys().copied().collect::<Vec<u8>>(),
            vec![3],
            "{read:?}"
        );
        assert_eq!(day(read).span(2), ("09:00", "18:30"), "the default");
    }

    #[test]
    fn a_reason_longer_than_a_journal_should_carry_is_refused() {
        let long = "x".repeat(crate::switch::MAX_REASON_CHARS + 1);
        let body = format!(
            r#"{{"days": [1], "starts_at": "09:00", "ends_at": "18:00", "reason": "{long}"}}"#
        );
        assert_eq!(
            parse_update(&body).map(|_| ()).unwrap_err().code(),
            "working_day_reason_too_long"
        );
    }

    #[test]
    fn a_wall_clock_is_two_digits_a_colon_and_two_digits() {
        for good in ["00:00", "09:05", "23:59", "18:30"] {
            assert!(is_wall_clock(good), "{good}");
        }
        for bad in ["9:05", "09:5", "24:00", "23:60", "0900", "09:00:00", "", "ab:cd"] {
            assert!(!is_wall_clock(bad), "{bad}");
        }
    }
}
