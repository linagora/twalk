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
use tracing::{error, info, warn};

use crate::hermes_answer::{AnswerRefusal, Received, MAX_PUSH_BYTES, SIGNATURE_HEADER};
use crate::http::Gateway;

pub fn routes() -> Router<Gateway> {
    // The path is written out here and not as `ANSWER_PATH`, because
    // `tests/openapi.rs` reads this crate's own source for `.route("…")` calls
    // and fails on a path it cannot see — which is how a route added without a
    // description is caught. The constant and the literal are asserted equal by
    // `hermes_answer_http::tests`.
    Router::new()
        .route("/_twalk/hermes/answers", post(receive_answer))
        .route(
            "/_twalk/hermes/mail-rule-proposals",
            post(propose_mail_rule),
        )
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
        // Also a `200`, and for a stronger reason: the agent did the right
        // thing (#367). It needed something only the owner can say, asked
        // them in their own channel, and told this Gateway so — which is a
        // line in the owner's journal, not a failure. The `4xx` this used to
        // be was the route reading a question as an unreadable answer.
        Ok(Received::Deferred { trigger_event_id }) => (
            StatusCode::OK,
            Json(json!({ "status": "deferred", "trigger_event_id": trigger_event_id })),
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
        // And the proposal's, which is what the guard's table was missing
        // altogether until #431: a `/_twalk/` path the table does not name is
        // closed to everything but a browser's device token.
        assert_eq!(
            crate::hermes_answer::PROPOSAL_PATH,
            "/_twalk/hermes/mail-rule-proposals"
        );
    }
}

/// `POST /_twalk/hermes/mail-rule-proposals` — the drafting agent proposing a
/// triage rule (#420, ADR 0042).
///
/// **Proposing is the only thing it can do here.** There is no route by which
/// the agent writes a rule or moves a mail, and that is the ticket rather than
/// a precaution: the agent reads text written by strangers, and a proposal is
/// the only shape in which that reading cannot become an action. Approving is
/// the owner's, on their own screen, and the rule is then written with the
/// owner as the actor.
///
/// Signed with the same secret the answers hook uses, and verified before a
/// byte of the body is looked at.
///
/// **Every refusal here goes through [`api_error`]**, like every other answer
/// this origin gives. It did not, until #431 described this route: the
/// refusals named their code `code` where the whole origin names it `error`,
/// which `openapi.yaml`'s `Error` schema forbids outright
/// (`additionalProperties: false`). Nothing read it — this is the one route
/// the description had missed, so no client had been generated against it —
/// and that is exactly the window in which a second error vocabulary gets
/// established. The codes are the sibling route's words too, for one reason
/// each: the seam is unconfigured by the same variable, and a body over the
/// limit is `push_too_large` on both.
async fn propose_mail_rule(
    State(gateway): State<Gateway>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(answers) = gateway.answers() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "hermes_answers_not_configured",
            "this Gateway takes no proposals: set GATEWAY_HERMES_ANSWER_SECRET",
        );
    };
    if body.len() > MAX_PUSH_BYTES {
        return api_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "push_too_large",
            "a proposal is one rule and a sentence",
        );
    }
    let signature = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok());
    if !answers.authenticates(signature, &body) {
        warn!("refused a rule proposal: the signature is missing or wrong");
        return api_error(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "the proposal is not signed by this deployment's Hermes",
        );
    }
    // Unreachable as this Gateway is wired, and kept: `main.rs` builds the
    // Hermes seam inside the branch that has the store, so `answers()` is
    // `Some` only where `consent()` is. A route that writes to a store must
    // not assume one, and the description says this is the branch no
    // deployment can produce.
    let Some(consent) = gateway.consent() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "consent_not_configured",
            "no store to journal a proposal in",
        );
    };
    #[derive(serde::Deserialize)]
    struct Body {
        rule: crate::mail_rules::Rule,
        #[serde(default)]
        because: Option<String>,
    }
    let proposal: Body = match serde_json::from_slice(&body) {
        Ok(proposal) => proposal,
        Err(error) => {
            return api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "malformed_request",
                &format!("the proposal could not be read: {error}"),
            );
        }
    };
    // The rule's own shape is checked now — an empty match, an age out of
    // range — so a proposal the owner could never approve is refused at the
    // door rather than shown to them. The **allowlist** is deliberately not
    // checked here: a destination the owner has not declared yet is a
    // reasonable thing to propose, and they declare it when they approve.
    // The **allowlist** is deliberately not checked — a destination the owner
    // has not declared yet is a reasonable thing to propose, and they declare
    // it when they approve. The trash is not: ADR 0042 says neither a typo nor
    // a proposal can invent one, so it is refused at the door rather than
    // shown to the owner as something they might approve (found in review).
    if let Err(why) = crate::mail_rules::check_destination(&proposal.rule.destination) {
        return api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            why.code(),
            "this rule is not one this deployment could apply",
        );
    }
    if let Err(why) = proposal.rule.matches.check() {
        return api_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            why.code(),
            "this rule is not one this deployment could apply",
        );
    }
    let at = crate::consent::rfc3339_millis(std::time::SystemTime::now());
    let because = proposal
        .because
        .as_deref()
        .map(|words| words.chars().take(500).collect::<String>());
    match consent
        .store()
        .record_rule_proposal(&proposal.rule, because.as_deref(), &at)
    {
        Ok(sequence) => {
            info!(sequence, rule = %proposal.rule.id, "the assistant proposed a triage rule; nothing is applied until the owner says so");
            (
                StatusCode::CREATED,
                Json(json!({ "sequence": sequence, "state": "proposed" })),
            )
                .into_response()
        }
        Err(error) => {
            error!(%error, "failed to record a rule proposal");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "code": "store_unavailable", "detail": "the proposal could not be recorded" })),
            )
                .into_response()
        }
    }
}
