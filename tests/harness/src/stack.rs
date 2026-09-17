//! The test stack's lifecycle: a real Synapse and a real NATS JetStream,
//! brought up from `compose.test.yaml` next to this crate.
//!
//! The stack is shared by every component's suite — it is the seam's other
//! side, not the Sensor's private fixture — and it is NOT the reference
//! deployment (that lives in `deploy/` and is exercised by
//! `sensor/tests/deployment.rs`).

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::process::Command;

/// The Matrix server name the test Synapse answers for (see
/// `synapse/homeserver.yaml`).
pub const SERVER_NAME: &str = "test.twalk";

/// The stack is parameterizable so that parallel worktrees each run their
/// own isolated instance: TWALK_TEST_STACK names the compose project,
/// TWALK_TEST_SYNAPSE_PORT / TWALK_TEST_NATS_PORT move the host ports.
/// Defaults match the main checkout.
pub fn synapse_url() -> String {
    let port = std::env::var("TWALK_TEST_SYNAPSE_PORT").unwrap_or_else(|_| "18008".to_owned());
    format!("http://localhost:{port}")
}

pub fn nats_url() -> String {
    let port = std::env::var("TWALK_TEST_NATS_PORT").unwrap_or_else(|_| "14222".to_owned());
    format!("nats://localhost:{port}")
}

/// The compose project the stack runs under. The default keeps the name the
/// Sensor suite bootstrapped the stack with: renaming it would leave every
/// developer's running project behind, holding the host ports the new one
/// needs.
fn stack_id() -> String {
    std::env::var("TWALK_TEST_STACK").unwrap_or_else(|_| "twalk-sensor-test".to_owned())
}

/// This crate's own directory: the compose file, the Synapse configuration
/// and the provisioning script travel with the harness, so a component's
/// suite needs no knowledge of where they live.
fn harness_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Brings the compose stack up (idempotent) and provisions the bots.
/// Safe to call at the top of every test: the bootstrap is serialized
/// through a OnceCell (all tests of one binary share one process), so the
/// first caller does the work and concurrent callers wait for it instead of
/// racing parallel `docker compose up` invocations on a cold volume.
pub async fn ensure_stack() -> Result<()> {
    static STACK: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    STACK.get_or_try_init(do_ensure_stack).await?;
    Ok(())
}

async fn do_ensure_stack() -> Result<()> {
    let compose = harness_dir().join("compose.test.yaml");
    let status = Command::new("docker")
        .args([
            "compose".to_owned(),
            "-p".to_owned(),
            stack_id(),
            "-f".to_owned(),
            compose.to_string_lossy().into_owned(),
            "up".to_owned(),
            "-d".to_owned(),
            "--wait".to_owned(),
        ])
        .stdout(Stdio::null())
        .status()
        .await
        .context("failed to run docker compose up")?;
    if !status.success() {
        bail!("docker compose up failed with {status}");
    }

    let status = Command::new(harness_dir().join("scripts").join("provision-bots.sh"))
        .status()
        .await
        .context("failed to run provision-bots.sh")?;
    if !status.success() {
        bail!("bot provisioning failed with {status}");
    }
    Ok(())
}
