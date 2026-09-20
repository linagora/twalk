//! Ticket #206, the outbound half of ADR 0032's seam, at the persona's own
//! seam: the real persona **container**, the real bus, and a stub of Hermes's
//! webhook route on the other side of a real socket.
//!
//! ADR 0032 makes one claim about this direction and the whole integration
//! rests on it:
//!
//! > a persona reaches Hermes by **posting a signed webhook** […] and what
//! > crosses the wire is a **narrow template of named fields**, never the raw
//! > event, because Hermes's memory absorbs whatever it is shown and a body
//! > copied into `MEMORY.md` never expires.
//!
//! So the assertions here are, in order of what they are worth:
//!
//! 1. **the body, field by field, including what is absent** — asserted
//!    against the bytes that really went over the socket, not against a
//!    builder's return value. Its companion is `sdk/python/tests/test_webhook.py`,
//!    which asserts the same property on the pure function; this one asserts
//!    that the function is what the persona actually sends;
//! 2. **the consent gate still runs before anything crosses**. A `pending` and
//!    a `revoked` message reach Hermes not at all — no wake, and no *refused*
//!    wake either, because a request turned away at Hermes's door is a
//!    request that was made;
//! 3. **the persona proposes nothing itself** when a seam is configured: it
//!    publishes `persona.thinking.emitted` and no `persona.suggest.produced`,
//!    because the answer comes back through the Companion Gateway
//!    (`companion-gateway/tests/hermes_answers.rs` is that half), and it does
//!    not call the model either — two brains asked the same question is not a
//!    design, it is a race;
//! 4. **a redelivered trigger is one wake**, because the delivery id a persona
//!    gives Hermes is the deterministic id of the suggestion it will produce,
//!    so Hermes's idempotency and the bus's deduplication agree on one key;
//! 5. **Hermes unreachable does not stop the deployment**: the persona starts,
//!    says so at `ERROR` naming the URL, and retries in the background — the
//!    shape the Sensor already uses for the Gateway's consent snapshot.
//!
//! The stub is `tests/harness/hermes.rs`, and it verifies the signature the
//! way the real adapter does. That matters: a stub that accepted anything
//! would let a persona sign nothing and still pass every assertion below.

mod harness;

use anyhow::Result;
use harness::{
    contract_fixture, contract_variant_fixture, sha256_hex, traceparent_for,
    validate_against_contract, PersonaRun, StubHermes, INBOUND_TYPE, PERSONA_ID, SUGGEST_TYPE,
    THINKING_TYPE, UNREACHABLE_HERMES_URL,
};
use serde_json::{json, Value};

/// The message the persona is woken by. Its body is the run's marker, and
/// every *other* field of the fixture is a marker too — the sender's Matrix
/// ID, the display name, the phone number, the room, the attachment's
/// decryption key — because what the seam must not carry is the interesting
/// half of this test.
fn inbound_message(marker: &str, consent: &str, body: &str) -> Result<Value> {
    let mut event = contract_fixture("inbound.message.received")?;
    let id = sha256_hex(marker);
    event["id"] = json!(id);
    event["traceparent"] = json!(traceparent_for(&id));
    event["consent"] = json!(consent);
    event["data"]["body"] = json!(format!("{body} [{marker}]"));
    event["data"]["attachments"][0]["caption"] = json!(format!("caption-{marker}"));
    // A quoted excerpt: somebody else's words inside this message (ADR 0012),
    // and one of the things the seam must reduce to a boolean.
    event["data"]["reply_to"] = json!({
        "matrix_event_id": "$quotedD206",
        "excerpt": format!("quoted-{marker}"),
    });
    validate_against_contract(&event, "inbound.message.received")?;
    Ok(event)
}

fn revoked_message(marker: &str) -> Result<Value> {
    let mut event = contract_variant_fixture("inbound.message.received", "revoked-sender")?;
    let id = sha256_hex(marker);
    event["id"] = json!(id);
    event["traceparent"] = json!(traceparent_for(&id));
    validate_against_contract(&event, "inbound.message.received")?;
    Ok(event)
}

fn event_id(event: &Value) -> String {
    event["id"].as_str().expect("a string id").to_owned()
}

// ---------------------------------------------------------------------------
// 1 and 3. What crosses, and what the persona does not do itself
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_granted_message_wakes_hermes_with_a_narrow_template_and_nothing_else() -> Result<()> {
    let marker = "d206-narrow-template";
    let hermes = StubHermes::start().await?;
    let run = PersonaRun::start_with_hermes(
        "hermes-seam-narrow",
        "a reply this persona must not produce",
        &hermes.route_url(),
    )
    .await?;

    let trigger = inbound_message(marker, "granted", "On décale à 20h ?")?;
    let trigger_id = event_id(&trigger);
    run.publish_inbound(&trigger).await?;

    // Thinking, because oversight has to see activity in real time even when
    // the reasoning happens on another machine.
    run.wait_for(THINKING_TYPE, &trigger_id)
        .await?;

    let wake = harness::poll_until(
        || async {
            let wakes = hermes.wakes();
            (!wakes.is_empty()).then(|| wakes[0].clone())
        },
        "the wake the persona posted to Hermes",
    )
    .await?;

    // --- the body, field by field
    assert_eq!(
        wake.body,
        json!({
            "event_type": "twalk.message.received",
            "template_version": 1,
            "reference": format!("TWALK-REF:{PERSONA_ID}:{trigger_id}:1"),
            "network": "whatsapp",
            "received_at": trigger["time"],
            "message": trigger["data"]["body"],
            "format": "text/plain",
            "quotes_an_earlier_message": true,
            "has_attachments": true,
            "user_language": Value::Null,
        }),
        "the template is a closed list of named fields, and this is it"
    );

    // --- and, the half that matters, what is absent from the bytes on the
    // wire. Hermes's memory absorbs whatever it is shown and nothing expires
    // it, so each of these leaking would be permanent on a machine Twalk may
    // not own (ADR 0028's seven days stop at this hop).
    for (absent, what) in [
        (
            trigger["subject"].as_str().unwrap(),
            "the sender's Matrix ID",
        ),
        (
            trigger["source"].as_str().unwrap(),
            "the portal room the message arrived in",
        ),
        ("Aïcha Benali", "the contact's display name"),
        ("+33612345678", "the network's own identifier for the contact"),
        (
            &format!("quoted-{marker}"),
            "a quoted excerpt, which belongs to its author (ADR 0012)",
        ),
        (&format!("caption-{marker}"), "an attachment's caption"),
        (
            "aWF6-32KGYaC3A_FEUCk1Bt0JA37zP0wrStgmdCaW-0",
            "an attachment's decryption material",
        ),
        (
            trigger["traceparent"].as_str().unwrap(),
            "the deployment's own trace",
        ),
    ] {
        assert!(
            !wake.raw.contains(absent),
            "{what} crossed the seam to Hermes: {}",
            wake.raw
        );
    }

    // The trigger's id *does* cross, inside the reference, and that is the
    // deliberate exception: it is how the answer finds its way home, it is an
    // opaque digest this deployment gave itself, and it names nobody. Every
    // other identity above is withheld.

    // --- the delivery id is the suggestion's own deterministic id, so that
    // Hermes's idempotency cache and the bus's deduplication cannot disagree.
    assert_eq!(
        wake.delivery_id,
        sha256_hex(&format!("{PERSONA_ID}:{trigger_id}:1")),
        "the wake's idempotency key is the contract's natural key for the \
         suggestion it will produce"
    );

    // --- and the persona proposed nothing. The suggestion comes back through
    // the Companion Gateway (ADR 0032), so this process publishing one would
    // mean two answers to one message.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(
        run.published(SUGGEST_TYPE).await?.is_empty(),
        "the persona published a suggestion of its own while Hermes was reasoning"
    );
    assert!(
        run.llm_requests_mentioning(marker).is_empty(),
        "the persona also asked the model: two brains, one message, and \
         whichever answered first wins"
    );
    // Not the language ask either (ADR 0031): the disclosure on this path is
    // the Companion Gateway's to select from the language Hermes declares in
    // its answer, so a persona that handed the message on has no reply to
    // ask the language of — and a request here would be a request made,
    // about text this process never drafted.
    assert_eq!(
        run.llm.request_count(),
        0,
        "the persona sent the model nothing at all, not a reply and not a language \
         ask: {:?}",
        run.llm.requests()
    );
    let logs = run.logs().await?;
    assert!(
        logs.contains("handed to hermes") && logs.contains(&trigger_id),
        "the log says the trigger was handed on rather than that there was \
         nothing to suggest: {logs}"
    );

    assert_eq!(hermes.refusals(), 0, "Hermes refused the persona's signature");
    run.shutdown().await
}

// ---------------------------------------------------------------------------
// 2. The consent gate, before anything crosses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_consent_gate_runs_before_anything_reaches_hermes() -> Result<()> {
    let hermes = StubHermes::start().await?;
    let run = PersonaRun::start_with_hermes(
        "hermes-seam-consent",
        "a reply nothing here may produce",
        &hermes.route_url(),
    )
    .await?;

    // Three messages a persona may not process, and one it may — published
    // last, so that waiting for *its* wake proves the other three had their
    // chance and were dropped. A bare sleep would prove only that the test
    // waited.
    let pending = inbound_message("d206-gate-pending", "pending", "personne n'a décidé")?;
    let revoked = revoked_message("d206-gate-revoked")?;
    let owners_own = {
        let mut event = contract_fixture("outbound.message.sent")?;
        let id = sha256_hex("d206-gate-owner");
        event["id"] = json!(id);
        validate_against_contract(&event, "outbound.message.sent")?;
        event
    };
    let granted = inbound_message("d206-gate-granted", "granted", "et celui-ci est accordé")?;
    let granted_id = event_id(&granted);

    run.publish_inbound(&pending).await?;
    run.publish_inbound(&revoked).await?;
    run.publish_typed(&owners_own).await?;
    run.publish_inbound(&granted).await?;

    let wake = harness::poll_until(
        || async {
            let wakes = hermes.wakes();
            (!wakes.is_empty()).then(|| wakes[0].clone())
        },
        "the wake the granted message produced",
    )
    .await?;
    assert!(
        wake.body["reference"]
            .as_str()
            .unwrap_or_default()
            .contains(&granted_id),
        "the first thing to reach Hermes is the granted message: {wake:?}"
    );
    assert_eq!(
        hermes.wakes().len(),
        1,
        "something other than the granted message reached Hermes: {:?}",
        hermes.wakes()
    );
    // And not refused at the door either: a request Hermes turned away is a
    // request that was made, and this project's promise is that it was not.
    assert_eq!(
        hermes.refusals(),
        0,
        "a message the consent gate should have dropped was posted and refused"
    );
    // Nothing of theirs is anywhere in the bytes Hermes was shown.
    let raw = hermes
        .wakes()
        .iter()
        .map(|wake| wake.raw.clone())
        .collect::<String>();
    for marker in ["personne n'a décidé", "d206-gate-pending", "d206-gate-revoked"] {
        assert!(!raw.contains(marker), "{marker} crossed the consent gate: {raw}");
    }
    run.shutdown().await
}

// ---------------------------------------------------------------------------
// 4. A redelivered trigger is one wake
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_redelivered_trigger_is_one_wake_and_not_a_second_agent_run() -> Result<()> {
    let hermes = StubHermes::start().await?;
    let run = PersonaRun::start_with_hermes(
        "hermes-seam-redelivery",
        "a reply nothing here may produce",
        &hermes.route_url(),
    )
    .await?;

    let trigger = inbound_message("d206-redelivery", "granted", "On décale à 20h ?")?;
    let trigger_id = event_id(&trigger);
    run.publish_inbound(&trigger).await?;
    harness::poll_until(
        || async { (!hermes.wakes().is_empty()).then_some(()) },
        "the first wake",
    )
    .await?;
    run.redeliver_inbound(&trigger).await?;

    // The bus absorbs the redelivery, so the persona may not even see it. What
    // this asserts is the invariant that holds either way: whatever reaches
    // Hermes carries **one** delivery id, so a second run is impossible.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let expected = sha256_hex(&format!("{PERSONA_ID}:{trigger_id}:1"));
    for wake in hermes.wakes() {
        assert_eq!(
            wake.delivery_id, expected,
            "a redelivery produced a wake under another key, which would be a \
             second agent run and a second draft of one message"
        );
    }
    let ran: Vec<_> = hermes
        .wakes()
        .into_iter()
        .filter(|wake| !wake.duplicate)
        .collect();
    assert_eq!(
        ran.len(),
        1,
        "Hermes was asked to reason about one message {} times",
        ran.len()
    );
    run.shutdown().await
}

// ---------------------------------------------------------------------------
// 5. Hermes unreachable does not stop the deployment
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_hermes_that_is_not_there_does_not_stop_the_persona() -> Result<()> {
    // The shape the Sensor already uses for the Gateway's consent snapshot,
    // copied rather than reinvented: start anyway, `ERROR` naming the URL and
    // what happens instead, retry in the background.
    let run = PersonaRun::start_with_hermes(
        "hermes-seam-unreachable",
        "a reply nothing here may produce",
        UNREACHABLE_HERMES_URL,
    )
    .await?;

    // `start_with_hermes` waits for the persona to report itself ready, so
    // reaching this line at all is the first half of the assertion: a persona
    // whose Hermes is down still starts, still subscribes, and still holds its
    // place on the bus.
    let logs = run.logs().await?;
    assert!(
        logs.contains("hermes does not answer"),
        "the persona started and said nothing about the seam being down: {logs}"
    );
    assert!(
        logs.contains(UNREACHABLE_HERMES_URL.trim_end_matches("/webhooks/twalk-messages")),
        "the ERROR does not name the URL, which is the one thing an operator \
         needs from it: {logs}"
    );

    // And a message that arrives during the outage is not lost: the trigger is
    // redelivered by the bus rather than acked, so the consumer still owes it.
    let trigger = inbound_message("d206-unreachable", "granted", "On décale à 20h ?")?;
    let trigger_id = event_id(&trigger);
    run.publish_inbound(&trigger).await?;
    run.wait_for(THINKING_TYPE, &trigger_id)
        .await?;
    harness::poll_until(
        || async {
            let logs = run.logs().await.ok()?;
            logs.contains("hermes did not answer at").then_some(())
        },
        "the failed wake, named with its URL",
    )
    .await?;
    assert!(
        run.published(SUGGEST_TYPE).await?.is_empty(),
        "a suggestion was published although Hermes never answered"
    );
    assert_eq!(
        run.published(INBOUND_TYPE).await?.len(),
        1,
        "the trigger is on the bus once"
    );
    run.shutdown().await
}
