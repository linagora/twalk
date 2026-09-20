//! A stub bridge that answers **recorded mautrix documents** (#106): the
//! flows, the start, a blocking step the test releases on command, the
//! completion, the cancels, the logout, the logins list and `whoami`.
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
//! # Why every document here is recorded rather than written
//!
//! Because a hand-written stub cost three bugs in a row. Each time, the stub
//! answered a shape no mautrix bridge produces, the ten tests below went
//! green, and a real WhatsApp login failed: on the name of the login process
//! id, on the `?user_id=` the bridge wants on *every* call, and on the
//! `txn_id` the bridge issues and then validates. So the bodies this stub
//! sends come out of `harness::bridge_fixtures`, captured from
//! mautrix-whatsapp and mautrix-signal v26.09 on the reference deployment.
//! Only what a live bridge would invent per call — the process id, the
//! transaction id, the QR payload — is substituted.
//!
//! The corollary is the rule for changing this file: **a shape that is not in
//! a fixture does not go in here.** Capture it from a bridge first.
//!
//! # And why it refuses as much as it answers
//!
//! A stub that accepts more than the real thing is exactly how all three bugs
//! got through, so every refusal a reference bridge was observed to give is
//! enforced here: the acting user on every endpoint, the transaction id
//! issued with a step and validated on the call that advances it, the step id
//! and step type that have to match, `404` for a process the bridge has
//! forgotten, and the step-cancel endpoint answering `500 M_BAD_STATE`
//! because neither reference bridge supports cancelling a `display_and_wait`
//! step. What releases a held request is the *process* cancel, and the held
//! request then comes back `410 FI.MAU.BRIDGE.LOGIN_CANCELLED` — the real
//! sequence, not a convenient one.
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
//! # A sequence of steps
//!
//! A submitted step used to answer the completion, always, which meant this
//! stub could not serve two `user_input` steps in a row — so a
//! password-then-code sequence, a step re-issued after a refusal and a field
//! type nobody can draw had no coverage and no fixture able to express them
//! (ADR 0030). [`StubBridge::queue_next_step`] and
//! [`StubBridge::queue_input_step`] queue what the next submit is answered
//! with, in order, and the completion is what happens once the queue is
//! empty.
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

use super::bridge_fixtures::{self, Bridge};

/// The provisioning secret the stub expects, and the Gateway is configured
/// with. Sixteen characters or more, as mautrix requires — below that a real
/// bridge answers `M_FORBIDDEN` to the whole provisioning API.
pub const STUB_PROVISIONING_SECRET: &str = "test-only-stub-bridge-provisioning-secret";

/// The appservice token the stub bridge authenticates its **status pushes**
/// with (ticket #56), and the one the Gateway is configured with for it.
/// A throwaway constant for the local test stack; in a deployment this is
/// `openssl rand -hex 32` and impersonates the appservice.
pub const STUB_AS_TOKEN: &str = "test-only-stub-bridge-as-token-0123456789abcdef";

/// The flow ids the stub offers. `qr` and `phone` are mautrix-whatsapp's own
/// two, served from its captured answers; `cookies` and `webauthn` have no
/// counterpart on either reference bridge and are the stub's own, kept
/// because the Gateway has to refuse a step type it cannot drive and there is
/// no recorded bridge that offers one.
pub const QR_FLOW: &str = "qr";
pub const PHONE_FLOW: &str = "phone";
pub const COOKIES_FLOW: &str = "cookies";
pub const WEBAUTHN_FLOW: &str = "webauthn";

/// The step ids the stub uses, which the Companion submits against. The first
/// two are mautrix-whatsapp's real ones — step ids are namespaced per
/// connector, and a Signal stub answers `fi.mau.signal.login.qr` instead, so
/// nothing may hard-code these beyond the default persona.
pub const QR_STEP: &str = "fi.mau.whatsapp.login.qr";
pub const PHONE_STEP: &str = "fi.mau.whatsapp.login.phone";
pub const COOKIES_STEP: &str = "fi.mau.stub.login.cookies";
pub const WEBAUTHN_STEP: &str = "fi.mau.stub.login.webauthn";

/// What the stub should answer a held `display_and_wait` with.
#[derive(Debug, Clone)]
pub enum Release {
    /// A refreshed code: the same step, a new payload, and — as a real bridge
    /// does — a new `txn_id`. This is what a bridge answers every ~20 seconds
    /// while nobody has scanned yet.
    RefreshedQr { data: String },
    /// Another **step**: a blocking step answered with a question rather than
    /// a code. That is the shape a QR login takes when the account has
    /// two-factor authentication on, and the body is the test's own for the
    /// reason [`StubBridge::queue_next_step`]'s is.
    Step { body: Value },
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
    /// The `txn_id` query parameter, which makes a retry idempotent and which
    /// the stub validates exactly as a bridge does.
    pub txn_id: Option<String>,
}

/// A login process the stub is running, and the step it is on. A real bridge
/// keeps all of this, which is why it can refuse a call that names the wrong
/// step or a stale transaction.
#[derive(Debug, Clone)]
struct Process {
    flow_id: String,
    step_id: String,
    step_type: String,
    /// Re-issued with every answer this process gives, as mautrix does.
    txn_id: String,
}

#[derive(Default)]
struct Inner {
    /// Live login processes, by id.
    processes: HashMap<String, Process>,
    /// Queued answers for the held blocking step.
    releases: VecDeque<Release>,
    /// How many times the Gateway has sat down in the blocking step.
    blocking_arrivals: u64,
    /// How many held requests are inside the stub right now.
    held: u64,
    /// The logins the stub pretends to hold, as `whoami` describes them.
    logins: Vec<Value>,
    /// A cap on those, so a test can provoke the network's own refusal.
    max_logins: Option<usize>,
    starts: Vec<StartRecord>,
    submits: Vec<SubmitRecord>,
    /// Canned refusals for the next start calls.
    refuse_start: VecDeque<(u16, String)>,
    /// Bodies a test has told the stub to answer a start or a `whoami` with,
    /// `200` and all, in place of the captured document. Never a bridge's
    /// shape — that is what they are for.
    misshape_start: VecDeque<Value>,
    misshape_whoami: VecDeque<Value>,
    /// Canned refusals for the next submitted (non-blocking) step calls.
    refuse_step: VecDeque<(u16, String)>,
    /// Steps a test has queued for the next submits, in order — what makes a
    /// login of more than one question expressible at all (ADR 0030).
    next_steps: VecDeque<Value>,
    /// Processes the test has had the stub cancel from its own side.
    cancelled: Vec<String>,
    logged_out: Vec<String>,
    next_process: u64,
    next_txn: u64,
    next_qr: u64,
}

struct StubState {
    /// Whose captured answers this stub serves.
    bridge: Bridge,
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
    /// Starts the stub on a free loopback port, answering mautrix-whatsapp's
    /// captured documents.
    pub async fn start() -> Result<Self> {
        Self::start_as(Bridge::Whatsapp).await
    }

    /// Starts a stub wearing the other reference bridge's face. Worth doing
    /// because the two do not agree: Signal offers one flow where WhatsApp
    /// offers two, its step ids are namespaced differently, and an unknown
    /// flow id is a `404` there and a silent fallback to QR on WhatsApp.
    pub async fn start_as(bridge: Bridge) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("failed to bind the stub bridge")?;
        let addr = listener.local_addr()?;
        let state = Arc::new(StubState {
            bridge,
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

    /// Which bridge's answers this stub serves.
    pub fn bridge(&self) -> Bridge {
        self.state.bridge
    }

    /// The QR step id this stub's captured answers use.
    pub fn qr_step_id(&self) -> &'static str {
        self.state.bridge.qr_step_id()
    }

    /// The QR payload prefix this bridge's network really uses — WhatsApp's
    /// `https://wa.me/...` versus Signal's `sgnl://linkdevice?...`. A test
    /// that wants to prove the payload reached the browser unchanged asserts
    /// against this rather than against a shape somebody imagined.
    pub fn qr_payload_prefix(&self) -> &'static str {
        match self.state.bridge {
            Bridge::Whatsapp => "https://wa.me/settings/linked_devices#",
            Bridge::Signal => "sgnl://linkdevice?",
        }
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
            inner.next_steps.clear();
            inner.refuse_step.clear();
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

    /// Answers the held blocking step with another **step**, which is what a
    /// bridge does when the network interjects a question mid-scan — a QR login
    /// by an account with two-factor authentication on.
    pub fn release_step(&self, body: Value) {
        self.queue(Release::Step { body });
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

    /// Makes the next `login/start` answer `200` with **this exact body**,
    /// whatever it is.
    ///
    /// # Why this does not break the fixture rule
    ///
    /// The rule this file lives under is that a shape which is not in a
    /// fixture does not go in here — because a stub that answers a document
    /// no bridge produces is how #106's three bugs got through. This method
    /// does not bend it: the stub still authors no provisioning document.
    /// The body comes from the calling test, and the test's whole point is
    /// that it is **not** a bridge's shape.
    ///
    /// That is the case #116 is about. A bridge that answers promptly and
    /// well, in JSON the Gateway cannot use, must be reported as a bridge
    /// that answered — not as one that could not be reached, which is the
    /// sentence that sent a live WhatsApp login's debugging session to look
    /// at containers and ports.
    pub fn answer_next_start_with(&self, body: Value) {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .misshape_start
            .push_back(body);
    }

    /// The same for `whoami`, which is the call the networks screen reads a
    /// link's state from (#108) — so a `whoami` this build cannot read is
    /// how a working WhatsApp link could be reported as a bridge that is
    /// down. See [`Self::answer_next_start_with`] for why an uncaptured
    /// shape belongs here.
    pub fn answer_next_whoami_with(&self, body: Value) {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .misshape_whoami
            .push_back(body);
    }

    /// Makes the next submit answer with another **step** instead of the
    /// completion, so a login of several questions can be driven.
    ///
    /// The document is the caller's, for the same reason
    /// [`Self::answer_next_start_with`]'s is: a shape no capture covers
    /// belongs in the test that needs it, where it is visible, rather than in
    /// this file where it would read as a bridge's own. The process id and a
    /// fresh transaction id are substituted here, because those are what a
    /// live bridge invents per call — and the transaction id matters: a
    /// bridge re-issues one with every answer and then validates it (#106).
    /// [`Self::queue_input_step`] is the shorthand for the ordinary case.
    pub fn queue_next_step(&self, body: Value) {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .next_steps
            .push_back(body);
    }

    /// Queues a `user_input` step asking for the given fields.
    ///
    /// Built on mautrix-whatsapp's **captured** `user_input` answer — its
    /// envelope, its `user_input.attachments: null`, its field shape — with
    /// the step id and the field list replaced, so a sequence a test invents
    /// is still made of a document a bridge really sent
    /// (`fixtures/*/login-start.json`, answer `phone`).
    pub fn queue_input_step(&self, step_id: &str, fields: Value) {
        let mut body = bridge_fixtures::body(self.state.bridge, "login-start", "phone");
        body["step_id"] = json!(step_id);
        body["user_input"]["fields"] = fields;
        self.queue_next_step(body);
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

    /// Caps how many logins this bridge will hold, so a start beyond the cap
    /// answers `403 FI.MAU.BRIDGE.TOO_MANY_LOGINS` — the network refusing
    /// another linked device. bridgev2 has this as `max_logins`; the
    /// reference deployment sets no cap, so it is the one refusal in this
    /// stub that no capture confirms.
    pub fn set_max_logins(&self, max: usize) {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .max_logins = Some(max);
    }

    /// Adds a login the stub pretends to already hold: what the Companion
    /// reads to offer "reconnect".
    ///
    /// The object is `whoami`'s shape, because that is the only call that
    /// describes a login. `GET /logins` answers bare id strings, so the name
    /// given here never comes back from *that* endpoint — which is the
    /// divergence #106 found.
    pub fn add_existing_login(&self, login_id: &str, name: &str) {
        self.state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .logins
            .push(json!({
                "id": login_id,
                "name": name,
                "profile": { "phone": name },
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

/// One of the error documents a real bridge gave, served with the status it
/// gave it under. Looked up by name in the fixture for the endpoint, so a
/// refusal is never invented either.
fn refusal(bridge: Bridge, endpoint: &str, name: &str) -> Response {
    let answer = bridge_fixtures::answer(bridge, endpoint, name);
    (
        StatusCode::from_u16(answer.status).expect("a captured status is a status"),
        Json(answer.body),
    )
        .into_response()
}

/// mautrix's own `{errcode, error}` for a code no capture covers — a test's
/// canned refusal, or one of the two codes bridgev2 defines that the
/// reference deployment never provoked.
fn mautrix_error(status: StatusCode, errcode: &str) -> Response {
    (
        status,
        Json(json!({ "errcode": errcode, "error": format!("the stub bridge answers {errcode}") })),
    )
        .into_response()
}

/// The bearer check every endpoint starts with, and the answer a real bridge
/// gives a bad one: `401 M_UNKNOWN_TOKEN`, not a 403.
///
/// A real bridge would also accept a Matrix access token; the reference
/// configuration turns that off (`provisioning.allow_matrix_auth: false`), so
/// the stub does not have it.
fn authorised(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        == Some(STUB_PROVISIONING_SECRET)
}

/// mautrix requires the acting user in `?user_id=` on **every** provisioning
/// call, and answers `403 M_FORBIDDEN "User does not have login permissions"`
/// without it — including on a step, a cancel and a logout, which is how a
/// real WhatsApp login failed while ten tests here passed (#106). The stub
/// refuses it too, so that cannot happen again.
fn acting_user_named(query: &HashMap<String, String>) -> bool {
    query
        .get("user_id")
        .is_some_and(|user_id| user_id.starts_with('@') && user_id.contains(':'))
}

/// The two checks every endpoint makes, in the order and with the answers a
/// real bridge makes them: the token first, then the acting user.
fn admitted(
    bridge: Bridge,
    endpoint: &str,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Option<Response> {
    if !authorised(headers) {
        // Every endpoint answers the same document for this, so whoami's
        // captured one stands for all of them.
        return Some(refusal(bridge, "whoami", "bad_secret"));
    }
    if !acting_user_named(query) {
        return Some(refusal(bridge, endpoint, "no_user_id"));
    }
    None
}

async fn flows(
    headers: HeaderMap,
    State(state): State<Arc<StubState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(refused) = admitted(state.bridge, "login-flows", &headers, &query) {
        return refused;
    }
    let name = match state.bridge {
        Bridge::Whatsapp => "two_flows",
        Bridge::Signal => "one_flow",
    };
    let mut body = bridge_fixtures::body(state.bridge, "login-flows", name);
    // The two flows no reference bridge offers, appended so the Gateway can
    // still be shown refusing a step type it cannot drive. Marked as the
    // stub's own in the flow name, so nobody mistakes them for captured.
    if let Some(list) = body.get_mut("flows").and_then(Value::as_array_mut) {
        list.push(json!({
            "id": COOKIES_FLOW,
            "name": "Paste cookies (stub-only, no captured bridge offers this)",
        }));
        list.push(json!({
            "id": WEBAUTHN_FLOW,
            "name": "Passkey (stub-only, no captured bridge offers this)",
        }));
    }
    Json(body).into_response()
}

async fn whoami(
    headers: HeaderMap,
    State(state): State<Arc<StubState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(refused) = admitted(state.bridge, "whoami", &headers, &query) {
        return refused;
    }
    let logins = {
        let mut inner = state.inner.lock().expect("the stub is not poisoned");
        if let Some(body) = inner.misshape_whoami.pop_front() {
            return Json(body).into_response();
        }
        inner.logins.clone()
    };
    // The captured document, with only the login list swapped for the one
    // this test set up: the network block, the flow list, the homeserver and
    // the bridge bot are the bridge's own words.
    let mut body = bridge_fixtures::body(state.bridge, "whoami", "no_logins");
    body["logins"] = Value::Array(logins);
    Json(body).into_response()
}

async fn logins(
    headers: HeaderMap,
    State(state): State<Arc<StubState>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(refused) = admitted(state.bridge, "logins", &headers, &query) {
        return refused;
    }
    let ids: Vec<Value> = state
        .inner
        .lock()
        .expect("the stub is not poisoned")
        .logins
        .iter()
        .filter_map(|login| login.get("id").cloned())
        .collect();
    // `login_ids`, bare strings, and nothing else — the shape the Gateway
    // used to reject, which made the reconnect list report every bridge
    // unreachable (#106). A name or a profile here would be a fiction: the
    // real endpoint has neither, and whoami is where they live.
    let mut body = bridge_fixtures::body(state.bridge, "logins", "no_logins");
    body["login_ids"] = Value::Array(ids);
    Json(body).into_response()
}

async fn start(
    headers: HeaderMap,
    State(state): State<Arc<StubState>>,
    Path(flow_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(refused) = admitted(state.bridge, "login-start", &headers, &query) {
        return refused;
    }
    let (process_id, txn_id, qr) = {
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
        // A `200` with a body the test wrote: the bridge answering promptly
        // and unusably, which is not the same failure as not answering.
        if let Some(body) = inner.misshape_start.pop_front() {
            return Json(body).into_response();
        }
        // The network refusing another linked device, before any process
        // exists. Uncaptured: bridgev2 defines it, the deployment set no cap.
        if inner
            .max_logins
            .is_some_and(|max| inner.logins.len() >= max)
        {
            return mautrix_error(StatusCode::FORBIDDEN, "FI.MAU.BRIDGE.TOO_MANY_LOGINS");
        }
        inner.next_process += 1;
        inner.next_qr += 1;
        inner.next_txn += 1;
        (
            format!("stub-process-{}", inner.next_process),
            format!("bls_stub-txn-{}", inner.next_txn),
            inner.next_qr,
        )
    };

    // An unknown flow id, which the two reference bridges disagree about:
    // Signal answers 404, and WhatsApp answers 200 with the QR flow. The
    // disagreement is served rather than smoothed over, because a flow-id
    // typo being invisible on one bridge is the sort of thing this stub
    // exists to expose.
    let flow_id = match flow_id.as_str() {
        QR_FLOW | PHONE_FLOW | COOKIES_FLOW | WEBAUTHN_FLOW => flow_id,
        _ if state.bridge == Bridge::Whatsapp => QR_FLOW.to_owned(),
        _ => return refusal(state.bridge, "login-start", "unknown_flow"),
    };
    // Signal has no phone flow at all, so asking for one is asking for a
    // flow it does not have.
    if flow_id == PHONE_FLOW && state.bridge == Bridge::Signal {
        return refusal(state.bridge, "login-start", "unknown_flow");
    }

    let answer = match flow_id.as_str() {
        QR_FLOW => qr_step(
            state.bridge,
            &process_id,
            &txn_id,
            &format!("{}stub-qr-payload-{qr}", prefix(state.bridge)),
        ),
        PHONE_FLOW => {
            let mut body = bridge_fixtures::body(state.bridge, "login-start", "phone");
            body["login_id"] = json!(process_id);
            body["txn_id"] = json!(txn_id);
            body
        }
        // The stub's own two, which no capture covers. bridgev2's declared
        // shapes; the Gateway refuses both without reading their payloads.
        COOKIES_FLOW => json!({
            "login_id": process_id,
            "txn_id": txn_id,
            "type": "cookies",
            "step_id": COOKIES_STEP,
            "instructions": "Paste the cookies from a private window",
            // bridgev2's own `LoginCookieField`: a field has no type of its
            // own — it carries `sources`, each with the type, the name the
            // value goes by in the browser and the domain (`mautrix/go`,
            // `bridgev2/login.go`). This stub used to invent a `type` on the
            // field itself, which is a shape no bridge sends, and #106 is
            // what that class of invention costs.
            "cookies": {
                "url": "https://messages.google.com/web/authentication",
                "fields": [{
                    "id": "SID",
                    "required": true,
                    "sources": [{
                        "type": "cookie",
                        "name": "SID",
                        "cookie_domain": ".google.com"
                    }]
                }]
            }
        }),
        _ => json!({
            "login_id": process_id,
            "txn_id": txn_id,
            "type": "webauthn",
            "step_id": WEBAUTHN_STEP,
            "instructions": "Use your passkey",
            "webauthn": { "publicKey": {} }
        }),
    };

    remember(&state, &process_id, &flow_id, &answer, &txn_id);
    Json(answer).into_response()
}

/// The QR payload prefix each network really uses, so a refreshed code the
/// test supplies without one still looks like the network's own.
fn prefix(bridge: Bridge) -> &'static str {
    match bridge {
        Bridge::Whatsapp => "https://wa.me/settings/linked_devices#",
        Bridge::Signal => "sgnl://linkdevice?pub_key=",
    }
}

/// The captured `display_and_wait` answer, with the three values a live
/// bridge would have made up fresh substituted.
///
/// Everything else is the bridge's own: the login **process** id travelling
/// as `login_id` at the top level (the same word mautrix's `?login_id=` query
/// parameter uses for an existing login — the trap of #106), the namespaced
/// step id, the connector's instructions, and a payload object with `type`
/// and `data` and nothing else. There is no `can_cancel`; the stub used to
/// invent one.
fn qr_step(bridge: Bridge, process_id: &str, txn_id: &str, data: &str) -> Value {
    let mut body = bridge_fixtures::body(bridge, "login-start", "qr");
    body["login_id"] = json!(process_id);
    body["txn_id"] = json!(txn_id);
    body["display_and_wait"]["data"] = json!(data);
    body
}

/// Records what step a process is now on, so the stub can refuse a call that
/// names the wrong one — as a real bridge does with `M_BAD_STATE`.
fn remember(state: &Arc<StubState>, process_id: &str, flow_id: &str, answer: &Value, txn_id: &str) {
    let mut inner = state.inner.lock().expect("the stub is not poisoned");
    inner.processes.insert(
        process_id.to_owned(),
        Process {
            flow_id: flow_id.to_owned(),
            step_id: answer["step_id"].as_str().unwrap_or_default().to_owned(),
            step_type: answer["type"].as_str().unwrap_or_default().to_owned(),
            txn_id: txn_id.to_owned(),
        },
    );
}

async fn step(
    headers: HeaderMap,
    State(state): State<Arc<StubState>>,
    Path((process_id, step_id, step_type)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
    body: String,
) -> Response {
    if let Some(refused) = admitted(state.bridge, "login-step", &headers, &query) {
        return refused;
    }
    // Every check a reference bridge was observed to make, in its order and
    // with its own errcode. A process the stub does not know comes first:
    // that is what a restarted bridge answers, and the one case the Gateway
    // must report as a lost login.
    let process = {
        let inner = state.inner.lock().expect("the stub is not poisoned");
        match inner.processes.get(&process_id) {
            Some(process) => process.clone(),
            None => return refusal(state.bridge, "login-step", "unknown_process"),
        }
    };
    if step_id != process.step_id {
        return refusal(state.bridge, "login-step", "wrong_step_id");
    }
    // The step cancel is not usable for a `display_and_wait` step: both
    // reference bridges answer `500 M_BAD_STATE: Login process does not
    // support cancelling steps`. What releases a held request is the process
    // cancel, and the held request then comes back `410`.
    if step_type == "cancel" {
        return refusal(state.bridge, "login-step", "step_cancel_unsupported");
    }
    if step_type != process.step_type {
        return refusal(state.bridge, "login-step", "wrong_step_type");
    }
    // The transaction id must be the one issued with the step being
    // advanced — and a bridge issues a fresh one with every answer, so the
    // caller has to echo the latest rather than the first. A caller that
    // invents one gets mautrix's own answer (#106): this is what a real
    // WhatsApp login failed on while the tests passed. Omitting it skips the
    // check, exactly as the real bridges do.
    if let Some(txn_id) = query.get("txn_id") {
        if txn_id != &process.txn_id {
            return refusal(state.bridge, "login-step", "wrong_txn_id");
        }
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
        return held_step(state, &process_id, &process.flow_id).await;
    }

    // A non-blocking step: a canned refusal if the test asked for one, then
    // the next step if the test queued one, and the completion otherwise.
    // Each queue is popped in a statement of its own, and never inside an
    // `if let` that still holds the guard: in edition 2021 the temporary
    // `MutexGuard` lives to the end of an `if let` block, so locking again in
    // the body deadlocks this stub against itself — which it did, and which
    // presented as one test of this suite sitting there for ever.
    let refused = state
        .inner
        .lock()
        .expect("the stub is not poisoned")
        .refuse_step
        .pop_front();
    if let Some((status, errcode)) = refused {
        // A connector's `400` **destroys the login process**: the capture is
        // explicit that every later call against it, the step cancel and the
        // process cancel included, answered `404 M_NOT_FOUND`
        // (`fixtures/*/login-step.json`, answer `rejected_user_input`). So
        // there is no retrying a refused step, and a stub that let one be
        // retried would be inviting the Companion to offer exactly that.
        //
        // Except the decoder's own `400`: bridgev2 answers `M_NOT_JSON` when
        // the body does not decode into its `map[string]string`, **before**
        // `doLoginStep` runs, and only a connector error reaches `deleteLogin`
        // (`bridgev2/matrix/provisioninglogin.go`). The process is untouched
        // and the same step is still waiting — which is what #221 needs a
        // Gateway to say, so it is what this stub does.
        let decoder_refusal = matches!(errcode.as_str(), "M_NOT_JSON" | "M_BAD_JSON");
        if status == 400 && !decoder_refusal {
            state
                .inner
                .lock()
                .expect("the stub is not poisoned")
                .processes
                .remove(&process_id);
        }
        return mautrix_error(
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            &errcode,
        );
    }
    let queued = state
        .inner
        .lock()
        .expect("the stub is not poisoned")
        .next_steps
        .pop_front();
    if let Some(mut queued) = queued {
        let txn_id = {
            let mut inner = state.inner.lock().expect("the stub is not poisoned");
            inner.next_txn += 1;
            format!("bls_stub-txn-{}", inner.next_txn)
        };
        queued["login_id"] = json!(process_id);
        queued["txn_id"] = json!(txn_id);
        remember(&state, &process_id, &process.flow_id, &queued, &txn_id);
        return Json(queued).into_response();
    }
    let login_id = format!("stub-login-{process_id}");
    complete(state, &process_id, &login_id)
}

/// The blocking step: it does not answer until the test releases it.
async fn held_step(state: Arc<StubState>, process_id: &str, flow_id: &str) -> Response {
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
        Release::RefreshedQr { data } => {
            // A fresh code comes with a fresh transaction id, as a real
            // bridge's refresh does — so a Gateway that echoed the id from
            // the *first* answer would be refused on the next call.
            let txn_id = {
                let mut inner = state.inner.lock().expect("the stub is not poisoned");
                inner.next_txn += 1;
                format!("bls_stub-txn-{}", inner.next_txn)
            };
            let answer = qr_step(state.bridge, process_id, &txn_id, &data);
            remember(&state, process_id, flow_id, &answer, &txn_id);
            Json(answer).into_response()
        }
        Release::Step { mut body } => {
            // A fresh transaction id, as every answer from a real bridge
            // carries — so a Gateway echoing the previous one is refused.
            let txn_id = {
                let mut inner = state.inner.lock().expect("the stub is not poisoned");
                inner.next_txn += 1;
                format!("bls_stub-txn-{}", inner.next_txn)
            };
            body["login_id"] = json!(process_id);
            body["txn_id"] = json!(txn_id);
            remember(&state, process_id, flow_id, &body, &txn_id);
            Json(body).into_response()
        }
        Release::Complete { login_id } => complete(state, process_id, &login_id),
        Release::Refusal { status, errcode } => mautrix_error(
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            &errcode,
        ),
    }
}

/// The completion step, and the login it leaves behind on the bridge.
///
/// **The one shape here no capture covers**: completing a login needs a human
/// with the account's phone, so this is bridgev2's declared `complete`
/// document (`tests/harness/fixtures/*/login-step.json`, answer `complete`)
/// and not a recorded one. It is therefore the one place in this stub where
/// the failure mode of #106 could still be hiding.
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
                "profile": { "phone": login_id },
            }));
        }
    }
    let mut body = bridge_fixtures::body(state.bridge, "login-step", "complete");
    body["complete"]["login_id"] = json!(login_id);
    body["complete"]["user_login_id"] = json!(login_id);
    Json(body).into_response()
}

async fn cancel_process(
    headers: HeaderMap,
    State(state): State<Arc<StubState>>,
    Path(process_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    // The acting user is required here too — the cancel is one of the two
    // calls that got `403 M_FORBIDDEN` from a real bridge while the stub
    // waved it through (#106).
    if let Some(refused) = admitted(state.bridge, "login-cancel", &headers, &query) {
        return refused;
    }
    let held = {
        let mut inner = state.inner.lock().expect("the stub is not poisoned");
        if inner.processes.remove(&process_id).is_none() {
            return refusal(state.bridge, "login-cancel", "already_gone");
        }
        inner.cancelled.push(process_id);
        inner.held
    };
    // Cancelling the process is what releases a held `display_and_wait`,
    // since the step cancel refuses — and the held request comes back
    // `410 FI.MAU.BRIDGE.LOGIN_CANCELLED`.
    for _ in 0..held {
        state
            .inner
            .lock()
            .expect("the stub is not poisoned")
            .releases
            .push_back(Release::Refusal {
                status: 410,
                errcode: "FI.MAU.BRIDGE.LOGIN_CANCELLED".to_owned(),
            });
    }
    if held > 0 {
        state.released.notify_waiters();
    }
    // `200 {}`, not `204`: what both reference bridges answer.
    let answer = bridge_fixtures::answer(state.bridge, "login-cancel", "cancelled");
    (
        StatusCode::from_u16(answer.status).expect("a captured status is a status"),
        Json(answer.body),
    )
        .into_response()
}

async fn logout(
    headers: HeaderMap,
    State(state): State<Arc<StubState>>,
    Path(login_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(refused) = admitted(state.bridge, "logout", &headers, &query) {
        return refused;
    }
    {
        let mut inner = state.inner.lock().expect("the stub is not poisoned");
        let before = inner.logins.len();
        inner.logins.retain(|login| login["id"] != json!(login_id));
        if inner.logins.len() == before {
            return refusal(state.bridge, "logout", "unknown_login");
        }
        inner.logged_out.push(login_id);
    }
    // Uncaptured: the only login on the reference deployment was the owner's.
    // See `fixtures/mautrix-whatsapp/logout.json`.
    let answer = bridge_fixtures::answer(state.bridge, "logout", "logged_out");
    (
        StatusCode::from_u16(answer.status).expect("a captured status is a status"),
        Json(answer.body),
    )
        .into_response()
}
