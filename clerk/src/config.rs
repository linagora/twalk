//! The clerk's configuration, read from the environment alone, so it joins
//! the compose deployment the way the Sensor, Hermes and the Companion
//! Gateway do.
//!
//! `Config::from_env` is a thin wrapper over [`Config::from_vars`], which
//! takes the environment as a closure (`&dyn Fn(&str) -> Option<String>`)
//! rather than reading `std::env::var` itself, so a test can build a
//! `Config` from a fixed map and never touch the process environment. An
//! absent or empty variable is unset either way: an empty value in a
//! compose `.env` file is how an operator leaves an option out.
//!
//! The clerk holds no Gateway credential and no key of Hermes's own — its
//! only secret is the Nostr key it signs its own posts with, named by
//! [`Config::nostr_key_file`] and read by the binary, never here.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// The bus namespace a Twalk deployment publishes under, and the stream
/// that captures it (`sensor/src/normalize.rs`).
pub const DEFAULT_STREAM: &str = "twalk";
pub const DEFAULT_SUBJECT_PREFIX: &str = "twalk";
pub const DEFAULT_NATS_URL: &str = "nats://localhost:4222";
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8084";

/// The interface languages the Companion ships, which are the five the
/// Companion Gateway stores a preference among
/// (`companion-gateway/src/settings.rs`) and the five the Hermes runtime
/// validates against (`hermes/src/config.rs::USER_LANGUAGES`).
///
/// The clerk quotes no contact — a post is `suggestion.body` plus its own
/// sentences — but those sentences are still words in a language, and the
/// clerk validates against the same five for the same reason the runtime
/// does: a value only it would refuse is better refused once, at startup,
/// than surfacing as a post nobody can read.
pub const USER_LANGUAGES: [&str; 5] = ["en", "fr", "it", "es", "de"];

/// The whole clerk configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub nats_url: String,
    /// The stream the bus's events live on — the same stream every other
    /// component reads and writes (`sensor/src/normalize.rs`).
    pub stream: String,
    /// This deployment's bus namespace: `fr.linagora.twalk.X` becomes
    /// `<subject_prefix>.X` ([`Config::bus_subject`]).
    pub subject_prefix: String,
    /// The URL the owner's Buzz relay **announces** (its own `RELAY_URL`),
    /// with no trailing slash and no path: NIP-98 binds every signed
    /// request to exactly this URL, and a relay that is multi-tenant by
    /// `Host` would accept a request signed for a different tenant if this
    /// were merely "a URL that reaches the relay".
    pub relay_url: String,
    /// Where the clerk's own Nostr key lives: one line, 64 hex characters
    /// or an `nsec1…` string. Read by the binary, never opened here — the
    /// path is all `Config` knows about it.
    pub nostr_key_file: PathBuf,
    /// The Buzz channel the clerk posts one forum entry per suggestion to.
    pub channel_approvals: String,
    /// The Buzz channel the clerk posts what calls for a look to.
    pub channel_activity: String,
    /// The Buzz channel the clerk posts one line per posted reply to.
    pub channel_journal: String,
    /// The language the clerk writes its own sentences in — never a
    /// contact's words, which it never quotes. One of [`USER_LANGUAGES`].
    pub user_language: String,
    /// Where `/health` and `/metrics` are served.
    pub listen: SocketAddr,
    /// How often the clerk looks for a post whose suggestion has expired
    /// and deletes it.
    pub sweep: Duration,
    pub log_level: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_vars(&|name| std::env::var(name).ok())
    }

    /// Builds a `Config` from an environment given as a closure rather than
    /// the process's own, so a test never has to touch `std::env` — and
    /// never has to serialise with every other test that does, since the
    /// process environment is one global the whole binary shares.
    pub fn from_vars(vars: &dyn Fn(&str) -> Option<String>) -> Result<Self> {
        let config = Self {
            nats_url: optional(vars, "CLERK_NATS_URL")
                .unwrap_or_else(|| DEFAULT_NATS_URL.to_owned()),
            stream: optional(vars, "CLERK_STREAM").unwrap_or_else(|| DEFAULT_STREAM.to_owned()),
            subject_prefix: optional(vars, "CLERK_SUBJECT_PREFIX")
                .unwrap_or_else(|| DEFAULT_SUBJECT_PREFIX.to_owned()),
            relay_url: required(vars, "CLERK_RELAY_URL")?,
            nostr_key_file: PathBuf::from(required(vars, "CLERK_NOSTR_KEY_FILE")?),
            channel_approvals: required(vars, "CLERK_CHANNEL_APPROVALS")?,
            channel_activity: required(vars, "CLERK_CHANNEL_ACTIVITY")?,
            channel_journal: required(vars, "CLERK_CHANNEL_JOURNAL")?,
            user_language: optional(vars, "CLERK_USER_LANGUAGE").unwrap_or_else(|| "en".to_owned()),
            listen: optional(vars, "CLERK_LISTEN")
                .unwrap_or_else(|| DEFAULT_LISTEN.to_owned())
                .parse()
                .context("CLERK_LISTEN must be a host:port address")?,
            sweep: Duration::from_secs(number(vars, "CLERK_SWEEP_SECONDS", 60)?),
            log_level: optional(vars, "CLERK_LOG_LEVEL").unwrap_or_else(|| "info".to_owned()),
        };
        config.validate()?;
        Ok(config)
    }

    /// The bus subject of a contract event type, in this deployment's
    /// namespace: `fr.linagora.twalk.<domain>.<action>.<version>` becomes
    /// `<prefix>.<domain>.<action>.<version>`, exactly as the Sensor and
    /// Hermes map it.
    pub fn bus_subject(&self, event_type: &str) -> String {
        let rest = event_type
            .strip_prefix("fr.linagora.twalk.")
            .expect("contract event types always carry the fr.linagora.twalk prefix");
        format!("{}.{rest}", self.subject_prefix)
    }

    fn validate(&self) -> Result<()> {
        if !is_relay_url(&self.relay_url) {
            bail!(
                "CLERK_RELAY_URL is the URL the relay announces (its RELAY_URL) — http:// or \
                 https://, host and port only — because NIP-98 binds every signed request to it \
                 and the relay is multi-tenant by host; got {:?}",
                self.relay_url
            );
        }
        validate_channel("CLERK_CHANNEL_APPROVALS", &self.channel_approvals)?;
        validate_channel("CLERK_CHANNEL_ACTIVITY", &self.channel_activity)?;
        validate_channel("CLERK_CHANNEL_JOURNAL", &self.channel_journal)?;
        if !USER_LANGUAGES.contains(&self.user_language.as_str()) {
            bail!(
                "CLERK_USER_LANGUAGE is the language the clerk writes its own sentences in — it \
                 quotes no contact, so this is the only language its posts ever appear in — and \
                 must be one of {}, spelled as the Companion writes it; got {:?}",
                USER_LANGUAGES.join(", "),
                self.user_language
            );
        }
        if self.sweep < Duration::from_secs(1) {
            bail!(
                "CLERK_SWEEP_SECONDS must be at least 1: it is how often the clerk looks for an \
                 expired suggestion to delete, and a sweep tighter than a second would poll the \
                 relay for no reason"
            );
        }
        Ok(())
    }
}

/// Whether `url` is exactly what a relay announces as its own `RELAY_URL`:
/// `http://` or `https://`, a host and optionally a port, and nothing after
/// it — no trailing slash and no path.
fn is_relay_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return false;
    };
    !rest.is_empty() && !rest.contains('/')
}

/// A Buzz channel is identified by a UUID, the same shape
/// `deploy/docker-compose/provision-buzz-channels.sh` prints.
fn validate_channel(name: &str, value: &str) -> Result<()> {
    if uuid::Uuid::parse_str(value).is_err() {
        bail!(
            "{name} must be the channel's UUID, the way \
             deploy/docker-compose/provision-buzz-channels.sh prints it; got {:?}",
            value
        );
    }
    Ok(())
}

/// An environment variable that is absent or empty is unset: an empty
/// value in a compose `.env` file is how an operator leaves an option out.
fn optional(vars: &dyn Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    vars(name).filter(|value| !value.is_empty())
}

fn required(vars: &dyn Fn(&str) -> Option<String>, name: &str) -> Result<String> {
    optional(vars, name).with_context(|| format!("missing required environment variable {name}"))
}

fn number<T>(vars: &dyn Fn(&str) -> Option<String>, name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match optional(vars, name) {
        Some(raw) => raw
            .parse()
            .with_context(|| format!("environment variable {name} has an invalid value")),
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
                .filter(|v| !v.is_empty())
        }
    }

    const FULL: &[(&str, &str)] = &[
        ("CLERK_RELAY_URL", "http://127.0.0.1:17800"),
        ("CLERK_NOSTR_KEY_FILE", "/run/secrets/clerk.key"),
        (
            "CLERK_CHANNEL_APPROVALS",
            "9b1ba94a-38c3-49fe-9eb0-ffaafa62571a",
        ),
        (
            "CLERK_CHANNEL_ACTIVITY",
            "2487096b-23e1-45cb-a611-402a5fae439a",
        ),
        (
            "CLERK_CHANNEL_JOURNAL",
            "29a57768-7513-43bc-9cc2-6915453467f4",
        ),
    ];

    #[test]
    fn a_full_configuration_is_accepted_with_the_defaults() {
        let c = Config::from_vars(&vars(FULL)).unwrap();
        assert_eq!(c.user_language, "en");
        assert_eq!(c.sweep, Duration::from_secs(60));
        assert_eq!(c.listen.to_string(), "127.0.0.1:8084");
    }

    #[test]
    fn every_required_variable_is_named_when_missing() {
        for (name, _) in FULL {
            let rest: Vec<_> = FULL.iter().filter(|(k, _)| k != name).cloned().collect();
            let err = Config::from_vars(&vars(&rest)).unwrap_err().to_string();
            assert!(err.contains(name), "{err}");
        }
    }

    #[test]
    fn the_relay_url_is_the_announced_one_without_a_path() {
        for bad in [
            "127.0.0.1:17800",
            "http://127.0.0.1:17800/",
            "http://127.0.0.1:17800/events",
            "ws://x",
        ] {
            let mut v = FULL.to_vec();
            v[0] = ("CLERK_RELAY_URL", bad);
            let err = Config::from_vars(&vars(&v)).unwrap_err().to_string();
            assert!(
                err.contains("CLERK_RELAY_URL") && err.contains("announces"),
                "{bad}: {err}"
            );
        }
    }

    #[test]
    fn a_channel_must_be_a_uuid() {
        let mut v = FULL.to_vec();
        v[2] = ("CLERK_CHANNEL_APPROVALS", "approbations");
        let err = Config::from_vars(&vars(&v)).unwrap_err().to_string();
        assert!(err.contains("CLERK_CHANNEL_APPROVALS") && err.contains("UUID"));
    }

    #[test]
    fn the_language_is_one_of_the_companions_five() {
        for bad in ["fr-FR", "FR", "pt"] {
            let mut v = FULL.to_vec();
            v.push(("CLERK_USER_LANGUAGE", bad));
            let err = Config::from_vars(&vars(&v)).unwrap_err().to_string();
            assert!(err.contains("en, fr, it, es, de") && err.contains(bad));
        }
    }

    #[test]
    fn the_sweep_is_at_least_a_second() {
        let mut v = FULL.to_vec();
        v.push(("CLERK_SWEEP_SECONDS", "0"));
        assert!(Config::from_vars(&vars(&v))
            .unwrap_err()
            .to_string()
            .contains("CLERK_SWEEP_SECONDS"));
    }

    #[test]
    fn bus_subjects_follow_the_prefix() {
        let c = Config::from_vars(&vars(FULL)).unwrap();
        assert_eq!(
            c.bus_subject("fr.linagora.twalk.persona.suggest.produced.v1"),
            "twalk.persona.suggest.produced.v1"
        );
    }
}
