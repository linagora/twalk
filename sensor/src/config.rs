//! Environment-driven configuration. The Sensor runs from environment
//! variables alone, so it deploys with the rest of the compose stack.

use std::time::Duration;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    /// Homeserver base URL, e.g. `https://matrix.example.com`.
    pub homeserver_url: String,
    /// Full Matrix user ID of the Sensor account, e.g. `@sensor:example.com`.
    pub user_id: String,
    /// Password of the Sensor account (access-token login comes with the
    /// crypto bootstrap work in ticket 04).
    pub password: String,
    /// NATS server URL, e.g. `nats://nats:4222`.
    pub nats_url: String,
    /// Matrix user IDs allowed to invite the Sensor into a room: the bridge
    /// provisioning users and the operator's own account. Invitations from
    /// anyone else are ignored.
    pub allowed_inviters: Vec<String>,
    /// Log level filter, e.g. `info` or `info,twalk_sensor=debug`.
    pub log_level: String,
    /// Base delay of the outbound send retry backoff; doubles with each
    /// redelivery (SENSOR_SEND_RETRY_BASE_MS, default 1000).
    pub send_retry_base: Duration,
    /// Delivery attempts an approved reply gets before it moves to the
    /// dead-letter subject (SENSOR_SEND_RETRY_MAX_ATTEMPTS, default 5).
    pub send_retry_max_attempts: i64,
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
            send_retry_base: Duration::from_millis(optional("SENSOR_SEND_RETRY_BASE_MS", 1000)?),
            send_retry_max_attempts: optional("SENSOR_SEND_RETRY_MAX_ATTEMPTS", 5)?,
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
