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

/// Header carrying the original event id on a copy of an approved reply the
/// Sensor publishes of its own accord — the dead-letter copy, and the reach
/// report below. The copy's own `Nats-Msg-Id` has to be derived (see
/// [`dead_letter_msg_id`]), so the event id needs somewhere to stay visible.
pub const EVENT_ID_HEADER: &str = "event-id";

/// Header carrying **why** the Sensor gave up, on a dead-letter copy
/// (#311). The collector has set it since #278; the Sensor did not, and a
/// screen that says a reply did not leave has to say what stopped it —
/// otherwise the owner is told "it failed" and has to read a log they do
/// not have to learn anything more. The sender caps it (the collector at
/// 512 characters), and it names the failure, never the reply's words.
pub const REASON_HEADER: &str = "reason";

/// How much of a reason is kept. The collector caps it the same way
/// (`collector/src/outbound.rs`), for the same purpose: the reason is read
/// by a person on the approval screen, and a service's whole answer pasted
/// into a line is not read at all.
pub const REASON_CAP: usize = 512;

/// The subject the Sensor reports **what a posted reply reached** on
/// (issue #216): `<source subject>.posted`, inside the twalk stream like the
/// dead-letter subject beside it.
///
/// Why a subject of the Sensor's own rather than a field: the answer is not a
/// property of the approval, it is a property of the *send*, and every v1
/// schema is `additionalProperties: false` — there is no attribute in the
/// contract that could carry it and no type for "a reply was posted". The
/// dead-letter subject is the precedent this copies exactly: a Sensor-defined
/// subject in the twalk stream, carrying the contract event **unchanged**, with
/// the new facts in headers. So no schema moves, `validate_against_contract`
/// keeps passing on the payload, and a consumer reads the answer off
/// [`POSTED_REACH_HEADER`].
///
/// Published on **every** successful post, not only on the ones that reached
/// nobody. A signal that exists only in the bad case makes the good case a
/// silence, and two situations behind one silence is the defect #216 is about.
pub fn posted_subject() -> String {
    format!("{}.posted", normalize::bus_subject(REPLY_APPROVED_TYPE))
}

/// `Nats-Msg-Id` of the reach report, derived for the reason
/// [`dead_letter_msg_id`] is: the report shares the twalk stream with the
/// approval it copies, which was published under the event id. Stable, so a
/// redelivered approval reposted (and deduplicated by the homeserver on its
/// transaction id) is reported once.
pub fn posted_msg_id(event_id: &str) -> String {
    format!("{event_id}:posted")
}

/// Header carrying what the posted reply reached: `contact`, or `nobody`. See
/// `crate::owner_device::Reach`.
pub const POSTED_REACH_HEADER: &str = "reach";

/// Header carrying the Matrix ID the reply was posted **by** — the owner's own
/// account, or the Sensor's. The `reach` header is the answer; this is the
/// reason for it, so an operator reading one message does not have to infer
/// which identity spoke.
pub const POSTED_AS_HEADER: &str = "posted-as";

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

/// Whose an approval on the bus is: the Sensor's, when its target is a
/// portal room, or another component's — the collector's, when the target
/// is a mail connection (#278). Two components consume one subject, and
/// each acknowledges the other's approvals untouched: an approval is never
/// dead-lettered for being the other's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Ours(ApprovedReply),
    /// A target this Sensor does not send to, named by the connection it is
    /// for.
    AnotherComponents {
        connection: String,
    },
}

impl ApprovedReply {
    /// Extracts the job from a contract event. A malformed event can never be
    /// delivered, so the caller dead-letters it instead of retrying; an event
    /// whose target is another component's is neither — see [`Parsed`].
    pub fn parse(event: &Value) -> Result<Parsed> {
        let event_id = event
            .get("id")
            .and_then(Value::as_str)
            .context("persona.reply.approved event has no id")?
            .to_owned();
        let data = event
            .get("data")
            .context("persona.reply.approved event has no data")?;
        if data.pointer("/target/room_id").is_none() {
            if let Some(connection) = data.pointer("/target/connection").and_then(Value::as_str) {
                return Ok(Parsed::AnotherComponents {
                    connection: connection.to_owned(),
                });
            }
        }
        Ok(Parsed::Ours(Self {
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
        }))
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
    fn the_reach_report_is_a_sibling_subject_of_the_approval_and_of_the_dead_letter() {
        // One subject per answer, all three derived from the contract type, so
        // nothing here can drift from `persona.reply.approved.v1` on its own.
        assert_eq!(posted_subject(), "twalk.persona.reply.approved.v1.posted");
        assert_ne!(posted_subject(), dead_letter_subject());
        assert_ne!(
            posted_msg_id("abc123"),
            dead_letter_msg_id("abc123"),
            "one approval may be posted and later dead-lettered on a redelivery: the two copies \
             must not deduplicate each other out of the stream"
        );
        assert_eq!(posted_msg_id("abc123"), posted_msg_id("abc123"));
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
        let Parsed::Ours(job) = ApprovedReply::parse(&sample_event()).unwrap() else {
            panic!("a room target is the Sensor's")
        };
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
        event["data"]["target"]
            .as_object_mut()
            .unwrap()
            .remove("reply_to_event_id");
        let Parsed::Ours(job) = ApprovedReply::parse(&event).unwrap() else {
            panic!("a room target is the Sensor's")
        };
        assert_eq!(job.reply_to_event_id, None);
    }

    #[test]
    fn the_traceparent_is_carried_when_present() {
        let Parsed::Ours(job) = ApprovedReply::parse(&sample_event()).unwrap() else {
            panic!("a room target is the Sensor's")
        };
        assert_eq!(job.traceparent, None);
        let mut event = sample_event();
        event["traceparent"] = json!("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");
        let Parsed::Ours(job) = ApprovedReply::parse(&event).unwrap() else {
            panic!("a room target is the Sensor's")
        };
        assert_eq!(
            job.traceparent.as_deref(),
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        );
    }

    #[test]
    fn malformed_events_are_rejected() {
        let mut no_room = sample_event();
        no_room["data"]["target"]
            .as_object_mut()
            .unwrap()
            .remove("room_id");
        assert!(ApprovedReply::parse(&no_room).is_err());
        // A mail target (#278) is not malformed: it is the collector's, and
        // the Sensor leaves it alone.
        let mut mail = sample_event();
        mail["data"]["target"] = json!({
            "connection": "mail-linagora",
            "in_reply_to": "<a@b>",
            "recipient": "mailto:a@b"
        });
        assert_eq!(
            ApprovedReply::parse(&mail).unwrap(),
            Parsed::AnotherComponents {
                connection: "mail-linagora".to_owned()
            }
        );

        let mut no_body = sample_event();
        no_body["data"]["final"]
            .as_object_mut()
            .unwrap()
            .remove("body");
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
