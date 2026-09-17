//! Normalization: turn an observed Matrix event into a CloudEvents envelope
//! conforming to `contracts/cloudevents/v1/`. Pure functions, no I/O.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::consent::Consent;
use crate::network::Network;

pub const MESSAGE_RECEIVED_TYPE: &str = "fr.linagora.twalk.inbound.message.received.v1";
pub const MESSAGE_RECEIVED_DATASCHEMA: &str =
    "https://schemas.twalk.dev/cloudevents/v1/inbound.message.received.schema.json";

pub const REACTION_ADDED_TYPE: &str = "fr.linagora.twalk.inbound.reaction.added.v1";
pub const REACTION_ADDED_DATASCHEMA: &str =
    "https://schemas.twalk.dev/cloudevents/v1/inbound.reaction.added.schema.json";

pub const PRESENCE_UPDATED_TYPE: &str = "fr.linagora.twalk.inbound.presence.updated.v1";
pub const PRESENCE_UPDATED_DATASCHEMA: &str =
    "https://schemas.twalk.dev/cloudevents/v1/inbound.presence.updated.schema.json";

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

/// Deterministic CloudEvents id: sha256(matrix_event_id + ':' + matrix_room_id),
/// the contract's natural key for inbound events. Replaying the same Matrix
/// event always yields the same id, so consumers can deduplicate.
pub fn cloud_event_id(matrix_event_id: &str, matrix_room_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(matrix_event_id.as_bytes());
    hasher.update(b":");
    hasher.update(matrix_room_id.as_bytes());
    hex_encode(hasher.finalize())
}

/// Deterministic CloudEvents id for presence updates:
/// sha256(matrix_user_id + ':' + presence + ':' + receipt_timestamp_ms), the
/// contract's natural key. Presence EDUs carry no Matrix event id; the
/// receipt timestamp is what keeps bus replays idempotent.
pub fn presence_event_id(
    matrix_user_id: &str,
    presence: Presence,
    receipt_timestamp_ms: u64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(matrix_user_id.as_bytes());
    hasher.update(b":");
    hasher.update(presence.as_str().as_bytes());
    hasher.update(b":");
    hasher.update(receipt_timestamp_ms.to_string().as_bytes());
    hex_encode(hasher.finalize())
}

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// The contract caps several strings (reaction at 64, display_name at 256,
/// excerpt at 512, ...): truncate on a char boundary so every published
/// event stays schema-valid whatever the network sends.
pub fn cap_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// The contract caps `target.excerpt` at 512 characters.
pub fn excerpt(body: &str) -> String {
    cap_chars(body, 512)
}

/// Everything needed to build an `inbound.message.received.v1` envelope,
/// already resolved at the seam.
pub struct InboundMessage {
    pub matrix_event_id: String,
    pub matrix_room_id: String,
    /// Server name of the Sensor's homeserver (the `source` authority).
    pub server_name: String,
    /// Matrix user ID of the observed sender (the CloudEvents `subject`).
    pub sender: String,
    pub body: String,
    pub network: Network,
    pub consent: Consent,
    pub display_name: String,
    /// RFC 3339 timestamp of when the Sensor produced the event.
    pub produced_at: String,
    /// RFC 3339 timestamp reported by the source network, when the bridge
    /// provides one (the homeserver's receive time is NOT a network
    /// timestamp and must never fill this field).
    pub network_timestamp: Option<String>,
}

pub fn build_message_received(input: &InboundMessage) -> Value {
    let mut data = json!({
        "body": input.body,
        "format": "text/plain",
        "reply_to": Value::Null,
        "attachments": [],
        "contact": { "display_name": cap_chars(&input.display_name, 256) },
    });
    if let Some(network_timestamp) = &input.network_timestamp {
        data["network_timestamp"] = json!(network_timestamp);
    }
    json!({
        "specversion": "1.0",
        "id": cloud_event_id(&input.matrix_event_id, &input.matrix_room_id),
        "source": format!("matrix://{}/{}", input.server_name, input.matrix_room_id),
        "type": MESSAGE_RECEIVED_TYPE,
        "time": input.produced_at,
        "subject": input.sender,
        "datacontenttype": "application/json",
        "dataschema": MESSAGE_RECEIVED_DATASCHEMA,
        "network": input.network.as_str(),
        "consent": input.consent.as_str(),
        "data": data,
    })
}

/// Everything needed to build an `inbound.reaction.added.v1` envelope,
/// already resolved at the seam.
pub struct InboundReaction {
    /// Event id of the `m.reaction` event itself (the natural key).
    pub matrix_event_id: String,
    pub matrix_room_id: String,
    /// Server name of the Sensor's homeserver (the `source` authority).
    pub server_name: String,
    /// Matrix user ID of the reactor (the CloudEvents `subject`).
    pub reactor: String,
    /// The reaction key (a Unicode emoji or a network-specific key).
    pub reaction: String,
    /// Event id of the message the reaction applies to.
    pub target_event_id: String,
    /// Excerpt of the target message, when it could be fetched.
    pub target_excerpt: Option<String>,
    pub network: Network,
    pub consent: Consent,
    pub display_name: String,
    /// RFC 3339 timestamp of when the Sensor produced the event.
    pub produced_at: String,
    /// RFC 3339 timestamp reported by the source network, when the bridge
    /// provides one.
    pub network_timestamp: Option<String>,
}

pub fn build_reaction_added(input: &InboundReaction) -> Value {
    let mut target = json!({ "matrix_event_id": input.target_event_id });
    if let Some(excerpt) = &input.target_excerpt {
        target["excerpt"] = json!(excerpt);
    }
    let mut data = json!({
        "reaction": cap_chars(&input.reaction, 64),
        "target": target,
        "contact": { "display_name": cap_chars(&input.display_name, 256) },
    });
    if let Some(network_timestamp) = &input.network_timestamp {
        data["network_timestamp"] = json!(network_timestamp);
    }
    json!({
        "specversion": "1.0",
        "id": cloud_event_id(&input.matrix_event_id, &input.matrix_room_id),
        "source": format!("matrix://{}/{}", input.server_name, input.matrix_room_id),
        "type": REACTION_ADDED_TYPE,
        "time": input.produced_at,
        "subject": input.reactor,
        "datacontenttype": "application/json",
        "dataschema": REACTION_ADDED_DATASCHEMA,
        "network": input.network.as_str(),
        "consent": input.consent.as_str(),
        "data": data,
    })
}

/// A contact's connectivity on a network, normalized across networks (the
/// contract's `data.presence` enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Online,
    Offline,
    Unavailable,
}

impl Presence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Everything needed to build an `inbound.presence.updated.v1` envelope,
/// already resolved at the seam.
pub struct InboundPresence {
    /// Matrix user ID of the contact whose presence changed (the CloudEvents
    /// `subject`).
    pub matrix_user_id: String,
    pub presence: Presence,
    /// Server name of the Sensor's homeserver (the `source` authority).
    pub server_name: String,
    /// The observed portal room chosen as the event's `source`. Matrix
    /// presence updates are not room-scoped, so the binary picks one
    /// observed room shared with the contact.
    pub matrix_room_id: String,
    pub network: Network,
    pub consent: Consent,
    pub display_name: String,
    /// RFC 3339 timestamp of when the Sensor produced the event: the same
    /// instant as `receipt_timestamp_ms`, so consumers can recompute the id.
    pub produced_at: String,
    /// Receipt time of the presence update in milliseconds since the epoch.
    /// Presence EDUs carry no server timestamp, so the Sensor's receipt
    /// instant is the natural key's receipt timestamp.
    pub receipt_timestamp_ms: u64,
    /// RFC 3339 timestamp of when the contact was last active, when the
    /// observed presence update reports it.
    pub last_active_at: Option<String>,
}

pub fn build_presence_updated(input: &InboundPresence) -> Value {
    let mut data = json!({
        "presence": input.presence.as_str(),
        "contact": { "display_name": cap_chars(&input.display_name, 256) },
    });
    if let Some(last_active_at) = &input.last_active_at {
        data["last_active_at"] = json!(last_active_at);
    }
    json!({
        "specversion": "1.0",
        "id": presence_event_id(
            &input.matrix_user_id,
            input.presence,
            input.receipt_timestamp_ms
        ),
        "source": format!("matrix://{}/{}", input.server_name, input.matrix_room_id),
        "type": PRESENCE_UPDATED_TYPE,
        "time": input.produced_at,
        "subject": input.matrix_user_id,
        "datacontenttype": "application/json",
        "dataschema": PRESENCE_UPDATED_DATASCHEMA,
        "network": input.network.as_str(),
        "consent": input.consent.as_str(),
        "data": data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_deterministic_id_from_the_natural_key() {
        // Known vector, computed independently from the contract formula:
        // sha256("$AbCdEfGh1234:!abcXYZ123:example.com").
        assert_eq!(
            cloud_event_id("$AbCdEfGh1234", "!abcXYZ123:example.com"),
            "20be32e73506b9104a6a1bf76fc2d2a15cbd2b8a0a421833be01f865ca9886d0"
        );
    }

    #[test]
    fn maps_contract_types_to_bus_subjects() {
        assert_eq!(
            bus_subject("fr.linagora.twalk.inbound.message.received.v1"),
            "twalk.inbound.message.received.v1"
        );
        assert_eq!(
            bus_subject("fr.linagora.twalk.persona.suggest.produced.v1"),
            "twalk.persona.suggest.produced.v1"
        );
    }

    fn sample_input() -> InboundMessage {
        InboundMessage {
            matrix_event_id: "$AbCdEfGh1234".to_owned(),
            matrix_room_id: "!abcXYZ123:example.com".to_owned(),
            server_name: "example.com".to_owned(),
            sender: "@whatsapp_33612345678:example.com".to_owned(),
            body: "hello".to_owned(),
            network: Network::Whatsapp,
            consent: Consent::Pending,
            display_name: "Aïcha".to_owned(),
            produced_at: "2026-09-17T10:00:00Z".to_owned(),
            network_timestamp: None,
        }
    }

    #[test]
    fn envelope_matches_the_contract_shape() {
        let event = build_message_received(&sample_input());
        assert_eq!(event["specversion"], "1.0");
        assert_eq!(
            event["id"],
            "20be32e73506b9104a6a1bf76fc2d2a15cbd2b8a0a421833be01f865ca9886d0"
        );
        assert_eq!(
            event["source"],
            "matrix://example.com/!abcXYZ123:example.com"
        );
        assert_eq!(event["type"], MESSAGE_RECEIVED_TYPE);
        assert_eq!(event["subject"], "@whatsapp_33612345678:example.com");
        assert_eq!(event["network"], "whatsapp");
        assert_eq!(event["consent"], "pending");
        assert_eq!(event["data"]["body"], "hello");
        assert_eq!(event["data"]["reply_to"], Value::Null);
        assert_eq!(event["data"]["attachments"], json!([]));
    }

    #[test]
    fn network_timestamp_is_omitted_when_unknown() {
        let event = build_message_received(&sample_input());
        assert!(event["data"].get("network_timestamp").is_none());
    }

    #[test]
    fn network_timestamp_is_included_when_known() {
        let mut input = sample_input();
        input.network_timestamp = Some("2026-09-17T09:59:58Z".to_owned());
        let event = build_message_received(&input);
        assert_eq!(event["data"]["network_timestamp"], "2026-09-17T09:59:58Z");
    }

    #[test]
    fn excerpt_keeps_short_bodies_intact() {
        assert_eq!(excerpt("on décale à 20h ?"), "on décale à 20h ?");
        assert_eq!(excerpt(""), "");
    }

    #[test]
    fn excerpt_truncates_at_the_contract_limit_on_a_char_boundary() {
        let long = "é".repeat(600);
        let excerpted = excerpt(&long);
        assert_eq!(excerpted.chars().count(), 512);
        assert_eq!(excerpt("x".repeat(513).as_str()).chars().count(), 512);
    }

    #[test]
    fn contract_string_caps_are_enforced() {
        // Reaction keys are capped at 64 chars by the contract.
        let mut input = sample_reaction();
        input.reaction = "👍".repeat(100);
        let event = build_reaction_added(&input);
        assert_eq!(event["data"]["reaction"].as_str().unwrap().chars().count(), 64);
        // Display names are capped at 256 chars.
        let mut message = sample_input();
        message.display_name = "x".repeat(300);
        let event = build_message_received(&message);
        assert_eq!(
            event["data"]["contact"]["display_name"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            256
        );
    }

    fn sample_reaction() -> InboundReaction {
        InboundReaction {
            matrix_event_id: "$ReAcTiOn5678".to_owned(),
            matrix_room_id: "!abcXYZ123:example.com".to_owned(),
            server_name: "example.com".to_owned(),
            reactor: "@whatsapp_33612345678:example.com".to_owned(),
            reaction: "👍".to_owned(),
            target_event_id: "$AbCdEfGh1234".to_owned(),
            target_excerpt: Some("On décale à 20h ?".to_owned()),
            network: Network::Whatsapp,
            consent: Consent::Pending,
            display_name: "Aïcha".to_owned(),
            produced_at: "2026-09-17T10:12:00Z".to_owned(),
            network_timestamp: None,
        }
    }

    fn sample_presence() -> InboundPresence {
        InboundPresence {
            matrix_user_id: "@whatsapp_33612345678:example.com".to_owned(),
            presence: Presence::Online,
            server_name: "example.com".to_owned(),
            matrix_room_id: "!abcXYZ123:example.com".to_owned(),
            network: Network::Whatsapp,
            consent: Consent::Pending,
            display_name: "Aïcha".to_owned(),
            produced_at: "2026-09-17T10:00:00.000Z".to_owned(),
            receipt_timestamp_ms: 1758000000000,
            last_active_at: None,
        }
    }

    #[test]
    fn reaction_envelope_matches_the_contract_shape() {
        let event = build_reaction_added(&sample_reaction());
        assert_eq!(event["specversion"], "1.0");
        assert_eq!(
            event["id"],
            cloud_event_id("$ReAcTiOn5678", "!abcXYZ123:example.com")
        );
        assert_eq!(
            event["source"],
            "matrix://example.com/!abcXYZ123:example.com"
        );
        assert_eq!(event["type"], REACTION_ADDED_TYPE);
        assert_eq!(event["dataschema"], REACTION_ADDED_DATASCHEMA);
        assert_eq!(event["subject"], "@whatsapp_33612345678:example.com");
        assert_eq!(event["network"], "whatsapp");
        assert_eq!(event["consent"], "pending");
        assert_eq!(event["data"]["reaction"], "👍");
        assert_eq!(event["data"]["target"]["matrix_event_id"], "$AbCdEfGh1234");
        assert_eq!(event["data"]["target"]["excerpt"], "On décale à 20h ?");
        assert_eq!(event["data"]["contact"]["display_name"], "Aïcha");
        assert!(event["data"].get("network_timestamp").is_none());
    }

    #[test]
    fn reaction_target_omits_the_excerpt_when_unknown() {
        let mut input = sample_reaction();
        input.target_excerpt = None;
        let event = build_reaction_added(&input);
        assert_eq!(event["data"]["target"]["matrix_event_id"], "$AbCdEfGh1234");
        assert!(event["data"]["target"].get("excerpt").is_none());
    }

    #[test]
    fn presence_states_match_the_contract_strings() {
        assert_eq!(Presence::Online.as_str(), "online");
        assert_eq!(Presence::Offline.as_str(), "offline");
        assert_eq!(Presence::Unavailable.as_str(), "unavailable");
    }

    #[test]
    fn derives_presence_id_from_the_natural_key() {
        // Known vector, computed independently from the contract formula:
        // sha256("@whatsapp_33612345678:example.com:online:1758000000000").
        assert_eq!(
            presence_event_id(
                "@whatsapp_33612345678:example.com",
                Presence::Online,
                1758000000000
            ),
            "89a9e3a32a9df41c5d7eb2c9e17b6f5c67fc307fcbfd35721108d88621993c87"
        );
    }

    #[test]
    fn presence_envelope_matches_the_contract_shape() {
        let event = build_presence_updated(&sample_presence());
        assert_eq!(event["specversion"], "1.0");
        assert_eq!(
            event["id"],
            "89a9e3a32a9df41c5d7eb2c9e17b6f5c67fc307fcbfd35721108d88621993c87"
        );
        assert_eq!(
            event["source"],
            "matrix://example.com/!abcXYZ123:example.com"
        );
        assert_eq!(event["type"], PRESENCE_UPDATED_TYPE);
        assert_eq!(event["subject"], "@whatsapp_33612345678:example.com");
        assert_eq!(event["network"], "whatsapp");
        assert_eq!(event["consent"], "pending");
        assert_eq!(event["data"]["presence"], "online");
        assert_eq!(event["data"]["contact"]["display_name"], "Aïcha");
        assert!(
            event["data"].get("last_active_at").is_none(),
            "last_active_at is omitted when the update does not report it"
        );
    }

    #[test]
    fn last_active_at_is_included_when_known() {
        let mut input = sample_presence();
        input.last_active_at = Some("2026-09-17T09:59:58Z".to_owned());
        let event = build_presence_updated(&input);
        assert_eq!(event["data"]["last_active_at"], "2026-09-17T09:59:58Z");
    }
}
