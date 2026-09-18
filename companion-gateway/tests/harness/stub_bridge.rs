//! A stub bridge implementing mautrix bridgev2's provisioning contract
//! (ticket #55): the flows, the start, a **blocking** step the test releases
//! on command, the completion, the cancels, the logout, the logins list and
//! `whoami`.
//!
//! # Why a stub and not a real bridge
//!
//! A real mautrix-whatsapp needs a live WhatsApp account and a human with a
//! phone to scan a code, so it can never be in a suite (spec #47 says so in
//! as many words). What *can* be in a suite is the contract: the Gateway's
//! job is to speak that API, hold its blocking step and expose a pollable
//! state, and every one of those properties is provable against a stub that
//! answers like the real thing. What is therefore never proven by tests is
//! the network side — an accepted limitation, stated rather than discovered.
//!
//! # The blocking step
//!
//! `POST /_matrix/provision/v3/login/step/{process}/{step}/display_and_wait`
//! does not answer until the test says so: [`StubBridge::release_refreshed_qr`]
//! makes it answer with a *new* code (mautrix's QR refresh), and
//! [`StubBridge::release_completion`] makes it answer `complete`. Answers are
//! queued, so a test never has to win a race against the Gateway's own
//! request arriving.
//!
//! The stub runs inside the test process — the control surface is method
//! calls on [`StubBridge`], not a second HTTP API — and it serves on a port
//! the kernel picked, so parallel tests and parallel worktrees never collide.

#![allow(dead_code, unused_imports)]

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};

/// The provisioning secret the stub expects, and the Gateway is configured
/// with. Sixteen characters or more, as mautrix requires — below that a real
/// bridge answers `M_FORBIDDEN` to the whole provisioning API.
pub const STUB_PROVISIONING_SECRET: &str = "test-only-stub-bridge-provisioning-secret";

/// The appservice token the stub bridge authenticates its **status pushes**
/// with (ticket #56), and the one the Gateway is configured with for it.
/// A throwaway constant for the local test stack; in a deployment this is
/// `openssl rand -hex 32` and impersonates the appservice.
pub const STUB_AS_TOKEN: &str = "test-only-stub-bridge-as-token-0123456789abcdef";

/// The flow ids the stub offers.
pub const QR_FLOW: &str = "qr";
pub const PHONE_FLOW: &str = "phone";
pub const COOKIES_FLOW: &str = "cookies";
pub const WEBAUTHN_FLOW: &str = "webauthn";

/// The step ids the stub uses, which the Companion submits against.
pub const QR_STEP: &str = "fi.mau.stub.login.qr";
pub const PHONE_STEP: &str = "fi.mau.stub.login.phone";
pub const COOKIES_STEP: &str = "fi.mau.stub.login.cookies";

/// What the stub should answer a held `display_and_wait` with.
#[derive(Debug, Clone)]
pub enum Release {
    /// A refreshed code: the same step, a new payload. This is what a real
    /// bridge does every ~20 seconds while nobody has scanned yet.
    RefreshedQr { data: String },
    /// The network accepted the login.
    Complete { login_id: String },
    /// A refusal, with mautrix's own error document.
    Refusal { status: u16, errcode: String },
}

/// One start the stub saw, for a test that asserts what the Gateway sent.
#[derive(Debug, Clone)]
pub struct StartRecord {
    pub flow_id: String,
    /// The acting user from the `user_id` query parameter — with
    /// shared-secret auth, whoever the caller says, so the test checks the
    /// Gateway says the owner.
    pub user_id: Option<String>,
    /// The `login_id` query parameter: present when this was a reconnect.
    pub login_id: Option<String>,
}

/// One step submission the stub saw: the body the Gateway relayed, which is
/// how a test proves a credential passed *through*.
#[derive(Debug, Clone)]
pub struct SubmitRecord {
    pub step_id: String,
    pub step_type: String,
    pub body: Value,
    /// The `txn_id` query parameter, which makes a retry idempotent.
    pub txn_id: Option<String>,
}

#[derive(Default)]
struct Inner {
    /// Live login processes, by id: the flow each one is running.
    processes: HashMap<String, String>,
    /// Queued answers for the held blocking step.
    releases: VecDeque<Release>,
    /// How many times the Gateway has sat down in the blocking step.
    blocking_arrivals: u64,
    /// How many held requests are inside the stub right now.
    held: u64,
    /// The logins the stub pretends to hold.
    logins: Vec<Value>,
    starts: Vec<StartRecord>,
    submits: Vec<SubmitRecord>,
    /// Canned refusals for the next start calls.
    refuse_start: VecDeque<(u16, String)>,
    /// Canned refusals for the next submitted (non-blocking) step calls.
    refuse_step: VecDeque<(u16, String)>,
    /// Processes the test has had the stub cancel from its own side.
    cancelled: Vec<String>,
    logged_out: Vec<String>,
    next_process: u64,
    next_qr: u64,
}

struct StubState {
    inner: Mutex<Inner>,
    /// Woken when a release is queued, so the held request answers at once.
    released: tokio::sync::Notify,
}

/// The stub bridge: a server on a kernel-chosen port, and the methods a test
/// drives it with.
pub struct StubBridge {
    addr: SocketAddr,
    state: Arc<StubState>,
    server: Option<tokio::task::JoinHandle<()>>,
}

impl StubBridge {
    /// Starts the stub on a free loopback port.
    pub async fn start() -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("failed to bind the stub bridge")?;
        let addr = listener.local_addr()?;
        let state = Arc::new(StubState {
            inner: Mutex::new(Inner::default()),
            released: tokio::sync::Notify::new(),
        });
        let server = serve(listener, state.clone());
        Ok(Self {
            addr,
            state,
            server: Some(server),
        })
    }

    /// The base URL the Gateway is configured with — the bridge's appservice
    /// listener.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// A bridge restart, as the Gateway can tell one: every login process is
    /// forgotten, and the request the Gateway is holding comes back with the
    /// `404` a bridge answers for a process it has never heard of.
    ///
    /// Why that rather than killing the socket: both are restarts as far as
    /// the facade is concerned, and both land on the same state
    /// (`failed` / `login_lost`) — a dropped connection is
    /// [`crate::bridge::BridgeRefusal::BridgeUnreachable`] and a forgotten
    /// process is `NotFoundOnBridge`. This one is deterministic, so the test
    /// asserts a state instead of racing a TCP teardown. The dropped-socket
    /// half is covered by `bridge::tests::a_bridge_that_stops_answering_a_held_step_loses_the_login`
    /// and, at this seam, by the bridge that is configured and down.
    pub async fn restart(&mut self) -> Result<()> {
        let held = {
            let mut inner = self.state.inner.lock().expect("the stub is not poisoned");
            inner.processes.clear();
            inner.releases.clear();
            inner.held
        };
        // One answer per held request, so nothing is left waiting on a
        // process the new instance does not have.
        for _ in 0..held.max(1) {
            self.release_refusal(404, "M_NOT_FOUND");
        }
        Ok(())
    }

    /// Answers the held blocking step with a refreshed code, as a bridge
    /// does when nobody has scanned yet.
    pub fn release_refreshed_qr(&self, data: &str) {
        self.queue(Release::RefreshedQr {
            data: data.to_owned(),
        });
    }

    /// Answers the held blocking step with the completion, as a bridge does
    /// when the phone has scanned.
    pub fn release_completion(&self, login_id: &str) {
        self.queue(Release::Complete {
            login_id: login_id.to_owned(),
        });
    }

    /// Answers the held blocking step with one of mautrix's own refusals.
    pub fn release_refusal(&self, status: u16, errcode: &str) {
        self.queue(Release::Refusal {
            status,
            errcode: errcode.to_owned(),
        });
    }

    fn queue(&self, release: Release) {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .releases
            .push_back(release);
        self.state.released.notify_waiters();
    }

    /// Makes the next `login/start` answer this refusal instead.
    pub fn refuse_next_start(&self, status: u16, errcode: &str) {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .refuse_start
            .push_back((status, errcode.to_owned()));
    }

    /// Makes the next submitted (non-blocking) step answer this refusal.
    pub fn refuse_next_step(&self, status: u16, errcode: &str) {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .refuse_step
            .push_back((status, errcode.to_owned()));
    }

    /// Adds a login the stub pretends to already hold: what the Companion
    /// reads to offer "reconnect".
    pub fn add_existing_login(&self, login_id: &str, name: &str) {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .logins
            .push(json!({
                "id": login_id,
                "name": name,
                "profile": { "id": login_id, "name": name },
            }));
    }

    /// Gives a login the `state` a real bridge reports under `whoami`
    /// (ticket #56): the same `BridgeState` document the status webhook
    /// carries, nested under the login. This is what startup reconciliation
    /// reads, and it is the only thing `whoami` says about health.
    ///
    /// A login with no `state` at all is a bridge that has just restarted —
    /// its state lives in memory — and the stub leaves one that way until a
    /// test says otherwise.
    pub fn set_login_state(&self, login_id: &str, state: Value) {
        let mut inner = self.state.inner.lock().expect("the stub is not poisoned");
        if let Some(login) = inner
            .logins
            .iter_mut()
            .find(|login| login["id"] == json!(login_id))
        {
            login["state"] = state;
        }
    }

    /// Forgets every login: what a `whoami` on a bridge nobody has logged in
    /// to answers, and what a logout leaves behind.
    pub fn clear_logins(&self) {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .logins
            .clear();
    }

    /// How many times a held blocking request has arrived: the count that
    /// proves the Gateway, and not the browser, is holding the step.
    pub fn blocking_arrivals(&self) -> u64 {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .blocking_arrivals
    }

    /// How many held requests are sitting in the stub right now.
    pub fn held(&self) -> u64 {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .held
    }

    pub fn starts(&self) -> Vec<StartRecord> {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .starts
            .clone()
    }

    /// Every step body the Gateway relayed: how a test proves a credential
    /// reached the bridge unchanged.
    pub fn submits(&self) -> Vec<SubmitRecord> {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .submits
            .clone()
    }

    pub fn cancelled_processes(&self) -> Vec<String> {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .cancelled
            .clone()
    }

    pub fn logged_out(&self) -> Vec<String> {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .logged_out
            .clone()
    }

    pub async fn stop(mut self) {
        if let Some(server) = self.server.take() {
            server.abort();
            let _ = server.await;
        }
    }
}

fn serve(listener: tokio::net::TcpListener, state: Arc<StubState>) -> tokio::task::JoinHandle<()> {
    let app = Router::new()
        .route("/_matrix/provision/v3/login/flows", get(flows))
        .route("/_matrix/provision/v3/login/start/{flow_id}", post(start))
        .route(
            "/_matrix/provision/v3/login/step/{process_id}/{step_id}/{step_type}",
            post(step),
        )
        .route(
            "/_matrix/provision/v3/login/cancel/{process_id}",
            post(cancel_process),
        )
        .route("/_matrix/provision/v3/logout/{login_id}", post(logout))
        .route("/_matrix/provision/v3/logins", get(logins))
        .route("/_matrix/provision/v3/whoami", get(whoami))
        .with_state(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    })
}

/// mautrix's own answer to a call without the provisioning secret.
fn forbidden() -> Response {
    mautrix_error(StatusCode::FORBIDDEN, "M_FORBIDDEN")
}

fn mautrix_error(status: StatusCode, errcode: &str) -> Response {
    (
        status,
        Json(json!({ "errcode": errcode, "error": format!("the stub bridge answers {errcode}") })),
    )
        .into_response()
}

/// The bearer check every endpoint starts with. A real bridge would also
/// accept a Matrix access token; the reference configuration turns that off
/// (`provisioning.allow_matrix_auth: false`), so the stub does not have it.
fn authorised(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        == Some(STUB_PROVISIONING_SECRET)
}

async fn flows(headers: HeaderMap) -> Response {
    if !authorised(&headers) {
        return forbidden();
    }
    Json(json!({
        "flows": [
            { "id": QR_FLOW, "name": "Scan a QR code", "description": "Link a device by scanning" },
            { "id": PHONE_FLOW, "name": "Phone number" },
            { "id": COOKIES_FLOW, "name": "Paste cookies" },
            { "id": WEBAUTHN_FLOW, "name": "Passkey" },
        ]
    }))
    .into_response()
}

async fn whoami(headers: HeaderMap, State(state): State<Arc<StubState>>) -> Response {
    if !authorised(&headers) {
        return forbidden();
    }
    let logins = state
        .inner
        .lock()
        .expect("the stub is not poisoned")
        .logins
        .clone();
    Json(json!({ "network": { "id": "stub", "display_name": "Stub" }, "logins": logins }))
        .into_response()
}

async fn logins(headers: HeaderMap, State(state): State<Arc<StubState>>) -> Response {
    if !authorised(&headers) {
        return forbidden();
    }
    let logins = state
        .inner
        .lock()
        .expect("the stub is not poisoned")
        .logins
        .clone();
    Json(Value::Array(logins)).into_response()
}

async fn start(
    headers: HeaderMap,
    State(state): State<Arc<StubState>>,
    Path(flow_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !authorised(&headers) {
        return forbidden();
    }
    let (process_id, first_qr) = {
        let mut inner = state.inner.lock().expect("the stub is not poisoned");
        inner.starts.push(StartRecord {
            flow_id: flow_id.clone(),
            user_id: query.get("user_id").cloned(),
            login_id: query.get("login_id").cloned(),
        });
        if let Some((status, errcode)) = inner.refuse_start.pop_front() {
            return mautrix_error(
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
                &errcode,
            );
        }
        inner.next_process += 1;
        inner.next_qr += 1;
        let process_id = format!("stub-process-{}", inner.next_process);
        inner.processes.insert(process_id.clone(), flow_id.clone());
        let qr = inner.next_qr;
        (process_id, format!("2@stub-qr-payload-{qr}"))
    };
    match flow_id.as_str() {
        QR_FLOW => Json(qr_step(&process_id, &first_qr)).into_response(),
        PHONE_FLOW => Json(json!({
            "login_id": process_id,
            "type": "user_input",
            "step_id": PHONE_STEP,
            "instructions": "Enter the phone number of the account",
            "user_input": {
                "fields": [
                    { "type": "phone_number", "id": "phone_number", "name": "Phone number" }
                ]
            }
        }))
        .into_response(),
        COOKIES_FLOW => Json(json!({
            "login_id": process_id,
            "type": "cookies",
            "step_id": COOKIES_STEP,
            "instructions": "Paste the cookies from a private window",
            "cookies": {
                "url": "https://messages.google.com/web/authentication",
                "fields": [{ "type": "cookie", "cookie_domain": ".google.com", "id": "SID" }]
            }
        }))
        .into_response(),
        WEBAUTHN_FLOW => Json(json!({
            "login_id": process_id,
            "type": "webauthn",
            "step_id": "fi.mau.stub.login.webauthn",
            "instructions": "Use your passkey",
            "webauthn": { "publicKey": {} }
        }))
        .into_response(),
        unknown => mautrix_error(
            StatusCode::NOT_FOUND,
            &format!("M_NOT_FOUND (no flow {unknown})"),
        ),
    }
}

/// Shaped like a real mautrix answer: the login **process** id travels as
/// `login_id` at the top level — the same word mautrix's `?login_id=` query
/// parameter uses for an existing login. The Gateway was first written
/// against a stub that called it `login_process_id`, and the real bridge
/// then failed with "names no login process" (see the ticket in the PR).
fn qr_step(process_id: &str, data: &str) -> Value {
    json!({
        "login_id": process_id,
        "type": "display_and_wait",
        "step_id": QR_STEP,
        "instructions": "Scan this code from the phone",
        // `data` is the raw payload: a real bridge renders no image, so the
        // browser is what draws the code.
        "display_and_wait": { "type": "qr", "data": data, "can_cancel": true }
    })
}

async fn step(
    headers: HeaderMap,
    State(state): State<Arc<StubState>>,
    Path((process_id, step_id, step_type)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: String,
) -> Response {
    if !authorised(&headers) {
        return forbidden();
    }
    // A process the stub does not know: what a restarted bridge answers, and
    // the one case the Gateway must report as a lost login.
    {
        let inner = state.inner.lock().expect("the stub is not poisoned");
        if !inner.processes.contains_key(&process_id) {
            return mautrix_error(StatusCode::NOT_FOUND, "M_NOT_FOUND");
        }
    }
    if step_type == "cancel" {
        // Cancelling the step releases whatever request is held on it.
        state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .releases
            .push_back(Release::Refusal {
                status: 409,
                errcode: "FI.MAU.LOGIN_STEP_CANCELLED".to_owned(),
            });
        state.released.notify_waiters();
        return StatusCode::NO_CONTENT.into_response();
    }

    let parsed: Value = serde_json::from_str(body.trim()).unwrap_or(Value::Null);
    {
        let mut inner = state.inner.lock().expect("the stub is not poisoned");
        inner.submits.push(SubmitRecord {
            step_id: step_id.clone(),
            step_type: step_type.clone(),
            body: parsed.clone(),
            txn_id: query.get("txn_id").cloned(),
        });
    }

    if step_type == "display_and_wait" {
        return held_step(state, &process_id).await;
    }

    // A non-blocking step: a canned refusal if the test asked for one, and
    // the completion otherwise.
    if let Some((status, errcode)) = state
        .inner
        .lock()
        .expect("the stub is not poisoned")
        .refuse_step
        .pop_front()
    {
        return mautrix_error(
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            &errcode,
        );
    }
    let login_id = format!("stub-login-{process_id}");
    complete(state, &process_id, &login_id)
}

/// The blocking step: it does not answer until the test releases it.
async fn held_step(state: Arc<StubState>, process_id: &str) -> Response {
    {
        let mut inner = state.inner.lock().expect("the stub is not poisoned");
        inner.blocking_arrivals += 1;
        inner.held += 1;
    }
    let release = loop {
        // Subscribe before looking, so a release queued between the two is
        // not missed.
        let woken = state.released.notified();
        if let Some(release) = state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .releases
            .pop_front()
        {
            break release;
        }
        woken.await;
    };
    state.inner.lock().expect("the stub is not poisoned").held -= 1;
    match release {
        Release::RefreshedQr { data } => Json(qr_step(process_id, &data)).into_response(),
        Release::Complete { login_id } => complete(state, process_id, &login_id),
        Release::Refusal { status, errcode } => mautrix_error(
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            &errcode,
        ),
    }
}

/// The completion step, and the login it leaves behind on the bridge.
fn complete(state: Arc<StubState>, process_id: &str, login_id: &str) -> Response {
    {
        let mut inner = state.inner.lock().expect("the stub is not poisoned");
        inner.processes.remove(process_id);
        if !inner
            .logins
            .iter()
            .any(|login| login["id"] == json!(login_id))
        {
            inner.logins.push(json!({
                "id": login_id,
                "name": "the stub's account",
                "profile": { "id": login_id },
            }));
        }
    }
    Json(json!({
        "type": "complete",
        "step_id": "fi.mau.stub.login.complete",
        "instructions": "Connected",
        "complete": { "login_id": login_id, "user_login_id": login_id }
    }))
    .into_response()
}

async fn cancel_process(
    headers: HeaderMap,
    State(state): State<Arc<StubState>>,
    Path(process_id): Path<String>,
) -> Response {
    if !authorised(&headers) {
        return forbidden();
    }
    let mut inner = state.inner.lock().expect("the stub is not poisoned");
    if inner.processes.remove(&process_id).is_none() {
        return mautrix_error(StatusCode::NOT_FOUND, "M_NOT_FOUND");
    }
    inner.cancelled.push(process_id);
    StatusCode::NO_CONTENT.into_response()
}

async fn logout(
    headers: HeaderMap,
    State(state): State<Arc<StubState>>,
    Path(login_id): Path<String>,
) -> Response {
    if !authorised(&headers) {
        return forbidden();
    }
    let mut inner = state.inner.lock().expect("the stub is not poisoned");
    let before = inner.logins.len();
    inner.logins.retain(|login| login["id"] != json!(login_id));
    if inner.logins.len() == before {
        return mautrix_error(StatusCode::NOT_FOUND, "M_NOT_FOUND");
    }
    inner.logged_out.push(login_id);
    StatusCode::NO_CONTENT.into_response()
}
