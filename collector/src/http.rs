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
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::calendars::Calendars;
use crate::freebusy::Window;
use crate::metrics::Metrics;
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

/// Which of the endpoint's two reads a refusal belongs to (#355). They
/// share their shape and their refusal codes and are counted apart, because
/// "how often was my agenda pulled" and "how often was an event asked
/// about" are two questions an owner asks separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    FreeBusy,
    EventFacts,
}

/// Every outcome a read of one event's facts is counted under (#355).
/// `served` covers an event this collector holds and one it does not: what
/// differs there is the answer's `found`, not whether the read worked.
pub const EVENT_FACT_OUTCOMES: [&str; 7] = [
    "served",
    "unauthenticated",
    "connection_unknown",
    "invalid_uid",
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
    pub access: crate::replies::SharedCredential,
    pub metrics: Arc<Metrics>,
}

pub fn router(endpoint: Endpoint) -> Router {
    Router::new()
        .route("/freebusy", get(free_busy))
        .route("/event-facts", get(event_facts))
        .with_state(endpoint)
}

/// Whether a request carries this collector's own service token — the
/// Gateway's, and nobody else's. Compared as digests, in constant time: the
/// Companion Gateway's habit with this token (`consent_snapshot.rs`), kept
/// on this side, and one function so the two routes cannot drift into two
/// standards.
fn authenticated(endpoint: &Endpoint, headers: &HeaderMap) -> bool {
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    let presented = bearer.map(|token| Sha256::digest(token.as_bytes()));
    let expected = Sha256::digest(endpoint.service_token.as_bytes());
    presented.is_some_and(|presented| presented == expected)
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
    if !authenticated(&endpoint, &headers) {
        return refuse(
            &endpoint,
            Read::FreeBusy,
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "this endpoint answers the Companion Gateway's service token and nothing else",
            None,
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
            Read::FreeBusy,
            StatusCode::NOT_FOUND,
            "connection_unknown",
            &format!("this collector holds no calendar connection named {connection:?}"),
            None,
        );
    };
    let window = match Window::parse(
        query.from.as_deref().unwrap_or_default(),
        query.to.as_deref().unwrap_or_default(),
    ) {
        Ok(window) => window,
        Err(error) => {
            return refuse(
                &endpoint,
                Read::FreeBusy,
                StatusCode::BAD_REQUEST,
                error.code(),
                &error.message(),
                None,
            );
        }
    };
    // The connection's state as the run loop last observed it, and the two
    // things a read needs that the loop holds — the owner's id on the side
    // service and the credential. A state of `connected` with either
    // missing is a round that has not completed yet: `unknown`, not
    // `connected`, since "connected but unreadable" is not a state.
    let calendar = endpoint.calendar_access.read().await.clone();
    let token = endpoint.access.read().await.clone();
    let (owner_id, token) = match (calendar.state, calendar.owner_id, token) {
        (Some("connected"), Some(owner_id), Some(token)) => (owner_id, token),
        (state, _, _) => {
            let state = state
                .filter(|state| *state != "connected")
                .unwrap_or("unknown");
            return refuse(
                &endpoint,
                Read::FreeBusy,
                StatusCode::CONFLICT,
                "connection_not_connected",
                &format!(
                    "the calendar connection {connection:?} is {state}; the agenda cannot be \
                     read until it is connected"
                ),
                Some(state),
            );
        }
    };
    match calendars.free_busy(&owner_id, &token, &window).await {
        Ok((busy, zone)) => {
            endpoint.metrics.record_freebusy_read("served");
            // The zone and the hour it is there travel with the intervals
            // (#369). A zone the calendar declares but that is not an IANA
            // name is worth saying out loud and worth nothing to a reader, so
            // it is dropped rather than passed on: a model handed "Romance
            // Standard Time" will use it in a sentence.
            let local = zone.as_ref().and_then(|zone| match zone.name.parse::<chrono_tz::Tz>() {
                Ok(tz) => Some((zone, chrono::Utc::now().with_timezone(&tz))),
                Err(_) => {
                    warn!(
                        connection,
                        timezone = zone.name,
                        "the calendar declares a zone this build does not know; the read says \
                         it has none rather than name one nobody can convert"
                    );
                    None
                }
            });
            info!(
                connection,
                from = %window.from,
                to = %window.to,
                intervals = busy.len(),
                timezone = local.as_ref().map(|(zone, _)| zone.name.as_str()).unwrap_or("(none declared)"),
                "a free/busy read was served"
            );
            // The gaps, spelled in the owner's own time when it is known
            // (#379): a drafting agent that copies them cannot write an hour
            // in the wrong zone, and one that computes them already has.
            let free = crate::freebusy::free_between(
                &busy,
                &window,
                local.as_ref().map(|(zone, _)| zone.name.as_str()),
            );
            let mut answer = json!({
                "connection": connection,
                "from": window.from.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                "to": window.to.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                "busy": busy,
                "free": free,
            });
            // Absent, not null, when there is no zone to name: a member that
            // is there and empty says "I looked and the answer is nothing",
            // which is a different sentence from "there is no such fact" —
            // #359's rule, at a third door. The three go together.
            if let Some((zone, at)) = local {
                answer["timezone"] = json!(zone.name);
                answer["timezone_source"] = json!(zone.source);
                answer["now"] = json!(at.format("%Y-%m-%dT%H:%M:%S%:z").to_string());
            }
            (StatusCode::OK, Json(answer)).into_response()
        }
        Err(SideError::Refused { status, .. }) => refuse(
            &endpoint,
            Read::FreeBusy,
            StatusCode::BAD_GATEWAY,
            "caldav_refused",
            &format!("the calendar service refused the free-busy report with HTTP {status}"),
            None,
        ),
        Err(SideError::Unreachable { detail }) => refuse(
            &endpoint,
            Read::FreeBusy,
            StatusCode::BAD_GATEWAY,
            "caldav_unreachable",
            &format!("the calendar service did not answer the free-busy report: {detail}"),
            None,
        ),
    }
}

#[derive(Debug, Deserialize)]
struct FactsQuery {
    connection: Option<String>,
    uid: Option<String>,
}

/// `GET /event-facts?connection=&uid=` — what one of the owner's events
/// carries, without its words (#355).
///
/// The same terms as `/freebusy` above, and for the same reasons: the
/// Gateway's service token and nothing else, the internal network, and the
/// record of who asked and for what is the Gateway's, because that is where
/// Hermes's signature was checked.
///
/// What it answers is counts, one flag's worth of knowledge and at most one
/// URL — never the description, never an attachment's name. An event this
/// collector never published is one it does not answer about: `null`, said
/// as `found: false`, so a persona learns "I hold nothing about that" and
/// not "that meeting carries nothing".
async fn event_facts(
    State(endpoint): State<Endpoint>,
    headers: HeaderMap,
    Query(query): Query<FactsQuery>,
) -> Response {
    if !authenticated(&endpoint, &headers) {
        return refuse(
            &endpoint,
            Read::EventFacts,
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "this endpoint answers the Companion Gateway's service token and nothing else",
            None,
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
            Read::EventFacts,
            StatusCode::NOT_FOUND,
            "connection_unknown",
            &format!("this collector holds no calendar connection named {connection:?}"),
            None,
        );
    };
    let uid = query.uid.unwrap_or_default();
    if uid.is_empty() || uid.chars().count() > 512 {
        return refuse(
            &endpoint,
            Read::EventFacts,
            StatusCode::BAD_REQUEST,
            "invalid_uid",
            "uid is the event's iCalendar UID, between 1 and 512 characters",
            None,
        );
    }
    let calendar = endpoint.calendar_access.read().await.clone();
    let token = endpoint.access.read().await.clone();
    let (owner_id, token) = match (calendar.state, calendar.owner_id, token) {
        (Some("connected"), Some(owner_id), Some(token)) => (owner_id, token),
        (state, _, _) => {
            let state = state
                .filter(|state| *state != "connected")
                .unwrap_or("unknown");
            return refuse(
                &endpoint,
                Read::EventFacts,
                StatusCode::CONFLICT,
                "connection_not_connected",
                &format!(
                    "the calendar connection {connection:?} is {state}; an event cannot be read \
                     until it is connected"
                ),
                Some(state),
            );
        }
    };
    match calendars.facts_about(&uid, &owner_id, &token).await {
        Ok(facts) => {
            endpoint.metrics.record_event_fact_read("served");
            info!(
                connection,
                found = facts.is_some(),
                "the facts about an event were served"
            );
            let body = match facts {
                Some(facts) => json!({
                    "connection": connection,
                    "uid": uid,
                    "found": true,
                    "conference": facts.conference,
                    "description_characters": facts.description_characters,
                    "attachments": facts.attachments,
                }),
                None => json!({
                    "connection": connection,
                    "uid": uid,
                    "found": false,
                    "conference": Value::Null,
                    "description_characters": Value::Null,
                    "attachments": 0,
                }),
            };
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(SideError::Refused { status, .. }) => refuse(
            &endpoint,
            Read::EventFacts,
            StatusCode::BAD_GATEWAY,
            "caldav_refused",
            &format!("the calendar service refused the read with HTTP {status}"),
            None,
        ),
        Err(SideError::Unreachable { detail }) => refuse(
            &endpoint,
            Read::EventFacts,
            StatusCode::BAD_GATEWAY,
            "caldav_unreachable",
            &format!("the calendar service did not answer the read: {detail}"),
            None,
        ),
    }
}

/// One refusal: counted under its code, said at `warn`, answered in the
/// Companion Gateway's `Error` shape — with the connection's `state` beside
/// it when that is what was refused.
fn refuse(
    endpoint: &Endpoint,
    read: Read,
    status: StatusCode,
    code: &'static str,
    detail: &str,
    state: Option<&str>,
) -> Response {
    match read {
        Read::FreeBusy => endpoint.metrics.record_freebusy_read(code),
        Read::EventFacts => endpoint.metrics.record_event_fact_read(code),
    }
    warn!(%code, status = status.as_u16(), state, detail, "a free/busy read was refused");
    let mut body = json!({ "error": code, "detail": detail });
    if let Some(state) = state {
        body["state"] = json!(state);
    }
    (status, Json(body)).into_response()
}

/// Serves the endpoint until the process ends.
pub async fn serve(listener: tokio::net::TcpListener, endpoint: Endpoint) {
    if let Err(error) = axum::serve(listener, router(endpoint)).await {
        warn!(%error, "the internal HTTP endpoint stopped");
    }
}
