// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Ticket 02, lifecycle: the Sensor joins rooms on invitation from an
//! allowed inviter, ignores everyone else, and stops observing a room
//! after being removed from it.

mod harness;

use anyhow::Result;
use harness::{
    ensure_stack, make_whatsapp_portal, poll_until, sensor_env, sensor_env_with, Bot, Bus,
    SensorProc, SENSOR_USER_ID,
};

const STREAM: &str = "twalk";
const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";

#[tokio::test]
async fn joins_when_invited_by_an_allowed_inviter_and_ignores_others() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;
    let beta = Bot::login("bot_beta").await?;

    let portal = alpha.create_room("portal-allowed", false).await?;
    alpha.invite(&portal, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&portal, SENSOR_USER_ID, "join")
        .await?;

    // An invitation from anyone else is ignored: still "invite" after the
    // Sensor has had ample time to act on it.
    let not_a_bridge = beta.create_room("not-a-bridge", false).await?;
    beta.invite(&not_a_bridge, SENSOR_USER_ID).await?;
    beta.wait_for_membership(&not_a_bridge, SENSOR_USER_ID, "invite")
        .await?;
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    assert_eq!(
        beta.get_membership(&not_a_bridge, SENSOR_USER_ID).await?,
        "invite",
        "the Sensor must not join a room on a disallowed invitation"
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn leaves_the_room_after_being_removed() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let portal = alpha.create_room("portal-removed", false).await?;
    alpha.invite(&portal, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&portal, SENSOR_USER_ID, "join")
        .await?;

    alpha.kick(&portal, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&portal, SENSOR_USER_ID, "leave")
        .await?;

    sensor.stop().await;
    Ok(())
}

/// Issue #254 / ADR 0029: the register notices a move, not the Sensor — the
/// Sensor's part is two facts. Nothing that arrives in a room carrying an
/// `m.room.tombstone` is published: bridges post to the successor, so what
/// lands in a dead room is stray, and publishing it would attribute a
/// conversation to a room the register no longer lists. And once the Sensor
/// joins the successor — invited there by the register, played here by the
/// bridge bot — it leaves the predecessor, because its membership there is
/// what the register reads as `observing` and what `observed_rooms` counts:
/// a dead room in that count is #232's drift, one migration at a time.
#[tokio::test]
async fn a_dead_room_publishes_nothing_and_is_left_once_the_successor_is_joined() -> Result<()> {
    const METRICS_LISTEN: &str = "127.0.0.1:19012";
    const METRICS_URL: &str = "http://127.0.0.1:19012/metrics";

    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env_with(&[(
        "SENSOR_METRICS_LISTEN",
        METRICS_LISTEN,
    )]))?;
    let alpha = Bot::login("bot_alpha").await?;
    let contact = Bot::login("bot_beta").await?;

    let old = make_whatsapp_portal(&alpha, "portal-that-moves").await?;
    alpha.invite(&old, SENSOR_USER_ID).await?;
    alpha.invite(&old, contact.user_id()).await?;
    contact.join_room(&old).await?;
    alpha
        .wait_for_membership(&old, SENSOR_USER_ID, "join")
        .await?;
    // The room counts, and a message in it reaches the bus: the baseline.
    let counted = poll_until(
        || async {
            let body = reqwest::get(METRICS_URL).await.ok()?.text().await.ok()?;
            sample_of(&body, "twalk_sensor_observed_rooms ")
        },
        "the observed-rooms gauge",
    )
    .await?;
    contact.send_message(&old, "before the move").await?;
    bus.wait_for_room_message(STREAM, MESSAGE_SUBJECT, &old)
        .await?;
    let published_before = bus
        .fetch_room_messages(STREAM, MESSAGE_SUBJECT, &old)
        .await?
        .len();

    // The migration: the homeserver replaces the room and makes the old one
    // read-only for everybody but the account that upgraded it — so the one
    // thing that can still speak in a dead room is the bridge itself, as its
    // bot does when it leaves a notice behind. The bridge re-marks the
    // successor.
    let new = alpha.upgrade_room(&old).await?;
    let (state_key, content) = harness::whatsapp_bridge_state(alpha.user_id(), "portal-that-moves");
    alpha
        .send_state_event(&new, "m.bridge", &state_key, content)
        .await?;
    alpha
        .send_message(&old, "shouting into a dead room")
        .await?;
    // Something the Sensor *does* publish, sent after the stray one, proves
    // the stray was seen and dropped rather than not yet processed.
    let dropped = poll_until(
        || async {
            let body = reqwest::get(METRICS_URL).await.ok()?.text().await.ok()?;
            sample_of(
                &body,
                "twalk_sensor_events_dropped_total{reason=\"tombstoned_room\"} ",
            )
            .filter(|count| *count >= 1)
        },
        "the stray message to be counted as dropped",
    )
    .await?;
    assert!(dropped >= 1);
    assert_eq!(
        bus.fetch_room_messages(STREAM, MESSAGE_SUBJECT, &old)
            .await?
            .len(),
        published_before,
        "nothing from a dead room reaches the bus"
    );

    // The register would invite the Sensor into the successor (#255); the
    // bridge bot does it here. Joining the successor is what makes the Sensor
    // leave the room it replaced.
    alpha.invite(&new, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&new, SENSOR_USER_ID, "join")
        .await?;
    alpha
        .wait_for_membership(&old, SENSOR_USER_ID, "leave")
        .await?;
    let after = poll_until(
        || async {
            let body = reqwest::get(METRICS_URL).await.ok()?.text().await.ok()?;
            sample_of(&body, "twalk_sensor_observed_rooms ").filter(|rooms| *rooms == counted)
        },
        "the observed-rooms gauge back to its value before the move",
    )
    .await?;
    assert_eq!(after, counted, "one conversation, one room counted");

    let logs = sensor.logs().await;
    assert!(
        logs.iter()
            .any(|line| line.contains("left the room it replaced")),
        "the departure is logged once, naming both rooms: {logs:?}"
    );

    sensor.stop().await;
    Ok(())
}

/// A Prometheus sample's value, by the exact prefix of its line.
fn sample_of(body: &str, prefix: &str) -> Option<u64> {
    body.lines()
        .find_map(|line| line.strip_prefix(prefix))
        .and_then(|rest| rest.trim().parse().ok())
}

/// A homeserver whose password login is disabled (`password_config.enabled:
/// false`, the normal SSO-only configuration) leaves an operator with no
/// password to give the Sensor: the only way in is a token for a
/// pre-provisioned device (#72). The test stack does allow password login,
/// so it stands in for the operator's out-of-band step — Synapse's admin
/// registration API returns the same pair — and then starts the Sensor with
/// no password at all.
#[tokio::test]
async fn starts_from_a_configured_access_token_without_a_password() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let credentials = Bot::login("sensor").await?;
    let sensor = SensorProc::start(&harness::sensor_env_with(&[
        ("SENSOR_PASSWORD", ""),
        ("SENSOR_ACCESS_TOKEN", credentials.access_token()),
        ("SENSOR_DEVICE_ID", credentials.device_id()),
    ]))?;
    let alpha = Bot::login("bot_alpha").await?;

    // Joining an invited room proves the session is real: the token was
    // accepted by the homeserver and the sync loop is running.
    let portal = alpha.create_room("portal-token-start", false).await?;
    alpha.invite(&portal, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&portal, SENSOR_USER_ID, "join")
        .await?;

    assert!(
        sensor
            .logs()
            .await
            .iter()
            .any(|line| line.contains("started from the configured access token")),
        "the Sensor must start from the token, not fall back to a password login"
    );

    sensor.stop().await;
    Ok(())
}
