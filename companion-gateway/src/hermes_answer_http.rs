//! `POST /_twalk/hermes/answers` — the one route Hermes calls (ticket #206).
//!
//! The third caller this origin has that is not a browser, after the Sensor's
//! consent snapshot and a bridge's status push, and it follows the shape those
//! two established: outside `/api/`, under the reserved `/_twalk/` prefix,
//! declared in [`crate::session_http::requirement`] as a credential of its own
//! so the guard's table has no hole in it, and verifying that credential in its
//! own handler.
//!
//! The body is taken as [`axum::body::Bytes`] and not as `Json<T>`, because the
//! signature covers the bytes as sent: a body parsed, re-serialised or even
//! trimmed before verification authenticates something other than what
//! arrived. Verify, then parse.
//!
//! [`crate::hermes_answer`] holds every decision. This file is the route, the
//! size limit, and the mapping from a refusal to an answer.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;
use tracing::warn;

use crate::hermes_answer::{AnswerRefusal, Received, MAX_PUSH_BYTES, SIGNATURE_HEADER};
use crate::http::Gateway;

pub fn routes() -> Router<Gateway> {
    // The path is written out here and not as `ANSWER_PATH`, because
    // `tests/openapi.rs` reads this crate's own source for `.route("…")` calls
    // and fails on a path it cannot see — which is how a route added without a
    // description is caught. The constant and the literal are asserted equal by
    // `hermes_answer_http::tests`.
    Router::new().route("/_twalk/hermes/answers", post(receive_answer))
}

async fn receive_answer(
    State(gateway): State<Gateway>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(answers) = gateway.answers() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "hermes_answers_not_configured",
            "this Gateway receives no answer from Hermes: set GATEWAY_HERMES_ANSWER_SECRET and \
             GATEWAY_HERMES_DOMAIN (and GATEWAY_NATS_URL, which the suggestion is published on). \
             Saying so beats accepting an answer and throwing it away.",
        );
    };
    if body.len() > MAX_PUSH_BYTES {
        gateway.metrics().record_hermes_answer("push_too_large");
        return api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "push_too_large",
            &format!(
                "the push is {} bytes and this endpoint reads at most {MAX_PUSH_BYTES}. An answer \
                 is one reply and three short fields; a body this size means the Hermes hook is \
                 pointed at a fatter event than transform_llm_output.",
                body.len()
            ),
        );
    }
    let signature = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok());
    match answers.receive(&body, signature).await {
        Ok(Received::Published {
            suggestion_event_id,
            language,
            stream_sequence,
        }) => (
            StatusCode::OK,
            Json(json!({
                "status": "published",
                "suggestion_event_id": suggestion_event_id,
                "language": language,
                "stream_sequence": stream_sequence,
            })),
        )
            .into_response(),
        // A `200` on purpose: a turn that was never a Twalk wake is not a
        // failure, and a `4xx` would teach an operator to ignore this endpoint's
        // errors. The reason is in the answer and in `/metrics`.
        Ok(Received::Ignored { reason }) => (
            StatusCode::OK,
            Json(json!({ "status": "ignored", "reason": reason })),
        )
            .into_response(),
        Err(refusal) => refuse(&gateway, refusal),
    }
}

/// One refusal, counted under the same code the caller is given — so what an
/// operator reads in `/metrics` is the word the answer carried.
fn refuse(gateway: &Gateway, refusal: AnswerRefusal) -> Response {
    let code = refusal.code();
    let status = refusal.status();
    let message = refusal.message();
    gateway.metrics().record_hermes_answer(code);
    // At `warn` and not `debug`: every one of these is a suggestion the user
    // will never see, and the whole point of refusing rather than defaulting is
    // that somebody can find out why.
    warn!(%code, status = status.as_u16(), detail = %message, "a Hermes answer was refused");
    api_error(status, code, &message)
}

/// One error answer shape for the whole origin: `openapi.yaml`'s `Error`.
fn api_error(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({ "error": code, "detail": detail }))).into_response()
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_route_and_the_constant_are_one_path() {
        // Two spellings of one path exist only because the conformance test
        // reads the source for a literal; this is what keeps them from
        // drifting, which would leave the guard's table classifying a path the
        // router does not serve.
        assert_eq!(crate::hermes_answer::ANSWER_PATH, "/_twalk/hermes/answers");
    }
}
