//! The Twalk Companion Gateway binary. The decision logic lives in the
//! library modules; this file binds the origin, wires the router and handles
//! the process lifecycle.

use std::future::IntoFuture;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{info, warn};
use twalk_companion_gateway::config::Config;
use twalk_companion_gateway::http::{router, Gateway};
use twalk_companion_gateway::metrics::Metrics;

/// How long in-flight requests get to finish after SIGTERM before the
/// process exits anyway. Static files and a JSON document: a request that
/// has not finished in this long is not going to.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    tracing_subscriber::fmt()
        .with_env_filter(&config.log_level)
        .init();
    info!(
        listen = %config.listen,
        static_dir = %config.static_dir.display(),
        "companion gateway starting"
    );
    // A directory that is absent or holds no build is not fatal: the origin
    // answers a plain 404 for the Companion until the build appears, while
    // health and metrics stay up. Warn loudly, because for an operator who
    // did not intend it this is the whole of what is wrong.
    if !config.static_dir.join("index.html").is_file() {
        warn!(
            static_dir = %config.static_dir.display(),
            "no index.html in the static directory: the Companion origin will answer 404 until a build is present"
        );
    }

    let metrics = Arc::new(Metrics::started_at(now_unix_seconds()));
    // Binding fails fast and loud — a configured-but-unusable origin is an
    // operator error to fix, not a condition to swallow (the Sensor's
    // metrics endpoint behaves the same way).
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("failed to bind the gateway origin on {}", config.listen))?;
    let address = listener
        .local_addr()
        .context("failed to read the bound address")?;
    // The readiness marker, and the address a test (GATEWAY_LISTEN with port
    // 0) discovers the origin by.
    info!("companion gateway listening on {address}");

    let app = router(Gateway::new(
        config.static_dir.clone(),
        metrics,
        now_unix_seconds,
    ));
    let (shutdown, shutdown_requested) = tokio::sync::oneshot::channel::<()>();
    let mut server = tokio::spawn(
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_requested.await;
            })
            .into_future(),
    );

    tokio::select! {
        joined = &mut server => {
            joined.context("the http server task panicked")?.context("the http server failed")?;
        }
        _ = shutdown_signal() => {
            info!("shutdown signal received, draining in-flight requests");
            let _ = shutdown.send(());
            match tokio::time::timeout(DRAIN_TIMEOUT, &mut server).await {
                Ok(joined) => {
                    joined.context("the http server task panicked")?.context("the http server failed")?;
                    info!("in-flight requests drained, shutting down");
                }
                Err(_) => warn!("shutdown timed out with requests still in flight"),
            }
        }
    }
    info!("companion gateway stopped");
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

fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}
