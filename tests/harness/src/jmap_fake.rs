//! The fake JMAP server behind the fake SSO's grant (issue #276, RFC 8620,
//! RFC 8621): what the collector's `jmap` module and its mail poll are
//! tested against, so that no test needs a TMail. One account, three
//! mailboxes (INBOX, Sent, Archive), an Email state that moves on every
//! delivery, and the four methods the collector calls — `Mailbox/get`,
//! `Email/changes`, `Email/get`, and `Email/query` for the recovery #277
//! adds. A test delivers mail with [`FakeMail`]; nothing else writes.
//!
//! What it does not fake: blobs, keywords, threads beyond `threadId`, and
//! push — the collector reads none of them in #276.

use std::collections::BTreeMap;

use serde_json::{json, Value};

/// The JMAP account id the fake serves, and the three mailboxes' ids.
pub const ACCOUNT_ID: &str = "u1";
pub const INBOX_ID: &str = "inbox-1";
pub const SENT_ID: &str = "sent-1";
pub const ARCHIVE_ID: &str = "archive-1";

/// One person on a mail: the display name, when one, and the address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

impl Address {
    pub fn new(name: Option<&str>, email: &str) -> Self {
        Self {
            name: name.map(str::to_owned),
            email: email.to_owned(),
        }
    }

    fn json(&self) -> Value {
        json!({ "name": self.name, "email": self.email })
    }
}

/// A mail a test delivers. Everything a real Email object has that the
/// collector reads, and the raw headers the frontier decides on.
#[derive(Debug, Clone)]
pub struct FakeMail {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    /// The text part, when the mail has one.
    pub text: Option<String>,
    /// The HTML part, when the mail has one.
    pub html: Option<String>,
    pub message_id: String,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// Raw headers the collector asks for by name (`Auto-Submitted`,
    /// `List-Id`, `List-Unsubscribe`, `Precedence`, `Content-Type` of an
    /// iTIP part is `attachments` below).
    pub headers: Vec<(String, String)>,
    /// Attachments: (filename, media type, size).
    pub attachments: Vec<(String, String, u64)>,
    pub received_at: String,
}

impl FakeMail {
    /// A plain mail from a person to the owner: the shape most tests need.
    pub fn from_person(name: &str, email: &str, owner: &str, subject: &str, text: &str) -> Self {
        Self {
            from: Address::new(Some(name), email),
            to: vec![Address::new(None, owner)],
            cc: Vec::new(),
            subject: subject.to_owned(),
            text: Some(text.to_owned()),
            html: None,
            message_id: format!(
                "<{}@{}>",
                uuid_like(subject),
                email.split('@').nth(1).unwrap_or("example")
            ),
            in_reply_to: None,
            references: Vec::new(),
            headers: Vec::new(),
            attachments: Vec::new(),
            received_at: "2026-09-21T08:14:58Z".to_owned(),
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn attachment(mut self, filename: &str, media_type: &str, size: u64) -> Self {
        self.attachments
            .push((filename.to_owned(), media_type.to_owned(), size));
        self
    }
}

fn uuid_like(seed: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The mail store: every delivered mail by id, with the mailbox it is in
/// and the Email state it arrived at. Email ids start from a per-instance
/// offset, so two fakes on one shared bus never deliver two mails with one
/// id — and so one CloudEvents id the bus would deduplicate.
pub(crate) struct MailStore {
    mails: BTreeMap<String, (String, u64, FakeMail)>,
    /// The Email state: moves on every delivery.
    state: u64,
    /// States older than this are forgotten: `Email/changes` answers
    /// `cannotCalculateChanges` for them (#277).
    forgotten_before: u64,
    next_id: u64,
    /// Every Email id whose content (`bodyValues`) was read, in order —
    /// what "a mail in Sent or Archive is never read" is asserted on.
    read_ids: Vec<String>,
}

impl Default for MailStore {
    fn default() -> Self {
        let offset = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64 % 1_000_000_000)
            .unwrap_or(0);
        Self {
            mails: BTreeMap::new(),
            state: 0,
            forgotten_before: 0,
            next_id: offset * 1000,
            read_ids: Vec::new(),
        }
    }
}

impl MailStore {
    pub(crate) fn deliver(&mut self, mailbox: &str, mail: FakeMail) -> String {
        self.state += 1;
        self.next_id += 1;
        let id = format!("M{}", self.next_id);
        self.mails
            .insert(id.clone(), (mailbox.to_owned(), self.state, mail));
        id
    }

    pub(crate) fn state(&self) -> String {
        self.state.to_string()
    }

    pub(crate) fn forget_states_before(&mut self, state: u64) {
        self.forgotten_before = state;
    }

    pub(crate) fn read_ids(&self) -> Vec<String> {
        self.read_ids.clone()
    }
}

/// The session document, as the collector reads it: the account, its mail
/// capability, and where the API is.
pub(crate) fn session(account: &str, issuer: &str, state: &str) -> Value {
    json!({
        "capabilities": {
            "urn:ietf:params:jmap:core": {
                "maxSizeUpload": 50000000, "maxConcurrentUpload": 4, "maxSizeRequest": 10000000,
                "maxConcurrentRequests": 4, "maxCallsInRequest": 16, "maxObjectsInGet": 500,
                "maxObjectsInSet": 500, "collationAlgorithms": ["i;unicode-casemap"]
            },
            "urn:ietf:params:jmap:mail": {}
        },
        "accounts": {
            ACCOUNT_ID: {
                "name": account,
                "isPersonal": true,
                "isReadOnly": false,
                "accountCapabilities": {
                    "urn:ietf:params:jmap:mail": {
                        "maxMailboxesPerEmail": 10, "maxMailboxDepth": 10,
                        "maxSizeMailboxName": 200, "maxSizeAttachmentsPerEmail": 20000000,
                        "emailQuerySortOptions": ["receivedAt"], "mayCreateTopLevelMailbox": true
                    }
                }
            }
        },
        "primaryAccounts": { "urn:ietf:params:jmap:mail": ACCOUNT_ID },
        "username": account,
        "apiUrl": format!("{issuer}/jmap/api"),
        "downloadUrl": format!("{issuer}/jmap/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
        "uploadUrl": format!("{issuer}/jmap/upload/{{accountId}}"),
        "eventSourceUrl": format!("{issuer}/jmap/eventsource?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
        "state": state
    })
}

/// One API request: every method call answered in order, RFC 8620 §3.3.
pub(crate) fn api(body: &str, store: &mut MailStore) -> (&'static str, Value) {
    let Ok(request) = serde_json::from_str::<Value>(body) else {
        return (
            "400 Bad Request",
            json!({ "type": "urn:ietf:params:jmap:error:notJSON", "status": 400 }),
        );
    };
    let Some(calls) = request.get("methodCalls").and_then(Value::as_array) else {
        return (
            "400 Bad Request",
            json!({ "type": "urn:ietf:params:jmap:error:notRequest", "status": 400 }),
        );
    };
    let mut responses = Vec::new();
    for call in calls {
        let (Some(name), Some(args), Some(call_id)) = (
            call.get(0).and_then(Value::as_str),
            call.get(1),
            call.get(2),
        ) else {
            continue;
        };
        let account = args.get("accountId").and_then(Value::as_str);
        if account != Some(ACCOUNT_ID) {
            responses.push(json!(["error", { "type": "accountNotFound" }, call_id]));
            continue;
        }
        let (name, result) = match name {
            "Mailbox/get" => ("Mailbox/get", mailbox_get(store)),
            "Email/changes" => match email_changes(args, store) {
                Ok(result) => ("Email/changes", result),
                Err(error) => ("error", error),
            },
            "Email/get" => ("Email/get", email_get(args, store)),
            "Email/query" => ("Email/query", email_query(args, store)),
            _ => ("error", json!({ "type": "unknownMethod" })),
        };
        responses.push(json!([name, result, call_id]));
    }
    (
        "200 OK",
        json!({ "methodResponses": responses, "sessionState": "session-1" }),
    )
}

fn mailbox_get(store: &MailStore) -> Value {
    let mailbox = |id: &str, name: &str, role: Option<&str>| {
        json!({
            "id": id, "name": name, "parentId": null, "role": role, "sortOrder": 0,
            "totalEmails": store.mails.values().filter(|(m, _, _)| m == id).count(),
            "unreadEmails": 0, "totalThreads": 0, "unreadThreads": 0,
            "myRights": { "mayReadItems": true, "mayAddItems": true, "mayRemoveItems": true,
                          "maySetSeen": true, "maySetKeywords": true, "mayCreateChild": true,
                          "mayRename": true, "mayDelete": true, "maySubmit": true },
            "isSubscribed": true
        })
    };
    json!({
        "accountId": ACCOUNT_ID,
        "state": "mbx-1",
        "list": [
            mailbox(INBOX_ID, "INBOX", Some("inbox")),
            mailbox(SENT_ID, "Sent", Some("sent")),
            mailbox(ARCHIVE_ID, "Archive", Some("archive")),
        ],
        "notFound": []
    })
}

fn email_changes(args: &Value, store: &MailStore) -> Result<Value, Value> {
    let since: u64 = args
        .get("sinceState")
        .and_then(Value::as_str)
        .and_then(|state| state.parse().ok())
        .ok_or_else(|| json!({ "type": "invalidArguments", "description": "sinceState is not a state this server issued" }))?;
    if since < store.forgotten_before {
        return Err(json!({ "type": "cannotCalculateChanges" }));
    }
    let created: Vec<&String> = store
        .mails
        .iter()
        .filter(|(_, (_, at, _))| *at > since)
        .map(|(id, _)| id)
        .collect();
    Ok(json!({
        "accountId": ACCOUNT_ID,
        "oldState": since.to_string(),
        "newState": store.state(),
        "hasMoreChanges": false,
        "created": created,
        "updated": [],
        "destroyed": []
    }))
}

fn email_get(args: &Value, store: &mut MailStore) -> Value {
    let ids: Vec<String> = args
        .get("ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let properties: Option<Vec<String>> =
        args.get("properties").and_then(Value::as_array).map(|p| {
            p.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    for id in ids {
        match store.mails.get(&id) {
            Some((mailbox, _, mail)) => {
                list.push(email_object(&id, mailbox, mail, properties.as_deref()))
            }
            None => not_found.push(id),
        }
    }
    json!({
        "accountId": ACCOUNT_ID,
        "state": store.state(),
        "list": list,
        "notFound": not_found
    })
}

/// The Email object, with the properties asked for — every one when none
/// were named — the way RFC 8621 §4.1 has a server answer, `header:<Name>:asText`
/// included.
fn email_object(id: &str, mailbox: &str, mail: &FakeMail, properties: Option<&[String]>) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("id".to_owned(), json!(id));
    let wants = |name: &str| properties.is_none_or(|p| p.iter().any(|w| w == name));
    if wants("blobId") {
        object.insert("blobId".to_owned(), json!(format!("b{id}")));
    }
    if wants("threadId") {
        object.insert(
            "threadId".to_owned(),
            json!(format!(
                "t{}",
                mail.references
                    .first()
                    .unwrap_or(&mail.message_id)
                    .trim_matches(|c| c == '<' || c == '>')
            )),
        );
    }
    if wants("mailboxIds") {
        object.insert("mailboxIds".to_owned(), json!({ mailbox: true }));
    }
    if wants("keywords") {
        object.insert("keywords".to_owned(), json!({}));
    }
    if wants("size") {
        object.insert("size".to_owned(), json!(4321));
    }
    if wants("receivedAt") {
        object.insert("receivedAt".to_owned(), json!(mail.received_at));
    }
    if wants("messageId") {
        object.insert(
            "messageId".to_owned(),
            json!([mail.message_id.trim_matches(|c| c == '<' || c == '>')]),
        );
    }
    if wants("inReplyTo") {
        object.insert(
            "inReplyTo".to_owned(),
            json!(mail
                .in_reply_to
                .as_deref()
                .map(|v| vec![v.trim_matches(|c| c == '<' || c == '>').to_owned()])),
        );
    }
    if wants("references") {
        object.insert(
            "references".to_owned(),
            if mail.references.is_empty() {
                Value::Null
            } else {
                json!(mail
                    .references
                    .iter()
                    .map(|r| r.trim_matches(|c| c == '<' || c == '>'))
                    .collect::<Vec<_>>())
            },
        );
    }
    if wants("from") {
        object.insert("from".to_owned(), json!([mail.from.json()]));
    }
    if wants("to") {
        object.insert(
            "to".to_owned(),
            json!(mail.to.iter().map(Address::json).collect::<Vec<_>>()),
        );
    }
    if wants("cc") {
        object.insert(
            "cc".to_owned(),
            if mail.cc.is_empty() {
                Value::Null
            } else {
                json!(mail.cc.iter().map(Address::json).collect::<Vec<_>>())
            },
        );
    }
    if wants("subject") {
        object.insert("subject".to_owned(), json!(mail.subject));
    }
    if wants("sentAt") {
        object.insert("sentAt".to_owned(), json!(mail.received_at));
    }
    if wants("hasAttachment") {
        object.insert(
            "hasAttachment".to_owned(),
            json!(!mail.attachments.is_empty()),
        );
    }
    if wants("preview") {
        object.insert(
            "preview".to_owned(),
            json!(mail
                .text
                .as_deref()
                .unwrap_or_default()
                .chars()
                .take(256)
                .collect::<String>()),
        );
    }
    let mut body_values = serde_json::Map::new();
    let mut text_body = Vec::new();
    let mut html_body = Vec::new();
    if let Some(text) = &mail.text {
        body_values.insert(
            "1".to_owned(),
            json!({ "value": text, "isEncodingProblem": false, "isTruncated": false }),
        );
        text_body.push(json!({ "partId": "1", "blobId": format!("b{id}-1"), "size": text.len(), "type": "text/plain", "charset": "utf-8" }));
    }
    if let Some(html) = &mail.html {
        body_values.insert(
            "2".to_owned(),
            json!({ "value": html, "isEncodingProblem": false, "isTruncated": false }),
        );
        html_body.push(json!({ "partId": "2", "blobId": format!("b{id}-2"), "size": html.len(), "type": "text/html", "charset": "utf-8" }));
    }
    if wants("textBody") {
        object.insert("textBody".to_owned(), json!(text_body));
    }
    if wants("htmlBody") {
        object.insert("htmlBody".to_owned(), json!(html_body));
    }
    if wants("bodyValues") {
        object.insert("bodyValues".to_owned(), Value::Object(body_values));
    }
    if wants("attachments") {
        object.insert(
            "attachments".to_owned(),
            json!(mail
                .attachments
                .iter()
                .enumerate()
                .map(|(index, (name, media_type, size))| json!({
                    "partId": format!("{}", index + 3),
                    "blobId": format!("b{id}-{}", index + 3),
                    "size": size,
                    "name": name,
                    "type": media_type,
                    "disposition": "attachment"
                }))
                .collect::<Vec<_>>()),
        );
    }
    if let Some(properties) = properties {
        for property in properties {
            if let Some(header) = property
                .strip_prefix("header:")
                .and_then(|rest| rest.strip_suffix(":asText"))
            {
                let value = mail
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(header))
                    .map(|(_, value)| json!(value))
                    .unwrap_or(Value::Null);
                object.insert(property.clone(), value);
            }
        }
    }
    Value::Object(object)
}

/// `Email/query` with `inMailbox` and `after` (receivedAt) filters: the
/// recovery read (#277).
fn email_query(args: &Value, store: &MailStore) -> Value {
    let filter = args.get("filter").cloned().unwrap_or(Value::Null);
    let in_mailbox = filter.get("inMailbox").and_then(Value::as_str);
    let after = filter.get("after").and_then(Value::as_str);
    let ids: Vec<&String> = store
        .mails
        .iter()
        .filter(|(_, (mailbox, _, mail))| {
            in_mailbox.is_none_or(|wanted| wanted == mailbox)
                && after.is_none_or(|after| mail.received_at.as_str() >= after)
        })
        .map(|(id, _)| id)
        .collect();
    json!({
        "accountId": ACCOUNT_ID,
        "queryState": store.state(),
        "canCalculateChanges": false,
        "position": 0,
        "ids": ids,
        "total": ids.len()
    })
}
