//! The OIDC grant the collector holds, and how it stays held (issue #274).
//!
//! # The shape of the grant
//!
//! The collector is a **confidential client** of the deployment's SSO: it has
//! a client id and a client secret, and it holds one **refresh token** for
//! the owner's account. Everything it does against a mailbox or a calendar
//! it does with a short-lived access token renewed from that refresh token.
//! The SSO **rotates** the refresh token on every renewal — the one it
//! answered with is the only live one, the one just presented is dead — and
//! that single fact shapes this module:
//!
//! - the rotated token is written to disk **before** the access token it
//!   came with serves anything, because a crash between the two must resume
//!   from the token the SSO still knows, not from one it has forgotten;
//! - one renewal is in flight at a time (a mutex around the whole exchange),
//!   because two concurrent renewals would each spend the same token and one
//!   of them would be refused as revoked;
//! - one process per grant (`lib.rs`), for the same reason across processes.
//!
//! # Two refusals, never one
//!
//! The SSO refusing to renew — `400`/`401` on the token endpoint, typically
//! `invalid_grant` — means the grant is gone: revoked by the user, expired,
//! or rotated away by another holder. Only the operator can give a new one,
//! so this is [`Renewal::ReconnectRequired`]. A **service** refusing a fresh
//! access token — `401`/`403` from JMAP or the calendar side service — means
//! the grant stands and the token is fine, but lacks what that service
//! expects: an audience, a scope, a client the service was not told about.
//! The operator changes the *client*, not the grant, so this is
//! [`ServiceRefusal::pending_operator`], named per service. Folding the two
//! into one "unauthorized" would send the operator to re-authorize for a
//! problem authorizing again cannot fix, or to reconfigure a client for a grant that is
//! simply gone.
//!
//! # What is never printed
//!
//! Tokens. The authorization URL the operator opens carries a PKCE challenge
//! and a `state`, both random and both worthless once used; the callback
//! URL the operator pastes carries a code that is spent on the spot. Neither
//! the code, the access token nor the refresh token appears in a log line or
//! a refusal's words, and the client secret is read from a file
//! (`COLLECTOR_OIDC_CLIENT_SECRET_FILE`) — never argv, where `ps` shows it
//! (the lesson of #239), never a bare variable a `docker inspect` prints.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// What the deployment configures: the SSO, the client, where the grant lives.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The issuer, whose `/.well-known/openid-configuration` names the rest
    /// (`COLLECTOR_OIDC_ISSUER`).
    pub issuer: String,
    pub client_id: String,
    /// The client secret, as a file the operator owns
    /// (`COLLECTOR_OIDC_CLIENT_SECRET_FILE`). Read at each use, trimmed.
    pub client_secret_file: PathBuf,
    /// The redirect URI registered with the client. Nothing listens there:
    /// the operator pastes the callback URL back, which is why it may be a
    /// loopback address that resolves to nothing.
    pub redirect_uri: String,
    /// The scopes asked for; `offline_access` is what makes the SSO issue a
    /// refresh token, and a grant without one is refused.
    pub scopes: Vec<String>,
    /// Where the grant is written: `<state>/oidc/grant.json`, mode 0600.
    pub grant_file: PathBuf,
}

/// What the SSO's discovery document told us.
#[derive(Debug, Clone, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
}

/// The grant on disk: the one thing the collector persists about the SSO.
/// Its `Debug` names everything but the token, so a `{:?}` in a log line
/// cannot be the leak.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub issuer: String,
    pub client_id: String,
    pub refresh_token: String,
    /// When this refresh token was obtained — for the operator reading the
    /// file, not for any decision here.
    pub obtained_at: String,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("refresh_token", &"<redacted>")
            .field("obtained_at", &self.obtained_at)
            .finish()
    }
}

impl Grant {
    /// Reads the grant, or `None` when none was ever written. A file that
    /// exists and does not parse is an error, not an absence: the operator
    /// wrote something there, and guessing past it would hide that.
    pub fn read(path: &Path) -> Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(serde_json::from_str(&text).with_context(|| {
                format!("{} holds something that is not a grant", path.display())
            })?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    /// Writes the grant at 0600, atomically: the bytes land in a sibling
    /// file, are synced, and are renamed over the previous grant, so a crash
    /// mid-write leaves either the old grant or the new one and never half
    /// of one. The directory is created 0700 when missing.
    pub fn write(&self, path: &Path) -> Result<()> {
        crate::fs::write_json_private(path, self)
    }
}

use std::os::unix::fs::PermissionsExt;

/// A short-lived access token, held in memory only. `Debug` shows when it
/// expires and never what it is.
#[derive(Clone)]
pub struct AccessToken {
    pub token: String,
    pub expires_at: SystemTime,
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessToken")
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl AccessToken {
    /// Whether the token is good for at least `margin` more: what decides a
    /// renewal before a request rather than a `401` after one.
    pub fn is_fresh(&self, margin: Duration) -> bool {
        SystemTime::now() + margin < self.expires_at
    }
}

/// What a renewal came to.
#[derive(Debug, Clone)]
pub enum Renewal {
    /// A fresh access token, and the grant the SSO rotated to — already on
    /// disk when this is returned.
    Renewed { grant: Grant, access: AccessToken },
    /// The SSO refused to renew because the **grant** is gone
    /// (`invalid_grant`: revoked, expired, rotated away by another holder)
    /// and only the owner signing in again can give a new one. `detail` is
    /// the SSO's own error code and description, never a token.
    ReconnectRequired { detail: String },
    /// The SSO refused to renew because of the **client**, not the grant
    /// (`invalid_client`, `unauthorized_client`, `invalid_scope`: a wrong
    /// secret, a client the SSO no longer allows this flow, a scope it does
    /// not grant). The grant may well stand; authorizing again would not
    /// help, and the operator changes the client's configuration. The third
    /// refusal the two-refusals rule owes: sent to re-authorize for a wrong
    /// secret, an operator re-authorizes for nothing.
    PendingOperator { detail: String },
    /// The SSO did not answer, or answered something that is not a token
    /// response: nothing is wrong with the grant, and the next attempt may
    /// succeed.
    Unreachable { detail: String },
}

/// An authorization the operator is in the middle of: the link they open, and
/// what the callback must match.
#[derive(Debug, Clone)]
pub struct StartedAuthorization {
    pub authorization_url: String,
    state: String,
    verifier: String,
}

/// The collector's client of the SSO.
pub struct Client {
    settings: Settings,
    discovery: Discovery,
    http: reqwest::Client,
    /// One renewal in flight at a time: two would spend the same token.
    renewing: tokio::sync::Mutex<()>,
}

impl Client {
    /// Reads the SSO's discovery document. Refused when its `issuer` is not
    /// the one configured: a document answering for another issuer is a
    /// misconfiguration, or something worse, and a token from it would be
    /// presented to services that expect this issuer's.
    pub async fn discover(settings: Settings) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()?;
        let url = format!(
            "{}/.well-known/openid-configuration",
            settings.issuer.trim_end_matches('/')
        );
        let discovery: Discovery = http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("the SSO's discovery document at {url} did not answer"))?
            .error_for_status()
            .with_context(|| format!("the SSO's discovery document at {url} was refused"))?
            .json()
            .await
            .with_context(|| format!("the SSO's discovery document at {url} is not one"))?;
        anyhow::ensure!(
            discovery.issuer.trim_end_matches('/') == settings.issuer.trim_end_matches('/'),
            "the discovery document names the issuer {:?}, not the configured {:?}",
            discovery.issuer,
            settings.issuer
        );
        Ok(Self {
            settings,
            discovery,
            http,
            renewing: tokio::sync::Mutex::new(()),
        })
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The first half of the authorization: the link the operator opens in a browser.
    /// PKCE S256 and a random `state`, both minted here and both checked when
    /// the callback comes back.
    pub fn begin_authorization(&self) -> Result<StartedAuthorization> {
        let verifier = random_token(32);
        let state = random_token(16);
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        let query = form(&[
            ("response_type", "code"),
            ("client_id", &self.settings.client_id),
            ("redirect_uri", &self.settings.redirect_uri),
            ("scope", &self.settings.scopes.join(" ")),
            ("state", &state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        Ok(StartedAuthorization {
            authorization_url: format!("{}?{query}", self.discovery.authorization_endpoint),
            state,
            verifier,
        })
    }

    /// The second half: the callback URL the operator pasted. Its `state`
    /// must be the one minted; its `code` is exchanged once, with the PKCE
    /// verifier; a grant with no refresh token is refused, because a
    /// collector that cannot renew is one that stops working in an hour.
    /// The grant is on disk when this returns.
    pub async fn complete_authorization(
        &self,
        started: &StartedAuthorization,
        callback: &str,
    ) -> Result<Grant> {
        let (_, query) = callback
            .split_once('?')
            .context("the callback URL carries no query: paste the whole address bar")?;
        let params = parse_form(query);
        anyhow::ensure!(
            params.get("state").map(String::as_str) == Some(started.state.as_str()),
            "the callback's state is not the one this authorization started with: paste the \
             address the SSO redirected to, from this run and not an earlier one"
        );
        if let Some(error) = params.get("error") {
            anyhow::bail!(
                "the SSO refused the sign-in: {error} {}",
                params.get("error_description").cloned().unwrap_or_default()
            );
        }
        let code = params
            .get("code")
            .filter(|code| !code.is_empty())
            .context("the callback URL carries no code")?;
        let secret = self.client_secret()?;
        let response = self
            .http
            .post(&self.discovery.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", self.settings.redirect_uri.as_str()),
                ("client_id", self.settings.client_id.as_str()),
                ("client_secret", secret.as_str()),
                ("code_verifier", started.verifier.as_str()),
            ])
            .send()
            .await
            .context("the SSO's token endpoint did not answer")?;
        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .context("the SSO's token endpoint answered something that is not JSON")?;
        anyhow::ensure!(
            status.is_success(),
            "the SSO refused the code exchange ({status}): {}",
            refusal_words(&body)
        );
        let tokens: TokenResponse = serde_json::from_value(body)
            .context("the SSO's token response is missing what a token response has")?;
        let refresh_token = tokens.refresh_token.filter(|t| !t.is_empty()).context(
            "the SSO issued no refresh token: the client must be allowed offline_access, or \
             the collector could not renew and would stop in an hour",
        )?;
        let grant = Grant {
            issuer: self.settings.issuer.clone(),
            client_id: self.settings.client_id.clone(),
            refresh_token,
            obtained_at: now_rfc3339(),
        };
        grant.write(&self.settings.grant_file)?;
        Ok(grant)
    }

    /// Renews the access token from the grant. The rotated grant is on disk
    /// before this returns it; one renewal runs at a time.
    pub async fn renew(&self, grant: &Grant) -> Result<Renewal> {
        let _one_at_a_time = self.renewing.lock().await;
        let secret = self.client_secret()?;
        let response = match self
            .http
            .post(&self.discovery.token_endpoint)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", grant.refresh_token.as_str()),
                ("client_id", self.settings.client_id.as_str()),
                ("client_secret", secret.as_str()),
            ])
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return Ok(Renewal::Unreachable {
                    detail: format!("the SSO's token endpoint did not answer: {error}"),
                })
            }
        };
        let status = response.status();
        let body: serde_json::Value = match response.json().await {
            Ok(body) => body,
            Err(error) => {
                return Ok(Renewal::Unreachable {
                    detail: format!(
                        "the SSO's token endpoint answered something that is not JSON: {error}"
                    ),
                })
            }
        };
        if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED
        {
            // RFC 6749 §5.2: the error code says whose fault it is. The
            // grant's (`invalid_grant`) is the owner's to renew by signing
            // in again; the client's is the operator's to fix at the SSO,
            // and no sign-in changes it. An unknown code is read as the
            // grant's: the remedy that costs the operator a minute rather
            // than the one that costs a wrong diagnosis.
            let words = refusal_words(&body);
            let code = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
            return Ok(match code {
                "invalid_client" | "unauthorized_client" | "invalid_scope" => {
                    Renewal::PendingOperator {
                        detail: format!(
                            "the SSO refused the client, not the grant ({status}): {words}. Check \
                             COLLECTOR_OIDC_CLIENT_ID, the secret in \
                             COLLECTOR_OIDC_CLIENT_SECRET_FILE and the client's configuration \
                             at the SSO; signing in again would not change this"
                        ),
                    }
                }
                _ => Renewal::ReconnectRequired {
                    detail: format!("the SSO refused to renew the grant ({status}): {words}"),
                },
            });
        }
        if !status.is_success() {
            return Ok(Renewal::Unreachable {
                detail: format!(
                    "the SSO's token endpoint answered {status}: {}",
                    refusal_words(&body)
                ),
            });
        }
        let tokens: TokenResponse = match serde_json::from_value(body) {
            Ok(tokens) => tokens,
            Err(error) => {
                return Ok(Renewal::Unreachable {
                    detail: format!(
                        "the SSO's token response is missing what a token response has: {error}"
                    ),
                })
            }
        };
        // The rotated token, on disk first. An SSO that did not rotate hands
        // the same token back, and writing it again costs nothing.
        let rotated = Grant {
            refresh_token: tokens
                .refresh_token
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| grant.refresh_token.clone()),
            obtained_at: now_rfc3339(),
            ..grant.clone()
        };
        rotated.write(&self.settings.grant_file)?;
        let expires_in = tokens.expires_in.unwrap_or(300);
        Ok(Renewal::Renewed {
            grant: rotated,
            access: AccessToken {
                token: tokens.access_token,
                expires_at: SystemTime::now() + Duration::from_secs(expires_in),
            },
        })
    }

    /// The client secret, read at each use so a rotated file is the next
    /// request's secret. Refused when the file is readable by anyone but its
    /// owner, the way the clerk refuses its key (`clerk/src/relay.rs`): the
    /// entrypoint checks the same thing, and the binary run outside it
    /// deserves the same refusal.
    fn client_secret(&self) -> Result<String> {
        let path = &self.settings.client_secret_file;
        let mode = std::fs::metadata(path)
            .with_context(|| {
                format!(
                    "failed to read the client secret from {} (COLLECTOR_OIDC_CLIENT_SECRET_FILE)",
                    path.display()
                )
            })?
            .permissions()
            .mode()
            & 0o777;
        anyhow::ensure!(
            mode & 0o077 == 0,
            "the client secret file {} is readable by group or others (mode {mode:o}): chmod 0600 it",
            path.display()
        );
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "failed to read the client secret from {} (COLLECTOR_OIDC_CLIENT_SECRET_FILE)",
                path.display()
            )
        })?;
        let secret = text.trim().to_owned();
        anyhow::ensure!(
            !secret.is_empty(),
            "{} is empty: a named but empty client secret file is an error, not an absence",
            self.settings.client_secret_file.display()
        );
        Ok(secret)
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// The SSO's `error` and `error_description`, and nothing else of its body:
/// a refusal's words go in a log line and on the bus, and a body that
/// echoed a token would put it there.
fn refusal_words(body: &serde_json::Value) -> String {
    let code = body
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("no error code");
    match body.get("error_description").and_then(|v| v.as_str()) {
        Some(description) => format!("{code}: {description}"),
        None => code.to_owned(),
    }
}

/// The two services a grant opens, and the one question asked of each.
#[derive(Debug, Clone)]
pub struct Services {
    /// TMail's JMAP session resource (`COLLECTOR_JMAP_SESSION_URL`): its
    /// `username` is the account.
    pub jmap_session_url: String,
    /// The calendar side service's root (`COLLECTOR_CALDAV_URL`): `/api/user`
    /// answers the account's `email`.
    pub caldav_url: String,
}

/// Why a service did not answer the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceRefusal {
    /// `401`/`403` with a fresh token: the grant stands, the service wants
    /// something the client does not carry. The operator's, not the user's.
    PendingOperator { status: u16, detail: String },
    /// No answer, or one that is not the document expected.
    Unreachable { detail: String },
}

impl ServiceRefusal {
    pub fn pending_operator(&self) -> bool {
        matches!(self, ServiceRefusal::PendingOperator { .. })
    }

    /// The operator's sentence.
    pub fn detail(&self) -> &str {
        match self {
            ServiceRefusal::PendingOperator { detail, .. }
            | ServiceRefusal::Unreachable { detail } => detail,
        }
    }
}

/// What each service says the account is.
#[derive(Debug, Clone)]
pub struct Identities {
    pub jmap: Result<String, ServiceRefusal>,
    pub caldav: Result<String, ServiceRefusal>,
    /// The owner's id on the calendar side service (`/api/user`'s `_id`),
    /// which their calendar collections are under. `None` when the side
    /// service did not answer, or answered without one.
    pub caldav_owner_id: Option<String>,
}

impl Identities {
    /// The services whose account is not `owner`, with what they said: a
    /// grant for another account is nothing to publish from, and the log
    /// names the account so the operator sees whose it was.
    /// The two answers, each with the service's name, in the order they
    /// are reported.
    pub fn by_service(&self) -> [(&'static str, &Result<String, ServiceRefusal>); 2] {
        [("jmap", &self.jmap), ("caldav", &self.caldav)]
    }

    /// Whether a service answered `401` — the token itself refused, which
    /// is what a revocation looks like from the service's side — as opposed
    /// to `403`, the client lacking what the service wants.
    pub fn unauthenticated(&self) -> bool {
        self.by_service().iter().any(|(_, identity)| {
            matches!(
                identity,
                Err(ServiceRefusal::PendingOperator { status: 401, .. })
            )
        })
    }

    pub fn owner_mismatch(&self, owner: &str) -> Vec<(&'static str, String)> {
        let mut mismatched = Vec::new();
        for (service, identity) in self.by_service() {
            if let Ok(account) = identity {
                if !account.eq_ignore_ascii_case(owner) {
                    mismatched.push((service, account.clone()));
                }
            }
        }
        mismatched
    }
}

impl Services {
    /// Asks both services who the token belongs to, with a fresh token.
    pub async fn whoami(&self, access: &AccessToken) -> Result<Identities> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()?;
        let jmap = ask(&http, "jmap", &self.jmap_session_url, access, |body| {
            body.get("username")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .await;
        // The side service's `/api/user` is the OpenPaaS one: the owner's
        // `_id`, which the calendar collections are under (#280), beside
        // `preferredEmail`. Both are read here so the calendar half asks
        // nothing more.
        let caldav_user = format!("{}/api/user", self.caldav_url.trim_end_matches('/'));
        let caldav_document = ask(&http, "caldav", &caldav_user, access, |body| {
            body.get("preferredEmail")
                .and_then(|v| v.as_str())
                .map(|email| {
                    (
                        email.to_owned(),
                        body.get("_id").and_then(|v| v.as_str()).map(str::to_owned),
                    )
                })
        })
        .await;
        let (caldav, caldav_owner_id) = match caldav_document {
            Ok((email, id)) => (Ok(email), id),
            Err(refusal) => (Err(refusal), None),
        };
        Ok(Identities {
            jmap,
            caldav,
            caldav_owner_id,
        })
    }
}

async fn ask<T>(
    http: &reqwest::Client,
    service: &str,
    url: &str,
    access: &AccessToken,
    account_of: impl Fn(&serde_json::Value) -> Option<T>,
) -> Result<T, ServiceRefusal> {
    let response = http
        .get(url)
        .bearer_auth(&access.token)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|error| ServiceRefusal::Unreachable {
            detail: format!("{service} did not answer at {url}: {error}"),
        })?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        // What the operator has to do depends on what the service said, and
        // until #320 the collector said one thing for every refusal: "add an
        // audience or a scope at the SSO". That is the right sentence for a
        // resource server that took the token and wanted more of it, and a
        // wrong instruction for a service that does not read bearer tokens
        // at all — a Cozy instance where the deployment expected an OpenPaaS
        // side service answers `401 text/plain` with no challenge whatever,
        // and no scope added anywhere would change that. So the hint names
        // what was observed: the challenge the service offered, or its
        // absence.
        let challenge = response
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().to_owned());
        return Err(ServiceRefusal::PendingOperator {
            status: status.as_u16(),
            detail: refusal_detail(service, url, status.as_u16(), challenge.as_deref()),
        });
    }
    if !status.is_success() {
        return Err(ServiceRefusal::Unreachable {
            detail: format!("{service} answered {status} at {url}"),
        });
    }
    let body: serde_json::Value =
        response
            .json()
            .await
            .map_err(|error| ServiceRefusal::Unreachable {
                detail: format!("{service} answered something that is not JSON: {error}"),
            })?;
    account_of(&body).ok_or_else(|| ServiceRefusal::Unreachable {
        detail: format!("{service} answered a document with no account in it"),
    })
}

/// The operator's sentence for a service that refused a fresh token,
/// derived from the challenge it offered (RFC 9110 §11.6.1) and from
/// nothing else (#320).
///
/// Three cases, because they are three different mornings:
/// - a `Bearer` challenge: the service reads bearer tokens and refused
///   this one — an audience, a scope, or an expired grant;
/// - another scheme: it asked for something this collector does not send.
///   It may still accept a bearer (a service that advertises `Basic` often
///   does), so the sentence says what was asked rather than what to do;
/// - no challenge at all: it refused without saying how to authenticate,
///   which an OAuth resource server does not do. The URL is the first
///   thing to check.
pub fn refusal_detail(service: &str, url: &str, status: u16, challenge: Option<&str>) -> String {
    let scheme = challenge
        .and_then(|challenge| challenge.split_whitespace().next())
        .map(str::to_ascii_lowercase);
    match scheme.as_deref() {
        Some("bearer") => format!(
            "{service} refused a fresh token with {status} and asks for a bearer: the grant \
             stands, but the client lacks what {service} expects — an audience or a scope the \
             operator has to add to the client at the SSO"
        ),
        Some(_) => {
            let challenge = challenge.unwrap_or_default();
            format!(
                "{service} refused a fresh token with {status} and asked for {challenge} \
                 instead of a bearer: it is not taking the SSO's tokens on this route. Check \
                 that {url} is the service this collector reads, and that its operator accepts \
                 the SSO's tokens there"
            )
        }
        None => format!(
            "{service} refused a fresh token with {status} and offered no authentication \
             challenge, which an OAuth resource server does not do. Check that {url} names the \
             service this collector reads — no scope added at the SSO will change this answer"
        ),
    }
}

/// `bytes` random bytes from the OS CSPRNG, base64url — the shape a PKCE
/// verifier and a `state` take.
fn random_token(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("the operating system's CSPRNG is available");
    base64url(&buffer)
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut buffer = [0u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let triple = u32::from(buffer[0]) << 16 | u32::from(buffer[1]) << 8 | u32::from(buffer[2]);
        for position in 0..(chunk.len() + 1) {
            let index = (triple >> (18 - 6 * position)) & 0x3f;
            out.push(ALPHABET[index as usize] as char);
        }
    }
    out
}

/// `application/x-www-form-urlencoded`, encoded.
fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `application/x-www-form-urlencoded`, decoded — the callback's query.
fn parse_form(query: &str) -> std::collections::HashMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                match u8::from_str_radix(&value[index + 1..index + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// RFC 3339, UTC, to the second: what the grant file says about itself.
pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .replace_nanosecond(0)
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #320: the sentence an operator reads is derived from the challenge
    /// the service offered, because that is the only thing that says what
    /// is wrong. Measured on two real services: LINAGORA's Twake Calendar
    /// (a Cozy instance) refuses a bearer with `401` and **no**
    /// `WWW-Authenticate` at all, and the Twake dev platform's OpenPaaS
    /// side service refuses with `Basic realm="ESN"` — neither of which is
    /// a missing audience, which is what the collector used to say to both.
    #[test]
    fn what_the_operator_is_told_follows_what_the_service_asked_for() {
        let bearer = refusal_detail(
            "caldav",
            "https://side.example.org",
            403,
            Some("Bearer realm=\"openpaas\", error=\"insufficient_scope\""),
        );
        assert!(
            bearer.contains(
                "an audience or a scope the operator has to add to the client at the SSO"
            ),
            "{bearer}"
        );

        // A service that asks for another scheme may still take a bearer —
        // many advertise only `Basic` — so the sentence says what was
        // asked and leaves the conclusion to the operator.
        let basic = refusal_detail(
            "caldav",
            "https://sabre.example.org",
            401,
            Some("Basic realm=\"ESN\", charset=\"UTF-8\""),
        );
        assert!(basic.contains("asked for Basic realm=\"ESN\""), "{basic}");
        assert!(basic.contains("https://sabre.example.org"), "{basic}");
        assert!(
            !basic.contains("audience"),
            "not a missing audience: {basic}"
        );

        // No challenge at all: not an OAuth resource server, and no scope
        // added anywhere will change its answer.
        let silent = refusal_detail("caldav", "https://cozy.example.org", 401, None);
        assert!(
            silent.contains("offered no authentication challenge"),
            "{silent}"
        );
        assert!(
            silent.contains("no scope added at the SSO will change this answer"),
            "{silent}"
        );
        assert!(silent.contains("https://cozy.example.org"), "{silent}");

        // The scheme is read case-insensitively: RFC 9110 §11.1 does not
        // fix its spelling.
        assert!(
            refusal_detail("jmap", "https://mail.example.org", 401, Some("bearer"))
                .contains("an audience or a scope")
        );
    }

    #[test]
    fn the_pkce_challenge_is_the_s256_of_the_verifier_in_base64url() {
        // RFC 7636 appendix B's worked example.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            base64url(&Sha256::digest(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn the_form_round_trips_and_the_callback_is_read_whole() {
        let query = form(&[("state", "a b&c"), ("code", "x=y")]);
        assert_eq!(query, "state=a%20b%26c&code=x%3Dy");
        let parsed = parse_form(&query);
        assert_eq!(parsed["state"], "a b&c");
        assert_eq!(parsed["code"], "x=y");
    }

    #[test]
    fn a_refusals_words_are_the_code_and_the_description_and_nothing_else() {
        let body = serde_json::json!({
            "error": "invalid_grant",
            "error_description": "revoked",
            "refresh_token": "never-shown"
        });
        assert_eq!(refusal_words(&body), "invalid_grant: revoked");
    }

    #[test]
    fn a_grant_file_that_is_not_one_is_an_error_and_a_missing_one_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grant.json");
        assert!(Grant::read(&path).unwrap().is_none());
        std::fs::write(&path, "not json").unwrap();
        assert!(Grant::read(&path).is_err());
        let grant = Grant {
            issuer: "https://sso.example".to_owned(),
            client_id: "c".to_owned(),
            refresh_token: "r".to_owned(),
            obtained_at: now_rfc3339(),
        };
        grant.write(&path).unwrap();
        assert_eq!(Grant::read(&path).unwrap(), Some(grant));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(
            !dir.path().join(".grant.json.tmp").exists(),
            "the temporary file was renamed away"
        );
    }
}
