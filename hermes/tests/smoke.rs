//! Smoke test for the shared test harness on the Hermes side (ticket #20).
//!
//! Hermes has no runtime yet; what this proves is the harness the rest of
//! the Hermes lot is built on. It is the Sensor's ticket-01 smoke test read
//! from the other side of the monorepo: the shared stack boots and its bus
//! round-trips a persona event with the contract's deterministic id, the
//! stub LLM answers persona tests the same way every run, and contract
//! validation rejects an event the schemas forbid.
//!
//! The seam is unchanged: nothing here reaches inside a component process.

use anyhow::Result;
use serde_json::{json, Value};
use twalk_test_harness::stub_llm::DEFAULT_REPLY;
use twalk_test_harness::{
    contract_fixture, ensure_stack, sha256_hex, validate_against_contract, Bus, StubLlm,
};

/// Harness self-test streams live outside the `twalk.*` namespace, which
/// belongs to the Sensor's own stream (JetStream forbids overlapping
/// subjects across streams).
const STREAM: &str = "hermes-smoke";
const SUBJECT_PREFIX: &str = "hermes.smoke";

/// A unique id per run: the stack persists across runs, so reusing fixed
/// subjects and event ids would leak messages between them.
fn run_id() -> Result<String> {
    Ok(format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ))
}

/// The `persona.suggest.produced` fixture, re-keyed onto a trigger of this
/// run: `id` is recomputed from the contract's natural key
/// (sha256(persona_id:trigger_event_id:attempt)), so the event stays
/// schema-valid and replay-safe.
fn suggest_event(run: &str) -> Result<Value> {
    let mut event = contract_fixture("persona.suggest.produced")?;
    let trigger_event_id = sha256_hex(&format!("hermes-smoke-trigger-{run}"));
    let persona_id = event["data"]["persona_id"]
        .as_str()
        .expect("the fixture names its persona")
        .to_owned();
    let attempt = event["data"]["attempt"]
        .as_u64()
        .expect("the fixture carries an attempt counter");
    event["subject"] = json!(trigger_event_id);
    event["data"]["trigger"]["event_id"] = json!(trigger_event_id);
    event["id"] = json!(sha256_hex(&format!(
        "{persona_id}:{trigger_event_id}:{attempt}"
    )));
    Ok(event)
}

#[tokio::test]
async fn the_shared_stack_boots_and_its_bus_round_trips_a_persona_event() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    bus.ensure_stream(STREAM, &[&format!("{SUBJECT_PREFIX}.>")])
        .await?;

    let run = run_id()?;
    let event = suggest_event(&run)?;
    validate_against_contract(&event, "persona.suggest.produced")?;
    let subject = format!("{SUBJECT_PREFIX}.{run}.persona.suggest.produced.v1");

    bus.publish_event(&subject, &event).await?;

    let stored = bus.fetch_all_with_headers(STREAM, &subject).await?;
    assert_eq!(
        stored.len(),
        1,
        "the bus must store exactly the published event"
    );
    assert_eq!(
        stored[0].payload, event,
        "the bus must return exactly what was published"
    );
    assert_eq!(
        stored[0].header("Nats-Msg-Id"),
        event["id"].as_str(),
        "the harness must publish with Nats-Msg-Id set to the event id"
    );

    // Republishing the same event is what a persona restart does; the id is
    // deterministic, so the bus de-duplicates it.
    bus.publish_event(&subject, &event).await?;
    let stored = bus.fetch_all_with_headers(STREAM, &subject).await?;
    assert_eq!(
        stored.len(),
        1,
        "a replayed event with the same deterministic id must not duplicate on the bus"
    );
    Ok(())
}

#[tokio::test]
async fn the_stub_llm_answers_chat_completions_deterministically() -> Result<()> {
    let stub = StubLlm::start().await?;
    let request = json!({
        "model": "qwen2.5-32b-instruct",
        "messages": [
            { "role": "system", "content": "You draft replies for the user to approve." },
            { "role": "user", "content": "on décale à 20h ?" },
        ],
    });

    let http = reqwest::Client::new();
    let mut answers = Vec::new();
    for _ in 0..2 {
        let response = http
            .post(stub.chat_completions_url())
            .bearer_auth("test-only-llm-key")
            .json(&request)
            .send()
            .await?
            .error_for_status()?;
        answers.push(response.json::<Value>().await?);
    }

    assert_eq!(
        answers[0]
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some(DEFAULT_REPLY),
        "the stub must serve its canned completion"
    );
    assert_eq!(
        answers[0], answers[1],
        "the same request must get the same answer, so persona assertions are stable"
    );
    assert_eq!(
        stub.request_count(),
        2,
        "the stub records every call, so a test can prove a persona did not call the model"
    );
    assert_eq!(
        stub.requests()[0].last_message_content(),
        Some("on décale à 20h ?"),
        "a persona test must be able to read the prompt the persona sent"
    );
    Ok(())
}

#[tokio::test]
async fn contract_validation_accepts_the_fixtures_and_rejects_an_invalid_event() -> Result<()> {
    for type_name in [
        "persona.thinking.emitted",
        "persona.suggest.produced",
        "persona.reply.approved",
    ] {
        let fixture = contract_fixture(type_name)?;
        validate_against_contract(&fixture, type_name)?;
    }

    // A non-deterministic id is exactly the mistake the contract forbids
    // (the schema pins `id` to 64 hex characters).
    let mut invalid = contract_fixture("persona.suggest.produced")?;
    invalid["id"] = json!("not-a-sha256");
    let error = validate_against_contract(&invalid, "persona.suggest.produced")
        .expect_err("an event with a non-deterministic id must fail validation");
    assert!(
        error.to_string().contains("/id"),
        "the failure must name the offending field, got: {error}"
    );

    // A missing required member fails too, rather than passing silently.
    let mut incomplete = contract_fixture("persona.suggest.produced")?;
    incomplete
        .as_object_mut()
        .expect("the fixture is an object")
        .remove("consent");
    validate_against_contract(&incomplete, "persona.suggest.produced")
        .expect_err("a message-flow event without consent must fail validation");
    Ok(())
}
