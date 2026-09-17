//! Outbound: closing the loop. A `persona.reply.approved.v1` event on the bus
//! sends the final, approved content back into its target portal room; sends
//! that keep failing are retried with exponential backoff and then moved to a
//! dead-letter subject — an approved reply is never silently dropped.
//! Pure logic, no I/O: the binary wires it to matrix-sdk and NATS JetStream.

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::normalize;

pub const REPLY_APPROVED_TYPE: &str = "fr.linagora.twalk.persona.reply.approved.v1";

/// Durable name of the JetStream pull consumer for approved replies: the
/// consumer survives Sensor restarts, so approved replies are not lost.
pub const REPLY_CONSUMER: &str = "sensor-persona-reply-approved";

/// Cap on the exponential backoff between redeliveries.
pub const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(30);

/// The subject a failed approved reply moves to once its delivery attempts
/// are exhausted: `<source subject>.dead`, inside the twalk stream like
/// every `twalk.*` subject.
pub fn dead_letter_subject() -> String {
    format!("{}.dead", normalize::bus_subject(REPLY_APPROVED_TYPE))
}

/// Header carrying the original event id on a dead-letter copy.
pub const DEAD_LETTER_EVENT_ID_HEADER: &str = "event-id";

/// `Nats-Msg-Id` of the dead-letter copy of an event. The bus de-duplicates
/// per stream, and the dead-letter subject shares the twalk stream with the
/// approved reply, published under its event id: reusing that id would make
/// the bus drop the copy as a duplicate. The derived id is still stable, so a
/// redelivered event dead-lettered twice is stored once.
pub fn dead_letter_msg_id(event_id: &str) -> String {
    format!("{event_id}:dead-letter")
}

/// An approved reply to post, parsed from a `persona.reply.approved.v1`
/// event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovedReply {
    /// CloudEvents id of the approval event, for logs and dead-letter tracing.
    pub event_id: String,
    /// Portal room the reply must be posted to.
    pub room_id: String,
    /// Original message the reply threads under, when the approval has one.
    pub reply_to_event_id: Option<String>,
    /// The exact content to send: the final, possibly edited text — never
    /// the raw suggestion.
    pub body: String,
    /// Contract content type of `body` (text/plain, text/markdown, text/html).
    pub format: String,
    /// The approval's W3C Trace Context `traceparent`, when it carries one
    /// (Hermes normally continues it from the trigger event). The Sensor
    /// POSTs to Matrix, which has no trace context to inject into, so the
    /// trace does not travel with the send itself: the Sensor carries the
    /// value in its send logs, and the bridge echo closes the loop by
    /// re-entering as a fresh inbound event. Continuing the trace through
    /// Hermes and its downstream services is Hermes's job.
    pub traceparent: Option<String>,
}

impl ApprovedReply {
    /// Extracts the job from a contract event. A malformed event can never be
    /// delivered, so the caller dead-letters it instead of retrying.
    pub fn parse(event: &Value) -> Result<Self> {
        let event_id = event
            .get("id")
            .and_then(Value::as_str)
            .context("persona.reply.approved event has no id")?
            .to_owned();
        let data = event
            .get("data")
            .context("persona.reply.approved event has no data")?;
        Ok(Self {
            event_id,
            room_id: required_str(data, "/target/room_id")?,
            reply_to_event_id: data
                .pointer("/target/reply_to_event_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            body: required_str(data, "/final/body")?,
            format: required_str(data, "/final/format")?,
            traceparent: event
                .get("traceparent")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }
}

fn required_str(data: &Value, pointer: &str) -> Result<String> {
    data.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("persona.reply.approved event has no string at data{pointer}"))
}

/// The delay before the next redelivery of a failed send: the base delay
/// doubling with each delivery attempt (the JetStream delivered count is
/// 1-based), capped at [`MAX_RETRY_BACKOFF`].
pub fn retry_delay(base: Duration, delivered: i64) -> Duration {
    let factor = 2u32.saturating_pow(delivered.max(1) as u32 - 1);
    base.saturating_mul(factor).min(MAX_RETRY_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_dead_letter_subject_derives_from_the_contract_type() {
        assert_eq!(
            normalize::bus_subject(REPLY_APPROVED_TYPE),
            "twalk.persona.reply.approved.v1"
        );
        assert_eq!(
            dead_letter_subject(),
            "twalk.persona.reply.approved.v1.dead"
        );
    }

    #[test]
    fn the_dead_letter_msg_id_is_distinct_from_the_event_id_and_stable() {
        let id = dead_letter_msg_id("abc123");
        assert_ne!(id, "abc123");
        assert_eq!(id, dead_letter_msg_id("abc123"));
        assert_eq!(id, "abc123:dead-letter");
    }

    fn sample_event() -> Value {
        json!({
            "specversion": "1.0",
            "id": "57f0e4d352d1ba5e6bf0e92223634253cd852e3d7d018ea91025dc098c1a564a",
            "source": "hermes://twalk.example.com/personas/assistant",
            "type": REPLY_APPROVED_TYPE,
            "data": {
                "persona_id": "assistant",
                "suggestion_event_id": "319be8ff15d5dee005c8aa27119b983da8223959987e5dbc639d81e370b5ef9b",
                "approved_by": "@michel:example.com",
                "final": { "body": "Pas de problème, à 20h ! 👍", "format": "text/plain" },
                "edited": true,
                "target": {
                    "room_id": "!abcXYZ123:example.com",
                    "reply_to_event_id": "$AbCdEfGh1234"
                }
            }
        })
    }

    #[test]
    fn parses_an_approved_reply() {
        let job = ApprovedReply::parse(&sample_event()).unwrap();
        assert_eq!(
            job.event_id,
            "57f0e4d352d1ba5e6bf0e92223634253cd852e3d7d018ea91025dc098c1a564a"
        );
        assert_eq!(job.room_id, "!abcXYZ123:example.com");
        assert_eq!(job.reply_to_event_id.as_deref(), Some("$AbCdEfGh1234"));
        assert_eq!(job.body, "Pas de problème, à 20h ! 👍");
        assert_eq!(job.format, "text/plain");
    }

    #[test]
    fn the_reply_target_is_optional() {
        let mut event = sample_event();
        event["data"]["target"].as_object_mut().unwrap().remove("reply_to_event_id");
        let job = ApprovedReply::parse(&event).unwrap();
        assert_eq!(job.reply_to_event_id, None);
    }

    #[test]
    fn the_traceparent_is_carried_when_present() {
        let job = ApprovedReply::parse(&sample_event()).unwrap();
        assert_eq!(job.traceparent, None);
        let mut event = sample_event();
        event["traceparent"] =
            json!("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");
        let job = ApprovedReply::parse(&event).unwrap();
        assert_eq!(
            job.traceparent.as_deref(),
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        );
    }

    #[test]
    fn malformed_events_are_rejected() {
        let mut no_room = sample_event();
        no_room["data"]["target"].as_object_mut().unwrap().remove("room_id");
        assert!(ApprovedReply::parse(&no_room).is_err());

        let mut no_body = sample_event();
        no_body["data"]["final"].as_object_mut().unwrap().remove("body");
        assert!(ApprovedReply::parse(&no_body).is_err());

        assert!(ApprovedReply::parse(&json!({"unrelated": true})).is_err());
    }

    #[test]
    fn the_backoff_doubles_with_each_attempt_and_is_capped() {
        let base = Duration::from_millis(100);
        assert_eq!(retry_delay(base, 1), Duration::from_millis(100));
        assert_eq!(retry_delay(base, 2), Duration::from_millis(200));
        assert_eq!(retry_delay(base, 3), Duration::from_millis(400));
        assert_eq!(retry_delay(base, 4), Duration::from_millis(800));
        assert_eq!(retry_delay(base, 100), MAX_RETRY_BACKOFF);
    }
}
