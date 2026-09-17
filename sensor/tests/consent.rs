//! Ticket 05, contact resolution and consent labelling: the Sensor keeps an
//! in-memory consent cache fed by a durable consumer on
//! `twalk.consent.state.changed.v1`, so every published event carries the
//! sender's CURRENT consent state. Unknown senders label `pending`; a
//! granted contact exposes its `contact.network_identifier` (derived from
//! the ghost localpart); a consent change on the bus relabels that sender's
//! subsequent events — on the message, reaction and presence paths alike.

mod harness;

use anyhow::Result;
use harness::{
    contract_fixture, ensure_stack, make_whatsapp_portal, poll_until, sensor_env, sha256_hex,
    validate_against_contract, Bot, Bus, SensorProc, StoredMessage, SENSOR_USER_ID,
};
use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const STREAM: &str = "twalk";
const CONSENT_SUBJECT: &str = "twalk.consent.state.changed.v1";
const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";
const REACTION_SUBJECT: &str = "twalk.inbound.reaction.added.v1";

/// A fresh CloudEvents id per consent event: the bus persists across runs,
/// so reusing the fixture id would collide in JetStream dedup.
fn unique_event_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    sha256_hex(&format!("{nanos}:{}", COUNTER.fetch_add(1, Ordering::Relaxed)))
}

/// A contact-scoped `consent.state.changed` event patched from the contract
/// fixture — what the Companion Gateway, the single writer of consent state
/// (ADR 0006), will publish; the test bus stands in for it. The patched
/// event is re-validated so the test stays a contract citizen.
fn consent_change(
    subject_id: &str,
    networks: &[&str],
    old_state: &str,
    new_state: &str,
) -> Result<Value> {
    let mut event = contract_fixture("consent.state.changed")?;
    let now = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
    event["id"] = json!(unique_event_id());
    event["time"] = json!(now);
    event["subject"] = json!(subject_id);
    event["consent"] = json!(new_state);
    // The top-level network extension is only set when the change targets
    // exactly one network, per the schema.
    if let [network] = networks {
        event["network"] = json!(network);
    } else {
        event.as_object_mut().unwrap().remove("network");
    }
    event["data"]["subject"] = json!({ "type": "contact", "id": subject_id });
    event["data"]["old_state"] = json!(old_state);
    event["data"]["new_state"] = json!(new_state);
    event["data"]["scope"]["networks"] = json!(networks);
    event["data"]["occurred_at"] = json!(now);
    validate_against_contract(&event, "consent.state.changed")?;
    Ok(event)
}

/// Polls until the bus holds the event produced from one Matrix event,
/// found by recomputing the contract's deterministic id independently of
/// the Sensor's own code.
async fn wait_for_matrix_event(
    bus: &Bus,
    room_id: &str,
    matrix_event_id: &str,
) -> Result<StoredMessage> {
    let expected_id = sha256_hex(&format!("{matrix_event_id}:{room_id}"));
    poll_until(
        || async {
            bus.fetch_room_messages(STREAM, MESSAGE_SUBJECT, room_id)
                .await
                .ok()?
                .into_iter()
                .find(|m| m.payload["id"].as_str() == Some(expected_id.as_str()))
        },
        &format!("the stored event for {matrix_event_id}"),
    )
    .await
}

#[tokio::test]
async fn granted_consent_relabels_events_and_reveals_the_network_identifier() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;
    // A mautrix puppet: the ghost user the bridge relays the contact as. Its
    // localpart carries the network identifier.
    let puppet = Bot::login("whatsapp_33612345678").await?;

    let room_id = make_whatsapp_portal(&alpha, "consent-grant-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha.wait_for_membership(&room_id, SENSOR_USER_ID, "join").await?;
    alpha.invite(&room_id, puppet.user_id()).await?;
    puppet.join_room(&room_id).await?;

    // Unknown sender: labelled pending, and no network identifier leaks.
    puppet.send_message(&room_id, "one").await?;
    let first = bus
        .wait_for_room_message(STREAM, MESSAGE_SUBJECT, &room_id)
        .await?;
    validate_against_contract(&first.payload, "inbound.message.received")?;
    assert_eq!(
        first.payload["consent"].as_str(),
        Some("pending"),
        "unknown senders default to consent pending"
    );
    assert_eq!(first.header("consent"), Some("pending"));
    assert!(
        first.payload["data"]["contact"].get("network_identifier").is_none(),
        "a pending contact's network identifier never leaves the Sensor"
    );

    // The user grants the contact on whatsapp; the Companion Gateway
    // publishes the decision (the test bus stands in for it).
    bus.publish(
        CONSENT_SUBJECT,
        &consent_change(puppet.user_id(), &["whatsapp"], "pending", "granted")?,
    )
    .await?;

    // The consent consumer and the sync loop race, so resend until the
    // label flips — the harness's established pattern for racing producers
    // (presence.rs does the same with presence transitions).
    let relabelled = poll_until(
        || async {
            puppet.send_message(&room_id, "two").await.ok()?;
            bus.fetch_room_messages(STREAM, MESSAGE_SUBJECT, &room_id)
                .await
                .ok()?
                .into_iter()
                .find(|m| {
                    m.payload["subject"].as_str() == Some(puppet.user_id())
                        && m.payload["consent"].as_str() == Some("granted")
                })
        },
        "an event relabelled granted",
    )
    .await?;
    validate_against_contract(&relabelled.payload, "inbound.message.received")?;
    assert_eq!(
        relabelled.payload["data"]["contact"]["network_identifier"].as_str(),
        Some("+33612345678"),
        "a granted ghost contact exposes its network identifier, the `+` restored"
    );
    assert_eq!(relabelled.header("consent"), Some("granted"));

    // From then on the label is stable — no race left: the cache holds the
    // state before this message is sent.
    let third_id = puppet.send_message(&room_id, "three").await?;
    let third = wait_for_matrix_event(&bus, &room_id, &third_id).await?;
    validate_against_contract(&third.payload, "inbound.message.received")?;
    assert_eq!(third.payload["consent"].as_str(), Some("granted"));
    assert_eq!(
        third.payload["data"]["contact"]["network_identifier"].as_str(),
        Some("+33612345678")
    );

    // The reaction path labels from the same cache.
    puppet.send_reaction(&room_id, &third_id, "👍").await?;
    let reaction = bus
        .wait_for_room_message(STREAM, REACTION_SUBJECT, &room_id)
        .await?;
    validate_against_contract(&reaction.payload, "inbound.reaction.added")?;
    assert_eq!(reaction.payload["subject"].as_str(), Some(puppet.user_id()));
    assert_eq!(reaction.payload["consent"].as_str(), Some("granted"));
    assert_eq!(
        reaction.payload["data"]["contact"]["network_identifier"].as_str(),
        Some("+33612345678")
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn revoked_consent_relabels_subsequent_events() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = make_whatsapp_portal(&alpha, "consent-revoke-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha.wait_for_membership(&room_id, SENSOR_USER_ID, "join").await?;

    alpha.send_message(&room_id, "before the decision").await?;
    let first = bus
        .wait_for_room_message(STREAM, MESSAGE_SUBJECT, &room_id)
        .await?;
    validate_against_contract(&first.payload, "inbound.message.received")?;
    assert_eq!(
        first.payload["consent"].as_str(),
        Some("pending"),
        "unknown senders default to consent pending"
    );

    // The user revokes the contact on whatsapp.
    bus.publish(
        CONSENT_SUBJECT,
        &consent_change(alpha.user_id(), &["whatsapp"], "pending", "revoked")?,
    )
    .await?;

    let relabelled = poll_until(
        || async {
            alpha.send_message(&room_id, "after the decision").await.ok()?;
            bus.fetch_room_messages(STREAM, MESSAGE_SUBJECT, &room_id)
                .await
                .ok()?
                .into_iter()
                .find(|m| {
                    m.payload["subject"].as_str() == Some(alpha.user_id())
                        && m.payload["consent"].as_str() == Some("revoked")
                })
        },
        "an event relabelled revoked",
    )
    .await?;
    validate_against_contract(&relabelled.payload, "inbound.message.received")?;
    assert_eq!(relabelled.header("consent"), Some("revoked"));

    // The label is stable from then on.
    let later_id = alpha.send_message(&room_id, "still revoked").await?;
    let later = wait_for_matrix_event(&bus, &room_id, &later_id).await?;
    validate_against_contract(&later.payload, "inbound.message.received")?;
    assert_eq!(later.payload["consent"].as_str(), Some("revoked"));

    sensor.stop().await;
    Ok(())
}
