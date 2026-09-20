// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Issue #174, ADR 0037: the bus has a retention policy, and the Sensor is
//! what puts it there.
//!
//! Until this ticket the `twalk` stream was created with `..Default::default()`
//! — every event kept for ever, no size ceiling, no compression, and a
//! `Nats-Msg-Id` remembered for two minutes — and nobody had decided any of
//! it. Three things are asserted here at the process boundary, on the bus's
//! own account of the stream (`STREAM.INFO`) and never on what the Sensor
//! says about itself:
//!
//! - after the Sensor starts, the stream carries the policy, every field;
//! - a stream that already existed with the defaults is **updated in
//!   place**, and the Sensor says field by field what changed; a stream
//!   that already carries the policy is left alone and the Sensor says so;
//! - a `Nats-Msg-Id` published twice is **one** message.
//!
//! The duplicate window is asserted from the bus's configuration: it is a
//! day. The test the ticket sketches — publish, wait past the old two-minute
//! window, publish again, count one — is **not** a test this suite runs,
//! because a two-minute wait in a required suite is a two-minute wait on
//! every pull request; what it would prove is that JetStream honours its own
//! `duplicate_window`, and what this suite proves is that the window is the
//! one the operator decided.
//!
//! These tests run on the shared `twalk` test stream — the Sensor's stream
//! name is a constant, and every Sensor test starts a Sensor that reconciles
//! it — which is compatible with the rest of the suite: the harness's
//! `Bus::ensure_stream` reuses an existing stream whatever its configuration,
//! and the policy changes nothing a consumer sees. The first test puts the
//! stream back into the default shape before it starts, which is what a
//! deployment upgraded across this ticket looks like; the other Sensor tests
//! never read the stream's configuration, so the order does not matter to
//! them. Isolation is the suite's (`TWALK_TEST_STACK`, `TWALK_TEST_NATS_PORT`;
//! `sensor/tests/harness/mod.rs`).

mod harness;

use std::time::Duration;

use anyhow::{Context, Result};
use async_nats::jetstream::stream::{
    Compression, Config, DiscardPolicy, RetentionPolicy, StorageType,
};
use harness::{
    ensure_stack, nats_url, poll_until, sensor_env, sensor_env_with, SensorProc, SENSOR_LOCK,
};

const STREAM: &str = "twalk";
const NINETY_DAYS: Duration = Duration::from_secs(90 * 86_400);
const TWO_GIB: i64 = 2_147_483_648;
const A_DAY: Duration = Duration::from_secs(86_400);
/// What NATS remembers a `Nats-Msg-Id` for when nobody decided.
const NATS_DEFAULT_DUPLICATE_WINDOW: Duration = Duration::from_secs(120);

async fn jetstream() -> Result<async_nats::jetstream::Context> {
    let client = async_nats::connect(&nats_url())
        .await
        .context("failed to connect to the test bus")?;
    Ok(async_nats::jetstream::new(client))
}

/// The stream's configuration as the bus reports it now.
async fn stream_config(js: &async_nats::jetstream::Context) -> Result<Config> {
    let mut stream = js.get_stream(STREAM).await.context("no twalk stream")?;
    Ok(stream.info().await.context("stream info")?.config.clone())
}

/// Puts the stream back into the shape a `..Default::default()` deployment
/// left behind — what the reference deployment's own `meta.inf` read on
/// 2026-09-19: no age, no size ceiling, no compression, two minutes of
/// duplicate tracking — or creates it in that shape when there is none.
/// The bus fills the defaults in on an update the same way it does on a
/// create, so this is the same request the components made before #174.
async fn leave_the_stream_as_the_defaults_would(
    js: &async_nats::jetstream::Context,
) -> Result<Config> {
    let bare = Config {
        name: STREAM.to_owned(),
        subjects: vec!["twalk.>".to_owned()],
        ..Default::default()
    };
    let info = js
        .create_or_update_stream(bare)
        .await
        .context("failed to reset the stream to the defaults")?;
    Ok(info.config)
}

fn assert_carries_the_policy(config: &Config) {
    assert_eq!(config.max_age, NINETY_DAYS, "max_age: {config:?}");
    assert_eq!(config.max_bytes, TWO_GIB, "max_bytes: {config:?}");
    assert_eq!(config.discard, DiscardPolicy::Old, "discard: {config:?}");
    assert_eq!(
        config.compression,
        Some(Compression::S2),
        "compression: {config:?}"
    );
    assert_eq!(
        config.duplicate_window, A_DAY,
        "duplicate_window: {config:?}"
    );
    assert_eq!(config.storage, StorageType::File, "storage: {config:?}");
    assert_eq!(config.num_replicas, 1, "num_replicas: {config:?}");
    assert_eq!(
        config.retention,
        RetentionPolicy::Limits,
        "retention: {config:?}"
    );
    assert_eq!(config.max_messages, -1, "no count ceiling: {config:?}");
    assert_eq!(
        config.max_messages_per_subject, -1,
        "no per-subject count ceiling: {config:?}"
    );
    assert_eq!(config.subjects, vec!["twalk.>".to_owned()]);
}

/// A stream created with the defaults is updated in place when the Sensor
/// starts, field by field and said so; a stream that already carries the
/// policy is left alone, and said so too.
#[tokio::test]
async fn an_existing_stream_is_reconciled_onto_the_policy_in_place() -> Result<()> {
    ensure_stack().await?;
    let _guard = SENSOR_LOCK.lock().await;
    let js = jetstream().await?;

    // The precondition is asserted rather than assumed: a fixture that
    // already carried the policy would make the update path untested.
    let before = leave_the_stream_as_the_defaults_would(&js).await?;
    assert_eq!(before.max_age, Duration::ZERO, "for ever: {before:?}");
    assert_eq!(before.max_bytes, -1, "no ceiling: {before:?}");
    assert_ne!(before.compression, Some(Compression::S2), "{before:?}");
    assert_eq!(
        before.duplicate_window, NATS_DEFAULT_DUPLICATE_WINDOW,
        "two minutes: {before:?}"
    );

    let sensor = SensorProc::start(&sensor_env())?;
    let after = poll_until(
        || async {
            let config = stream_config(&js).await.ok()?;
            (config.max_age == NINETY_DAYS).then_some(config)
        },
        "waiting for the Sensor to reconcile the stream onto the policy",
    )
    .await?;
    assert_carries_the_policy(&after);

    // What changed is said field by field, old → new, and nothing was
    // refused: the four fields the operator decided are the four that
    // differ from the defaults, and none of them is one JetStream cannot
    // change in place.
    let logs = sensor.logs().await;
    for line in [
        "max_age: 0s (for ever) → 7776000s (90d)",
        "max_bytes: -1 → 2147483648",
        "compression: none → s2",
        "duplicate_window: 120s (2m) → 86400s (1d)",
    ] {
        assert!(
            logs.iter().any(|l| l.contains(line)),
            "the change is said: {line:?} in {logs:?}"
        );
    }
    assert!(
        !logs
            .iter()
            .any(|l| l.contains("refused to update the stream's retention policy")),
        "nothing was refused: {logs:?}"
    );
    sensor.stop().await;

    // A second start finds the policy in place and changes nothing.
    let sensor = SensorProc::start(&sensor_env())?;
    poll_until(
        || async {
            sensor
                .logs()
                .await
                .iter()
                .any(|l| l.contains("already carries the retention policy"))
                .then_some(())
        },
        "waiting for the Sensor to find the policy already in place",
    )
    .await?;
    let logs = sensor.logs().await;
    assert!(
        !logs
            .iter()
            .any(|l| l.contains("bus stream policy updated in place")),
        "a stream that carries the policy is not rewritten: {logs:?}"
    );
    assert_carries_the_policy(&stream_config(&js).await?);
    sensor.stop().await;
    Ok(())
}

/// The same `Nats-Msg-Id` published twice is one message on the bus — the
/// second publish is acknowledged as a duplicate at the first one's
/// sequence, and the last message on the subject is the first one — and the
/// window the bus remembers an id for is the day the operator decided, read
/// off the stream's configuration.
#[tokio::test]
async fn the_same_message_id_published_twice_is_one_message() -> Result<()> {
    ensure_stack().await?;
    let _guard = SENSOR_LOCK.lock().await;
    let js = jetstream().await?;

    let sensor = SensorProc::start(&sensor_env())?;
    let config = poll_until(
        || async {
            let config = stream_config(&js).await.ok()?;
            (config.duplicate_window == A_DAY).then_some(config)
        },
        "waiting for the Sensor to put the policy on the stream",
    )
    .await?;
    assert_eq!(
        config.duplicate_window, A_DAY,
        "the bus remembers an id for a day, not two minutes"
    );

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let subject = format!("twalk.test.bus-policy.{unique}");
    let msg_id = format!("bus-policy-{unique}");
    let publish = || async {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(async_nats::header::NATS_MESSAGE_ID, msg_id.as_str());
        js.publish_with_headers(subject.clone(), headers, "{}".into())
            .await
            .context("publish")?
            .await
            .context("publish ack")
    };
    let first = publish().await?;
    let second = publish().await?;
    assert!(!first.duplicate, "the first publish is stored: {first:?}");
    assert!(
        second.duplicate,
        "the second publish of the same id is a duplicate: {second:?}"
    );
    assert_eq!(
        second.sequence, first.sequence,
        "the duplicate is answered with the stored message's own sequence"
    );

    let stream = js.get_stream(STREAM).await?;
    let last = stream
        .get_last_raw_message_by_subject(&subject)
        .await
        .context("the subject holds a message")?;
    assert_eq!(
        last.sequence, first.sequence,
        "the bus holds one copy, the first: {last:?}"
    );

    sensor.stop().await;
    Ok(())
}

/// A value NATS would read as "no limit" is refused at startup, naming the
/// variable: a zero age is the undecided policy this ticket replaced, and
/// passing it through would restore it silently.
#[tokio::test]
async fn a_value_nats_reads_as_no_limit_is_refused_at_startup() -> Result<()> {
    ensure_stack().await?;
    let _guard = SENSOR_LOCK.lock().await;

    let mut sensor = SensorProc::start(&sensor_env_with(&[("SENSOR_BUS_MAX_AGE_DAYS", "0")]))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while sensor.is_running() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(!sensor.is_running(), "the Sensor refuses to start");
    let logs = sensor.logs().await;
    assert!(
        logs.iter().any(|l| l.contains("SENSOR_BUS_MAX_AGE_DAYS")),
        "the refusal names the variable: {logs:?}"
    );
    sensor.stop().await;
    Ok(())
}
