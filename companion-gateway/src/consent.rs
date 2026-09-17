//! Consent as the Companion Gateway writes it (ticket #49): the vocabulary of
//! a decision, the deterministic envelope it is published as, and the
//! precedence a network default has against a per-contact decision.
//!
//! Everything here is pure: a decision in, a validated decision or an
//! envelope out. The journal and its projection are [`crate::store`]; the
//! publication is [`crate::outbox`]; the HTTP surface is [`crate::http`].
//! The Gateway is the single writer of consent state (ADR 0006) and its
//! local store is the record of truth, with the bus as the audit trail
//! (ADR 0010).

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The contract type the Gateway produces, and the schema it validates
/// against.
pub const CONSENT_CHANGED_TYPE: &str = "fr.linagora.twalk.consent.state.changed.v1";
pub const CONSENT_CHANGED_DATASCHEMA: &str =
    "https://schemas.twalk.dev/cloudevents/v1/consent.state.changed.schema.json";

/// The bus stream and its subjects, as the Sensor declares them
/// (`sensor/src/normalize.rs`). The Gateway ensures the same stream: the
/// reference deployment must publish consent decisions whether or not a
/// Sensor ever started.
pub const STREAM_NAME: &str = "twalk";
pub const STREAM_SUBJECTS: [&str; 1] = ["twalk.>"];

/// Maps a contract event type to its bus subject:
/// `fr.linagora.twalk.<domain>.<action>.<version>` → `twalk.<domain>.<action>.<version>`.
pub fn bus_subject(event_type: &str) -> String {
    let rest = event_type
        .strip_prefix("fr.linagora.twalk.")
        .expect("contract event types always carry the fr.linagora.twalk prefix");
    format!("twalk.{rest}")
}

/// A messaging network as the user experiences it (ADR 0005), and Matrix
/// itself for native rooms (ADR 0009) — the contract's `network` enum,
/// exactly. A bridge id is never a network: nothing here folds `gmessages`
/// into `sms`, because the Gateway's inputs come from the Companion and from
/// the bus, never from a bridge.
///
/// Deliberately not `Ord`: the contract sorts a scope by the network's
/// string value ("ascending order" over an array of strings), and a
/// derived ordering would silently sort by the order the variants happen to
/// be declared in. Sorting goes through [`Network::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Network {
    Whatsapp,
    Telegram,
    Signal,
    Discord,
    Sms,
    Matrix,
}

impl Network {
    pub const ALL: [Network; 6] = [
        Network::Whatsapp,
        Network::Telegram,
        Network::Signal,
        Network::Discord,
        Network::Sms,
        Network::Matrix,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Network::Whatsapp => "whatsapp",
            Network::Telegram => "telegram",
            Network::Signal => "signal",
            Network::Discord => "discord",
            Network::Sms => "sms",
            Network::Matrix => "matrix",
        }
    }

    /// Parses a contract `network` value. `matrix` is one of them: a native
    /// Matrix room is a network like any other (ADR 0009).
    pub fn parse(value: &str) -> Option<Self> {
        Network::ALL.into_iter().find(|n| n.as_str() == value)
    }
}

/// The data-processing agreement state of a subject, as the contract's
/// `new_state` enum. `unset` is not one of them — it is the absence of a
/// decision, which only `old_state` can carry ([`OldState`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Granted,
    Pending,
    Revoked,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Granted => "granted",
            State::Pending => "pending",
            State::Revoked => "revoked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "granted" => Some(State::Granted),
            "pending" => Some(State::Pending),
            "revoked" => Some(State::Revoked),
            _ => None,
        }
    }
}

/// The contract's `old_state`: a state, or `unset` when no decision had ever
/// been recorded. An absent subject means "never decided", never "revoked"
/// (ADR 0010), and this type is where that distinction is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OldState {
    Unset,
    Was(State),
}

impl OldState {
    pub fn as_str(self) -> &'static str {
        match self {
            OldState::Unset => "unset",
            OldState::Was(state) => state.as_str(),
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unset" => Some(OldState::Unset),
            other => State::parse(other).map(OldState::Was),
        }
    }
}

/// What a decision applies to. `persona` is deliberately absent from the
/// write API this ticket builds: persona activation is #60, and it will use
/// this same write path rather than a control API (ADR 0013).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectType {
    Contact,
    Network,
}

impl SubjectType {
    pub fn as_str(self) -> &'static str {
        match self {
            SubjectType::Contact => "contact",
            SubjectType::Network => "network",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "contact" => Some(SubjectType::Contact),
            "network" => Some(SubjectType::Network),
            _ => None,
        }
    }
}

/// Who or what a decision applies to: a contact by Matrix user ID, or a whole
/// network by its contract value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub kind: SubjectType,
    pub id: String,
}

/// A decision the user asks the Gateway to record, once validated: a subject,
/// the state it moves to, and the networks the decision covers. The scope is
/// held sorted and deduplicated, because it is part of the event's natural
/// key (the id recipe joins it in ascending order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub subject: Subject,
    pub new_state: State,
    pub networks: Vec<Network>,
    pub reason: Option<String>,
}

/// Why a decision cannot be recorded. Each variant is one 4xx answer with a
/// stable machine-readable code, because the Companion renders these and a
/// third party codes against them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    /// A field the write API requires is missing or not of the right JSON
    /// type.
    Malformed(String),
    /// `subject.type` names something the contract knows but this write path
    /// does not accept — today only `persona` (#60).
    UnsupportedSubjectType(String),
    /// A value outside the contract's enums: a state, a network, a subject
    /// type.
    Unknown { field: String, value: String },
    /// A network subject whose scope does not name exactly that network, which
    /// the contract forbids.
    ScopeContradictsSubject { subject: String, scope: Vec<String> },
}

impl Invalid {
    /// The stable `error` code of the response.
    pub fn code(&self) -> &'static str {
        match self {
            Invalid::Malformed(_) => "malformed_request",
            Invalid::UnsupportedSubjectType(_) => "unsupported_subject_type",
            Invalid::Unknown { .. } => "unknown_value",
            Invalid::ScopeContradictsSubject { .. } => "scope_contradicts_subject",
        }
    }

    /// The human-readable half, for the operator's logs and the developer's
    /// console.
    pub fn message(&self) -> String {
        match self {
            Invalid::Malformed(what) => format!("{what} is missing or of the wrong type"),
            Invalid::UnsupportedSubjectType(kind) => format!(
                "subject.type {kind:?} is not accepted on this endpoint: a decision applies to a contact or a network"
            ),
            Invalid::Unknown { field, value } => {
                format!("{field} has the unknown value {value:?}")
            }
            Invalid::ScopeContradictsSubject { subject, scope } => format!(
                "a network subject must be scoped to exactly its own network: subject {subject:?} against scope {scope:?}"
            ),
        }
    }
}

impl Decision {
    /// Reads a decision from the write API's JSON body, mirroring the
    /// contract's `data` shape minus what the Gateway fills in itself
    /// (`old_state`, `occurred_at`, `actor`):
    ///
    /// ```json
    /// {
    ///   "subject": { "type": "contact", "id": "@whatsapp_33612345678:example.com" },
    ///   "new_state": "granted",
    ///   "scope": { "networks": ["whatsapp"] },
    ///   "reason": "optional, kept in the audit trail"
    /// }
    /// ```
    ///
    /// The scope comes back sorted and deduplicated: the id recipe joins the
    /// networks in ascending order, so two spellings of one scope must not
    /// become two different decisions.
    pub fn parse(body: &Value) -> Result<Self, Invalid> {
        let kind = string(body, "/subject/type")?;
        let kind = match SubjectType::parse(&kind) {
            Some(kind) => kind,
            // A subject type the contract knows but this endpoint does not
            // is a different answer from a typo: one is "not yet", the other
            // is "never".
            None if kind == "persona" => return Err(Invalid::UnsupportedSubjectType(kind)),
            None => {
                return Err(Invalid::Unknown {
                    field: "subject.type".to_owned(),
                    value: kind,
                })
            }
        };
        let id = string(body, "/subject/id")?;
        if id.is_empty() || id.chars().count() > 256 {
            return Err(Invalid::Malformed("subject.id".to_owned()));
        }
        let new_state = string(body, "/new_state")?;
        let new_state = State::parse(&new_state).ok_or(Invalid::Unknown {
            field: "new_state".to_owned(),
            value: new_state,
        })?;
        let scope = body
            .pointer("/scope/networks")
            .and_then(Value::as_array)
            .ok_or_else(|| Invalid::Malformed("scope.networks".to_owned()))?;
        let mut networks = Vec::new();
        for entry in scope {
            let value = entry
                .as_str()
                .ok_or_else(|| Invalid::Malformed("scope.networks".to_owned()))?;
            let network = Network::parse(value).ok_or(Invalid::Unknown {
                field: "scope.networks".to_owned(),
                value: value.to_owned(),
            })?;
            if !networks.contains(&network) {
                networks.push(network);
            }
        }
        if networks.is_empty() {
            return Err(Invalid::Malformed("scope.networks".to_owned()));
        }
        // Ascending order of the contract's own string values — the order
        // the id recipe joins them in.
        networks.sort_by_key(|network| network.as_str());
        // The contract: "for a network subject the id is the network value
        // itself, in which case scope.networks holds exactly that one
        // network". Anything else would publish a decision whose own two
        // halves disagree.
        if kind == SubjectType::Network
            && networks.as_slice()
                != [Network::parse(&id).ok_or(Invalid::Unknown {
                    field: "subject.id".to_owned(),
                    value: id.clone(),
                })?]
        {
            return Err(Invalid::ScopeContradictsSubject {
                subject: id,
                scope: networks.iter().map(|n| n.as_str().to_owned()).collect(),
            });
        }
        let reason = match body.get("reason") {
            None | Some(Value::Null) => None,
            Some(Value::String(reason)) if reason.chars().count() <= 1024 => {
                Some(reason.to_owned())
            }
            Some(_) => return Err(Invalid::Malformed("reason".to_owned())),
        };
        Ok(Self {
            subject: Subject { kind, id },
            new_state,
            networks,
            reason,
        })
    }

    /// The scope as the id recipe renders it: the networks in ascending
    /// order, joined by single ASCII commas with no spaces, so a single
    /// network renders as just that value.
    pub fn scope_key(&self) -> String {
        self.networks
            .iter()
            .map(|network| network.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// A decision as recorded: the decision itself plus everything the Gateway
/// stamped on it. This is what the journal holds and what the envelope is
/// rendered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub decision: Decision,
    /// The state the subject held on this perimeter before the decision.
    pub old_state: OldState,
    /// When the user took the decision, RFC 3339 — part of the id's natural
    /// key, and stored verbatim so the id stays reproducible.
    pub occurred_at: String,
    /// The Matrix user ID of the owner who took it (ADR 0011: one owner per
    /// deployment).
    pub actor: String,
}

impl Recorded {
    /// The contract's deterministic id: the lowercase-hex sha256 of
    /// `subject.type:subject.id:new_state:<networks>:occurred_at`, where
    /// `<networks>` is the scope sorted ascending and comma-joined, and
    /// `occurred_at` is verbatim what the event carries.
    ///
    /// The scope is in the key on purpose: two decisions on the same subject,
    /// to the same state, at the same instant but over different networks are
    /// different decisions, and a key without the scope would collide them
    /// onto one id that the bus would then deduplicate down to one.
    pub fn event_id(&self) -> String {
        let key = format!(
            "{}:{}:{}:{}:{}",
            self.decision.subject.kind.as_str(),
            self.decision.subject.id,
            self.decision.new_state.as_str(),
            self.decision.scope_key(),
            self.occurred_at
        );
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// The CloudEvent, rendered exactly as it will be published.
    ///
    /// Three envelope conventions are settled here, and a third party codes
    /// against them:
    ///
    /// - `source` is `gateway://<domain>/consent`, the consent resource on
    ///   this Gateway.
    /// - the `network` extension is set only when the scope names exactly one
    ///   network, so a single-network change can be filtered server-side on
    ///   NATS; the authoritative scope is always `data.scope.networks`.
    /// - the `consent` extension is never set: on an event that announces a
    ///   consent change it would be redundant with `data.new_state` at best
    ///   and self-contradictory on a revocation. Consumers read
    ///   `data.new_state` (ADR 0007, and the schema's own wording).
    ///
    /// `time` is when the Gateway produced the event; `occurred_at` is when
    /// the user decided. They differ on a republish, and only `occurred_at`
    /// is in the id.
    pub fn envelope(&self, domain: &str, produced_at: &str) -> Value {
        let mut event = json!({
            "specversion": "1.0",
            "id": self.event_id(),
            "source": format!("gateway://{domain}/consent"),
            "type": CONSENT_CHANGED_TYPE,
            "time": produced_at,
            "subject": self.decision.subject.id,
            "datacontenttype": "application/json",
            "dataschema": CONSENT_CHANGED_DATASCHEMA,
            "data": {
                "subject": {
                    "type": self.decision.subject.kind.as_str(),
                    "id": self.decision.subject.id,
                },
                "old_state": self.old_state.as_str(),
                "new_state": self.decision.new_state.as_str(),
                "scope": {
                    "networks": self.decision.networks
                        .iter()
                        .map(|network| Value::from(network.as_str()))
                        .collect::<Vec<_>>(),
                },
                "occurred_at": self.occurred_at,
                "actor": self.actor,
            }
        });
        if let [only] = self.decision.networks.as_slice() {
            event["network"] = Value::from(only.as_str());
        }
        if let Some(reason) = &self.decision.reason {
            event["data"]["reason"] = Value::from(reason.as_str());
        }
        event
    }
}

/// The effective consent state of a contact on one network, with the decision
/// that produced it.
///
/// Precedence, per CONTEXT.md and the contract: a network-level decision is
/// that network's default, and a contact-scoped decision for the same network
/// always overrides it. No decision at all resolves to `pending` — the safe
/// direction — with `decided_by` left empty, because an absent subject means
/// "never decided", never "revoked".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    pub state: State,
    pub decided_by: Option<Subject>,
}

impl Effective {
    /// Resolves the precedence from the only two decisions that can bear on a
    /// contact on one network: the contact's own, and the network's default.
    pub fn resolve(
        contact: &str,
        network: Network,
        contact_state: Option<State>,
        default_state: Option<State>,
    ) -> Self {
        if let Some(state) = contact_state {
            return Self {
                state,
                decided_by: Some(Subject {
                    kind: SubjectType::Contact,
                    id: contact.to_owned(),
                }),
            };
        }
        if let Some(state) = default_state {
            return Self {
                state,
                decided_by: Some(Subject {
                    kind: SubjectType::Network,
                    id: network.as_str().to_owned(),
                }),
            };
        }
        Self {
            state: State::Pending,
            decided_by: None,
        }
    }
}

/// Reads a required string out of the request body by JSON pointer.
fn string(body: &Value, pointer: &str) -> Result<String, Invalid> {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Invalid::Malformed(pointer.trim_start_matches('/').replace('/', ".")))
}

/// The contract's timestamps: RFC 3339, to the millisecond.
///
/// Formatted as the Sensor formats its own (`sensor/src/main.rs::rfc3339`),
/// and truncated to milliseconds on purpose: `occurred_at` is part of the
/// event's natural key, so the resolution is the resolution at which two
/// decisions on one subject stay two events. Milliseconds are finer than any
/// human clicking, and coarse enough to read in an audit trail.
pub fn rfc3339_millis(at: std::time::SystemTime) -> String {
    let at = time::OffsetDateTime::from(at);
    let millisecond = at.millisecond();
    at.replace_nanosecond(u32::from(millisecond) * 1_000_000)
        .unwrap_or(at)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(body: Value) -> Result<Decision, Invalid> {
        Decision::parse(&body)
    }

    fn contact_decision(networks: Value) -> Value {
        json!({
            "subject": { "type": "contact", "id": "@whatsapp_33612345678:example.com" },
            "new_state": "granted",
            "scope": { "networks": networks }
        })
    }

    #[test]
    fn maps_the_contract_type_to_its_bus_subject() {
        assert_eq!(
            bus_subject(CONSENT_CHANGED_TYPE),
            "twalk.consent.state.changed.v1"
        );
    }

    #[test]
    fn matrix_is_a_network_like_any_other() {
        assert_eq!(Network::parse("matrix"), Some(Network::Matrix));
        assert_eq!(Network::parse("whatsapp"), Some(Network::Whatsapp));
        // A bridge id is never a network (ADR 0005).
        assert_eq!(Network::parse("gmessages"), None);
    }

    #[test]
    fn a_scope_is_sorted_and_deduplicated_so_one_perimeter_has_one_id() {
        let decision = request(contact_decision(json!([
            "whatsapp", "matrix", "signal", "whatsapp"
        ])))
        .unwrap();
        assert_eq!(decision.scope_key(), "matrix,signal,whatsapp");
        let other_spelling =
            request(contact_decision(json!(["signal", "whatsapp", "matrix"]))).unwrap();
        assert_eq!(decision.scope_key(), other_spelling.scope_key());
    }

    #[test]
    fn the_id_is_the_contract_recipe_over_the_sorted_scope() {
        // The contract's own fixture, recomputed: persona is not writable
        // here, but the recipe is the same string for every subject type,
        // and this is the one published example of it.
        let recorded = Recorded {
            decision: Decision {
                subject: Subject {
                    kind: SubjectType::Contact,
                    id: "@whatsapp_33612345678:example.com".to_owned(),
                },
                new_state: State::Granted,
                networks: vec![Network::Signal, Network::Whatsapp],
                reason: None,
            },
            old_state: OldState::Unset,
            occurred_at: "2026-09-17T10:05:00Z".to_owned(),
            actor: "@michel:example.com".to_owned(),
        };
        let expected = {
            let mut hasher = Sha256::new();
            hasher.update(
                "contact:@whatsapp_33612345678:example.com:granted:signal,whatsapp:2026-09-17T10:05:00Z"
                    .as_bytes(),
            );
            hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        assert_eq!(recorded.event_id(), expected);
        assert_eq!(recorded.event_id().len(), 64);
    }

    #[test]
    fn the_network_extension_is_set_only_for_a_single_network_scope() {
        let mut recorded = Recorded {
            decision: Decision {
                subject: Subject {
                    kind: SubjectType::Contact,
                    id: "@a:example.com".to_owned(),
                },
                new_state: State::Granted,
                networks: vec![Network::Whatsapp],
                reason: None,
            },
            old_state: OldState::Unset,
            occurred_at: "2026-09-17T10:05:00Z".to_owned(),
            actor: "@michel:example.com".to_owned(),
        };
        let single = recorded.envelope("example.com", "2026-09-17T10:05:01Z");
        assert_eq!(single["network"].as_str(), Some("whatsapp"));
        assert_eq!(
            single["source"].as_str(),
            Some("gateway://example.com/consent")
        );
        assert!(
            single.get("consent").is_none(),
            "the consent extension is never set on the Gateway's own events: {single}"
        );
        assert_eq!(
            single["data"]["actor"].as_str(),
            Some("@michel:example.com")
        );

        recorded.decision.networks = vec![Network::Signal, Network::Whatsapp];
        let several = recorded.envelope("example.com", "2026-09-17T10:05:01Z");
        assert!(
            several.get("network").is_none(),
            "a multi-network scope leaves the extension out: {several}"
        );
        assert_eq!(
            several["data"]["scope"]["networks"],
            json!(["signal", "whatsapp"])
        );
    }

    #[test]
    fn a_reason_travels_in_the_audit_trail_and_is_left_out_when_absent() {
        let mut recorded = Recorded {
            decision: Decision {
                subject: Subject {
                    kind: SubjectType::Network,
                    id: "whatsapp".to_owned(),
                },
                new_state: State::Revoked,
                networks: vec![Network::Whatsapp],
                reason: Some("stepping away for a while".to_owned()),
            },
            old_state: OldState::Was(State::Granted),
            occurred_at: "2026-09-17T10:05:00Z".to_owned(),
            actor: "@michel:example.com".to_owned(),
        };
        let with_reason = recorded.envelope("example.com", "2026-09-17T10:05:00Z");
        assert_eq!(
            with_reason["data"]["reason"].as_str(),
            Some("stepping away for a while")
        );
        assert_eq!(with_reason["data"]["old_state"].as_str(), Some("granted"));
        recorded.decision.reason = None;
        assert!(
            recorded.envelope("example.com", "2026-09-17T10:05:00Z")["data"]
                .get("reason")
                .is_none()
        );
    }

    #[test]
    fn a_persona_subject_is_refused_as_not_yet_supported() {
        let error = request(json!({
            "subject": { "type": "persona", "id": "assistant" },
            "new_state": "granted",
            "scope": { "networks": ["whatsapp"] }
        }))
        .unwrap_err();
        assert_eq!(error.code(), "unsupported_subject_type");
    }

    #[test]
    fn a_network_subject_must_be_scoped_to_its_own_network() {
        let error = request(json!({
            "subject": { "type": "network", "id": "whatsapp" },
            "new_state": "granted",
            "scope": { "networks": ["whatsapp", "signal"] }
        }))
        .unwrap_err();
        assert_eq!(error.code(), "scope_contradicts_subject");

        let ok = request(json!({
            "subject": { "type": "network", "id": "whatsapp" },
            "new_state": "granted",
            "scope": { "networks": ["whatsapp"] }
        }))
        .unwrap();
        assert_eq!(ok.networks, vec![Network::Whatsapp]);
    }

    #[test]
    fn malformed_and_unknown_values_are_told_apart() {
        assert_eq!(
            request(json!({ "new_state": "granted", "scope": { "networks": ["whatsapp"] } }))
                .unwrap_err()
                .code(),
            "malformed_request"
        );
        assert_eq!(
            request(contact_decision(json!([]))).unwrap_err().code(),
            "malformed_request"
        );
        assert_eq!(
            request(contact_decision(json!(["irc"])))
                .unwrap_err()
                .code(),
            "unknown_value"
        );
        let mut unknown_state = contact_decision(json!(["whatsapp"]));
        unknown_state["new_state"] = json!("unsure");
        assert_eq!(request(unknown_state).unwrap_err().code(), "unknown_value");
        // `unset` is the absence of a decision, not a state to move to.
        let mut unset = contact_decision(json!(["whatsapp"]));
        unset["new_state"] = json!("unset");
        assert_eq!(request(unset).unwrap_err().code(), "unknown_value");
    }

    #[test]
    fn a_contact_decision_overrides_the_networks_default() {
        let contact = "@whatsapp_33612345678:example.com";
        // The contact's own decision wins, whatever the default says, and
        // the answer names which decision that was.
        let overridden = Effective::resolve(
            contact,
            Network::Whatsapp,
            Some(State::Revoked),
            Some(State::Granted),
        );
        assert_eq!(overridden.state, State::Revoked);
        assert_eq!(
            overridden.decided_by,
            Some(Subject {
                kind: SubjectType::Contact,
                id: contact.to_owned()
            })
        );
        // No contact decision: the network default applies, as a default.
        let defaulted = Effective::resolve(contact, Network::Whatsapp, None, Some(State::Granted));
        assert_eq!(defaulted.state, State::Granted);
        assert_eq!(
            defaulted.decided_by,
            Some(Subject {
                kind: SubjectType::Network,
                id: "whatsapp".to_owned()
            })
        );
        // Neither: pending, the safe direction, and no decision to name —
        // "never decided" is not "revoked".
        let undecided = Effective::resolve(contact, Network::Whatsapp, None, None);
        assert_eq!(undecided.state, State::Pending);
        assert_eq!(undecided.decided_by, None);
    }

    #[test]
    fn timestamps_are_rfc_3339_to_the_millisecond() {
        let at = std::time::UNIX_EPOCH
            + std::time::Duration::from_millis(1_789_567_890_123)
            + std::time::Duration::from_nanos(456_789);
        let formatted = rfc3339_millis(at);
        assert!(
            formatted.starts_with("2026-09-16T14:11:30"),
            "the instant is formatted as the contract's date-time: {formatted}"
        );
        assert!(
            formatted.contains(".123"),
            "milliseconds are kept: {formatted}"
        );
        assert!(
            !formatted.contains(".123456"),
            "and nothing finer is: {formatted}"
        );
    }
}
