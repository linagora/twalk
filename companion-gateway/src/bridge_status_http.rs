//! The bridge status webhook: the one route on this origin a bridge calls
//! (ticket #56).
//!
//! # Why it is not behind the device-token guard
//!
//! Every other route under `/api/` requires the browser's device token, and
//! that default is what keeps a later ticket from forgetting authentication
//! (#52). This route cannot live there: its caller is a mautrix bridge, which
//! has no browser, no device and no Matrix OpenID token. So it sits outside
//! `/api/`, at `/_twalk/bridges/{bridge_id}/status`, and it authenticates the
//! bridge instead — by **that bridge's own `as_token`**, from the Gateway's
//! configuration.
//!
//! What it deliberately does not do is trust the caller's position on the
//! network. "It came from the compose network, so it is the bridge" was
//! considered and refused, for the same reason the Sensor was given a service
//! token rather than an IP range: a deployment is one Docker network, every
//! container on it can reach this port, and a forged push would let anything
//! on that network tell the user their WhatsApp session had expired — or, far
//! worse, that a dead one was fine.
//!
//! The requirement is declared where the rest of the policy is, as
//! [`crate::session_http::Requirement::BridgeToken`], so the Gateway's
//! authentication table stays one function with no hole in it. The guard
//! recognises `/_twalk/` as a reserved prefix and closes everything under it
//! that is not this exact path.
//!
//! # What an answer means
//!
//! `204` means the push was verified and applied — whether or not it changed
//! anything. A repeated identical state is applied and publishes nothing,
//! which is the de-duplication [`crate::bridge_status`] is about; the bridge
//! has no use for the difference, and a distinct status would only invite a
//! bridge to retry on one of them.
//!
//! Everything else is the Gateway's usual `Error` document. A refusal is a
//! real refusal: mautrix retries a failed push with backoff, so a `401` is
//! seen again, which is exactly what an operator whose tokens disagree should
//! experience.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use tracing::{debug, error, warn};

use crate::bridge_status::StatusRefusal;
use crate::http::Gateway;

/// The webhook route. Merged into the Gateway's router like every other
/// ticket's — and, unlike every other ticket's, declared in #52's guard as
/// taking the calling bridge's `as_token`.
pub fn routes() -> Router<Gateway> {
    Router::new().route("/_twalk/bridges/{bridge_id}/status", post(status_webhook))
}

/// `POST /_twalk/bridges/{bridge_id}/status` — one bridge's connection
/// state, as mautrix pushes it.
///
/// The body is a mautrix `BridgeState`: `state_event` (required), and
/// optionally `message`, `error`, `timestamp`, `user_action` and `info`. The
/// credential is the bridge's `as_token` as `Authorization: Bearer …`, which
/// is what mautrix sends and what the Gateway has in its own configuration
/// for that bridge.
///
/// This is mautrix's only push channel, and therefore the only one that sees
/// a transition at the moment it happens. `GET /v3/whoami` reconciles at
/// startup; management-room notices are not parsed at all.
async fn status_webhook(
    State(gateway): State<Gateway>,
    Path(bridge_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Some(statuses) = gateway.statuses() else {
        // No store and no bus: there is nowhere to record a transition and
        // nowhere to publish it. Saying so is better than answering 204 to a
        // push the Gateway silently threw away.
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "bridge_status_not_configured",
            "this Gateway records no bridge status: set GATEWAY_NATS_URL (and GATEWAY_OWNER, \
             which the Gateway's store takes its state directory and domain from)",
        );
    };
    let parsed: Value = match serde_json::from_str(body.trim()) {
        Ok(parsed) => parsed,
        Err(error) => {
            gateway
                .metrics()
                .record_bridge_status_refusal("invalid_request");
            return api_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("the status push is not JSON: {error}"),
            );
        }
    };
    match statuses.push(&bridge_id, bearer(&headers), &parsed) {
        Ok(Some(transition)) => {
            debug!(
                bridge = %bridge_id,
                to_state = transition.to_state.as_str(),
                "recorded a bridge status push"
            );
            StatusCode::NO_CONTENT.into_response()
        }
        // Verified, applied, and nothing to say: the bridge re-pushed the
        // state it was already in.
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(refusal) => {
            gateway
                .metrics()
                .record_bridge_status_refusal(refusal.label());
            refused(&bridge_id, refusal)
        }
    }
}

fn refused(bridge_id: &str, refusal: StatusRefusal) -> Response {
    match &refusal {
        StatusRefusal::UnknownBridge { bridge_id } => api_error(
            StatusCode::NOT_FOUND,
            "unknown_bridge",
            &format!(
                "no bridge reports status under the id {bridge_id:?}: it is the \
                 GATEWAY_BRIDGE_<ID>_STATUS_ID of a configured bridge, and it is what that \
                 bridge's homeserver.status_endpoint must name"
            ),
        ),
        StatusRefusal::NoAsToken { bridge_id } => {
            // Loud: the operator wired the bridge's status_endpoint at this
            // Gateway and did not give it the token to check the push with.
            // Accepting it unverified was the alternative, and it is the one
            // thing this endpoint must never do.
            error!(
                bridge = %bridge_id,
                "refused a bridge status push: no as_token is configured for this bridge, so \
                 the push cannot be verified — and an unverified push is not accepted"
            );
            api_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "as_token_not_configured",
                "this Gateway has no as_token for that bridge, so it cannot verify the push: \
                 set GATEWAY_BRIDGE_<ID>_AS_TOKEN to the same value as that bridge's \
                 appservice.as_token",
            )
        }
        StatusRefusal::Unauthenticated => {
            warn!(
                bridge = %bridge_id,
                "refused a bridge status push: the as_token is missing or wrong"
            );
            api_error(
                StatusCode::UNAUTHORIZED,
                "unauthenticated",
                "the status webhook takes the calling bridge's own as_token as an \
                 Authorization: Bearer credential; a device token is not accepted here",
            )
        }
        StatusRefusal::Invalid { detail } => {
            api_error(StatusCode::BAD_REQUEST, "invalid_request", detail)
        }
        StatusRefusal::Store { detail } => {
            error!(%detail, bridge = %bridge_id, "failed to record a bridge status change");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_unavailable",
                "the bridge status could not be recorded",
            )
        }
    }
}

/// One error answer shape for the whole origin: `openapi.yaml`'s `Error`.
fn api_error(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({ "error": code, "detail": detail }))).into_response()
}

/// The credential out of an `Authorization: Bearer …` header, with the
/// scheme matched case-insensitively as RFC 9110 requires.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, credential) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| credential.trim())
        .filter(|credential| !credential.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn authorization(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(value).expect("an ascii header"),
        );
        headers
    }

    #[test]
    fn the_credential_is_a_bearer_token_and_a_cookie_is_not_one() {
        assert_eq!(bearer(&authorization("Bearer as-token")), Some("as-token"));
        assert_eq!(bearer(&authorization("bearer as-token")), Some("as-token"));
        assert_eq!(bearer(&authorization("Basic as-token")), None);
        assert_eq!(bearer(&authorization("Bearer ")), None);
        assert_eq!(bearer(&authorization("as-token")), None);
        assert_eq!(bearer(&HeaderMap::new()), None);
        let mut cookie = HeaderMap::new();
        cookie.insert(
            header::COOKIE,
            HeaderValue::from_static("twalk_device=a-device-token"),
        );
        assert_eq!(
            bearer(&cookie),
            None,
            "a device cookie is not a credential on this route"
        );
    }
}
