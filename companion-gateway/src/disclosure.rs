//! The disclosure: the one sentence every reply a persona drafted carries to
//! the contact, and what this Gateway does with it (ticket #121, ADR 0019,
//! ADR 0031).
//!
//! # What the Gateway does, and what it deliberately does not
//!
//! ADR 0031 closed three of the four homes the sentence could have had, and
//! one of the three was **the Gateway composing it**: no event carries a
//! language, no component detects one, and there is no server-side message
//! catalogue in this repository for a legally significant text. So the
//! sentence is *selected* by whoever knows the language of the reply — the
//! persona, on the SDK path; Hermes's own answer, on the return path of ADR
//! 0032 — and travels as `data.disclosure` on `persona.suggest.produced`, a
//! field of its own and never inside the body. What this Gateway does is
//! **append** it: at approval, `final.body` becomes the body, a newline and
//! the sentence, and the approved event carries the sentence as
//! `data.disclosure` too, so the bus records what the contact received and
//! what part of it was the disclosure.
//!
//! # Whose sentences these are
//!
//! `contracts/disclosure/v1/sentences.json` — the contract's, not this
//! crate's. The file is compiled in with `include_str!` for the reason the
//! connection kinds are: one authority, two readers (the Python SDK is the
//! other), and if a component and the contract disagree, the contract wins.
//! A sentence changes only through a new version directory, never an in-place
//! edit; the tests below pin the five keys to the five languages the
//! Companion ships, because a sixth sentence with no interface behind it, or
//! a sixth interface language with no sentence, is a gap somebody should
//! have to argue for.
//!
//! The Gateway reads the table for one thing only: **Hermes's answer names a
//! language and not a sentence**, because Hermes is outside this deployment
//! and holds no copy of the contract. [`sentence_for`] maps that tag to its
//! primary subtag (`fr-CA` is French) and looks the sentence up; a language
//! outside the five is a named, counted refusal and no suggestion at all,
//! which is ADR 0031's posture on the SDK path carried over: a suggestion
//! that cannot be disclosed is one that should not exist.
//!
//! # The switch
//!
//! Removable **only globally**, never from a single message, and every
//! removal a dated, attributed row in an append-only journal of its own
//! ([`crate::store`]'s `disclosure_decision`): a sibling of the consent
//! journal, sharing its triggers and its `occurred_at`/`actor`/`reason`
//! shape, and deliberately neither the consent journal itself nor the
//! settings table, whose single-row upsert forgets who decided and when.
//! No row means **on**: the disclosure ships on by default and the journal
//! records the decisions to turn it off and back.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_json::Value;

/// The contract's own table: `contracts/disclosure/v1/sentences.json`, one
/// sentence per language tag, verbatim.
pub const SENTENCES_JSON: &str = include_str!("../../contracts/disclosure/v1/sentences.json");

/// The contract's cap on `data.disclosure` (`maxLength: 200` on both
/// schemas). What a reply's body cap is reduced by, plus the newline, so the
/// appended `final.body` never exceeds the contract's 65 536.
pub const MAX_CHARS: usize = 200;

/// The longest a `reason` on a switch decision may be — the consent
/// journal's own cap on the same member.
pub const MAX_REASON_CHARS: usize = 1024;

fn sentences() -> &'static BTreeMap<String, String> {
    static SENTENCES: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    SENTENCES.get_or_init(|| {
        serde_json::from_str(SENTENCES_JSON)
            .expect("contracts/disclosure/v1/sentences.json is a JSON object of strings")
    })
}

/// The language tags the contract holds a sentence for, in the file's own
/// (sorted) order.
pub fn languages() -> Vec<&'static str> {
    sentences().keys().map(String::as_str).collect()
}

/// The primary subtag of a BCP 47 language tag: `fr-CA` is `fr`, `sr-Latn-RS`
/// is `sr`. The region and the script say nothing about which sentence a
/// contact can read.
pub fn primary_subtag(tag: &str) -> &str {
    tag.split('-').next().unwrap_or(tag).trim()
}

/// The sentence for a language tag, by its primary subtag, or `None` when the
/// contract holds none — which the caller turns into a refusal, never into a
/// sentence in another language (ADR 0031).
pub fn sentence_for(tag: &str) -> Option<&'static str> {
    sentences()
        .get(&primary_subtag(tag).to_ascii_lowercase())
        .map(String::as_str)
}

/// Whether a sentence is one of the contract's five, verbatim.
///
/// The check the SDK path needs and the Hermes path does not: on the
/// Hermes path the sentence is *selected here* from a language tag, so it
/// cannot be anything else, but on the SDK path it arrives on the bus as
/// `data.disclosure`, and the schema says only `string, 1..200`. The
/// reference bus has no authentication, so "what a contact is told is one
/// of the contract's own sentences" — the property ADR 0031 exists for, and
/// the reason the model never writes the sentence — holds at approval only
/// if this Gateway asks. Verbatim, not trimmed, not case-folded: a sentence
/// that differs from the contract's by a character is not the contract's.
pub fn is_contract_sentence(candidate: &str) -> bool {
    sentences().values().any(|sentence| sentence == candidate)
}

/// The body as the contact receives it: the reply, then the sentence on a
/// line of its own. After and not before, because a prefix is precisely what
/// a bridge's relay mode does and ADR 0025 rejected that by name.
pub fn append(body: &str, sentence: &str) -> String {
    format!("{body}\n{sentence}")
}

/// The switch as the journal answers it: the last decision, or the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisclosureState {
    pub enabled: bool,
    /// When the last decision was taken, RFC 3339; `None` when none ever
    /// was, which is the default and says "on since the deployment began".
    pub since: Option<String>,
    /// Who took it: the deployment's owner, as a consent decision's `actor`.
    pub actor: Option<String>,
    pub reason: Option<String>,
}

impl DisclosureState {
    /// The state a store with no decision in it holds: on, and nobody
    /// decided.
    pub const DEFAULT: Self = Self {
        enabled: true,
        since: None,
        actor: None,
        reason: None,
    };

    /// The journal's own vocabulary for a state: what `new_state` holds.
    /// The one place the two words are spelled, so the writer and the reader
    /// in [`crate::store`] cannot drift from each other.
    pub fn word(enabled: bool) -> &'static str {
        if enabled {
            "on"
        } else {
            "off"
        }
    }

    /// The reverse: a `new_state` read back, or `None` for a word the
    /// journal never writes.
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
pub struct DisclosureUpdate {
    pub enabled: bool,
    pub reason: Option<String>,
}

/// What is wrong with a switch request. One code, `malformed_request`, and a
/// sentence naming the member, because every one of these is something the
/// caller can fix.
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

/// Parses a switch request. A closed object: `enabled` is required and a
/// boolean, `reason` is optional and at most [`MAX_REASON_CHARS`], and any
/// other member is refused rather than ignored — a client that sent
/// `{"disclosure": false}` should learn that nothing happened.
pub fn parse_update(body: &Value) -> Result<DisclosureUpdate, Invalid> {
    let object = body.as_object().ok_or_else(|| {
        Invalid("the request body is not a JSON object: send {\"enabled\": true|false}".to_owned())
    })?;
    for member in object.keys() {
        if !matches!(member.as_str(), "enabled" | "reason") {
            return Err(Invalid(format!(
                "the request body has the unknown member {member:?}: a disclosure decision \
                 carries enabled and an optional reason"
            )));
        }
    }
    let enabled = match object.get("enabled") {
        Some(Value::Bool(enabled)) => *enabled,
        Some(_) => {
            return Err(Invalid(
                "enabled is a boolean: true appends the disclosure to every approved reply, \
                 false stops it for every reply until it is turned on again"
                    .to_owned(),
            ))
        }
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
    Ok(DisclosureUpdate { enabled, reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_contracts_languages_are_exactly_the_five_the_companion_ships() {
        // Two readers of one table, and a third list in `settings.rs`: this
        // is what keeps them one. A sixth sentence with no interface behind
        // it, or a sixth interface language with no sentence, fails here
        // rather than being discovered by a user.
        let mut companion: Vec<&str> = crate::settings::LANGUAGES
            .iter()
            .map(|language| language.as_str())
            .collect();
        companion.sort_unstable();
        assert_eq!(languages(), companion);
    }

    #[test]
    fn every_sentence_is_one_plain_line_the_contract_allows() {
        for (tag, sentence) in sentences() {
            assert!(!sentence.is_empty(), "{tag}: an empty disclosure");
            assert!(
                sentence.chars().count() <= MAX_CHARS,
                "{tag}: the sentence is longer than the contract's {MAX_CHARS}"
            );
            assert!(
                !sentence.contains('\n'),
                "{tag}: the disclosure is one line, on a line of its own"
            );
            assert_eq!(
                sentence.trim(),
                sentence,
                "{tag}: no leading or trailing blank"
            );
            assert!(
                !sentence.to_lowercase().contains("twalk"),
                "{tag}: the sentence describes the message and not the user's infrastructure \
                 (ADR 0031): {sentence}"
            );
        }
    }

    #[test]
    fn the_sentences_are_the_contracts_own() {
        // The five, verbatim, so that a change to the contract file is a
        // change a reviewer sees here as well.
        assert_eq!(sentence_for("en"), Some("Drafted with my AI assistant."));
        assert_eq!(sentence_for("fr"), Some("Rédigé avec mon assistant IA."));
        assert_eq!(
            sentence_for("it"),
            Some("Scritto con il mio assistente IA.")
        );
        assert_eq!(
            sentence_for("es"),
            Some("Redactado con mi asistente de IA.")
        );
        assert_eq!(
            sentence_for("de"),
            Some("Verfasst mit meinem KI-Assistenten.")
        );
    }

    #[test]
    fn a_refined_tag_is_read_by_its_primary_subtag() {
        assert_eq!(sentence_for("fr-CA"), sentence_for("fr"));
        assert_eq!(sentence_for("de-CH"), sentence_for("de"));
        assert_eq!(sentence_for("sr-Latn-RS"), None, "no Serbian sentence");
        assert_eq!(primary_subtag("pt-BR"), "pt");
        assert_eq!(primary_subtag("en"), "en");
    }

    #[test]
    fn a_language_outside_the_five_has_no_sentence_and_is_not_defaulted() {
        for tag in ["ja", "pt", "pt-BR", "ar", "zh", "", "français"] {
            assert_eq!(sentence_for(tag), None, "{tag:?}");
        }
    }

    #[test]
    fn only_the_contracts_own_sentences_verbatim_are_sentences() {
        for (_, sentence) in sentences() {
            assert!(is_contract_sentence(sentence), "{sentence}");
        }
        for forged in [
            // A persona's own wording, the length of a real sentence.
            "Written by an assistant you can trust.",
            // One character off the contract's: a period lost, a case
            // changed, a blank added. Not the contract's.
            "Rédigé avec mon assistant IA",
            "rédigé avec mon assistant IA.",
            " Rédigé avec mon assistant IA.",
            "Rédigé avec mon assistant IA.\n",
            // The schema refuses an empty member and so does this.
            "",
            // A language tag is what Hermes sends and not what a persona
            // sends: a tag on the bus is not a sentence.
            "fr",
        ] {
            assert!(!is_contract_sentence(forged), "{forged:?}");
        }
    }

    #[test]
    fn the_sentence_goes_after_the_body_on_a_line_of_its_own() {
        assert_eq!(
            append(
                "Pas de problème, à 20h ! 👍",
                "Rédigé avec mon assistant IA."
            ),
            "Pas de problème, à 20h ! 👍\nRédigé avec mon assistant IA."
        );
    }

    #[test]
    fn a_decision_is_a_boolean_and_an_optional_reason() {
        assert_eq!(
            parse_update(&json!({ "enabled": false, "reason": "a test" })),
            Ok(DisclosureUpdate {
                enabled: false,
                reason: Some("a test".to_owned())
            })
        );
        assert_eq!(
            parse_update(&json!({ "enabled": true, "reason": null })),
            Ok(DisclosureUpdate {
                enabled: true,
                reason: None
            })
        );
        for (body, names) in [
            (json!([]), "JSON object"),
            (json!({}), "enabled is required"),
            (json!({ "enabled": "off" }), "enabled is a boolean"),
            (
                json!({ "enabled": false, "reason": 3 }),
                "reason is a string",
            ),
            (
                json!({ "enabled": false, "reason": "x".repeat(1025) }),
                "1024",
            ),
            (json!({ "disclosure": false }), "unknown member"),
        ] {
            let refusal = parse_update(&body).expect_err(&body.to_string());
            assert_eq!(refusal.code(), "malformed_request");
            assert!(
                refusal.message().contains(names),
                "{body}: {}",
                refusal.message()
            );
        }
    }

    #[test]
    fn the_default_is_on_and_nobody_decided() {
        let state = DisclosureState::DEFAULT;
        assert_eq!(
            state,
            DisclosureState {
                enabled: true,
                since: None,
                actor: None,
                reason: None
            }
        );
        assert_eq!(DisclosureState::word(state.enabled), "on");
        assert_eq!(DisclosureState::word(false), "off");
        assert_eq!(DisclosureState::enabled_from("on"), Some(true));
        assert_eq!(DisclosureState::enabled_from("off"), Some(false));
        assert_eq!(DisclosureState::enabled_from("ON"), None);
    }
}
