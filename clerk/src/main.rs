//! The clerk binary.
//!
//! What this file wires in, in order: the configuration
//! ([`twalk_clerk::config`]) — and refuses to start on the same kind of
//! refusal Hermes and the Sensor make, a long operator-facing sentence
//! naming the variable and the way out — tracing, and a wait for the
//! process to be asked to stop. The three durable JetStream consumers, the
//! sweep timer and the `/health`/`/metrics` origin are wired in by Task 6
//! (`.scratch/clerk/plan.md`).

use anyhow::Result;
use tracing::info;
use twalk_clerk::config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    tracing_subscriber::fmt()
        .with_env_filter(&config.log_level)
        .init();
    info!(relay = %config.relay_url, stream = %config.stream, "clerk starting");

    shutdown_signal().await;

    info!("clerk stopped");
    Ok(())
}

/// Resolves when the process is asked to stop (SIGTERM, or SIGINT from an
/// interactive operator).
async fn shutdown_signal() {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("installing a SIGTERM handler never fails");
    tokio::select! {
        _ = sigterm.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}
