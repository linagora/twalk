//! Ticket #21, the Python persona SDK and the `assistant` skeleton, at the
//! spec's seam 1: the real persona process, driven by publishing contract
//! fixtures to the bus, with the stub LLM answering a canned completion.
//!
//! The persona under test is the container image `assistant` ships as
//! (personas ship as images — ADR 0008, and the host has no pip), talking
//! to the bus itself and never to the test process. Every assertion is
//! made from outside it: what appeared on the bus, and what the model was
//! asked.
//!
//! The consent gate is the one that carries the project's promise. It is
//! asserted here by absence — no event and no completion request for a
//! `pending` or `revoked` message — and case by case in the SDK's own unit
//! tests (`sdk/python/tests/test_consent_gate.py`), because a gate that is
//! "impossible for an author to forget" has to be both.

mod harness;

use anyhow::Result;
use harness::{
    contract_fixture, contract_variant_fixture, sha256_hex, trace_id, traceparent_for,
    validate_against_contract, PersonaRun, HERMES_DOMAIN, INBOUND_TYPE, LLM_API_KEY, MODEL,
    PERSONA_ID, SUGGEST_TYPE, THINKING_TYPE,
};
use serde_json::{json, Value};

/// The `inbound.message.received` fixture, re-keyed onto a trigger of this
/// run and labelled with a consent state. The body carries the run's marker
/// so that an assertion about what reached the model is about exactly this
/// message. Re-validated, so a patched fixture stays a contract citizen.
fn inbound_message(marker: &str, consent: &str, body: &str) -> Result<Value> {
    let mut event = contract_fixture("inbound.message.received")?;
    let id = sha256_hex(marker);
    event["id"] = json!(id);
    event["traceparent"] = json!(traceparent_for(&id));
    event["consent"] = json!(consent);
    event["data"]["body"] = json!(format!("{body} [{marker}]"));
    // The caption repeats the body in the fixture; leave it consistent.
    event["data"]["attachments"][0]["caption"] = event["data"]["body"].clone();
    validate_against_contract(&event, "inbound.message.received")?;
    Ok(event)
}

/// The contract's reduced shape for a revoked sender: same envelope, no
/// body, no excerpt, no attachment reference (ADR 0012, issue #58). The
/// gate must drop it before that shape could matter — and it carries no
/// text to mark, which is the point.
fn revoked_message(marker: &str) -> Result<Value> {
    let mut event = contract_variant_fixture("inbound.message.received", "revoked-sender")?;
    let id = sha256_hex(marker);
    event["id"] = json!(id);
    event["traceparent"] = json!(traceparent_for(&id));
    validate_against_contract(&event, "inbound.message.received")?;
    Ok(event)
}

fn event_id(event: &Value) -> String {
    event["id"]
        .as_str()
        .expect("a patched fixture has a string id")
        .to_owned()
}

fn attribute<'a>(event: &'a Value, name: &str) -> &'a str {
    event[name]
        .as_str()
        .unwrap_or_else(|| panic!("the event has no string {name}: {event}"))
}

#[tokio::test]
async fn a_granted_message_produces_thinking_then_a_schema_valid_suggestion() -> Result<()> {
    const REPLY: &str = "Pas de problème, à 20h !";
    let run = PersonaRun::start("granted", REPLY).await?;

    let marker = format!("granted-{}", run.prefix);
    let trigger = inbound_message(&marker, "granted", "On décale à 20h ?")?;
    let trigger_id = event_id(&trigger);
    run.publish_inbound(&trigger).await?;

    // 1. thinking: the persona started on this message.
    let thinking = run.wait_for(THINKING_TYPE, &trigger_id).await?;
    validate_against_contract(&thinking.payload, "persona.thinking.emitted")?;
    let event = &thinking.payload;
    assert_eq!(
        attribute(event, "id"),
        sha256_hex(&format!("{PERSONA_ID}:{trigger_id}")),
        "the thinking id is sha256(persona_id:trigger_event_id)"
    );
    assert_eq!(
        attribute(event, "source"),
        format!("hermes://{HERMES_DOMAIN}/personas/{PERSONA_ID}")
    );
    assert_eq!(
        attribute(event, "subject"),
        trigger_id,
        "the subject is the trigger's event id"
    );
    assert_eq!(
        event["data"]["trigger"],
        json!({ "event_id": trigger_id, "event_type": INBOUND_TYPE }),
        "oversight must be able to link the activity to the trigger without a bus lookup"
    );
    assert_eq!(attribute(event, "network"), "whatsapp");
    assert_eq!(attribute(event, "consent"), "granted");
    assert_eq!(
        event["data"]["model"],
        json!(MODEL),
        "the thinking event names the model that will reason"
    );
    assert_eq!(
        trace_id(attribute(event, "traceparent")),
        trace_id(attribute(&trigger, "traceparent")),
        "the persona continues the trigger's trace"
    );
    assert_eq!(
        thinking.header("Nats-Msg-Id"),
        Some(attribute(event, "id")),
        "Nats-Msg-Id is the deterministic id, so a replay collapses on the bus"
    );
    for extension in ["network", "consent", "traceparent"] {
        assert_eq!(
            thinking.header(extension),
            Some(attribute(event, extension)),
            "{extension} is duplicated as a NATS header for server-side filtering"
        );
    }

    // 2. suggest: the reply the user may approve.
    let suggest = run.wait_for(SUGGEST_TYPE, &trigger_id).await?;
    validate_against_contract(&suggest.payload, "persona.suggest.produced")?;
    let event = &suggest.payload;
    assert_eq!(
        attribute(event, "id"),
        sha256_hex(&format!("{PERSONA_ID}:{trigger_id}:1")),
        "the suggest id is sha256(persona_id:trigger_event_id:attempt)"
    );
    assert_eq!(attribute(event, "subject"), trigger_id);
    assert_eq!(
        event["data"]["suggestion"],
        json!({ "body": REPLY, "format": "text/plain" }),
        "the suggestion carries the completion, as text"
    );
    assert_eq!(event["data"]["attempt"], json!(1));
    assert_eq!(
        event["data"]["trigger"],
        json!({ "event_id": trigger_id, "event_type": INBOUND_TYPE })
    );
    assert_eq!(attribute(event, "network"), "whatsapp");
    assert_eq!(attribute(event, "consent"), "granted");
    assert_eq!(
        trace_id(attribute(event, "traceparent")),
        trace_id(attribute(&trigger, "traceparent"))
    );
    assert_eq!(
        suggest.header("Nats-Msg-Id"),
        Some(attribute(event, "id"))
    );
    assert!(
        event["data"].get("expires_at").is_none(),
        "the suggestion policy — the attempt counter and the expiry — is H3 (#22); \
         this ticket builds the envelope it extends, and sets no expiry"
    );

    assert!(
        thinking.sequence < suggest.sequence,
        "thinking must reach the bus before the suggestion it precedes \
         (thinking at {}, suggest at {})",
        thinking.sequence,
        suggest.sequence
    );

    // 3. what the persona asked the model.
    let requests = run.llm_requests_mentioning(&marker);
    assert_eq!(
        requests.len(),
        1,
        "one message, one completion request (v0.1 is a single completion)"
    );
    let request = &requests[0];
    assert_eq!(
        request.last_message_content(),
        Some(
            trigger["data"]["body"]
                .as_str()
                .expect("the fixture has a body")
        ),
        "the message the user received is what the persona asked about"
    );
    assert_eq!(
        request.body["messages"][0]["role"],
        json!("system"),
        "the persona frames the request with a system prompt"
    );
    assert_eq!(request.body["model"], json!(MODEL));
    assert_eq!(
        request.authorization.as_deref(),
        Some(format!("Bearer {LLM_API_KEY}").as_str()),
        "the credentials the operator configured reach the endpoint they chose"
    );

    run.shutdown().await
}

#[tokio::test]
async fn a_pending_or_revoked_message_produces_no_event_and_no_llm_call() -> Result<()> {
    let run = PersonaRun::start("gate", "this reply must never be drafted").await?;

    let pending = inbound_message(
        &format!("pending-{}", run.prefix),
        "pending",
        "Tu peux répondre ?",
    )?;
    let revoked = revoked_message(&format!("revoked-{}", run.prefix))?;
    // Published first, so that the fence below cannot be processed before
    // them: the persona consumes its subject in stream order.
    run.publish_inbound(&pending).await?;
    run.publish_inbound(&revoked).await?;

    // The fence: a granted message, published last. Its suggestion is the
    // proof that the persona has been through the two above — which is how
    // an absence becomes assertable instead of a wait of arbitrary length.
    let fence_marker = format!("fence-{}", run.prefix);
    let fence = inbound_message(&fence_marker, "granted", "On décale à 20h ?")?;
    let fence_id = event_id(&fence);
    run.publish_inbound(&fence).await?;
    run.wait_for(SUGGEST_TYPE, &fence_id).await?;

    for (label, event) in [("pending", &pending), ("revoked", &revoked)] {
        let trigger_id = event_id(event);
        for event_type in [THINKING_TYPE, SUGGEST_TYPE] {
            let about: Vec<String> = run
                .published(event_type)
                .await?
                .into_iter()
                .filter(|message| message.payload["subject"].as_str() == Some(&trigger_id))
                .map(|message| message.payload["id"].as_str().unwrap_or_default().to_owned())
                .collect();
            assert!(
                about.is_empty(),
                "a {label} message must produce no {event_type}, got {about:?}"
            );
        }
    }

    // And the model was never asked: the gate runs before any persona code,
    // so nothing about those two messages was ever sent anywhere.
    let requests = run.llm.requests();
    assert_eq!(
        requests.len(),
        1,
        "three messages, one granted: the model must have been asked exactly once, \
         got {} requests",
        requests.len()
    );
    assert!(
        requests[0].body.to_string().contains(&fence_marker),
        "the one completion request must be the granted message's"
    );

    // Every event the persona did publish in this run is about the fence.
    for event_type in [THINKING_TYPE, SUGGEST_TYPE] {
        let published = run.published(event_type).await?;
        assert_eq!(
            published.len(),
            1,
            "only the granted message may produce a {event_type}"
        );
        assert_eq!(published[0].payload["subject"].as_str(), Some(&*fence_id));
        assert_eq!(published[0].payload["consent"].as_str(), Some("granted"));
    }

    run.shutdown().await
}

#[tokio::test]
async fn a_message_with_no_text_is_skipped_without_a_suggestion() -> Result<()> {
    let run = PersonaRun::start("no-text", "this reply must never be drafted").await?;

    // A granted message whose body is empty: v0.1 answers text, and a
    // persona that guessed at an attachment would be worse than one that
    // says nothing. It still reports that it started — the user sees the
    // activity — and produces no suggestion.
    let mut silent = inbound_message(&format!("no-text-{}", run.prefix), "granted", "")?;
    silent["data"]["body"] = json!("");
    silent["data"]["attachments"][0]["caption"] = json!("");
    validate_against_contract(&silent, "inbound.message.received")?;
    let silent_id = event_id(&silent);
    run.publish_inbound(&silent).await?;

    let fence_marker = format!("fence-{}", run.prefix);
    let fence = inbound_message(&fence_marker, "granted", "On décale à 20h ?")?;
    let fence_id = event_id(&fence);
    run.publish_inbound(&fence).await?;
    run.wait_for(SUGGEST_TYPE, &fence_id).await?;

    run.wait_for(THINKING_TYPE, &silent_id).await?;
    let suggestions: Vec<u64> = run
        .published(SUGGEST_TYPE)
        .await?
        .into_iter()
        .filter(|message| message.payload["subject"].as_str() == Some(&silent_id))
        .map(|message| message.sequence)
        .collect();
    assert!(
        suggestions.is_empty(),
        "a message with no text must produce no suggestion, found {suggestions:?}"
    );
    assert_eq!(
        run.llm.request_count(),
        1,
        "the model is not asked to reply to nothing"
    );

    run.shutdown().await
}
