//! The Gateway's HTTP origin: the Companion's static files, the health
//! endpoint, the metrics endpoint, and the observation layer every request
//! passes through (structured log line, `traceparent`, request counter).
//!
//! One origin serves all of it. The Companion is a static export with
//! client-side routing, so a path that matches no file of its build is
//! answered with the SPA fallback (`200.html`) and a 200 — a deep link
//! reloaded cold must load the app, not a 404. How a path resolves to a file
//! is [`crate::static_files`]. The Gateway's own API surface is the one
//! exception: under `/api/` a 404 stays a 404, as JSON, because a client
//! parsing an API response must never be handed an HTML page instead.
//!
//! Every answer this module gives is described in
//! `companion-gateway/openapi.yaml`, which the origin also serves
//! ([`crate::openapi`]): a route added here without a line there fails
//! `tests/openapi.rs`.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{any, get};
use axum::{middleware, Router};
use tower::ServiceExt;
use tower_http::services::ServeFile;
use tracing::{debug, warn};

use crate::approval::Approvals;
use crate::approval_http;
use crate::bootstrap::Bootstrap;
use crate::bootstrap_http;
use crate::bridge::Bridges;
use crate::bridge_http;
use crate::bridge_status::Statuses;
use crate::bridge_status_http;
use crate::connections_http;
use crate::consent_http;
use crate::consent_snapshot::{self, Snapshots};
use crate::contacts::Contacts;
use crate::contacts_http;
use crate::hermes_answer::Answers;
use crate::hermes_answer_http;
use crate::metrics::{Metrics, Route};
use crate::outbox::Outbox;
use crate::portals::Portals;
use crate::portals_http;
use crate::runtime_presence::RuntimePresence;
use crate::runtime_presence_http;
use crate::session::Sessions;
use crate::session_http;
use crate::settings::Settings;
use crate::settings_http;
use crate::static_files::{Resolution, Resolver};
use crate::suggestions::Suggestions;
use crate::suggestions_http;
use crate::trace;

/// Everything the handlers share. Cheap to clone: one `Arc` each.
#[derive(Clone)]
pub struct Gateway {
    /// How a request path resolves to a file of the Companion's build.
    companion: Arc<Resolver>,
    metrics: Arc<Metrics>,
    /// The user's session: the device list and the per-device tokens
    /// ([`crate::session`]). `None` when sign-in is not configured — the
    /// origin still serves the Companion, and its API is closed.
    sessions: Option<Arc<Sessions>>,
    /// Bootstrap: the registration relay and the Sensor's invitation
    /// ([`crate::bootstrap`], ticket #53). `None` when the Gateway has no
    /// homeserver to bootstrap against — each half is then answered with the
    /// variable that would enable it.
    bootstrap: Option<Arc<Bootstrap>>,
    /// The consent store and its outbox ([`crate::store`],
    /// [`crate::outbox`]). `None` when this deployment writes no consent —
    /// the consent endpoints then answer `503 consent_not_configured`.
    consent: Option<Arc<Outbox>>,
    /// The bridge login facade ([`crate::bridge`], ticket #55). Never
    /// `None`: a deployment with no bridge configured has an empty one, and
    /// `GET /api/bridges` answers an empty list rather than a refusal —
    /// which is the honest answer to "what can I connect?".
    bridges: Arc<Bridges>,
    /// The consent snapshot's own half ([`crate::consent_snapshot`], ticket
    /// #50): the service token that authenticates the Sensor's read, and the
    /// cap. `None` when `GATEWAY_SERVICE_TOKEN` is unset — the snapshot
    /// endpoint then answers `503 service_token_not_configured`, and nothing
    /// else changes.
    snapshots: Option<Arc<Snapshots>>,
    /// The bridge status half ([`crate::bridge_status`], ticket #56): each
    /// bridge's contract id, its network and the `as_token` its status
    /// pushes are verified against. `None` when this Gateway has no store to
    /// record a transition in and no bus to publish it on — the webhook then
    /// answers `503 bridge_status_not_configured`, rather than accepting a
    /// push it would throw away.
    statuses: Option<Arc<Statuses>>,
    /// The pending-contact projection ([`crate::contacts`], ticket #54): the
    /// list of who has written and is still waiting for a decision. `None`
    /// on the same terms as [`Self::consent`] — it needs the same bus and
    /// the same store — and the contact endpoints then answer `503
    /// contacts_not_configured` rather than an empty list, because "nobody
    /// has written to you" and "this Gateway is not watching" are very
    /// different claims.
    contacts: Option<Arc<Contacts>>,
    /// The approval half ([`crate::approval`], ticket #24): the act that
    /// turns a suggestion into an outbound reply. `None` on the same terms
    /// as [`Self::consent`] — it reads suggestions from the same bus and
    /// checks the same consent store — and the approval endpoints then
    /// answer `503 approvals_not_configured`, because "this Gateway cannot
    /// send" and "that suggestion cannot be approved" are very different
    /// claims and only one of them is about the suggestion.
    approvals: Option<Arc<Approvals>>,
    /// The suggestion projection ([`crate::suggestions`], ticket #97): the
    /// read side of the same act — what a persona proposed, for which
    /// message, and where it stands. `None` on the same terms as
    /// [`Self::approvals`], because a suggestion lives on the bus and a
    /// Gateway with no bus has nowhere to read one from; the suggestion
    /// endpoints then answer `503 suggestions_not_configured` rather than an
    /// empty list, because "nobody has suggested anything" and "this Gateway
    /// is not watching" are very different claims.
    suggestions: Option<Arc<Suggestions>>,
    /// Whether a persona runtime is present ([`crate::runtime_presence`],
    /// ticket #189): a read of the bus's consumer list, on whenever the bus
    /// is configured. Without one `GET /api/runtime` answers
    /// `503 runtime_not_configured` rather than `never`.
    runtime_presence: Option<Arc<RuntimePresence>>,
    /// The model configuration and the language preference
    /// ([`crate::settings`], ticket #98): what the operator chose to reason
    /// with, and the language a persona falls back to. `None` on the same
    /// terms as [`Self::sessions`] — the store is a file in
    /// `GATEWAY_STATE_DIR` and there is no owner to hold a preference for
    /// without one — so in practice the guard has already refused the
    /// request, and the handlers say `503 settings_not_configured` anyway
    /// rather than leaving a corner of the surface silent.
    settings: Option<Arc<Settings>>,
    /// The portal register ([`crate::portals`], ticket #105): which
    /// conversations this deployment's bridges have built, and where the
    /// Sensor stands in each. `None` when there is no Sensor to invite, no
    /// bridge configured, or no homeserver to ask — the portal endpoints
    /// then answer `503 portals_not_configured`, because "your bridges have
    /// built no conversations" and "this Gateway cannot see them" are very
    /// different claims and only one of them is about the user's messages.
    portals: Option<Arc<Portals>>,
    /// The registry of connections ([`crate::connections`], ADR 0033, #269).
    /// Always present: with no bridge and no declaration it is empty, which
    /// is a fact and not a refusal.
    connections: Arc<crate::connections::Registry>,
    /// What each connection last said about itself (#275), read off the bus
    /// into the store: `None` on a Gateway with no store, where nothing is
    /// recorded and no connection has a status.
    connection_statuses: Option<Arc<crate::store::Store>>,
    /// Hermes's answers ([`crate::hermes_answer`], ticket #206). `None` when
    /// no seam is configured, which is every deployment that has not opted
    /// into ADR 0032's integration — and then the route says which variable
    /// would open it rather than refusing as though the answer were wrong.
    answers: Option<Arc<Answers>>,
    /// Reads the clock in seconds since the epoch — injected so the uptime
    /// gauge and the request logs are testable against a clock the caller
    /// controls.
    now_unix_seconds: fn() -> u64,
}

impl Gateway {
    pub fn new(companion: Resolver, metrics: Arc<Metrics>, now_unix_seconds: fn() -> u64) -> Self {
        Self {
            companion: Arc::new(companion),
            metrics,
            sessions: None,
            bootstrap: None,
            consent: None,
            bridges: Arc::new(
                Bridges::new(Vec::new()).expect("no bridge configured is a valid configuration"),
            ),
            snapshots: None,
            statuses: None,
            contacts: None,
            approvals: None,
            suggestions: None,
            runtime_presence: None,
            settings: None,
            portals: None,
            connections: Arc::new(crate::connections::Registry::default()),
            connection_statuses: None,
            answers: None,
            now_unix_seconds,
        }
    }

    /// Adds the session half (ticket #52). A separate step rather than a
    /// constructor argument, so that a Gateway with no sign-in configured is
    /// still a Gateway and later tickets add their own halves the same way.
    pub fn with_sessions(mut self, sessions: Option<Arc<Sessions>>) -> Self {
        self.sessions = sessions;
        self
    }

    /// Adds the bootstrap half (ticket #53), the same way.
    pub fn with_bootstrap(mut self, bootstrap: Option<Arc<Bootstrap>>) -> Self {
        self.bootstrap = bootstrap;
        self
    }

    pub fn sessions(&self) -> Option<Arc<Sessions>> {
        self.sessions.clone()
    }

    pub fn bootstrap(&self) -> Option<Arc<Bootstrap>> {
        self.bootstrap.clone()
    }

    /// Adds the consent half (ticket #49), the same way the others are added.
    pub fn with_consent(mut self, consent: Option<Arc<Outbox>>) -> Self {
        self.consent = consent;
        self
    }

    pub fn consent(&self) -> Option<Arc<Outbox>> {
        self.consent.clone()
    }

    /// Adds the bridge facade (ticket #55), the same way the others are
    /// added.
    pub fn with_bridges(mut self, bridges: Arc<Bridges>) -> Self {
        self.bridges = bridges;
        self
    }

    pub fn bridges(&self) -> Arc<Bridges> {
        self.bridges.clone()
    }

    /// Adds the consent snapshot's half (ticket #50), the same way. Separate
    /// from [`Self::with_consent`] because the two are independently
    /// configured: a deployment can write consent without serving a snapshot
    /// (no service token), and an operator who set a token before setting a
    /// bus gets an answer that names what is actually missing.
    pub fn with_snapshots(mut self, snapshots: Option<Arc<Snapshots>>) -> Self {
        self.snapshots = snapshots;
        self
    }

    pub fn snapshots(&self) -> Option<Arc<Snapshots>> {
        self.snapshots.clone()
    }

    /// Adds the bridge status half (ticket #56), the same way. Separate from
    /// [`Self::with_bridges`] because the two are independently available: a
    /// Gateway always has a bridge facade (possibly empty), and it has a
    /// status half only once it has the store and the bus that a transition
    /// needs.
    pub fn with_statuses(mut self, statuses: Option<Arc<Statuses>>) -> Self {
        self.statuses = statuses;
        self
    }

    pub fn statuses(&self) -> Option<Arc<Statuses>> {
        self.statuses.clone()
    }

    /// Adds the half that receives Hermes's answers (ticket #206), the same
    /// way. Configured together with approvals — it reuses that half's store,
    /// bus, bounded lookup and refusal vocabulary — and still its own half,
    /// because a deployment can have approvals and no seam.
    pub fn with_answers(mut self, answers: Option<Arc<Answers>>) -> Self {
        self.answers = answers;
        self
    }

    pub fn answers(&self) -> Option<Arc<Answers>> {
        self.answers.clone()
    }

    /// Adds the pending-contact projection (ticket #54), the same way. It is
    /// configured together with consent — same store, same bus — but kept as
    /// its own half so that the endpoints of each say which of them a
    /// deployment is missing.
    pub fn with_contacts(mut self, contacts: Option<Arc<Contacts>>) -> Self {
        self.contacts = contacts;
        self
    }

    pub fn contacts(&self) -> Option<Arc<Contacts>> {
        self.contacts.clone()
    }

    /// Adds the approval half (ticket #24), the same way. Configured
    /// together with consent — same store, same bus — and kept as its own
    /// half so that the endpoints say which of them a deployment is missing.
    pub fn with_approvals(mut self, approvals: Option<Arc<Approvals>>) -> Self {
        self.approvals = approvals;
        self
    }

    pub fn approvals(&self) -> Option<Arc<Approvals>> {
        self.approvals.clone()
    }

    /// Adds the suggestion projection (ticket #97), the same way. Configured
    /// with the approval half — same bus, same store, same window — and kept
    /// as its own half so that a deployment missing one is told which.
    pub fn with_suggestions(mut self, suggestions: Option<Arc<Suggestions>>) -> Self {
        self.suggestions = suggestions;
        self
    }

    pub fn suggestions(&self) -> Option<Arc<Suggestions>> {
        self.suggestions.clone()
    }

    pub fn with_runtime_presence(mut self, presence: Option<Arc<RuntimePresence>>) -> Self {
        self.runtime_presence = presence;
        self
    }

    pub fn runtime_presence(&self) -> Option<Arc<RuntimePresence>> {
        self.runtime_presence.clone()
    }

    /// Adds the model and language settings (ticket #98), the same way. It
    /// is configured with sign-in rather than with consent: naming a model
    /// needs no bus, and an operator who has not set `GATEWAY_NATS_URL` can
    /// still say what their personas will reason with.
    pub fn with_settings(mut self, settings: Option<Arc<Settings>>) -> Self {
        self.settings = settings;
        self
    }

    pub fn settings(&self) -> Option<Arc<Settings>> {
        self.settings.clone()
    }

    /// Adds the portal register (ticket #105), the same way. It needs none
    /// of the store, the bus or the owner's session — only the homeserver,
    /// the Sensor's Matrix ID and each bridge's appservice token — which is
    /// why it is its own half rather than a corner of the bridge facade.
    pub fn with_portals(mut self, portals: Option<Arc<Portals>>) -> Self {
        self.portals = portals;
        self
    }

    /// The store the connection statuses are read from (#275).
    pub fn with_connection_statuses(mut self, store: Option<Arc<crate::store::Store>>) -> Self {
        self.connection_statuses = store;
        self
    }

    pub fn connection_statuses(&self) -> Option<Arc<crate::store::Store>> {
        self.connection_statuses.clone()
    }

    pub fn with_connections(mut self, connections: Arc<crate::connections::Registry>) -> Self {
        self.connections = connections;
        self
    }

    pub fn connections(&self) -> &crate::connections::Registry {
        &self.connections
    }

    pub fn portals(&self) -> Option<Arc<Portals>> {
        self.portals.clone()
    }

    /// The Matrix ID every bridge call acts as: this deployment's owner,
    /// from configuration and never from a request. mautrix's shared-secret
    /// auth takes the acting user on trust, so the Gateway is what decides
    /// whose login a provisioning call drives (ADR 0011).
    ///
    /// Empty only when sign-in is unconfigured, which #52's guard has
    /// already refused the request for.
    pub fn owner(&self) -> String {
        self.sessions
            .as_ref()
            .map(|sessions| sessions.owner().to_owned())
            .unwrap_or_default()
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }
}

/// The Gateway's routes. Everything that is not the Gateway's own surface is
/// the Companion's: its files, or its shell.
pub fn router(gateway: Gateway) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_exposition))
        // The Gateway's own HTTP description (ticket #63). Outside `/api`
        // and unauthenticated: it is what the Companion's build points a
        // client generator at, it must be readable before anyone can sign
        // in, and it holds no secret.
        .route("/openapi.yaml", get(crate::openapi::description))
        // The session's own routes (ticket #52). Merged, so every later
        // ticket's routes are added the same way — and every one of them is
        // behind the guard layered below without asking.
        .merge(session_http::routes())
        // Bootstrap: the registration relay and the Sensor's invitation
        // (ticket #53). Merged the same way, and behind the same guard.
        .merge(bootstrap_http::routes())
        // Consent's own routes (ticket #49), merged the same way — and
        // behind the same guard, so they need a device token without asking
        // for one.
        .merge(consent_http::routes())
        // The bridge login facade (ticket #55): the same merge, the same
        // guard. The Gateway holds each bridge's blocking login step and
        // these routes are what the Companion polls.
        .merge(bridge_http::routes())
        // The consent snapshot (ticket #50), merged the same way — and
        // behind the same guard, which for this one route requires the
        // service token instead of a device cookie.
        .merge(consent_snapshot::routes())
        // The bridge status webhook (ticket #56): the one route a *bridge*
        // calls. Outside `/api/` because its caller is not a browser, and
        // still inside the guard's table — as `Requirement::BridgeToken`, so
        // the policy has no hole in it.
        .merge(bridge_status_http::routes())
        // The pending contacts (ticket #54): the same merge, the same
        // guard. The owner's own read of who is waiting for a decision.
        .merge(contacts_http::routes())
        // Approvals (ticket #24): the same merge, the same guard. The one
        // act on this origin that causes a message to be sent — deliberate,
        // one suggestion at a time, and refused when the sender's consent is
        // no longer granted at that moment.
        .merge(approval_http::routes())
        // Reading suggestions (ticket #97): the same merge, the same guard.
        // The read side of the act above — a projection of the bus, written
        // nowhere, carrying the persona's own words and none of the message
        // they answer.
        .merge(suggestions_http::routes())
        // Whether a runtime is present (ticket #189): the same merge, the
        // same guard. A projection of the bus's consumer list — the one trace
        // a runtime leaves — so the screens can stop saying what was true
        // the day their copy was written (#177).
        .merge(runtime_presence_http::routes())
        // The model and the language (ticket #98): the same merge, the same
        // guard. Six of the seven operations are the owner's browser; the
        // seventh is the Hermes runtime's read, which the guard's table
        // declares as a service-token route, because the runtime is not a
        // device and a persona is never handed that token (ADR 0015).
        .merge(settings_http::routes())
        // The portal register (ticket #105): the same merge, the same
        // guard. What a bridge has actually built, and which of those
        // conversations the Sensor is inside — read live from the
        // homeserver, stored nowhere.
        .merge(portals_http::routes())
        .merge(connections_http::routes())
        // Hermes's answer (ticket #206, ADR 0032): the second route on this
        // origin whose caller is not a browser and not the Sensor, and the
        // first whose caller is outside the deployment altogether. Merged like
        // every other, so the guard's table decides what it must carry — a
        // signature of its own, verified by its own handler.
        .merge(hermes_answer_http::routes())
        // The Gateway's API surface keeps growing this way, and the prefix
        // answers as an API throughout: a JSON 404, never the app shell.
        .route("/api", any(api_not_found))
        .route("/api/{*rest}", any(api_not_found))
        .fallback(companion)
        // Inner: what each API route requires (a device token unless the
        // guard's table says otherwise — see [`crate::session_http`]).
        .layer(middleware::from_fn_with_state(
            gateway.clone(),
            session_http::guard,
        ))
        // Outer: so a refusal is counted and logged like any other answer.
        .layer(middleware::from_fn_with_state(gateway.clone(), observe))
        .with_state(gateway)
}

/// `GET /health` — the service's liveness and its version.
///
/// Always `200 OK` with a JSON document (described, like every other answer
/// of this origin, in `companion-gateway/openapi.yaml`):
///
/// - `status` (string): `ok` while the process answers. Liveness, not
///   readiness — the skeleton depends on nothing to be ready for.
/// - `version` (string, non-empty): the Gateway's package version. The stable
///   field of the version handshake: the Companion compares it with the
///   version baked into its own build and reloads when a service worker has
///   left it holding a stale app shell.
/// - `revision` (string, non-empty): the revision the binary was built from,
///   or `unknown`. Provenance for an operator, not part of the handshake.
/// - `companion_build` (string or null): the build id of the Companion this
///   origin serves, from the export's own `_app/version.json` (#222). The
///   running app knows its own; the two differing means the browser holds a
///   build this Gateway no longer ships, which is the version-skew nothing
///   could name before. `null` when the export carries no id.
async fn health(State(gateway): State<Gateway>) -> Response {
    Json(serde_json::json!({
        "status": "ok",
        "version": crate::VERSION,
        "revision": crate::REVISION,
        "companion_build": gateway.companion.build_id(),
    }))
    .into_response()
}

async fn metrics_exposition(State(gateway): State<Gateway>) -> Response {
    let body = gateway.metrics.render((gateway.now_unix_seconds)());
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

/// No API route matched: a 404 the Companion's fetch can parse.
async fn api_not_found(request: Request) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": "not_found",
            "path": request.uri().path(),
        })),
    )
        .into_response()
}

/// The Companion: the file its build has for this path, the other spelling of
/// a prerendered page (307), the SPA fallback with 200 for a client-side
/// route, and a plain 404 when there is no build to serve at all.
async fn companion(State(gateway): State<Gateway>, request: Request) -> Response {
    let query = request
        .uri()
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let cache = CachePolicy::for_path(request.uri().path());
    match gateway.companion.resolve(request.uri().path()).await {
        Resolution::File { path, content_type } => serve(&path, content_type, cache, request).await,
        Resolution::Fallback { path } => {
            // 200, not a redirect and not a 404: the route exists, in the
            // client-side router.
            serve(&path, "text/html; charset=utf-8", cache, request).await
        }
        Resolution::Redirect { location } => (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, format!("{location}{query}"))],
        )
            .into_response(),
        Resolution::NotFound => {
            let root = gateway.companion.root().display();
            debug!(static_dir = %root, "no companion build to serve");
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!(
                    "The Companion is not available: no build in {root}.\n\
                     Point GATEWAY_STATIC_DIR at a Companion build.\n"
                ),
            )
                .into_response()
        }
    }
}

/// What a browser may do with one of the Companion's files once it has it
/// (issue #222).
///
/// Stated, never left to the browser's heuristics: with no `Cache-Control` a
/// browser reuses a response for a fraction of its age without asking, so an
/// open Companion kept running the previous build after a redeploy — the HTML
/// it held named the previous chunks, which it also held, and nothing was
/// requested. The server reported itself current and the screen rendered fine,
/// and the two disagreed about what the code was; three fixes to #221 were
/// "verified" against that screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CachePolicy {
    /// `/_app/immutable/*`: the file's name is a hash of its content, so the
    /// name changes when the content does and the bytes under one name never
    /// do. Cached hard, for a year, and marked so the browser does not even
    /// revalidate.
    Immutable,
    /// Everything else, the HTML first: the entry document is what names the
    /// current chunks, so it is the one thing that must never be reused blind.
    /// Revalidated on every load; unchanged costs a 304.
    Revalidate,
}

impl CachePolicy {
    fn for_path(path: &str) -> Self {
        if path.starts_with("/_app/immutable/") {
            Self::Immutable
        } else {
            Self::Revalidate
        }
    }

    fn header(self) -> HeaderValue {
        HeaderValue::from_static(match self {
            Self::Immutable => "public, max-age=31536000, immutable",
            Self::Revalidate => "no-cache",
        })
    }
}

/// A validator for one representation of one file: its modification time and
/// size, and the encoding it went out in.
///
/// Metadata and not a digest of the bytes, because it is computed on every
/// request and the crypto module is megabytes; a build writes every file
/// afresh, so a redeploy moves the time. The encoding is part of it because
/// the brotli sibling and the raw file are two representations of one path,
/// and a cache handed the one under the other's tag would serve compressed
/// bytes to a client that cannot decode them.
fn etag(metadata: &std::fs::Metadata, encoding: Option<&HeaderValue>) -> Option<HeaderValue> {
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let encoding = encoding
        .and_then(|value| value.to_str().ok())
        .unwrap_or("identity");
    HeaderValue::from_str(&format!(
        "\"{:x}-{:x}-{:x}-{encoding}\"",
        modified.as_secs(),
        modified.subsec_nanos(),
        metadata.len()
    ))
    .ok()
}

/// Whether an `If-None-Match` names this representation, so the answer can be
/// a 304. `*` matches anything that exists, per RFC 9110 §13.1.2; a weak
/// prefix on the client's side is ignored, since the comparison is weak.
fn if_none_match(request: &axum::http::HeaderMap, etag: &HeaderValue) -> bool {
    let Some(candidates) = request
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Ok(etag) = etag.to_str() else {
        return false;
    };
    candidates.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

/// Serves one file, with the content type the path calls for and the cache
/// policy its half of the export gets.
///
/// `ServeFile` brings the parts worth not hand-rolling — `If-Modified-Since`,
/// byte ranges — and, when a pre-compressed sibling exists next to the file
/// (`crypto.wasm.br`, `crypto.wasm.gz`) and the client accepts that encoding,
/// serves it with the matching `Content-Encoding`. The Matrix crypto
/// WebAssembly is ~7.5 MB raw and ~1.3 MB brotli-compressed, which on a phone
/// is the difference between a usable onboarding and a broken one. The content
/// type is then overwritten with the one the *original* extension calls for,
/// which is what makes `.wasm` exactly `application/wasm` whichever encoding
/// went out.
///
/// What `ServeFile` does not bring is an `ETag` or `Cache-Control`, so those
/// are set here, and an `If-None-Match` that names the representation about
/// to go out turns the answer into a 304 before its body is read.
async fn serve(
    path: &std::path::Path,
    content_type: &'static str,
    cache: CachePolicy,
    request: Request,
) -> Response {
    let metadata = tokio::fs::metadata(path).await.ok();
    let request_headers = request.headers().clone();
    let served = ServeFile::new(path)
        .precompressed_br()
        .precompressed_gzip()
        .oneshot(request)
        .await
        .expect("serving a file is infallible");
    let mut response = served.map(Body::new);
    if !response.status().is_success() {
        return response;
    }
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, cache.header());
    let Some(etag) = metadata
        .as_ref()
        .and_then(|metadata| etag(metadata, response.headers().get(header::CONTENT_ENCODING)))
    else {
        return response;
    };
    if if_none_match(&request_headers, &etag) {
        let mut not_modified = Response::new(Body::empty());
        *not_modified.status_mut() = StatusCode::NOT_MODIFIED;
        not_modified.headers_mut().insert(header::ETAG, etag);
        not_modified
            .headers_mut()
            .insert(header::CACHE_CONTROL, cache.header());
        return not_modified;
    }
    response.headers_mut().insert(header::ETAG, etag);
    response
}

/// Every request passes through here: it continues (or originates) the trace
/// context, counts the answer, returns the `traceparent` to the caller and
/// leaves one structured log line behind.
async fn observe(State(gateway): State<Gateway>, request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let route = classify(&path);
    let traceparent = trace::propagate(
        request
            .headers()
            .get("traceparent")
            .and_then(|value| value.to_str().ok()),
    );
    let started = std::time::Instant::now();

    let mut response = next.run(request).await;

    let status = response.status();
    gateway.metrics.record_request(route, status.as_u16());
    match HeaderValue::from_str(&traceparent) {
        Ok(value) => {
            response.headers_mut().insert("traceparent", value);
        }
        // Unreachable: a traceparent is hex and dashes.
        Err(error) => warn!(%error, "the traceparent is not a valid header value"),
    }
    let elapsed_ms = started.elapsed().as_millis();
    debug!(
        %method,
        path = %path,
        route = route.label(),
        status = status.as_u16(),
        elapsed_ms,
        traceparent = %traceparent,
        "request answered"
    );
    response
}

/// The metrics route label of a request path — a closed set, so a
/// client-side-routed app cannot grow one time series per deep link.
fn classify(path: &str) -> Route {
    match path {
        "/health" => Route::Health,
        "/metrics" => Route::Metrics,
        "/openapi.yaml" => Route::OpenApi,
        "/api" => Route::Api,
        path if path.starts_with("/api/") => Route::Api,
        // The bridge status webhook and anything else under the Gateway's
        // reserved prefix (#56): its own label, so a flapping bridge is
        // visible in the exposition without being mistaken for the
        // Companion's own traffic.
        crate::hermes_answer::ANSWER_PATH => Route::HermesAnswer,
        path if crate::bridge_status::is_reserved_path(path) => Route::BridgeStatus,
        _ => Route::Companion,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_classified_into_a_closed_set_of_routes() {
        assert_eq!(classify("/health"), Route::Health);
        assert_eq!(classify("/metrics"), Route::Metrics);
        assert_eq!(classify("/openapi.yaml"), Route::OpenApi);
        assert_eq!(classify("/api"), Route::Api);
        assert_eq!(classify("/api/consent"), Route::Api);
        assert_eq!(classify("/api/bridges/mautrix-whatsapp/login"), Route::Api);
        assert_eq!(
            classify("/_twalk/bridges/bridge-whatsapp/status"),
            Route::BridgeStatus
        );
        assert_eq!(classify("/_twalk/hermes/answers"), Route::HermesAnswer);
        assert_eq!(classify("/"), Route::Companion);
        assert_eq!(classify("/onboarding/whatsapp"), Route::Companion);
        // A path that merely starts with the same letters is not the API.
        assert_eq!(classify("/apiary"), Route::Companion);
    }
}
