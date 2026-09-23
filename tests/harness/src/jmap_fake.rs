//! The fake JMAP server behind the fake SSO's grant (issue #276, RFC 8620,
//! RFC 8621): what the collector's `jmap` module and its mail poll are
//! tested against, so that no test needs a TMail. One account, three
//! mailboxes (INBOX, Sent, Archive), an Email state that moves on every
//! delivery, and the methods the collector calls: `Mailbox/get`,
//! `Email/changes`, `Email/get` to read (#276); `Identity/get`,
//! `Email/query` by Message-ID, `Email/set` and `EmailSubmission/set` to
//! reply (#278). A test delivers mail with [`FakeMail`]; the collector's
//! replies are what [`Submission`] records.
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
    /// Keywords, as a creation set them (`$draft`, `$seen`); empty on a
    /// delivered mail.
    pub keywords: Vec<String>,
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
            received_at: now_rfc3339(),
            keywords: Vec::new(),
        }
    }

    /// A mail received at another instant than now (#277): what a
    /// look-back window is proved against.
    pub fn received(mut self, at: &str) -> Self {
        self.received_at = at.to_owned();
        self
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
    /// States older than this are forgotten: `Email/changes` from one
    /// answers `cannotCalculateChanges` (#277), which the collector recovers
    /// from by querying the look-back window.
    forgotten_before: u64,
    next_id: u64,
    /// Where deliveries are announced to the push endpoint (#277).
    pub(crate) push: Option<tokio::sync::broadcast::Sender<String>>,
    /// Every Email id whose content (`bodyValues`) was read, in order —
    /// what "a mail in Sent or Archive is never read" is asserted on.
    read_ids: Vec<String>,
    /// Every submission the collector made (#278), in order: the mail as
    /// created by `Email/set`, and what `EmailSubmission/set` said.
    submissions: Vec<Submission>,
    /// A test can make `EmailSubmission/set` refuse (`pending_operator`'s
    /// shape on the sending side): every submission answers `notCreated`.
    refuse_submissions: bool,
    /// Which form of a `Message-ID` this server's search index holds
    /// (#331): the value as written, `<id@host>`, or the parsed one
    /// without its brackets. A real server holds one of them and answers
    /// an empty list — never a refusal — to a filter written the other
    /// way, which reads exactly like a mail that is not there.
    bare_message_id_index: bool,
    /// Whether this server answers a `header` filter at all (#331). TMail
    /// answers an empty list to one on `Message-ID`, in either form, for a
    /// mail that is in the mailbox — and an empty list is what an absent
    /// mail looks like, so a client cannot tell the difference.
    no_header_filter: bool,
}

/// One reply the collector submitted, as the fake received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    /// The created Email object's id.
    pub email_id: String,
    pub identity_id: String,
    /// The SMTP envelope the collector asked for — who the server is told to
    /// deliver to — beside the headers of the mail it wrote.
    pub envelope_from: String,
    pub envelope_to: Vec<String>,
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub text: String,
    /// The mailboxes the created mail was in when submitted, then after
    /// `onSuccessUpdateEmail` was applied; the keywords likewise, from the
    /// keywords the collector set on the created mail.
    pub mailboxes_at_submission: Vec<String>,
    pub mailboxes_after: Vec<String>,
    pub keywords_after: Vec<String>,
    /// Every header the created mail carried by name (`header:<Name>:asText`
    /// on the creation), the approval's id among them.
    pub headers: Vec<(String, String)>,
}

/// The owner's JMAP identity id on the fake.
pub const IDENTITY_ID: &str = "id-owner";
pub const DRAFTS_ID: &str = "drafts-1";

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
            push: None,
            read_ids: Vec::new(),
            submissions: Vec::new(),
            refuse_submissions: false,
            bare_message_id_index: false,
            // As measured against TMail (#331): the fake follows the one
            // real server this project has run against, and a test that
            // needs the filter path says so.
            no_header_filter: true,
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
        if let Some(push) = &self.push {
            let _ = push.send(self.state());
        }
        id
    }

    pub(crate) fn forget_states_before(&mut self, state: u64) {
        self.forgotten_before = state;
    }

    pub(crate) fn state(&self) -> String {
        self.state.to_string()
    }

    pub(crate) fn read_ids(&self) -> Vec<String> {
        self.read_ids.clone()
    }

    pub(crate) fn submissions(&self) -> Vec<Submission> {
        self.submissions.clone()
    }

    pub(crate) fn mails_in(&self, mailbox: &str) -> Vec<String> {
        self.mails
            .iter()
            .filter(|(_, (m, _, _))| m == mailbox)
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub(crate) fn refuse_submissions(&mut self, refuse: bool) {
        self.refuse_submissions = refuse;
    }

    pub(crate) fn index_message_ids_bare(&mut self, bare: bool) {
        self.bare_message_id_index = bare;
    }

    pub(crate) fn answer_no_header_filter(&mut self, none: bool) {
        self.no_header_filter = none;
    }
}

/// The instant, to the second, as a JMAP `receivedAt`.
pub fn now_rfc3339() -> String {
    let now = time::OffsetDateTime::now_utc();
    now.replace_nanosecond(0)
        .unwrap_or(now)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// What the session says about push (#277): where the socket is, and
/// whether TMail's ticket endpoint is named beside it.
#[derive(Debug, Clone)]
pub(crate) struct PushOffer {
    pub(crate) url: String,
    pub(crate) with_ticket: bool,
}

/// The session document, as the collector reads it: the account, its mail
/// capability, and where the API is.
pub(crate) fn session(account: &str, issuer: &str, state: &str, push: Option<&PushOffer>) -> Value {
    let mut capabilities = json!({
        "urn:ietf:params:jmap:core": {
            "maxSizeUpload": 50000000, "maxConcurrentUpload": 4, "maxSizeRequest": 10000000,
            "maxConcurrentRequests": 4, "maxCallsInRequest": 16, "maxObjectsInGet": 500,
            "maxObjectsInSet": 500, "collationAlgorithms": ["i;unicode-casemap"]
        },
        "urn:ietf:params:jmap:mail": {},
        "urn:ietf:params:jmap:submission": {}
    });
    // Push (#277): RFC 8887's capability names the socket; TMail's names
    // the ticket endpoint a browser — and this collector — opens it with.
    if let Some(push) = push {
        capabilities["urn:ietf:params:jmap:websocket"] =
            json!({ "url": push.url, "supportsPush": true });
        if push.with_ticket {
            capabilities["com:linagora:params:jmap:ws:ticket"] =
                json!({ "generationEndpoint": format!("{issuer}/jmap/ws/ticket") });
        }
    }
    json!({
        "capabilities": capabilities,
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
                    },
                    "urn:ietf:params:jmap:submission": {
                        "maxDelayedSend": 0,
                        "submissionExtensions": {}
                    }
                }
            }
        },
        "primaryAccounts": { "urn:ietf:params:jmap:mail": ACCOUNT_ID, "urn:ietf:params:jmap:submission": ACCOUNT_ID },
        "username": account,
        "apiUrl": format!("{issuer}/jmap/api"),
        "downloadUrl": format!("{issuer}/jmap/download/{{accountId}}/{{blobId}}/{{name}}?type={{type}}"),
        "uploadUrl": format!("{issuer}/jmap/upload/{{accountId}}"),
        "eventSourceUrl": format!("{issuer}/jmap/eventsource?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
        "state": state
    })
}

/// One API request: every method call answered in order, RFC 8620 §3.3.
pub(crate) fn api(body: &str, store: &mut MailStore, account: &str) -> (&'static str, Value) {
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
    // What the request says it uses (RFC 8620 §3.2). A request that
    // declares nothing is not a request: everything below is answered
    // against this list, and a call whose capability is missing from it is
    // `unknownMethod` — which is what a real server answers and what this
    // fake used to let through (#328).
    let Some(using) = request.get("using").and_then(Value::as_array) else {
        return (
            "400 Bad Request",
            json!({ "type": "urn:ietf:params:jmap:error:notRequest", "status": 400 }),
        );
    };
    let using: Vec<&str> = using.iter().filter_map(Value::as_str).collect();
    // And a capability this server does not offer is refused whole, which
    // is what a server does with a `using` list it cannot honour.
    if let Some(unknown) = using
        .iter()
        .find(|capability| !OFFERED_CAPABILITIES.contains(capability))
    {
        return (
            "400 Bad Request",
            json!({ "type": "urn:ietf:params:jmap:error:unknownCapability", "status": 400, "detail": format!("this server does not offer {unknown}") }),
        );
    }
    let mut responses = Vec::new();
    // Creation ids (`#reply`) of this request, for a back-reference from a
    // later call (RFC 8620 §3.7).
    let mut created: BTreeMap<String, String> = BTreeMap::new();
    for call in calls {
        let (Some(name), Some(args), Some(call_id)) = (
            call.get(0).and_then(Value::as_str),
            call.get(1),
            call.get(2),
        ) else {
            continue;
        };
        // RFC 8620 §3.2: a call whose capability the request did not
        // declare is `unknownMethod`, and so is a method of an object this
        // server does not serve at all.
        if !capability_of(name).is_some_and(|capability| using.contains(&capability)) {
            responses.push(json!(["error", { "type": "unknownMethod" }, call_id]));
            continue;
        }
        let account_id = args.get("accountId").and_then(Value::as_str);
        if account_id != Some(ACCOUNT_ID) {
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
            "Identity/get" => ("Identity/get", identity_get(account)),
            "Email/query" => ("Email/query", email_query(args, store)),
            "Email/set" => ("Email/set", email_set(args, store, &mut created)),
            "EmailSubmission/set" => match submission_set(args, store, &created) {
                Ok(result) => ("EmailSubmission/set", result),
                Err(error) => ("error", error),
            },
            _ => ("error", json!({ "type": "unknownMethod" })),
        };
        responses.push(json!([name, result, call_id]));
    }
    (
        "200 OK",
        json!({ "methodResponses": responses, "sessionState": "session-1" }),
    )
}

/// What this server offers, and therefore the only capabilities a request
/// may say it uses (RFC 8620 §3.2).
const OFFERED_CAPABILITIES: [&str; 3] = [
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:submission",
];

/// The capability a method belongs to. `Identity/get` and
/// `EmailSubmission/set` are the submission capability's (RFC 8621 §6 and
/// §7), not the mail capability's — the distinction #328 was found by, on
/// a real server, after every test here had passed.
///
/// The collector holds the same table (`capability_of` in
/// `collector/src/jmap.rs`), where it decides what a request declares;
/// here it decides what this server accepts. They are two sides of one
/// rule and are edited together.
fn capability_of(method: &str) -> Option<&'static str> {
    // A server knows which objects it serves and answers `unknownMethod`
    // for the rest, rather than guessing a capability for a name it does
    // not recognise — a fake that guessed would agree with a client that
    // guessed the same way, and prove nothing.
    match method.split('/').next().unwrap_or_default() {
        "Email" | "Mailbox" | "Thread" | "SearchSnippet" => Some("urn:ietf:params:jmap:mail"),
        "Identity" | "EmailSubmission" => Some("urn:ietf:params:jmap:submission"),
        _ => None,
    }
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
            mailbox(DRAFTS_ID, "Drafts", Some("drafts")),
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
    // A read of the content is one that asks for `bodyValues` — or for
    // everything; a read of the mailboxes alone is not a read of the mail.
    let reads_content = properties
        .as_deref()
        .is_none_or(|p| p.iter().any(|w| w == "bodyValues"));
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    for id in ids {
        match store.mails.get(&id) {
            Some((mailbox, _, mail)) => {
                if reads_content {
                    store.read_ids.push(id.clone());
                }
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
    // `header:<name>:asText` (RFC 8621 §4.1.4): a property read off the
    // mail itself, which is how a client asks about a header a server
    // does not index (#331). Asked for by name, answered by name.
    for asked in properties.unwrap_or(&[]) {
        if let Some(name) = asked
            .strip_prefix("header:")
            .and_then(|rest| rest.strip_suffix(":asText"))
        {
            let value = mail
                .headers
                .iter()
                .find(|(held, _)| held.eq_ignore_ascii_case(name))
                .map(|(_, value)| Value::String(value.trim().to_owned()))
                .unwrap_or(Value::Null);
            object.insert(asked.clone(), value);
        }
    }
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

/// The owner's one identity: the account's address.
fn identity_get(account: &str) -> Value {
    json!({
        "accountId": ACCOUNT_ID,
        "state": "idn-1",
        "list": [{
            "id": IDENTITY_ID,
            "name": "The Owner",
            "email": account,
            "replyTo": null,
            "bcc": null,
            "textSignature": "",
            "htmlSignature": "",
            "mayDelete": false
        }],
        "notFound": []
    })
}

/// `Email/query` with the filters the collector uses: `inMailbox`, `after`
/// on `receivedAt` (#277's look-back, oldest first, `position` and `limit`
/// honoured so a recovery pages), and `header: ["Message-ID", "<…>"]` to
/// find the mail a reply answers (#278).
fn email_query(args: &Value, store: &MailStore) -> Value {
    let filter = args.get("filter").cloned().unwrap_or(Value::Null);
    let in_mailbox = filter.get("inMailbox").and_then(Value::as_str);
    let header = filter
        .get("header")
        .and_then(Value::as_array)
        .and_then(|pair| {
            Some((
                pair.first()?.as_str()?.to_ascii_lowercase(),
                pair.get(1)?.as_str()?.trim().to_owned(),
            ))
        });
    let after = filter.get("after").and_then(Value::as_str);
    // A server whose index holds no headers answers an empty list, not a
    // refusal (#331): the shape TMail showed on the reference deployment.
    if store.no_header_filter && header.is_some() {
        return json!({ "accountId": args.get("accountId"), "ids": [], "position": 0, "total": 0 });
    }
    let descending = args
        .get("sort")
        .and_then(Value::as_array)
        .and_then(|sort| sort.first())
        .is_some_and(|by| by.get("isAscending") == Some(&Value::Bool(false)));
    let ids: Vec<&String> = store
        .mails
        .iter()
        .filter(|(_, (mailbox, _, mail))| {
            in_mailbox.is_none_or(|wanted| wanted == mailbox)
                && after.is_none_or(|after| mail.received_at.as_str() >= after)
                && header.as_ref().is_none_or(|(name, value)| {
                    if name == "message-id" {
                        // Exactly as this server indexes it, and not both
                        // ways: a fake more forgiving than the real
                        // service proves nothing (#328's lesson, #331's
                        // case).
                        let indexed = if store.bare_message_id_index {
                            mail.message_id.trim_matches(|c| c == '<' || c == '>')
                        } else {
                            mail.message_id.as_str()
                        };
                        indexed == value
                    } else {
                        mail.headers
                            .iter()
                            .any(|(n, v)| n.eq_ignore_ascii_case(name) && v.trim() == value)
                    }
                })
        })
        .map(|(id, (_, delivered, mail))| (mail.received_at.as_str(), *delivered, id))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|(_, _, id)| id)
        .collect();
    let ids: Vec<&String> = if descending {
        ids.into_iter().rev().collect()
    } else {
        ids
    };
    let total = ids.len();
    let position = args.get("position").and_then(Value::as_u64).unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(total, |limit| limit as usize);
    let page: Vec<&String> = ids.into_iter().skip(position).take(limit).collect();
    json!({
        "accountId": ACCOUNT_ID,
        "queryState": store.state(),
        "canCalculateChanges": false,
        "position": position,
        "ids": page,
        "total": total
    })
}

/// `Email/set` with `create`: the reply the collector writes, kept as a mail
/// in the mailboxes it named (Drafts, as a client does), so the submission
/// can move it to Sent.
fn email_set(args: &Value, store: &mut MailStore, created: &mut BTreeMap<String, String>) -> Value {
    let mut created_out = serde_json::Map::new();
    let mut not_created = serde_json::Map::new();
    if let Some(creations) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in creations {
            let addresses = |name: &str| -> Vec<Address> {
                object
                    .get(name)
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(|a| {
                                Some(Address::new(
                                    a.get("name").and_then(Value::as_str),
                                    a.get("email")?.as_str()?,
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            // The body is what `textBody` names, not whatever
            // `bodyValues` happens to hold: a part nobody points at is
            // not a body, and a server that stored it anyway would hide
            // a create that sends an empty mail (#332).
            let text = object
                .get("textBody")
                .and_then(Value::as_array)
                .and_then(|parts| parts.first())
                .and_then(|part| part.get("partId"))
                .and_then(Value::as_str)
                .and_then(|part_id| {
                    object
                        .get("bodyValues")
                        .and_then(Value::as_object)?
                        .get(part_id)?
                        .get("value")?
                        .as_str()
                })
                .unwrap_or_default()
                .to_owned();
            let ids = |name: &str| -> Vec<String> {
                object
                    .get(name)
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(Value::as_str)
                            .map(|s| format!("<{}>", s.trim_matches(|c| c == '<' || c == '>')))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let from = addresses("from");
            let Some(sender) = from.first().cloned() else {
                not_created.insert(
                    creation_id.clone(),
                    json!({ "type": "invalidProperties", "properties": ["from"] }),
                );
                continue;
            };
            let mailbox = object
                .get("mailboxIds")
                .and_then(Value::as_object)
                .and_then(|m| m.keys().next().cloned())
                .unwrap_or_else(|| DRAFTS_ID.to_owned());
            // `header:<Name>:asText` on a creation is a header of the mail
            // (RFC 8621 §4.1.3), kept by name so a query finds it.
            let headers: Vec<(String, String)> = object
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(key, value)| {
                    let name = key.strip_prefix("header:")?.strip_suffix(":asText")?;
                    Some((name.to_owned(), value.as_str()?.to_owned()))
                })
                .collect();
            let keywords: Vec<String> = object
                .get("keywords")
                .and_then(Value::as_object)
                .map(|k| k.keys().cloned().collect())
                .unwrap_or_default();
            let mail = FakeMail {
                from: sender,
                to: addresses("to"),
                cc: addresses("cc"),
                subject: object
                    .get("subject")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                text: Some(text),
                html: None,
                message_id: ids("messageId")
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| format!("<{}@fake>", store.next_id + 1)),
                in_reply_to: ids("inReplyTo").into_iter().next(),
                references: ids("references"),
                headers,
                attachments: Vec::new(),
                received_at: "2026-09-21T08:20:12Z".to_owned(),
                keywords,
            };
            let id = store.deliver(&mailbox, mail);
            created.insert(creation_id.clone(), id.clone());
            created_out.insert(creation_id.clone(), json!({ "id": id, "blobId": format!("b{id}"), "threadId": format!("t{id}"), "size": 1234 }));
        }
    }
    // `destroy`: the draft a refused submission left behind, removed.
    let mut destroyed = Vec::new();
    for id in args
        .get("destroy")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if store.mails.remove(id).is_some() {
            store.state += 1;
            destroyed.push(id.to_owned());
        }
    }
    json!({
        "accountId": ACCOUNT_ID,
        "oldState": (store.state.saturating_sub(1)).to_string(),
        "newState": store.state(),
        "created": created_out,
        "notCreated": not_created,
        "updated": null,
        "destroyed": destroyed
    })
}

/// `EmailSubmission/set` with `create` and `onSuccessUpdateEmail`: the
/// submission recorded with everything the reply carried, and the created
/// mail moved as the update says (to Sent, out of Drafts, `$draft` off).
fn submission_set(
    args: &Value,
    store: &mut MailStore,
    created: &BTreeMap<String, String>,
) -> Result<Value, Value> {
    let mut created_out = serde_json::Map::new();
    let mut not_created = serde_json::Map::new();
    let mut updated_emails = serde_json::Map::new();
    if let Some(creations) = args.get("create").and_then(Value::as_object) {
        for (creation_id, object) in creations {
            let email_ref = object
                .get("emailId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let email_id = email_ref
                .strip_prefix('#')
                .and_then(|reference| created.get(reference).cloned())
                .unwrap_or_else(|| email_ref.to_owned());
            let identity_id = object
                .get("identityId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if store.refuse_submissions {
                not_created.insert(creation_id.clone(), json!({ "type": "forbiddenFrom", "description": "the fake was told to refuse every submission" }));
                continue;
            }
            let Some((mailbox, _, mail)) = store.mails.get(&email_id).cloned() else {
                not_created.insert(creation_id.clone(), json!({ "type": "emailNotFound" }));
                continue;
            };
            if identity_id != IDENTITY_ID {
                not_created.insert(
                    creation_id.clone(),
                    json!({ "type": "invalidProperties", "properties": ["identityId"] }),
                );
                continue;
            }
            let envelope_from = object
                .pointer("/envelope/mailFrom/email")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let envelope_to: Vec<String> = object
                .pointer("/envelope/rcptTo")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|r| r.get("email").and_then(Value::as_str))
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let mailboxes_at_submission = vec![mailbox.clone()];
            // onSuccessUpdateEmail: `#<creationId>` → the patch.
            let mut mailboxes_after = mailboxes_at_submission.clone();
            let mut keywords_after = mail.keywords.clone();
            if let Some(patch) = args
                .pointer(&format!("/onSuccessUpdateEmail/#{creation_id}"))
                .and_then(Value::as_object)
            {
                for (path, value) in patch {
                    if let Some(id) = path.strip_prefix("mailboxIds/") {
                        if value.is_null() || value == &Value::Bool(false) {
                            mailboxes_after.retain(|m| m != id);
                        } else {
                            mailboxes_after.push(id.to_owned());
                        }
                    } else if let Some(keyword) = path.strip_prefix("keywords/") {
                        if value.is_null() || value == &Value::Bool(false) {
                            keywords_after.retain(|k| k != keyword);
                        } else {
                            keywords_after.push(keyword.to_owned());
                        }
                    }
                }
                if let Some(target) = mailboxes_after.first().cloned() {
                    if let Some(entry) = store.mails.get_mut(&email_id) {
                        entry.0 = target;
                    }
                }
                updated_emails.insert(email_id.clone(), Value::Null);
            }
            store.submissions.push(Submission {
                email_id: email_id.clone(),
                identity_id,
                envelope_from,
                envelope_to,
                from: vec![mail.from.email.clone()],
                to: mail.to.iter().map(|a| a.email.clone()).collect(),
                cc: mail.cc.iter().map(|a| a.email.clone()).collect(),
                subject: mail.subject.clone(),
                in_reply_to: mail.in_reply_to.iter().cloned().collect(),
                references: mail.references.clone(),
                text: mail.text.clone().unwrap_or_default(),
                mailboxes_at_submission,
                mailboxes_after,
                keywords_after,
                headers: mail.headers.clone(),
            });
            created_out.insert(creation_id.clone(), json!({ "id": format!("s{}", store.submissions.len()), "sendAt": "2026-09-21T08:20:12Z", "undoStatus": "final" }));
        }
    }
    Ok(json!({
        "accountId": ACCOUNT_ID,
        "oldState": "sub-0",
        "newState": "sub-1",
        "created": created_out,
        "notCreated": not_created
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8620 §3.2: a server answers `unknownMethod` to a call whose
    /// capability the request did not declare. The fake owed the suites
    /// this rule — without it, a request that asks for `Identity/get`
    /// under the mail capability alone passes here and is refused by every
    /// real server (#328).
    #[test]
    fn a_call_whose_capability_is_not_declared_is_unknown_to_this_server() {
        let mut store = MailStore::default();
        let mut ask = |using: &str| {
            let body = json!({
                "using": ["urn:ietf:params:jmap:core", using],
                "methodCalls": [["Identity/get", { "accountId": ACCOUNT_ID, "ids": null }, "c0"]]
            })
            .to_string();
            let (status, answer) = api(&body, &mut store, "owner@example.com");
            assert_eq!(status, "200 OK");
            answer["methodResponses"][0][0]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        };
        assert_eq!(ask("urn:ietf:params:jmap:mail"), "error");
        assert_eq!(ask("urn:ietf:params:jmap:submission"), "Identity/get");
    }

    /// A capability this server does not offer is refused whole.
    #[test]
    fn a_capability_this_server_does_not_offer_is_refused_whole() {
        let mut store = MailStore::default();
        let body = json!({
            "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
            "methodCalls": [["Mailbox/get", { "accountId": ACCOUNT_ID }, "c0"]]
        })
        .to_string();
        let (status, answer) = api(&body, &mut store, "owner@example.com");
        assert_eq!(status, "400 Bad Request");
        assert_eq!(
            answer["type"],
            "urn:ietf:params:jmap:error:unknownCapability"
        );
    }

    /// And a request that declares nothing at all is not a request.
    #[test]
    fn a_request_with_no_using_list_is_refused_whole() {
        let mut store = MailStore::default();
        let body = json!({
            "methodCalls": [["Mailbox/get", { "accountId": ACCOUNT_ID }, "c0"]]
        })
        .to_string();
        let (status, answer) = api(&body, &mut store, "owner@example.com");
        assert_eq!(status, "400 Bad Request");
        assert_eq!(answer["type"], "urn:ietf:params:jmap:error:notRequest");
    }
}
