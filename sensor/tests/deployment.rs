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
//! TWALK_DEPLOY_TEST_NATS_PORT (default 14418),
//! TWALK_DEPLOY_TEST_GATEWAY_PORT (default 18328 — deliberately distinct
//! from the 18318 default of companion-gateway/tests/deployment.rs, so two
//! default-configured deploy stacks do not collide on it). The stack's
//! images are tagged per compose project (`twalk/sensor:<stack>`,
//! `twalk/companion-gateway:<stack>`, issue #38) so that parallel worktrees
//! never overwrite each other's build; the operator defaults,
//! twalk/sensor:local and twalk/companion-gateway:local, are untouched.
//!
//! The stack and its image stay up between runs: that is what makes a warm
//! run fast. Set TWALK_DEPLOY_TEST_TEARDOWN=1 to drop both at the end of a
//! passing run instead, leaving the Docker daemon as the test found it —
//! by hand, `docker compose -p <stack> down -v` followed by
//! `docker image rm twalk/sensor:<stack>`.
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

/// Host port the stack's Companion Gateway is published on. The Sensor's
/// pipeline does not touch it, but the whole stack comes up here, and the
/// operator default (8080) is a busy port on a developer's machine — and the
/// port two deploy stacks would collide on.
fn gateway_port() -> String {
    std::env::var("TWALK_DEPLOY_TEST_GATEWAY_PORT").unwrap_or_else(|_| "18328".to_owned())
}

/// The tag the deploy stack's Sensor image is built and run under. One tag
/// per compose project, so that two worktrees on two stacks each rebuild
/// their own image instead of overwriting a shared one (issue #38); compose
/// project names are already restricted to the characters a Docker tag
/// accepts. TWALK_SENSOR_IMAGE overrides it, as it does for an operator.
fn sensor_image() -> String {
    std::env::var("TWALK_SENSOR_IMAGE")
        .ok()
        .filter(|image| !image.is_empty())
        .unwrap_or_else(|| format!("twalk/sensor:{}", deploy_stack()))
}

/// [`sensor_image`] for the stack's Companion Gateway image.
fn gateway_image() -> String {
    format!("twalk/companion-gateway:{}", deploy_stack())
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
    let sensor_image = sensor_image();
    let gateway_port = gateway_port();
    let gateway_image = gateway_image();
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
         NATS_PORT={nats_port}\n\
         TWALK_SENSOR_IMAGE={sensor_image}\n\
         GATEWAY_HTTP_PORT={gateway_port}\n\
         TWALK_GATEWAY_IMAGE={gateway_image}\n"
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

/// Whether the run asked for the stack to be torn down at the end
/// (TWALK_DEPLOY_TEST_TEARDOWN=1). Off by default: the stack and its image
/// stay up, and the next run reuses them warm.
fn teardown_requested() -> bool {
    matches!(
        std::env::var("TWALK_DEPLOY_TEST_TEARDOWN").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Removes everything this run created: the stack's containers, networks and
/// volumes, its per-stack images, and the generated env file.
async fn teardown(env_file: &Path) -> Result<()> {
    compose(env_file, &["down", "-v"], "down").await?;
    for image in [sensor_image(), gateway_image()] {
        let output = Command::new("docker")
            .args(["image", "rm", &image])
            .output()
            .await
            .context("failed to run docker image rm")?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Tolerate an already-removed image: teardown stays idempotent.
        if !output.status.success() && !stderr.contains("No such image") {
            bail!(
                "docker image rm {image} failed with {}:\n{stderr}",
                output.status
            );
        }
    }
    std::fs::remove_file(env_file)
        .with_context(|| format!("failed to remove {}", env_file.display()))?;
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

/// The stack's containers as `docker compose ps --format json` reports them:
/// one JSON object per line, one line per service.
async fn compose_ps(env_file: &Path) -> Result<Vec<serde_json::Value>> {
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
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect())
}

/// Asserts the stack's provision one-shot ran and exited successfully.
async fn assert_sensor_account_provisioned(env_file: &Path) -> Result<()> {
    let entries = compose_ps(env_file).await?;
    let entry = entries
        .iter()
        .find(|entry| entry["Service"].as_str() == Some("provision"))
        .context("the provision one-shot service has no container")?;
    assert_eq!(
        entry["ExitCode"].as_i64(),
        Some(0),
        "the provision one-shot must have created the Sensor account: {entry}"
    );
    Ok(())
}

/// Asserts the running sensor container is this stack's own image, not a
/// shared tag another stack (another worktree's checkout) may have rebuilt
/// under it (issue #38).
async fn assert_sensor_runs_its_own_image(env_file: &Path) -> Result<()> {
    let entries = compose_ps(env_file).await?;
    let entry = entries
        .iter()
        .find(|entry| entry["Service"].as_str() == Some("sensor"))
        .context("the sensor service has no container")?;
    assert_eq!(
        entry["Image"].as_str(),
        Some(sensor_image().as_str()),
        "the sensor container must run this stack's own image: {entry}"
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
    assert_sensor_runs_its_own_image(&env_file).await?;
    wait_for_sensor_running(&env_file).await?;

    // Operator-side setup only: the test bot standing in for a bridge.
    provision(&env_file, &[BRIDGE_LOCALPART, BRIDGE_PASSWORD]).await?;

    let alpha = Bot::login_with(
        &synapse_url(),
        SERVER_NAME,
        BRIDGE_LOCALPART,
        BRIDGE_PASSWORD,
    )
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

    // Only on request: a stack left up (with its image) is what makes the
    // next run warm. Deliberately after the assertions, so a failure leaves
    // the stack and its logs in place to inspect.
    if teardown_requested() {
        teardown(&env_file).await?;
    }
    Ok(())
}
