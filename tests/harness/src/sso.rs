//! A fake SSO, and the two services a grant from it opens (issue #274, ADR
//! 0033): what the collector's `oidc` module and its `authorize` command are
//! tested against, so that no test needs `sso.linagora.com`, a real client
//! secret, or a browser.
//!
//! In Rust and in the shape of the Sensor's stub Companion Gateway
//! (`sensor/tests/harness/gateway.rs`): one loopback listener, one request
//! per connection, a hand-rolled request reader — because the thing under
//! test is an HTTP client, and a fake built on the same client library would
//! agree with it for the wrong reasons.
//!
//! What it fakes, and only that:
//!
//! - **discovery** — `/.well-known/openid-configuration` naming the
//!   authorization and token endpoints;
//! - **the authorization code grant with PKCE** — the browser half is the
//!   test's: [`FakeSso::sign_in`] plays the user, checking the S256
//!   challenge the collector put in the link and handing back the callback
//!   URL the operator would paste;
//! - **the token endpoint** — a code exchanged once, a refresh token that
//!   **rotates** on every renewal (the old one dies), a client secret
//!   checked, and a grant the test can revoke, after which renewal answers
//!   `400 invalid_grant`;
//! - **two services behind the grant** — a JMAP session (`/jmap/session`,
//!   `username`) and the calendar side service (`/api/user`, `email`), each
//!   accepting a live access token and each one the test can make refuse a
//!   fresh token with `403`, which is the `pending_operator` case: the grant
//!   stands, the service wants an audience the client does not have.
//!
//! Every token the fake issues is random and never a secret worth
//! protecting; what the tests assert is that the collector never prints one.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The client the fake knows, as the deployment would configure it.
pub const CLIENT_ID: &str = "twalk-collector-test";
pub const CLIENT_SECRET: &str = "test-only-client-secret";
/// How long an access token lives, in seconds: an hour, what a real SSO
/// issues. A test that wants a renewal asks for one (a stale token, a
/// revocation) rather than waiting.
pub const ACCESS_TOKEN_SECONDS: u64 = 3600;

/// Writes `CLIENT_SECRET` into `dir/client-secret` at mode 0600 — the shape
/// the collector accepts a secret file in — and returns the path.
pub fn write_client_secret(dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("client-secret");
    std::fs::write(&path, format!("{CLIENT_SECRET}\n"))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(path)
}

#[derive(Default)]
struct State {
    /// The account the grant is for: what both `whoami`s answer.
    account: String,
    /// Codes issued by `sign_in`, each bound to its PKCE challenge and the
    /// redirect URI the link named; spent when exchanged.
    codes: HashMap<String, PendingCode>,
    /// Live access tokens.
    access_tokens: HashMap<String, ()>,
    /// The one live refresh token, or `None` before any grant or after a
    /// revocation. Rotation replaces it: the previous value is dead.
    refresh_token: Option<String>,
    revoked: bool,
    /// Services told to refuse a fresh token with 403 (`pending_operator`).
    refusing: Vec<&'static str>,
    /// Services told not to answer at all (`unreachable`): the connection is
    /// accepted and closed.
    silent: Vec<&'static str>,
    /// The listener's port: the issuer discovery names has to be this fake.
    port: u16,
    /// What happened, for the assertions: every token request's grant type,
    /// every refresh token presented.
    token_requests: Vec<String>,
    refresh_tokens_presented: Vec<String>,
    counter: u64,
}

struct PendingCode {
    challenge: String,
    redirect_uri: String,
}

pub struct FakeSso {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl FakeSso {
    /// Starts the fake on an ephemeral loopback port, holding a grant for
    /// `account` — the email both services answer as the signed-in user.
    pub async fn start(account: &str) -> Result<Self> {
        let listener = TcpListener::bind(format!("{}:0", Ipv4Addr::LOCALHOST))
            .await
            .context("failed to bind the fake SSO")?;
        let addr = listener.local_addr()?;
        let state = Arc::new(Mutex::new(State {
            account: account.to_owned(),
            port: addr.port(),
            ..State::default()
        }));
        let accept_task = tokio::spawn(accept_loop(listener, state.clone()));
        Ok(Self {
            addr,
            state,
            accept_task,
        })
    }

    /// The issuer: what `COLLECTOR_OIDC_ISSUER` names, and where discovery is.
    pub fn issuer(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The JMAP session URL, as `COLLECTOR_JMAP_SESSION_URL` names it.
    pub fn jmap_session_url(&self) -> String {
        format!("http://{}/jmap/session", self.addr)
    }

    /// The calendar side service's root, as `COLLECTOR_CALDAV_URL` names it.
    pub fn caldav_url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    /// The user's half of the authorization code flow: given the link the
    /// collector printed, signs in and returns the callback URL the operator
    /// pastes. Refuses a link with no S256 challenge or no `state` — the two
    /// things the collector must never leave out.
    pub fn sign_in(&self, authorization_url: &str) -> Result<String> {
        let (_, query) = authorization_url
            .split_once('?')
            .context("the authorization link has no query")?;
        let params = parse_form(query);
        anyhow::ensure!(
            params.get("response_type").map(String::as_str) == Some("code"),
            "response_type must be code: {authorization_url}"
        );
        anyhow::ensure!(
            params.get("client_id").map(String::as_str) == Some(CLIENT_ID),
            "the link names another client: {authorization_url}"
        );
        anyhow::ensure!(
            params.get("code_challenge_method").map(String::as_str) == Some("S256"),
            "PKCE must be S256: {authorization_url}"
        );
        let challenge = params
            .get("code_challenge")
            .filter(|c| !c.is_empty())
            .context("the link carries no code_challenge")?;
        let state = params
            .get("state")
            .filter(|s| !s.is_empty())
            .context("the link carries no state")?;
        let redirect_uri = params
            .get("redirect_uri")
            .context("the link names no redirect_uri")?;
        anyhow::ensure!(
            params
                .get("scope")
                .is_some_and(|scope| scope.split(' ').any(|s| s == "offline_access")),
            "the link does not ask for offline_access, so no refresh token would be issued: {authorization_url}"
        );
        let code = {
            let mut guard = self.lock();
            guard.counter += 1;
            let code = format!("code-{}", guard.counter);
            guard.codes.insert(
                code.clone(),
                PendingCode {
                    challenge: challenge.clone(),
                    redirect_uri: redirect_uri.clone(),
                },
            );
            code
        };
        Ok(format!("{redirect_uri}?code={code}&state={state}"))
    }

    /// Kills the grant, as an SSO's revocation does: the access tokens it
    /// issued die with it, so a service answers 401 to the next request, and
    /// the renewal that must follow answers `invalid_grant` — which is
    /// `reconnect_required`.
    pub fn revoke(&self) {
        let mut guard = self.lock();
        guard.revoked = true;
        guard.refresh_token = None;
        guard.access_tokens.clear();
    }

    /// Makes one service (`"jmap"` or `"caldav"`) refuse every token with
    /// 403: the grant is fine, the service wants something else.
    pub fn refuse(&self, service: &'static str) {
        self.lock().refusing.push(service);
    }

    /// Makes one service stop answering: `unreachable`, not a refusal.
    pub fn silence(&self, service: &'static str) {
        self.lock().silent.push(service);
    }

    /// Restores a service's answers.
    pub fn restore(&self, service: &'static str) {
        let mut guard = self.lock();
        guard.refusing.retain(|s| *s != service);
        guard.silent.retain(|s| *s != service);
    }

    /// The live refresh token, for a test that asserts what is on disk is
    /// what the SSO holds.
    pub fn current_refresh_token(&self) -> Option<String> {
        self.lock().refresh_token.clone()
    }

    /// Every refresh token the collector presented, in order.
    pub fn refresh_tokens_presented(&self) -> Vec<String> {
        self.lock().refresh_tokens_presented.clone()
    }

    /// Every token request's grant type, in order.
    pub fn token_requests(&self) -> Vec<String> {
        self.lock().token_requests.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("the fake SSO is not poisoned")
    }
}

impl Drop for FakeSso {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

async fn accept_loop(listener: TcpListener, state: Arc<Mutex<State>>) {
    while let Ok((stream, _)) = listener.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            let _ = serve_connection(stream, state).await;
        });
    }
}

struct RawRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    body: String,
}

async fn serve_connection(mut stream: TcpStream, state: Arc<Mutex<State>>) -> Result<()> {
    let Some(request) = read_request(&mut stream).await? else {
        return Ok(());
    };
    let Some((status, body)) = respond(&request, &state) else {
        // A silenced service: the connection closes with nothing said.
        return Ok(());
    };
    let body = serde_json::to_vec(&body)?;
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}

/// Reads one HTTP/1.1 request, head and — when `content-length` says so —
/// body: the token endpoint takes a form.
async fn read_request(stream: &mut TcpStream) -> Result<Option<RawRequest>> {
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break position;
        }
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_owned();
    let path = request_line.next().unwrap_or_default().to_owned();
    let mut authorization = None;
    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_owned());
            } else if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Ok(Some(RawRequest {
        method,
        path,
        authorization,
        body: String::from_utf8_lossy(&body).into_owned(),
    }))
}

/// Routes one request. `None` is a silenced service.
fn respond(request: &RawRequest, state: &Arc<Mutex<State>>) -> Option<(&'static str, Value)> {
    let path = request.path.split('?').next().unwrap_or_default();
    let mut guard = state.lock().expect("the fake SSO is not poisoned");
    match (request.method.as_str(), path) {
        ("GET", "/.well-known/openid-configuration") => {
            let issuer = format!("http://127.0.0.1:{}", guard.port);
            Some((
                "200 OK",
                json!({
                    "issuer": issuer,
                    "authorization_endpoint": format!("{issuer}/authorize"),
                    "token_endpoint": format!("{issuer}/token"),
                    "code_challenge_methods_supported": ["S256"],
                }),
            ))
        }
        ("POST", "/token") => Some(token(&request.body, &mut guard)),
        ("GET", "/jmap/session") => service(
            "jmap",
            request,
            &mut guard,
            |account| json!({ "username": account, "apiUrl": "http://jmap.invalid/api" }),
        ),
        ("GET", "/api/user") => service(
            "caldav",
            request,
            &mut guard,
            |account| json!({ "email": account, "id": "user-1" }),
        ),
        _ => Some((
            "404 Not Found",
            json!({ "error": "not_found", "detail": format!("the fake SSO does not serve {path}") }),
        )),
    }
}

/// The token endpoint: two grant types, one client, rotating refresh tokens.
fn token(body: &str, guard: &mut State) -> (&'static str, Value) {
    let form = parse_form(body);
    if form.get("client_id").map(String::as_str) != Some(CLIENT_ID)
        || form.get("client_secret").map(String::as_str) != Some(CLIENT_SECRET)
    {
        return (
            "401 Unauthorized",
            json!({ "error": "invalid_client", "error_description": "unknown client or wrong secret" }),
        );
    }
    let grant_type = form.get("grant_type").cloned().unwrap_or_default();
    guard.token_requests.push(grant_type.clone());
    match grant_type.as_str() {
        "authorization_code" => {
            let code = form.get("code").cloned().unwrap_or_default();
            let Some(pending) = guard.codes.remove(&code) else {
                return (
                    "400 Bad Request",
                    json!({ "error": "invalid_grant", "error_description": "unknown or spent code" }),
                );
            };
            let verifier = form.get("code_verifier").cloned().unwrap_or_default();
            if s256(&verifier) != pending.challenge {
                return (
                    "400 Bad Request",
                    json!({ "error": "invalid_grant", "error_description": "the code_verifier does not match the challenge" }),
                );
            }
            if form.get("redirect_uri") != Some(&pending.redirect_uri) {
                return (
                    "400 Bad Request",
                    json!({ "error": "invalid_grant", "error_description": "redirect_uri differs from the one the code was issued for" }),
                );
            }
            guard.revoked = false;
            issue(guard)
        }
        "refresh_token" => {
            let presented = form.get("refresh_token").cloned().unwrap_or_default();
            guard.refresh_tokens_presented.push(presented.clone());
            if guard.revoked || guard.refresh_token.as_deref() != Some(presented.as_str()) {
                return (
                    "400 Bad Request",
                    json!({ "error": "invalid_grant", "error_description": "the refresh token is revoked, expired, or was rotated away" }),
                );
            }
            issue(guard)
        }
        other => (
            "400 Bad Request",
            json!({ "error": "unsupported_grant_type", "error_description": format!("{other:?}") }),
        ),
    }
}

/// A fresh access token and a fresh refresh token; the previous refresh
/// token is dead from here on.
fn issue(guard: &mut State) -> (&'static str, Value) {
    guard.counter += 1;
    let access = format!("access-{}", guard.counter);
    guard.counter += 1;
    let refresh = format!("refresh-{}", guard.counter);
    guard.access_tokens.insert(access.clone(), ());
    guard.refresh_token = Some(refresh.clone());
    (
        "200 OK",
        json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": ACCESS_TOKEN_SECONDS,
            "refresh_token": refresh,
            "scope": "openid email offline_access",
        }),
    )
}

/// A service behind the grant: a live bearer answers the account, a stale
/// one is refused with 401, and a service the test told to refuse answers
/// 403 whatever the token — the shape of an audience the client lacks.
fn service(
    name: &'static str,
    request: &RawRequest,
    guard: &mut State,
    document: impl Fn(&str) -> Value,
) -> Option<(&'static str, Value)> {
    if guard.silent.contains(&name) {
        return None;
    }
    let bearer = request
        .authorization
        .as_deref()
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim().to_owned());
    if guard.refusing.contains(&name) {
        return Some((
            "403 Forbidden",
            json!({ "error": "forbidden", "detail": format!("{name} wants an audience this token does not carry") }),
        ));
    }
    match bearer {
        Some(token) if guard.access_tokens.contains_key(&token) => {
            let account = guard.account.clone();
            Some(("200 OK", document(&account)))
        }
        _ => Some((
            "401 Unauthorized",
            json!({ "error": "unauthenticated", "detail": "no live access token" }),
        )),
    }
}

/// `application/x-www-form-urlencoded`, decoded.
fn parse_form(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            Some((percent_decode(key), percent_decode(value)))
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

/// PKCE S256: base64url, no padding, of the SHA-256 of the verifier.
fn s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64url(&digest)
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
