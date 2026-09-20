//! The seam to the Companion Gateway (#284): the clerk's session as its
//! `Buzz` device, and `POST /api/approvals` for the suggestion a ✅ decided.
//!
//! The clerk is a **device of the owner** on the Companion Gateway, the way
//! a phone or a browser tab is, and nothing more privileged: it holds no
//! service token (that one opens the consent snapshot, the list of every
//! contact) and no credential the Gateway would take for anyone but the
//! owner's own device. What it holds is one **refresh token**, in a file it
//! owns and rotates ([`SessionFile`]), and one **device token** that lives in
//! memory for the fifteen minutes the Gateway grants it and is never written
//! anywhere. An operator signs the device in once
//! (`provision-clerk-device.sh`), and from then on the clerk keeps the
//! session alive by itself.
//!
//! Four things this module decides. The refresh token is **rotated on disk
//! before the new device token is believed** ([`Gateway::refresh`]): the
//! Gateway rotates both tokens on every refresh and the old refresh token
//! dies at that moment, so a clerk that kept the device token and then failed
//! to write the file would run for fifteen minutes and then be signed out for
//! good — the order makes the file the one place the session survives. A
//! `401` is answered by **one refresh and one retry, never two**
//! ([`Gateway::approve`], [`Gateway::devices`]): the first `401` is a device
//! token that expired in memory, which a refresh cures; a second one right
//! after a refresh that succeeded is a session the Gateway will not have —
//! the device was revoked from the dashboard, or the refresh token died
//! (thirty days unused, or rotated away by another process holding the same
//! file) — and that is [`GatewayError::Unauthenticated`], the one error the
//! loop must stop on, since retrying it would turn a revocation into a hot
//! loop of refusals. **No error and no log line carries a token or a
//! response body**: the refresh token is in the `Cookie` header and the
//! device token in the `Set-Cookie`, neither of which is ever formatted, and
//! a body that could not be read is described by its length and where the
//! parse stopped, the way `relay.rs` describes one. And the **session file is
//! nobody else's to read** ([`SessionFile::open`]): a refresh token is thirty
//! days of the owner's approval right, and a file the whole host can open is
//! one every other container mounting the same directory can too, so a loose
//! mode is refused before the file is read, naming the `chmod` that fixes it.
//!
//! What it deliberately leaves alone: **the refusal vocabulary**. A `4xx`
//! or `5xx` with an `error` code is [`Outcome::Refused`] carrying that code
//! and nothing else, because the sentence a code becomes is `refusals.rs`'s
//! to say, in the owner's language, in the thread on Buzz — and this module
//! must not learn a second one. The exception is `409 already_approved`,
//! which is the Gateway's normal answer to a duplicate and is therefore
//! **success** ([`Outcome::Approved`] with `already`): a ✅ redelivered, or a
//! clerk restarted between the approval and its record on Buzz, is one reply
//! that went out once, and the Gateway hands back the record so the clerk can
//! say where it went instead of telling the owner to try again.

use std::fmt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, COOKIE, SET_COOKIE};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// The variable the session file holds the refresh token under:
/// `TWALK_GATEWAY_REFRESH_TOKEN=<token>` on a line of its own, the shape
/// `provision-clerk-device.sh` writes and the clerk rewrites.
pub const SESSION_KEY: &str = "TWALK_GATEWAY_REFRESH_TOKEN";
/// The Companion Gateway's device-token cookie: what every `/api` route
/// reads, from the `Cookie` header and nowhere else.
pub const DEVICE_COOKIE: &str = "twalk_device";
/// The Companion Gateway's refresh-token cookie, scoped to
/// `/api/session`: what `POST /api/session/refresh` is authenticated by.
pub const REFRESH_COOKIE: &str = "twalk_refresh";
/// How long one request to the Companion Gateway may take. The same as the
/// relay's: long enough for a bus lookup behind `POST /api/approvals`, short
/// enough that a Gateway that hangs does not hold the decision loop.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest `error` code an error or a log line keeps. The Gateway's
/// longest is 35 characters; a "code" longer than this is not one.
const CODE_CHARS: usize = 64;

/// Why one exchange with the Companion Gateway did not produce what was
/// asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayError {
    /// Nothing answered: connection refused, DNS, TLS, or the request timed
    /// out. The Gateway may be down or may be starting.
    Unreachable(String),
    /// The Gateway will not have this session: a `401` on a refresh, or a
    /// `401` on a request made with a device token a refresh had just
    /// issued. The device was revoked, or the refresh token died — the
    /// Gateway deliberately says which no more than this — and the way out
    /// is an operator signing the device in again.
    Unauthenticated,
    /// The Gateway answered something that is not this route's answer, or
    /// an answer with no `error` code: described by its status and by where
    /// the parse stopped and how long the body was, never by the body.
    Malformed { status: u16, why: String },
    /// The session file could not be opened, read or rewritten. Names the
    /// path and the operation, never the token.
    Session(String),
}

impl GatewayError {
    /// Whether the same request, made again later, could succeed: the
    /// Gateway could not be reached, or it answered `429` or `5xx` to
    /// something that reached an error — the same line the relay client
    /// draws (`relay.rs`, `RelayError::is_transient`). A Gateway answering
    /// `503 sign_in_not_configured` or `500 store_failed` during a deploy,
    /// or a reverse proxy's `502` page in front of one that is restarting,
    /// is an outage to wait out, not an answer whose shape will be the same
    /// next time. `Unauthenticated` is the one that must **not** be retried,
    /// and a `2xx`/`4xx` that is not the route's shape, or a session file
    /// that cannot be written, will be the same next time. (A **coded**
    /// `5xx` on an approval never reaches here: it is [`Outcome::Refused`],
    /// and the loop reads the code's remedy.)
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            GatewayError::Unreachable(_)
                | GatewayError::Malformed {
                    status: 429 | 500..=599,
                    ..
                }
        )
    }
}

impl fmt::Display for GatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GatewayError::Unreachable(why) => {
                write!(f, "the Companion Gateway could not be reached: {why}")
            }
            GatewayError::Unauthenticated => write!(
                f,
                "the Companion Gateway will not have the clerk's session: the Buzz device was \
                 revoked or its refresh token died, so sign the device in again \
                 (provision-clerk-device.sh)"
            ),
            GatewayError::Malformed { status, why } => write!(
                f,
                "the Companion Gateway's answer (HTTP {status}) could not be used: {why}"
            ),
            GatewayError::Session(why) => write!(f, "the clerk's session file: {why}"),
        }
    }
}

impl std::error::Error for GatewayError {}

/// The `Set-Cookie` headers of one response as `(name, value)` pairs, the
/// attributes (`Path`, `Max-Age`, `HttpOnly`, …) dropped. The Gateway sets
/// its two session cookies as two headers, and a proxy may merge or reorder
/// them, so every header is read and none is assumed first.
pub fn cookies(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| {
            let pair = value.split(';').next()?;
            let (name, value) = pair.split_once('=')?;
            Some((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

/// The refresh token file: one `TWALK_GATEWAY_REFRESH_TOKEN=<token>` line,
/// mode `0600`, owned by the clerk. Every other line the operator wrote (a
/// comment, say) survives a rewrite; only that one is replaced.
#[derive(Debug, Clone)]
pub struct SessionFile {
    path: PathBuf,
}

impl SessionFile {
    /// Opens the session file at `path`, refusing one that group or others
    /// can read, the way `relay::load_keys` refuses the key file: the token
    /// in it is thirty days of the owner's approval right. The file must
    /// exist — the operator's script creates it — because a clerk that
    /// created an empty one would run and refresh nothing, and say so only
    /// on its first approval.
    pub fn open(path: &Path) -> Result<Self, GatewayError> {
        let mode = std::fs::metadata(path)
            .map_err(|e| GatewayError::Session(format!("reading {}: {e}", path.display())))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(GatewayError::Session(format!(
                "{} is readable by group or others (mode {:04o}); it holds the refresh token of \
                 the owner's Buzz device, so run `chmod 0600 {}` and start again",
                path.display(),
                mode & 0o7777,
                path.display()
            )));
        }
        Ok(Self {
            path: path.to_owned(),
        })
    }

    /// The path, for a log line that names the file and never its contents.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The refresh token: the value of the `TWALK_GATEWAY_REFRESH_TOKEN=`
    /// line (an `export` allowed, quotes dropped). A file with no such line
    /// is a session that was never provisioned, and the error says so.
    pub fn read(&self) -> Result<String, GatewayError> {
        let contents = std::fs::read_to_string(&self.path)
            .map_err(|e| GatewayError::Session(format!("reading {}: {e}", self.path.display())))?;
        contents
            .lines()
            .find_map(token_in)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                GatewayError::Session(format!(
                    "{} holds no {SESSION_KEY}=<token> line: the Buzz device was never signed \
                     in, or the file was emptied; run provision-clerk-device.sh",
                    self.path.display()
                ))
            })
    }

    /// Rewrites the file with `token` on its `TWALK_GATEWAY_REFRESH_TOKEN=`
    /// line, **atomically**: a temporary file in the same directory, created
    /// `0600` before a byte is written, synced, then renamed over the
    /// original — so a clerk killed mid-write leaves either the old token or
    /// the new one, never a truncated file. Other lines are carried over.
    pub fn write(&self, token: &str) -> Result<(), GatewayError> {
        let path = &self.path;
        let existing = std::fs::read_to_string(path).unwrap_or_default();
        let mut replaced = false;
        let mut lines: Vec<String> = existing
            .lines()
            .map(|line| {
                if token_in(line).is_some() && !replaced {
                    replaced = true;
                    format!("{SESSION_KEY}={token}")
                } else {
                    line.to_owned()
                }
            })
            .collect();
        if !replaced {
            lines.push(format!("{SESSION_KEY}={token}"));
        }
        let mut contents = lines.join("\n");
        contents.push('\n');

        let dir = path.parent().filter(|dir| !dir.as_os_str().is_empty());
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("session");
        let temp = dir
            .unwrap_or_else(|| Path::new("."))
            .join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
        let failed = |what: &str, e: std::io::Error| {
            GatewayError::Session(format!("{what} {}: {e}", path.display()))
        };
        let result = (|| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)
                .map_err(|e| failed("creating the temporary file beside", e))?;
            file.write_all(contents.as_bytes())
                .map_err(|e| failed("writing the temporary file beside", e))?;
            file.sync_all()
                .map_err(|e| failed("syncing the temporary file beside", e))?;
            std::fs::rename(&temp, path).map_err(|e| failed("renaming the temporary file over", e))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
}

/// The token on one line of the session file, if that line is its
/// assignment: `TWALK_GATEWAY_REFRESH_TOKEN=<token>`, an `export` allowed,
/// surrounding quotes dropped.
fn token_in(line: &str) -> Option<String> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    let assignment = line
        .strip_prefix("export ")
        .map(str::trim_start)
        .unwrap_or(line);
    let value = assignment
        .strip_prefix(SESSION_KEY)
        .and_then(|rest| rest.trim_start().strip_prefix('='))?
        .trim();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value);
    Some(value.to_owned())
}

/// What one approval came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `201`, or `409 already_approved` — the Gateway's normal answer to a
    /// duplicate, which is success: the reply went out once. `event_id` is
    /// the approval's, `edited` whether the text sent was the owner's rather
    /// than the persona's, `already` whether this call was the duplicate.
    Approved {
        event_id: String,
        edited: bool,
        already: bool,
    },
    /// Any other `4xx`/`5xx` with an `error` code. The sentence that code
    /// becomes is `refusals.rs`'s, not this module's.
    Refused { status: u16, code: String },
}

/// One row of `GET /api/devices`, as the Gateway lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub created_unix_seconds: i64,
    pub last_seen_unix_seconds: i64,
    #[serde(default)]
    pub revoked_unix_seconds: Option<i64>,
    #[serde(default)]
    pub current: bool,
}

/// The device token as held in memory: the token, when it dies, and how
/// long it was issued for — the last so that "a fifth of its life left" is
/// a fifth of the lifetime this Gateway grants rather than of a number
/// hard-coded here.
#[derive(Debug, Clone)]
struct DeviceToken {
    token: String,
    expires_at_unix: i64,
    lifetime_seconds: i64,
}

impl DeviceToken {
    /// Whether the token is worth sending: at least a fifth of its lifetime
    /// ahead of it — the Companion's own rule (`refreshAfterSeconds`, #186),
    /// so a request that starts with the token alive does not finish with it
    /// dead.
    fn is_fresh(&self, now_unix: i64) -> bool {
        self.expires_at_unix - now_unix >= self.lifetime_seconds / 5
    }
}

/// The Companion Gateway, reached as the owner's `Buzz` device.
pub struct Gateway {
    /// The Gateway's origin, no trailing slash, so `{base}/api/…` composes.
    base: String,
    /// No redirects — a `302` to a sign-in page would otherwise be followed
    /// with the cookie — no cookie store, so nothing persists but what the
    /// session file holds, and [`REQUEST_TIMEOUT`] on every request.
    http: reqwest::Client,
    session: SessionFile,
    /// The device token, or none before the first refresh. A `tokio` mutex,
    /// held across the refresh itself, because two refreshes in flight at
    /// once would each carry the same refresh token and the second would be
    /// refused as a replay — and answered `Unauthenticated`, which the loop
    /// stops on.
    device: Mutex<Option<DeviceToken>>,
}

impl fmt::Debug for Gateway {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Gateway")
            .field("base", &self.base)
            .field("session_file", &self.session.path)
            .finish_non_exhaustive()
    }
}

/// The body of `POST /api/session/refresh`: only the one field the clerk
/// reads. The tokens are in the cookies, never here.
#[derive(Deserialize)]
struct IssuedSession {
    expires_in: i64,
}

/// The body of a `201` from `POST /api/approvals`, the fields the clerk
/// keeps. `already_approved` carries the same shape under `approval`.
#[derive(Deserialize)]
struct ApprovalRecord {
    event_id: String,
    edited: bool,
}

#[derive(Deserialize)]
struct DeviceList {
    devices: Vec<Device>,
}

/// One answer from the Gateway, read into memory: the status, the headers
/// and the body's text. The body is read whole so that a connection that
/// went while the answer was in flight is `Unreachable` rather than a parse
/// failure.
struct Answer {
    status: u16,
    headers: HeaderMap,
    body: String,
}

impl Gateway {
    /// `base` is the Companion Gateway's origin; a trailing slash is dropped
    /// so the route paths compose.
    pub fn new(base: &str, session: SessionFile) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("a reqwest client with a timeout and no redirects builds");
        Self {
            base: base.trim_end_matches('/').to_owned(),
            http,
            session,
            device: Mutex::new(None),
        }
    }

    /// The origin as stored: no trailing slash.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// `POST /api/session/refresh` with `Cookie: twalk_refresh=<the file's
    /// token>`. On `200` both `Set-Cookie` headers are read, the rotated
    /// refresh token is written to the file **first**, and only then is the
    /// device token kept in memory with `expires_at = now + expires_in` —
    /// because the Gateway has already rotated when it answers, and a device
    /// token believed before the file holds its refresh token is a session
    /// with fifteen minutes to live. `401` is [`GatewayError::Unauthenticated`]:
    /// the device was revoked, or the refresh token died (thirty days
    /// unused, or rotated away by another process). Never logs a token.
    pub async fn refresh(&self) -> Result<(), GatewayError> {
        let mut device = self.device.lock().await;
        self.refresh_locked(&mut device).await
    }

    /// [`Gateway::refresh`] with the device slot already held: the shape
    /// [`Gateway::device_token`] needs so that a check-then-refresh is one
    /// critical section.
    async fn refresh_locked(&self, device: &mut Option<DeviceToken>) -> Result<(), GatewayError> {
        // Whatever was held is stale the moment a refresh is attempted: a
        // refresh that fails leaves no token to send by mistake.
        *device = None;
        let refresh_token = self.session.read()?;
        let request = self
            .http
            .post(format!("{}/api/session/refresh", self.base))
            .header(COOKIE, format!("{REFRESH_COOKIE}={refresh_token}"));
        let answer = self.send(request).await?;
        if answer.status == 401 {
            return Err(GatewayError::Unauthenticated);
        }
        if !(200..300).contains(&answer.status) {
            return Err(GatewayError::Malformed {
                status: answer.status,
                why: match error_code(&answer.body) {
                    Some(code) => format!("a refresh was answered with the error code {code}"),
                    None => "a refresh was answered with no error code".to_owned(),
                },
            });
        }
        let set = cookies(&answer.headers);
        let cookie = |name: &str| {
            set.iter()
                .find(|(key, value)| key == name && !value.is_empty())
                .map(|(_, value)| value.clone())
                .ok_or_else(|| GatewayError::Malformed {
                    status: answer.status,
                    why: format!("the answer to a refresh carried no {name} Set-Cookie"),
                })
        };
        let device_token = cookie(DEVICE_COOKIE)?;
        let rotated = cookie(REFRESH_COOKIE)?;
        let issued: IssuedSession = parse(&answer, "a refresh")?;
        if issued.expires_in <= 0 {
            return Err(GatewayError::Malformed {
                status: answer.status,
                why: format!(
                    "the answer to a refresh names a device token lifetime of {} seconds",
                    issued.expires_in
                ),
            });
        }
        // The file first. If this fails the Gateway has already rotated, the
        // token the file still holds is dead, and the error must say so
        // rather than let the next refresh report an unexplained 401.
        self.session.write(&rotated).map_err(|e| match e {
            GatewayError::Session(why) => GatewayError::Session(format!(
                "{why} — the Companion Gateway had already rotated the refresh token, so the \
                 one the file holds is dead; sign the Buzz device in again"
            )),
            other => other,
        })?;
        *device = Some(DeviceToken {
            token: device_token,
            expires_at_unix: now_unix() + issued.expires_in,
            lifetime_seconds: issued.expires_in,
        });
        info!(
            device_expires_in = issued.expires_in,
            session_file = %self.session.path.display(),
            "gateway session refreshed"
        );
        Ok(())
    }

    /// A device token with at least a fifth of its life left, refreshing
    /// first otherwise (the Companion's own rule, `refreshAfterSeconds`,
    /// #186).
    async fn device_token(&self) -> Result<String, GatewayError> {
        let mut device = self.device.lock().await;
        if let Some(held) = device.as_ref().filter(|held| held.is_fresh(now_unix())) {
            return Ok(held.token.clone());
        }
        self.refresh_locked(&mut device).await?;
        Ok(device
            .as_ref()
            .expect("a refresh that succeeded holds a device token")
            .token
            .clone())
    }

    /// `POST /api/approvals` with `{"suggestion_event_id": id, "final":
    /// {"body": text}}` — no `final` when `edited_body` is `None`, which
    /// sends the persona's own words and is recorded as such. One `401` is
    /// answered by one refresh and one retry; a second `401` is
    /// [`GatewayError::Unauthenticated`].
    pub async fn approve(
        &self,
        suggestion_id: &str,
        edited_body: Option<&str>,
    ) -> Result<Outcome, GatewayError> {
        let mut body = serde_json::json!({ "suggestion_event_id": suggestion_id });
        if let Some(text) = edited_body {
            body["final"] = serde_json::json!({ "body": text });
        }
        let body = serde_json::to_vec(&body).map_err(|e| GatewayError::Malformed {
            status: 0,
            why: format!("serialising the approval request: {e}"),
        })?;
        let answer = self
            .as_device(|token| {
                self.http
                    .post(format!("{}/api/approvals", self.base))
                    .header(COOKIE, format!("{DEVICE_COOKIE}={token}"))
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.clone())
            })
            .await?;
        if (200..300).contains(&answer.status) {
            let record: ApprovalRecord = parse(&answer, "an approval")?;
            return Ok(Outcome::Approved {
                event_id: record.event_id,
                edited: record.edited,
                already: false,
            });
        }
        let Some(code) = error_code(&answer.body) else {
            return Err(GatewayError::Malformed {
                status: answer.status,
                why: format!(
                    "an approval was refused with no error code ({} bytes of body)",
                    answer.body.len()
                ),
            });
        };
        if answer.status == 409 && code == "already_approved" {
            let record = serde_json::from_str::<Value>(&answer.body)
                .ok()
                .and_then(|value| value.get("approval").cloned())
                .and_then(|approval| serde_json::from_value::<ApprovalRecord>(approval).ok())
                .ok_or_else(|| GatewayError::Malformed {
                    status: answer.status,
                    why: "already_approved carried no approval record".to_owned(),
                })?;
            debug!(
                event_id = %record.event_id,
                "the Companion Gateway had already recorded this approval"
            );
            return Ok(Outcome::Approved {
                event_id: record.event_id,
                edited: record.edited,
                already: true,
            });
        }
        warn!(
            status = answer.status,
            code = %code,
            "the Companion Gateway refused an approval"
        );
        Ok(Outcome::Refused {
            status: answer.status,
            code,
        })
    }

    /// `GET /api/devices` — the owner's device list, which the deployment
    /// suite and the operator's script read to see the `Buzz` device; the
    /// decision loop never calls it. The same one-refresh-one-retry as an
    /// approval.
    pub async fn devices(&self) -> Result<Vec<Device>, GatewayError> {
        let answer = self
            .as_device(|token| {
                self.http
                    .get(format!("{}/api/devices", self.base))
                    .header(COOKIE, format!("{DEVICE_COOKIE}={token}"))
            })
            .await?;
        if !(200..300).contains(&answer.status) {
            return Err(GatewayError::Malformed {
                status: answer.status,
                why: match error_code(&answer.body) {
                    Some(code) => format!("the device list was refused with the error code {code}"),
                    None => "the device list was refused with no error code".to_owned(),
                },
            });
        }
        let list: DeviceList = parse(&answer, "the device list")?;
        Ok(list.devices)
    }

    /// One request as the device: built by `request` from a fresh-enough
    /// device token, sent; a `401` answered by one refresh and one more
    /// send, and a `401` to that one is [`GatewayError::Unauthenticated`].
    /// Every other status is the caller's to read.
    async fn as_device<F>(&self, request: F) -> Result<Answer, GatewayError>
    where
        F: Fn(&str) -> reqwest::RequestBuilder,
    {
        let token = self.device_token().await?;
        let answer = self.send(request(&token)).await?;
        if answer.status != 401 {
            return Ok(answer);
        }
        debug!("the Companion Gateway answered 401 to the device token; refreshing once");
        self.refresh().await?;
        let token = self.device_token().await?;
        let answer = self.send(request(&token)).await?;
        if answer.status == 401 {
            return Err(GatewayError::Unauthenticated);
        }
        Ok(answer)
    }

    /// Sends one request and reads its answer whole. Nothing answered, or a
    /// body that could not be read after a status was, is `Unreachable`:
    /// the connection went, and that is the transient class either way.
    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Answer, GatewayError> {
        let response = request
            .send()
            .await
            .map_err(|e| GatewayError::Unreachable(e.to_string()))?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = response.text().await.map_err(|e| {
            GatewayError::Unreachable(format!("reading the Companion Gateway's answer: {e}"))
        })?;
        Ok(Answer {
            status,
            headers,
            body,
        })
    }
}

/// The `error` field of a JSON refusal, provided it is a **code**: at most
/// [`CODE_CHARS`] of `a-z`, `0-9` and `_`, the shape every code the Gateway
/// answers has. Anything else in that field is a sentence, and a sentence
/// is a body — so it is not a code, and nothing of it is kept.
fn error_code(body: &str) -> Option<String> {
    let code = serde_json::from_str::<Value>(body)
        .ok()?
        .get("error")?
        .as_str()?
        .to_owned();
    let is_code = !code.is_empty()
        && code.len() <= CODE_CHARS
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    is_code.then_some(code)
}

/// The body of `answer` read as `T`; a body that is not that shape is
/// described by serde's category of failure, the line and column it stopped
/// at and the body's length — never by its text, because a `201` from
/// `/api/approvals` names the contact and the error is what ends up in a
/// log line.
fn parse<T: serde::de::DeserializeOwned>(answer: &Answer, what: &str) -> Result<T, GatewayError> {
    serde_json::from_str(&answer.body).map_err(|e| GatewayError::Malformed {
        status: answer.status,
        why: format!(
            "the answer to {what} is not the shape that route gives: {:?} at line {} column {} \
             of a body of {} bytes",
            e.classify(),
            e.line(),
            e.column(),
            answer.body.len()
        ),
    })
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::sync::{Arc, Mutex as StdMutex};

    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{HeaderMap as AxumHeaders, Response};
    use axum::routing::{get, post};
    use axum::Router;

    use super::*;

    const MARKER: &str = "SECRET-MARKER";

    /// A directory of the test's own, removed when the test ends.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("twalk-clerk-gateway-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        /// A session file holding `token`, mode 0600.
        fn session(&self, token: &str) -> SessionFile {
            let path = self.0.join("session.env");
            fs::write(
                &path,
                format!("# the clerk's session\n{SESSION_KEY}={token}\n"),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            SessionFile::open(&path).unwrap()
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// One request the stub saw: which route, the `Cookie` header, the body.
    #[derive(Debug, Clone)]
    struct Seen {
        route: &'static str,
        cookie: Option<String>,
        body: String,
    }

    /// One scripted answer: status, extra headers, body.
    #[derive(Clone)]
    struct Scripted {
        status: u16,
        headers: Vec<(&'static str, String)>,
        body: String,
    }

    fn json(status: u16, body: &str) -> Scripted {
        Scripted {
            status,
            headers: vec![],
            body: body.to_owned(),
        }
    }

    /// A `200` to a refresh: both cookies, `expires_in` 900.
    fn issued(device: &str, refresh: &str) -> Scripted {
        Scripted {
            status: 200,
            headers: vec![
                (
                    "set-cookie",
                    format!("{DEVICE_COOKIE}={device}; Path=/; HttpOnly"),
                ),
                (
                    "set-cookie",
                    format!("{REFRESH_COOKIE}={refresh}; Path=/api/session; HttpOnly"),
                ),
            ],
            body: r#"{"owner":"@owner:example.com","expires_in":900}"#.to_owned(),
        }
    }

    #[derive(Default)]
    struct Stub {
        seen: StdMutex<Vec<Seen>>,
        refresh: StdMutex<VecDeque<Scripted>>,
        approve: StdMutex<VecDeque<Scripted>>,
        devices: StdMutex<VecDeque<Scripted>>,
    }

    impl Stub {
        fn seen(&self, route: &str) -> Vec<Seen> {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .filter(|seen| seen.route == route)
                .cloned()
                .collect()
        }

        fn answer(
            &self,
            route: &'static str,
            script: &StdMutex<VecDeque<Scripted>>,
            headers: &AxumHeaders,
            body: String,
        ) -> Response<Body> {
            let cookie = headers
                .get("cookie")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            self.seen.lock().unwrap().push(Seen {
                route,
                cookie,
                body,
            });
            let scripted = script
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| json(500, r#"{"error":"unscripted"}"#));
            let mut response = Response::builder().status(scripted.status);
            for (name, value) in scripted.headers {
                response = response.header(name, value);
            }
            response
                .header("content-type", "application/json")
                .body(Body::from(scripted.body))
                .unwrap()
        }
    }

    async fn refresh_route(
        State(stub): State<Arc<Stub>>,
        headers: AxumHeaders,
        body: String,
    ) -> Response<Body> {
        stub.answer("refresh", &stub.refresh, &headers, body)
    }

    async fn approve_route(
        State(stub): State<Arc<Stub>>,
        headers: AxumHeaders,
        body: String,
    ) -> Response<Body> {
        stub.answer("approve", &stub.approve, &headers, body)
    }

    async fn devices_route(
        State(stub): State<Arc<Stub>>,
        headers: AxumHeaders,
        body: String,
    ) -> Response<Body> {
        stub.answer("devices", &stub.devices, &headers, body)
    }

    /// A stub Companion Gateway on a loopback port of its own, answering the
    /// three routes from their scripts, recording what it saw.
    async fn stub_gateway() -> (Arc<Stub>, String) {
        let stub = Arc::new(Stub::default());
        let router = Router::new()
            .route("/api/session/refresh", post(refresh_route))
            .route("/api/approvals", post(approve_route))
            .route("/api/devices", get(devices_route))
            .with_state(stub.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (stub, format!("http://127.0.0.1:{port}/"))
    }

    fn script(script: &StdMutex<VecDeque<Scripted>>, answers: impl IntoIterator<Item = Scripted>) {
        script.lock().unwrap().extend(answers);
    }

    const SUGGESTION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const APPROVAL: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

    fn approved_body(edited: bool) -> String {
        format!(
            r#"{{"event_id":"{APPROVAL}","suggestion_event_id":"{SUGGESTION}","approved_by":"@owner:example.com","persona_id":"assistant","network":"whatsapp","contact":"@whatsapp_33600000000:example.com","edited":{edited},"approved_at":"2026-09-20T10:00:00Z","publication":"published","stream_sequence":42,"published_at":"2026-09-20T10:00:00Z","posted":null}}"#
        )
    }

    #[tokio::test]
    async fn refresh_rotates_the_file_before_it_keeps_the_device_token() {
        let dir = ScratchDir::new();
        let session = dir.session("R1");
        let path = session.path().to_owned();
        let (stub, base) = stub_gateway().await;
        script(&stub.refresh, [issued("D1", "R2")]);
        let gateway = Gateway::new(&base, session);

        gateway.refresh().await.unwrap();

        let seen = stub.seen("refresh");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].cookie.as_deref(), Some("twalk_refresh=R1"));
        assert_eq!(seen[0].body, "", "a refresh carries no body");
        let contents = fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains(&format!("{SESSION_KEY}=R2\n")),
            "the file holds the rotated token: {contents:?}"
        );
        assert!(
            !contents.contains("R1"),
            "the dead token is gone: {contents:?}"
        );
        assert!(
            contents.starts_with("# the clerk's session\n"),
            "the operator's other lines survive: {contents:?}"
        );
        let held = gateway
            .device
            .lock()
            .await
            .clone()
            .expect("a device token is held");
        assert_eq!(held.token, "D1");
        assert_eq!(held.lifetime_seconds, 900);
        assert!((held.expires_at_unix - now_unix() - 900).abs() <= 2);
        assert!(!gateway.base().ends_with('/'), "{}", gateway.base());
    }

    #[tokio::test]
    async fn refresh_401_is_unauthenticated_and_leaves_the_file() {
        let dir = ScratchDir::new();
        let session = dir.session("R1");
        let path = session.path().to_owned();
        let (stub, base) = stub_gateway().await;
        script(
            &stub.refresh,
            [json(
                401,
                &format!(r#"{{"error":"unauthenticated","detail":"{MARKER}"}}"#),
            )],
        );
        let gateway = Gateway::new(&base, session);

        let err = gateway.refresh().await.unwrap_err();

        assert_eq!(err, GatewayError::Unauthenticated);
        assert!(!err.is_transient());
        assert!(!err.to_string().contains(MARKER));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("# the clerk's session\n{SESSION_KEY}=R1\n")
        );
        assert!(gateway.device.lock().await.is_none());
    }

    #[tokio::test]
    async fn approve_sends_the_device_cookie_and_the_body() {
        let dir = ScratchDir::new();
        let (stub, base) = stub_gateway().await;
        script(&stub.refresh, [issued("D1", "R2")]);
        script(&stub.approve, [json(201, &approved_body(true))]);
        let gateway = Gateway::new(&base, dir.session("R1"));

        let outcome = gateway.approve(SUGGESTION, Some("texte")).await.unwrap();

        assert_eq!(
            outcome,
            Outcome::Approved {
                event_id: APPROVAL.to_owned(),
                edited: true,
                already: false,
            }
        );
        let seen = stub.seen("approve");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].cookie.as_deref(), Some("twalk_device=D1"));
        let body: Value = serde_json::from_str(&seen[0].body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "suggestion_event_id": SUGGESTION, "final": { "body": "texte" } })
        );
        assert_eq!(
            stub.seen("refresh").len(),
            1,
            "the first call signs in once"
        );
    }

    #[tokio::test]
    async fn approve_without_final_omits_the_field() {
        let dir = ScratchDir::new();
        let (stub, base) = stub_gateway().await;
        script(&stub.refresh, [issued("D1", "R2")]);
        script(&stub.approve, [json(201, &approved_body(false))]);
        let gateway = Gateway::new(&base, dir.session("R1"));

        let outcome = gateway.approve(SUGGESTION, None).await.unwrap();

        assert_eq!(
            outcome,
            Outcome::Approved {
                event_id: APPROVAL.to_owned(),
                edited: false,
                already: false,
            }
        );
        let body: Value = serde_json::from_str(&stub.seen("approve")[0].body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "suggestion_event_id": SUGGESTION })
        );
        assert!(body.get("final").is_none());
    }

    #[tokio::test]
    async fn already_approved_is_success() {
        let dir = ScratchDir::new();
        let (stub, base) = stub_gateway().await;
        script(&stub.refresh, [issued("D1", "R2")]);
        script(
            &stub.approve,
            [json(
                409,
                &format!(
                    r#"{{"error":"already_approved","detail":"approved earlier","approval":{}}}"#,
                    approved_body(false)
                ),
            )],
        );
        let gateway = Gateway::new(&base, dir.session("R1"));

        let outcome = gateway.approve(SUGGESTION, Some("again")).await.unwrap();

        assert_eq!(
            outcome,
            Outcome::Approved {
                event_id: APPROVAL.to_owned(),
                edited: false,
                already: true,
            }
        );
    }

    #[tokio::test]
    async fn a_refusal_carries_its_code() {
        let dir = ScratchDir::new();
        let (stub, base) = stub_gateway().await;
        script(&stub.refresh, [issued("D1", "R2")]);
        script(
            &stub.approve,
            [
                json(
                    409,
                    r#"{"error":"consent_revoked","detail":"revoked at 10:00"}"#,
                ),
                json(503, r#"{"error":"approvals_not_configured"}"#),
                json(502, "upstream is gone"),
            ],
        );
        let gateway = Gateway::new(&base, dir.session("R1"));

        assert_eq!(
            gateway.approve(SUGGESTION, None).await.unwrap(),
            Outcome::Refused {
                status: 409,
                code: "consent_revoked".to_owned()
            }
        );
        assert_eq!(
            gateway.approve(SUGGESTION, None).await.unwrap(),
            Outcome::Refused {
                status: 503,
                code: "approvals_not_configured".to_owned()
            }
        );
        // A refusal with no code is not one the clerk can put a sentence to
        // — and a code-less 502 is a proxy in front of a Gateway that is not
        // there, which is worth a later try.
        let err = gateway.approve(SUGGESTION, None).await.unwrap_err();
        assert!(
            matches!(err, GatewayError::Malformed { status: 502, .. }),
            "{err}"
        );
        assert!(err.is_transient());
        assert!(!err.to_string().contains("upstream"), "{err}");
    }

    #[tokio::test]
    async fn one_401_on_approve_refreshes_once_and_retries() {
        let dir = ScratchDir::new();
        let session = dir.session("R1");
        let path = session.path().to_owned();
        let (stub, base) = stub_gateway().await;
        script(&stub.refresh, [issued("D1", "R2"), issued("D2", "R3")]);
        script(
            &stub.approve,
            [
                json(401, r#"{"error":"unauthenticated"}"#),
                json(201, &approved_body(false)),
            ],
        );
        let gateway = Gateway::new(&base, session);
        // Signed in, holding D1 — the 401 that follows is D1 dying under it.
        gateway.refresh().await.unwrap();
        assert_eq!(stub.seen("refresh").len(), 1);

        let outcome = gateway.approve(SUGGESTION, None).await.unwrap();

        assert!(matches!(outcome, Outcome::Approved { already: false, .. }));
        let approves = stub.seen("approve");
        assert_eq!(approves.len(), 2, "one retry, exactly");
        assert_eq!(approves[0].cookie.as_deref(), Some("twalk_device=D1"));
        assert_eq!(approves[1].cookie.as_deref(), Some("twalk_device=D2"));
        let refreshes = stub.seen("refresh");
        assert_eq!(refreshes.len(), 2, "one refresh for the 401, exactly");
        assert_eq!(refreshes[1].cookie.as_deref(), Some("twalk_refresh=R2"));
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains(&format!("{SESSION_KEY}=R3\n")));
    }

    #[tokio::test]
    async fn two_401s_are_unauthenticated() {
        let dir = ScratchDir::new();
        let (stub, base) = stub_gateway().await;
        script(&stub.refresh, [issued("D1", "R2"), issued("D2", "R3")]);
        script(
            &stub.approve,
            [
                json(401, r#"{"error":"unauthenticated"}"#),
                json(401, r#"{"error":"unauthenticated"}"#),
                json(201, &approved_body(false)),
            ],
        );
        let gateway = Gateway::new(&base, dir.session("R1"));

        let err = gateway.approve(SUGGESTION, None).await.unwrap_err();

        assert_eq!(err, GatewayError::Unauthenticated);
        assert_eq!(stub.seen("approve").len(), 2, "no third try");
        assert_eq!(stub.seen("refresh").len(), 2);
        assert_eq!(
            stub.approve.lock().unwrap().len(),
            1,
            "the 201 the stub would have given a third try is still queued"
        );
    }

    #[tokio::test]
    async fn a_fresh_device_token_is_reused_and_a_stale_one_is_not() {
        let dir = ScratchDir::new();
        let (stub, base) = stub_gateway().await;
        script(&stub.refresh, [issued("D1", "R2"), issued("D2", "R3")]);
        script(
            &stub.approve,
            [
                json(201, &approved_body(false)),
                json(201, &approved_body(false)),
                json(201, &approved_body(false)),
            ],
        );
        let gateway = Gateway::new(&base, dir.session("R1"));

        gateway.approve(SUGGESTION, None).await.unwrap();
        gateway.approve(SUGGESTION, None).await.unwrap();
        assert_eq!(stub.seen("refresh").len(), 1, "a fresh token is reused");

        // Less than a fifth of its life left: refreshed before it is sent.
        gateway
            .device
            .lock()
            .await
            .as_mut()
            .unwrap()
            .expires_at_unix = now_unix() + 900 / 5 - 1;
        gateway.approve(SUGGESTION, None).await.unwrap();
        assert_eq!(stub.seen("refresh").len(), 2);
        assert_eq!(
            stub.seen("approve")[2].cookie.as_deref(),
            Some("twalk_device=D2")
        );
    }

    #[tokio::test]
    async fn devices_are_listed_as_the_device() {
        let dir = ScratchDir::new();
        let (stub, base) = stub_gateway().await;
        script(&stub.refresh, [issued("D1", "R2")]);
        script(
            &stub.devices,
            [json(
                200,
                r#"{"devices":[{"id":"dev-1","name":"Pixel 8","created_unix_seconds":1,"last_seen_unix_seconds":2,"revoked_unix_seconds":null,"current":false},{"id":"dev-2","name":"Buzz","created_unix_seconds":3,"last_seen_unix_seconds":4,"revoked_unix_seconds":null,"current":true}]}"#,
            )],
        );
        let gateway = Gateway::new(&base, dir.session("R1"));

        let devices = gateway.devices().await.unwrap();

        assert_eq!(devices.len(), 2);
        assert_eq!(devices[1].name, "Buzz");
        assert!(devices[1].current);
        assert_eq!(devices[0].revoked_unix_seconds, None);
        assert_eq!(
            stub.seen("devices")[0].cookie.as_deref(),
            Some("twalk_device=D1")
        );
    }

    #[tokio::test]
    async fn unreachable_is_transient() {
        let dir = ScratchDir::new();
        // A port nothing listens on: bound, then released.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let gateway = Gateway::new(&format!("http://127.0.0.1:{port}"), dir.session("R1"));

        let err = gateway.approve(SUGGESTION, None).await.unwrap_err();

        assert!(matches!(err, GatewayError::Unreachable(_)), "{err}");
        assert!(err.is_transient());
        assert!(!err.to_string().contains("R1"), "{err}");
    }

    #[test]
    fn session_file_refuses_a_loose_mode() {
        let dir = ScratchDir::new();
        for mode in [0o640, 0o604, 0o644, 0o660] {
            let path = dir.0.join(format!("open-{mode:o}.env"));
            fs::write(&path, format!("{SESSION_KEY}=R1\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            let err = SessionFile::open(&path).unwrap_err();
            assert!(matches!(err, GatewayError::Session(_)));
            let text = err.to_string();
            assert!(
                text.contains(&format!("chmod 0600 {}", path.display())),
                "{mode:o}: {text}"
            );
            assert!(!text.contains("R1"), "{text}");
        }
        let missing = dir.0.join("missing.env");
        let err = SessionFile::open(&missing).unwrap_err();
        assert!(matches!(err, GatewayError::Session(_)), "{err}");

        let empty = dir.0.join("empty.env");
        fs::write(&empty, "# nothing here\n").unwrap();
        fs::set_permissions(&empty, fs::Permissions::from_mode(0o600)).unwrap();
        let err = SessionFile::open(&empty).unwrap().read().unwrap_err();
        assert!(
            err.to_string()
                .contains(&format!("holds no {SESSION_KEY}=<token> line")),
            "{err}"
        );
    }

    #[test]
    fn session_file_reads_the_env_shapes() {
        let dir = ScratchDir::new();
        for (contents, want) in [
            (format!("{SESSION_KEY}=R1\n"), "R1"),
            (format!("export {SESSION_KEY}=\"R2\"\n"), "R2"),
            (
                format!("# comment\nOTHER=x\n  {SESSION_KEY} = 'R3'  \n"),
                "R3",
            ),
        ] {
            let path = dir.0.join(format!("{want}.env"));
            fs::write(&path, &contents).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(
                SessionFile::open(&path).unwrap().read().unwrap(),
                want,
                "{contents:?}"
            );
        }
    }

    #[test]
    fn session_file_write_is_atomic_and_0600() {
        let dir = ScratchDir::new();
        let session = dir.session("R1");
        let path = session.path().to_owned();
        // A loose mode on the original does not survive a rewrite either.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        session.write("R2").unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600, "{mode:04o}");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("# the clerk's session\n{SESSION_KEY}=R2\n")
        );
        assert_eq!(session.read().unwrap(), "R2");
        let left: Vec<_> = fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(
            left,
            vec!["session.env"],
            "no temporary file is left behind"
        );

        // A file with no token line gains one; a rewrite is still one line.
        let fresh = dir.0.join("fresh.env");
        fs::write(&fresh, "").unwrap();
        fs::set_permissions(&fresh, fs::Permissions::from_mode(0o600)).unwrap();
        let fresh = SessionFile::open(&fresh).unwrap();
        fresh.write("R9").unwrap();
        fresh.write("R10").unwrap();
        assert_eq!(
            fs::read_to_string(fresh.path()).unwrap(),
            format!("{SESSION_KEY}=R10\n")
        );

        // A directory that cannot take the temporary file is a `Session`
        // error naming the path, not a panic and not a truncated original.
        let nowhere = SessionFile {
            path: dir.0.join("no-such-dir").join("session.env"),
        };
        let err = nowhere.write("R3").unwrap_err();
        assert!(matches!(err, GatewayError::Session(_)), "{err}");
        assert!(!err.to_string().contains("R3"), "{err}");
    }

    #[tokio::test]
    async fn no_error_display_carries_a_token() {
        let dir = ScratchDir::new();
        let (stub, base) = stub_gateway().await;
        let tainted = |status: u16| {
            json(
                status,
                &format!("{{\"error\":\"x\",\"detail\":\"{MARKER}\"}}"),
            )
        };
        // A refresh whose cookies and body are all marker: the Set-Cookie
        // values are tokens, and the body is not a session.
        script(
            &stub.refresh,
            [
                tainted(401),
                // A sentence in `error` is not a code, and is not kept.
                json(500, &format!(r#"{{"error":"store_failed {MARKER}"}}"#)),
                Scripted {
                    status: 200,
                    headers: vec![("set-cookie", format!("{DEVICE_COOKIE}={MARKER}-D"))],
                    body: format!("{{\"expires_in\":900,\"x\":\"{MARKER}\"}}"),
                },
                Scripted {
                    status: 200,
                    headers: vec![
                        ("set-cookie", format!("{DEVICE_COOKIE}={MARKER}-D; Path=/")),
                        (
                            "set-cookie",
                            format!("{REFRESH_COOKIE}={MARKER}-R; Path=/api/session"),
                        ),
                    ],
                    body: format!("not json {MARKER}"),
                },
                issued(&format!("{MARKER}-D1"), &format!("{MARKER}-R2")),
                issued(&format!("{MARKER}-D2"), &format!("{MARKER}-R3")),
            ],
        );
        script(
            &stub.approve,
            [
                json(201, &format!("{{\"event_id\":\"{MARKER}\"}}")),
                json(
                    409,
                    &format!(r#"{{"error":"already_approved","detail":"{MARKER}"}}"#),
                ),
                tainted(500),
                tainted(401),
                tainted(401),
            ],
        );
        let gateway = Gateway::new(&base, dir.session(&format!("{MARKER}-R1")));
        let mut errors: Vec<GatewayError> = Vec::new();

        errors.push(gateway.refresh().await.unwrap_err()); // 401
        errors.push(gateway.refresh().await.unwrap_err()); // 500, code tainted
        errors.push(gateway.refresh().await.unwrap_err()); // no refresh cookie
        errors.push(gateway.refresh().await.unwrap_err()); // body not JSON
        errors.push(gateway.approve(SUGGESTION, None).await.unwrap_err()); // 201, no `edited`
        errors.push(gateway.approve(SUGGESTION, None).await.unwrap_err()); // 409, no record
                                                                           // A refusal's code is what the clerk keeps; a marker in `detail` is not.
        let refused = gateway.approve(SUGGESTION, None).await.unwrap();
        assert_eq!(
            refused,
            Outcome::Refused {
                status: 500,
                code: "x".to_owned()
            }
        );
        errors.push(gateway.approve(SUGGESTION, None).await.unwrap_err()); // 401, 401
        let loose = dir.0.join("loose.env");
        fs::write(&loose, format!("{SESSION_KEY}={MARKER}\n")).unwrap();
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o644)).unwrap();
        errors.push(SessionFile::open(&loose).unwrap_err());

        assert_eq!(errors.len(), 8);
        for err in &errors {
            let text = format!("{err}");
            assert!(!text.contains(MARKER), "{err:?}");
            assert!(!format!("{err:?}").contains(MARKER), "{err:?}");
        }
        let kinds: Vec<_> = errors
            .iter()
            .map(|e| match e {
                GatewayError::Unreachable(_) => "unreachable",
                GatewayError::Unauthenticated => "unauthenticated",
                GatewayError::Malformed { .. } => "malformed",
                GatewayError::Session(_) => "session",
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "unauthenticated",
                "malformed",
                "malformed",
                "malformed",
                "malformed",
                "malformed",
                "unauthenticated",
                "session",
            ]
        );
    }

    #[test]
    fn cookies_reads_every_set_cookie_and_drops_the_attributes() {
        let mut headers = HeaderMap::new();
        headers.append(
            SET_COOKIE,
            "twalk_device=D1; Max-Age=900; Path=/; HttpOnly; SameSite=Lax"
                .parse()
                .unwrap(),
        );
        headers.append(
            SET_COOKIE,
            "twalk_refresh=R2; Path=/api/session; HttpOnly"
                .parse()
                .unwrap(),
        );
        headers.append(SET_COOKIE, "junk".parse().unwrap());
        assert_eq!(
            cookies(&headers),
            vec![
                ("twalk_device".to_owned(), "D1".to_owned()),
                ("twalk_refresh".to_owned(), "R2".to_owned()),
            ]
        );
        assert!(cookies(&HeaderMap::new()).is_empty());
    }

    #[test]
    fn transient_is_unreachable_or_a_gateway_that_cannot_serve_now() {
        let malformed = |status: u16| GatewayError::Malformed {
            status,
            why: String::new(),
        };
        assert!(GatewayError::Unreachable("connection refused".into()).is_transient());
        // A Gateway that is up and cannot serve now: the same line the
        // relay client draws.
        assert!(malformed(429).is_transient());
        assert!(malformed(500).is_transient());
        assert!(malformed(502).is_transient());
        assert!(malformed(503).is_transient());
        assert!(malformed(599).is_transient());
        // An answer whose shape was wrong will be wrong again.
        assert!(!malformed(200).is_transient());
        assert!(!malformed(201).is_transient());
        assert!(!malformed(400).is_transient());
        assert!(!malformed(404).is_transient());
        assert!(!malformed(409).is_transient());
        assert!(!malformed(600).is_transient());
        assert!(!GatewayError::Unauthenticated.is_transient());
        assert!(!GatewayError::Session(String::new()).is_transient());
    }
}
