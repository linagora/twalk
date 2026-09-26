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
//! **Wall-clock times, never instants.** `09:00` means nine in the morning
//! where the owner is, in summer and in winter both. The zone is the
//! calendar's own (#369), read where the gaps are computed, which is the
//! collector — this module holds no zone and does no arithmetic.
//!
//! **Shipped unset**, which means the deployment has no opinion and every gap
//! is answered exactly as before this existed. There is no default working
//! day, because a default would be a decision about somebody's life taken by
//! whoever wrote the migration.

use serde_json::Value;

/// The most days a week has, and the numbers they are named by: ISO weekday,
/// `1` for Monday, `7` for Sunday.
pub const MONDAY: u8 = 1;
pub const SUNDAY: u8 = 7;

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
            Invalid::Reason(_) => "working_day_reason_too_long",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Invalid::Unreadable(detail) | Invalid::Days(detail) | Invalid::Time(detail) => {
                detail.clone()
            }
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
    // format is for.
    if ends_at <= starts_at {
        return Err(Invalid::Order(format!(
            "the day ends at {ends_at} and starts at {starts_at}: an amplitude that ends before \
             it begins would be a night, and a working day that wraps midnight is a different \
             thing this route does not hold"
        )));
    }
    Ok(Update::Set {
        day: WorkingDay {
            days: parsed,
            starts_at,
            ends_at,
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
