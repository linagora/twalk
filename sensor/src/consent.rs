//! Consent: the data-processing agreement state of a contact, or of a whole
//! connection (CONTEXT.md, ADR 0033). The Companion Gateway is the single writer of
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
    /// A connection's own default — the contract's `network` subject type,
    /// scoped to a connection since #270 — which applies to every contact on
    /// that connection that has no decision of its own.
    NetworkDefault,
}

impl ConsentSubject {
    /// The subject of a snapshot entry or of a decision's `data`: the one
    /// ladder both parsers climb, so a `persona` is told apart from a
    /// malformed subject in one place.
    fn parse(subject: &Value) -> Result<Self, Unusable> {
        match subject
            .get("type")
            .and_then(Value::as_str)
            .ok_or(Unusable::Malformed)?
        {
            "contact" => Ok(Self::Contact(
                subject
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or(Unusable::Malformed)?
                    .to_owned(),
            )),
            "network" => Ok(Self::NetworkDefault),
            _ => Err(Unusable::NotAboutASender),
        }
    }
}

/// One (subject, connection) of the consent state — the shape the Gateway's
/// snapshot is a list of, and the shape one scoped connection of a decision
/// reduces to (ADR 0033, #271). The connection is the perimeter: two
/// connections of one network hold two decisions about one contact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentEntry {
    pub subject: ConsentSubject,
    /// The connection's id, as the Gateway's registry spells it.
    pub connection: String,
    pub state: Consent,
}

/// Why an entry or a change was not applied. The two that are defects are
/// counted (`twalk_sensor_consent_refused_total`), because a decision the
/// cache silently did not take is a sender labelled by the wrong state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unusable {
    /// A `persona` subject: activating a persona is a consent decision (ADR
    /// 0013), but it is not state a sender is labelled by. Well-formed, not
    /// a defect, not counted.
    NotAboutASender,
    /// The entry or the scope names no connection (#271) — a Gateway older
    /// than #270, or a producer that still scopes by network. Never read as
    /// the network's connection: that is a guess about the perimeter, and
    /// the Gateway is the one that knows.
    NoConnection,
    /// Missing or mistyped members.
    Malformed,
}

impl Unusable {
    /// What the operator is told, beside the count.
    pub fn explained(&self) -> &'static str {
        match self {
            Unusable::NotAboutASender => "a persona decision never labels a sender",
            Unusable::NoConnection => {
                "a decision scoped by network alone — a Gateway older than #270 — is not read \
                 as its network's connection"
            }
            Unusable::Malformed => "the document is missing a member the contract requires",
        }
    }
}

impl ConsentEntry {
    /// Parses one `ConsentStateEntry` of the Gateway's snapshot
    /// (`companion-gateway/openapi.yaml`). Refused, with the reason, for an
    /// entry this Sensor cannot label a sender by: a `persona` subject, an
    /// entry that names its network but no connection.
    pub fn parse(entry: &Value) -> Result<Self, Unusable> {
        // The subject first, in both parsers: a persona entry is not about a
        // sender whatever else it carries, and must not be counted as one
        // that names no connection.
        let subject = ConsentSubject::parse(entry.get("subject").ok_or(Unusable::Malformed)?)?;
        let connection = entry
            .get("connection")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or(Unusable::NoConnection)?
            .to_owned();
        let state = Consent::from_label(
            entry
                .get("state")
                .and_then(Value::as_str)
                .ok_or(Unusable::Malformed)?,
        );
        Ok(Self {
            subject,
            connection,
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
    /// The connections the decision applies to (the contract's
    /// `scope.connections`, #270).
    pub connections: Vec<String>,
}

impl ConsentChange {
    /// Extracts the labelling change from a `consent.state.changed` event.
    /// Refused, with the reason, when the event cannot label a sender:
    /// malformed, scoped to no connection, or about a persona.
    /// `scope.networks` is not read: a decision that names only its network
    /// is one whose perimeter this Sensor would have to guess.
    pub fn parse(event: &Value) -> Result<Self, Unusable> {
        let data = event.get("data").ok_or(Unusable::Malformed)?;
        let subject = ConsentSubject::parse(data.get("subject").ok_or(Unusable::Malformed)?)?;
        let new_state = Consent::from_label(
            data.get("new_state")
                .and_then(Value::as_str)
                .ok_or(Unusable::Malformed)?,
        );
        let connections: Vec<String> = data
            .get("scope")
            .and_then(|scope| scope.get("connections"))
            .and_then(Value::as_array)
            .ok_or(Unusable::NoConnection)?
            .iter()
            .filter_map(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect();
        if connections.is_empty() {
            return Err(Unusable::NoConnection);
        }
        Ok(Self {
            subject,
            new_state,
            connections,
        })
    }

    /// What this decision is in the state: one entry per scoped connection.
    pub fn entries(&self) -> impl Iterator<Item = ConsentEntry> + '_ {
        self.connections.iter().map(|connection| ConsentEntry {
            subject: self.subject.clone(),
            connection: connection.clone(),
            state: self.new_state,
        })
    }

    /// How the decision names its subject in a log line: the contact's
    /// Matrix ID, or the network whose default it sets.
    pub fn subject_label(&self) -> &str {
        match &self.subject {
            ConsentSubject::Contact(id) => id,
            ConsentSubject::NetworkDefault => "<connection default>",
        }
    }
}

/// The Sensor's view of current consent state, keyed by (subject,
/// connection) (ADR 0033, #271): a contact reachable through several
/// connections holds one decision per connection — two WhatsApp accounts
/// are two perimeters, and a decision about one says nothing about the
/// other — and each published event labels the sender by the connection
/// the event arrived on, the one it is stamped with.
///
/// A connection also holds a **default**, which applies to every contact on
/// it that has no decision of its own — the user grants a whole connection
/// rather than each of hundreds of contacts. The precedence is the
/// Gateway's (`GET /api/consent/effective`): the contact's own decision
/// wins, the connection's default answers otherwise, and `pending` is what
/// is left. An absent subject means "never decided", never "revoked".
///
/// In-memory only, and filled in one order: the Gateway's snapshot first,
/// then the durable consumer from the sequence after it (ADR 0010). The
/// Sensor never writes consent state (ADR 0006).
///
/// The owner has no consent state, and the cache is where that is enforced
/// rather than at every read: a decision about one of the operator's
/// confirmed identities is **refused entry**, whether it arrives in the
/// Gateway's snapshot or on the `consent.state.changed` stream (ADR 0021).
///
/// This is not hypothetical. Before ADR 0018 the operator's own messages
/// were published as a contact's, so they fed the Gateway's pending-contact
/// projection and the user could be offered a decision about their own
/// ghost. A deployment upgraded across that change can still hold such a
/// row, and applying it would label the operator's own traffic with it —
/// `granted`, on a network-wide grant, which is how the operator's own phone
/// number came to be published as a contact's attribute.
///
/// The Sensor can only refuse to *use* such a row; removing it from the
/// Gateway's store is the Gateway's half, and the dropped entry is logged at
/// `warn` so an operator can see that their Gateway still holds one.
///
/// A **bridge's own bot** is refused on exactly the same terms (issue #152).
/// It is not hypothetical either, and for the same reason: until #152 a bot
/// reaching a handler was resolved as a subject and fed the Gateway's
/// pending-contact projection, so a deployment can already hold a row about
/// `@whatsappbot` and offer the user a decision about a robot.
#[derive(Clone, Debug, Default)]
pub struct ConsentCache {
    states: Arc<RwLock<States>>,
    /// The operator, when the deployment named one. `None` is a deployment
    /// that has not been told who its owner is: every subject is a contact,
    /// which is the behaviour that existed before ADR 0018.
    owner: Option<crate::owner::Owner>,
    /// The bridges' own bots, as the deployment named them. Empty is a
    /// deployment that has not named any: every bot is then a contact, which
    /// is the behaviour that existed before issue #152.
    bridge_bots: crate::bridge_bot::BridgeBots,
}

#[derive(Debug, Default)]
struct States {
    contacts: HashMap<(String, String), Consent>,
    defaults: HashMap<String, Consent>,
}

impl ConsentCache {
    /// A cache that knows who the operator is and which accounts are the
    /// bridges' own bots, and therefore that no decision about either may
    /// enter it: neither is a person with a consent state.
    pub fn for_people_only(
        owner: Option<crate::owner::Owner>,
        bridge_bots: crate::bridge_bot::BridgeBots,
    ) -> Self {
        Self {
            states: Arc::default(),
            owner,
            bridge_bots,
        }
    }

    /// Why this subject is not a person the cache may hold a decision about —
    /// or `None` when it is one. An exact match against what the deployment
    /// confirmed, and nothing else: an identity it did not name is a contact,
    /// so failing safe here means keeping the decision.
    fn not_a_person(&self, subject_id: &str) -> Option<&'static str> {
        if self
            .owner
            .as_ref()
            .is_some_and(|owner| owner.is_owner(subject_id))
        {
            return Some("the operator");
        }
        if self.bridge_bots.contains(subject_id) {
            return Some("a bridge's own bot");
        }
        None
    }

    /// The consent state that applies to a subject on a connection: its own
    /// decision there, the connection's default, or `pending` when neither
    /// exists.
    pub fn state(&self, subject_id: &str, connection: &str) -> Consent {
        let states = self.read();
        states
            .contacts
            .get(&(subject_id.to_owned(), connection.to_owned()))
            .or_else(|| states.defaults.get(connection))
            .copied()
            .unwrap_or(Consent::Pending)
    }

    /// Records one consent decision: one state per scoped connection,
    /// overwriting the subject's previous state on each.
    pub fn apply(&self, change: &ConsentChange) {
        let mut states = self.write();
        for entry in change.entries() {
            if self.refuse(&entry) {
                continue;
            }
            insert(&mut states, entry);
        }
    }

    /// Replaces the whole state with the Gateway's snapshot.
    ///
    /// A replacement and not a merge, because that is what the snapshot is:
    /// every (subject, connection) ever decided about, revocations as
    /// explicit as grants. In practice it only ever runs on a cold cache, before the
    /// stream consumer is created — which is precisely why nothing has to
    /// arbitrate between the two.
    pub fn apply_snapshot(&self, snapshot: &ConsentSnapshot) {
        let mut states = self.write();
        *states = States::default();
        for entry in &snapshot.entries {
            if self.refuse(entry) {
                continue;
            }
            insert(&mut states, entry.clone());
        }
    }

    /// Whether this entry is refused entry to the cache — a decision about
    /// somebody who is not a person: one of the operator's confirmed
    /// identities (ADR 0021), or a bridge's own bot (issue #152) — and says
    /// so once, at `warn`, because the row it names should not exist at the
    /// Gateway either.
    fn refuse(&self, entry: &ConsentEntry) -> bool {
        let ConsentSubject::Contact(id) = &entry.subject else {
            return false; // a network default is about a network, not a person
        };
        let Some(who) = self.not_a_person(id) else {
            return false;
        };
        tracing::warn!(
            subject = %id,
            connection = %entry.connection,
            state = %entry.state.as_str(),
            "refusing a consent decision about {who}: it is not a contact and has no consent \
             state (ADR 0021, issue #152). The Companion Gateway should not be holding this row"
        );
        true
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
            states.contacts.insert((id, entry.connection), entry.state);
        }
        ConsentSubject::NetworkDefault => {
            states.defaults.insert(entry.connection, entry.state);
        }
    }
}

/// The Gateway's consent snapshot: the whole current state, and the stream
/// sequence a consumer applying it carries on from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentSnapshot {
    pub entries: Vec<ConsentEntry>,
    /// The entries the Gateway served that this Sensor could not label a
    /// sender by, and why — `NoConnection` is the one an operator must hear
    /// about: a Gateway older than #270 serves a state this Sensor will not
    /// guess the perimeter of, and every sender in it stays `pending`.
    pub unusable: Vec<Unusable>,
    /// The registry of connections the Gateway serves with the snapshot
    /// (ADR 0033, #269) — `None` for a deployment with no Gateway, which has
    /// the implicit registry, one connection per network named after it.
    pub connections: Option<crate::connection::Registry>,
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
            unusable: Vec::new(),
            connections: None,
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
        let mut parsed = Vec::new();
        let mut unusable = Vec::new();
        for entry in entries {
            match ConsentEntry::parse(entry) {
                Ok(entry) => parsed.push(entry),
                Err(why) => unusable.push(why),
            }
        }
        Ok(Self {
            entries: parsed,
            unusable,
            connections: Some(crate::connection::Registry::from_snapshot(document)),
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

    fn contact_change(subject_id: &str, new_state: Consent, connections: &[&str]) -> ConsentChange {
        ConsentChange {
            subject: ConsentSubject::Contact(subject_id.to_owned()),
            new_state,
            connections: connections.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    fn default_change(new_state: Consent, connections: &[&str]) -> ConsentChange {
        ConsentChange {
            subject: ConsentSubject::NetworkDefault,
            new_state,
            connections: connections.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    /// A cache belonging to a deployment whose operator is `@michel`, with
    /// one confirmed WhatsApp ghost, and which runs the two bridges of the
    /// reference deployment.
    fn cache_with_an_owner() -> ConsentCache {
        ConsentCache::for_people_only(
            Some(crate::owner::Owner::new(
                "@michel:example.com",
                ["@whatsapp_33660469852:example.com".to_owned()],
            )),
            crate::bridge_bot::BridgeBots::new([
                "@whatsappbot:example.com".to_owned(),
                "@signalbot:example.com".to_owned(),
            ]),
        )
    }

    #[test]
    fn a_decision_about_the_operator_is_refused_entry() {
        // The owner is never a contact and has no consent state (ADR 0021).
        // A row about one of their identities is a row that should not
        // exist — before ADR 0018 the user's own messages fed the Gateway's
        // pending-contact projection, so an upgraded deployment can still
        // hold one — and applying it is how the operator's own traffic came
        // to be labelled by a decision nobody should ever have been offered.
        let cache = cache_with_an_owner();
        cache.apply(&contact_change(
            "@whatsapp_33660469852:example.com",
            Consent::Granted,
            &["whatsapp"],
        ));
        cache.apply(&contact_change(
            "@michel:example.com",
            Consent::Revoked,
            &["whatsapp"],
        ));
        assert_eq!(
            cache.state("@whatsapp_33660469852:example.com", "whatsapp"),
            Consent::Pending,
            "nothing was stored about the ghost"
        );
        assert_eq!(
            cache.state("@michel:example.com", "whatsapp"),
            Consent::Pending,
            "nor about the operator's own Matrix ID, which is always one of their identities"
        );
        // Whatever the connection (#271): the refusal is about who the
        // subject is, and an id that is not the network's name changes
        // nothing about that.
        cache.apply(&contact_change(
            "@whatsapp_33660469852:example.com",
            Consent::Granted,
            &["wa-work"],
        ));
        assert_eq!(
            cache.state("@whatsapp_33660469852:example.com", "wa-work"),
            Consent::Pending
        );
    }

    #[test]
    fn a_snapshot_entry_about_the_operator_is_refused_entry() {
        let cache = cache_with_an_owner();
        cache.apply_snapshot(&ConsentSnapshot {
            connections: None,
            unusable: vec![],
            entries: vec![
                ConsentEntry {
                    subject: ConsentSubject::Contact(
                        "@whatsapp_33660469852:example.com".to_owned(),
                    ),
                    connection: "whatsapp".to_owned(),
                    state: Consent::Granted,
                },
                ConsentEntry {
                    subject: ConsentSubject::Contact(
                        "@whatsapp_33612345678:example.com".to_owned(),
                    ),
                    connection: "whatsapp".to_owned(),
                    state: Consent::Granted,
                },
            ],
            next_stream_sequence: 12,
        });
        assert_eq!(
            cache.state("@whatsapp_33660469852:example.com", "whatsapp"),
            Consent::Pending
        );
        assert_eq!(
            cache.state("@whatsapp_33612345678:example.com", "whatsapp"),
            Consent::Granted,
            "and a real contact in the same snapshot is unaffected: this refuses one subject, \
             not the snapshot"
        );
    }

    #[test]
    fn a_decision_about_a_bridge_bot_is_refused_entry() {
        // A bridge bot is not a person, so there is no decision to hold about
        // it, on the stream or in the snapshot (issue #152). The row is not
        // hypothetical: until #152 a bot reaching a handler was resolved as a
        // subject and fed the Gateway's pending-contact projection, so a
        // deployment can be holding one and offering the user a decision
        // about a robot.
        let cache = cache_with_an_owner();
        cache.apply(&contact_change(
            "@whatsappbot:example.com",
            Consent::Granted,
            &["whatsapp"],
        ));
        cache.apply_snapshot(&ConsentSnapshot {
            connections: None,
            unusable: vec![],
            entries: vec![
                ConsentEntry {
                    subject: ConsentSubject::Contact("@signalbot:example.com".to_owned()),
                    connection: "signal".to_owned(),
                    state: Consent::Granted,
                },
                ConsentEntry {
                    subject: ConsentSubject::Contact(
                        "@signal_75af9e9a-b173-4fa0-9228-03d4a03a1e2c:example.com".to_owned(),
                    ),
                    connection: "signal".to_owned(),
                    state: Consent::Granted,
                },
            ],
            next_stream_sequence: 12,
        });
        assert_eq!(
            cache.state("@whatsappbot:example.com", "whatsapp"),
            Consent::Pending,
            "nothing was stored about the bot the stream named"
        );
        assert_eq!(
            cache.state("@signalbot:example.com", "signal"),
            Consent::Pending,
            "nor about the one the snapshot named"
        );
        assert_eq!(
            cache.state(
                "@signal_75af9e9a-b173-4fa0-9228-03d4a03a1e2c:example.com",
                "signal"
            ),
            Consent::Granted,
            "and a ghost of the same bridge — a person the bridge stands in for — keeps its \
             decision: this refuses one subject, not a network"
        );
    }

    #[test]
    fn an_unnamed_account_that_looks_like_a_bot_keeps_its_decision() {
        // Unknown is not a bot, exactly as unknown is not the operator. The
        // failure to avoid here is the other one: a contact suppressed on a
        // resemblance vanishes from the bus in silence.
        let cache = cache_with_an_owner();
        for lookalike in [
            "@whatsappbot2:example.com",
            "@whatsappbot:evil.example",
            "@telegrambot:example.com",
        ] {
            cache.apply(&contact_change(lookalike, Consent::Granted, &["whatsapp"]));
            assert_eq!(
                cache.state(lookalike, "whatsapp"),
                Consent::Granted,
                "{lookalike} is not one of the bots the deployment named"
            );
        }
    }

    #[test]
    fn an_unconfirmed_identity_keeps_its_decision() {
        // Unknown is not the operator, exactly as unknown is not consent. A
        // ghost of the same shape, one digit apart, is a contact and their
        // decision is theirs.
        let cache = cache_with_an_owner();
        cache.apply(&contact_change(
            "@whatsapp_33660469853:example.com",
            Consent::Granted,
            &["whatsapp"],
        ));
        assert_eq!(
            cache.state("@whatsapp_33660469853:example.com", "whatsapp"),
            Consent::Granted
        );
    }

    #[test]
    fn a_network_default_is_about_a_network_and_is_never_refused() {
        // The network default is not a decision about a person, so there is
        // no owner in it to refuse — but it *is* what used to reach the
        // operator, because `state` falls back to it when a subject has no
        // row. Refusing the owner's own rows is not enough on its own, which
        // is why the producers no longer consult this cache for them at all.
        let cache = cache_with_an_owner();
        cache.apply(&default_change(Consent::Granted, &["whatsapp"]));
        assert_eq!(
            cache.state("@whatsapp_33612345678:example.com", "whatsapp"),
            Consent::Granted
        );
        assert_eq!(
            cache.state("@whatsapp_33660469852:example.com", "whatsapp"),
            Consent::Granted,
            "the fallback still answers for the operator's ghost, which is exactly how their \
             own reaction came to be labelled `granted` — the fix is that no producer asks"
        );
    }

    #[test]
    fn a_deployment_with_no_operator_refuses_nothing() {
        let cache = ConsentCache::default();
        cache.apply(&contact_change(
            "@whatsapp_33660469852:example.com",
            Consent::Granted,
            &["whatsapp"],
        ));
        assert_eq!(
            cache.state("@whatsapp_33660469852:example.com", "whatsapp"),
            Consent::Granted
        );
    }

    #[test]
    fn a_cache_miss_labels_the_sender_pending() {
        let cache = ConsentCache::default();
        assert_eq!(
            cache.state("@whatsapp_33612345678:example.com", "whatsapp"),
            Consent::Pending
        );
    }

    #[test]
    fn a_decision_is_scoped_to_its_connections() {
        let cache = ConsentCache::default();
        cache.apply(&contact_change(
            "@a:example.com",
            Consent::Granted,
            &["whatsapp"],
        ));
        assert_eq!(cache.state("@a:example.com", "whatsapp"), Consent::Granted);
        assert_eq!(cache.state("@a:example.com", "telegram"), Consent::Pending);
    }

    #[test]
    fn a_multi_network_decision_applies_to_each_scoped_network() {
        let cache = ConsentCache::default();
        cache.apply(&contact_change(
            "@a:example.com",
            Consent::Granted,
            &["whatsapp", "telegram"],
        ));
        assert_eq!(cache.state("@a:example.com", "whatsapp"), Consent::Granted);
        assert_eq!(cache.state("@a:example.com", "telegram"), Consent::Granted);
        assert_eq!(cache.state("@a:example.com", "signal"), Consent::Pending);
    }

    #[test]
    fn a_new_decision_overwrites_the_subjects_previous_one() {
        let cache = ConsentCache::default();
        cache.apply(&contact_change(
            "@a:example.com",
            Consent::Granted,
            &["whatsapp"],
        ));
        cache.apply(&contact_change(
            "@b:example.com",
            Consent::Granted,
            &["whatsapp"],
        ));
        cache.apply(&contact_change(
            "@a:example.com",
            Consent::Revoked,
            &["whatsapp"],
        ));
        assert_eq!(cache.state("@a:example.com", "whatsapp"), Consent::Revoked);
        assert_eq!(
            cache.state("@b:example.com", "whatsapp"),
            Consent::Granted,
            "a decision about one subject never touches another"
        );
    }

    #[test]
    fn a_contacts_own_decision_wins_over_the_connections_default() {
        let cache = ConsentCache::default();
        cache.apply(&default_change(Consent::Granted, &["whatsapp"]));
        assert_eq!(
            cache.state("@unknown:example.com", "whatsapp"),
            Consent::Granted,
            "a contact with no decision of its own takes the network's default"
        );
        assert_eq!(
            cache.state("@unknown:example.com", "signal"),
            Consent::Pending,
            "the default is scoped to its own connection"
        );

        cache.apply(&contact_change(
            "@a:example.com",
            Consent::Revoked,
            &["whatsapp"],
        ));
        assert_eq!(
            cache.state("@a:example.com", "whatsapp"),
            Consent::Revoked,
            "the contact's own decision overrides the network's default"
        );
        assert_eq!(
            cache.state("@b:example.com", "whatsapp"),
            Consent::Granted,
            "and overrides it for that contact only"
        );

        // The default itself is revised like any other decision.
        cache.apply(&default_change(Consent::Revoked, &["whatsapp"]));
        assert_eq!(cache.state("@b:example.com", "whatsapp"), Consent::Revoked);
        assert_eq!(cache.state("@a:example.com", "whatsapp"), Consent::Revoked);
    }

    /// A `consent.state.changed` as the Gateway publishes it since #270:
    /// the scope's connections, and their kinds beside them for consumers
    /// not yet migrated — which this Sensor no longer is.
    fn consent_event(
        subject_type: &str,
        subject_id: &str,
        new_state: &str,
        connections: Value,
    ) -> Value {
        json!({
            "type": CONSENT_CHANGED_TYPE,
            "data": {
                "subject": { "type": subject_type, "id": subject_id },
                "old_state": "unset",
                "new_state": new_state,
                "scope": { "connections": connections, "networks": ["whatsapp"] },
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
        assert_eq!(change.connections, vec!["whatsapp"]);
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
        assert_eq!(change.connections, vec!["whatsapp"]);
    }

    #[test]
    fn persona_changes_do_not_label_senders() {
        // Activating a persona is a consent decision (ADR 0013) and never a
        // state a sender is labelled by.
        let persona = consent_event("persona", "assistant", "granted", json!(["whatsapp"]));
        assert_eq!(
            ConsentChange::parse(&persona),
            Err(Unusable::NotAboutASender)
        );
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
    fn a_change_scoped_by_network_alone_is_refused_and_never_read_as_the_networks_connection() {
        // A producer older than #270, or one that still scopes by network:
        // the perimeter is not this Sensor's to guess (#271). The reason is
        // what the metric counts.
        let mut networks_only =
            consent_event("contact", "@a:example.com", "granted", json!(["whatsapp"]));
        networks_only["data"]["scope"]
            .as_object_mut()
            .unwrap()
            .remove("connections");
        assert_eq!(
            ConsentChange::parse(&networks_only),
            Err(Unusable::NoConnection)
        );
        let empty_scope = consent_event("contact", "@a:example.com", "granted", json!([]));
        assert_eq!(
            ConsentChange::parse(&empty_scope),
            Err(Unusable::NoConnection)
        );
        // An id is opaque: whatever the registry named is applied as is.
        let two = consent_event(
            "contact",
            "@a:example.com",
            "granted",
            json!(["wa-home", "wa-work"]),
        );
        assert_eq!(
            ConsentChange::parse(&two).unwrap().connections,
            vec!["wa-home", "wa-work"]
        );
    }

    #[test]
    fn malformed_changes_are_rejected() {
        assert_eq!(
            ConsentChange::parse(&json!({"unrelated": true})),
            Err(Unusable::Malformed)
        );
        let mut no_id = consent_event("contact", "@a:example.com", "granted", json!(["whatsapp"]));
        no_id["data"]["subject"]
            .as_object_mut()
            .unwrap()
            .remove("id");
        assert_eq!(ConsentChange::parse(&no_id), Err(Unusable::Malformed));
        let mut no_state =
            consent_event("contact", "@a:example.com", "granted", json!(["whatsapp"]));
        no_state["data"]
            .as_object_mut()
            .unwrap()
            .remove("new_state");
        assert_eq!(ConsentChange::parse(&no_state), Err(Unusable::Malformed));
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
                    "connection": "whatsapp",
                    "network": "whatsapp",
                    "state": "granted",
                    "decided_at": "2026-09-17T10:00:00.000Z",
                    "decision_sequence": 3
                },
                {
                    "subject": { "type": "contact", "id": "@a:example.com" },
                    "connection": "whatsapp",
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
                    connection: "whatsapp".to_owned(),
                    state: Consent::Granted,
                },
                ConsentEntry {
                    subject: ConsentSubject::Contact("@a:example.com".to_owned()),
                    connection: "whatsapp".to_owned(),
                    state: Consent::Revoked,
                },
            ]
        );

        // Applied, the precedence is the Gateway's own.
        let cache = ConsentCache::default();
        cache.apply_snapshot(&snapshot);
        assert_eq!(cache.state("@a:example.com", "whatsapp"), Consent::Revoked);
        assert_eq!(cache.state("@b:example.com", "whatsapp"), Consent::Granted);
        assert_eq!(cache.state("@b:example.com", "signal"), Consent::Pending);
    }

    #[test]
    fn an_empty_snapshot_starts_the_consumer_at_the_beginning_of_the_stream() {
        let snapshot = ConsentSnapshot::parse(&snapshot_document(0, json!([]))).unwrap();
        assert_eq!(snapshot.entries, ConsentSnapshot::empty().entries);
        assert_eq!(snapshot.next_stream_sequence, 1);
        // A Gateway's snapshot hands a registry over, even an old Gateway's
        // — read as the implicit one (#269); a deployment with no Gateway
        // has none to hand.
        assert!(snapshot.connections.is_some());
        assert!(ConsentSnapshot::empty().connections.is_none());
    }

    #[test]
    fn a_snapshot_entry_this_sensor_cannot_use_is_dropped_counted_and_the_rest_applies() {
        // A persona subject is well-formed and not a sender's state; an
        // entry with a network and no connection is a Gateway older than
        // #270, refused and counted rather than read as the network's
        // connection (#271); the rest applies.
        let snapshot = ConsentSnapshot::parse(&snapshot_document(
            3,
            json!([
                { "subject": { "type": "persona", "id": "assistant" }, "connection": "whatsapp",
                  "network": "whatsapp", "state": "granted",
                  "decided_at": "2026-09-17T10:00:00.000Z", "decision_sequence": 1 },
                { "subject": { "type": "contact", "id": "@a:example.com" }, "network": "whatsapp",
                  "state": "granted", "decided_at": "2026-09-17T10:00:00.000Z",
                  "decision_sequence": 2 },
                { "subject": { "type": "contact", "id": "@b:example.com" }, "connection": "signal",
                  "network": "signal", "state": "granted",
                  "decided_at": "2026-09-17T10:00:00.000Z", "decision_sequence": 3 }
            ]),
        ))
        .unwrap();
        assert_eq!(
            snapshot.entries,
            vec![ConsentEntry {
                subject: ConsentSubject::Contact("@b:example.com".to_owned()),
                connection: "signal".to_owned(),
                state: Consent::Granted,
            }],
            "a persona subject and an entry without a connection are dropped, not fatal"
        );
        assert_eq!(
            snapshot.unusable,
            vec![Unusable::NotAboutASender, Unusable::NoConnection],
            "and each says why, so the one that matters is counted"
        );
    }

    #[test]
    fn two_connections_of_one_network_hold_two_decisions_about_one_contact() {
        // The whole reason the perimeter exists (ADR 0033): a decision on
        // the work account says nothing about the home one, and a default
        // on one connection is that connection's alone.
        let cache = ConsentCache::default();
        cache.apply(&contact_change(
            "@a:example.com",
            Consent::Granted,
            &["wa-work"],
        ));
        cache.apply(&default_change(Consent::Revoked, &["wa-home"]));
        assert_eq!(cache.state("@a:example.com", "wa-work"), Consent::Granted);
        assert_eq!(cache.state("@a:example.com", "wa-home"), Consent::Revoked);
        assert_eq!(cache.state("@b:example.com", "wa-work"), Consent::Pending);
        assert_eq!(cache.state("@b:example.com", "wa-home"), Consent::Revoked);
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
