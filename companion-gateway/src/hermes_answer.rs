//! Hermes's answer, coming home (ticket #206, ADR 0032).
//!
//! A persona wakes Hermes — Nous Research's agent runtime, outside this
//! deployment — by posting a signed webhook, and Hermes's answer comes back
//! **here**, through the Gateway's own origin, and not by publishing to the
//! bus. The reason is one sentence: the reference bus has no authentication,
//! so "Hermes may publish" means "anything that can reach the bus may publish
//! a suggestion", which stops being theoretical the moment the two are
//! different machines. What arrives is turned into exactly the event a
//! persona would have published itself — `persona.suggest.produced.v1`, the
//! contract gains no type for "a suggestion that came from outside" — so the
//! approval screen, the listing and the approval path see one kind of
//! suggestion.
//!
//! # Who is allowed to say this
//!
//! The push is Hermes's own outbound-hook wire format, signed with
//! HMAC-SHA256 over the raw body as `X-Hermes-Signature-256: sha256=<hex>`,
//! with a secret that is **Twalk's** configuration and not Hermes's
//! (`GATEWAY_HERMES_ANSWER_SECRET`). Two things follow, and both are
//! deliberate.
//!
//! The signature authenticates the *sender* and not the content — Nous
//! Research's own documentation says so about the inbound direction, and it is
//! equally true here. So nothing in the body is trusted as an instruction or
//! as a capability: the reference it carries is checked against this
//! Gateway's own journal and against the bus before anything is published, and
//! a reference naming a message that does not exist, or a contact whose
//! consent is not `granted` at this moment, produces a refusal rather than a
//! suggestion about somebody else's conversation.
//!
//! And the body carries a timestamp and a delivery id **inside** the signed
//! bytes, which is the only replay protection this wire format has — it works
//! if the receiver checks it, so the receiver checks it
//! ([`CLOCK_SKEW_SECONDS`]). Beyond that, replay is answered structurally: the
//! suggestion's id is the contract's deterministic
//! `sha256(persona_id:trigger_event_id:attempt)`, so a repeated answer
//! republishes one event that the bus absorbs rather than a second draft of
//! one message.
//!
//! # What is read, and what is refused loudly
//!
//! The hook fires for every turn of the Hermes profile, so a turn the owner
//! typed themselves reaches this endpoint too. That is **ignored** with a
//! `200` and a counted reason, not refused: a run that was never a Twalk wake
//! is not an error, and an endpoint that answered `4xx` to it would fill an
//! operator's log with a failure that is not one. ADR 0032's separate profile
//! is what keeps that rare.
//!
//! The answer itself is a small JSON object — a reply, the language it is
//! written in, and the reference it belongs to — and every part of it is
//! required. **The language especially**: ADR 0031 has the persona compose the
//! disclosure in the language of the reply, and an answer with no language
//! would let that silently fall back to the user's own, which is how a French
//! disclosure ends up under an English reply with nothing anywhere to signal
//! it. So an answer with no language is **refused**, not defaulted: a refusal
//! is a `422` with its own code, a line in the log and a counted outcome,
//! while a default would be a guess nobody ever sees.
//!
//! # The one place this is provisional
//!
//! The language is carried onto the bus as a NATS **header**, because
//! `persona.suggest.produced.v1` has `additionalProperties: false` at both
//! levels and therefore has nowhere to put it. That is a real gap rather than
//! a tidy design: the header is invisible to `GET /api/suggestions`, so the
//! Companion cannot show it and the disclosure cannot yet read it. Closing it
//! means a field in the contract, which is a change of its own.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use tracing::{info, warn};

use crate::approval::{
    is_event_id, Approvals, Format, Refusal, TriggerEnvelope, MAX_BODY, SUGGEST_PRODUCED_TYPE,
};
use crate::metrics::Metrics;

/// The schema the event this module publishes validates against.
pub const SUGGEST_PRODUCED_DATASCHEMA: &str =
    "https://schemas.twalk.dev/cloudevents/v1/persona.suggest.produced.schema.json";

/// The path this module answers on. Under the Gateway's reserved `/_twalk/`
/// prefix, like the bridge status webhook and for the same reason: its caller
/// is not a browser, carries no device cookie, and verifies its own
/// credential.
pub const ANSWER_PATH: &str = "/_twalk/hermes/answers";

/// The header Hermes's outbound hook signs with: `sha256=<hex>` over the raw
/// body, GitHub's shape.
pub const SIGNATURE_HEADER: &str = "X-Hermes-Signature-256";
const SIGNATURE_PREFIX: &str = "sha256=";

/// The Hermes hook this endpoint expects, and the reason it is that one.
///
/// `transform_llm_output` carries the turn's answer, its session and the
/// platform, and **nothing else**. The obvious alternative, `post_llm_call`,
/// carries the same answer plus the whole `conversation_history` — which on
/// Hermes means `SOUL.md`, `MEMORY.md` and `USER.md` in the system prompt, so
/// the return path would ship Hermes's accumulated memory of the user back
/// into this process on every message. The seam is narrow in both directions
/// or it is narrow in neither.
pub const HOOK_EVENT: &str = "transform_llm_output";

/// The Hermes platform a Twalk wake runs under: its webhook adapter. Any
/// other platform is a turn somebody had with Hermes directly, which this
/// endpoint ignores rather than refuses.
pub const WEBHOOK_PLATFORM: &str = "webhook";

/// How far the push's own timestamp may be from this clock. Five minutes,
/// which is the window Hermes's *inbound* V2 signature check uses — the same
/// number in the other direction, so an operator has one fact to remember
/// about clock skew across the seam.
pub const CLOCK_SKEW_SECONDS: i64 = 300;

/// The most this endpoint will read. Small on purpose: the body is one reply
/// and three short fields, and a cap is what keeps a hook misconfigured onto
/// a fatter event from filling this process's memory instead of being refused.
pub const MAX_PUSH_BYTES: usize = 262_144;

/// The shortest acceptable shared secret, and the reason the Gateway refuses
/// to start with a shorter one: it is the whole authentication of a route that
/// creates suggestions about the user's conversations. Same length and same
/// argument as `GATEWAY_SERVICE_TOKEN`.
pub const MINIMUM_SECRET_LENGTH: usize = 32;

/// How long a suggestion this Gateway publishes stays approvable, when the
/// operator names no window. The SDK's own default (`twalk_sdk.policy`), kept
/// identical deliberately: a suggestion that came through Hermes must not age
/// differently from one a persona drafted itself, and a suggestion with no
/// expiry at all would be approvable for ever, which ticket #22 exists to
/// prevent.
pub const DEFAULT_SUGGESTION_TTL_SECONDS: u64 = 3600;

/// The token a persona puts in the wake, and Hermes copies into its answer.
const REFERENCE_PREFIX: &str = "TWALK-REF:";

// ---------------------------------------------------------------------------
// What arrives
// ---------------------------------------------------------------------------

/// Hermes's outbound-hook push, as much of it as this Gateway reads.
///
/// A type and not a `Value`, for the reason [`crate::contacts::InboundHeader`]
/// is a type: what the struct does not declare, the process does not hold.
/// Serde drops every other member as it parses, so a hook that grows a field —
/// or one an operator pointed at a fatter event — cannot leave that field
/// anywhere in this process.
#[derive(Debug, Deserialize)]
struct Push {
    hook_event_name: String,
    #[serde(default)]
    delivery_id: String,
    #[serde(default)]
    timestamp: String,
    extra: PushExtra,
}

#[derive(Debug, Deserialize)]
struct PushExtra {
    /// The turn's answer. What the model wrote, which is what this endpoint
    /// exists to receive.
    #[serde(default)]
    response_text: String,
    /// Hermes's own session key. Read only to be *logged* and to be checked
    /// against the reference when it happens to carry the delivery id: it is a
    /// cross-check, never the correlation, because a key's shape is Hermes's
    /// internal business and treating it as the contract would break on a
    /// Tuesday.
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    model: String,
}

/// The answer Hermes is asked to write: a reply, the language it is in, and
/// the reference it belongs to. Three required fields — see the module
/// docstring on why the language is not optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub reference: Reference,
    pub reply: String,
    pub language: String,
}

/// `TWALK-REF:<persona_id>:<trigger event id>:<attempt>` — the three facts the
/// suggestion's envelope is keyed on. Parsed and validated here; every one of
/// them is then checked against the bus or the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub persona_id: String,
    pub trigger_event_id: String,
    pub attempt: u64,
}

impl Reference {
    /// Finds and parses the reference in a string, wherever it sits in it.
    ///
    /// Tolerant of its surroundings on purpose: the reference travels through
    /// a prompt and comes back inside something a model wrote, so requiring it
    /// to be the whole value would turn a model that added a space into a lost
    /// suggestion. Its *shape* is not tolerated at all — a persona id the
    /// contract's pattern allows, 64 lowercase hex characters, an attempt of
    /// at least one — because the shape is what makes a wrong reference a
    /// refusal instead of a suggestion about the wrong message.
    pub fn find(haystack: &str) -> Option<Self> {
        let start = haystack.find(REFERENCE_PREFIX)?;
        let rest = &haystack[start + REFERENCE_PREFIX.len()..];
        let mut parts = rest.splitn(3, ':');
        let persona_id = parts.next()?;
        let trigger_event_id = parts.next()?;
        let attempt = parts.next()?;
        // The attempt runs to the first character that is not a digit: the
        // token is the tail of a sentence more often than not.
        let digits: String = attempt.chars().take_while(char::is_ascii_digit).collect();
        if persona_id.is_empty()
            || !persona_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
            || !is_event_id(trigger_event_id)
            || digits.is_empty()
        {
            return None;
        }
        let attempt: u64 = digits.parse().ok()?;
        if attempt < 1 {
            return None;
        }
        Some(Self {
            persona_id: persona_id.to_owned(),
            trigger_event_id: trigger_event_id.to_owned(),
            attempt,
        })
    }

    /// The contract's deterministic id for the suggestion this reference names:
    /// `sha256(persona_id:trigger_event_id:attempt)`, the same value
    /// `twalk_sdk.envelope.suggest_id` computes and the same value the persona
    /// used as Hermes's idempotency key. One key from the wake to the bus.
    pub fn suggestion_event_id(&self) -> String {
        use sha2::Digest;
        let mut hasher = Sha256::new();
        hasher.update(self.persona_id.as_bytes());
        hasher.update(b":");
        hasher.update(self.trigger_event_id.as_bytes());
        hasher.update(b":");
        hasher.update(self.attempt.to_string().as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

/// Parses the answer out of what the model wrote.
///
/// A fenced code block is unwrapped first, because a model asked for JSON
/// writes ```` ```json ```` in front of it often enough that refusing one
/// would make this seam look broken when it is working.
pub fn parse_answer(written: &str) -> Result<Answer, AnswerRefusal> {
    let text = unfence(written.trim());
    let value: Value = serde_json::from_str(text).map_err(|error| {
        AnswerRefusal::Unreadable(format!(
            "the answer is not the JSON object the route asks Hermes for \
             ({{\"reference\", \"reply\", \"language\"}}): {error}"
        ))
    })?;
    let object = value.as_object().ok_or_else(|| {
        AnswerRefusal::Unreadable("the answer is JSON but not an object".to_owned())
    })?;
    let reference = object
        .get("reference")
        .and_then(Value::as_str)
        .and_then(Reference::find)
        // Falling back to the whole answer rather than only the member: a
        // model that put the token in the reply instead of in its own field
        // has still told us which message this is, and losing a suggestion
        // over the placement of a token nobody reads would be absurd.
        .or_else(|| Reference::find(text))
        .ok_or(AnswerRefusal::NoReference)?;
    let language = object
        .get("language")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_owned();
    if language.is_empty() {
        return Err(AnswerRefusal::NoLanguage);
    }
    if !is_language_tag(&language) {
        return Err(AnswerRefusal::LanguageUnreadable(language));
    }
    let reply = object
        .get("reply")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_owned();
    if reply.is_empty() {
        return Err(AnswerRefusal::EmptyReply);
    }
    if reply.chars().count() > MAX_BODY {
        return Err(AnswerRefusal::ReplyTooLong(reply.chars().count()));
    }
    Ok(Answer {
        reference,
        reply,
        language,
    })
}

fn unfence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let rest = rest.split_once('\n').map(|(_tag, body)| body).unwrap_or("");
    rest.trim_end()
        .strip_suffix("```")
        .unwrap_or(rest)
        .trim()
}

/// Whether a string is a language tag in the shape ADR 0016 needs: a
/// primary subtag of two or three letters, optionally refined.
///
/// Deliberately **not** the Companion's closed list of five: that list is the
/// languages the *interface* ships and the language the user falls back to,
/// while this is the language of a reply to a contact, who may write in any
/// language at all (ADR 0016 — a suggestion follows the conversation and never
/// the user). So the shape is checked and the value is not.
pub fn is_language_tag(value: &str) -> bool {
    let mut subtags = value.split('-');
    let Some(primary) = subtags.next() else {
        return false;
    };
    if !(2..=3).contains(&primary.len()) || !primary.bytes().all(|b| b.is_ascii_lowercase()) {
        return false;
    }
    subtags.all(|subtag| {
        (1..=8).contains(&subtag.len()) && subtag.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

// ---------------------------------------------------------------------------
// The refusals
// ---------------------------------------------------------------------------

/// Why an answer did not become a suggestion.
///
/// The variants that are about a fact the approval path also establishes — the
/// trigger, the consent state, the bus — are [`AnswerRefusal::Shared`] and
/// delegate their code and status to [`Refusal`]. Two doors onto one fact must
/// not teach a client two vocabularies, which is why this enum wraps that one
/// rather than restating it.
#[derive(Debug)]
pub enum AnswerRefusal {
    /// No signature, one that cannot be read, or one that is not this
    /// Gateway's.
    Unauthenticated,
    /// The push is not the JSON Hermes's outbound hook sends.
    Malformed(String),
    /// The push's own timestamp is outside [`CLOCK_SKEW_SECONDS`]. Its own
    /// answer, because a well-formed and correctly signed push that is hours
    /// old is a replay and not a mistake.
    Stale { timestamp: String },
    /// The answer is not the JSON object the route asks Hermes to write.
    Unreadable(String),
    /// No `TWALK-REF:` token anywhere in the answer, so there is no telling
    /// which message it answers.
    NoReference,
    /// The answer names no language. See the module docstring: refused, and
    /// deliberately not defaulted.
    NoLanguage,
    LanguageUnreadable(String),
    EmptyReply,
    ReplyTooLong(usize),
    /// A fact the approval path establishes the same way.
    Shared(Refusal),
}

impl From<Refusal> for AnswerRefusal {
    fn from(refusal: Refusal) -> Self {
        AnswerRefusal::Shared(refusal)
    }
}

impl AnswerRefusal {
    pub fn code(&self) -> &'static str {
        match self {
            AnswerRefusal::Unauthenticated => "unauthenticated",
            AnswerRefusal::Malformed(_) => "invalid_request",
            AnswerRefusal::Stale { .. } => "hermes_answer_stale",
            AnswerRefusal::Unreadable(_) => "hermes_answer_unreadable",
            AnswerRefusal::NoReference => "hermes_answer_has_no_reference",
            AnswerRefusal::NoLanguage => "hermes_answer_has_no_language",
            AnswerRefusal::LanguageUnreadable(_) => "hermes_answer_language_unreadable",
            AnswerRefusal::EmptyReply => "hermes_answer_is_empty",
            AnswerRefusal::ReplyTooLong(_) => "hermes_answer_too_long",
            AnswerRefusal::Shared(refusal) => refusal.code(),
        }
    }

    /// The status. `422` for the four facts about the *answer*, which is the
    /// distinction worth keeping: a `400` says the request was malformed and
    /// these requests are not — they are well-formed, correctly signed pushes
    /// carrying an answer this Gateway cannot turn into a suggestion, and an
    /// operator debugging the two needs to tell them apart.
    pub fn status(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode;
        match self {
            AnswerRefusal::Unauthenticated => StatusCode::UNAUTHORIZED,
            AnswerRefusal::Malformed(_) | AnswerRefusal::Stale { .. } => StatusCode::BAD_REQUEST,
            AnswerRefusal::Unreadable(_)
            | AnswerRefusal::NoReference
            | AnswerRefusal::NoLanguage
            | AnswerRefusal::LanguageUnreadable(_)
            | AnswerRefusal::EmptyReply
            | AnswerRefusal::ReplyTooLong(_) => StatusCode::UNPROCESSABLE_ENTITY,
            AnswerRefusal::Shared(refusal) => refusal.status(),
        }
    }

    pub fn message(&self) -> String {
        match self {
            AnswerRefusal::Unauthenticated => format!(
                "the push carries no usable {SIGNATURE_HEADER}. It is HMAC-SHA256 over the raw \
                 body as sha256=<hex>, with the secret in GATEWAY_HERMES_ANSWER_SECRET here and \
                 in the hook's own secret_env on the Hermes side."
            ),
            AnswerRefusal::Malformed(detail) => detail.clone(),
            AnswerRefusal::Stale { timestamp } => format!(
                "the push's own timestamp is {timestamp:?}, which is more than \
                 {CLOCK_SKEW_SECONDS}s from this clock. That timestamp is inside the signed body \
                 and is this wire format's only replay protection, so it is checked rather than \
                 read: either the two hosts' clocks disagree, or this is a replay."
            ),
            AnswerRefusal::Unreadable(detail) => detail.clone(),
            AnswerRefusal::NoReference => format!(
                "the answer carries no {REFERENCE_PREFIX} token, so there is no telling which \
                 message it answers. The route's prompt template hands Hermes the reference and \
                 asks for it back verbatim; a model that dropped it has produced an answer this \
                 Gateway cannot attribute, and attributing it by guesswork would mean drafting a \
                 reply into somebody else's conversation."
            ),
            AnswerRefusal::NoLanguage => {
                "the answer names no language, and it is refused rather than defaulted. A reply's \
                 disclosure is written in the language of the reply and not the user's \
                 (ADR 0031), so an answer with no language would make that fall back silently — \
                 a French disclosure under an English reply, with nothing anywhere to say so. The \
                 route's prompt asks Hermes for a language member on every answer."
                    .to_owned()
            }
            AnswerRefusal::LanguageUnreadable(value) => format!(
                "the answer names the language {value:?}, which is not a language tag (two or \
                 three lowercase letters, optionally refined: fr, en, pt-BR). Any language is \
                 allowed — a suggestion follows the conversation and not the user (ADR 0016) — but \
                 it has to be a tag."
            ),
            AnswerRefusal::EmptyReply => {
                "the answer's reply is empty. An empty suggestion is a row on the approval screen \
                 with nothing in it, which is worse than no suggestion at all."
                    .to_owned()
            }
            AnswerRefusal::ReplyTooLong(length) => format!(
                "the answer's reply is {length} characters and the contract's limit is {MAX_BODY}"
            ),
            AnswerRefusal::Shared(refusal) => refusal.message(),
        }
    }
}

// ---------------------------------------------------------------------------
// The act
// ---------------------------------------------------------------------------

/// What happened to one push.
#[derive(Debug)]
pub enum Received {
    /// The answer became a suggestion, and here is where it landed.
    Published {
        suggestion_event_id: String,
        language: String,
        stream_sequence: u64,
    },
    /// Not a Twalk wake. Counted and answered `200`: see the module docstring.
    Ignored { reason: &'static str },
}

/// The answer half of the Gateway.
///
/// It holds no store of its own: the trigger lookup, the consent read, the
/// refusal vocabulary and the publish are [`Approvals`]', which is also why
/// this half exists only on a deployment that has consent configured — the
/// same store and the same bus.
pub struct Answers {
    approvals: Arc<Approvals>,
    metrics: Arc<Metrics>,
    /// The shared secret, held to verify and for nothing else.
    secret: String,
    /// The authority of the `source` URI this Gateway writes onto a
    /// suggestion: `hermes://<domain>/personas/<persona id>`.
    ///
    /// **Configuration, never built from the request.** The reference names a
    /// persona and a signed push is not a licence to name a source, so the
    /// domain half is the operator's (`GATEWAY_HERMES_DOMAIN`, the same value
    /// the persona runtime publishes under) and only the persona id comes from
    /// the answer — where it is checked against the contract's pattern.
    hermes_domain: String,
    suggestion_ttl: Duration,
    now: fn() -> SystemTime,
}

impl Answers {
    pub fn new(
        approvals: Arc<Approvals>,
        metrics: Arc<Metrics>,
        secret: String,
        hermes_domain: String,
        suggestion_ttl_seconds: u64,
        now: fn() -> SystemTime,
    ) -> Self {
        Self {
            approvals,
            metrics,
            secret,
            hermes_domain,
            suggestion_ttl: Duration::from_secs(suggestion_ttl_seconds),
            now,
        }
    }

    /// Whether this signature is one this Gateway's secret produces over these
    /// bytes. Constant-time, through the `hmac` crate's own verification.
    pub fn authenticates(&self, presented: Option<&str>, body: &[u8]) -> bool {
        let Some(presented) = presented.and_then(|value| value.trim().strip_prefix(SIGNATURE_PREFIX))
        else {
            return false;
        };
        let Ok(expected) = hex_bytes(presented) else {
            return false;
        };
        let mut mac = Hmac::<Sha256>::new_from_slice(self.secret.as_bytes())
            .expect("HMAC accepts a key of any length");
        mac.update(body);
        mac.verify_slice(&expected).is_ok()
    }

    /// The whole act: authenticate the sender, read the answer, check every
    /// fact in it against this deployment, and publish the suggestion.
    ///
    /// The order is the point. Nothing about the body is looked at before the
    /// signature; the reference is validated for shape before the bus is
    /// asked anything; and the contact's consent is read **after** the trigger
    /// is found and **before** anything is published, so a contact the user
    /// revoked while Hermes was reasoning gets no draft on the approval
    /// screen.
    pub async fn receive(
        &self,
        body: &[u8],
        signature: Option<&str>,
    ) -> Result<Received, AnswerRefusal> {
        if !self.authenticates(signature, body) {
            return Err(AnswerRefusal::Unauthenticated);
        }
        let push: Push = serde_json::from_slice(body).map_err(|error| {
            AnswerRefusal::Malformed(format!(
                "the push is not one of Hermes's outbound-hook deliveries \
                 (hook_event_name, extra.response_text): {error}"
            ))
        })?;
        if push.hook_event_name != HOOK_EVENT {
            return Ok(self.ignored("not_our_hook", &push));
        }
        if push.extra.platform != WEBHOOK_PLATFORM {
            // A turn somebody had with Hermes directly, on the same profile.
            return Ok(self.ignored("not_a_webhook_run", &push));
        }
        self.fresh(&push.timestamp)?;

        let answer = parse_answer(&push.extra.response_text)?;
        let trigger = self
            .approvals
            .trigger_envelope(&answer.reference.trigger_event_id)
            .await?;
        self.approvals
            .consent_now(&trigger.contact, trigger.network)?;

        let suggestion_event_id = answer.reference.suggestion_event_id();
        let produced_at = (self.now)();
        let envelope = self.envelope(&answer, &trigger, &suggestion_event_id, produced_at);
        let mut extensions = vec![
            ("network", trigger.network.as_str()),
            ("connection", trigger.connection.as_str()),
            ("consent", trigger.consent_label.as_str()),
            // The provisional carrier for the one fact the contract has no
            // field for — see the module docstring.
            ("language", answer.language.as_str()),
        ];
        if let Some(traceparent) = &trigger.traceparent {
            extensions.push(("traceparent", traceparent.as_str()));
        }
        let stream_sequence = self
            .approvals
            .publish_envelope(
                SUGGEST_PRODUCED_TYPE,
                &suggestion_event_id,
                &envelope,
                &extensions,
            )
            .await
            .map_err(|error| {
                warn!(%error, %suggestion_event_id, "a suggestion from Hermes could not be published");
                AnswerRefusal::Shared(Refusal::BusUnreachable(format!("{error:#}")))
            })?;

        self.metrics.record_hermes_answer("published");
        info!(
            suggestion = %suggestion_event_id,
            trigger = %trigger.event_id,
            persona = %answer.reference.persona_id,
            network = trigger.network.as_str(),
            language = %answer.language,
            model = %push.extra.model,
            session = %push.extra.session_id,
            delivery = %push.delivery_id,
            stream_sequence,
            "hermes answered: the suggestion is on the bus"
        );
        Ok(Received::Published {
            suggestion_event_id,
            language: answer.language,
            stream_sequence,
        })
    }

    fn ignored(&self, reason: &'static str, push: &Push) -> Received {
        self.metrics.record_hermes_answer(reason);
        info!(
            hook = %push.hook_event_name,
            platform = %push.extra.platform,
            reason,
            "a Hermes push was not a Twalk wake and was ignored"
        );
        Received::Ignored { reason }
    }

    fn fresh(&self, timestamp: &str) -> Result<(), AnswerRefusal> {
        let stale = || AnswerRefusal::Stale {
            timestamp: timestamp.to_owned(),
        };
        let sent = parse_rfc3339_seconds(timestamp).ok_or_else(stale)?;
        let now = (self.now)()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_secs() as i64)
            .unwrap_or_default();
        if (now - sent).abs() > CLOCK_SKEW_SECONDS {
            return Err(stale());
        }
        Ok(())
    }

    /// The suggestion, exactly as a persona's own SDK would have built it.
    ///
    /// Every attribute but the body comes from the trigger or from
    /// configuration: the id from the contract's natural key, the `subject`
    /// and `trigger` reference from the message being answered, the `network`,
    /// `consent` and trace copied from it, and the `source` from the persona
    /// id plus this Gateway's configured Hermes domain. So a consumer cannot
    /// tell — and must not need to tell — that this one came from outside.
    fn envelope(
        &self,
        answer: &Answer,
        trigger: &TriggerEnvelope,
        suggestion_event_id: &str,
        produced_at: SystemTime,
    ) -> Value {
        let produced = rfc3339_seconds(produced_at);
        let expires_at = rfc3339_seconds(produced_at + self.suggestion_ttl);
        let mut event = json!({
            "specversion": "1.0",
            "id": suggestion_event_id,
            "source": format!(
                "hermes://{}/personas/{}", self.hermes_domain, answer.reference.persona_id
            ),
            "type": SUGGEST_PRODUCED_TYPE,
            "time": produced,
            "subject": trigger.event_id,
            "datacontenttype": "application/json",
            "dataschema": SUGGEST_PRODUCED_DATASCHEMA,
            "network": trigger.network.as_str(),
            "connection": trigger.connection,
            "consent": trigger.consent_label.as_str(),
            "data": {
                "persona_id": answer.reference.persona_id,
                "trigger": {
                    "event_id": trigger.event_id,
                    "event_type": trigger.event_type,
                },
                "suggestion": {
                    "body": answer.reply,
                    "format": Format::Plain.as_str(),
                },
                "attempt": answer.reference.attempt,
                "expires_at": expires_at,
            }
        });
        if let Some(traceparent) = &trigger.traceparent {
            event["traceparent"] = Value::String(traceparent.clone());
        }
        event
    }
}

fn hex_bytes(value: &str) -> Result<Vec<u8>, ()> {
    if value.len() % 2 != 0 {
        return Err(());
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).map_err(|_| ()))
        .collect()
}

/// The contract's `date-time`, to the second, in UTC — the same precision the
/// SDK writes a suggestion's `time` and `expires_at` with.
fn rfc3339_seconds(at: SystemTime) -> String {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();
    let days = (seconds / 86_400) as i64;
    let time_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time_of_day / 3600,
        (time_of_day % 3600) / 60,
        time_of_day % 60
    )
}

/// Unix seconds out of an RFC 3339 instant, or `None`.
///
/// Hand-rolled, and the reason is the crate's own: the Gateway carries no date
/// library, formats its own timestamps in [`crate::consent::rfc3339_millis`],
/// and needs exactly one thing here — whether a push is minutes old or hours.
/// A fractional part and an offset are both accepted, because the sender's
/// formatting is not this endpoint's contract.
fn parse_rfc3339_seconds(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let number = |from: usize, to: usize| value.get(from..to)?.parse::<i64>().ok();
    let year = number(0, 4)?;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut instant =
        days_from_civil(year, month as u32, day as u32) * 86_400 + hour * 3600 + minute * 60 + second;
    // The offset, when there is one. `Z` and a missing offset are both UTC.
    let tail = &value[19..];
    let offset_at = tail.find(['+', '-']);
    if let Some(at) = offset_at {
        let offset = &tail[at..];
        let sign = if offset.starts_with('-') { 1 } else { -1 };
        let hours: i64 = offset.get(1..3)?.parse().ok()?;
        let minutes: i64 = offset.get(4..6).unwrap_or("0").parse().unwrap_or(0);
        instant += sign * (hours * 3600 + minutes * 60);
    }
    Some(instant)
}

/// Howard Hinnant's `days_from_civil`, the standard proleptic-Gregorian
/// conversion, and its inverse below. Twelve lines each and no dependency.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i64;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIGGER: &str = "20be32e73506b9104a6a1bf76fc2d2a15cbd2b8a0a421833be01f865ca9886d0";

    #[test]
    fn a_reference_is_found_wherever_a_model_put_it() {
        let expected = Reference {
            persona_id: "assistant".to_owned(),
            trigger_event_id: TRIGGER.to_owned(),
            attempt: 1,
        };
        for haystack in [
            format!("TWALK-REF:assistant:{TRIGGER}:1"),
            format!("the reference is TWALK-REF:assistant:{TRIGGER}:1."),
            format!("  TWALK-REF:assistant:{TRIGGER}:1  "),
        ] {
            assert_eq!(Reference::find(&haystack), Some(expected.clone()), "{haystack}");
        }
    }

    #[test]
    fn a_reference_of_the_wrong_shape_is_no_reference() {
        for haystack in [
            "TWALK-REF:assistant:not-an-event-id:1",
            "TWALK-REF::0000:1",
            &format!("TWALK-REF:Assistant:{TRIGGER}:1"),
            &format!("TWALK-REF:assistant:{TRIGGER}:0"),
            &format!("TWALK-REF:assistant:{TRIGGER}:x"),
            &format!("TWALK-REF:assistant:{}:1", &TRIGGER[..63]),
            "nothing here",
        ] {
            assert!(Reference::find(haystack).is_none(), "{haystack}");
        }
    }

    #[test]
    fn the_suggestions_id_is_the_contracts_own() {
        // `sha256("assistant:<trigger>:1")` — the value the contract's own
        // fixture carries for this trigger, which is what makes a suggestion
        // published here indistinguishable from one the SDK published.
        let reference = Reference::find(&format!("TWALK-REF:assistant:{TRIGGER}:1")).unwrap();
        assert_eq!(
            reference.suggestion_event_id(),
            "319be8ff15d5dee005c8aa27119b983da8223959987e5dbc639d81e370b5ef9b"
        );
    }

    #[test]
    fn an_answer_is_a_reply_a_language_and_a_reference() {
        let answer = parse_answer(&format!(
            r#"{{"reference":"TWALK-REF:assistant:{TRIGGER}:1","reply":"D'accord, à 20h !","language":"fr"}}"#
        ))
        .expect("the route asks Hermes for exactly this");
        assert_eq!(answer.reply, "D'accord, à 20h !");
        assert_eq!(answer.language, "fr");
        assert_eq!(answer.reference.attempt, 1);
    }

    #[test]
    fn a_fenced_answer_is_unwrapped() {
        let answer = parse_answer(&format!(
            "```json\n{{\"reference\":\"TWALK-REF:assistant:{TRIGGER}:1\",\"reply\":\"ok\",\"language\":\"en\"}}\n```"
        ))
        .expect("a model that fences its JSON has still answered");
        assert_eq!(answer.reply, "ok");
    }

    #[test]
    fn an_answer_with_no_language_is_refused_and_not_defaulted() {
        let refusal = parse_answer(&format!(
            r#"{{"reference":"TWALK-REF:assistant:{TRIGGER}:1","reply":"ok"}}"#
        ))
        .expect_err("ADR 0031's disclosure would fall back silently");
        assert_eq!(refusal.code(), "hermes_answer_has_no_language");
        assert_eq!(
            refusal.status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        );
        assert!(refusal.message().contains("ADR 0031"));
    }

    #[test]
    fn a_language_that_is_not_a_tag_is_its_own_refusal() {
        let refusal = parse_answer(&format!(
            r#"{{"reference":"TWALK-REF:assistant:{TRIGGER}:1","reply":"ok","language":"French"}}"#
        ))
        .expect_err("a language name is not a language tag");
        assert_eq!(refusal.code(), "hermes_answer_language_unreadable");
    }

    #[test]
    fn any_language_is_allowed_and_only_the_shape_is_checked() {
        // A suggestion follows the conversation (ADR 0016), so the five the
        // Companion ships are not the limit.
        for tag in ["fr", "en", "ar", "zh", "pt-BR", "sr-Latn-RS"] {
            assert!(is_language_tag(tag), "{tag}");
        }
        for not_a_tag in ["", "f", "FR", "français", "fr_FR", "fr-", "toolongsubtag"] {
            assert!(!is_language_tag(not_a_tag), "{not_a_tag}");
        }
    }

    #[test]
    fn an_answer_with_no_reference_cannot_be_attributed() {
        let refusal = parse_answer(r#"{"reply":"ok","language":"en"}"#)
            .expect_err("attributing it by guesswork would draft into another conversation");
        assert_eq!(refusal.code(), "hermes_answer_has_no_reference");
    }

    #[test]
    fn an_empty_reply_is_refused() {
        let refusal = parse_answer(&format!(
            r#"{{"reference":"TWALK-REF:assistant:{TRIGGER}:1","reply":"   ","language":"en"}}"#
        ))
        .expect_err("an empty row on the approval screen is worse than none");
        assert_eq!(refusal.code(), "hermes_answer_is_empty");
    }

    #[test]
    fn prose_instead_of_json_is_unreadable_and_says_what_was_wanted() {
        let refusal = parse_answer("Sure! Here is a reply: see you at 8.")
            .expect_err("the route asks for a JSON object");
        assert_eq!(refusal.code(), "hermes_answer_unreadable");
        assert!(refusal.message().contains("language"));
    }

    #[test]
    fn a_shared_fact_keeps_the_approval_paths_own_code_and_status() {
        let refusal: AnswerRefusal = Refusal::TriggerNotFound {
            trigger_event_id: TRIGGER.to_owned(),
        }
        .into();
        assert_eq!(refusal.code(), "trigger_not_found");
        assert_eq!(refusal.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn timestamps_round_trip_through_the_hand_rolled_conversion() {
        for (text, seconds) in [
            ("1970-01-01T00:00:00Z", 0),
            ("2026-09-19T08:32:06Z", 1_789_806_726),
            ("2026-09-19T08:32:06.123456Z", 1_789_806_726),
            ("2026-09-19T10:32:06+02:00", 1_789_806_726),
        ] {
            assert_eq!(parse_rfc3339_seconds(text), Some(seconds), "{text}");
        }
        assert_eq!(
            rfc3339_seconds(UNIX_EPOCH + Duration::from_secs(1_789_806_726)),
            "2026-09-19T08:32:06Z"
        );
        assert!(parse_rfc3339_seconds("yesterday").is_none());
    }
}
