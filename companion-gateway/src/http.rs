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

use crate::bootstrap::Bootstrap;
use crate::bootstrap_http;
use crate::bridge::Bridges;
use crate::bridge_http;
use crate::bridge_status::Statuses;
use crate::bridge_status_http;
use crate::consent_http;
use crate::consent_snapshot::{self, Snapshots};
use crate::contacts::Contacts;
use crate::contacts_http;
use crate::metrics::{Metrics, Route};
use crate::outbox::Outbox;
use crate::session::Sessions;
use crate::session_http;
use crate::static_files::{Resolution, Resolver};
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
async fn health() -> Response {
    Json(serde_json::json!({
        "status": "ok",
        "version": crate::VERSION,
        "revision": crate::REVISION,
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
    match gateway.companion.resolve(request.uri().path()).await {
        Resolution::File { path, content_type } => serve(&path, content_type, request).await,
        Resolution::Fallback { path } => {
            // 200, not a redirect and not a 404: the route exists, in the
            // client-side router.
            serve(&path, "text/html; charset=utf-8", request).await
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

/// Serves one file, with the content type the path calls for.
///
/// `ServeFile` brings the parts worth not hand-rolling — conditional
/// requests, byte ranges — and, when a pre-compressed sibling exists next to
/// the file (`crypto.wasm.br`, `crypto.wasm.gz`) and the client accepts that
/// encoding, serves it with the matching `Content-Encoding`. The Matrix
/// crypto WebAssembly is ~7.5 MB raw and ~1.3 MB brotli-compressed, which on
/// a phone is the difference between a usable onboarding and a broken one.
/// The content type is then overwritten with the one the *original*
/// extension calls for, which is what makes `.wasm` exactly
/// `application/wasm` whichever encoding went out.
async fn serve(path: &std::path::Path, content_type: &'static str, request: Request) -> Response {
    let served = ServeFile::new(path)
        .precompressed_br()
        .precompressed_gzip()
        .oneshot(request)
        .await
        .expect("serving a file is infallible");
    let mut response = served.map(Body::new);
    if response.status().is_success() {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    }
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
        assert_eq!(classify("/"), Route::Companion);
        assert_eq!(classify("/onboarding/whatsapp"), Route::Companion);
        // A path that merely starts with the same letters is not the API.
        assert_eq!(classify("/apiary"), Route::Companion);
    }
}
