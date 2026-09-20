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
    /// A mailbox (ADR 0033, #268). Parsed and served like any network; the
    /// store's own `CHECK` constraints admit it from the migration #270
    /// lands, and until then a decision about an `email` subject — of which
    /// there is none, no collector existing yet — is refused by the store.
    Email,
}

impl Network {
    /// Every value, in the contract's order (`definitions/network.schema.json`,
    /// the one authority — a test below reads it).
    pub const ALL: [Network; 7] = [
        Network::Whatsapp,
        Network::Telegram,
        Network::Signal,
        Network::Discord,
        Network::Sms,
        Network::Matrix,
        Network::Email,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Network::Whatsapp => "whatsapp",
            Network::Telegram => "telegram",
            Network::Signal => "signal",
            Network::Discord => "discord",
            Network::Sms => "sms",
            Network::Matrix => "matrix",
            Network::Email => "email",
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

/// What a decision applies to: one contact, a whole network's default, or a
/// persona.
///
/// `persona` is on this write path and not on a control API of its own,
/// because activating or pausing a persona *is* a consent decision (ADR
/// 0013): the same journal, the same deterministic envelope, the same bus
/// subject. `scope.networks` on a persona subject means the networks that
/// persona may read, which is why activation never spreads — a network
/// connected later is simply not in any scope the user has decided on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectType {
    Contact,
    Network,
    Persona,
}

impl SubjectType {
    pub fn as_str(self) -> &'static str {
        match self {
            SubjectType::Contact => "contact",
            SubjectType::Network => "network",
            SubjectType::Persona => "persona",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "contact" => Some(SubjectType::Contact),
            "network" => Some(SubjectType::Network),
            "persona" => Some(SubjectType::Persona),
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
/// the state it moves to, and the connections the decision covers (ADR
/// 0033, #270). The scope is held sorted and deduplicated, because it is
/// part of the event's natural key (the id recipe joins it in ascending
/// order). `networks` is the kinds of those connections — derivable, and
/// carried because `scope.networks` stays on the wire for every consumer
/// not yet migrated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub subject: Subject,
    pub new_state: State,
    /// The perimeter: connection ids, ascending, no duplicate.
    pub connections: Vec<String>,
    /// The kinds of `connections`, ascending, no duplicate.
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
    /// does not accept. Nothing does today — `persona` was the last one, and
    /// ADR 0013 put it on this path — but the code stays part of the
    /// description's contract so a client that branches on it keeps
    /// compiling.
    UnsupportedSubjectType(String),
    /// A value outside the contract's enums: a state, a network, a subject
    /// type.
    Unknown { field: String, value: String },
    /// A network subject whose scope does not name exactly that network, which
    /// the contract forbids.
    ScopeContradictsSubject { subject: String, scope: Vec<String> },
    /// `scope.networks` alone names a network that has several connections
    /// (#270): the caller has to say which perimeter it means, and the
    /// Gateway will not pick one. Answered under `malformed_request` — the
    /// member that would settle it, `scope.connections`, is missing — with
    /// the candidates named.
    ScopeNeedsConnections {
        network: String,
        connections: Vec<String>,
    },
    /// `scope.networks` and `scope.connections` were both sent and do not
    /// describe one perimeter: the networks are not the connections' kinds.
    /// Under `malformed_request` too — the body disagrees with itself.
    ScopeMembersDisagree {
        networks: Vec<String>,
        connections: Vec<String>,
    },
    /// The subject is one of the owner's own identities (ticket #149, ADR
    /// 0018, ADR 0021). The only variant [`Decision::parse`] never produces:
    /// it takes knowing who the owner is, which is
    /// [`Decision::refuse_if_owner`]'s question.
    ///
    /// Its own code, and its own status, because the one thing this refusal
    /// must not be confusable with is "there is no such subject": a client
    /// that read them as one signal would retry, or offer the user the
    /// decision again, on a subject that can never have one.
    SubjectIsTheOwner { id: String },
}

impl Invalid {
    /// The stable `error` code of the response.
    pub fn code(&self) -> &'static str {
        match self {
            Invalid::Malformed(_) => "malformed_request",
            Invalid::UnsupportedSubjectType(_) => "unsupported_subject_type",
            Invalid::Unknown { .. } => "unknown_value",
            Invalid::ScopeContradictsSubject { .. } => "scope_contradicts_subject",
            Invalid::ScopeNeedsConnections { .. } => "malformed_request",
            Invalid::ScopeMembersDisagree { .. } => "malformed_request",
            Invalid::SubjectIsTheOwner { .. } => "subject_is_the_owner",
        }
    }

    /// The HTTP status the refusal is answered with, as a number so that this
    /// module stays free of the HTTP stack.
    ///
    /// `400` for a request the contract does not allow; `409` for the owner,
    /// because the request is well-formed and its subject is a perfectly good
    /// Matrix ID — what refuses it is a rule about who that Matrix ID is. A
    /// `404` would say "no such subject", which is the conflation this
    /// refusal exists to avoid, and a `400` would invite a client to go and
    /// look for the malformed field.
    pub fn status(&self) -> u16 {
        match self {
            Invalid::SubjectIsTheOwner { .. } => 409,
            _ => 400,
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
            Invalid::ScopeNeedsConnections {
                network,
                connections,
            } => format!(
                "scope.networks names {network:?}, which has {} connections on this deployment \
                 ({}): name the one you mean in scope.connections",
                connections.len(),
                connections.join(", ")
            ),
            Invalid::ScopeMembersDisagree {
                networks,
                connections,
            } => format!(
                "scope.networks {networks:?} is not the kinds of scope.connections {connections:?}: \
                 send one member, or two that agree"
            ),
            Invalid::SubjectIsTheOwner { id } => format!(
                "{id:?} is one of this deployment's owner identities, and the owner is never a \
                 contact and never has a consent state: there is nothing to decide, and no \
                 decision about them can be recorded (ADR 0018, ADR 0021). This is not \
                 \"no such subject\": the identity is known, and it is yours"
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
    ///   "scope": { "connections": ["whatsapp"] },
    ///   "reason": "optional, kept in the audit trail"
    /// }
    /// ```
    ///
    /// The scope is `scope.connections`, looked up in the registry (#270). A
    /// caller not yet migrated sends `scope.networks` alone, and each
    /// network is read as its single connection on this deployment — a
    /// lookup, not a guess: a network with several is refused naming them.
    /// Both members together must agree.
    ///
    /// The scope comes back sorted and deduplicated: the id recipe joins the
    /// networks in ascending order, so two spellings of one scope must not
    /// become two different decisions.
    pub fn parse(body: &Value, registry: &crate::connections::Registry) -> Result<Self, Invalid> {
        let kind = string(body, "/subject/type")?;
        let kind = SubjectType::parse(&kind).ok_or(Invalid::Unknown {
            field: "subject.type".to_owned(),
            value: kind,
        })?;
        let id = string(body, "/subject/id")?;
        if id.is_empty() || id.chars().count() > 256 {
            return Err(Invalid::Malformed("subject.id".to_owned()));
        }
        let new_state = string(body, "/new_state")?;
        let new_state = State::parse(&new_state).ok_or(Invalid::Unknown {
            field: "new_state".to_owned(),
            value: new_state,
        })?;
        let (connections, networks) = Self::parse_scope(body, registry)?;
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
            connections,
            networks,
            reason,
        })
    }

    /// The scope, as connection ids and their kinds, both ascending and
    /// deduplicated.
    fn parse_scope(
        body: &Value,
        registry: &crate::connections::Registry,
    ) -> Result<(Vec<String>, Vec<Network>), Invalid> {
        let strings = |member: &str| -> Result<Option<Vec<String>>, Invalid> {
            let Some(value) = body.pointer(&format!("/scope/{member}")) else {
                return Ok(None);
            };
            let entries = value
                .as_array()
                .ok_or_else(|| Invalid::Malformed(format!("scope.{member}")))?;
            let mut values = Vec::new();
            for entry in entries {
                let value = entry
                    .as_str()
                    .ok_or_else(|| Invalid::Malformed(format!("scope.{member}")))?;
                if !values.iter().any(|known| known == value) {
                    values.push(value.to_owned());
                }
            }
            if values.is_empty() {
                return Err(Invalid::Malformed(format!("scope.{member}")));
            }
            Ok(Some(values))
        };
        let declared_networks = match strings("networks")? {
            None => None,
            Some(values) => {
                let mut networks = Vec::new();
                for value in values {
                    networks.push(Network::parse(&value).ok_or(Invalid::Unknown {
                        field: "scope.networks".to_owned(),
                        value,
                    })?);
                }
                Some(networks)
            }
        };
        let mut connections = match strings("connections")? {
            Some(ids) => {
                for id in &ids {
                    if registry.get(id).is_none() {
                        return Err(Invalid::Unknown {
                            field: "scope.connections".to_owned(),
                            value: id.clone(),
                        });
                    }
                }
                ids
            }
            // A caller not yet migrated: each network is its single
            // connection on this deployment, read in the registry.
            None => {
                let networks = declared_networks
                    .as_ref()
                    .ok_or_else(|| Invalid::Malformed("scope.connections".to_owned()))?;
                let mut ids = Vec::new();
                for network in networks {
                    match registry.resolve(None, network.as_str()) {
                        Ok(connection) => ids.push(connection.id.clone()),
                        Err(_) => {
                            return Err(Invalid::ScopeNeedsConnections {
                                network: network.as_str().to_owned(),
                                connections: registry
                                    .of_kind(network.as_str())
                                    .iter()
                                    .map(|c| c.id.clone())
                                    .collect(),
                            })
                        }
                    }
                }
                ids
            }
        };
        // Ascending order — the order the id recipe joins them in.
        connections.sort();
        let mut networks = Vec::new();
        for id in &connections {
            let kind = registry
                .get(id)
                .map(|connection| connection.kind.as_str())
                .unwrap_or_default();
            // A connection whose kind is not a network — a calendar — has
            // no consent to decide (ADR 0033).
            let network = Network::parse(kind).ok_or(Invalid::Unknown {
                field: "scope.connections".to_owned(),
                value: id.clone(),
            })?;
            if !networks.contains(&network) {
                networks.push(network);
            }
        }
        networks.sort_by_key(|network| network.as_str());
        if let Some(mut declared) = declared_networks {
            declared.sort_by_key(|network| network.as_str());
            declared.dedup();
            if declared != networks {
                return Err(Invalid::ScopeMembersDisagree {
                    networks: declared.iter().map(|n| n.as_str().to_owned()).collect(),
                    connections,
                });
            }
        }
        Ok((connections, networks))
    }

    /// Refuses a decision whose subject is one of the owner's own identities
    /// (ticket #149).
    ///
    /// Separate from [`Decision::parse`] because it is a different kind of
    /// question: `parse` asks whether the contract allows this document, and
    /// this asks who the subject is — which needs the deployment's own
    /// configuration ([`crate::owner::Owner`]). Called by
    /// [`crate::outbox::Outbox::record`], which is the Gateway's single writer
    /// of consent state, so no route added later can write such a row by
    /// forgetting to ask.
    ///
    /// Only a `contact` subject is checked, and deliberately: a `network`
    /// subject's id is a network value, and a `persona` subject's id is a
    /// persona name chosen by whoever ships it. Neither is a Matrix user ID,
    /// so an id that collides with one of the owner's identities there is not
    /// a decision about the owner — and refusing it would refuse a persona
    /// over the shape of its name.
    pub fn refuse_if_owner(&self, owner: &crate::owner::Owner) -> Result<(), Invalid> {
        if self.subject.kind == SubjectType::Contact && owner.is_owner(&self.subject.id) {
            return Err(Invalid::SubjectIsTheOwner {
                id: self.subject.id.clone(),
            });
        }
        Ok(())
    }

    /// The scope as the id recipe renders it: the connections in ascending
    /// order, joined by single ASCII commas with no spaces, so a single
    /// connection renders as just that value. Before #270 this joined the
    /// networks; on a deployment whose connections are named after their
    /// networks the string is the same, so no id changed.
    pub fn scope_key(&self) -> String {
        self.connections.join(",")
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
    /// `subject.type:subject.id:new_state:<connections>:occurred_at`, where
    /// `<connections>` is the scope sorted ascending and comma-joined, and
    /// `occurred_at` is verbatim what the event carries.
    ///
    /// The scope is in the key on purpose: two decisions on the same subject,
    /// to the same state, at the same instant but over different perimeters
    /// are different decisions, and a key without the scope would collide
    /// them onto one id that the bus would then deduplicate down to one.
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
                    "connections": self.decision.connections.clone(),
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

    /// The contract is the one authority for the network values (ADR 0033,
    /// #268): this enum is a copy, tested against what it copies, order
    /// included.
    #[test]
    fn the_contract_is_the_authority_for_the_network_values() {
        let authority = twalk_test_harness::contract_definition_values("network")
            .expect("the contract's network definition");
        let copy: Vec<&str> = Network::ALL
            .iter()
            .map(|network| network.as_str())
            .collect();
        assert_eq!(
            copy, authority,
            "companion-gateway/src/consent.rs disagrees with the contract"
        );
        for value in &authority {
            assert_eq!(
                Network::parse(value).map(Network::as_str),
                Some(value.as_str()),
                "{value} is in the contract and not parsed here"
            );
        }
    }

    /// The reference deployment's registry: one connection per network,
    /// named after it — the shape under which `scope.networks` alone is
    /// enough — plus a second SMS account for the tests that need a kind
    /// with two.
    fn registry() -> crate::connections::Registry {
        crate::connections::Registry::from_config(
            Some(
                "whatsapp=whatsapp,signal=signal,telegram=telegram,sms=sms,sms-work=sms=Work,\
                 mail-linagora=email",
            ),
            &[],
            "example.com",
        )
        .expect("a registry")
    }

    fn request(body: Value) -> Result<Decision, Invalid> {
        Decision::parse(&body, &registry())
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
                connections: vec!["signal".to_owned(), "whatsapp".to_owned()],
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
                connections: vec!["whatsapp".to_owned()],
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

        recorded.decision.connections = vec!["signal".to_owned(), "whatsapp".to_owned()];
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
        assert_eq!(
            several["data"]["scope"]["connections"],
            json!(["signal", "whatsapp"]),
            "the perimeter itself, beside its kinds"
        );
    }

    #[test]
    fn a_scope_of_networks_alone_is_read_as_their_single_connections() {
        // A caller not yet migrated (#270): the registry has one Signal
        // connection, so `signal` is it — read there, not spelled from the
        // name — and the decision carries both members.
        let decision = request(contact_decision(json!(["signal"]))).unwrap();
        assert_eq!(decision.connections, ["signal"]);
        assert_eq!(decision.networks, [Network::Signal]);
        // SMS has two on this registry: the Gateway will not pick.
        match request(contact_decision(json!(["sms"]))) {
            Err(Invalid::ScopeNeedsConnections {
                network,
                connections,
            }) => {
                assert_eq!(network, "sms");
                assert_eq!(connections, ["sms", "sms-work"]);
            }
            other => panic!("expected the two candidates named, got {other:?}"),
        }
        assert_eq!(
            Invalid::ScopeNeedsConnections {
                network: "whatsapp".to_owned(),
                connections: vec![]
            }
            .code(),
            "malformed_request",
            "not a new code: the member that would settle it is missing"
        );
    }

    #[test]
    fn a_scope_of_connections_is_the_perimeter_and_its_kinds_follow() {
        let decision = request(json!({
            "subject": { "type": "contact", "id": "@whatsapp_33612345678:example.com" },
            "new_state": "granted",
            "scope": { "connections": ["sms-work", "signal", "sms-work"] }
        }))
        .unwrap();
        assert_eq!(decision.connections, ["signal", "sms-work"]);
        assert_eq!(decision.networks, [Network::Signal, Network::Sms]);
        assert_eq!(decision.scope_key(), "signal,sms-work");
        // Both members: they must describe one perimeter.
        let disagree = request(json!({
            "subject": { "type": "contact", "id": "@whatsapp_33612345678:example.com" },
            "new_state": "granted",
            "scope": { "connections": ["sms-work"], "networks": ["signal"] }
        }));
        assert!(
            matches!(disagree, Err(Invalid::ScopeMembersDisagree { .. })),
            "{disagree:?}"
        );
        // A connection the registry does not know, and one whose kind has
        // no consent to decide (a mailbox is a network; a calendar is not,
        // and none is registered here, so the unknown id is the case).
        let unknown = request(json!({
            "subject": { "type": "contact", "id": "@whatsapp_33612345678:example.com" },
            "new_state": "granted",
            "scope": { "connections": ["wa-home"] }
        }));
        assert!(
            matches!(unknown, Err(Invalid::Unknown { ref field, .. }) if field == "scope.connections"),
            "{unknown:?}"
        );
        // A network default can be held on one connection of its kind.
        let default = request(json!({
            "subject": { "type": "network", "id": "sms" },
            "new_state": "revoked",
            "scope": { "connections": ["sms-work"] }
        }))
        .unwrap();
        assert_eq!(default.connections, ["sms-work"]);
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
                connections: vec!["whatsapp".to_owned()],
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
    fn a_persona_is_activated_on_exactly_the_networks_the_scope_names() {
        let decision = request(json!({
            "subject": { "type": "persona", "id": "assistant" },
            "new_state": "granted",
            "scope": { "networks": ["signal", "whatsapp"] }
        }))
        .unwrap();
        assert_eq!(decision.subject.kind, SubjectType::Persona);
        assert_eq!(decision.subject.id, "assistant");
        // Not "every network this deployment has": activation does not
        // spread, so a network connected later is in no scope the user
        // decided on (ADR 0013).
        assert_eq!(decision.networks, vec![Network::Signal, Network::Whatsapp]);
    }

    #[test]
    fn a_persona_subject_is_not_bound_to_a_network_of_the_same_name() {
        // The scope-contradiction rule is the *network* subject's alone: a
        // persona named `assistant` is scoped to whatever networks the user
        // gave it, and `whatsapp` is not its id.
        let decision = request(json!({
            "subject": { "type": "persona", "id": "assistant" },
            "new_state": "revoked",
            "scope": { "networks": ["whatsapp"] }
        }))
        .unwrap();
        assert_eq!(decision.new_state, State::Revoked);
    }

    #[test]
    fn a_subject_type_outside_the_contract_is_an_unknown_value() {
        let error = request(json!({
            "subject": { "type": "device", "id": "assistant" },
            "new_state": "granted",
            "scope": { "networks": ["whatsapp"] }
        }))
        .unwrap_err();
        assert_eq!(error.code(), "unknown_value");
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
    fn a_decision_about_an_owner_identity_is_refused_with_its_own_code() {
        let owner = crate::owner::Owner::new(
            "@michel:example.com",
            ["@whatsapp_lid-115332874281144:example.com".to_owned()],
        );
        for id in [
            // The ghost a deployment upgraded across #109 can already hold a
            // row about…
            "@whatsapp_lid-115332874281144:example.com",
            // …and the owner's own account, which is always an identity and
            // is the one a browser can name (#170).
            "@michel:example.com",
        ] {
            let decision = request(json!({
                "subject": { "type": "contact", "id": id },
                "new_state": "revoked",
                "scope": { "networks": ["whatsapp"] }
            }))
            .expect("the document is well formed: what refuses it is who the subject is");
            let refusal = decision.refuse_if_owner(&owner).unwrap_err();
            assert_eq!(refusal.code(), "subject_is_the_owner");
            assert_eq!(
                refusal.status(),
                409,
                "never 404: \"no such subject\" and \"that subject is you\" are two facts"
            );
            let message = refusal.message();
            assert!(
                message.contains(id) && message.contains("never has a consent state"),
                "the refusal says why, and about whom: {message}"
            );
        }
    }

    #[test]
    fn a_contact_the_deployment_has_not_confirmed_is_still_a_subject() {
        // Unknown is not the owner: one digit apart from a confirmed ghost is
        // a contact, and the user decides about them as usual.
        let owner = crate::owner::Owner::new(
            "@michel:example.com",
            ["@whatsapp_lid-115332874281144:example.com".to_owned()],
        );
        let decision = request(json!({
            "subject": { "type": "contact", "id": "@whatsapp_lid-115332874281145:example.com" },
            "new_state": "granted",
            "scope": { "networks": ["whatsapp"] }
        }))
        .unwrap();
        assert!(decision.refuse_if_owner(&owner).is_ok());
    }

    #[test]
    fn a_persona_or_a_network_named_like_the_owner_is_not_the_owner() {
        // The check is a `contact` subject's alone. A persona id is not a
        // Matrix user ID, so a collision there is a name and not a decision
        // about the user — refusing it would refuse a persona over the shape
        // of its name.
        let owner = crate::owner::Owner::new("@michel:example.com", []);
        let persona = request(json!({
            "subject": { "type": "persona", "id": "@michel:example.com" },
            "new_state": "granted",
            "scope": { "networks": ["whatsapp"] }
        }))
        .unwrap();
        assert!(persona.refuse_if_owner(&owner).is_ok());
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
