//! The owner's mailbox, read over JMAP (issue #276, ADR 0033: email reuses
//! the message types): the pure half — the session, the requests, the
//! Email object read as a mail, the frontier that decides whether a mail
//! is a person writing, and the `inbound.message.received.v1` envelope in
//! its two shapes. No I/O: the requests live in `mails.rs`.
//!
//! The **frontier** is the decision to argue with. A mail is dropped only
//! on a *positive* non-human signal — `Auto-Submitted` other than `no`, a
//! `List-Id` or `List-Unsubscribe` header, `Precedence: bulk|list|junk` —
//! and a mail with no signal is a person until proven otherwise, labelled
//! `pending` like a stranger's WhatsApp message (ADR 0026's rule for the
//! bridge bots, run the other way: the safe failure here is a newsletter
//! reaching the consent screen, not a person silenced). An iTIP part
//! (`text/calendar`) is a calendar invitation and the calendar's business,
//! dropped as such; a mail from the owner's own address is never a contact
//! (ADR 0021) and is not published on this type.
//!
//! What is published is the message as the contract has it: the text part
//! — HTML reduced to text when there is no text part — the Subject as
//! `title`, the reply and thread relations by Message-ID, the sender's
//! display name, and each attachment's kind, media type and size — never
//! the filename, never the bytes. A `revoked` sender's mail is the reduced
//! shape: the relations, the shape of the attachments, and no words.

use serde_json::{json, Value};
use twalk_consent_cache::Consent;

use crate::status::sha256_hex;

pub const MESSAGE_RECEIVED_TYPE: &str = "fr.linagora.twalk.inbound.message.received.v1";
const SCHEMA: &str =
    "https://schemas.twalk.dev/cloudevents/v1/inbound.message.received.schema.json";
pub const MAIL_CAPABILITY: &str = "urn:ietf:params:jmap:mail";

/// The session, as the collector needs it: where the API is and which
/// account the owner's mail is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub api_url: String,
    pub account_id: String,
}

impl Session {
    /// Reads the session resource (RFC 8620 §2): the primary mail account,
    /// or the first account with the mail capability.
    pub fn parse(document: &Value) -> anyhow::Result<Self> {
        let api_url = document
            .get("apiUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("the session names no apiUrl"))?
            .to_owned();
        let account_id = document
            .pointer(&format!(
                "/primaryAccounts/{}",
                MAIL_CAPABILITY.replace('/', "~1")
            ))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                document
                    .get("accounts")
                    .and_then(Value::as_object)?
                    .iter()
                    .find(|(_, account)| {
                        account
                            .pointer(&format!(
                                "/accountCapabilities/{}",
                                MAIL_CAPABILITY.replace('/', "~1")
                            ))
                            .is_some()
                    })
                    .map(|(id, _)| id.clone())
            })
            .ok_or_else(|| {
                anyhow::anyhow!("the session names no account with the mail capability")
            })?;
        Ok(Self {
            api_url,
            account_id,
        })
    }
}

/// The properties `Email/get` is asked for: what the contract publishes,
/// what the frontier decides on, and nothing more — not the blobs, not the
/// keywords.
pub const EMAIL_PROPERTIES: &[&str] = &[
    "id",
    "mailboxIds",
    "receivedAt",
    "messageId",
    "inReplyTo",
    "references",
    "from",
    "to",
    "cc",
    "subject",
    "textBody",
    "htmlBody",
    "bodyValues",
    "attachments",
    "header:Auto-Submitted:asText",
    "header:List-Id:asText",
    "header:List-Unsubscribe:asText",
    "header:Precedence:asText",
];

/// One JMAP request, RFC 8620 §3.3: the `using` list and the calls.
pub fn request(calls: Vec<(&str, Value)>) -> Value {
    json!({
        "using": ["urn:ietf:params:jmap:core", MAIL_CAPABILITY],
        "methodCalls": calls
            .into_iter()
            .enumerate()
            .map(|(index, (name, args))| json!([name, args, format!("c{index}")]))
            .collect::<Vec<_>>()
    })
}

pub fn mailbox_get(account_id: &str) -> (&'static str, Value) {
    (
        "Mailbox/get",
        json!({ "accountId": account_id, "ids": null, "properties": ["id", "name", "role"] }),
    )
}

/// `Email/get` with no ids: what answers the current Email state, which is
/// the cursor a first start takes without reading a mail (no backfill).
pub fn email_state(account_id: &str) -> (&'static str, Value) {
    (
        "Email/get",
        json!({ "accountId": account_id, "ids": [], "properties": ["id"] }),
    )
}

pub fn email_changes(account_id: &str, since_state: &str) -> (&'static str, Value) {
    (
        "Email/changes",
        json!({ "accountId": account_id, "sinceState": since_state, "maxChanges": 500 }),
    )
}

/// `Email/get` of the mailboxes alone: what says whether a changed mail is
/// in the INBOX before anything else of it is read.
pub fn email_mailboxes(account_id: &str, ids: &[String]) -> (&'static str, Value) {
    (
        "Email/get",
        json!({ "accountId": account_id, "ids": ids, "properties": ["id", "mailboxIds"] }),
    )
}

pub fn email_get(account_id: &str, ids: &[String]) -> (&'static str, Value) {
    (
        "Email/get",
        json!({
            "accountId": account_id,
            "ids": ids,
            "properties": EMAIL_PROPERTIES,
            "fetchTextBodyValues": true,
            "fetchHTMLBodyValues": true,
            "maxBodyValueBytes": 262144
        }),
    )
}

/// The result of the `index`th call of a response, or the error the server
/// answered it with — by name, since a server may answer `error`.
pub fn method_result(response: &Value, index: usize) -> Result<Value, MethodError> {
    let call = response
        .get("methodResponses")
        .and_then(Value::as_array)
        .and_then(|responses| responses.get(index))
        .ok_or_else(|| MethodError {
            kind: "noResponse".to_owned(),
            description: format!("the server answered no method response #{index}"),
        })?;
    let name = call.get(0).and_then(Value::as_str).unwrap_or_default();
    let args = call.get(1).cloned().unwrap_or(Value::Null);
    if name == "error" {
        return Err(MethodError {
            kind: args
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            description: args
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        });
    }
    Ok(args)
}

/// A JMAP method error (RFC 8620 §3.6.2), by its `type`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodError {
    pub kind: String,
    pub description: String,
}

impl std::fmt::Display for MethodError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.description)
    }
}

impl std::error::Error for MethodError {}

/// The INBOX's id off `Mailbox/get`: the mailbox whose role is `inbox`, or
/// the one named INBOX when the server sets no roles.
pub fn inbox_id(mailbox_get: &Value) -> Option<String> {
    let list = mailbox_get.get("list").and_then(Value::as_array)?;
    list.iter()
        .find(|mailbox| mailbox.get("role").and_then(Value::as_str) == Some("inbox"))
        .or_else(|| {
            list.iter().find(|mailbox| {
                mailbox
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case("inbox"))
            })
        })
        .and_then(|mailbox| mailbox.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// What `Email/changes` said.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Changes {
    pub new_state: String,
    pub has_more: bool,
    pub created: Vec<String>,
    pub updated: Vec<String>,
}

impl Changes {
    pub fn parse(result: &Value) -> Self {
        let ids = |name: &str| {
            result
                .get(name)
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            new_state: result
                .get("newState")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            has_more: result
                .get("hasMoreChanges")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            created: ids("created"),
            updated: ids("updated"),
        }
    }
}

/// The ids in the INBOX among an `Email/get` of mailboxes.
pub fn in_mailbox(email_get: &Value, mailbox_id: &str) -> Vec<String> {
    email_get
        .get("list")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|email| {
                    email
                        .pointer(&format!("/mailboxIds/{mailbox_id}"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .filter_map(|email| email.get("id").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// One person on a mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub name: Option<String>,
    /// The address, lower-cased.
    pub email: String,
}

impl Person {
    fn parse(value: &Value) -> Option<Self> {
        let email = value.get("email")?.as_str()?.trim();
        (!email.is_empty() && email.contains('@')).then(|| Self {
            name: value
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned),
            email: email.to_ascii_lowercase(),
        })
    }

    pub fn mailto(&self) -> String {
        format!("mailto:{}", self.email)
    }
}

/// An attachment's shape: its kind by media type, the type, the size.
/// Never the filename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub kind: &'static str,
    pub mime_type: String,
    pub size_bytes: u64,
}

/// A mail, as the Email object says it and as far as the contract goes.
/// Its `Debug` names the id and the sender's address and nothing the sender
/// wrote, so a `{:?}` in a log line is not a body in the log.
#[derive(Clone, PartialEq, Eq)]
pub struct Mail {
    pub id: String,
    pub received_at: String,
    pub from: Person,
    pub to: Vec<Person>,
    pub cc: Vec<Person>,
    pub subject: String,
    pub body: String,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    /// The first Message-ID of References: the thread's root.
    pub thread_root: Option<String>,
    pub attachments: Vec<Attachment>,
    pub auto_submitted: Option<String>,
    pub list_id: Option<String>,
    pub list_unsubscribe: Option<String>,
    pub precedence: Option<String>,
    /// Whether a part of the mail is `text/calendar`: an iTIP invitation.
    pub has_itip_part: bool,
}

impl std::fmt::Debug for Mail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mail")
            .field("id", &self.id)
            .field("from", &self.from.email)
            .field("received_at", &self.received_at)
            .field("attachments", &self.attachments.len())
            .field("words", &"<redacted>")
            .finish()
    }
}

impl Mail {
    /// Reads an Email object (RFC 8621 §4.1) with the properties
    /// `EMAIL_PROPERTIES` asked for. The body is the first text part's
    /// value, or the first HTML part's reduced to text.
    pub fn parse(email: &Value) -> anyhow::Result<Self> {
        let id = email
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("an Email object with no id"))?
            .to_owned();
        let from = email
            .get("from")
            .and_then(Value::as_array)
            .and_then(|from| from.iter().find_map(Person::parse))
            .ok_or_else(|| anyhow::anyhow!("the mail {id} names no sender address"))?;
        let people = |name: &str| -> Vec<Person> {
            email
                .get(name)
                .and_then(Value::as_array)
                .map(|list| list.iter().filter_map(Person::parse).collect())
                .unwrap_or_default()
        };
        let header = |name: &str| -> Option<String> {
            email
                .get(format!("header:{name}:asText"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let body_values = email.get("bodyValues");
        let value_of = |parts: &str| -> Option<String> {
            email
                .get(parts)
                .and_then(Value::as_array)?
                .iter()
                .find_map(|part| {
                    let part_id = part.get("partId")?.as_str()?;
                    body_values?
                        .get(part_id)?
                        .get("value")?
                        .as_str()
                        .map(str::to_owned)
                })
        };
        let body = match value_of("textBody") {
            Some(text) => text,
            None => value_of("htmlBody")
                .map(|html| html_to_text(&html))
                .unwrap_or_default(),
        };
        let message_ids = |name: &str| -> Vec<String> {
            email
                .get(name)
                .and_then(Value::as_array)
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_str)
                        .map(|id| format!("<{}>", id.trim_matches(|c| c == '<' || c == '>')))
                        .collect()
                })
                .unwrap_or_default()
        };
        let has_itip_part = email
            .get("attachments")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .chain(
                email
                    .get("textBody")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten(),
            )
            .chain(
                email
                    .get("htmlBody")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten(),
            )
            .any(|part| {
                part.get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|media| media.eq_ignore_ascii_case("text/calendar"))
            });
        let attachments = email
            .get("attachments")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .map(|part| {
                        let mime_type = part
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("application/octet-stream")
                            .to_ascii_lowercase();
                        Attachment {
                            kind: attachment_kind(&mime_type),
                            mime_type,
                            size_bytes: part.get("size").and_then(Value::as_u64).unwrap_or(0),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            received_at: email
                .get("receivedAt")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            from,
            to: people("to"),
            cc: people("cc"),
            subject: email
                .get("subject")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            body,
            message_id: message_ids("messageId").into_iter().next(),
            in_reply_to: message_ids("inReplyTo").into_iter().next(),
            thread_root: message_ids("references").into_iter().next(),
            attachments,
            auto_submitted: header("Auto-Submitted"),
            list_id: header("List-Id"),
            list_unsubscribe: header("List-Unsubscribe"),
            precedence: header("Precedence"),
            has_itip_part,
            id,
        })
    }
}

/// The contract's attachment kind, from the media type.
fn attachment_kind(mime_type: &str) -> &'static str {
    match mime_type.split('/').next().unwrap_or_default() {
        "image" => "image",
        "video" => "video",
        "audio" => "audio",
        _ => "file",
    }
}

/// Why a mail is not published as a person's message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dropped {
    /// A positive non-human signal: `Auto-Submitted`, a list header, a
    /// bulk precedence.
    NonHumanSender,
    /// An iTIP part: the calendar's business, not a message.
    CalendarInvitation,
    /// From the owner's own address: never a contact (ADR 0021).
    Owner,
}

impl Dropped {
    /// Every reason, for a counter that shows each at zero.
    pub const ALL: [Dropped; 3] = [
        Dropped::NonHumanSender,
        Dropped::CalendarInvitation,
        Dropped::Owner,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NonHumanSender => "non_human_sender",
            Self::CalendarInvitation => "calendar_invitation",
            Self::Owner => "owner",
        }
    }
}

/// The frontier: `Ok` is a person writing, `Err` says why not. Checked in
/// the order that names the more specific reason first — the owner's own
/// invitation is still the owner's.
pub fn frontier(mail: &Mail, owner_email: &str) -> Result<(), Dropped> {
    if mail.from.email == owner_email.trim().to_ascii_lowercase() {
        return Err(Dropped::Owner);
    }
    if mail.has_itip_part {
        return Err(Dropped::CalendarInvitation);
    }
    let auto_submitted = mail
        .auto_submitted
        .as_deref()
        .is_some_and(|value| !value.trim().eq_ignore_ascii_case("no"));
    let precedence = mail.precedence.as_deref().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "bulk" | "list" | "junk"
        )
    });
    if auto_submitted || mail.list_id.is_some() || mail.list_unsubscribe.is_some() || precedence {
        return Err(Dropped::NonHumanSender);
    }
    Ok(())
}

/// `direct` when the owner was the only recipient, `group` otherwise.
pub fn audience(mail: &Mail, owner_email: &str) -> &'static str {
    let owner = owner_email.trim().to_ascii_lowercase();
    let only_the_owner = mail.to.len() == 1 && mail.to[0].email == owner && mail.cc.is_empty();
    if only_the_owner {
        "direct"
    } else {
        "group"
    }
}

/// HTML reduced to text: tags dropped, block boundaries kept as line
/// breaks, the common entities decoded, whitespace collapsed. Not a
/// renderer — what a persona reads of a mail that carries no text part.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.char_indices().peekable();
    let lower = html.to_ascii_lowercase();
    let mut skip_until: Option<&str> = None;
    while let Some((index, c)) = chars.next() {
        if let Some(end) = skip_until {
            if lower[index..].starts_with(end) {
                for _ in 0..end.len() - 1 {
                    chars.next();
                }
                skip_until = None;
            }
            continue;
        }
        if c == '<' {
            let rest = &lower[index..];
            if rest.starts_with("<style") {
                skip_until = Some("</style>");
                continue;
            }
            if rest.starts_with("<script") {
                skip_until = Some("</script>");
                continue;
            }
            let block = [
                "<br",
                "<p",
                "</p",
                "<div",
                "</div",
                "<li",
                "<tr",
                "<h1",
                "<h2",
                "<h3",
                "<h4",
                "<blockquote",
                "</blockquote",
            ]
            .iter()
            .any(|tag| rest.starts_with(tag));
            if block {
                out.push('\n');
            }
            for (_, inner) in chars.by_ref() {
                if inner == '>' {
                    break;
                }
            }
            continue;
        }
        if c == '&' {
            let rest = &html[index..];
            let entities = [
                ("&amp;", "&"),
                ("&lt;", "<"),
                ("&gt;", ">"),
                ("&quot;", "\""),
                ("&#39;", "'"),
                ("&apos;", "'"),
                ("&nbsp;", " "),
            ];
            if let Some((entity, replacement)) = entities.iter().find(|(e, _)| rest.starts_with(e))
            {
                out.push_str(replacement);
                for _ in 0..entity.len() - 1 {
                    chars.next();
                }
                continue;
            }
        }
        out.push(c);
    }
    // Collapse runs of spaces on a line and more than one blank line.
    let mut lines: Vec<String> = Vec::new();
    for line in out.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() && lines.last().is_some_and(|last| last.is_empty()) {
            continue;
        }
        lines.push(collapsed);
    }
    lines.join("\n").trim().to_owned()
}

/// The schema's caps on the text fields: `data.body`, `data.title`,
/// `data.contact.display_name`.
pub const BODY_MAX: usize = 65_536;
pub const TITLE_MAX: usize = 1_024;
pub const DISPLAY_NAME_MAX: usize = 256;

/// The first `max` characters, on a character boundary.
fn capped(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Where the events of one mailbox come from.
#[derive(Debug, Clone)]
pub struct Envelopes {
    pub connection: String,
    /// `jmap://<host>/<accountId>/<INBOX id>`.
    pub source: String,
    pub account_id: String,
    pub owner_email: String,
}

impl Envelopes {
    pub fn new(
        connection: &str,
        host: &str,
        account_id: &str,
        inbox_id: &str,
        owner_email: &str,
    ) -> Self {
        Self {
            connection: connection.to_owned(),
            source: format!("jmap://{host}/{account_id}/{inbox_id}"),
            account_id: account_id.to_owned(),
            owner_email: owner_email.to_owned(),
        }
    }

    /// The envelope for a mail the frontier let through, in the shape the
    /// sender's consent selects: `granted` and `pending` whole, `revoked`
    /// without the words — no body, no title, and the attachments as shapes
    /// (which they are on every mail).
    pub fn message_received(&self, mail: &Mail, consent: Consent, time: &str) -> Value {
        let reduced = consent.reduces_publication();
        let attachments: Vec<Value> = mail
            .attachments
            .iter()
            .map(|attachment| {
                json!({
                    "kind": attachment.kind,
                    "mime_type": attachment.mime_type,
                    "size_bytes": attachment.size_bytes,
                })
            })
            .collect();
        // The From name; the address itself when From carries none — the
        // contract requires a display name, and the address is already the
        // subject, so nothing more is said by saying it here. Every text
        // field is held to the schema's cap, as the Sensor holds its own
        // (`sensor/src/normalize.rs`): a long mail is a valid event.
        let display_name = mail
            .from
            .name
            .clone()
            .unwrap_or_else(|| mail.from.email.clone());
        let mut data = json!({
            "format": "text/plain",
            "reply_to": mail
                .in_reply_to
                .as_ref()
                .map(|id| json!({ "message_id": id }))
                .unwrap_or(Value::Null),
            "attachments": attachments,
            "contact": { "display_name": capped(&display_name, DISPLAY_NAME_MAX) },
            "audience": audience(mail, &self.owner_email),
        });
        if !mail.received_at.is_empty() {
            data["network_timestamp"] = json!(mail.received_at);
        }
        if let Some(root) = &mail.thread_root {
            data["thread_root"] = json!(root);
        }
        if !reduced {
            data["body"] = json!(capped(&mail.body, BODY_MAX));
            if !mail.subject.is_empty() {
                data["title"] = json!(capped(&mail.subject, TITLE_MAX));
            }
        }
        json!({
            "specversion": "1.0",
            "id": sha256_hex(&format!("jmap:{}:{}", self.account_id, mail.id)),
            "source": self.source,
            "type": MESSAGE_RECEIVED_TYPE,
            "time": time,
            "subject": mail.from.mailto(),
            "datacontenttype": "application/json",
            "dataschema": SCHEMA,
            "network": "email",
            "connection": self.connection,
            "consent": consent.as_str(),
            "data": data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email_object() -> Value {
        json!({
            "id": "M4f2a9c",
            "mailboxIds": { "inbox-1": true },
            "receivedAt": "2026-09-21T08:14:58Z",
            "messageId": ["9b8c7d6e-1@example.org"],
            "inReplyTo": ["7a1e0c2b-9f3d-4b8e-a1c5-2d6f8e9b0c1a@example.com"],
            "references": ["c9d8e7f6-5a4b-3c2d-1e0f-9a8b7c6d5e4f@example.org", "7a1e0c2b-9f3d-4b8e-a1c5-2d6f8e9b0c1a@example.com"],
            "from": [{ "name": "Alice Martin", "email": "Alice@Example.org" }],
            "to": [{ "name": null, "email": "michel@example.com" }],
            "cc": null,
            "subject": "Re: Point hebdo",
            "textBody": [{ "partId": "1", "type": "text/plain" }],
            "htmlBody": [{ "partId": "2", "type": "text/html" }],
            "bodyValues": {
                "1": { "value": "Bonjour Michel,\n\nOn se voit toujours lundi pour le point hebdo ?\n\nAlice" },
                "2": { "value": "<p>Bonjour Michel,</p><p>On se voit toujours lundi ?</p>" }
            },
            "attachments": [{ "partId": "3", "blobId": "b3", "size": 48213, "name": "ordre-du-jour.pdf", "type": "application/pdf", "disposition": "attachment" }],
            "header:Auto-Submitted:asText": null,
            "header:List-Id:asText": null,
            "header:List-Unsubscribe:asText": null,
            "header:Precedence:asText": null
        })
    }

    #[test]
    fn a_mail_becomes_the_fixture_field_by_field_and_the_filename_never_leaves() {
        let mail = Mail::parse(&email_object()).unwrap();
        let envelopes = Envelopes::new(
            "mail-linagora",
            "mail.example.com",
            "u1",
            "inbox-1",
            "michel@example.com",
        );
        let event = envelopes.message_received(&mail, Consent::Granted, "2026-09-21T08:15:03Z");
        let fixture: Value = serde_json::from_str(include_str!(
            "../../contracts/cloudevents/v1/fixtures/variants/inbound.message.received/email.json"
        ))
        .unwrap();
        assert_eq!(
            event, fixture,
            "the fixture is what the collector publishes"
        );
        assert!(!event.to_string().contains("ordre-du-jour"));

        let reduced = envelopes.message_received(&mail, Consent::Revoked, "2026-09-21T08:15:03Z");
        let fixture: Value = serde_json::from_str(include_str!(
            "../../contracts/cloudevents/v1/fixtures/variants/inbound.message.received/email-revoked-sender.json"
        ))
        .unwrap();
        assert_eq!(reduced, fixture);
        assert!(reduced["data"].get("body").is_none() && reduced["data"].get("title").is_none());
    }

    #[test]
    fn a_long_mail_is_held_to_the_schemas_caps_and_a_mail_never_debugs_its_words() {
        let mut object = email_object();
        object["subject"] = json!("é".repeat(2_000));
        object["bodyValues"]["1"]["value"] = json!("x".repeat(100_000));
        object["from"][0]["name"] = json!("n".repeat(500));
        let mail = Mail::parse(&object).unwrap();
        let envelopes = Envelopes::new("mail-linagora", "h", "u1", "i", "michel@example.com");
        let event = envelopes.message_received(&mail, Consent::Pending, "2026-09-21T08:15:03Z");
        assert_eq!(
            event["data"]["body"].as_str().unwrap().chars().count(),
            BODY_MAX
        );
        assert_eq!(
            event["data"]["title"].as_str().unwrap().chars().count(),
            TITLE_MAX
        );
        assert_eq!(
            event["data"]["contact"]["display_name"]
                .as_str()
                .unwrap()
                .len(),
            DISPLAY_NAME_MAX
        );
        let debugged = format!("{mail:?}");
        assert!(
            !debugged.contains("xxxx") && !debugged.contains("éé"),
            "{debugged}"
        );
        assert!(debugged.contains("alice@example.org"));
    }

    #[test]
    fn the_frontier_drops_on_a_positive_signal_only() {
        let mut mail = Mail::parse(&email_object()).unwrap();
        assert_eq!(frontier(&mail, "michel@example.com"), Ok(()));
        assert_eq!(audience(&mail, "Michel@example.com"), "direct");

        mail.auto_submitted = Some("no".to_owned());
        assert_eq!(
            frontier(&mail, "michel@example.com"),
            Ok(()),
            "Auto-Submitted: no is a person"
        );
        mail.auto_submitted = Some("auto-generated".to_owned());
        assert_eq!(
            frontier(&mail, "michel@example.com"),
            Err(Dropped::NonHumanSender)
        );
        mail.auto_submitted = None;
        mail.list_unsubscribe = Some("<https://example.org/unsubscribe>".to_owned());
        assert_eq!(
            frontier(&mail, "michel@example.com"),
            Err(Dropped::NonHumanSender)
        );
        mail.list_unsubscribe = None;
        mail.precedence = Some("Bulk".to_owned());
        assert_eq!(
            frontier(&mail, "michel@example.com"),
            Err(Dropped::NonHumanSender)
        );
        mail.precedence = Some("first-class".to_owned());
        assert_eq!(frontier(&mail, "michel@example.com"), Ok(()));

        mail.has_itip_part = true;
        assert_eq!(
            frontier(&mail, "michel@example.com"),
            Err(Dropped::CalendarInvitation)
        );
        mail.has_itip_part = false;
        mail.from.email = "michel@example.com".to_owned();
        assert_eq!(frontier(&mail, "Michel@Example.com"), Err(Dropped::Owner));

        mail.cc.push(Person {
            name: None,
            email: "bob@example.org".to_owned(),
        });
        assert_eq!(audience(&mail, "michel@example.com"), "group");
    }

    #[test]
    fn an_html_only_mail_is_read_as_text_and_an_itip_part_is_seen_wherever_it_is() {
        let mut object = email_object();
        object["textBody"] = json!([]);
        object["bodyValues"].as_object_mut().unwrap().remove("1");
        object["bodyValues"]["2"]["value"] = json!("<html><head><style>p{color:red}</style></head><body><p>Bonjour &amp; bienvenue,</p><div>On se voit <b>lundi</b>&nbsp;?</div><br/>Alice</body></html>");
        let mail = Mail::parse(&object).unwrap();
        assert_eq!(
            mail.body, "Bonjour & bienvenue,\n\nOn se voit lundi ?\n\nAlice",
            "paragraphs stay paragraphs; the style block is gone"
        );

        let mut object = email_object();
        object["textBody"] = json!([{ "partId": "1", "type": "text/calendar" }]);
        assert!(Mail::parse(&object).unwrap().has_itip_part);
    }

    #[test]
    fn the_session_the_inbox_the_changes_and_a_method_error_are_read() {
        let session = Session::parse(&json!({
            "apiUrl": "https://mail.example.com/jmap/api",
            "primaryAccounts": { "urn:ietf:params:jmap:mail": "u1" },
            "accounts": { "u1": { "accountCapabilities": { "urn:ietf:params:jmap:mail": {} } } },
            "username": "michel@example.com"
        }))
        .unwrap();
        assert_eq!(session.account_id, "u1");
        assert!(Session::parse(&json!({ "apiUrl": "x", "accounts": {} })).is_err());

        assert_eq!(
            inbox_id(&json!({ "list": [
                { "id": "s", "name": "Sent", "role": "sent" },
                { "id": "i", "name": "Boîte", "role": "inbox" }
            ] })),
            Some("i".to_owned())
        );
        assert_eq!(
            inbox_id(&json!({ "list": [{ "id": "x", "name": "INBOX", "role": null }] })),
            Some("x".to_owned())
        );

        let response = json!({ "methodResponses": [
            ["Email/changes", { "newState": "7", "hasMoreChanges": false, "created": ["M1"], "updated": [], "destroyed": [] }, "c0"],
            ["error", { "type": "cannotCalculateChanges" }, "c1"]
        ] });
        let changes = Changes::parse(&method_result(&response, 0).unwrap());
        assert_eq!(changes.created, ["M1"]);
        assert_eq!(changes.new_state, "7");
        assert_eq!(
            method_result(&response, 1).unwrap_err().kind,
            "cannotCalculateChanges"
        );
        assert_eq!(
            in_mailbox(
                &json!({ "list": [
                { "id": "M1", "mailboxIds": { "inbox-1": true } },
                { "id": "M2", "mailboxIds": { "sent-1": true } }
            ] }),
                "inbox-1"
            ),
            ["M1"]
        );
        let request = request(vec![mailbox_get("u1"), email_state("u1")]);
        assert_eq!(request["methodCalls"][1][2], "c1");
        assert_eq!(request["methodCalls"][1][1]["ids"], json!([]));
    }
}
