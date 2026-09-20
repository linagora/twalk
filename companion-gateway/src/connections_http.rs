//! `GET /api/connections` — the registry of connections (ADR 0033, #269), as
//! the Gateway holds it: every configured account the deployment observes or
//! acts through, with its kind, its label and, for the ones a bridge carries,
//! the bridge and its bot.
//!
//! Read-only: the registry is configuration (`GATEWAY_CONNECTIONS`, or one
//! connection per bridge), and the Companion reads it to draw a card per
//! connection rather than per network — the fix for `bridges.find(network)`,
//! first match wins, which is how granting one network would have granted an
//! employer's workspace and a personal one in one gesture (#272).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use serde_json::json;

use crate::http::Gateway;

pub fn routes() -> Router<Gateway> {
    Router::new().route("/api/connections", get(connections))
}

/// `GET /api/connections` — the registry, in the order declared.
async fn connections(State(gateway): State<Gateway>) -> Response {
    (
        StatusCode::OK,
        Json(json!({ "connections": gateway.connections().connections() })),
    )
        .into_response()
}
