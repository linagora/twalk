// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Ticket 10, observability: an operator can tell at a glance whether the
//! Sensor is healthy — a Prometheus metrics endpoint (events published per
//! type, decryption failures, sync age, outbound send failures, dead-lettered
//! events), structured logs with an env-configurable level, a W3C
//! `traceparent` originated on every inbound event, and a clean shutdown on
//! SIGTERM that drains in-flight publishes.

mod harness;

use anyhow::Result;
use harness::{
    ensure_stack, make_whatsapp_portal, poll_until, sensor_env, sensor_env_with,
    validate_against_contract, Bot, Bus, SensorProc, SENSOR_USER_ID,
};

const STREAM: &str = "twalk";
const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";
/// The metrics listen address these tests bind the Sensor to; unique to this
/// stack so parallel worktrees never collide.
const METRICS_LISTEN: &str = "127.0.0.1:19010";
const METRICS_URL: &str = "http://127.0.0.1:19010/metrics";

/// A metric sample name with its value, parsed from the text exposition.
fn parse_exposition(body: &str) -> Vec<(String, u64)> {
    body.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (name, value) = line
                .rsplit_once(' ')
                .unwrap_or_else(|| panic!("metric line has no value: {line:?}"));
            let value = value
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("metric value is not an integer: {line:?}"));
            (name.to_owned(), value)
        })
        .collect()
}

/// The stricter W3C origin form the Sensor originates: `00-<32 lowercase
/// hex>-<16 lowercase hex>-01` (the schema pattern allows any two hex flags).
fn assert_valid_traceparent(value: &str) {
    let parts: Vec<&str> = value.split('-').collect();
    assert_eq!(parts.len(), 4, "a traceparent has four parts: {value}");
    assert_eq!(parts[0], "00", "traceparent version: {value}");
    assert_eq!(parts[1].len(), 32, "trace id length: {value}");
    assert_eq!(parts[2].len(), 16, "span id length: {value}");
    assert_eq!(parts[3], "01", "trace flags (sampled): {value}");
    for hex in [&parts[1], &parts[2]] {
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "ids are lowercase hex: {value}"
        );
    }
}

#[tokio::test]
async fn the_metrics_endpoint_reports_sensor_health() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env_with(&[(
        "SENSOR_METRICS_LISTEN",
        METRICS_LISTEN,
    )]))?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = make_whatsapp_portal(&alpha, "metrics-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    alpha.send_message(&room_id, "on décale à 20h ?").await?;

    bus.wait_for_room_message(STREAM, MESSAGE_SUBJECT, &room_id)
        .await?;

    // The publish ack may still be in flight when the event appears on the
    // bus, so poll the endpoint until the counter reflects it.
    let body = poll_until(
        || async {
            let body = reqwest::get(METRICS_URL).await.ok()?.text().await.ok()?;
            parse_exposition(&body)
                .iter()
                .any(|(name, value)| {
                    name == "twalk_sensor_events_published_total{type=\"fr.linagora.twalk.inbound.message.received.v1\"}"
                        && *value >= 1
                })
                .then_some(body)
        },
        "the published counter to appear on the metrics endpoint",
    )
    .await?;

    let samples = parse_exposition(&body);
    for expected in [
        "twalk_sensor_decryption_failures_total",
        "twalk_sensor_outbound_send_failures_total",
        "twalk_sensor_dead_lettered_events_total",
        "twalk_sensor_last_sync_age_seconds",
    ] {
        assert!(
            samples.iter().any(|(name, _)| name == expected),
            "the exposition must carry {expected}:\n{body}"
        );
    }

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn inbound_events_originate_a_valid_traceparent() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = make_whatsapp_portal(&alpha, "traceparent-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    alpha.send_message(&room_id, "on décale à 20h ?").await?;

    let stored = bus
        .wait_for_room_message(STREAM, MESSAGE_SUBJECT, &room_id)
        .await?;
    validate_against_contract(&stored.payload, "inbound.message.received")?;

    let traceparent = stored.payload["traceparent"]
        .as_str()
        .expect("an inbound event carries a traceparent");
    assert_valid_traceparent(traceparent);
    assert_eq!(
        stored.header("traceparent"),
        Some(traceparent),
        "the traceparent is duplicated as a NATS header for server-side filtering"
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn sigterm_shuts_the_sensor_down_cleanly() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let sensor = SensorProc::start(&sensor_env())?;

    // Wait until the Sensor is fully up — the stream persists across runs,
    // so its existence says nothing about this process; the "sensor running"
    // log line is emitted right before the sync loop and signal handlers
    // start.
    poll_until(
        || async {
            sensor
                .logs()
                .await
                .iter()
                .any(|line| line.contains("sensor running"))
                .then_some(())
        },
        "the sensor to enter its sync loop",
    )
    .await?;

    let status = sensor.terminate().await?;
    assert_eq!(
        status.code(),
        Some(0),
        "SIGTERM must shut the Sensor down with exit code 0, got {status}"
    );
    Ok(())
}

#[tokio::test]
async fn the_log_level_env_is_honored() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;

    // At `error`, the Sensor's routine info-level startup lines stay silent.
    // Readiness marker: the metrics listener is bound right after the first
    // startup log line would have been emitted, so a served scrape proves
    // startup passed that point — no sensor log of its own appears at error
    // level in a healthy run, and bus-side markers persist across runs.
    let quiet = SensorProc::start(&sensor_env_with(&[
        ("SENSOR_LOG_LEVEL", "error"),
        ("SENSOR_METRICS_LISTEN", METRICS_LISTEN),
    ]))?;
    poll_until(
        || async {
            reqwest::get(METRICS_URL)
                .await
                .ok()?
                .error_for_status()
                .ok()?;
            Some(())
        },
        "the quiet sensor's metrics endpoint",
    )
    .await?;
    // Give the log capture a beat to drain, then assert the silence.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let lines = quiet.logs().await;
    assert!(
        lines.iter().all(|line| !line.contains("sensor starting")),
        "SENSOR_LOG_LEVEL=error must suppress the info-level startup lines:\n{}",
        lines.join("\n")
    );
    quiet.stop().await;

    // With the harness default (info), the same lines show.
    let loud = SensorProc::start(&sensor_env())?;
    poll_until(
        || async {
            loud.logs()
                .await
                .iter()
                .any(|line| line.contains("sensor running"))
                .then_some(())
        },
        "the info-level startup lines",
    )
    .await?;
    loud.stop().await;
    Ok(())
}
