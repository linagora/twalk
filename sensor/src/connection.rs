//! Which **connection** an observed event belongs to (ADR 0033, issue #269).
//!
//! A connection is one configured account the deployment observes through —
//! for the Sensor, a bridge — by the opaque id the deployment gave it.
//! `network` says what kind of thing it is; the connection says which one,
//! and a consent decision is scoped to it. The Sensor stamps every event it
//! publishes with one, and it **never derives an id**: the registry is the
//! Companion Gateway's, served on the consent snapshot, and this module only
//! looks an event's room up in it — the same rule ADR 0018 set for the
//! owner's identities, handed over and not inferred.
//!
//! The lookup is by the **bridge bot** that built the room, read from the
//! portal's own `m.bridge` marker: a registry entry that names that bot is
//! the answer. An entry that names no bot — the operator configured none —
//! is the answer when it is the only connection of the room's kind, which
//! is every deployment today. Two connections of one kind with no bot on
//! either is a registry that cannot tell them apart, and the event is not
//! published rather than published under a guess: a message attributed to
//! the wrong perimeter is a message a decision about the *other* perimeter
//! would govern. Native Matrix traffic is the `matrix` connection.
//!
//! A deployment with no Gateway at all has no registry to be handed; it has
//! exactly one connection per network by construction, and the implicit
//! registry below names each after its network — the same ids the Gateway
//! would derive, and the same ids every consent decision was migrated onto.

use serde_json::Value;

use crate::network::Network;

/// One connection, as the snapshot spells it (`GET /api/consent/snapshot`,
/// `connections[]`, the `Connection` schema of `openapi.yaml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub id: String,
    pub kind: String,
    /// The bridge's bot, when the operator named one.
    pub bridge_bot: Option<String>,
}

/// The registry, as handed over — or the implicit one of a deployment with
/// nobody to hand it over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    connections: Vec<Connection>,
    /// Whether this registry was read from a Gateway. An implicit registry
    /// says so in the log, once, because it is the one thing here that is
    /// derived — and it is correct only while a deployment without a
    /// Gateway has one bridge per network.
    handed_over: bool,
}

/// What the lookup found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The connection to stamp.
    Connection(String),
    /// No entry names this room's bot and the room's kind has no single
    /// entry to fall back on: not published, counted.
    Unknown { reason: &'static str },
}

impl Registry {
    /// The connections a snapshot carries. A snapshot with no `connections`
    /// member is a Gateway older than #269, which is read as an implicit
    /// registry rather than as an empty one: an empty one would publish
    /// nothing at all.
    pub fn from_snapshot(document: &Value) -> Self {
        let Some(entries) = document.get("connections").and_then(Value::as_array) else {
            return Self::implicit();
        };
        let connections = entries
            .iter()
            .filter_map(|entry| {
                Some(Connection {
                    id: entry.get("id")?.as_str()?.to_owned(),
                    kind: entry.get("kind")?.as_str()?.to_owned(),
                    bridge_bot: entry
                        .get("bridge_bot")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect();
        Self {
            connections,
            handed_over: true,
        }
    }

    /// One connection per network, named after it: what a deployment with
    /// no Gateway has by construction.
    pub fn implicit() -> Self {
        Self {
            connections: Network::ALL
                .iter()
                .map(|network| Connection {
                    id: network.as_str().to_owned(),
                    kind: network.as_str().to_owned(),
                    bridge_bot: None,
                })
                .collect(),
            handed_over: false,
        }
    }

    pub fn handed_over(&self) -> bool {
        self.handed_over
    }

    pub fn len(&self) -> usize {
        self.connections.len()
    }

    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }

    /// The connection of a room: by its bridge bot when the registry names
    /// it, else the single connection of the room's kind. `bridge_bot` is the
    /// `bridgebot` the room's own `m.bridge` marker names — `None` for a
    /// native Matrix room, whose kind is `matrix`.
    pub fn resolve(&self, bridge_bot: Option<&str>, network: Network) -> Resolution {
        if let Some(bot) = bridge_bot {
            if let Some(connection) = self
                .connections
                .iter()
                .find(|connection| connection.bridge_bot.as_deref() == Some(bot))
            {
                return Resolution::Connection(connection.id.clone());
            }
        }
        let of_kind: Vec<&Connection> = self
            .connections
            .iter()
            .filter(|connection| connection.kind == network.as_str())
            .collect();
        match of_kind.as_slice() {
            [only] => Resolution::Connection(only.id.clone()),
            [] => Resolution::Unknown {
                reason: "no connection of this kind is registered",
            },
            _ => Resolution::Unknown {
                reason: "several connections of this kind are registered and none names this \
                         room's bridge bot",
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn handed(connections: Value) -> Registry {
        Registry::from_snapshot(&json!({ "connections": connections }))
    }

    #[test]
    fn a_room_is_the_connection_whose_bot_built_it() {
        let registry = handed(json!([
            { "id": "wa-home", "kind": "whatsapp", "bridge_bot": "@homebot:x" },
            { "id": "wa-work", "kind": "whatsapp", "bridge_bot": "@workbot:x" },
        ]));
        assert_eq!(
            registry.resolve(Some("@workbot:x"), Network::Whatsapp),
            Resolution::Connection("wa-work".to_owned())
        );
        assert_eq!(
            registry.resolve(Some("@homebot:x"), Network::Whatsapp),
            Resolution::Connection("wa-home".to_owned())
        );
    }

    #[test]
    fn the_only_connection_of_a_kind_answers_when_no_bot_is_named() {
        // The reference deployment: one bridge per network, bots unnamed on
        // the Gateway side. The room's kind is enough, and not a guess.
        let registry = handed(json!([
            { "id": "whatsapp", "kind": "whatsapp" },
            { "id": "signal", "kind": "signal" },
            { "id": "matrix", "kind": "matrix" },
        ]));
        assert_eq!(
            registry.resolve(Some("@whatsappbot:x"), Network::Whatsapp),
            Resolution::Connection("whatsapp".to_owned())
        );
        assert_eq!(
            registry.resolve(None, Network::Matrix),
            Resolution::Connection("matrix".to_owned())
        );
    }

    #[test]
    fn two_connections_of_one_kind_and_an_unnamed_bot_publish_nothing() {
        // Never a guess: the wrong perimeter is the one a decision about the
        // *other* account would govern.
        let registry = handed(json!([
            { "id": "wa-home", "kind": "whatsapp", "bridge_bot": "@homebot:x" },
            { "id": "wa-work", "kind": "whatsapp" },
        ]));
        assert!(matches!(
            registry.resolve(Some("@stranger:x"), Network::Whatsapp),
            Resolution::Unknown { .. }
        ));
        assert!(matches!(
            registry.resolve(Some("@bot:x"), Network::Signal),
            Resolution::Unknown { reason } if reason.starts_with("no connection")
        ));
    }

    #[test]
    fn a_gateway_older_than_the_registry_and_no_gateway_at_all_read_as_the_implicit_one() {
        let older = Registry::from_snapshot(&json!({ "entries": [] }));
        assert!(!older.handed_over());
        assert_eq!(older, Registry::implicit());
        // Named after the network, which is the migration id every decision
        // landed on.
        assert_eq!(
            older.resolve(Some("@whatsappbot:x"), Network::Whatsapp),
            Resolution::Connection("whatsapp".to_owned())
        );
        assert_eq!(
            older.resolve(None, Network::Matrix),
            Resolution::Connection("matrix".to_owned())
        );
    }

    #[test]
    fn an_empty_registry_handed_over_is_not_the_implicit_one() {
        // "No connection configured" and "a Gateway that has none to hand
        // over" are two facts; the first publishes nothing, and says why.
        let empty = handed(json!([]));
        assert!(empty.handed_over());
        assert!(matches!(
            empty.resolve(None, Network::Matrix),
            Resolution::Unknown { .. }
        ));
    }
}
