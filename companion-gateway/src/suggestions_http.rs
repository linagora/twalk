//! Reading suggestions over HTTP: the listing #100's approval screen draws
//! from, and the single read a deep link needs (ticket #97).
//!
//! # Two routes
//!
//! `GET /api/suggestions` is the listing: what each persona proposed, for
//! which message, when, whether it has expired and whether it was already
//! approved. It is a **projection of the bus** — nothing here is stored, and
//! the answer carries the stretch of the stream it was computed from, so a
//! bounded read is a bound the caller can see.
//!
//! `GET /api/suggestions/{suggestion_event_id}` is one of them, for a screen
//! that was given an id — a deep link, a notification, a reload. It is the
//! route where the three situations are told apart by status rather than by
//! a field: `404` for a suggestion the whole retained stream does not hold,
//! `410` for one the Gateway did not look far enough back for, and `200` with
//! `standing` for one that is there, expired or approved or neither.
//!
//! # What these answers deliberately do not carry
//!
//! Nothing of the message being answered. Not its body, not an excerpt of
//! what *it* was quoting, not the sender's display name, not a
//! `network_identifier` — and the enforcement is not a serialiser leaving
//! fields out, it is that [`crate::suggestions`] never opens an inbound
//! event. A suggestion quotes a contact's message, and an excerpt belongs to
//! the author of the quoted message rather than to whoever sent the event
//! carrying it (#110, ADR 0012). A listing that re-published a revoked
//! contact's words through a new door would be that same defect one layer up.
//!
//! The persona's proposed text *is* carried: it is the thing being approved
//! and the reason the screen exists.
//!
//! # Who is calling
//!
//! Nothing here authenticates: #52's guard already does. Neither route is in
//! [`crate::session_http::requirement`]'s exception table, so both require a
//! device token — the owner's own read of their own suggestions.
//!
//! # What the refusals look like
//!
//! The Gateway's one `Error` document, and the codes are
//! [`crate::approval::Refusal`]'s — literally, not by convention. A
//! suggestion that is out of reach is `410 suggestion_out_of_reach` here and
//! at `POST /api/approvals`, because it is the same fact about the same
//! bounded read, and a client that learned one code should not have to learn
//! a second for the same situation.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use tracing::debug;

use crate::approval::{is_event_id, Invalid, Refusal};
use crate::http::Gateway;
use crate::store::RecordedApproval;
use crate::suggestions::{Listed, Listing, DEFAULT_LIMIT, MAX_LIMIT};

/// The suggestion routes. Merged into the Gateway's router like every other
/// ticket's, so they are behind the guard without asking.
pub fn routes() -> Router<Gateway> {
    Router::new()
        .route("/api/suggestions", get(list))
        .route("/api/suggestions/{suggestion_event_id}", get(one))
}

/// `GET /api/suggestions?limit=50` — the suggestions waiting to be read.
///
/// ```json
/// {
///   "suggestions": [{
///     "event_id": "…", "persona_id": "assistant", "network": "whatsapp",
///     "produced_at": "2026-09-17T10:00:00Z",
///     "expires_at": "2026-09-17T11:00:00Z",
///     "standing": "approvable",
///     "trigger": { "event_id": "…", "event_type": "…inbound.message.received.v1" },
///     "suggestion": { "body": "Pas de problème, à 20h !", "format": "text/plain" },
///     "approval": null
///   }],
///   "window": { "from_sequence": 1, "to_sequence": 42, "sequences": 20000,
///               "reached_start_of_stream": true },
///   "truncated": false,
///   "unreadable": 0
/// }
/// ```
///
/// Newest first. `window` is the stretch of the stream this answer was
/// computed from: `reached_start_of_stream: false` means there may be older
/// suggestions the Gateway did not read, which is a fact about the answer and
/// belongs in it. `truncated` means the limit cut the list rather than the
/// window. `unreadable` counts suggestions found and not understood — a
/// missing row is never silent.
async fn list(
    State(gateway): State<Gateway>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(suggestions) = gateway.suggestions() else {
        return not_configured();
    };
    let limit = match query.get("limit").filter(|value| !value.is_empty()) {
        None => DEFAULT_LIMIT,
        Some(raw) => match raw.parse::<usize>() {
            Ok(limit) if (1..=MAX_LIMIT).contains(&limit) => limit,
            _ => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "malformed_request",
                    &format!(
                        "limit is {raw:?}: it is a whole number between 1 and {MAX_LIMIT}. A \
                         screen draws a page; reading further back is what the deployment's \
                         GATEWAY_APPROVAL_LOOKUP_WINDOW is for"
                    ),
                )
            }
        },
    };
    match suggestions.list(limit).await {
        Ok(listing) => Json(listing_json(&listing)).into_response(),
        Err(refusal) => refuse(refusal),
    }
}

/// `GET /api/suggestions/{suggestion_event_id}` — one suggestion.
///
/// The route where an expired suggestion, a missing one and one the Gateway
/// did not look far enough back for are three different answers:
///
/// - `200` with `standing: "expired"` — it is there, and it cannot be
///   approved;
/// - `404 suggestion_not_found` — the whole retained stream was read;
/// - `410 suggestion_out_of_reach` — the bounded read gave up first, so it
///   may exist further back.
///
/// And `200` with `standing: "approved"` carries the approval record, which
/// is where "did the reply go out?" is answered (#24).
async fn one(State(gateway): State<Gateway>, Path(event_id): Path<String>) -> Response {
    let Some(suggestions) = gateway.suggestions() else {
        return not_configured();
    };
    if !is_event_id(&event_id) {
        return api_error(
            StatusCode::BAD_REQUEST,
            "malformed_request",
            &Invalid::SuggestionNotAnEventId(event_id).message(),
        );
    }
    match suggestions.one(&event_id).await {
        Ok(listed) => Json(suggestion_json(&listed)).into_response(),
        Err(refusal) => refuse(refusal),
    }
}

fn refuse(refusal: Refusal) -> Response {
    debug!(
        code = refusal.code(),
        status = refusal.status().as_u16(),
        "refused a suggestion read"
    );
    api_error(refusal.status(), refusal.code(), &refusal.message())
}

/// This deployment reads no suggestions: a `503` naming the variable, not an
/// empty list. "Nobody has suggested anything" and "this Gateway is not
/// watching the bus" are very different claims, and only one of them is about
/// the personas — the same distinction [`crate::contacts_http`] draws.
fn not_configured() -> Response {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "suggestions_not_configured",
        "this Gateway cannot read suggestions: they live on the bus and it has none configured. \
         Set GATEWAY_NATS_URL",
    )
}

fn api_error(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({ "error": code, "detail": detail }))).into_response()
}

fn listing_json(listing: &Listing) -> Value {
    json!({
        "suggestions": listing
            .suggestions
            .iter()
            .map(suggestion_json)
            .collect::<Vec<_>>(),
        "window": {
            "from_sequence": listing.window.from_sequence,
            "to_sequence": listing.window.to_sequence,
            "sequences": listing.window.sequences,
            "reached_start_of_stream": listing.window.reached_start_of_stream,
        },
        "truncated": listing.truncated,
        "unreadable": listing.unreadable,
    })
}

/// One suggestion, as both routes render one — the listing and the single
/// read are one shape, for the reason [`crate::approval_http`] renders a
/// `POST` answer and a read-back through one function: a member present in
/// one and absent in the other is how a screen grows two code paths and a
/// state only one of them handles.
fn suggestion_json(listed: &Listed) -> Value {
    json!({
        "event_id": listed.event_id,
        "source": listed.source,
        "persona_id": listed.persona_id,
        "network": listed.network.as_str(),
        "consent": listed.consent_label.as_str(),
        "produced_at": listed.produced_at,
        "expires_at": listed.expires_at,
        "attempt": listed.attempt,
        "standing": listed.standing.as_str(),
        "trigger": {
            "event_id": listed.trigger_event_id,
            "event_type": listed.trigger_event_type,
        },
        "suggestion": {
            "body": listed.suggestion.body,
            "format": listed.suggestion.format.as_str(),
        },
        "stream_sequence": listed.stream_sequence,
        "approval": listed.approval.as_ref().map(approval_json),
        // Before the approval: whether a reply could reach the contact at
        // all, from the owner's account's membership of the trigger's room
        // (#216). After it: what the Sensor said the reply reached. Two
        // members, because "published on your bus" and "delivered" are two
        // facts and the screen must never render them as one.
        "delivery": {
            "reach": listed.delivery.reach(),
            "detail": listed.delivery.detail(),
        },
        "posted": listed.posted.as_ref().map(posted_json),
    })
}

/// The Sensor's report, as both the listing and `GET /api/approvals/{id}`
/// render it.
pub fn posted_json(posted: &crate::approval::Posted) -> Value {
    json!({
        "reach": posted.reach,
        "posted_as": posted.posted_as,
        "stream_sequence": posted.stream_sequence,
    })
}

/// The approval record, in the shape `GET /api/approvals/{id}` answers with
/// (#24) — the same members under the same names, so a client parses one
/// document type wherever it meets it.
fn approval_json(recorded: &RecordedApproval) -> Value {
    json!({
        "event_id": recorded.event_id,
        "suggestion_event_id": recorded.suggestion_event_id,
        "approved_by": recorded.approved_by,
        "persona_id": recorded.persona_id,
        "network": recorded.network.as_str(),
        "contact": recorded.contact,
        "edited": recorded.edited,
        "approved_at": recorded.approved_at,
        "publication": recorded.publication(),
        "stream_sequence": recorded.stream_sequence,
        "published_at": recorded.published_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::{Content, Format};
    use crate::consent::{Network, State};
    use crate::portals::Delivery;
    use crate::suggestions::{Standing, Window};

    fn listed(standing: Standing, approval: Option<RecordedApproval>) -> Listed {
        Listed {
            event_id: "a".repeat(64),
            source: "hermes://twalk.example.com/personas/assistant".to_owned(),
            persona_id: "assistant".to_owned(),
            network: Network::Whatsapp,
            consent_label: State::Granted,
            produced_at: "2026-09-17T10:00:00Z".to_owned(),
            expires_at: Some("2026-09-17T11:00:00Z".to_owned()),
            attempt: Some(1),
            trigger_event_id: "b".repeat(64),
            trigger_event_type: "fr.linagora.twalk.inbound.message.received.v1".to_owned(),
            suggestion: Content {
                body: "Pas de problème, à 20h !".to_owned(),
                format: Format::Plain,
            },
            stream_sequence: 42,
            standing,
            approval,
            delivery: Delivery::Unknown {
                why: "trigger_out_of_reach",
            },
            posted: None,
        }
    }

    #[test]
    fn a_suggestion_names_the_message_it_answers_and_carries_none_of_it() {
        let rendered = suggestion_json(&listed(Standing::Approvable, None));
        assert_eq!(rendered["trigger"]["event_id"], json!("b".repeat(64)));
        assert_eq!(
            rendered["trigger"]["event_type"],
            json!("fr.linagora.twalk.inbound.message.received.v1")
        );
        // Identity, and nothing else. A member here would be somebody else's
        // words travelling under the persona's consent label (#110).
        let trigger = rendered["trigger"]
            .as_object()
            .expect("the trigger is an object");
        assert_eq!(
            trigger.keys().collect::<Vec<_>>(),
            vec!["event_id", "event_type"],
            "the trigger is an identity here: {trigger:?}"
        );
        for member in ["body", "excerpt", "display_name", "contact", "reply_to"] {
            assert!(
                trigger.get(member).is_none(),
                "the listing carries the trigger's {member:?}"
            );
        }
    }

    #[test]
    fn an_unapproved_suggestion_says_so_with_a_named_state_and_not_a_null() {
        let rendered = suggestion_json(&listed(Standing::Approvable, None));
        assert_eq!(rendered["standing"], json!("approvable"));
        assert_eq!(rendered["approval"], Value::Null);
        // An expired one is a different word, not the absence of one.
        let rendered = suggestion_json(&listed(Standing::Expired, None));
        assert_eq!(rendered["standing"], json!("expired"));
        assert_eq!(rendered["expires_at"], json!("2026-09-17T11:00:00Z"));
    }

    #[test]
    fn an_approved_suggestion_carries_the_record_that_says_where_the_reply_went() {
        let recorded = RecordedApproval {
            event_id: "c".repeat(64),
            suggestion_event_id: "a".repeat(64),
            approved_by: "@michel:example.com".to_owned(),
            persona_id: "assistant".to_owned(),
            network: Network::Whatsapp,
            contact: "@whatsapp_336:example.com".to_owned(),
            edited: true,
            approved_at: "2026-09-17T10:04:37.000Z".to_owned(),
            published_at: Some("2026-09-17T10:04:37.100Z".to_owned()),
            stream_sequence: Some(4242),
        };
        let rendered = suggestion_json(&listed(Standing::Approved, Some(recorded)));
        assert_eq!(rendered["standing"], json!("approved"));
        assert_eq!(rendered["approval"]["publication"], json!("published"));
        assert_eq!(rendered["approval"]["stream_sequence"], json!(4242));
        // And it is the shape `GET /api/approvals/{id}` answers with, member
        // for member, so a client parses one document type.
        assert_eq!(rendered["approval"]["edited"], json!(true));
        assert_eq!(
            rendered["approval"]["contact"],
            json!("@whatsapp_336:example.com")
        );
    }

    #[test]
    fn the_answer_says_how_far_back_it_looked() {
        let listing = Listing {
            suggestions: vec![listed(Standing::Approvable, None)],
            window: Window {
                from_sequence: 100,
                to_sequence: 20_100,
                sequences: 20_000,
                reached_start_of_stream: false,
            },
            truncated: true,
            unreadable: 2,
        };
        let rendered = listing_json(&listing);
        assert_eq!(rendered["window"]["reached_start_of_stream"], json!(false));
        assert_eq!(rendered["window"]["from_sequence"], json!(100));
        assert_eq!(rendered["truncated"], json!(true));
        assert_eq!(
            rendered["unreadable"],
            json!(2),
            "a row the Gateway could not read is counted, not silently missing"
        );
    }

    #[test]
    fn a_read_of_this_surface_uses_the_same_codes_as_the_act_it_precedes() {
        // The same fact about the same bounded read, so the same code and
        // the same status on both doors.
        assert_eq!(Refusal::SuggestionNotFound.code(), "suggestion_not_found");
        assert_eq!(Refusal::SuggestionNotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            Refusal::SuggestionOutOfReach { window: 20_000 }.code(),
            "suggestion_out_of_reach"
        );
        assert_eq!(
            Refusal::SuggestionOutOfReach { window: 20_000 }.status(),
            StatusCode::GONE
        );
    }
}
