//! Ticket #24, the approval API at the Gateway's process boundary: the real
//! binary, the real NATS JetStream a persona publishes suggestions to, real
//! contract events, and HTTP calls from the test.
//!
//! `CONTEXT.md` defines the act being tested, and the definition is the test
//! plan:
//!
//! > The human act that turns a suggestion into an outbound reply, carrying
//! > the identity of whoever approved it. Deliberate by construction — an
//! > explicit call, never a default, never a batch — and refused if the
//! > sender's consent is no longer `granted` at that moment.
//!
//! So: a valid approval publishes a schema-valid `persona.reply.approved.v1`
//! and says where it landed; the edited content wins over the persona's own
//! — and since ticket #121 both go out with the disclosure the suggestion
//! carried appended on a line of its own, while `edited` stays about the
//! body alone and a body that leaves no room for the line is refused with
//! the line named; an approval is never a batch and never under another name; and the three
//! refusals that matter are asserted **separately**, because two failures
//! sharing one signal is what cost this project seven incidents in two days
//! (#116, #141):
//!
//! - approving an **expired** suggestion — `409 suggestion_expired`;
//! - approving one whose sender's consent was **revoked after it was
//!   produced** — `409 consent_revoked`. This is the case the definition's
//!   "at that moment" exists for, and the one that would pass on a
//!   label-only check: the suggestion's own `consent` extension still says
//!   `granted`, because that is what the Sensor observed when the message
//!   arrived;
//! - approving **twice** — `409 already_approved`, carrying the first
//!   approval so a client that lost an answer learns where its reply went.
//!
//! Every refusal is asserted to publish **nothing**, through a core-NATS
//! subscription that sees every publish including one the stream would
//! deduplicate — the harness's `subscribe_raw`, which is how this repository
//! proves a component never emitted something rather than trusting the bus to
//! absorb it.
//!
//! The suggestions here are published by the test rather than by a persona:
//! the Gateway's seam is the bus, and what crosses it is a contract-valid
//! CloudEvent, which every event below is checked to be. The persona's own
//! half is `hermes/tests/suggestion.rs`.
//!
//! The bus is shared by every suite and every run, so nothing here asserts on
//! totals: each test invents its own contact, its own room and its own
//! suggestion, and asks about those.

mod harness;

use std::path::PathBuf;

use anyhow::{Context, Result};
use harness::{
    companion_build, ensure_stack, gateway_env_with, gateway_env_with_consent, nats_url,
    owner_user_id, poll_until, unreachable_nats_url, validate_against_contract, Bus, GatewayProc,
    SERVER_NAME,
};
use serde_json::{json, Value};

const STREAM: &str = "twalk";
const INBOUND_SUBJECT: &str = "twalk.inbound.message.received.v1";
const INBOUND_TYPE: &str = "fr.linagora.twalk.inbound.message.received.v1";
const SUGGEST_SUBJECT: &str = "twalk.persona.suggest.produced.v1";
const SUGGEST_TYPE: &str = "fr.linagora.twalk.persona.suggest.produced.v1";
const APPROVED_SUBJECT: &str = "twalk.persona.reply.approved.v1";

/// The traceparent every fixture here carries, so the assertion that the
/// approval continues the suggestion's trace is about a known value.
const TRACEPARENT: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

/// The sentence every suggestion here carries as `data.disclosure` (ticket
/// #121): the contract's French one, because the suggestions are French. The
/// persona selected it; the Gateway appends it.
const DISCLOSURE: &str = "Rédigé avec mon assistant IA.";

// ---------------------------------------------------------------------------
// Driving the seam
// ---------------------------------------------------------------------------

fn unique(label: &str) -> String {
    format!(
        "{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos()
    )
}

/// A bridged ghost's Matrix ID, as a bridge materialises one.
fn ghost(label: &str) -> String {
    format!("@whatsapp_{}:{SERVER_NAME}", unique(label))
}

/// A portal room id of this run's own.
fn portal_room(label: &str) -> String {
    let opaque: String = harness::sha256_hex(&unique(label))
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(18)
        .collect();
    format!("!{opaque}:{SERVER_NAME}")
}

fn rfc3339(at: time::OffsetDateTime) -> String {
    at.replace_nanosecond(0)
        .expect("a whole second is a valid instant")
        .format(&time::format_description::well_known::Rfc3339)
        .expect("an instant formats as RFC 3339")
}

fn in_seconds(seconds: i64) -> String {
    rfc3339(time::OffsetDateTime::now_utc() + time::Duration::seconds(seconds))
}

/// One `inbound.message.received.v1` as the Sensor publishes it: the message
/// a suggestion answers, and the only place the portal room and the sender
/// are named.
/// A revoked sender's message is **reduced**: the contract forbids the body
/// outright (ADR 0012), so the fixture has to reduce exactly as the Sensor
/// would — otherwise this file would be asserting against an event no
/// producer could publish.
fn inbound_event(sender: &str, room_id: &str, consent: &str) -> Value {
    let at = in_seconds(-120);
    let data = if consent == "revoked" {
        json!({
            "format": "text/plain",
            "reply_to": null,
            "attachments": [],
            "contact": { "display_name": "Aicha Benali G24" }
        })
    } else {
        json!({
            "body": "On décale à 20h ?",
            "format": "text/plain",
            "reply_to": null,
            "attachments": [],
            "contact": { "display_name": "Aicha Benali G24" }
        })
    };
    let event = json!({
        "specversion": "1.0",
        "id": harness::sha256_hex(&format!("g24-inbound:{sender}:{room_id}:{at}")),
        "source": format!("matrix://{SERVER_NAME}/{room_id}"),
        "type": INBOUND_TYPE,
        "time": at,
        "subject": sender,
        "datacontenttype": "application/json",
        "dataschema": "https://schemas.twalk.dev/cloudevents/v1/inbound.message.received.schema.json",
        "traceparent": TRACEPARENT,
        "network": "whatsapp",
        "connection": "whatsapp",
        "consent": consent,
        "data": data
    });
    validate_against_contract(&event, "inbound.message.received")
        .expect("the fixture is an event the contract allows");
    event
}

/// One `persona.suggest.produced.v1` as the assistant publishes it, with the
/// expiry ticket #22's policy always sets and the disclosure #121's SDK
/// always selects.
fn suggest_event(trigger: &Value, body: &str, expires_at: &str) -> Value {
    let trigger_id = trigger["id"].as_str().expect("the trigger has an id");
    let attempt = 1;
    let event = json!({
        "specversion": "1.0",
        "id": harness::sha256_hex(&format!("assistant:{trigger_id}:{attempt}")),
        "source": format!("hermes://{SERVER_NAME}/personas/assistant"),
        "type": SUGGEST_TYPE,
        "time": in_seconds(-60),
        "subject": trigger_id,
        "datacontenttype": "application/json",
        "dataschema": "https://schemas.twalk.dev/cloudevents/v1/persona.suggest.produced.schema.json",
        "traceparent": TRACEPARENT,
        "network": trigger["network"],
        "connection": trigger["connection"],
        // The label the Sensor observed, copied from the trigger. It stays
        // `granted` after a revocation: that is exactly why a label check is
        // not a consent check.
        "consent": trigger["consent"],
        "data": {
            "persona_id": "assistant",
            "trigger": { "event_id": trigger_id, "event_type": INBOUND_TYPE },
            "suggestion": { "body": body, "format": "text/plain" },
            "disclosure": DISCLOSURE,
            "attempt": attempt,
            "expires_at": expires_at
        }
    });
    validate_against_contract(&event, "persona.suggest.produced")
        .expect("the fixture is an event the contract allows");
    event
}

async fn bus() -> Result<Bus> {
    let bus = Bus::connect().await?;
    bus.ensure_stream(STREAM, &["twalk.>"]).await?;
    Ok(bus)
}

/// A Gateway with a bus and a consent store, signed in.
struct Running {
    #[allow(dead_code)]
    gateway: GatewayProc,
    base: String,
    static_dir: PathBuf,
    device: String,
    http: reqwest::Client,
}

impl Running {
    async fn start(test_name: &str) -> Result<Self> {
        let static_dir = companion_build(test_name)?;
        let env = gateway_env_with_consent(&static_dir, &nats_url());
        Self::start_with(static_dir, env).await
    }

    /// The same, with the environment the caller wants: the lookup window is
    /// what several tests move, because the bound on the bus read is the
    /// difference between `suggestion_not_found` and
    /// `suggestion_out_of_reach`.
    async fn start_with(static_dir: PathBuf, env: Vec<(String, String)>) -> Result<Self> {
        let gateway = GatewayProc::start(&env)?;
        let base = gateway.base_url().await?;
        poll_until(
            || async {
                reqwest::get(format!("{base}/health"))
                    .await
                    .ok()?
                    .error_for_status()
                    .ok()
            },
            "the gateway health endpoint",
        )
        .await?;
        let device = harness::signed_in_device_token(&base).await?;
        Ok(Self {
            gateway,
            base,
            static_dir,
            device,
            http: reqwest::Client::new(),
        })
    }

    fn cookie(&self) -> String {
        format!("twalk_device={}", self.device)
    }

    async fn post(&self, path: &str, body: &Value) -> Result<(reqwest::StatusCode, Value)> {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .header(reqwest::header::COOKIE, self.cookie())
            .json(body)
            .send()
            .await
            .with_context(|| format!("failed to call POST {path}"))?;
        let status = response.status();
        let text = response.text().await?;
        let body = serde_json::from_str(&text)
            .with_context(|| format!("POST {path} answered {status} with non-JSON: {text}"))?;
        Ok((status, body))
    }

    async fn get(&self, path: &str) -> Result<(reqwest::StatusCode, Value)> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .header(reqwest::header::COOKIE, self.cookie())
            .send()
            .await
            .with_context(|| format!("failed to call GET {path}"))?;
        let status = response.status();
        let text = response.text().await?;
        let body = serde_json::from_str(&text)
            .with_context(|| format!("GET {path} answered {status} with non-JSON: {text}"))?;
        Ok((status, body))
    }

    /// Records one consent decision the way the user does — through #49's
    /// write API, which is the only writer of consent state.
    async fn decide(&self, contact: &str, new_state: &str) -> Result<()> {
        let (status, body) = self
            .post(
                "/api/consent/decisions",
                &json!({
                    "subject": { "type": "contact", "id": contact },
                    "new_state": new_state,
                    "scope": { "networks": ["whatsapp"] }
                }),
            )
            .await?;
        anyhow::ensure!(
            status == reqwest::StatusCode::CREATED,
            "the decision was refused with {status}: {body}"
        );
        Ok(())
    }

    async fn approve(&self, body: &Value) -> Result<(reqwest::StatusCode, Value)> {
        self.post("/api/approvals", body).await
    }

    fn state_dir(&self) -> PathBuf {
        harness::gateway_state_dir(&self.static_dir)
    }
}

/// A contact whose consent is granted, with the message a persona answered
/// and the suggestion it produced already on the bus.
struct Conversation {
    contact: String,
    room_id: String,
    suggestion_id: String,
    suggestion_body: String,
}

async fn conversation(
    running: &Running,
    bus: &Bus,
    label: &str,
    consent_label: &str,
    expires_at: &str,
) -> Result<Conversation> {
    let contact = ghost(label);
    let room_id = portal_room(label);
    running.decide(&contact, "granted").await?;
    let trigger = inbound_event(&contact, &room_id, consent_label);
    bus.publish_event(INBOUND_SUBJECT, &trigger).await?;
    let body = format!("Pas de problème, à 20h ! ({label})");
    let suggestion = suggest_event(&trigger, &body, expires_at);
    bus.publish_event(SUGGEST_SUBJECT, &suggestion).await?;
    Ok(Conversation {
        contact,
        room_id,
        suggestion_id: suggestion["id"].as_str().unwrap().to_owned(),
        suggestion_body: body,
    })
}

/// The `persona.reply.approved.v1` the bus stored at a position, with its
/// NATS headers.
async fn stored_reply(bus: &Bus, sequence: u64) -> Result<harness::StoredMessage> {
    let messages = bus
        .consume_from(STREAM, APPROVED_SUBJECT, sequence, 1)
        .await?;
    messages
        .into_iter()
        .next()
        .with_context(|| format!("no approved reply is stored at sequence {sequence}"))
}

/// Watches the approved-reply subject with core NATS — every publish, even
/// one the stream would deduplicate — so that "nothing was sent" is a
/// property proved rather than inferred.
async fn watch_replies(bus: &Bus) -> Result<tokio::sync::mpsc::UnboundedReceiver<Value>> {
    bus.subscribe_raw(APPROVED_SUBJECT).await
}

/// Gives the bus a moment to deliver anything the act might have published,
/// then reports the replies published **about one suggestion**.
///
/// Filtered by suggestion, because the subject is shared: the tests of this
/// file run in parallel on one bus, and every other test's approvals travel
/// past this subscription too. Each test invents its own suggestion, so its
/// own `subject` is the filter a real consumer would not need and this one
/// does.
async fn replies_about(
    replies: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
    suggestion_event_id: &str,
) -> Vec<String> {
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let mut seen = Vec::new();
    while let Ok(event) = replies.try_recv() {
        if event["subject"].as_str() == Some(suggestion_event_id) {
            seen.push(event["id"].as_str().unwrap_or_default().to_owned());
        }
    }
    seen
}

/// The same, asserting that the act published nothing at all.
async fn nothing_was_sent(
    replies: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
    suggestion_event_id: &str,
    what: &str,
) -> Result<()> {
    let seen = replies_about(replies, suggestion_event_id).await;
    anyhow::ensure!(
        seen.is_empty(),
        "{what} published {} approved repl{} on the bus: {seen:?}",
        seen.len(),
        if seen.len() == 1 { "y" } else { "ies" }
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 1. A valid approval publishes a schema-valid reply, and says where
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_valid_approval_publishes_a_schema_valid_reply_and_names_its_position() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("approve-valid").await?;
    let talk = conversation(&running, &bus, "valid", "granted", &in_seconds(3600)).await?;

    let (status, answer) = running
        .approve(&json!({ "suggestion_event_id": talk.suggestion_id }))
        .await?;
    assert_eq!(
        status,
        reqwest::StatusCode::CREATED,
        "the approval was refused: {answer}"
    );
    assert_eq!(answer["suggestion_event_id"], json!(talk.suggestion_id));
    assert_eq!(answer["approved_by"], json!(owner_user_id()));
    assert_eq!(
        answer["edited"],
        json!(false),
        "nothing was edited, so the persona's own words went out"
    );
    assert_eq!(
        answer["publication"],
        json!("published"),
        "an approval is published inside its own request or refused — never left in flight: {answer}"
    );
    let sequence = answer["stream_sequence"]
        .as_u64()
        .context("the answer names no stream position")?;

    // The event itself, as the bus stored it.
    let stored = stored_reply(&bus, sequence).await?;
    validate_against_contract(&stored.payload, "persona.reply.approved")?;
    let event = &stored.payload;
    assert_eq!(event["id"], answer["event_id"]);
    assert_eq!(
        event["id"],
        json!(harness::sha256_hex(&format!(
            "{}:{}",
            talk.suggestion_id,
            owner_user_id()
        ))),
        "the id is the contract's natural key, sha256(suggestion_event_id:approved_by)"
    );
    assert_eq!(event["subject"], json!(talk.suggestion_id));
    assert_eq!(
        event["source"],
        json!(format!("hermes://{SERVER_NAME}/personas/assistant")),
        "the source names the persona whose suggestion was approved"
    );
    assert_eq!(
        event["data"]["final"]["body"],
        json!(format!("{}\n{DISCLOSURE}", talk.suggestion_body)),
        "with no edit, the final content is the suggestion's own body — and, after it on a \
         line of its own, the disclosure the suggestion carried (#121, ADR 0031)"
    );
    assert_eq!(
        event["data"]["disclosure"],
        json!(DISCLOSURE),
        "the sentence is named again as a member, so a consumer need not parse the body"
    );
    assert_eq!(
        event["data"]["edited"],
        json!(false),
        "appending the disclosure is not an edit: the comparison is the body alone"
    );
    assert_eq!(
        event["data"]["target"]["room_id"],
        json!(talk.room_id),
        "the reply goes to the portal room the message it answers came from"
    );
    assert_eq!(event["data"]["approved_by"], json!(owner_user_id()));
    assert_eq!(event["consent"], json!("granted"));
    assert_eq!(
        event["traceparent"],
        json!(TRACEPARENT),
        "the trace continues from the suggestion, so sensor → persona → approval is one trace"
    );
    // The headers a consumer filters on, as the Sensor and the SDK set them.
    assert_eq!(
        stored.header("Nats-Msg-Id"),
        event["id"].as_str(),
        "the deterministic id is the dedup key, which is what makes a republish safe"
    );
    assert_eq!(stored.header("network"), Some("whatsapp"));
    // #269: the connection travels as a header too, the way the Sensor and
    // the SDK publish it, so a consumer filtering on it sees every producer.
    assert_eq!(stored.header("connection"), Some("whatsapp"));
    assert_eq!(stored.header("consent"), Some("granted"));
    assert_eq!(stored.header("traceparent"), Some(TRACEPARENT));

    // And the question that comes afterwards has an answer.
    let (status, recorded) = running
        .get(&format!("/api/approvals/{}", talk.suggestion_id))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK, "{recorded}");
    assert_eq!(recorded["publication"], json!("published"));
    assert_eq!(recorded["stream_sequence"], json!(sequence));
    assert_eq!(recorded["event_id"], event["id"]);

    // Nothing of what was said is in the Gateway's own store: the reply is
    // on the bus, where the retention is declared. Every file in the state
    // directory — the store is in WAL mode and the Gateway is still up, so
    // this run's rows are in `consent.sqlite3-wal`, and a search of the main
    // file alone would pass vacuously. The approval row is found first, so
    // that the absence below is an absence from bytes that hold the row.
    let bytes = state_bytes(&running.state_dir())?;
    assert!(
        contains(&bytes, talk.suggestion_id.as_bytes()),
        "the approval row was not found in the state directory, so the search below would \
         prove nothing"
    );
    assert!(
        !contains(&bytes, talk.suggestion_body.as_bytes()),
        "the Gateway's store holds the text of the reply that was sent"
    );
    Ok(())
}

/// Every byte of every file in the Gateway's state directory, as
/// `tests/pending.rs` reads it: the store and its WAL.
fn state_bytes(state_dir: &std::path::Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for entry in std::fs::read_dir(state_dir)
        .with_context(|| format!("failed to read {}", state_dir.display()))?
    {
        let path = entry?.path();
        if path.is_file() {
            bytes.extend(std::fs::read(&path)?);
        }
    }
    Ok(bytes)
}

/// Whether a byte slice contains another — a substring search over the
/// database files, as `tests/pending.rs` does.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

// ---------------------------------------------------------------------------
// 2. The edited content wins
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_edited_content_wins_over_what_the_persona_wrote() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("approve-edited").await?;
    let talk = conversation(&running, &bus, "edited", "granted", &in_seconds(3600)).await?;

    let edited = "Plutôt 20h30, si ça te va 👍";
    let (status, answer) = running
        .approve(&json!({
            "suggestion_event_id": talk.suggestion_id,
            "final": { "body": edited, "format": "text/plain" }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{answer}");
    assert_eq!(answer["edited"], json!(true));

    let stored = stored_reply(&bus, answer["stream_sequence"].as_u64().unwrap()).await?;
    validate_against_contract(&stored.payload, "persona.reply.approved")?;
    assert_eq!(
        stored.payload["data"]["final"]["body"],
        json!(format!("{edited}\n{DISCLOSURE}")),
        "what the user approved is exactly what goes out, with the disclosure after it: the \
         sentence is invariant whether the suggestion was sent untouched or rewritten (ADR 0031)"
    );
    assert_ne!(
        stored.payload["data"]["final"]["body"],
        json!(format!("{}\n{DISCLOSURE}", talk.suggestion_body)),
        "the persona's own words were replaced, not appended to"
    );
    assert_eq!(stored.payload["data"]["disclosure"], json!(DISCLOSURE));
    assert_eq!(stored.payload["data"]["edited"], json!(true));

    // A body that leaves no room for the line: the contract's 65 536 less a
    // newline and 200 characters is 65 335, and one over is refused before
    // anything is read from the bus — with the refusal naming what the room
    // is reserved for.
    let (status, refusal) = running
        .approve(&json!({
            "suggestion_event_id": talk.suggestion_id,
            "final": { "body": "x".repeat(65_336), "format": "text/plain" }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{refusal}");
    assert_eq!(refusal["error"], json!("malformed_request"));
    let detail = refusal["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("65335") && detail.contains("disclosure"),
        "the detail names the limit and the line it reserves room for: {refusal}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. The refusal the definition exists for: consent revoked *since*
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_approval_is_refused_when_consent_was_revoked_after_the_suggestion() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("approve-revoked").await?;
    // Granted when the message arrived and when the persona answered: the
    // suggestion's own `consent` extension says `granted`, and it still will
    // after the revocation, because a label is a fact about the past.
    let talk = conversation(&running, &bus, "revoked", "granted", &in_seconds(3600)).await?;

    // The user changes their mind — after the suggestion exists.
    running.decide(&talk.contact, "revoked").await?;

    let mut replies = watch_replies(&bus).await?;
    let (status, answer) = running
        .approve(&json!({ "suggestion_event_id": talk.suggestion_id }))
        .await?;
    assert_eq!(
        status,
        reqwest::StatusCode::CONFLICT,
        "a revoked contact's suggestion must not be approvable: {answer}"
    );
    assert_eq!(
        answer["error"],
        json!("consent_revoked"),
        "the refusal names its cause, and it is this one: {answer}"
    );
    assert!(
        answer["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(&talk.contact)),
        "the detail says which contact: {answer}"
    );
    nothing_was_sent(
        &mut replies,
        &talk.suggestion_id,
        "an approval for a revoked contact",
    )
    .await?;

    // And the Gateway has no record of an approval, because there was none.
    let (status, _) = running
        .get(&format!("/api/approvals/{}", talk.suggestion_id))
        .await?;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

    // The proof that this is not the label check: the suggestion on the bus
    // still carries `consent: granted`, so a Gateway that read only the
    // label would have sent this reply.
    let suggestions = bus.fetch_all(STREAM, SUGGEST_SUBJECT).await?;
    let suggestion = suggestions
        .iter()
        .find(|event| event["id"] == json!(talk.suggestion_id))
        .context("the suggestion is on the bus")?;
    assert_eq!(
        suggestion["consent"],
        json!("granted"),
        "the label the Sensor observed is unchanged: what moved is the consent state, now"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. An expired suggestion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_expired_suggestion_cannot_be_approved_late() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("approve-expired").await?;
    // The policy's window has passed (ticket #22: every suggestion carries
    // an expiry, and an absent one is the thing that policy exists to
    // prevent).
    let talk = conversation(&running, &bus, "expired", "granted", &in_seconds(-1)).await?;

    let mut replies = watch_replies(&bus).await?;
    let (status, answer) = running
        .approve(&json!({ "suggestion_event_id": talk.suggestion_id }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "{answer}");
    assert_eq!(
        answer["error"],
        json!("suggestion_expired"),
        "a stale suggestion is refused as stale, and not as anything else: {answer}"
    );
    nothing_was_sent(
        &mut replies,
        &talk.suggestion_id,
        "an approval of an expired suggestion",
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 5. Approving twice
// ---------------------------------------------------------------------------

#[tokio::test]
async fn approving_twice_refuses_the_second_and_sends_nothing_again() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("approve-twice").await?;
    let talk = conversation(&running, &bus, "twice", "granted", &in_seconds(3600)).await?;

    let (status, first) = running
        .approve(&json!({ "suggestion_event_id": talk.suggestion_id }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{first}");

    let mut replies = watch_replies(&bus).await?;
    let (status, second) = running
        .approve(&json!({
            "suggestion_event_id": talk.suggestion_id,
            "final": { "body": "et finalement 21h" }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "{second}");
    assert_eq!(second["error"], json!("already_approved"));
    // The second answer carries the first approval, so a client whose first
    // answer was lost learns where its reply went instead of being told to
    // try again.
    assert_eq!(second["approval"]["event_id"], first["event_id"]);
    assert_eq!(second["approval"]["publication"], json!("published"));
    assert_eq!(
        second["approval"]["stream_sequence"],
        first["stream_sequence"]
    );
    nothing_was_sent(
        &mut replies,
        &talk.suggestion_id,
        "a second approval of the same suggestion",
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 6. A suggestion that should never have existed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_suggestion_whose_trigger_was_never_consented_is_refused_under_its_own_code() -> Result<()>
{
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("approve-unconsented").await?;
    // The audit fact spec #19 asks about: the message was observed with a
    // label that was not `granted`, so the suggestion should never have been
    // produced. Its own code, separate from the two that are about now —
    // and asserted independently, which is what the design review on #24
    // asked for.
    let talk = conversation(&running, &bus, "unconsented", "revoked", &in_seconds(3600)).await?;

    let mut replies = watch_replies(&bus).await?;
    let (status, answer) = running
        .approve(&json!({ "suggestion_event_id": talk.suggestion_id }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "{answer}");
    assert_eq!(
        answer["error"],
        json!("suggestion_was_never_consented"),
        "the label at observation time and the state now are two checks and two codes: {answer}"
    );
    nothing_was_sent(
        &mut replies,
        &talk.suggestion_id,
        "an approval of an unconsented suggestion",
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 7. The refusals do not share a signal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_suggestion_a_revoked_one_and_an_expired_one_are_three_answers() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    // A window wide enough to reach the stream's first retained message, so
    // "not found" here is the whole stream having been read and not the
    // search giving up — which is the other answer, asserted below.
    let static_dir = companion_build("approve-distinct")?;
    let env = gateway_env_with(
        &static_dir,
        &[
            ("GATEWAY_NATS_URL", &nats_url()),
            ("GATEWAY_APPROVAL_LOOKUP_WINDOW", "1000000"),
        ],
    );
    let running = Running::start_with(static_dir, env).await?;

    let absent = harness::sha256_hex(&unique("nobody-suggested-this"));
    let (missing_status, missing) = running
        .approve(&json!({ "suggestion_event_id": absent }))
        .await?;
    assert_eq!(missing_status, reqwest::StatusCode::NOT_FOUND, "{missing}");
    assert_eq!(missing["error"], json!("suggestion_not_found"));

    let expired = conversation(&running, &bus, "distinct-exp", "granted", &in_seconds(-1)).await?;
    let (expired_status, expired_answer) = running
        .approve(&json!({ "suggestion_event_id": expired.suggestion_id }))
        .await?;

    let revoked =
        conversation(&running, &bus, "distinct-rev", "granted", &in_seconds(3600)).await?;
    running.decide(&revoked.contact, "revoked").await?;
    let (revoked_status, revoked_answer) = running
        .approve(&json!({ "suggestion_event_id": revoked.suggestion_id }))
        .await?;

    // Three situations, three codes. The statuses may coincide — expired and
    // revoked are both conflicts with the world's state — but the code a
    // client branches on never does, which is the lesson of #116 and #141.
    let codes = [
        missing["error"].as_str().unwrap_or_default(),
        expired_answer["error"].as_str().unwrap_or_default(),
        revoked_answer["error"].as_str().unwrap_or_default(),
    ];
    assert_eq!(
        codes
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "three different situations answered with {codes:?}"
    );
    assert_ne!(
        missing_status, expired_status,
        "a suggestion that does not exist and one that is too old are not the same answer"
    );
    assert_eq!(expired_status, revoked_status);
    for answer in [&missing, &expired_answer, &revoked_answer] {
        assert!(
            answer["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "every refusal says something an operator can act on: {answer}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 8. Never a batch, and never under another name
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_approval_is_never_a_batch_and_never_under_another_name() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("approve-batch").await?;
    let talk = conversation(&running, &bus, "batch", "granted", &in_seconds(3600)).await?;

    let mut replies = watch_replies(&bus).await?;

    // A list of suggestions is not an approval. The refusal is its own code,
    // not "malformed_request": a client that sent a batch meant to send a
    // batch, and the answer says there is no such thing.
    let (status, answer) = running
        .approve(&json!([{ "suggestion_event_id": talk.suggestion_id }]))
        .await?;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(answer["error"], json!("approval_is_not_a_batch"));

    let (status, answer) = running
        .approve(&json!({ "suggestion_event_id": [talk.suggestion_id] }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(answer["error"], json!("approval_is_not_a_batch"));

    // And an approval carries the identity of whoever approved it, which
    // this deployment knows: naming somebody else is refused rather than
    // quietly corrected.
    let (status, answer) = running
        .approve(&json!({
            "suggestion_event_id": talk.suggestion_id,
            "approved_by": "@someone-else:example.com"
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN, "{answer}");
    assert_eq!(answer["error"], json!("approved_by_is_not_the_owner"));

    // Stating the owner is accepted — the spec's body shape still works.
    let (status, answer) = running
        .approve(&json!({
            "suggestion_event_id": talk.suggestion_id,
            "approved_by": owner_user_id()
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::CREATED, "{answer}");

    // Exactly one reply went out of all that.
    let sent = replies_about(&mut replies, &talk.suggestion_id).await;
    assert_eq!(
        sent.len(),
        1,
        "four requests, one of them an approval: {sent:?}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 9. A bus that does not answer is not a Gateway that does not approve
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unreachable_bus_and_an_unconfigured_gateway_are_two_different_answers() -> Result<()> {
    ensure_stack().await?;
    let suggestion = harness::sha256_hex(&unique("never-published"));

    // No bus configured at all: this deployment does not approve anything,
    // and says so before looking at the suggestion.
    let static_dir = companion_build("approve-unconfigured")?;
    let unconfigured = Running::start_with(
        static_dir.clone(),
        gateway_env_with(&static_dir, &[("GATEWAY_NATS_URL", "")]),
    )
    .await?;
    let (status, answer) = unconfigured
        .approve(&json!({ "suggestion_event_id": suggestion }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert_eq!(answer["error"], json!("approvals_not_configured"));
    let (status, answer) = unconfigured
        .get(&format!("/api/approvals/{suggestion}"))
        .await?;
    assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert_eq!(answer["error"], json!("approvals_not_configured"));

    // A bus that is configured and does not answer: a different code and a
    // different status, because the fixes are different — one is a
    // deployment that was never set up, the other is an outage to wait out.
    let static_dir = companion_build("approve-bus-down")?;
    let down = Running::start_with(
        static_dir.clone(),
        gateway_env_with(
            &static_dir,
            &[("GATEWAY_NATS_URL", &unreachable_nats_url()?)],
        ),
    )
    .await?;
    let (status, answer) = down
        .approve(&json!({ "suggestion_event_id": suggestion }))
        .await?;
    assert_eq!(
        status,
        reqwest::StatusCode::BAD_GATEWAY,
        "a bus that does not answer is not a Gateway that refuses: {answer}"
    );
    assert_eq!(answer["error"], json!("bus_unreachable"));
    assert!(
        answer["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("Nothing was sent")),
        "the refusal says plainly that nothing went out: {answer}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 10. "I did not look that far" is not "it is not there"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_suggestion_beyond_the_search_window_is_out_of_reach_and_not_missing() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let static_dir = companion_build("approve-out-of-reach")?;
    // One stream position: everything but the newest message is beyond the
    // search.
    let running = Running::start_with(
        static_dir.clone(),
        gateway_env_with(
            &static_dir,
            &[
                ("GATEWAY_NATS_URL", &nats_url()),
                ("GATEWAY_APPROVAL_LOOKUP_WINDOW", "1"),
            ],
        ),
    )
    .await?;
    let talk = conversation(&running, &bus, "far", "granted", &in_seconds(3600)).await?;
    // Something newer, so the suggestion is not the stream's head.
    for _ in 0..3 {
        let filler = inbound_event(&ghost("filler"), &portal_room("filler"), "pending");
        bus.publish_event(INBOUND_SUBJECT, &filler).await?;
    }

    let (status, answer) = running
        .approve(&json!({ "suggestion_event_id": talk.suggestion_id }))
        .await?;
    assert_eq!(
        status,
        reqwest::StatusCode::GONE,
        "a bounded search that gave up is not a suggestion that does not exist: {answer}"
    );
    assert_eq!(answer["error"], json!("suggestion_out_of_reach"));
    assert!(
        answer["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("GATEWAY_APPROVAL_LOOKUP_WINDOW")),
        "the refusal names the knob that widens the search: {answer}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 11. Asking about an approval nobody gave
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_suggestion_nobody_approved_says_so_rather_than_nothing() -> Result<()> {
    ensure_stack().await?;
    let running = Running::start("approve-absent-record").await?;
    let (status, answer) = running
        .get(&format!(
            "/api/approvals/{}",
            harness::sha256_hex(&unique("unapproved"))
        ))
        .await?;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{answer}");
    assert_eq!(answer["error"], json!("approval_not_found"));
    assert!(
        answer["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("never approved here")),
        "the answer says the reply did not go out, rather than leaving it open: {answer}"
    );

    // And a path that is not an event id at all is a different answer again.
    let (status, answer) = running.get("/api/approvals/not-an-event-id").await?;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{answer}");
    assert_eq!(answer["error"], json!("malformed_request"));
    Ok(())
}
