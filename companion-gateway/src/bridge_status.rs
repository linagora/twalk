//! Bridge status: how a bridge's connection state reaches the bus as
//! `bridge.status.changed.v1` (ticket #56, spec #47).
//!
//! # Where the state comes from
//!
//! Mautrix offers exactly one push channel, and this module is its other
//! end: the bridge POSTs its `BridgeState` to the URL its
//! `homeserver.status_endpoint` names — the Gateway's
//! `/_twalk/bridges/{bridge_id}/status` — authenticated with **its own
//! `as_token`**, retried with backoff and deduplicated by the bridge within
//! a TTL. Everything else was considered and rejected:
//!
//! - **Management-room notices are not parsed.** The default
//!   `bridge_status_notices` setting suppresses a logout entirely, and what
//!   it does emit is human markdown — a format that changes upstream without
//!   anybody calling it a breaking change.
//! - **Polling is not the primary channel.** A bridge's state lives in its
//!   own memory, is empty right after a bridge restart, and carries no "last
//!   connected" field; `state.timestamp` is the instant of the last *change*.
//!   So polling would miss every transition between two polls and would
//!   report a restarted bridge as brand new.
//!
//! Polling does have one job, and only one: **reconciliation at startup**
//! ([`reconcile`]). A Gateway that restarts has forgotten nothing — the last
//! state is in its store — but the world may have moved while it was down,
//! and `GET /_matrix/provision/v3/whoami` is how it finds out, rather than
//! carrying a stale `connected` forward until the next push.
//!
//! # The mapping table is the Gateway's, not the contract's
//!
//! Mautrix's vocabulary and the contract's are different sets, and the
//! translation lives here ([`BridgeStateEvent::contract_state`]) so that
//! exactly one place has to be read to know what a dashboard is being told.
//! The one mapping worth stating out loud is `BAD_CREDENTIALS` →
//! `session_expired`: **a session revoked from the user's phone reports
//! `BAD_CREDENTIALS`**, and no mautrix bridge emits `LOGGED_OUT` — a Gateway
//! that waited for the latter would never report an expired session at all.
//!
//! # Only transitions are published
//!
//! A bridge re-pushes the same state on its own schedule, and a
//! reconciliation re-reads a state that has not moved. Neither is an event:
//! the current state is compared with the one the store holds, and an
//! identical state records nothing. What the bus carries is the change, with
//! the state it came from — which is also why the store, and not memory, is
//! what remembers: after a restart `from_state` is still true.
//!
//! A bridge nobody has ever heard from counts as [`ContractState::INITIAL`] —
//! `disconnected`. That is the honest prior (nothing has said it is
//! connected) and it is what keeps a first push of `CONNECTING` from having
//! to invent a state it came from.

use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::bridge::{BridgeConfig, Bridges};
use crate::consent::{rfc3339_millis, Network};
use crate::metrics::Metrics;
use crate::outbox::Outbox;

/// The contract type this module produces, and the schema it validates
/// against.
pub const BRIDGE_STATUS_CHANGED_TYPE: &str = "fr.linagora.twalk.bridge.status.changed.v1";
pub const BRIDGE_STATUS_CHANGED_DATASCHEMA: &str =
    "https://schemas.twalk.dev/cloudevents/v1/bridge.status.changed.schema.json";

/// The longest `reason` the contract accepts. A bridge's message is written
/// for a human and is normally one line; a longer one is truncated rather
/// than dropped, because a truncated cause is still a cause and an event
/// refused by the schema is none.
pub const MAX_REASON: usize = 1024;

/// The path prefix a bridge pushes its status to, and the suffix that closes
/// it: `/_twalk/bridges/{bridge_id}/status`.
///
/// Deliberately **not** under `/api/`. `/api/` is the Companion's surface,
/// where every route requires the browser's device token by default (#52);
/// this one is called by a bridge, which has no device and no browser. It is
/// still declared in [`crate::session_http::requirement`] — as
/// `Requirement::BridgeToken` — so that the Gateway's authentication policy
/// stays one readable table with no hole in it.
pub const WEBHOOK_PREFIX: &str = "/_twalk/bridges/";
pub const WEBHOOK_SUFFIX: &str = "/status";

/// The webhook URL path for one bridge, as an operator writes it into that
/// bridge's `homeserver.status_endpoint`.
///
/// ```
/// # use twalk_companion_gateway::bridge_status::webhook_path;
/// assert_eq!(webhook_path("bridge-whatsapp"), "/_twalk/bridges/bridge-whatsapp/status");
/// ```
pub fn webhook_path(bridge_id: &str) -> String {
    format!("{WEBHOOK_PREFIX}{bridge_id}{WEBHOOK_SUFFIX}")
}

/// The `bridge_id` a request path names, when the path is a status webhook
/// and nothing else. Used by #52's guard to decide what the request must
/// carry, so it is deliberately strict: one segment, non-empty, no traversal.
///
/// ```
/// # use twalk_companion_gateway::bridge_status::webhook_bridge_id;
/// assert_eq!(webhook_bridge_id("/_twalk/bridges/bridge-whatsapp/status"), Some("bridge-whatsapp"));
/// assert_eq!(webhook_bridge_id("/_twalk/bridges//status"), None);
/// assert_eq!(webhook_bridge_id("/_twalk/bridges/a/b/status"), None);
/// assert_eq!(webhook_bridge_id("/api/bridges/x/status"), None);
/// ```
pub fn webhook_bridge_id(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(WEBHOOK_PREFIX)?;
    let bridge_id = rest.strip_suffix(WEBHOOK_SUFFIX)?;
    (!bridge_id.is_empty() && !bridge_id.contains('/')).then_some(bridge_id)
}

/// Whether a path is the Gateway's reserved bridge-webhook prefix at all —
/// `/_twalk/`, which no Companion route may claim. The guard uses it to keep
/// a mistyped webhook path closed instead of falling through to the
/// Companion's app shell.
pub fn is_reserved_path(path: &str) -> bool {
    path.starts_with("/_twalk/") || path == "/_twalk"
}

/// Mautrix's own state vocabulary, as `state_event` spells it.
///
/// Seven values, and no more: bridgev2 declares exactly these. An eighth
/// arriving from a future bridge parses as `None` and maps to
/// `disconnected` — the safe direction, because the alternative is telling a
/// dashboard that a state nobody understands is fine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeStateEvent {
    Connecting,
    Backfilling,
    Connected,
    TransientDisconnect,
    BadCredentials,
    UnknownError,
    LoggedOut,
}

impl BridgeStateEvent {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "CONNECTING" => Self::Connecting,
            "BACKFILLING" => Self::Backfilling,
            "CONNECTED" => Self::Connected,
            "TRANSIENT_DISCONNECT" => Self::TransientDisconnect,
            "BAD_CREDENTIALS" => Self::BadCredentials,
            "UNKNOWN_ERROR" => Self::UnknownError,
            "LOGGED_OUT" => Self::LoggedOut,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "CONNECTING",
            Self::Backfilling => "BACKFILLING",
            Self::Connected => "CONNECTED",
            Self::TransientDisconnect => "TRANSIENT_DISCONNECT",
            Self::BadCredentials => "BAD_CREDENTIALS",
            Self::UnknownError => "UNKNOWN_ERROR",
            Self::LoggedOut => "LOGGED_OUT",
        }
    }

    /// **The mapping table**, owned here (spec #47).
    ///
    /// - `BAD_CREDENTIALS` → `session_expired`. This, and not `LOGGED_OUT`,
    ///   is what a session revoked from the user's own phone reports; no
    ///   mautrix bridge emits `LOGGED_OUT` at all today, which is why it is
    ///   handled but never expected.
    /// - `TRANSIENT_DISCONNECT` → `degraded`. The bridge expects to come
    ///   back on its own; nothing is asked of the user.
    /// - `CONNECTING`, `BACKFILLING` → `starting`. Backfilling is a healthy
    ///   bridge that is not caught up yet, which is a start and not a fault.
    /// - `CONNECTED` → `connected`.
    /// - everything else → `disconnected`.
    pub fn contract_state(self) -> ContractState {
        match self {
            Self::BadCredentials => ContractState::SessionExpired,
            Self::TransientDisconnect => ContractState::Degraded,
            Self::Connecting | Self::Backfilling => ContractState::Starting,
            Self::Connected => ContractState::Connected,
            Self::UnknownError | Self::LoggedOut => ContractState::Disconnected,
        }
    }
}

/// The contract's state vocabulary — the five values `from_state` and
/// `to_state` take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractState {
    Starting,
    Connected,
    Degraded,
    Disconnected,
    SessionExpired,
}

impl ContractState {
    /// What a bridge nobody has heard from is assumed to be in. Nothing has
    /// said it is connected, so it is not: the prior errs towards "you have
    /// no working session", which is the direction a user can act on.
    pub const INITIAL: ContractState = ContractState::Disconnected;

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Connected => "connected",
            Self::Degraded => "degraded",
            Self::Disconnected => "disconnected",
            Self::SessionExpired => "session_expired",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "starting" => Self::Starting,
            "connected" => Self::Connected,
            "degraded" => Self::Degraded,
            "disconnected" => Self::Disconnected,
            "session_expired" => Self::SessionExpired,
            _ => return None,
        })
    }
}

/// One state a bridge reported, translated but not yet compared with
/// anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub state: ContractState,
    /// Mautrix's own `state_event`, verbatim, for the log line — `None` when
    /// the bridge sent a value this build does not know, which is itself
    /// worth seeing.
    pub reported: Option<String>,
    /// The bridge's human message, which becomes the event's `reason`.
    pub reason: Option<String>,
    pub last_message_at: Option<String>,
    /// When the state changed, as the bridge timestamps it — its
    /// `state.timestamp`, which is the instant of the last *change* and not
    /// of the push. Falls back to the Gateway's clock when the bridge sends
    /// none.
    pub occurred_at: String,
    /// Mautrix's `user_action` (`OPEN_NATIVE`, `RELOGIN`, `RESTART`), logged
    /// rather than published: the contract has no field for it.
    pub user_action: Option<String>,
}

/// Reads one mautrix `BridgeState` document.
///
/// The shape accepted is the flat `BridgeState` a bridge POSTs to
/// `status_endpoint`. A `GlobalBridgeState` — the `{"remoteState": {...}}`
/// wrapper the provisioning API answers with — is unwrapped too, because it
/// costs a dozen lines and a bridge that sends one would otherwise be read
/// as an empty state.
///
/// A body with no `state_event` at all is refused rather than guessed at:
/// the one thing this endpoint exists to carry is the state.
pub fn observe(body: &Value, now: SystemTime) -> Result<Observed, String> {
    let state = unwrap_state(body).ok_or_else(|| {
        "the body is not a mautrix BridgeState: no state_event member, and no remoteState \
         holding one"
            .to_owned()
    })?;
    let reported = state
        .get("state_event")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "state_event is required: it is the state being reported".to_owned())?;
    let parsed = BridgeStateEvent::parse(reported);
    let contract_state = parsed
        .map(BridgeStateEvent::contract_state)
        .unwrap_or(ContractState::Disconnected);
    Ok(Observed {
        state: contract_state,
        reported: parsed.map(|state| state.as_str().to_owned()),
        reason: reason_of(state),
        last_message_at: last_message_at_of(state),
        occurred_at: timestamp_of(state).unwrap_or_else(|| rfc3339_millis(now)),
        user_action: state
            .get("user_action")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    })
}

/// The `BridgeState` inside whatever the bridge sent: the document itself, or
/// the one login of a `GlobalBridgeState`'s `remoteState` map.
fn unwrap_state(body: &Value) -> Option<&Value> {
    if body.get("state_event").is_some() {
        return Some(body);
    }
    let remote = body
        .get("remoteState")
        .or_else(|| body.get("remote_states"))?
        .as_object()?;
    // v0.1 is one login per bridge instance, so a map with one entry is the
    // normal case; a bridge that somehow sent several is read in key order,
    // deterministically rather than by whatever the JSON parser happened to
    // do.
    remote
        .iter()
        .find(|(_, state)| state.get("state_event").is_some())
        .map(|(_, state)| state)
}

/// The event's `reason`: the bridge's own message, falling back to its error
/// code — a code is a worse sentence than a message and a much better one
/// than nothing.
fn reason_of(state: &Value) -> Option<String> {
    let text = ["message", "error"]
        .into_iter()
        .filter_map(|member| state.get(member).and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())?;
    Some(truncate(text, MAX_REASON))
}

/// `state.timestamp`, which mautrix sends as whole seconds since the epoch.
/// An RFC 3339 string is accepted too, so that a bridge (or a test) that
/// speaks the contract's own format is not misread as "no timestamp".
fn timestamp_of(state: &Value) -> Option<String> {
    let timestamp = state.get("timestamp")?;
    if let Some(text) = timestamp.as_str() {
        let text = text.trim();
        return (!text.is_empty()).then(|| text.to_owned());
    }
    from_unix_seconds(timestamp)
}

/// `info.last_message_at` (or `info.last_message_ts`), when the bridge puts
/// one there.
///
/// No mautrix bridge sets it today — `BridgeState.info` is a free-form map
/// and this is a key nothing writes — so the event's `last_message_at` is
/// normally absent. It is read rather than omitted because the contract has
/// the field, the dashboard wants it, and a bridge that starts reporting one
/// should not need a Gateway release.
fn last_message_at_of(state: &Value) -> Option<String> {
    let info = state.get("info")?;
    let value = ["last_message_at", "last_message_ts"]
        .into_iter()
        .find_map(|member| info.get(member))?;
    if let Some(text) = value.as_str() {
        let text = text.trim();
        return (!text.is_empty()).then(|| text.to_owned());
    }
    from_unix_seconds(value)
}

/// Whole (or fractional) seconds since the epoch, as RFC 3339. `None` for a
/// value that is not a positive number — a zero timestamp is mautrix's way
/// of saying "unset", not 1970.
fn from_unix_seconds(value: &Value) -> Option<String> {
    let seconds = value.as_f64()?;
    if !(seconds.is_finite() && seconds > 0.0) {
        return None;
    }
    let at = SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_secs_f64(seconds))?;
    Some(rfc3339_millis(at))
}

/// Truncates on a character boundary, so a multi-byte message is shortened
/// rather than made invalid.
fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// One state change, as the contract publishes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    /// The contract's `bridge_id`: `^bridge-[a-z0-9-]+$`, from configuration
    /// and stable across restarts.
    pub bridge_id: String,
    pub network: Network,
    pub from_state: ContractState,
    pub to_state: ContractState,
    pub occurred_at: String,
    pub reason: Option<String>,
    pub last_message_at: Option<String>,
}

impl Transition {
    /// The contract's deterministic id: the lowercase-hex sha256 of
    /// `bridge_id:to_state:occurred_at`, exactly as the schema's description
    /// spells the recipe. Re-emitting a recorded transition produces the same
    /// id, which is what makes the outbox's republish a de-duplication on the
    /// bus rather than a second event.
    pub fn event_id(&self) -> String {
        let key = format!(
            "{}:{}:{}",
            self.bridge_id,
            self.to_state.as_str(),
            self.occurred_at
        );
        Sha256::digest(key.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// The CloudEvent, rendered exactly as it will be published.
    ///
    /// `source` is `gateway://<domain>/bridges/<bridge_id>` — the bridge
    /// resource on this Gateway, which is what makes the producer of a
    /// status event identifiable without reading its payload. The `network`
    /// extension mirrors `data.network` so a consumer can filter one
    /// network's bridges server-side on NATS; the `consent` extension is
    /// never set, because a bridge state change has no consent dimension
    /// (the schema says so too).
    ///
    /// `time` is when the Gateway produced the event and `occurred_at` when
    /// the transition was observed. They differ on a republish, and only
    /// `occurred_at` is in the id.
    pub fn envelope(&self, domain: &str, produced_at: &str) -> Value {
        let mut event = json!({
            "specversion": "1.0",
            "id": self.event_id(),
            "source": format!("gateway://{domain}/bridges/{}", self.bridge_id),
            "type": BRIDGE_STATUS_CHANGED_TYPE,
            "time": produced_at,
            "subject": self.bridge_id,
            "datacontenttype": "application/json",
            "dataschema": BRIDGE_STATUS_CHANGED_DATASCHEMA,
            "network": self.network.as_str(),
            "data": {
                "bridge_id": self.bridge_id,
                "network": self.network.as_str(),
                "from_state": self.from_state.as_str(),
                "to_state": self.to_state.as_str(),
                "occurred_at": self.occurred_at,
            }
        });
        if let Some(reason) = &self.reason {
            event["data"]["reason"] = Value::from(reason.as_str());
        }
        if let Some(last_message_at) = &self.last_message_at {
            event["data"]["last_message_at"] = Value::from(last_message_at.as_str());
        }
        event
    }
}

/// One bridge as the status half knows it: the id its events carry, the
/// network they name, and the credential that proves a push came from it.
pub struct StatusBridge {
    /// The contract's `bridge_id` — the segment of the webhook URL, and what
    /// `data.bridge_id` carries.
    pub bridge_id: String,
    /// The instance's id in configuration and in `/api/bridges`
    /// ([`BridgeConfig::bridge_id`]). Different from the above on purpose:
    /// one names the software instance an operator configured, the other is
    /// the identity the contract froze.
    pub instance_id: String,
    pub network: Network,
    /// SHA-256 of the bridge's `as_token`. Kept as a digest, like the
    /// service token (#50): the comparison is then over two fixed-length
    /// values, so it leaks neither the token's length nor how far a guess
    /// got. `None` when the operator configured no token for this bridge —
    /// in which case its webhook is refused, never trusted.
    as_token_sha256: Option<[u8; 32]>,
}

impl StatusBridge {
    /// Whether this bridge's `as_token` is what was presented. `false` when
    /// no token is configured: an unverifiable push is refused, because
    /// trusting the compose network was explicitly rejected — the same
    /// reasoning that gave the Sensor a service token
    /// (`docs/architecture/security-model.md`).
    pub fn authenticates(&self, presented: &str) -> bool {
        match &self.as_token_sha256 {
            Some(expected) => {
                constant_time_eq(&Sha256::digest(presented.as_bytes()).into(), expected)
            }
            None => false,
        }
    }

    pub fn has_as_token(&self) -> bool {
        self.as_token_sha256.is_some()
    }
}

/// Why a status push was refused.
#[derive(Debug)]
pub enum StatusRefusal {
    /// No bridge with that id is configured.
    UnknownBridge { bridge_id: String },
    /// The bridge is configured without an `as_token`, so no push to it can
    /// be verified — and an unverified push is not accepted.
    NoAsToken { bridge_id: String },
    /// The credential is missing or wrong. One answer for both.
    Unauthenticated,
    /// The body is not a mautrix `BridgeState`.
    Invalid { detail: String },
    /// The transition could not be recorded.
    Store { detail: String },
}

impl StatusRefusal {
    pub fn label(&self) -> &'static str {
        match self {
            Self::UnknownBridge { .. } => "unknown_bridge",
            Self::NoAsToken { .. } => "as_token_not_configured",
            Self::Unauthenticated => "unauthenticated",
            Self::Invalid { .. } => "invalid_request",
            Self::Store { .. } => "store_unavailable",
        }
    }
}

/// The status half of the bridge facade: every bridge's identity and
/// credential, and the store-backed comparison that turns a reported state
/// into an event — or into nothing.
pub struct Statuses {
    bridges: Vec<StatusBridge>,
    /// #49's publication machinery, reused rather than doubled: the same
    /// journal, the same "commit, then publish, then mark published" loop,
    /// the same `Nats-Msg-Id` de-duplication. A second publisher would be a
    /// second thing to get wrong about exactly-once.
    outbox: Arc<Outbox>,
    metrics: Arc<Metrics>,
    now: fn() -> SystemTime,
}

impl Statuses {
    /// Builds the status half from the bridge configuration.
    ///
    /// Fails when a bridge's contract id or network is not one the contract
    /// accepts: an event that the schema would refuse is a bridge whose
    /// health can never be reported, which is an operator error to fix at
    /// startup rather than a warning in a log nobody reads.
    pub fn new(
        configured: &[BridgeConfig],
        outbox: Arc<Outbox>,
        metrics: Arc<Metrics>,
        now: fn() -> SystemTime,
    ) -> Result<Self> {
        let mut bridges = Vec::with_capacity(configured.len());
        for config in configured {
            anyhow::ensure!(
                is_contract_bridge_id(&config.status_bridge_id),
                "the status bridge_id configured for bridge {:?} is {:?}, which the contract \
                 does not accept: bridge.status.changed requires ^bridge-[a-z0-9-]+$. Set \
                 GATEWAY_BRIDGE_{}_STATUS_ID to something like bridge-whatsapp",
                config.bridge_id,
                config.status_bridge_id,
                crate::config::variable_slug(&config.bridge_id),
            );
            anyhow::ensure!(
                !bridges
                    .iter()
                    .any(|existing: &StatusBridge| existing.bridge_id == config.status_bridge_id),
                "two bridges report status under the id {:?}: a bridge_id identifies one \
                 instance on the bus, and two would overwrite each other's state",
                config.status_bridge_id
            );
            let network = Network::parse(&config.network).with_context(|| {
                format!(
                    "the network configured for bridge {:?} is {:?}, which is not one the \
                     contract knows ({}). Set GATEWAY_BRIDGE_{}_NETWORK",
                    config.bridge_id,
                    config.network,
                    Network::ALL
                        .iter()
                        .map(|network| network.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    crate::config::variable_slug(&config.bridge_id),
                )
            })?;
            bridges.push(StatusBridge {
                bridge_id: config.status_bridge_id.clone(),
                instance_id: config.bridge_id.clone(),
                network,
                as_token_sha256: config
                    .as_token
                    .as_ref()
                    .map(|token| Sha256::digest(token.as_bytes()).into()),
            });
        }
        Ok(Self {
            bridges,
            outbox,
            metrics,
            now,
        })
    }

    pub fn bridges(&self) -> &[StatusBridge] {
        &self.bridges
    }

    fn bridge(&self, bridge_id: &str) -> Option<&StatusBridge> {
        self.bridges
            .iter()
            .find(|bridge| bridge.bridge_id == bridge_id)
    }

    /// Verifies a push and records what it reports.
    ///
    /// `Ok(None)` means the state had not moved: the push was accepted and
    /// nothing was published, which is the de-duplication the ticket asks
    /// for. `Ok(Some(_))` is the transition that reached the outbox.
    pub fn push(
        &self,
        bridge_id: &str,
        presented_token: Option<&str>,
        body: &Value,
    ) -> Result<Option<Transition>, StatusRefusal> {
        let Some(bridge) = self.bridge(bridge_id) else {
            return Err(StatusRefusal::UnknownBridge {
                bridge_id: bridge_id.to_owned(),
            });
        };
        if !bridge.has_as_token() {
            return Err(StatusRefusal::NoAsToken {
                bridge_id: bridge_id.to_owned(),
            });
        }
        let authenticated = presented_token
            .map(|token| bridge.authenticates(token))
            .unwrap_or(false);
        if !authenticated {
            return Err(StatusRefusal::Unauthenticated);
        }
        let observed =
            observe(body, (self.now)()).map_err(|detail| StatusRefusal::Invalid { detail })?;
        self.observed(bridge, &observed, "webhook")
            .map_err(|error| StatusRefusal::Store {
                detail: format!("{error:#}"),
            })
    }

    /// Records one observation, whatever channel it arrived on, and returns
    /// the transition it caused — `None` when the state had not moved.
    fn observed(
        &self,
        bridge: &StatusBridge,
        observed: &Observed,
        channel: &'static str,
    ) -> Result<Option<Transition>> {
        let from_state = self
            .outbox
            .store()
            .bridge_status(&bridge.bridge_id)
            .context("failed to read the bridge's last known state")?
            .unwrap_or(ContractState::INITIAL);
        if from_state == observed.state {
            debug!(
                bridge = %bridge.bridge_id,
                state = observed.state.as_str(),
                reported = observed.reported.as_deref().unwrap_or("(unknown)"),
                channel,
                "a bridge reported the state it was already in; nothing published"
            );
            self.metrics.record_bridge_status(channel, "unchanged");
            return Ok(None);
        }
        let transition = Transition {
            bridge_id: bridge.bridge_id.clone(),
            network: bridge.network,
            from_state,
            to_state: observed.state,
            occurred_at: observed.occurred_at.clone(),
            reason: observed.reason.clone(),
            last_message_at: observed.last_message_at.clone(),
        };
        let committed = self.outbox.record_bridge_status(&transition)?;
        if committed.replayed {
            debug!(
                bridge = %bridge.bridge_id,
                event_id = %committed.event_id,
                "the identical bridge transition was already recorded; keeping the first"
            );
        } else {
            info!(
                bridge = %bridge.bridge_id,
                network = bridge.network.as_str(),
                from_state = from_state.as_str(),
                to_state = observed.state.as_str(),
                reported = observed.reported.as_deref().unwrap_or("(unknown)"),
                user_action = observed.user_action.as_deref().unwrap_or("(none)"),
                channel,
                "a bridge changed state"
            );
        }
        self.metrics
            .record_bridge_status(channel, observed.state.as_str());
        Ok(Some(transition))
    }
}

/// Whether a value is the contract's `bridge_id`: `^bridge-[a-z0-9-]+$`.
///
/// ```
/// # use twalk_companion_gateway::bridge_status::is_contract_bridge_id;
/// assert!(is_contract_bridge_id("bridge-whatsapp"));
/// assert!(is_contract_bridge_id("bridge-sms-android-1"));
/// assert!(!is_contract_bridge_id("bridge-"));
/// assert!(!is_contract_bridge_id("mautrix-whatsapp"));
/// assert!(!is_contract_bridge_id("bridge-WhatsApp"));
/// ```
pub fn is_contract_bridge_id(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("bridge-") else {
        return false;
    };
    !rest.is_empty()
        && rest.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
}

/// Startup reconciliation: `GET /_matrix/provision/v3/whoami` per bridge, so
/// that a Gateway which restarts does not carry a stale state forward.
///
/// What it deliberately does *not* do is invent one. A bridge that cannot be
/// reached leaves the stored state alone and logs a warning: in a compose
/// stack the Gateway and the bridges start together, and publishing
/// `disconnected` for every bridge that is three seconds behind would be
/// noise the user would learn to ignore. The webhook corrects it as soon as
/// the bridge has anything to say.
///
/// A bridge that answers with **no login** is a different thing and is
/// reported: there is no session, so the state is `disconnected`. A login
/// with no state yet — what a bridge that has just restarted looks like,
/// because its state is in memory — is `starting`: the bridge holds the
/// login and is bringing it up.
pub async fn reconcile(statuses: Arc<Statuses>, bridges: Arc<Bridges>, owner: String) {
    for bridge in statuses.bridges() {
        let observed = match bridges.whoami(&bridge.instance_id, &owner).await {
            Ok(logins) => whoami_state(&logins, (statuses.now)()),
            Err(refusal) => {
                warn!(
                    bridge = %bridge.bridge_id,
                    outcome = refusal.label(),
                    "could not reconcile a bridge's status at startup: the last known state \
                     is kept, and the bridge's own webhook will correct it"
                );
                statuses
                    .metrics
                    .record_bridge_status("startup", "unreachable");
                continue;
            }
        };
        match statuses.observed(bridge, &observed, "startup") {
            Ok(Some(transition)) => info!(
                bridge = %bridge.bridge_id,
                to_state = transition.to_state.as_str(),
                "startup reconciliation found a bridge in a state it had not reported"
            ),
            Ok(None) => debug!(
                bridge = %bridge.bridge_id,
                state = observed.state.as_str(),
                "startup reconciliation agreed with the stored state"
            ),
            Err(error) => warn!(
                bridge = %bridge.bridge_id,
                %error,
                "failed to record a reconciled bridge status"
            ),
        }
    }
}

/// What `whoami`'s `logins` array says about the bridge's state.
///
/// One login per bridge instance in v0.1: the first login is the login. A
/// second one is read only for the log line, because the contract has no
/// login dimension to report it under (spec #47).
pub fn whoami_state(logins: &[Value], now: SystemTime) -> Observed {
    let Some(login) = logins.first() else {
        // No login at all: nothing is connected, and nothing can be. This is
        // the state a bridge the user has never logged in to is in, and the
        // one a logout leaves behind.
        return Observed {
            state: ContractState::Disconnected,
            reported: None,
            reason: Some("the bridge holds no login".to_owned()),
            last_message_at: None,
            occurred_at: rfc3339_millis(now),
            user_action: None,
        };
    };
    match login.get("state") {
        Some(state) if state.get("state_event").is_some() => {
            observe(state, now).unwrap_or_else(|_| starting(now))
        }
        // A login the bridge holds but says nothing about. Its state lives in
        // the bridge's memory and is empty right after a restart, so this is
        // what "bringing the login back up" looks like from outside.
        _ => starting(now),
    }
}

fn starting(now: SystemTime) -> Observed {
    Observed {
        state: ContractState::Starting,
        reported: None,
        reason: Some("the bridge holds a login and has not reported its state yet".to_owned()),
        last_message_at: None,
        occurred_at: rfc3339_millis(now),
        user_action: None,
    }
}

/// Compares two digests without an early return, so the time a refusal takes
/// says nothing about how much of a guess was right.
fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mapping_table_is_the_specs() {
        for (reported, expected) in [
            ("CONNECTING", ContractState::Starting),
            ("BACKFILLING", ContractState::Starting),
            ("CONNECTED", ContractState::Connected),
            ("TRANSIENT_DISCONNECT", ContractState::Degraded),
            ("BAD_CREDENTIALS", ContractState::SessionExpired),
            ("UNKNOWN_ERROR", ContractState::Disconnected),
            ("LOGGED_OUT", ContractState::Disconnected),
        ] {
            let state = BridgeStateEvent::parse(reported)
                .unwrap_or_else(|| panic!("{reported} is one of mautrix's states"));
            assert_eq!(state.contract_state(), expected, "{reported}");
            assert_eq!(state.as_str(), reported);
        }
    }

    #[test]
    fn a_revoked_session_is_session_expired_and_it_is_bad_credentials_that_says_so() {
        // The property the whole ticket turns on: no mautrix bridge emits
        // LOGGED_OUT, so a Gateway that waited for it would never report an
        // expired session.
        assert_eq!(
            BridgeStateEvent::BadCredentials.contract_state(),
            ContractState::SessionExpired
        );
        assert_eq!(
            BridgeStateEvent::LoggedOut.contract_state(),
            ContractState::Disconnected,
            "handled, but no bridge emits it"
        );
    }

    #[test]
    fn a_state_this_build_does_not_know_is_disconnected_rather_than_fine() {
        assert_eq!(BridgeStateEvent::parse("FUTURE_STATE"), None);
        let observed = observe(&json!({ "state_event": "FUTURE_STATE" }), SystemTime::now())
            .expect("a state_event is a state");
        assert_eq!(observed.state, ContractState::Disconnected);
        assert_eq!(observed.reported, None, "the unknown value is not invented");
    }

    #[test]
    fn a_push_carries_the_bridges_message_its_instant_and_its_user_action() {
        let observed = observe(
            &json!({
                "state_event": "BAD_CREDENTIALS",
                "error": "whatsapp-logged-out",
                "message": "You were logged out from another device",
                "user_action": "RELOGIN",
                "timestamp": 1_789_000_000u64,
            }),
            SystemTime::now(),
        )
        .expect("a bridge state");
        assert_eq!(observed.state, ContractState::SessionExpired);
        assert_eq!(
            observed.reason.as_deref(),
            Some("You were logged out from another device"),
            "the message is preferred to the error code"
        );
        assert_eq!(observed.user_action.as_deref(), Some("RELOGIN"));
        assert!(
            observed.occurred_at.starts_with("2026-"),
            "the bridge's own timestamp is the instant of the change: {}",
            observed.occurred_at
        );
    }

    #[test]
    fn an_error_code_stands_in_when_the_bridge_sends_no_message() {
        let observed = observe(
            &json!({ "state_event": "UNKNOWN_ERROR", "error": "ws-connection-error" }),
            SystemTime::now(),
        )
        .expect("a bridge state");
        assert_eq!(observed.reason.as_deref(), Some("ws-connection-error"));
    }

    #[test]
    fn a_body_that_is_not_a_bridge_state_is_refused_rather_than_guessed_at() {
        assert!(observe(&json!({}), SystemTime::now()).is_err());
        assert!(observe(&json!({ "state_event": "" }), SystemTime::now()).is_err());
        assert!(observe(&Value::Null, SystemTime::now()).is_err());
    }

    #[test]
    fn a_global_bridge_state_wrapper_is_unwrapped() {
        let observed = observe(
            &json!({ "remoteState": { "33612345678": { "state_event": "CONNECTED" } } }),
            SystemTime::now(),
        )
        .expect("a wrapped bridge state");
        assert_eq!(observed.state, ContractState::Connected);
    }

    #[test]
    fn a_reason_longer_than_the_contract_allows_is_truncated_not_dropped() {
        let long = "é".repeat(2000);
        let observed = observe(
            &json!({ "state_event": "UNKNOWN_ERROR", "message": long }),
            SystemTime::now(),
        )
        .expect("a bridge state");
        let reason = observed.reason.expect("a reason");
        assert!(reason.len() <= MAX_REASON);
        assert!(!reason.is_empty());
    }

    #[test]
    fn the_event_id_is_the_contracts_recipe() {
        let transition = Transition {
            bridge_id: "bridge-gmessages-1".to_owned(),
            network: Network::Sms,
            from_state: ContractState::Starting,
            to_state: ContractState::Connected,
            occurred_at: "2026-09-17T10:20:00Z".to_owned(),
            reason: None,
            last_message_at: None,
        };
        let expected: String = Sha256::digest(b"bridge-gmessages-1:connected:2026-09-17T10:20:00Z")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(transition.event_id(), expected);
        // And it is the id of the contract's own fixture, which is the same
        // bridge, the same state and the same instant.
        assert_eq!(
            transition.event_id(),
            "029285728dfbbe8e00996a1e7a4c9baceb984a94e8360eb4b593ff9a70d661d9"
        );
    }

    #[test]
    fn the_envelope_names_the_bridge_resource_and_mirrors_the_network() {
        let transition = Transition {
            bridge_id: "bridge-gmessages-1".to_owned(),
            network: Network::Sms,
            from_state: ContractState::Starting,
            to_state: ContractState::Connected,
            occurred_at: "2026-09-17T10:20:00Z".to_owned(),
            reason: None,
            last_message_at: None,
        };
        let envelope = transition.envelope("example.com", "2026-09-17T10:20:00Z");
        assert_eq!(
            envelope["source"],
            json!("gateway://example.com/bridges/bridge-gmessages-1")
        );
        assert_eq!(envelope["subject"], json!("bridge-gmessages-1"));
        assert_eq!(envelope["network"], json!("sms"));
        assert_eq!(envelope["data"]["from_state"], json!("starting"));
        assert_eq!(envelope["data"]["to_state"], json!("connected"));
        assert_eq!(envelope["data"].get("reason"), None);
        assert_eq!(envelope.get("consent"), None);
    }

    #[test]
    fn a_reason_and_a_last_message_reach_the_event_when_they_are_known() {
        let transition = Transition {
            bridge_id: "bridge-whatsapp".to_owned(),
            network: Network::Whatsapp,
            from_state: ContractState::Connected,
            to_state: ContractState::SessionExpired,
            occurred_at: "2026-09-17T10:20:00.000Z".to_owned(),
            reason: Some("session revoked by network".to_owned()),
            last_message_at: Some("2026-09-17T09:00:00.000Z".to_owned()),
        };
        let envelope = transition.envelope("example.com", "2026-09-17T10:21:00.000Z");
        assert_eq!(
            envelope["data"]["reason"],
            json!("session revoked by network")
        );
        assert_eq!(
            envelope["data"]["last_message_at"],
            json!("2026-09-17T09:00:00.000Z")
        );
    }

    #[test]
    fn whoami_reports_no_login_as_disconnected_and_a_silent_login_as_starting() {
        let now = SystemTime::now();
        assert_eq!(whoami_state(&[], now).state, ContractState::Disconnected);
        assert_eq!(
            whoami_state(&[json!({ "id": "33612345678" })], now).state,
            ContractState::Starting
        );
        assert_eq!(
            whoami_state(
                &[json!({ "id": "x", "state": { "state_event": "CONNECTED" } })],
                now
            )
            .state,
            ContractState::Connected
        );
        assert_eq!(
            whoami_state(
                &[json!({ "id": "x", "state": { "state_event": "BAD_CREDENTIALS" } })],
                now
            )
            .state,
            ContractState::SessionExpired
        );
    }

    #[test]
    fn a_webhook_path_round_trips_and_nothing_else_parses_as_one() {
        assert_eq!(
            webhook_bridge_id(&webhook_path("bridge-whatsapp")),
            Some("bridge-whatsapp")
        );
        assert_eq!(webhook_bridge_id("/_twalk/bridges/x/status/extra"), None);
        assert_eq!(webhook_bridge_id("/_twalk/bridges/status"), None);
        assert!(is_reserved_path("/_twalk/bridges/x/status"));
        assert!(!is_reserved_path("/_twalkish/x"));
    }
}
