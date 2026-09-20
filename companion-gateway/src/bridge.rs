//! The bridge login provisioning facade: the Gateway speaks each bridge's
//! own provisioning API on the user's behalf, holds the blocking step of a
//! QR login itself, and keeps a pollable state the Companion reads (ticket
//! #55, spec #47).
//!
//! # Why the Gateway is in the middle at all
//!
//! Mautrix bridgev2 exposes provisioning on the bridge's own appservice
//! listener (`/_matrix/provision/v3/*`), and its QR step *blocks*: the caller
//! POSTs to the step and the request stays open until the phone answers, for
//! up to WhatsApp's whole QR budget. A browser cannot hold that request — a
//! phone that locks mid-scan kills it, and the login dies with it. So the
//! Gateway holds it:
//!
//! - the Companion **starts** a login, then **polls** a small document: the
//!   current step's type, its payload, and how long it is valid;
//!
//! - the Gateway keeps one task per bridge sitting in the blocking call. Each
//!   answer it gets — a refreshed QR, the next step, the completion — updates
//!   that document and bumps its [`LoginView::generation`], which is how the
//!   UI knows to redraw a code before the old one expires;
//!
//! - the browser never calls a bridge directly, even though mautrix's CORS
//!   would allow it: the provisioning secret is impersonation-grade
//!   (`docs/architecture/security-model.md`) and must not leave the server.
//!
//! # What does not survive a restart, stated rather than hidden
//!
//! The login process lives in the **bridge's** memory, with a 30-minute cap
//! ([`MAX_LOGIN_LIFETIME`]), and the held step lives in **this** process's
//! memory. A restart of either side loses a login in flight: the Gateway
//! reports it as `failed` with [`LoginFailure::LoginLost`], and the user
//! starts again. Nothing is persisted, which is also the point — see below.
//!
//! # Credentials pass through and are never stored
//!
//! A QR payload, a pairing code, a phone number, the seven Google cookies of
//! the SMS preview path: all of them are network credentials (ADR 0011). They
//! travel in the request or the polled document and are held nowhere else —
//! no SQLite table in this module, no log line carrying a payload, no error
//! detail quoting one. What the logs get is the bridge, the step type and the
//! step id; [`redacted`] is what the step payloads go through before anything
//! is said about them.
//!
//! # One login at a time per bridge
//!
//! The v0.1 contract has no login dimension (spec #47), so a bridge instance
//! holds one login. Starting a second while one is in flight is refused with
//! the device and the time that started the first, so the user can tell
//! "my other phone is mid-scan" from "the server is stuck".
//!
//! "Reconnect" is the same start with the existing login id
//! ([`StartLogin::login_id`], mautrix's `?login_id=`): it repairs a broken
//! session in place and never restarts a container.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::consent::rfc3339_millis;
use crate::session::Device;

/// The prefix every provisioning endpoint lives under, on the bridge's own
/// appservice listener.
const PROVISION_PREFIX: &str = "_matrix/provision/v3";

/// How long a login process can live, as bridgev2 caps it. The Gateway does
/// not enforce it — the bridge does — but it is what the pollable state
/// reports as the process deadline, so the Companion can say "start again"
/// instead of polling a corpse.
pub const MAX_LOGIN_LIFETIME: Duration = Duration::from_secs(30 * 60);

/// How long one QR code is worth drawing. The bridge states no expiry on a
/// `display_and_wait` step, and WhatsApp's total QR budget is about 2m40
/// across refreshes — roughly 20 seconds per code. So this is the Gateway's
/// own estimate, reported as such: the authoritative signal is a bumped
/// [`LoginView::generation`], which means a fresh code has already arrived.
pub const QR_CODE_VALIDITY: Duration = Duration::from_secs(20);

/// How long the Gateway waits on a *non-blocking* provisioning call (flows,
/// start, cancel, logout, the logins list). Short: these answer immediately
/// or the bridge is in trouble.
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the Gateway is willing to sit in a blocking step. Longer than
/// the whole QR budget and shorter than the process cap, so the held request
/// never outlives the process it belongs to.
const BLOCKING_STEP_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How long `GET /api/bridges` will wait on one bridge's `whoami` before
/// answering "cannot tell" for it (#108).
///
/// Shorter than [`BRIDGE_TIMEOUT`] on purpose. That one is what a user's own
/// click is allowed to cost; this one is on the path of a *screen drawing*,
/// and the networks picker must not sit blank because one bridge is wedged.
/// The read is concurrent across bridges, so this is the whole budget and not
/// a per-bridge share of one.
pub const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// The step types bridgev2 can hand back. `Complete` ends the flow; the two
/// input types wait for the browser; `DisplayAndWait` is the blocking one the
/// Gateway holds; `ClientHttp` and `Webauthn` are the two the Companion
/// cannot drive, and they fail loudly instead of hanging (spec #47).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepType {
    UserInput,
    Cookies,
    DisplayAndWait,
    ClientHttp,
    Webauthn,
    Complete,
}

impl StepType {
    pub fn as_str(self) -> &'static str {
        match self {
            StepType::UserInput => "user_input",
            StepType::Cookies => "cookies",
            StepType::DisplayAndWait => "display_and_wait",
            StepType::ClientHttp => "client_http",
            StepType::Webauthn => "webauthn",
            StepType::Complete => "complete",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "user_input" => StepType::UserInput,
            "cookies" => StepType::Cookies,
            "display_and_wait" => StepType::DisplayAndWait,
            "client_http" => StepType::ClientHttp,
            "webauthn" => StepType::Webauthn,
            "complete" => StepType::Complete,
            _ => return None,
        })
    }

    /// Whether the Gateway holds this step itself (rather than waiting for
    /// the browser to submit something).
    fn blocks(self) -> bool {
        matches!(self, StepType::DisplayAndWait)
    }
}

/// Where a login has got to, as the Companion polls it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginPhase {
    /// The step needs something from the user: a phone number, a set of
    /// cookies. The Companion submits it.
    AwaitingInput,
    /// The Gateway is holding the blocking step: the user is scanning, and
    /// the browser has nothing to do but poll.
    AwaitingRemote,
    /// The network accepted the login. [`LoginView::login`] names it.
    Complete,
    /// The login failed. [`LoginView::error`] says why.
    Failed,
    /// Somebody cancelled it — this device, or the remote side.
    Cancelled,
}

impl LoginPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            LoginPhase::AwaitingInput => "awaiting_input",
            LoginPhase::AwaitingRemote => "awaiting_remote",
            LoginPhase::Complete => "complete",
            LoginPhase::Failed => "failed",
            LoginPhase::Cancelled => "cancelled",
        }
    }

    /// Whether this phase can be replaced by a new login on the same bridge.
    fn is_terminal(self) -> bool {
        matches!(
            self,
            LoginPhase::Complete | LoginPhase::Failed | LoginPhase::Cancelled
        )
    }
}

/// Why a login ended badly. A closed set of codes the Companion branches on;
/// no variant carries a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginFailure {
    /// The bridge forgot the process, or the Gateway lost the held request:
    /// a restart of either side, which a login in flight does not survive.
    LoginLost,
    /// The process ran out of time — the bridge's own 30-minute cap, or the
    /// network's QR budget.
    Expired,
    /// The flow asked for a passkey. The Companion cannot drive WebAuthn, so
    /// the login fails here rather than hanging on a step nobody will answer
    /// (mautrix's `provisioning.fail_on_webauthn` makes the bridge refuse it
    /// one step earlier, which is the setting to prefer).
    WebauthnRequired,
    /// The flow asked for a step type this facade does not drive.
    UnsupportedStep,
    /// The bridge refused, with its own error code and an HTTP status.
    BridgeRefused,
    /// The bridge could not be reached at all.
    BridgeUnreachable,
    /// The bridge **answered**, and the Gateway could not use the answer.
    /// The bridge is running; this is a defect in Twalk's reading of that
    /// bridge's provisioning API. See [`BridgeRefusal::BridgeAnswerUnusable`].
    BridgeAnswerUnusable,
}

impl LoginFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            LoginFailure::LoginLost => "login_lost",
            LoginFailure::Expired => "login_expired",
            LoginFailure::WebauthnRequired => "webauthn_required",
            LoginFailure::UnsupportedStep => "unsupported_step",
            LoginFailure::BridgeRefused => "bridge_refused",
            LoginFailure::BridgeUnreachable => "bridge_unreachable",
            LoginFailure::BridgeAnswerUnusable => "bridge_answer_unusable",
        }
    }
}

/// One endpoint of a bridge's provisioning API, as the Gateway calls it.
///
/// It exists so that a refusal can say **which call** it is about without
/// anybody having to remember to pass a label alongside the path segments:
/// the call is the method and the endpoint, and [`call_bridge`] takes it
/// instead of a [`reqwest::Method`]. The strings are the ones
/// `tests/harness/fixtures/*/*.json` key their captured answers on, so the
/// endpoint a refusal names is the endpoint whose recorded answers a reader
/// can go and look at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisioningCall {
    Flows,
    Logins,
    Whoami,
    Start,
    Step,
    CancelStep,
    CancelProcess,
    Logout,
}

impl ProvisioningCall {
    /// The method this endpoint is called with. Derived rather than passed,
    /// so a call site cannot name one endpoint and use another's verb.
    fn method(self) -> reqwest::Method {
        match self {
            ProvisioningCall::Flows | ProvisioningCall::Logins | ProvisioningCall::Whoami => {
                reqwest::Method::GET
            }
            _ => reqwest::Method::POST,
        }
    }

    /// The endpoint as `fixtures/README.md` writes it: what a refusal names,
    /// and what a reader greps the fixture corpus for.
    pub fn endpoint(self) -> &'static str {
        match self {
            ProvisioningCall::Flows => "GET /_matrix/provision/v3/login/flows",
            ProvisioningCall::Logins => "GET /_matrix/provision/v3/logins",
            ProvisioningCall::Whoami => "GET /_matrix/provision/v3/whoami",
            ProvisioningCall::Start => "POST /_matrix/provision/v3/login/start/{flow}",
            ProvisioningCall::Step => {
                "POST /_matrix/provision/v3/login/step/{process}/{step}/{type}"
            }
            ProvisioningCall::CancelStep => {
                "POST /_matrix/provision/v3/login/step/{process}/{step}/cancel"
            }
            ProvisioningCall::CancelProcess => "POST /_matrix/provision/v3/login/cancel/{process}",
            ProvisioningCall::Logout => "POST /_matrix/provision/v3/logout/{login_id}",
        }
    }

    /// A stable one-word label for a log line or a metric.
    pub fn label(self) -> &'static str {
        match self {
            ProvisioningCall::Flows => "login_flows",
            ProvisioningCall::Logins => "logins",
            ProvisioningCall::Whoami => "whoami",
            ProvisioningCall::Start => "login_start",
            ProvisioningCall::Step => "login_step",
            ProvisioningCall::CancelStep => "login_step_cancel",
            ProvisioningCall::CancelProcess => "login_cancel",
            ProvisioningCall::Logout => "logout",
        }
    }
}

/// One configured bridge instance: what the Gateway needs to speak to it.
///
/// [`Debug`] is written by hand rather than derived, and prints no secret:
/// this struct is the kind of thing that ends up in a `?config` log line one
/// day, and the provisioning secret drives logins on the user's account.
#[derive(Clone)]
pub struct BridgeConfig {
    /// The Matrix user every provisioning call acts as, in mautrix's
    /// `?user_id=` query parameter. mautrix requires it on **every**
    /// request, not only on the ones that start something: a step or a
    /// cancel without it answers `403 M_FORBIDDEN` (#106). Keeping it here
    /// rather than at each call site is deliberate — it is how a later call
    /// cannot forget it.
    pub acting_as: String,
    /// The instance's id, declared in configuration
    /// (`GATEWAY_BRIDGES`), e.g. `mautrix-whatsapp`. Never a network value
    /// (CONTEXT.md): it identifies the implementation, and the v0.1
    /// constraint is one login per instance.
    pub bridge_id: String,
    /// The `bridge_id` this instance's **events** carry (ticket #56): the
    /// contract's `^bridge-[a-z0-9-]+$`, e.g. `bridge-whatsapp`. It is also
    /// the segment of the status webhook's URL, which is what an operator
    /// writes into the bridge's `homeserver.status_endpoint`.
    ///
    /// Separate from [`Self::bridge_id`] because the two identify different
    /// things: that one is the software instance an operator configured
    /// (`mautrix-whatsapp`) and names the Gateway's own API routes, this one
    /// is the identity the event contract froze and third parties code
    /// against. Derived from the instance id by default, so a deployment
    /// sets nothing; stable across restarts either way, which is the
    /// property the contract needs.
    pub status_bridge_id: String,
    /// The network the user experiences, e.g. `whatsapp` — what the
    /// Companion labels the screen with.
    pub network: String,
    /// Base URL of the bridge's appservice listener, e.g.
    /// `http://bridge-whatsapp:29318`. Without a trailing slash.
    pub base_url: String,
    /// The bridge's provisioning shared secret. It drives logins and logouts
    /// on the user's account, so it stays on this side: never in a response,
    /// never in a log line.
    pub provisioning_secret: String,
    /// The bridge's appservice token, which is what it authenticates its
    /// **status pushes** with (ticket #56) — the Gateway holds it to verify
    /// them, and for nothing else: it never acts as the appservice.
    ///
    /// `None` when the operator configured none, in which case that bridge's
    /// webhook is refused rather than trusted. Trusting the compose network
    /// instead was explicitly rejected — the same reasoning that gave the
    /// Sensor a service token (`docs/architecture/security-model.md`).
    pub as_token: Option<String>,
    /// The Matrix ID of this bridge's own **bot** — the account that is in
    /// every portal room the bridge built and has the power to invite in them
    /// (`GATEWAY_BRIDGE_<ID>_BOT_USER_ID`, ticket #171).
    ///
    /// Configured, never derived. An appservice token used with no
    /// `?user_id=` acts as the registration's `sender_localpart`, and a
    /// generated registration's sender is a random localpart joined to
    /// nothing — so the register read 32 conversations as an account in zero
    /// rooms and reported zero without a word (#171). The right asker is the
    /// bot, whose localpart is the bridge's own `bridge.bot_username` and is
    /// therefore the operator's to state: ADR 0018 refused to string-build
    /// the owner's ghosts for the same reason, and a Gateway that guessed
    /// `@{bridge_id}bot:` would be wrong on any bridge that renamed its bot.
    ///
    /// `None` when the operator configured none: the register then acts as
    /// the token's own identity — correct for an ordinary access token — and
    /// says in its answer which account that turned out to be.
    pub bot_user_id: Option<String>,
}

impl std::fmt::Debug for BridgeConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BridgeConfig")
            .field("bridge_id", &self.bridge_id)
            .field("status_bridge_id", &self.status_bridge_id)
            .field("network", &self.network)
            .field("base_url", &self.base_url)
            .field("provisioning_secret", &"<redacted>")
            .field(
                "as_token",
                &self
                    .as_token
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("None"),
            )
            .finish()
    }
}

/// The device that started a login, as the refusal of a second one names it.
#[derive(Debug, Clone)]
pub struct StartedBy {
    pub device_id: String,
    pub device_name: String,
}

/// The login the network accepted.
#[derive(Debug, Clone)]
pub struct CompletedLogin {
    pub login_id: String,
    /// The Matrix ID of the user login the bridge created, when it names
    /// one.
    pub user_id: Option<String>,
}

/// One step, as the Companion has to render it.
#[derive(Debug, Clone)]
pub struct StepView {
    pub step_id: String,
    /// The transaction id the bridge issued with this step, echoed on the
    /// call that advances it (#106).
    pub txn_id: Option<String>,
    pub step_type: StepType,
    /// The bridge's own instructions for the user, when it gives any.
    pub instructions: Option<String>,
    /// The step's payload, passed through from the bridge: for a QR step
    /// `{type: "qr", data: "…"}`, whose `data` is the raw payload the
    /// browser draws — the bridge renders no image. A credential in flight,
    /// held only here and only until the next step replaces it.
    pub payload: Value,
    pub received_at: SystemTime,
    /// How long this step is worth acting on, from [`Self::received_at`].
    pub valid_for: Duration,
}

/// The whole pollable state of one login.
#[derive(Debug, Clone)]
pub struct LoginView {
    pub bridge_id: String,
    pub network: String,
    pub process_id: String,
    pub flow_id: String,
    /// The existing login this flow re-logs in to, when it is a reconnect.
    pub login_id: Option<String>,
    pub phase: LoginPhase,
    pub started_at: SystemTime,
    pub started_by: StartedBy,
    /// The bridge's own deadline for the process.
    pub expires_at: SystemTime,
    /// Bumped on every step the bridge hands back, refreshes included. The
    /// Companion redraws when it changes — that, not a clock, is the signal
    /// that a fresh QR code has arrived.
    pub generation: u64,
    pub step: Option<StepView>,
    pub login: Option<CompletedLogin>,
    pub error: Option<LoginFailure>,
    /// A human-readable note for the operator's logs, never for display.
    pub detail: Option<String>,
}

/// What the Companion asks for when it starts a login.
#[derive(Debug, Clone)]
pub struct StartLogin {
    pub flow_id: String,
    /// Reconnect: restart the flow against this existing login id, which is
    /// how a broken session is repaired.
    pub login_id: Option<String>,
}

/// Why a provisioning call was refused. The HTTP status and the error code
/// the Companion sees are [`crate::bridge_http`]'s translation of this.
#[derive(Debug)]
pub enum BridgeRefusal {
    /// No bridge with that id is configured.
    UnknownBridge { bridge_id: String },
    /// A login is already in flight on this bridge. Names the device and
    /// the time that started it — one login per instance in v0.1.
    LoginInFlight {
        started_by: StartedBy,
        started_at: SystemTime,
    },
    /// No login is in flight on this bridge, so there is nothing to poll,
    /// submit to or cancel.
    NoLoginInFlight,
    /// The request does not fit the flow's current step.
    InvalidRequest { detail: String },
    /// The network will not accept another login
    /// (`FI.MAU.BRIDGE.TOO_MANY_LOGINS`).
    TooManyLogins,
    /// The login is over: cancelled, timed out or already finished
    /// (mautrix's three 410s).
    LoginExpired { errcode: String },
    /// The step was cancelled under us (`FI.MAU.LOGIN_STEP_CANCELLED`).
    StepCancelled,
    /// The bridge does not know what the request named: a login process it
    /// has forgotten, a login id that is not one of its own.
    NotFoundOnBridge { errcode: String },
    /// The bridge refused, with its own error code.
    BridgeRefused { errcode: String, status: u16 },
    /// **The bridge could not read what this Gateway sent it** (issue #221):
    /// `400 M_NOT_JSON`, which bridgev2 answers when the submit body does not
    /// decode into its `map[string]string` — and answers **before** the
    /// connector runs, so the login process is untouched and the network saw
    /// nothing. The mirror of [`Self::BridgeAnswerUnusable`]: that one is a
    /// defect in Twalk's reading of the bridge, this one in its writing, and
    /// neither is the user's, the network's or the deployment's.
    ///
    /// Its own variant because the alternative was the defect itself: read
    /// as a `400`, it rendered *"the network refused what was submitted,
    /// start the login again"* — four wrong sentences, the last of which sent
    /// the user round the same paste three times.
    RequestUnreadable { errcode: String },
    /// **Nothing answered.** The connection was refused, timed out, found no
    /// route, or died mid-body. The bridge is down, or the Gateway is
    /// pointed at the wrong address.
    ///
    /// This is the refusal an operator should go and look at containers,
    /// ports and networking for — and it is the *only* one. See
    /// [`Self::BridgeAnswerUnusable`] for the other half of what this
    /// variant used to mean.
    BridgeUnreachable { detail: String },
    /// **The bridge answered, and the Gateway could not use the answer.**
    ///
    /// The connection was fine and the bridge replied — promptly, usually
    /// with a `200` — and this build then looked for something in that
    /// answer and did not find it. That is a defect in Twalk's reading of
    /// the bridge's provisioning API, not a broken deployment, and the two
    /// must never again arrive under one name: the first live WhatsApp login
    /// (#106) failed here, was reported as "could not reach this network's
    /// bridge", and cost a debugging session spent on networking, containers
    /// and ports — the one place the fault was not.
    ///
    /// # Why both members are what they are
    ///
    /// [`ProvisioningCall`] is a closed set and `looked_for` is a
    /// `&'static str`: **neither can carry a byte of the bridge's answer**.
    /// That is deliberate and it is the acceptance criterion. A bridge's
    /// answer can hold identifiers from a network account — a phone number
    /// as a login id, a display name, a QR payload — so a refusal names what
    /// was *missing*, never what was received. The answer's shape goes to
    /// the operator's log through [`redacted`]; nothing of it reaches the
    /// browser.
    ///
    /// Adding a case therefore means adding a `&'static str` literal. There
    /// is no `format!` to reach for, which is the point.
    BridgeAnswerUnusable {
        /// Which provisioning call answered.
        call: ProvisioningCall,
        /// What the Gateway was looking for in that answer and did not find
        /// — a field name or a shape this build knows about, never a value
        /// the bridge sent.
        looked_for: &'static str,
    },
}

impl BridgeRefusal {
    /// The stable label for logs and the login metric's `outcome` label. A
    /// closed set: never anything derived from a request.
    pub fn label(&self) -> &'static str {
        match self {
            BridgeRefusal::UnknownBridge { .. } => "unknown_bridge",
            BridgeRefusal::LoginInFlight { .. } => "login_in_flight",
            BridgeRefusal::NoLoginInFlight => "no_login_in_flight",
            BridgeRefusal::InvalidRequest { .. } => "invalid_request",
            BridgeRefusal::TooManyLogins => "too_many_logins",
            BridgeRefusal::LoginExpired { .. } => "login_expired",
            BridgeRefusal::StepCancelled => "step_cancelled",
            BridgeRefusal::NotFoundOnBridge { .. } => "not_found_on_bridge",
            BridgeRefusal::BridgeRefused { .. } => "bridge_refused",
            BridgeRefusal::RequestUnreadable { .. } => "bridge_request_unusable",
            BridgeRefusal::BridgeUnreachable { .. } => "bridge_unreachable",
            BridgeRefusal::BridgeAnswerUnusable { .. } => "bridge_answer_unusable",
        }
    }
}

/// One login flow the bridge offers, as `GET /v3/login/flows` describes it.
#[derive(Debug, Clone)]
pub struct Flow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// One configured bridge, with both of the things the networks screen has to
/// keep apart (#108):
///
/// - [`Self::login`], the login **process** this Gateway may have in flight —
///   a QR scan somebody is in the middle of, in memory, gone on a restart;
/// - [`Self::logins`], the logins the **bridge** holds — the persistent
///   links, with the network's credentials behind them, which is what the
///   user means by "my WhatsApp is connected".
///
/// Keeping them in one struct with two names is the point: the defect this
/// ticket fixes was reading the first where the second was meant.
pub struct BridgeConnection {
    pub config: BridgeConfig,
    /// The login process in flight, or `None`. Never an answer to "is this
    /// network connected".
    pub login: Option<LoginView>,
    /// `whoami`'s `logins` array, or the refusal that means the bridge could
    /// not be asked. `Err` is "we do not know", which the Companion shows as
    /// such — it is not `disconnected`.
    pub logins: Result<Vec<Value>, BridgeRefusal>,
}

/// One login the bridge already holds, as `GET /v3/logins` lists it.
#[derive(Debug, Clone)]
pub struct ExistingLogin {
    pub login_id: String,
    pub name: Option<String>,
    pub profile: Value,
}

/// Where the Gateway keeps the login in flight for one bridge.
enum Slot {
    Idle,
    Active(Arc<ActiveLogin>),
}

/// One bridge instance, and the one login it may have in flight.
struct Bridge {
    config: BridgeConfig,
    /// The login slot. A `std` mutex held for the length of a comparison —
    /// never across an await.
    slot: Mutex<Slot>,
    /// Serialises the *starting* of a login, which spans a call to the
    /// bridge. Reads of the slot never touch it, so polling is never stuck
    /// behind a start.
    start_gate: tokio::sync::Mutex<()>,
}

/// A login in flight: what the holding task writes and the poll reads.
struct ActiveLogin {
    view: Mutex<LoginView>,
    /// Bumped by the holding task; the view's `generation` is a copy of it.
    generation: AtomicU64,
    /// Set once a cancel has been asked for, so the holding task stops
    /// writing over the cancelled state when its request finally returns.
    cancelled: std::sync::atomic::AtomicBool,
}

impl ActiveLogin {
    fn read(&self) -> LoginView {
        self.view
            .lock()
            .expect("the login view mutex is never poisoned")
            .clone()
    }

    fn write(&self, update: impl FnOnce(&mut LoginView)) {
        let mut view = self
            .view
            .lock()
            .expect("the login view mutex is never poisoned");
        update(&mut view);
    }
}

/// The facade: every configured bridge, and the HTTP clients it speaks to
/// them with.
pub struct Bridges {
    bridges: Vec<Arc<Bridge>>,
    clients: Clients,
}

/// What a provisioning call needs, and nothing else: the two clients and the
/// clock. A spawned holding task carries this rather than the bridge list,
/// so the same call and the same state transitions serve both sides.
#[derive(Clone)]
struct Clients {
    /// For the calls that answer immediately.
    http: reqwest::Client,
    /// For the blocking step, which must be allowed to sit there.
    blocking_http: reqwest::Client,
    now: fn() -> SystemTime,
}

impl Bridges {
    /// Builds the facade from configuration. Fails when two bridges share an
    /// id, or when a bridge's base URL is not one — an operator error to fix
    /// at startup, not a 502 to discover later.
    pub fn new(configured: Vec<BridgeConfig>) -> Result<Self> {
        Self::with_clock(configured, SystemTime::now)
    }

    pub fn with_clock(configured: Vec<BridgeConfig>, now: fn() -> SystemTime) -> Result<Self> {
        let mut bridges = Vec::with_capacity(configured.len());
        for mut config in configured {
            anyhow::ensure!(
                !bridges
                    .iter()
                    .any(|existing: &Arc<Bridge>| existing.config.bridge_id == config.bridge_id),
                "two bridges are configured with the id {:?}: a bridge_id identifies one \
                 instance (CONTEXT.md)",
                config.bridge_id
            );
            config.base_url = config.base_url.trim_end_matches('/').to_owned();
            let parsed = reqwest::Url::parse(&config.base_url).with_context(|| {
                format!(
                    "the base URL configured for bridge {:?} is not a URL: {:?}",
                    config.bridge_id, config.base_url
                )
            })?;
            // `host:port` parses as a URL whose *scheme* is the host, which
            // would then fail as a 502 the first time a user pressed the
            // button. Refuse it here, where the operator can read why.
            anyhow::ensure!(
                matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some(),
                "the base URL configured for bridge {:?} is not an http(s) URL with a host: \
                 {:?} — the bridge's appservice listener, e.g. http://bridge-whatsapp:29318",
                config.bridge_id,
                config.base_url
            );
            bridges.push(Arc::new(Bridge {
                config,
                slot: Mutex::new(Slot::Idle),
                start_gate: tokio::sync::Mutex::new(()),
            }));
        }
        let http = reqwest::Client::builder()
            .timeout(BRIDGE_TIMEOUT)
            .build()
            .map_err(|error| anyhow::anyhow!("{}", error.without_url()))?;
        let blocking_http = reqwest::Client::builder()
            .timeout(BLOCKING_STEP_TIMEOUT)
            .build()
            .map_err(|error| anyhow::anyhow!("{}", error.without_url()))?;
        Ok(Self {
            bridges,
            clients: Clients {
                http,
                blocking_http,
                now,
            },
        })
    }

    /// The configured bridges, in configuration order, with the login each
    /// has in flight. Reads no bridge: a list the Companion can draw while
    /// every bridge is down.
    pub fn list(&self) -> Vec<(BridgeConfig, Option<LoginView>)> {
        self.bridges
            .iter()
            .map(|bridge| {
                let login = match &*bridge
                    .slot
                    .lock()
                    .expect("the slot mutex is never poisoned")
                {
                    Slot::Idle => None,
                    Slot::Active(active) => Some(active.read()),
                };
                (bridge.config.clone(), login)
            })
            .collect()
    }

    /// The same list, with each bridge's **`whoami`** answer beside it: the
    /// logins it actually holds, which is what "is this network connected?"
    /// is a question about (#108).
    ///
    /// # Why this one does contact the bridges
    ///
    /// [`Self::list`] deliberately does not, and for a while nothing did —
    /// which is how the Companion ended up computing a network's connected
    /// badge from [`LoginView::phase`], the state of a login *process* living
    /// in this process's memory. Starting a login cleared the badge.
    /// Cancelling one cleared it. Restarting the Gateway cleared it. All
    /// three while the bridge reported `logins: 1, CONNECTED` throughout.
    ///
    /// A login is persistent and the bridge is the only thing that knows
    /// about it, so the answer has to be asked for. It is asked for the way a
    /// screen can afford:
    ///
    /// - **every bridge at once**, not one after another, so the cost is one
    ///   bridge's latency and not the sum of them;
    /// - **on a short leash** ([`CONNECTION_TIMEOUT`], well under the
    ///   15-second timeout a login call gets), because this is a page's
    ///   first paint and not a user-initiated action;
    /// - **and never fatally**: a bridge that cannot be reached comes back as
    ///   `Err`, the other bridges still answer, and the networks screen still
    ///   draws. A Gateway that could not ask says so; it does not report
    ///   `disconnected`, which would be the same lie in a new place.
    pub async fn connections(&self, acting_as: &str) -> Vec<BridgeConnection> {
        let reads = self.bridges.iter().map(|bridge| async move {
            let login = match &*bridge
                .slot
                .lock()
                .expect("the slot mutex is never poisoned")
            {
                Slot::Idle => None,
                Slot::Active(active) => Some(active.read()),
            };
            let query = [("user_id", acting_as)];
            let call = call_bridge(
                &bridge.config,
                &self.clients.http,
                ProvisioningCall::Whoami,
                &["whoami"],
                &query,
                None,
            );
            let logins = match tokio::time::timeout(CONNECTION_TIMEOUT, call).await {
                Ok(Ok(body)) => whoami_logins(&body),
                Ok(Err(refusal)) => Err(refusal),
                Err(_elapsed) => Err(BridgeRefusal::BridgeUnreachable {
                    detail: format!(
                        "the bridge did not answer whoami within {}s",
                        CONNECTION_TIMEOUT.as_secs()
                    ),
                }),
            };
            if let Err(refusal) = &logins {
                debug!(
                    bridge = %bridge.config.bridge_id,
                    outcome = refusal.label(),
                    "could not read a bridge's logins for the networks list; \
                     the list still answers, and says the state is unknown"
                );
            }
            BridgeConnection {
                config: bridge.config.clone(),
                login,
                logins,
            }
        });
        futures::future::join_all(reads).await
    }

    fn bridge(&self, bridge_id: &str) -> Result<&Arc<Bridge>, BridgeRefusal> {
        self.bridges
            .iter()
            .find(|bridge| bridge.config.bridge_id == bridge_id)
            .ok_or_else(|| BridgeRefusal::UnknownBridge {
                bridge_id: bridge_id.to_owned(),
            })
    }

    /// `GET /_matrix/provision/v3/login/flows` — the flows this bridge
    /// offers, for the screen that asks "how do you want to connect?".
    pub async fn flows(
        &self,
        bridge_id: &str,
        acting_as: &str,
    ) -> Result<Vec<Flow>, BridgeRefusal> {
        let bridge = self.bridge(bridge_id)?;
        let body = self
            .call(
                bridge,
                &self.clients.http,
                ProvisioningCall::Flows,
                &["login", "flows"],
                &[("user_id", acting_as)],
                None,
            )
            .await?;
        let flows = body
            .get("flows")
            .and_then(Value::as_array)
            .ok_or_else(|| unusable(ProvisioningCall::Flows, "a `flows` array", &body))?;
        Ok(flows
            .iter()
            .filter_map(|flow| {
                Some(Flow {
                    id: flow.get("id")?.as_str()?.to_owned(),
                    name: flow
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    description: flow
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect())
    }

    /// `GET /_matrix/provision/v3/logins` — the ids of the logins this
    /// bridge already holds. What the Companion reads to offer "reconnect"
    /// against an existing login id.
    ///
    /// # This endpoint answers ids, and only ids
    ///
    /// Its name invites the assumption that it lists *logins*. It does not.
    /// Both reference bridges answer
    ///
    /// ```text
    /// {"login_ids":["33612345678"]}
    /// ```
    ///
    /// — bare strings, under `login_ids`. There is no name here, no profile,
    /// and no connection state, so [`ExistingLogin::name`] comes back `None`
    /// and [`ExistingLogin::profile`] `null` against a real bridge.
    ///
    /// **For a login's name, profile or state, call [`Self::whoami`]**, whose
    /// `logins` array carries the whole object — `id`, `name`, `profile`, and
    /// the nested `state` document the status webhook also pushes. That is the
    /// only call that has them, which is also why it is the only honest source
    /// of "is this network connected?" (#108).
    ///
    /// The Gateway used to read this answer as a list of objects, found
    /// neither a bare array nor a `logins` member, and reported the bridge
    /// unreachable — so the reconnect list never worked at all (#106). The
    /// two object shapes below are kept as a courtesy to a bridge that grows
    /// one; the id-only shape is the one the reference bridges send.
    pub async fn logins(
        &self,
        bridge_id: &str,
        acting_as: &str,
    ) -> Result<Vec<ExistingLogin>, BridgeRefusal> {
        let bridge = self.bridge(bridge_id)?;
        let body = self
            .call(
                bridge,
                &self.clients.http,
                ProvisioningCall::Logins,
                &["logins"],
                &[("user_id", acting_as)],
                None,
            )
            .await?;
        // What the reference bridges send: ids under `login_ids`.
        if let Some(ids) = body.get("login_ids").and_then(Value::as_array) {
            return Ok(ids
                .iter()
                .filter_map(|id| {
                    Some(ExistingLogin {
                        login_id: id.as_str()?.to_owned(),
                        // Not in this answer. See whoami.
                        name: None,
                        profile: Value::Null,
                    })
                })
                .collect());
        }
        // A bare array, or an object wrapping one, of login objects: not
        // what mautrix v26.09 sends, kept because a bridge that answers
        // either is still answering the question.
        let logins = match body.as_array() {
            Some(logins) => logins.clone(),
            None => body
                .get("logins")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| {
                    unusable(
                        ProvisioningCall::Logins,
                        "a `login_ids` array, or a list of logins",
                        &body,
                    )
                })?,
        };
        Ok(logins
            .iter()
            .filter_map(|login| {
                // A bare array of id strings, which is the same contract as
                // `login_ids` without the wrapper.
                if let Some(id) = login.as_str() {
                    return Some(ExistingLogin {
                        login_id: id.to_owned(),
                        name: None,
                        profile: Value::Null,
                    });
                }
                let login_id = login
                    .get("id")
                    .or_else(|| login.get("login_id"))
                    .and_then(Value::as_str)?
                    .to_owned();
                Some(ExistingLogin {
                    login_id,
                    name: login.get("name").and_then(Value::as_str).map(str::to_owned),
                    profile: login.get("profile").cloned().unwrap_or(Value::Null),
                })
            })
            .collect())
    }

    /// `GET /_matrix/provision/v3/whoami` — what the bridge says about
    /// itself, as the `logins` array of its answer.
    ///
    /// # The only call that describes a login
    ///
    /// Each member of that array is the whole login object — `id`, `name`,
    /// `profile`, and a nested `state` document with `state_event`,
    /// `timestamp`, `ttl` and `source`. [`Self::logins`] answers bare id
    /// strings and nothing else, so **whoami is where a login's name, its
    /// profile and its connection state come from**, and the only provisioning
    /// call that can answer "is this network connected right now?" (#108).
    ///
    /// This is the **reconciliation read** (ticket #56): it is called at
    /// startup, once per bridge, so that a Gateway which restarts does not
    /// carry a stale state forward. It is never a substitute for the
    /// webhook. A bridge's state lives in the bridge's own memory, is empty
    /// right after a bridge restart, and carries no "last connected" field —
    /// so polling it would miss every transition between two reads. The
    /// translation of what comes back is [`crate::bridge_status`]'s.
    pub async fn whoami(
        &self,
        bridge_id: &str,
        acting_as: &str,
    ) -> Result<Vec<Value>, BridgeRefusal> {
        let bridge = self.bridge(bridge_id)?;
        let body = self
            .call(
                bridge,
                &self.clients.http,
                ProvisioningCall::Whoami,
                &["whoami"],
                &[("user_id", acting_as)],
                None,
            )
            .await?;
        whoami_logins(&body)
    }

    /// The login in flight on this bridge, as the Companion polls it.
    pub fn login(&self, bridge_id: &str) -> Result<LoginView, BridgeRefusal> {
        let bridge = self.bridge(bridge_id)?;
        match &*bridge
            .slot
            .lock()
            .expect("the slot mutex is never poisoned")
        {
            Slot::Idle => Err(BridgeRefusal::NoLoginInFlight),
            Slot::Active(active) => Ok(active.read()),
        }
    }

    /// Starts a login — or restarts one against an existing login id, which
    /// is what "reconnect" means.
    ///
    /// The bridge answers the flow's first step; if that step blocks, the
    /// holding task is spawned here and the answer already carries the first
    /// QR. The Companion then polls.
    pub async fn start(
        &self,
        bridge_id: &str,
        acting_as: &str,
        device: &Device,
        request: &StartLogin,
    ) -> Result<LoginView, BridgeRefusal> {
        let bridge = self.bridge(bridge_id)?;
        // Serialises two starts against each other without ever making a
        // poll wait: the slot itself is only ever locked for a comparison.
        let _starting = bridge.start_gate.lock().await;
        if let Some((started_by, started_at)) = in_flight(bridge) {
            return Err(BridgeRefusal::LoginInFlight {
                started_by,
                started_at,
            });
        }

        let mut query: Vec<(&str, &str)> = vec![("user_id", acting_as)];
        if let Some(login_id) = &request.login_id {
            query.push(("login_id", login_id.as_str()));
        }
        let answer = self
            .call(
                bridge,
                &self.clients.http,
                ProvisioningCall::Start,
                &["login", "start", &request.flow_id],
                &query,
                Some(json!({})),
            )
            .await?;
        let step = parse_step(&answer)
            .map_err(|looked_for| unusable(ProvisioningCall::Start, looked_for, &answer))?;
        let started_at = (self.clients.now)();
        let view = LoginView {
            bridge_id: bridge.config.bridge_id.clone(),
            network: bridge.config.network.clone(),
            process_id: step.process_id.clone().unwrap_or_default(),
            flow_id: request.flow_id.clone(),
            login_id: request.login_id.clone(),
            phase: LoginPhase::AwaitingInput,
            started_at,
            started_by: StartedBy {
                device_id: device.id.clone(),
                device_name: device.name.clone(),
            },
            expires_at: started_at + MAX_LOGIN_LIFETIME,
            generation: 0,
            step: None,
            login: None,
            error: None,
            detail: None,
        };
        let active = Arc::new(ActiveLogin {
            view: Mutex::new(view),
            generation: AtomicU64::new(0),
            cancelled: std::sync::atomic::AtomicBool::new(false),
        });
        // The process id travels on every later call; a bridge that answered
        // a non-complete step without one cannot be driven.
        if step.process_id.is_none() && step.step_type != StepType::Complete {
            return Err(unusable(
                ProvisioningCall::Start,
                "a `login_id`, which is what mautrix calls the id of the login process \
                 it just started",
                &answer,
            ));
        }
        info!(
            bridge = %bridge.config.bridge_id,
            flow = %request.flow_id,
            reconnect = request.login_id.is_some(),
            device = %device.id,
            step_type = step.step_type.as_str(),
            "a bridge login started"
        );
        apply(&self.clients, bridge, &active, step);
        *bridge
            .slot
            .lock()
            .expect("the slot mutex is never poisoned") = Slot::Active(active.clone());
        Ok(active.read())
    }

    /// Submits what the current step asked the user for: a phone number, a
    /// set of cookies. The values are relayed to the bridge and dropped —
    /// never stored, never logged (ADR 0011).
    pub async fn submit(
        &self,
        bridge_id: &str,
        step_id: &str,
        data: Value,
    ) -> Result<LoginView, BridgeRefusal> {
        let bridge = self.bridge(bridge_id)?;
        let active = active_login(bridge)?;
        let view = active.read();
        // The phase first: a login that is over, or one whose blocking step
        // the Gateway is holding, has nothing to submit — and saying so is
        // more use than "no login in flight", which is a different fact.
        if view.phase != LoginPhase::AwaitingInput {
            return Err(BridgeRefusal::InvalidRequest {
                detail: format!(
                    "this login is {}, so there is nothing to submit: \
                     the Gateway is holding the step, or the login is over. \
                     Poll the login and act on what it reports",
                    view.phase.as_str()
                ),
            });
        }
        let step = view.step.as_ref().ok_or(BridgeRefusal::NoLoginInFlight)?;
        if step.step_id != step_id {
            return Err(BridgeRefusal::InvalidRequest {
                detail: format!(
                    "this login is on step {:?}, not {step_id:?}: poll the login and \
                     submit against the step it reports",
                    step.step_id
                ),
            });
        }
        let step_type = step.step_type;
        let process_id = view.process_id.clone();
        let step_id = step.step_id.clone();
        // The transaction id the bridge issued with *this* step. A submit
        // that omits it is accepted — the reference bridges only validate
        // the parameter when it is there — but echoing it is what makes a
        // retry idempotent, and it is the same rule the held step follows
        // (#106). It has to be the latest one: a bridge re-issues the id
        // with every answer it gives.
        let txn_id = step.txn_id.clone();
        drop(view);

        let query: Vec<(&str, &str)> = match &txn_id {
            Some(txn_id) => vec![("txn_id", txn_id.as_str())],
            None => Vec::new(),
        };
        let answer = self
            .call(
                bridge,
                &self.clients.http,
                ProvisioningCall::Step,
                &["login", "step", &process_id, &step_id, step_type.as_str()],
                &query,
                Some(data),
            )
            .await;
        let answer = match answer {
            Ok(answer) => answer,
            // A body the bridge could not read never reached a connector, so
            // the bridge still holds the process at the same step: recording
            // a failure here would end, on this side, a login the other side
            // kept — and tell a polling browser to start again (#221).
            Err(refusal @ BridgeRefusal::RequestUnreadable { .. }) => return Err(refusal),
            Err(refusal) => {
                record_refusal(&active, &refusal);
                return Err(refusal);
            }
        };
        let next = parse_step(&answer).map_err(|looked_for| {
            let refusal = unusable(ProvisioningCall::Step, looked_for, &answer);
            // The login is over either way, and a browser that is polling
            // has to learn *why* rather than watch a step that never moves.
            record_refusal(&active, &refusal);
            refusal
        })?;
        debug!(
            bridge = %bridge.config.bridge_id,
            step_type = next.step_type.as_str(),
            "a bridge login step was submitted"
        );
        apply(&self.clients, bridge, &active, next);
        Ok(active.read())
    }

    /// Cancels the login in flight: the current step first, then the whole
    /// process, as bridgev2's two cancel endpoints do.
    pub async fn cancel(&self, bridge_id: &str) -> Result<(), BridgeRefusal> {
        let bridge = self.bridge(bridge_id)?;
        let active = active_login(bridge)?;
        let view = active.read();
        let process_id = view.process_id.clone();
        let step_id = view.step.as_ref().map(|step| step.step_id.clone());
        let was_holding = view.phase == LoginPhase::AwaitingRemote;
        drop(view);

        // Mark first: the held request may be released by the cancel below,
        // and its answer must not write over the cancelled state.
        active.cancelled.store(true, Ordering::SeqCst);
        active.write(|view| {
            view.phase = LoginPhase::Cancelled;
            view.step = None;
            view.detail = Some("cancelled from the Companion".to_owned());
        });

        // Both cancels, in bridgev2's order — and the first one is expected
        // to fail. Against mautrix-whatsapp and mautrix-signal v26.09 the
        // step cancel answers `500 M_BAD_STATE: Login process does not
        // support cancelling steps` for a `display_and_wait` step, so the
        // endpoint is simply not usable for a QR flow; what releases the held
        // request is the *process* cancel below, and the held request then
        // comes back `410 FI.MAU.BRIDGE.LOGIN_CANCELLED`. Both outcomes are
        // discarded on purpose: the login is over either way, which is what
        // was asked for. The call stays because a connector whose flow does
        // support it is the case the endpoint exists for.
        if was_holding {
            if let Some(step_id) = &step_id {
                let _ = self
                    .call(
                        bridge,
                        &self.clients.http,
                        ProvisioningCall::CancelStep,
                        &["login", "step", &process_id, step_id, "cancel"],
                        &[],
                        Some(json!({})),
                    )
                    .await;
            }
        }
        let outcome = self
            .call(
                bridge,
                &self.clients.http,
                ProvisioningCall::CancelProcess,
                &["login", "cancel", &process_id],
                &[],
                Some(json!({})),
            )
            .await;
        info!(
            bridge = %bridge.config.bridge_id,
            bridge_acknowledged = outcome.is_ok(),
            "a bridge login was cancelled"
        );
        // A bridge that has already forgotten the process is not an error
        // here: the login is over either way, which is what was asked for.
        Ok(())
    }

    /// `POST /_matrix/provision/v3/logout/{loginID}` — drops a login the
    /// bridge holds. The network credentials it was using are the bridge's
    /// to forget; the Gateway never had them.
    pub async fn logout(
        &self,
        bridge_id: &str,
        login_id: &str,
        acting_as: &str,
    ) -> Result<(), BridgeRefusal> {
        let bridge = self.bridge(bridge_id)?;
        self.call(
            bridge,
            &self.clients.http,
            ProvisioningCall::Logout,
            &["logout", login_id],
            &[("user_id", acting_as)],
            Some(json!({})),
        )
        .await?;
        info!(
            bridge = %bridge.config.bridge_id,
            "a bridge login was logged out"
        );
        Ok(())
    }

    /// One provisioning call: the bearer secret, the acting user in the
    /// query, and mautrix's error document translated into a refusal.
    async fn call(
        &self,
        bridge: &Arc<Bridge>,
        http: &reqwest::Client,
        call: ProvisioningCall,
        segments: &[&str],
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<Value, BridgeRefusal> {
        call_bridge(&bridge.config, http, call, segments, query, body).await
    }
}

/// `whoami`'s `logins` array — the one call that describes a login (#108).
///
/// Strict on purpose. Every captured answer from both reference bridges
/// carries `logins`, as `[]` when nobody has logged in, so a `whoami` without
/// it is not "no logins": it is an answer this build cannot read, and that is
/// precisely the mistake #106 was — a missing field read as an empty one, or
/// as a bridge that was down.
fn whoami_logins(body: &Value) -> Result<Vec<Value>, BridgeRefusal> {
    body.get("logins")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            unusable(
                ProvisioningCall::Whoami,
                "a `logins` array — empty when nobody is logged in, but always present",
                body,
            )
        })
}

/// Builds a [`BridgeRefusal::BridgeAnswerUnusable`] and tells the operator's
/// log about it.
///
/// The split this function exists to enforce: the **log** gets the shape of
/// what the bridge actually sent ([`redacted`] — key names and lengths, never
/// a value), and the **refusal**, which reaches a browser, gets only the
/// endpoint and the `&'static str` naming what was missing. `answer` is
/// therefore borrowed and dropped here; it cannot escape into the refusal,
/// because the refusal has nowhere to put it.
fn unusable(call: ProvisioningCall, looked_for: &'static str, answer: &Value) -> BridgeRefusal {
    warn!(
        call = call.label(),
        endpoint = call.endpoint(),
        looked_for,
        // Shapes only: a bridge's answer can carry a phone number as a login
        // id, a display name, or a QR payload.
        answer = %redacted(answer),
        "the bridge answered and this build could not use its answer; the bridge is \
         running — this is a defect in Twalk's reading of its provisioning API"
    );
    BridgeRefusal::BridgeAnswerUnusable { call, looked_for }
}

/// Takes the bridge's answer and turns it into the pollable state,
/// spawning the holding task when the new step blocks.
fn apply(clients: &Clients, bridge: &Arc<Bridge>, active: &Arc<ActiveLogin>, step: ParsedStep) {
    if active.cancelled.load(Ordering::SeqCst) {
        return;
    }
    let generation = active.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let now = (clients.now)();
    match step.step_type {
        StepType::Complete => {
            let login_id = step.login_id.clone().unwrap_or_default();
            active.write(|view| {
                view.phase = LoginPhase::Complete;
                view.generation = generation;
                view.step = None;
                view.login = Some(CompletedLogin {
                    login_id: login_id.clone(),
                    user_id: step.user_id.clone(),
                });
                view.error = None;
                view.detail = None;
            });
            info!(
                bridge = %bridge.config.bridge_id,
                "a bridge login completed: the network accepted it"
            );
        }
        StepType::Webauthn | StepType::ClientHttp => {
            // Explicit rather than a hang: the Companion cannot drive a
            // passkey or a client-side HTTP challenge, and a step nobody
            // will answer would sit there until the process expires.
            let failure = if step.step_type == StepType::Webauthn {
                LoginFailure::WebauthnRequired
            } else {
                LoginFailure::UnsupportedStep
            };
            let detail = format!(
                "the flow asked for a {} step, which the Companion cannot drive; \
                 set provisioning.fail_on_webauthn on the bridge to have it refuse \
                 one step earlier",
                step.step_type.as_str()
            );
            active.write(|view| {
                view.phase = LoginPhase::Failed;
                view.generation = generation;
                view.step = None;
                view.error = Some(failure);
                view.detail = Some(detail);
            });
            warn!(
                bridge = %bridge.config.bridge_id,
                step_type = step.step_type.as_str(),
                "a bridge login asked for a step the Companion cannot drive"
            );
            let http = clients.http.clone();
            let config = bridge.config.clone();
            let process_id = active.read().process_id.clone();
            tokio::spawn(async move {
                let _ = call_bridge(
                    &config,
                    &http,
                    ProvisioningCall::CancelProcess,
                    &["login", "cancel", &process_id],
                    &[],
                    Some(json!({})),
                )
                .await;
            });
        }
        step_type => {
            let deadline = active.read().expires_at;
            let valid_for = step_validity(step_type, &step.payload, now, deadline);
            let view_step = StepView {
                step_id: step.step_id.clone(),
                txn_id: step.txn_id.clone(),
                step_type,
                instructions: step.instructions.clone(),
                payload: step.payload.clone(),
                received_at: now,
                valid_for,
            };
            active.write(|view| {
                view.phase = if step_type.blocks() {
                    LoginPhase::AwaitingRemote
                } else {
                    LoginPhase::AwaitingInput
                };
                view.generation = generation;
                view.step = Some(view_step);
                view.error = None;
                view.detail = None;
            });
            debug!(
                bridge = %bridge.config.bridge_id,
                step_type = step_type.as_str(),
                step_id = %step.step_id,
                generation,
                payload = %redacted(&step.payload),
                "a bridge login advanced to a new step"
            );
            if step_type.blocks() {
                hold(
                    clients,
                    bridge.clone(),
                    active.clone(),
                    step.step_id,
                    step.txn_id,
                );
            }
        }
    }
}

/// Spawns the task that sits in the blocking step, so the browser never
/// has to. Each answer is another step — a refreshed QR, the next step,
/// the completion — and each one goes through [`Self::apply`], which
/// spawns the next hold if the new step blocks too.
fn hold(
    clients: &Clients,
    bridge: Arc<Bridge>,
    active: Arc<ActiveLogin>,
    step_id: String,
    bridge_txn_id: Option<String>,
) {
    let clients = clients.clone();
    tokio::spawn(async move {
        let process_id = active.read().process_id.clone();
        // `txn_id` makes a retry idempotent at the bridge. It must be the
        // one the bridge issued **with this step**: mautrix validates it and
        // answers `500 M_BAD_STATE: Transaction ID does not match` for
        // anything else (#106). The derived value is a fallback for a bridge
        // that issues none, and it keeps the same retry property.
        let txn_id = bridge_txn_id.unwrap_or_else(|| {
            format!(
                "twalk-{process_id}-{step_id}-{}",
                active.generation.load(Ordering::SeqCst)
            )
        });
        let answer = call_bridge(
            &bridge.config,
            &clients.blocking_http,
            ProvisioningCall::Step,
            &[
                "login",
                "step",
                &process_id,
                &step_id,
                StepType::DisplayAndWait.as_str(),
            ],
            &[("txn_id", txn_id.as_str())],
            Some(json!({})),
        )
        .await;
        if active.cancelled.load(Ordering::SeqCst) {
            return;
        }
        match answer {
            Ok(answer) => match parse_step(&answer) {
                Ok(next) => apply(&clients, &bridge, &active, next),
                Err(looked_for) => record_refusal(
                    &active,
                    &unusable(ProvisioningCall::Step, looked_for, &answer),
                ),
            },
            Err(refusal) => record_refusal(&active, &refusal),
        }
    });
}

/// Writes a refusal of a provisioning call into the pollable state, so a
/// browser that is polling learns what happened to the login it started.
fn record_refusal(active: &Arc<ActiveLogin>, refusal: &BridgeRefusal) {
    let (failure, detail) = match refusal {
        BridgeRefusal::LoginExpired { errcode } => (
            LoginFailure::Expired,
            format!("the bridge ended this login: {errcode}"),
        ),
        BridgeRefusal::StepCancelled => (
            LoginFailure::Expired,
            "the step was cancelled on the bridge's side".to_owned(),
        ),
        BridgeRefusal::TooManyLogins => (
            LoginFailure::BridgeRefused,
            "the network will not accept another login (FI.MAU.BRIDGE.TOO_MANY_LOGINS)".to_owned(),
        ),
        BridgeRefusal::BridgeUnreachable { detail } => (
            LoginFailure::LoginLost,
            format!(
                "the bridge stopped answering while the Gateway was holding this login \
                 ({detail}). A login in flight does not survive a restart of the bridge \
                 or of the Gateway: start again"
            ),
        ),
        // The other half of what `BridgeUnreachable` used to mean, and the
        // reason this ticket exists: the bridge answered, this build could
        // not read the answer, and telling the user their bridge could not
        // be reached sent a whole debugging session to look at networking
        // (#106). Nothing here is the deployment's fault and the detail says
        // so — while naming only the call and the missing field, never the
        // answer.
        BridgeRefusal::BridgeAnswerUnusable { call, looked_for } => (
            LoginFailure::BridgeAnswerUnusable,
            format!(
                "the bridge answered {} and this Gateway could not use its answer: it \
                 looked for {looked_for} and did not find it. The bridge is running and \
                 replied — this is a defect in Twalk, not a broken deployment",
                call.endpoint()
            ),
        ),
        // A process the bridge no longer knows is the restart case seen
        // from the other side: it kept the connection but lost the login.
        BridgeRefusal::NotFoundOnBridge { errcode } => (
            LoginFailure::LoginLost,
            format!(
                "the bridge no longer knows this login process ({errcode}). A login in \
                 flight does not survive a restart of the bridge or of the Gateway: \
                 start again"
            ),
        ),
        BridgeRefusal::BridgeRefused { errcode, status } => (
            LoginFailure::BridgeRefused,
            format!("the bridge refused with {errcode} ({status})"),
        ),
        other => (
            LoginFailure::BridgeRefused,
            format!("the bridge call was refused: {}", other.label()),
        ),
    };
    fail(active, failure, detail);
}

fn fail(active: &Arc<ActiveLogin>, failure: LoginFailure, detail: String) {
    if active.cancelled.load(Ordering::SeqCst) {
        return;
    }
    let generation = active.generation.fetch_add(1, Ordering::SeqCst) + 1;
    warn!(
        error = failure.as_str(),
        detail = %detail,
        "a bridge login failed"
    );
    active.write(|view| {
        view.phase = LoginPhase::Failed;
        view.generation = generation;
        view.step = None;
        view.error = Some(failure);
        view.detail = Some(detail);
    });
}

/// The login in flight on this bridge, or the refusal that there is none. A
/// terminal login still counts as present: the Companion has to be able to
/// read how its login ended.
fn active_login(bridge: &Arc<Bridge>) -> Result<Arc<ActiveLogin>, BridgeRefusal> {
    match &*bridge
        .slot
        .lock()
        .expect("the slot mutex is never poisoned")
    {
        Slot::Idle => Err(BridgeRefusal::NoLoginInFlight),
        Slot::Active(active) => Ok(active.clone()),
    }
}

/// The device and instant of a login that is still live on this bridge, or
/// `None` when the slot is free — a login that has completed, failed or been
/// cancelled does not stand in the way of the next one.
fn in_flight(bridge: &Arc<Bridge>) -> Option<(StartedBy, SystemTime)> {
    let slot = bridge
        .slot
        .lock()
        .expect("the slot mutex is never poisoned");
    match &*slot {
        Slot::Idle => None,
        Slot::Active(active) => {
            let view = active.read();
            (!view.phase.is_terminal()).then_some((view.started_by.clone(), view.started_at))
        }
    }
}

/// One provisioning call against one bridge.
async fn call_bridge(
    config: &BridgeConfig,
    http: &reqwest::Client,
    call: ProvisioningCall,
    segments: &[&str],
    query: &[(&str, &str)],
    body: Option<Value>,
) -> Result<Value, BridgeRefusal> {
    // Every provisioning call carries the acting user, whether or not the
    // caller thought to pass it (#106).
    let mut owned_query: Vec<(&str, &str)> = query.to_vec();
    if !owned_query.iter().any(|(name, _)| *name == "user_id") {
        owned_query.push(("user_id", config.acting_as.as_str()));
    }
    let query: &[(&str, &str)] = &owned_query;
    let mut url = reqwest::Url::parse(&format!("{}/{PROVISION_PREFIX}", config.base_url)).map_err(
        |error| BridgeRefusal::BridgeUnreachable {
            detail: format!("the bridge's base URL is not a URL: {error}"),
        },
    )?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| BridgeRefusal::BridgeUnreachable {
                detail: "the bridge's base URL cannot carry a path".to_owned(),
            })?;
        for segment in segments {
            path.push(segment);
        }
    }
    for (name, value) in query {
        url.query_pairs_mut().append_pair(name, value);
    }
    let mut request = http
        .request(call.method(), url)
        // The provisioning secret: impersonation-grade, so it goes in the
        // header of an outbound call and nowhere else.
        .bearer_auth(&config.provisioning_secret);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .map_err(|error| BridgeRefusal::BridgeUnreachable {
            // `without_url` keeps the query string — and therefore nothing
            // secret, but also nothing noisy — out of the message.
            detail: error.without_url().to_string(),
        })?;
    let status = response.status();
    if status.is_success() {
        let text = response
            .text()
            .await
            // The connection died part-way through the body: nothing usable
            // arrived, so this is still "could not reach", not "answered
            // something I could not read".
            .map_err(|error| BridgeRefusal::BridgeUnreachable {
                detail: error.without_url().to_string(),
            })?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        // The bridge answered, with a body that is not JSON. That is the
        // unusable half, not the unreachable one — and the serde error is
        // not repeated, because it can quote the bytes it choked on.
        return serde_json::from_str(&text).map_err(|error| {
            warn!(
                call = call.label(),
                endpoint = call.endpoint(),
                bytes = text.len(),
                %error,
                "the bridge answered a body that is not JSON"
            );
            BridgeRefusal::BridgeAnswerUnusable {
                call,
                looked_for: "a JSON document",
            }
        });
    }
    let (errcode, _) = mautrix_error(response).await;
    // Which of these are observed and which are read out of mautrix's source
    // is written down, because #106 was three bugs in a row where the
    // difference mattered. Observed against mautrix-whatsapp and
    // mautrix-signal v26.09 (`tests/harness/fixtures`): the 400 from a
    // connector refusing a submitted value, the 401, the 403 with no
    // `?user_id=`, the 404 for a forgotten process, the 410
    // `FI.MAU.BRIDGE.LOGIN_CANCELLED` a held step comes back with, and the
    // 500 `M_BAD_STATE` for a stale `txn_id`, step id or step type.
    // Unexercised, kept because bridgev2 defines them and the deployment
    // simply never provoked one: `FI.MAU.BRIDGE.TOO_MANY_LOGINS` (needs a
    // `max_logins` cap) and `FI.MAU.LOGIN_STEP_CANCELLED` (the reference
    // bridges refuse the step-cancel endpoint outright for a QR flow).
    Err(match (status.as_u16(), errcode.as_str()) {
        (403, "FI.MAU.BRIDGE.TOO_MANY_LOGINS") => BridgeRefusal::TooManyLogins,
        (409, "FI.MAU.LOGIN_STEP_CANCELLED") => BridgeRefusal::StepCancelled,
        (410, _) => BridgeRefusal::LoginExpired { errcode },
        (404, _) => BridgeRefusal::NotFoundOnBridge { errcode },
        // bridgev2's decoder refusing the body, before any connector saw it
        // (`provisioninglogin.go`: `MNotJSON` on decode failure, then
        // `doLoginStep`). Not the network, not the user, and the login is
        // still there — so not the arm below, whose every sentence would be
        // wrong for it (#221).
        (400, "M_NOT_JSON" | "M_BAD_JSON") => BridgeRefusal::RequestUnreadable { errcode },
        // The network itself would not take what was submitted — a phone
        // number it calls too short, a cookie it will not accept. The user
        // can act on that, so it must not arrive as a bad gateway. The
        // bridge drops the login process along with the refusal, which the
        // detail has to say or the user retries into a 404.
        (400, _) => BridgeRefusal::InvalidRequest {
            detail: format!(
                "the network would not accept what was submitted ({errcode}). The bridge \
                 drops the login process when a step is refused, so start the login again \
                 rather than submitting a correction to this one"
            ),
        },
        (status, _) => BridgeRefusal::BridgeRefused { errcode, status },
    })
}

/// The bridge's error document: mautrix answers `{errcode, error}`, the same
/// shape as the Matrix spec's. The human half goes to the operator's log and
/// never to the client.
async fn mautrix_error(response: reqwest::Response) -> (String, String) {
    let text = response.text().await.unwrap_or_default();
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let errcode = parsed
        .get("errcode")
        .and_then(Value::as_str)
        .unwrap_or("M_UNKNOWN")
        .to_owned();
    let error = parsed
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    (errcode, error)
}

/// One step as the bridge answered it.
#[derive(Debug, Clone)]
pub struct ParsedStep {
    pub process_id: Option<String>,
    /// The transaction id **the bridge issued for this step**. mautrix
    /// validates it on the next call and answers `500 M_BAD_STATE:
    /// Transaction ID does not match` for anything else, so it is echoed
    /// rather than invented (#106).
    pub txn_id: Option<String>,
    pub step_id: String,
    pub step_type: StepType,
    pub instructions: Option<String>,
    pub payload: Value,
    pub login_id: Option<String>,
    pub user_id: Option<String>,
}

/// Reads bridgev2's `RespLoginStep`: a `type`, a `step_id`, optional
/// `instructions`, and one member named after the type carrying its payload.
///
/// The error is a `&'static str` naming **what was looked for**, not what was
/// found. It is the message that ends up in an HTTP answer to a browser, and
/// a bridge's answer can carry identifiers from a network account, so the
/// type forbids interpolating any of it. The unrecognised value itself goes
/// to the operator's log below and no further.
pub fn parse_step(answer: &Value) -> Result<ParsedStep, &'static str> {
    let step_type = answer
        .get("type")
        .and_then(Value::as_str)
        .ok_or("a `type` naming the step")?;
    let step_type = StepType::parse(step_type).ok_or_else(|| {
        // A step type is a protocol token rather than a credential, so the
        // operator's log may name it — and wants to, since a bridge growing
        // a new step type is exactly what this branch catches.
        warn!(
            step_type,
            "the bridge answered a login step type this build does not know"
        );
        "a step `type` this build knows: user_input, cookies, display_and_wait, \
         client_http, webauthn or complete"
    })?;
    let payload = answer
        .get(step_type.as_str())
        .cloned()
        .unwrap_or(Value::Null);
    let complete = answer.get("complete").cloned().unwrap_or(Value::Null);
    Ok(ParsedStep {
        txn_id: answer
            .get("txn_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        // mautrix calls the process id `login_id` at the top level of a
        // start or step answer — the same word its `?login_id=` query
        // parameter uses for an *existing* login, which is why this reads
        // like a trap. The other two names are accepted because a stub or
        // another bridge implementation may use them.
        process_id: answer
            .get("login_process_id")
            .or_else(|| answer.get("process_id"))
            .or_else(|| answer.get("login_id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        step_id: answer
            .get("step_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        step_type,
        instructions: answer
            .get("instructions")
            .and_then(Value::as_str)
            .map(str::to_owned),
        payload,
        // Only from the `complete` payload: the top level's `login_id` is
        // the login *process*, not the login this flow produced.
        login_id: complete
            .get("login_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        user_id: complete
            .get("user_login_id")
            .or_else(|| complete.get("user_id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// How long a step is worth acting on: for a QR, one refresh interval; for
/// anything else, whatever is left of the process. Never past the process
/// deadline, so a Companion that trusts this number never waits on a login
/// the bridge has already forgotten.
fn step_validity(
    step_type: StepType,
    payload: &Value,
    now: SystemTime,
    deadline: SystemTime,
) -> Duration {
    let remaining = deadline.duration_since(now).unwrap_or_default();
    let is_qr = step_type == StepType::DisplayAndWait
        && payload.get("type").and_then(Value::as_str) == Some("qr");
    if is_qr {
        QR_CODE_VALIDITY.min(remaining)
    } else {
        remaining
    }
}

/// A step payload with every value replaced by its shape: what a log line is
/// allowed to say about a QR payload, a pairing code or a set of cookies.
///
/// Network credentials are never logged (ADR 0011), and a truncated
/// credential is still a credential — so this keeps the *keys* and states the
/// length of what was there, never a prefix of it.
pub fn redacted(payload: &Value) -> String {
    match payload {
        Value::Null => "none".to_owned(),
        Value::Object(members) => {
            let described: Vec<String> = members
                .iter()
                .map(|(key, value)| format!("{key}={}", shape(value)))
                .collect();
            format!("{{{}}}", described.join(", "))
        }
        other => shape(other),
    }
}

fn shape(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(_) => "<number>".to_owned(),
        Value::String(value) => format!("<{} chars>", value.chars().count()),
        Value::Array(values) => format!("<{} items>", values.len()),
        Value::Object(members) => format!("<{} members>", members.len()),
    }
}

/// The pollable state as JSON, for [`crate::bridge_http`] and for the tests
/// that read it.
pub fn login_json(view: &LoginView) -> Value {
    let mut document = json!({
        "bridge_id": view.bridge_id,
        "network": view.network,
        "process_id": view.process_id,
        "flow_id": view.flow_id,
        "login_id": view.login_id,
        "state": view.phase.as_str(),
        "started_at": rfc3339_millis(view.started_at),
        "started_by": {
            "device_id": view.started_by.device_id,
            "device_name": view.started_by.device_name,
        },
        "expires_at": rfc3339_millis(view.expires_at),
        "generation": view.generation,
        "step": Value::Null,
        "login": Value::Null,
        "error": Value::Null,
    });
    if let Some(step) = &view.step {
        document["step"] = json!({
            "step_id": step.step_id,
            "type": step.step_type.as_str(),
            "instructions": step.instructions,
            "payload": step.payload,
            "received_at": rfc3339_millis(step.received_at),
            "valid_for_seconds": step.valid_for.as_secs(),
            "expires_at": rfc3339_millis(step.received_at + step.valid_for),
        });
    }
    if let Some(login) = &view.login {
        document["login"] = json!({
            "login_id": login.login_id,
            "user_id": login.user_id,
        });
    }
    if let Some(error) = view.error {
        document["error"] = json!({
            "code": error.as_str(),
            "detail": view.detail,
        });
    }
    document
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_qr_step_is_read_out_of_the_bridges_own_answer() {
        let answer = json!({
            "login_process_id": "process-1",
            "type": "display_and_wait",
            "step_id": "fi.mau.whatsapp.login.qr",
            "instructions": "Scan this from WhatsApp",
            "display_and_wait": { "type": "qr", "data": "2@raw-qr-payload" }
        });
        let step = parse_step(&answer).expect("a step");
        assert_eq!(step.process_id.as_deref(), Some("process-1"));
        assert_eq!(step.step_type, StepType::DisplayAndWait);
        assert_eq!(step.payload["data"], json!("2@raw-qr-payload"));
    }

    #[test]
    fn a_complete_step_names_the_login_it_created() {
        let answer = json!({
            "type": "complete",
            "step_id": "fi.mau.whatsapp.login.complete",
            "complete": { "login_id": "33612345678", "user_login_id": "33612345678" }
        });
        let step = parse_step(&answer).expect("a step");
        assert_eq!(step.step_type, StepType::Complete);
        assert_eq!(step.login_id.as_deref(), Some("33612345678"));
    }

    #[test]
    fn an_unknown_step_type_is_refused_rather_than_guessed() {
        let answer = json!({ "type": "telepathy", "step_id": "x" });
        let looked_for = parse_step(&answer).expect_err("an unknown type is not guessed at");
        assert!(
            !looked_for.contains("telepathy"),
            "what was looked for is named, never what was received: {looked_for}"
        );
    }

    /// The defect this ticket is about (#116): "I could not connect" and "the
    /// bridge answered something I could not read" were one refusal with one
    /// name, and the Companion rendered both as *your Twalk server could not
    /// reach this network's bridge* — false for the first live WhatsApp
    /// login, and worth a debugging session spent on ports and containers.
    #[test]
    fn nothing_answering_and_an_answer_that_cannot_be_read_are_different_refusals() {
        let nothing_answered = BridgeRefusal::BridgeUnreachable {
            detail: "connection refused".to_owned(),
        };
        let answered_unusably = BridgeRefusal::BridgeAnswerUnusable {
            call: ProvisioningCall::Start,
            looked_for: "a `login_id`",
        };
        assert_eq!(nothing_answered.label(), "bridge_unreachable");
        assert_eq!(answered_unusably.label(), "bridge_answer_unusable");
        assert_ne!(
            nothing_answered.label(),
            answered_unusably.label(),
            "an API caller must tell these apart without reading prose"
        );
        // In the polled login state they are equally distinct: one is a lost
        // login the user should start again, the other is a Twalk defect.
        assert_eq!(
            LoginFailure::BridgeAnswerUnusable.as_str(),
            "bridge_answer_unusable"
        );
        assert_ne!(
            LoginFailure::BridgeAnswerUnusable.as_str(),
            LoginFailure::BridgeUnreachable.as_str()
        );
    }

    /// A bridge's answer can carry identifiers from a network account — a
    /// phone number as a login id, a display name, a QR payload. None of it
    /// may ride out to the browser inside a refusal, so the refusal is built
    /// from a body stuffed with such values and then searched for them.
    ///
    /// The type is what actually enforces this — `looked_for` is a
    /// `&'static str` and there is no member an answer could go in — and
    /// this is the test that fails if somebody ever widens it.
    #[test]
    fn an_unusable_answer_names_the_call_and_the_missing_field_and_nothing_else() {
        let answer = json!({
            "login_ids": ["33612345678"],
            "name": "+33612345678",
            "display_and_wait": { "type": "qr", "data": "2@a-real-qr-payload" },
            "unexpected": "M_SOMETHING_THE_BRIDGE_SAID",
        });
        let refusal = unusable(
            ProvisioningCall::Whoami,
            "a `logins` array — empty when nobody is logged in, but always present",
            &answer,
        );
        let (call, looked_for) = match &refusal {
            BridgeRefusal::BridgeAnswerUnusable { call, looked_for } => (*call, *looked_for),
            other => panic!("an unusable answer is its own refusal: {other:?}"),
        };
        assert_eq!(call.endpoint(), "GET /_matrix/provision/v3/whoami");
        assert!(looked_for.contains("`logins`"));
        // Everything a refusal can say, as the browser would see it.
        let said = format!("{} {looked_for} {refusal:?}", call.endpoint());
        for from_the_answer in [
            "33612345678",
            "2@a-real-qr-payload",
            "M_SOMETHING_THE_BRIDGE_SAID",
        ] {
            assert!(
                !said.contains(from_the_answer),
                "a refusal repeats nothing out of the bridge's answer, and this one has \
                 {from_the_answer:?} in it: {said}"
            );
        }
    }

    /// The same distinction where it is hardest to keep: inside the task
    /// holding a QR step, whose only report to the user is the polled state.
    #[test]
    fn a_held_step_that_comes_back_unreadable_is_not_a_bridge_that_went_away() {
        let login = || {
            Arc::new(ActiveLogin {
                view: Mutex::new(LoginView {
                    bridge_id: "mautrix-whatsapp".to_owned(),
                    network: "whatsapp".to_owned(),
                    process_id: "process-1".to_owned(),
                    flow_id: "qr".to_owned(),
                    login_id: None,
                    phase: LoginPhase::AwaitingRemote,
                    started_at: SystemTime::UNIX_EPOCH,
                    started_by: StartedBy {
                        device_id: "device-1".to_owned(),
                        device_name: "the phone".to_owned(),
                    },
                    expires_at: SystemTime::UNIX_EPOCH + MAX_LOGIN_LIFETIME,
                    generation: 1,
                    step: None,
                    login: None,
                    error: None,
                    detail: None,
                }),
                generation: AtomicU64::new(1),
                cancelled: std::sync::atomic::AtomicBool::new(false),
            })
        };

        let unreadable = login();
        record_refusal(
            &unreadable,
            &BridgeRefusal::BridgeAnswerUnusable {
                call: ProvisioningCall::Step,
                looked_for: "a `type` naming the step",
            },
        );
        let view = unreadable.read();
        assert_eq!(view.phase, LoginPhase::Failed);
        assert_eq!(view.error, Some(LoginFailure::BridgeAnswerUnusable));
        let detail = view.detail.clone().unwrap_or_default();
        assert!(
            detail.contains("answered") && detail.contains("not a broken deployment"),
            "the detail says the bridge replied and whose defect this is: {detail}"
        );
        assert!(
            detail.contains("/login/step/"),
            "and which call it was: {detail}"
        );
        assert!(
            !detail.contains("stopped answering"),
            "it is precisely not the bridge going away: {detail}"
        );

        // Where a bridge that actually went away still lands, unchanged.
        let gone = login();
        record_refusal(
            &gone,
            &BridgeRefusal::BridgeUnreachable {
                detail: "connection refused".to_owned(),
            },
        );
        assert_eq!(gone.read().error, Some(LoginFailure::LoginLost));
    }

    /// `whoami` is the only call that describes a login (#108), and both
    /// reference bridges always send `logins` — as `[]` when nobody has
    /// logged in. So an answer without it is not "no logins": it is an
    /// answer this build cannot read, and it says so rather than reporting a
    /// deployment with a working WhatsApp link as having none.
    #[test]
    fn a_whoami_without_a_logins_array_is_unreadable_rather_than_empty() {
        assert_eq!(
            whoami_logins(&json!({ "logins": [] })).expect("an empty list is an answer"),
            Vec::<Value>::new()
        );
        let refusal = whoami_logins(&json!({ "bridge_bot": "@whatsappbot:twalk.localhost" }))
            .expect_err("a whoami with no logins array cannot be read");
        assert_eq!(refusal.label(), "bridge_answer_unusable");
    }

    #[test]
    fn a_qr_is_valid_for_one_refresh_interval_and_never_past_the_process() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let payload = json!({ "type": "qr", "data": "2@payload" });
        assert_eq!(
            step_validity(
                StepType::DisplayAndWait,
                &payload,
                now,
                now + MAX_LOGIN_LIFETIME
            ),
            QR_CODE_VALIDITY
        );
        // Five seconds left in the process: the code cannot outlive it.
        assert_eq!(
            step_validity(
                StepType::DisplayAndWait,
                &payload,
                now,
                now + Duration::from_secs(5)
            ),
            Duration::from_secs(5)
        );
        // A step that is not a QR is worth acting on for as long as the
        // process lives.
        assert_eq!(
            step_validity(
                StepType::UserInput,
                &json!({ "fields": [] }),
                now,
                now + Duration::from_secs(600)
            ),
            Duration::from_secs(600)
        );
    }

    #[test]
    fn a_log_line_describes_a_payload_without_quoting_it() {
        let payload = json!({
            "type": "qr",
            "data": "2@a-real-qr-payload-that-logs-must-never-carry",
        });
        let described = redacted(&payload);
        assert!(
            !described.contains("2@a-real"),
            "a redacted payload keeps no prefix of a credential: {described}"
        );
        assert!(described.contains("data=<46 chars>"), "{described}");
        assert_eq!(redacted(&Value::Null), "none");
    }

    #[test]
    fn two_bridges_cannot_share_an_id() {
        let config = |bridge_id: &str| BridgeConfig {
            bridge_id: bridge_id.to_owned(),
            status_bridge_id: format!("bridge-{bridge_id}"),
            network: "whatsapp".to_owned(),
            base_url: "http://bridge:29318".to_owned(),
            provisioning_secret: "a-secret-of-at-least-16".to_owned(),
            acting_as: "@owner:twalk.localhost".to_owned(),
            as_token: None,
            bot_user_id: None,
        };
        assert!(Bridges::new(vec![config("a"), config("b")]).is_ok());
        let error = match Bridges::new(vec![config("a"), config("a")]) {
            Ok(_) => panic!("two bridges with one id is a configuration error"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("two bridges"), "{error}");
    }

    #[test]
    fn a_configuration_never_prints_its_provisioning_secret() {
        let printed = format!(
            "{:?}",
            BridgeConfig {
                bridge_id: "mautrix-whatsapp".to_owned(),
                status_bridge_id: "bridge-whatsapp".to_owned(),
                network: "whatsapp".to_owned(),
                base_url: "http://bridge-whatsapp:29318".to_owned(),
                provisioning_secret: "the-secret-that-drives-logins".to_owned(),
                acting_as: "@owner:twalk.localhost".to_owned(),
                as_token: Some("the-token-that-impersonates-the-appservice".to_owned()),
                bot_user_id: Some("@whatsappbot:twalk.localhost".to_owned()),
            }
        );
        assert!(
            !printed.contains("the-secret"),
            "a bridge configuration must not print its provisioning secret: {printed}"
        );
        assert!(
            !printed.contains("the-token"),
            "nor its as_token, which impersonates the appservice: {printed}"
        );
        assert!(printed.contains("<redacted>"), "{printed}");
    }

    #[test]
    fn a_base_url_that_is_not_one_fails_at_startup() {
        let error = match Bridges::new(vec![BridgeConfig {
            bridge_id: "mautrix-whatsapp".to_owned(),
            status_bridge_id: "bridge-whatsapp".to_owned(),
            network: "whatsapp".to_owned(),
            base_url: "bridge-whatsapp:29318".to_owned(),
            provisioning_secret: "a-secret-of-at-least-16".to_owned(),
            acting_as: "@owner:twalk.localhost".to_owned(),
            as_token: None,
            bot_user_id: None,
        }]) {
            Ok(_) => panic!("a base URL without a scheme is not a URL"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("is not an http(s) URL"),
            "{error}"
        );
    }

    #[test]
    fn a_terminal_login_does_not_stand_in_the_way_of_the_next_one() {
        let now = SystemTime::UNIX_EPOCH;
        let view = |phase: LoginPhase| LoginView {
            bridge_id: "mautrix-whatsapp".to_owned(),
            network: "whatsapp".to_owned(),
            process_id: "process-1".to_owned(),
            flow_id: "qr".to_owned(),
            login_id: None,
            phase,
            started_at: now,
            started_by: StartedBy {
                device_id: "device-1".to_owned(),
                device_name: "the phone".to_owned(),
            },
            expires_at: now + MAX_LOGIN_LIFETIME,
            generation: 1,
            step: None,
            login: None,
            error: None,
            detail: None,
        };
        let bridge = |phase: LoginPhase| {
            Arc::new(Bridge {
                config: BridgeConfig {
                    bridge_id: "mautrix-whatsapp".to_owned(),
                    status_bridge_id: "bridge-whatsapp".to_owned(),
                    network: "whatsapp".to_owned(),
                    base_url: "http://bridge:29318".to_owned(),
                    provisioning_secret: "a-secret-of-at-least-16".to_owned(),
                    acting_as: "@owner:twalk.localhost".to_owned(),
                    as_token: None,
                    bot_user_id: None,
                },
                slot: Mutex::new(Slot::Active(Arc::new(ActiveLogin {
                    view: Mutex::new(view(phase)),
                    generation: AtomicU64::new(1),
                    cancelled: std::sync::atomic::AtomicBool::new(false),
                }))),
                start_gate: tokio::sync::Mutex::new(()),
            })
        };
        assert!(in_flight(&bridge(LoginPhase::AwaitingRemote)).is_some());
        assert!(in_flight(&bridge(LoginPhase::AwaitingInput)).is_some());
        for terminal in [
            LoginPhase::Complete,
            LoginPhase::Failed,
            LoginPhase::Cancelled,
        ] {
            assert!(
                in_flight(&bridge(terminal)).is_none(),
                "{} must not block the next login",
                terminal.as_str()
            );
        }
    }

    /// Both halves of "a login in flight does not survive a restart": the
    /// bridge dropping the held connection, and a restarted bridge answering
    /// `404` for a process it has never heard of. The second is what
    /// `tests/bridges.rs` drives against the stub; the first cannot be raced
    /// deterministically at that seam, so it is asserted here.
    #[test]
    fn a_bridge_that_stops_answering_a_held_step_loses_the_login() {
        let login = || {
            Arc::new(ActiveLogin {
                view: Mutex::new(LoginView {
                    bridge_id: "mautrix-whatsapp".to_owned(),
                    network: "whatsapp".to_owned(),
                    process_id: "process-1".to_owned(),
                    flow_id: "qr".to_owned(),
                    login_id: None,
                    phase: LoginPhase::AwaitingRemote,
                    started_at: SystemTime::UNIX_EPOCH,
                    started_by: StartedBy {
                        device_id: "device-1".to_owned(),
                        device_name: "the phone".to_owned(),
                    },
                    expires_at: SystemTime::UNIX_EPOCH + MAX_LOGIN_LIFETIME,
                    generation: 1,
                    step: None,
                    login: None,
                    error: None,
                    detail: None,
                }),
                generation: AtomicU64::new(1),
                cancelled: std::sync::atomic::AtomicBool::new(false),
            })
        };

        let dropped = login();
        record_refusal(
            &dropped,
            &BridgeRefusal::BridgeUnreachable {
                detail: "connection closed before message completed".to_owned(),
            },
        );
        let view = dropped.read();
        assert_eq!(view.phase, LoginPhase::Failed);
        assert_eq!(view.error, Some(LoginFailure::LoginLost));
        assert!(
            view.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("does not survive a restart")),
            "{:?}",
            view.detail
        );

        let forgotten = login();
        record_refusal(
            &forgotten,
            &BridgeRefusal::NotFoundOnBridge {
                errcode: "M_NOT_FOUND".to_owned(),
            },
        );
        assert_eq!(forgotten.read().error, Some(LoginFailure::LoginLost));

        // And the bridge ending the login itself is not a lost login but an
        // expired one: the user is told to start again either way, and the
        // operator can tell the two apart.
        let timed_out = login();
        record_refusal(
            &timed_out,
            &BridgeRefusal::LoginExpired {
                errcode: "LOGIN_TIMED_OUT".to_owned(),
            },
        );
        assert_eq!(timed_out.read().error, Some(LoginFailure::Expired));
    }

    #[test]
    fn a_cancelled_login_is_not_written_over_by_a_late_answer() {
        let active = Arc::new(ActiveLogin {
            view: Mutex::new(LoginView {
                bridge_id: "mautrix-whatsapp".to_owned(),
                network: "whatsapp".to_owned(),
                process_id: "process-1".to_owned(),
                flow_id: "qr".to_owned(),
                login_id: None,
                phase: LoginPhase::Cancelled,
                started_at: SystemTime::UNIX_EPOCH,
                started_by: StartedBy {
                    device_id: "device-1".to_owned(),
                    device_name: "the phone".to_owned(),
                },
                expires_at: SystemTime::UNIX_EPOCH + MAX_LOGIN_LIFETIME,
                generation: 2,
                step: None,
                login: None,
                error: None,
                detail: None,
            }),
            generation: AtomicU64::new(2),
            cancelled: std::sync::atomic::AtomicBool::new(true),
        });
        // The held request comes back after the cancel: a race the cancel
        // path is built to win, or the user would see a login they stopped
        // carry on.
        fail(
            &active,
            LoginFailure::BridgeRefused,
            "a late answer".to_owned(),
        );
        assert_eq!(active.read().phase, LoginPhase::Cancelled);
        assert_eq!(active.read().error, None);
    }

    #[test]
    fn the_pollable_state_names_the_step_and_how_long_it_is_valid() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let view = LoginView {
            bridge_id: "mautrix-whatsapp".to_owned(),
            network: "whatsapp".to_owned(),
            process_id: "process-1".to_owned(),
            flow_id: "qr".to_owned(),
            login_id: None,
            phase: LoginPhase::AwaitingRemote,
            started_at: now,
            started_by: StartedBy {
                device_id: "device-1".to_owned(),
                device_name: "the phone".to_owned(),
            },
            expires_at: now + MAX_LOGIN_LIFETIME,
            generation: 3,
            step: Some(StepView {
                step_id: "qr".to_owned(),
                txn_id: Some("bls_the-one-the-bridge-issued".to_owned()),
                step_type: StepType::DisplayAndWait,
                instructions: Some("Scan it".to_owned()),
                payload: json!({ "type": "qr", "data": "2@payload" }),
                received_at: now,
                valid_for: QR_CODE_VALIDITY,
            }),
            login: None,
            error: None,
            detail: None,
        };
        let document = login_json(&view);
        assert_eq!(document["state"], json!("awaiting_remote"));
        assert_eq!(document["generation"], json!(3));
        assert_eq!(document["step"]["type"], json!("display_and_wait"));
        assert_eq!(document["step"]["payload"]["data"], json!("2@payload"));
        assert_eq!(document["step"]["valid_for_seconds"], json!(20));
        assert_eq!(document["login"], Value::Null);
        assert_eq!(document["error"], Value::Null);
    }
}
