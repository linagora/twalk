//! Ticket 07, reactions: a reaction added in an observed room becomes a
//! schema-valid `inbound.reaction.added.v1` CloudEvent on the bus, carrying
//! the reaction key, the target message's event id and excerpt, and the
//! resolved contact. Removing a reaction produces no event.

mod harness;

use anyhow::Result;
use harness::{
    ensure_stack, sensor_env, validate_against_contract, Bot, Bus, SensorProc, SENSOR_USER_ID,
};
use serde_json::json;
use sha2::{Digest, Sha256};

const REACTION_SUBJECT: &str = "twalk.inbound.reaction.added.v1";
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

/// A mautrix-style portal room marker: the bridge identifies the network
/// through an m.bridge state event.
async fn make_whatsapp_portal(bridge: &Bot, name: &str) -> Result<String> {
    let room_id = bridge.create_room(name, false).await?;
    bridge
        .send_state_event(
            &room_id,
            "m.bridge",
            "",
            json!({
                "bridgebot": bridge.user_id(),
                "creator": bridge.user_id(),
                "protocol": { "id": "whatsapp", "displayname": "WhatsApp" },
                "network": { "id": "whatsapp", "displayname": "WhatsApp" },
            }),
        )
        .await?;
    Ok(room_id)
}

/// The bridge creates the portal room, the Sensor joins it, and the reactor
/// (a ghost stood in for by bot_beta) is let in. Returns the room id.
async fn observed_portal_with_reactor(alpha: &Bot, beta: &Bot, name: &str) -> Result<String> {
    let room_id = make_whatsapp_portal(alpha, name).await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    alpha.invite(&room_id, beta.user_id()).await?;
    beta.join_room(&room_id).await?;
    Ok(room_id)
}

#[tokio::test]
async fn a_reaction_becomes_a_schema_valid_cloud_event() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;
    let beta = Bot::login("bot_beta").await?;

    let room_id = observed_portal_with_reactor(&alpha, &beta, "reaction-portal").await?;
    let target_id = alpha.send_message(&room_id, "on décale à 20h ?").await?;
    let reaction_id = beta.send_reaction(&room_id, &target_id, "👍").await?;

    let stored = bus
        .wait_for_room_message(STREAM, REACTION_SUBJECT, &room_id)
        .await?;
    let event = &stored.payload;

    validate_against_contract(event, "inbound.reaction.added")?;

    // The id is derived from the m.reaction event's natural key, computed
    // here independently of the Sensor's own code.
    let expected_id = sha256_hex(&format!("{reaction_id}:{room_id}"));
    assert_eq!(event["id"].as_str(), Some(expected_id.as_str()));
    assert_eq!(
        stored.header("Nats-Msg-Id"),
        Some(expected_id.as_str()),
        "NATS-Msg-Id must equal the CloudEvents id for JetStream dedup"
    );
    assert_eq!(stored.header("network"), Some("whatsapp"));
    assert_eq!(stored.header("consent"), Some("pending"));
    assert_eq!(
        event["source"].as_str(),
        Some(format!("matrix://test.twalk/{room_id}").as_str())
    );
    assert_eq!(
        event["type"].as_str(),
        Some("fr.linagora.twalk.inbound.reaction.added.v1")
    );
    assert_eq!(
        event["subject"].as_str(),
        Some("@bot_beta:test.twalk"),
        "the subject is the reactor, not the message author"
    );
    assert_eq!(event["network"].as_str(), Some("whatsapp"));
    assert_eq!(
        event["consent"].as_str(),
        Some("pending"),
        "unknown senders default to consent pending"
    );
    assert_eq!(event["data"]["reaction"].as_str(), Some("👍"));
    assert_eq!(
        event["data"]["target"]["matrix_event_id"].as_str(),
        Some(target_id.as_str())
    );
    assert_eq!(
        event["data"]["target"]["excerpt"].as_str(),
        Some("on décale à 20h ?"),
        "the excerpt quotes the target message so consumers need no lookup"
    );
    assert_eq!(
        event["data"]["contact"]["display_name"].as_str(),
        Some("bot_beta"),
        "the reactor resolves like a message sender: membership display name, localpart fallback"
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_removed_reaction_produces_no_event() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;
    let beta = Bot::login("bot_beta").await?;

    let room_id = observed_portal_with_reactor(&alpha, &beta, "reaction-removal-portal").await?;
    let target_id = alpha.send_message(&room_id, "rendez-vous demain").await?;
    let reaction_id = beta.send_reaction(&room_id, &target_id, "👍").await?;

    // The addition lands on the bus…
    let stored = bus
        .wait_for_room_message(STREAM, REACTION_SUBJECT, &room_id)
        .await?;
    validate_against_contract(&stored.payload, "inbound.reaction.added")?;

    // …and removing the reaction must not publish anything new.
    beta.redact(&room_id, &reaction_id).await?;
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    let messages = bus
        .fetch_room_messages(STREAM, REACTION_SUBJECT, &room_id)
        .await?;
    assert_eq!(
        messages.len(),
        1,
        "reaction removals produce no v1 event, per the contract"
    );

    sensor.stop().await;
    Ok(())
}
