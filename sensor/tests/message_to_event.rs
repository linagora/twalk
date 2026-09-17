// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Ticket 02, walking skeleton: a plain text message in an observed,
//! unencrypted room becomes a schema-valid `inbound.message.received.v1`
//! CloudEvent on the bus, with a deterministic id and `NATS-Msg-Id` set.

mod harness;

use anyhow::Result;
use harness::{
    ensure_stack, make_whatsapp_portal, sensor_env, sha256_hex, validate_against_contract, Bot,
    Bus, SensorProc, SENSOR_USER_ID,
};
use serde_json::{json, Value};

const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";
const STREAM: &str = "twalk";

#[tokio::test]
async fn text_message_becomes_a_schema_valid_cloud_event() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = make_whatsapp_portal(&alpha, "whatsapp-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha.wait_for_membership(&room_id, SENSOR_USER_ID, "join").await?;

    let matrix_event_id = alpha
        .send_message(&room_id, "on décale à 20h ?")
        .await?;

    let stored = bus
        .wait_for_room_message(STREAM, MESSAGE_SUBJECT, &room_id)
        .await?;
    let event = &stored.payload;

    validate_against_contract(event, "inbound.message.received")?;

    // The id is derived from the contract's natural key, computed here
    // independently of the Sensor's own code.
    let expected_id = sha256_hex(&format!("{matrix_event_id}:{room_id}"));
    assert_eq!(event["id"].as_str(), Some(expected_id.as_str()));
    assert_eq!(
        stored.header("Nats-Msg-Id"),
        Some(expected_id.as_str()),
        "NATS-Msg-Id must equal the CloudEvents id for JetStream dedup"
    );
    assert_eq!(
        event["source"].as_str(),
        Some(format!("matrix://test.twalk/{room_id}").as_str())
    );
    assert_eq!(
        event["type"].as_str(),
        Some("fr.linagora.twalk.inbound.message.received.v1")
    );
    assert_eq!(event["subject"].as_str(), Some("@bot_alpha:test.twalk"));
    assert_eq!(event["network"].as_str(), Some("whatsapp"));
    assert_eq!(
        event["consent"].as_str(),
        Some("pending"),
        "unknown senders default to consent pending"
    );
    assert_eq!(event["data"]["body"].as_str(), Some("on décale à 20h ?"));
    assert_eq!(event["data"]["format"].as_str(), Some("text/plain"));
    assert_eq!(event["data"]["reply_to"], Value::Null);
    assert_eq!(event["data"]["attachments"], json!([]));
    assert_eq!(
        event["data"]["contact"]["display_name"].as_str(),
        Some("bot_alpha")
    );
    assert!(
        event["data"].get("network_timestamp").is_some(),
        "a message in a bridge portal room carries the bridge-reported network timestamp"
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn ghost_prefix_determines_the_network_without_bridge_state() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    // A mautrix puppet: the ghost user the bridge relays messages as.
    let puppet = Bot::login("whatsapp_33612345678").await?;

    // The bridge creates the portal room and invites the Sensor; the ghost
    // joins and speaks. No m.bridge state event this time.
    let alpha = Bot::login("bot_alpha").await?;
    let room_id = alpha.create_room("ghost-portal", false).await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    alpha.invite(&room_id, puppet.user_id()).await?;
    puppet.join_room(&room_id).await?;

    puppet.send_message(&room_id, "hello from a ghost").await?;

    let stored = bus
        .wait_for_room_message(STREAM, MESSAGE_SUBJECT, &room_id)
        .await?;
    let event = &stored.payload;

    validate_against_contract(event, "inbound.message.received")?;
    assert_eq!(
        event["subject"].as_str(),
        Some("@whatsapp_33612345678:test.twalk")
    );
    assert_eq!(
        event["network"].as_str(),
        Some("whatsapp"),
        "the ghost prefix carries the network when no m.bridge state exists"
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_removed_room_stops_producing_events() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = make_whatsapp_portal(&alpha, "removal-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha.wait_for_membership(&room_id, SENSOR_USER_ID, "join").await?;

    alpha.send_message(&room_id, "before the removal").await?;
    let stored = bus
        .wait_for_room_message(STREAM, MESSAGE_SUBJECT, &room_id)
        .await?;
    validate_against_contract(&stored.payload, "inbound.message.received")?;

    alpha.kick(&room_id, SENSOR_USER_ID).await?;
    alpha.wait_for_membership(&room_id, SENSOR_USER_ID, "leave").await?;

    let after_kick_id = alpha.send_message(&room_id, "after the removal").await?;
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    let messages = bus.fetch_room_messages(STREAM, MESSAGE_SUBJECT, &room_id).await?;
    assert_eq!(
        messages.len(),
        1,
        "exactly one event may be published: the room stopped being observed at removal"
    );
    let unexpected_id = sha256_hex(&format!("{after_kick_id}:{room_id}"));
    assert!(
        messages
            .iter()
            .all(|m| m.payload["id"].as_str() != Some(unexpected_id.as_str())),
        "the post-removal message must never reach the bus"
    );

    sensor.stop().await;
    Ok(())
}
