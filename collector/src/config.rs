//! The collector's configuration, from the environment — one `COLLECTOR_*`
//! variable per fact, in the Sensor's shape (`sensor/src/config.rs`).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::oidc::{Services, Settings};

/// A connection this process holds, as the registry names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldConnection {
    /// The connection's id in the Companion Gateway's registry
    /// (`GET /api/connections`).
    pub id: String,
    /// `email` or `calendar`.
    pub kind: &'static str,
}

/// A credential the operator holds, for a service that challenges `Basic`
/// rather than reading the SSO's tokens (#342). The password lives in a
/// file of its own at 0600, never in `.env` — the rule #239 established
/// for the OIDC client's secret, for the same reason.
#[derive(Clone)]
pub struct Basic {
    pub user: String,
    pub password: String,
}

/// Redacted: a struct that printed its password would end up in a log.
impl std::fmt::Debug for Basic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "basic as {}", self.user)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// The SSO and the client (`COLLECTOR_OIDC_ISSUER`,
    /// `COLLECTOR_OIDC_CLIENT_ID`, `COLLECTOR_OIDC_CLIENT_SECRET_FILE`,
    /// `COLLECTOR_OIDC_REDIRECT_URI`, `COLLECTOR_OIDC_SCOPES`), and where the
    /// grant lives (`COLLECTOR_STATE_DIR/oidc/grant.json`).
    pub oidc: Option<Settings>,
    /// The credential when it is not an OIDC grant (#342): a username and
    /// a password, for a service that challenges `Basic`. Exactly one of
    /// this and `oidc` is set, which `COLLECTOR_CREDENTIAL` decides.
    pub basic: Option<Basic>,
    /// The services the grant opens, one per connection held
    /// (`COLLECTOR_JMAP_SESSION_URL` with a mail connection,
    /// `COLLECTOR_CALDAV_URL` with a calendar one) — and none for a service
    /// this process does not read (#321).
    pub services: Services,
    /// The account both services must answer as (`COLLECTOR_OWNER_EMAIL`):
    /// a grant for anybody else publishes nothing.
    pub owner_email: String,
    /// The connections this process holds: the mail one
    /// (`COLLECTOR_MAIL_CONNECTION`) and the calendar one
    /// (`COLLECTOR_CALENDAR_CONNECTION`), each optional, at least one set.
    pub connections: Vec<HeldConnection>,
    /// The Companion Gateway, whose registry the connections must be in
    /// (`COLLECTOR_GATEWAY_URL`, `COLLECTOR_GATEWAY_SERVICE_TOKEN`) — the
    /// same service token the Sensor presents for the consent snapshot.
    pub gateway_url: Option<String>,
    pub gateway_service_token: Option<String>,
    /// NATS server URL (`COLLECTOR_NATS_URL`).
    pub nats_url: String,
    /// This collector's name in `source` (`COLLECTOR_HOST`), e.g. the compose
    /// service's hostname. Never a secret.
    pub host: String,
    pub state_dir: PathBuf,
    pub metrics_listen: Option<SocketAddr>,
    /// Where the internal HTTP endpoint the Companion Gateway relays a
    /// free/busy read to listens (`COLLECTOR_HTTP_LISTEN`, #281); `None`
    /// serves none. Its bearer is `gateway_service_token`, so one is not
    /// set without the other.
    pub http_listen: Option<SocketAddr>,
    pub log_level: String,
    /// How often the run loop checks the grant and the services when all is
    /// well (`COLLECTOR_HEALTH_INTERVAL_SECONDS`, 60 by default; a test sets
    /// 1). The access token is renewed ahead of its expiry regardless.
    pub health_interval: std::time::Duration,
    /// How often the calendars are polled for a change
    /// (`COLLECTOR_CALENDAR_POLL_SECONDS`, 60 by default — #251's "poll
    /// every 60 s"; a test sets 1). Its own variable, because a health
    /// check and a read of the owner's agenda are two things to tune.
    pub calendar_poll_interval: std::time::Duration,
    /// How often the mailbox is polled for a delivery
    /// (`COLLECTOR_MAIL_POLL_SECONDS`, 60 by default; #277 makes the push
    /// the rule and this the fallback).
    pub mail_poll_interval: std::time::Duration,
    /// The retry policy for an approved reply the mailbox would not take
    /// (`COLLECTOR_SEND_RETRY_BASE_MS`, 1000; `COLLECTOR_SEND_RETRY_MAX_ATTEMPTS`,
    /// 5) — the Sensor's variables, on this side.
    pub send_retry_base: std::time::Duration,
    pub send_retry_max_attempts: i64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let state_dir = PathBuf::from(required("COLLECTOR_STATE_DIR")?);
        // Which kind of credential this process holds (#342). `oidc` is the
        // default and what every deployment before this one had; `basic` is
        // for a service that challenges `Basic` and does not read the SSO's
        // tokens at all — the ESN's sabre, measured under #320. One process
        // holds one, which is why the two blocks below are exclusive: a
        // second connection on a second credential is a second collector,
        // and its secret does not see this one's.
        let kind = optional_string("COLLECTOR_CREDENTIAL").unwrap_or_else(|| "oidc".to_owned());
        let (oidc, basic) = match kind.as_str() {
            "basic" => {
                let password_file = PathBuf::from(required("COLLECTOR_BASIC_PASSWORD_FILE")?);
                let password = std::fs::read_to_string(&password_file)
                    .with_context(|| {
                        format!(
                            "failed to read COLLECTOR_BASIC_PASSWORD_FILE {}",
                            password_file.display()
                        )
                    })?
                    .trim()
                    .to_owned();
                anyhow::ensure!(
                    !password.is_empty(),
                    "COLLECTOR_BASIC_PASSWORD_FILE {} is empty",
                    password_file.display()
                );
                (
                    None,
                    Some(Basic {
                        user: required("COLLECTOR_BASIC_USER")?,
                        password,
                    }),
                )
            }
            "oidc" => (Some(oidc_settings(&state_dir)?), None),
            other => anyhow::bail!(
                "COLLECTOR_CREDENTIAL is {other:?}: it is \"oidc\" — an SSO grant, the default — \
                 or \"basic\", a username and a password file for a service that challenges Basic"
            ),
        };

        anyhow::ensure!(
            oidc.as_ref()
                .is_none_or(|oidc| oidc.scopes.iter().any(|scope| scope == "offline_access")),
            "COLLECTOR_OIDC_SCOPES must include offline_access: without it the SSO issues no \
             refresh token and the collector would stop in an hour"
        );
        let mut connections = Vec::new();
        if let Some(id) = optional_string("COLLECTOR_MAIL_CONNECTION") {
            connections.push(HeldConnection { id, kind: "email" });
        }
        if let Some(id) = optional_string("COLLECTOR_CALENDAR_CONNECTION") {
            connections.push(HeldConnection {
                id,
                kind: "calendar",
            });
        }
        anyhow::ensure!(
            !connections.is_empty(),
            "the collector holds no connection: set COLLECTOR_MAIL_CONNECTION and/or \
             COLLECTOR_CALENDAR_CONNECTION to the ids GATEWAY_CONNECTIONS declares"
        );
        let gateway_url = optional_string("COLLECTOR_GATEWAY_URL");
        let gateway_service_token = optional_string("COLLECTOR_GATEWAY_SERVICE_TOKEN");
        let gateway_service_token_set = gateway_service_token.is_some();
        // The Gateway's two variables go together, as the Sensor's do
        // (`sensor/src/config.rs`): half a configuration would skip the
        // registry check exactly like none, but silently.
        match (&gateway_url, &gateway_service_token) {
            (Some(_), None) => anyhow::bail!(
                "COLLECTOR_GATEWAY_URL is set without COLLECTOR_GATEWAY_SERVICE_TOKEN: the \
                 registry is read with the Companion Gateway's service token (its own \
                 GATEWAY_SERVICE_TOKEN)"
            ),
            (None, Some(_)) => anyhow::bail!(
                "COLLECTOR_GATEWAY_SERVICE_TOKEN is set without COLLECTOR_GATEWAY_URL: there is \
                 no Companion Gateway to read the registry from"
            ),
            _ => {}
        }
        // A URL per connection held, and none for a service this process
        // does not read (#321): a mail-only collector neither needs a
        // calendar URL nor may be refused for the lack of one, and a
        // calendar-only collector is the shape #342 needs to exist at all.
        let url_for = |kind: &str, variable: &str| -> Result<Option<String>> {
            if !connections.iter().any(|held| held.kind == kind) {
                return Ok(None);
            }
            Ok(Some(required(variable).with_context(|| {
                format!("a {kind} connection is declared, so {variable} says where to read it")
            })?))
        };
        Ok(Self {
            oidc,
            basic,
            services: Services {
                jmap_session_url: url_for("email", "COLLECTOR_JMAP_SESSION_URL")?,
                caldav_url: url_for("calendar", "COLLECTOR_CALDAV_URL")?,
            },
            owner_email: required("COLLECTOR_OWNER_EMAIL")?,
            connections,
            gateway_url,
            gateway_service_token,
            nats_url: required("COLLECTOR_NATS_URL")?,
            host: optional_string("COLLECTOR_HOST").unwrap_or_else(|| "collector".to_owned()),
            state_dir,
            metrics_listen: match optional_string("COLLECTOR_METRICS_LISTEN") {
                Some(value) => Some(value.parse().with_context(|| {
                    format!("COLLECTOR_METRICS_LISTEN is not host:port: {value:?}")
                })?),
                None => None,
            },
            http_listen: match optional_string("COLLECTOR_HTTP_LISTEN") {
                Some(value) => {
                    anyhow::ensure!(
                        gateway_service_token_set,
                        "COLLECTOR_HTTP_LISTEN is set without COLLECTOR_GATEWAY_SERVICE_TOKEN: the \
                         free/busy endpoint answers the Companion Gateway's service token and \
                         nothing else, so without one it would answer nobody (#281)"
                    );
                    Some(value.parse().with_context(|| {
                        format!("COLLECTOR_HTTP_LISTEN is not host:port: {value:?}")
                    })?)
                }
                None => None,
            },
            log_level: optional_string("COLLECTOR_LOG_LEVEL").unwrap_or_else(|| "info".to_owned()),
            health_interval: std::time::Duration::from_secs(
                match optional_string("COLLECTOR_HEALTH_INTERVAL_SECONDS") {
                    Some(value) => value.parse().with_context(|| {
                        format!("COLLECTOR_HEALTH_INTERVAL_SECONDS is not a number: {value:?}")
                    })?,
                    None => 60,
                },
            ),
            calendar_poll_interval: std::time::Duration::from_secs(
                match optional_string("COLLECTOR_CALENDAR_POLL_SECONDS") {
                    Some(value) => value.parse().with_context(|| {
                        format!("COLLECTOR_CALENDAR_POLL_SECONDS is not a number: {value:?}")
                    })?,
                    None => 60,
                },
            ),
            mail_poll_interval: std::time::Duration::from_secs(
                match optional_string("COLLECTOR_MAIL_POLL_SECONDS") {
                    Some(value) => value.parse().with_context(|| {
                        format!("COLLECTOR_MAIL_POLL_SECONDS is not a number: {value:?}")
                    })?,
                    None => 60,
                },
            ),
            send_retry_base: std::time::Duration::from_millis(
                match optional_string("COLLECTOR_SEND_RETRY_BASE_MS") {
                    Some(value) => value.parse().with_context(|| {
                        format!("COLLECTOR_SEND_RETRY_BASE_MS is not a number: {value:?}")
                    })?,
                    None => 1000,
                },
            ),
            send_retry_max_attempts: match optional_string("COLLECTOR_SEND_RETRY_MAX_ATTEMPTS") {
                Some(value) => value.parse().with_context(|| {
                    format!("COLLECTOR_SEND_RETRY_MAX_ATTEMPTS is not a number: {value:?}")
                })?,
                None => 5,
            },
        })
    }

    /// Refuses a held connection the registry does not name (ADR 0033), or
    /// names as another kind: every event it published would be about a
    /// perimeter no decision governs — or about a calendar, under a
    /// mailbox's id. Said in words, naming the variable and the kind to
    /// declare. `registry` is `(id, kind)` as the Companion Gateway's
    /// snapshot spells its `connections[]`.
    pub fn refuse_unknown_connections(&self, registry: &[(String, String)]) -> Result<()> {
        for held in &self.connections {
            let listed = || {
                if registry.is_empty() {
                    "empty".to_owned()
                } else {
                    registry
                        .iter()
                        .map(|(id, kind)| format!("{id} ({kind})"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            };
            match registry.iter().find(|(id, _)| id == &held.id) {
                None => anyhow::bail!(
                    "the connection {:?} ({}) is not in the Companion Gateway's registry ({}): \
                     declare it in GATEWAY_CONNECTIONS with the kind {}",
                    held.id,
                    held.variable(),
                    listed(),
                    held.kind
                ),
                Some((_, kind)) if kind != held.kind => anyhow::bail!(
                    "the connection {:?} ({}) is a {} here and a {kind} in the Companion \
                     Gateway's registry ({}): one of the two is the wrong id",
                    held.id,
                    held.variable(),
                    held.kind,
                    listed()
                ),
                Some(_) => {}
            }
        }
        Ok(())
    }
}

impl HeldConnection {
    /// The variable that named this connection.
    pub fn variable(&self) -> &'static str {
        match self.kind {
            "calendar" => "COLLECTOR_CALENDAR_CONNECTION",
            _ => "COLLECTOR_MAIL_CONNECTION",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(connections: Vec<HeldConnection>) -> Config {
        Config {
            basic: None,
            oidc: Some(Settings {
                issuer: "https://sso.example".to_owned(),
                client_id: "c".to_owned(),
                client_secret_file: PathBuf::from("/nonexistent"),
                redirect_uri: "http://localhost:1/callback".to_owned(),
                scopes: vec!["offline_access".to_owned()],
                grant_file: PathBuf::from("/nonexistent/grant.json"),
            }),
            services: Services {
                jmap_session_url: Some("https://mail.example/jmap/session".to_owned()),
                caldav_url: Some("https://calendar.example/".to_owned()),
            },
            owner_email: "michel@example.com".to_owned(),
            connections,
            gateway_url: None,
            gateway_service_token: None,
            nats_url: "nats://localhost:4222".to_owned(),
            host: "collector".to_owned(),
            state_dir: PathBuf::from("/nonexistent"),
            metrics_listen: None,
            http_listen: None,
            log_level: "info".to_owned(),
            health_interval: std::time::Duration::from_secs(60),
            calendar_poll_interval: std::time::Duration::from_secs(60),
            mail_poll_interval: std::time::Duration::from_secs(60),
            send_retry_base: std::time::Duration::from_millis(1000),
            send_retry_max_attempts: 5,
        }
    }

    #[test]
    fn a_connection_the_registry_does_not_name_is_refused_naming_the_variable_and_the_kind() {
        let config = config(vec![
            HeldConnection {
                id: "mail-linagora".to_owned(),
                kind: "email",
            },
            HeldConnection {
                id: "calendar-linagora".to_owned(),
                kind: "calendar",
            },
        ]);
        let entry = |id: &str, kind: &str| (id.to_owned(), kind.to_owned());
        assert!(config
            .refuse_unknown_connections(&[
                entry("mail-linagora", "email"),
                entry("calendar-linagora", "calendar")
            ])
            .is_ok());
        let refused = config
            .refuse_unknown_connections(&[
                entry("mail-linagora", "email"),
                entry("whatsapp", "whatsapp"),
            ])
            .unwrap_err()
            .to_string();
        assert!(refused.contains("calendar-linagora"), "{refused}");
        assert!(
            refused.contains("COLLECTOR_CALENDAR_CONNECTION"),
            "{refused}"
        );
        assert!(refused.contains("kind calendar"), "{refused}");
        assert!(refused.contains("GATEWAY_CONNECTIONS"), "{refused}");
        // The id is there and it is not a calendar: a mailbox's id given to
        // the calendar variable, refused rather than published under.
        let wrong_kind = config
            .refuse_unknown_connections(&[
                entry("mail-linagora", "email"),
                entry("calendar-linagora", "email"),
            ])
            .unwrap_err()
            .to_string();
        assert!(wrong_kind.contains("calendar-linagora"), "{wrong_kind}");
        assert!(wrong_kind.contains("wrong id"), "{wrong_kind}");
    }
}

fn optional_string(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// The SSO and the client, when this collector's credential is a grant.
fn oidc_settings(state_dir: &std::path::Path) -> Result<Settings> {
    Ok(Settings {
        issuer: required("COLLECTOR_OIDC_ISSUER")?
            .trim_end_matches('/')
            .to_owned(),
        client_id: required("COLLECTOR_OIDC_CLIENT_ID")?,
        client_secret_file: PathBuf::from(required("COLLECTOR_OIDC_CLIENT_SECRET_FILE")?),
        redirect_uri: required("COLLECTOR_OIDC_REDIRECT_URI")?,
        scopes: optional_string("COLLECTOR_OIDC_SCOPES")
            .unwrap_or_else(|| "openid profile email offline_access".to_owned())
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        grant_file: state_dir.join("oidc").join("grant.json"),
    })
}

fn required(name: &str) -> Result<String> {
    optional_string(name).with_context(|| format!("missing required environment variable {name}"))
}
