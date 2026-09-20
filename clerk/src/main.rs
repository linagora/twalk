//! The clerk binary.
//!
//! What this file wires in, in order: the configuration
//! ([`twalk_clerk::config`]) — refusing to start on the same kind of
//! refusal Hermes and the Sensor make, a long operator-facing sentence
//! naming the variable and the way out — tracing, the clerk's own Nostr key
//! and the relay it signs for, the `/health`/`/metrics` origin, then the
//! bus: the three durable consumers and the sweep
//! ([`twalk_clerk::consumers`]), and a wait for the process to be asked to
//! stop.
//!
//! Two orderings are deliberate. The origin is bound **before** the bus is
//! asked for, so `/health` answers while a late bus is still being waited
//! for; and the wait for the bus is a spawned task rather than the main
//! one, so a SIGTERM during it still stops the process cleanly. Binding
//! fails fast and loud — a configured-but-unusable origin is an operator
//! error to fix, not a condition to swallow (the Companion Gateway's and
//! the Sensor's origins behave the same way).
//!
//! The write half (#284) is wired the same way when it is configured: the
//! session file is opened here, and refused here on the same terms as the
//! Nostr key file — a file the whole host can read is a misconfiguration
//! to fix before the clerk runs, not one to run past — then the Companion
//! Gateway client is built from it and the decisions loop
//! ([`twalk_clerk::consumers::decisions`]) is spawned beside the bus, not
//! from it, because a ✅ on Buzz does not need the bus to be carried.

use std::future::IntoFuture;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use tracing::{info, warn};
use twalk_clerk::config::Config;
use twalk_clerk::consumers::{self, Clerk};
use twalk_clerk::gateway::{Gateway, SessionFile};
use twalk_clerk::metrics::Metrics;
use twalk_clerk::relay::{load_keys, Relay};
use twalk_clerk::text;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    tracing_subscriber::fmt()
        .with_env_filter(&config.log_level)
        .init();

    let metrics = Arc::new(Metrics::new());
    let keys = load_keys(&config.nostr_key_file)?;
    let relay = Relay::new(&config.relay_url, keys)?;
    let (lang, fallback_to_english) = text::lang(&config.user_language);
    info!(
        relay = %config.relay_url,
        pubkey = %relay.public_key_hex(),
        stream = %config.stream,
        language = %config.user_language,
        language_unset = config.user_language_unset,
        fallback_to_english,
        write_half = config.write_half().is_some(),
        "clerk starting"
    );
    // The write half turns a ✅ on Buzz into a Gateway approval (#284); its
    // absence is a supported deployment — the clerk still reads the bus and
    // posts — but a silent one would leave an operator wondering why
    // reacting to a post never sends anything.
    let gateway = match config.write_half() {
        Some(write) => {
            let session = SessionFile::open(&write.session_file).with_context(|| {
                format!(
                    "the clerk's session file {} cannot be used; run \
                     provision-clerk-device.sh, or unset CLERK_OWNER_PUBKEY, \
                     CLERK_GATEWAY_URL and CLERK_GATEWAY_SESSION_FILE to run without the \
                     write half",
                    write.session_file.display()
                )
            })?;
            info!(
                gateway = %write.gateway_url,
                owner = %write.owner_pubkey,
                session_file = %session.path().display(),
                decision_seconds = write.decision.as_secs(),
                "the write half is configured: a ✅ on Buzz by the owner is an approval"
            );
            Some(Gateway::new(&write.gateway_url, session))
        }
        None => {
            warn!(
                "the write half is not configured: a ✅ on Buzz decides nothing; set \
                 CLERK_OWNER_PUBKEY, CLERK_GATEWAY_URL and CLERK_GATEWAY_SESSION_FILE (#284)"
            );
            None
        }
    };
    // Unset is a supported state and is said rather than chosen silently:
    // on the reference deployment the personas' language comes from the
    // Companion's settings, which the clerk cannot read (it holds no
    // Gateway credential), so a French deployment gets an English clerk
    // unless the operator knows to set this.
    if config.user_language_unset {
        warn!(
            "CLERK_USER_LANGUAGE is not set; the clerk writes in English. Set it in \
             deploy/docker-compose/.env"
        );
    }
    if fallback_to_english {
        warn!(
            language = %config.user_language,
            "CLERK_USER_LANGUAGE names a language the clerk has no sentences in; its own lines \
             are written in English (the suggestion's body is posted as it was, whatever its \
             language)"
        );
    }

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("failed to bind the clerk's origin on {}", config.listen))?;
    let address = listener
        .local_addr()
        .context("failed to read the bound address")?;
    info!(%address, "clerk listening");
    tokio::spawn(axum::serve(listener, router(metrics.clone())).into_future());

    let clerk = Arc::new(Clerk {
        config,
        relay,
        metrics,
        lang,
        gateway,
    });
    if clerk.gateway.is_some() {
        tokio::spawn(consumers::decisions(clerk.clone()));
    }
    tokio::spawn(consumers::run(clerk));

    shutdown_signal().await;

    info!("clerk stopped");
    Ok(())
}

/// The origin: `/health` and `/metrics`, the same shape as the Companion
/// Gateway's own (`companion-gateway/src/http.rs`).
fn router(metrics: Arc<Metrics>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_exposition))
        .with_state(metrics)
}

async fn health() -> Response {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
}

async fn metrics_exposition(State(metrics): State<Arc<Metrics>>) -> Response {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics.render(now),
    )
        .into_response()
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
