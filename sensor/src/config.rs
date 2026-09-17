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
    /// Password of the Sensor account (SENSOR_PASSWORD). The Sensor logs in
    /// with it; the password also lets the SDK complete the UIAA dance the
    /// first time it bootstraps cross-signing (ticket 04). Optional only
    /// when `access_token` is set: a homeserver with password login disabled
    /// (SSO-only) cannot be given one.
    pub password: Option<String>,
    /// Access token of a pre-provisioned device (SENSOR_ACCESS_TOKEN, with
    /// SENSOR_DEVICE_ID). The operator obtains it out of band — Synapse's
    /// admin registration API returns one, and so does any SSO login — and
    /// the Sensor starts as that device instead of logging in. Required on a
    /// homeserver where password login is disabled, since `login_username`
    /// simply has no path there. Without a password the SDK cannot answer a
    /// UIAA challenge, so cross-signing bootstrap needs either
    /// `recovery_key` or a device already cross-signed by the operator.
    pub access_token: Option<String>,
    /// Device ID the `access_token` belongs to (SENSOR_DEVICE_ID). Must be
    /// the device the token was issued for: the crypto store is bound to it,
    /// and matrix-sdk refuses to open a store belonging to another device.
    pub device_id: Option<String>,
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
    /// The **Sensor account's own** recovery key (SENSOR_RECOVERY_KEY,
    /// optional), kept by the operator from provisioning that account —
    /// never the user's recovery key, which no Twalk service may hold
    /// (ADR 0011, `docs/architecture/security-model.md`). When set, the
    /// Sensor opens its own account's secret storage with it at startup and
    /// imports that account's cross-signing secrets and key-backup
    /// decryption key, so a replacement device regains the backed-up
    /// room-key history. When unset, the Sensor relies on its
    /// local crypto store only: new traffic still decrypts (senders share
    /// Megolm keys with its device), history from before the device existed
    /// does not.
    pub recovery_key: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let config = Self {
            homeserver_url: required("SENSOR_HOMESERVER")?,
            user_id: required("SENSOR_USER_ID")?,
            password: optional_string("SENSOR_PASSWORD"),
            access_token: optional_string("SENSOR_ACCESS_TOKEN"),
            device_id: optional_string("SENSOR_DEVICE_ID"),
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
            recovery_key: optional_string("SENSOR_RECOVERY_KEY"),
        };
        config.validate_credentials()?;
        Ok(config)
    }

    /// One of the two credential shapes must be complete: a password to log
    /// in with, or an access token and the device ID it was issued for.
    /// Checked here rather than at login so a misconfigured deployment fails
    /// on startup with a name to fix, not several seconds later inside the
    /// SDK.
    fn validate_credentials(&self) -> Result<()> {
        match (&self.access_token, &self.device_id) {
            (Some(_), Some(_)) => Ok(()),
            (Some(_), None) => anyhow::bail!(
                "SENSOR_ACCESS_TOKEN is set without SENSOR_DEVICE_ID: the token's device ID is \
                 required, as the crypto store is bound to it"
            ),
            (None, _) if self.password.is_some() => Ok(()),
            (None, _) => anyhow::bail!(
                "no Sensor credentials: set SENSOR_PASSWORD, or SENSOR_ACCESS_TOKEN with \
                 SENSOR_DEVICE_ID on a homeserver whose password login is disabled"
            ),
        }
    }
}

/// An environment variable that is absent or empty is unset: an empty value
/// in a compose `.env` file is how an operator leaves an option out.
fn optional_string(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
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
