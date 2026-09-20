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
use crate::hermes_freebusy::{ReadRefusal, ReadRequest, DELIVERY_HEADER, TIMESTAMP_HEADER};
use crate::http::Gateway;

pub fn routes() -> Router<Gateway> {
    // Written out, not `FREEBUSY_PATH`: `tests/openapi.rs` reads this crate's
    // source for `.route("…")` literals. The two are asserted equal below.
    Router::new().route("/_twalk/hermes/freebusy", get(read_free_busy))
}

async fn read_free_busy(
    State(gateway): State<Gateway>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    let Some(reads) = gateway.reads() else {
        return refuse(&gateway, ReadRefusal::SeamNotConfigured);
    };
    // The members are read off the same string the signature covers, by
    // hand: an extractor would refuse a malformed query in its own words,
    // before the signature was checked and without a record — and every
    // read is recorded, the malformed ones included.
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
    match reads.read(&request).await {
        Ok(busy) => (
            StatusCode::OK,
            Json(json!({
                "connection": request.connection,
                "from": request.from,
                "to": request.to,
                "busy": busy,
            })),
        )
            .into_response(),
        Err(refusal) => refuse(&gateway, refusal),
    }
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
    fn the_route_and_the_constant_are_one_path() {
        assert_eq!(
            crate::hermes_freebusy::FREEBUSY_PATH,
            "/_twalk/hermes/freebusy"
        );
    }
}
