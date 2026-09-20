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
    /// contact's words, which it never quotes. One of [`USER_LANGUAGES`];
    /// `en` when `CLERK_USER_LANGUAGE` is unset, which
    /// [`Config::user_language_unset`] records so the binary can say so.
    pub user_language: String,
    /// Whether `user_language` is the default rather than a choice: on the
    /// reference deployment the personas' language comes from the
    /// Companion's settings (#184), which the clerk cannot read, so an
    /// unset variable is a French deployment with an English clerk — a
    /// supported state, and one to be told about at startup rather than
    /// discovered on the relay.
    pub user_language_unset: bool,
    /// Where `/health` and `/metrics` are served.
    pub listen: SocketAddr,
    /// How often the clerk looks for a post whose suggestion has expired
    /// and deletes it.
    pub sweep: Duration,
    pub log_level: String,
    /// The write half of the clerk (#284): present only when its three
    /// variables are all set. `None` is a supported deployment — a clerk
    /// with no write half still reads the bus and posts, it just cannot
    /// turn a ✅ into an approval — and the binary says so at startup
    /// rather than a ✅ silently deciding nothing.
    pub write_half: Option<WriteHalf>,
}

/// The write half of the clerk (#284), present only when all three are
/// set.
#[derive(Debug, Clone)]
pub struct WriteHalf {
    /// 64 lowercase hex characters: the owner's Nostr public key.
    pub owner_pubkey: String,
    /// The Companion Gateway's origin, no trailing slash, `http://` or
    /// `https://`.
    pub gateway_url: String,
    /// The file holding `TWALK_GATEWAY_REFRESH_TOKEN=<token>`; the clerk
    /// rewrites it.
    pub session_file: PathBuf,
    /// How often the clerk looks at the gestures on its posts (default
    /// 5 s).
    pub decision: Duration,
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
        let user_language = optional(vars, "CLERK_USER_LANGUAGE");
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
            user_language_unset: user_language.is_none(),
            user_language: user_language.unwrap_or_else(|| "en".to_owned()),
            listen: optional(vars, "CLERK_LISTEN")
                .unwrap_or_else(|| DEFAULT_LISTEN.to_owned())
                .parse()
                .context("CLERK_LISTEN must be a host:port address")?,
            sweep: Duration::from_secs(number(vars, "CLERK_SWEEP_SECONDS", 60)?),
            log_level: optional(vars, "CLERK_LOG_LEVEL").unwrap_or_else(|| "info".to_owned()),
            write_half: build_write_half(vars)?,
        };
        config.validate()?;
        Ok(config)
    }

    /// The write half of the clerk (#284), when its three variables are all
    /// set.
    pub fn write_half(&self) -> Option<&WriteHalf> {
        self.write_half.as_ref()
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
        // Refused here and not discovered in the loop: a zero interval
        // panics the decisions task on its first line, and a panic in a
        // spawned task is a write half silently dead behind a green /health.
        if self
            .write_half
            .as_ref()
            .is_some_and(|write| write.decision < Duration::from_secs(1))
        {
            bail!(
                "CLERK_DECISION_SECONDS must be at least 1: it is how often the clerk reads the \
                 gestures on its posts, and a read tighter than a second would poll the relay \
                 for no reason"
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

/// Builds the write half from its three variables, or refuses to build a
/// partial one: `None` unless all three are set, because a clerk holding
/// two of the three has a broken write half rather than an absent one, and
/// silently treating that as "no write half" would leave an operator
/// looking for why a ✅ still decides nothing.
fn build_write_half(vars: &dyn Fn(&str) -> Option<String>) -> Result<Option<WriteHalf>> {
    let owner_pubkey = optional(vars, "CLERK_OWNER_PUBKEY");
    let gateway_url = optional(vars, "CLERK_GATEWAY_URL");
    let session_file = optional(vars, "CLERK_GATEWAY_SESSION_FILE");
    match (owner_pubkey, gateway_url, session_file) {
        (None, None, None) => Ok(None),
        (Some(owner_pubkey), Some(gateway_url), Some(session_file)) => {
            let owner_pubkey = normalize_owner_pubkey(&owner_pubkey)?;
            if !is_origin_url(&gateway_url) {
                bail!(
                    "CLERK_GATEWAY_URL must be the Companion Gateway's origin — http:// or \
                     https://, host and port only, no path and no trailing slash — because the \
                     clerk builds every request path onto it; got {:?}",
                    gateway_url
                );
            }
            Ok(Some(WriteHalf {
                owner_pubkey,
                gateway_url,
                session_file: PathBuf::from(session_file),
                decision: Duration::from_secs(number(vars, "CLERK_DECISION_SECONDS", 5)?),
            }))
        }
        (owner_pubkey, gateway_url, session_file) => {
            let missing: Vec<&str> = [
                (owner_pubkey.is_none(), "CLERK_OWNER_PUBKEY"),
                (gateway_url.is_none(), "CLERK_GATEWAY_URL"),
                (session_file.is_none(), "CLERK_GATEWAY_SESSION_FILE"),
            ]
            .into_iter()
            .filter_map(|(is_missing, name)| is_missing.then_some(name))
            .collect();
            bail!(
                "the clerk's write half (#284) needs CLERK_OWNER_PUBKEY, CLERK_GATEWAY_URL and \
                 CLERK_GATEWAY_SESSION_FILE together; missing: {}",
                missing.join(", ")
            );
        }
    }
}

/// The owner's Nostr public key, as `CLERK_OWNER_PUBKEY` gives it: 64
/// hexadecimal characters, lowercased (Nostr's own convention for a hex
/// key), never an `npub1…` bech32 string — the clerk needs the same raw
/// hex the rest of the Nostr ecosystem tags a pubkey with, not the address
/// a human reads it as.
fn normalize_owner_pubkey(value: &str) -> Result<String> {
    let lowered = value.to_ascii_lowercase();
    if lowered.len() == 64 && lowered.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(lowered)
    } else {
        bail!(
            "CLERK_OWNER_PUBKEY must be the owner's Nostr public key as 64 hexadecimal \
             characters, not an npub1… string or anything else; got {:?}",
            value
        );
    }
}

/// Whether `url` is the Companion Gateway's origin alone: the same shape
/// [`is_relay_url`] checks for the relay — `http://` or `https://`, a host
/// and optionally a port, and nothing after it — because the clerk builds
/// every request path onto this value and a trailing slash or a path
/// already there would double up.
fn is_origin_url(url: &str) -> bool {
    is_relay_url(url)
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
        assert!(c.user_language_unset, "unset is English, and known to be");
        assert_eq!(c.sweep, Duration::from_secs(60));
        assert_eq!(c.listen.to_string(), "127.0.0.1:8084");
    }

    #[test]
    fn a_language_that_was_set_is_not_the_default_even_when_it_is_english() {
        for (set, unset) in [("fr", false), ("en", false), ("", true)] {
            let mut v = FULL.to_vec();
            v.push(("CLERK_USER_LANGUAGE", set));
            let c = Config::from_vars(&vars(&v)).unwrap();
            assert_eq!(c.user_language_unset, unset, "{set:?}");
            assert_eq!(c.user_language, if unset { "en" } else { set }, "{set:?}");
        }
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

    /// A valid write half, added onto [`FULL`] the same way `FULL` itself
    /// is extended in the tests above.
    const WRITE_HALF: &[(&str, &str)] = &[
        (
            "CLERK_OWNER_PUBKEY",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
        ("CLERK_GATEWAY_URL", "http://127.0.0.1:8080"),
        (
            "CLERK_GATEWAY_SESSION_FILE",
            "/run/secrets/clerk-gateway-session",
        ),
    ];

    #[test]
    fn write_half_is_absent_when_none_of_its_variables_is_set() {
        let c = Config::from_vars(&vars(FULL)).unwrap();
        assert!(c.write_half().is_none());
    }

    #[test]
    fn write_half_needs_all_three() {
        let mut v = FULL.to_vec();
        v.push(WRITE_HALF[0]); // CLERK_OWNER_PUBKEY alone
        let err = Config::from_vars(&vars(&v)).unwrap_err().to_string();
        assert!(err.contains("CLERK_GATEWAY_URL"), "{err}");
        assert!(err.contains("CLERK_GATEWAY_SESSION_FILE"), "{err}");
    }

    #[test]
    fn owner_pubkey_is_64_lowercase_hex() {
        for bad in [
            "npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abc", // 63 chars
        ] {
            let mut v = FULL.to_vec();
            v.push(("CLERK_OWNER_PUBKEY", bad));
            v.push(WRITE_HALF[1]);
            v.push(WRITE_HALF[2]);
            let err = Config::from_vars(&vars(&v)).unwrap_err().to_string();
            assert!(
                err.contains("CLERK_OWNER_PUBKEY") && err.contains("64 hexadecimal characters"),
                "{bad}: {err}"
            );
        }

        let mut v = FULL.to_vec();
        v.push((
            "CLERK_OWNER_PUBKEY",
            "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF",
        ));
        v.push(WRITE_HALF[1]);
        v.push(WRITE_HALF[2]);
        let c = Config::from_vars(&vars(&v)).unwrap();
        assert_eq!(
            c.write_half().unwrap().owner_pubkey,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn gateway_url_has_no_path_and_no_trailing_slash() {
        for bad in ["https://gw.example/", "https://gw.example/api"] {
            let mut v = FULL.to_vec();
            v.push(WRITE_HALF[0]);
            v.push(("CLERK_GATEWAY_URL", bad));
            v.push(WRITE_HALF[2]);
            let err = Config::from_vars(&vars(&v)).unwrap_err().to_string();
            assert!(err.contains("CLERK_GATEWAY_URL"), "{bad}: {err}");
        }

        let mut v = FULL.to_vec();
        v.extend_from_slice(WRITE_HALF);
        let c = Config::from_vars(&vars(&v)).unwrap();
        assert_eq!(c.write_half().unwrap().gateway_url, "http://127.0.0.1:8080");
    }

    #[test]
    fn decision_seconds_defaults_to_five() {
        let mut v = FULL.to_vec();
        v.extend_from_slice(WRITE_HALF);
        let c = Config::from_vars(&vars(&v)).unwrap();
        assert_eq!(c.write_half().unwrap().decision, Duration::from_secs(5));
    }

    #[test]
    fn the_decision_interval_is_at_least_a_second() {
        let mut v = FULL.to_vec();
        v.extend_from_slice(WRITE_HALF);
        v.push(("CLERK_DECISION_SECONDS", "0"));
        assert!(Config::from_vars(&vars(&v))
            .unwrap_err()
            .to_string()
            .contains("CLERK_DECISION_SECONDS"));
        // Without a write half the variable is not read, so a zero there
        // refuses nothing.
        let mut v = FULL.to_vec();
        v.push(("CLERK_DECISION_SECONDS", "0"));
        assert!(Config::from_vars(&vars(&v)).unwrap().write_half().is_none());
    }

    #[test]
    fn bus_subjects_follow_the_prefix() {
        let c = Config::from_vars(&vars(FULL)).unwrap();
        assert_eq!(
            c.bus_subject("fr.linagora.twalk.persona.suggest.produced.v1"),
            "twalk.persona.suggest.produced.v1"
        );
    }

    /// The `environment:` block of the `clerk` service in
    /// `deploy/docker-compose/compose.yaml`, as `(key, value)` pairs, read
    /// by a line scan of the indented block rather than a YAML parser: no
    /// Cargo package here depends on `serde_yaml` (the Companion Gateway
    /// uses `serde_yaml_ng`, for its own reasons), and a scan that knows
    /// compose's two-space indentation is enough to find one mapping. It
    /// fails, rather than answering an empty block, when the service or
    /// the block is not where it looks.
    fn compose_clerk_environment() -> Vec<(String, String)> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../deploy/docker-compose/compose.yaml"
        );
        let compose =
            std::fs::read_to_string(path).expect("the reference deployment's compose file");
        let mut lines = compose.lines();
        lines
            .by_ref()
            .find(|line| *line == "  clerk:")
            .expect("a `clerk:` service at the services' indentation");
        let mut in_service = lines.take_while(|line| {
            // The service ends at the next line indented two spaces or less
            // that is not blank and not a comment.
            let indent = line.len() - line.trim_start().len();
            line.trim().is_empty() || line.trim_start().starts_with('#') || indent > 2
        });
        in_service
            .by_ref()
            .find(|line| *line == "    environment:")
            .expect("an `environment:` block on the clerk service");
        in_service
            .take_while(|line| {
                let indent = line.len() - line.trim_start().len();
                line.trim().is_empty() || line.trim_start().starts_with('#') || indent > 4
            })
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| {
                let (key, value) = line
                    .split_once(':')
                    .unwrap_or_else(|| panic!("an environment entry is `KEY: value`: {line:?}"));
                (key.trim().to_owned(), value.trim().to_owned())
            })
            .collect()
    }

    /// The ticket's container-environment criterion, guarded at the file
    /// that decides it: the clerk's compose service is handed its own
    /// `CLERK_*` variables and no credential of the Gateway's or Hermes's —
    /// not the service token that opens the consent snapshot, not Hermes's
    /// key, no token or secret of any kind — with one named exception, the
    /// `${HERMES_USER_LANGUAGE:-}` fallback source for the clerk's own
    /// language, a preference and not a credential. Since #284 the write
    /// half's own credential is a refresh token in a file the compose
    /// service mounts (`CLERK_GATEWAY_SESSION_FILE`), never a value in its
    /// environment, so a `CLERK_GATEWAY_URL: ${CLERK_GATEWAY_URL:-…}` line
    /// naming the Gateway is expected and passes; only a credential —
    /// `_TOKEN`, `_SECRET`, the Gateway's own service token or Hermes's key
    /// — is refused. Asserting it on a container Docker really started is
    /// #284's deployment suite; this is what fails first if a later edit
    /// adds a variable here.
    #[test]
    fn the_compose_service_holds_no_gateway_or_hermes_credential() {
        let environment = compose_clerk_environment();
        assert!(
            !environment.is_empty(),
            "the clerk service's environment block was found but empty"
        );
        let keys: Vec<&str> = environment.iter().map(|(k, _)| k.as_str()).collect();
        for required in [
            "CLERK_NATS_URL",
            "CLERK_RELAY_URL",
            "CLERK_NOSTR_KEY_FILE",
            "CLERK_CHANNEL_APPROVALS",
            "CLERK_CHANNEL_ACTIVITY",
            "CLERK_CHANNEL_JOURNAL",
            "CLERK_USER_LANGUAGE",
        ] {
            assert!(keys.contains(&required), "{required} missing from {keys:?}");
        }
        for (key, value) in &environment {
            assert!(
                key.starts_with("CLERK_"),
                "the clerk's environment holds only CLERK_* variables; found {key}"
            );
            // What the value may name: the deployment's own CLERK_* and
            // NATS_PORT interpolations, and HERMES_USER_LANGUAGE as a
            // fallback source — never Hermes's key or a credential of any
            // kind. A bare `GATEWAY_` reference is not itself the leak
            // (CLERK_GATEWAY_URL: ${CLERK_GATEWAY_URL:-…} must pass) — the
            // write half's credential lives in a mounted file, never here.
            let names = value
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|name| !name.is_empty());
            for name in names {
                assert_ne!(
                    name, "GATEWAY_SERVICE_TOKEN",
                    "{key} references the Gateway's service token"
                );
                assert!(
                    !name.starts_with("HERMES_") || name == "HERMES_USER_LANGUAGE",
                    "{key} references Hermes's {name}"
                );
                assert_ne!(name, "BUZZ_PRIVATE_KEY", "{key} references Hermes's key");
                assert!(
                    !name.ends_with("_TOKEN") && !name.ends_with("_SECRET"),
                    "{key} references a credential: {name}"
                );
            }
        }
    }
}
