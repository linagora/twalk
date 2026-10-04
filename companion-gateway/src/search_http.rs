//! `GET /api/search` — the search of the owner's archive (lot 3a). Under
//! `/api/`, so behind the session guard (device token, ticket #52): this is a
//! capability of the owner in their session, not a tool an agent holds. The
//! route relays to the collector, which holds the index and applies the
//! consent filter, and answers what the collector answered.
//!
//! The Gateway **reads no message body** here, and this file is where that is
//! true: the query string is read for the five members the collector's route
//! takes, the answer is relayed as [`crate::search::SearchAnswer`] — `hits`,
//! `count`, `withheld`, `withheld_reason` — and nothing on this path opens an
//! inbound event or parses a hit. The `snippet` in a hit is the collector's,
//! and a `body` never crosses because the Gateway never adds one.

use axum::extract::{RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::http::Gateway;
use crate::search::{SearchRequest, SearchRefusal};

pub fn routes() -> Router<Gateway> {
    // Written out, not `SEARCH_PATH`: `tests/openapi.rs` reads this crate's
    // source for `.route("…")` literals. The two are asserted equal below.
    Router::new()
        .route("/api/search", get(search))
        .route("/api/index/status", get(index_status))
}

async fn search(
    State(gateway): State<Gateway>,
    RawQuery(query): RawQuery,
) -> Response {
    let Some(searches) = gateway.searches() else {
        return refuse(SearchRefusal::CollectorNotConfigured);
    };
    let request = parse_query(query.as_deref().unwrap_or_default());
    match searches.search(&request).await {
        Ok(answer) => (StatusCode::OK, Json(answer)).into_response(),
        Err(refusal) => refuse(refusal),
    }
}

/// `GET /api/index/status` — how many documents the index holds, for the
/// progress bar (#XXX, lot 3a). Relayed the same way, behind the same guard;
/// the collector's refusal is passed through unchanged, so a deployment with
/// no index says `index_not_configured` on both routes with one word.
async fn index_status(State(gateway): State<Gateway>) -> Response {
    let Some(searches) = gateway.searches() else {
        return refuse(SearchRefusal::CollectorNotConfigured);
    };
    match searches.index_status().await {
        Ok(answer) => (StatusCode::OK, Json(answer)).into_response(),
        Err(refusal) => refuse(refusal),
    }
}

/// The members, read by hand from the query string — not an extractor, for
/// the reason the free/busy read does it by hand: an extractor would refuse
/// a malformed query in its own words, and this route's refusals are the
/// collector's, carried unchanged.
fn parse_query(query: &str) -> SearchRequest {
    let mut request = SearchRequest::default();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = crate::contacts_http::percent_decode(value);
        match name {
            "q" => request.q = value,
            "source" => request.source = Some(value),
            "from" => request.from = Some(value),
            "to" => request.to = Some(value),
            "limit" => request.limit = value.parse().ok(),
            _ => {}
        }
    }
    request
}

/// One refusal, in `openapi.yaml`'s `Error` shape. `search_unavailable` is
/// this Gateway's own code (C8), deliberately not freebusy's
/// `collector_not_configured`: this route's caller is the owner's browser.
fn refuse(refusal: SearchRefusal) -> Response {
    (
        StatusCode::from_u16(refusal.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(json!({ "error": refusal.code(), "detail": refusal.message() })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_routes_and_the_constants_are_the_same_paths() {
        // The router spells them out so `tests/openapi.rs` can read them out
        // of this source; these assertions keep the two spellings one.
        assert_eq!(crate::search::SEARCH_PATH, "/api/search");
        assert_eq!(crate::search::INDEX_STATUS_PATH, "/api/index/status");
    }

    #[test]
    fn the_members_are_read_off_the_query_string() {
        let request = parse_query("q=hebdo&source=mail-linagora&limit=5");
        assert_eq!(request.q, "hebdo");
        assert_eq!(request.source.as_deref(), Some("mail-linagora"));
        assert_eq!(request.limit, Some(5));
        // An absent member is `None`, never an empty string: an empty
        // `source` is a filter that matches nothing.
        assert_eq!(request.from, None);
        assert_eq!(request.to, None);
    }

    #[test]
    fn a_member_is_percent_decoded_before_it_is_relayed() {
        // A date carries `:` and a query carries spaces; both arrive encoded
        // and the collector must see the values the browser meant.
        let request = parse_query("q=point%20hebdo&from=2026-09-01T00%3A00%3A00Z");
        assert_eq!(request.q, "point hebdo");
        assert_eq!(request.from.as_deref(), Some("2026-09-01T00:00:00Z"));
    }
}
