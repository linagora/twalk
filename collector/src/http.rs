//! The collector's internal HTTP endpoint (issue #281): the one route the
//! Companion Gateway calls on this process, `GET /freebusy`, the snapshot
//! seam the other way round. The Gateway reads the owner's free/busy here
//! on Hermes's behalf and never from the side service itself, so the one
//! process that holds the owner's grant is the one that reads their agenda,
//! and what leaves it is the intervals `freebusy.rs` allows — nothing else.
//!
//! The caller is the Gateway and only the Gateway: the bearer is the same
//! service token this collector presents to read the registry
//! (`COLLECTOR_GATEWAY_SERVICE_TOKEN`), which is why `COLLECTOR_HTTP_LISTEN`
//! cannot be set without it. The endpoint is not the Gateway's API: it
//! carries no device token, no session, and answers on the internal network
//! the compose file gives it. Every read is counted by outcome; the record
//! of who asked, for what window, and when, is the Gateway's (`hermes_read`),
//! because the Gateway is where Hermes's signature was checked.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::calendars::Calendars;
use crate::freebusy::Window;
use crate::metrics::Metrics;
use crate::replies::SharedAccess;
use crate::side::SideError;

/// Every outcome a read is counted under, `served` first.
pub const READ_OUTCOMES: [&str; 8] = [
    "served",
    "unauthenticated",
    "connection_unknown",
    "invalid_window",
    "window_too_wide",
    "connection_not_connected",
    "caldav_refused",
    "caldav_unreachable",
];

/// What the run loop knows about the calendar connection, for a read to
/// check before it asks the side service: the owner's id there, and the
/// connection's state as last observed.
#[derive(Debug, Clone, Default)]
pub struct CalendarAccess {
    pub owner_id: Option<String>,
    /// The state's name, as published; `None` before the first observation.
    pub state: Option<&'static str>,
}

pub type SharedCalendarAccess = Arc<RwLock<CalendarAccess>>;

#[derive(Clone)]
pub struct Endpoint {
    pub service_token: String,
    pub calendars: Option<Arc<Calendars>>,
    pub calendar_access: SharedCalendarAccess,
    pub access: SharedAccess,
    pub metrics: Arc<Metrics>,
}

pub fn router(endpoint: Endpoint) -> Router {
    Router::new()
        .route("/freebusy", get(free_busy))
        .with_state(endpoint)
}

#[derive(Debug, Deserialize)]
struct FreeBusyQuery {
    connection: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

async fn free_busy(
    State(endpoint): State<Endpoint>,
    headers: HeaderMap,
    Query(query): Query<FreeBusyQuery>,
) -> Response {
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    if bearer != Some(endpoint.service_token.as_str()) {
        return refuse(
            &endpoint,
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "this endpoint answers the Companion Gateway's service token and nothing else",
        );
    }
    let connection = query.connection.unwrap_or_default();
    let Some(calendars) = endpoint
        .calendars
        .as_ref()
        .filter(|calendars| calendars.connection == connection)
    else {
        return refuse(
            &endpoint,
            StatusCode::NOT_FOUND,
            "connection_unknown",
            &format!("this collector holds no calendar connection named {connection:?}"),
        );
    };
    let window = match Window::parse(
        query.from.as_deref().unwrap_or_default(),
        query.to.as_deref().unwrap_or_default(),
    ) {
        Ok(window) => window,
        Err(error) => {
            let code = error.code();
            return refuse(&endpoint, StatusCode::BAD_REQUEST, code, &error.message());
        }
    };
    let calendar = endpoint.calendar_access.read().await.clone();
    let token = endpoint.access.read().await.clone();
    let (owner_id, token) = match (calendar.state, calendar.owner_id, token) {
        (Some("connected"), Some(owner_id), Some(token)) => (owner_id, token),
        (state, _, _) => {
            let state = state.unwrap_or("unknown");
            endpoint
                .metrics
                .record_freebusy_read("connection_not_connected");
            warn!(
                connection,
                state,
                "a free/busy read on a calendar connection that is not connected was refused"
            );
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "connection_not_connected",
                    "state": state,
                    "detail": format!("the calendar connection {connection:?} is {state}; the agenda cannot be read until it is connected"),
                })),
            )
                .into_response();
        }
    };
    match calendars.free_busy(&owner_id, &token.token, &window).await {
        Ok(busy) => {
            endpoint.metrics.record_freebusy_read("served");
            info!(
                connection,
                from = %window.from,
                to = %window.to,
                intervals = busy.len(),
                "a free/busy read was served"
            );
            (
                StatusCode::OK,
                Json(json!({
                    "connection": connection,
                    "from": window.from.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                    "to": window.to.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                    "busy": busy,
                })),
            )
                .into_response()
        }
        Err(SideError::Refused { status }) => refuse(
            &endpoint,
            StatusCode::BAD_GATEWAY,
            "caldav_refused",
            &format!("the calendar service refused the free-busy report with HTTP {status}"),
        ),
        Err(SideError::Unreachable { detail }) => refuse(
            &endpoint,
            StatusCode::BAD_GATEWAY,
            "caldav_unreachable",
            &format!("the calendar service did not answer the free-busy report: {detail}"),
        ),
    }
}

fn refuse(endpoint: &Endpoint, status: StatusCode, code: &'static str, detail: &str) -> Response {
    endpoint.metrics.record_freebusy_read(code);
    warn!(%code, status = status.as_u16(), detail, "a free/busy read was refused");
    (status, Json(json!({ "error": code, "detail": detail }))).into_response()
}

/// Serves the endpoint until the process ends.
pub async fn serve(listener: tokio::net::TcpListener, endpoint: Endpoint) {
    if let Err(error) = axum::serve(listener, router(endpoint)).await {
        warn!(%error, "the internal HTTP endpoint stopped");
    }
}
