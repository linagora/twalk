//! Approval's HTTP surface: giving one, and asking what became of it
//! (ticket #24).
//!
//! # Two routes, and why the second one exists
//!
//! `POST /api/approvals` is the deliberate act. It takes **one** suggestion,
//! publishes the approved reply inside the request, and answers `201` with
//! the position the bus stored it at — or a refusal that names its cause and
//! carries a status a client can act on. There is no third outcome: an
//! approval is never "accepted, we will try later", because a send held for
//! later is a send whose consent check has gone stale.
//!
//! `GET /api/approvals/{suggestion_event_id}` answers the question that
//! follows: *did that reply actually go out?* It reads the Gateway's own
//! record and says `published` with a stream position, `unpublished` when
//! this Gateway wrote the row and the publication did not land, or `404`
//! when this suggestion was never approved. All three are terminal. This
//! project has shipped a spinner with no terminal state three times (#111,
//! #135, #139), and the way not to ship a fourth is for every state a screen
//! can be in to have a name here.
//!
//! # Who is calling
//!
//! Nothing here authenticates: #52's guard already does. `/api/approvals` is
//! not in [`crate::session_http::requirement`]'s exception table, so it
//! requires a device token like everything else — the direction working as
//! intended. The [`Device`] the guard injected is read for the log line;
//! **who approved** is the deployment's owner, not the device, exactly as a
//! consent decision's actor is (ADR 0011).
//!
//! # What the answers look like
//!
//! One shape for every refusal, the Gateway's own `Error` document: a stable
//! `error` code and a `detail` for an operator's logs. The codes are
//! [`crate::approval::Refusal`]'s, one per situation, and the statuses are
//! chosen per situation too — see that type for why a bus that does not
//! answer is a `502` and not the `503` a client would read as "this
//! deployment has no approvals".

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::{json, Value};
use tracing::{debug, info};

use crate::approval::{is_event_id, Approved, Invalid, Refusal, Request};
use crate::http::Gateway;
use crate::session::Device;
use crate::store::RecordedApproval;

/// The approval routes. Merged into the Gateway's router like every other
/// ticket's, so they are behind the guard without asking.
pub fn routes() -> Router<Gateway> {
    Router::new().route("/api/approvals", post(approve)).route(
        "/api/approvals/{suggestion_event_id}",
        get(approval_of_suggestion),
    )
}

/// `POST /api/approvals` — approve one suggestion, and send the reply.
///
/// ```json
/// {
///   "suggestion_event_id": "<the persona.suggest.produced event's id>",
///   "final": { "body": "the edited text", "format": "text/plain" }
/// }
/// ```
///
/// `final` is optional: without it the persona's own suggestion is sent
/// unchanged, and the published event's `edited` flag says which happened.
/// `approved_by` may be stated and must then be this deployment's owner —
/// the Gateway stamps it either way, and will not record an approval under
/// another name.
///
/// There is deliberately **no** endpoint that approves a list. An approval is
/// a deliberate act, never a batch (`CONTEXT.md`), and a body carrying an
/// array is refused with `approval_is_not_a_batch` rather than helpfully
/// interpreted.
///
/// `201` with the approval and its position on the bus. Everything else is a
/// refusal whose code says which of a dozen situations occurred, and which
/// this handler counts under that same code.
async fn approve(
    State(gateway): State<Gateway>,
    Extension(device): Extension<Device>,
    body: String,
) -> Response {
    let Some(approvals) = gateway.approvals() else {
        return not_configured();
    };
    let body: Value = match serde_json::from_str(&body) {
        Ok(body) => body,
        Err(error) => {
            gateway
                .metrics()
                .record_approval_refusal("malformed_request");
            return api_error(
                StatusCode::BAD_REQUEST,
                "malformed_request",
                &format!("the request body is not JSON: {error}"),
            );
        }
    };
    let request = match Request::parse(&body) {
        Ok(request) => request,
        Err(invalid) => return refuse(&gateway, &device, Refusal::Invalid(invalid)),
    };
    let suggestion_event_id = request.suggestion_event_id.clone();
    match approvals.approve(request).await {
        Ok(approved) => {
            info!(
                device = %device.id,
                event_id = %approved.event_id,
                suggestion = %suggestion_event_id,
                "an approval was given"
            );
            (StatusCode::CREATED, Json(approved_json(&approved))).into_response()
        }
        Err(refusal) => refuse(&gateway, &device, refusal),
    }
}

/// `GET /api/approvals/{suggestion_event_id}` — what became of one approval.
///
/// `200` with the record, `404 approval_not_found` when this suggestion was
/// never approved. The record carries `posted` as well: the Sensor's report
/// of what the reply reached once posted (`contact` or `nobody`, and by which
/// account), or `null` while there is none — published on the bus and
/// delivered to the contact are two facts (#216). And `undelivered`, the
/// third: the sender gave up, with its reason (#311). A client that reads
/// `publication` alone is reading the first of three. The record's
/// `publication` is `published` or
/// `unpublished`, and never anything a screen should render as a spinner:
/// an approval is published inside its own request or it is refused, so
/// `unpublished` means a crash happened between the Gateway's write and the
/// bus's acknowledgement — a state worth naming, and repaired by approving
/// the same suggestion again (the bus deduplicates on the contract's id, so
/// nothing is sent twice).
async fn approval_of_suggestion(
    State(gateway): State<Gateway>,
    Path(suggestion_event_id): Path<String>,
) -> Response {
    let Some(approvals) = gateway.approvals() else {
        return not_configured();
    };
    if !is_event_id(&suggestion_event_id) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "malformed_request",
            &Invalid::SuggestionNotAnEventId(suggestion_event_id).message(),
        );
    }
    match approvals.recorded(&suggestion_event_id) {
        Ok(Some(recorded)) => {
            // "Did my reply go out?" has two answers and this is the one
            // place a client asks it after the fact: published on the bus,
            // which the record says, and delivered to the contact, which
            // only the Sensor's own report can say (#216).
            let posted = approvals.posted(&recorded).await;
            // And whether it was given up on (#311), which is neither of the
            // other two answers: a reply the Sensor or the collector could
            // not send is not waiting to be posted, and a record that said
            // only `published` let an owner believe it went out.
            let undelivered = approvals.undelivered(&recorded).await;
            let mut rendered = recorded_json(&recorded);
            rendered["posted"] = posted
                .as_ref()
                .map(crate::suggestions_http::posted_json)
                .unwrap_or(Value::Null);
            rendered["undelivered"] = undelivered
                .as_ref()
                .map(crate::suggestions_http::undelivered_json)
                .unwrap_or(Value::Null);
            Json(rendered).into_response()
        }
        Ok(None) => api_error(
            StatusCode::NOT_FOUND,
            "approval_not_found",
            "this Gateway has no record of approving that suggestion: it was never approved \
             here, or it was approved by another deployment",
        ),
        Err(error) => {
            tracing::error!(%error, "failed to read an approval");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_unavailable",
                "the approval could not be read",
            )
        }
    }
}

/// One refusal: counted under its own code, logged, and answered.
fn refuse(gateway: &Gateway, device: &Device, refusal: Refusal) -> Response {
    gateway.metrics().record_approval_refusal(refusal.outcome());
    debug!(
        code = refusal.code(),
        status = refusal.status().as_u16(),
        device = %device.id,
        "refused an approval"
    );
    // The already-approved refusal carries the first approval, so a client
    // that lost the first answer learns where its reply went instead of
    // being told to try again.
    if let Refusal::AlreadyApproved(existing) = &refusal {
        return (
            refusal.status(),
            Json(json!({
                "error": refusal.code(),
                "detail": refusal.message(),
                "approval": recorded_json(existing),
            })),
        )
            .into_response();
    }
    api_error(refusal.status(), refusal.code(), &refusal.message())
}

/// This deployment approves nothing: a `503` naming the variables, not a
/// `404` and not a silent success.
fn not_configured() -> Response {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "approvals_not_configured",
        "this Gateway cannot approve a suggestion: it needs the bus the suggestions are on and \
         the consent store the approval is checked against. Set GATEWAY_NATS_URL (and \
         GATEWAY_OWNER, which the approver's identity comes from)",
    )
}

fn api_error(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({ "error": code, "detail": detail }))).into_response()
}

/// The answer to an approval that happened — rendered through the same
/// function as a read-back, so the two answers are one shape.
///
/// That matters more than it looks: a client renders "your reply went out"
/// from a `POST` answer and from a `GET` answer, and a member present in one
/// and absent in the other is how a screen ends up with two code paths and a
/// state that only one of them handles.
fn approved_json(approved: &Approved) -> Value {
    recorded_json(&RecordedApproval {
        event_id: approved.event_id.clone(),
        suggestion_event_id: approved.approval.suggestion.event_id.clone(),
        approved_by: approved.approval.approved_by.clone(),
        persona_id: approved.approval.suggestion.persona_id.clone(),
        network: approved.approval.trigger.network,
        contact: approved.approval.trigger.contact.clone(),
        edited: approved.approval.edited,
        written_by: approved.approval.written_by.as_str().to_owned(),
        approved_at: approved.approved_at.clone(),
        published_at: Some(approved.approved_at.clone()),
        stream_sequence: Some(approved.stream_sequence),
    })
}

/// One approval, as every answer of this surface renders one.
fn recorded_json(recorded: &RecordedApproval) -> Value {
    json!({
        "event_id": recorded.event_id,
        "suggestion_event_id": recorded.suggestion_event_id,
        "approved_by": recorded.approved_by,
        "persona_id": recorded.persona_id,
        "network": recorded.network.as_str(),
        "contact": recorded.contact,
        "edited": recorded.edited,
        // Whose words went out (#327): what the disclosure followed, and
        // the question `edited` could not answer on its own.
        "written_by": recorded.written_by,
        "approved_at": recorded.approved_at,
        "publication": recorded.publication(),
        "stream_sequence": recorded.stream_sequence,
        "published_at": recorded.published_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::Network;

    #[test]
    fn a_published_approval_says_where_it_landed() {
        let recorded = RecordedApproval {
            event_id: "c".repeat(64),
            suggestion_event_id: "a".repeat(64),
            approved_by: "@michel:example.com".to_owned(),
            persona_id: "assistant".to_owned(),
            network: Network::Whatsapp,
            contact: "@whatsapp_336:example.com".to_owned(),
            edited: true,
            written_by: "persona".to_owned(),
            approved_at: "2026-09-17T10:04:37.000Z".to_owned(),
            published_at: Some("2026-09-17T10:04:37.100Z".to_owned()),
            stream_sequence: Some(4242),
        };
        let rendered = recorded_json(&recorded);
        assert_eq!(rendered["publication"], json!("published"));
        assert_eq!(rendered["stream_sequence"], json!(4242));
    }

    #[test]
    fn an_approval_that_never_reached_the_bus_says_so_rather_than_nothing() {
        // The crash window: the row exists and the publication did not land.
        // It is a named, terminal state — not an absence, and not a spinner.
        let recorded = RecordedApproval {
            event_id: "c".repeat(64),
            suggestion_event_id: "a".repeat(64),
            approved_by: "@michel:example.com".to_owned(),
            persona_id: "assistant".to_owned(),
            network: Network::Whatsapp,
            contact: "@whatsapp_336:example.com".to_owned(),
            edited: false,
            written_by: "persona".to_owned(),
            approved_at: "2026-09-17T10:04:37.000Z".to_owned(),
            published_at: None,
            stream_sequence: None,
        };
        let rendered = recorded_json(&recorded);
        assert_eq!(rendered["publication"], json!("unpublished"));
        assert_eq!(rendered["stream_sequence"], Value::Null);
        assert_eq!(rendered["published_at"], Value::Null);
    }

    #[test]
    fn no_answer_of_this_surface_carries_the_text_that_was_sent() {
        // The reply's body is on the bus and is read from there. A member
        // here would make the Gateway a second copy of the conversation.
        let recorded = RecordedApproval {
            event_id: "c".repeat(64),
            suggestion_event_id: "a".repeat(64),
            approved_by: "@michel:example.com".to_owned(),
            persona_id: "assistant".to_owned(),
            network: Network::Whatsapp,
            contact: "@whatsapp_336:example.com".to_owned(),
            edited: true,
            written_by: "persona".to_owned(),
            approved_at: "2026-09-17T10:04:37.000Z".to_owned(),
            published_at: Some("2026-09-17T10:04:37.100Z".to_owned()),
            stream_sequence: Some(1),
        };
        let rendered = recorded_json(&recorded);
        for member in ["body", "final", "suggestion", "text"] {
            assert!(
                rendered.get(member).is_none(),
                "the approval answer carries {member:?}, which is the conversation"
            );
        }
    }
}
