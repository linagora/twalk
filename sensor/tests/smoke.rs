// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Smoke test for the integration-test harness itself (ticket 01).
//!
//! Proves the seam before any Sensor code exists: bots can talk to the
//! test Synapse, the bus round-trips through JetStream, and every contract
//! fixture validates against its schema.

mod harness;

use anyhow::Result;
use harness::{
    contract_fixture, contract_fixture_types, ensure_stack, validate_against_contract, Bot, Bus,
};

#[tokio::test]
async fn bots_can_create_invite_post_and_read_on_synapse() -> Result<()> {
    ensure_stack().await?;
    let alpha = Bot::login("bot_alpha").await?;
    let beta = Bot::login("bot_beta").await?;

    let room_id = alpha.create_room("portal-room", false).await?;
    alpha.invite(&room_id, beta.user_id()).await?;
    beta.join_room(&room_id).await?;

    let event_id = alpha
        .send_message(&room_id, "hello from the harness")
        .await?;

    let event = beta
        .wait_for_event(
            &room_id,
            |event| {
                event.get("type").and_then(|t| t.as_str()) == Some("m.room.message")
                    && event
                        .get("content")
                        .and_then(|c| c.get("body"))
                        .and_then(|b| b.as_str())
                        == Some("hello from the harness")
            },
            "the hello message",
        )
        .await?;
    assert_eq!(
        event.get("event_id").and_then(|id| id.as_str()),
        Some(event_id.as_str()),
        "beta should read back the very event alpha sent"
    );
    Ok(())
}

#[tokio::test]
async fn encrypted_rooms_enable_megolm_from_creation() -> Result<()> {
    ensure_stack().await?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = alpha.create_room("encrypted-portal", true).await?;

    let event = alpha
        .wait_for_event(
            &room_id,
            |event| event.get("type").and_then(|t| t.as_str()) == Some("m.room.encryption"),
            "the m.room.encryption state event",
        )
        .await?;
    assert_eq!(
        event
            .get("content")
            .and_then(|c| c.get("algorithm"))
            .and_then(|a| a.as_str()),
        Some("m.megolm.v1.aes-sha2"),
        "an encrypted portal room must enable Megolm, like a real bridge room"
    );
    Ok(())
}

#[tokio::test]
async fn reaction_and_presence_helpers_work() -> Result<()> {
    ensure_stack().await?;
    let alpha = Bot::login("bot_alpha").await?;
    let beta = Bot::login("bot_beta").await?;

    let room_id = alpha.create_room("reactions", false).await?;
    alpha.invite(&room_id, beta.user_id()).await?;
    beta.join_room(&room_id).await?;

    let target = alpha.send_message(&room_id, "on décale à 20h ?").await?;
    beta.send_reaction(&room_id, &target, "👍").await?;

    alpha
        .wait_for_event(
            &room_id,
            |event| {
                event.get("type").and_then(|t| t.as_str()) == Some("m.reaction")
                    && event
                        .pointer("/content/m.relates_to/key")
                        .and_then(|k| k.as_str())
                        == Some("👍")
                    && event
                        .pointer("/content/m.relates_to/event_id")
                        .and_then(|id| id.as_str())
                        == Some(target.as_str())
            },
            "beta's 👍 reaction",
        )
        .await?;

    beta.set_presence("online").await?;
    assert_eq!(
        alpha.get_presence(beta.user_id()).await?,
        "online",
        "alpha should see beta's presence after the update"
    );
    Ok(())
}

#[tokio::test]
async fn bus_round_trips_through_jetstream() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    // Harness self-test streams live outside the twalk.* namespace, which
    // belongs to the Sensor's own stream (JetStream forbids overlapping
    // subjects across streams).
    bus.ensure_stream("harness-smoke", &["harness.smoke.>"])
        .await?;

    let payload = serde_json::json!({ "ping": "from the harness" });
    bus.publish("harness.smoke.ping", &payload).await?;

    let stored = bus
        .last_message("harness-smoke", "harness.smoke.ping")
        .await?
        .expect("the message must be stored by JetStream");
    assert_eq!(
        stored, payload,
        "the bus must return exactly what was published"
    );

    // Consuming a subject yields exactly its own messages, in order.
    // Subjects are suffixed per run: JetStream persists across test runs,
    // so reusing fixed subjects would leak messages between runs.
    bus.ensure_stream("harness-consume", &["harness.consume.>"])
        .await?;
    let run_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let wanted = serde_json::json!({ "n": 1 });
    let other = serde_json::json!({ "n": 99 });
    bus.publish(&format!("harness.consume.{run_id}.alpha"), &wanted)
        .await?;
    bus.publish(&format!("harness.consume.{run_id}.beta"), &other)
        .await?;
    let consumed = bus
        .fetch_all(
            "harness-consume",
            &format!("harness.consume.{run_id}.alpha"),
        )
        .await?;
    assert_eq!(
        consumed,
        vec![wanted],
        "consume must return exactly the subject's messages"
    );
    Ok(())
}

#[tokio::test]
async fn every_contract_fixture_validates_against_its_schema() -> Result<()> {
    ensure_stack().await?;
    let types = contract_fixture_types()?;
    assert_eq!(
        types.len(),
        8,
        "the v1 contract defines exactly 8 fixture types; found {types:?}"
    );
    for type_name in types {
        let fixture = contract_fixture(&type_name)?;
        validate_against_contract(&fixture, &type_name)?;
    }
    Ok(())
}
