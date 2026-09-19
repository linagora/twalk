//! Ticket #206, the return half of ADR 0032's seam, at the Gateway's process
//! boundary: the real binary, the real NATS JetStream, real contract events,
//! and signed HTTP calls from the test standing in for Hermes.
//!
//! `CONTEXT.md` defines what is being tested and the definition is the test
//! plan:
//!
//! > **Hermes**: Nous Research's agent runtime, external to this project and
//! > to this deployment […] Reached by a signed webhook from a persona, so
//! > that what it sees has already passed the consent gate; its answer returns
//! > through the Companion Gateway and carries the language it was written in
//! > (ADR 0032).
//!
//! So, in order:
//!
//! 1. an answer becomes a `persona.suggest.produced.v1` that is **contract
//!    valid**, carries the trigger's network, consent and trace, and is
//!    readable on `GET /api/suggestions` — which is what puts it on the
//!    Companion's existing approval screen. The contract gains no type for
//!    "a suggestion that came from outside", so this assertion is that the
//!    event is indistinguishable from the SDK's own, id included;
//! 2. the **same answer twice** is one suggestion, because the id is the
//!    contract's deterministic key and the bus absorbs the second publish —
//!    the property that makes a webhook's retry harmless;
//! 3. a contact **revoked while Hermes was reasoning** gets no suggestion at
//!    all, which is the clause "at that moment" exists for and the one a
//!    label check would pass: the trigger's own `consent` extension still says
//!    `granted`, because that is what the Sensor observed;
//! 4. a contact **nobody ever decided about** gets none either — `pending` is
//!    not a weak `granted` (ADR 0010);
//! 5. a bus that is configured and does not answer is a `502`, not the `503`
//!    that would say this deployment has no seam;
//! 6. a message the bounded read did not reach is `410`, not `404`.
//!
//! Every refusal asserts that **nothing was published**, through a core-NATS
//! subscription that sees every publish including one the stream would
//! deduplicate — the habit `CONTRIBUTING.md` records, and the only kind of
//! assertion that can prove a component never emitted something.
//!
//! What a *wrong* answer is answered with — unsigned, stale, unreadable, with
//! no language — is `tests/openapi.rs`'s, because those are reached without any
//! bus state and the codes are the description's business. This file is about
//! what a right one does to the bus.
//!
//! The bus is shared by every suite and every run, so nothing here asserts on
//! totals: each test invents its own contact, its own room and its own
//! trigger, and asks about those.

mod harness;

use anyhow::{Context, Result};
use harness::{
    companion_build, ensure_stack, gateway_env_with, gateway_env_with_hermes, hermes_answer,
    hermes_push, hermes_reference, hermes_signature, nats_url, poll_until, post_hermes_answer,
    unreachable_nats_url, validate_against_contract, Bus, GatewayProc, HERMES_DOMAIN, SERVER_NAME,
};
use serde_json::{json, Value};

const STREAM: &str = "twalk";
const INBOUND_SUBJECT: &str = "twalk.inbound.message.received.v1";
const INBOUND_TYPE: &str = "fr.linagora.twalk.inbound.message.received.v1";
const SUGGEST_SUBJECT: &str = "twalk.persona.suggest.produced.v1";
const TRACEPARENT: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";

/// What Hermes is said to have written. French, because the message it answers
/// is French: a suggestion follows the conversation and not the user
/// (ADR 0016), and the language the answer declares is the one ADR 0031's
/// disclosure would be written in.
const REPLY: &str = "D'accord, à 20h alors !";
const LANGUAGE: &str = "fr";

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
/// the persona woke Hermes about, and the only place the sender and the portal
/// room are named.
///
/// It carries a body, a display name and a `network_identifier` — none of which
/// reached Hermes (`sdk/python/tests/test_webhook.py` is where that is
/// asserted) and none of which comes back here.
fn inbound_event(sender: &str, room_id: &str, consent: &str) -> Value {
    let at = in_seconds(-120);
    let event = json!({
        "specversion": "1.0",
        "id": harness::sha256_hex(&format!("d206-inbound:{sender}:{room_id}:{at}")),
        "source": format!("matrix://{SERVER_NAME}/{room_id}"),
        "type": INBOUND_TYPE,
        "time": at,
        "subject": sender,
        "datacontenttype": "application/json",
        "dataschema": "https://schemas.twalk.dev/cloudevents/v1/inbound.message.received.schema.json",
        "traceparent": TRACEPARENT,
        "network": "whatsapp",
        "consent": consent,
        "data": {
            "body": "On décale à 20h ?",
            "format": "text/plain",
            "reply_to": null,
            "attachments": [],
            "contact": { "display_name": "Aicha Benali D206", "network_identifier": "+33612345678" }
        }
    });
    validate_against_contract(&event, "inbound.message.received")
        .expect("the fixture is an event the contract allows");
    event
}

async fn bus() -> Result<Bus> {
    let bus = Bus::connect().await?;
    bus.ensure_stream(STREAM, &["twalk.>"]).await?;
    Ok(bus)
}

/// A Gateway with a bus, a consent store, the seam configured, and a signed-in
/// device (which the test needs only to *record consent* — the answer webhook
/// itself carries no cookie and would refuse one).
struct Running {
    #[allow(dead_code)]
    gateway: GatewayProc,
    base: String,
    device: String,
    http: reqwest::Client,
}

impl Running {
    async fn start(test_name: &str) -> Result<Self> {
        let static_dir = companion_build(test_name)?;
        Self::start_with(gateway_env_with_hermes(&static_dir, &nats_url())).await
    }

    async fn start_with(env: Vec<(String, String)>) -> Result<Self> {
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
            device,
            http: reqwest::Client::new(),
        })
    }

    /// Records one consent decision the way the user does — through #49's write
    /// API, the only writer of consent state.
    async fn decide(&self, contact: &str, new_state: &str) -> Result<()> {
        let response = self
            .http
            .post(format!("{}/api/consent/decisions", self.base))
            .header(
                reqwest::header::COOKIE,
                format!("twalk_device={}", self.device),
            )
            .json(&json!({
                "subject": { "type": "contact", "id": contact },
                "new_state": new_state,
                "scope": { "networks": ["whatsapp"] }
            }))
            .send()
            .await
            .context("the consent write API did not answer")?;
        let status = response.status();
        let text = response.text().await?;
        anyhow::ensure!(
            status == reqwest::StatusCode::CREATED,
            "the decision was refused with {status}: {text}"
        );
        Ok(())
    }

    async fn get(&self, path: &str) -> Result<(reqwest::StatusCode, Value)> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .header(
                reqwest::header::COOKIE,
                format!("twalk_device={}", self.device),
            )
            .send()
            .await
            .with_context(|| format!("failed to call GET {path}"))?;
        let status = response.status();
        let text = response.text().await?;
        let body = serde_json::from_str(&text)
            .with_context(|| format!("GET {path} answered {status} with non-JSON: {text}"))?;
        Ok((status, body))
    }

    /// One answer from Hermes, signed as its outbound hook signs one.
    async fn answer(&self, reference: &str, language: Option<&str>) -> Result<(u16, Value)> {
        let push = hermes_push(&hermes_answer(reference, REPLY, language));
        let response =
            post_hermes_answer(&self.base, Some(&hermes_signature(&push)), &push).await?;
        let status = response.status().as_u16();
        let text = response.text().await?;
        let body = serde_json::from_str(&text)
            .with_context(|| format!("the answer webhook answered {status} with non-JSON: {text}"))?;
        Ok((status, body))
    }
}

/// A contact, a room and a reference nobody else uses.
fn conversation(label: &str) -> (String, String) {
    let contact = format!("@whatsapp_d206{label}:{SERVER_NAME}");
    let room = format!("!d206{label}:{SERVER_NAME}");
    (contact, room)
}

// ---------------------------------------------------------------------------
// 1. An answer becomes a suggestion, and the approval screen can read it
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_answer_from_hermes_becomes_a_suggestion_the_approval_screen_can_read() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("hermes-answer-published").await?;
    let (contact, room) = conversation("published");

    running.decide(&contact, "granted").await?;
    let trigger = inbound_event(&contact, &room, "granted");
    let trigger_id = trigger["id"].as_str().unwrap().to_owned();
    bus.publish_event(INBOUND_SUBJECT, &trigger).await?;

    let reference = hermes_reference("assistant", &trigger_id, 1);
    let (status, body) = running.answer(&reference, Some(LANGUAGE)).await?;
    assert_eq!(status, 200, "the answer was refused: {body}");
    assert_eq!(body["status"].as_str(), Some("published"), "{body}");
    assert_eq!(
        body["language"].as_str(),
        Some(LANGUAGE),
        "the answer's own language is echoed, so a caller knows it was read: {body}"
    );

    // The suggestion's id is the contract's own natural key — the same value
    // `twalk_sdk.envelope.suggest_id` computes, and the same value the persona
    // gave Hermes as the wake's idempotency key. One key from the wake to the
    // bus.
    let expected_id = harness::sha256_hex(&format!("assistant:{trigger_id}:1"));
    assert_eq!(
        body["suggestion_event_id"].as_str(),
        Some(expected_id.as_str()),
        "a suggestion from outside is keyed exactly as one from a persona: {body}"
    );

    // The event on the bus, read back by the sequence the answer named.
    let sequence = body["stream_sequence"].as_u64().context("a sequence")?;
    let stored = bus
        .consume_from(STREAM, SUGGEST_SUBJECT, sequence, 1)
        .await?
        .into_iter()
        .next()
        .context("the suggestion the answer said it published")?;
    let event = &stored.payload;
    validate_against_contract(event, "persona.suggest.produced")
        .context("the event the Gateway published is not one the contract allows")?;
    assert_eq!(event["id"].as_str(), Some(expected_id.as_str()));
    assert_eq!(event["subject"].as_str(), Some(trigger_id.as_str()));
    assert_eq!(
        event["source"].as_str(),
        Some(format!("hermes://{HERMES_DOMAIN}/personas/assistant").as_str()),
        "the source names the persona whose suggestion it is, under the domain the \
         operator configured — never one taken from the push"
    );
    assert_eq!(event["data"]["suggestion"]["body"].as_str(), Some(REPLY));
    assert_eq!(event["data"]["attempt"].as_u64(), Some(1));
    assert!(
        event["data"]["expires_at"].is_string(),
        "a suggestion that never expires stays approvable for ever (ticket #22): {event}"
    );
    // Copied from the message being answered, not decided here.
    assert_eq!(event["network"].as_str(), Some("whatsapp"));
    assert_eq!(event["consent"].as_str(), Some("granted"));
    assert_eq!(
        event["traceparent"].as_str(),
        Some(TRACEPARENT),
        "one message's trace links sensor to persona to Hermes to approval"
    );
    assert_eq!(
        event["data"]["trigger"]["event_type"].as_str(),
        Some(INBOUND_TYPE)
    );

    // The headers a consumer filters on, and the one provisional carrier: the
    // language has nowhere to live in the contract (`additionalProperties:
    // false` at both levels), so it travels as a header and this assertion is
    // what will fail when a field replaces it.
    assert_eq!(stored.header("Nats-Msg-Id"), Some(expected_id.as_str()));
    assert_eq!(stored.header("network"), Some("whatsapp"));
    assert_eq!(stored.header("consent"), Some("granted"));
    assert_eq!(
        stored.header("language"),
        Some(LANGUAGE),
        "the language the answer was written in reaches the bus; it has no contract \
         field yet, which is ticket #206's stated gap"
    );

    // And the whole point: the Companion's existing approval screen draws from
    // this read, so a suggestion Hermes produced is on it with no new endpoint
    // and no new contract type.
    let (status, listing) = running
        .get(&format!("/api/suggestions/{expected_id}"))
        .await?;
    assert_eq!(status, 200, "the suggestion is not readable: {listing}");
    assert_eq!(listing["suggestion"]["body"].as_str(), Some(REPLY));
    assert_eq!(listing["standing"].as_str(), Some("approvable"));
    assert_eq!(listing["persona_id"].as_str(), Some("assistant"));
    // #97's rule, unchanged by this path: the listing names the trigger and
    // never opens it.
    let bytes = serde_json::to_string(&listing)?;
    for marker in ["On décale", "Aicha Benali", "+33612345678", &contact] {
        assert!(
            !bytes.contains(marker),
            "the listing leaked {marker:?}: an excerpt belongs to the author of the \
             quoted message (#110, ADR 0012)"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 2. The same answer twice is one suggestion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_same_answer_twice_is_one_suggestion() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("hermes-answer-replayed").await?;
    let (contact, room) = conversation("replayed");

    running.decide(&contact, "granted").await?;
    let trigger = inbound_event(&contact, &room, "granted");
    let trigger_id = trigger["id"].as_str().unwrap().to_owned();
    bus.publish_event(INBOUND_SUBJECT, &trigger).await?;
    let reference = hermes_reference("assistant", &trigger_id, 1);

    let (first_status, first) = running.answer(&reference, Some(LANGUAGE)).await?;
    let (second_status, second) = running.answer(&reference, Some(LANGUAGE)).await?;
    assert_eq!(first_status, 200, "{first}");
    assert_eq!(
        second_status, 200,
        "a webhook retries; the second delivery must not be an error: {second}"
    );
    assert_eq!(
        first["suggestion_event_id"], second["suggestion_event_id"],
        "both answers name one suggestion"
    );

    // One event on the stream, because the id is the contract's deterministic
    // key and `Nats-Msg-Id` is set from it. The user is offered one draft of
    // one message, not two.
    let suggestion_id = first["suggestion_event_id"].as_str().unwrap();
    let stored: Vec<_> = bus
        .fetch_all(STREAM, SUGGEST_SUBJECT)
        .await?
        .into_iter()
        .filter(|event| event["id"].as_str() == Some(suggestion_id))
        .collect();
    assert_eq!(
        stored.len(),
        1,
        "two deliveries of one answer produced {} suggestions",
        stored.len()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 3 and 4. Consent, at the moment the answer comes home
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_contact_revoked_while_hermes_was_reasoning_gets_no_suggestion() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("hermes-answer-revoked").await?;
    let (contact, room) = conversation("revoked");

    // Granted when the message arrived — which is what the Sensor stamped on
    // it, and what a label-only check would read and accept.
    running.decide(&contact, "granted").await?;
    let trigger = inbound_event(&contact, &room, "granted");
    let trigger_id = trigger["id"].as_str().unwrap().to_owned();
    bus.publish_event(INBOUND_SUBJECT, &trigger).await?;

    // Revoked while Hermes was reasoning. Twalk cannot reach what Hermes has
    // already learned (ADR 0032 says so and the consent screen says so), and
    // what it *can* do is refuse to put a draft about this person in front of
    // the user.
    running.decide(&contact, "revoked").await?;

    let watch = bus.subscribe_raw(SUGGEST_SUBJECT).await?;
    let reference = hermes_reference("assistant", &trigger_id, 1);
    let (status, body) = running.answer(&reference, Some(LANGUAGE)).await?;
    assert_eq!(status, 409, "{body}");
    assert_eq!(
        body["error"].as_str(),
        Some("consent_revoked"),
        "the same code POST /api/approvals gives for the same fact: {body}"
    );
    assert!(nothing_about(watch, &trigger_id).await, "a suggestion was published anyway");
    Ok(())
}

#[tokio::test]
async fn a_contact_nobody_decided_about_gets_no_suggestion() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("hermes-answer-pending").await?;
    let (contact, room) = conversation("pending");

    // No decision at all, which is the default of the whole model and most of a
    // real list (ADR 0010): an absent subject means nobody ever decided, never
    // that somebody decided no — and it is not a weak yes either.
    let trigger = inbound_event(&contact, &room, "pending");
    let trigger_id = trigger["id"].as_str().unwrap().to_owned();
    bus.publish_event(INBOUND_SUBJECT, &trigger).await?;

    let watch = bus.subscribe_raw(SUGGEST_SUBJECT).await?;
    let reference = hermes_reference("assistant", &trigger_id, 1);
    let (status, body) = running.answer(&reference, Some(LANGUAGE)).await?;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"].as_str(), Some("consent_pending"), "{body}");
    assert!(nothing_about(watch, &trigger_id).await, "a suggestion was published anyway");
    Ok(())
}

// ---------------------------------------------------------------------------
// 5 and 6. The two answers a bound and an outage give
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_bus_that_does_not_answer_is_a_502_and_not_a_503() -> Result<()> {
    ensure_stack().await?;
    let static_dir = companion_build("hermes-answer-bus-down")?;
    let running = Running::start_with(gateway_env_with(
        &static_dir,
        &[
            ("GATEWAY_NATS_URL", &unreachable_nats_url()?),
            (
                "GATEWAY_HERMES_ANSWER_SECRET",
                harness::HERMES_ANSWER_SECRET,
            ),
            ("GATEWAY_HERMES_DOMAIN", HERMES_DOMAIN),
        ],
    ))
    .await?;

    let reference = hermes_reference("assistant", &harness::sha256_hex("no such trigger"), 1);
    let (status, body) = running.answer(&reference, Some(LANGUAGE)).await?;
    assert_eq!(
        status, 502,
        "what failed is the thing behind the Gateway, not the Gateway: {body}"
    );
    assert_eq!(body["error"].as_str(), Some("bus_unreachable"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_message_the_read_did_not_reach_is_a_410_and_not_a_404() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running_wide = Running::start("hermes-answer-window-wide").await?;
    let (contact, room) = conversation("window");
    running_wide.decide(&contact, "granted").await?;
    let trigger = inbound_event(&contact, &room, "granted");
    let trigger_id = trigger["id"].as_str().unwrap().to_owned();
    bus.publish_event(INBOUND_SUBJECT, &trigger).await?;

    // Traffic after it, so that a window of one position genuinely excludes it.
    for index in 0..4 {
        let filler = inbound_event(
            &format!("@whatsapp_d206filler{index}:{SERVER_NAME}"),
            &format!("!d206filler{index}:{SERVER_NAME}"),
            "pending",
        );
        bus.publish_event(INBOUND_SUBJECT, &filler).await?;
    }

    let static_dir = companion_build("hermes-answer-window-narrow")?;
    let narrow = Running::start_with(gateway_env_with(
        &static_dir,
        &[
            ("GATEWAY_NATS_URL", &nats_url()),
            ("GATEWAY_APPROVAL_LOOKUP_WINDOW", "1"),
            (
                "GATEWAY_HERMES_ANSWER_SECRET",
                harness::HERMES_ANSWER_SECRET,
            ),
            ("GATEWAY_HERMES_DOMAIN", HERMES_DOMAIN),
        ],
    ))
    .await?;
    let reference = hermes_reference("assistant", &trigger_id, 1);
    let (status, body) = narrow.answer(&reference, Some(LANGUAGE)).await?;
    assert_eq!(
        status, 410,
        "\"I did not look that far\" is not \"it is not there\": {body}"
    );
    assert_eq!(body["error"].as_str(), Some("trigger_out_of_reach"), "{body}");
    Ok(())
}

/// Whether nothing at all was published about this trigger, read off core NATS
/// — which sees every publish, including one the stream would deduplicate.
async fn nothing_about(
    mut watch: tokio::sync::mpsc::UnboundedReceiver<Value>,
    trigger_id: &str,
) -> bool {
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    while let Ok(event) = watch.try_recv() {
        if event["subject"].as_str() == Some(trigger_id) {
            return false;
        }
    }
    true
}
