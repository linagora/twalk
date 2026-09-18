//! Bootstrap's HTTP surface: the registration relay for the one account, and
//! the Sensor's invitation into the rooms the user selects (ticket #53).
//!
//! Two routes, and they sit on opposite sides of the guard
//! ([`crate::session_http`]) for a reason that is the whole shape of the
//! bootstrap problem:
//!
//! - `POST /api/bootstrap/account` is **open**, because it runs before any
//!   account exists and therefore before anyone can sign in. What stands in
//!   for authentication is the one-account rule: the username must be the
//!   owner's, and once that account exists nothing creates another. The
//!   endpoint answers 503 unless the operator set
//!   `GATEWAY_REGISTRATION_SHARED_SECRET`, so a deployment whose account was
//!   provisioned by hand never opens it at all.
//! - `POST /api/bootstrap/rooms` takes a **device token** like every other
//!   endpoint — the guard's default, so this module adds no row for it. By
//!   screen 3d the user has signed in, and the request carries their Matrix
//!   access token as a parameter of that one operation.
//!
//! The documents are small on purpose. The registration answer is the Matrix
//! session the browser needs to bootstrap its cryptographic identity
//! (ADR 0014) and nothing more: no recovery key in, no recovery key out.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum::Router;
use serde::Deserialize;
use tracing::{info, warn};

use crate::bootstrap::{
    parse_body, Bootstrap, InvitationRefusal, RecoveryOrInvalid, RegistrationRefusal, RoomOutcome,
    RoomStatus, MAX_ROOMS_PER_REQUEST,
};
use crate::http::Gateway;
use crate::session::Sessions;

/// The bootstrap routes. Merged into the Gateway's router like the session's.
pub fn routes() -> Router<Gateway> {
    Router::new()
        .route("/api/bootstrap/account", post(create_account))
        .route("/api/bootstrap/rooms", post(invite_sensor))
}

/// `POST /api/bootstrap/account` — create the owner's account, once.
///
/// Body: `{"username": "...", "password": "..."}`. The username must be the
/// localpart of `GATEWAY_OWNER`; anything else is `not_the_owner`. A recovery
/// key in the body is `recovery_key_refused`: the Gateway must never see one.
///
/// `201` answers `{user_id, device_id, access_token, home_server}` — the
/// Matrix session the Companion continues in the browser. The Gateway keeps
/// none of it.
async fn create_account(State(gateway): State<Gateway>, body: Bytes) -> Response {
    let Some(sessions) = gateway.sessions() else {
        return super::session_http::not_configured();
    };
    let Some(bootstrap) = gateway.bootstrap() else {
        return registration_not_configured();
    };
    match register(&sessions, &bootstrap, &body).await {
        Ok(account) => {
            gateway.metrics().record_registration("created");
            // The user id, never the token and never the password.
            info!(
                user_id = %account.user_id,
                device_id = %account.device_id,
                "created this deployment's one account"
            );
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "user_id": account.user_id,
                    "device_id": account.device_id,
                    // Returned so the browser can bootstrap cross-signing and
                    // the recovery key itself (ADR 0014). Not stored, not
                    // logged: the Gateway holds no Matrix access token
                    // (ADR 0011).
                    "access_token": account.access_token,
                    "home_server": account.home_server,
                })),
            )
                .into_response()
        }
        Err(refusal) => {
            gateway.metrics().record_registration(refusal.label());
            let (status, error, detail) = match &refusal {
                RegistrationRefusal::NotConfigured => return registration_not_configured(),
                RegistrationRefusal::NotTheOwner { username } => {
                    warn!(
                        username = %username,
                        owner = %sessions.owner(),
                        "refused a registration: this deployment serves one owner, and that is not the username asked for"
                    );
                    (
                        StatusCode::FORBIDDEN,
                        "not_the_owner",
                        Some(
                            "this deployment serves one owner, named in GATEWAY_OWNER: no other \
                             account can be created"
                                .to_owned(),
                        ),
                    )
                }
                RegistrationRefusal::AlreadyExists => {
                    warn!(
                        owner = %sessions.owner(),
                        "refused a registration: this deployment's one account already exists"
                    );
                    (
                        StatusCode::CONFLICT,
                        "account_already_exists",
                        Some(
                            "this deployment's account has already been created; sign in instead"
                                .to_owned(),
                        ),
                    )
                }
                RegistrationRefusal::RecoveryKeyRefused { field } => {
                    // Loud on purpose: a client that sent one has a bug that
                    // breaks a promise shown to the user on screen 2.
                    warn!(
                        field = %field,
                        "refused a registration: it carried a recovery key, which the Gateway must never see"
                    );
                    (
                        StatusCode::BAD_REQUEST,
                        "recovery_key_refused",
                        Some(
                            "the recovery key is generated in the browser and never sent to the \
                             Gateway (ADR 0014)"
                                .to_owned(),
                        ),
                    )
                }
                RegistrationRefusal::InvalidRequest { detail } => (
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    Some((*detail).to_owned()),
                ),
                RegistrationRefusal::HomeserverRefused { errcode } => {
                    // The homeserver's error code, which the Companion can
                    // turn into a message (a password policy, a username it
                    // will not take).
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(serde_json::json!({
                            "error": "homeserver_refused",
                            "matrix_errcode": errcode,
                        })),
                    )
                        .into_response();
                }
                RegistrationRefusal::Unreachable { detail } => {
                    warn!(%detail, "could not reach the homeserver to create the account");
                    (
                        StatusCode::BAD_GATEWAY,
                        "homeserver_unreachable",
                        None::<String>,
                    )
                }
            };
            refused(status, error, detail)
        }
    }
}

/// The registration decision, in order: the body, the owner check, the
/// one-account check, and only then the homeserver call. Every refusal that
/// can be decided locally is decided before the registration secret is used.
async fn register(
    sessions: &Sessions,
    bootstrap: &Bootstrap,
    body: &[u8],
) -> Result<crate::bootstrap::Account, RegistrationRefusal> {
    let body = parse_body(body).map_err(|error| match error {
        RecoveryOrInvalid::RecoveryKey { field } => {
            RegistrationRefusal::RecoveryKeyRefused { field }
        }
        RecoveryOrInvalid::Invalid => RegistrationRefusal::InvalidRequest {
            detail: "expected a JSON object with a username and a password",
        },
    })?;
    // `deny_unknown_fields`: the relay takes a username and a password, and
    // there is deliberately no field a recovery key — or anything else —
    // could ride in on.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        username: String,
        password: String,
    }
    let request: Request =
        serde_json::from_value(body).map_err(|_| RegistrationRefusal::InvalidRequest {
            detail: "expected a JSON object with a username and a password, and nothing else",
        })?;
    if request.password.is_empty() {
        return Err(RegistrationRefusal::InvalidRequest {
            detail: "the password must not be empty",
        });
    }
    // One deployment, one identity. Checked before the secret is touched: the
    // relay is not a registration service with a filter in front of it.
    if request.username != sessions.owner_localpart() {
        return Err(RegistrationRefusal::NotTheOwner {
            username: request.username,
        });
    }
    // The Gateway's own promise, from its own store: the account was created
    // here already.
    match sessions.owner_account_created() {
        Ok(Some(_)) => return Err(RegistrationRefusal::AlreadyExists),
        Ok(None) => {}
        Err(error) => {
            // A store that cannot answer must not be read as "no account
            // yet": that would re-open the one window this rule closes.
            warn!(%error, "the session store could not say whether the account exists");
            return Err(RegistrationRefusal::Unreachable {
                detail: "the Gateway's store is unreadable".to_owned(),
            });
        }
    }
    let account = match bootstrap
        .register(&request.username, &request.password)
        .await
    {
        Ok(account) => account,
        // The homeserver already has the account, and the store did not know:
        // the same refusal, so a wiped store cannot re-open the window. The
        // marker is written back below on a success only, which is why this
        // records nothing.
        Err(RegistrationRefusal::HomeserverRefused { errcode }) if errcode == "M_USER_IN_USE" => {
            return Err(RegistrationRefusal::AlreadyExists)
        }
        Err(refusal) => return Err(refusal),
    };
    if let Err(error) = sessions.record_owner_account_created(&account.user_id) {
        // The account is real; refusing the response would be a lie, and the
        // homeserver's own M_USER_IN_USE still refuses a second attempt.
        warn!(
            %error,
            "created the account but could not record it: a second attempt will be refused by the homeserver instead"
        );
    }
    Ok(account)
}

/// `POST /api/bootstrap/rooms` — invite the Sensor into the rooms the user
/// selected on screen 3d.
///
/// Body: `{"matrix_access_token": "...", "rooms": ["!a:server", ...]}`. The
/// token is used for these calls and forgotten: it is never stored and never
/// logged. Answers `200` with one outcome per room — `invited`,
/// `already_present`, or `failed` with a reason — because the user ticked
/// several rooms and wants to know which took. A token the homeserver rejects
/// fails the whole request, since then nothing was attempted anywhere.
async fn invite_sensor(State(gateway): State<Gateway>, body: Bytes) -> Response {
    if gateway.sessions().is_none() {
        return super::session_http::not_configured();
    }
    let Some(bootstrap) = gateway.bootstrap() else {
        return invitation_not_configured();
    };
    let request = match parse_body(&body) {
        Ok(body) => body,
        Err(RecoveryOrInvalid::RecoveryKey { field }) => {
            warn!(
                field = %field,
                "refused an invitation request: it carried a recovery key, which the Gateway must never see"
            );
            return refused(
                StatusCode::BAD_REQUEST,
                "recovery_key_refused",
                Some(
                    "the recovery key is generated in the browser and never sent to the Gateway \
                     (ADR 0014)"
                        .to_owned(),
                ),
            );
        }
        Err(RecoveryOrInvalid::Invalid) => {
            return refused(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                Some(
                    "expected a JSON object with a Matrix access token and a room list".to_owned(),
                ),
            )
        }
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        matrix_access_token: String,
        rooms: Vec<String>,
    }
    let Ok(request) = serde_json::from_value::<Request>(request) else {
        return refused(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some(
                "expected a JSON object with a Matrix access token and a room list, and nothing \
                 else"
                    .to_owned(),
            ),
        );
    };
    if request.matrix_access_token.is_empty() {
        return refused(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            Some("the Matrix access token must not be empty".to_owned()),
        );
    }
    if request.rooms.len() > MAX_ROOMS_PER_REQUEST {
        return refused(
            StatusCode::BAD_REQUEST,
            "too_many_rooms",
            Some(format!(
                "at most {MAX_ROOMS_PER_REQUEST} rooms can be selected in one request"
            )),
        );
    }
    match bootstrap
        .invite_sensor(&request.matrix_access_token, &request.rooms)
        .await
    {
        Ok(outcomes) => {
            for outcome in &outcomes {
                gateway
                    .metrics()
                    .record_sensor_invitation(outcome.status.label());
            }
            Json(serde_json::json!({
                "sensor": bootstrap.sensor_user_id(),
                "rooms": outcomes.iter().map(room_document).collect::<Vec<_>>(),
            }))
            .into_response()
        }
        Err(refusal) => {
            gateway.metrics().record_sensor_invitation(refusal.label());
            match &refusal {
                InvitationRefusal::NotConfigured => invitation_not_configured(),
                InvitationRefusal::TokenRejected => {
                    warn!("the homeserver rejected the Matrix access token sent with an invitation request");
                    // `400`, not `401`. A `401` from this origin means "your
                    // credentials *to me* are not good", and a client is
                    // entitled to read it as an expired session and repair it
                    // by refreshing — which the Companion's central handler
                    // does, so a rejected third-party token sent it refreshing
                    // a session that was never the problem, twice per click
                    // (#141). What was refused is a credential the caller put
                    // in the request body, for a different server.
                    refused(
                        StatusCode::BAD_REQUEST,
                        "matrix_token_rejected",
                        Some(
                            "the homeserver does not accept this Matrix access token: nothing was \
                             invited"
                                .to_owned(),
                        ),
                    )
                }
                InvitationRefusal::InvalidRequest { detail } => refused(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    Some((*detail).to_owned()),
                ),
                InvitationRefusal::Unreachable { detail } => {
                    warn!(%detail, "could not reach the homeserver to invite the Sensor");
                    refused(StatusCode::BAD_GATEWAY, "homeserver_unreachable", None)
                }
            }
        }
    }
}

/// One room's outcome as the API renders it.
fn room_document(outcome: &RoomOutcome) -> serde_json::Value {
    let mut document = serde_json::json!({
        "room_id": outcome.room_id,
        "status": outcome.status.label(),
    });
    if let RoomStatus::Failed { reason } = &outcome.status {
        document["reason"] = serde_json::json!(reason);
    }
    document
}

/// A refusal the Companion's fetch can parse: a machine-readable `error`, and
/// a `detail` for the operator reading a network tab.
fn refused(status: StatusCode, error: &str, detail: Option<String>) -> Response {
    let mut document = serde_json::json!({ "error": error });
    if let Some(detail) = detail {
        document["detail"] = serde_json::json!(detail);
    }
    (status, Json(document)).into_response()
}

/// The registration relay is off: the answer names the variable that opens it.
fn registration_not_configured() -> Response {
    refused(
        StatusCode::SERVICE_UNAVAILABLE,
        "registration_not_configured",
        Some(
            "this deployment does not relay account registration: set \
             GATEWAY_REGISTRATION_SHARED_SECRET to the homeserver's registration shared secret to \
             enable it"
                .to_owned(),
        ),
    )
}

/// Inviting the Sensor is off: same shape, other variable.
fn invitation_not_configured() -> Response {
    refused(
        StatusCode::SERVICE_UNAVAILABLE,
        "sensor_not_configured",
        Some(
            "this deployment does not know which Sensor to invite: set GATEWAY_SENSOR_USER_ID to \
             the Sensor's Matrix ID to enable it"
                .to_owned(),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rooms_document_carries_a_reason_only_when_it_failed() {
        let invited = room_document(&RoomOutcome {
            room_id: "!a:example.com".to_owned(),
            status: RoomStatus::Invited,
        });
        assert_eq!(invited["status"].as_str(), Some("invited"));
        assert!(invited.get("reason").is_none());

        let failed = room_document(&RoomOutcome {
            room_id: "!b:example.com".to_owned(),
            status: RoomStatus::Failed {
                reason: "M_FORBIDDEN".to_owned(),
            },
        });
        assert_eq!(failed["status"].as_str(), Some("failed"));
        assert_eq!(failed["reason"].as_str(), Some("M_FORBIDDEN"));

        let present = room_document(&RoomOutcome {
            room_id: "!c:example.com".to_owned(),
            status: RoomStatus::AlreadyPresent,
        });
        assert_eq!(present["status"].as_str(), Some("already_present"));
    }

    #[test]
    fn each_half_that_is_off_answers_503() {
        assert_eq!(
            registration_not_configured().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            invitation_not_configured().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
