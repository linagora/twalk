//! Issue #162: a model that spends its budget thinking, at the persona's
//! process boundary.
//!
//! What was found on the reference deployment, against the operator's own
//! Qwen behind LiteLLM: the endpoint answers `HTTP 200`, `finish_reason:
//! "length"`, `content: null`, and the whole budget in `reasoning_content`.
//! The persona logged *"the chat-completions answer carries no content"* and
//! asked the same question again, and again, at the operator's expense. The
//! model's behaviour is not the defect — it was reproduced with `curl`
//! outside Twalk — everything after it was.
//!
//! So this file drives the real persona container against a stub LLM serving
//! that exact answer, and asserts the three things that have to be true:
//!
//! 1. the outcome is **named** and is not the outcome of a model that
//!    answered nothing;
//! 2. the trigger is **asked about once**. Not three times, not for ever: a
//!    retry cannot change this answer and each one is billed;
//! 3. the trigger is not left **apparently unprocessed** — the bus is asked,
//!    not the persona, because "the persona gave up" and "the bus will hand
//!    it over again" are different facts and only the second one costs money.
//!
//! And beside them, the distinction the whole ticket is about: a model that
//! stopped on its own having said nothing *is* retried, because that one
//! might pass. Both cases run against one container, in order, by moving the
//! stub's standing answer between them.

mod harness;

use anyhow::Result;
use harness::{
    contract_fixture, poll_until, sha256_hex, traceparent_for, validate_against_contract,
    PersonaRun, StubAnswer, SUGGEST_TYPE, THINKING_TYPE,
};
use serde_json::{json, Value};

/// A granted inbound message carrying this run's marker, so an assertion
/// about what reached the model is about exactly this message.
fn inbound_message(marker: &str, body: &str) -> Result<Value> {
    let mut event = contract_fixture("inbound.message.received")?;
    let id = sha256_hex(marker);
    event["id"] = json!(id);
    event["traceparent"] = json!(traceparent_for(&id));
    event["consent"] = json!("granted");
    event["data"]["body"] = json!(format!("{body} [{marker}]"));
    event["data"]["attachments"][0]["caption"] = event["data"]["body"].clone();
    validate_against_contract(&event, "inbound.message.received")?;
    Ok(event)
}

fn event_id(event: &Value) -> String {
    event["id"]
        .as_str()
        .expect("a patched fixture has a string id")
        .to_owned()
}

/// Waits for a line in the persona's logs, so a failure carries the logs
/// rather than a timeout.
async fn wait_for_log(run: &PersonaRun, needle: &str) -> Result<String> {
    let found = poll_until(
        || async {
            let logs = run.logs().await.ok()?;
            logs.contains(needle).then_some(logs)
        },
        &format!("the persona to log {needle:?}"),
    )
    .await;
    match found {
        Ok(logs) => Ok(logs),
        Err(error) => anyhow::bail!(
            "{error}; the persona's logs were:\n{}",
            run.logs().await.unwrap_or_default()
        ),
    }
}

#[tokio::test]
async fn a_budget_spent_thinking_is_refused_once_and_a_model_that_said_nothing_is_retried(
) -> Result<()> {
    let run = PersonaRun::start("budget", "this reply is never served").await?;

    // ── 1. The model spends its whole budget reasoning. ──────────────────
    run.llm.set_answer(StubAnswer::budget_spent_reasoning());

    let marker = format!("budget-{}", run.prefix);
    let trigger = inbound_message(&marker, "on décale à 20h ?")?;
    let trigger_id = event_id(&trigger);
    run.publish_inbound(&trigger).await?;

    // The persona did start on this message: `thinking` is published before
    // the model is called, so oversight shows activity and the trigger is
    // not invisible.
    let thinking = run.wait_for(THINKING_TYPE, &trigger_id).await?;
    validate_against_contract(&thinking.payload, "persona.thinking.emitted")?;

    let logs = wait_for_log(&run, "and no retry").await?;
    let refusal = logs
        .lines()
        .find(|line| line.contains("and no retry") && line.contains(&trigger_id))
        .unwrap_or_else(|| panic!("no refusal names this trigger; the logs were:\n{logs}"))
        .to_owned();

    // The outcome is named, and it names the remedy — which is an operator's
    // to apply, in the variable an operator actually types.
    assert!(
        refusal.contains("spent its whole") && refusal.contains("on reasoning"),
        "the refusal must say the budget went to reasoning, not that the answer \
         'carries no content': {refusal}"
    );
    assert!(
        refusal.contains("2000-token budget"),
        "it must name the budget that was too small, which is the persona's own \
         (issue #162 raised it from 300): {refusal}"
    );
    assert!(
        refusal.contains("HERMES_LLM_PARAMS"),
        "and where a larger one is set: {refusal}"
    );
    assert!(
        !logs.contains("carries no content"),
        "the message this ticket was opened about must be gone: it named neither \
         the cause nor the cure. The logs were:\n{logs}"
    );

    // Asked once. A second completion request would be a second invoice for
    // an answer that cannot change — and the persona's own retry delay is
    // five seconds, so this waits past it before counting.
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;
    let requests = run.llm_requests_mentioning(&marker);
    assert_eq!(
        requests.len(),
        1,
        "the persona must ask once and refuse, not retry an answer that will \
         not change; it sent {} requests",
        requests.len()
    );
    assert!(
        run.published(SUGGEST_TYPE).await?.is_empty(),
        "a persona that could not reason must publish no suggestion rather than \
         inventing one"
    );

    // And the trigger is not left in flight. The bus's own account: nothing
    // pending, nothing awaiting an ack, nothing redelivered.
    let state = run.consumer_state().await?;
    assert_eq!(
        (state.pending, state.awaiting_ack, state.redelivered),
        (0, 0, 0),
        "the trigger must be dealt with rather than left apparently \
         unprocessed for ever, and never handed over twice: {state:?}"
    );

    // ── 2. A model that stopped on its own having said nothing. ──────────
    // A different outcome, a different message, and this one *is* retried:
    // it might pass. Bounded by the consumer's redelivery limit, and the
    // last delivery says so rather than letting the trigger disappear.
    run.llm.set_answer(StubAnswer::NoContent);

    let silent_marker = format!("silent-{}", run.prefix);
    let silent = inbound_message(&silent_marker, "tu es dispo demain ?")?;
    let silent_id = event_id(&silent);
    run.publish_inbound(&silent).await?;

    let logs = wait_for_log(&run, "which is this consumer's limit").await?;
    let exhausted = logs
        .lines()
        .find(|line| line.contains("which is this consumer's limit") && line.contains(&silent_id))
        .unwrap_or_else(|| panic!("no line accounts for the last delivery:\n{logs}"))
        .to_owned();
    assert!(
        exhausted.contains("answered nothing"),
        "a model that said nothing must be reported as that, and not as a budget \
         spent thinking: {exhausted}"
    );
    assert!(
        !exhausted.contains("HERMES_LLM_PARAMS"),
        "and must not name the budget as the remedy, because it is not one here: \
         {exhausted}"
    );

    let retried = run.llm_requests_mentioning(&silent_marker);
    assert_eq!(
        retried.len(),
        3,
        "this one is worth retrying — an endpoint is briefly away often enough — \
         and the consumer's limit is what bounds it; got {} requests",
        retried.len()
    );
    let state = run.consumer_state().await?;
    assert_eq!(
        (state.pending, state.awaiting_ack),
        (0, 0),
        "even the give-up is accounted for: nothing is left in flight: {state:?}"
    );

    // The one message that was never sent to the model: the trigger is still
    // a message the persona read, so the budget refusal above says nothing
    // about the contact and neither does this one.
    assert!(
        run.published(SUGGEST_TYPE).await?.is_empty(),
        "neither outcome produces a suggestion"
    );

    run.shutdown().await
}
