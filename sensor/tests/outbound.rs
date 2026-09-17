// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Ticket 09, outbound — approved replies: a `persona.reply.approved.v1`
//! event on the bus makes the Sensor post the final, approved content into
//! the target portal room, threaded under the original message. Sends that
//! keep failing are retried with exponential backoff and then dead-lettered
//! — an approved reply is never silently dropped. The bridge echo of the
//! sent reply flows back as a normal inbound event, closing the audit trail.

mod harness;

use anyhow::Result;
use harness::{
    contract_fixture, ensure_stack, make_whatsapp_portal, poll_until, sensor_env,
    validate_against_contract, Bot, Bus, SensorProc, SENSOR_USER_ID,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const STREAM: &str = "twalk";
const REPLY_APPROVED_SUBJECT: &str = "twalk.persona.reply.approved.v1";
const DEAD_LETTER_SUBJECT: &str = "twalk.persona.reply.approved.v1.dead";
const MESSAGE_RECEIVED_SUBJECT: &str = "twalk.inbound.message.received.v1";

/// Short retry delays keep the backoff schedule observable within the test
/// timeout; three attempts are enough to watch the retries be exhausted.
fn outbound_sensor_env() -> Vec<(String, String)> {
    let mut env = sensor_env();
    env.push(("SENSOR_SEND_RETRY_BASE_MS".to_owned(), "100".to_owned()));
    env.push(("SENSOR_SEND_RETRY_MAX_ATTEMPTS".to_owned(), "3".to_owned()));
    env
}

/// A fresh CloudEvents id per published event: the bus persists across runs,
/// so reusing the fixture id would leak dead-lettered events between runs.
fn unique_event_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_string().as_bytes());
    hasher.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_string().as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The contract fixture with its placeholders patched to real test values,
/// re-validated so the published event stays a contract event.
fn approved_reply(room_id: &str, reply_to_event_id: &str) -> Result<Value> {
    let mut event = contract_fixture("persona.reply.approved")?;
    event["id"] = json!(unique_event_id());
    event["data"]["target"]["room_id"] = json!(room_id);
    event["data"]["target"]["reply_to_event_id"] = json!(reply_to_event_id);
    validate_against_contract(&event, "persona.reply.approved")?;
    Ok(event)
}

#[tokio::test]
async fn an_approved_reply_is_posted_as_a_threaded_reply_and_echoes_back() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&outbound_sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = make_whatsapp_portal(&alpha, "outbound-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha.wait_for_membership(&room_id, SENSOR_USER_ID, "join").await?;

    let original_event_id = alpha.send_message(&room_id, "on décale à 20h ?").await?;

    let approved = approved_reply(&room_id, &original_event_id)?;
    let final_body = approved["data"]["final"]["body"]
        .as_str()
        .unwrap()
        .to_owned();
    bus.publish_event(REPLY_APPROVED_SUBJECT, &approved).await?;

    // The Sensor posts the final content as a native reply to the original
    // message — never the raw suggestion.
    let posted = alpha
        .wait_for_event(
            &room_id,
            |event| {
                event.get("sender").and_then(|s| s.as_str()) == Some(SENSOR_USER_ID)
                    && event.pointer("/content/msgtype").and_then(|t| t.as_str())
                        == Some("m.text")
            },
            "the approved reply posted by the sensor",
        )
        .await?;
    assert_eq!(
        posted.pointer("/content/body").and_then(|b| b.as_str()),
        Some(final_body.as_str()),
        "the posted content must be the final, approved content"
    );
    assert_eq!(
        posted
            .pointer("/content/m.relates_to/m.in_reply_to/event_id")
            .and_then(|id| id.as_str()),
        Some(original_event_id.as_str()),
        "the posted message must thread under the original inbound message"
    );

    // The bridge carries the message to the network, and its echo flows back
    // through the Sensor as a normal inbound event (the bot stands in for
    // the bridge echo; the Sensor's own sends never loop back).
    alpha.send_message(&room_id, &final_body).await?;
    let echo = poll_until(
        || async {
            bus.fetch_room_messages(STREAM, MESSAGE_RECEIVED_SUBJECT, &room_id)
                .await
                .ok()?
                .into_iter()
                .find(|m| {
                    m.payload.pointer("/data/body").and_then(|b| b.as_str())
                        == Some(final_body.as_str())
                })
        },
        "the bridge echo on the inbound subject",
    )
    .await?;
    validate_against_contract(&echo.payload, "inbound.message.received")?;

    // Exactly one outbound post: no duplicate sends from the retry machinery.
    let events = alpha.room_events(&room_id, 50).await?;
    let sensor_posts = events
        .iter()
        .filter(|event| {
            event.get("sender").and_then(|s| s.as_str()) == Some(SENSOR_USER_ID)
                && event.pointer("/content/body").and_then(|b| b.as_str())
                    == Some(final_body.as_str())
        })
        .count();
    assert_eq!(sensor_posts, 1, "the approved reply must be posted exactly once");

    sensor.stop().await;
    Ok(())
}

/// The dead-letter copy lives in the same stream as the approved reply, so it
/// needs its own — still stable — message id: reusing the event id would make
/// the bus drop it as a duplicate of the original publish. The event id stays
/// visible in its own header, next to the message-flow extensions.
fn assert_dead_letter_headers(dead: &harness::StoredMessage, approved_id: &str) {
    assert_eq!(
        dead.header("Nats-Msg-Id"),
        Some(format!("{approved_id}:dead-letter").as_str()),
        "the dead-letter copy carries a derived, stable message id"
    );
    assert_eq!(
        dead.header("event-id"),
        Some(approved_id),
        "the dead-letter copy keeps the original event id visible"
    );
    assert_eq!(dead.header("network"), Some("whatsapp"));
    assert_eq!(dead.header("consent"), Some("granted"));
}

/// A reply that can never be posted is dead-lettered on its first delivery —
/// well within the bus duplicate window of the approved reply's own publish.
#[tokio::test]
async fn a_permanently_unpostable_reply_lands_on_the_dead_letter_subject() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&outbound_sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = make_whatsapp_portal(&alpha, "outbound-markdown").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha.wait_for_membership(&room_id, SENSOR_USER_ID, "join").await?;
    let original_event_id = alpha.send_message(&room_id, "tu peux confirmer ?").await?;

    // Markdown is a contract format the Sensor does not render yet.
    let mut approved = approved_reply(&room_id, &original_event_id)?;
    approved["data"]["final"]["format"] = json!("text/markdown");
    validate_against_contract(&approved, "persona.reply.approved")?;
    let approved_id = approved["id"].as_str().unwrap().to_owned();
    bus.publish_event(REPLY_APPROVED_SUBJECT, &approved).await?;

    let dead = poll_until(
        || async {
            bus.fetch_all_with_headers(STREAM, DEAD_LETTER_SUBJECT)
                .await
                .ok()?
                .into_iter()
                .find(|m| m.payload["id"].as_str() == Some(approved_id.as_str()))
        },
        "the markdown reply on the dead-letter subject",
    )
    .await?;
    assert_eq!(dead.payload, approved, "the dead-letter copy is the approved reply, unchanged");
    assert_dead_letter_headers(&dead, &approved_id);

    // The Sensor is a member here (its join is in the timeline): check that
    // it never posted a message.
    let events = alpha.room_events(&room_id, 50).await?;
    assert!(
        events.iter().all(|event| {
            event.get("sender").and_then(|s| s.as_str()) != Some(SENSOR_USER_ID)
                || event.get("type").and_then(|t| t.as_str()) != Some("m.room.message")
        }),
        "an unpostable reply must never reach the room"
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn an_undeliverable_reply_lands_on_the_dead_letter_subject() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&outbound_sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    // A room the Sensor is not — and never will be — a member of.
    let room_id = alpha.create_room("not-observed", false).await?;

    let approved = approved_reply(&room_id, "$AbCdEfGh1234")?;
    let approved_id = approved["id"].as_str().unwrap().to_owned();
    bus.publish_event(REPLY_APPROVED_SUBJECT, &approved).await?;

    // After the retries are exhausted the event moves to the dead-letter
    // subject instead of being silently dropped.
    let dead = poll_until(
        || async {
            bus.fetch_all_with_headers(STREAM, DEAD_LETTER_SUBJECT)
                .await
                .ok()?
                .into_iter()
                .find(|m| m.payload["id"].as_str() == Some(approved_id.as_str()))
        },
        "the event on the dead-letter subject",
    )
    .await?;
    assert_eq!(
        dead.payload["data"]["target"]["room_id"].as_str(),
        Some(room_id.as_str()),
        "the dead-lettered event is the undeliverable approved reply"
    );
    assert_dead_letter_headers(&dead, &approved_id);

    // And nothing was ever posted to the room.
    let events = alpha.room_events(&room_id, 50).await?;
    assert!(
        events.iter().all(|event| {
            event.get("sender").and_then(|s| s.as_str()) != Some(SENSOR_USER_ID)
        }),
        "the Sensor must never post into a room it is not a member of"
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_reply_the_homeserver_rejects_is_retried_then_dead_lettered() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&outbound_sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    // The Sensor is a joined member of the portal room, but the room's power
    // levels forbid it to post: every Sensor-side check passes and only the
    // homeserver refuses the send.
    let room_id = make_whatsapp_portal(&alpha, "outbound-forbidden").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha.wait_for_membership(&room_id, SENSOR_USER_ID, "join").await?;
    let mut power_levels = alpha
        .get_state_event(&room_id, "m.room.power_levels", "")
        .await?;
    power_levels["events_default"] = json!(50);
    power_levels["events"]["m.room.message"] = json!(50);
    alpha
        .send_state_event(&room_id, "m.room.power_levels", "", power_levels)
        .await?;

    let approved = approved_reply(&room_id, "$AbCdEfGh1234")?;
    let approved_id = approved["id"].as_str().unwrap().to_owned();
    bus.publish_event(REPLY_APPROVED_SUBJECT, &approved).await?;

    // The rejection must not be swallowed by an early ack: the approval goes
    // through the retry schedule and ends on the dead-letter subject.
    let dead = poll_until(
        || async {
            bus.fetch_all_with_headers(STREAM, DEAD_LETTER_SUBJECT)
                .await
                .ok()?
                .into_iter()
                .find(|m| m.payload["id"].as_str() == Some(approved_id.as_str()))
        },
        "the rejected approval on the dead-letter subject",
    )
    .await?;
    assert_eq!(
        dead.payload["data"]["target"]["room_id"].as_str(),
        Some(room_id.as_str()),
        "the dead-lettered event is the rejected approved reply"
    );
    assert_dead_letter_headers(&dead, &approved_id);

    // It was retried before being dead-lettered, not given up on at once:
    // a forbidden send may succeed once the room's power levels change.
    let logs = sensor.logs().await;
    assert!(
        logs.iter()
            .any(|line| line.contains("approved reply send failed, scheduling a retry")),
        "a send the homeserver rejects must be retried before dead-lettering"
    );

    // And nothing was ever posted to the room.
    let events = alpha.room_events(&room_id, 50).await?;
    assert!(
        events.iter().all(|event| {
            event.get("sender").and_then(|s| s.as_str()) != Some(SENSOR_USER_ID)
                || event.pointer("/content/msgtype").is_none()
        }),
        "the homeserver must have refused every post"
    );

    sensor.stop().await;
    Ok(())
}
