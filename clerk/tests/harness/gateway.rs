//! A stub Companion Gateway for the clerk's write half (#284): the session
//! of the owner's `Buzz` device, `POST /api/approvals` and — since #300 —
//! `GET /api/suggestions/{id}`, the read the clerk makes before a post.
//!
//! The clerk's suite runs at its process boundary — the real binary, a real
//! Buzz relay, the real bus — and the Companion Gateway is the one thing it
//! cannot bring up cheaply: a real one needs a homeserver, a state
//! directory, an owner and a signed-in device, and what the clerk needs
//! from it is four routes. So this is a stub of exactly those four, on the
//! same terms as `hermes/tests/harness/gateway.rs` is a stub of the runtime
//! settings and `sensor/tests/harness/gateway.rs` of the consent snapshot.
//!
//! Three things it does **not** stub away, because they are what the suite
//! is about:
//!
//! - **the session.** `POST /api/session/refresh` is authenticated by the
//!   `twalk_refresh` cookie alone and **rotates both tokens** on every
//!   call, exactly as the Gateway does (`companion-gateway/src/session.rs`):
//!   the refresh token the file presented dies the moment the answer is
//!   sent, and presenting it again is `401 unauthenticated`. A clerk that
//!   kept the device token and lost the rotated refresh token would pass a
//!   stub that never rotated and be signed out for good on the reference
//!   deployment.
//! - **the device token.** Every other route reads `twalk_device` from the
//!   `Cookie` header and nowhere else, and only the **current** one — the
//!   last issued — is accepted; a revoked, unknown or stale token is
//!   answered identically, `401 unauthenticated`, so a probe learns nothing.
//!   [`State::revoked`] answers that to everything, refresh included: the
//!   device revoked from the dashboard.
//! - **the shapes.** The `IssuedSession` and `Approval` bodies are the
//!   Gateway's own, field for field (`companion-gateway/openapi.yaml`,
//!   `session_http.rs`, `approval_http.rs`), copied here rather than
//!   imported, because a fixture that took the Gateway's own constructor
//!   would agree with it whatever it became. The cookie names are copied
//!   from `clerk/src/gateway.rs` for the same reason in the other
//!   direction.
//!
//! What it records is what a test asserts: every authenticated
//! `POST /api/approvals` ([`ApprovalCall`]: the suggestion, the `final`
//! body if one was sent, the device token that made the call, when), every
//! authenticated `GET /api/suggestions/{id}` ([`State::reads`], the ids in
//! order), how many refreshes happened, and how many requests were turned
//! away at the door. The answer to an approval is scripted per suggestion
//! id ([`State::answers`], default [`Answer::Approve`]); the answer to a
//! read too ([`State::suggestions`], a [`SuggestionAnswer`]), and an id
//! with none is `404 suggestion_not_found`.
//!
//! **Unreachable** is not an [`Answer`]: a Gateway that is not there is a
//! port nothing listens on, [`UNREACHABLE_GATEWAY_URL`], reserved outside
//! the range a stub may take so that a parallel test cannot claim it between
//! one test's drop and another's connect and make "unreachable" mean
//! "answered somebody else's stub".

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::State as Shared;
use axum::http::{HeaderMap, Response, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use tokio::task::JoinHandle;
use twalk_test_harness::sha256_hex;

use super::events::now_rfc3339;

/// The ports a stub Gateway may bind: this suite's own band, one per test
/// (`decisions.rs` takes `17400 + n`), so that the tests of one binary run
/// in parallel without sharing a stub. 17400–17499 because it is the one
/// hundred nothing in the repository claims: the Sensor's suites have
/// 17200–17399, the Hermes stub Gateway 17500–17699, #172's stacks
/// 17700–17899 (the clerk's own relay is 17800), and 17900–18099 is
/// #175's bridges block — `twalk-bridges-test` publishes its telegram
/// bridge on 17906 on any host that has run
/// `sensor/tests/bridges_deployment.rs`, which is where this band lived
/// first and failed a test with `Address already in use`.
pub const PORT_RANGE: std::ops::Range<u16> = 17400..17499;

/// The last port of the band, deliberately outside [`PORT_RANGE`] and
/// therefore bound by no stub: the address of a Companion Gateway that is
/// not there. `CLERK_GATEWAY_URL` for the test that watches the clerk retry.
pub const UNREACHABLE_GATEWAY_URL: &str = "http://127.0.0.1:17499";

/// The Companion Gateway's device-token cookie (`clerk/src/gateway.rs`,
/// `DEVICE_COOKIE`), spelled again here so a rename on either side fails
/// this suite.
pub const DEVICE_COOKIE: &str = "twalk_device";
/// The Companion Gateway's refresh-token cookie (`REFRESH_COOKIE`).
pub const REFRESH_COOKIE: &str = "twalk_refresh";

/// The refresh token the stub starts with — what `provision-clerk-device.sh`
/// would have written into the session file.
pub const INITIAL_REFRESH_TOKEN: &str = "R0";
/// The device token lifetime the stub grants, in seconds: the Gateway's own
/// default (`DEFAULT_DEVICE_TOKEN_TTL_SECONDS`, fifteen minutes).
pub const DEVICE_TTL: u64 = 900;

/// The deployment's owner, as the stub's `IssuedSession` and `Approval`
/// name them: every approval is stamped `approved_by` this, whatever device
/// made the call, because that is what the Gateway does (`approval_http.rs`:
/// "who approved is the deployment's owner, not the device").
pub const OWNER_MATRIX_ID: &str = "@owner:example.test";
/// The homeserver the stub's session names.
pub const HOMESERVER: &str = "https://synapse.example.test";
/// The one device the stub lists: the clerk's own, as the operator's script
/// names it.
pub const DEVICE_ID: &str = "buzz-1";
pub const DEVICE_NAME: &str = "Buzz";

/// The persona and the network the stub's `Approval` names.
pub const PERSONA_ID: &str = "assistant";
pub const NETWORK: &str = "whatsapp";
/// The contact the stub's `Approval` names — a Matrix user ID a real
/// Gateway's `201` carries, and one the clerk must write onto **no**
/// channel: distinctive, so a test can search every event of every channel
/// for it.
pub const CONTACT: &str = "@whatsapp_33600000000:example.test";
/// The persona's words as the stub's `Suggestion` carries them (#300) —
/// distinctive, and **not** the bus's, so a post that quoted the Gateway's
/// body rather than the event's would show it.
pub const GATEWAY_SUGGESTION_BODY: &str = "MARQUEUR-corps-servi-par-la-passerelle-9c1d";

/// The `event_id` the stub's `Approval` carries for `suggestion_id`: the
/// Gateway's own rule, `sha256(suggestion_event_id ":" approved_by)` as 64
/// hex characters (`companion-gateway/src/approval.rs`).
pub fn approval_event_id(suggestion_id: &str) -> String {
    sha256_hex(&format!("{suggestion_id}:{OWNER_MATRIX_ID}"))
}

/// What the stub answers one `GET /api/suggestions/{id}` with, by
/// suggestion id (#300): the `Suggestion` shape's `standing` and its
/// `delivery`'s `reach` and `detail`, kept as strings so a test can script
/// a value the clerk has never met. An id with no answer scripted is
/// `404 suggestion_not_found`, which is what the Gateway says of one the
/// bus does not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestionAnswer {
    /// `approvable`, `expired` or `approved`.
    pub standing: String,
    /// `can_reach`, `cannot_reach` or `unknown`.
    pub reach: String,
    /// The word behind the reach: `owner_joined`, `owner_invited`, …
    pub detail: String,
}

impl SuggestionAnswer {
    pub fn new(standing: &str, reach: &str, detail: &str) -> Self {
        Self {
            standing: standing.to_owned(),
            reach: reach.to_owned(),
            detail: detail.to_owned(),
        }
    }
}

/// What the stub answers one `POST /api/approvals` with, by suggestion id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// `201` with the `Approval` record — the reply went out.
    Approve,
    /// `status` with `{"error": code, "detail": …}`: one of the Gateway's
    /// own refusals (`companion-gateway/openapi.yaml`, `POST /api/approvals`).
    Refuse { status: u16, code: String },
    /// `409 already_approved`, carrying the earlier record under `approval`
    /// (`edited: false`): the Gateway's normal answer to a duplicate.
    AlreadyApproved,
}

/// One authenticated `POST /api/approvals` the stub saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalCall {
    /// `suggestion_event_id` in the body.
    pub suggestion_id: String,
    /// `final.body` in the body, if a `final` was sent: the owner's own
    /// text; `None` sends the persona's words verbatim.
    pub final_body: Option<String>,
    /// The `twalk_device` cookie the call carried.
    pub device_token: String,
    /// When, in unix seconds.
    pub at_unix: u64,
}

/// What the stub holds and what it was asked. Every field is the test's
/// to read and to set through [`StubGateway::state`].
#[derive(Debug)]
pub struct State {
    /// The refresh token the file must present; rotated on every refresh
    /// (`R<n>`).
    pub refresh_token: String,
    /// Every device token issued, in order (`D<n>`); the current one — the
    /// only one accepted — is the last.
    pub device_tokens: Vec<String>,
    /// `expires_in` on every issued session.
    pub device_ttl: u64,
    /// The device was revoked from the dashboard: `401 unauthenticated` to
    /// everything, the refresh included.
    pub revoked: bool,
    /// The answer to an approval, by suggestion id; [`Answer::Approve`] for
    /// an id not named.
    pub answers: HashMap<String, Answer>,
    /// Every `POST /api/approvals` that reached the route — that is, one
    /// whose device token was the current one. A call turned away at the
    /// door is counted in `unauthenticated` instead, because the Gateway's
    /// approval handler never sees one either.
    pub approvals: Vec<ApprovalCall>,
    /// The answer to `GET /api/suggestions/{id}`, by suggestion id; an id
    /// not named is `404 suggestion_not_found` (#300).
    pub suggestions: HashMap<String, SuggestionAnswer>,
    /// Every `GET /api/suggestions/{id}` that reached the route, as the
    /// ids read, in order — one per read, so "read once" is
    /// `reads.iter().filter(|read| *read == id).count() == 1`. A read
    /// turned away at the door is counted in `unauthenticated`, as an
    /// approval is.
    pub reads: Vec<String>,
    /// How many refreshes succeeded.
    pub refreshes: u32,
    /// How many requests, on any route, were answered `401`.
    pub unauthenticated: u32,
    /// How many tokens `reissue` minted (`P<n>`), so each is new.
    pub reissued: u32,
}

impl State {
    fn new() -> Self {
        Self {
            refresh_token: INITIAL_REFRESH_TOKEN.to_owned(),
            device_tokens: Vec::new(),
            device_ttl: DEVICE_TTL,
            revoked: false,
            answers: HashMap::new(),
            approvals: Vec::new(),
            suggestions: HashMap::new(),
            reads: Vec::new(),
            refreshes: 0,
            unauthenticated: 0,
            reissued: 0,
        }
    }

    /// The device token currently accepted, if a refresh ever issued one.
    pub fn current_device_token(&self) -> Option<&str> {
        self.device_tokens.last().map(String::as_str)
    }

    /// The approvals of one suggestion, in order.
    pub fn approvals_of(&self, suggestion_id: &str) -> Vec<ApprovalCall> {
        self.approvals
            .iter()
            .filter(|call| call.suggestion_id == suggestion_id)
            .cloned()
            .collect()
    }

    /// Scripts the answer to `suggestion_id`.
    pub fn answer(&mut self, suggestion_id: &str, answer: Answer) {
        self.answers.insert(suggestion_id.to_owned(), answer);
    }

    /// Scripts what `GET /api/suggestions/{id}` says of `suggestion_id`.
    pub fn suggestion(&mut self, suggestion_id: &str, answer: SuggestionAnswer) {
        self.suggestions.insert(suggestion_id.to_owned(), answer);
    }

    /// How many times `suggestion_id` was read through the door.
    pub fn reads_of(&self, suggestion_id: &str) -> usize {
        self.reads
            .iter()
            .filter(|read| *read == suggestion_id)
            .count()
    }
}

type Locked = Arc<Mutex<State>>;

/// A running stub Companion Gateway. Dropping it stops the server;
/// [`stop`](Self::stop) is the explicit way.
pub struct StubGateway {
    /// The origin the clerk is pointed at (`CLERK_GATEWAY_URL`):
    /// `http://127.0.0.1:<port>`, no trailing slash.
    pub base_url: String,
    state: Locked,
    handle: JoinHandle<()>,
}

impl StubGateway {
    /// Starts a stub on `port` — one of [`PORT_RANGE`], the test's own —
    /// holding [`INITIAL_REFRESH_TOKEN`] and no device token yet.
    pub async fn start(port: u16) -> Result<Self> {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding the stub Companion Gateway on {addr}"))?;
        Self::serve(listener).await
    }

    /// Starts a stub on the first free port of [`PORT_RANGE`], for a test
    /// that does not care which.
    /// Scanned from the **top** of the band, because `decisions.rs` takes its
    /// ports from the bottom (`17400 + n`) and this test runs in the same
    /// binary, in parallel: a port claimed here a moment before a test binds
    /// it by number would fail that test for a reason it could not see.
    pub async fn start_anywhere() -> Result<Self> {
        for port in PORT_RANGE.rev() {
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            if let Ok(listener) = tokio::net::TcpListener::bind(addr).await {
                return Self::serve(listener).await;
            }
        }
        anyhow::bail!(
            "no free port in {}..{} for a stub Companion Gateway",
            PORT_RANGE.start,
            PORT_RANGE.end
        )
    }

    async fn serve(listener: tokio::net::TcpListener) -> Result<Self> {
        let addr = listener
            .local_addr()
            .context("the stub Gateway listener has no local address")?;
        let state: Locked = Arc::new(Mutex::new(State::new()));
        let router = Router::new()
            .route("/api/session/refresh", post(refresh_route))
            .route("/api/approvals", post(approvals_route))
            .route("/api/suggestions/{id}", get(suggestion_route))
            .route("/api/devices", get(devices_route))
            .fallback(fallback_route)
            .with_state(state.clone());
        let handle = tokio::spawn(async move {
            // The server ends with the task, which is aborted on stop.
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            base_url: format!("http://{addr}"),
            state,
            handle,
        })
    }

    /// The refresh token the stub started with: what the session file must
    /// hold before the clerk's first refresh.
    pub fn initial_refresh_token(&self) -> String {
        INITIAL_REFRESH_TOKEN.to_owned()
    }

    /// The stub's state, to read or to script. The guard is a `std` mutex's:
    /// hold it across no `await`.
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("the stub Gateway mutex is never poisoned")
    }

    /// What `provision-clerk-device.sh` does on the Gateway's side: signs
    /// the device in again, so a **new** refresh token (`P<n>`) is the one
    /// the file must present from now on, and the device is no longer
    /// revoked. Returns the token, for the test to write into the file
    /// (`Run::provision_device` does both).
    pub fn reissue(&self) -> String {
        let mut state = self.state();
        state.reissued += 1;
        let token = format!("P{}", state.reissued);
        state.refresh_token = token.clone();
        state.revoked = false;
        token
    }

    /// Stops the server.
    pub async fn stop(self) {
        self.handle.abort();
    }
}

impl Drop for StubGateway {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// The cookies of one request, `(name, value)`, from every `Cookie` header.
fn cookies(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            Some((name.trim().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    cookies(headers)
        .into_iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

fn respond(status: u16, body: Value) -> Response<Body> {
    Response::builder()
        .status(StatusCode::from_u16(status).expect("a scripted status is a status"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("a JSON response builds")
}

/// The Gateway's own refusal of a session it will not have: the same body
/// for a missing, unknown, stale or revoked token (`session_http.rs`).
fn unauthenticated(state: &mut State) -> Response<Body> {
    state.unauthenticated += 1;
    respond(
        401,
        json!({
            "error": "unauthenticated",
            "detail": "this route needs a valid device token; sign in again",
        }),
    )
}

/// The device row `GET /api/devices` and `IssuedSession` carry.
fn device_json() -> Value {
    let now = now_unix();
    json!({
        "id": DEVICE_ID,
        "name": DEVICE_NAME,
        "created_unix_seconds": now.saturating_sub(3600),
        "last_seen_unix_seconds": now,
        "revoked_unix_seconds": Value::Null,
        "current": true,
    })
}

/// The Gateway's `Approval` (`openapi.yaml`, `approval_http.rs`'s
/// `approved_json`): `posted` is always `null` on the `POST` answer.
fn approval_json(suggestion_id: &str, edited: bool) -> Value {
    let now = now_rfc3339();
    json!({
        "posted": Value::Null,
        "event_id": approval_event_id(suggestion_id),
        "suggestion_event_id": suggestion_id,
        "approved_by": OWNER_MATRIX_ID,
        "persona_id": PERSONA_ID,
        "network": NETWORK,
        "contact": CONTACT,
        "edited": edited,
        "approved_at": now,
        "publication": "published",
        "stream_sequence": 42,
        "published_at": now,
    })
}

/// `POST /api/session/refresh`: the refresh cookie must be the current
/// token, the answer rotates both.
async fn refresh_route(Shared(state): Shared<Locked>, headers: HeaderMap) -> Response<Body> {
    let mut state = state
        .lock()
        .expect("the stub Gateway mutex is never poisoned");
    let presented = cookie(&headers, REFRESH_COOKIE);
    if state.revoked || presented.as_deref() != Some(state.refresh_token.as_str()) {
        return unauthenticated(&mut state);
    }
    state.refreshes += 1;
    let n = state.refreshes;
    let refresh = format!("R{n}");
    let device = format!("D{n}");
    state.refresh_token = refresh.clone();
    state.device_tokens.push(device.clone());
    let body = json!({
        "owner": OWNER_MATRIX_ID,
        "homeserver": HOMESERVER,
        "device": device_json(),
        "sensor": Value::Null,
        "expires_in": state.device_ttl,
    });
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .header(
            "set-cookie",
            format!(
                "{DEVICE_COOKIE}={device}; Path=/; Max-Age={}; HttpOnly; SameSite=Lax",
                state.device_ttl
            ),
        )
        .header(
            "set-cookie",
            format!("{REFRESH_COOKIE}={refresh}; Path=/api/session; HttpOnly; SameSite=Lax"),
        )
        .body(Body::from(body.to_string()))
        .expect("the issued session builds")
}

/// The device token of one request, if it is the current one.
fn as_device(state: &State, headers: &HeaderMap) -> Option<String> {
    let presented = cookie(headers, DEVICE_COOKIE)?;
    (!state.revoked && state.current_device_token() == Some(presented.as_str()))
        .then_some(presented)
}

/// `POST /api/approvals`: recorded, then answered as scripted.
async fn approvals_route(
    Shared(state): Shared<Locked>,
    headers: HeaderMap,
    body: String,
) -> Response<Body> {
    let mut state = state
        .lock()
        .expect("the stub Gateway mutex is never poisoned");
    let Some(device_token) = as_device(&state, &headers) else {
        return unauthenticated(&mut state);
    };
    let request: Value = match serde_json::from_str(&body) {
        Ok(value) => value,
        Err(_) => {
            return respond(
                400,
                json!({ "error": "malformed_request", "detail": "the body is not JSON" }),
            )
        }
    };
    let Some(suggestion_id) = request
        .get("suggestion_event_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return respond(
            400,
            json!({
                "error": "malformed_request",
                "detail": "suggestion_event_id is required",
            }),
        );
    };
    let final_body = request
        .get("final")
        .and_then(|value| value.get("body"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let edited = final_body.is_some();
    state.approvals.push(ApprovalCall {
        suggestion_id: suggestion_id.clone(),
        final_body,
        device_token,
        at_unix: now_unix(),
    });
    match state
        .answers
        .get(&suggestion_id)
        .cloned()
        .unwrap_or(Answer::Approve)
    {
        Answer::Approve => respond(201, approval_json(&suggestion_id, edited)),
        Answer::Refuse { status, code } => respond(
            status,
            json!({
                "error": code,
                "detail": format!("the stub Companion Gateway was told to answer {code}"),
            }),
        ),
        Answer::AlreadyApproved => respond(
            409,
            json!({
                "error": "already_approved",
                "detail": "this suggestion was approved earlier; the reply was not sent again",
                "approval": approval_json(&suggestion_id, false),
            }),
        ),
    }
}

/// The Gateway's `Suggestion` (`openapi.yaml`, `suggestions_http.rs`),
/// every required member present, so that the clerk's read is exercised
/// against the whole shape and not only the two members it keeps: the
/// trigger by identity alone, the persona's own words for the body — a
/// marker, so a test can check the clerk posts the bus's body and not the
/// Gateway's — and, when `standing` is `approved`, the `Approval` record
/// the real route carries, which names the contact and the owner.
fn suggestion_json(suggestion_id: &str, answer: &SuggestionAnswer) -> Value {
    let now = now_rfc3339();
    let approval = (answer.standing == "approved").then(|| approval_json(suggestion_id, false));
    json!({
        "event_id": suggestion_id,
        "source": format!("hermes://example.test/personas/{PERSONA_ID}"),
        "persona_id": PERSONA_ID,
        "network": NETWORK,
        "consent": "granted",
        "produced_at": now,
        "expires_at": now,
        "attempt": 1,
        "standing": answer.standing,
        "trigger": {
            "event_id": sha256_hex(&format!("trigger of {suggestion_id}")),
            "event_type": "fr.linagora.twalk.inbound.message.received.v1",
        },
        "suggestion": {
            "body": GATEWAY_SUGGESTION_BODY,
            "format": "text/plain",
        },
        "stream_sequence": 42,
        "approval": approval,
        "delivery": {
            "reach": answer.reach,
            "detail": answer.detail,
        },
        "posted": Value::Null,
    })
}

/// `GET /api/suggestions/{id}`: the device's read of one suggestion (#300),
/// recorded in [`State::reads`], then answered as scripted — or
/// `404 suggestion_not_found` for an id no test scripted, the Gateway's
/// own answer for a suggestion the bus does not hold.
async fn suggestion_route(
    Shared(state): Shared<Locked>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response<Body> {
    let mut state = state
        .lock()
        .expect("the stub Gateway mutex is never poisoned");
    if as_device(&state, &headers).is_none() {
        return unauthenticated(&mut state);
    }
    state.reads.push(id.clone());
    match state.suggestions.get(&id) {
        Some(answer) => respond(200, suggestion_json(&id, answer)),
        None => respond(
            404,
            json!({
                "error": "suggestion_not_found",
                "detail": "no suggestion with this id is on the bus",
            }),
        ),
    }
}

/// `GET /api/devices`: the one `Buzz` device, current.
async fn devices_route(Shared(state): Shared<Locked>, headers: HeaderMap) -> Response<Body> {
    let mut state = state
        .lock()
        .expect("the stub Gateway mutex is never poisoned");
    if as_device(&state, &headers).is_none() {
        return unauthenticated(&mut state);
    }
    respond(200, json!({ "devices": [device_json()] }))
}

async fn fallback_route(request: axum::extract::Request) -> Response<Body> {
    respond(
        404,
        json!({
            "error": "not_found",
            "detail": format!(
                "this stub Companion Gateway serves POST /api/session/refresh, POST /api/approvals, \
                 GET /api/suggestions/{{id}} and GET /api/devices only, not {} {}",
                request.method(),
                request.uri().path()
            ),
        }),
    )
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("a reqwest client builds")
    }

    /// The `Set-Cookie` values of `response`, `(name, value)`.
    fn set_cookies(response: &reqwest::Response) -> Vec<(String, String)> {
        response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| {
                let (name, value) = value.split(';').next()?.split_once('=')?;
                Some((name.trim().to_owned(), value.trim().to_owned()))
            })
            .collect()
    }

    fn cookie_named(cookies: &[(String, String)], name: &str) -> String {
        cookies
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| panic!("no {name} cookie in {cookies:?}"))
    }

    #[tokio::test]
    async fn the_stub_rotates_and_refuses_a_stale_refresh_token() {
        let stub = StubGateway::start_anywhere().await.unwrap();
        let http = client();
        let refresh_url = format!("{}/api/session/refresh", stub.base_url);
        let initial = stub.initial_refresh_token();
        assert_eq!(initial, INITIAL_REFRESH_TOKEN);

        // A refresh with the initial token rotates both.
        let first = http
            .post(&refresh_url)
            .header("cookie", format!("{REFRESH_COOKIE}={initial}"))
            .send()
            .await
            .unwrap();
        assert_eq!(first.status(), 200);
        let cookies = set_cookies(&first);
        let device = cookie_named(&cookies, DEVICE_COOKIE);
        let rotated = cookie_named(&cookies, REFRESH_COOKIE);
        assert_eq!(device, "D1");
        assert_eq!(rotated, "R1");
        let issued: Value = first.json().await.unwrap();
        assert_eq!(issued["expires_in"], json!(DEVICE_TTL));
        assert_eq!(issued["owner"], json!(OWNER_MATRIX_ID));
        assert_eq!(issued["device"]["name"], json!(DEVICE_NAME));
        assert!(
            issued.get("device").is_some() && issued.get("homeserver").is_some(),
            "the IssuedSession shape: {issued}"
        );
        assert!(
            !issued.to_string().contains("D1") && !issued.to_string().contains("R1"),
            "the tokens travel in the cookies, never in the body: {issued}"
        );
        {
            let state = stub.state();
            assert_eq!(state.refreshes, 1);
            assert_eq!(state.refresh_token, "R1");
            assert_eq!(state.current_device_token(), Some("D1"));
        }

        // The initial token is dead: presenting it again is 401.
        let stale = http
            .post(&refresh_url)
            .header("cookie", format!("{REFRESH_COOKIE}={initial}"))
            .send()
            .await
            .unwrap();
        assert_eq!(stale.status(), 401);
        let body: Value = stale.json().await.unwrap();
        assert_eq!(body["error"], json!("unauthenticated"));
        assert_eq!(stub.state().unauthenticated, 1);

        // The current device token opens the routes; an approval is
        // recorded with what it carried and answered with the record.
        let suggestion = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let approved = http
            .post(format!("{}/api/approvals", stub.base_url))
            .header("cookie", format!("{DEVICE_COOKIE}={device}"))
            .header("content-type", "application/json")
            .body(
                json!({ "suggestion_event_id": suggestion, "final": { "body": "Merci." } })
                    .to_string(),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(approved.status(), 201);
        let record: Value = approved.json().await.unwrap();
        assert_eq!(record["event_id"], json!(approval_event_id(suggestion)));
        assert_eq!(record["suggestion_event_id"], json!(suggestion));
        assert_eq!(record["approved_by"], json!(OWNER_MATRIX_ID));
        assert_eq!(record["edited"], json!(true));
        assert_eq!(record["publication"], json!("published"));
        assert_eq!(record["posted"], Value::Null);
        {
            let state = stub.state();
            assert_eq!(state.approvals.len(), 1);
            assert_eq!(state.approvals[0].suggestion_id, suggestion);
            assert_eq!(state.approvals[0].final_body.as_deref(), Some("Merci."));
            assert_eq!(state.approvals[0].device_token, "D1");
        }

        // A scripted refusal and the duplicate answer.
        let refused_id = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
        stub.state().answer(
            refused_id,
            Answer::Refuse {
                status: 409,
                code: "consent_revoked".to_owned(),
            },
        );
        let refused = http
            .post(format!("{}/api/approvals", stub.base_url))
            .header("cookie", format!("{DEVICE_COOKIE}={device}"))
            .header("content-type", "application/json")
            .body(json!({ "suggestion_event_id": refused_id }).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), 409);
        let body: Value = refused.json().await.unwrap();
        assert_eq!(body["error"], json!("consent_revoked"));
        stub.state().answer(suggestion, Answer::AlreadyApproved);
        let again = http
            .post(format!("{}/api/approvals", stub.base_url))
            .header("cookie", format!("{DEVICE_COOKIE}={device}"))
            .header("content-type", "application/json")
            .body(json!({ "suggestion_event_id": suggestion }).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(again.status(), 409);
        let body: Value = again.json().await.unwrap();
        assert_eq!(body["error"], json!("already_approved"));
        assert_eq!(body["approval"]["edited"], json!(false));
        assert_eq!(
            body["approval"]["event_id"],
            json!(approval_event_id(suggestion))
        );
        assert_eq!(stub.state().approvals.len(), 3, "every call that got in");

        // A suggestion read, as the device: scripted, the whole shape with
        // the scripted standing and delivery; unscripted, 404 with the
        // Gateway's code; both recorded, in order. Without the device
        // token it is 401 and not recorded.
        stub.state().suggestion(
            suggestion,
            SuggestionAnswer::new("approvable", "cannot_reach", "owner_invited"),
        );
        let read = http
            .get(format!("{}/api/suggestions/{suggestion}", stub.base_url))
            .header("cookie", format!("{DEVICE_COOKIE}={device}"))
            .send()
            .await
            .unwrap();
        assert_eq!(read.status(), 200);
        let found: Value = read.json().await.unwrap();
        assert_eq!(found["event_id"], json!(suggestion));
        assert_eq!(found["standing"], json!("approvable"));
        assert_eq!(found["delivery"]["reach"], json!("cannot_reach"));
        assert_eq!(found["delivery"]["detail"], json!("owner_invited"));
        assert_eq!(found["approval"], Value::Null);
        assert_eq!(found["suggestion"]["body"], json!(GATEWAY_SUGGESTION_BODY));
        for member in [
            "source",
            "persona_id",
            "network",
            "consent",
            "produced_at",
            "expires_at",
            "attempt",
            "trigger",
            "stream_sequence",
            "posted",
        ] {
            assert!(found.get(member).is_some(), "the Suggestion shape: {found}");
        }
        let unscripted = http
            .get(format!("{}/api/suggestions/{refused_id}", stub.base_url))
            .header("cookie", format!("{DEVICE_COOKIE}={device}"))
            .send()
            .await
            .unwrap();
        assert_eq!(unscripted.status(), 404);
        let body: Value = unscripted.json().await.unwrap();
        assert_eq!(body["error"], json!("suggestion_not_found"));
        let no_device = http
            .get(format!("{}/api/suggestions/{suggestion}", stub.base_url))
            .send()
            .await
            .unwrap();
        assert_eq!(no_device.status(), 401);
        {
            let state = stub.state();
            assert_eq!(
                state.reads,
                vec![suggestion.to_owned(), refused_id.to_owned()]
            );
            assert_eq!(state.reads_of(suggestion), 1);
        }
        stub.state().suggestion(
            suggestion,
            SuggestionAnswer::new("approved", "can_reach", "owner_joined"),
        );
        let approved_read = http
            .get(format!("{}/api/suggestions/{suggestion}", stub.base_url))
            .header("cookie", format!("{DEVICE_COOKIE}={device}"))
            .send()
            .await
            .unwrap();
        let found: Value = approved_read.json().await.unwrap();
        assert_eq!(found["standing"], json!("approved"));
        assert_eq!(
            found["approval"]["contact"],
            json!(CONTACT),
            "an approved suggestion carries the record, as the Gateway's does"
        );
        assert_eq!(stub.state().reads_of(suggestion), 2);

        // The device list, as the device.
        let devices = http
            .get(format!("{}/api/devices", stub.base_url))
            .header("cookie", format!("{DEVICE_COOKIE}={device}"))
            .send()
            .await
            .unwrap();
        assert_eq!(devices.status(), 200);
        let list: Value = devices.json().await.unwrap();
        assert_eq!(list["devices"][0]["name"], json!(DEVICE_NAME));
        assert_eq!(list["devices"][0]["current"], json!(true));

        // A second refresh rotates again, and then the first device token
        // is stale: 401, and not recorded as an approval.
        let second = http
            .post(&refresh_url)
            .header("cookie", format!("{REFRESH_COOKIE}={rotated}"))
            .send()
            .await
            .unwrap();
        assert_eq!(second.status(), 200);
        assert_eq!(cookie_named(&set_cookies(&second), DEVICE_COOKIE), "D2");
        let stale_device = http
            .post(format!("{}/api/approvals", stub.base_url))
            .header("cookie", format!("{DEVICE_COOKIE}={device}"))
            .header("content-type", "application/json")
            .body(json!({ "suggestion_event_id": suggestion }).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(stale_device.status(), 401);
        assert_eq!(stub.state().approvals.len(), 3);

        // Revoked: everything is 401, the refresh with the right token too.
        stub.state().revoked = true;
        let revoked = http
            .post(&refresh_url)
            .header("cookie", format!("{REFRESH_COOKIE}=R2"))
            .send()
            .await
            .unwrap();
        assert_eq!(revoked.status(), 401);
        let revoked_device = http
            .get(format!("{}/api/devices", stub.base_url))
            .header("cookie", format!("{DEVICE_COOKIE}=D2"))
            .send()
            .await
            .unwrap();
        assert_eq!(revoked_device.status(), 401);

        // Reissued, as the operator's script would: a new token opens the
        // session again and the old one does not.
        let provisioned = stub.reissue();
        assert_eq!(provisioned, "P1");
        let back = http
            .post(&refresh_url)
            .header("cookie", format!("{REFRESH_COOKIE}={provisioned}"))
            .send()
            .await
            .unwrap();
        assert_eq!(back.status(), 200);
        assert_eq!(cookie_named(&set_cookies(&back), DEVICE_COOKIE), "D3");

        // A route the stub does not serve says so.
        let elsewhere = http
            .get(format!("{}/api/session", stub.base_url))
            .send()
            .await
            .unwrap();
        assert_eq!(elsewhere.status(), 404);

        stub.stop().await;
    }
}
