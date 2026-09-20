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
//! `not_found_on_bridge`, `bridge_refused`, `bridge_unreachable` and
//! `bridge_answer_unusable`; `unauthenticated` and `sign_in_not_configured`
//! come from the guard.
//!
//! # The last two are not the same fact, and must never be rendered as one
//!
//! `bridge_unreachable` means **nothing answered**: connection refused,
//! timeout, no route. `bridge_answer_unusable` means **the bridge answered
//! and this build could not use the answer** — it replied in milliseconds,
//! correctly as far as it is concerned, and the Gateway went looking for a
//! field it does not send.
//!
//! They shared a code once. The first live WhatsApp login (#106) failed on
//! the second and was reported as the first, and a whole debugging session
//! went to networking, containers and ports — the one place the fault was
//! not. So a client branches on the code, and the message for the second one
//! has to say that the bridge is running: that is the sentence that gets the
//! right bug report instead of the wrong investigation.
//!
//! # What a refusal never carries
//!
//! The bridge's answer. Not a quoted field, not a truncated one, not a serde
//! error that echoes the bytes it choked on: a provisioning answer can hold a
//! phone number as a login id, an account's display name, or a QR payload.
//! `bridge_answer_unusable` names the endpoint and what the Gateway was
//! looking for — and [`crate::bridge::BridgeRefusal::BridgeAnswerUnusable`]
//! has nowhere to put anything else, by construction.

use std::time::SystemTime;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Router};
use serde_json::{json, Value};
use tracing::{debug, info};

use crate::bridge::{login_json, redacted, BridgeRefusal, StartLogin};
use crate::bridge_status::{linked_logins, whoami_state};
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

/// `GET /api/bridges` — the bridges this deployment has configured, what each
/// one is **connected** to, and the login each one has in flight.
///
/// # The two things this answer keeps apart
///
/// `connection` is the bridge's own answer, read live from its `whoami`: the
/// logins it holds, each with the account's name and the state of the link.
/// `login` is the login *process* this Gateway has in flight — a QR scan in
/// somebody's browser right now.
///
/// They are separate members because conflating them was the bug (#108). The
/// Companion computed its connected badge from `login.state == "complete"`,
/// so starting a login, cancelling one or restarting the Gateway each made a
/// live WhatsApp link read as no link at all. A `connection` survives all
/// three, because it is not this process's to lose.
///
/// # A bridge that cannot be asked
///
/// `connection.reachable` is `false` and `connection.state` is `null`: the
/// Gateway does not know, and says so. It deliberately does not fall back to
/// `disconnected` — telling a user with a working link that it is broken is
/// exactly what this endpoint is being fixed for. The list itself never
/// fails: the other bridges answer, and the networks screen draws.
///
/// A deployment with no bridge configured answers an empty list — that is
/// the honest answer, not an error.
async fn list_bridges(State(gateway): State<Gateway>) -> Response {
    let owner = gateway.owner();
    let now = SystemTime::now();
    let listed: Vec<Value> = gateway
        .bridges()
        .connections(&owner)
        .await
        .iter()
        .map(|bridge| {
            json!({
                "bridge_id": bridge.config.bridge_id,
                "network": bridge.config.network,
                "connection": connection_json(&bridge.logins, now),
                "login": bridge.login.as_ref().map(login_json),
            })
        })
        .collect();
    Json(json!({ "bridges": listed })).into_response()
}

/// One bridge's `whoami` answer, as the networks screen reads it.
///
/// The state vocabulary is #56's and only #56's — `connected`, `starting`,
/// `degraded`, `disconnected`, `session_expired` — because a second
/// vocabulary for the same fact is a second thing to keep in step. In
/// particular a session revoked from the user's own phone arrives as
/// `BAD_CREDENTIALS` and comes out here as `session_expired`; no mautrix
/// bridge emits `LOGGED_OUT`, so nothing waits for it.
fn connection_json(logins: &Result<Vec<Value>, BridgeRefusal>, now: SystemTime) -> Value {
    let logins = match logins {
        Ok(logins) => logins,
        Err(refusal) => {
            return json!({
                "reachable": false,
                "state": Value::Null,
                "reported": Value::Null,
                "reason": Value::Null,
                "logins": [],
                "unreachable_because": refusal.label(),
            })
        }
    };
    let observed = whoami_state(logins, now);
    json!({
        "reachable": true,
        "state": observed.state.as_str(),
        "reported": observed.reported,
        "reason": observed.reason,
        "logins": linked_logins(logins, now)
            .iter()
            .map(|login| json!({
                "login_id": login.login_id,
                "name": login.name,
                "profile": login.profile,
                "state": login.state.as_str(),
                "reported": login.reported,
                "reason": login.reason,
                "since": login.since,
            }))
            .collect::<Vec<_>>(),
        "unreachable_because": Value::Null,
    })
}

/// `GET /api/bridges/{bridge_id}/login/flows` — the login flows this bridge
/// offers, as the bridge itself describes them (`qr`, a phone number, a
/// cookie paste).
async fn login_flows(State(gateway): State<Gateway>, Path(bridge_id): Path<String>) -> Response {
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
    match gateway
        .bridges()
        .logout(&bridge_id, &login_id, &owner)
        .await
    {
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
        // `bridge_refused` is what is left once the refusals a caller can
        // act on have been named, and "the bridge refused with M_BAD_STATE
        // (500)" is not something anybody can read. So the three refusals a
        // real mautrix bridge was actually observed to give here each say
        // what to check; see `tests/harness/fixtures` for the captured
        // answers.
        BridgeRefusal::BridgeRefused { errcode, status } => api_error(
            StatusCode::BAD_GATEWAY,
            "bridge_refused",
            &match (*status, errcode.as_str()) {
                (401, _) => format!(
                    "the bridge rejected this Gateway's provisioning secret ({errcode}): the \
                     secret configured here for this bridge is not the one in the bridge's \
                     own `provisioning.shared_secret`"
                ),
                (403, _) => format!(
                    "the bridge will not act for this user ({errcode}): mautrix requires the \
                     acting user in `?user_id=` on every provisioning call, and grants login \
                     permissions only to the Matrix IDs in its own `permissions:` block. \
                     Check that the owner's Matrix ID is one of them"
                ),
                (500, "M_BAD_STATE") => format!(
                    "the bridge and this Gateway disagree about where this login has got to \
                     ({errcode}): the transaction id, the step id or the step type sent did \
                     not match the step the bridge is on. Every step answer carries a fresh \
                     `txn_id` and the bridge validates it, so poll the login and act on the \
                     step it reports"
                ),
                _ => format!("the bridge refused the provisioning call with {errcode} ({status})"),
            },
        ),
        // Nothing answered. This is the one refusal that is worth checking
        // containers, ports and networking for.
        BridgeRefusal::BridgeUnreachable { detail } => api_error(
            StatusCode::BAD_GATEWAY,
            "bridge_unreachable",
            &format!(
                "nothing answered at the bridge's provisioning API: {detail}. Is the \
                 bridge running, and is this Gateway configured with its address?"
            ),
        ),
        // The bridge answered. Everything about this message is chosen so
        // that the reader does not go and look at the network (#116): it
        // says the bridge replied, it names the call and the field, and it
        // says whose defect this is. It names nothing out of the answer —
        // the variant cannot carry any of it.
        // The mirror of the arm below: the bridge is reachable and replied,
        // and what it could not read is what *this* build sent. Nothing here
        // blames the network, nothing here says to start again — bridgev2
        // refuses the body before the connector runs, so the login is still
        // waiting on the same step (#221).
        BridgeRefusal::RequestUnreadable { errcode } => api_error(
            StatusCode::BAD_GATEWAY,
            "bridge_request_unusable",
            &format!(
                "the bridge could not read the request this Gateway sent it ({errcode}). The \
                 network saw nothing and refused nothing: this is a defect in Twalk's writing \
                 of that bridge's provisioning API, not in your account, the network or the \
                 deployment — checking containers, ports or the provisioning secret will find \
                 nothing, and submitting the same values again will fail the same way. The \
                 login is unchanged and still waiting on this step. Please report it with this \
                 message and the bridge's version"
            ),
        ),
        BridgeRefusal::BridgeAnswerUnusable { call, looked_for } => api_error(
            StatusCode::BAD_GATEWAY,
            "bridge_answer_unusable",
            &format!(
                "the bridge answered {} and this Gateway could not use its answer: it \
                 looked for {looked_for} and did not find it. The bridge is reachable \
                 and replied, so this is a defect in Twalk's reading of that bridge's \
                 provisioning API, not a broken deployment — checking containers, ports \
                 or the provisioning secret will find nothing. Please report it with \
                 this message and the bridge's version. The answer itself is not \
                 repeated here, because a bridge's answer can carry identifiers from a \
                 network account; its shape is in this Gateway's log",
                call.endpoint()
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
