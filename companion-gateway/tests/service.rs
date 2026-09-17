//! Ticket #48, the Companion Gateway's service skeleton: the binary is
//! configured by environment variables alone, serves the Companion's static
//! files from a configured directory on its own origin, answers a health
//! endpoint, exposes Prometheus metrics in the Sensor's conventions,
//! propagates a `traceparent` on inbound HTTP, logs at an env-configurable
//! level and shuts down cleanly on SIGTERM.
//!
//! The seam is the process boundary: every assertion below is an HTTP call
//! to the real binary, or an observation of its process (logs, exit status).

mod harness;

use anyhow::Result;
use harness::{
    assert_valid_traceparent, gateway_env, gateway_env_with, missing_static_dir, parse_exposition,
    poll_until, static_dir_with_index, GatewayProc, INDEX_HTML,
};

const METRIC_PREFIX: &str = "twalk_companion_gateway_";

/// The Gateway under test with a static directory of its own, ready to
/// answer: returns the process and its origin.
async fn start(test_name: &str) -> Result<(GatewayProc, String)> {
    let static_dir = static_dir_with_index(test_name)?;
    let gateway = GatewayProc::start(&gateway_env(&static_dir))?;
    let base = gateway.base_url().await?;
    Ok((gateway, base))
}

#[tokio::test]
async fn the_health_endpoint_answers_as_soon_as_the_gateway_listens() -> Result<()> {
    let (gateway, base) = start("health").await?;

    let body = poll_until(
        || async {
            let response = reqwest::get(format!("{base}/health")).await.ok()?;
            (response.status() == reqwest::StatusCode::OK)
                .then_some(response)?
                .text()
                .await
                .ok()
        },
        "the gateway health endpoint",
    )
    .await?;
    let health: serde_json::Value = serde_json::from_str(&body)?;
    assert_eq!(
        health["status"].as_str(),
        Some("ok"),
        "the health document reports the service state: {body}"
    );
    // The version handshake's server half: the Companion, installed as a PWA,
    // compares this with the version baked into its own build and reloads
    // when a service worker has left it holding a stale app shell.
    let version = health["version"]
        .as_str()
        .unwrap_or_else(|| panic!("the health document names the Gateway's version: {body}"));
    assert!(!version.is_empty(), "the version is non-empty: {body}");
    let revision = health["revision"]
        .as_str()
        .unwrap_or_else(|| panic!("the health document names the build's revision: {body}"));
    assert!(!revision.is_empty(), "the revision is non-empty: {body}");

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn the_metrics_endpoint_exposes_the_gateway_counters() -> Result<()> {
    let (gateway, base) = start("metrics").await?;

    // One health scrape, so the request counter has something to report.
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

    let body = poll_until(
        || async {
            let body = reqwest::get(format!("{base}/metrics"))
                .await
                .ok()?
                .text()
                .await
                .ok()?;
            parse_exposition(&body)
                .iter()
                .any(|(name, value)| {
                    name == "twalk_companion_gateway_http_requests_total{route=\"health\",status=\"200\"}"
                        && *value >= 1
                })
                .then_some(body)
        },
        "the health request to appear on the metrics endpoint",
    )
    .await?;

    let samples = parse_exposition(&body);
    assert!(
        samples
            .iter()
            .any(|(name, _)| name == "twalk_companion_gateway_uptime_seconds"),
        "the exposition must carry the uptime gauge:\n{body}"
    );
    // Every sample line is `name[labels] value` with an integer value, under
    // one namespace — the Sensor's exposition conventions.
    for (name, _) in &samples {
        assert!(name.starts_with(METRIC_PREFIX), "{name}");
    }
    assert!(
        body.contains("# TYPE twalk_companion_gateway_http_requests_total counter"),
        "every metric is declared with its HELP and TYPE:\n{body}"
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn the_companion_is_served_from_the_configured_directory() -> Result<()> {
    let (gateway, base) = start("static").await?;

    let index = poll_until(
        || async {
            let response = reqwest::get(format!("{base}/")).await.ok()?;
            (response.status() == reqwest::StatusCode::OK)
                .then_some(response)?
                .text()
                .await
                .ok()
        },
        "the Companion index at the origin root",
    )
    .await?;
    assert_eq!(index, INDEX_HTML, "the root serves index.html");

    let named = reqwest::get(format!("{base}/index.html")).await?;
    assert_eq!(named.status(), reqwest::StatusCode::OK);
    assert_eq!(named.text().await?, INDEX_HTML);

    let asset = reqwest::get(format!("{base}/app.css")).await?;
    assert_eq!(asset.status(), reqwest::StatusCode::OK);
    assert_eq!(
        asset
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.starts_with("text/css")),
        Some(true),
        "static files are served with their content type"
    );

    // Path traversal: the configured directory is the whole of what the
    // origin exposes. The escape attempt is an unknown path like any other,
    // so it gets the app shell — never a file from outside the directory.
    let escape = reqwest::get(format!("{base}/%2e%2e%2f%2e%2e%2fetc%2fpasswd")).await?;
    let escaped_body = escape.text().await?;
    assert!(
        !escaped_body.contains("root:"),
        "the Gateway must not serve anything outside its static directory: {escaped_body}"
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn an_unknown_app_path_loads_the_companion_and_an_unknown_api_path_is_a_json_404(
) -> Result<()> {
    let (gateway, base) = start("client-routing").await?;
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

    // The Companion is a static export with client-side routing: a deep link
    // reloaded cold must load the app, not a 404.
    let deep_link = reqwest::get(format!("{base}/onboarding/whatsapp")).await?;
    assert_eq!(
        deep_link.status(),
        reqwest::StatusCode::OK,
        "an unknown app path serves the Companion's shell"
    );
    assert_eq!(
        deep_link
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.starts_with("text/html")),
        Some(true)
    );
    assert_eq!(deep_link.text().await?, INDEX_HTML);

    // The Gateway's own API surface keeps its status codes: a client parsing
    // an API response must never be handed an HTML page instead.
    let api = reqwest::get(format!("{base}/api/consent")).await?;
    assert_eq!(
        api.status(),
        reqwest::StatusCode::NOT_FOUND,
        "an unknown API path is a 404, not the app shell"
    );
    assert_eq!(
        api.headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.starts_with("application/json")),
        Some(true),
        "an API error is JSON the Companion can parse"
    );
    let body: serde_json::Value = serde_json::from_str(&api.text().await?)?;
    assert_eq!(body["error"].as_str(), Some("not_found"));
    assert_eq!(body["path"].as_str(), Some("/api/consent"));

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn an_inbound_traceparent_is_propagated_and_one_is_originated_otherwise() -> Result<()> {
    let (gateway, base) = start("traceparent").await?;
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

    let client = reqwest::Client::new();
    let inbound = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    let response = client
        .get(format!("{base}/health"))
        .header("traceparent", inbound)
        .send()
        .await?;
    assert_eq!(
        response
            .headers()
            .get("traceparent")
            .and_then(|value| value.to_str().ok()),
        Some(inbound),
        "the caller's trace context is continued, not replaced"
    );

    let originated = reqwest::get(format!("{base}/health")).await?;
    let traceparent = originated
        .headers()
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .expect("a request without a trace context gets one")
        .to_owned();
    assert_valid_traceparent(&traceparent);

    // A garbage header is replaced by a valid one rather than propagated.
    let garbage = client
        .get(format!("{base}/health"))
        .header("traceparent", "not-a-traceparent")
        .send()
        .await?;
    let replaced = garbage
        .headers()
        .get("traceparent")
        .and_then(|value| value.to_str().ok())
        .expect("an invalid trace context is replaced")
        .to_owned();
    assert_valid_traceparent(&replaced);
    assert_ne!(replaced, "not-a-traceparent");

    // The trace context reaches the structured logs, so an operator can join
    // a Gateway request to the trace it belongs to.
    let logged = poll_until(
        || async {
            gateway
                .logs()
                .await
                .iter()
                .any(|line| line.contains(inbound))
                .then_some(())
        },
        "the inbound traceparent in the request log",
    )
    .await;
    assert!(logged.is_ok(), "the request log carries the traceparent");

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn sigterm_shuts_the_gateway_down_cleanly() -> Result<()> {
    let (gateway, base) = start("sigterm").await?;
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

    let status = gateway.terminate().await?;
    assert_eq!(
        status.code(),
        Some(0),
        "SIGTERM must shut the Gateway down with exit code 0, got {status}"
    );
    Ok(())
}

#[tokio::test]
async fn the_log_level_env_is_honored() -> Result<()> {
    let static_dir = static_dir_with_index("log-level")?;

    // At `error`, the Gateway's routine info-level startup lines stay silent
    // — the listen address among them, so this one test asks for a fixed
    // port (unique to this suite) instead of reading the address off the log.
    // Readiness marker: an answered health probe proves the listener is up.
    let quiet = GatewayProc::start(&gateway_env_with(
        &static_dir,
        &[
            ("GATEWAY_LOG_LEVEL", "error"),
            ("GATEWAY_LISTEN", "127.0.0.1:19483"),
        ],
    ))?;
    poll_until(
        || async {
            reqwest::get("http://127.0.0.1:19483/health")
                .await
                .ok()?
                .error_for_status()
                .ok()
        },
        "the quiet gateway's health endpoint",
    )
    .await?;
    // Give the log capture a beat to drain, then assert the silence.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let lines = quiet.logs().await;
    assert!(
        lines
            .iter()
            .all(|line| !line.contains("companion gateway starting")),
        "GATEWAY_LOG_LEVEL=error must suppress the info-level startup lines:\n{}",
        lines.join("\n")
    );
    quiet.stop().await;

    // With the harness default (info), the same lines show.
    let loud = GatewayProc::start(&gateway_env(&static_dir))?;
    poll_until(
        || async {
            loud.logs()
                .await
                .iter()
                .any(|line| line.contains("companion gateway starting"))
                .then_some(())
        },
        "the info-level startup lines",
    )
    .await?;
    loud.stop().await;
    Ok(())
}

#[tokio::test]
async fn an_unset_static_directory_variable_fails_loudly() -> Result<()> {
    let static_dir = static_dir_with_index("misconfigured")?;

    // No GATEWAY_STATIC_DIR at all is a configuration error: the Gateway
    // refuses to start and names the variable to fix, rather than guessing a
    // path.
    let mut unset = GatewayProc::start(&gateway_env_with(
        &static_dir,
        &[("GATEWAY_STATIC_DIR", "")],
    ))?;
    let status = unset.wait_for_exit().await?;
    assert_ne!(
        status.code(),
        Some(0),
        "a Gateway without GATEWAY_STATIC_DIR must exit non-zero"
    );
    let logs = unset.logs().await;
    assert!(
        logs.iter().any(|line| line.contains("GATEWAY_STATIC_DIR")),
        "the error must name GATEWAY_STATIC_DIR:\n{}",
        logs.join("\n")
    );
    Ok(())
}

#[tokio::test]
async fn an_absent_companion_build_does_not_take_the_service_down() -> Result<()> {
    // The Companion's build lands in its own lot, and an operator can point
    // the Gateway at an empty volume: the origin then answers a clear 404
    // while health and metrics — what the operator debugs with — stay up.
    let absent = missing_static_dir("no-build");
    let gateway = GatewayProc::start(&gateway_env_with(
        &static_dir_with_index("no-build")?,
        &[("GATEWAY_STATIC_DIR", &absent.to_string_lossy())],
    ))?;
    let base = gateway.base_url().await?;

    let health = poll_until(
        || async {
            let response = reqwest::get(format!("{base}/health")).await.ok()?;
            (response.status() == reqwest::StatusCode::OK).then_some(())
        },
        "the health endpoint of a Gateway with no Companion build",
    )
    .await;
    assert!(health.is_ok(), "health stays up without a Companion build");

    let metrics = reqwest::get(format!("{base}/metrics")).await?;
    assert_eq!(metrics.status(), reqwest::StatusCode::OK);

    let index = reqwest::get(format!("{base}/")).await?;
    assert_eq!(
        index.status(),
        reqwest::StatusCode::NOT_FOUND,
        "with no build to serve, the Companion's origin is a plain 404"
    );
    let explanation = index.text().await?;
    assert!(
        explanation.contains("GATEWAY_STATIC_DIR"),
        "the 404 says what to fix: {explanation}"
    );

    // And the operator was warned at startup.
    let warned = poll_until(
        || async {
            gateway
                .logs()
                .await
                .iter()
                .any(|line| line.contains("no index.html in the static directory"))
                .then_some(())
        },
        "the startup warning about the absent build",
    )
    .await;
    assert!(warned.is_ok(), "startup warns about the absent build");

    gateway.stop().await;
    Ok(())
}
