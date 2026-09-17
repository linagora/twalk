//! Environment-driven configuration. Like the Sensor, the Companion Gateway
//! runs from environment variables alone, so it deploys with the rest of the
//! compose stack — same flat struct, same `from_env`, same error messages
//! naming the variable at fault.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    /// Address the Gateway's HTTP origin listens on (GATEWAY_LISTEN, default
    /// `0.0.0.0:8080`). One origin serves everything: the Companion's static
    /// files, the health endpoint and the metrics endpoint. Same origin means
    /// no CORS and, later, an `HttpOnly` device cookie (ADR 0011). Port 0
    /// asks the kernel for a free port — what the test suite uses.
    pub listen: SocketAddr,
    /// Directory the Companion's static files are served from
    /// (GATEWAY_STATIC_DIR, required). The image ships the Companion's build
    /// at `/srv/companion`; an operator serving their own build mounts it
    /// over that path or points this elsewhere. A directory that is absent or
    /// empty is not fatal: the Gateway serves a clear 404 until the build
    /// appears, so health and metrics stay up either way.
    pub static_dir: PathBuf,
    /// Name of the Companion's SPA fallback file inside the static directory
    /// (GATEWAY_FALLBACK_FILE, default `200.html`): what a path matching no
    /// file of the build is answered with, so a deep link reloaded cold loads
    /// the app. `200.html` is SvelteKit's recommended name — an `index.html`
    /// fallback collides with a prerendered homepage — and it is
    /// configurable because the name is the Companion build's to choose.
    pub fallback_file: String,
    /// Log level filter (GATEWAY_LOG_LEVEL), e.g. `info` or
    /// `info,twalk_companion_gateway=debug`. Per-request logs are at debug:
    /// a static origin at info level would be nothing but access logs.
    pub log_level: String,
    /// How the user signs in (ticket #52), or `None` when GATEWAY_OWNER is
    /// unset: the origin then serves the Companion, `/health` and
    /// `/metrics`, and its whole API answers `503 sign_in_not_configured`.
    ///
    /// Why not a hard startup failure, as a missing GATEWAY_STATIC_DIR is:
    /// the reference deployment brings the whole stack up in one
    /// `docker compose up`, and an operator upgrading a Gateway that has no
    /// `GATEWAY_OWNER` in their `.env` yet would lose the origin — and with
    /// it the page that could tell them why. Keeping the origin up while the
    /// API is closed fails in the safe direction: nothing can be
    /// authenticated, so nothing can be decided.
    pub sign_in: Option<SignIn>,
}

/// Everything sign-in needs. Present as a whole or not at all: GATEWAY_OWNER
/// is what turns it on, and the rest is then required, because a Gateway that
/// knows its owner but not where to verify a token would refuse every
/// sign-in for a reason no error message would make obvious.
#[derive(Debug, Clone)]
pub struct SignIn {
    /// The Matrix ID of the single human this deployment serves
    /// (GATEWAY_OWNER, e.g. `@you:example.com`). Configuration, exactly as
    /// `SENSOR_ALLOWED_INVITERS` is: one owner per deployment, any number of
    /// devices, and multi-user is out of scope (ADR 0011). Any other Matrix
    /// ID is refused, the Sensor's own account included.
    pub owner: String,
    /// The Matrix server name derived from [`Self::owner`]: the domain an
    /// accepted OpenID token must belong to. Derived rather than configured
    /// so the two can never drift — a deployment whose owner is
    /// `@you:example.com` serves `example.com`.
    pub homeserver_name: String,
    /// Base URL of the homeserver's federation API
    /// (GATEWAY_HOMESERVER_FEDERATION_URL, e.g. `http://synapse:8008`),
    /// where the OpenID userinfo endpoint is reached.
    ///
    /// Pinned rather than resolved from the server name: a deployment serves
    /// one homeserver, which the operator already configured, and
    /// implementing Matrix's server-name resolution (`.well-known`, SRV, the
    /// 8448 fallback) would be a federation stack inside a service that does
    /// not federate. It decides which server is asked; the domain check on
    /// the answer stays either way (see [`crate::matrix_openid`]).
    pub federation_base_url: String,
    /// Directory the Gateway keeps its SQLite stores in (GATEWAY_STATE_DIR).
    /// The session store is `sessions.db` inside it.
    pub state_dir: PathBuf,
    /// Lifetime of a device token in seconds (GATEWAY_DEVICE_TOKEN_TTL,
    /// default 15 minutes). Short: it is the credential that travels on
    /// every request, and the Companion refreshes it while in use.
    pub device_token_ttl_seconds: u64,
    /// Lifetime of a refresh token in seconds (GATEWAY_REFRESH_TOKEN_TTL,
    /// default 30 days): how long a device that was left alone can come back
    /// without signing in again.
    pub refresh_token_ttl_seconds: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            listen: optional("GATEWAY_LISTEN", "0.0.0.0:8080")?,
            static_dir: PathBuf::from(required("GATEWAY_STATIC_DIR")?),
            fallback_file: std::env::var("GATEWAY_FALLBACK_FILE")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "200.html".to_owned()),
            log_level: std::env::var("GATEWAY_LOG_LEVEL")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "info".to_owned()),
            sign_in: SignIn::from_env()?,
        })
    }
}

impl SignIn {
    /// `Ok(None)` when GATEWAY_OWNER is unset; an error when it is set and
    /// something it needs is missing or malformed.
    fn from_env() -> Result<Option<Self>> {
        let Some(owner) = std::env::var("GATEWAY_OWNER")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        let homeserver_name = crate::matrix_openid::domain_of(&owner)
            .with_context(|| {
                format!(
                    "environment variable GATEWAY_OWNER is not a Matrix user ID \
                     (expected @localpart:server_name, got {owner:?})"
                )
            })?
            .to_owned();
        Ok(Some(Self {
            owner,
            homeserver_name,
            federation_base_url: required("GATEWAY_HOMESERVER_FEDERATION_URL")?,
            state_dir: PathBuf::from(required("GATEWAY_STATE_DIR")?),
            device_token_ttl_seconds: optional(
                "GATEWAY_DEVICE_TOKEN_TTL",
                &crate::session::DEFAULT_DEVICE_TOKEN_TTL_SECONDS.to_string(),
            )?,
            refresh_token_ttl_seconds: optional(
                "GATEWAY_REFRESH_TOKEN_TTL",
                &crate::session::DEFAULT_REFRESH_TOKEN_TTL_SECONDS.to_string(),
            )?,
        }))
    }
}

fn required(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("missing required environment variable {name}"))
}

fn optional<T>(name: &str, default: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let raw = std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned());
    raw.parse()
        .with_context(|| format!("environment variable {name} has an invalid value"))
}
