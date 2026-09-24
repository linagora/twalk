//! `GET /_twalk/hermes/freebusy` — the one route Hermes reads from (ticket
//! #281), beside the one it posts to. The same shape as the answers' route:
//! outside `/api/`, under the reserved `/_twalk/` prefix, declared in
//! [`crate::session_http::requirement`] as a credential of its own, and
//! verifying that credential in its own handler.
//!
//! The query string is taken **as sent** — from the request's URI, not from
//! a parsed and re-encoded map — because the signature covers it as sent:
//! a query re-serialised before verification authenticates something other
//! than what arrived. Verify, then read.
//!
//! [`crate::hermes_freebusy`] holds every decision. This file is the route
//! and the mapping from a refusal to an answer.

use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::hermes_answer::SIGNATURE_HEADER;
use crate::hermes_freebusy::{
    FreeBusy, ReadRefusal, ReadRequest, DELIVERY_HEADER, TIMESTAMP_HEADER,
};
use crate::http::Gateway;

pub fn routes() -> Router<Gateway> {
    // Written out, not `FREEBUSY_PATH`: `tests/openapi.rs` reads this crate's
    // source for `.route("…")` literals. The two are asserted equal below.
    Router::new()
        .route("/_twalk/hermes/freebusy", get(read_free_busy))
        .route("/_twalk/hermes/event-facts", get(read_event_facts))
}

/// The members a read carries, read off the same string the signature
/// covers — by hand, for the reason the free/busy read does it by hand: an
/// extractor would refuse a malformed query in its own words, before the
/// signature was checked and without a record, and every read is recorded,
/// the malformed ones included.
fn request_of(uri: &axum::http::Uri, headers: &HeaderMap) -> ReadRequest {
    let query = uri.query().unwrap_or_default().to_owned();
    let mut request = ReadRequest {
        query: query.clone(),
        ..ReadRequest::default()
    };
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = crate::contacts_http::percent_decode(value);
        match name {
            "connection" => request.connection = Some(value),
            "from" => request.from = Some(value),
            "to" => request.to = Some(value),
            "uid" => request.uid = Some(value),
            _ => {}
        }
    }
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim().to_owned())
    };
    request.timestamp = header(TIMESTAMP_HEADER);
    request.signature = header(SIGNATURE_HEADER);
    request.delivery = header(DELIVERY_HEADER);
    request
}

/// `GET /_twalk/hermes/event-facts` — what one of the owner's events
/// carries, and never what it says (#355).
async fn read_event_facts(
    State(gateway): State<Gateway>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    let Some(reads) = gateway.reads() else {
        return refuse(&gateway, ReadRefusal::SeamNotConfigured);
    };
    let request = request_of(&uri, &headers);
    match reads.event_facts(&request).await {
        Ok(facts) => (
            StatusCode::OK,
            Json(json!({
                "connection": request.connection,
                "uid": request.uid,
                "found": facts.found,
                "conference": facts.conference,
                "description_characters": facts.description_characters,
                "attachments": facts.attachments,
            })),
        )
            .into_response(),
        Err(refusal) => refuse(&gateway, refusal),
    }
}

async fn read_free_busy(
    State(gateway): State<Gateway>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    let Some(reads) = gateway.reads() else {
        return refuse(&gateway, ReadRefusal::SeamNotConfigured);
    };
    let request = request_of(&uri, &headers);
    match reads.read(&request).await {
        Ok(answer) => (StatusCode::OK, Json(answered(&request, answer))).into_response(),
        Err(refusal) => refuse(&gateway, refusal),
    }
}

/// The answer's body: the window as asked, the intervals, and the owner's
/// own time when the deployment knows it (#369).
///
/// The three members go **together or not at all**. A calendar that declares
/// no zone, or a collector older than this, produces an answer with none of
/// them, and the skill tells the agent what to do then: speak in UTC and name
/// it. Half of them — a zone with no hour, an hour with no zone — would be
/// worse than none, because each is only usable with the other.
fn answered(request: &ReadRequest, answer: FreeBusy) -> serde_json::Value {
    let mut body = json!({
        "connection": request.connection,
        "from": request.from,
        "to": request.to,
        "busy": answer.busy,
    });
    if let (Some(timezone), Some(source), Some(now)) =
        (answer.timezone, answer.timezone_source, answer.now)
    {
        body["timezone"] = json!(timezone);
        body["timezone_source"] = json!(source);
        body["now"] = json!(now);
    }
    body
}

/// One refusal, in `openapi.yaml`'s `Error` shape, with the connection's
/// `state` beside it when that is what was refused — the same shape an
/// approval towards that connection gets (#275).
fn refuse(gateway: &Gateway, refusal: ReadRefusal) -> Response {
    if refusal == ReadRefusal::SeamNotConfigured {
        // Counted here, since no `Reads` exists to count it.
        gateway.metrics().record_hermes_read(refusal.code());
    }
    let mut body = json!({ "error": refusal.code(), "detail": refusal.message() });
    if let Some(state) = refusal.state() {
        body["state"] = json!(state);
    }
    (
        StatusCode::from_u16(refusal.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(body),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_routes_and_the_constants_are_the_same_paths() {
        // The router spells them out so `tests/openapi.rs` can read them
        // out of this source; these assertions are what keeps the two
        // spellings one.
        assert_eq!(
            crate::hermes_freebusy::FREEBUSY_PATH,
            "/_twalk/hermes/freebusy"
        );
        assert_eq!(
            crate::hermes_freebusy::EVENT_FACTS_PATH,
            "/_twalk/hermes/event-facts"
        );
    }

    use super::*;
    use crate::hermes_freebusy::Busy;

    fn request() -> ReadRequest {
        ReadRequest {
            connection: Some("calendar-linagora".to_owned()),
            from: Some("2026-09-28T06:00:00Z".to_owned()),
            to: Some("2026-10-02T18:00:00Z".to_owned()),
            ..ReadRequest::default()
        }
    }

    fn intervals() -> Vec<Busy> {
        vec![Busy {
            start: "2026-10-01T07:00:00Z".to_owned(),
            end: "2026-10-01T10:00:00Z".to_owned(),
        }]
    }

    #[test]
    fn the_owners_own_time_is_answered_when_the_deployment_knows_it() {
        let body = answered(
            &request(),
            FreeBusy {
                busy: intervals(),
                timezone: Some("Europe/Paris".to_owned()),
                timezone_source: Some("calendar".to_owned()),
                now: Some("2026-09-24T20:36:26+02:00".to_owned()),
            },
        );
        assert_eq!(body["timezone"], "Europe/Paris");
        assert_eq!(body["timezone_source"], "calendar");
        assert_eq!(body["now"], "2026-09-24T20:36:26+02:00");
        assert_eq!(body.as_object().map(|object| object.len()), Some(7));
    }

    #[test]
    fn a_deployment_that_knows_no_zone_answers_none_of_the_three() {
        // A calendar that declares no zone, or a collector older than #369.
        // Half the members would be worse than none: each is only usable
        // with the other, and a zone with no hour invites the arithmetic
        // this ticket exists to stop.
        let body = answered(
            &request(),
            FreeBusy {
                busy: intervals(),
                timezone: None,
                timezone_source: None,
                now: None,
            },
        );
        for member in ["timezone", "timezone_source", "now"] {
            assert!(body.get(member).is_none(), "{member} was answered: {body}");
        }
        assert_eq!(body.as_object().map(|object| object.len()), Some(4));
    }

    #[test]
    fn a_zone_with_no_hour_is_not_half_answered() {
        let body = answered(
            &request(),
            FreeBusy {
                busy: intervals(),
                timezone: Some("Europe/Paris".to_owned()),
                timezone_source: Some("calendar".to_owned()),
                now: None,
            },
        );
        assert!(body.get("timezone").is_none(), "half of it was answered: {body}");
    }
}
