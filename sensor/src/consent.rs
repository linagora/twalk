//! Consent: the data-processing agreement state of a contact or channel
//! (CONTEXT.md). The Companion Gateway is the single writer of consent
//! state (ADR 0006); the Sensor only labels events with the current state,
//! served from an in-memory cache fed by `consent.state.changed` events.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde_json::Value;

use crate::network::Network;

pub const CONSENT_CHANGED_TYPE: &str = "fr.linagora.twalk.consent.state.changed.v1";

/// Durable name of the JetStream pull consumer feeding the consent cache:
/// the consumer survives Sensor restarts, so no recorded decision is lost
/// before it has been applied.
pub const CONSENT_CONSUMER: &str = "sensor-consent-state-changed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    Granted,
    Pending,
    Revoked,
}

impl Consent {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Pending => "pending",
            Self::Revoked => "revoked",
        }
    }

    /// Parses a contract consent label. An unknown label cannot be trusted:
    /// it logs and degrades to `pending`, the safe default.
    pub fn from_label(label: &str) -> Self {
        match label {
            "granted" => Self::Granted,
            "pending" => Self::Pending,
            "revoked" => Self::Revoked,
            other => {
                tracing::warn!(%other, "unknown consent label, treating the state as pending");
                Self::Pending
            }
        }
    }
}

/// One contact-scoped consent decision, parsed from a `consent.state.changed`
/// event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentChange {
    /// Matrix user ID of the contact the decision applies to.
    pub subject_id: String,
    pub new_state: Consent,
    /// The networks the decision applies to (the contract's scope).
    pub networks: Vec<Network>,
}

impl ConsentChange {
    /// Extracts the sender-labelling change from a `consent.state.changed`
    /// event. Returns None when the event cannot label a sender: malformed,
    /// or scoped to a channel or persona subject rather than a contact.
    pub fn parse(event: &Value) -> Option<Self> {
        let data = event.get("data")?;
        let subject = data.get("subject")?;
        if subject.get("type").and_then(Value::as_str) != Some("contact") {
            return None;
        }
        let subject_id = subject.get("id").and_then(Value::as_str)?.to_owned();
        let new_state = Consent::from_label(data.get("new_state")?.as_str()?);
        let networks: Vec<Network> = data
            .get("scope")?
            .get("networks")?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .filter_map(Network::from_contract_value)
            .collect();
        if networks.is_empty() {
            return None;
        }
        Some(Self {
            subject_id,
            new_state,
            networks,
        })
    }
}

/// The Sensor's view of current consent state, keyed by (subject id,
/// network): a contact reachable on several networks holds one decision per
/// network, and each published event labels the sender by the network the
/// event arrived on. Fed by the durable `consent.state.changed` consumer —
/// and, once the Companion Gateway exists, by the initial snapshot fetch
/// below. The Sensor never writes consent state (ADR 0006).
///
/// In-memory only: on a restart the durable consumer resumes from its ack
/// position, so earlier decisions are NOT replayed and senders re-label
/// `pending` until the snapshot fetch exists. Persistence across restarts
/// is deliberately out of scope (ticket 05).
#[derive(Clone, Debug, Default)]
pub struct ConsentCache {
    states: Arc<RwLock<HashMap<(String, Network), Consent>>>,
}

impl ConsentCache {
    /// The current consent state of a subject on a network. A cache miss
    /// labels the sender `pending`, per the contract.
    pub fn state(&self, subject_id: &str, network: Network) -> Consent {
        self.states
            .read()
            .expect("the consent cache lock is poisoned")
            .get(&(subject_id.to_owned(), network))
            .copied()
            .unwrap_or(Consent::Pending)
    }

    /// Records one consent decision: one state per scoped network,
    /// overwriting the subject's previous state on each.
    pub fn apply(&self, change: &ConsentChange) {
        let mut states = self
            .states
            .write()
            .expect("the consent cache lock is poisoned");
        for network in &change.networks {
            states.insert((change.subject_id.clone(), *network), change.new_state);
        }
    }
}

/// The initial consent snapshot, priming the cache at startup: the durable
/// consumer only delivers decisions recorded after its last ack, so without
/// a snapshot a restarted Sensor would re-label every sender `pending`. The
/// Companion Gateway — the single writer of consent state (ADR 0006) — will
/// serve the snapshot; until it exists the Sensor wires in
/// [`NoConsentSnapshot`], whose empty snapshot leaves the cache cold.
pub trait ConsentSnapshotSource: Send + Sync {
    fn fetch_snapshot(&self) -> impl std::future::Future<Output = Vec<ConsentChange>> + Send;
}

/// The no-op snapshot source: no Companion Gateway exists yet.
#[derive(Debug, Default)]
pub struct NoConsentSnapshot;

impl ConsentSnapshotSource for NoConsentSnapshot {
    async fn fetch_snapshot(&self) -> Vec<ConsentChange> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn states_match_the_contract_strings() {
        assert_eq!(Consent::Granted.as_str(), "granted");
        assert_eq!(Consent::Pending.as_str(), "pending");
        assert_eq!(Consent::Revoked.as_str(), "revoked");
    }

    #[test]
    fn unknown_labels_degrade_to_pending() {
        assert_eq!(Consent::from_label("granted"), Consent::Granted);
        assert_eq!(Consent::from_label("revoked"), Consent::Revoked);
        assert_eq!(Consent::from_label("unsure"), Consent::Pending);
    }

    fn contact_change(subject_id: &str, new_state: Consent, networks: &[Network]) -> ConsentChange {
        ConsentChange {
            subject_id: subject_id.to_owned(),
            new_state,
            networks: networks.to_vec(),
        }
    }

    #[test]
    fn a_cache_miss_labels_the_sender_pending() {
        let cache = ConsentCache::default();
        assert_eq!(
            cache.state("@whatsapp_33612345678:example.com", Network::Whatsapp),
            Consent::Pending
        );
    }

    #[test]
    fn a_decision_is_scoped_to_its_networks() {
        let cache = ConsentCache::default();
        cache.apply(&contact_change(
            "@a:example.com",
            Consent::Granted,
            &[Network::Whatsapp],
        ));
        assert_eq!(cache.state("@a:example.com", Network::Whatsapp), Consent::Granted);
        assert_eq!(cache.state("@a:example.com", Network::Telegram), Consent::Pending);
    }

    #[test]
    fn a_multi_network_decision_applies_to_each_scoped_network() {
        let cache = ConsentCache::default();
        cache.apply(&contact_change(
            "@a:example.com",
            Consent::Granted,
            &[Network::Whatsapp, Network::Telegram],
        ));
        assert_eq!(cache.state("@a:example.com", Network::Whatsapp), Consent::Granted);
        assert_eq!(cache.state("@a:example.com", Network::Telegram), Consent::Granted);
        assert_eq!(cache.state("@a:example.com", Network::Signal), Consent::Pending);
    }

    #[test]
    fn a_new_decision_overwrites_the_subjects_previous_one() {
        let cache = ConsentCache::default();
        cache.apply(&contact_change("@a:example.com", Consent::Granted, &[Network::Whatsapp]));
        cache.apply(&contact_change("@b:example.com", Consent::Granted, &[Network::Whatsapp]));
        cache.apply(&contact_change("@a:example.com", Consent::Revoked, &[Network::Whatsapp]));
        assert_eq!(cache.state("@a:example.com", Network::Whatsapp), Consent::Revoked);
        assert_eq!(
            cache.state("@b:example.com", Network::Whatsapp),
            Consent::Granted,
            "a decision about one subject never touches another"
        );
    }

    fn consent_event(subject_type: &str, subject_id: &str, new_state: &str, networks: Value) -> Value {
        json!({
            "type": CONSENT_CHANGED_TYPE,
            "data": {
                "subject": { "type": subject_type, "id": subject_id },
                "old_state": "unset",
                "new_state": new_state,
                "scope": { "networks": networks },
                "occurred_at": "2026-09-17T10:05:00Z"
            }
        })
    }

    #[test]
    fn parses_a_contact_scoped_change() {
        let change = ConsentChange::parse(&consent_event(
            "contact",
            "@whatsapp_33612345678:example.com",
            "granted",
            json!(["whatsapp"]),
        ))
        .unwrap();
        assert_eq!(change.subject_id, "@whatsapp_33612345678:example.com");
        assert_eq!(change.new_state, Consent::Granted);
        assert_eq!(change.networks, vec![Network::Whatsapp]);
    }

    #[test]
    fn channel_and_persona_changes_do_not_label_senders() {
        let persona = consent_event("persona", "assistant", "granted", json!(["whatsapp"]));
        assert_eq!(ConsentChange::parse(&persona), None);
        let channel = consent_event("channel", "family", "granted", json!(["whatsapp"]));
        assert_eq!(ConsentChange::parse(&channel), None);
    }

    #[test]
    fn an_unknown_new_state_degrades_to_pending() {
        let change = ConsentChange::parse(&consent_event(
            "contact",
            "@a:example.com",
            "unsure",
            json!(["whatsapp"]),
        ))
        .unwrap();
        assert_eq!(change.new_state, Consent::Pending);
    }

    #[test]
    fn changes_without_a_known_network_are_rejected() {
        let unknown_only = consent_event("contact", "@a:example.com", "granted", json!(["irc"]));
        assert_eq!(ConsentChange::parse(&unknown_only), None);
        let empty_scope = consent_event("contact", "@a:example.com", "granted", json!([]));
        assert_eq!(ConsentChange::parse(&empty_scope), None);
        // Unknown entries are dropped; the known ones still apply.
        let mixed = consent_event("contact", "@a:example.com", "granted", json!(["irc", "sms"]));
        assert_eq!(ConsentChange::parse(&mixed).unwrap().networks, vec![Network::Sms]);
    }

    #[test]
    fn malformed_changes_are_rejected() {
        assert_eq!(ConsentChange::parse(&json!({"unrelated": true})), None);
        let mut no_id = consent_event("contact", "@a:example.com", "granted", json!(["whatsapp"]));
        no_id["data"]["subject"].as_object_mut().unwrap().remove("id");
        assert_eq!(ConsentChange::parse(&no_id), None);
        let mut no_state = consent_event("contact", "@a:example.com", "granted", json!(["whatsapp"]));
        no_state["data"].as_object_mut().unwrap().remove("new_state");
        assert_eq!(ConsentChange::parse(&no_state), None);
    }
}
