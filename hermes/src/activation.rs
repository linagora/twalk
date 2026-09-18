//! Persona activation: the user's decision that a persona may read a given
//! network, recorded as a consent decision on that persona and nothing else
//! (ADR 0013).
//!
//! Two things follow from that ADR, and they are the whole of this module.
//!
//! **The runtime learns activation from the bus, not from an API.** It
//! follows `consent.state.changed` from the beginning of the stream, keeps
//! the last decision per (persona, network), and needs no snapshot: persona
//! decisions are a handful over a deployment's life, unlike a contact's.
//! Nothing is activated by default — a persona nobody decided about is
//! paused, because activation never spreads on its own.
//!
//! **A paused persona still runs, and receives nothing.** So the thing
//! activation moves cannot be the process. It is the persona's durable
//! consumer, which the runtime owns: an active persona's consumer is
//! filtered to `inbound.message.received`, and a paused one's is filtered
//! to [`paused_subject`] — a subject inside the stream that no producer in
//! Twalk ever publishes on. The persona process stays up, stays connected,
//! keeps pulling, and is handed nothing. That is ADR 0013 rendered as a
//! consumer configuration rather than as a kill.
//!
//! One consequence is deliberate and worth stating: messages that arrive
//! while a persona is paused are not replayed to it when it is activated
//! again. The user paused it; the messages of the pause are not its
//! business afterwards either.

use std::collections::HashMap;

use serde_json::Value;

pub const CONSENT_CHANGED_TYPE: &str = "fr.linagora.twalk.consent.state.changed.v1";
pub const MESSAGE_RECEIVED_TYPE: &str = "fr.linagora.twalk.inbound.message.received.v1";

/// The contract's consent states, as they appear in `data.new_state`.
pub const GRANTED: &str = "granted";

/// The subject a paused persona's consumer is filtered to.
///
/// It is inside the deployment's namespace, so JetStream accepts it as a
/// filter on the stream, and it is not a contract event type, so nothing
/// publishes there — ever. A consumer filtered to it is a consumer that
/// receives nothing, which is exactly what ADR 0013 asks for, without
/// stopping the process and without the runtime standing between the
/// persona and the bus.
pub fn paused_subject(subject_prefix: &str, persona_id: &str) -> String {
    format!("{subject_prefix}.hermes.paused.{persona_id}")
}

/// One recorded decision about one persona.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaDecision {
    pub persona_id: String,
    /// The networks the decision applies to (the contract's
    /// `data.scope.networks`).
    pub networks: Vec<String>,
    /// `data.new_state`, verbatim: an unknown label is not `granted`, and
    /// that is all this module needs to know.
    pub new_state: String,
}

impl PersonaDecision {
    /// Reads a `consent.state.changed` event, if it is one and if it is
    /// about a persona.
    ///
    /// `None` for a contact's or a network's decision — the same stream
    /// carries all three — and for anything malformed, because a runtime
    /// that guessed at a decision it could not read would be guessing about
    /// consent.
    pub fn parse(event: &Value) -> Option<Self> {
        if event.get("type").and_then(Value::as_str)? != CONSENT_CHANGED_TYPE {
            return None;
        }
        let data = event.get("data")?;
        let subject = data.get("subject")?;
        if subject.get("type").and_then(Value::as_str)? != "persona" {
            return None;
        }
        let networks = data
            .get("scope")?
            .get("networks")?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if networks.is_empty() {
            return None;
        }
        Some(Self {
            persona_id: subject.get("id").and_then(Value::as_str)?.to_owned(),
            networks,
            new_state: data.get("new_state").and_then(Value::as_str)?.to_owned(),
        })
    }
}

/// What the runtime has learned from the consent stream so far: the last
/// decision per (persona, network).
#[derive(Debug, Default, Clone)]
pub struct Activation {
    states: HashMap<String, HashMap<String, String>>,
}

impl Activation {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one decision. Last writer wins per network, which is what
    /// stream order means: the events arrive in the order the user took the
    /// decisions.
    pub fn apply(&mut self, decision: &PersonaDecision) {
        let per_network = self.states.entry(decision.persona_id.clone()).or_default();
        for network in &decision.networks {
            per_network.insert(network.clone(), decision.new_state.clone());
        }
    }

    /// Whether this persona may be handed events at all.
    ///
    /// Active means "granted on at least one network". The runtime's
    /// enforcement is necessarily this coarse: a bus subject carries no
    /// network, so a consumer cannot be filtered to one. Per-network
    /// scoping is a consumer-side decision like the SDK's consent gate, and
    /// it is not enforced here — see `hermes/README.md`.
    pub fn is_active(&self, persona_id: &str) -> bool {
        self.networks_granted(persona_id).next().is_some()
    }

    /// The networks this persona is activated on, sorted: what an operator
    /// reads in the runtime's logs.
    pub fn granted_networks(&self, persona_id: &str) -> Vec<String> {
        let mut networks = self
            .networks_granted(persona_id)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        networks.sort();
        networks
    }

    fn networks_granted<'a>(&'a self, persona_id: &str) -> impl Iterator<Item = &'a str> {
        self.states
            .get(persona_id)
            .into_iter()
            .flat_map(|per_network| {
                per_network
                    .iter()
                    .filter(|(_, state)| state.as_str() == GRANTED)
                    .map(|(network, _)| network.as_str())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decision(subject_type: &str, id: &str, state: &str, networks: &[&str]) -> Value {
        json!({
            "specversion": "1.0",
            "id": "0".repeat(64),
            "source": "gateway://example.com/consent",
            "type": CONSENT_CHANGED_TYPE,
            "time": "2026-09-17T10:05:00Z",
            "subject": id,
            "datacontenttype": "application/json",
            "data": {
                "subject": { "type": subject_type, "id": id },
                "old_state": "unset",
                "new_state": state,
                "scope": { "networks": networks },
                "occurred_at": "2026-09-17T10:05:00Z",
            }
        })
    }

    #[test]
    fn a_persona_decision_is_read_and_a_contacts_is_not() {
        let parsed = PersonaDecision::parse(&decision(
            "persona",
            "assistant",
            "granted",
            &["whatsapp", "signal"],
        ))
        .expect("a persona decision parses");
        assert_eq!(
            parsed,
            PersonaDecision {
                persona_id: "assistant".to_owned(),
                networks: vec!["whatsapp".to_owned(), "signal".to_owned()],
                new_state: "granted".to_owned(),
            }
        );
        assert_eq!(
            PersonaDecision::parse(&decision(
                "contact",
                "@whatsapp_33612345678:example.com",
                "granted",
                &["whatsapp"]
            )),
            None,
            "a contact's decision is on the same stream and is not activation"
        );
        assert_eq!(
            PersonaDecision::parse(&decision("network", "whatsapp", "granted", &["whatsapp"])),
            None
        );
    }

    #[test]
    fn an_event_that_is_not_a_consent_decision_is_not_one() {
        let mut other = decision("persona", "assistant", "granted", &["whatsapp"]);
        other["type"] = json!(MESSAGE_RECEIVED_TYPE);
        assert_eq!(PersonaDecision::parse(&other), None);
        assert_eq!(PersonaDecision::parse(&json!({})), None);
        assert_eq!(PersonaDecision::parse(&json!("not an object")), None);
    }

    #[test]
    fn a_persona_nobody_decided_about_is_paused() {
        let activation = Activation::new();
        assert!(
            !activation.is_active("assistant"),
            "activation never spreads on its own (ADR 0013)"
        );
        assert!(activation.granted_networks("assistant").is_empty());
    }

    #[test]
    fn the_last_decision_per_network_wins() {
        let mut activation = Activation::new();
        for event in [
            decision("persona", "assistant", "granted", &["whatsapp", "signal"]),
            decision("persona", "assistant", "revoked", &["signal"]),
        ] {
            activation.apply(&PersonaDecision::parse(&event).expect("a persona decision"));
        }
        assert!(activation.is_active("assistant"));
        assert_eq!(
            activation.granted_networks("assistant"),
            vec!["whatsapp".to_owned()]
        );

        activation.apply(
            &PersonaDecision::parse(&decision("persona", "assistant", "revoked", &["whatsapp"]))
                .expect("a persona decision"),
        );
        assert!(
            !activation.is_active("assistant"),
            "a persona revoked on every network it had is paused"
        );
    }

    #[test]
    fn one_personas_decision_says_nothing_about_another() {
        let mut activation = Activation::new();
        activation.apply(
            &PersonaDecision::parse(&decision("persona", "assistant", "granted", &["whatsapp"]))
                .expect("a persona decision"),
        );
        assert!(activation.is_active("assistant"));
        assert!(!activation.is_active("watch"));
    }

    #[test]
    fn a_paused_personas_subject_is_inside_the_stream_and_is_no_contract_type() {
        let subject = paused_subject("twalk", "assistant");
        assert_eq!(subject, "twalk.hermes.paused.assistant");
        assert!(
            subject.starts_with("twalk."),
            "the filter has to overlap the stream's subjects"
        );
        assert!(
            !subject.contains(".inbound.")
                && !subject.contains(".outbound.")
                && !subject.contains(".persona.")
                && !subject.contains(".consent.")
                && !subject.contains(".bridge."),
            "nothing may ever publish there: {subject}"
        );
    }
}
