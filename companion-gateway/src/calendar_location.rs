//! Whether a calendar event may carry **where** the meeting is (#354, #351).
//!
//! The owner asked for it, and the reason is a good one: a persona that
//! knows a meeting is in Paris can say when to leave, and one that does not
//! cannot. The field is defensible where a description is not — it is
//! short, it is almost always a place, and it is the one the owner named.
//!
//! It is not free, which is why it is a switch and not a field. A location
//! can be a home address. On an invitation it was written by whoever
//! organised the meeting, who decided nothing about this deployment. And
//! what leaves on the bus reaches whatever model the deployment named on
//! its settings screen (ADR 0012, ADR 0028). So:
//!
//! - **off unless the owner turned it on**, which is the opposite of the
//!   disclosure's default and the only thing this module decides that
//!   [`crate::switch`] does not;
//! - one decision for the whole deployment, recorded as a dated act with
//!   who took it, because it is a decision and not a preference;
//! - and it governs the **location alone**. No description and no
//!   attachment leaves the collector at any setting of it, which is #351's
//!   other half and [#355](https://github.com/linagora/twalk/issues/355)'s
//!   subject: a persona asks what an event carries and is answered with
//!   facts, never with somebody's prose.

use serde_json::Value;

use crate::switch::{Invalid, Meaning, State, Update};

/// The state a store with no decision in it holds: **off**, and nobody
/// decided. A deployment that never opened the settings screen sends no
/// location.
pub const DEFAULT: State = State::shipped_as(false);

/// What `enabled` means here, for the sentence a refusal gives back.
const MEANING: Meaning = Meaning {
    decision: "a calendar location decision",
    enabled: "true lets a calendar event carry where the meeting is, false keeps every \
              location on this machine",
};

/// Parses a calendar location decision: see [`crate::switch::parse_update`].
pub fn parse_update(body: &Value) -> Result<Update, Invalid> {
    crate::switch::parse_update(body, MEANING)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn it_ships_off_and_says_nobody_decided() {
        // The difference from the disclosure, which ships on (ADR 0031).
        // A deployment upgraded into #354 sends no location until somebody
        // opens the switch — the upgrade itself decides nothing.
        assert!(!DEFAULT.enabled);
        assert_eq!(DEFAULT.since, None);
        assert_eq!(DEFAULT.actor, None);
        assert_eq!(DEFAULT.reason, None);
    }

    #[test]
    fn a_refusal_names_this_switch_and_what_the_flag_does() {
        let refusal = parse_update(&json!({ "location": true })).expect_err("an unknown member");
        assert!(
            refusal.message().contains("a calendar location decision"),
            "a caller who spelled the member wrong learns which decision they were making: \
             {refusal:?}"
        );
        let refusal = parse_update(&json!({ "enabled": "yes" })).expect_err("not a boolean");
        assert!(
            refusal.message().contains("where the meeting is"),
            "and what the flag would have done: {refusal:?}"
        );
        assert_eq!(
            parse_update(&json!({ "enabled": true }))
                .expect("a decision")
                .enabled,
            true
        );
    }
}
