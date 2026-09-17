//! Ticket 03, persistence — resume without replay. With SENSOR_STATE_DIR
//! set, the Sensor keeps its session, sync token and crypto store on disk,
//! so a restarted process resumes the sync instead of re-syncing from
//! scratch: messages sent while it was down are delivered exactly once, and
//! no already-published event is emitted again.
//!
//! The no-re-emit assertions observe the bus with a core NATS subscription
//! (`Bus::subscribe_raw`): JetStream deduplicates republished ids at the
//! storage layer, so only a raw subscription can observe a re-emission.

mod harness;

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use harness::{
    ensure_stack, make_whatsapp_portal, poll_until, sensor_env_with, sha256_hex,
    validate_against_contract, Bot, Bus, SensorProc, StoredMessage, SERVER_NAME, SENSOR_USER_ID,
};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";
const STREAM: &str = "twalk";

/// A fresh state directory path per test: unique per run, and deliberately
/// NOT created beforehand — starting with an empty store must work, so the
/// Sensor creates the directory itself.
fn fresh_state_dir(test_name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "twalk-sensor-state-{test_name}-{}-{unique}",
        std::process::id()
    ))
}

fn env_with_state_dir(state_dir: &std::path::Path) -> Vec<(String, String)> {
    sensor_env_with(&[("SENSOR_STATE_DIR", &state_dir.to_string_lossy())])
}

/// Waits until the bus holds at least `count` events sourced from the room.
async fn wait_for_room_events(
    bus: &Bus,
    room_id: &str,
    count: usize,
) -> Result<Vec<StoredMessage>> {
    poll_until(
        || async {
            let messages = bus
                .fetch_room_messages(STREAM, MESSAGE_SUBJECT, room_id)
                .await
                .ok()?;
            (messages.len() >= count).then_some(messages)
        },
        &format!("waiting for {count} events from {room_id} on {MESSAGE_SUBJECT}"),
    )
    .await
}

/// Every message event published for `room_id` that the raw subscription
/// observed. JetStream dedup is bypassed, so a re-emission shows up here as
/// a second copy of the same id. Only message events are counted: presence
/// updates legitimately pick any shared portal room as their `source`.
fn room_publishes(rx: &mut UnboundedReceiver<Value>, room_id: &str) -> Vec<Value> {
    let expected_source = format!("matrix://{SERVER_NAME}/{room_id}");
    let mut events = Vec::new();
    while let Ok(payload) = rx.try_recv() {
        if payload["source"].as_str() == Some(expected_source.as_str())
            && payload["type"].as_str() == Some("fr.linagora.twalk.inbound.message.received.v1")
        {
            events.push(payload);
        }
    }
    events
}

/// The bus holds exactly the expected events for the room: schema-valid,
/// unique ids matching the deterministic natural keys, expected bodies.
fn assert_bus_events(
    messages: &[StoredMessage],
    room_id: &str,
    matrix_event_ids: &[String],
    bodies: &[&str],
) -> Result<()> {
    assert_eq!(
        messages.len(),
        matrix_event_ids.len(),
        "unexpected event count on the bus"
    );
    let mut expected_ids = HashSet::new();
    for matrix_event_id in matrix_event_ids {
        expected_ids.insert(sha256_hex(&format!("{matrix_event_id}:{room_id}")));
    }
    let mut seen_bodies = HashSet::new();
    for message in messages {
        let event = &message.payload;
        validate_against_contract(event, "inbound.message.received")?;
        let id = event["id"].as_str().unwrap().to_owned();
        assert!(
            expected_ids.remove(&id),
            "unexpected or duplicate event id {id} on the bus"
        );
        assert_eq!(
            message.header("Nats-Msg-Id"),
            Some(id.as_str()),
            "NATS-Msg-Id must equal the CloudEvents id"
        );
        seen_bodies.insert(event["data"]["body"].as_str().unwrap().to_owned());
    }
    let expected_bodies: HashSet<String> = bodies.iter().map(|body| body.to_string()).collect();
    assert_eq!(seen_bodies, expected_bodies);
    Ok(())
}

/// The raw subscription observed exactly one publish per id: the Sensor
/// never re-emitted an event, even across restarts.
fn assert_no_republish(rx: &mut UnboundedReceiver<Value>, room_id: &str, expected_count: usize) {
    let published = room_publishes(rx, room_id);
    let ids: HashSet<&str> = published
        .iter()
        .map(|event| event["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        published.len(),
        expected_count,
        "every event must be published exactly once, got {published:?}"
    );
    assert_eq!(
        ids.len(),
        published.len(),
        "an event id was published more than once: {published:?}"
    );
}

#[tokio::test]
async fn messages_sent_while_down_are_delivered_exactly_once_after_restart() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let mut publishes = bus.subscribe_raw("twalk.>").await?;
    let state_dir = fresh_state_dir("downtime");
    let env = env_with_state_dir(&state_dir);
    let alpha = Bot::login("bot_alpha").await?;

    // First run: fresh start on a state directory that does not exist yet.
    let sensor = SensorProc::start(&env)?;
    let room_id = make_whatsapp_portal(&alpha, "persistence-downtime-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha.wait_for_membership(&room_id, SENSOR_USER_ID, "join").await?;

    let event_a1_id = alpha.send_message(&room_id, "A1: before the restart").await?;
    let event_a2_id = alpha.send_message(&room_id, "A2: still before the restart").await?;
    wait_for_room_events(&bus, &room_id, 2).await?;

    // The state and crypto stores must be on disk by now.
    assert!(
        state_dir.join("matrix-sdk-state.sqlite3").exists(),
        "the state store must be persisted under SENSOR_STATE_DIR"
    );
    assert!(
        state_dir.join("matrix-sdk-crypto.sqlite3").exists(),
        "the crypto store must be persisted under SENSOR_STATE_DIR"
    );

    // Kill the Sensor mid-traffic; a message sent during the downtime must
    // still be delivered, exactly once, after the restart.
    sensor.stop().await;
    let event_b_id = alpha.send_message(&room_id, "B: sent while the sensor was down").await?;

    let sensor = SensorProc::start(&env)?;
    let messages = wait_for_room_events(&bus, &room_id, 3).await?;

    // Let any erroneous re-emission happen before counting publishes.
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;

    assert_bus_events(
        &messages,
        &room_id,
        &[event_a1_id, event_a2_id, event_b_id],
        &[
            "A1: before the restart",
            "A2: still before the restart",
            "B: sent while the sensor was down",
        ],
    )?;
    assert_no_republish(&mut publishes, &room_id, 3);

    sensor.stop().await;
    let _ = std::fs::remove_dir_all(&state_dir);
    Ok(())
}

#[tokio::test]
async fn restarting_mid_traffic_twice_emits_no_duplicate_event_ids() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let mut publishes = bus.subscribe_raw("twalk.>").await?;
    let state_dir = fresh_state_dir("restarts");
    let env = env_with_state_dir(&state_dir);
    let alpha = Bot::login("bot_alpha").await?;

    let sensor = SensorProc::start(&env)?;
    let room_id = make_whatsapp_portal(&alpha, "persistence-restart-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha.wait_for_membership(&room_id, SENSOR_USER_ID, "join").await?;

    let event_1_id = alpha.send_message(&room_id, "one: before the first restart").await?;
    wait_for_room_events(&bus, &room_id, 1).await?;
    sensor.stop().await;

    let sensor = SensorProc::start(&env)?;
    let event_2_id = alpha.send_message(&room_id, "two: after the first restart").await?;
    wait_for_room_events(&bus, &room_id, 2).await?;
    sensor.stop().await;

    let sensor = SensorProc::start(&env)?;
    let event_3_id = alpha.send_message(&room_id, "three: after the second restart").await?;
    let messages = wait_for_room_events(&bus, &room_id, 3).await?;

    // Let any erroneous re-emission happen before counting publishes.
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;

    assert_bus_events(
        &messages,
        &room_id,
        &[event_1_id, event_2_id, event_3_id],
        &[
            "one: before the first restart",
            "two: after the first restart",
            "three: after the second restart",
        ],
    )?;
    assert_no_republish(&mut publishes, &room_id, 3);

    sensor.stop().await;
    let _ = std::fs::remove_dir_all(&state_dir);
    Ok(())
}
