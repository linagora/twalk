//! The registry of **connections** (ADR 0033, issue #269): the perimeters a
//! consent decision is scoped to.
//!
//! A connection is one configured account the deployment observes or acts
//! through — a bridge, a mailbox, a calendar — with an opaque stable id, a
//! kind and a label. `network` was standing in for it and the substitution
//! had already cost a defect: a network is a *kind*, and a deployment with
//! two bridges of one kind has two perimeters that one word cannot tell
//! apart. So the id joins the envelope as the identity while the network
//! stays as the kind — two facts, two fields.
//!
//! The Gateway keeps the registry because it is the single writer of consent
//! (ADR 0022) and a perimeter is what consent is scoped to. Everything
//! downstream is **handed the registry and never derives it**: the Sensor
//! reads it off the consent snapshot and stamps every event with the
//! connection of the bridge that built the room, a collector starts with an
//! id and refuses to publish on one the registry does not know — ADR 0018's
//! rule for the owner's identities, applied to perimeters.
//!
//! On a deployment that names no connection (`GATEWAY_CONNECTIONS` unset)
//! the registry is derived once, here and nowhere else, from the bridges:
//! one connection per bridge, **whose id is the network's name**. That is
//! the id every existing consent decision is migrated onto (#270), and it is
//! correct only because it is done now, while every deployment has exactly
//! one bridge per network; a second one is declared, never guessed. The
//! native Matrix connection — the user's own account on the homeserver,
//! which the Sensor observes without any bridge (ADR 0009) — is in every
//! registry, declared or derived, because every deployment has it by
//! construction: the Sensor *is* an account on that homeserver.

use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::bridge::BridgeConfig;

/// The contract's definitions, compiled in: the one authority (#268),
/// carried by the binary rather than copied into it.
const KIND_DEFINITION: &str =
    include_str!("../../contracts/cloudevents/v1/definitions/kind.schema.json");
const CONNECTION_DEFINITION: &str =
    include_str!("../../contracts/cloudevents/v1/definitions/connection.schema.json");

/// The kind of the native Matrix connection, and its id: the network's name,
/// like every derived connection's.
const MATRIX: &str = "matrix";

/// Every kind the contract knows, in its order.
pub fn kinds() -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(KIND_DEFINITION)
        .ok()
        .and_then(|definition| {
            definition["enum"].as_array().map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// What an id may look like, as the contract spells it — quoted in the
/// refusal so the operator reads the rule, not a paraphrase of it.
pub fn id_pattern() -> String {
    serde_json::from_str::<serde_json::Value>(CONNECTION_DEFINITION)
        .ok()
        .and_then(|definition| definition["pattern"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// One connection of the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Connection {
    pub id: String,
    /// One of the contract's kinds (`definitions/kind.schema.json`): a
    /// network, or `calendar`.
    pub kind: String,
    pub label: String,
    /// For a connection a bridge carries: the bridge instance
    /// (`GATEWAY_BRIDGES`), and its bot when the operator named one — the
    /// account the Sensor recognises a portal by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge_bot: Option<String>,
}

impl Connection {
    fn carried_by(id: String, kind: String, label: String, bridge: Option<&BridgeConfig>) -> Self {
        Self {
            id,
            kind,
            label,
            bridge_id: bridge.map(|bridge| bridge.bridge_id.clone()),
            bridge_bot: bridge.and_then(|bridge| bridge.bot_user_id.clone()),
        }
    }
}

/// Why [`Registry::resolve`] could not name a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unresolved {
    /// An id was carried, and the registry does not know it.
    UnknownId(String),
    /// Nothing was carried, and the kind has no connection.
    NoneOfKind(String),
    /// Nothing was carried, and the kind has several: the registry will
    /// not pick one.
    SeveralOfKind { kind: String, ids: Vec<String> },
}

impl std::fmt::Display for Unresolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unresolved::UnknownId(id) => {
                write!(f, "the connection {id:?} is not in the registry")
            }
            Unresolved::NoneOfKind(kind) => {
                write!(f, "no connection of the kind {kind:?} is registered")
            }
            Unresolved::SeveralOfKind { kind, ids } => write!(
                f,
                "{} connections of the kind {kind:?} are registered ({}) and none was named",
                ids.len(),
                ids.join(", ")
            ),
        }
    }
}

/// The registry: every connection, in the order declared.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registry {
    connections: Vec<Connection>,
}

impl Registry {
    /// The registry as configured: `GATEWAY_CONNECTIONS` when set, otherwise
    /// one connection per bridge whose id is the network's name; and the
    /// native Matrix connection either way, labelled with the homeserver's
    /// name, unless the operator declared one.
    ///
    /// `declared` is the raw variable: `id=kind[=label]` entries, comma
    /// separated. The bridge a declared connection rides is the one whose
    /// `GATEWAY_BRIDGES` id is the connection's id, else the only bridge of
    /// its kind. That is how a second WhatsApp account is declared rather
    /// than guessed: two connections of one kind are each named after their
    /// bridge, and two that are not is refused, because the Sensor could
    /// not tell their portals apart.
    pub fn from_config(
        declared: Option<&str>,
        bridges: &[BridgeConfig],
        homeserver_name: &str,
    ) -> Result<Self> {
        let kinds = kinds();
        let mut connections = match declared.map(str::trim).filter(|value| !value.is_empty()) {
            None => Self::derived_from_bridges(bridges),
            Some(declared) => Self::parse_declared(declared, bridges)?,
        };
        if !connections
            .iter()
            .any(|connection| connection.kind == MATRIX)
        {
            connections.push(Connection::carried_by(
                MATRIX.to_owned(),
                MATRIX.to_owned(),
                homeserver_name.to_owned(),
                None,
            ));
        }
        for connection in &connections {
            if !kinds.iter().any(|kind| kind == &connection.kind) {
                bail!(
                    "GATEWAY_CONNECTIONS names the connection {:?} with the kind {:?}, which the \
                     contract does not know (contracts/cloudevents/v1/definitions/kind.schema.json \
                     names {})",
                    connection.id,
                    connection.kind,
                    kinds.join(", ")
                );
            }
            if !id_is_valid(&connection.id) {
                bail!(
                    "GATEWAY_CONNECTIONS names the connection {:?}, which is not a valid id: \
                     lower-case letters, digits and dashes, 1 to 64 characters, starting with a \
                     letter or digit ({})",
                    connection.id,
                    id_pattern()
                );
            }
        }
        let mut seen = BTreeSet::new();
        for connection in &connections {
            if !seen.insert(connection.id.as_str()) {
                bail!(
                    "GATEWAY_CONNECTIONS names the connection {:?} twice",
                    connection.id
                );
            }
        }
        // Two connections of one kind that ride no bridge of their own are
        // two the Sensor cannot tell apart: a portal names its bridge's bot,
        // and neither connection names one.
        for connection in &connections {
            if connection.bridge_id.is_some() {
                continue;
            }
            let of_kind: Vec<&Connection> = connections
                .iter()
                .filter(|other| other.kind == connection.kind)
                .collect();
            if of_kind.len() > 1 && bridges.iter().any(|b| b.network == connection.kind) {
                bail!(
                    "GATEWAY_CONNECTIONS names {} connections of the kind {:?} and {:?} is not \
                     named after the bridge that carries it: name each after its GATEWAY_BRIDGES \
                     id ({})",
                    of_kind.len(),
                    connection.kind,
                    connection.id,
                    bridges
                        .iter()
                        .filter(|b| b.network == connection.kind)
                        .map(|b| b.bridge_id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        // And two connections of one kind are told apart by their bridges'
        // **bots**, not by their bridge ids: the Sensor reads the bot a
        // portal's own `m.bridge` marker names and looks it up here, so a
        // bridge whose bot the operator did not name leaves its connection
        // unrecognisable — every one of its portals would resolve to
        // "several of this kind and none names this bot" and publish
        // nothing. That is a configuration error with a name, refused here
        // rather than discovered as a silence on the bus.
        for connection in &connections {
            let Some(bridge_id) = &connection.bridge_id else {
                continue;
            };
            let of_kind = connections
                .iter()
                .filter(|other| other.kind == connection.kind)
                .count();
            if of_kind > 1 && connection.bridge_bot.is_none() {
                bail!(
                    "GATEWAY_CONNECTIONS names {of_kind} connections of the kind {:?}, and the \
                     bridge {bridge_id:?} carrying {:?} names no bot: the Sensor tells two \
                     connections of one kind apart by the bot that built each portal, so set \
                     GATEWAY_BRIDGE_{}_BOT_USER_ID",
                    connection.kind,
                    connection.id,
                    crate::config::variable_slug(bridge_id)
                );
            }
        }
        Ok(Self { connections })
    }

    fn derived_from_bridges(bridges: &[BridgeConfig]) -> Vec<Connection> {
        let mut connections: Vec<Connection> = Vec::new();
        for bridge in bridges {
            // Two bridges of one network with no declaration: the first wins
            // the network's name and the second is not a connection at all —
            // `uncovered_bridges` names it and the Gateway says so at startup.
            // Never two connections nobody named.
            if connections.iter().any(|known| known.kind == bridge.network) {
                continue;
            }
            connections.push(Connection::carried_by(
                bridge.network.clone(),
                bridge.network.clone(),
                bridge.bridge_id.clone(),
                Some(bridge),
            ));
        }
        connections
    }

    fn parse_declared(declared: &str, bridges: &[BridgeConfig]) -> Result<Vec<Connection>> {
        let mut connections = Vec::new();
        for entry in declared.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let mut parts = entry.splitn(3, '=');
            let id = parts.next().unwrap_or_default().trim().to_owned();
            let kind = parts
                .next()
                .map(str::trim)
                .filter(|kind| !kind.is_empty())
                .with_context(|| {
                    format!(
                        "GATEWAY_CONNECTIONS entry {entry:?} has no kind: the form is \
                         id=kind or id=kind=label"
                    )
                })?
                .to_owned();
            let label = parts
                .next()
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| id.clone());
            // The transport: the bridge named like the connection, else the
            // only bridge of its kind. None for a mailbox or a calendar.
            let bridge = bridges
                .iter()
                .find(|bridge| bridge.bridge_id == id)
                .or_else(|| {
                    let mut of_kind = bridges.iter().filter(|bridge| bridge.network == kind);
                    match (of_kind.next(), of_kind.next()) {
                        (Some(only), None) => Some(only),
                        _ => None,
                    }
                });
            connections.push(Connection::carried_by(id, kind, label, bridge));
        }
        Ok(connections)
    }

    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }

    pub fn get(&self, id: &str) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id == id)
    }

    /// Every connection of a kind, in the order declared.
    pub fn of_kind(&self, kind: &str) -> Vec<&Connection> {
        self.connections.iter().filter(|c| c.kind == kind).collect()
    }

    /// The single connection of a kind, when the kind has exactly one: what
    /// an event that names its network but no connection resolves to — a
    /// lookup in the registry, not a derivation from the name.
    pub fn only_of_kind(&self, kind: &str) -> Option<&Connection> {
        match self.of_kind(kind).as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }

    /// The connection an event, a query or a decision is about: the id it
    /// carries when it carries one — which must be the registry's — else the
    /// single connection of the kind it names. The one resolver every read
    /// of the registry goes through (#270), so that the approval path, the
    /// pending-contact projection and the consent API cannot disagree about
    /// what an event older than #269 belongs to: a lookup, never a guess,
    /// and when the registry cannot say, [`Unresolved`] says why.
    pub fn resolve(&self, carried: Option<&str>, kind: &str) -> Result<&Connection, Unresolved> {
        if let Some(id) = carried.map(str::trim).filter(|id| !id.is_empty()) {
            return self
                .get(id)
                .ok_or_else(|| Unresolved::UnknownId(id.to_owned()));
        }
        let of_kind = self.of_kind(kind);
        match of_kind.as_slice() {
            [only] => Ok(only),
            [] => Err(Unresolved::NoneOfKind(kind.to_owned())),
            several => Err(Unresolved::SeveralOfKind {
                kind: kind.to_owned(),
                ids: several.iter().map(|c| c.id.clone()).collect(),
            }),
        }
    }

    /// The bridges no connection covers: a second bridge of a kind that was
    /// derived rather than declared. Its portals' traffic is not published
    /// until the operator names it, and the log says so at startup.
    pub fn uncovered_bridges<'a>(&self, bridges: &'a [BridgeConfig]) -> Vec<&'a BridgeConfig> {
        bridges
            .iter()
            .filter(|bridge| {
                !self
                    .connections
                    .iter()
                    .any(|c| c.bridge_id.as_deref() == Some(bridge.bridge_id.as_str()))
            })
            .collect()
    }
}

/// The contract's `pattern` for an id, by hand so the Gateway carries no
/// regex engine for one rule; the unit test holds it to the contract.
pub fn id_is_valid(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() || first.is_ascii_digit() => {}
        _ => return false,
    }
    id.len() <= 64 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge(bridge_id: &str, network: &str, bot: Option<&str>) -> BridgeConfig {
        BridgeConfig {
            acting_as: "@michel:example.com".to_owned(),
            bridge_id: bridge_id.to_owned(),
            status_bridge_id: bridge_id.to_owned(),
            network: network.to_owned(),
            base_url: "http://bridge.example".to_owned(),
            provisioning_secret: "secret".to_owned(),
            as_token: None,
            bot_user_id: bot.map(str::to_owned),
        }
    }

    fn ids(registry: &Registry) -> Vec<&str> {
        registry
            .connections()
            .iter()
            .map(|c| c.id.as_str())
            .collect()
    }

    #[test]
    fn the_kinds_compiled_in_are_the_contracts() {
        assert_eq!(
            kinds(),
            twalk_test_harness::contract_definition_values("kind").expect("the contract's kinds")
        );
    }

    #[test]
    fn the_id_check_agrees_with_the_contracts_pattern() {
        // The contract's `definitions/connection.schema.json` is the one
        // authority on what an id looks like; the check here is a hand copy
        // so the Gateway needs no regex engine, and this is what keeps the
        // copy honest: the contract's own validator and the copy must agree
        // on every candidate, including the ones at the edges.
        let definition =
            twalk_test_harness::contract_definition("connection").expect("the definition");
        let validator = jsonschema::validator_for(&definition).expect("a valid schema");
        for candidate in [
            "whatsapp",
            "mail-linagora",
            "a",
            "0",
            &"a".repeat(64),
            &"a".repeat(65),
            "",
            "-a",
            "Mail",
            "a_b",
            "a.b",
            "a b",
            "é",
        ] {
            assert_eq!(
                id_is_valid(candidate),
                validator.is_valid(&serde_json::json!(candidate)),
                "{candidate:?}: the hand copy and the contract disagree"
            );
        }
    }

    #[test]
    fn with_nothing_declared_each_bridge_is_a_connection_named_after_its_network() {
        // The migration id (#270): every existing decision lands on it, which
        // is correct only because every deployment has one bridge per network
        // today. And the native Matrix connection, which every deployment
        // has by construction and no bridge carries.
        let registry = Registry::from_config(
            None,
            &[
                bridge("mautrix-whatsapp", "whatsapp", Some("@whatsappbot:x")),
                bridge("mautrix-signal", "signal", None),
            ],
            "matrix.example.com",
        )
        .unwrap();
        assert_eq!(ids(&registry), ["whatsapp", "signal", "matrix"]);
        assert_eq!(
            registry.get("whatsapp").unwrap().bridge_bot.as_deref(),
            Some("@whatsappbot:x")
        );
        assert_eq!(
            registry.get("signal").unwrap().bridge_id.as_deref(),
            Some("mautrix-signal")
        );
        let matrix = registry.get("matrix").unwrap();
        assert_eq!(
            (
                matrix.kind.as_str(),
                matrix.label.as_str(),
                matrix.bridge_id.is_none(),
                matrix.bridge_bot.is_none()
            ),
            ("matrix", "matrix.example.com", true, true)
        );
    }

    #[test]
    fn a_deployment_with_no_bridge_still_has_its_native_matrix_connection() {
        let registry = Registry::from_config(None, &[], "matrix.example.com").unwrap();
        assert_eq!(ids(&registry), ["matrix"]);
    }

    #[test]
    fn a_second_bridge_of_one_kind_is_never_derived_into_a_connection() {
        let bridges = [
            bridge("mautrix-whatsapp", "whatsapp", None),
            bridge("mautrix-whatsapp-work", "whatsapp", None),
        ];
        let registry = Registry::from_config(None, &bridges, "x").unwrap();
        assert_eq!(
            ids(&registry),
            ["whatsapp", "matrix"],
            "the first wins the name, the second is nobody's"
        );
        assert_eq!(
            registry
                .uncovered_bridges(&bridges)
                .iter()
                .map(|b| b.bridge_id.as_str())
                .collect::<Vec<_>>(),
            ["mautrix-whatsapp-work"]
        );
    }

    #[test]
    fn a_declaration_names_a_second_account_and_the_bridge_that_carries_it() {
        // Two bridges of one kind: each connection is named after the bridge
        // it rides, which is what ties it to that bridge's bot. A connection
        // of a kind with exactly one bridge takes that bridge without being
        // named after it, and a connection no bridge carries has none.
        let bridges = [
            bridge("mautrix-whatsapp", "whatsapp", Some("@whatsappbot:x")),
            bridge("mautrix-whatsapp-work", "whatsapp", Some("@workbot:x")),
            bridge("mautrix-signal", "signal", Some("@signalbot:x")),
        ];
        let registry = Registry::from_config(
            Some(
                "mautrix-whatsapp=whatsapp=Home,mautrix-whatsapp-work=whatsapp=Work,\
                 signal=signal,mail-linagora=email=Twake Mail",
            ),
            &bridges,
            "matrix.example.com",
        )
        .unwrap();
        assert_eq!(
            ids(&registry),
            [
                "mautrix-whatsapp",
                "mautrix-whatsapp-work",
                "signal",
                "mail-linagora",
                "matrix"
            ]
        );
        assert_eq!(
            registry
                .get("mautrix-whatsapp-work")
                .unwrap()
                .bridge_bot
                .as_deref(),
            Some("@workbot:x")
        );
        assert_eq!(registry.get("mautrix-whatsapp").unwrap().label, "Home");
        let signal = registry.get("signal").unwrap();
        assert_eq!(
            (signal.label.as_str(), signal.bridge_bot.as_deref()),
            ("signal", Some("@signalbot:x"))
        );
        let mail = registry.get("mail-linagora").unwrap();
        assert_eq!(
            (
                mail.kind.as_str(),
                mail.label.as_str(),
                mail.bridge_id.is_none()
            ),
            ("email", "Twake Mail", true)
        );
        assert!(registry.uncovered_bridges(&bridges).is_empty());
    }

    #[test]
    fn the_resolver_takes_what_is_carried_else_the_kinds_only_one_and_never_guesses() {
        let registry = Registry::from_config(
            Some("wa-home=whatsapp,wa-work=whatsapp,signal=signal"),
            &[],
            "example.com",
        )
        .unwrap();
        assert_eq!(
            registry.resolve(Some(" wa-work "), "whatsapp").unwrap().id,
            "wa-work"
        );
        assert_eq!(registry.resolve(None, "signal").unwrap().id, "signal");
        assert_eq!(registry.resolve(Some(""), "matrix").unwrap().id, "matrix");
        assert_eq!(
            registry.resolve(Some("wa-old"), "whatsapp").unwrap_err(),
            Unresolved::UnknownId("wa-old".to_owned())
        );
        assert_eq!(
            registry.resolve(None, "telegram").unwrap_err(),
            Unresolved::NoneOfKind("telegram".to_owned())
        );
        assert_eq!(
            registry.resolve(None, "whatsapp").unwrap_err(),
            Unresolved::SeveralOfKind {
                kind: "whatsapp".to_owned(),
                ids: vec!["wa-home".to_owned(), "wa-work".to_owned()]
            }
        );
    }

    #[test]
    fn a_declared_matrix_connection_is_the_operators_and_not_added_twice() {
        let registry =
            Registry::from_config(Some("matrix=matrix=Own homeserver"), &[], "x").unwrap();
        assert_eq!(ids(&registry), ["matrix"]);
        assert_eq!(registry.get("matrix").unwrap().label, "Own homeserver");
    }

    #[test]
    fn two_declared_connections_of_one_kind_that_no_bridge_tells_apart_are_refused() {
        // Neither is named after a bridge, and the kind has two: the
        // registry would hand the Sensor two connections it cannot tell
        // apart, so the operator is told to name them after their bridges.
        let bridges = [
            bridge("mautrix-whatsapp", "whatsapp", Some("@whatsappbot:x")),
            bridge("mautrix-whatsapp-work", "whatsapp", Some("@workbot:x")),
        ];
        let refused =
            Registry::from_config(Some("wa-home=whatsapp,wa-work=whatsapp"), &bridges, "x")
                .unwrap_err()
                .to_string();
        assert!(refused.contains("GATEWAY_CONNECTIONS"), "{refused}");
        assert!(refused.contains("mautrix-whatsapp-work"), "{refused}");
    }

    #[test]
    fn two_connections_of_one_kind_need_both_bridges_bots_named() {
        // Each connection rides its own bridge, so the bridge-id rule is
        // satisfied — and the Sensor still could not tell the second one's
        // portals from the first's, because it looks a portal up by the bot
        // that built it and one bridge names none. Refused at startup, naming
        // the variable, rather than found as a silence on the bus.
        let bridges = [
            bridge("mautrix-whatsapp", "whatsapp", Some("@whatsappbot:x")),
            bridge("mautrix-whatsapp-work", "whatsapp", None),
        ];
        let refused = Registry::from_config(
            Some("mautrix-whatsapp=whatsapp,mautrix-whatsapp-work=whatsapp"),
            &bridges,
            "x",
        )
        .unwrap_err()
        .to_string();
        assert!(
            refused.contains("GATEWAY_BRIDGE_MAUTRIX_WHATSAPP_WORK_BOT_USER_ID"),
            "{refused}"
        );
        // One connection of a kind needs no bot at all: the kind is enough.
        Registry::from_config(Some("mautrix-whatsapp-work=whatsapp"), &bridges[1..], "x")
            .expect("a single connection of a kind resolves by its kind");
    }

    #[test]
    fn a_kind_the_contract_does_not_know_a_bad_id_and_a_duplicate_are_refused_naming_the_variable()
    {
        let refused = |declared: &str| {
            Registry::from_config(Some(declared), &[], "x")
                .unwrap_err()
                .to_string()
        };
        for (declared, reason) in [
            ("irc-home=irc", "kind.schema.json"),
            ("Mail=email", "not a valid id"),
            ("a=email,a=calendar", "twice"),
            ("a", "has no kind"),
        ] {
            let message = refused(declared);
            assert!(message.contains(reason), "{declared}: {message}");
            assert!(
                message.contains("GATEWAY_CONNECTIONS"),
                "{declared}: {message}"
            );
        }
    }
}
