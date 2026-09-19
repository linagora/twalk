// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Issue #123 / ADR 0025 / ADR 0034: the outbound path has an identity, and it
//! is the user's own.
//!
//! A mautrix bridge relays to its network only what the **logged-in user's own
//! Matrix account** sends. The Sensor had nothing that could be that account,
//! so an approved reply went out as `@sensor:`, Synapse returned an event id,
//! and the bridge logged nothing at all — not a refusal, not a warning. The
//! outbound half of this product had never worked on a live deployment while
//! every component reported itself healthy. And the one identity a bridge would
//! have relayed was not even in the room: on the reference deployment the
//! owner's account showed `invite: 33, join: 0`, because the whole premise of
//! Twalk is that the user does not run a Matrix client.
//!
//! So the Sensor gains a **second, write-only Matrix client**: a device of the
//! owner's own account, beside its own `@sensor:` client. One identity
//! observes, one identity acts.
//!
//! What this file asserts, from the homeserver and the bus and never from
//! inside the Sensor:
//!
//! - the owner's device **joins** a portal room a configured bridge bot invited
//!   it to, so the invitation stops being the dead end #123 found;
//! - and joins **nothing else** — a room a stranger built, wrote a WhatsApp
//!   `m.bridge` marker into and invited the owner to is left at `invite`,
//!   because everything in an invitation except the inviter is chosen by
//!   whoever sent it, and this device posts messages;
//! - an approved reply is posted **by the owner's account**, not by `@sensor:`,
//!   and the Sensor says on the bus that it reached the contact (#216);
//! - in an **encrypted** portal — which every real portal is — the reply is
//!   Megolm-encrypted by a device that runs no message sync, and another device
//!   entirely (the Sensor's own) decrypts it, which is the only way to prove the
//!   room key was really shared;
//! - with **no** owner device the behaviour is unchanged — `@sensor:` posts —
//!   and the Sensor now says the reply reached nobody instead of calling it
//!   sent, which is #216's whole complaint;
//! - and a token for **another account** is refused at startup, rather than
//!   being found later by a contact receiving a reply from a stranger.
//!
//! Isolation is the suite's: `TWALK_TEST_STACK`, `TWALK_TEST_SYNAPSE_PORT` and
//! `TWALK_TEST_NATS_PORT` move the whole stack aside for a parallel worktree
//! (`sensor/tests/harness/mod.rs`).

mod harness;

use anyhow::Result;
use harness::crypto::{make_encrypted_whatsapp_portal, CryptoBot};
use harness::{
    contract_fixture, ensure_stack, fresh_state_dir, make_whatsapp_portal, poll_until,
    sensor_env_with, validate_against_contract, Bot, Bus, SensorProc, StoredMessage,
    SENSOR_USER_ID,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const STREAM: &str = "twalk";
const REPLY_APPROVED_SUBJECT: &str = "twalk.persona.reply.approved.v1";
const POSTED_SUBJECT: &str = "twalk.persona.reply.approved.v1.posted";
const OUTBOUND_SUBJECT: &str = "twalk.outbound.message.sent.v1";

/// The owner: unlike every other suite, a **real account** on the test stack.
/// The identity Twalk acts through has to log in, hold a device, and join
/// rooms, so it cannot be the never-resolved `@michel:test.twalk` the
/// publication suites use.
const OWNER: &str = "@owner:test.twalk";

/// The bridge, playing mautrix's own bot: the `sender_localpart` that creates
/// portals and invites the user into them. It is the **only** authenticated
/// fact in an invitation, so it is the only thing the join policy reads.
const BRIDGE_BOT: &str = "@whatsappbot:test.twalk";

/// A perfectly ordinary account that is not a bridge: the attacker in the
/// second half of the first test, and a reminder that a Matrix ID in an
/// invitation costs nothing to obtain.
const STRANGER: &str = "@bot_beta:test.twalk";

/// A fresh CloudEvents id per published event: the bus persists across runs, so
/// reusing the fixture id would make a second run's approval a duplicate the
/// stream silently drops.
fn unique_event_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_string().as_bytes());
    hasher.update(
        COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .to_string()
            .as_bytes(),
    );
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The contract fixture with its placeholders patched to real test values,
/// re-validated so what is published stays a contract event.
fn approved_reply(room_id: &str, body: &str) -> Result<Value> {
    let mut event = contract_fixture("persona.reply.approved")?;
    event["id"] = json!(unique_event_id());
    event["data"]["target"]["room_id"] = json!(room_id);
    event["data"]["target"]
        .as_object_mut()
        .unwrap()
        .remove("reply_to_event_id");
    event["data"]["final"]["body"] = json!(body);
    event["data"]["approved_by"] = json!(OWNER);
    validate_against_contract(&event, "persona.reply.approved")?;
    Ok(event)
}

/// The environment of a Sensor that knows who its operator is, which accounts
/// are the bridges' bots, and — unless `owner_device` is `None` — holds a
/// device of the operator's own account.
fn owner_device_env(test_name: &str, owner_device: Option<(&str, &str)>) -> Vec<(String, String)> {
    let state_dir = fresh_state_dir(test_name);
    let mut overrides = vec![
        ("SENSOR_OWNER", OWNER.to_owned()),
        ("SENSOR_ALLOWED_INVITERS", BRIDGE_BOT.to_owned()),
        ("SENSOR_BRIDGE_BOTS", BRIDGE_BOT.to_owned()),
        ("SENSOR_STATE_DIR", state_dir.to_string_lossy().into_owned()),
        ("SENSOR_SEND_RETRY_BASE_MS", "100".to_owned()),
        ("SENSOR_SEND_RETRY_MAX_ATTEMPTS", "3".to_owned()),
    ];
    if let Some((access_token, device_id)) = owner_device {
        overrides.push(("SENSOR_OWNER_DEVICE_ACCESS_TOKEN", access_token.to_owned()));
        overrides.push(("SENSOR_OWNER_DEVICE_ID", device_id.to_owned()));
    }
    let overrides: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    sensor_env_with(&overrides)
}

/// Waits for the Sensor's report of what one posted reply reached.
async fn posted_report(bus: &Bus, approval_id: &str) -> Result<StoredMessage> {
    poll_until(
        || async {
            bus.fetch_all_with_headers(STREAM, POSTED_SUBJECT)
                .await
                .ok()?
                .into_iter()
                .find(|m| m.payload["id"].as_str() == Some(approval_id))
        },
        "the Sensor's report of what the posted reply reached",
    )
    .await
}

/// The first `m.room.message` (or `m.room.encrypted` event) a given account sent
/// in a room.
async fn event_from(bot: &Bot, room_id: &str, sender: &str, description: &str) -> Result<Value> {
    bot.wait_for_event(
        room_id,
        |event| {
            event.get("sender").and_then(Value::as_str) == Some(sender)
                && matches!(
                    event.get("type").and_then(Value::as_str),
                    Some("m.room.message") | Some("m.room.encrypted")
                )
        },
        description,
    )
    .await
}

/// The owner's device joins the portals of configured bridges, and refuses
/// every other invitation.
///
/// The two halves are one test on purpose. What makes the refusal assertable is
/// that the *other* invitation, sent at the same moment by a stranger, was
/// still sitting at `invite` after the loop had demonstrably run by joining the
/// legitimate one — without that ordering, "has not joined yet" and "will never
/// join" are the same observation.
#[tokio::test]
async fn the_owners_device_joins_portals_and_nothing_else() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let owner = Bot::login("owner").await?;
    let bridge = Bot::login("whatsappbot").await?;
    let stranger = Bot::login("bot_beta").await?;
    assert_eq!(owner.user_id(), OWNER);
    assert_eq!(bridge.user_id(), BRIDGE_BOT);
    assert_eq!(stranger.user_id(), STRANGER);

    // A portal, as mautrix builds one: the bridge's own bot creates it, marks
    // it, and invites the user.
    let portal = make_whatsapp_portal(&bridge, "owner-device-portal").await?;
    bridge.invite(&portal, SENSOR_USER_ID).await?;
    bridge.invite(&portal, OWNER).await?;

    // And a room that *claims* to be a portal. Everything in this invitation
    // except its sender is the stranger's to choose, including the `m.bridge`
    // marker — which is exactly why the marker is not what the Sensor reads.
    let impostor = stranger.create_room("owner-device-impostor", false).await?;
    let (state_key, content) = harness::whatsapp_bridge_state(BRIDGE_BOT, "impostor");
    stranger
        .send_state_event(&impostor, "m.bridge", &state_key, content)
        .await?;
    stranger.invite(&impostor, OWNER).await?;

    let sensor = SensorProc::start(&owner_device_env(
        "joins-portals",
        Some((owner.access_token(), owner.device_id())),
    ))?;

    bridge.wait_for_membership(&portal, OWNER, "join").await?;
    // The observing identity is unchanged (ADR 0024) and joins on its own
    // schedule: the two clients are independent, so this is waited for rather
    // than read once.
    bridge
        .wait_for_membership(&portal, SENSOR_USER_ID, "join")
        .await?;

    // The loop has run — it joined the portal above — so the impostor's
    // invitation is not one it has yet to reach.
    assert_eq!(
        stranger.get_membership(&impostor, OWNER).await?,
        "invite",
        "a device of the user's own account must not be placed in a room by anybody who can send \
         an invitation"
    );

    let logs = sensor.logs().await;
    assert!(
        logs.iter()
            .any(|line| line.contains("joined a portal of a configured bridge")),
        "the join is announced: {logs:?}"
    );
    assert!(
        logs.iter().any(|line| {
            line.contains("not joining the owner's device to this room")
                && line.contains("inviter_is_not_a_bridge_bot")
        }),
        "the refusal is announced with its reason, not silent: {logs:?}"
    );

    sensor.stop().await;
    Ok(())
}

/// Issue #237: a portal the owner's device can never join is given up on
/// **once**, from what the homeserver answered, and the loop goes on joining
/// the portals it can.
///
/// The shape is the one measured live: a portal every member has left. The
/// homeserver has no server to join through and says so with a 404 — and that
/// answer will never change, so the seventy-eight retries in five minutes it
/// produced were noise that buried the next real failure. What is asserted,
/// from outside the Sensor:
///
/// - a *later* invitation, arriving after the first failure, is joined — so the
///   loop has run at least once more since, and would have retried the orphan
///   had it not remembered it;
/// - the orphan is announced exactly once, as unjoinable, and never as "retrying";
/// - and the fact is readable off `/metrics`, not only out of the log: the
///   `unjoinable` outcome is 1 and the gauge of such portals is 1.
#[tokio::test]
async fn a_portal_that_can_never_be_joined_is_given_up_on_once() -> Result<()> {
    const METRICS_LISTEN: &str = "127.0.0.1:19011";
    const METRICS_URL: &str = "http://127.0.0.1:19011/metrics";

    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let owner = Bot::login("owner").await?;
    let bridge = Bot::login("whatsappbot").await?;

    // The orphan: built and marked like any portal, the owner invited — then
    // the only member leaves. Synapse now has no server that is in the room.
    let orphan = make_whatsapp_portal(&bridge, "owner-device-orphan").await?;
    bridge.invite(&orphan, OWNER).await?;
    bridge.leave_room(&orphan).await?;

    let mut env = owner_device_env(
        "unjoinable-portal",
        Some((owner.access_token(), owner.device_id())),
    );
    env.push((
        "SENSOR_METRICS_LISTEN".to_owned(),
        METRICS_LISTEN.to_owned(),
    ));
    let sensor = SensorProc::start(&env)?;

    // The first pass over the orphan has happened once its refusal is logged.
    poll_until(
        || async {
            sensor
                .logs()
                .await
                .iter()
                .any(|line| line.contains("can never be joined"))
                .then_some(())
        },
        "waiting for the orphan portal to be given up on",
    )
    .await?;

    // A portal that arrives *after* that: joining it proves the loop ran again.
    let portal = make_whatsapp_portal(&bridge, "owner-device-after-orphan").await?;
    bridge.invite(&portal, OWNER).await?;
    bridge.wait_for_membership(&portal, OWNER, "join").await?;

    let logs = sensor.logs().await;
    let given_up = logs
        .iter()
        .filter(|line| line.contains(&orphan) && line.contains("can never be joined"))
        .count();
    assert_eq!(given_up, 1, "one refusal, not a stream: {logs:?}");
    assert!(
        !logs
            .iter()
            .any(|line| line.contains(&orphan) && line.contains("failed to join a portal")),
        "a permanent answer is never retried: {logs:?}"
    );
    assert_eq!(
        bridge.get_membership(&orphan, OWNER).await.ok().as_deref(),
        Some("invite"),
        "the orphan invitation is left where it was"
    );

    // The stack persists across runs, so the owner may hold orphans from
    // earlier runs too: what is asserted is that this one is counted, and
    // that the gauge and the once-per-room counter agree with each other.
    let metrics = reqwest::get(METRICS_URL).await?.text().await?;
    let sample = |name: &str| -> u64 {
        metrics
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.trim().parse().ok())
            .unwrap_or_else(|| panic!("no sample {name} in {metrics}"))
    };
    let unjoinable = sample("twalk_sensor_owner_device_invites_total{outcome=\"unjoinable\"} ");
    let portals = sample("twalk_sensor_owner_device_unjoinable_portals ");
    assert!(unjoinable >= 1, "the orphan is counted: {metrics}");
    assert_eq!(
        portals, unjoinable,
        "readable without grepping logs, and consistent: {metrics}"
    );

    sensor.stop().await;
    Ok(())
}

/// An approved reply is posted by the **owner's own account**, and the Sensor
/// says on the bus that it reached the contact.
#[tokio::test]
async fn an_approved_reply_is_posted_by_the_owners_own_account() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let owner = Bot::login("owner").await?;
    let bridge = Bot::login("whatsappbot").await?;

    let portal = make_whatsapp_portal(&bridge, "owner-device-reply").await?;
    bridge.invite(&portal, SENSOR_USER_ID).await?;
    bridge.invite(&portal, OWNER).await?;

    let sensor = SensorProc::start(&owner_device_env(
        "posts-as-the-owner",
        Some((owner.access_token(), owner.device_id())),
    ))?;
    bridge.wait_for_membership(&portal, OWNER, "join").await?;

    let body = "Oui, 20h c'est parfait.";
    let approved = approved_reply(&portal, body)?;
    let approval_id = approved["id"].as_str().unwrap().to_owned();
    bus.publish_event(REPLY_APPROVED_SUBJECT, &approved).await?;

    let posted = event_from(&bridge, &portal, OWNER, "the reply posted by the owner").await?;
    assert_eq!(
        posted.pointer("/content/body").and_then(Value::as_str),
        Some(body),
        "the posted content is the final, approved content"
    );

    // And `@sensor:` posted nothing at all: the two identities are not
    // interchangeable, and one of them is the one a bridge ignores.
    let events = bridge.room_events(&portal, 50).await?;
    assert!(
        events.iter().all(|event| {
            event.get("sender").and_then(Value::as_str) != Some(SENSOR_USER_ID)
                || event.get("type").and_then(Value::as_str) != Some("m.room.message")
        }),
        "the observing identity must never be the one that speaks"
    );

    // #216: the answer is on the bus, not only in a log line.
    let report = posted_report(&bus, &approval_id).await?;
    assert_eq!(report.header("reach"), Some("contact"));
    assert_eq!(report.header("posted-as"), Some(OWNER));
    assert_eq!(report.header("event-id"), Some(approval_id.as_str()));
    assert_eq!(
        report.header("Nats-Msg-Id"),
        Some(format!("{approval_id}:posted").as_str()),
        "the report needs a message id of its own: it shares the stream with the approval, which \
         was published under the event id"
    );
    assert_eq!(
        report.payload, approved,
        "the report carries the approval unchanged, so no schema moves to make room for it"
    );
    validate_against_contract(&report.payload, "persona.reply.approved")?;

    sensor.stop().await;
    Ok(())
}

/// The reply into an **encrypted** portal — which every real portal is.
///
/// The owner's device runs no message sync: its `/sync` asks for zero timeline
/// events and nothing is registered to receive them. The claim being tested is
/// that this is still enough for Megolm, and the only honest way to test it is
/// to have a **different device** decrypt what it wrote. The Sensor's own
/// client is that device: it is in the room, it holds its own crypto store, and
/// when it decrypts a message from the owner it publishes
/// `outbound.message.sent` (ADR 0018) — so that event appearing with the
/// approved body is proof the room key was really shared with a device the
/// owner's client only learned about through the send path itself.
#[tokio::test]
async fn an_encrypted_portal_reply_is_readable_by_another_device() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let owner = Bot::login("owner").await?;
    // The bridge needs the real SDK here: raw HTTP cannot create a room that
    // negotiates Megolm the way a mautrix portal does.
    let bridge = CryptoBot::login("whatsappbot").await?;
    let reader = Bot::login("whatsappbot").await?;

    let portal = make_encrypted_whatsapp_portal(&bridge, "owner-device-encrypted").await?;
    bridge.invite(&portal, SENSOR_USER_ID).await?;
    bridge.invite(&portal, OWNER).await?;

    let sensor = SensorProc::start(&owner_device_env(
        "encrypted-portal",
        Some((owner.access_token(), owner.device_id())),
    ))?;
    reader.wait_for_membership(&portal, OWNER, "join").await?;
    reader
        .wait_for_membership(&portal, SENSOR_USER_ID, "join")
        .await?;

    let body = "Chiffré, et envoyé par le compte du propriétaire.";
    let approved = approved_reply(&portal, body)?;
    let approval_id = approved["id"].as_str().unwrap().to_owned();
    bus.publish_event(REPLY_APPROVED_SUBJECT, &approved).await?;

    let posted = event_from(&reader, &portal, OWNER, "the encrypted reply").await?;
    assert_eq!(
        posted.get("type").and_then(Value::as_str),
        Some("m.room.encrypted"),
        "a write-only device still encrypts: the ciphertext is what reaches the room"
    );

    // Another device read it. Nothing else in this suite proves the room key
    // went anywhere.
    let decrypted = poll_until(
        || async {
            bus.fetch_room_messages(STREAM, OUTBOUND_SUBJECT, &portal)
                .await
                .ok()?
                .into_iter()
                .find(|m| m.payload.pointer("/data/body").and_then(Value::as_str) == Some(body))
        },
        "the Sensor's own device decrypting the owner-device's reply",
    )
    .await?;
    assert_eq!(
        decrypted.payload["subject"].as_str(),
        Some(OWNER),
        "the reply really is the user's own message, which is the whole point (ADR 0018)"
    );
    validate_against_contract(&decrypted.payload, "outbound.message.sent")?;

    let report = posted_report(&bus, &approval_id).await?;
    assert_eq!(report.header("reach"), Some("contact"));
    assert_eq!(report.header("posted-as"), Some(OWNER));

    sensor.stop().await;
    Ok(())
}

/// With no device of the owner's account, nothing about the send changes — and
/// everything about what is *claimed* of it does.
#[tokio::test]
async fn without_the_owners_device_the_reply_reaches_nobody_and_says_so() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let bridge = Bot::login("whatsappbot").await?;

    let portal = make_whatsapp_portal(&bridge, "owner-device-absent").await?;
    bridge.invite(&portal, SENSOR_USER_ID).await?;

    let sensor = SensorProc::start(&owner_device_env("no-owner-device", None))?;
    bridge
        .wait_for_membership(&portal, SENSOR_USER_ID, "join")
        .await?;

    let body = "Postée par le Sensor, et lue par personne.";
    let approved = approved_reply(&portal, body)?;
    let approval_id = approved["id"].as_str().unwrap().to_owned();
    bus.publish_event(REPLY_APPROVED_SUBJECT, &approved).await?;

    // Unchanged: the Sensor posts, exactly as it did before any of this.
    let posted = event_from(
        &bridge,
        &portal,
        SENSOR_USER_ID,
        "the reply posted by the Sensor",
    )
    .await?;
    assert_eq!(
        posted.pointer("/content/body").and_then(Value::as_str),
        Some(body)
    );

    // Changed: it is no longer reported as sent. This is the sentence #216 is
    // about — "published on your bus" and "delivered to the contact" are two
    // different facts, and only one of them happened.
    let report = posted_report(&bus, &approval_id).await?;
    assert_eq!(
        report.header("reach"),
        Some("nobody"),
        "a mautrix bridge relays only the logged-in user's own account"
    );
    assert_eq!(report.header("posted-as"), Some(SENSOR_USER_ID));

    let logs = sensor.logs().await;
    assert!(
        logs.iter().any(|line| {
            line.contains("no device of the owner's account configured") && line.contains("#123")
        }),
        "the degradation is stated once at startup and named after the issue: {logs:?}"
    );

    sensor.stop().await;
    Ok(())
}

/// A token for the wrong account is a configuration error, and the Sensor
/// refuses to start on it.
///
/// The alternative is that it starts, joins portal rooms as somebody else's
/// device and writes into other people's conversations under a Matrix ID nobody
/// chose — a defect whose first symptom is a contact receiving a reply from a
/// stranger, which is not a thing a deployment can take back.
#[tokio::test]
async fn a_token_for_another_account_is_refused_at_startup() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let somebody_else = Bot::login("bot_beta").await?;

    let mut sensor = SensorProc::start(&owner_device_env(
        "wrong-account",
        Some((somebody_else.access_token(), somebody_else.device_id())),
    ))?;

    poll_until(
        || async {
            let logs = sensor.logs().await;
            logs.iter()
                .any(|line| {
                    line.contains("SENSOR_OWNER_DEVICE_ACCESS_TOKEN belongs to")
                        && line.contains(STRANGER)
                        && line.contains(OWNER)
                })
                .then_some(())
        },
        "the Sensor naming both accounts and refusing to start",
    )
    .await?;
    // `is_running` takes the process mutably, so this is a bounded loop rather
    // than `poll_until`. The refusal is a startup one: it happens before the
    // sync loop, so a second is generous.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while sensor.is_running() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        !sensor.is_running(),
        "the Sensor must exit rather than run as a device of somebody else's account"
    );

    sensor.stop().await;
    Ok(())
}
