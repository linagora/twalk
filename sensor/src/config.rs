//! Environment-driven configuration. The Sensor runs from environment
//! variables alone, so it deploys with the rest of the compose stack.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    /// Homeserver base URL, e.g. `https://matrix.example.com`.
    pub homeserver_url: String,
    /// Full Matrix user ID of the Sensor account, e.g. `@sensor:example.com`.
    pub user_id: String,
    /// Password of the Sensor account. The Sensor logs in with it; the
    /// password also lets the SDK complete the UIAA dance the first time it
    /// bootstraps cross-signing (ticket 04).
    pub password: String,
    /// NATS server URL, e.g. `nats://nats:4222`.
    pub nats_url: String,
    /// Matrix user IDs allowed to invite the Sensor into a room: the bridge
    /// provisioning users and the operator's own account. Invitations from
    /// anyone else are ignored.
    pub allowed_inviters: Vec<String>,
    /// Log level filter, e.g. `info` or `info,twalk_sensor=debug`.
    pub log_level: String,
    /// Listen address of the Prometheus metrics endpoint
    /// (SENSOR_METRICS_LISTEN, e.g. `0.0.0.0:9090`). Unset (the default): no
    /// metrics server at all — an operator who does not scrape pays nothing.
    pub metrics_listen: Option<std::net::SocketAddr>,
    /// Directory the Sensor persists its session, sync token and crypto
    /// store in (SENSOR_STATE_DIR). Set on a volume so a restart resumes the
    /// sync instead of re-syncing (and re-emitting) recent traffic. Unset:
    /// everything stays in memory and every start is a fresh login followed
    /// by an initial sync.
    pub state_dir: Option<PathBuf>,
    /// Base delay of the outbound send retry backoff; doubles with each
    /// redelivery (SENSOR_SEND_RETRY_BASE_MS, default 1000).
    pub send_retry_base: Duration,
    /// Delivery attempts an approved reply gets before it moves to the
    /// dead-letter subject (SENSOR_SEND_RETRY_MAX_ATTEMPTS, default 5).
    pub send_retry_max_attempts: i64,
    /// The operator's recovery key (SENSOR_RECOVERY_KEY, optional), saved
    /// during onboarding. When set, the Sensor opens the account's secret
    /// storage with it at startup and imports the cross-signing secrets and
    /// the key-backup decryption key, so a replacement device regains the
    /// backed-up room-key history. When unset, the Sensor relies on its
    /// local crypto store only: new traffic still decrypts (senders share
    /// Megolm keys with its device), history from before the device existed
    /// does not.
    pub recovery_key: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            homeserver_url: required("SENSOR_HOMESERVER")?,
            user_id: required("SENSOR_USER_ID")?,
            password: required("SENSOR_PASSWORD")?,
            nats_url: required("SENSOR_NATS_URL")?,
            allowed_inviters: required("SENSOR_ALLOWED_INVITERS")?
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            log_level: std::env::var("SENSOR_LOG_LEVEL").unwrap_or_else(|_| "info".to_owned()),
            metrics_listen: std::env::var("SENSOR_METRICS_LISTEN")
                .ok()
                .filter(|value| !value.is_empty())
                .map(|value| {
                    value.parse().with_context(|| {
                        "environment variable SENSOR_METRICS_LISTEN has an invalid value"
                    })
                })
                .transpose()?,
            state_dir: std::env::var("SENSOR_STATE_DIR")
                .ok()
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            send_retry_base: Duration::from_millis(optional("SENSOR_SEND_RETRY_BASE_MS", 1000)?),
            send_retry_max_attempts: optional("SENSOR_SEND_RETRY_MAX_ATTEMPTS", 5)?,
            recovery_key: std::env::var("SENSOR_RECOVERY_KEY")
                .ok()
                .filter(|value| !value.is_empty()),
        })
    }
}

fn required(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("missing required environment variable {name}"))
}

fn optional<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match std::env::var(name) {
        Ok(raw) => raw
            .parse()
            .with_context(|| format!("environment variable {name} has an invalid value")),
        Err(_) => Ok(default),
    }
}
