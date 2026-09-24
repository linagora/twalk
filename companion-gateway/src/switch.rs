//! A switch the owner decides, kept as a journal rather than a setting.
//!
//! Two of them exist now and they are the same machine. The **disclosure**
//! (#121, ADR 0019, ADR 0031) says whether a persona's reply carries the
//! line that discloses it; the **calendar location** (#354, #351) says
//! whether a calendar event may carry where the meeting is. What they
//! govern has nothing in common. How they are decided has everything:
//!
//! - one decision for the whole deployment, never per contact and never per
//!   message, because a switch that can be flipped for one case is a switch
//!   whose state nobody can state;
//! - taken by the owner and **recorded** — who, when, and optionally why —
//!   rather than kept as a preference, which is ADR 0019's "recorded
//!   deliberate act" and what an append-only journal is for;
//! - and answered as "the last row, or the default", so a deployment that
//!   never touched it has an empty table and a true answer instead of a
//!   seeded row pretending somebody decided.
//!
//! The **default differs**, and that is the only thing each switch decides
//! for itself: the disclosure ships on, the location ships off. A switch's
//! default says what happens to somebody who never read the settings
//! screen, so it belongs to the switch, not here.
//!
//! This module holds the shape and the parsing. Where the rows live is
//! [`crate::store`] — one table per switch, not one table with a kind
//! column, because an append-only journal with a discriminator is a place
//! where one switch's migration can break the other's.

use serde_json::Value;

/// The most a decision's `reason` may carry. Long enough for a sentence
/// somebody will read in a year, short enough not to be an essay in a
/// journal nothing reads back but a screen.
pub const MAX_REASON_CHARS: usize = 1024;

/// A switch as its journal answers it: the last decision, or the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub enabled: bool,
    /// When the last decision was taken, RFC 3339; `None` when none ever
    /// was, which is the default and says "as it shipped".
    pub since: Option<String>,
    /// Who took it: the deployment's owner, as a consent decision's `actor`.
    pub actor: Option<String>,
    pub reason: Option<String>,
}

impl State {
    /// The state a journal with no decision in it holds, for a switch that
    /// ships in `enabled`.
    pub const fn shipped_as(enabled: bool) -> Self {
        Self {
            enabled,
            since: None,
            actor: None,
            reason: None,
        }
    }

    /// The journal's own vocabulary for a state: what `new_state` holds.
    /// The one place the two words are spelled, so the writer and the
    /// reader in [`crate::store`] cannot drift from each other.
    pub fn word(enabled: bool) -> &'static str {
        if enabled {
            "on"
        } else {
            "off"
        }
    }

    /// The reverse: a `new_state` read back, or `None` for a word no
    /// journal writes.
    pub fn enabled_from(word: &str) -> Option<bool> {
        match word {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        }
    }
}

/// One decision as a client states it: `{"enabled": bool, "reason"?: string}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub enabled: bool,
    pub reason: Option<String>,
}

/// What is wrong with a switch request. One code, `malformed_request`, and
/// a sentence naming the member, because every one of these is something
/// the caller can fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(pub String);

impl Invalid {
    pub fn code(&self) -> &'static str {
        "malformed_request"
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

/// What a caller is told `enabled` means for this switch — one line each
/// way, in the imperative, because the sentence is read by whoever sent
/// the wrong body and has to fix it.
#[derive(Debug, Clone, Copy)]
pub struct Meaning {
    /// What this switch is called in a refusal: "a disclosure decision", "a
    /// calendar location decision".
    pub decision: &'static str,
    /// What `true` does, and what `false` does, as one sentence.
    pub enabled: &'static str,
}

/// Parses a switch request. A closed object: `enabled` is required and a
/// boolean, `reason` is optional and at most [`MAX_REASON_CHARS`], and any
/// other member is refused rather than ignored — a client that sent
/// `{"disclosure": false}` should learn that nothing happened.
pub fn parse_update(body: &Value, meaning: Meaning) -> Result<Update, Invalid> {
    let Meaning { decision, enabled } = meaning;
    let object = body.as_object().ok_or_else(|| {
        Invalid("the request body is not a JSON object: send {\"enabled\": true|false}".to_owned())
    })?;
    for member in object.keys() {
        if !matches!(member.as_str(), "enabled" | "reason") {
            return Err(Invalid(format!(
                "the request body has the unknown member {member:?}: {decision} carries enabled \
                 and an optional reason"
            )));
        }
    }
    let enabled = match object.get("enabled") {
        Some(Value::Bool(enabled)) => *enabled,
        Some(_) => return Err(Invalid(format!("enabled is a boolean: {enabled}"))),
        None => return Err(Invalid("enabled is required".to_owned())),
    };
    let reason = match object.get("reason") {
        None | Some(Value::Null) => None,
        Some(Value::String(reason)) => {
            let count = reason.chars().count();
            if count > MAX_REASON_CHARS {
                return Err(Invalid(format!(
                    "reason is {count} characters, and the limit is {MAX_REASON_CHARS}"
                )));
            }
            Some(reason.clone())
        }
        Some(_) => return Err(Invalid("reason is a string when it is present".to_owned())),
    };
    Ok(Update { enabled, reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MEANING: Meaning = Meaning {
        decision: "a test decision",
        enabled: "true opens it, false closes it",
    };

    #[test]
    fn a_default_says_nobody_decided_rather_than_naming_a_decider() {
        // The difference that matters on a screen: "off since the
        // deployment began" is not "somebody turned it off".
        let shipped = State::shipped_as(false);
        assert!(!shipped.enabled);
        assert_eq!(shipped.since, None);
        assert_eq!(shipped.actor, None);
        assert!(State::shipped_as(true).enabled);
    }

    #[test]
    fn the_journals_two_words_survive_the_round_trip() {
        for enabled in [true, false] {
            assert_eq!(State::enabled_from(State::word(enabled)), Some(enabled));
        }
        // A word no journal writes is not silently read as `off`: a state
        // this binary cannot read is a fact, not a default.
        assert_eq!(State::enabled_from("false"), None);
        assert_eq!(State::enabled_from(""), None);
    }

    #[test]
    fn an_unknown_member_is_refused_rather_than_ignored() {
        // The whole point of the closed object: a client that spelled the
        // member after the switch learns nothing happened, instead of
        // getting a 200 and the state it did not ask for.
        let refusal = parse_update(&json!({ "disclosure": false }), MEANING)
            .expect_err("an unknown member is refused");
        assert!(refusal.message().contains("disclosure"), "{refusal:?}");
        assert!(refusal.message().contains("a test decision"), "{refusal:?}");
        assert_eq!(refusal.code(), "malformed_request");

        assert!(
            parse_update(&json!({}), MEANING).is_err(),
            "enabled is required"
        );
        assert!(
            parse_update(&json!({ "enabled": "true" }), MEANING).is_err(),
            "enabled is a boolean, and the string is not it"
        );
        assert!(
            parse_update(&json!("off"), MEANING).is_err(),
            "not an object"
        );
    }

    #[test]
    fn a_reason_is_optional_and_bounded() {
        let bare = parse_update(&json!({ "enabled": true }), MEANING).expect("a bare decision");
        assert_eq!(
            bare,
            Update {
                enabled: true,
                reason: None
            }
        );
        assert_eq!(
            parse_update(&json!({ "enabled": false, "reason": Value::Null }), MEANING)
                .expect("an explicit null")
                .reason,
            None
        );
        let long = "é".repeat(MAX_REASON_CHARS + 1);
        let refusal = parse_update(&json!({ "enabled": false, "reason": long }), MEANING)
            .expect_err("a reason over the limit");
        // Counted in characters, not bytes: the limit a user was told is
        // the limit they are held to, whatever alphabet they wrote in.
        assert!(
            refusal
                .message()
                .contains(&format!("{} characters", MAX_REASON_CHARS + 1)),
            "{refusal:?}"
        );
        assert!(
            parse_update(
                &json!({ "enabled": false, "reason": "é".repeat(MAX_REASON_CHARS) }),
                MEANING
            )
            .is_ok(),
            "the limit itself is allowed"
        );
    }
}
