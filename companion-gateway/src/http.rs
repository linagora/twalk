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

use crate::metrics::{Metrics, Route};
use crate::static_files::{Resolution, Resolver};
use crate::trace;

/// Everything the handlers share. Cheap to clone: one `Arc` each.
#[derive(Clone)]
pub struct Gateway {
    /// How a request path resolves to a file of the Companion's build.
    companion: Arc<Resolver>,
    metrics: Arc<Metrics>,
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
            now_unix_seconds,
        }
    }
}

/// The Gateway's routes. Everything that is not the Gateway's own surface is
/// the Companion's: its files, or its shell.
pub fn router(gateway: Gateway) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_exposition))
        // The Gateway's API surface is empty in this skeleton (consent,
        // session and bridge routes land in the later tickets of spec #46),
        // but the prefix already answers as an API: a JSON 404, never the
        // app shell.
        .route("/api", any(api_not_found))
        .route("/api/{*rest}", any(api_not_found))
        .fallback(companion)
        .layer(middleware::from_fn_with_state(gateway.clone(), observe))
        .with_state(gateway)
}

/// `GET /health` — the service's liveness and its version.
///
/// Always `200 OK` with a JSON document (a description ticket #63 will
/// formalise):
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
        "/api" => Route::Api,
        path if path.starts_with("/api/") => Route::Api,
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
        assert_eq!(classify("/api"), Route::Api);
        assert_eq!(classify("/api/consent"), Route::Api);
        assert_eq!(classify("/"), Route::Companion);
        assert_eq!(classify("/onboarding/whatsapp"), Route::Companion);
        // A path that merely starts with the same letters is not the API.
        assert_eq!(classify("/apiary"), Route::Companion);
    }
}
