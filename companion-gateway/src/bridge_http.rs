//! The bridge facade's HTTP surface: the flows a bridge offers, starting a
//! login, polling the step it is on, submitting what a step asked for,
//! cancelling, and logging a login out (ticket #55).
//!
//! # Who is calling
//!
//! Nothing here authenticates: #52's guard already does, for everything
//! under `/api/` that its table does not except — and this ticket adds no
//! row to that table, so every route below needs a live device token by
//! default ([`crate::session_http`]). The device the guard put in the
//! request's extensions is what a refusal of a *second* login names, so the
//! user reading "already in flight" learns which of their devices started
//! it.
//!
//! The acting Matrix user is the deployment's **owner**, taken from
//! configuration and not from the request: mautrix's shared-secret auth
//! trusts the `user_id` query parameter completely, so the Gateway is the
//! one that decides whose login this is. One owner per deployment
//! (ADR 0011).
//!
//! # Why the browser polls instead of waiting
//!
//! `GET /api/bridges/{bridge_id}/login` answers immediately, always: the
//! blocking step is held inside the Gateway ([`crate::bridge`]), and this
//! endpoint reports what the last answer from the bridge was. A phone that
//! sleeps mid-scan therefore loses a poll, not the login.
//!
//! # What the answers look like
//!
//! One shape for every refusal, the Gateway's own `Error` document
//! (`openapi.yaml`): a stable `error` code a client branches on, and a
//! `detail` for an operator's logs. The codes are `unknown_bridge`,
//! `no_login_in_flight`, `login_in_flight`, `invalid_request`,
//! `too_many_logins`, `login_expired`, `step_cancelled`,
//! `not_found_on_bridge`, `bridge_refused` and `bridge_unreachable`;
//! `unauthenticated` and `sign_in_not_configured` come from the guard.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Router};
use serde_json::{json, Value};
use tracing::{debug, info};

use crate::bridge::{login_json, redacted, BridgeRefusal, StartLogin};
use crate::http::Gateway;
use crate::session::Device;

/// The bridge routes. Merged into the Gateway's router, so registering them
/// is additive to whatever else the API grows — and so they are behind #52's
/// guard without asking for it.
pub fn routes() -> Router<Gateway> {
    Router::new()
        .route("/api/bridges", get(list_bridges))
        .route("/api/bridges/{bridge_id}/login/flows", get(login_flows))
        .route(
            "/api/bridges/{bridge_id}/login",
            post(start_login).get(poll_login).delete(cancel_login),
        )
        .route("/api/bridges/{bridge_id}/login/submit", post(submit_step))
        .route("/api/bridges/{bridge_id}/logins", get(existing_logins))
        .route("/api/bridges/{bridge_id}/logins/{login_id}", delete(logout))
}

/// `GET /api/bridges` — the bridges this deployment has configured, and the
/// login each one has in flight.
///
/// Reads configuration and the Gateway's own memory: no bridge is contacted,
/// so the Companion can draw the networks screen while every bridge is down.
/// A deployment with no bridge configured answers an empty list — that is
/// the honest answer, not an error.
async fn list_bridges(State(gateway): State<Gateway>) -> Response {
    let bridges = gateway.bridges();
    let listed: Vec<Value> = bridges
        .list()
        .iter()
        .map(|(config, login)| {
            json!({
                "bridge_id": config.bridge_id,
                "network": config.network,
                "login": login.as_ref().map(login_json),
            })
        })
        .collect();
    Json(json!({ "bridges": listed })).into_response()
}

/// `GET /api/bridges/{bridge_id}/login/flows` — the login flows this bridge
/// offers, as the bridge itself describes them (`qr`, a phone number, a
/// cookie paste).
async fn login_flows(
    State(gateway): State<Gateway>,
    Path(bridge_id): Path<String>,
) -> Response {
    let owner = gateway.owner();
    match gateway.bridges().flows(&bridge_id, &owner).await {
        Ok(flows) => Json(json!({
            "flows": flows
                .iter()
                .map(|flow| json!({
                    "id": flow.id,
                    "name": flow.name,
                    "description": flow.description,
                }))
                .collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(refusal) => refused(refusal),
    }
}

/// `POST /api/bridges/{bridge_id}/login` — start a login, or restart one
/// against an existing login id.
///
/// The body is `{"flow_id": "qr"}`, plus `"login_id"` to **reconnect**: the
/// flow then repairs the login the bridge already holds instead of creating
/// a second one, which is how a broken session is fixed (spec #47). It never
/// restarts a container.
///
/// `201` with the login's state, already carrying the flow's first step — a
/// QR login answers with the first code here, and the Companion polls from
/// then on. A login already in flight is `409`, naming the device and the
/// instant that started it: one login per bridge instance in v0.1.
async fn start_login(
    State(gateway): State<Gateway>,
    Extension(device): Extension<Device>,
    Path(bridge_id): Path<String>,
    body: String,
) -> Response {
    let request = match parse_start(&body) {
        Ok(request) => request,
        Err(detail) => return api_error(StatusCode::BAD_REQUEST, "invalid_request", &detail),
    };
    let owner = gateway.owner();
    match gateway
        .bridges()
        .start(&bridge_id, &owner, &device, &request)
        .await
    {
        Ok(view) => (StatusCode::CREATED, Json(login_json(&view))).into_response(),
        Err(refusal) => {
            debug!(
                bridge = %bridge_id,
                device = %device.id,
                outcome = refusal.label(),
                "refused to start a bridge login"
            );
            refused(refusal)
        }
    }
}

/// `GET /api/bridges/{bridge_id}/login` — the pollable state of the login in
/// flight.
///
/// The document the Companion polls: `state`, the current `step` with its
/// payload and how long it is valid, and a `generation` that changes every
/// time the bridge hands back a new step. A QR refresh is a bumped
/// generation with a new payload — that is the signal to redraw before the
/// old code expires.
///
/// Always immediate. `404 no_login_in_flight` once nothing has been started
/// on this bridge — a login that finished, failed or was cancelled is still
/// reported, so the Companion can show how it ended.
async fn poll_login(State(gateway): State<Gateway>, Path(bridge_id): Path<String>) -> Response {
    match gateway.bridges().login(&bridge_id) {
        Ok(view) => Json(login_json(&view)).into_response(),
        Err(refusal) => refused(refusal),
    }
}

/// `POST /api/bridges/{bridge_id}/login/submit` — answer the step the login
/// is waiting on.
///
/// The body is `{"step_id": "…", "data": {…}}`, where `data` is what the
/// step's own payload asked for: a phone number for a `user_input` step, the
/// Google cookies for the SMS preview path's `cookies` step. Those are
/// network credentials — relayed to the bridge and forgotten (ADR 0011).
/// Nothing here stores them, and the log line says only the shape of what
/// went through.
async fn submit_step(
    State(gateway): State<Gateway>,
    Extension(device): Extension<Device>,
    Path(bridge_id): Path<String>,
    body: String,
) -> Response {
    let (step_id, data) = match parse_submit(&body) {
        Ok(parsed) => parsed,
        Err(detail) => return api_error(StatusCode::BAD_REQUEST, "invalid_request", &detail),
    };
    info!(
        bridge = %bridge_id,
        device = %device.id,
        step_id = %step_id,
        // The shape of the credential, never the credential.
        submitted = %redacted(&data),
        "relaying a bridge login step to the bridge"
    );
    match gateway.bridges().submit(&bridge_id, &step_id, data).await {
        Ok(view) => Json(login_json(&view)).into_response(),
        Err(refusal) => refused(refusal),
    }
}

/// `DELETE /api/bridges/{bridge_id}/login` — cancel the login in flight.
///
/// Cancels the current step (which releases the request the Gateway is
/// holding) and then the process, so the bridge forgets it. `204`, and the
/// login's state then reads `cancelled` until something else is started.
async fn cancel_login(State(gateway): State<Gateway>, Path(bridge_id): Path<String>) -> Response {
    match gateway.bridges().cancel(&bridge_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(refusal) => refused(refusal),
    }
}

/// `GET /api/bridges/{bridge_id}/logins` — the logins this bridge already
/// holds.
///
/// What the Companion reads to offer "reconnect": the login id to pass back
/// to the start endpoint, and the name the network gives it.
async fn existing_logins(
    State(gateway): State<Gateway>,
    Path(bridge_id): Path<String>,
) -> Response {
    let owner = gateway.owner();
    match gateway.bridges().logins(&bridge_id, &owner).await {
        Ok(logins) => Json(json!({
            "logins": logins
                .iter()
                .map(|login| json!({
                    "login_id": login.login_id,
                    "name": login.name,
                    "profile": login.profile,
                }))
                .collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(refusal) => refused(refusal),
    }
}

/// `DELETE /api/bridges/{bridge_id}/logins/{login_id}` — log a login out.
///
/// The bridge drops the session and the network credentials it kept for it;
/// the Gateway never held either. `204` when it is gone.
async fn logout(
    State(gateway): State<Gateway>,
    Path((bridge_id, login_id)): Path<(String, String)>,
) -> Response {
    let owner = gateway.owner();
    match gateway.bridges().logout(&bridge_id, &login_id, &owner).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(refusal) => refused(refusal),
    }
}

/// `{"flow_id": "qr", "login_id": "optional"}`, and nothing else that
/// matters: a closed shape, so a caller cannot smuggle a `user_id` past the
/// owner check.
fn parse_start(body: &str) -> Result<StartLogin, String> {
    let body: Value = serde_json::from_str(body.trim())
        .map_err(|error| format!("the request body is not JSON: {error}"))?;
    let flow_id = body
        .get("flow_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "flow_id is required: one of the ids GET /api/bridges/{bridge_id}/login/flows lists"
                .to_owned()
        })?
        .to_owned();
    let login_id = body
        .get("login_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Ok(StartLogin { flow_id, login_id })
}

/// `{"step_id": "…", "data": {…}}`. `data` is the step's own payload and is
/// passed through untouched — the Gateway does not know what a network's
/// input fields are called, and must not pretend to.
fn parse_submit(body: &str) -> Result<(String, Value), String> {
    let body: Value = serde_json::from_str(body.trim())
        .map_err(|error| format!("the request body is not JSON: {error}"))?;
    let step_id = body
        .get("step_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "step_id is required: the step_id the polled login state reports".to_owned()
        })?
        .to_owned();
    let data = body.get("data").cloned().unwrap_or(Value::Null);
    if !data.is_object() {
        return Err("data is required, and is the object the step's payload asked for".to_owned());
    }
    Ok((step_id, data))
}

/// One refusal, translated into the status and the code the description
/// declares.
fn refused(refusal: BridgeRefusal) -> Response {
    match &refusal {
        BridgeRefusal::UnknownBridge { bridge_id } => api_error(
            StatusCode::NOT_FOUND,
            "unknown_bridge",
            &format!(
                "no bridge {bridge_id:?} is configured: GET /api/bridges lists the ones \
                 this deployment has"
            ),
        ),
        BridgeRefusal::NoLoginInFlight => api_error(
            StatusCode::NOT_FOUND,
            "no_login_in_flight",
            "no login has been started on this bridge",
        ),
        // The one refusal the user can act on: their other device is
        // mid-scan. Named, with the instant, so they can tell that from a
        // Gateway that is stuck.
        BridgeRefusal::LoginInFlight {
            started_by,
            started_at,
        } => api_error(
            StatusCode::CONFLICT,
            "login_in_flight",
            &format!(
                "a login started at {} from the device {:?} is still in flight on this \
                 bridge; cancel it (DELETE /api/bridges/{{bridge_id}}/login) before starting \
                 another. One login per bridge instance in v0.1",
                crate::consent::rfc3339_millis(*started_at),
                started_by.device_name,
            ),
        ),
        BridgeRefusal::InvalidRequest { detail } => {
            api_error(StatusCode::BAD_REQUEST, "invalid_request", detail)
        }
        BridgeRefusal::TooManyLogins => api_error(
            StatusCode::FORBIDDEN,
            "too_many_logins",
            "the network will not accept another login on this account: remove a linked \
             device on the network itself, or log one of this bridge's logins out",
        ),
        BridgeRefusal::LoginExpired { errcode } => api_error(
            StatusCode::GONE,
            "login_expired",
            &format!(
                "this login is over ({errcode}): a login in flight lives in the bridge's \
                 memory, for at most 30 minutes and for as long as the network's code is \
                 valid. Start a new one"
            ),
        ),
        BridgeRefusal::StepCancelled => api_error(
            StatusCode::CONFLICT,
            "step_cancelled",
            "the step was cancelled before this answer arrived: poll the login and act on \
             the step it reports",
        ),
        BridgeRefusal::NotFoundOnBridge { errcode } => api_error(
            StatusCode::NOT_FOUND,
            "not_found_on_bridge",
            &format!(
                "the bridge does not know what this request named ({errcode}): a login it \
                 has forgotten, or a login id that is not one of its own"
            ),
        ),
        BridgeRefusal::BridgeRefused { errcode, status } => api_error(
            StatusCode::BAD_GATEWAY,
            "bridge_refused",
            &format!("the bridge refused the provisioning call with {errcode} ({status})"),
        ),
        BridgeRefusal::BridgeUnreachable { detail } => api_error(
            StatusCode::BAD_GATEWAY,
            "bridge_unreachable",
            &format!(
                "the bridge's provisioning API could not be reached: {detail}. Is the \
                 bridge running, and is its provisioning secret the one this Gateway is \
                 configured with?"
            ),
        ),
    }
}

/// One error answer shape for the whole surface: the `Error` schema of
/// `openapi.yaml`.
fn api_error(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({ "error": code, "detail": detail }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_start_request_names_a_flow_and_optionally_a_login_to_repair() {
        let start = parse_start(r#"{"flow_id":"qr"}"#).expect("a start request");
        assert_eq!(start.flow_id, "qr");
        assert_eq!(start.login_id, None);

        let reconnect =
            parse_start(r#"{"flow_id":"qr","login_id":"33612345678"}"#).expect("a reconnect");
        assert_eq!(reconnect.login_id.as_deref(), Some("33612345678"));

        assert!(parse_start("{}").is_err(), "flow_id is required");
        assert!(parse_start(r#"{"flow_id":""}"#).is_err());
        assert!(parse_start("not json").is_err());
    }

    #[test]
    fn a_submitted_step_carries_the_steps_own_payload_untouched() {
        let (step_id, data) =
            parse_submit(r#"{"step_id":"phone","data":{"phone_number":"+33612345678"}}"#)
                .expect("a submission");
        assert_eq!(step_id, "phone");
        assert_eq!(data["phone_number"], json!("+33612345678"));

        assert!(
            parse_submit(r#"{"data":{}}"#).is_err(),
            "the step being answered must be named"
        );
        assert!(
            parse_submit(r#"{"step_id":"phone"}"#).is_err(),
            "data is required: a step is answered with an object"
        );
        assert!(
            parse_submit(r#"{"step_id":"phone","data":"+33612345678"}"#).is_err(),
            "data is the step's object, not a bare value"
        );
    }
}
