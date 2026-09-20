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
    contract_fixture, contract_fixture_types, contract_schema_types, contract_variant_fixture,
    contract_variant_fixtures, ensure_stack, validate_against_contract, Bot, Bus,
};
use serde_json::{json, Value};

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
        14,
        "the v1 contract defines exactly 14 fixture types; found {types:?}"
    );
    // And every schema has one. The count above is a tripwire for a fixture
    // added or lost; this is the tripwire for a *schema* added without the
    // worked example a third-party producer copies from.
    assert_eq!(
        contract_schema_types()?,
        types,
        "every contract schema must have a fixture, and every fixture a schema"
    );
    for type_name in types {
        let fixture = contract_fixture(&type_name)?;
        validate_against_contract(&fixture, &type_name)?;
    }
    Ok(())
}

#[tokio::test]
async fn every_contract_variant_fixture_validates_against_its_type() -> Result<()> {
    ensure_stack().await?;
    // A variant demonstrates one conditional shape of a type — a shape the
    // schema allows only under a condition — and validates against the same
    // schema as the type's canonical fixture.
    let variants = contract_variant_fixtures()?;
    assert!(
        variants.contains(&(
            "inbound.message.received".to_owned(),
            "revoked-sender".to_owned()
        )),
        "the reduced shape of a revoked sender's message must stay demonstrated by a fixture (ADR 0012); found {variants:?}"
    );
    for (type_name, variant) in variants {
        let fixture = contract_variant_fixture(&type_name, &variant)?;
        validate_against_contract(&fixture, &type_name)?;
    }
    Ok(())
}

/// An event about a connection names it, and the contract refuses one that
/// does not (ADR 0033, #269): the eight `inbound.*`, `outbound.*` and
/// `persona.*` message-flow types require the extension, and so do
/// `connection.status.changed` (#274), whose subject is the connection
/// itself, and the three `calendar.*` types (#280), the owner's own
/// calendar on the calendar connection; the two status types about a
/// bridge and about a decision do not carry it. Both halves are asserted,
/// so a type moved from one list to the other is a change somebody made on
/// purpose.
#[tokio::test]
async fn an_event_about_a_connection_without_the_connection_is_invalid() -> Result<()> {
    ensure_stack().await?;
    for type_name in contract_fixture_types()? {
        let mut fixture = contract_fixture(&type_name)?;
        let names_a_connection = type_name.starts_with("inbound.")
            || type_name.starts_with("outbound.")
            || type_name.starts_with("persona.")
            || type_name.starts_with("connection.")
            || type_name.starts_with("calendar.");
        if names_a_connection {
            let connection = fixture
                .as_object_mut()
                .and_then(|event| event.remove("connection"));
            assert!(
                connection.is_some(),
                "{type_name}: the fixture must carry `connection`"
            );
            assert!(
                validate_against_contract(&fixture, &type_name).is_err(),
                "{type_name}: an event without `connection` must not validate"
            );
        } else {
            assert!(
                fixture.get("connection").is_none(),
                "{type_name}: a status type is about no message and names no connection"
            );
        }
    }
    Ok(())
}

/// The contract does not merely describe the reduction (ADR 0012), it
/// enforces it: an event labelled `revoked` that still carries content is
/// invalid, and one labelled `granted` or `pending` that dropped it is
/// invalid too. That is what makes a third-party producer reduce like the
/// Sensor.
#[tokio::test]
async fn the_contract_enforces_the_two_shapes_of_a_received_message() -> Result<()> {
    ensure_stack().await?;
    const TYPE: &str = "inbound.message.received";
    let revoked = || contract_variant_fixture(TYPE, "revoked-sender");
    let full = || contract_fixture(TYPE);
    validate_against_contract(&revoked()?, TYPE)?;
    validate_against_contract(&full()?, TYPE)?;

    // Content on a revoked sender's event: rejected, field by field.
    for (what, leak) in [
        ("/data/body", json!("on décale à 20h ?")),
        ("/data/reply_to/excerpt", json!("et le cadeau ?")),
        (
            "/data/attachments/0/mxc_uri",
            json!("mxc://matrix.example.com/QWxpY2VQaG90bzIwMjYwOTE3"),
        ),
        ("/data/attachments/0/caption", json!("regarde cette photo")),
    ] {
        let mut event = revoked()?;
        plant(&mut event, what, leak);
        assert!(
            validate_against_contract(&event, TYPE).is_err(),
            "a revoked sender's event must not be allowed to carry {what}"
        );
    }

    // The full shape keeps every requirement it had before the rule, for a
    // granted and for a pending sender alike.
    for consent in ["granted", "pending"] {
        let mut event = full()?;
        event["consent"] = json!(consent);
        if consent != "granted" {
            // Only a granted contact's identifier is ever published.
            event["data"]["contact"]
                .as_object_mut()
                .unwrap()
                .remove("network_identifier");
        }
        validate_against_contract(&event, TYPE)?;
        for what in ["/data/body", "/data/attachments/0/mxc_uri"] {
            let mut stripped = event.clone();
            let (parent, field) = what.rsplit_once('/').unwrap();
            stripped
                .pointer_mut(parent)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(
                validate_against_contract(&stripped, TYPE).is_err(),
                "a {consent} sender's event must still carry {what}"
            );
        }
        let mut without_excerpt = event.clone();
        without_excerpt["data"]["reply_to"] = json!({ "matrix_event_id": "$PaReNt9876" });
        assert!(
            validate_against_contract(&without_excerpt, TYPE).is_err(),
            "a {consent} sender's reply must still quote its parent"
        );
    }
    Ok(())
}

/// Sets the field a JSON pointer names, creating it when the fixture does
/// not have it — which is the point for a reduced fixture.
fn plant(event: &mut Value, pointer: &str, value: Value) {
    let (parent, field) = pointer.rsplit_once('/').expect("a rooted JSON pointer");
    event
        .pointer_mut(parent)
        .expect("the parent object exists in the fixture")
        .as_object_mut()
        .expect("the parent is an object")
        .insert(field.to_owned(), value);
}
