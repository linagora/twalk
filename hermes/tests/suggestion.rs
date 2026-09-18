//! Ticket #22, the suggestion policy, at the spec's seam 1: the real
//! `assistant` container, driven by publishing contract fixtures to the bus
//! with the stub LLM behind it.
//!
//! `assistant.rs` asserts that a suggestion is *produced* — schema-valid,
//! deterministically keyed, trace-continued. This file asserts how it
//! **ages**, which is the persona's other promise to the user: a draft the
//! user never got to is stale rather than eternally approvable, and a
//! message redelivered by the bus is still one draft rather than two.
//!
//! Both are properties of the whole process, so both are asserted from
//! outside it: what appeared on the bus, and what the model was asked. The
//! pure half — the arithmetic of a window, the refusal of one that never
//! ends — is unit-tested in `sdk/python/tests/test_policy.py`, because a
//! policy that is "the operator's to set" has to be both.

mod harness;

use anyhow::{Context, Result};
use harness::{
    contract_fixture, sha256_hex, trace_id, traceparent_for, validate_against_contract, PersonaRun,
    DEFAULT_SUGGESTION_TTL_SECONDS, INBOUND_TYPE, PERSONA_ID, SUGGEST_TYPE, THINKING_TYPE,
};
use serde_json::{json, Value};

/// A window an operator might choose instead of the default: short enough
/// that no arithmetic accident could make it look like an hour.
const SHORT_WINDOW_SECONDS: i64 = 900;

/// The `inbound.message.received` fixture, re-keyed onto a trigger of this
/// run and marked, so an assertion about what reached the model is about
/// exactly this message. Re-validated, so a patched fixture stays a
/// contract citizen.
fn inbound_message(marker: &str, body: &str) -> Result<Value> {
    let mut event = contract_fixture("inbound.message.received")?;
    let id = sha256_hex(marker);
    event["id"] = json!(id);
    event["traceparent"] = json!(traceparent_for(&id));
    event["consent"] = json!("granted");
    event["data"]["body"] = json!(format!("{body} [{marker}]"));
    // The caption repeats the body in the fixture; leave it consistent.
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

fn attribute<'a>(event: &'a Value, name: &str) -> &'a str {
    event[name]
        .as_str()
        .unwrap_or_else(|| panic!("the event has no string {name}: {event}"))
}

/// The contract's `date-time` as seconds since the epoch.
///
/// Deliberately strict, and deliberately hand-rolled rather than a date
/// dependency: the one shape the contract's producers write is
/// `YYYY-MM-DDTHH:MM:SSZ`, UTC and to the second, so anything else is a
/// finding rather than something to be lenient about.
fn epoch_seconds(moment: &str) -> Result<i64> {
    let bytes = moment.as_bytes();
    if bytes.len() != 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        anyhow::bail!("{moment:?} is not the contract's YYYY-MM-DDTHH:MM:SSZ");
    }
    if bytes[13] != b':' || bytes[16] != b':' || bytes[19] != b'Z' {
        anyhow::bail!("{moment:?} is not the contract's YYYY-MM-DDTHH:MM:SSZ");
    }
    let field = |from: usize, to: usize| -> Result<i64> {
        moment[from..to]
            .parse::<i64>()
            .with_context(|| format!("{moment:?} has a non-numeric field at {from}..{to}"))
    };
    let (year, month, day) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (hour, minute, second) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);
    Ok(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn now_epoch_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs() as i64
}

/// The window one suggestion is approvable for: `expires_at` minus the time
/// it was produced at, in seconds.
fn window_of(suggestion: &Value) -> Result<i64> {
    let expires_at = suggestion["data"]["expires_at"]
        .as_str()
        .with_context(|| format!("the suggestion carries no expires_at: {suggestion}"))?;
    Ok(epoch_seconds(expires_at)? - epoch_seconds(attribute(suggestion, "time"))?)
}

#[tokio::test]
async fn a_suggestion_expires_an_hour_after_it_was_produced() -> Result<()> {
    const REPLY: &str = "Pas de problème, à 20h !";
    let run = PersonaRun::start("expiry", REPLY).await?;

    let marker = format!("expiry-{}", run.prefix);
    let trigger = inbound_message(&marker, "On décale à 20h ?")?;
    let trigger_id = event_id(&trigger);
    let published_at = now_epoch_seconds();
    run.publish_inbound(&trigger).await?;

    let suggest = run.wait_for(SUGGEST_TYPE, &trigger_id).await?;
    validate_against_contract(&suggest.payload, "persona.suggest.produced")?;
    let event = &suggest.payload;

    // 1. Every suggestion carries one. The contract makes the field
    //    optional; a persona that left it out would be publishing a draft
    //    that stays approvable for ever, which is the thing the approval
    //    path's expiry check exists to refuse.
    let expires_at = event["data"]["expires_at"]
        .as_str()
        .unwrap_or_else(|| panic!("a suggestion must carry an expiry: {event}"));

    // 2. It is exactly one default window after the suggestion was
    //    produced.
    assert_eq!(
        window_of(event)?,
        DEFAULT_SUGGESTION_TTL_SECONDS,
        "an operator who named no window gets the policy's default hour \
         (time={}, expires_at={expires_at})",
        attribute(event, "time"),
    );

    // 3. And the window runs from the *production*, not from the trigger.
    //    A persona is activated by a consent decision (ADR 0013) and its
    //    consumer starts at the beginning of the stream, so the message it
    //    is answering can be arbitrarily old: keyed off the trigger's own
    //    time, a newly activated persona's first suggestions would arrive
    //    already expired. The contract fixture is dated in the past, which
    //    is what makes the two answers tell apart here.
    let trigger_time = epoch_seconds(attribute(&trigger, "time"))?;
    assert!(
        trigger_time + DEFAULT_SUGGESTION_TTL_SECONDS < published_at,
        "this assertion needs a trigger older than one window; the fixture's \
         time is {}",
        attribute(&trigger, "time")
    );
    assert!(
        epoch_seconds(expires_at)? > now_epoch_seconds(),
        "the suggestion must still be approvable when it lands, got \
         expires_at={expires_at} for a trigger dated {}",
        attribute(&trigger, "time")
    );
    let produced_at = epoch_seconds(attribute(event, "time"))?;
    assert!(
        (published_at..=now_epoch_seconds()).contains(&produced_at),
        "the suggestion's time is when the persona produced it, got {produced_at} \
         outside {published_at}..={}",
        now_epoch_seconds()
    );

    // 4. The rest of the envelope is unchanged by carrying an expiry: same
    //    deterministic id, same trigger reference, same trace.
    assert_eq!(
        attribute(event, "id"),
        sha256_hex(&format!("{PERSONA_ID}:{trigger_id}:1")),
        "the expiry is not part of the id's natural key: two suggestions \
         produced a second apart must still be one suggestion"
    );
    assert_eq!(
        event["data"]["trigger"],
        json!({ "event_id": trigger_id, "event_type": INBOUND_TYPE })
    );
    assert_eq!(
        trace_id(attribute(event, "traceparent")),
        trace_id(attribute(&trigger, "traceparent")),
        "the persona continues the trigger's trace"
    );
    assert_eq!(
        event["data"]["suggestion"],
        json!({ "body": REPLY, "format": "text/plain" }),
        "the suggestion still carries the completion, as text"
    );

    // 5. The expiry belongs to the suggestion alone: `thinking` says the
    //    persona started, which is not something a user approves.
    let thinking = run.wait_for(THINKING_TYPE, &trigger_id).await?;
    validate_against_contract(&thinking.payload, "persona.thinking.emitted")?;
    assert!(
        thinking.payload["data"].get("expires_at").is_none(),
        "only a suggestion expires, got {}",
        thinking.payload["data"]
    );

    run.shutdown().await
}

#[tokio::test]
async fn the_window_a_suggestion_stays_approvable_for_is_the_operators() -> Result<()> {
    let run = PersonaRun::start_with_suggestion_ttl(
        "window",
        "Ça marche pour 20h.",
        SHORT_WINDOW_SECONDS,
    )
    .await?;

    let marker = format!("window-{}", run.prefix);
    let trigger = inbound_message(&marker, "On décale à 20h ?")?;
    let trigger_id = event_id(&trigger);
    run.publish_inbound(&trigger).await?;

    let suggest = run.wait_for(SUGGEST_TYPE, &trigger_id).await?;
    validate_against_contract(&suggest.payload, "persona.suggest.produced")?;
    assert_eq!(
        window_of(&suggest.payload)?,
        SHORT_WINDOW_SECONDS,
        "the window is configuration, not the persona's judgement: an \
         operator who asked for {SHORT_WINDOW_SECONDS}s gets it (time={}, \
         expires_at={})",
        attribute(&suggest.payload, "time"),
        suggest.payload["data"]["expires_at"],
    );
    assert_ne!(
        window_of(&suggest.payload)?,
        DEFAULT_SUGGESTION_TTL_SECONDS,
        "and the default is a default, not a constant the configuration \
         cannot reach"
    );

    run.shutdown().await
}

#[tokio::test]
async fn a_redelivered_trigger_is_one_suggestion_and_never_a_second_attempt() -> Result<()> {
    const FIRST_DRAFT: &str = "Pas de problème, à 20h !";
    const SECOND_DRAFT: &str = "Ça marche, on dit 20h.";
    let run = PersonaRun::start("replay", "no further draft was expected").await?;
    // Two different answers, so that a second suggestion would be visibly a
    // second one rather than an identical copy the bus could absorb by
    // accident.
    run.llm.push_reply(FIRST_DRAFT);
    run.llm.push_reply(SECOND_DRAFT);

    let marker = format!("replay-{}", run.prefix);
    let trigger = inbound_message(&marker, "On décale à 20h ?")?;
    let trigger_id = event_id(&trigger);
    run.publish_inbound(&trigger).await?;
    // The same CloudEvent again, without the producer's `Nats-Msg-Id`: the
    // bus is at-least-once, and this is what a persona sees after a crash
    // between processing and acking.
    run.redeliver_inbound(&trigger).await?;

    // The fence: a granted message published last, whose suggestion proves
    // the persona has been through both deliveries — which is what turns
    // "there is no second suggestion" into an assertion instead of a wait.
    let fence_marker = format!("fence-{}", run.prefix);
    let fence = inbound_message(&fence_marker, "Et pour dimanche ?")?;
    let fence_id = event_id(&fence);
    run.publish_inbound(&fence).await?;
    run.wait_for(SUGGEST_TYPE, &fence_id).await?;

    // The redelivery really happened: without this the rest of the test
    // would pass against a bus that quietly absorbed the second publish.
    let delivered: Vec<u64> = run
        .published(INBOUND_TYPE)
        .await?
        .into_iter()
        .filter(|message| message.payload["id"].as_str() == Some(&*trigger_id))
        .map(|message| message.sequence)
        .collect();
    assert_eq!(
        delivered.len(),
        2,
        "the trigger must be on the stream twice for this test to mean \
         anything, found it at {delivered:?}"
    );

    // One suggestion, at attempt 1, with the first draft: the attempt counts
    // suggestions that exist, not deliveries that were tried.
    let suggestions: Vec<Value> = run
        .published(SUGGEST_TYPE)
        .await?
        .into_iter()
        .filter(|message| message.payload["subject"].as_str() == Some(&*trigger_id))
        .map(|message| message.payload)
        .collect();
    assert_eq!(
        suggestions.len(),
        1,
        "a redelivered message must not offer the user a second draft of \
         itself, got {:?}",
        suggestions
            .iter()
            .map(|event| &event["data"]["suggestion"]["body"])
            .collect::<Vec<_>>()
    );
    let suggestion = &suggestions[0];
    validate_against_contract(suggestion, "persona.suggest.produced")?;
    assert_eq!(suggestion["data"]["attempt"], json!(1));
    assert_eq!(
        attribute(suggestion, "id"),
        sha256_hex(&format!("{PERSONA_ID}:{trigger_id}:1")),
        "the id is a function of the persona, the trigger and the attempt — \
         and of nothing the redelivery changed"
    );
    assert_eq!(
        suggestion["data"]["suggestion"]["body"],
        json!(FIRST_DRAFT),
        "the draft the user was offered first is the one that stands"
    );

    // Nothing in the whole run is a second attempt, for any trigger.
    let attempts: Vec<i64> = run
        .published(SUGGEST_TYPE)
        .await?
        .into_iter()
        .filter_map(|message| message.payload["data"]["attempt"].as_i64())
        .collect();
    assert!(
        attempts.iter().all(|attempt| *attempt == 1),
        "no delivery may escalate the attempt counter, got {attempts:?}"
    );

    // The `thinking` event collapses the same way: one message, one
    // statement that the persona started on it.
    let thinking: Vec<u64> = run
        .published(THINKING_TYPE)
        .await?
        .into_iter()
        .filter(|message| message.payload["subject"].as_str() == Some(&*trigger_id))
        .map(|message| message.sequence)
        .collect();
    assert_eq!(
        thinking.len(),
        1,
        "oversight must not be told twice that the persona started on one \
         message, found {thinking:?}"
    );

    // What it did cost: the model was asked again. That is deliberate and
    // is the safe side of the trade — treating "I already said I was
    // thinking about this" as "I already answered it" would silently lose
    // the suggestion in the case the retry exists for, an endpoint that was
    // away when the first delivery reached it.
    assert_eq!(
        run.llm_requests_mentioning(&marker).len(),
        2,
        "the redelivery re-asks the model; what it must not do is publish \
         the answer as a second suggestion"
    );

    // And the persona says so, which is how an operator reading its logs
    // tells an absorbed replay from a suggestion that never came.
    let logs = run.logs().await?;
    assert!(
        logs.contains("the bus already held this suggestion"),
        "the persona must report the absorbed redelivery; its logs were:\n{logs}"
    );

    run.shutdown().await
}
