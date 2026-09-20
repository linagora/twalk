//! An approved reply to a mail, sent from the owner's own mailbox (issue
//! #278, ADR 0037): the pure half — which approvals on the bus are this
//! collector's, the reply as a JMAP `Email/set` creation and its
//! `EmailSubmission/set`, the subjects and ids of the retry policy the
//! Sensor already has (`sensor/src/outbound.rs`, copied: the two binaries
//! share no crate for it yet — the consent cache is the precedent for one,
//! and a third consumer of these names would be the moment to extract it).
//!
//! Two components consume one subject. The Sensor sends a target that is a
//! room; the collector sends one that is a mail connection it holds; each
//! acknowledges the other's approvals untouched, so an approval is never
//! dead-lettered for being the other's. A reply the collector cannot send
//! is retried with exponential backoff and then dead-lettered with the
//! reason — an approved reply is never silently dropped — and a sent one is
//! reported on `<subject>.posted` with `reach=contact`, as the Sensor
//! reports its own (#216).
//!
//! The reply goes to the **sender alone** — reply-all is a decision the
//! owner did not take in v1 — under `Re:` and the original subject, with
//! `In-Reply-To` and `References` continued from the mail it answers, the
//! approved text as its body (ADR 0031's disclosure already in it), from
//! the owner's own JMAP identity, with a copy left in Sent.

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::jmap::Mail;

pub const REPLY_APPROVED_TYPE: &str = "fr.linagora.twalk.persona.reply.approved.v1";
/// The durable consumer's name, per mail connection: survives a restart,
/// so an approval is never lost, and is one collector's alone — two
/// collectors (two accounts, two grants) on one bus each consume the whole
/// subject and leave each other's approvals alone, rather than sharing one
/// consumer and taking turns at messages that are not theirs.
pub fn reply_consumer(connection: &str) -> String {
    format!("collector-{connection}-persona-reply-approved")
}
pub const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(30);
pub const EVENT_ID_HEADER: &str = "event-id";
pub const POSTED_REACH_HEADER: &str = "reach";
pub const POSTED_AS_HEADER: &str = "posted-as";
/// The header a dead letter says why on: the collector's own, since a
/// mailbox refuses in words a room does not.
pub const REASON_HEADER: &str = "reason";
/// The mail header every reply carries the approval's id in: what makes a
/// redelivered approval find its reply already in Sent rather than send it
/// twice — JMAP has no transaction id to deduplicate on.
pub const APPROVAL_HEADER: &str = "X-Twalk-Approval";

pub fn dead_letter_subject() -> String {
    format!("{}.dead", crate::status::bus_subject(REPLY_APPROVED_TYPE))
}

pub fn posted_subject() -> String {
    format!("{}.posted", crate::status::bus_subject(REPLY_APPROVED_TYPE))
}

pub fn posted_msg_id(event_id: &str) -> String {
    format!("{event_id}:posted")
}

pub fn dead_letter_msg_id(event_id: &str) -> String {
    format!("{event_id}:dead-letter")
}

/// Whose an approval is — the Sensor's two cases, seen from the other side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// A mail target on one of this process's connections.
    Ours(ApprovedReply),
    /// A room target (the Sensor's), or a mail connection another process
    /// holds.
    AnotherComponents { why: Whose },
}

/// Why an approval is another component's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whose {
    TheSensors,
    AnotherCollectors,
}

impl Whose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TheSensors => "a portal room: the Sensor's",
            Self::AnotherCollectors => "a mail connection this collector does not hold",
        }
    }
}

/// An approved reply to send.
#[derive(Clone, PartialEq, Eq)]
pub struct ApprovedReply {
    pub event_id: String,
    pub connection: String,
    /// The Message-ID of the mail answered, angle brackets kept.
    pub in_reply_to: String,
    pub body: String,
    pub format: String,
    pub traceparent: Option<String>,
}

impl std::fmt::Debug for ApprovedReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovedReply")
            .field("event_id", &self.event_id)
            .field("connection", &self.connection)
            .field("in_reply_to", &self.in_reply_to)
            .field("body", &"<redacted>")
            .finish()
    }
}

impl ApprovedReply {
    /// Reads an approval off the bus. A malformed event can never be sent
    /// and is dead-lettered by the caller; one that is not this collector's
    /// is acknowledged and left alone.
    pub fn parse(event: &Value, held: &[String]) -> Result<Parsed> {
        let event_id = event
            .get("id")
            .and_then(Value::as_str)
            .context("persona.reply.approved event has no id")?
            .to_owned();
        let data = event
            .get("data")
            .context("persona.reply.approved event has no data")?;
        let target = data.get("target").context("the approval names no target")?;
        if target.get("room_id").is_some() {
            return Ok(Parsed::AnotherComponents {
                why: Whose::TheSensors,
            });
        }
        let connection = target
            .get("connection")
            .and_then(Value::as_str)
            .context("the approval's target names neither a room nor a connection")?
            .to_owned();
        if !held.iter().any(|id| id == &connection) {
            return Ok(Parsed::AnotherComponents {
                why: Whose::AnotherCollectors,
            });
        }
        Ok(Parsed::Ours(Self {
            event_id,
            connection,
            in_reply_to: target
                .get("in_reply_to")
                .and_then(Value::as_str)
                .context("the approval's mail target names no in_reply_to")?
                .to_owned(),
            body: data
                .pointer("/final/body")
                .and_then(Value::as_str)
                .context("the approval has no final body")?
                .to_owned(),
            format: data
                .pointer("/final/format")
                .and_then(Value::as_str)
                .unwrap_or("text/plain")
                .to_owned(),
            traceparent: event
                .get("traceparent")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }))
    }
}

/// Where a reply is written from and lands: the account, the owner's
/// sending identity, and the two mailboxes, as `Mailbox/get` and
/// `Identity/get` named them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sender {
    pub account_id: String,
    pub identity_id: String,
    pub owner_email: String,
    pub drafts_id: String,
    pub sent_id: String,
}

/// The reply, as the two JMAP calls that send it: `Email/set` creating the
/// mail in Drafts under the creation id `#reply`, and `EmailSubmission/set`
/// submitting it from the owner's identity, moving it to Sent and clearing
/// `$draft` on success (RFC 8621 §7.5). `original` is the mail answered,
/// as the collector read it back by Message-ID.
pub fn reply_calls(
    reply: &ApprovedReply,
    original: &Mail,
    sender: &Sender,
) -> Vec<(&'static str, Value)> {
    let Sender {
        account_id,
        identity_id,
        owner_email,
        drafts_id,
        sent_id,
    } = sender;
    let subject = if original
        .subject
        .trim_start()
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("re:"))
    {
        original.subject.clone()
    } else {
        format!("Re: {}", original.subject)
    };
    // References (RFC 5322 §3.6.4): the original's References whole — or,
    // when it has none, its own In-Reply-To — then the original itself,
    // once.
    let mut references: Vec<String> = if original.references.is_empty() {
        original.in_reply_to.iter().cloned().collect()
    } else {
        original.references.clone()
    };
    if !references.iter().any(|id| id == &reply.in_reply_to) {
        references.push(reply.in_reply_to.clone());
    }
    let bare = |id: &str| id.trim_matches(|c| c == '<' || c == '>').to_owned();
    let email = json!({
        "mailboxIds": { drafts_id: true },
        "keywords": { "$draft": true, "$seen": true },
        "from": [{ "name": null, "email": owner_email }],
        "to": [{ "name": original.from.name, "email": original.from.email }],
        "subject": subject,
        "inReplyTo": [bare(&reply.in_reply_to)],
        "references": references.iter().map(|id| bare(id)).collect::<Vec<_>>(),
        "bodyStructure": { "partId": "1", "type": "text/plain", "charset": "utf-8" },
        "bodyValues": { "1": { "value": reply.body, "isTruncated": false } },
        format!("header:{APPROVAL_HEADER}:asText"): reply.event_id
    });
    vec![
        (
            "Email/set",
            json!({ "accountId": account_id, "create": { "reply": email } }),
        ),
        (
            "EmailSubmission/set",
            json!({
                "accountId": account_id,
                "create": {
                    "submission": {
                        "emailId": "#reply",
                        "identityId": identity_id,
                        "envelope": {
                            "mailFrom": { "email": owner_email },
                            "rcptTo": [{ "email": original.from.email }]
                        }
                    }
                },
                "onSuccessUpdateEmail": {
                    "#submission": {
                        format!("mailboxIds/{sent_id}"): true,
                        format!("mailboxIds/{drafts_id}"): null,
                        "keywords/$draft": null
                    }
                }
            }),
        ),
    ]
}

/// `Email/query` for the reply already sent for an approval: the mail in
/// the mailbox carrying its id in `X-Twalk-Approval`, if any.
pub fn reply_already_sent(account_id: &str, event_id: &str) -> (&'static str, Value) {
    (
        "Email/query",
        json!({
            "accountId": account_id,
            "filter": { "header": [APPROVAL_HEADER, event_id] },
            "limit": 1
        }),
    )
}

/// `Email/set` destroying the draft a failed submission left behind.
pub fn destroy_draft(account_id: &str, email_id: &str) -> (&'static str, Value) {
    (
        "Email/set",
        json!({ "accountId": account_id, "destroy": [email_id] }),
    )
}

/// The delay before the next redelivery: the base doubling with each
/// attempt (JetStream's delivered count is 1-based), capped.
pub fn retry_delay(base: Duration, delivered: i64) -> Duration {
    let factor = 2u32.saturating_pow(delivered.max(1) as u32 - 1);
    base.saturating_mul(factor).min(MAX_RETRY_BACKOFF)
}

/// Why a send did not happen: for good, or for now.
#[derive(Debug)]
pub enum SendError {
    /// The server refused what was asked in a way a retry cannot change: a
    /// submission `notCreated`, an original mail that no longer exists.
    Permanent(String),
    /// The server did not answer, or answered something a retry may fix.
    Transient(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Permanent(why) => write!(f, "permanent: {why}"),
            Self::Transient(why) => write!(f, "transient: {why}"),
        }
    }
}

impl std::error::Error for SendError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jmap::{Attachment, Person};

    fn approval() -> Value {
        serde_json::from_str(include_str!(
            "../../contracts/cloudevents/v1/fixtures/variants/persona.reply.approved/mail.json"
        ))
        .unwrap()
    }

    fn original() -> Mail {
        Mail {
            id: "M4f2a9c".to_owned(),
            received_at: "2026-09-21T08:14:58Z".to_owned(),
            from: Person {
                name: Some("Alice Martin".to_owned()),
                email: "alice@example.org".to_owned(),
            },
            to: vec![],
            cc: vec![],
            subject: "Point hebdo".to_owned(),
            body: "never sent back".to_owned(),
            message_id: Some("<9b8c7d6e-1@example.org>".to_owned()),
            in_reply_to: Some("<mid-2@example.org>".to_owned()),
            references: vec![
                "<c9d8e7f6@example.org>".to_owned(),
                "<mid-2@example.org>".to_owned(),
            ],
            attachments: vec![Attachment {
                kind: "file",
                mime_type: "application/pdf".to_owned(),
                size_bytes: 1,
            }],
            auto_submitted: None,
            list_id: None,
            list_unsubscribe: None,
            precedence: None,
            has_itip_part: false,
        }
    }

    #[test]
    fn the_fixture_is_ours_a_room_is_the_sensors_and_another_connection_is_not_ours() {
        let held = vec!["mail-linagora".to_owned()];
        let Parsed::Ours(reply) = ApprovedReply::parse(&approval(), &held).unwrap() else {
            panic!("the fixture targets the held connection")
        };
        assert_eq!(reply.in_reply_to, "<9b8c7d6e-1@example.org>");
        assert!(reply.body.starts_with("Oui, lundi 9h"));
        assert!(!format!("{reply:?}").contains("lundi"));

        let room: Value = serde_json::from_str(include_str!(
            "../../contracts/cloudevents/v1/fixtures/persona.reply.approved.json"
        ))
        .unwrap();
        assert_eq!(
            ApprovedReply::parse(&room, &held).unwrap(),
            Parsed::AnotherComponents {
                why: Whose::TheSensors
            }
        );
        assert_eq!(
            ApprovedReply::parse(&approval(), &["mail-other".to_owned()]).unwrap(),
            Parsed::AnotherComponents {
                why: Whose::AnotherCollectors
            }
        );
        let mut malformed = approval();
        malformed["data"]["target"] = json!({ "connection": "mail-linagora" });
        assert!(ApprovedReply::parse(&malformed, &held).is_err());
    }

    #[test]
    fn the_reply_goes_to_the_sender_alone_in_the_thread_from_the_owner_and_lands_in_sent() {
        let held = vec!["mail-linagora".to_owned()];
        let Parsed::Ours(reply) = ApprovedReply::parse(&approval(), &held).unwrap() else {
            panic!()
        };
        let sender = Sender {
            account_id: "u1".to_owned(),
            identity_id: "id-owner".to_owned(),
            owner_email: "michel@example.com".to_owned(),
            drafts_id: "drafts-1".to_owned(),
            sent_id: "sent-1".to_owned(),
        };
        let calls = reply_calls(&reply, &original(), &sender);
        let (name, set) = &calls[0];
        assert_eq!(*name, "Email/set");
        let email = &set["create"]["reply"];
        assert_eq!(
            email["to"],
            json!([{ "name": "Alice Martin", "email": "alice@example.org" }])
        );
        assert!(email.get("cc").is_none(), "the sender alone");
        assert_eq!(email["from"][0]["email"], "michel@example.com");
        assert_eq!(email["subject"], "Re: Point hebdo");
        assert_eq!(email["inReplyTo"], json!(["9b8c7d6e-1@example.org"]));
        assert_eq!(
            email["references"],
            json!([
                "c9d8e7f6@example.org",
                "mid-2@example.org",
                "9b8c7d6e-1@example.org"
            ]),
            "the original's References whole, then the original"
        );
        // An original with no References but an In-Reply-To: that is the
        // thread, per RFC 5322's fallback.
        let mut sparse = original();
        sparse.references.clear();
        let calls = reply_calls(&reply, &sparse, &sender);
        assert_eq!(
            calls[0].1["create"]["reply"]["references"],
            json!(["mid-2@example.org", "9b8c7d6e-1@example.org"])
        );
        assert_eq!(email["mailboxIds"], json!({ "drafts-1": true }));
        assert_eq!(
            email["header:X-Twalk-Approval:asText"], reply.event_id,
            "the approval's id travels in the mail, so a redelivery finds it"
        );
        assert!(
            !set.to_string().contains("never sent back"),
            "the original's words stay put"
        );
        let (name, submission) = &calls[1];
        assert_eq!(*name, "EmailSubmission/set");
        assert_eq!(submission["create"]["submission"]["identityId"], "id-owner");
        assert_eq!(submission["create"]["submission"]["emailId"], "#reply");
        assert_eq!(
            submission["create"]["submission"]["envelope"]["rcptTo"],
            json!([{ "email": "alice@example.org" }])
        );
        assert_eq!(
            submission["onSuccessUpdateEmail"]["#submission"]["mailboxIds/sent-1"],
            true
        );
        assert!(submission["onSuccessUpdateEmail"]["#submission"]["keywords/$draft"].is_null());

        // A subject already answered is not answered twice.
        let mut again = original();
        again.subject = "RE: Point hebdo".to_owned();
        let calls = reply_calls(&reply, &again, &sender);
        assert_eq!(calls[0].1["create"]["reply"]["subject"], "RE: Point hebdo");
    }

    #[test]
    fn the_subjects_and_the_backoff_are_the_sensors() {
        assert_eq!(
            dead_letter_subject(),
            "twalk.persona.reply.approved.v1.dead"
        );
        assert_eq!(posted_subject(), "twalk.persona.reply.approved.v1.posted");
        assert_eq!(posted_msg_id("abc"), "abc:posted");
        assert_eq!(dead_letter_msg_id("abc"), "abc:dead-letter");
        let base = Duration::from_secs(2);
        assert_eq!(retry_delay(base, 1), base);
        assert_eq!(retry_delay(base, 3), Duration::from_secs(8));
        assert_eq!(retry_delay(base, 100), MAX_RETRY_BACKOFF);
    }
}
