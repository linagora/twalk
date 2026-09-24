//! Approval: the human act that turns a suggestion into an outbound reply
//! (ticket #24).
//!
//! `CONTEXT.md` defines it with unusual precision, and every clause of the
//! definition is a line of this module:
//!
//! > The human act that turns a suggestion into an outbound reply, carrying
//! > the identity of whoever approved it. Deliberate by construction — an
//! > explicit call, never a default, never a batch — and refused if the
//! > sender's consent is no longer `granted` at that moment.
//!
//! - **an explicit call**: `POST /api/approvals`, and nothing else in this
//!   Gateway publishes `persona.reply.approved`. No timer, no rule, no
//!   "auto-send" setting — the design review that struck auto-send off
//!   screen 4 (#74) struck it here too.
//! - **never a batch**: the request names exactly one suggestion. A body
//!   carrying a list is refused as malformed rather than helpfully
//!   interpreted, because a batch approval is a single click that sends
//!   several messages and that is the thing this sentence forbids.
//! - **carrying the identity of whoever approved it**: `approved_by` is the
//!   deployment's owner, from configuration, exactly as a consent decision's
//!   `actor` is. A request may state it — the spec's body shape does — and a
//!   request that states somebody *else* is refused rather than corrected,
//!   because silently rewriting the audit trail's one identity field is
//!   worse than saying no.
//! - **at that moment**: the consent check is a read of this Gateway's own
//!   consent state at approval time, not the label the suggestion was born
//!   with. Consent can be revoked between the suggestion and the approval,
//!   and that is the whole point of the clause.
//!
//! # Why the Gateway and not the runtime
//!
//! Spec #19 put this endpoint on the Hermes runtime, with the runtime asking
//! the Gateway over HTTP for the current consent. ADR 0022 records why it
//! moved: the Gateway is the single writer of consent state, so putting the
//! "is it still granted?" question in another process turns a local read
//! into a network call that can fail — and an authority that refuses and an
//! authority that cannot be reached would arrive at the caller as one
//! signal. That conflation is what #116 and #141 were.
//!
//! # No outbox: an approval publishes inside the request or not at all
//!
//! [`crate::outbox`] commits a consent decision and publishes it later,
//! because a decision the user took must never be lost and a bus outage must
//! not refuse it. An approval is the opposite case. An approval held for
//! later publication is a message the Gateway has promised to send at an
//! unknown future moment — possibly after the consent it checked has been
//! revoked — and there is no honest way to hold one. So there is no queue:
//! the request publishes and answers `201`, or it publishes nothing and
//! answers a refusal that names its cause. A bus that does not answer is a
//! `502` a human can retry, not a promise.
//!
//! The row this module writes is therefore bookkeeping, not an outbox:
//! written unpublished, published, then marked with the position the bus
//! stored it at. That order leaves a crash visible instead of silent — a row
//! that was never marked says "this was approved and the Gateway does not
//! know where it landed", and the next attempt republishes it, which the
//! bus deduplicates on the contract's id. The row holds the suggestion's id,
//! who approved it, whether they edited it and where on the bus it landed —
//! **never the text that was approved**. `GET /api/approvals/{id}` reads it
//! back, which is how "did my reply actually go out?" has an answer that is
//! not a spinner.
//!
//! # The disclosure is appended here, and composed nowhere
//!
//! Ticket #121 (ADR 0019, ADR 0031): a reply a persona drafted reaches the
//! contact with one sentence after it, on a line of its own — *"Rédigé avec
//! mon assistant IA."* The suggestion carries that sentence as
//! `data.disclosure`, selected by whoever knew the language of the reply, and
//! this module **appends** it at approval when the switch is on:
//! `final.body` is the body, a newline and the sentence, and the approved
//! event carries the sentence as `data.disclosure` as well. Three things
//! follow, and each is a line of [`Approval::envelope`] or of
//! [`Approvals::approve`]: `edited` compares the body alone, so a user who
//! sent the suggestion untouched is not recorded as having edited it; the
//! body's cap is the contract's less the line reserved for the sentence
//! ([`MAX_BODY`]), so the sum never exceeds the schema; and when the switch
//! is off — the journal's last row says so
//! ([`crate::store::Store::disclosure_state`]) — neither the line nor the
//! member is there, because a bus that recorded a disclosure nobody received
//! would be lying about what the contact got.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::consent::{bus_subject, Network, State, STREAM_NAME};
use crate::metrics::Metrics;
use crate::store::{RecordedApproval, Store};

/// The contract type this module produces, and the schema it validates
/// against.
pub const REPLY_APPROVED_TYPE: &str = "fr.linagora.twalk.persona.reply.approved.v1";
pub const REPLY_APPROVED_DATASCHEMA: &str =
    "https://schemas.twalk.dev/cloudevents/v1/persona.reply.approved.schema.json";

/// The contract type a suggestion arrives as, and the one the approval is
/// about.
pub const SUGGEST_PRODUCED_TYPE: &str = "fr.linagora.twalk.persona.suggest.produced.v1";

/// How many stream sequences back from the head the suggestion lookup walks,
/// and how far back from the suggestion the trigger lookup walks.
///
/// There is no index from a CloudEvents id to a stream position — the bus
/// deduplicates by `Nats-Msg-Id` but does not let anything look a message up
/// by it — so finding a suggestion means reading the stream. The read is
/// bounded, and the bound is visible in the answer: a suggestion the window
/// did not reach is refused as `suggestion_out_of_reach` and never as
/// `suggestion_not_found`, because "it is not there" and "I did not look
/// that far" are different facts and a user acts differently on each.
///
/// The trigger's window is anchored on the suggestion rather than on the
/// stream's head, which is what makes an old suggestion workable: a persona
/// activated today reads the stream from the beginning (ADR 0013), so its
/// first suggestions answer messages from weeks ago, and a window counted
/// from the head would miss every one of their triggers.
pub const DEFAULT_LOOKUP_WINDOW: u64 = 20_000;

/// How long a lookup's ephemeral consumer survives being forgotten. The read
/// deletes its own consumer; this is what reaps one whose request died.
const LOOKUP_CONSUMER_IDLE: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// The request
// ---------------------------------------------------------------------------

/// The content of a reply: what the contract calls `final` on the approved
/// event and `suggestion` on the suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content {
    pub body: String,
    pub format: Format,
}

/// The contract's content types, exactly. Not a free string: a format the
/// contract does not name would produce an event the schema refuses, and it
/// is better to refuse the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Plain,
    Markdown,
    Html,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Plain => "text/plain",
            Format::Markdown => "text/markdown",
            Format::Html => "text/html",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text/plain" => Some(Format::Plain),
            "text/markdown" => Some(Format::Markdown),
            "text/html" => Some(Format::Html),
            _ => None,
        }
    }
}

/// The contract's cap on `final.body` (`maxLength: 65536` on the approved
/// event and on the suggestion).
pub const CONTRACT_MAX_BODY: usize = 65_536;

/// The cap on the reply's **body**: the contract's, less the newline and the
/// disclosure the Gateway appends after it (ticket #121, ADR 0031). Enforced
/// here so an oversized body is a `400` naming the limit rather than an
/// event the schema rejects after the Gateway has already tried to publish
/// it — and reserved whether the switch is on or off, because a body that
/// only fits while the disclosure is off is a body that stops fitting the
/// day it is turned back on. The SDK's own cap is the same number.
pub const MAX_BODY: usize = CONTRACT_MAX_BODY - 1 - crate::disclosure::MAX_CHARS;

/// One approval, as a request states it.
///
/// One suggestion id. Not a list — see the module's second bullet.
/// Who wrote the text going out (#327).
///
/// Not measured on the body, declared by the gesture. A body that differs
/// from the draft says *something changed*; it cannot say whether the
/// owner corrected a word or threw the draft away and wrote their own
/// reply, and those are the two cases ADR 0019 treats differently. So the
/// client says which gesture it was, and a client that says nothing is
/// taken to have sent the persona's text — the reading that keeps the
/// sentence rather than the one that drops it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrittenBy {
    /// The persona drafted it; the owner approved it, corrections included.
    #[default]
    Persona,
    /// The owner wrote it themselves, in place of the draft.
    Owner,
}

impl WrittenBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Persona => "persona",
            Self::Owner => "owner",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "persona" => Some(Self::Persona),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub suggestion_event_id: String,
    /// Stated by the caller, optional, and checked against the owner rather
    /// than trusted. Spec #19's body carries it; this Gateway knows who its
    /// owner is and does not need to be told, so the field exists to be
    /// agreed with.
    pub approved_by: Option<String>,
    /// The edited content, when the user changed the suggestion before
    /// approving. Absent means "send the suggestion as it stands".
    pub edited: Option<Content>,
    /// Who wrote what is being sent (#327): `final.written_by`, or
    /// [`WrittenBy::Persona`] when the caller says nothing — including
    /// when there is no `final` at all, which is the persona's text by
    /// definition.
    pub written_by: WrittenBy,
}

impl Request {
    /// Parses the body. Every refusal here is a `400` with its own code —
    /// what is wrong with a request is something the caller can fix, and
    /// "malformed" on its own does not tell them which part.
    pub fn parse(body: &Value) -> Result<Self, Invalid> {
        let object = body.as_object().ok_or_else(|| {
            // An array is the specific mistake worth naming: it is what a
            // client writes when it means to approve several at once.
            if body.is_array() {
                Invalid::Batch
            } else {
                Invalid::NotAnObject
            }
        })?;
        for member in object.keys() {
            if !matches!(
                member.as_str(),
                "suggestion_event_id" | "approved_by" | "final"
            ) {
                return Err(Invalid::UnknownMember(member.clone()));
            }
        }
        let suggestion_event_id = object
            .get("suggestion_event_id")
            .ok_or(Invalid::MissingSuggestion)?;
        if suggestion_event_id.is_array() {
            return Err(Invalid::Batch);
        }
        let suggestion_event_id = suggestion_event_id
            .as_str()
            .ok_or(Invalid::MissingSuggestion)?;
        if !is_event_id(suggestion_event_id) {
            return Err(Invalid::SuggestionNotAnEventId(
                suggestion_event_id.to_owned(),
            ));
        }
        let approved_by = match object.get("approved_by") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => return Err(Invalid::ApprovedByNotAString),
        };
        let (edited, written_by) = match object.get("final") {
            None | Some(Value::Null) => (None, WrittenBy::default()),
            Some(value) => {
                let (content, written_by) = parse_content(value)?;
                (Some(content), written_by)
            }
        };
        Ok(Self {
            suggestion_event_id: suggestion_event_id.to_owned(),
            approved_by,
            edited,
            written_by,
        })
    }
}

fn parse_content(value: &Value) -> Result<(Content, WrittenBy), Invalid> {
    let object = value.as_object().ok_or(Invalid::FinalNotAnObject)?;
    for member in object.keys() {
        if !matches!(member.as_str(), "body" | "format" | "written_by") {
            return Err(Invalid::UnknownMember(format!("final.{member}")));
        }
    }
    // Inside `final` on purpose (#327): a claim about who wrote a text
    // cannot exist where there is no text.
    let written_by = match object.get("written_by") {
        None | Some(Value::Null) => WrittenBy::default(),
        Some(value) => {
            let raw = value
                .as_str()
                .ok_or_else(|| Invalid::UnknownWrittenBy(value.to_string()))?;
            WrittenBy::parse(raw).ok_or_else(|| Invalid::UnknownWrittenBy(raw.to_owned()))?
        }
    };
    let body = object
        .get("body")
        .and_then(Value::as_str)
        .ok_or(Invalid::MissingFinalBody)?;
    if body.is_empty() {
        return Err(Invalid::EmptyFinalBody);
    }
    if body.chars().count() > MAX_BODY {
        return Err(Invalid::FinalBodyTooLong(body.chars().count()));
    }
    let format = match object.get("format") {
        None => Format::Plain,
        Some(value) => {
            let raw = value
                .as_str()
                .ok_or(Invalid::UnknownFormat(value.to_string()))?;
            Format::parse(raw).ok_or_else(|| Invalid::UnknownFormat(raw.to_owned()))?
        }
    };
    Ok((
        Content {
            body: body.to_owned(),
            format,
        },
        written_by,
    ))
}

/// A CloudEvents id as every `persona.*` and `inbound.*` type spells one:
/// 64 lowercase hex characters, the contract's `^[a-f0-9]{64}$`.
pub fn is_event_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What is wrong with the request itself. All of these are `400`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    NotAnObject,
    /// The caller sent a list. Its own variant, and its own message, because
    /// "never a batch" is a rule of the domain and not a parser accident.
    Batch,
    UnknownMember(String),
    MissingSuggestion,
    SuggestionNotAnEventId(String),
    ApprovedByNotAString,
    FinalNotAnObject,
    MissingFinalBody,
    EmptyFinalBody,
    FinalBodyTooLong(usize),
    UnknownFormat(String),
    UnknownWrittenBy(String),
}

impl Invalid {
    pub fn code(&self) -> &'static str {
        match self {
            Invalid::Batch => "approval_is_not_a_batch",
            Invalid::UnknownFormat(_) | Invalid::UnknownWrittenBy(_) => "unknown_value",
            _ => "malformed_request",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Invalid::NotAnObject => "the request body is not a JSON object".to_owned(),
            Invalid::Batch => "an approval names exactly one suggestion: it is a deliberate act \
                 and never a batch, so there is no endpoint that approves a list. Send one \
                 request per suggestion."
                .to_owned(),
            Invalid::UnknownMember(member) => format!(
                "the request body has the unknown member {member:?}: an approval carries \
                 suggestion_event_id, an optional approved_by and an optional final"
            ),
            Invalid::MissingSuggestion => {
                "suggestion_event_id is required: the CloudEvents id of the \
                 persona.suggest.produced event being approved"
                    .to_owned()
            }
            Invalid::SuggestionNotAnEventId(value) => format!(
                "suggestion_event_id {value:?} is not a contract event id (64 lowercase hex \
                 characters)"
            ),
            Invalid::ApprovedByNotAString => {
                "approved_by is a Matrix user ID when it is present at all".to_owned()
            }
            Invalid::UnknownWrittenBy(value) => format!(
                "final.written_by is {value:?}: it says who wrote the text being sent, and it \
                 is \"persona\" — the draft, corrections included — or \"owner\", the reply you \
                 wrote in its place. Absent means the persona's (#327)"
            ),
            Invalid::FinalNotAnObject => {
                "final is an object with a body and an optional format".to_owned()
            }
            Invalid::MissingFinalBody => "final.body is required when final is present".to_owned(),
            Invalid::EmptyFinalBody => {
                "final.body is empty: approving an empty reply sends an empty message, which is \
                 never what was meant — refuse the suggestion instead"
                    .to_owned()
            }
            Invalid::FinalBodyTooLong(length) => format!(
                "final.body is {length} characters, and the limit is {MAX_BODY}: the contract \
                 allows {CONTRACT_MAX_BODY}, less the line the disclosure is appended on — a \
                 newline and a sentence of up to {} characters (ADR 0031)",
                crate::disclosure::MAX_CHARS
            ),
            Invalid::UnknownFormat(value) => format!(
                "final.format has the unknown value {value}: the contract's formats are \
                 text/plain, text/markdown and text/html"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// What the bus is asked for
// ---------------------------------------------------------------------------

/// A suggestion, as this Gateway reads one off the bus.
///
/// Spelled as a type rather than as pointers into a `Value`, for the same
/// reason [`crate::contacts::InboundHeader`] is a type: what the struct does
/// not declare, the process does not hold. What it does declare includes the
/// suggestion's body, because the approval has to send it when the user did
/// not edit it — and it is held for the length of one request and written
/// nowhere. The store has no column for it.
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub event_id: String,
    /// `hermes://<domain>/personas/<persona id>` — the persona whose
    /// suggestion this is. Copied onto the approved event unchanged: the
    /// contract's `source` there identifies *the persona whose suggestion
    /// was approved*, not whichever process published the approval.
    pub source: String,
    pub persona_id: String,
    /// The inbound event the suggestion answers (the suggestion's
    /// `subject`).
    pub trigger_event_id: String,
    pub network: Network,
    /// The connection the trigger arrived on (ADR 0033, #269), copied onto the
    /// approved reply unchanged. A suggestion older than #269 carries none and
    /// is read as its network's single connection — the id every existing
    /// decision was migrated onto.
    pub connection: String,
    /// The consent label the trigger carried when the Sensor observed it.
    /// The audit fact, checked as the spec asks — and not a substitute for
    /// the current state, which is checked separately.
    pub consent_label: State,
    pub suggestion: Content,
    /// The sentence the reply discloses itself with, in the language the
    /// reply was written in — selected by the persona (or, for an answer from
    /// Hermes, by [`crate::hermes_answer`]) and carried as `data.disclosure`
    /// (ticket #121, ADR 0031). `None` on a suggestion published before the
    /// member existed or by a persona that set none; nothing is appended
    /// then, because this Gateway composes no sentence of its own. When it
    /// is set it is one of the contract's five, verbatim, or the suggestion
    /// was refused at the read ([`contract_sentence`]).
    pub disclosure: Option<String>,
    pub expires_at: Option<String>,
    pub traceparent: Option<String>,
    /// Where on the stream it was found: the anchor the trigger lookup walks
    /// back from.
    pub stream_sequence: u64,
}

/// The half of the trigger event an approval needs: who wrote, and where to
/// answer.
///
/// Four values, and the struct has no `data` member — so the trigger's body,
/// its attachments and the contact's `network_identifier` never become values
/// in this process, exactly as they do not on the pending-contact path. The
/// one field read under `data` is a mail's `message_id` (#278): an
/// identifier the sending server minted, not the sender's words, and the
/// only thing a reply to a mail can be threaded under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    /// The sender: a Matrix user ID, or a `mailto:` on the mail connection
    /// (#276) — the contact whose consent is checked.
    pub contact: String,
    /// The perimeter the message arrived on — what the consent is read
    /// against (#270).
    pub connection: String,
    pub network: Network,
    /// Where the reply goes: the portal room, or the mail connection.
    pub target: Target,
}

/// Where an approved reply is sent, by whom (ADR 0033: one shape per kind).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A portal room, from the event's `matrix://<homeserver>/<room id>`
    /// source: the Sensor posts it as the owner's device.
    Room { room_id: String },
    /// A mail connection, from a `jmap://` source (#278): the collector
    /// holding the connection sends it from the owner's own mailbox, in
    /// reply to the trigger mail's Message-ID.
    Mail {
        connection: String,
        in_reply_to: String,
        /// The trigger's `subject` — the sender whose consent this approval
        /// checked — carried as the one address the reply goes to. The
        /// collector finds the original again by Message-ID, and a
        /// Message-ID is the sender's to choose: without this, a stranger
        /// mailing the owner under another mail's Message-ID is who the
        /// owner's approved words would be sent to.
        recipient: String,
    },
}

impl Target {
    /// The contract's `data.target`.
    pub fn json(&self) -> Value {
        match self {
            Target::Room { room_id } => json!({ "room_id": room_id }),
            Target::Mail {
                connection,
                in_reply_to,
                recipient,
            } => json!({
                "connection": connection,
                "in_reply_to": in_reply_to,
                "recipient": recipient,
            }),
        }
    }

    /// One word for the log line.
    pub fn describe(&self) -> String {
        match self {
            Target::Room { room_id } => format!("room {room_id}"),
            Target::Mail { connection, .. } => format!("mail connection {connection}"),
        }
    }
}

/// The connection an event on the bus belongs to: the one it carries — which
/// must be the registry's — or, for an event published before #269 or by a
/// producer that has not learned the extension yet, the registry's single
/// connection of its kind. [`crate::connections::Registry::resolve`], with
/// the refusal every unreadable event gets: the Gateway does not guess which
/// perimeter the event was about, and says why it could not tell.
pub fn connection_of(
    registry: &crate::connections::Registry,
    carried: Option<&str>,
    network: Network,
) -> Result<String, Refusal> {
    registry
        .resolve(carried, network.as_str())
        .map(|connection| connection.id.clone())
        .map_err(|why| {
            Refusal::SuggestionUnreadable(format!("its connection cannot be told: {why}"))
        })
}

/// Whether a connection can take a reply (#275): it last said it was
/// `connected`; or it is a bridge's, which says nothing on the status
/// subject and is not refused here — its reach is #216's `.posted` report. A
/// collector's connection (kind `email` or `calendar`) that never spoke
/// cannot: no collector is holding it, and the reply would sit on the bus
/// for nobody.
pub fn connection_can_send(
    store: &Store,
    connections: &crate::connections::Registry,
    connection: &str,
) -> Result<(), Refusal> {
    match store.connection_status(connection) {
        Ok(Some(current)) if !current.is_connected() => Err(Refusal::ConnectionNotConnected {
            connection: connection.to_owned(),
            state: current.state,
            hint: current.hint,
        }),
        Ok(Some(_)) => Ok(()),
        Ok(None) => {
            let a_collectors = connections
                .get(connection)
                .is_some_and(|known| matches!(known.kind.as_str(), "email" | "calendar"));
            if a_collectors {
                Err(Refusal::ConnectionNotConnected {
                    connection: connection.to_owned(),
                    state: "unknown".to_owned(),
                    hint: Some(
                        "No collector has reported this connection yet: start the collector \
                         that holds it, or check its log"
                            .to_owned(),
                    ),
                })
            } else {
                Ok(())
            }
        }
        Err(error) => Err(Refusal::StoreUnavailable(format!("{error:#}"))),
    }
}

/// The room id out of an inbound event's `source`
/// (`matrix://<homeserver>/!room:server`), or `None` when the source is not
/// one.
pub fn room_from_source(source: &str) -> Option<String> {
    let rest = source.strip_prefix("matrix://")?;
    let (_homeserver, room) = rest.split_once('/')?;
    if !room.starts_with('!') || !room.contains(':') || room.contains('/') {
        return None;
    }
    Some(room.to_owned())
}

/// What the Sensor said one approved reply reached, once it posted it
/// (issue #216): its report is the approval event republished unchanged on
/// `twalk.persona.reply.approved.v1.posted`, and what it says is in two
/// headers. `contact` means a bridge relays it — posted by the owner's own
/// account into a portal, or native Matrix with no bridge in the way;
/// `nobody` means it was posted as `@sensor:` into a portal, which the
/// bridge ignores (#123).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posted {
    pub reach: String,
    pub posted_as: String,
    pub stream_sequence: u64,
}

/// The two headers of a posted report, as `sensor/src/outbound.rs` names them.
pub const POSTED_REACH_HEADER: &str = "reach";
pub const POSTED_AS_HEADER: &str = "posted-as";

/// A reply the component that had to send it gave up on (#311): the
/// approval republished unchanged on `twalk.persona.reply.approved.v1.dead`
/// by the Sensor or the collector, once a post can never succeed or has
/// burned its attempts.
///
/// A thing of its own beside `publication` and [`Posted`], not a variant of
/// either. `CONTEXT.md` fixes the three facts about a reply that went out —
/// published on your bus, posted into a room, delivered to your contact —
/// and this is the fourth, about a reply that did **not**: its send was
/// abandoned. Folding it into the first is what made such a reply read as
/// `published` for ever.
///
/// Note that it is not "undelivered": by the glossary, a reply whose reach
/// is `nobody` is undelivered too, and that one was posted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GivenUp {
    /// Why, in the sender's own words — the [`REASON_HEADER`] of the dead
    /// letter, capped by the sender. Neither sender quotes the reply's body
    /// in it.
    ///
    /// `None` when the sender set none: a Sensor older than #311 published
    /// dead letters without the header. The Gateway answers the absence
    /// rather than a sentence of its own, because the screen that renders
    /// it speaks five languages and the one thing the Gateway must not do
    /// is put English prose where a translation belongs.
    pub reason: Option<String>,
    pub stream_sequence: u64,
}

/// The header both senders put the reason in (`sensor/src/outbound.rs`,
/// `collector/src/outbound.rs`).
pub const REASON_HEADER: &str = "reason";

/// The reason a dead letter carries, if it carries one. An empty header is
/// no reason. One function, because the listing and the single record must
/// not answer two different things about the same dead letter.
pub fn reason_of(headers: Option<&async_nats::HeaderMap>) -> Option<String> {
    headers
        .and_then(|headers| headers.get(REASON_HEADER))
        .map(|reason| reason.as_str().trim().to_owned())
        .filter(|reason| !reason.is_empty())
}

/// The report's envelope: the approval's own id, and nothing else read.
#[derive(Debug, Deserialize)]
struct PostedEnvelope {
    id: String,
}

/// What the Gateway reads of a suggestion event.
#[derive(Debug, Deserialize)]
struct SuggestionDocument {
    id: String,
    source: String,
    subject: String,
    network: String,
    #[serde(default)]
    connection: Option<String>,
    consent: String,
    #[serde(default)]
    traceparent: Option<String>,
    data: SuggestionData,
}

#[derive(Debug, Deserialize)]
struct SuggestionData {
    persona_id: String,
    suggestion: SuggestionContent,
    #[serde(default)]
    disclosure: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SuggestionContent {
    body: String,
    format: String,
}

/// What the Gateway reads of the trigger event: its CloudEvents attributes,
/// no `data`.
#[derive(Debug, Deserialize)]
struct TriggerDocument {
    id: String,
    source: String,
    subject: String,
    network: String,
    /// The perimeter (#269); absent on an event older than it.
    #[serde(default)]
    connection: Option<String>,
    /// The one member of `data` this path reads: a mail's own Message-ID
    /// (#278). Every other member is left in the bytes.
    #[serde(default)]
    data: TriggerData,
}

#[derive(Debug, Default, serde::Deserialize)]
struct TriggerData {
    #[serde(default)]
    message_id: Option<String>,
}

/// The message a suggestion answers, as **one route** may read it (#336,
/// resolving the third option of #160): the Companion's own approval card,
/// served to the owner through their identity provider, asking for the one
/// message they are about to reply to.
///
/// A third view of an inbound event, and deliberately the widest — the
/// reason the other two are narrow is that they had no use for the words,
/// and this one is the use. It is read here, in the module that already
/// reads triggers and already owns the consent check, rather than in a
/// second place implementing the same rules: that second place is how
/// #110 happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    /// The contact's display name as the event carried it, when it carried
    /// one. Their identifier is not here: the screen names a person, it
    /// does not address one.
    pub contact: Option<String>,
    /// When the network says they wrote it.
    pub received_at: Option<String>,
    pub body: String,
    pub format: String,
    /// How many attachments came with it, never what they are called: the
    /// owner is told there are files, and the files stay where they are.
    pub attachments: usize,
}

#[derive(Debug, serde::Deserialize)]
struct AnsweredDocument {
    id: String,
    subject: String,
    network: String,
    #[serde(default)]
    connection: Option<String>,
    #[serde(default)]
    data: AnsweredData,
}

#[derive(Debug, Default, serde::Deserialize)]
struct AnsweredData {
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    network_timestamp: Option<String>,
    #[serde(default)]
    contact: Option<AnsweredContact>,
    #[serde(default)]
    attachments: Vec<serde_json::Value>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct AnsweredContact {
    #[serde(default)]
    display_name: Option<String>,
}

/// The trigger event as a suggestion built **outside** a persona needs it
/// (ticket #206): the attributes a `persona.*` envelope copies from the
/// message it answers.
///
/// A second view of one event rather than a wider [`Trigger`], for the reason
/// that struct exists: an approval needs the room to post into and nothing
/// else, and widening it would hand the sender's identity to a path that has
/// no use for it. This one needs the consent label and the trace — which the
/// SDK copies from the trigger when a persona publishes a suggestion itself
/// (`sdk/python/twalk_sdk/envelope.py`) — and, still, no `data`: the message's
/// own words never become a value on this path either. Hermes was sent them;
/// the Gateway is not told them back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerEnvelope {
    /// The connection the message arrived on (ADR 0033, #269); a trigger older
    /// than #269 is read as its network's single connection.
    pub connection: String,
    pub event_id: String,
    pub event_type: String,
    /// The sender's Matrix user ID: the contact whose consent is checked
    /// again at the moment the answer comes home.
    pub contact: String,
    /// How the network names that contact, as the trigger carried it
    /// (`data.contact.display_name`) — what an approval surface says who is
    /// being answered with (#334, #360). `None` when the trigger gave none.
    ///
    /// The name and not the identity, because on a bridged network the
    /// identity is a phone number inside a Matrix ID
    /// (`@whatsapp_33612345678:…`), which is not something a person reading
    /// an approval recognises. On a mail connection the two are close; on
    /// WhatsApp they are not, and the screen is read by a human either way.
    pub display_name: Option<String>,
    pub network: Network,
    /// The consent label the Sensor stamped when it observed the message. An
    /// audit fact, copied onto the suggestion — never a substitute for the
    /// current state, which is read separately.
    pub consent_label: State,
    pub traceparent: Option<String>,
}

/// What the Gateway reads of the trigger event on the answer path. No
/// `source`, which names a room this path never posts into, and **one**
/// member of `data`: `contact.display_name`.
///
/// That one and nothing else (#360). `data` is where somebody's words are —
/// the body, an excerpt, an attachment's caption — and reopening an inbound
/// event to draw an approval screen is what #110 closed. A display name is
/// none of those: it is a structured member the Sensor resolved from Matrix
/// membership state, or the collector from a `From` header, and it is how
/// the network names a person rather than anything that person wrote. The
/// contract governs it as such, and `network_identifier` beside it is
/// "populated only when consent allows" — which is why this reads the name
/// and never its sibling.
#[derive(Debug, Deserialize)]
struct TriggerEnvelopeDocument {
    #[serde(default)]
    connection: Option<String>,
    id: String,
    #[serde(rename = "type")]
    event_type: String,
    subject: String,
    network: String,
    consent: String,
    #[serde(default)]
    traceparent: Option<String>,
    #[serde(default)]
    data: Option<TriggerDataDocument>,
}

/// The one part of a trigger's `data` this path reads.
#[derive(Debug, Deserialize)]
struct TriggerDataDocument {
    #[serde(default)]
    contact: Option<TriggerContactDocument>,
}

#[derive(Debug, Deserialize)]
struct TriggerContactDocument {
    #[serde(default)]
    display_name: Option<String>,
}

// ---------------------------------------------------------------------------
// The act
// ---------------------------------------------------------------------------

/// The approval as it will be published.
#[derive(Debug, Clone)]
pub struct Approval {
    pub suggestion: Suggestion,
    pub trigger: Trigger,
    pub approved_by: String,
    /// The body as the human approved it — the persona's or their own edit
    /// of it — and **without** the disclosure, which is not in the field
    /// they edit.
    pub content: Content,
    /// Whether `content` differs from what the persona wrote. The body
    /// alone: the disclosure is appended after this comparison, so it can
    /// neither make an untouched reply look edited nor hide an edit.
    pub edited: bool,
    /// Who wrote what is going out (#327): the persona, corrections
    /// included, or the owner in place of the draft. What the disclosure
    /// follows, and a fact of the trail — "how often do I correct my
    /// assistant?" and "how often do I write it myself?" are two questions,
    /// and `edited` alone answered neither.
    pub written_by: WrittenBy,
    /// The sentence appended after the body, when the switch is on and the
    /// suggestion carries one (ticket #121). `None` means neither the line
    /// nor the `data.disclosure` member is on the event.
    pub disclosure: Option<String>,
}

impl Approval {
    /// The contract's deterministic id:
    /// `sha256(suggestion_event_id + ':' + approved_by)`.
    ///
    /// No clock in it, on purpose — "a given suggestion is approved at most
    /// once by a given user", says the schema — which is what makes a
    /// republish after a crash land on the message the bus already holds
    /// instead of sending a second reply.
    pub fn event_id(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.suggestion.event_id.as_bytes());
        hasher.update(b":");
        hasher.update(self.approved_by.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// The CloudEvent, as the contract's schema describes it.
    ///
    /// `consent` is the suggestion's label, which by the time this is built
    /// is `granted` on both counts — the label the trigger carried and the
    /// state this Gateway holds now. The two are checked separately and the
    /// event carries the one the contract asks for: "consent state of the
    /// original sender, copied from the trigger event".
    ///
    /// On a room target, `reply_to_event_id` is deliberately absent. The
    /// contract's inbound event does not carry its own Matrix event ID — only
    /// the id of a message it was itself a reply to — so there is nothing
    /// there to thread under, and the contract makes the field optional. The
    /// reply lands in the room; it is not a native Matrix reply. Inventing an
    /// id would be worse than the gap, and the gap is named in ADR 0022. A
    /// mail target (#278) has what a room does not: the mail's own
    /// Message-ID, so a reply to a mail *is* threaded.
    ///
    /// `final.body` is what the contact receives: the approved body, then —
    /// when [`Self::disclosure`] is set — a newline and the sentence, which
    /// the event carries again as `data.disclosure` so a consumer can tell
    /// the reply from its disclosure without parsing the body (ADR 0031).
    pub fn envelope(&self, produced_at: &str) -> Value {
        let body = match &self.disclosure {
            Some(sentence) => crate::disclosure::append(&self.content.body, sentence),
            None => self.content.body.clone(),
        };
        let mut event = json!({
            "specversion": "1.0",
            "id": self.event_id(),
            "source": self.suggestion.source,
            "type": REPLY_APPROVED_TYPE,
            "time": produced_at,
            "subject": self.suggestion.event_id,
            "datacontenttype": "application/json",
            "dataschema": REPLY_APPROVED_DATASCHEMA,
            "network": self.suggestion.network.as_str(),
            "connection": self.suggestion.connection,
            "consent": self.suggestion.consent_label.as_str(),
            "data": {
                "persona_id": self.suggestion.persona_id,
                "suggestion_event_id": self.suggestion.event_id,
                "approved_by": self.approved_by,
                "final": {
                    "body": body,
                    "format": self.content.format.as_str(),
                },
                "edited": self.edited,
                "written_by": self.written_by.as_str(),
                "target": self.trigger.target.json(),
            }
        });
        if let Some(sentence) = &self.disclosure {
            event["data"]["disclosure"] = Value::String(sentence.clone());
        }
        // Continued from the suggestion when it carries one, so a message's
        // trace links sensor → persona → approval → outbound.
        if let Some(traceparent) = &self.suggestion.traceparent {
            event["traceparent"] = Value::String(traceparent.clone());
        }
        event
    }
}

/// Why an approval was refused — or, in [`Refusal::Invalid`], why the
/// request was.
///
/// Each variant is a different situation for whoever reads the message, and
/// each therefore has its own code and its own status. That is not
/// decoration: this project has spent two days on incidents caused by two
/// failures sharing one signal (#116's `bridge_unreachable`, #141's `401`
/// meaning both an expired session and a rejected third-party token), and a
/// status code is part of the answer — #141 was a `401` telling a client to
/// refresh a session that was fine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The request itself.
    Invalid(Invalid),
    /// The request named an approver who is not this deployment's owner.
    NotTheOwner { stated: String, owner: String },
    /// The whole retained stream was read and no suggestion has this id.
    SuggestionNotFound,
    /// The lookup window was exhausted before the stream's first retained
    /// message. The suggestion may exist, further back than the Gateway
    /// looks — and a suggestion that old has expired anyway.
    SuggestionOutOfReach { window: u64 },
    /// The suggestion exists, and the trigger it answers does not — the
    /// stream was read to its first retained message.
    TriggerNotFound { trigger_event_id: String },
    /// As above, bounded by the window rather than by the stream.
    TriggerOutOfReach {
        trigger_event_id: String,
        window: u64,
    },
    /// The trigger event's `source` is neither a portal room nor a mailbox,
    /// so there is nowhere to send the reply.
    TriggerHasNoRoom { source: String },
    /// The suggestion is on the bus and this build cannot read it — an
    /// unknown network, an unknown consent state, a content type the
    /// contract does not name. Its own code, because "I found it and do not
    /// understand it" is not "it is not there".
    SuggestionUnreadable(String),
    /// The suggestion's `expires_at` has passed. The suggestion stays
    /// visible; it is no longer approvable (ticket #22's policy).
    Expired { expires_at: String, now: String },
    /// The consent label the trigger carried was not `granted`: this
    /// suggestion should never have existed. The audit fact, checked as spec
    /// #19 asks, and separate from the check below.
    NeverConsented { label: State },
    /// The sender's consent is `revoked` **now**. The clause the definition
    /// exists for.
    ConsentRevoked { contact: String, network: Network },
    /// The sender's consent is `pending` now — never decided, or decided
    /// "not yet". A different sentence for the user from a revocation, so a
    /// different code.
    ConsentPending { contact: String, network: Network },
    /// The connection the reply would leave by last said it cannot send
    /// (#275): the collector holding it published `reconnect_required`,
    /// `pending_operator` or `unreachable`, and an approval published now
    /// would sit on the bus for a sender that will not take it. The state
    /// and the operator's own hint travel in the detail.
    ConnectionNotConnected {
        connection: String,
        state: String,
        hint: Option<String>,
    },
    /// This suggestion has already been approved by this person. The answer
    /// carries the first approval, so a client that lost the first response
    /// learns where the reply went rather than being told to try again.
    AlreadyApproved(RecordedApproval),
    /// The bus could not be reached, or refused the publication. Nothing was
    /// published and nothing was recorded.
    BusUnreachable(String),
    /// The Gateway's own store could not be read or written, before anything
    /// was published. The same code the consent endpoints answer with, for
    /// the same condition.
    StoreUnavailable(String),
    /// The store refused to mark an approval that *was* published. Rare and
    /// worth its own code, because the reply did go out.
    Unrecorded(String),
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::Invalid(invalid) => invalid.code(),
            Refusal::NotTheOwner { .. } => "approved_by_is_not_the_owner",
            Refusal::SuggestionNotFound => "suggestion_not_found",
            Refusal::SuggestionOutOfReach { .. } => "suggestion_out_of_reach",
            Refusal::TriggerNotFound { .. } => "trigger_not_found",
            Refusal::TriggerOutOfReach { .. } => "trigger_out_of_reach",
            Refusal::TriggerHasNoRoom { .. } => "trigger_has_no_room",
            Refusal::SuggestionUnreadable(_) => "suggestion_unreadable",
            Refusal::Expired { .. } => "suggestion_expired",
            Refusal::NeverConsented { .. } => "suggestion_was_never_consented",
            Refusal::ConsentRevoked { .. } => "consent_revoked",
            Refusal::ConsentPending { .. } => "consent_pending",
            Refusal::ConnectionNotConnected { .. } => "connection_not_connected",
            Refusal::AlreadyApproved(_) => "already_approved",
            Refusal::BusUnreachable(_) => "bus_unreachable",
            Refusal::StoreUnavailable(_) => "store_unavailable",
            Refusal::Unrecorded(_) => "approval_published_but_not_recorded",
        }
    }

    /// The status code, chosen per situation rather than per family.
    ///
    /// - `400` — the request is wrong and the caller can fix it.
    /// - `403` — the caller is not who they say they are.
    /// - `404` — this suggestion does not exist.
    /// - `409` — the suggestion exists and the approval conflicts with the
    ///   world's state: expired, unconsented, already approved. A client
    ///   must not retry these; refreshing the screen is the way out.
    /// - `410` — it may have existed and is beyond the Gateway's reach. Not
    ///   a `404`, because "gone" and "never was" lead to different sentences.
    /// - `502` — the bus. Not a `503`: the Gateway is configured and
    ///   answering, and what failed is the thing behind it.
    /// - `500` — the reply went out and the Gateway could not write that
    ///   down. The one case where retrying is safe and reading back is not.
    pub fn status(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            Refusal::Invalid(_) => StatusCode::BAD_REQUEST,
            Refusal::NotTheOwner { .. } => StatusCode::FORBIDDEN,
            Refusal::SuggestionNotFound | Refusal::TriggerNotFound { .. } => StatusCode::NOT_FOUND,
            Refusal::SuggestionOutOfReach { .. } | Refusal::TriggerOutOfReach { .. } => {
                StatusCode::GONE
            }
            Refusal::TriggerHasNoRoom { .. } => StatusCode::CONFLICT,
            Refusal::SuggestionUnreadable(_) => StatusCode::CONFLICT,
            Refusal::Expired { .. } => StatusCode::CONFLICT,
            Refusal::NeverConsented { .. } => StatusCode::CONFLICT,
            Refusal::ConsentRevoked { .. }
            | Refusal::ConsentPending { .. }
            | Refusal::ConnectionNotConnected { .. } => StatusCode::CONFLICT,
            Refusal::AlreadyApproved(_) => StatusCode::CONFLICT,
            Refusal::BusUnreachable(_) => StatusCode::BAD_GATEWAY,
            Refusal::StoreUnavailable(_) | Refusal::Unrecorded(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    pub fn message(&self) -> String {
        match self {
            Refusal::Invalid(invalid) => invalid.message(),
            Refusal::NotTheOwner { stated, owner } => format!(
                "approved_by names {stated:?}, and this deployment's owner is {owner:?}: an \
                 approval carries the identity of whoever approved it, so the Gateway will not \
                 record one under another name. Omit approved_by and it is stamped for you"
            ),
            Refusal::SuggestionNotFound => {
                "no suggestion with this id is on the bus: the whole retained stream was read"
                    .to_owned()
            }
            Refusal::SuggestionOutOfReach { window } => format!(
                "this suggestion is further back than the Gateway reads: {window} stream \
                 positions were searched and the stream begins before them. It was not found, \
                 which is not the same as not existing — a suggestion this old has expired in \
                 any case. GATEWAY_APPROVAL_LOOKUP_WINDOW widens the search"
            ),
            Refusal::TriggerNotFound { trigger_event_id } => format!(
                "the suggestion answers the event {trigger_event_id}, which is not on the bus: \
                 there is no message to reply to and therefore no room to reply in"
            ),
            Refusal::TriggerOutOfReach {
                trigger_event_id,
                window,
            } => format!(
                "the suggestion answers the event {trigger_event_id}, which is further back than \
                 the Gateway reads: {window} stream positions before the suggestion were \
                 searched. GATEWAY_APPROVAL_LOOKUP_WINDOW widens the search"
            ),
            Refusal::TriggerHasNoRoom { source } => format!(
                "the trigger event names the source {source:?}, which is neither a portal room \
                 (matrix://<homeserver>/<room id>) nor a mailbox (jmap://<host>/<account>/<inbox>): \
                 there is nowhere to send the reply"
            ),
            Refusal::SuggestionUnreadable(detail) => format!(
                "the suggestion is on the bus and this Gateway cannot read it: {detail}. Nothing \
                 was sent"
            ),
            Refusal::Expired { expires_at, now } => format!(
                "this suggestion expired at {expires_at} and it is now {now}: a stale suggestion \
                 cannot be approved late. Ask the persona for a new one"
            ),
            Refusal::NeverConsented { label } => format!(
                "the message this suggestion answers was observed with consent {:?}, not \
                 granted: the suggestion should never have been produced and will not be sent",
                label.as_str()
            ),
            Refusal::ConsentRevoked { contact, network } => format!(
                "consent for {contact} on {} is revoked: an approval is refused if the sender's \
                 consent is no longer granted at the moment it is given, and it is not. The \
                 suggestion stays visible and nothing was sent",
                network.as_str()
            ),
            Refusal::ConsentPending { contact, network } => format!(
                "consent for {contact} on {} is pending: nothing may be sent to a contact the \
                 user has not granted. Decide about this contact first",
                network.as_str()
            ),
            Refusal::ConnectionNotConnected {
                connection,
                state,
                hint,
            } => format!(
                "the connection {connection} is {state}, not connected: the reply would sit on \
                 the bus for a sender that cannot take it, so nothing was published. {}",
                hint.as_deref().unwrap_or(
                    "The collector holding the connection says what to do next in its log"
                )
            ),
            Refusal::AlreadyApproved(approval) => format!(
                "this suggestion was already approved by {} at {}, and published on the bus as \
                 {}. A suggestion is approved at most once by a given user; nothing was sent \
                 again",
                approval.approved_by, approval.approved_at, approval.event_id
            ),
            Refusal::BusUnreachable(detail) => format!(
                "the approved reply could not be published: {detail}. Nothing was sent — this \
                 approval can be given again once the bus answers"
            ),
            Refusal::StoreUnavailable(detail) => {
                format!("the Gateway's own store could not be read: {detail}. Nothing was sent")
            }
            Refusal::Unrecorded(detail) => format!(
                "the approved reply was published on the bus and the Gateway could not record \
                 it: {detail}. The reply has gone out; it will not appear under GET \
                 /api/approvals until this is recorded"
            ),
        }
    }

    /// The label this refusal is counted under.
    pub fn outcome(&self) -> &'static str {
        self.code()
    }
}

// ---------------------------------------------------------------------------
// The half that talks to the bus and the store
// ---------------------------------------------------------------------------

/// The approval half of the Gateway: the consent store it checks against, the
/// bus it reads suggestions from and publishes approvals to, and the owner
/// every approval is attributed to.
pub struct Approvals {
    /// The same SQLite file the consent journal lives in — because the
    /// question an approval asks ("is this contact granted, now?") is a read
    /// of that journal's projection and not a call to anybody.
    store: Arc<Store>,
    metrics: Arc<Metrics>,
    /// The registry of connections (#269): what an event on the bus that
    /// names none is resolved against, by kind.
    connections: Arc<crate::connections::Registry>,
    /// This deployment's owner: who an approval is by (ADR 0011).
    owner: String,
    nats_url: String,
    lookup_window: u64,
    now: fn() -> std::time::SystemTime,
    bus: tokio::sync::OnceCell<async_nats::jetstream::Context>,
}

impl Approvals {
    pub fn new(
        store: Arc<Store>,
        metrics: Arc<Metrics>,
        connections: Arc<crate::connections::Registry>,
        owner: String,
        nats_url: String,
        lookup_window: u64,
        now: fn() -> std::time::SystemTime,
    ) -> Self {
        Self {
            store,
            metrics,
            connections,
            owner,
            nats_url,
            lookup_window,
            now,
            bus: tokio::sync::OnceCell::new(),
        }
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn lookup_window(&self) -> u64 {
        self.lookup_window
    }

    /// Finds one inbound message on the bus by its CloudEvents id, and reads
    /// the attributes a `persona.*` envelope copies from it.
    ///
    /// Public because a second door opens onto this fact since ticket #206:
    /// Hermes's answer arrives at [`crate::hermes_answer`] naming a trigger,
    /// and the suggestion it becomes must carry that trigger's network,
    /// consent label and trace — the same values the SDK copies when a persona
    /// publishes a suggestion of its own. Reusing this scan rather than
    /// writing a second one is the point: the bound, its visibility in the
    /// answer and the refusal vocabulary are then one implementation, so the
    /// two doors cannot teach a client two vocabularies.
    ///
    /// The window is counted from the stream's **head**, unlike the trigger
    /// lookup an approval makes, which is anchored on the suggestion it
    /// already found. There is no suggestion to anchor on here: the answer is
    /// what will create one.
    pub async fn trigger_envelope(
        &self,
        trigger_event_id: &str,
    ) -> Result<TriggerEnvelope, Refusal> {
        let jetstream = self.jetstream().await.map_err(|error| {
            warn!(%error, "the bus did not answer a trigger lookup");
            Refusal::BusUnreachable(format!("{error:#}"))
        })?;
        let wanted = trigger_event_id.to_owned();
        let found = self
            .scan(
                jetstream,
                &bus_subject(crate::contacts::INBOUND_MESSAGE_TYPE),
                None,
                |message| {
                    let document: TriggerEnvelopeDocument =
                        serde_json::from_slice(&message.payload).ok()?;
                    (document.id == wanted).then_some(document)
                },
            )
            .await
            .map_err(|error| {
                warn!(%error, "the trigger lookup could not read the bus");
                Refusal::BusUnreachable(format!("{error:#}"))
            })?;
        let document = match found {
            Found::Match(document, _) => document,
            Found::None { exhaustive } => {
                return Err(if exhaustive {
                    Refusal::TriggerNotFound {
                        trigger_event_id: trigger_event_id.to_owned(),
                    }
                } else {
                    Refusal::TriggerOutOfReach {
                        trigger_event_id: trigger_event_id.to_owned(),
                        window: self.lookup_window,
                    }
                })
            }
        };
        let network = Network::parse(&document.network).ok_or_else(|| {
            Refusal::SuggestionUnreadable(format!(
                "the trigger names the network {:?}, which this build does not know",
                document.network
            ))
        })?;
        let consent_label = State::parse(&document.consent).ok_or_else(|| {
            Refusal::SuggestionUnreadable(format!(
                "the trigger names the consent state {:?}, which this build does not know",
                document.consent
            ))
        })?;
        Ok(TriggerEnvelope {
            connection: connection_of(&self.connections, document.connection.as_deref(), network)?,
            event_id: document.id,
            event_type: document.event_type,
            contact: document.subject,
            display_name: document
                .data
                .and_then(|data| data.contact)
                .and_then(|contact| contact.display_name)
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty()),
            network,
            consent_label,
            traceparent: document.traceparent,
        })
    }

    /// Whether this contact's consent is `granted` **now**, read from this
    /// Gateway's own journal.
    ///
    /// Public for the same reason as [`Self::trigger_envelope`]: the question
    /// an approval asks before it sends is the question the answer path asks
    /// before it publishes a suggestion, and a contact revoked while Hermes was
    /// reasoning must not have a draft about them appear on the approval
    /// screen. One implementation, one refusal vocabulary.
    pub fn consent_now(
        &self,
        contact: &str,
        connection: &str,
        network: Network,
    ) -> Result<(), Refusal> {
        let effective = self
            .store
            .effective(contact, connection, network)
            .map_err(|error| {
                warn!(%error, %contact, "a consent state could not be read");
                Refusal::StoreUnavailable(format!("the consent state could not be read: {error:#}"))
            })?;
        match effective.state {
            State::Granted => Ok(()),
            State::Revoked => Err(Refusal::ConsentRevoked {
                contact: contact.to_owned(),
                network,
            }),
            State::Pending => Err(Refusal::ConsentPending {
                contact: contact.to_owned(),
                network,
            }),
        }
    }

    /// Publishes one contract event inside the caller's request, with the
    /// contract's id as `Nats-Msg-Id` and whatever extensions the caller
    /// duplicates as headers.
    ///
    /// The one publishing path on this origin that does not go through the
    /// outbox, shared by the two acts that must not be held for later: an
    /// approval (a send whose consent check would go stale in a queue) and a
    /// suggestion arriving from Hermes (whose reference and consent were
    /// checked against this instant). Deduplication is the contract's
    /// deterministic id, so a repeated publish is absorbed by the bus rather
    /// than shown to the user twice.
    pub async fn publish_envelope(
        &self,
        event_type: &str,
        event_id: &str,
        envelope: &Value,
        extensions: &[(&str, &str)],
    ) -> Result<u64> {
        let jetstream = self.jetstream().await?;
        jetstream
            .get_or_create_stream(async_nats::jetstream::stream::Config {
                name: STREAM_NAME.to_owned(),
                subjects: crate::consent::STREAM_SUBJECTS
                    .iter()
                    .map(|subject| subject.to_string())
                    .collect(),
                ..Default::default()
            })
            .await
            .context("the bus stream could not be reached")?;
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(async_nats::header::NATS_MESSAGE_ID, event_id);
        for (name, value) in extensions {
            headers.insert(*name, *value);
        }
        let payload = serde_json::to_vec(envelope)?;
        let ack = jetstream
            .publish_with_headers(bus_subject(event_type), headers, payload.into())
            .await
            .with_context(|| format!("the bus refused a {event_type}"))?
            .await
            .with_context(|| format!("the bus did not acknowledge a {event_type}"))?;
        Ok(ack.sequence)
    }

    /// One approval this Gateway recorded, or `None` when this suggestion was
    /// never approved. The answer to "did my reply go out?".
    /// What the Sensor said one published reply reached, when it has said
    /// (issue #216): the approval republished unchanged on the `.posted`
    /// sibling subject, with `reach` and `posted-as` as headers. Read from
    /// the publication's own position forward, since the report follows the
    /// reply. `None` when the Sensor has not posted it yet, or when the
    /// report lies beyond the window — and the bus being unreachable is
    /// `None` too, logged, because this is a fact *about* a record that was
    /// already read and must not turn that read into a refusal.
    pub async fn posted(&self, recorded: &RecordedApproval) -> Option<Posted> {
        let sequence = recorded.stream_sequence?;
        let jetstream = match self.jetstream().await {
            Ok(jetstream) => jetstream,
            Err(error) => {
                warn!(%error, "the bus did not answer a read of the Sensor's posted reports");
                return None;
            }
        };
        let wanted = recorded.event_id.clone();
        let found = self
            .scan_forward(
                jetstream,
                &format!("{}.posted", bus_subject(REPLY_APPROVED_TYPE)),
                sequence,
                |message| {
                    let envelope: PostedEnvelope = serde_json::from_slice(&message.payload).ok()?;
                    if envelope.id != wanted {
                        return None;
                    }
                    let headers = message.headers.as_ref()?;
                    let header =
                        |name: &str| headers.get(name).map(|value| value.as_str().to_owned());
                    let stream_sequence = message.info().map(|info| info.stream_sequence).ok()?;
                    Some(Posted {
                        reach: header(POSTED_REACH_HEADER)?,
                        posted_as: header(POSTED_AS_HEADER)?,
                        stream_sequence,
                    })
                },
            )
            .await;
        match found {
            Ok(posted) => posted,
            Err(error) => {
                warn!(%error, "the Sensor's posted reports could not be read");
                None
            }
        }
    }

    /// Whether the component that had to send this reply gave up on it
    /// (#311), read from the publication's position forward on the `.dead`
    /// sibling subject — exactly as [`Self::posted`] reads the `.posted`
    /// one, and for the same reason: the dead letter follows the reply.
    ///
    /// Both are read on every request, because they are not opposites. A
    /// bus that cannot be reached is `None` and logged, since this is a
    /// fact *about* a record already read.
    pub async fn given_up(&self, recorded: &RecordedApproval) -> Option<GivenUp> {
        self.sibling_report(recorded, "dead", |message| {
            let stream_sequence = message.info().map(|info| info.stream_sequence).ok()?;
            Some(GivenUp {
                reason: reason_of(message.headers.as_ref()),
                stream_sequence,
            })
        })
        .await
    }

    /// One report about one published reply, read off a sibling subject of
    /// the approval's own — `.posted` or `.dead` — from the publication's
    /// position forward, since a report follows the reply it is about. The
    /// copy carries the approval **unchanged**, so `id` is what identifies
    /// it; `read` says what the report means.
    ///
    /// `None` for all the reasons a fact *about* an already-read record may
    /// be missing, none of which may turn that read into a refusal: no
    /// report yet, a report beyond the window, a bus that did not answer —
    /// the last two logged.
    async fn sibling_report<T>(
        &self,
        recorded: &RecordedApproval,
        sibling: &str,
        read: impl Fn(&async_nats::jetstream::Message) -> Option<T>,
    ) -> Option<T> {
        let sequence = recorded.stream_sequence?;
        let jetstream = match self.jetstream().await {
            Ok(jetstream) => jetstream,
            Err(error) => {
                warn!(%error, %sibling, "the bus did not answer a read of an approval's reports");
                return None;
            }
        };
        let wanted = recorded.event_id.clone();
        let found = self
            .scan_forward(
                jetstream,
                &format!("{}.{sibling}", bus_subject(REPLY_APPROVED_TYPE)),
                sequence,
                |message| {
                    let envelope: PostedEnvelope = serde_json::from_slice(&message.payload).ok()?;
                    if envelope.id != wanted {
                        return None;
                    }
                    read(message)
                },
            )
            .await;
        match found {
            Ok(report) => report,
            Err(error) => {
                warn!(%error, %sibling, "an approval's reports could not be read");
                None
            }
        }
    }

    pub fn recorded(&self, suggestion_event_id: &str) -> Result<Option<RecordedApproval>> {
        self.store.approval(suggestion_event_id)
    }

    /// The bus, connected on first need — the same `OnceCell` the
    /// pending-contact projection uses, and for the same reason: a Gateway
    /// that starts before the bus still comes up.
    ///
    /// Unlike that projection, this connection is made **without**
    /// `retry_on_initial_connect`. An approval that cannot reach the bus must
    /// be refused now, not held in a queue behind a reconnection: the caller
    /// is a human waiting to know whether their reply went out.
    async fn jetstream(&self) -> Result<&async_nats::jetstream::Context> {
        self.bus
            .get_or_try_init(|| async {
                let client = async_nats::ConnectOptions::new()
                    .connect(&self.nats_url)
                    .await
                    .with_context(|| format!("the bus at {} did not answer", self.nats_url))?;
                Ok(async_nats::jetstream::new(client))
            })
            .await
    }

    /// The whole act: find the suggestion, find its trigger, check the three
    /// things that can refuse it, publish, record.
    ///
    /// The order is deliberate and each step is a different answer:
    ///
    /// 1. the approver is the owner — cheap, and about the caller;
    /// 2. the suggestion exists;
    /// 3. it has not expired — before the consent read, so a stale suggestion
    ///    for a revoked contact is reported as stale, which is the fact the
    ///    user acted on;
    /// 4. its trigger's label was `granted` (the audit fact, spec #19);
    /// 5. the trigger exists, which is what names the contact and the room;
    /// 6. **that contact's consent is `granted` now** (`CONTEXT.md`);
    /// 7. it has not already been approved;
    /// 8. publish, then record.
    pub async fn approve(&self, request: Request) -> Result<Approved, Refusal> {
        if let Some(stated) = &request.approved_by {
            if stated != &self.owner {
                return Err(Refusal::NotTheOwner {
                    stated: stated.clone(),
                    owner: self.owner.clone(),
                });
            }
        }
        let jetstream = self.jetstream().await.map_err(|error| {
            warn!(%error, "an approval could not reach the bus");
            Refusal::BusUnreachable(format!("{error:#}"))
        })?;

        let suggestion = self
            .find_suggestion(jetstream, &request.suggestion_event_id)
            .await?;

        let now = crate::consent::rfc3339_millis((self.now)());
        if let Some(expires_at) = &suggestion.expires_at {
            if has_passed(expires_at, &now) {
                return Err(Refusal::Expired {
                    expires_at: expires_at.clone(),
                    now,
                });
            }
        } else {
            // The SDK always sets one (`sdk/python/twalk_sdk/policy.py`); a
            // third-party persona need not. The contract allows it, so this
            // is not a refusal — but it is the one case where an approval can
            // be given at any later date, and an operator should be able to
            // see that it happened.
            debug!(
                suggestion = %suggestion.event_id,
                persona = %suggestion.persona_id,
                "this suggestion carries no expires_at, so nothing makes it stale"
            );
        }

        if suggestion.consent_label != State::Granted {
            return Err(Refusal::NeverConsented {
                label: suggestion.consent_label,
            });
        }

        let trigger = self.find_trigger(jetstream, &suggestion).await?;

        // The clause the definition exists for. A read of this Gateway's own
        // consent state, at this moment — not the label the suggestion was
        // born with, which step 4 has already checked and which says nothing
        // about now. Failing closed: an approval whose consent cannot be read
        // is refused, because refusing a send is recoverable and sending is
        // not ([`Self::consent_now`], shared with the answer path since #206).
        self.consent_now(&trigger.contact, &trigger.connection, trigger.network)?;

        let content = request
            .edited
            .clone()
            .unwrap_or_else(|| suggestion.suggestion.clone());
        // Who wrote what is going out (#327). The gesture says it; a text
        // identical to the draft is the draft, whatever the gesture said,
        // because the one thing a claim of authorship must not be is a way
        // to send the persona's own words undisclosed.
        let written_by = match request.written_by {
            WrittenBy::Owner if content != suggestion.suggestion => WrittenBy::Owner,
            WrittenBy::Owner => {
                info!(
                    suggestion = %suggestion.event_id,
                    "this approval claims the owner's authorship for the persona's own text: \
                     the reply is the draft, and it is disclosed as one"
                );
                WrittenBy::Persona
            }
            WrittenBy::Persona => WrittenBy::Persona,
        };
        // The body alone, before the disclosure is appended: an untouched
        // reply is not an edit, and the sentence is not in the field the
        // user edits (ADR 0031).
        let edited = content != suggestion.suggestion;
        // The switch, read now (ticket #121): the journal's last row, or on.
        // Failing closed the way the consent read does — an approval whose
        // switch cannot be read is refused, since the row it would write
        // next is in the same file and would fail the same way.
        let switch = self.store.disclosure_state().map_err(|error| {
            warn!(%error, "an approval could not read the disclosure switch");
            Refusal::StoreUnavailable(format!(
                "the disclosure switch could not be read: {error:#}"
            ))
        })?;
        // ADR 0019, in its own words: *"A message the user wrote themselves
        // carries nothing: the disclosure states a fact about how **this**
        // message was produced, and attaching it to everything would make
        // it meaningless."* Until #327 the Gateway attached it whenever the
        // switch was on, and a reply the owner had written in place of the
        // draft went out saying a model had written it. A correction keeps
        // the sentence — the words are still the persona's, improved — and
        // that asymmetry is the decision, not an oversight: the alternative
        // makes the disclosure removable by typing a character, which is
        // exactly what the switch exists to do openly instead.
        let disclosure = if switch.enabled && written_by == WrittenBy::Persona {
            suggestion.disclosure.clone()
        } else {
            None
        };
        if let Some(sentence) = &disclosure {
            if !fits_with_disclosure(&content.body, sentence) {
                return Err(Refusal::SuggestionUnreadable(format!(
                    "its body is {} characters and, with the disclosure appended, the reply \
                     would exceed the contract's {CONTRACT_MAX_BODY}. The SDK caps a body at \
                     {MAX_BODY} for exactly this, so the persona that published it ignored the \
                     cap; edit the reply shorter, or refuse it",
                    content.body.chars().count()
                )));
            }
        }
        if switch.enabled && disclosure.is_none() && written_by == WrittenBy::Persona {
            // Not a refusal: the contract allows a suggestion without the
            // member, and a persona that set none is one that predates
            // #121 or is not the first-party SDK. But a reply going out
            // undisclosed while the switch is on is worth one line.
            warn!(
                suggestion = %suggestion.event_id,
                persona = %suggestion.persona_id,
                "this suggestion carries no disclosure, so the reply goes out without one: the \
                 Gateway composes no sentence of its own (ADR 0031)"
            );
        }
        let approval = Approval {
            suggestion,
            trigger,
            approved_by: self.owner.clone(),
            content,
            edited,
            written_by,
            disclosure,
        };
        let event_id = approval.event_id();

        // Already approved? Asked before publishing, so the ordinary second
        // click is answered from the store instead of republishing.
        match self.store.approval(&approval.suggestion.event_id) {
            Ok(Some(existing)) if existing.published_at.is_some() => {
                return Err(Refusal::AlreadyApproved(existing))
            }
            // A row with no publication is the crash window below: the
            // approval was recorded and the publish did not land, or did land
            // and was not marked. Publishing again is safe — the bus
            // deduplicates on the contract's id — and marking it is the
            // repair.
            Ok(Some(existing)) => debug!(
                event_id = %existing.event_id,
                "this approval was recorded and not marked published; republishing it"
            ),
            Ok(None) => {}
            Err(error) => {
                warn!(%error, "an approval could not read the approval store");
                return Err(Refusal::StoreUnavailable(format!("{error:#}")));
            }
        }
        // And the connection itself (#275): a collector that said its
        // connection cannot send is believed, before anything is published —
        // after the store's answer, so a second click on a reply that
        // already left while the connection was up is told where it went
        // rather than that the connection is down now. A bridge's connection
        // says nothing here and is not refused; its reach is #216's
        // `.posted` report. A collector's connection that has never spoken
        // is refused too: nothing is holding it.
        self.connection_can_send(&approval.trigger.connection)?;

        let envelope = approval.envelope(&now);
        // Recorded first, unpublished, so that a crash between the write and
        // the publication leaves a row saying "this was approved and the
        // Gateway does not know where it landed" rather than nothing at all.
        // The row holds no message content: see `store::MIGRATIONS` v5.
        if let Err(error) = self.store.record_approval(
            &event_id,
            &approval.suggestion.event_id,
            &self.owner,
            &approval.suggestion.persona_id,
            approval.trigger.network,
            &approval.trigger.contact,
            approval.edited,
            approval.written_by.as_str(),
            &now,
        ) {
            warn!(%error, "an approval could not be recorded; nothing was published");
            return Err(Refusal::StoreUnavailable(format!("{error:#}")));
        }

        let sequence = self
            .publish(&event_id, &envelope, &approval)
            .await
            .map_err(|error| {
                warn!(%error, %event_id, "an approved reply could not be published");
                Refusal::BusUnreachable(format!("{error:#}"))
            })?;

        if let Err(error) = self.store.mark_approval_published(
            &event_id,
            &crate::consent::rfc3339_millis((self.now)()),
            sequence,
        ) {
            // The reply is out. Saying so is the honest answer, and the
            // status is a 500 because the Gateway's own record is wrong.
            warn!(%error, %event_id, "an approved reply was published and could not be marked");
            return Err(Refusal::Unrecorded(format!("{error:#}")));
        }

        self.metrics.record_approval_published();
        info!(
            event_id = %event_id,
            suggestion = %approval.suggestion.event_id,
            persona = %approval.suggestion.persona_id,
            network = approval.trigger.network.as_str(),
            target = %approval.trigger.target.describe(),
            edited = approval.edited,
            disclosed = approval.disclosure.is_some(),
            stream_sequence = sequence,
            "approved a suggestion: the reply is on the bus"
        );
        Ok(Approved {
            event_id,
            approval,
            approved_at: now,
            stream_sequence: sequence,
        })
    }

    /// Publishes the approved reply, with the contract's id as `Nats-Msg-Id`
    /// and the extensions duplicated as headers — the same shape the
    /// Sensor and the SDK publish with, so a consumer filtering on headers
    /// sees this event like any other.
    async fn publish(&self, event_id: &str, envelope: &Value, approval: &Approval) -> Result<u64> {
        let mut extensions = vec![
            ("network", approval.suggestion.network.as_str()),
            ("connection", approval.suggestion.connection.as_str()),
            ("consent", approval.suggestion.consent_label.as_str()),
        ];
        if let Some(traceparent) = &approval.suggestion.traceparent {
            extensions.push(("traceparent", traceparent.as_str()));
        }
        self.publish_envelope(REPLY_APPROVED_TYPE, event_id, envelope, &extensions)
            .await
    }

    /// Finds one suggestion on the bus by its CloudEvents id.
    /// The message one suggestion answers (#336).
    ///
    /// Everything this route refuses, it refuses for a reason the approval
    /// path already has a name for: a suggestion nobody can find, a trigger
    /// beyond the bounded read, a contact whose consent does not stand **at
    /// this moment**. The last is the one that matters: a screen that could
    /// show a revoked contact's message because it was granted yesterday
    /// would be a way around the decision, and this read is refused exactly
    /// where an approval would be.
    pub async fn answered(&self, suggestion_event_id: &str) -> Result<Answered, Refusal> {
        let jetstream = self.jetstream().await.map_err(|error| {
            warn!(%error, "a message read could not reach the bus");
            Refusal::BusUnreachable(format!("{error:#}"))
        })?;
        let suggestion = self.find_suggestion(jetstream, suggestion_event_id).await?;
        if suggestion.consent_label != State::Granted {
            return Err(Refusal::NeverConsented {
                label: suggestion.consent_label,
            });
        }
        let trigger_event_id = suggestion.trigger_event_id.clone();
        let found = self
            .scan(
                jetstream,
                &bus_subject(crate::contacts::INBOUND_MESSAGE_TYPE),
                Some(suggestion.stream_sequence),
                |message| {
                    let document: AnsweredDocument =
                        serde_json::from_slice(&message.payload).ok()?;
                    (document.id == trigger_event_id).then_some(document)
                },
            )
            .await
            .map_err(|error| {
                warn!(%error, "a message read could not read the bus");
                Refusal::BusUnreachable(format!("{error:#}"))
            })?;
        let document = match found {
            Found::Match(document, _) => document,
            Found::None { exhaustive } => {
                return Err(if exhaustive {
                    Refusal::TriggerNotFound {
                        trigger_event_id: suggestion.trigger_event_id.clone(),
                    }
                } else {
                    Refusal::TriggerOutOfReach {
                        trigger_event_id: suggestion.trigger_event_id.clone(),
                        window: self.lookup_window,
                    }
                })
            }
        };
        let network = Network::parse(&document.network).unwrap_or(suggestion.network);
        let connection = connection_of(&self.connections, document.connection.as_deref(), network)?;
        // The clause this route exists under: consent now, not the label
        // the message arrived with.
        self.consent_now(&document.subject, &connection, network)?;
        Ok(Answered {
            contact: document
                .data
                .contact
                .and_then(|contact| contact.display_name),
            received_at: document.data.network_timestamp,
            body: document.data.body.unwrap_or_default(),
            format: document
                .data
                .format
                .unwrap_or_else(|| Format::Plain.as_str().to_owned()),
            attachments: document.data.attachments.len(),
        })
    }

    async fn find_suggestion(
        &self,
        jetstream: &async_nats::jetstream::Context,
        event_id: &str,
    ) -> Result<Suggestion, Refusal> {
        let found = self
            .scan(
                jetstream,
                &bus_subject(SUGGEST_PRODUCED_TYPE),
                None,
                |message| {
                    let document: SuggestionDocument =
                        serde_json::from_slice(&message.payload).ok()?;
                    (document.id == event_id).then_some(document)
                },
            )
            .await
            .map_err(|error| {
                warn!(%error, "the suggestion lookup could not read the bus");
                Refusal::BusUnreachable(format!("{error:#}"))
            })?;
        let (document, sequence) = match found {
            Found::Match(document, sequence) => (document, sequence),
            Found::None { exhaustive } => {
                return Err(if exhaustive {
                    Refusal::SuggestionNotFound
                } else {
                    Refusal::SuggestionOutOfReach {
                        window: self.lookup_window,
                    }
                })
            }
        };
        let network = Network::parse(&document.network).ok_or_else(|| {
            Refusal::SuggestionUnreadable(format!(
                "it names the network {:?}, which this build does not know",
                document.network
            ))
        })?;
        let consent_label = State::parse(&document.consent).ok_or_else(|| {
            Refusal::SuggestionUnreadable(format!(
                "it names the consent state {:?}, which this build does not know",
                document.consent
            ))
        })?;
        let format = Format::parse(&document.data.suggestion.format).ok_or_else(|| {
            Refusal::SuggestionUnreadable(format!(
                "its body has the format {:?}, which the contract does not name",
                document.data.suggestion.format
            ))
        })?;
        Ok(Suggestion {
            event_id: document.id,
            source: document.source,
            persona_id: document.data.persona_id,
            trigger_event_id: document.subject,
            connection: connection_of(&self.connections, document.connection.as_deref(), network)?,
            network,
            consent_label,
            suggestion: Content {
                body: document.data.suggestion.body,
                format,
            },
            disclosure: contract_sentence(document.data.disclosure)?,
            expires_at: document.data.expires_at,
            traceparent: document.traceparent,
            stream_sequence: sequence,
        })
    }

    /// Finds the inbound event a suggestion answers, searching back from the
    /// suggestion's own position rather than from the stream's head.
    async fn find_trigger(
        &self,
        jetstream: &async_nats::jetstream::Context,
        suggestion: &Suggestion,
    ) -> Result<Trigger, Refusal> {
        let trigger_event_id = suggestion.trigger_event_id.clone();
        let found = self
            .scan(
                jetstream,
                &bus_subject(crate::contacts::INBOUND_MESSAGE_TYPE),
                Some(suggestion.stream_sequence),
                |message| {
                    let document: TriggerDocument =
                        serde_json::from_slice(&message.payload).ok()?;
                    (document.id == trigger_event_id).then_some(document)
                },
            )
            .await
            .map_err(|error| {
                warn!(%error, "the trigger lookup could not read the bus");
                Refusal::BusUnreachable(format!("{error:#}"))
            })?;
        let document = match found {
            Found::Match(document, _) => document,
            Found::None { exhaustive } => {
                return Err(if exhaustive {
                    Refusal::TriggerNotFound {
                        trigger_event_id: suggestion.trigger_event_id.clone(),
                    }
                } else {
                    Refusal::TriggerOutOfReach {
                        trigger_event_id: suggestion.trigger_event_id.clone(),
                        window: self.lookup_window,
                    }
                })
            }
        };
        let network = Network::parse(&document.network).unwrap_or(suggestion.network);
        let connection = connection_of(&self.connections, document.connection.as_deref(), network)?;
        // A room to post into, or a mail to reply to: the source says which
        // (ADR 0033), and a mail without its Message-ID cannot be answered
        // in its thread — refused rather than sent as a new conversation.
        let target = match room_from_source(&document.source) {
            Some(room_id) => Target::Room { room_id },
            None if document.source.starts_with("jmap://") => Target::Mail {
                connection: connection.clone(),
                in_reply_to: document.data.message_id.clone().ok_or_else(|| {
                    Refusal::SuggestionUnreadable(
                        "the trigger is a mail without a Message-ID: the reply has nothing to \
                         thread under"
                            .to_owned(),
                    )
                })?,
                // The address is the sender the consent check is about, and
                // nothing the collector could look up: a mail's subject on
                // this source is its sender's `mailto:` by contract.
                recipient: if document.subject.starts_with("mailto:") {
                    document.subject.clone()
                } else {
                    return Err(Refusal::SuggestionUnreadable(format!(
                        "the trigger is a mail whose subject {:?} is not a mailto: address",
                        document.subject
                    )));
                },
            },
            None => {
                return Err(Refusal::TriggerHasNoRoom {
                    source: document.source.clone(),
                })
            }
        };
        Ok(Trigger {
            contact: document.subject,
            connection,
            network,
            target,
        })
    }

    /// Whether the connection can take a reply (#275); see
    /// [`connection_can_send`].
    fn connection_can_send(&self, connection: &str) -> Result<(), Refusal> {
        connection_can_send(&self.store, &self.connections, connection)
    }

    /// Reads one subject from `start` forward, for at most the window, and
    /// returns the first message a predicate accepts. The report of a posted
    /// reply follows the reply, so this reads the other way from [`Self::scan`].
    async fn scan_forward<T>(
        &self,
        jetstream: &async_nats::jetstream::Context,
        filter_subject: &str,
        start: u64,
        mut matches: impl FnMut(&async_nats::jetstream::Message) -> Option<T>,
    ) -> Result<Option<T>> {
        let mut stream = jetstream
            .get_stream(STREAM_NAME)
            .await
            .context("failed to reach the bus stream")?;
        let last_sequence = stream
            .info()
            .await
            .context("failed to read the bus stream's state")?
            .state
            .last_sequence;
        if last_sequence < start {
            return Ok(None);
        }
        let ceiling = start.saturating_add(self.lookup_window).min(last_sequence);
        let name = format!(
            "gateway-posted-lookup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        );
        let consumer = stream
            .create_consumer(async_nats::jetstream::consumer::pull::Config {
                name: Some(name.clone()),
                filter_subject: filter_subject.to_owned(),
                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::ByStartSequence {
                    start_sequence: start,
                },
                ack_policy: async_nats::jetstream::consumer::AckPolicy::None,
                inactive_threshold: LOOKUP_CONSUMER_IDLE,
                ..Default::default()
            })
            .await
            .context("failed to open a read of the bus")?;
        let mut batch = consumer
            .fetch()
            .max_messages(usize::try_from(ceiling - start + 1).unwrap_or(usize::MAX))
            .messages()
            .await
            .context("failed to read from the bus")?;
        let mut found = None;
        {
            use futures::StreamExt;
            while let Some(message) = batch.next().await {
                let Ok(message) = message else { break };
                let sequence = message
                    .info()
                    .map(|info| info.stream_sequence)
                    .unwrap_or_default();
                if sequence > ceiling {
                    break;
                }
                if let Some(hit) = matches(&message) {
                    found = Some(hit);
                    break;
                }
            }
        }
        if let Err(error) = stream.delete_consumer(&name).await {
            debug!(%error, "failed to delete the posted lookup's consumer");
        }
        Ok(found)
    }

    /// Reads the tail of one subject, newest window first, and returns the
    /// first message a predicate accepts.
    ///
    /// `anchor` bounds the search above: the trigger lookup passes the
    /// suggestion's own position, because the event a suggestion answers is
    /// always before it. `exhaustive` says whether the window reached the
    /// stream's first retained message — which is what makes "not there" and
    /// "not looked that far" two different answers.
    async fn scan<T>(
        &self,
        jetstream: &async_nats::jetstream::Context,
        filter_subject: &str,
        anchor: Option<u64>,
        mut matches: impl FnMut(&async_nats::jetstream::Message) -> Option<T>,
    ) -> Result<Found<T>> {
        let mut stream = jetstream
            .get_stream(STREAM_NAME)
            .await
            .context("failed to reach the bus stream")?;
        let (first_sequence, last_sequence) = {
            let info = stream
                .info()
                .await
                .context("failed to read the bus stream's state")?;
            (info.state.first_sequence, info.state.last_sequence)
        };
        let first = first_sequence.max(1);
        let ceiling = anchor.unwrap_or(last_sequence);
        if last_sequence == 0 || ceiling < first {
            return Ok(Found::None { exhaustive: true });
        }
        let start = ceiling.saturating_sub(self.lookup_window).max(first);
        let exhaustive = start <= first;
        let name = format!(
            "gateway-approval-lookup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        );
        let consumer = stream
            .create_consumer(async_nats::jetstream::consumer::pull::Config {
                name: Some(name.clone()),
                filter_subject: filter_subject.to_owned(),
                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::ByStartSequence {
                    start_sequence: start,
                },
                // Nothing is acked and nothing resumes: this consumer exists
                // for the length of one request and is deleted below.
                ack_policy: async_nats::jetstream::consumer::AckPolicy::None,
                inactive_threshold: LOOKUP_CONSUMER_IDLE,
                ..Default::default()
            })
            .await
            .context("failed to open a read of the bus")?;
        let mut batch = consumer
            .fetch()
            .max_messages(usize::try_from(ceiling - start + 1).unwrap_or(usize::MAX))
            .messages()
            .await
            .context("failed to read from the bus")?;
        let mut found = None;
        {
            use futures::StreamExt;
            while let Some(message) = batch.next().await {
                let Ok(message) = message else { break };
                let sequence = message
                    .info()
                    .map(|info| info.stream_sequence)
                    .unwrap_or_default();
                if sequence > ceiling {
                    break;
                }
                if let Some(hit) = matches(&message) {
                    // Newest wins: the loop keeps going only until a match,
                    // and the contract's ids are unique, so the first match
                    // is the only one.
                    found = Some((hit, sequence));
                    break;
                }
            }
        }
        if let Err(error) = stream.delete_consumer(&name).await {
            // Harmless: `inactive_threshold` reaps it anyway.
            debug!(%error, "failed to delete the approval lookup's consumer");
        }
        Ok(match found {
            Some((hit, sequence)) => Found::Match(hit, sequence),
            None => Found::None { exhaustive },
        })
    }
}

/// The outcome of a bounded read of the bus.
enum Found<T> {
    Match(T, u64),
    /// Nothing matched. `exhaustive` is true when the read reached the
    /// stream's first retained message, and false when the window stopped it
    /// first — the difference between "it is not there" and "I did not look
    /// that far".
    None {
        exhaustive: bool,
    },
}

/// An approval that happened: the event on the bus, and where it landed.
#[derive(Debug, Clone)]
pub struct Approved {
    pub event_id: String,
    pub approval: Approval,
    pub approved_at: String,
    pub stream_sequence: u64,
}

/// Whether a body and the sentence appended after it fit the contract's
/// `final.body` (ticket #121).
///
/// The edited path cannot get here over the limit — [`parse_content`] caps
/// the body at [`MAX_BODY`] — but the **unedited** path sends the
/// suggestion's own body, and the suggestion schema still allows 65 536:
/// a third-party persona that ignored the SDK's cap can publish a body the
/// sentence no longer fits after. That is refused rather than the sentence
/// withheld, because a reply going out undisclosed is the one outcome ADR
/// 0031 exists to prevent — a suggestion that cannot be disclosed is one
/// that should not exist — and refused as `suggestion_unreadable` rather
/// than `malformed_request`, because the request was fine: what this
/// Gateway cannot turn into a contract event is the suggestion. The user's
/// way out is the edited path, which caps the body so the line always fits.
fn fits_with_disclosure(body: &str, sentence: &str) -> bool {
    body.chars().count() + 1 + sentence.chars().count() <= CONTRACT_MAX_BODY
}

/// The suggestion's `data.disclosure` as this Gateway will append it: one
/// of the contract's five sentences, verbatim, or none — never anything
/// else (ticket #121, ADR 0031).
///
/// The schema says only `string, 1..200`, and the reference bus has no
/// authentication, so without this check anything that can publish a
/// suggestion — a third-party persona, or anything on the bus — could put
/// two hundred characters of its own after the user's reply, in the block
/// the approval screen draws as *fixed* and trains the user not to read,
/// and this Gateway would send it. ADR 0031's guarantee is that the model
/// never writes the sentence so it cannot be argued out of it; a sentence
/// the contract does not hold is refused for the same reason and in the
/// same shape as a body the line no longer fits after
/// ([`fits_with_disclosure`]): `suggestion_unreadable`, because the request
/// was fine and what cannot be turned into a contract event is the
/// suggestion — and refused rather than sent without the line, since a reply
/// going out undisclosed is the outcome the ADR exists to prevent. An empty
/// string is refused with the rest: the schema does not allow one either.
///
/// The text itself is not echoed: the refusal names the contract file and
/// the length, which is what an operator needs to find the persona that
/// published it, and a forged sentence is not something to relay to a
/// screen.
fn contract_sentence(member: Option<String>) -> Result<Option<String>, Refusal> {
    match member {
        None => Ok(None),
        Some(sentence) if crate::disclosure::is_contract_sentence(&sentence) => Ok(Some(sentence)),
        Some(other) => Err(Refusal::SuggestionUnreadable(format!(
            "its disclosure is not one of the contract's sentences: {} characters that \
             contracts/disclosure/v1/sentences.json does not hold, verbatim, in any of its {} \
             languages. What a contact is told is the contract's own sentence and never a \
             persona's wording (ADR 0031), so this Gateway will not append it, and will not \
             send the reply without it either",
            other.chars().count(),
            crate::disclosure::languages().len()
        ))),
    }
}

/// Whether an RFC 3339 instant is at or before another.
///
/// Compared as instants rather than as strings: a suggestion's `expires_at`
/// is stamped by a persona (whole seconds, `Z`) and the Gateway's clock is
/// formatted with milliseconds, so the two spellings do not compare.
/// An `expires_at` that is not RFC 3339 cannot make a suggestion stale, and
/// says so in a log line rather than silently expiring or silently not.
pub fn has_passed(expires_at: &str, now: &str) -> bool {
    use time::format_description::well_known::Rfc3339;
    let (Ok(expiry), Ok(now)) = (
        time::OffsetDateTime::parse(expires_at, &Rfc3339),
        time::OffsetDateTime::parse(now, &Rfc3339),
    ) else {
        warn!(
            %expires_at,
            "a suggestion's expires_at is not RFC 3339, so nothing makes it stale"
        );
        return false;
    };
    expiry <= now
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggestion() -> Suggestion {
        Suggestion {
            event_id: "a".repeat(64),
            source: "hermes://twalk.example.com/personas/assistant".to_owned(),
            persona_id: "assistant".to_owned(),
            trigger_event_id: "b".repeat(64),
            network: Network::Whatsapp,
            connection: "whatsapp".to_owned(),
            consent_label: State::Granted,
            suggestion: Content {
                body: "Pas de problème, à 20h !".to_owned(),
                format: Format::Plain,
            },
            disclosure: Some("Rédigé avec mon assistant IA.".to_owned()),
            expires_at: Some("2026-09-17T11:00:00Z".to_owned()),
            traceparent: Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_owned()),
            stream_sequence: 42,
        }
    }

    fn trigger() -> Trigger {
        Trigger {
            contact: "@whatsapp_33612345678:example.com".to_owned(),
            connection: "whatsapp".to_owned(),
            network: Network::Whatsapp,
            target: Target::Room {
                room_id: "!abcXYZ123:example.com".to_owned(),
            },
        }
    }

    #[test]
    fn the_id_is_the_contracts_natural_key() {
        let approval = Approval {
            suggestion: suggestion(),
            trigger: trigger(),
            approved_by: "@michel:example.com".to_owned(),
            content: suggestion().suggestion,
            edited: false,
            written_by: WrittenBy::Persona,
            disclosure: None,
        };
        let mut hasher = Sha256::new();
        hasher.update(format!("{}:@michel:example.com", "a".repeat(64)).as_bytes());
        assert_eq!(approval.event_id(), format!("{:x}", hasher.finalize()));
        // And it carries no clock, so the same approval given twice is one
        // event on the bus.
        assert_eq!(approval.event_id(), approval.event_id());
    }

    #[test]
    fn a_connection_can_send_when_it_said_connected_or_is_a_bridges_and_not_otherwise() {
        // #275: three connections in a registry — a bridge's, which never
        // speaks on the status subject, and two of the collector's kinds, one
        // that said `reconnect_required` and one that never spoke.
        let dir = std::env::temp_dir().join(format!(
            "gateway-approval-connection-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let owner = Arc::new(crate::owner::Owner::new("@michel:example.com", []));
        let store = Store::open(&dir, owner).unwrap();
        let registry = crate::connections::Registry::from_config(
            Some("whatsapp=whatsapp,mail-linagora=email,agenda-linagora=calendar"),
            &[],
            "example.com",
        )
        .unwrap();
        let mut down = crate::connection_status::Change {
            event_id: "e1".to_owned(),
            connection: "mail-linagora".to_owned(),
            kind: "email".to_owned(),
            from_state: "connected".to_owned(),
            to_state: "reconnect_required".to_owned(),
            occurred_at: "2026-09-20T09:00:00Z".to_owned(),
            service: Some("sso".to_owned()),
            hint: Some("Run `twalk-collector authorize --renew`.".to_owned()),
        };
        store
            .record_connection_status_change(&down, "2026-09-20T09:00:01Z")
            .unwrap();

        assert!(
            connection_can_send(&store, &registry, "whatsapp").is_ok(),
            "a bridge's says nothing here"
        );
        match connection_can_send(&store, &registry, "mail-linagora") {
            Err(Refusal::ConnectionNotConnected { state, hint, .. }) => {
                assert_eq!(state, "reconnect_required");
                assert!(hint.unwrap().contains("authorize --renew"));
            }
            other => panic!("a connection that said it cannot send is refused: {other:?}"),
        }
        match connection_can_send(&store, &registry, "agenda-linagora") {
            Err(Refusal::ConnectionNotConnected { state, hint, .. }) => {
                assert_eq!(state, "unknown");
                assert!(hint.unwrap().contains("No collector has reported"));
            }
            other => panic!("a collector's connection nobody spoke for is refused: {other:?}"),
        }
        down.event_id = "e2".to_owned();
        down.from_state = "reconnect_required".to_owned();
        down.to_state = "connected".to_owned();
        down.occurred_at = "2026-09-20T10:00:00Z".to_owned();
        store
            .record_connection_status_change(&down, "2026-09-20T10:00:01Z")
            .unwrap();
        assert!(connection_can_send(&store, &registry, "mail-linagora").is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// #327: the gesture says who wrote the text, and the envelope carries
    /// it — beside `edited`, which answers a different question.
    #[test]
    fn a_request_says_who_wrote_the_text_and_says_nothing_by_saying_persona() {
        let request = |final_member: Value| {
            Request::parse(&json!({
                "suggestion_event_id": "a".repeat(64),
                "final": final_member
            }))
        };
        assert_eq!(
            request(json!({ "body": "Merci !" })).unwrap().written_by,
            WrittenBy::Persona,
            "a client that says nothing sends the persona's text: the reading that keeps the \
             sentence, never the one that drops it"
        );
        assert_eq!(
            request(json!({ "body": "Merci !", "written_by": "owner" }))
                .unwrap()
                .written_by,
            WrittenBy::Owner
        );
        assert_eq!(
            Request::parse(&json!({ "suggestion_event_id": "a".repeat(64) }))
                .unwrap()
                .written_by,
            WrittenBy::Persona,
            "no final at all is the persona's text by definition"
        );
        // A claim about who wrote a text cannot exist where there is no
        // text, which is why it lives inside `final`.
        match Request::parse(&json!({
            "suggestion_event_id": "a".repeat(64),
            "written_by": "owner"
        })) {
            Err(Invalid::UnknownMember(member)) => assert_eq!(member, "written_by"),
            other => panic!("{other:?}"),
        }
        match request(json!({ "body": "Merci !", "written_by": "somebody" })) {
            Err(Invalid::UnknownWrittenBy(value)) => assert_eq!(value, "somebody"),
            other => panic!("{other:?}"),
        }
        let envelope = Approval {
            suggestion: suggestion(),
            trigger: trigger(),
            approved_by: "@michel:example.com".to_owned(),
            content: Content {
                body: "Bonjour, je regarde ça cette semaine.".to_owned(),
                format: Format::Plain,
            },
            edited: true,
            written_by: WrittenBy::Owner,
            // The owner's own words carry nothing (ADR 0019), which is why
            // the composer sets no sentence for them in the first place.
            disclosure: None,
        }
        .envelope("2026-09-23T10:04:37.000Z");
        assert_eq!(envelope["data"]["written_by"], json!("owner"));
        assert_eq!(envelope["data"]["edited"], json!(true));
        assert!(
            !envelope["data"]["final"]["body"]
                .as_str()
                .unwrap_or_default()
                .contains("assistant"),
            "a reply the owner wrote says nothing about an assistant: {envelope}"
        );
    }

    #[test]
    fn the_envelope_is_shaped_as_the_contract_describes() {
        let approval = Approval {
            suggestion: suggestion(),
            trigger: trigger(),
            approved_by: "@michel:example.com".to_owned(),
            content: Content {
                body: "Pas de problème, à 20h ! 👍".to_owned(),
                format: Format::Plain,
            },
            edited: true,
            written_by: WrittenBy::Persona,
            disclosure: Some("Rédigé avec mon assistant IA.".to_owned()),
        };
        let event = approval.envelope("2026-09-17T10:04:37.000Z");
        assert_eq!(event["type"], json!(REPLY_APPROVED_TYPE));
        assert_eq!(event["subject"], json!("a".repeat(64)));
        assert_eq!(
            event["source"],
            json!("hermes://twalk.example.com/personas/assistant"),
            "the source names the persona whose suggestion was approved, not the publisher"
        );
        assert_eq!(event["network"], json!("whatsapp"));
        assert_eq!(event["consent"], json!("granted"));
        assert_eq!(event["data"]["edited"], json!(true));
        assert_eq!(
            event["data"]["target"]["room_id"],
            json!("!abcXYZ123:example.com")
        );
        assert!(
            event["data"]["target"].get("reply_to_event_id").is_none(),
            "the inbound contract carries no Matrix event id of its own, so there is nothing to \
             thread under"
        );
        assert_eq!(
            event["traceparent"],
            json!("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        );
        // The disclosure: after the body, on a line of its own, and named
        // again as a member so a consumer need not parse the body (#121).
        assert_eq!(
            event["data"]["final"]["body"],
            json!("Pas de problème, à 20h ! 👍\nRédigé avec mon assistant IA."),
            "the contract's own fixture: body, newline, sentence"
        );
        assert_eq!(
            event["data"]["disclosure"],
            json!("Rédigé avec mon assistant IA.")
        );
    }

    #[test]
    fn with_the_switch_off_the_event_carries_neither_the_line_nor_the_member() {
        let approval = Approval {
            suggestion: suggestion(),
            trigger: trigger(),
            approved_by: "@michel:example.com".to_owned(),
            content: suggestion().suggestion,
            edited: false,
            written_by: WrittenBy::Persona,
            disclosure: None,
        };
        let event = approval.envelope("2026-09-17T10:04:37.000Z");
        assert_eq!(
            event["data"]["final"]["body"],
            json!("Pas de problème, à 20h !"),
            "the suggestion carried a sentence, and the switch being off means it is not sent"
        );
        assert!(
            event["data"].get("disclosure").is_none(),
            "an absent member, not a null: the schema closes `data` and a bus that recorded a \
             disclosure nobody received would be lying about what the contact got"
        );
        assert_eq!(event["data"]["edited"], json!(false));
    }

    #[test]
    fn the_body_cap_leaves_room_for_the_disclosure_line() {
        assert_eq!(MAX_BODY, 65_335, "65536, less a newline and 200 characters");
        let longest = "x".repeat(MAX_BODY);
        assert!(parse_content(&json!({ "body": longest })).is_ok());
        let refusal = parse_content(&json!({ "body": "x".repeat(MAX_BODY + 1) }))
            .expect_err("one over is refused before the disclosure is appended");
        assert_eq!(refusal.code(), "malformed_request");
        assert!(
            refusal.message().contains("disclosure"),
            "{}",
            refusal.message()
        );
        assert!(refusal.message().contains("65335"), "{}", refusal.message());
        // And the sum never exceeds the schema, whatever the sentence.
        let appended =
            crate::disclosure::append(&longest, &"y".repeat(crate::disclosure::MAX_CHARS));
        assert_eq!(appended.chars().count(), CONTRACT_MAX_BODY);
    }

    #[test]
    fn an_unedited_body_the_sentence_no_longer_fits_after_is_refused_not_undisclosed() {
        let sentence = "Rédigé avec mon assistant IA.";
        // The SDK's cap, and every body under it: the line always fits.
        assert!(fits_with_disclosure(&"x".repeat(MAX_BODY), sentence));
        assert!(fits_with_disclosure(
            &"x".repeat(MAX_BODY),
            &"y".repeat(crate::disclosure::MAX_CHARS)
        ));
        // A contract-valid suggestion from a persona that ignored the cap.
        assert!(!fits_with_disclosure(
            &"x".repeat(CONTRACT_MAX_BODY),
            sentence
        ));
        assert!(fits_with_disclosure(
            &"x".repeat(CONTRACT_MAX_BODY - 1 - sentence.chars().count()),
            sentence
        ));
        assert!(!fits_with_disclosure(
            &"x".repeat(CONTRACT_MAX_BODY - sentence.chars().count()),
            sentence
        ));
    }

    #[test]
    fn a_disclosure_the_contract_does_not_hold_is_refused_not_appended_and_not_withheld() {
        // The contract's own, verbatim: read as it is. None: none.
        assert_eq!(
            contract_sentence(Some("Rédigé avec mon assistant IA.".to_owned())).unwrap(),
            Some("Rédigé avec mon assistant IA.".to_owned())
        );
        assert_eq!(contract_sentence(None).unwrap(), None);
        // A persona's own wording, the length of a real sentence and
        // contract-valid on the bus (`string, 1..200`): what a rogue
        // persona would put after the user's reply. Refused as
        // `suggestion_unreadable` — the request was fine — with the contract
        // file named and the text not echoed.
        let forged = "Written by an assistant you can trust.";
        assert_eq!(forged.chars().count(), 38);
        let refusal = contract_sentence(Some(forged.to_owned()))
            .expect_err("a sentence outside the contract is refused");
        assert_eq!(refusal.code(), "suggestion_unreadable");
        assert_eq!(refusal.status(), axum::http::StatusCode::CONFLICT);
        let message = refusal.message();
        assert!(
            message.contains("contracts/disclosure/v1/sentences.json"),
            "{message}"
        );
        assert!(message.contains("38 characters"), "{message}");
        assert!(
            !message.contains(forged),
            "the forged text is not relayed: {message}"
        );
        assert!(message.contains("Nothing was sent"), "{message}");
        // And the empty string the schema refuses is refused here too.
        assert_eq!(
            contract_sentence(Some(String::new())).unwrap_err().code(),
            "suggestion_unreadable"
        );
    }

    #[test]
    fn a_mail_triggers_approval_targets_the_mail_connection_in_its_thread() {
        // #278: the contract's second target shape, which the collector
        // sends and the Sensor leaves alone. The fixture's data is the
        // Gateway's own.
        let approval = Approval {
            suggestion: suggestion(),
            trigger: Trigger {
                contact: "mailto:alice@example.org".to_owned(),
                connection: "mail-linagora".to_owned(),
                network: Network::Email,
                target: Target::Mail {
                    connection: "mail-linagora".to_owned(),
                    in_reply_to: "<9b8c7d6e-1@example.org>".to_owned(),
                    recipient: "mailto:alice@example.org".to_owned(),
                },
            },
            approved_by: "@michel:example.com".to_owned(),
            content: Content {
                body: "Oui, lundi 9h me va.".to_owned(),
                format: Format::Plain,
            },
            edited: false,
            written_by: WrittenBy::Persona,
            // The switch is not what this test is about; a mail reply
            // carries the disclosure exactly as a room reply does (#121).
            disclosure: None,
        };
        let event = approval.envelope("2026-09-21T08:20:11Z");
        let fixture: Value = serde_json::from_str(include_str!(
            "../../contracts/cloudevents/v1/fixtures/variants/persona.reply.approved/mail.json"
        ))
        .unwrap();
        assert_eq!(event["data"]["target"], fixture["data"]["target"]);
        assert!(event["data"]["target"].get("room_id").is_none());
        // And the reader: a mailbox source with a Message-ID is a mail
        // target; without one, refused rather than answered out of thread.
        let with: TriggerDocument = serde_json::from_value(json!({
            "id": "x", "source": "jmap://mail.example.com/u1/inbox-1",
            "subject": "mailto:alice@example.org", "network": "email",
            "connection": "mail-linagora",
            "data": { "message_id": "<9b8c7d6e-1@example.org>", "body": "never a value here" }
        }))
        .unwrap();
        assert_eq!(
            with.data.message_id.as_deref(),
            Some("<9b8c7d6e-1@example.org>")
        );
        assert!(room_from_source(&with.source).is_none());
    }

    #[test]
    fn a_list_of_suggestions_is_refused_as_a_batch_and_not_as_a_typo() {
        let refusal = Request::parse(&json!([{ "suggestion_event_id": "a".repeat(64) }]))
            .expect_err("a list is not an approval");
        assert_eq!(refusal.code(), "approval_is_not_a_batch");
        assert!(
            refusal.message().contains("one request per suggestion"),
            "{}",
            refusal.message()
        );
        // And so is a single body whose id member is a list.
        let refusal = Request::parse(&json!({ "suggestion_event_id": ["a".repeat(64)] }))
            .expect_err("a list of ids is not an approval");
        assert_eq!(refusal.code(), "approval_is_not_a_batch");
    }

    #[test]
    fn the_request_is_one_suggestion_and_an_optional_edit() {
        let request = Request::parse(&json!({
            "suggestion_event_id": "a".repeat(64),
            "final": { "body": "à 20h", "format": "text/markdown" }
        }))
        .expect("a well-formed approval");
        assert_eq!(request.suggestion_event_id, "a".repeat(64));
        assert_eq!(request.approved_by, None);
        assert_eq!(
            request.edited,
            Some(Content {
                body: "à 20h".to_owned(),
                format: Format::Markdown,
            })
        );
        // The format defaults to the safest of the contract's three.
        let request = Request::parse(&json!({
            "suggestion_event_id": "a".repeat(64),
            "final": { "body": "à 20h" }
        }))
        .expect("a well-formed approval");
        assert_eq!(request.edited.unwrap().format, Format::Plain);
    }

    #[test]
    fn a_member_nobody_declared_is_refused_rather_than_ignored() {
        let refusal = Request::parse(&json!({
            "suggestion_event_id": "a".repeat(64),
            "send_now": true
        }))
        .expect_err("an unknown member is refused");
        assert_eq!(refusal.code(), "malformed_request");
        assert!(
            refusal.message().contains("send_now"),
            "{}",
            refusal.message()
        );
    }

    #[test]
    fn an_empty_reply_is_refused_because_approving_one_sends_it() {
        let refusal = Request::parse(&json!({
            "suggestion_event_id": "a".repeat(64),
            "final": { "body": "" }
        }))
        .expect_err("an empty body is refused");
        assert_eq!(refusal.code(), "malformed_request");
        assert!(
            refusal.message().contains("refuse the suggestion instead"),
            "{}",
            refusal.message()
        );
    }

    #[test]
    fn an_id_that_is_not_the_contracts_is_refused_before_the_bus_is_read() {
        for value in ["", "not-an-id", &"A".repeat(64), &"a".repeat(63)] {
            assert!(
                Request::parse(&json!({ "suggestion_event_id": value })).is_err(),
                "{value:?} is not a contract event id"
            );
        }
        assert!(is_event_id(&"0123456789abcdef".repeat(4)));
    }

    #[test]
    fn an_event_that_names_no_connection_is_resolved_in_the_registry_or_refused() {
        // A producer older than #269 on a deployment with one connection per
        // kind: the registry's single one of that kind stands in — read
        // there, not spelled from the network's name. Two of the kind, or
        // none, and the Gateway will not guess which perimeter it was.
        let registry = crate::connections::Registry::from_config(
            Some("wa-home=whatsapp,wa-work=whatsapp,signal=signal"),
            &[],
            "example.com",
        )
        .expect("a registry");
        assert_eq!(
            connection_of(&registry, Some(" wa-work "), Network::Whatsapp).unwrap(),
            "wa-work",
            "what the event carries is what it belongs to"
        );
        assert!(
            matches!(
                connection_of(&registry, Some("wa-old"), Network::Whatsapp),
                Err(Refusal::SuggestionUnreadable(_))
            ),
            "a carried id the registry does not know is not trusted either"
        );
        assert_eq!(
            connection_of(&registry, None, Network::Signal).unwrap(),
            "signal"
        );
        assert_eq!(
            connection_of(&registry, None, Network::Matrix).unwrap(),
            "matrix",
            "the native connection every registry has"
        );
        for (network, reason_names) in [
            (Network::Whatsapp, "2 connections"),
            (Network::Telegram, "no connection"),
        ] {
            match connection_of(&registry, Some(""), network) {
                Err(Refusal::SuggestionUnreadable(reason)) => {
                    assert!(reason.contains(reason_names), "{reason}");
                }
                other => panic!("{network:?}: expected a refusal, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_room_comes_out_of_the_triggers_source() {
        assert_eq!(
            room_from_source("matrix://matrix.example.com/!abcXYZ123:example.com").as_deref(),
            Some("!abcXYZ123:example.com")
        );
        for source in [
            "hermes://twalk.example.com/personas/assistant",
            "matrix://matrix.example.com/abcXYZ123:example.com",
            "matrix://matrix.example.com",
            "",
        ] {
            assert_eq!(
                room_from_source(source),
                None,
                "{source:?} is not a portal room"
            );
        }
    }

    #[test]
    fn expiry_compares_instants_and_not_spellings() {
        // The persona stamps whole seconds; the Gateway stamps milliseconds.
        assert!(has_passed(
            "2026-09-17T11:00:00Z",
            "2026-09-17T11:00:00.001Z"
        ));
        assert!(!has_passed(
            "2026-09-17T11:00:00Z",
            "2026-09-17T10:59:59.999Z"
        ));
        // An offset is an instant like any other.
        assert!(has_passed(
            "2026-09-17T11:00:00+02:00",
            "2026-09-17T09:30:00.000Z"
        ));
        // Unreadable: nothing makes the suggestion stale, which is the
        // direction that refuses nothing wrongly.
        assert!(!has_passed("whenever", "2026-09-17T11:00:00.000Z"));
    }

    #[test]
    fn every_refusal_has_its_own_code_and_its_own_status() {
        let refusals = [
            Refusal::Invalid(Invalid::MissingSuggestion),
            Refusal::NotTheOwner {
                stated: "@someone:example.com".to_owned(),
                owner: "@michel:example.com".to_owned(),
            },
            Refusal::SuggestionNotFound,
            Refusal::SuggestionOutOfReach { window: 20_000 },
            Refusal::TriggerNotFound {
                trigger_event_id: "b".repeat(64),
            },
            Refusal::TriggerOutOfReach {
                trigger_event_id: "b".repeat(64),
                window: 20_000,
            },
            Refusal::TriggerHasNoRoom {
                source: "matrix://x".to_owned(),
            },
            Refusal::Expired {
                expires_at: "2026-09-17T11:00:00Z".to_owned(),
                now: "2026-09-17T12:00:00.000Z".to_owned(),
            },
            Refusal::NeverConsented {
                label: State::Revoked,
            },
            Refusal::ConsentRevoked {
                contact: "@a:example.com".to_owned(),
                network: Network::Whatsapp,
            },
            Refusal::ConsentPending {
                contact: "@a:example.com".to_owned(),
                network: Network::Whatsapp,
            },
            Refusal::SuggestionUnreadable("an unknown network".to_owned()),
            Refusal::BusUnreachable("no answer".to_owned()),
            Refusal::StoreUnavailable("locked".to_owned()),
            Refusal::Unrecorded("disk full".to_owned()),
        ];
        let mut codes = std::collections::BTreeSet::new();
        for refusal in &refusals {
            assert!(
                codes.insert(refusal.code()),
                "{} shares a code with an earlier refusal: two situations sharing one signal is \
                 the defect this taxonomy exists to avoid",
                refusal.code()
            );
            assert!(
                !refusal.message().is_empty(),
                "{} says nothing",
                refusal.code()
            );
        }
        // The two consent refusals are the ones most likely to be collapsed
        // into one, and they are the two that must not be.
        assert_ne!(
            Refusal::ConsentRevoked {
                contact: "@a:example.com".to_owned(),
                network: Network::Whatsapp
            }
            .code(),
            Refusal::ConsentPending {
                contact: "@a:example.com".to_owned(),
                network: Network::Whatsapp
            }
            .code()
        );
        // And "gone" is not "never was".
        assert_ne!(
            Refusal::SuggestionNotFound.status(),
            Refusal::SuggestionOutOfReach { window: 1 }.status()
        );
        // The bus failing is not the Gateway being unconfigured: 502, not
        // 503, and never the 401 that told #141's client to refresh a
        // session that was fine.
        assert_eq!(
            Refusal::BusUnreachable("x".to_owned()).status(),
            axum::http::StatusCode::BAD_GATEWAY
        );
    }
}
