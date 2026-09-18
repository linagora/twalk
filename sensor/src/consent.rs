//! Consent: the data-processing agreement state of a contact or of a whole
//! network (CONTEXT.md). The Companion Gateway is the single writer of
//! consent state (ADR 0006); the Sensor only labels events with the current
//! state, held in an in-memory cache.
//!
//! The cache is filled from two sources, in a strict order (ADR 0010):
//!
//! 1. the Gateway's **snapshot** — the whole current state, and the stream
//!    sequence it reflects — read once at startup over HTTP;
//! 2. the durable `consent.state.changed` consumer, created at the sequence
//!    **after** the one the snapshot named.
//!
//! That order is the whole design. The cache is last-writer-wins, so nothing
//! arbitrates between the two: they are made not to overlap. Everything up to
//! the snapshot's position is in the snapshot; everything after it comes off
//! the stream, in stream order. Never both, never neither — which is what
//! fixes issue #16, where a restarted Sensor resumed its durable consumer at
//! its ack floor, was therefore never told again about decisions it had
//! already applied, and silently relabelled every granted contact `pending`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::network::Network;

pub const CONSENT_CHANGED_TYPE: &str = "fr.linagora.twalk.consent.state.changed.v1";

/// Durable name of the JetStream pull consumer feeding the consent cache:
/// the consumer survives Sensor restarts, so no recorded decision is lost
/// before it has been applied.
pub const CONSENT_CONSUMER: &str = "sensor-consent-state-changed";

/// The Companion Gateway route serving the snapshot, appended to the base URL
/// the Sensor is configured with (`companion-gateway/openapi.yaml`).
pub const SNAPSHOT_PATH: &str = "/api/consent/snapshot";

/// How long one snapshot read may take before it counts as a failure. The
/// first read happens before the sync loop starts, so it is deliberately
/// short: a Gateway that does not answer promptly is a Gateway to retry
/// against in the background, not one to wait for.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

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

    /// Whether this state reduces what the Sensor publishes about the
    /// subject: a `revoked` contact's events keep their identity, labels,
    /// timestamps, references and relations, and carry no content at all —
    /// no body, no excerpt, no media reference (ADR 0012). `pending` is
    /// unchanged: the Sensor labels, and consumers refuse.
    pub fn reduces_publication(&self) -> bool {
        matches!(self, Self::Revoked)
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

/// Who a consent state is about. The contract has a third subject type,
/// `persona` — activating a persona is a consent decision (ADR 0013), but it
/// is not state a sender is labelled by, so the Sensor drops it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentSubject {
    /// One contact, by Matrix user ID.
    Contact(String),
    /// The network's own default, which applies to every contact on it that
    /// has no decision of its own.
    NetworkDefault,
}

/// One (subject, network) of the consent state — the shape the Gateway's
/// snapshot is a list of, and the shape one scoped network of a decision
/// reduces to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentEntry {
    pub subject: ConsentSubject,
    pub network: Network,
    pub state: Consent,
}

impl ConsentEntry {
    /// Parses one `ConsentStateEntry` of the Gateway's snapshot
    /// (`companion-gateway/openapi.yaml`). `None` for an entry this Sensor
    /// cannot label a sender by: a `persona` subject, or a network this
    /// version does not know — both are well-formed answers from a Gateway
    /// that knows more than this build does, and dropping them is how the
    /// two versions stay compatible.
    pub fn parse(entry: &Value) -> Option<Self> {
        let network = Network::from_contract_value(entry.get("network")?.as_str()?)?;
        let state = Consent::from_label(entry.get("state")?.as_str()?);
        let subject = entry.get("subject")?;
        let subject = match subject.get("type")?.as_str()? {
            "contact" => ConsentSubject::Contact(subject.get("id")?.as_str()?.to_owned()),
            "network" => ConsentSubject::NetworkDefault,
            _ => return None,
        };
        Some(Self {
            subject,
            network,
            state,
        })
    }
}

/// One consent decision, parsed from a `consent.state.changed` event: the
/// deltas that follow the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentChange {
    pub subject: ConsentSubject,
    pub new_state: Consent,
    /// The networks the decision applies to (the contract's scope).
    pub networks: Vec<Network>,
}

impl ConsentChange {
    /// Extracts the labelling change from a `consent.state.changed` event.
    /// Returns None when the event cannot label a sender: malformed, scoped
    /// to no network this version knows, or about a persona.
    pub fn parse(event: &Value) -> Option<Self> {
        let data = event.get("data")?;
        let subject = data.get("subject")?;
        let subject = match subject.get("type").and_then(Value::as_str)? {
            "contact" => ConsentSubject::Contact(subject.get("id")?.as_str()?.to_owned()),
            "network" => ConsentSubject::NetworkDefault,
            _ => return None,
        };
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
            subject,
            new_state,
            networks,
        })
    }

    /// What this decision is in the state: one entry per scoped network.
    pub fn entries(&self) -> impl Iterator<Item = ConsentEntry> + '_ {
        self.networks.iter().map(|network| ConsentEntry {
            subject: self.subject.clone(),
            network: *network,
            state: self.new_state,
        })
    }

    /// How the decision names its subject in a log line: the contact's
    /// Matrix ID, or the network whose default it sets.
    pub fn subject_label(&self) -> &str {
        match &self.subject {
            ConsentSubject::Contact(id) => id,
            ConsentSubject::NetworkDefault => "<network default>",
        }
    }
}

/// The Sensor's view of current consent state, keyed by (subject, network):
/// a contact reachable on several networks holds one decision per network,
/// and each published event labels the sender by the network the event
/// arrived on.
///
/// A network also holds a **default**, which applies to every contact on it
/// that has no decision of its own — the user grants a whole network rather
/// than each of hundreds of contacts. The precedence is the Gateway's
/// (`GET /api/consent/effective`): the contact's own decision wins, the
/// network's default answers otherwise, and `pending` is what is left. An
/// absent subject means "never decided", never "revoked".
///
/// In-memory only, and filled in one order: the Gateway's snapshot first,
/// then the durable consumer from the sequence after it (ADR 0010). The
/// Sensor never writes consent state (ADR 0006).
#[derive(Clone, Debug, Default)]
pub struct ConsentCache {
    states: Arc<RwLock<States>>,
}

#[derive(Debug, Default)]
struct States {
    contacts: HashMap<(String, Network), Consent>,
    networks: HashMap<Network, Consent>,
}

impl ConsentCache {
    /// The consent state that applies to a subject on a network: its own
    /// decision, the network's default, or `pending` when neither exists.
    pub fn state(&self, subject_id: &str, network: Network) -> Consent {
        let states = self.read();
        states
            .contacts
            .get(&(subject_id.to_owned(), network))
            .or_else(|| states.networks.get(&network))
            .copied()
            .unwrap_or(Consent::Pending)
    }

    /// Records one consent decision: one state per scoped network,
    /// overwriting the subject's previous state on each.
    pub fn apply(&self, change: &ConsentChange) {
        let mut states = self.write();
        for entry in change.entries() {
            insert(&mut states, entry);
        }
    }

    /// Replaces the whole state with the Gateway's snapshot.
    ///
    /// A replacement and not a merge, because that is what the snapshot is:
    /// every (subject, network) ever decided about, revocations as explicit
    /// as grants. In practice it only ever runs on a cold cache, before the
    /// stream consumer is created — which is precisely why nothing has to
    /// arbitrate between the two.
    pub fn apply_snapshot(&self, snapshot: &ConsentSnapshot) {
        let mut states = self.write();
        *states = States::default();
        for entry in &snapshot.entries {
            insert(&mut states, entry.clone());
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, States> {
        self.states
            .read()
            .expect("the consent cache lock is poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, States> {
        self.states
            .write()
            .expect("the consent cache lock is poisoned")
    }
}

fn insert(states: &mut States, entry: ConsentEntry) {
    match entry.subject {
        ConsentSubject::Contact(id) => {
            states.contacts.insert((id, entry.network), entry.state);
        }
        ConsentSubject::NetworkDefault => {
            states.networks.insert(entry.network, entry.state);
        }
    }
}

/// The Gateway's consent snapshot: the whole current state, and the stream
/// sequence a consumer applying it carries on from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentSnapshot {
    pub entries: Vec<ConsentEntry>,
    /// Where the stream consumer starts — the Gateway's
    /// `next_stream_sequence`, which is `stream_sequence + 1`. Taken as the
    /// Gateway spells it out rather than recomputed here: that off-by-one is
    /// the one mistake that would skip a decision, and it is not this side's
    /// to make.
    pub next_stream_sequence: u64,
}

impl ConsentSnapshot {
    /// The snapshot of a deployment that has decided nothing: start at the
    /// beginning of the stream.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            next_stream_sequence: 1,
        }
    }

    /// Parses the `ConsentSnapshot` document of `companion-gateway/
    /// openapi.yaml`. A document without `next_stream_sequence` is refused
    /// outright rather than guessed at: a snapshot whose position is unknown
    /// cannot be handed off from, and a guess would silently skip or replay
    /// decisions.
    pub fn parse(document: &Value) -> Result<Self> {
        let next_stream_sequence = document
            .get("next_stream_sequence")
            .and_then(Value::as_u64)
            .context(
                "the consent snapshot names no next_stream_sequence: without it there is no \
                 position to follow the stream from",
            )?;
        let entries = document
            .get("entries")
            .and_then(Value::as_array)
            .context("the consent snapshot has no entries array")?;
        let parsed: Vec<ConsentEntry> = entries.iter().filter_map(ConsentEntry::parse).collect();
        if parsed.len() != entries.len() {
            tracing::debug!(
                served = entries.len(),
                applied = parsed.len(),
                "the consent snapshot holds entries this Sensor does not label senders by"
            );
        }
        Ok(Self {
            entries: parsed,
            next_stream_sequence,
        })
    }
}

/// The initial consent snapshot, priming the cache at startup: the durable
/// consumer only delivers decisions recorded after its last ack, so without a
/// snapshot a restarted Sensor relabels every sender `pending` (issue #16).
/// [`GatewaySnapshot`] reads it from the Companion Gateway, the single writer
/// of consent state (ADR 0006); [`NoConsentSnapshot`] is what a deployment
/// without a Gateway — and a test that runs without one — gets instead.
pub trait ConsentSnapshotSource: Send + Sync {
    fn fetch_snapshot(&self) -> impl std::future::Future<Output = Result<ConsentSnapshot>> + Send;
}

/// The empty snapshot: nothing decided, follow the stream from its
/// beginning. For deployments and tests with no Companion Gateway in front
/// of the Sensor.
#[derive(Debug, Default)]
pub struct NoConsentSnapshot;

impl ConsentSnapshotSource for NoConsentSnapshot {
    async fn fetch_snapshot(&self) -> Result<ConsentSnapshot> {
        Ok(ConsentSnapshot::empty())
    }
}

/// The real source: `GET /api/consent/snapshot` on the Companion Gateway.
///
/// Authenticated by a **service token** from the Sensor's own environment,
/// as an `Authorization: Bearer` credential. The Sensor is a service and not
/// one of the owner's browsers: it has no Matrix OpenID token to sign in with
/// and must never be given a device token (ADR 0011). The token is the same
/// secret the Gateway is configured with, and it grants a read of every
/// contact the user ever decided about — so it is never logged, and no error
/// this module raises carries it.
pub struct GatewaySnapshot {
    client: reqwest::Client,
    url: String,
    service_token: String,
}

impl GatewaySnapshot {
    /// `base_url` is the Gateway's origin (e.g. `http://companion-gateway:8080`);
    /// the snapshot route is appended to it.
    pub fn new(base_url: &str, service_token: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(SNAPSHOT_TIMEOUT)
            .build()
            .context("failed to build the Companion Gateway HTTP client")?;
        Ok(Self {
            client,
            url: format!("{}{SNAPSHOT_PATH}", base_url.trim_end_matches('/')),
            service_token: service_token.to_owned(),
        })
    }

    /// Where the snapshot is read from — safe to log, unlike the token.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl ConsentSnapshotSource for GatewaySnapshot {
    async fn fetch_snapshot(&self) -> Result<ConsentSnapshot> {
        let response = self
            .client
            .get(&self.url)
            .bearer_auth(&self.service_token)
            .send()
            .await
            .with_context(|| format!("the Companion Gateway at {} is unreachable", self.url))?;
        let status = response.status();
        if !status.is_success() {
            // The body carries the Gateway's own error code (`unauthenticated`,
            // `consent_not_configured`, `snapshot_too_large`, …), which is
            // what an operator needs to see; it never carries the token.
            let detail = response.text().await.unwrap_or_default();
            let detail: String = detail.chars().take(500).collect();
            anyhow::bail!(
                "the Companion Gateway at {} answered {status} to the consent snapshot: {detail}",
                self.url
            );
        }
        let document: Value = response
            .json()
            .await
            .context("the Companion Gateway's consent snapshot is not JSON")?;
        ConsentSnapshot::parse(&document)
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
    fn only_revocation_reduces_what_is_published() {
        assert!(Consent::Revoked.reduces_publication());
        assert!(!Consent::Granted.reduces_publication());
        assert!(
            !Consent::Pending.reduces_publication(),
            "pending behaviour is unchanged: the Sensor labels, consumers refuse"
        );
    }

    #[test]
    fn unknown_labels_degrade_to_pending() {
        assert_eq!(Consent::from_label("granted"), Consent::Granted);
        assert_eq!(Consent::from_label("revoked"), Consent::Revoked);
        assert_eq!(Consent::from_label("unsure"), Consent::Pending);
    }

    fn contact_change(subject_id: &str, new_state: Consent, networks: &[Network]) -> ConsentChange {
        ConsentChange {
            subject: ConsentSubject::Contact(subject_id.to_owned()),
            new_state,
            networks: networks.to_vec(),
        }
    }

    fn network_change(new_state: Consent, networks: &[Network]) -> ConsentChange {
        ConsentChange {
            subject: ConsentSubject::NetworkDefault,
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
        assert_eq!(
            cache.state("@a:example.com", Network::Whatsapp),
            Consent::Granted
        );
        assert_eq!(
            cache.state("@a:example.com", Network::Telegram),
            Consent::Pending
        );
    }

    #[test]
    fn a_multi_network_decision_applies_to_each_scoped_network() {
        let cache = ConsentCache::default();
        cache.apply(&contact_change(
            "@a:example.com",
            Consent::Granted,
            &[Network::Whatsapp, Network::Telegram],
        ));
        assert_eq!(
            cache.state("@a:example.com", Network::Whatsapp),
            Consent::Granted
        );
        assert_eq!(
            cache.state("@a:example.com", Network::Telegram),
            Consent::Granted
        );
        assert_eq!(
            cache.state("@a:example.com", Network::Signal),
            Consent::Pending
        );
    }

    #[test]
    fn a_new_decision_overwrites_the_subjects_previous_one() {
        let cache = ConsentCache::default();
        cache.apply(&contact_change(
            "@a:example.com",
            Consent::Granted,
            &[Network::Whatsapp],
        ));
        cache.apply(&contact_change(
            "@b:example.com",
            Consent::Granted,
            &[Network::Whatsapp],
        ));
        cache.apply(&contact_change(
            "@a:example.com",
            Consent::Revoked,
            &[Network::Whatsapp],
        ));
        assert_eq!(
            cache.state("@a:example.com", Network::Whatsapp),
            Consent::Revoked
        );
        assert_eq!(
            cache.state("@b:example.com", Network::Whatsapp),
            Consent::Granted,
            "a decision about one subject never touches another"
        );
    }

    #[test]
    fn a_contacts_own_decision_wins_over_the_networks_default() {
        let cache = ConsentCache::default();
        cache.apply(&network_change(Consent::Granted, &[Network::Whatsapp]));
        assert_eq!(
            cache.state("@unknown:example.com", Network::Whatsapp),
            Consent::Granted,
            "a contact with no decision of its own takes the network's default"
        );
        assert_eq!(
            cache.state("@unknown:example.com", Network::Signal),
            Consent::Pending,
            "the default is scoped to its own network"
        );

        cache.apply(&contact_change(
            "@a:example.com",
            Consent::Revoked,
            &[Network::Whatsapp],
        ));
        assert_eq!(
            cache.state("@a:example.com", Network::Whatsapp),
            Consent::Revoked,
            "the contact's own decision overrides the network's default"
        );
        assert_eq!(
            cache.state("@b:example.com", Network::Whatsapp),
            Consent::Granted,
            "and overrides it for that contact only"
        );

        // The default itself is revised like any other decision.
        cache.apply(&network_change(Consent::Revoked, &[Network::Whatsapp]));
        assert_eq!(
            cache.state("@b:example.com", Network::Whatsapp),
            Consent::Revoked
        );
        assert_eq!(
            cache.state("@a:example.com", Network::Whatsapp),
            Consent::Revoked
        );
    }

    fn consent_event(
        subject_type: &str,
        subject_id: &str,
        new_state: &str,
        networks: Value,
    ) -> Value {
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
        assert_eq!(
            change.subject,
            ConsentSubject::Contact("@whatsapp_33612345678:example.com".to_owned())
        );
        assert_eq!(change.new_state, Consent::Granted);
        assert_eq!(change.networks, vec![Network::Whatsapp]);
    }

    #[test]
    fn parses_a_network_default() {
        // A `network` subject is that network's default consent state (the
        // contract renamed the value from `channel` in #43). The snapshot
        // carries those defaults, so the stream has to carry them too — a
        // Sensor that applied the default once and then ignored every
        // revision of it would drift from the Gateway.
        let change = ConsentChange::parse(&consent_event(
            "network",
            "whatsapp",
            "granted",
            json!(["whatsapp"]),
        ))
        .unwrap();
        assert_eq!(change.subject, ConsentSubject::NetworkDefault);
        assert_eq!(change.new_state, Consent::Granted);
        assert_eq!(change.networks, vec![Network::Whatsapp]);
    }

    #[test]
    fn persona_changes_do_not_label_senders() {
        // Activating a persona is a consent decision (ADR 0013) and never a
        // state a sender is labelled by.
        let persona = consent_event("persona", "assistant", "granted", json!(["whatsapp"]));
        assert_eq!(ConsentChange::parse(&persona), None);
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
        let mixed = consent_event(
            "contact",
            "@a:example.com",
            "granted",
            json!(["irc", "sms"]),
        );
        assert_eq!(
            ConsentChange::parse(&mixed).unwrap().networks,
            vec![Network::Sms]
        );
    }

    #[test]
    fn malformed_changes_are_rejected() {
        assert_eq!(ConsentChange::parse(&json!({"unrelated": true})), None);
        let mut no_id = consent_event("contact", "@a:example.com", "granted", json!(["whatsapp"]));
        no_id["data"]["subject"]
            .as_object_mut()
            .unwrap()
            .remove("id");
        assert_eq!(ConsentChange::parse(&no_id), None);
        let mut no_state =
            consent_event("contact", "@a:example.com", "granted", json!(["whatsapp"]));
        no_state["data"]
            .as_object_mut()
            .unwrap()
            .remove("new_state");
        assert_eq!(ConsentChange::parse(&no_state), None);
    }

    /// The Gateway's document, as `companion-gateway/openapi.yaml` describes
    /// it and `consent_snapshot.rs` renders it.
    fn snapshot_document(stream_sequence: u64, entries: Value) -> Value {
        json!({
            "stream": "twalk",
            "subject": "twalk.consent.state.changed.v1",
            "stream_sequence": stream_sequence,
            "next_stream_sequence": stream_sequence + 1,
            "decision_sequence": 7,
            "entries": entries,
        })
    }

    #[test]
    fn a_snapshot_carries_contacts_defaults_and_the_position_to_follow_from() {
        let snapshot = ConsentSnapshot::parse(&snapshot_document(
            41,
            json!([
                {
                    "subject": { "type": "network", "id": "whatsapp" },
                    "network": "whatsapp",
                    "state": "granted",
                    "decided_at": "2026-09-17T10:00:00.000Z",
                    "decision_sequence": 3
                },
                {
                    "subject": { "type": "contact", "id": "@a:example.com" },
                    "network": "whatsapp",
                    "state": "revoked",
                    "decided_at": "2026-09-17T10:01:00.000Z",
                    "decision_sequence": 5
                }
            ]),
        ))
        .unwrap();
        assert_eq!(
            snapshot.next_stream_sequence, 42,
            "the consumer starts after the position the snapshot reflects"
        );
        assert_eq!(
            snapshot.entries,
            vec![
                ConsentEntry {
                    subject: ConsentSubject::NetworkDefault,
                    network: Network::Whatsapp,
                    state: Consent::Granted,
                },
                ConsentEntry {
                    subject: ConsentSubject::Contact("@a:example.com".to_owned()),
                    network: Network::Whatsapp,
                    state: Consent::Revoked,
                },
            ]
        );

        // Applied, the precedence is the Gateway's own.
        let cache = ConsentCache::default();
        cache.apply_snapshot(&snapshot);
        assert_eq!(
            cache.state("@a:example.com", Network::Whatsapp),
            Consent::Revoked
        );
        assert_eq!(
            cache.state("@b:example.com", Network::Whatsapp),
            Consent::Granted
        );
        assert_eq!(
            cache.state("@b:example.com", Network::Signal),
            Consent::Pending
        );
    }

    #[test]
    fn an_empty_snapshot_starts_the_consumer_at_the_beginning_of_the_stream() {
        let snapshot = ConsentSnapshot::parse(&snapshot_document(0, json!([]))).unwrap();
        assert_eq!(snapshot, ConsentSnapshot::empty());
        assert_eq!(snapshot.next_stream_sequence, 1);
    }

    #[test]
    fn a_snapshot_entry_this_sensor_cannot_use_is_dropped_and_the_rest_applies() {
        let snapshot = ConsentSnapshot::parse(&snapshot_document(
            3,
            json!([
                { "subject": { "type": "persona", "id": "assistant" }, "network": "whatsapp",
                  "state": "granted", "decided_at": "2026-09-17T10:00:00.000Z",
                  "decision_sequence": 1 },
                { "subject": { "type": "contact", "id": "@a:example.com" }, "network": "irc",
                  "state": "granted", "decided_at": "2026-09-17T10:00:00.000Z",
                  "decision_sequence": 2 },
                { "subject": { "type": "contact", "id": "@b:example.com" }, "network": "signal",
                  "state": "granted", "decided_at": "2026-09-17T10:00:00.000Z",
                  "decision_sequence": 3 }
            ]),
        ))
        .unwrap();
        assert_eq!(
            snapshot.entries,
            vec![ConsentEntry {
                subject: ConsentSubject::Contact("@b:example.com".to_owned()),
                network: Network::Signal,
                state: Consent::Granted,
            }],
            "a persona subject and an unknown network are dropped, not fatal"
        );
    }

    #[test]
    fn a_snapshot_without_a_position_is_refused() {
        let mut document = snapshot_document(41, json!([]));
        document
            .as_object_mut()
            .unwrap()
            .remove("next_stream_sequence");
        let error = ConsentSnapshot::parse(&document).unwrap_err().to_string();
        assert!(error.contains("next_stream_sequence"), "{error}");
        // And so is an answer that is not a snapshot at all.
        assert!(ConsentSnapshot::parse(&json!({ "error": "unauthenticated" })).is_err());
    }

    #[test]
    fn the_snapshot_url_is_the_gateways_origin_plus_the_route() {
        let source = GatewaySnapshot::new("http://companion-gateway:8080", "t").unwrap();
        assert_eq!(
            source.url(),
            "http://companion-gateway:8080/api/consent/snapshot"
        );
        let trailing = GatewaySnapshot::new("http://companion-gateway:8080/", "t").unwrap();
        assert_eq!(
            trailing.url(),
            "http://companion-gateway:8080/api/consent/snapshot"
        );
    }

    #[tokio::test]
    async fn without_a_gateway_the_cache_stays_cold_and_the_stream_is_read_whole() {
        let snapshot = NoConsentSnapshot.fetch_snapshot().await.unwrap();
        assert!(snapshot.entries.is_empty());
        assert_eq!(snapshot.next_stream_sequence, 1);
    }
}
