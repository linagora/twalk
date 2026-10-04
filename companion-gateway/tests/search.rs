//! The owner's search of their archive (lot 3a), at the Gateway's process
//! boundary: the real binary, a stub standing in for the collector, and
//! device-token reads from the test standing in for the Companion.
//!
//! In order:
//!
//! 1. a signed-in device's `GET /api/search` relays to the collector's
//!    `/search` route and answers what the collector answered — and the
//!    property the whole lot rests on holds: **no `body` crosses**, only the
//!    `snippet` the collector sent;
//! 2. the filters the browser sends (`source`, `from`, `to`) are **relayed**
//!    to the collector, not applied here (C20): the stub's request line holds
//!    them;
//! 3. the collector's refusals are relayed with their own code and status —
//!    `index_not_configured` is a `503`, `invalid_query` a `400` — and a
//!    collector that does not answer is a `502`, never a `503` that would say
//!    the deployment has no collector;
//! 4. a Gateway with no `GATEWAY_COLLECTOR_URL` answers `503
//!    search_unavailable` — this Gateway's own code (C8), not freebusy's
//!    `collector_not_configured`;
//! 5. `/api/index/status` relays the collector's document count the same way;
//! 6. every outcome is counted on `twalk_companion_gateway_searches_total`.
//!
//! What a request with no device token is answered with is `tests/openapi.rs`'s,
//! since that is reached without a session; the guard table itself is
//! `session_http`'s own test.

mod harness;

use anyhow::Result;
use harness::{
    companion_build, ensure_stack, gateway_env_with, nats_url, poll_until, signed_in_device_token,
    GatewayProc, StubCollector, HERMES_ANSWER_SECRET, HERMES_DOMAIN, TEST_CONNECTIONS,
};
use serde_json::{json, Value};

fn unique(label: &str) -> String {
    format!(
        "{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos()
    )
}

/// A Gateway with the collector's URL (or none) and a signed-in device.
async fn gateway(test_name: &str, collector_url: Option<&str>) -> Result<(GatewayProc, String)> {
    let static_dir = companion_build(test_name)?;
    let mut overrides = vec![
        ("GATEWAY_NATS_URL", nats_url()),
        ("GATEWAY_CONNECTIONS", TEST_CONNECTIONS.to_owned()),
        // The collector's URL lives on the Hermes seam's configuration
        // (`GATEWAY_COLLECTOR_URL` beside `GATEWAY_HERMES_ANSWER_SECRET`), the
        // same couple the free/busy reads hold — so a search test configures
        // the seam even though it never signs anything with it.
        ("GATEWAY_HERMES_ANSWER_SECRET", HERMES_ANSWER_SECRET.to_owned()),
        ("GATEWAY_HERMES_DOMAIN", HERMES_DOMAIN.to_owned()),
    ];
    if let Some(url) = collector_url {
        overrides.push(("GATEWAY_COLLECTOR_URL", url.to_owned()));
    }
    let borrowed: Vec<(&str, &str)> = overrides
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    let gateway = GatewayProc::start(&gateway_env_with(&static_dir, &borrowed))?;
    let base = gateway.base_url().await?;
    poll_until(
        || async {
            reqwest::get(format!("{base}/health"))
                .await
                .ok()?
                .error_for_status()
                .ok()
        },
        "the gateway health endpoint",
    )
    .await?;
    Ok((gateway, base))
}

/// One device-token GET, with the cookie the owner's sign-in set.
async fn device_get(base: &str, token: &str, path: &str) -> Result<(u16, Value)> {
    let response = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .header("cookie", format!("twalk_device={token}"))
        .send()
        .await?;
    let status = response.status().as_u16();
    let body = response.json().await.unwrap_or(Value::Null);
    Ok((status, body))
}

async fn metric(base: &str, outcome: &str) -> Result<u64> {
    let text = reqwest::get(format!("{base}/metrics"))
        .await?
        .text()
        .await?;
    let needle = format!("twalk_companion_gateway_searches_total{{outcome=\"{outcome}\"}} ");
    Ok(text
        .lines()
        .find_map(|line| line.strip_prefix(&needle))
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0))
}

/// The relay, and the property that matters: a search answers the collector's
/// hits and never a message body.
#[tokio::test]
async fn search_relays_and_never_carries_a_body() -> Result<()> {
    ensure_stack().await?;
    let collector = StubCollector::start_search(json!({
        "hits": [{
            "id": "doc-1",
            "source": "mail-linagora",
            "correspondent": "mailto:alice@example.org",
            "mailbox": "inbox",
            "date": 1_756_720_800_i64,
            "subject": "Point hebdo",
            "snippet": "Le point de la semaine…",
        }],
        "count": 1,
        "withheld": 0,
    }))
    .await?;
    let (gateway, base) = gateway("search-served", Some(&collector.url())).await?;
    let token = signed_in_device_token(&base).await?;

    let (status, answer) = device_get(&base, &token, "/api/search?q=hebdo").await?;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["count"], 1);
    assert_eq!(answer["hits"][0]["subject"], "Point hebdo");
    assert_eq!(answer["hits"][0]["snippet"], "Le point de la semaine…");
    // The whole argument of the lot, asserted on the bytes: the relay serves
    // a `snippet` and never a `body`, and the Gateway adds neither a body nor
    // a member the collector did not send.
    let serialized = answer.to_string();
    assert!(
        !serialized.contains("\"body\""),
        "the relay served a body: {serialized}"
    );

    // What the Gateway relayed: the collector's own route, and the search's
    // `q` — the stub keeps the request line, so this is the wire and not the
    // answer.
    let relayed = collector.requests();
    assert_eq!(relayed.len(), 1, "{relayed:?}");
    assert!(
        relayed[0].0.starts_with("GET /search?"),
        "the collector's route: {}",
        relayed[0].0
    );
    assert!(relayed[0].0.contains("q=hebdo"), "{}", relayed[0].0);

    assert_eq!(metric(&base, "served").await?, 1);
    gateway.stop().await;
    Ok(())
}

/// The filters the browser sends are relayed, not applied here (C20): the
/// stub's request line carries `source`, `from` and `to`, because the
/// collector is the one that filters and this Gateway is only the wire.
#[tokio::test]
async fn the_filters_are_relayed_to_the_collector_not_applied_here() -> Result<()> {
    ensure_stack().await?;
    let source = unique("mail");
    let collector = StubCollector::start_search(json!({
        "hits": [],
        "count": 0,
        "withheld": 0,
    }))
    .await?;
    let (gateway, base) = gateway("search-filters", Some(&collector.url())).await?;
    let token = signed_in_device_token(&base).await?;

    let path = format!(
        "/api/search?q=hebdo&source={source}&from=2026-09-01T00:00:00Z&to=2026-09-30T00:00:00Z"
    );
    let (status, answer) = device_get(&base, &token, &path).await?;
    assert_eq!(status, 200, "{answer}");

    let relayed = collector
        .requests()
        .into_iter()
        .find(|(line, _)| line.starts_with("GET /search?"))
        .expect("the Gateway relayed the search to the collector");
    // The percent-encoding of `:` and `+` is the client's; what the collector
    // must see is each value the browser meant, on its own name.
    assert!(
        relayed.0.contains(&format!("source={source}")),
        "source was not relayed: {}",
        relayed.0
    );
    assert!(
        relayed.0.contains("from=2026-09-01T00%3A00%3A00Z")
            || relayed.0.contains("from=2026-09-01T00:00:00Z"),
        "from was not relayed: {}",
        relayed.0
    );
    assert!(
        relayed.0.contains("to=2026-09-30T00%3A00%3A00Z")
            || relayed.0.contains("to=2026-09-30T00:00:00Z"),
        "to was not relayed: {}",
        relayed.0
    );
    // And the bearer is the Gateway's own service token, the same couple the
    // free/busy read holds.
    assert_eq!(
        relayed.1.as_deref(),
        Some(&*format!("Bearer {}", harness::SERVICE_TOKEN))
    );

    gateway.stop().await;
    Ok(())
}

/// The collector's refusals are relayed with their own code and status.
#[tokio::test]
async fn the_collectors_refusal_is_relayed_with_its_code() -> Result<()> {
    ensure_stack().await?;
    let collector = StubCollector::start_search_refusing(503, "index_not_configured").await?;
    let (gateway, base) = gateway("search-refused", Some(&collector.url())).await?;
    let token = signed_in_device_token(&base).await?;

    let (status, body) = device_get(&base, &token, "/api/search?q=x").await?;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["error"], "index_not_configured");
    assert_eq!(metric(&base, "index_not_configured").await?, 1);

    // A `400` of the collector's own — an empty or over-long query — is
    // relayed as a `400`, not flattened into a `503`.
    collector.answer(400, json!({ "error": "invalid_query" }));
    let (status, body) = device_get(&base, &token, "/api/search?q=").await?;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "invalid_query");
    assert_eq!(metric(&base, "invalid_query").await?, 1);

    gateway.stop().await;
    Ok(())
}

/// A collector that does not answer is a `502`, and a refusal of its own with
/// a code this Gateway does not know is relayed under `collector_refused` with
/// the code in the sentence.
#[tokio::test]
async fn an_unreachable_or_unknown_collector_is_a_502() -> Result<()> {
    ensure_stack().await?;
    let collector = StubCollector::start_search(json!({ "hits": [], "count": 0, "withheld": 0 }))
        .await?;
    let (gateway, base) = gateway("search-unreachable", Some(&collector.url())).await?;
    let token = signed_in_device_token(&base).await?;

    // A code of the collector's this Gateway does not map: the code travels.
    collector.answer(502, json!({ "error": "index_corrupt" }));
    let (status, body) = device_get(&base, &token, "/api/search?q=x").await?;
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"], "collector_refused");
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("index_corrupt"),
        "the collector's code travels: {body}"
    );

    // And a collector that stops answering at all.
    collector.stop();
    let body = poll_until(
        || async {
            let (status, body) = device_get(&base, &token, "/api/search?q=x").await.ok()?;
            (status == 502).then_some(body)
        },
        "the search to reach the collector and find it down",
    )
    .await?;
    assert_eq!(body["error"], "collector_unreachable");
    assert_eq!(metric(&base, "collector_unreachable").await?, 1);

    gateway.stop().await;
    Ok(())
}

/// A Gateway with no collector answers `search_unavailable` — its own code,
/// not freebusy's — and counts it.
#[tokio::test]
async fn a_gateway_with_no_collector_answers_search_unavailable() -> Result<()> {
    ensure_stack().await?;
    let (gateway, base) = gateway("search-no-collector", None).await?;
    let token = signed_in_device_token(&base).await?;

    let (status, body) = device_get(&base, &token, "/api/search?q=x").await?;
    assert_eq!(status, 503, "{body}");
    assert_eq!(
        body["error"], "search_unavailable",
        "this route's own code, not freebusy's collector_not_configured"
    );
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("GATEWAY_COLLECTOR_URL"),
        "the variable that would open it: {body}"
    );
    assert_eq!(metric(&base, "search_unavailable").await?, 1);

    // The progress route refuses the same way, with the same code: two doors
    // onto one fact teach one vocabulary.
    let (status, body) = device_get(&base, &token, "/api/index/status").await?;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["error"], "search_unavailable");

    gateway.stop().await;
    Ok(())
}

/// `GET /api/index/status` relays the collector's document count.
#[tokio::test]
async fn index_status_relays_the_document_count() -> Result<()> {
    ensure_stack().await?;
    let collector = StubCollector::answering(200, json!({ "documents": 42 })).await?;
    let (gateway, base) = gateway("index-status", Some(&collector.url())).await?;
    let token = signed_in_device_token(&base).await?;

    let (status, body) = device_get(&base, &token, "/api/index/status").await?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["documents"], 42);
    // The progress members go together or not at all: an answer with a count
    // and nothing else, since a second member is a second thing to keep in
    // step with the collector.
    assert_eq!(body.as_object().map(|object| object.len()), Some(1), "{body}");

    let relayed = collector.requests();
    assert_eq!(relayed.len(), 1, "{relayed:?}");
    assert!(
        relayed[0].0.starts_with("GET /index/status"),
        "the collector's route: {}",
        relayed[0].0
    );

    gateway.stop().await;
    Ok(())
}
