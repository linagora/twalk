//! Ticket #55, the bridge login provisioning proxy: the Gateway speaks a
//! bridge's provisioning API, **holds its blocking step**, and exposes a
//! pollable state to the Companion.
//!
//! # The seam, and what it cannot cover
//!
//! The Gateway is the real binary, at its process boundary, with a real
//! Synapse minting the OpenID token its device signs in with. On the other
//! side is a **stub bridge** (`tests/harness/stub_bridge.rs`) implementing
//! bridgev2's provisioning contract, whose blocking step this suite releases
//! on command.
//!
//! A real mautrix bridge is never in the suite: it needs a live WhatsApp or
//! Signal account and a human with a phone (spec #47). So the facade's
//! *network* side is not proven here — an accepted limitation, stated rather
//! than discovered. What is proven is every property the ticket asks for: a
//! full login, a refresh mid-flow, a cancellation, the concurrent-login
//! refusal, a login lost when the bridge restarts, and that no credential
//! reaches the Gateway's store or its logs.

mod harness;

use std::time::Duration;

use anyhow::{Context, Result};
use harness::stub_bridge::{COOKIES_FLOW, COOKIES_STEP, PHONE_FLOW, PHONE_STEP, QR_FLOW, QR_STEP};
use harness::{
    companion_build, ensure_stack, gateway_env_with_bridges, gateway_state_dir, owner_user_id,
    poll_until, GatewayProc, MatrixUser, StubBridge, OWNER_LOCALPART, STUB_BRIDGE_ID,
    UNREACHABLE_BRIDGE_ID,
};
use serde_json::{json, Value};

/// The Gateway, a stub bridge, and a signed-in device: what every test here
/// starts from.
struct Fixture {
    gateway: GatewayProc,
    base: String,
    stub: StubBridge,
    http: reqwest::Client,
    device: String,
    static_dir: std::path::PathBuf,
}

impl Fixture {
    async fn start(test_name: &str) -> Result<Self> {
        ensure_stack().await?;
        let stub = StubBridge::start().await?;
        let static_dir = companion_build(test_name)?;
        let gateway = GatewayProc::start(&gateway_env_with_bridges(&static_dir, &stub.base_url()))?;
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
        let http = reqwest::Client::new();
        let device = sign_in(&http, &base, "the scanning device").await?;
        Ok(Self {
            gateway,
            base,
            stub,
            http,
            device,
            static_dir,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// One API call as a signed-in device, answered as (status, body).
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value)> {
        self.call_as(&self.device, method, path, body).await
    }

    async fn call_as(
        &self,
        device: &str,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value)> {
        let mut request = self
            .http
            .request(method, self.url(path))
            .header(reqwest::header::COOKIE, format!("twalk_device={device}"));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let text = response.text().await?;
        let body = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        Ok((status, body))
    }

    /// The pollable login state, as the Companion reads it.
    async fn login(&self) -> Result<Value> {
        let (status, body) = self
            .call(
                reqwest::Method::GET,
                &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
                None,
            )
            .await?;
        anyhow::ensure!(status == 200, "polling the login answered {status}: {body}");
        Ok(body)
    }

    /// Polls until the login state satisfies `ready`. Every poll is expected
    /// to answer immediately — that is the whole point of holding the
    /// blocking step server-side — so a slow answer here is a failure of the
    /// design and not of the test.
    async fn poll_login_until(&self, what: &str, ready: impl Fn(&Value) -> bool) -> Result<Value> {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let started = std::time::Instant::now();
            let view = self.login().await?;
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(5),
                "polling the login blocked for {:?}: the Gateway must answer a poll at once, \
                 because the browser must never hold a request",
                started.elapsed()
            );
            if ready(&view) {
                return Ok(view);
            }
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "the login never reached {what}; last state: {view}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn stop(self) {
        self.stub.stop().await;
        self.gateway.stop().await;
    }
}

/// Signs a device in under a name of its own: the refusal of a second login
/// names the device, so the tests need to tell two of them apart.
async fn sign_in(http: &reqwest::Client, base: &str, device_name: &str) -> Result<String> {
    let owner = MatrixUser::login(OWNER_LOCALPART).await?;
    let response = http
        .post(format!("{base}/api/session"))
        .json(&json!({
            "matrix_openid_token": owner.openid_token().await?,
            "device_name": device_name,
        }))
        .send()
        .await?;
    anyhow::ensure!(
        response.status() == reqwest::StatusCode::OK,
        "the owner's sign-in was refused with {}",
        response.status()
    );
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';').unwrap_or((value, ""));
            let (name, token) = pair.split_once('=')?;
            (name == "twalk_device").then(|| token.to_owned())
        })
        .context("the sign-in set no device cookie")
}

// ---------------------------------------------------------------------------
// A full login: the flows, the QR, the held step, the completion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_whole_qr_login_runs_through_the_gateway_while_the_browser_only_polls() -> Result<()> {
    let fixture = Fixture::start("bridges-full-login").await?;

    // The bridges the deployment has, from configuration alone — no bridge
    // is contacted, so this screen draws while a bridge is down.
    let (status, listed) = fixture
        .call(reqwest::Method::GET, "/api/bridges", None)
        .await?;
    assert_eq!(status, 200, "{listed}");
    let bridges = listed["bridges"].as_array().context("a bridge list")?;
    assert_eq!(bridges.len(), 2, "{listed}");
    assert_eq!(bridges[0]["bridge_id"], json!(STUB_BRIDGE_ID));
    assert_eq!(bridges[0]["network"], json!("whatsapp"));
    assert_eq!(
        bridges[0]["login"],
        Value::Null,
        "nothing has been started yet"
    );

    // The flows the bridge itself offers.
    let (status, flows) = fixture
        .call(
            reqwest::Method::GET,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login/flows"),
            None,
        )
        .await?;
    assert_eq!(status, 200, "{flows}");
    assert!(
        flows["flows"]
            .as_array()
            .context("flows")?
            .iter()
            .any(|flow| flow["id"] == json!(QR_FLOW)),
        "{flows}"
    );

    // Starting answers at once, with the first code already in it.
    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    assert_eq!(started["state"], json!("awaiting_remote"));
    assert_eq!(started["step"]["type"], json!("display_and_wait"));
    assert_eq!(started["step"]["payload"]["type"], json!("qr"));
    let first_code = started["step"]["payload"]["data"]
        .as_str()
        .context("a QR step carries the raw payload the browser draws")?
        .to_owned();
    assert!(first_code.starts_with("2@"), "{first_code}");
    assert_eq!(
        started["step"]["valid_for_seconds"],
        json!(20),
        "the state says how long the code is worth drawing: {started}"
    );
    assert_eq!(
        started["started_by"]["device_name"],
        json!("the scanning device")
    );

    // The acting user is the owner from configuration, never a value the
    // browser chose: mautrix's shared-secret auth takes it on trust.
    let starts = fixture.stub.starts();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].user_id.as_deref(), Some(owner_user_id().as_str()));
    assert_eq!(
        starts[0].login_id, None,
        "this is a first login, not a repair"
    );

    // And the Gateway is the one sitting in the blocking step.
    poll_until(
        || async { (fixture.stub.held() == 1).then_some(()) },
        "the gateway to be holding the blocking step",
    )
    .await?;
    assert_eq!(fixture.stub.blocking_arrivals(), 1);

    // A poll answers immediately, over and over, while the user scans.
    let waiting = fixture.login().await?;
    assert_eq!(waiting["state"], json!("awaiting_remote"));
    assert_eq!(waiting["step"]["payload"]["data"], json!(first_code));

    // The phone scans: the held request answers, and the next poll says so.
    fixture.stub.release_completion("stub-login-full");
    let complete = fixture
        .poll_login_until("complete", |view| view["state"] == json!("complete"))
        .await?;
    assert_eq!(complete["login"]["login_id"], json!("stub-login-full"));
    assert_eq!(complete["step"], Value::Null);
    assert_eq!(complete["error"], Value::Null);

    // The login the bridge now holds is the one the Companion can reconnect
    // or log out.
    let (status, logins) = fixture
        .call(
            reqwest::Method::GET,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/logins"),
            None,
        )
        .await?;
    assert_eq!(status, 200, "{logins}");
    assert!(
        logins["logins"]
            .as_array()
            .context("logins")?
            .iter()
            .any(|login| login["login_id"] == json!("stub-login-full")),
        "{logins}"
    );

    fixture.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// A refresh mid-flow: the user must see a fresh code before the old expires
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_refreshed_code_reaches_the_browser_as_a_new_generation() -> Result<()> {
    let fixture = Fixture::start("bridges-refresh").await?;
    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    let first_code = started["step"]["payload"]["data"]
        .as_str()
        .context("the first code")?
        .to_owned();
    let first_generation = started["generation"].as_u64().context("a generation")?;

    // The bridge refreshes the code, twice, as it does every ~20 seconds
    // while nobody has scanned.
    poll_until(
        || async { (fixture.stub.held() == 1).then_some(()) },
        "the gateway to be holding the first code's step",
    )
    .await?;
    fixture.stub.release_refreshed_qr("2@a-refreshed-code");
    let refreshed = fixture
        .poll_login_until("a refreshed code", |view| {
            view["step"]["payload"]["data"] == json!("2@a-refreshed-code")
        })
        .await?;
    assert_ne!(
        refreshed["step"]["payload"]["data"],
        json!(first_code),
        "the refresh must not be swallowed: the user has to see the new code"
    );
    assert!(
        refreshed["generation"].as_u64().context("a generation")? > first_generation,
        "a refresh bumps the generation, which is how the UI knows to redraw: {refreshed}"
    );
    assert_eq!(
        refreshed["state"],
        json!("awaiting_remote"),
        "a refresh is not the end of the login"
    );
    assert_eq!(refreshed["step"]["step_id"], json!(QR_STEP));

    // And the Gateway went straight back into the blocking step.
    poll_until(
        || async { (fixture.stub.blocking_arrivals() >= 2).then_some(()) },
        "the gateway to hold the refreshed code's step too",
    )
    .await?;
    fixture.stub.release_completion("stub-login-refresh");
    let complete = fixture
        .poll_login_until("complete", |view| view["state"] == json!("complete"))
        .await?;
    assert!(
        complete["generation"].as_u64().context("a generation")?
            > refreshed["generation"].as_u64().context("a generation")?,
        "{complete}"
    );

    fixture.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Cancelling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cancelled_login_stops_at_the_bridge_and_reads_as_cancelled() -> Result<()> {
    let fixture = Fixture::start("bridges-cancel").await?;
    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    let process_id = started["process_id"]
        .as_str()
        .context("a process")?
        .to_owned();
    poll_until(
        || async { (fixture.stub.held() == 1).then_some(()) },
        "the gateway to be holding the step",
    )
    .await?;

    let (status, body) = fixture
        .call(
            reqwest::Method::DELETE,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            None,
        )
        .await?;
    assert_eq!(status, 204, "{body}");

    let cancelled = fixture.login().await?;
    assert_eq!(cancelled["state"], json!("cancelled"));
    assert_eq!(cancelled["step"], Value::Null);
    // The bridge was told, so it does not keep a process nobody will finish.
    assert!(
        fixture.stub.cancelled_processes().contains(&process_id),
        "the bridge must be told to forget the process: {:?}",
        fixture.stub.cancelled_processes()
    );

    // And the bridge is free for the next attempt, which is the point of
    // cancelling rather than waiting out the 30-minute cap.
    let (status, restarted) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{restarted}");

    fixture.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// One login at a time per bridge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_second_login_is_refused_and_names_the_device_and_time_that_started_the_first(
) -> Result<()> {
    let fixture = Fixture::start("bridges-concurrent").await?;
    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    let started_at = started["started_at"]
        .as_str()
        .context("an instant")?
        .to_owned();

    // The user's other device tries the same thing.
    let other = sign_in(&fixture.http, &fixture.base, "the other phone").await?;
    let (status, refused) = fixture
        .call_as(
            &other,
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["error"], json!("login_in_flight"));
    let detail = refused["detail"].as_str().context("a detail")?;
    assert!(
        detail.contains("the scanning device"),
        "the refusal names the device that started the login: {detail}"
    );
    assert!(
        detail.contains(&started_at),
        "the refusal names when it started ({started_at}): {detail}"
    );
    // Exactly one process was started on the bridge.
    assert_eq!(fixture.stub.starts().len(), 1);

    fixture.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// A login in flight does not survive a restart of the bridge
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_login_in_flight_is_lost_when_the_bridge_restarts_and_says_so() -> Result<()> {
    let mut fixture = Fixture::start("bridges-restart").await?;
    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    poll_until(
        || async { (fixture.stub.held() == 1).then_some(()) },
        "the gateway to be holding the step",
    )
    .await?;

    // The operator restarts the bridge, or it crashes: the held request dies
    // with it and the new process has forgotten the login.
    fixture.stub.restart().await?;

    let lost = fixture
        .poll_login_until("failed", |view| view["state"] == json!("failed"))
        .await?;
    assert_eq!(lost["error"]["code"], json!("login_lost"));
    let detail = lost["error"]["detail"].as_str().context("a detail")?;
    assert!(
        detail.contains("does not survive a restart"),
        "the limitation is stated where the user meets it: {detail}"
    );

    // And a new login works against the restarted bridge.
    let (status, again) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{again}");

    fixture.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Credentials pass through, and are nowhere else (ADR 0011)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_relayed_credential_reaches_the_bridge_and_neither_the_store_nor_the_logs() -> Result<()>
{
    let fixture = Fixture::start("bridges-credentials").await?;
    // A marker no other part of the system could produce, standing in for
    // the Google cookies of the SMS preview path.
    let cookie_value = "twalk-g55-secret-cookie-value-1a2b3c";

    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": COOKIES_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    assert_eq!(
        started["state"],
        json!("awaiting_input"),
        "a cookies step waits for the browser, not for the network: {started}"
    );
    assert_eq!(started["step"]["step_id"], json!(COOKIES_STEP));

    let (status, submitted) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login/submit"),
            Some(json!({
                "step_id": COOKIES_STEP,
                "data": { "cookies": { "SID": cookie_value } }
            })),
        )
        .await?;
    assert_eq!(status, 200, "{submitted}");
    assert_eq!(submitted["state"], json!("complete"), "{submitted}");

    // It reached the bridge unchanged: that is what "passes through" means.
    let submits = fixture.stub.submits();
    let relayed = submits
        .iter()
        .find(|submit| submit.step_id == COOKIES_STEP)
        .context("the cookies step was never relayed to the bridge")?;
    assert_eq!(relayed.body["cookies"]["SID"], json!(cookie_value));

    // And it is nowhere on the Gateway's side. The QR payload of a second
    // login is checked the same way: both are network credentials.
    let (status, qr) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{qr}");
    let qr_payload = qr["step"]["payload"]["data"]
        .as_str()
        .context("a QR payload")?
        .to_owned();

    let state_dir = gateway_state_dir(&fixture.static_dir);
    for secret in [cookie_value, qr_payload.as_str()] {
        let found = grep_directory(&state_dir, secret)?;
        assert!(
            found.is_empty(),
            "a network credential must never reach the Gateway's store (ADR 0011); \
             {secret:?} was found in {found:?}"
        );
    }

    // The logs describe the shape of what went through and never the value.
    let logs = fixture.gateway.logs().await;
    for secret in [cookie_value, qr_payload.as_str()] {
        let leaked: Vec<&String> = logs.iter().filter(|line| line.contains(secret)).collect();
        assert!(
            leaked.is_empty(),
            "a network credential must never reach a log line; {secret:?} is in {leaked:?}"
        );
    }
    assert!(
        logs.iter()
            .any(|line| line.contains("relaying a bridge login step")),
        "the relay is logged — by shape, not by value"
    );
    // Nor does the provisioning secret, which is the powerful one.
    assert!(
        !logs
            .iter()
            .any(|line| line.contains(harness::STUB_PROVISIONING_SECRET)),
        "the bridge's provisioning secret must never be logged"
    );

    fixture.stop().await;
    Ok(())
}

/// Every file under a directory that contains this text, as a byte search —
/// a SQLite file is not UTF-8, so the search is over bytes.
fn grep_directory(directory: &std::path::Path, needle: &str) -> Result<Vec<std::path::PathBuf>> {
    let mut found = Vec::new();
    if !directory.exists() {
        return Ok(found);
    }
    let mut pending = vec![directory.to_path_buf()];
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            for entry in std::fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
            continue;
        }
        let bytes = std::fs::read(&path)?;
        if bytes
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
        {
            found.push(path);
        }
    }
    Ok(found)
}

// ---------------------------------------------------------------------------
// Reconnect, logout, and the steps the Companion cannot drive
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reconnect_restarts_the_flow_against_the_login_the_bridge_already_holds() -> Result<()> {
    let fixture = Fixture::start("bridges-reconnect").await?;
    fixture
        .stub
        .add_existing_login("stub-login-broken", "the broken session");

    // What the Companion reads to offer the repair.
    let (status, logins) = fixture
        .call(
            reqwest::Method::GET,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/logins"),
            None,
        )
        .await?;
    assert_eq!(status, 200, "{logins}");
    assert_eq!(logins["logins"][0]["login_id"], json!("stub-login-broken"));

    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW, "login_id": "stub-login-broken" })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    assert_eq!(
        started["login_id"],
        json!("stub-login-broken"),
        "the state says which login is being repaired: {started}"
    );
    // The bridge was asked to re-log in to that login, not to make a second
    // one: mautrix's `?login_id=`.
    let starts = fixture.stub.starts();
    assert_eq!(
        starts.last().and_then(|start| start.login_id.as_deref()),
        Some("stub-login-broken"),
        "{starts:?}"
    );

    // Logging out drops it at the bridge.
    fixture.stub.release_completion("stub-login-broken");
    fixture
        .poll_login_until("complete", |view| view["state"] == json!("complete"))
        .await?;
    let (status, body) = fixture
        .call(
            reqwest::Method::DELETE,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/logins/stub-login-broken"),
            None,
        )
        .await?;
    assert_eq!(status, 204, "{body}");
    assert_eq!(fixture.stub.logged_out(), vec!["stub-login-broken"]);
    // And a login the bridge does not have is a 404 naming that, not a 502.
    let (status, unknown) = fixture
        .call(
            reqwest::Method::DELETE,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/logins/never-existed"),
            None,
        )
        .await?;
    assert_eq!(status, 404, "{unknown}");
    assert_eq!(unknown["error"], json!("not_found_on_bridge"));

    fixture.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_step_the_companion_cannot_drive_fails_explicitly_instead_of_hanging() -> Result<()> {
    let fixture = Fixture::start("bridges-webauthn").await?;
    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": harness::stub_bridge::WEBAUTHN_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    assert_eq!(
        started["state"],
        json!("failed"),
        "a webauthn step fails at once rather than waiting out the process: {started}"
    );
    assert_eq!(started["error"]["code"], json!("webauthn_required"));
    assert!(
        started["error"]["detail"]
            .as_str()
            .context("a detail")?
            .contains("fail_on_webauthn"),
        "the detail names the bridge setting that refuses it one step earlier: {started}"
    );
    // The process was cancelled on the bridge rather than left to expire.
    poll_until(
        || async { (!fixture.stub.cancelled_processes().is_empty()).then_some(()) },
        "the gateway to cancel the process it cannot drive",
    )
    .await?;

    fixture.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// The refusals an operator and a client have to be able to tell apart
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_facades_refusals_name_what_is_wrong() -> Result<()> {
    let fixture = Fixture::start("bridges-refusals").await?;

    // A bridge this deployment does not have.
    let (status, unknown) = fixture
        .call(
            reqwest::Method::GET,
            "/api/bridges/mautrix-telegram/login",
            None,
        )
        .await?;
    assert_eq!(status, 404, "{unknown}");
    assert_eq!(unknown["error"], json!("unknown_bridge"));

    // Nothing started on a bridge that exists.
    let (status, nothing) = fixture
        .call(
            reqwest::Method::GET,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            None,
        )
        .await?;
    assert_eq!(status, 404, "{nothing}");
    assert_eq!(nothing["error"], json!("no_login_in_flight"));

    // A bridge that is configured and down: a 502 that says what to check,
    // not a hang and not a 500.
    let (status, down) = fixture
        .call(
            reqwest::Method::GET,
            &format!("/api/bridges/{UNREACHABLE_BRIDGE_ID}/login/flows"),
            None,
        )
        .await?;
    assert_eq!(status, 502, "{down}");
    assert_eq!(down["error"], json!("bridge_unreachable"));

    // A start with no flow.
    let (status, invalid) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({})),
        )
        .await?;
    assert_eq!(status, 400, "{invalid}");
    assert_eq!(invalid["error"], json!("invalid_request"));

    // The network refusing another linked device.
    fixture
        .stub
        .refuse_next_start(403, "FI.MAU.BRIDGE.TOO_MANY_LOGINS");
    let (status, too_many) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 403, "{too_many}");
    assert_eq!(too_many["error"], json!("too_many_logins"));

    // A submission against a step the login is not on.
    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": PHONE_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    let (status, wrong_step) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login/submit"),
            Some(json!({ "step_id": "a-step-from-another-flow", "data": {} })),
        )
        .await?;
    assert_eq!(status, 400, "{wrong_step}");
    assert_eq!(wrong_step["error"], json!("invalid_request"));

    // The bridge ending the login under us: one of mautrix's three 410s.
    fixture.stub.refuse_next_step(410, "LOGIN_TIMED_OUT");
    let (status, expired) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login/submit"),
            Some(json!({ "step_id": PHONE_STEP, "data": { "phone_number": "+33600000000" } })),
        )
        .await?;
    assert_eq!(status, 410, "{expired}");
    assert_eq!(expired["error"], json!("login_expired"));
    // And the polled state agrees, so a browser that was polling learns it.
    let after = fixture.login().await?;
    assert_eq!(after["state"], json!("failed"), "{after}");
    assert_eq!(after["error"]["code"], json!("login_expired"));

    fixture.stop().await;
    Ok(())
}

#[tokio::test]
async fn every_bridge_route_needs_a_device_token() -> Result<()> {
    let fixture = Fixture::start("bridges-guard").await?;
    let unauthenticated = reqwest::Client::new();
    for (method, path) in [
        (reqwest::Method::GET, "/api/bridges".to_owned()),
        (
            reqwest::Method::GET,
            format!("/api/bridges/{STUB_BRIDGE_ID}/login/flows"),
        ),
        (
            reqwest::Method::POST,
            format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
        ),
        (
            reqwest::Method::GET,
            format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
        ),
        (
            reqwest::Method::DELETE,
            format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
        ),
        (
            reqwest::Method::POST,
            format!("/api/bridges/{STUB_BRIDGE_ID}/login/submit"),
        ),
        (
            reqwest::Method::GET,
            format!("/api/bridges/{STUB_BRIDGE_ID}/logins"),
        ),
        (
            reqwest::Method::DELETE,
            format!("/api/bridges/{STUB_BRIDGE_ID}/logins/whatever"),
        ),
    ] {
        let response = unauthenticated
            .request(method.clone(), fixture.url(&path))
            .send()
            .await?;
        assert_eq!(
            response.status().as_u16(),
            401,
            "{method} {path} must require a device token: #55 adds no row to the guard's \
             exception table"
        );
    }
    // And nothing reached the bridge.
    assert!(fixture.stub.starts().is_empty());

    fixture.stop().await;
    Ok(())
}
