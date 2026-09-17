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

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// The contract caps `target.excerpt` at 512 characters: truncate the target
/// message body on a char boundary so every event stays schema-valid.
pub fn excerpt(body: &str) -> String {
    const MAX_CHARS: usize = 512;
    body.chars().take(MAX_CHARS).collect()
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
        "contact": { "display_name": input.display_name },
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
        "reaction": input.reaction,
        "target": target,
        "contact": { "display_name": input.display_name },
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
}
