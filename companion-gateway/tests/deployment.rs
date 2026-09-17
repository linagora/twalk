//! Ticket #48, the Companion Gateway in the reference deployment: a
//! `docker compose up` of deploy/docker-compose brings the Gateway up
//! configured only through the environment file, healthy, answering its
//! health endpoint, exposing its metrics and serving the Companion's index
//! file on its own origin.
//!
//! The skeleton Gateway depends on no other service (no homeserver, no bus
//! yet), so this test brings up the `companion-gateway` service alone —
//! compose still interpolates the whole file, so the env file below carries
//! every documented variable, exactly as `.env.example` does.
//!
//! The deploy stack runs under its own compose project and host ports, next
//! to the harness's own stack: TWALK_DEPLOY_TEST_STACK (default
//! twalk-deploy-test), TWALK_DEPLOY_TEST_GATEWAY_PORT (default 18318 —
//! deliberately distinct from the 18328 default of
//! sensor/tests/deployment.rs, so two default-configured deploy stacks do
//! not collide on it). The
//! Gateway image is tagged per compose project (`twalk/companion-gateway:
//! <stack>`, issue #38) so that parallel worktrees never overwrite each
//! other's build; the operator default, twalk/companion-gateway:local, is
//! untouched.
//!
//! The stack and its image stay up between runs: that is what makes a warm
//! run fast. Set TWALK_DEPLOY_TEST_TEARDOWN=1 to drop both at the end of a
//! passing run instead — by hand, `docker compose -p <stack> down -v`
//! followed by `docker image rm twalk/companion-gateway:<stack>`.
//!
//! The credentials below are throwaway constants for the local, ephemeral
//! deploy-test stack (same category as the test-bot passwords) — the env
//! file they land in is generated in a temp directory, never committed.

mod harness;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use harness::{parse_exposition, poll_until};
use tokio::process::Command;

const SERVER_NAME: &str = "deploy.twalk";

fn deploy_stack() -> String {
    std::env::var("TWALK_DEPLOY_TEST_STACK").unwrap_or_else(|_| "twalk-deploy-test".to_owned())
}

fn gateway_port() -> String {
    std::env::var("TWALK_DEPLOY_TEST_GATEWAY_PORT").unwrap_or_else(|_| "18318".to_owned())
}

fn synapse_port() -> String {
    std::env::var("TWALK_DEPLOY_TEST_SYNAPSE_PORT").unwrap_or_else(|_| "18218".to_owned())
}

fn nats_port() -> String {
    std::env::var("TWALK_DEPLOY_TEST_NATS_PORT").unwrap_or_else(|_| "14418".to_owned())
}

fn gateway_base_url() -> String {
    format!("http://localhost:{}", gateway_port())
}

/// The tag the deploy stack's Gateway image is built and run under. One tag
/// per compose project, so two worktrees on two stacks each rebuild their own
/// image instead of overwriting a shared one (issue #38); compose project
/// names are already restricted to the characters a Docker tag accepts.
/// TWALK_GATEWAY_IMAGE overrides it, as it does for an operator.
fn gateway_image() -> String {
    std::env::var("TWALK_GATEWAY_IMAGE")
        .ok()
        .filter(|image| !image.is_empty())
        .unwrap_or_else(|| format!("twalk/companion-gateway:{}", deploy_stack()))
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
        "twalk-gateway-deploy-test-{}-{unique}.env",
        std::process::id()
    ));
    let gateway_image = gateway_image();
    let (gateway_port, synapse_port, nats_port) = (gateway_port(), synapse_port(), nats_port());
    let contents = format!(
        "MATRIX_DOMAIN={SERVER_NAME}\n\
         MATRIX_HTTP_PORT={synapse_port}\n\
         MATRIX_REGISTRATION_SHARED_SECRET=deploy-test-only-registration-shared-secret\n\
         MATRIX_MACAROON_SECRET=deploy-test-only-macaroon-secret\n\
         MATRIX_FORM_SECRET=deploy-test-only-form-secret\n\
         SENSOR_USER_ID=@sensor:{SERVER_NAME}\n\
         SENSOR_PASSWORD=deploy-test-only-password-sensor\n\
         SENSOR_ALLOWED_INVITERS=@bot_alpha:{SERVER_NAME}\n\
         SENSOR_STATE_DIR=/data\n\
         SENSOR_LOG_LEVEL=info\n\
         NATS_PORT={nats_port}\n\
         GATEWAY_HTTP_PORT={gateway_port}\n\
         GATEWAY_LOG_LEVEL=info,twalk_companion_gateway=debug\n\
         TWALK_GATEWAY_IMAGE={gateway_image}\n"
    );
    std::fs::write(&path, contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// Runs `docker compose` against the deploy stack with the test's env file.
async fn compose(env_file: &Path, args: &[&str], what: &str) -> Result<String> {
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
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether the run asked for the stack to be torn down at the end
/// (TWALK_DEPLOY_TEST_TEARDOWN=1). Off by default, as in the Sensor's
/// deployment test: the stack and its image stay up, and the next run reuses
/// them warm.
fn teardown_requested() -> bool {
    matches!(
        std::env::var("TWALK_DEPLOY_TEST_TEARDOWN").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Removes what this run created: the Gateway service, its per-stack image
/// and the generated env file. The rest of the stack (Synapse, the bus) is
/// left to the Sensor's deployment test, which owns it.
async fn teardown(env_file: &Path) -> Result<()> {
    compose(
        env_file,
        &["rm", "-sfv", "companion-gateway"],
        "rm companion-gateway",
    )
    .await?;
    let image = gateway_image();
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
    std::fs::remove_file(env_file)
        .with_context(|| format!("failed to remove {}", env_file.display()))?;
    Ok(())
}

/// The stack's containers as `docker compose ps --format json` reports them:
/// one JSON object per line, one line per service.
async fn compose_ps(env_file: &Path) -> Result<Vec<serde_json::Value>> {
    let stdout = compose(env_file, &["ps", "-a", "--format", "json"], "ps").await?;
    Ok(stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect())
}

#[tokio::test]
async fn the_compose_stack_serves_the_companion_with_health_and_metrics() -> Result<()> {
    let env_file = write_env_file()?;

    // Build first so a build failure is attributed to the build; warm, this
    // is a cache hit. Then bring the service up exactly as an operator
    // would — the skeleton Gateway needs nothing else in the stack.
    compose(&env_file, &["build", "companion-gateway"], "build").await?;
    compose(
        &env_file,
        &["up", "-d", "--wait", "companion-gateway"],
        "up",
    )
    .await?;

    // The container runs this stack's own image, not a shared tag another
    // worktree's checkout may have rebuilt under it (issue #38), and compose
    // reports it healthy — `up --wait` returned, and the healthcheck is the
    // Gateway's own health endpoint.
    let entries = compose_ps(&env_file).await?;
    let entry = entries
        .iter()
        .find(|entry| entry["Service"].as_str() == Some("companion-gateway"))
        .context("the companion-gateway service has no container")?;
    assert_eq!(
        entry["Image"].as_str(),
        Some(gateway_image().as_str()),
        "the Gateway container must run this stack's own image: {entry}"
    );
    assert_eq!(
        entry["Health"].as_str(),
        Some("healthy"),
        "compose must report the Gateway healthy: {entry}"
    );

    let base = gateway_base_url();
    let health = poll_until(
        || async {
            let response = reqwest::get(format!("{base}/health")).await.ok()?;
            (response.status() == reqwest::StatusCode::OK)
                .then_some(response)?
                .text()
                .await
                .ok()
        },
        "the deployed Gateway's health endpoint",
    )
    .await?;
    let health: serde_json::Value = serde_json::from_str(&health)?;
    assert_eq!(health["status"].as_str(), Some("ok"));

    // Metrics, in the Sensor's exposition conventions.
    let metrics = reqwest::get(format!("{base}/metrics"))
        .await?
        .text()
        .await?;
    let samples = parse_exposition(&metrics);
    assert!(
        samples
            .iter()
            .any(|(name, _)| name == "twalk_companion_gateway_uptime_seconds"),
        "the deployed Gateway must expose its metrics:\n{metrics}"
    );

    // The Companion's origin: the image ships a holding page until the PWA
    // is built, and the operator mounts their own build over it.
    let index = reqwest::get(format!("{base}/")).await?;
    assert_eq!(index.status(), reqwest::StatusCode::OK);
    assert_eq!(
        index
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.starts_with("text/html")),
        Some(true),
        "the origin root serves an HTML index file"
    );
    assert!(
        index.text().await?.contains("Companion"),
        "the index file served is the Companion's"
    );

    // Only on request: a service left up (with its image) is what makes the
    // next run warm. Deliberately after the assertions, so a failure leaves
    // the container and its logs in place to inspect.
    if teardown_requested() {
        teardown(&env_file).await?;
    }
    Ok(())
}
