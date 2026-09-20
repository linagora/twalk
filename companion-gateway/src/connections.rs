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
//! one bridge per network; a second one is declared, never guessed.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::bridge::BridgeConfig;

/// What an id may look like: opaque, stable, URL- and subject-safe.
pub const ID_PATTERN: &str = "^[a-z0-9][a-z0-9-]{0,63}$";

/// The contract's definition of the kinds, compiled in: the one authority
/// (#268), carried by the binary rather than copied into it.
const KIND_DEFINITION: &str =
    include_str!("../../contracts/cloudevents/v1/definitions/kind.schema.json");

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

/// The registry: every connection, in the order declared.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registry {
    connections: Vec<Connection>,
}

impl Registry {
    /// The registry as configured: `GATEWAY_CONNECTIONS` when set, otherwise
    /// one connection per bridge whose id is the network's name.
    ///
    /// `declared` is the raw variable: `id=kind[=label]` entries, comma
    /// separated. A declared connection that names a bridge's network takes
    /// that bridge (and its bot) as its transport when it is the only one of
    /// that kind; two bridges of one kind must each be named by
    /// `bridge:<bridge_id>` as the label's transport, which is how a second
    /// WhatsApp account is declared rather than guessed.
    pub fn from_config(declared: Option<&str>, bridges: &[BridgeConfig]) -> Result<Self> {
        let kinds = kinds();
        let kinds = &kinds[..];
        let connections = match declared.map(str::trim).filter(|value| !value.is_empty()) {
            None => Self::derived_from_bridges(bridges),
            Some(declared) => Self::parse_declared(declared, bridges)?,
        };
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
                     letter or digit ({ID_PATTERN})",
                    connection.id
                );
            }
        }
        let mut seen = BTreeMap::new();
        for connection in &connections {
            if seen.insert(connection.id.clone(), ()).is_some() {
                bail!(
                    "GATEWAY_CONNECTIONS names the connection {:?} twice",
                    connection.id
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
            connections.push(Connection {
                id: bridge.network.clone(),
                kind: bridge.network.clone(),
                label: bridge.bridge_id.clone(),
                bridge_id: Some(bridge.bridge_id.clone()),
                bridge_bot: bridge.bot_user_id.clone(),
            });
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
                .map(str::to_owned);
            // The transport: `bridge:<bridge_id>` as the label names it
            // explicitly; otherwise the only bridge of that kind, if there is
            // exactly one.
            let (label, bridge) = match label.as_deref().and_then(|l| l.strip_prefix("bridge:")) {
                Some(bridge_id) => {
                    let bridge = bridges
                        .iter()
                        .find(|bridge| bridge.bridge_id == bridge_id)
                        .with_context(|| {
                            format!(
                                "GATEWAY_CONNECTIONS entry {entry:?} names the bridge \
                                 {bridge_id:?}, which GATEWAY_BRIDGES does not"
                            )
                        })?;
                    (bridge_id.to_owned(), Some(bridge))
                }
                None => {
                    let of_kind: Vec<&BridgeConfig> =
                        bridges.iter().filter(|b| b.network == kind).collect();
                    let bridge = if of_kind.len() == 1 {
                        Some(of_kind[0])
                    } else {
                        None
                    };
                    (label.unwrap_or_else(|| id.clone()), bridge)
                }
            };
            connections.push(Connection {
                id,
                kind,
                label,
                bridge_id: bridge.map(|b| b.bridge_id.clone()),
                bridge_bot: bridge.and_then(|b| b.bot_user_id.clone()),
            });
        }
        Ok(connections)
    }

    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }

    pub fn get(&self, id: &str) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id == id)
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

    #[test]
    fn the_kinds_compiled_in_are_the_contracts() {
        assert_eq!(
            kinds(),
            twalk_test_harness::contract_definition_values("kind").expect("the contract's kinds")
        );
    }

    #[test]
    fn with_nothing_declared_each_bridge_is_a_connection_named_after_its_network() {
        // The migration id (#270): every existing decision lands on it, which
        // is correct only because every deployment has one bridge per network
        // today.
        let registry = Registry::from_config(
            None,
            &[
                bridge("mautrix-whatsapp", "whatsapp", Some("@whatsappbot:x")),
                bridge("mautrix-signal", "signal", None),
            ],
        )
        .unwrap();
        let ids: Vec<&str> = registry
            .connections()
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(ids, ["whatsapp", "signal"]);
        assert_eq!(
            registry.get("whatsapp").unwrap().bridge_bot.as_deref(),
            Some("@whatsappbot:x")
        );
        assert_eq!(
            registry.get("signal").unwrap().bridge_id.as_deref(),
            Some("mautrix-signal")
        );
    }

    #[test]
    fn a_second_bridge_of_one_kind_is_never_derived_into_a_connection() {
        let bridges = [
            bridge("mautrix-whatsapp", "whatsapp", None),
            bridge("mautrix-whatsapp-work", "whatsapp", None),
        ];
        let registry = Registry::from_config(None, &bridges).unwrap();
        assert_eq!(
            registry.connections().len(),
            1,
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
    fn a_declaration_names_a_second_account_and_its_transport() {
        let bridges = [
            bridge("mautrix-whatsapp", "whatsapp", Some("@whatsappbot:x")),
            bridge("mautrix-whatsapp-work", "whatsapp", Some("@workbot:x")),
        ];
        let registry = Registry::from_config(
            Some("whatsapp=whatsapp=bridge:mautrix-whatsapp,wa-work=whatsapp=bridge:mautrix-whatsapp-work,mail-linagora=email=Twake Mail"),
            &bridges,
        )
        .unwrap();
        assert_eq!(registry.connections().len(), 3);
        assert_eq!(
            registry.get("wa-work").unwrap().bridge_bot.as_deref(),
            Some("@workbot:x")
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
    fn a_kind_the_contract_does_not_know_a_bad_id_and_a_duplicate_are_refused_loudly() {
        let refused = |declared: &str| {
            Registry::from_config(Some(declared), &[])
                .unwrap_err()
                .to_string()
        };
        assert!(refused("irc-home=irc").contains("kind.schema.json"));
        assert!(refused("Mail=email").contains("not a valid id"));
        assert!(refused("a=email,a=calendar").contains("twice"));
        assert!(refused("a").contains("has no kind"));
    }
}
