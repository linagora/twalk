// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Ticket 05, contact resolution and consent labelling: the Sensor keeps an
//! in-memory consent cache fed by a durable consumer on
//! `twalk.consent.state.changed.v1`, so every published event carries the
//! sender's CURRENT consent state. Unknown senders label `pending`; a
//! granted contact exposes its `contact.network_identifier` (derived from
//! the ghost localpart); a consent change on the bus relabels that sender's
//! subsequent events — on the message, reaction and presence paths alike.
//!
//! Ticket #51 (issue #16, ADR 0010) adds where that cache comes from on a
//! start: the Companion Gateway's snapshot, read over HTTP and applied before
//! the stream consumer is created at the position it names. The second half
//! of this file is that hand-off — and above all the restart property, which
//! is what issue #16 was.

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
/// The durable consumer the Sensor feeds its consent cache from, as it leaves
/// it on the bus.
const CONSENT_CONSUMER: &str = "sensor-consent-state-changed";
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
    sha256_hex(&format!(
        "{nanos}:{}",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
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
    // The reference deployment's shape (#270): one connection per network,
    // named after it — so the scope's connections are the networks' names.
    event["data"]["scope"]["connections"] = json!(networks);
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
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
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
        first.payload["data"]["contact"]
            .get("network_identifier")
            .is_none(),
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

/// Sends a message repeatedly until an event for this sender carries
/// `label`: the consent consumer and the sync loop race, so the first
/// message after a decision may still be labelled with the previous state
/// (the established harness pattern for racing producers).
async fn wait_for_label(
    bus: &Bus,
    sender: &Bot,
    room_id: &str,
    body: &str,
    label: &str,
) -> Result<StoredMessage> {
    poll_until(
        || async {
            sender.send_message(room_id, body).await.ok()?;
            bus.fetch_room_messages(STREAM, MESSAGE_SUBJECT, room_id)
                .await
                .ok()?
                .into_iter()
                .find(|m| {
                    m.payload["subject"].as_str() == Some(sender.user_id())
                        && m.payload["consent"].as_str() == Some(label)
                })
        },
        &format!("an event relabelled {label}"),
    )
    .await
}

/// ADR 0012: a revoked sender's message is published without its body. The
/// event still proves that a message arrived — same deterministic id, same
/// network, timestamps, room and sender references, same reply and thread
/// relations, attachments still shaped — but nothing that IS content
/// reaches the bus: no body, no reply excerpt, no `mxc://` reference and no
/// caption. The same contact, granted then revoked, produces a full event
/// and then a reduced one; both are schema-valid.
#[tokio::test]
async fn a_revoked_senders_message_is_published_without_its_body() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = make_whatsapp_portal(&alpha, "consent-reduction-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;

    // Granted: the contact's message reaches the bus in full.
    bus.publish(
        CONSENT_SUBJECT,
        &consent_change(alpha.user_id(), &["whatsapp"], "pending", "granted")?,
    )
    .await?;
    wait_for_label(&bus, &alpha, &room_id, "avant la décision", "granted").await?;

    let parent_id = alpha.send_message(&room_id, "et le cadeau ?").await?;
    let granted_reply_id = alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.text",
                "body": "on décale à 20h ?",
                "m.relates_to": { "m.in_reply_to": { "event_id": parent_id } },
            }),
        )
        .await?;
    let granted = wait_for_matrix_event(&bus, &room_id, &granted_reply_id).await?;
    validate_against_contract(&granted.payload, "inbound.message.received")?;
    assert_eq!(granted.payload["consent"].as_str(), Some("granted"));
    assert_eq!(
        granted.payload["data"]["body"].as_str(),
        Some("on décale à 20h ?"),
        "a granted contact's message keeps its body"
    );
    assert_eq!(
        granted.payload["data"]["reply_to"]["excerpt"].as_str(),
        Some("et le cadeau ?"),
        "a granted contact's reply quotes its parent"
    );

    // The user revokes the same contact on the same network.
    bus.publish(
        CONSENT_SUBJECT,
        &consent_change(alpha.user_id(), &["whatsapp"], "granted", "revoked")?,
    )
    .await?;
    let reduced = wait_for_label(&bus, &alpha, &room_id, "après la décision", "revoked").await?;
    validate_against_contract(&reduced.payload, "inbound.message.received")?;
    assert_eq!(reduced.header("consent"), Some("revoked"));
    assert!(
        reduced.payload["data"].get("body").is_none(),
        "a revoked contact's body never reaches the bus"
    );

    // A reply and a captioned image sent while revoked: both keep their
    // shape and lose their content.
    let revoked_reply_id = alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.text",
                "body": "et finalement 21h",
                "m.relates_to": { "m.in_reply_to": { "event_id": parent_id } },
            }),
        )
        .await?;
    let image_id = alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.image",
                "body": "regarde cette photo",
                "filename": "IMG_2042.jpg",
                "url": "mxc://test.twalk/ZyXwVu0987654321",
                "info": { "mimetype": "image/jpeg", "size": 102400, "w": 1920, "h": 1080 },
            }),
        )
        .await?;

    let reply = wait_for_matrix_event(&bus, &room_id, &revoked_reply_id).await?;
    validate_against_contract(&reply.payload, "inbound.message.received")?;
    // The evidence that a message arrived is intact.
    assert_eq!(reply.payload["consent"].as_str(), Some("revoked"));
    assert_eq!(reply.payload["network"].as_str(), Some("whatsapp"));
    assert_eq!(reply.payload["subject"].as_str(), Some(alpha.user_id()));
    assert!(reply.payload["source"]
        .as_str()
        .is_some_and(|source| source.starts_with("matrix://") && source.ends_with(&room_id)));
    assert!(
        reply.payload["time"].as_str().is_some()
            && reply.payload["data"]["network_timestamp"]
                .as_str()
                .is_some(),
        "both timestamps survive the reduction"
    );
    assert_eq!(
        reply.payload["data"]["reply_to"]["matrix_event_id"].as_str(),
        Some(parent_id.as_str()),
        "the reply relation survives the reduction"
    );
    // The content is gone.
    assert!(reply.payload["data"].get("body").is_none());
    assert!(
        reply.payload["data"]["reply_to"].get("excerpt").is_none(),
        "an excerpt of the quoted message is content too"
    );

    let image = wait_for_matrix_event(&bus, &room_id, &image_id).await?;
    validate_against_contract(&image.payload, "inbound.message.received")?;
    assert!(image.payload["data"].get("body").is_none());
    assert_eq!(
        image.payload["data"]["attachments"][0],
        json!({
            "kind": "image",
            "mime_type": "image/jpeg",
            "size_bytes": 102400,
            "dimensions": { "width": 1920, "height": 1080 },
        }),
        "the attachment keeps its shape and loses its reference, keys and caption"
    );

    // The reaction path leaks no excerpt either.
    alpha.send_reaction(&room_id, &parent_id, "👍").await?;
    let reaction = bus
        .wait_for_room_message(STREAM, REACTION_SUBJECT, &room_id)
        .await?;
    validate_against_contract(&reaction.payload, "inbound.reaction.added")?;
    assert_eq!(reaction.payload["consent"].as_str(), Some("revoked"));
    assert_eq!(reaction.payload["data"]["reaction"].as_str(), Some("👍"));
    assert_eq!(
        reaction.payload["data"]["target"]["matrix_event_id"].as_str(),
        Some(parent_id.as_str())
    );
    assert!(
        reaction.payload["data"]["target"].get("excerpt").is_none(),
        "an excerpt of the reacted-to message is content"
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
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;

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

    let relabelled =
        wait_for_label(&bus, &alpha, &room_id, "after the decision", "revoked").await?;
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

// --- The consent snapshot hand-off (ticket #51, issue #16, ADR 0010) ------
//
// Everything above assumes a Sensor that has been running since before the
// decision it labels by. What follows is what happens when it has not: a
// process that starts with an empty cache, a durable consumer that will not
// tell it again what it already applied, and a Companion Gateway that can.
//
// The stub Gateway (`harness::gateway`) stands in for the real one: the
// Sensor's side of this seam is one authenticated HTTP read, and the test
// needs to choose both what it answers and the stream position it answers
// for — the position is the hand-off. The Gateway's own half is tested where
// it lives (`companion-gateway/tests/consent_snapshot.rs`).

use harness::gateway::{contact_entry, network_entry, StubGateway};

/// The service token the Sensor presents. A throwaway constant for the local
/// stack, the same category as the test-bot passwords, and the length the
/// Gateway insists on for a real one.
const SERVICE_TOKEN: &str = "test-only-service-token-0123456789abcdef";

/// The Sensor environment with a Companion Gateway to read the snapshot from.
fn sensor_env_with_gateway(url: &str) -> Vec<(String, String)> {
    harness::sensor_env_with(&[
        ("SENSOR_GATEWAY_URL", url),
        ("SENSOR_GATEWAY_SERVICE_TOKEN", SERVICE_TOKEN),
    ])
}

/// Publishes one decision as the Gateway would, and returns the stream
/// sequence it landed at — which is what the snapshot names and what the
/// consumer resumes after.
async fn publish_decision(
    bus: &Bus,
    subject_id: &str,
    networks: &[&str],
    old_state: &str,
    new_state: &str,
) -> Result<u64> {
    publish_event(
        bus,
        &consent_change(subject_id, networks, old_state, new_state)?,
    )
    .await
}

/// A network-scoped decision, published the same way: that network's default
/// consent state, which applies to every contact on it that has no decision
/// of its own (the contract's `network` subject type, and the `id` it insists
/// is the network value itself).
async fn publish_network_default(
    bus: &Bus,
    network: &str,
    old_state: &str,
    new_state: &str,
) -> Result<u64> {
    let mut event = contract_fixture("consent.state.changed")?;
    let now = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
    event["id"] = json!(unique_event_id());
    event["time"] = json!(now);
    event["subject"] = json!(network);
    event["consent"] = json!(new_state);
    event["network"] = json!(network);
    event["data"]["subject"] = json!({ "type": "network", "id": network });
    event["data"]["old_state"] = json!(old_state);
    event["data"]["new_state"] = json!(new_state);
    event["data"]["scope"]["connections"] = json!([network]);
    event["data"]["scope"]["networks"] = json!([network]);
    event["data"]["occurred_at"] = json!(now);
    validate_against_contract(&event, "consent.state.changed")?;
    publish_event(bus, &event).await
}

async fn publish_event(bus: &Bus, event: &Value) -> Result<u64> {
    let id = event["id"]
        .as_str()
        .expect("the event has an id")
        .to_owned();
    bus.publish(CONSENT_SUBJECT, event).await?;
    poll_until(
        || async {
            bus.fetch_all_with_headers(STREAM, CONSENT_SUBJECT)
                .await
                .ok()?
                .into_iter()
                .find(|m| m.payload["id"].as_str() == Some(id.as_str()))
                .map(|m| m.sequence)
        },
        "the published decision to be stored",
    )
    .await
}

/// The stream sequence the consent subject currently ends at: what a Gateway
/// that has published every decision it holds would name. `0` when nothing
/// has ever been decided — the consumer then starts at `1`.
async fn consent_head(bus: &Bus) -> Result<u64> {
    Ok(bus
        .fetch_all_with_headers(STREAM, CONSENT_SUBJECT)
        .await?
        .last()
        .map(|m| m.sequence)
        .unwrap_or(0))
}

fn logged(logs: &[String], message: &str) -> bool {
    logs.iter().any(|line| line.contains(message))
}

fn logged_change_about(logs: &[String], subject_id: &str) -> bool {
    logs.iter()
        .any(|line| line.contains("applied a consent change") && line.contains(subject_id))
}

/// The hand-off itself, on a genuinely cold consumer: what the snapshot holds
/// is applied from the snapshot, what it does not hold arrives over the
/// stream from the position it named, and the two never overlap — the
/// decision the snapshot accounted for is never delivered a second time.
#[tokio::test]
async fn a_cold_sensor_applies_the_snapshot_and_follows_the_stream_after_it() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let alpha = Bot::login("bot_alpha").await?;
    let puppet = Bot::login("whatsapp_33612345678").await?;
    let beta = Bot::login("bot_beta").await?;

    // Two decisions, in order. The snapshot is taken between them: it
    // accounts for the puppet's grant and not for beta's, so each contact can
    // only have reached the Sensor one way.
    let snapshot_sequence =
        publish_decision(&bus, puppet.user_id(), &["whatsapp"], "pending", "granted").await?;
    let stream_sequence =
        publish_decision(&bus, beta.user_id(), &["whatsapp"], "pending", "granted").await?;
    assert!(
        stream_sequence > snapshot_sequence,
        "the second decision is published after the position the snapshot names"
    );

    let gateway = StubGateway::start(SERVICE_TOKEN).await?;
    gateway.serve(
        vec![contact_entry(puppet.user_id(), "whatsapp", "granted")],
        snapshot_sequence,
    );

    // A durable consumer outlives the process that made it, so a cold start
    // has to be made: without this the suite would only ever exercise the
    // warm path after its first run. The durable name is spelled out rather
    // than imported, like the subjects above: it is what the Sensor leaves on
    // the bus, so the test names it from the outside.
    bus.delete_consumer(STREAM, CONSENT_CONSUMER).await?;

    let sensor = SensorProc::start(&sensor_env_with_gateway(&gateway.url()))?;
    let room_id = make_whatsapp_portal(&alpha, "consent-snapshot-cold-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    alpha.invite(&room_id, puppet.user_id()).await?;
    puppet.join_room(&room_id).await?;
    alpha.invite(&room_id, beta.user_id()).await?;
    beta.join_room(&room_id).await?;

    // The snapshot was applied before the sync loop started, so the contact
    // it granted is granted on its very first event — no window, no polling.
    let first_id = puppet.send_message(&room_id, "depuis l'instantané").await?;
    let first = wait_for_matrix_event(&bus, &room_id, &first_id).await?;
    validate_against_contract(&first.payload, "inbound.message.received")?;
    assert_eq!(
        first.payload["consent"].as_str(),
        Some("granted"),
        "the snapshot labels the sender from the Sensor's first event on"
    );
    assert_eq!(
        first.payload["data"]["contact"]["network_identifier"].as_str(),
        Some("+33612345678"),
        "and unlocks what a granted contact's events carry"
    );

    // The decision the snapshot did not account for is not lost: the consumer
    // was created at the position after it, so the stream delivers it.
    wait_for_label(&bus, &beta, &room_id, "depuis le flux", "granted").await?;

    let logs = sensor.logs().await;
    assert!(
        logged(&logs, "applied the Companion Gateway's consent snapshot"),
        "{logs:?}"
    );
    assert!(
        logged_change_about(&logs, beta.user_id()),
        "the decision after the snapshot's position comes off the stream: {logs:?}"
    );
    assert!(
        !logged_change_about(&logs, puppet.user_id()),
        "the decision the snapshot already held is never delivered again — snapshot and \
         stream do not overlap: {logs:?}"
    );

    // The credential is the service token, as a bearer: the Sensor has no
    // OpenID token to sign in with and holds no device token (ADR 0011).
    assert_eq!(
        gateway.requests(),
        vec![Some(format!("Bearer {SERVICE_TOKEN}"))],
        "one authenticated snapshot read, and only one"
    );

    sensor.stop().await;
    Ok(())
}

/// Issue #16 itself: a contact the user granted stays granted across a full
/// Sensor restart. The durable consumer is warm — it acked that decision
/// before the Sensor stopped and will never deliver it again — so before the
/// snapshot existed the restarted Sensor labelled that contact `pending`
/// until the user decided something new.
#[tokio::test]
async fn a_granted_contact_stays_granted_across_a_sensor_restart() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let alpha = Bot::login("bot_alpha").await?;

    let gateway = StubGateway::start(SERVICE_TOKEN).await?;
    gateway.serve(Vec::new(), consent_head(&bus).await?);
    let env = sensor_env_with_gateway(&gateway.url());

    let sensor = SensorProc::start(&env)?;
    let room_id = make_whatsapp_portal(&alpha, "consent-snapshot-restart-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;

    // The user grants the contact; the running Sensor applies it from the bus.
    let sequence =
        publish_decision(&bus, alpha.user_id(), &["whatsapp"], "pending", "granted").await?;
    wait_for_label(&bus, &alpha, &room_id, "avant le redémarrage", "granted").await?;
    // The Gateway's state now reflects that decision, at its position.
    gateway.serve(
        vec![contact_entry(alpha.user_id(), "whatsapp", "granted")],
        sequence,
    );

    // A full restart: the process is killed, its cache dies with it, and the
    // durable consumer stays where it was.
    sensor.stop().await;
    let sensor = SensorProc::start(&env)?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;

    let after_id = alpha.send_message(&room_id, "après le redémarrage").await?;
    let after = wait_for_matrix_event(&bus, &room_id, &after_id).await?;
    validate_against_contract(&after.payload, "inbound.message.received")?;
    assert_eq!(
        after.payload["consent"].as_str(),
        Some("granted"),
        "a restart must not downgrade a granted contact to pending (issue #16)"
    );
    assert_eq!(after.header("consent"), Some("granted"));

    assert_eq!(
        gateway.requests().len(),
        2,
        "each start reads the snapshot exactly once"
    );

    sensor.stop().await;
    Ok(())
}

/// The other half of the restart: decisions taken while the Sensor was down.
/// They are in the snapshot — the Gateway recorded and published them — so
/// they are applied before the Sensor's first event, not after its first
/// message from each contact.
///
/// The two decisions here are also what the snapshot's shape is for: one
/// network-level default, and one contact-level decision overriding it. The
/// precedence — the contact's own decision first, the network's default next,
/// `pending` when neither exists — is the Gateway's, and this is the Sensor
/// applying it to real events.
#[tokio::test]
async fn a_decision_taken_while_the_sensor_was_down_is_applied_after_it() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let alpha = Bot::login("bot_alpha").await?;
    let puppet = Bot::login("whatsapp_33612345678").await?;

    let gateway = StubGateway::start(SERVICE_TOKEN).await?;
    gateway.serve(Vec::new(), consent_head(&bus).await?);
    let env = sensor_env_with_gateway(&gateway.url());

    let sensor = SensorProc::start(&env)?;
    let room_id = make_whatsapp_portal(&alpha, "consent-snapshot-downtime-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    alpha.invite(&room_id, puppet.user_id()).await?;
    puppet.join_room(&room_id).await?;

    // Unknown while the Sensor runs: pending, and no identifier leaks.
    let before_id = puppet.send_message(&room_id, "avant la décision").await?;
    let before = wait_for_matrix_event(&bus, &room_id, &before_id).await?;
    assert_eq!(before.payload["consent"].as_str(), Some("pending"));

    sensor.stop().await;

    // The user decides while nothing is listening: the whole network granted,
    // and one contact on it revoked.
    publish_network_default(&bus, "whatsapp", "unset", "granted").await?;
    let sequence =
        publish_decision(&bus, alpha.user_id(), &["whatsapp"], "unset", "revoked").await?;
    gateway.serve(
        vec![
            network_entry("whatsapp", "granted"),
            contact_entry(alpha.user_id(), "whatsapp", "revoked"),
        ],
        sequence,
    );

    let sensor = SensorProc::start(&env)?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;

    // The puppet has no decision of its own, so the network's default answers
    // for it — and a granted contact's events carry its network identifier.
    let after_id = puppet.send_message(&room_id, "après la décision").await?;
    let after = wait_for_matrix_event(&bus, &room_id, &after_id).await?;
    validate_against_contract(&after.payload, "inbound.message.received")?;
    assert_eq!(
        after.payload["consent"].as_str(),
        Some("granted"),
        "a decision taken while the Sensor was down applies as soon as it is back"
    );
    assert_eq!(
        after.payload["data"]["contact"]["network_identifier"].as_str(),
        Some("+33612345678")
    );

    // Alpha has one, and it wins over the default it contradicts — with
    // everything a revocation takes away (ADR 0012).
    let revoked_id = alpha.send_message(&room_id, "et moi j'ai refusé").await?;
    let revoked = wait_for_matrix_event(&bus, &room_id, &revoked_id).await?;
    validate_against_contract(&revoked.payload, "inbound.message.received")?;
    assert_eq!(
        revoked.payload["consent"].as_str(),
        Some("revoked"),
        "a contact's own decision overrides the network's default"
    );
    assert!(
        revoked.payload["data"].get("body").is_none(),
        "and it overrides it in the direction that removes content"
    );

    // Both decisions were taken while the Sensor was down, so its warm
    // durable consumer still holds them — unacked, and before the position
    // the snapshot named. They came from the snapshot, and the stream does
    // not deliver them a second time: the hand-off boundary holds on a warm
    // consumer too.
    assert!(
        !logged_change_about(&sensor.logs().await, alpha.user_id()),
        "a decision the snapshot already held is never applied from the stream as well"
    );

    // The bus outlives this run, and a network-wide grant left on it would
    // label every whatsapp sender in every later test. Put the default back
    // where the other tests expect it — the user changed their mind, which is
    // an ordinary decision.
    publish_network_default(&bus, "whatsapp", "granted", "pending").await?;

    sensor.stop().await;
    Ok(())
}

/// An unreachable Gateway must not stop the Sensor: a Sensor that waits loses
/// inbound events, which is worse than a degraded label. It starts, publishes
/// everything, labels `pending`, says so loudly, counts it — and recovers on
/// its own when the Gateway comes back.
#[tokio::test]
async fn an_unreachable_gateway_labels_pending_without_losing_an_event() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let alpha = Bot::login("bot_alpha").await?;

    // An address nothing answers on — and the one the Gateway will later
    // appear at, which is how the retry is observed rather than assumed.
    let gateway_addr = harness::free_loopback_addr()?;
    let metrics_listen = harness::free_loopback_addr()?;
    let sensor = SensorProc::start(&harness::sensor_env_with(&[
        ("SENSOR_GATEWAY_URL", &format!("http://{gateway_addr}")),
        ("SENSOR_GATEWAY_SERVICE_TOKEN", SERVICE_TOKEN),
        ("SENSOR_METRICS_LISTEN", &metrics_listen),
    ]))?;

    let room_id = make_whatsapp_portal(&alpha, "consent-snapshot-outage-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;

    // Every event still reaches the bus, and every one of them is labelled
    // pending — the safe default, never a guess at what was granted.
    let mut sent = Vec::new();
    for index in 0..3 {
        sent.push(
            alpha
                .send_message(&room_id, &format!("message {index}"))
                .await?,
        );
    }
    for matrix_event_id in &sent {
        let event = wait_for_matrix_event(&bus, &room_id, matrix_event_id).await?;
        validate_against_contract(&event.payload, "inbound.message.received")?;
        assert_eq!(
            event.payload["consent"].as_str(),
            Some("pending"),
            "an unreachable Gateway degrades the label and loses no event"
        );
    }

    // Loudly, and counted: a degraded label is invisible in the events
    // themselves, so it has to be visible to the operator.
    assert!(
        logged(
            &sensor.logs().await,
            "could not read the Companion Gateway's consent snapshot"
        ),
        "the failure is logged"
    );
    let metrics_url = format!("http://{metrics_listen}/metrics");
    poll_until(
        || async {
            let body = reqwest::get(&metrics_url).await.ok()?.text().await.ok()?;
            body.lines()
                .find(|line| line.starts_with("twalk_sensor_consent_snapshot_failures_total "))
                .and_then(|line| line.rsplit(' ').next())
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|failures| *failures >= 1)
        },
        "the consent snapshot failure counter",
    )
    .await?;

    // The Gateway comes up at the address the Sensor has been retrying,
    // already holding the decision — the retry must not be able to catch it
    // between binding and knowing its own state. The retry applies the
    // snapshot and only then creates the stream consumer, so the label flips
    // without anything having to arbitrate.
    let sequence =
        publish_decision(&bus, alpha.user_id(), &["whatsapp"], "pending", "granted").await?;
    let gateway = StubGateway::start_on(&gateway_addr, SERVICE_TOKEN).await?;
    gateway.serve(
        vec![contact_entry(alpha.user_id(), "whatsapp", "granted")],
        sequence,
    );

    wait_for_label(&bus, &alpha, &room_id, "après le retour", "granted").await?;

    sensor.stop().await;
    Ok(())
}

/// Issue #269 / ADR 0033: every event carries the **connection** it belongs to,
/// and the Sensor never derives one — the registry is the Gateway's, handed
/// over on the consent snapshot, and a room is looked up in it by the bridge
/// bot that built it.
#[tokio::test]
async fn every_event_is_stamped_with_the_connection_the_registry_names_for_its_room() -> Result<()>
{
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let alpha = Bot::login("bot_alpha").await?;
    let puppet = Bot::login("whatsapp_33612345678").await?;

    let gateway = StubGateway::start(SERVICE_TOKEN).await?;
    gateway.serve(Vec::new(), 0);
    // A registry where the WhatsApp bridge whose bot is alpha is not the
    // deployment's only WhatsApp account: the id is the registry's, not the
    // network's name.
    gateway.serve_connections(vec![
        json!({ "id": "wa-work", "kind": "whatsapp", "bridge_bot": alpha.user_id() }),
        json!({ "id": "wa-home", "kind": "whatsapp", "bridge_bot": "@someoneelse:test.twalk" }),
        json!({ "id": "matrix", "kind": "matrix" }),
    ]);
    bus.delete_consumer(STREAM, CONSENT_CONSUMER).await?;
    let sensor = SensorProc::start(&sensor_env_with_gateway(&gateway.url()))?;

    let room_id = make_whatsapp_portal(&alpha, "connection-stamped-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    alpha.invite(&room_id, puppet.user_id()).await?;
    puppet.join_room(&room_id).await?;

    let event_id = puppet
        .send_message(&room_id, "which account is this?")
        .await?;
    let stored = wait_for_matrix_event(&bus, &room_id, &event_id).await?;
    validate_against_contract(&stored.payload, "inbound.message.received")?;
    assert_eq!(
        stored.payload["connection"].as_str(),
        Some("wa-work"),
        "the connection is the registry's id for this room's bridge bot, not the network's name"
    );
    assert_eq!(stored.payload["network"].as_str(), Some("whatsapp"));
    assert_eq!(
        stored
            .headers
            .iter()
            .find(|(name, _)| name == "connection")
            .map(|(_, value)| value.as_str()),
        Some("wa-work"),
        "and the bus header duplicates it for server-side filtering"
    );

    sensor.stop().await;
    Ok(())
}

/// Two bridges of one network, one contact in a portal of each, one
/// decision per connection (ADR 0033, #271): the events carry `granted` on
/// the connection the user granted and `pending` on the other. This is the
/// whole reason the perimeter exists — a decision about the work account
/// says nothing about the home one — and the case a cache keyed on the
/// network would get wrong in both directions.
#[tokio::test]
async fn one_contact_on_two_connections_of_one_network_holds_one_decision_per_connection(
) -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let work_bot = Bot::login("bot_alpha").await?;
    let home_bot = Bot::login("bot_beta").await?;
    let puppet = Bot::login("whatsapp_33612345678").await?;

    let gateway = StubGateway::start(SERVICE_TOKEN).await?;
    // The snapshot: the contact granted on the work account only, each
    // entry keyed on its connection — served at the stream's head, so the
    // decisions earlier runs of this journey left on the shared bus are
    // the snapshot's business and not replayed.
    gateway.serve(
        vec![json!({
            "subject": { "type": "contact", "id": puppet.user_id() },
            "connection": "wa-work",
            "network": "whatsapp",
            "state": "granted",
            "decided_at": "2026-09-17T10:00:00.000Z",
            "decision_sequence": 1,
        })],
        consent_head(&bus).await?,
    );
    gateway.serve_connections(vec![
        json!({ "id": "wa-work", "kind": "whatsapp", "bridge_bot": work_bot.user_id() }),
        json!({ "id": "wa-home", "kind": "whatsapp", "bridge_bot": home_bot.user_id() }),
        json!({ "id": "matrix", "kind": "matrix" }),
    ]);
    bus.delete_consumer(STREAM, CONSENT_CONSUMER).await?;
    let sensor = SensorProc::start(&harness::sensor_env_with(&[
        ("SENSOR_GATEWAY_URL", &gateway.url()),
        ("SENSOR_GATEWAY_SERVICE_TOKEN", SERVICE_TOKEN),
        (
            "SENSOR_ALLOWED_INVITERS",
            &format!("{},{}", work_bot.user_id(), home_bot.user_id()),
        ),
    ]))?;

    let mut rooms = Vec::new();
    for (bot, name) in [
        (&work_bot, "two-accounts-work"),
        (&home_bot, "two-accounts-home"),
    ] {
        let room_id = make_whatsapp_portal(bot, name).await?;
        bot.invite(&room_id, SENSOR_USER_ID).await?;
        bot.wait_for_membership(&room_id, SENSOR_USER_ID, "join")
            .await?;
        bot.invite(&room_id, puppet.user_id()).await?;
        puppet.join_room(&room_id).await?;
        rooms.push(room_id);
    }
    let [work_room, home_room] = rooms.as_slice() else {
        unreachable!()
    };

    for (room_id, connection, consent) in [
        (work_room, "wa-work", "granted"),
        (home_room, "wa-home", "pending"),
    ] {
        let event_id = puppet
            .send_message(room_id, &format!("hello on {connection}"))
            .await?;
        let stored = wait_for_matrix_event(&bus, room_id, &event_id).await?;
        validate_against_contract(&stored.payload, "inbound.message.received")?;
        assert_eq!(stored.payload["connection"].as_str(), Some(connection));
        assert_eq!(
            stored.payload["consent"].as_str(),
            Some(consent),
            "{connection}: the same contact, the decision of *this* perimeter: {}",
            stored.payload
        );
    }

    // A decision on the stream, scoped to the home account: it lands there
    // and nowhere else.
    let mut event = consent_change(puppet.user_id(), &["whatsapp"], "unset", "revoked")?;
    event["data"]["scope"]["connections"] = json!(["wa-home"]);
    validate_against_contract(&event, "consent.state.changed")?;
    publish_event(&bus, &event).await?;
    poll_until(
        || async {
            let event_id = puppet
                .send_message(home_room, "after the revocation at home")
                .await
                .ok()?;
            let stored = wait_for_matrix_event(&bus, home_room, &event_id)
                .await
                .ok()?;
            (stored.payload["consent"].as_str() == Some("revoked")).then_some(())
        },
        "the home account's message to carry the revocation",
    )
    .await?;
    let event_id = puppet
        .send_message(work_room, "still granted at work")
        .await?;
    let stored = wait_for_matrix_event(&bus, work_room, &event_id).await?;
    assert_eq!(
        stored.payload["consent"].as_str(),
        Some("granted"),
        "the revocation at home is not a revocation at work: {}",
        stored.payload
    );

    sensor.stop().await;
    Ok(())
}

/// A room whose bot no connection names publishes nothing — never a guessed
/// perimeter, whose decisions would be another account's — and the silence
/// is counted and said. The registry here is the reference deployment's:
/// one WhatsApp connection, its bot named by the Gateway. The room is built
/// by a bot of the same kind that is not that one, which is exactly the
/// case a by-kind guess would get wrong.
#[tokio::test]
async fn a_room_whose_bot_no_connection_names_is_not_published_and_the_drop_is_counted(
) -> Result<()> {
    const METRICS_LISTEN: &str = "127.0.0.1:19013";
    const METRICS_URL: &str = "http://127.0.0.1:19013/metrics";

    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let alpha = Bot::login("bot_alpha").await?;
    let puppet = Bot::login("whatsapp_33612345678").await?;

    let gateway = StubGateway::start(SERVICE_TOKEN).await?;
    gateway.serve(Vec::new(), 0);
    // The one WhatsApp connection is another bot's.
    gateway.serve_connections(vec![
        json!({ "id": "whatsapp", "kind": "whatsapp", "bridge_bot": "@bot_beta:test.twalk" }),
        json!({ "id": "matrix", "kind": "matrix" }),
    ]);
    bus.delete_consumer(STREAM, CONSENT_CONSUMER).await?;
    let url = gateway.url();
    let sensor = SensorProc::start(&harness::sensor_env_with(&[
        ("SENSOR_GATEWAY_URL", &url),
        ("SENSOR_GATEWAY_SERVICE_TOKEN", SERVICE_TOKEN),
        ("SENSOR_METRICS_LISTEN", METRICS_LISTEN),
    ]))?;

    let room_id = make_whatsapp_portal(&alpha, "connection-uncovered-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    alpha.invite(&room_id, puppet.user_id()).await?;
    puppet.join_room(&room_id).await?;

    puppet.send_message(&room_id, "into no perimeter").await?;
    // The drop is counted; that is what proves the message was seen and
    // refused rather than not yet processed.
    poll_until(
        || async {
            let body = reqwest::get(METRICS_URL).await.ok()?.text().await.ok()?;
            body.lines()
                .find_map(|line| {
                    line.strip_prefix(
                        "twalk_sensor_events_dropped_total{reason=\"unknown_connection\"} ",
                    )
                })
                .and_then(|rest| rest.trim().parse::<u64>().ok())
                .filter(|count| *count >= 1)
        },
        "the uncovered room's message to be counted as dropped",
    )
    .await?;
    assert!(
        bus.fetch_room_messages(STREAM, MESSAGE_SUBJECT, &room_id)
            .await?
            .is_empty(),
        "nothing from a room no connection covers reaches the bus"
    );
    let logs = sensor.logs().await;
    assert!(
        logs.iter()
            .any(|line| line.contains("no connection covers this room") && line.contains(&room_id)),
        "the room is named once: {logs:?}"
    );

    sensor.stop().await;
    Ok(())
}
