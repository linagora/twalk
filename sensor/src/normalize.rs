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

/// The W3C Trace Context `traceparent` the Sensor originates for an event:
/// `00-<32 hex trace id>-<16 hex span id>-01` (sampled). The ids are carved
/// out of the event's deterministic id — a SHA-256 hex digest — so replaying
/// or resynchronizing the same occurrence keeps the same trace instead of
/// forking a new one. The Sensor has no trace backend: the field exists so
/// downstream consumers (Hermes, then anything after it) can continue the
/// trace, per the contract's optional extension.
pub fn originate_traceparent(event_id: &str) -> String {
    let hex = if event_id.len() >= 48 && event_id.chars().all(|c| c.is_ascii_hexdigit()) {
        event_id.to_ascii_lowercase()
    } else {
        // Defensive: every id the builders pass is a SHA-256 hex digest, but
        // a traceparent must exist whatever the caller hands over.
        hex_encode(Sha256::digest(event_id.as_bytes()))
    };
    format!("00-{}-{}-01", &hex[0..32], &hex[32..48])
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

/// The contract's `data.contact` object. The native network identifier is
/// contact PII the consent state gates: it is published only for a
/// `granted` contact, whoever resolved it upstream.
fn contact_entry(display_name: &str, consent: Consent, network_identifier: Option<&str>) -> Value {
    let mut contact = json!({ "display_name": cap_chars(display_name, 256) });
    if consent == Consent::Granted {
        if let Some(identifier) = network_identifier {
            contact["network_identifier"] = json!(cap_chars(identifier, 256));
        }
    }
    contact
}

/// A structured reply reference: the parent event id plus an excerpt of its
/// body, so consumers never need a bus lookup to reason about a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyTo {
    pub matrix_event_id: String,
    /// The parent's body, capped at 512 chars by the builder.
    pub excerpt: String,
}

/// The contract's `data.attachments[].kind` values the Sensor produces.
/// The contract also defines `location`, but geo messages carry no mxc URI
/// and the attachment shape requires one: v1 carries a location as the
/// message body only, with no attachment entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Video,
    Audio,
    File,
    Sticker,
}

impl AttachmentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::File => "file",
            Self::Sticker => "sticker",
        }
    }
}

/// One contract attachment entry, resolved at the seam: an `mxc://`
/// reference plus bridge-reported metadata. The binary itself stays in
/// Matrix media storage — only the reference ever transits the bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub kind: AttachmentKind,
    pub mxc_uri: String,
    /// IANA media type reported by the bridge; unreported or
    /// contract-invalid values publish as `application/octet-stream`.
    pub mime_type: Option<String>,
    /// Size reported by the bridge; unreported sizes publish as 0.
    pub size_bytes: Option<u64>,
    /// The message body when it differs from the filename (the Matrix media
    /// caption rule), capped at 1024 chars by the builder.
    pub caption: Option<String>,
    /// Pixel dimensions (width, height) for image and video attachments.
    pub dimensions: Option<(u64, u64)>,
    /// Duration in milliseconds for audio and video attachments.
    pub duration_ms: Option<u64>,
}

/// Bridges without native sticker events relay stickers as `m.room.message`
/// with msgtype `m.sticker`: the content keeps the sticker shape (`url`
/// plus an `info` object) and never reaches a typed ruma variant. Returns
/// None when the content has no usable mxc URI — an attachment entry
/// without one can never be schema-valid.
pub fn attachment_from_sticker_data(data: &serde_json::Map<String, Value>) -> Option<Attachment> {
    let mxc_uri = data.get("url")?.as_str()?;
    if !mxc_uri.starts_with("mxc://") {
        return None;
    }
    let info = data.get("info");
    let field = |key: &str| info.and_then(|info| info.get(key));
    let dimensions = match (
        field("w").and_then(Value::as_u64),
        field("h").and_then(Value::as_u64),
    ) {
        (Some(width), Some(height)) if width >= 1 && height >= 1 => Some((width, height)),
        _ => None,
    };
    Some(Attachment {
        kind: AttachmentKind::Sticker,
        mxc_uri: mxc_uri.to_owned(),
        mime_type: field("mimetype")
            .and_then(Value::as_str)
            .map(str::to_owned),
        size_bytes: field("size").and_then(Value::as_u64),
        caption: None,
        dimensions,
        duration_ms: None,
    })
}

/// The contract constrains `mime_type` to `^[a-z]+/[a-zA-Z0-9.+-]+$`. MIME
/// types are case-insensitive, so the bridge's value is lowercased first;
/// anything still off-pattern becomes the IANA "unknown binary" type rather
/// than breaking the whole event's validity.
fn mime_type_or_default(reported: Option<&str>) -> String {
    let Some(reported) = reported else {
        return "application/octet-stream".to_owned();
    };
    let lowercased = reported.to_ascii_lowercase();
    let valid = match lowercased.split_once('/') {
        Some((type_, subtype)) => {
            !type_.is_empty()
                && type_.chars().all(|c| c.is_ascii_lowercase())
                && !subtype.is_empty()
                && subtype
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'))
        }
        None => false,
    };
    if valid {
        lowercased
    } else {
        "application/octet-stream".to_owned()
    }
}

fn attachment_entry(attachment: &Attachment) -> Value {
    let mut entry = json!({
        "kind": attachment.kind.as_str(),
        "mxc_uri": attachment.mxc_uri,
        "mime_type": mime_type_or_default(attachment.mime_type.as_deref()),
        "size_bytes": attachment.size_bytes.unwrap_or(0),
    });
    if let Some(caption) = &attachment.caption {
        entry["caption"] = json!(cap_chars(caption, 1024));
    }
    if let Some((width, height)) = attachment.dimensions {
        entry["dimensions"] = json!({ "width": width, "height": height });
    }
    if let Some(duration_ms) = attachment.duration_ms {
        entry["duration_ms"] = json!(duration_ms);
    }
    entry
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
    /// The sender's native network identifier, derived from the ghost
    /// localpart; published only when `consent` is `granted`.
    pub network_identifier: Option<String>,
    /// Structured reply reference, when the message replies to a parent.
    pub reply_to: Option<ReplyTo>,
    /// Thread root event id, when the message is part of a Matrix thread.
    pub thread_root: Option<String>,
    /// Media references carried by the message — never the binaries.
    pub attachments: Vec<Attachment>,
    /// RFC 3339 timestamp of when the Sensor produced the event.
    pub produced_at: String,
    /// RFC 3339 timestamp reported by the source network, when the bridge
    /// provides one (the homeserver's receive time is NOT a network
    /// timestamp and must never fill this field).
    pub network_timestamp: Option<String>,
}

pub fn build_message_received(input: &InboundMessage) -> Value {
    let reply_to = match &input.reply_to {
        Some(reply) => json!({
            "matrix_event_id": reply.matrix_event_id,
            "excerpt": excerpt(&reply.excerpt),
        }),
        None => Value::Null,
    };
    let attachments: Vec<Value> = input.attachments.iter().map(attachment_entry).collect();
    let mut data = json!({
        "body": cap_chars(&input.body, 65536),
        "format": "text/plain",
        "reply_to": reply_to,
        "attachments": attachments,
        "contact": contact_entry(&input.display_name, input.consent, input.network_identifier.as_deref()),
    });
    if let Some(thread_root) = &input.thread_root {
        data["thread_root"] = json!(thread_root);
    }
    if let Some(network_timestamp) = &input.network_timestamp {
        data["network_timestamp"] = json!(network_timestamp);
    }
    let id = cloud_event_id(&input.matrix_event_id, &input.matrix_room_id);
    json!({
        "specversion": "1.0",
        "id": id,
        "source": format!("matrix://{}/{}", input.server_name, input.matrix_room_id),
        "type": MESSAGE_RECEIVED_TYPE,
        "time": input.produced_at,
        "subject": input.sender,
        "datacontenttype": "application/json",
        "dataschema": MESSAGE_RECEIVED_DATASCHEMA,
        "traceparent": originate_traceparent(&id),
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
    /// The reactor's native network identifier, derived from the ghost
    /// localpart; published only when `consent` is `granted`.
    pub network_identifier: Option<String>,
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
        "contact": contact_entry(&input.display_name, input.consent, input.network_identifier.as_deref()),
    });
    if let Some(network_timestamp) = &input.network_timestamp {
        data["network_timestamp"] = json!(network_timestamp);
    }
    let id = cloud_event_id(&input.matrix_event_id, &input.matrix_room_id);
    json!({
        "specversion": "1.0",
        "id": id,
        "source": format!("matrix://{}/{}", input.server_name, input.matrix_room_id),
        "type": REACTION_ADDED_TYPE,
        "time": input.produced_at,
        "subject": input.reactor,
        "datacontenttype": "application/json",
        "dataschema": REACTION_ADDED_DATASCHEMA,
        "traceparent": originate_traceparent(&id),
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
    /// The contact's native network identifier, derived from the ghost
    /// localpart; published only when `consent` is `granted`.
    pub network_identifier: Option<String>,
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
        "contact": contact_entry(&input.display_name, input.consent, input.network_identifier.as_deref()),
    });
    if let Some(last_active_at) = &input.last_active_at {
        data["last_active_at"] = json!(last_active_at);
    }
    let id = presence_event_id(
        &input.matrix_user_id,
        input.presence,
        input.receipt_timestamp_ms,
    );
    json!({
        "specversion": "1.0",
        "id": id,
        "source": format!("matrix://{}/{}", input.server_name, input.matrix_room_id),
        "type": PRESENCE_UPDATED_TYPE,
        "time": input.produced_at,
        "subject": input.matrix_user_id,
        "datacontenttype": "application/json",
        "dataschema": PRESENCE_UPDATED_DATASCHEMA,
        "traceparent": originate_traceparent(&id),
        "network": input.network.as_str(),
        "consent": input.consent.as_str(),
        "data": data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_traceparent_is_a_valid_w3c_value_derived_from_the_event_id() {
        // The id below is the known vector of derives_deterministic_id_from_the_natural_key.
        let id = "20be32e73506b9104a6a1bf76fc2d2a15cbd2b8a0a421833be01f865ca9886d0";
        let traceparent = originate_traceparent(id);
        assert_eq!(
            traceparent,
            "00-20be32e73506b9104a6a1bf76fc2d2a1-5cbd2b8a0a421833-01"
        );
        // Origination is deterministic: a replay keeps the same trace.
        assert_eq!(originate_traceparent(id), traceparent);
        // Every inbound builder stamps it.
        assert_eq!(
            build_message_received(&sample_input())["traceparent"],
            json!(traceparent)
        );
        assert!(
            build_reaction_added(&sample_reaction())["traceparent"]
                .as_str()
                .is_some_and(|value| value.starts_with("00-") && value.ends_with("-01"))
        );
        assert!(
            build_presence_updated(&sample_presence())["traceparent"]
                .as_str()
                .is_some_and(|value| value.starts_with("00-") && value.ends_with("-01"))
        );
    }

    #[test]
    fn the_traceparent_fallback_hashes_an_unexpected_id() {
        let traceparent = originate_traceparent("not-hex");
        let parts: Vec<&str> = traceparent.split('-').collect();
        assert_eq!(parts[0], "00");
        assert_eq!(parts[1].len(), 32);
        assert_eq!(parts[2].len(), 16);
        assert_eq!(parts[3], "01");
    }

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
            network_identifier: None,
            reply_to: None,
            thread_root: None,
            attachments: Vec::new(),
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

    fn sample_attachment() -> Attachment {
        Attachment {
            kind: AttachmentKind::Image,
            mxc_uri: "mxc://example.com/AbCdEf0123456789".to_owned(),
            mime_type: Some("image/png".to_owned()),
            size_bytes: Some(53201),
            caption: Some("regarde cette photo".to_owned()),
            dimensions: Some((800, 600)),
            duration_ms: None,
        }
    }

    #[test]
    fn reply_to_carries_the_parent_id_and_a_capped_excerpt() {
        let mut input = sample_input();
        input.reply_to = Some(ReplyTo {
            matrix_event_id: "$PaReNt9876".to_owned(),
            excerpt: "é".repeat(600),
        });
        let event = build_message_received(&input);
        assert_eq!(
            event["data"]["reply_to"]["matrix_event_id"],
            "$PaReNt9876"
        );
        assert_eq!(
            event["data"]["reply_to"]["excerpt"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            512
        );
    }

    #[test]
    fn thread_root_is_included_only_when_threaded() {
        let event = build_message_received(&sample_input());
        assert!(event["data"].get("thread_root").is_none());
        let mut input = sample_input();
        input.thread_root = Some("$RoOtThReAd1".to_owned());
        let event = build_message_received(&input);
        assert_eq!(event["data"]["thread_root"], "$RoOtThReAd1");
    }

    #[test]
    fn attachment_entries_carry_the_full_metadata() {
        let mut input = sample_input();
        input.attachments = vec![Attachment {
            duration_ms: Some(4200),
            ..sample_attachment()
        }];
        let event = build_message_received(&input);
        assert_eq!(
            event["data"]["attachments"][0],
            json!({
                "kind": "image",
                "mxc_uri": "mxc://example.com/AbCdEf0123456789",
                "mime_type": "image/png",
                "size_bytes": 53201,
                "caption": "regarde cette photo",
                "dimensions": { "width": 800, "height": 600 },
                "duration_ms": 4200,
            })
        );
    }

    #[test]
    fn unreported_attachment_metadata_falls_back_to_schema_valid_defaults() {
        let mut input = sample_input();
        input.attachments = vec![Attachment {
            mime_type: None,
            size_bytes: None,
            caption: None,
            dimensions: None,
            ..sample_attachment()
        }];
        let event = build_message_received(&input);
        let entry = &event["data"]["attachments"][0];
        assert_eq!(entry["mime_type"], "application/octet-stream");
        assert_eq!(entry["size_bytes"], 0);
        assert!(entry.get("caption").is_none());
        assert!(entry.get("dimensions").is_none());
        assert!(entry.get("duration_ms").is_none());
    }

    #[test]
    fn mime_types_are_lower_cased_and_invalid_ones_defaulted() {
        assert_eq!(
            mime_type_or_default(Some("IMAGE/PNG")),
            "image/png"
        );
        assert_eq!(
            mime_type_or_default(Some("image/svg+xml")),
            "image/svg+xml"
        );
        assert_eq!(
            mime_type_or_default(Some("not a mime type")),
            "application/octet-stream"
        );
        assert_eq!(mime_type_or_default(None), "application/octet-stream");
    }

    #[test]
    fn captions_are_capped_at_the_contract_limit() {
        let mut input = sample_input();
        input.attachments = vec![Attachment {
            caption: Some("x".repeat(1500)),
            ..sample_attachment()
        }];
        let event = build_message_received(&input);
        assert_eq!(
            event["data"]["attachments"][0]["caption"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            1024
        );
    }

    #[test]
    fn bodies_are_capped_at_the_contract_limit() {
        let mut input = sample_input();
        input.body = "x".repeat(70_000);
        let event = build_message_received(&input);
        assert_eq!(
            event["data"]["body"].as_str().unwrap().chars().count(),
            65536
        );
    }

    #[test]
    fn sticker_data_builds_a_sticker_attachment() {
        let data = serde_json::from_str::<serde_json::Map<String, Value>>(
            r#"{
                "url": "mxc://example.com/StIcKeR0123456",
                "info": { "mimetype": "image/png", "size": 2048, "w": 512, "h": 512 }
            }"#,
        )
        .unwrap();
        assert_eq!(
            attachment_from_sticker_data(&data),
            Some(Attachment {
                kind: AttachmentKind::Sticker,
                mxc_uri: "mxc://example.com/StIcKeR0123456".to_owned(),
                mime_type: Some("image/png".to_owned()),
                size_bytes: Some(2048),
                caption: None,
                dimensions: Some((512, 512)),
                duration_ms: None,
            })
        );
    }

    #[test]
    fn sticker_data_without_a_usable_mxc_yields_no_attachment() {
        let no_url = serde_json::Map::new();
        assert_eq!(attachment_from_sticker_data(&no_url), None);
        let mut http_url = serde_json::Map::new();
        http_url.insert("url".to_owned(), json!("https://example.com/x.png"));
        assert_eq!(attachment_from_sticker_data(&http_url), None);
    }

    #[test]
    fn sticker_data_without_info_still_builds_an_entry() {
        let mut data = serde_json::Map::new();
        data.insert("url".to_owned(), json!("mxc://example.com/StIcKeR0123456"));
        let attachment = attachment_from_sticker_data(&data).unwrap();
        assert_eq!(attachment.mime_type, None);
        assert_eq!(attachment.size_bytes, None);
        assert_eq!(attachment.dimensions, None);
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
            network_identifier: None,
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
            network_identifier: None,
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

    #[test]
    fn the_network_identifier_is_published_only_for_granted_contacts() {
        // Granted: the identifier is part of the contact on all three paths.
        let mut message = sample_input();
        message.consent = Consent::Granted;
        message.network_identifier = Some("+33612345678".to_owned());
        assert_eq!(
            build_message_received(&message)["data"]["contact"]["network_identifier"],
            "+33612345678"
        );
        let mut reaction = sample_reaction();
        reaction.consent = Consent::Granted;
        reaction.network_identifier = Some("+33612345678".to_owned());
        assert_eq!(
            build_reaction_added(&reaction)["data"]["contact"]["network_identifier"],
            "+33612345678"
        );
        let mut presence = sample_presence();
        presence.consent = Consent::Granted;
        presence.network_identifier = Some("+33612345678".to_owned());
        assert_eq!(
            build_presence_updated(&presence)["data"]["contact"]["network_identifier"],
            "+33612345678"
        );

        // Pending or revoked: the identifier never leaves the Sensor, even
        // when one was resolved.
        for consent in [Consent::Pending, Consent::Revoked] {
            let mut input = sample_input();
            input.consent = consent;
            input.network_identifier = Some("+33612345678".to_owned());
            assert!(
                build_message_received(&input)["data"]["contact"]
                    .get("network_identifier")
                    .is_none(),
                "consent {} must not expose the identifier",
                consent.as_str()
            );
        }

        // Granted but underivable: the field is omitted, not null.
        let mut input = sample_input();
        input.consent = Consent::Granted;
        assert!(
            build_message_received(&input)["data"]["contact"]
                .get("network_identifier")
                .is_none()
        );
    }
}
