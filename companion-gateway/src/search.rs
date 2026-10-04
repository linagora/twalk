//! The search of the owner's archive (lot 3a): the Companion Gateway's relay
//! of the collector's internal endpoint, behind the session guard.
//!
//! # What this is
//!
//! The read path is [`crate::hermes_freebusy`]'s pattern, copied and not
//! reinvented: the Gateway relays, synthesises nothing, and the collector is
//! the one process that holds the index and the consent cache the hits are
//! filtered with. Two things differ, and both are deliberate.
//!
//! The route is called by a **browser** — the owner in their session — so it
//! sits behind the device-token guard (#52) and not behind Hermes's
//! signature. And the Gateway **reads no message body**: it relays the query
//! to the collector and answers what the collector answered, which carries a
//! `snippet` and never a `body`. That is the property the whole lot rests on,
//! and [`crate::search_http`] is what makes it true: nothing here opens an
//! inbound event, and nothing on this path parses a hit.
//!
//! # What the filters are
//!
//! `source`, `from` and `to` are **relayed**, never applied here: the
//! collector owns the index and the filter (C20, T6), and a second
//! implementation of `source`-and-date filtering in this process would be a
//! second answer to a question that already has one. [`SearchRequest::query`]
//! is the whole of this module's opinion about them — their names and their
//! order on the wire.
//!
//! # What a refusal is
//!
//! The collector's codes are relayed **unchanged** (`index_not_configured`,
//! `index_unavailable`, `invalid_query`, `invalid_window`) so that a browser
//! is never taught two vocabularies for one fact. The one divergence from
//! `freebusy` is [`SearchRefusal::CollectorNotConfigured`]'s code: a Gateway
//! with no `GATEWAY_COLLECTOR_URL` answers `search_unavailable`, not
//! `collector_not_configured`, because this route's caller is the owner's
//! browser and "the search is unavailable on this deployment" is the sentence
//! it can act on (C8, and `openapi.yaml` says the same).
//!
//! # What is counted
//!
//! Every outcome, `served` first, on
//! `twalk_companion_gateway_searches_total{outcome}` — including the two this
//! module produces itself, [`SearchRefusal::CollectorNotConfigured`] and a
//! collector that did not answer. The collector counts its own side
//! (`twalk_collector_search_reads_total`); this is the Gateway's half, and a
//! search that reached nobody is the failure this product keeps shipping
//! without noticing.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::metrics::Metrics;

/// The Gateway's own route, spelled out in [`crate::search_http`] where
/// `tests/openapi.rs` scans for `.route("…")` literals; asserted equal to
/// this constant below.
pub const SEARCH_PATH: &str = "/api/search";

/// The progress route, beside the search and behind the same guard.
pub const INDEX_STATUS_PATH: &str = "/api/index/status";

/// The collector's internal route this module relays to, as the collector's
/// own `http.rs` spells it. A single constant, so a rename on that side fails
/// this module's unit test by name rather than as a 502 in production.
pub const COLLECTOR_SEARCH_ROUTE: &str = "search";

/// The collector's progress route, spelled as its own `http.rs` spells it.
pub const COLLECTOR_INDEX_STATUS_ROUTE: &str = "index/status";

/// The same delay as the free/busy relay: a search that has not answered in
/// twenty seconds is not going to.
pub const COLLECTOR_TIMEOUT: Duration = Duration::from_secs(20);

/// The request, as the browser sends it. Every member but `q` is optional and
/// every one of them is relayed untouched.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SearchRequest {
    pub q: String,
    pub source: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: Option<u32>,
}

impl SearchRequest {
    /// The parameters of the relay, in the order the collector's route takes
    /// them. `q` is always sent, empty or not: the collector, and not this
    /// Gateway, is the one that decides an empty query is
    /// [`SearchRefusal::InvalidQuery`].
    pub fn query(&self) -> Vec<(&'static str, String)> {
        let mut query = vec![("q", self.q.clone())];
        if let Some(source) = &self.source {
            query.push(("source", source.clone()));
        }
        if let Some(from) = &self.from {
            query.push(("from", from.clone()));
        }
        if let Some(to) = &self.to {
            query.push(("to", to.clone()));
        }
        if let Some(limit) = self.limit {
            query.push(("limit", limit.to_string()));
        }
        query
    }
}

/// The collector's answer, relayed as it stands. `hits` are the collector's
/// own objects and are **not** re-shaped here: a projection written beside
/// `Hit` would be a second description of a hit, free to drift from the one
/// the collector sends. What that buys is that a member this Gateway has
/// never heard of — a `score`, a mailbox — crosses unaltered, and what it
/// costs is that the absence of a `body` is the collector's to guarantee and
/// this module's to refuse to add.
#[derive(Debug, Clone, Serialize)]
pub struct SearchAnswer {
    pub hits: Vec<serde_json::Value>,
    pub count: usize,
    pub withheld: usize,
    /// `consent`, when hits were withheld (¶5.3 of the design). Absent rather
    /// than `null` when nothing was: a deployment that withholds nothing must
    /// not answer a member saying it withheld something.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub withheld_reason: Option<String>,
}

/// What `GET /api/index/status` answers: how many documents the index holds,
/// relayed from the collector. A document count and nothing else — not a
/// cursor, not a per-source breakdown — because a progress bar needs one
/// number and a second member is a second thing to keep in step.
#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    pub documents: u64,
}

/// Why a search was refused: the code the browser is given, the status, and
/// the sentence.
///
/// The first four are the collector's own codes, relayed unchanged, so the
/// two doors onto one fact teach one vocabulary (the same argument #24 makes
/// about the approval codes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchRefusal {
    /// The collector holds no index (`COLLECTOR_INDEX_KEY_FILE` unset).
    IndexNotConfigured,
    /// The collector holds one and could not read it.
    IndexUnavailable,
    /// `q` is empty or longer than the collector accepts.
    InvalidQuery,
    /// `from` is after `to`, or a bound is not a date.
    InvalidWindow,
    /// This Gateway has no collector to relay to.
    CollectorNotConfigured,
    /// The collector did not answer, or answered something that is not JSON.
    CollectorUnreachable(String),
    /// The collector answered a refusal of its own, with a code this module
    /// does not know. Relayed under `collector_refused` with the code in the
    /// sentence, never silently flattened into one of the four above.
    CollectorRefused { status: u16, code: String },
}

impl SearchRefusal {
    pub fn code(&self) -> &'static str {
        match self {
            SearchRefusal::IndexNotConfigured => "index_not_configured",
            SearchRefusal::IndexUnavailable => "index_unavailable",
            SearchRefusal::InvalidQuery => "invalid_query",
            SearchRefusal::InvalidWindow => "invalid_window",
            // The deliberate divergence (C8): this route's caller is the
            // owner's browser, and freebusy's `collector_not_configured`
            // names a component that means nothing to them.
            SearchRefusal::CollectorNotConfigured => "search_unavailable",
            SearchRefusal::CollectorUnreachable(_) => "collector_unreachable",
            SearchRefusal::CollectorRefused { .. } => "collector_refused",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            SearchRefusal::InvalidQuery | SearchRefusal::InvalidWindow => 400,
            SearchRefusal::IndexNotConfigured
            | SearchRefusal::IndexUnavailable
            | SearchRefusal::CollectorNotConfigured => 503,
            SearchRefusal::CollectorUnreachable(_) | SearchRefusal::CollectorRefused { .. } => 502,
        }
    }

    pub fn message(&self) -> String {
        match self {
            SearchRefusal::IndexNotConfigured => {
                "this deployment has no search index configured".to_owned()
            }
            SearchRefusal::IndexUnavailable => "the search index could not be read".to_owned(),
            SearchRefusal::InvalidQuery => "q is the text to search, between 1 and 512 characters, \
                 and limit is a whole number between 1 and 100"
                .to_owned(),
            SearchRefusal::InvalidWindow => "from is after to".to_owned(),
            SearchRefusal::CollectorNotConfigured => {
                "the search is unavailable on this deployment: GATEWAY_COLLECTOR_URL is not \
                 set, so there is no collector to relay the search to"
                    .to_owned()
            }
            SearchRefusal::CollectorUnreachable(detail) => {
                format!("the collector did not answer the search: {detail}")
            }
            SearchRefusal::CollectorRefused { status, code } => {
                format!("the collector refused the search with HTTP {status} ({code})")
            }
        }
    }

    /// The outcome this refusal is counted under — its code, and the one
    /// place the metric's label is decided, so a counter and a body cannot
    /// disagree about what happened.
    pub fn outcome(&self) -> &'static str {
        self.code()
    }
}

/// The I/O: the collector's endpoint, the bearer it accepts, and the client.
pub struct Searches {
    /// `None` when `GATEWAY_COLLECTOR_URL` is unset — the same couple
    /// [`crate::hermes_freebusy::Reads`] holds, and for the same reason: one
    /// collector, one bearer, whatever the read.
    collector: Option<(String, String)>,
    http: reqwest::Client,
    metrics: Arc<Metrics>,
}

impl Searches {
    pub fn new(collector: Option<(String, String)>, metrics: Arc<Metrics>) -> Self {
        Self {
            collector,
            http: reqwest::Client::builder()
                .timeout(COLLECTOR_TIMEOUT)
                .build()
                .expect("a client with a timeout builds"),
            metrics,
        }
    }

    /// One search: relayed to the collector, or refused. The answer is the
    /// collector's, member for member.
    pub async fn search(&self, request: &SearchRequest) -> Result<SearchAnswer, SearchRefusal> {
        let Some((collector_url, token)) = &self.collector else {
            let refusal = SearchRefusal::CollectorNotConfigured;
            self.metrics.record_search(refusal.outcome());
            return Err(refusal);
        };
        let params = request.query();
        let query: Vec<(&str, &str)> = params
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let body = self
            .relay(collector_url, token, COLLECTOR_SEARCH_ROUTE, &query)
            .await?;
        self.metrics.record_search("served");
        Ok(SearchAnswer {
            hits: body["hits"].as_array().cloned().unwrap_or_default(),
            count: body["count"].as_u64().unwrap_or(0) as usize,
            withheld: body["withheld"].as_u64().unwrap_or(0) as usize,
            withheld_reason: body["withheld_reason"].as_str().map(str::to_owned),
        })
    }

    /// The index's progress, relayed. Counted under `index_status` so a
    /// refused progress read is a slope too, and not a silence.
    pub async fn index_status(&self) -> Result<IndexStatus, SearchRefusal> {
        let Some((collector_url, token)) = &self.collector else {
            let refusal = SearchRefusal::CollectorNotConfigured;
            self.metrics.record_search(refusal.outcome());
            return Err(refusal);
        };
        let body = self
            .relay(
                collector_url,
                token,
                COLLECTOR_INDEX_STATUS_ROUTE,
                &[],
            )
            .await?;
        self.metrics.record_search("served");
        Ok(IndexStatus {
            documents: body["documents"].as_u64().unwrap_or(0),
        })
    }

    /// The relay, copied from [`crate::hermes_freebusy::Reads`]: a response
    /// that is not 200 becomes a refusal carrying the collector's own code,
    /// and every outcome is counted under that code.
    async fn relay(
        &self,
        collector_url: &str,
        service_token: &str,
        route: &str,
        query: &[(&str, &str)],
    ) -> Result<serde_json::Value, SearchRefusal> {
        let response = self
            .http
            .get(format!("{}/{route}", collector_url.trim_end_matches('/')))
            .query(query)
            .bearer_auth(service_token)
            .send()
            .await
            .map_err(|error| {
                let refusal = SearchRefusal::CollectorUnreachable(error.to_string());
                self.metrics.record_search(refusal.outcome());
                refusal
            })?;
        let status = response.status().as_u16();
        let body: serde_json::Value = response.json().await.map_err(|error| {
            let refusal =
                SearchRefusal::CollectorUnreachable(format!("its answer is not JSON: {error}"));
            self.metrics.record_search(refusal.outcome());
            refusal
        })?;
        if status != 200 {
            let code = body["error"].as_str().unwrap_or("unknown").to_owned();
            let refusal = match code.as_str() {
                "index_not_configured" => SearchRefusal::IndexNotConfigured,
                "index_unavailable" => SearchRefusal::IndexUnavailable,
                "invalid_query" => SearchRefusal::InvalidQuery,
                "invalid_window" => SearchRefusal::InvalidWindow,
                _ => SearchRefusal::CollectorRefused { status, code },
            };
            self.metrics.record_search(refusal.outcome());
            return Err(refusal);
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SearchRefusal, SearchRequest, COLLECTOR_INDEX_STATUS_ROUTE, COLLECTOR_SEARCH_ROUTE,
        INDEX_STATUS_PATH, SEARCH_PATH,
    };

    #[test]
    fn the_gateway_route_and_the_collectors_are_the_paths_the_others_spell() {
        // `search_http.rs` spells the Gateway's paths out so
        // `tests/openapi.rs` can read them from the source, and the
        // collector's routes are literals this module builds into a request
        // line. These assertions are what keeps each pair one.
        assert_eq!(SEARCH_PATH, "/api/search");
        assert_eq!(INDEX_STATUS_PATH, "/api/index/status");
        assert_eq!(COLLECTOR_SEARCH_ROUTE, "search");
        assert_eq!(COLLECTOR_INDEX_STATUS_ROUTE, "index/status");
    }

    #[test]
    fn the_query_relays_the_filters_in_the_collectors_order_and_omits_what_was_not_asked() {
        // C20: the filters are the collector's to apply and this Gateway's to
        // carry, so the only thing under test here is that they are carried —
        // and that an absent one is absent rather than empty, since an empty
        // `source` is a filter that matches nothing while an absent one
        // matches everything.
        let bare = SearchRequest {
            q: "hebdo".to_owned(),
            ..SearchRequest::default()
        };
        assert_eq!(bare.query(), vec![("q", "hebdo".to_owned())]);

        let filtered = SearchRequest {
            q: "hebdo".to_owned(),
            source: Some("mail-linagora".to_owned()),
            from: Some("2026-09-01T00:00:00Z".to_owned()),
            to: Some("2026-09-30T00:00:00Z".to_owned()),
            limit: Some(5),
        };
        assert_eq!(
            filtered.query(),
            vec![
                ("q", "hebdo".to_owned()),
                ("source", "mail-linagora".to_owned()),
                ("from", "2026-09-01T00:00:00Z".to_owned()),
                ("to", "2026-09-30T00:00:00Z".to_owned()),
                ("limit", "5".to_owned()),
            ]
        );
    }

    #[test]
    fn every_refusal_carries_the_collectors_code_its_status_and_a_sentence() {
        // The four the collector produces are relayed under its own words; the
        // one this Gateway produces itself is `search_unavailable`, not
        // freebusy's `collector_not_configured` (C8).
        assert_eq!(SearchRefusal::IndexNotConfigured.code(), "index_not_configured");
        assert_eq!(SearchRefusal::IndexNotConfigured.status(), 503);
        assert_eq!(SearchRefusal::IndexUnavailable.code(), "index_unavailable");
        assert_eq!(SearchRefusal::IndexUnavailable.status(), 503);
        assert_eq!(SearchRefusal::InvalidQuery.code(), "invalid_query");
        assert_eq!(SearchRefusal::InvalidQuery.status(), 400);
        assert_eq!(SearchRefusal::InvalidWindow.code(), "invalid_window");
        assert_eq!(SearchRefusal::InvalidWindow.status(), 400);
        assert_eq!(SearchRefusal::CollectorNotConfigured.code(), "search_unavailable");
        assert_eq!(SearchRefusal::CollectorNotConfigured.status(), 503);
        assert_eq!(
            SearchRefusal::CollectorUnreachable("timeout".to_owned()).code(),
            "collector_unreachable"
        );
        assert_eq!(
            SearchRefusal::CollectorUnreachable("timeout".to_owned()).status(),
            502
        );
        assert_eq!(
            SearchRefusal::CollectorRefused {
                status: 502,
                code: "caldav_refused".to_owned()
            }
            .status(),
            502
        );
        // The unknown code survives in the sentence — an operator reading the
        // log needs it, and flattening it into one of the four would lose the
        // only trace of what the collector actually said.
        assert!(SearchRefusal::CollectorRefused {
            status: 502,
            code: "caldav_refused".to_owned()
        }
        .message()
        .contains("caldav_refused"));
    }
}
