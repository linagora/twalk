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
    assert_valid_traceparent, companion_build, gateway_env, gateway_env_with, missing_static_dir,
    parse_exposition, poll_until, GatewayProc, FALLBACK_HTML, INDEX_HTML, SIGNAL_HTML, WASM,
    WASM_BROTLI, WHATSAPP_HTML,
};

const METRIC_PREFIX: &str = "twalk_companion_gateway_";

/// The Gateway under test with a Companion build of its own, ready to
/// answer: returns the process and its origin.
async fn start(test_name: &str) -> Result<(GatewayProc, String)> {
    let static_dir = companion_build(test_name)?;
    let gateway = GatewayProc::start(&gateway_env(&static_dir))?;
    let base = gateway.base_url().await?;
    Ok((gateway, base))
}

/// The response's `Content-Type`, whole — the tests assert exact values,
/// because for `.wasm` an extra parameter is a browser-side TypeError.
fn content_type(response: &reqwest::Response) -> Option<&str> {
    response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
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
    // The build of the Companion this origin ships, read from the export's own
    // `_app/version.json` (#222): the other half of a comparison the browser
    // makes with the build it runs, so a redeploy an open tab never picked up
    // is a named fact rather than five identical submissions.
    assert_eq!(
        health["companion_build"].as_str(),
        Some(harness::COMPANION_BUILD_ID),
        "the health document names the Companion build it serves: {body}"
    );

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
    assert_eq!(content_type(&asset), Some("text/css; charset=utf-8"));

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
async fn a_prerendered_page_is_resolved_the_way_the_static_export_lays_it_out() -> Result<()> {
    let (gateway, base) = start("prerendered").await?;
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
    // 307s are assertions here, not something to follow.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    // The build has onboarding/whatsapp.html: the path resolves through it.
    let page = client
        .get(format!("{base}/onboarding/whatsapp"))
        .send()
        .await?;
    assert_eq!(page.status(), reqwest::StatusCode::OK);
    assert_eq!(content_type(&page), Some("text/html; charset=utf-8"));
    assert_eq!(page.text().await?, WHATSAPP_HTML);

    // The build has onboarding/signal/index.html: the trailing-slash
    // spelling resolves through it.
    let page = client
        .get(format!("{base}/onboarding/signal/"))
        .send()
        .await?;
    assert_eq!(page.status(), reqwest::StatusCode::OK);
    assert_eq!(page.text().await?, SIGNAL_HTML);

    // Each spelling redirects to the one the build actually has, query
    // string included, rather than silently serving the app shell.
    let redirected = client
        .get(format!("{base}/onboarding/whatsapp/"))
        .send()
        .await?;
    assert_eq!(redirected.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        redirected
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some("/onboarding/whatsapp")
    );
    let redirected = client
        .get(format!("{base}/onboarding/signal?step=2"))
        .send()
        .await?;
    assert_eq!(redirected.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        redirected
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok()),
        Some("/onboarding/signal/?step=2")
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn the_crypto_webassembly_is_served_as_application_wasm() -> Result<()> {
    let (gateway, base) = start("wasm").await?;
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

    // WebAssembly.instantiateStreaming rejects anything but exactly
    // `application/wasm` — a charset parameter is already a TypeError, and
    // the Companion has no fallback path for it.
    let wasm = reqwest::get(format!("{base}/_app/immutable/crypto.wasm")).await?;
    assert_eq!(wasm.status(), reqwest::StatusCode::OK);
    assert_eq!(
        content_type(&wasm),
        Some("application/wasm"),
        "the WebAssembly MIME type carries no parameters"
    );
    assert_eq!(wasm.bytes().await?.as_ref(), WASM);

    // A pre-compressed sibling is served when the client accepts it — the
    // crypto module is megabytes raw — with the same content type.
    // This reqwest has no compression features enabled, so it neither
    // advertises an encoding on its own nor decodes the answer: the bytes
    // below are what went over the wire.
    let client = reqwest::Client::new();
    let compressed = client
        .get(format!("{base}/_app/immutable/crypto.wasm"))
        .header("accept-encoding", "br")
        .send()
        .await?;
    assert_eq!(compressed.status(), reqwest::StatusCode::OK);
    assert_eq!(
        compressed
            .headers()
            .get("content-encoding")
            .and_then(|value| value.to_str().ok()),
        Some("br"),
        "the brotli sibling is served to a client that accepts brotli"
    );
    assert_eq!(content_type(&compressed), Some("application/wasm"));
    assert_eq!(compressed.bytes().await?.as_ref(), WASM_BROTLI);

    gateway.stop().await;
    Ok(())
}

/// Issue #222: the Gateway states its cache policy, and the two halves of the
/// export get opposite ones.
///
/// Without a `Cache-Control` a browser applies heuristic freshness and reuses
/// the HTML — which names the old content-hashed chunks, which it also holds
/// — so a redeployed Companion is invisible to an open browser and nothing
/// says so. That is how three fixes to #221 were "verified" against a browser
/// that had never fetched them. So: the entry document is revalidated every
/// time and answers `304` when unchanged; the content-hashed assets, whose
/// name changes when their content does, are cached hard and for ever.
#[tokio::test]
async fn the_companion_states_its_cache_policy_and_revalidates_the_shell() -> Result<()> {
    let (gateway, base) = start("cache").await?;
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
    let header = |response: &reqwest::Response, name: &str| -> Option<String> {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let client = reqwest::Client::new();

    // The shell, by its prerendered spelling and by a client-side route that
    // falls back to 200.html: both are the document that names the current
    // chunks, so both are the one thing never to be reused blind.
    for path in ["/", "/onboarding/signal/", "/networks/sms"] {
        let shell = client.get(format!("{base}{path}")).send().await?;
        assert_eq!(shell.status(), reqwest::StatusCode::OK, "{path}");
        assert_eq!(
            header(&shell, "cache-control").as_deref(),
            Some("no-cache"),
            "{path}: the HTML is revalidated on every load"
        );
        let etag = header(&shell, "etag").unwrap_or_else(|| panic!("{path}: no ETag"));
        assert!(
            etag.starts_with('"') || etag.starts_with("W/\""),
            "{path}: {etag}"
        );

        let revalidated = client
            .get(format!("{base}{path}"))
            .header("if-none-match", &etag)
            .send()
            .await?;
        assert_eq!(
            revalidated.status(),
            reqwest::StatusCode::NOT_MODIFIED,
            "{path}: an unchanged shell costs a 304, not a re-download"
        );
        assert_eq!(
            header(&revalidated, "etag").as_deref(),
            Some(etag.as_str()),
            "{path}"
        );
        assert_eq!(
            header(&revalidated, "cache-control").as_deref(),
            Some("no-cache"),
            "{path}"
        );
        assert!(
            revalidated.bytes().await?.is_empty(),
            "{path}: a 304 carries no body"
        );

        let changed = client
            .get(format!("{base}{path}"))
            .header("if-none-match", "\"something-else\"")
            .send()
            .await?;
        assert_eq!(
            changed.status(),
            reqwest::StatusCode::OK,
            "{path}: a stale tag is re-sent"
        );
    }

    // The content-hashed half: the `immutable` in the path is a promise the
    // header now keeps. Whichever encoding goes out, the policy is the same.
    for accept in ["identity", "br"] {
        let asset = client
            .get(format!("{base}/_app/immutable/crypto.wasm"))
            .header("accept-encoding", accept)
            .send()
            .await?;
        assert_eq!(asset.status(), reqwest::StatusCode::OK);
        assert_eq!(
            header(&asset, "cache-control").as_deref(),
            Some("public, max-age=31536000, immutable"),
            "accept-encoding: {accept}"
        );
        assert!(
            header(&asset, "etag").is_some(),
            "accept-encoding: {accept}"
        );
    }
    // Two encodings are two representations, so they must not share a tag —
    // a cache that held the brotli bytes under the identity tag would hand
    // compressed bytes to a client that cannot decode them.
    let plain = client
        .get(format!("{base}/_app/immutable/crypto.wasm"))
        .header("accept-encoding", "identity")
        .send()
        .await?;
    let brotli = client
        .get(format!("{base}/_app/immutable/crypto.wasm"))
        .header("accept-encoding", "br")
        .send()
        .await?;
    assert_ne!(header(&plain, "etag"), header(&brotli, "etag"));

    // Two more names that outlive a build and therefore take the shell's
    // policy (#222): the worker's script — a browser that reused it from its
    // HTTP cache would keep an old worker installed, which is how a redeploy
    // stayed invisible through four reloads and one hard reload — and the
    // build id the handshake compares.
    for path in ["/service-worker.js", "/_app/version.json"] {
        let answer = client.get(format!("{base}{path}")).send().await?;
        assert_eq!(answer.status(), reqwest::StatusCode::OK, "{path}");
        assert_eq!(
            header(&answer, "cache-control").as_deref(),
            Some("no-cache"),
            "{path}: revalidated on every load, never reused blind"
        );
    }

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn an_unknown_app_path_loads_the_companion_and_the_api_answers_as_an_api() -> Result<()> {
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
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    // The Companion is a static export with client-side routing: a route
    // that was not prerendered exists only in the client's router, so a deep
    // link reloaded cold must serve the SPA fallback with a 200 — not a
    // redirect, not a 404.
    let deep_link = client
        .get(format!("{base}/contacts/%40alice%3Atest.twalk"))
        .send()
        .await?;
    assert_eq!(
        deep_link.status(),
        reqwest::StatusCode::OK,
        "an unknown app path serves the Companion's shell with 200"
    );
    assert_eq!(content_type(&deep_link), Some("text/html; charset=utf-8"));
    assert_eq!(
        deep_link.text().await?,
        FALLBACK_HTML,
        "the fallback is 200.html, not the prerendered index.html"
    );

    // The Gateway's own API surface answers as an API and never with the app
    // shell: a client parsing an API response must never be handed an HTML
    // page instead. Since ticket #52 the session guard answers first, so an
    // unauthenticated call is a 401 whether or not the route exists — what an
    // unknown path looks like to a *signed-in* caller is asserted in
    // tests/signin.rs, which has a device token to ask with.
    let api = reqwest::get(format!("{base}/api/consent")).await?;
    assert_eq!(
        api.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "the API requires a device token, and says so before saying anything else"
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
    assert_eq!(body["error"].as_str(), Some("unauthenticated"));

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
    let static_dir = companion_build("log-level")?;

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
    let static_dir = companion_build("misconfigured")?;

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
        &companion_build("no-build")?,
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
                .any(|line| line.contains("no fallback file in the static directory"))
                .then_some(())
        },
        "the startup warning about the absent build",
    )
    .await;
    assert!(warned.is_ok(), "startup warns about the absent build");

    gateway.stop().await;
    Ok(())
}

/// The server half of the upgrade path (#222): a Companion redeployed under a
/// running Gateway is what the Gateway serves and names from the next request
/// on. Nothing is read once and remembered — not the build id, not the shell's
/// tag — so a browser that revalidates (the `no-cache` above) gets the new
/// bytes and a browser comparing builds (the Companion's handshake) sees the
/// new one.
///
/// The browser half — a page running build A, told the Gateway ships B, that
/// purges its worker and reloads once, then says so — is the Companion's
/// `tests/e2e/version.spec.ts`. Between them: client on A, server moved to B,
/// client on B.
#[tokio::test]
async fn a_redeployed_companion_is_what_the_gateway_serves_and_names_from_the_next_request(
) -> Result<()> {
    let static_dir = companion_build("redeploy")?;
    let gateway = GatewayProc::start(&gateway_env(&static_dir))?;
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
    let client = reqwest::Client::new();

    // Build A: the health names it, the shell carries A's bytes and a tag.
    let health = client.get(format!("{base}/health")).send().await?;
    let health: serde_json::Value = health.json().await?;
    assert_eq!(
        health["companion_build"].as_str(),
        Some(harness::COMPANION_BUILD_ID)
    );
    let shell = client.get(format!("{base}/")).send().await?;
    let etag_a = shell
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .expect("the shell carries a tag");
    assert_eq!(shell.text().await?, INDEX_HTML);

    // The redeploy: a new export lands in the directory the Gateway serves —
    // a new shell naming new chunks, and a new build id. Nothing restarts.
    const INDEX_B: &str = "<!doctype html>\n<title>Companion home, build B</title>\n";
    std::fs::write(static_dir.join("index.html"), INDEX_B)?;
    std::fs::write(
        static_dir.join("_app/version.json"),
        "{\"version\":\"build-b\"}",
    )?;

    // Build B, from the next request: the health names it, and a browser
    // revalidating the shell with A's tag gets B's bytes and a 200 — never a
    // 304 that would keep it on A.
    let health = client.get(format!("{base}/health")).send().await?;
    let health: serde_json::Value = health.json().await?;
    assert_eq!(
        health["companion_build"].as_str(),
        Some("build-b"),
        "the Gateway names the build it ships now, not the one it started with: {health}"
    );
    let revalidated = client
        .get(format!("{base}/"))
        .header("if-none-match", &etag_a)
        .send()
        .await?;
    assert_eq!(
        revalidated.status(),
        reqwest::StatusCode::OK,
        "a shell that changed is re-sent, not confirmed"
    );
    assert_ne!(
        revalidated
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok()),
        Some(etag_a.as_str()),
        "the new shell has a tag of its own"
    );
    assert_eq!(revalidated.text().await?, INDEX_B);

    gateway.stop().await;
    Ok(())
}
