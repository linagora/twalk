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
        })
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
