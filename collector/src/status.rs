//! What a connection says about itself: `connection.status.changed.v1`
//! (issue #274, ADR 0033), published at start and on every transition.
//!
//! The state machine is small and the words are the point. Four states —
//! `connected`, `unreachable`, `reconnect_required`, `pending_operator` —
//! and each one but the first carries a `hint`: the operator's next step,
//! in a sentence, on the bus, so a dashboard can show it without reading the
//! log. The two refusals stay two (see `oidc.rs`): a grant that is gone and a
//! client that lacks what a service expects are different things to do.
//!
//! The first publication of a run has `from_state: unknown` — what came
//! before is not this process's to remember, and a consumer reads the first
//! event of a run as the state rather than as a change. After that, only a
//! transition is published: the same state observed again is silence, and
//! the id (`sha256(connection:to_state:occurred_at)`) makes a republished
//! transition a duplicate the bus drops.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const STATUS_CHANGED_TYPE: &str = "fr.linagora.twalk.connection.status.changed.v1";
pub const STATUS_CHANGED_DATASCHEMA: &str =
    "https://schemas.twalk.dev/cloudevents/v1/connection.status.changed.schema.json";

/// The bus subject of a contract type: `twalk.<type minus the prefix>`.
pub fn bus_subject(event_type: &str) -> String {
    let rest = event_type
        .strip_prefix("fr.linagora.twalk.")
        .expect("contract event types always carry the fr.linagora.twalk prefix");
    format!("twalk.{rest}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Connected,
    Unreachable,
    ReconnectRequired,
    PendingOperator,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Connected => "connected",
            State::Unreachable => "unreachable",
            State::ReconnectRequired => "reconnect_required",
            State::PendingOperator => "pending_operator",
        }
    }
}

/// One observation of a connection's state, with what the operator does
/// about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub state: State,
    /// Which service it is about, when one is: `sso`, `jmap`, `caldav`.
    pub service: Option<&'static str>,
    /// The operator's next step. Required for every state but `connected`.
    pub hint: Option<String>,
}

impl Observation {
    pub fn connected() -> Self {
        Self {
            state: State::Connected,
            service: None,
            hint: None,
        }
    }
}

/// A connection's last published state, so that only transitions publish.
#[derive(Debug, Clone)]
pub struct Tracker {
    connection: String,
    kind: &'static str,
    host: String,
    /// The last answer published for this connection: its state and the
    /// hint that came with it, since either changing is a change (#320).
    last: Option<(State, Option<String>)>,
}

impl Tracker {
    pub fn new(connection: &str, kind: &'static str, host: &str) -> Self {
        Self {
            connection: connection.to_owned(),
            kind,
            host: host.to_owned(),
            last: None,
        }
    }

    pub fn connection(&self) -> &str {
        &self.connection
    }

    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// The envelope to publish for this observation, or `None` when
    /// nothing changed — the same answer twice is silence.
    ///
    /// The **hint** is part of "the same answer" (#320). Two refusals can
    /// share a state and mean different work: a missing audience and a URL
    /// naming a service this collector does not read are both
    /// `pending_operator`, and an operator who acted on the first sentence
    /// has to be told when the second one becomes true. A state that did
    /// not change carries `from_state` equal to `to_state`, which is the
    /// honest reading: the connection did not move, the reason did.
    pub fn observe(&mut self, observation: &Observation, occurred_at: &str) -> Option<Value> {
        let answer = (observation.state, observation.hint.clone());
        if self.last.as_ref() == Some(&answer) {
            return None;
        }
        let from_state = self
            .last
            .as_ref()
            .map(|(state, _)| state.as_str())
            .unwrap_or("unknown");
        self.last = Some(answer);
        let id = sha256_hex(&format!(
            "{}:{}:{occurred_at}:{}",
            self.connection,
            observation.state.as_str(),
            observation.hint.as_deref().unwrap_or_default()
        ));
        let mut data = json!({
            "connection": self.connection,
            "kind": self.kind,
            "from_state": from_state,
            "to_state": observation.state.as_str(),
            "occurred_at": occurred_at,
        });
        if let Some(service) = observation.service {
            data["service"] = json!(service);
        }
        if let Some(hint) = &observation.hint {
            data["hint"] = json!(hint);
        }
        Some(json!({
            "specversion": "1.0",
            "id": id,
            "source": format!("collector://{}/connections/{}", self.host, self.connection),
            "type": STATUS_CHANGED_TYPE,
            "time": occurred_at,
            "subject": self.connection,
            "datacontenttype": "application/json",
            "dataschema": STATUS_CHANGED_DATASCHEMA,
            "connection": self.connection,
            "data": data,
        }))
    }
}

/// The contract's id digest: lowercase hex sha256 of the natural key.
pub fn sha256_hex(input: &str) -> String {
    Sha256::digest(input.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #320: two refusals can share a state and mean different work. The
    /// operator who acted on the first sentence is told the second one,
    /// and the connection is honest about not having moved.
    #[test]
    fn a_state_that_stays_with_a_new_reason_is_published_once_and_not_twice() {
        let mut tracker = Tracker::new("calendar-linagora", "calendar", "collector.example.com");
        let pending = |hint: &str| Observation {
            state: State::PendingOperator,
            service: Some("caldav"),
            hint: Some(hint.to_owned()),
        };
        let audience = pending("an audience or a scope the operator has to add at the SSO");
        let not_a_resource_server = pending("no scope added at the SSO will change this answer");
        tracker
            .observe(&audience, "2026-09-23T09:00:00Z")
            .expect("the first observation publishes");
        assert!(
            tracker.observe(&audience, "2026-09-23T09:01:00Z").is_none(),
            "the same sentence twice is silence"
        );
        let changed = tracker
            .observe(&not_a_resource_server, "2026-09-23T09:02:00Z")
            .expect("a reason that changed is published");
        assert_eq!(changed["data"]["from_state"], "pending_operator");
        assert_eq!(changed["data"]["to_state"], "pending_operator");
        assert_eq!(
            changed["data"]["hint"],
            "no scope added at the SSO will change this answer"
        );
        assert!(tracker
            .observe(&not_a_resource_server, "2026-09-23T09:03:00Z")
            .is_none());
    }

    #[test]
    fn the_first_observation_of_a_run_comes_from_unknown_and_the_same_state_twice_is_silence() {
        let mut tracker = Tracker::new("mail-linagora", "email", "collector.example.com");
        let first = tracker
            .observe(&Observation::connected(), "2026-09-20T09:00:00Z")
            .expect("the first observation publishes");
        assert_eq!(first["data"]["from_state"], "unknown");
        assert_eq!(first["data"]["to_state"], "connected");
        assert_eq!(first["subject"], "mail-linagora");
        assert_eq!(first["connection"], "mail-linagora");
        assert_eq!(
            first["source"],
            "collector://collector.example.com/connections/mail-linagora"
        );
        assert!(first["data"].get("hint").is_none());
        assert!(
            tracker
                .observe(&Observation::connected(), "2026-09-20T09:01:00Z")
                .is_none(),
            "connected again is not a change"
        );
        let refused = tracker
            .observe(
                &Observation {
                    state: State::ReconnectRequired,
                    service: Some("sso"),
                    hint: Some("Run `twalk-collector authorize --renew`.".to_owned()),
                },
                "2026-09-20T09:02:00Z",
            )
            .expect("a transition publishes");
        assert_eq!(refused["data"]["from_state"], "connected");
        assert_eq!(refused["data"]["service"], "sso");
        assert_eq!(
            refused["id"],
            sha256_hex(
                "mail-linagora:reconnect_required:2026-09-20T09:02:00Z:Run \
                 `twalk-collector authorize --renew`."
            ),
            "the id covers the hint too, so two reasons at one second are two events"
        );
    }

    #[test]
    fn the_envelope_is_the_contracts() {
        // The fixture's own id, recomputed by the recipe the schema states.
        let fixture: Value = serde_json::from_str(include_str!(
            "../../contracts/cloudevents/v1/fixtures/connection.status.changed.json"
        ))
        .unwrap();
        assert_eq!(
            fixture["id"],
            sha256_hex("mail-linagora:reconnect_required:2026-09-20T09:00:00Z")
        );
        assert_eq!(
            bus_subject(STATUS_CHANGED_TYPE),
            "twalk.connection.status.changed.v1"
        );
    }
}
