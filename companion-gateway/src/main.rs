//! The Twalk Companion Gateway binary. The decision logic lives in the
//! library modules; this file binds the origin, wires the router and handles
//! the process lifecycle.

use std::future::IntoFuture;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{info, warn};
use twalk_companion_gateway::bootstrap::Bootstrap;
use twalk_companion_gateway::config::{Config, Consent};
use twalk_companion_gateway::http::{router, Gateway};
use twalk_companion_gateway::matrix_openid::Verifier;
use twalk_companion_gateway::metrics::Metrics;
use twalk_companion_gateway::outbox::{publish_until_shutdown, Outbox};
use twalk_companion_gateway::session::Sessions;
use twalk_companion_gateway::static_files::Resolver;
use twalk_companion_gateway::store::Store;

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
        fallback_file = %config.fallback_file,
        "companion gateway starting"
    );
    let companion = Resolver::new(config.static_dir.clone(), config.fallback_file.clone());
    // A directory that is absent or holds no build is not fatal: the origin
    // answers a plain 404 for the Companion until the build appears, while
    // health and metrics stay up. Warn loudly, because for an operator who
    // did not intend it this is the whole of what is wrong.
    if !companion.fallback_path().is_file() {
        warn!(
            static_dir = %config.static_dir.display(),
            fallback_file = %config.fallback_file,
            "no fallback file in the static directory: the Companion origin will answer 404 for any path its build has no file for"
        );
    }

    // The user's session (ticket #52). Absent configuration it stays `None`:
    // the origin serves the Companion, health and metrics, and the whole API
    // answers 503 — see `config::SignIn` for why that is preferred to
    // refusing to start.
    let sessions = match &config.sign_in {
        Some(sign_in) => {
            info!(
                owner = %sign_in.owner,
                homeserver = %sign_in.homeserver_name,
                federation_base_url = %sign_in.federation_base_url,
                state_dir = %sign_in.state_dir.display(),
                device_token_ttl_seconds = sign_in.device_token_ttl_seconds,
                "sign-in configured: this deployment serves one owner"
            );
            let verifier = Verifier::new(&sign_in.federation_base_url, &sign_in.homeserver_name)
                .context("failed to build the OpenID verification client")?;
            Some(Arc::new(
                Sessions::open(
                    &sign_in.state_dir,
                    verifier,
                    sign_in.owner.clone(),
                    sign_in.device_token_ttl_seconds,
                    sign_in.refresh_token_ttl_seconds,
                    now_unix_seconds,
                )
                .context("failed to open the session store")?,
            ))
        }
        None => {
            warn!(
                "GATEWAY_OWNER is not set: nobody can sign in, so every /api endpoint answers 503. \
                 Set GATEWAY_OWNER, GATEWAY_HOMESERVER_FEDERATION_URL and GATEWAY_STATE_DIR to enable sign-in"
            );
            None
        }
    };

    // Bootstrap (ticket #53): the registration relay and the Sensor's
    // invitation. Each half is independently optional, and a half that is off
    // answers 503 naming the variable that would open it — so an operator
    // never has to guess which of the two is missing.
    let bootstrap = match &config.bootstrap.homeserver_url {
        Some(homeserver_url) => {
            let bootstrap = Bootstrap::new(
                homeserver_url,
                config.bootstrap.registration_shared_secret.clone(),
                config.bootstrap.sensor_user_id.clone(),
            )
            .context("failed to build the bootstrap client")?;
            if bootstrap.registers_accounts() {
                info!(
                    homeserver_url = %bootstrap.homeserver_url(),
                    owner = config.sign_in.as_ref().map(|sign_in| sign_in.owner.as_str()).unwrap_or("(none)"),
                    "the registration relay is enabled: it can create this deployment's one account, once, and refuses every other username"
                );
            } else {
                info!(
                    "GATEWAY_REGISTRATION_SHARED_SECRET is not set: the registration relay is off, and POST /api/bootstrap/account answers 503"
                );
            }
            match bootstrap.sensor_user_id() {
                Some(sensor) => info!(
                    sensor = %sensor,
                    "the Sensor can be invited into the rooms the user selects"
                ),
                None => info!(
                    "GATEWAY_SENSOR_USER_ID is not set: the Gateway cannot invite the Sensor, and POST /api/bootstrap/rooms answers 503"
                ),
            }
            Some(Arc::new(bootstrap))
        }
        // No sign-in configuration and no homeserver URL: the whole API is
        // closed already, so there is nothing to warn about twice.
        None => None,
    };

    let metrics = Arc::new(Metrics::started_at(now_unix_seconds()));

    // Consent (ticket #49). Configured, the Gateway opens its decision
    // journal, publishes whatever the last run left unpublished, and serves
    // the write API; unconfigured, the consent endpoints answer a 503 naming
    // what is missing and the rest of the origin is untouched.
    let consent = match &config.consent {
        Some(consent) => {
            let outbox = open_consent(consent, &metrics)?;
            tokio::spawn(publish_until_shutdown(
                outbox.clone(),
                consent.nats_url.clone(),
            ));
            Some(outbox)
        }
        None => {
            if config.sign_in.is_some() {
                warn!(
                    "GATEWAY_NATS_URL is not set: the consent endpoints answer 503, because a \
                     decision the bus never hears is a decision no persona can honour"
                );
            }
            None
        }
    };

    // The bridge login facade (ticket #55). Built from configuration alone:
    // no bridge is contacted at startup, because a bridge that is down must
    // not keep the Companion's origin from coming up. A misconfigured bridge
    // *is* fatal, though — a base URL that is not one, or two instances
    // sharing an id — because the operator can fix it now and a user cannot
    // diagnose it from the QR screen.
    let bridges = Arc::new(
        twalk_companion_gateway::bridge::Bridges::new(config.bridges.clone())
            .context("failed to build the bridge login facade")?,
    );
    if config.bridges.is_empty() {
        info!(
            "GATEWAY_BRIDGES is not set: no network can be connected from the Companion, and \
             GET /api/bridges answers an empty list"
        );
    } else {
        for bridge in &config.bridges {
            info!(
                bridge = %bridge.bridge_id,
                network = %bridge.network,
                url = %bridge.base_url,
                "a bridge is configured: the Gateway can drive its logins and holds their \
                 blocking steps itself"
            );
        }
    }

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

    let app = router(
        Gateway::new(companion, metrics, now_unix_seconds)
            .with_sessions(sessions)
            .with_bootstrap(bootstrap)
            .with_consent(consent)
            .with_bridges(bridges),
    );
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

/// Opens the consent store and hands back the outbox the router and the
/// publication task share.
///
/// Opening applies the embedded schema migrations, so an operator upgrades
/// the image and nothing else. The outbox gauge is published here, before
/// the origin binds: a restart that inherits unpublished decisions reports
/// them from its first scrape, not from its first request.
fn open_consent(consent: &Consent, metrics: &Arc<Metrics>) -> Result<Arc<Outbox>> {
    let store = Store::open(&consent.state_dir).context("failed to open the consent store")?;
    info!(
        store = %store.path().display(),
        owner = %consent.owner,
        source = %format!("gateway://{}/consent", consent.matrix_domain),
        nats_url = %consent.nats_url,
        "consent store ready: this Gateway is the single writer of consent state"
    );
    let store = Arc::new(store);
    let pending = store
        .unpublished_count()
        .context("failed to count the consent outbox")?;
    metrics.set_consent_outbox_pending(pending);
    if pending > 0 {
        // The crash-recovery path, stated plainly because it is the one an
        // operator will want to see after an unclean stop.
        info!(
            pending,
            "committed consent decisions were not published before the last stop; publishing them now"
        );
    }
    Ok(Arc::new(Outbox::new(
        store,
        metrics.clone(),
        consent.matrix_domain.clone(),
        consent.owner.clone(),
        std::time::SystemTime::now,
    )))
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
