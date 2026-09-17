//! Ticket 08, presence: when a bridge puppet sharing an observed portal
//! room changes presence, the Sensor publishes a schema-valid
//! `inbound.presence.updated.v1` CloudEvent on the bus, with the
//! deterministic id derived from the schema's documented natural key —
//! and the rest of the pipeline keeps flowing.

mod harness;

use std::time::Duration;

use anyhow::Result;
use harness::{
    ensure_stack, poll_until, sensor_env, validate_against_contract, Bot, Bus, SensorProc,
    SENSOR_USER_ID,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::time::sleep;

const PRESENCE_SUBJECT: &str = "twalk.inbound.presence.updated.v1";
const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";
const STREAM: &str = "twalk";

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test]
async fn puppet_presence_becomes_a_schema_valid_cloud_event() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;
    // A puppet-style user: the localpart carries no network prefix, so the
    // room's m.bridge state alone attributes the network.
    let puppet = Bot::login("bot_beta").await?;

    // The stack and the bot accounts persist across runs. Matrix presence
    // updates are not room-scoped, so the Sensor picks one observed room
    // shared with the user as the event's source; dropping stale
    // memberships keeps that choice deterministic (this run's portal only).
    for stale_room in puppet.joined_rooms().await? {
        puppet.leave_room(&stale_room).await?;
    }

    let room_id = alpha.create_room("presence-portal", false).await?;
    alpha
        .send_state_event(
            &room_id,
            "m.bridge",
            "",
            json!({
                "bridgebot": alpha.user_id(),
                "creator": alpha.user_id(),
                "protocol": { "id": "whatsapp", "displayname": "WhatsApp" },
                "network": { "id": "whatsapp", "displayname": "WhatsApp" },
            }),
        )
        .await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha.wait_for_membership(&room_id, SENSOR_USER_ID, "join").await?;
    alpha.invite(&room_id, puppet.user_id()).await?;
    puppet.join_room(&room_id).await?;

    // Synapse only broadcasts presence *transitions*, and the puppet may be
    // online already from a previous run: toggle the state on every poll
    // round so a fresh online transition is always emitted, and so a
    // transition racing the presence-routing update after the join is
    // simply retried.
    let stored = poll_until(
        || async {
            puppet.set_presence("offline").await.ok()?;
            sleep(Duration::from_millis(250)).await;
            puppet.set_presence("online").await.ok()?;
            sleep(Duration::from_millis(250)).await;
            bus.fetch_room_messages(STREAM, PRESENCE_SUBJECT, &room_id)
                .await
                .ok()?
                .into_iter()
                .find(|message| {
                    message.payload["subject"].as_str() == Some(puppet.user_id())
                        && message.payload["data"]["presence"].as_str() == Some("online")
                })
        },
        "waiting for the puppet's online presence event",
    )
    .await?;
    let event = &stored.payload;

    validate_against_contract(event, "inbound.presence.updated")?;

    assert_eq!(
        event["type"].as_str(),
        Some("fr.linagora.twalk.inbound.presence.updated.v1")
    );
    assert_eq!(event["subject"].as_str(), Some("@bot_beta:test.twalk"));
    assert_eq!(
        event["source"].as_str(),
        Some(format!("matrix://test.twalk/{room_id}").as_str()),
        "the observed room shared with the puppet is the event's source"
    );
    assert_eq!(event["network"].as_str(), Some("whatsapp"));
    assert_eq!(
        event["consent"].as_str(),
        Some("pending"),
        "unknown contacts default to consent pending"
    );
    assert_eq!(event["data"]["presence"].as_str(), Some("online"));
    assert_eq!(
        event["data"]["contact"]["display_name"].as_str(),
        Some("bot_beta")
    );

    // The id follows the schema's documented natural key,
    // sha256(matrix_user_id + ':' + presence + ':' + receipt_timestamp_ms),
    // recomputed here independently of the Sensor's own code. The event's
    // `time` is the receipt instant the natural key is derived from.
    let produced_at = OffsetDateTime::parse(event["time"].as_str().unwrap(), &Rfc3339)?;
    let receipt_timestamp_ms = produced_at.unix_timestamp_nanos() / 1_000_000;
    let expected_id = sha256_hex(&format!("@bot_beta:test.twalk:online:{receipt_timestamp_ms}"));
    assert_eq!(event["id"].as_str(), Some(expected_id.as_str()));
    assert_eq!(
        stored.header("Nats-Msg-Id"),
        Some(expected_id.as_str()),
        "NATS-Msg-Id must equal the CloudEvents id for JetStream dedup"
    );

    // Presence is best-effort and additive: the message pipeline keeps
    // flowing for the same puppet in the same room.
    puppet.send_message(&room_id, "still here?").await?;
    let message = bus
        .wait_for_room_message(STREAM, MESSAGE_SUBJECT, &room_id)
        .await?;
    validate_against_contract(&message.payload, "inbound.message.received")?;
    assert_eq!(
        message.payload["subject"].as_str(),
        Some("@bot_beta:test.twalk")
    );

    sensor.stop().await;
    Ok(())
}
