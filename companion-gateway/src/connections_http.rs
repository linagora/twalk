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

/// `GET /api/connections` — the registry, in the order declared, each entry
/// with the state its collector last said it was in (#275) when one did,
/// and the most recent transitions for the dashboard's feed.
async fn connections(State(gateway): State<Gateway>) -> Response {
    let mut connections: Vec<serde_json::Value> = gateway
        .connections()
        .connections()
        .iter()
        .map(|connection| serde_json::to_value(connection).expect("a connection serialises"))
        .collect();
    let mut transitions = Vec::new();
    if let Some(store) = gateway.connection_statuses() {
        let statuses = match store.connection_statuses() {
            Ok(statuses) => statuses,
            Err(error) => {
                tracing::warn!(%error, "the connection statuses could not be read");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({
                        "error": "store_unavailable",
                        "detail": "the Gateway's store could not be read"
                    })),
                )
                    .into_response();
            }
        };
        for entry in &mut connections {
            let id = entry["id"].as_str().unwrap_or_default();
            if let Some(status) = statuses.iter().find(|status| status.connection == id) {
                entry["status"] = status.json();
            }
        }
        transitions = store
            .connection_status_changes(crate::connection_status::RECENT_TRANSITIONS)
            .unwrap_or_default()
            .iter()
            .map(crate::connection_status::Change::json)
            .collect();
    }
    (
        StatusCode::OK,
        Json(json!({ "connections": connections, "transitions": transitions })),
    )
        .into_response()
}
