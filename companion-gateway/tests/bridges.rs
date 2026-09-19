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
    bridge_fixtures, companion_build, ensure_stack, gateway_env_with_bridges, gateway_state_dir,
    owner_user_id, poll_until, FixtureBridge, GatewayProc, MatrixUser, StubBridge, OWNER_LOCALPART,
    STUB_BRIDGE_ID, UNREACHABLE_BRIDGE_ID,
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
        Self::start_as(test_name, FixtureBridge::Whatsapp).await
    }

    /// The same fixture against the other reference bridge's captured
    /// answers. The two do not agree — one flow versus two, different step
    /// ids, and an unknown flow id that is a `404` on one and a silent
    /// fallback to QR on the other — so the Gateway is held to both.
    async fn start_as(test_name: &str, bridge: FixtureBridge) -> Result<Self> {
        ensure_stack().await?;
        let stub = StubBridge::start_as(bridge).await?;
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

    /// `GET /api/bridges`, as the networks screen reads it.
    async fn bridges(&self) -> Result<Value> {
        let (status, body) = self
            .call(reqwest::Method::GET, "/api/bridges", None)
            .await?;
        anyhow::ensure!(status == 200, "the bridge list answered {status}: {body}");
        Ok(body)
    }

    /// One row of that list, by bridge id.
    async fn bridge_row(&self, bridge_id: &str) -> Result<Value> {
        let listed = self.bridges().await?;
        listed["bridges"]
            .as_array()
            .context("the bridge list is not a list")?
            .iter()
            .find(|row| row["bridge_id"] == json!(bridge_id))
            .cloned()
            .with_context(|| format!("no {bridge_id} row in {listed}"))
    }

    /// Restarts the Gateway process against the same state directory and the
    /// same stub, and signs a device in again.
    ///
    /// The point of doing it for real rather than clearing a field: a login
    /// process lives in the Gateway's memory and does not survive this, while
    /// the bridge's own logins do. That asymmetry is the whole of #108, and
    /// the only way to assert it is to actually restart.
    async fn restart_gateway(&mut self) -> Result<()> {
        let stopped = std::mem::replace(
            &mut self.gateway,
            GatewayProc::start(&gateway_env_with_bridges(
                &self.static_dir,
                &self.stub.base_url(),
            ))?,
        );
        stopped.stop().await;
        self.base = self.gateway.base_url().await?;
        let base = self.base.clone();
        poll_until(
            || async {
                reqwest::get(format!("{base}/health"))
                    .await
                    .ok()?
                    .error_for_status()
                    .ok()
            },
            "the restarted gateway health endpoint",
        )
        .await?;
        self.device = sign_in(&self.http, &self.base, "the device after the restart").await?;
        Ok(())
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
    // The prefix a real mautrix-whatsapp sends, not the `2@` this test used
    // to assert — WhatsApp's own QR payload is a `https://wa.me/...` link and
    // the `2@` form was invented along with the old stub (#106).
    assert!(
        first_code.starts_with(fixture.stub.qr_payload_prefix()),
        "the payload the browser draws is the network's own: {first_code}"
    );
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

/// Ticket #116: **"I could not reach the bridge" and "the bridge answered
/// something I could not read" are two different facts**, and they are driven
/// here one after the other against the same Gateway — one bridge with
/// nothing listening on its port, one bridge answering well-formed JSON of
/// the wrong shape.
///
/// The second is what actually happened on the first live WhatsApp login
/// (#106): the bridge answered in about 200 ms, correctly, and the Gateway
/// went looking for a field mautrix does not call by that name. Because both
/// came back as `bridge_unreachable`, the Companion said *your Twalk server
/// could not reach this network's bridge*, and a whole debugging session went
/// to networking, containers and ports — the one place the fault was not.
///
/// So this test asserts three things, and the third is the one that is easy
/// to break: the two have different codes; the unusable one names the call,
/// says the bridge answered and does not accuse the deployment; and **no
/// fragment of the bridge's answer comes back to the browser**, because a
/// provisioning answer can carry identifiers from a network account.
#[tokio::test]
async fn a_bridge_that_answers_unreadably_is_not_a_bridge_that_could_not_be_reached() -> Result<()> {
    let fixture = Fixture::start("bridges-unreadable-answer").await?;

    // 1. Nothing is listening on that port. This one really is a bridge that
    //    could not be reached, and it keeps the name and the message.
    let (status, nothing_listening) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{UNREACHABLE_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 502, "{nothing_listening}");
    assert_eq!(
        nothing_listening["error"],
        json!("bridge_unreachable"),
        "nothing answered at all: {nothing_listening}"
    );

    // 2. A bridge that answers, promptly and in well-formed JSON, a document
    //    this build cannot use. Everything in it is the sort of thing a real
    //    provisioning answer carries and that must not come back out.
    let answered = json!({
        "ok": true,
        "login": {
            "id": "33612345678",
            "name": "+33612345678",
            "profile": { "phone": "+33612345678" }
        },
        "code": "2@a-qr-payload-that-is-a-network-credential",
        "hint": "M_SOMETHING_THE_BRIDGE_SAID"
    });
    let secrets = [
        "33612345678",
        "2@a-qr-payload-that-is-a-network-credential",
        "M_SOMETHING_THE_BRIDGE_SAID",
    ];
    fixture.stub.answer_next_start_with(answered);

    let started = std::time::Instant::now();
    let (status, unusable) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    // The bridge answered at once. Asserting this is not decoration: the
    // failure mode being fixed is a prompt, correct answer reported as a
    // connection problem, and the Gateway's own timeout is fifteen seconds.
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the bridge answered immediately; nothing here timed out: {:?}",
        started.elapsed()
    );
    assert_eq!(status, 502, "{unusable}");
    assert_eq!(
        unusable["error"],
        json!("bridge_answer_unusable"),
        "the bridge answered, and that is a different refusal from not answering: {unusable}"
    );
    assert_ne!(
        unusable["error"], nothing_listening["error"],
        "an API caller tells the two apart on the code alone, without reading prose"
    );

    let detail = unusable["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("/login/start/"),
        "the refusal names which provisioning call answered: {detail}"
    );
    assert!(
        detail.contains("answered"),
        "and says the bridge did answer: {detail}"
    );
    assert!(
        detail.contains("not a broken deployment"),
        "and says whose defect this is, which is the sentence that gets the right bug \
         report instead of the wrong investigation: {detail}"
    );
    assert!(
        !detail.contains("could not be reached") && !detail.contains("nothing answered"),
        "and never the sentence that sent #106's debugging session to look at ports: {detail}"
    );

    // The acceptance criterion that is easiest to break: a bridge's answer
    // can carry a phone number as a login id, an account name or a QR
    // payload, so **none** of it is quoted back — not a field, not a
    // fragment, not a truncated one.
    let whole_answer = unusable.to_string();
    for secret in secrets {
        assert!(
            !whole_answer.contains(secret),
            "a refusal names what was missing, never what was received, and this one \
             carries {secret:?}: {whole_answer}"
        );
    }

    // 3. And the same distinction on the networks screen's own read. A
    //    `whoami` this build cannot read is a bridge that answered; the list
    //    still draws, and it says which of the two happened.
    fixture.stub.answer_next_whoami_with(json!({
        "bridge_bot": "@whatsappbot:twalk.localhost",
        "accounts": [{ "id": "33612345678" }]
    }));
    let row = fixture.bridge_row(STUB_BRIDGE_ID).await?;
    assert_eq!(
        row["connection"]["unreachable_because"],
        json!("bridge_answer_unusable"),
        "the bridge answered its whoami; what failed is our reading of it: {row}"
    );
    assert_eq!(
        row["connection"]["state"],
        Value::Null,
        "and the Gateway still does not guess a state it does not know: {row}"
    );
    let dead = fixture.bridge_row(UNREACHABLE_BRIDGE_ID).await?;
    assert_eq!(
        dead["connection"]["unreachable_because"],
        json!("bridge_unreachable"),
        "while the bridge with nothing listening keeps the other code: {dead}"
    );
    assert!(
        !row.to_string().contains("33612345678"),
        "and no part of the answer the Gateway could not read reaches the browser: {row}"
    );

    fixture.stop().await;
    Ok(())
}

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

// ---------------------------------------------------------------------------
// One test per divergence a real bridge was found to have (#106)
//
// Every test below exists because the stub used to answer something a real
// mautrix bridge does not, and a real login failed on it. The stub now serves
// the captured documents (`tests/harness/fixtures/`), so these hold the
// Gateway to the bridge's contract rather than to somebody's memory of it.
// ---------------------------------------------------------------------------

/// `GET /logins` answers `{"login_ids": ["…"]}` — bare strings. The Gateway
/// read it as a list of objects, found neither, and reported the bridge
/// unreachable, so the reconnect list never worked against a real bridge.
#[tokio::test]
async fn the_reconnect_list_reads_the_bare_login_ids_a_real_bridge_answers() -> Result<()> {
    let fixture = Fixture::start("bridges-login-ids").await?;
    fixture
        .stub
        .add_existing_login("33612345678", "+33612345678");

    let (status, logins) = fixture
        .call(
            reqwest::Method::GET,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/logins"),
            None,
        )
        .await?;
    assert_eq!(
        status, 200,
        "a bridge that answered its logins is not unreachable: {logins}"
    );
    assert_eq!(logins["logins"][0]["login_id"], json!("33612345678"));
    // And the name is honestly absent: this endpoint carries none, whoami is
    // where a login's name and profile live. A name here would be a fiction.
    assert_eq!(
        logins["logins"][0]["name"],
        Value::Null,
        "GET /logins answers ids only: {logins}"
    );

    fixture.stop().await;
    Ok(())
}

/// mautrix wants `?user_id=` on **every** provisioning call and answers
/// `403 M_FORBIDDEN` without it. The stub now refuses it too, so a cancel or
/// a logout that forgot it fails here rather than in front of a user.
#[tokio::test]
async fn the_cancel_and_the_logout_carry_the_acting_user_too() -> Result<()> {
    let fixture = Fixture::start("bridges-acting-user").await?;
    fixture.stub.add_existing_login("33612345678", "a session");

    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");

    // The cancel: two provisioning calls, and the stub 403s either one that
    // omits the acting user.
    let (status, cancelled) = fixture
        .call(
            reqwest::Method::DELETE,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            None,
        )
        .await?;
    assert_eq!(status, 204, "{cancelled}");
    poll_until(
        || async { (!fixture.stub.cancelled_processes().is_empty()).then_some(()) },
        "the bridge to have been told to cancel the process",
    )
    .await?;

    // And the logout.
    let (status, logged_out) = fixture
        .call(
            reqwest::Method::DELETE,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/logins/33612345678"),
            None,
        )
        .await?;
    assert_eq!(status, 204, "{logged_out}");
    assert_eq!(fixture.stub.logged_out(), vec!["33612345678".to_owned()]);

    fixture.stop().await;
    Ok(())
}

/// A bridge issues a fresh `txn_id` with **every** step answer and validates
/// it on the call that advances that step, answering `500 M_BAD_STATE:
/// Transaction ID does not match` for anything else. So the Gateway has to
/// echo the *latest* one — not the first, and not one of its own.
#[tokio::test]
async fn each_held_step_echoes_the_transaction_id_that_step_carried() -> Result<()> {
    let fixture = Fixture::start("bridges-txn-id").await?;

    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");

    // A refresh: the stub answers with a new code and a new transaction id,
    // and the Gateway must sit back down in the step quoting the new one. If
    // it quoted the old one the stub would answer M_BAD_STATE and the login
    // would fail instead of refreshing.
    fixture.stub.release_refreshed_qr("a-refreshed-code");
    let refreshed = fixture
        .poll_login_until("a refreshed code", |view| {
            view["step"]["payload"]["data"] == json!("a-refreshed-code")
        })
        .await?;
    assert_eq!(refreshed["state"], json!("awaiting_remote"), "{refreshed}");
    poll_until(
        || async { (fixture.stub.blocking_arrivals() >= 2).then_some(()) },
        "the gateway to hold the refreshed code's step",
    )
    .await?;

    let held: Vec<Option<String>> = fixture
        .stub
        .submits()
        .iter()
        .filter(|submit| submit.step_type == "display_and_wait")
        .map(|submit| submit.txn_id.clone())
        .collect();
    assert!(held.len() >= 2, "two held steps were expected: {held:?}");
    assert!(
        held.iter().all(Option::is_some),
        "every held step quotes the transaction id the bridge issued: {held:?}"
    );
    assert_ne!(
        held[0], held[1],
        "a bridge re-issues the transaction id with every answer, so the second \
         held step must quote the second one: {held:?}"
    );

    fixture.stop().await;
    Ok(())
}

/// The same rule on the non-blocking path: a submitted step quotes the
/// transaction id the step it answers carried.
#[tokio::test]
async fn a_submitted_step_echoes_its_own_transaction_id() -> Result<()> {
    let fixture = Fixture::start("bridges-txn-id-submit").await?;

    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": PHONE_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    let (status, done) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login/submit"),
            Some(json!({ "step_id": PHONE_STEP, "data": { "phone_number": "+33612345678" } })),
        )
        .await?;
    assert_eq!(
        status, 200,
        "a submit quoting the wrong transaction id would be M_BAD_STATE: {done}"
    );
    let submitted = fixture
        .stub
        .submits()
        .into_iter()
        .find(|submit| submit.step_type == "user_input")
        .context("the user_input step reached the bridge")?;
    assert!(
        submitted.txn_id.is_some(),
        "the submit quotes the transaction id the step carried: {submitted:?}"
    );

    fixture.stop().await;
    Ok(())
}

/// Neither reference bridge supports cancelling a `display_and_wait` step:
/// both answer `500 M_BAD_STATE: Login process does not support cancelling
/// steps`. What releases the held request is the **process** cancel, which
/// then answers the held request `410 FI.MAU.BRIDGE.LOGIN_CANCELLED`. The
/// Gateway has to end up with a cancelled login either way.
#[tokio::test]
async fn the_process_cancel_is_what_releases_a_held_step() -> Result<()> {
    let fixture = Fixture::start("bridges-step-cancel").await?;

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
        "the gateway to be holding the blocking step",
    )
    .await?;

    let (status, cancelled) = fixture
        .call(
            reqwest::Method::DELETE,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            None,
        )
        .await?;
    assert_eq!(status, 204, "{cancelled}");
    // The step cancel was refused, so it is the process cancel that let the
    // held request go.
    poll_until(
        || async { (fixture.stub.held() == 0).then_some(()) },
        "the held request to be released by the process cancel",
    )
    .await?;
    let after = fixture.login().await?;
    assert_eq!(
        after["state"],
        json!("cancelled"),
        "a 410 arriving after the cancel must not overwrite it: {after}"
    );

    fixture.stop().await;
    Ok(())
}

/// The network refusing a submitted value — WhatsApp's
/// `400 FI.MAU.WHATSAPP.PHONE_NUMBER_TOO_SHORT` — is something the user can
/// act on, so it must not arrive as a bad gateway. And the bridge drops the
/// login process along with the refusal, which the detail has to say or the
/// user retries into a `404`.
#[tokio::test]
async fn a_value_the_network_refuses_is_a_400_that_says_to_start_again() -> Result<()> {
    let fixture = Fixture::start("bridges-network-refused").await?;

    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": PHONE_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    fixture
        .stub
        .refuse_next_step(400, "FI.MAU.WHATSAPP.PHONE_NUMBER_TOO_SHORT");
    let (status, refused) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login/submit"),
            Some(json!({ "step_id": PHONE_STEP, "data": { "phone_number": "+1" } })),
        )
        .await?;
    assert_eq!(status, 400, "{refused}");
    assert_eq!(refused["error"], json!("invalid_request"));
    let detail = refused["detail"].as_str().context("a detail")?;
    assert!(
        detail.contains("FI.MAU.WHATSAPP.PHONE_NUMBER_TOO_SHORT"),
        "the detail names the network's own code: {detail}"
    );
    assert!(
        detail.contains("start the login again"),
        "the detail says the process is gone, so the user does not retry into a 404: {detail}"
    );

    fixture.stop().await;
    Ok(())
}

/// A login of **more than one question**, which this suite could not express
/// until #175 gave the stub a queue-the-next-step control.
///
/// Telegram's `phone` flow is three sequential `user_input` steps and a QR
/// login on an account with two-factor authentication interjects a `password`
/// one, so "the Gateway drives an arbitrary sequence" was a claim resting on a
/// stub that completed the login on every submit. This is that claim, asserted:
/// two answers, two steps, one login process, and the transaction id re-issued
/// with each answer — which the stub validates exactly as a bridge does, and
/// which is what #106 was about.
#[tokio::test]
async fn a_login_of_several_questions_advances_one_step_at_a_time() -> Result<()> {
    let fixture = Fixture::start("bridges-sequence").await?;

    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": PHONE_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    assert_eq!(started["step"]["step_id"], json!(PHONE_STEP), "{started}");

    // The answer to the phone number is another question, not the completion.
    const CODE_STEP: &str = "fi.mau.whatsapp.login.code";
    fixture.stub.queue_input_step(
        CODE_STEP,
        json!([{ "type": "2fa_code", "id": "code", "name": "Code" }]),
    );
    let (status, asked) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login/submit"),
            Some(json!({ "step_id": PHONE_STEP, "data": { "phone_number": "+33600000000" } })),
        )
        .await?;
    assert_eq!(status, 200, "{asked}");
    assert_eq!(asked["state"], json!("awaiting_input"), "{asked}");
    assert_eq!(asked["step"]["step_id"], json!(CODE_STEP), "{asked}");
    assert_eq!(asked["step"]["type"], json!("user_input"), "{asked}");
    assert_eq!(
        asked["step"]["payload"]["fields"][0]["type"],
        json!("2fa_code"),
        "the field list is the new step's, not the first one's: {asked}"
    );

    // And answering *that* one finishes the login.
    let (status, done) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login/submit"),
            Some(json!({ "step_id": CODE_STEP, "data": { "code": "123456" } })),
        )
        .await?;
    assert_eq!(status, 200, "{done}");
    assert_eq!(done["state"], json!("complete"), "{done}");

    // One login process throughout: a question is a step, not a new process.
    assert_eq!(fixture.stub.starts().len(), 1);
    let submits = fixture.stub.submits();
    let answered: Vec<_> = submits
        .iter()
        .filter(|submit| submit.step_type == "user_input")
        .collect();
    assert_eq!(answered.len(), 2, "{submits:?}");
    assert_eq!(answered[0].step_id, PHONE_STEP);
    assert_eq!(answered[1].step_id, CODE_STEP);
    // Both values passed through, under the ids the bridge's own fields named.
    assert_eq!(answered[0].body["phone_number"], json!("+33600000000"));
    assert_eq!(answered[1].body["code"], json!("123456"));
    // The second call echoed the transaction id the *second* step carried: a
    // bridge re-issues one with every answer, and the stub refuses a stale one.
    assert_ne!(answered[0].txn_id, answered[1].txn_id, "{submits:?}");

    fixture.stop().await;
    Ok(())
}

/// The network refusing another linked device, enforced by the bridge rather
/// than canned: bridgev2's `max_logins`. The one refusal in the stub that no
/// capture confirms — the reference deployment sets no cap — so it is
/// exercised here and called out in the PR as unverified.
#[tokio::test]
async fn a_bridge_at_its_login_cap_refuses_another_one() -> Result<()> {
    let fixture = Fixture::start("bridges-login-cap").await?;
    fixture.stub.add_existing_login("33612345678", "a session");
    fixture.stub.set_max_logins(1);

    let (status, refused) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 403, "{refused}");
    assert_eq!(refused["error"], json!("too_many_logins"));

    fixture.stop().await;
    Ok(())
}

/// mautrix-whatsapp does not validate the flow id: any unknown one silently
/// gives you the QR flow. mautrix-signal answers `404 M_NOT_FOUND "Invalid
/// login flow ID"` for the same call — asserted in the Signal test below.
/// A flow-id typo being invisible on one bridge and fatal on the other is
/// exactly what a fixture set exists to make visible.
#[tokio::test]
async fn an_unknown_flow_id_is_a_silent_qr_fallback_on_whatsapp() -> Result<()> {
    let fixture = Fixture::start("bridges-unknown-flow-wa").await?;

    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": "not-a-flow-this-bridge-has" })),
        )
        .await?;
    assert_eq!(
        status, 201,
        "mautrix-whatsapp answers 200 and the QR flow for any flow id: {started}"
    );
    assert_eq!(started["step"]["step_id"], json!(QR_STEP), "{started}");

    fixture.stop().await;
    Ok(())
}

/// The whole QR login again, against mautrix-signal's captured answers: one
/// flow rather than two, a step id namespaced to Signal, an `sgnl://` payload
/// rather than an `https://` one, and a `404` for a flow it does not have.
/// Nothing in the Gateway may be WhatsApp-shaped.
#[tokio::test]
async fn the_same_flow_runs_against_signals_own_shapes() -> Result<()> {
    let fixture = Fixture::start_as("bridges-signal", FixtureBridge::Signal).await?;

    // One captured flow, not two. (The stub adds its own two for the step
    // types no reference bridge offers, and marks them as such.)
    let (status, flows) = fixture
        .call(
            reqwest::Method::GET,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login/flows"),
            None,
        )
        .await?;
    assert_eq!(status, 200, "{flows}");
    let ids: Vec<&str> = flows["flows"]
        .as_array()
        .context("flows")?
        .iter()
        .filter_map(|flow| flow["id"].as_str())
        .collect();
    assert!(ids.contains(&QR_FLOW), "{flows}");
    assert!(
        !ids.contains(&PHONE_FLOW),
        "Signal has no phone-number flow: {flows}"
    );

    // Signal's own step id and its own payload scheme.
    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    assert_eq!(
        started["step"]["step_id"],
        json!(FixtureBridge::Signal.qr_step_id()),
        "step ids are namespaced per connector: {started}"
    );
    let code = started["step"]["payload"]["data"]
        .as_str()
        .context("a QR payload")?;
    assert!(
        code.starts_with(fixture.stub.qr_payload_prefix()),
        "Signal's payload is a device-linking URI, not WhatsApp's link: {code}"
    );

    // And the flow it does not have is a hard error here, unlike on WhatsApp.
    let (status, cancelled) = fixture
        .call(
            reqwest::Method::DELETE,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            None,
        )
        .await?;
    assert_eq!(status, 204, "{cancelled}");
    let (status, unknown) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": PHONE_FLOW })),
        )
        .await?;
    assert_eq!(
        status, 404,
        "mautrix-signal validates the flow id where mautrix-whatsapp does not: {unknown}"
    );
    assert_eq!(unknown["error"], json!("not_found_on_bridge"));

    fixture.stop().await;
    Ok(())
}

/// The fixtures themselves: every captured step answer must be readable by
/// the Gateway's own parser, and every captured error document must be the
/// `{errcode, error}` pair the facade reads a refusal out of.
///
/// This is the test that would have caught all three bugs of #106 on the day
/// the answers were recorded, which is the whole argument for recording them.
#[test]
fn the_gateways_parser_reads_every_captured_step_answer() -> Result<()> {
    use twalk_companion_gateway::bridge::parse_step;

    for bridge in [FixtureBridge::Whatsapp, FixtureBridge::Signal] {
        let mut checked = 0;
        for (endpoint, name) in [
            ("login-start", "qr"),
            ("login-step", "refreshed_qr"),
            ("login-step", "complete"),
        ] {
            let body = bridge_fixtures::body(bridge, endpoint, name);
            let step = parse_step(&body).map_err(|detail| {
                anyhow::anyhow!(
                    "{}'s captured {endpoint}/{name} answer is not a login step: {detail}. \
                     This is the failure #106 hit three times: the Gateway reading a field \
                     the bridge does not send",
                    bridge.as_str(),
                )
            })?;
            if name == "complete" {
                assert!(
                    step.login_id.is_some(),
                    "a completion names the login it created: {body}"
                );
            } else {
                // The process id the rest of the flow travels on, which
                // mautrix puts in `login_id` at the top level.
                assert!(
                    step.process_id.is_some(),
                    "{}'s {endpoint}/{name} must name the login process: {body}",
                    bridge.as_str(),
                );
                assert!(
                    step.txn_id.is_some(),
                    "{}'s {endpoint}/{name} must carry the transaction id the next call \
                     echoes: {body}",
                    bridge.as_str(),
                );
            }
            checked += 1;
        }
        assert!(checked >= 3, "{} had no step answers", bridge.as_str());

        // And the refusals: a status and mautrix's two-member error document.
        for (endpoint, name) in [
            ("whoami", "no_user_id"),
            ("whoami", "bad_secret"),
            ("login-step", "unknown_process"),
            ("login-step", "wrong_txn_id"),
            ("login-step", "step_cancel_unsupported"),
            ("login-step", "held_request_when_process_cancelled"),
            ("login-cancel", "already_gone"),
            ("logout", "unknown_login"),
        ] {
            let answer = bridge_fixtures::answer(bridge, endpoint, name);
            assert!(
                answer.status >= 400,
                "{}'s {endpoint}/{name} is a refusal: {}",
                bridge.as_str(),
                answer.status
            );
            assert!(
                answer.body["errcode"].is_string() && answer.body["error"].is_string(),
                "a mautrix refusal is {{errcode, error}}: {}",
                answer.body
            );
        }
    }

    // The captured logins answer is ids, not objects. The assertion is here
    // rather than in prose because prose is what failed last time.
    for bridge in [FixtureBridge::Whatsapp, FixtureBridge::Signal] {
        let body = bridge_fixtures::body(bridge, "logins", "no_logins");
        assert!(
            body["login_ids"].is_array(),
            "{} answers GET /logins as {{login_ids: [...]}}: {body}",
            bridge.as_str(),
        );
        assert!(
            body.get("logins").is_none(),
            "there is no `logins` member in that answer, which is the trap: {body}"
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Ticket #108: a network's connected state comes from the bridge's logins,
// never from a login process
// ---------------------------------------------------------------------------

/// Exactly these members and no others, which is what `openapi.yaml` says of
/// both objects this ticket adds.
fn assert_members(value: &Value, expected: &[&str]) {
    let mut found: Vec<&str> = value
        .as_object()
        .unwrap_or_else(|| panic!("expected an object, got {value}"))
        .keys()
        .map(String::as_str)
        .collect();
    let mut wanted = expected.to_vec();
    found.sort_unstable();
    wanted.sort_unstable();
    assert_eq!(found, wanted, "in {value}");
}

/// A `whoami` login in a given mautrix state, shaped as the reference bridges
/// nest it: the same `BridgeState` document the status webhook carries, under
/// the login, with `timestamp` in whole seconds.
fn login_state(state_event: &str, timestamp: u64) -> Value {
    json!({
        "state_event": state_event,
        "timestamp": timestamp,
        "ttl": 21600,
        "source": "bridge",
    })
}

/// The defect, at the seam it was found at.
///
/// The live incident: a WhatsApp bridge reporting `logins: 1, id
/// 33660469852, state CONNECTED` throughout, and a Companion showing no badge
/// at all — because the badge was read from the login *process*, which the
/// Gateway had lost across a restart. So the assertion is not "connected is
/// reported" but "connected is reported **while the login process says
/// something else entirely**", three times over: with nothing in flight, with
/// a QR scan in flight, and after the Gateway has been restarted under it.
#[tokio::test]
async fn a_connected_bridge_reads_as_connected_whatever_the_login_process_is_doing() -> Result<()> {
    let mut fixture = Fixture::start("bridges-connected-not-login").await?;
    fixture
        .stub
        .add_existing_login("33660469852", "+33660469852");
    fixture
        .stub
        .set_login_state("33660469852", login_state("CONNECTED", 1_789_706_879));

    // Nothing in flight at all: the old Companion's `login?.state` was `null`
    // here, and the card said nothing.
    let row = fixture.bridge_row(STUB_BRIDGE_ID).await?;
    // The description closes both objects (`additionalProperties: false`), so
    // the members are part of the contract and not an implementation detail.
    assert_members(&row, &["bridge_id", "network", "connection", "login"]);
    assert_members(
        &row["connection"],
        &[
            "reachable",
            "state",
            "reported",
            "reason",
            "logins",
            "unreachable_because",
        ],
    );
    assert_members(
        &row["connection"]["logins"][0],
        &[
            "login_id", "name", "profile", "state", "reported", "reason", "since",
        ],
    );
    assert_eq!(row["login"], Value::Null, "nothing is in flight: {row}");
    assert_eq!(
        row["connection"]["state"],
        json!("connected"),
        "the bridge holds a CONNECTED login: {row}"
    );
    assert_eq!(row["connection"]["reachable"], json!(true));
    // And the management screen's three facts come with it.
    let login = &row["connection"]["logins"][0];
    assert_eq!(login["login_id"], json!("33660469852"));
    assert_eq!(
        login["name"],
        json!("+33660469852"),
        "which account is linked, as the bridge names it: {row}"
    );
    assert_eq!(login["state"], json!("connected"));
    assert_eq!(
        login["reported"],
        json!("CONNECTED"),
        "mautrix's own word is kept for the operator: {row}"
    );
    assert!(
        login["since"]
            .as_str()
            .is_some_and(|since| since.starts_with("2026-")),
        "since when, from the bridge's own state.timestamp: {row}"
    );

    // A login in flight must not make a connected network look disconnected.
    // This is the acceptance criterion in as many words.
    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    let row = fixture.bridge_row(STUB_BRIDGE_ID).await?;
    assert_eq!(
        row["login"]["state"],
        json!("awaiting_remote"),
        "a QR scan really is in flight: {row}"
    );
    assert_eq!(
        row["connection"]["state"],
        json!("connected"),
        "a scan in progress says nothing about the link that already exists: {row}"
    );

    // And a cancelled one does not either.
    let (status, cancelled) = fixture
        .call(
            reqwest::Method::DELETE,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            None,
        )
        .await?;
    assert_eq!(status, 204, "{cancelled}");
    let row = fixture.bridge_row(STUB_BRIDGE_ID).await?;
    assert_eq!(row["login"]["state"], json!("cancelled"), "{row}");
    assert_eq!(
        row["connection"]["state"],
        json!("connected"),
        "cancelling a scan does not unlink an account: {row}"
    );

    // The case that was actually in front of the owner: the Gateway has been
    // restarted, so there is no login process left at all, and the bridge is
    // still holding the same connected login.
    fixture.restart_gateway().await?;
    let row = fixture.bridge_row(STUB_BRIDGE_ID).await?;
    assert_eq!(
        row["login"],
        Value::Null,
        "a login process does not survive a restart, and is not supposed to: {row}"
    );
    assert_eq!(
        row["connection"]["state"],
        json!("connected"),
        "the link is the bridge's, so a Gateway restart cannot break it: {row}"
    );
    assert_eq!(
        row["connection"]["logins"][0]["login_id"],
        json!("33660469852"),
        "{row}"
    );

    fixture.stop().await;
    Ok(())
}

/// The vocabulary is #56's, including the mapping that matters most: a
/// session revoked from the user's own phone reports `BAD_CREDENTIALS`, and
/// no mautrix bridge emits `LOGGED_OUT` at all. A Gateway that waited for the
/// latter would never tell a user their session had expired.
#[tokio::test]
async fn the_connection_state_uses_the_mapping_table_56_already_defined() -> Result<()> {
    let fixture = Fixture::start("bridges-connection-vocabulary").await?;
    fixture
        .stub
        .add_existing_login("33612345678", "+33612345678");

    for (reported, expected) in [
        ("CONNECTED", "connected"),
        ("CONNECTING", "starting"),
        ("BACKFILLING", "starting"),
        ("TRANSIENT_DISCONNECT", "degraded"),
        ("BAD_CREDENTIALS", "session_expired"),
        ("UNKNOWN_ERROR", "disconnected"),
        // Handled, and never expected: no bridge sends it.
        ("LOGGED_OUT", "disconnected"),
        // A state a future bridge might invent is disconnected, not fine.
        ("FUTURE_STATE", "disconnected"),
    ] {
        fixture
            .stub
            .set_login_state("33612345678", login_state(reported, 1_789_706_879));
        let row = fixture.bridge_row(STUB_BRIDGE_ID).await?;
        assert_eq!(
            row["connection"]["state"],
            json!(expected),
            "{reported} maps to {expected}: {row}"
        );
        assert_eq!(
            row["connection"]["logins"][0]["state"],
            json!(expected),
            "the login says the same as the bridge: {row}"
        );
    }

    // A login the bridge holds but has said nothing about — a bridge that has
    // just restarted, its state being in memory — is `starting`, not
    // `disconnected`. Reporting a working link as broken is the whole defect.
    fixture.stub.clear_logins();
    fixture
        .stub
        .add_existing_login("33612345678", "+33612345678");
    let row = fixture.bridge_row(STUB_BRIDGE_ID).await?;
    assert_eq!(row["connection"]["state"], json!("starting"), "{row}");
    assert_eq!(
        row["connection"]["logins"][0]["since"],
        Value::Null,
        "a bridge that reported no timestamp is not given one: {row}"
    );

    // No login at all: nothing is connected, and nothing can be.
    fixture.stub.clear_logins();
    let row = fixture.bridge_row(STUB_BRIDGE_ID).await?;
    assert_eq!(row["connection"]["state"], json!("disconnected"), "{row}");
    assert_eq!(row["connection"]["logins"], json!([]), "{row}");

    fixture.stop().await;
    Ok(())
}

/// A bridge the Gateway cannot ask is *unknown*, not disconnected — and the
/// list still answers for the bridges it could reach.
///
/// The temptation is to report `disconnected` and be done with it. That is
/// the same lie in a new place: it would tell a user with a working link that
/// it is broken, which is what sent the owner to re-pair a live WhatsApp
/// account.
#[tokio::test]
async fn a_bridge_that_cannot_be_asked_is_unknown_rather_than_disconnected() -> Result<()> {
    let fixture = Fixture::start("bridges-connection-unknown").await?;
    fixture
        .stub
        .add_existing_login("33612345678", "+33612345678");
    fixture
        .stub
        .set_login_state("33612345678", login_state("CONNECTED", 1_789_706_879));

    let listed = fixture.bridges().await?;
    let dead = fixture.bridge_row(UNREACHABLE_BRIDGE_ID).await?;
    assert_eq!(
        dead["connection"]["reachable"],
        json!(false),
        "nothing listens on that port: {listed}"
    );
    assert_eq!(
        dead["connection"]["state"],
        Value::Null,
        "the Gateway does not know, and does not guess: {dead}"
    );
    assert_eq!(dead["connection"]["logins"], json!([]), "{dead}");
    assert_eq!(
        dead["connection"]["unreachable_because"],
        json!("bridge_unreachable"),
        "and it says why, in the same stable codes the errors use: {dead}"
    );

    // The bridge that is up is unaffected: one bridge being down must not
    // cost the networks screen the other's badge.
    let alive = fixture.bridge_row(STUB_BRIDGE_ID).await?;
    assert_eq!(alive["connection"]["state"], json!("connected"), "{alive}");

    fixture.stop().await;
    Ok(())
}

/// The manage journey's Gateway half: connected → disconnect → not connected
/// → connect again. Nothing here is the login process's doing.
#[tokio::test]
async fn disconnecting_a_login_leaves_the_network_not_connected_until_it_is_linked_again(
) -> Result<()> {
    let fixture = Fixture::start("bridges-disconnect-reconnect").await?;
    fixture
        .stub
        .add_existing_login("33660469852", "+33660469852");
    fixture
        .stub
        .set_login_state("33660469852", login_state("CONNECTED", 1_789_706_879));

    let row = fixture.bridge_row(STUB_BRIDGE_ID).await?;
    assert_eq!(row["connection"]["state"], json!("connected"), "{row}");

    // Disconnect: what the management screen's button does, and the only
    // provisioning call that ends a link.
    let (status, logged_out) = fixture
        .call(
            reqwest::Method::DELETE,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/logins/33660469852"),
            None,
        )
        .await?;
    assert_eq!(status, 204, "{logged_out}");
    assert_eq!(fixture.stub.logged_out(), vec!["33660469852".to_owned()]);

    let row = fixture.bridge_row(STUB_BRIDGE_ID).await?;
    assert_eq!(
        row["connection"]["state"],
        json!("disconnected"),
        "the bridge holds no login now: {row}"
    );
    assert_eq!(row["connection"]["logins"], json!([]), "{row}");

    // And connecting again works: a full QR login, and the card reads
    // connected off the login the bridge is left holding.
    let (status, started) = fixture
        .call(
            reqwest::Method::POST,
            &format!("/api/bridges/{STUB_BRIDGE_ID}/login"),
            Some(json!({ "flow_id": QR_FLOW })),
        )
        .await?;
    assert_eq!(status, 201, "{started}");
    poll_until(
        || async { (fixture.stub.held() > 0).then_some(()) },
        "the gateway to be holding the blocking step",
    )
    .await?;
    fixture.stub.release_completion("33660469852");
    fixture
        .poll_login_until("complete", |view| view["state"] == json!("complete"))
        .await?;
    fixture
        .stub
        .set_login_state("33660469852", login_state("CONNECTED", 1_789_707_000));

    let row = fixture.bridge_row(STUB_BRIDGE_ID).await?;
    assert_eq!(row["connection"]["state"], json!("connected"), "{row}");
    assert_eq!(
        row["connection"]["logins"][0]["login_id"],
        json!("33660469852"),
        "{row}"
    );

    fixture.stop().await;
    Ok(())
}
