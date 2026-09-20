//! `GET /api/runtime` — whether a persona runtime is present here, read off
//! the bus (ticket #189). The HTTP half of [`crate::runtime_presence`].
//!
//! One route, one document, three answers. The Companion's dashboard and
//! personas screens draw their "no agent runtime is deployed" sentence from
//! `presence` instead of from a copy key that was true the day it was written
//! (#177). Behind #52's guard like every `/api` route: the owner's own read of
//! their own deployment.
//!
//! It is a read and nothing else. It starts, stops and configures nothing —
//! ADR 0013's refusal of a control API stands, and this is observation.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use serde_json::json;
use tracing::warn;

use crate::http::Gateway;
use crate::runtime_presence::Reading;

/// The runtime route. Merged into the Gateway's router like every other
/// ticket's, so it is behind the guard without asking.
pub fn routes() -> Router<Gateway> {
    Router::new().route("/api/runtime", get(read))
}

/// `GET /api/runtime` — is a persona runtime hosting personas here?
///
/// ```json
/// {
///   "presence": "present",
///   "personas": [{
///     "persona_id": "assistant", "consumer": "persona-assistant",
///     "liveness": "live", "activation": "active",
///     "waiting_pulls": 1, "ack_pending": 0
///   }]
/// }
/// ```
///
/// `presence` is `never` (no runtime configured with a persona has ever run
/// against this bus), `gone` (one was here and is not now) or `present`.
/// `personas` is one row per consumer the runtime created, live or not, with
/// the counters the verdict was read from, so a reader can check the verdict
/// against its evidence.
async fn read(State(gateway): State<Gateway>) -> Response {
    let Some(presence) = gateway.runtime_presence() else {
        return not_configured();
    };
    match presence.read().await {
        Ok(reading) => Json(reading_json(&reading)).into_response(),
        Err(error) => {
            // The bus did not answer. Not `never`: a bus that cannot be asked
            // is a different situation from a bus with no runtime on it, and
            // the screen has to be able to tell the two apart.
            warn!(%error, "the runtime's presence could not be read from the bus");
            api_error(
                StatusCode::BAD_GATEWAY,
                "bus_unreachable",
                &format!(
                    "the bus did not answer, so whether a runtime is present is unknown: {error:#}"
                ),
            )
        }
    }
}

fn reading_json(reading: &Reading) -> serde_json::Value {
    json!({
        "presence": reading.presence,
        "personas": reading.personas,
    })
}

/// This deployment has no bus configured, so there is nothing to read a
/// runtime's presence from: a `503` naming the variable, not `never`. "No
/// runtime has ever been here" and "this Gateway is not watching the bus" are
/// different claims, and only the first is about the runtime.
fn not_configured() -> Response {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "runtime_not_configured",
        "this Gateway cannot say whether a runtime is present: that is read off the bus and it \
         has none configured. Set GATEWAY_NATS_URL",
    )
}

fn api_error(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({ "error": code, "detail": detail }))).into_response()
}
