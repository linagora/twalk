// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Ticket 11, reference compose deployment: a fresh `docker compose up` of
//! deploy/docker-compose — Synapse, NATS JetStream and the Sensor wired
//! together, configured only through the environment file — produces a
//! working pipeline with zero manual steps: the stack's provision one-shot
//! creates the Sensor account, a test bot invites the Sensor into a portal
//! room, sends a message, and a schema-valid inbound.message.received.v1
//! event lands on the deploy stack's bus.
//!
//! The deploy stack runs under its own compose project and host ports, next
//! to the harness's own stack: TWALK_DEPLOY_TEST_STACK (default
//! twalk-deploy-test), TWALK_DEPLOY_TEST_SYNAPSE_PORT (default 18218),
//! TWALK_DEPLOY_TEST_NATS_PORT (default 14418). The stack stays up between
//! runs; `docker compose -p twalk-deploy-test down -v` resets it to fresh.
//!
//! The credentials below are throwaway constants for the local, ephemeral
//! deploy-test stack (same category as the test-bot passwords) — the env
//! file they land in is generated in a temp directory, never committed.

mod harness;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use harness::{make_whatsapp_portal, validate_against_contract, Bot, Bus};
use tokio::process::Command;
use tokio::time::sleep;

const STREAM: &str = "twalk";
const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";
const SERVER_NAME: &str = "deploy.twalk";
const SENSOR_USER_ID: &str = "@sensor:deploy.twalk";
const BRIDGE_LOCALPART: &str = "bot_alpha";
const BRIDGE_PASSWORD: &str = "deploy-test-only-password-bot-alpha";

fn deploy_stack() -> String {
    std::env::var("TWALK_DEPLOY_TEST_STACK").unwrap_or_else(|_| "twalk-deploy-test".to_owned())
}

fn synapse_url() -> String {
    let port =
        std::env::var("TWALK_DEPLOY_TEST_SYNAPSE_PORT").unwrap_or_else(|_| "18218".to_owned());
    format!("http://localhost:{port}")
}

fn nats_url() -> String {
    let port = std::env::var("TWALK_DEPLOY_TEST_NATS_PORT").unwrap_or_else(|_| "14418".to_owned());
    format!("nats://localhost:{port}")
}

fn deploy_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../deploy/docker-compose")
}

/// Writes the environment file the deploy stack is configured through:
/// exactly what `.env.example` documents, with throwaway test values and the
/// test's own host ports.
fn write_env_file() -> Result<PathBuf> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "twalk-deploy-test-{}-{unique}.env",
        std::process::id()
    ));
    let synapse_port = synapse_url().rsplit(':').next().unwrap().to_owned();
    let nats_port = nats_url().rsplit(':').next().unwrap().to_owned();
    let contents = format!(
        "MATRIX_DOMAIN={SERVER_NAME}\n\
         MATRIX_HTTP_PORT={synapse_port}\n\
         MATRIX_REGISTRATION_SHARED_SECRET=deploy-test-only-registration-shared-secret\n\
         MATRIX_MACAROON_SECRET=deploy-test-only-macaroon-secret\n\
         MATRIX_FORM_SECRET=deploy-test-only-form-secret\n\
         SENSOR_USER_ID={SENSOR_USER_ID}\n\
         SENSOR_PASSWORD=deploy-test-only-password-sensor\n\
         SENSOR_ALLOWED_INVITERS=@{BRIDGE_LOCALPART}:{SERVER_NAME}\n\
         SENSOR_STATE_DIR=/data\n\
         SENSOR_LOG_LEVEL=info,twalk_sensor=debug\n\
         NATS_PORT={nats_port}\n"
    );
    std::fs::write(&path, contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// Runs `docker compose` against the deploy stack with the test's env file.
async fn compose(env_file: &Path, args: &[&str], what: &str) -> Result<()> {
    let output = Command::new("docker")
        .arg("compose")
        .arg("-p")
        .arg(deploy_stack())
        .arg("--env-file")
        .arg(env_file)
        .arg("-f")
        .arg(deploy_dir().join("compose.yaml"))
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to run docker compose {what}"))?;
    if !output.status.success() {
        bail!(
            "docker compose {what} failed with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// The documented ad-hoc provisioning step (deploy/docker-compose/
/// provision.sh), used here for the test bot standing in for a bridge —
/// operator-side setup. The Sensor account itself is NOT provisioned by the
/// test: the stack's provision one-shot service must have created it.
async fn provision(env_file: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new(deploy_dir().join("provision.sh"))
        .args(args)
        .env("TWALK_ENV_FILE", env_file)
        .env("TWALK_COMPOSE_PROJECT", deploy_stack())
        .output()
        .await
        .context("failed to run provision.sh")?;
    if !output.status.success() {
        bail!(
            "provision.sh {args:?} failed with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Asserts the stack's provision one-shot ran and exited successfully:
/// `docker compose ps --format json` prints one JSON object per service.
async fn assert_sensor_account_provisioned(env_file: &Path) -> Result<()> {
    let output = Command::new("docker")
        .arg("compose")
        .arg("-p")
        .arg(deploy_stack())
        .arg("--env-file")
        .arg(env_file)
        .arg("-f")
        .arg(deploy_dir().join("compose.yaml"))
        .args(["ps", "-a", "--format", "json"])
        .output()
        .await
        .context("failed to run docker compose ps")?;
    if !output.status.success() {
        bail!(
            "docker compose ps failed with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let entry = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|entry| entry["Service"].as_str() == Some("provision"))
        .context("the provision one-shot service has no container")?;
    assert_eq!(
        entry["ExitCode"].as_i64(),
        Some(0),
        "the provision one-shot must have created the Sensor account: {entry}"
    );
    Ok(())
}

/// Waits until the deploy stack's Sensor reached its sync loop. The sensor
/// container may have crash-looped until its account was provisioned
/// (restart: unless-stopped), so allow a generous minute.
async fn wait_for_sensor_running(env_file: &Path) -> Result<()> {
    for _ in 0..120 {
        let output = Command::new("docker")
            .arg("compose")
            .arg("-p")
            .arg(deploy_stack())
            .arg("--env-file")
            .arg(env_file)
            .arg("-f")
            .arg(deploy_dir().join("compose.yaml"))
            .args(["logs", "sensor"])
            .output()
            .await
            .context("failed to run docker compose logs")?;
        if String::from_utf8_lossy(&output.stdout).contains("sensor running") {
            return Ok(());
        }
        sleep(Duration::from_millis(500)).await;
    }
    bail!("the deploy stack's sensor did not reach its sync loop within 60s")
}

#[tokio::test]
async fn a_fresh_compose_up_produces_events_without_manual_steps() -> Result<()> {
    // Not a host SensorProc, but the same mutual exclusion applies: this test
    // runs a full Sensor, just containerized.
    let _guard = harness::SENSOR_LOCK.lock().await;
    let env_file = write_env_file()?;

    // Build first so a build failure is attributed to the build; warm, this
    // is a cache hit. Then bring the whole stack up in one go, exactly as an
    // operator would.
    compose(&env_file, &["build"], "build").await?;
    compose(&env_file, &["up", "-d", "--wait"], "up").await?;

    // Zero manual steps for the Sensor account: the stack's provision
    // one-shot created it on the way up. The Sensor reaching its sync loop
    // is the proof the account exists — its first act is a password login.
    assert_sensor_account_provisioned(&env_file).await?;
    wait_for_sensor_running(&env_file).await?;

    // Operator-side setup only: the test bot standing in for a bridge.
    provision(&env_file, &[BRIDGE_LOCALPART, BRIDGE_PASSWORD]).await?;

    let alpha = Bot::login_with(&synapse_url(), SERVER_NAME, BRIDGE_LOCALPART, BRIDGE_PASSWORD)
        .await?;
    let room_id = make_whatsapp_portal(&alpha, "deploy-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    alpha
        .send_message(&room_id, "deployment, no manual steps")
        .await?;

    let bus = Bus::connect_to(&nats_url()).await?;
    let stored = bus
        .wait_for_room_message_on(SERVER_NAME, STREAM, MESSAGE_SUBJECT, &room_id)
        .await?;
    let event = &stored.payload;
    validate_against_contract(event, "inbound.message.received")?;
    assert_eq!(
        event["type"].as_str(),
        Some("fr.linagora.twalk.inbound.message.received.v1")
    );
    assert_eq!(
        event["source"].as_str(),
        Some(format!("matrix://{SERVER_NAME}/{room_id}").as_str())
    );
    assert_eq!(event["network"].as_str(), Some("whatsapp"));
    assert_eq!(
        event["subject"].as_str(),
        Some(format!("@{BRIDGE_LOCALPART}:{SERVER_NAME}").as_str())
    );
    assert_eq!(
        event["data"]["body"].as_str(),
        Some("deployment, no manual steps")
    );
    Ok(())
}
