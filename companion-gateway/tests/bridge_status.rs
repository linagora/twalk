//! Ticket #56, bridge status: a bridge pushes its connection state at the
//! Gateway's webhook, the Gateway verifies it against that bridge's own
//! `as_token`, translates mautrix's vocabulary into the contract's, and
//! publishes the *changes* as schema-valid `bridge.status.changed.v1`.
//!
//! The seam is the process boundary, as everywhere else in this suite: the
//! real Gateway binary configured through its environment, a real NATS
//! JetStream, and a stub bridge implementing the provisioning contract
//! (`tests/harness/stub_bridge.rs`). A real mautrix bridge is never here —
//! it needs a live WhatsApp or Signal account and a human with a phone
//! (spec #47) — so what these tests prove is the contract the Gateway
//! speaks, and what they never prove is the network side.
//!
//! # How the tests stay out of each other's way
//!
//! Every suite shares one bus, and a bridge's events are keyed by its
//! `bridge_id`. So each test configures its own: `GATEWAY_BRIDGE_<ID>_
//! STATUS_ID` is exactly the variable for it, and a unique id per test makes
//! the shared subject readable without a filter anybody could get wrong.

mod harness;

use anyhow::{Context, Result};
use harness::{
    bridge_status_path, companion_build, ensure_stack, gateway_env_with_bridges_and_consent,
    nats_url, poll_until, push_bridge_status, signed_in_device_token, validate_against_contract,
    Bus, GatewayProc, StubBridge, BRIDGE_STATUS_SUBJECT, BRIDGE_STATUS_TYPE, CONSENT_STREAM,
    SERVER_NAME, STUB_AS_TOKEN, STUB_BRIDGE_ID, UNREACHABLE_BRIDGE_ID,
};
use serde_json::{json, Value};

/// One Gateway with a stub bridge behind it, a bus, and a `bridge_id` of its
/// own on that bus.
struct Fixture {
    gateway: GatewayProc,
    base: String,
    stub: StubBridge,
    bus: Bus,
    /// The contract's `bridge_id` for this test's stub bridge: unique, so
    /// the shared subject can be read without filtering on anything subtler.
    bridge_id: String,
    /// The second bridge, configured with no `as_token` at all.
    unverifiable_bridge_id: String,
    static_dir: std::path::PathBuf,
}

impl Fixture {
    async fn start(test_name: &str) -> Result<Self> {
        ensure_stack().await?;
        let bus = Bus::connect().await?;
        bus.ensure_stream(CONSENT_STREAM, &["twalk.>"]).await?;
        let stub = StubBridge::start().await?;
        let static_dir = companion_build(test_name)?;
        let bridge_id = unique_bridge_id(test_name, "stub");
        let unverifiable_bridge_id = unique_bridge_id(test_name, "dead");
        let gateway = Self::spawn(&static_dir, &stub, &bridge_id, &unverifiable_bridge_id)?;
        let base = gateway.base_url().await?;
        Ok(Self {
            gateway,
            base,
            stub,
            bus,
            bridge_id,
            unverifiable_bridge_id,
            static_dir,
        })
    }

    fn spawn(
        static_dir: &std::path::Path,
        stub: &StubBridge,
        bridge_id: &str,
        unverifiable_bridge_id: &str,
    ) -> Result<GatewayProc> {
        let mut env =
            gateway_env_with_bridges_and_consent(static_dir, &stub.base_url(), &nats_url());
        env.push((
            "GATEWAY_BRIDGE_MAUTRIX_STUB_STATUS_ID".to_owned(),
            bridge_id.to_owned(),
        ));
        env.push((
            "GATEWAY_BRIDGE_MAUTRIX_UNREACHABLE_STATUS_ID".to_owned(),
            unverifiable_bridge_id.to_owned(),
        ));
        GatewayProc::start(&env)
    }

    /// Restarts the Gateway on the same store and the same stub: what an
    /// operator's `docker compose up` does. The state each bridge was in is
    /// on the volume, so the new process compares against it instead of
    /// starting from nothing.
    async fn restart(self) -> Result<Self> {
        let Self {
            gateway,
            stub,
            bus,
            bridge_id,
            unverifiable_bridge_id,
            static_dir,
            ..
        } = self;
        gateway.stop().await;
        let gateway = Self::spawn(&static_dir, &stub, &bridge_id, &unverifiable_bridge_id)?;
        let base = gateway.base_url().await?;
        Ok(Self {
            gateway,
            base,
            stub,
            bus,
            bridge_id,
            unverifiable_bridge_id,
            static_dir,
        })
    }

    /// Pushes one state as the bridge does, with the right `as_token`, and
    /// fails the test if the Gateway did not accept it.
    async fn push(&self, state: Value) -> Result<()> {
        let response =
            push_bridge_status(&self.base, &self.bridge_id, Some(STUB_AS_TOKEN), &state).await?;
        anyhow::ensure!(
            response.status() == reqwest::StatusCode::NO_CONTENT,
            "the status webhook refused a verified push with {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
        Ok(())
    }

    /// Every event this fixture's bridge has published, in stream order.
    async fn events(&self) -> Result<Vec<Value>> {
        Ok(self
            .bus
            .fetch_all(CONSENT_STREAM, BRIDGE_STATUS_SUBJECT)
            .await?
            .into_iter()
            .filter(|event| event["data"]["bridge_id"] == json!(self.bridge_id))
            .collect())
    }

    /// Waits until this bridge has published `count` events, then returns
    /// them. Publication is asynchronous — the webhook answers before the
    /// outbox drains — so every assertion on the bus goes through here.
    async fn events_until(&self, count: usize) -> Result<Vec<Value>> {
        let events = poll_until(
            || async {
                let events = self.events().await.unwrap_or_default();
                (events.len() >= count).then_some(events)
            },
            &format!("{count} bridge status events on the bus"),
        )
        .await?;
        Ok(events)
    }

    /// Waits for a line in the Gateway's own captured output: the barrier
    /// for anything the startup path does after the origin is bound.
    async fn wait_for_log(&self, needle: &str) -> Result<()> {
        poll_until(
            || async {
                self.gateway
                    .logs()
                    .await
                    .iter()
                    .any(|line| line.contains(needle))
                    .then_some(())
            },
            &format!("the gateway to log {needle:?}"),
        )
        .await
    }

    async fn stop(self) {
        self.gateway.stop().await;
        self.stub.stop().await;
    }
}

/// A `bridge_id` no other test uses, matching the contract's
/// `^bridge-[a-z0-9-]+$`.
fn unique_bridge_id(test_name: &str, role: &str) -> String {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    let slug: String = test_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("bridge-{slug}-{role}-{}-{unique}", std::process::id())
}

/// One mautrix `BridgeState`, with an instant of its own so that two
/// otherwise identical pushes are two different events rather than one the
/// bus deduplicates.
fn state(state_event: &str, timestamp: u64) -> Value {
    json!({ "state_event": state_event, "timestamp": timestamp })
}

/// Every event the Gateway publishes must be valid against the contract, and
/// must name itself the way the Gateway's other events do.
fn assert_well_formed(event: &Value, bridge_id: &str) -> Result<()> {
    validate_against_contract(event, "bridge.status.changed")?;
    assert_eq!(event["type"], json!(BRIDGE_STATUS_TYPE));
    assert_eq!(
        event["source"],
        json!(format!("gateway://{SERVER_NAME}/bridges/{bridge_id}")),
        "a status event names the bridge resource on this Gateway"
    );
    assert_eq!(event["subject"], json!(bridge_id));
    assert_eq!(
        event["network"], event["data"]["network"],
        "the network extension mirrors data.network so a consumer can filter on it"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The mapping table, state by state
// ---------------------------------------------------------------------------

/// Every state mautrix can report, in an order that makes each one a change,
/// and the contract state the spec says it must become.
///
/// `BAD_CREDENTIALS` → `session_expired` is the row the whole ticket turns
/// on: that, and not `LOGGED_OUT`, is what a session revoked from the user's
/// own phone reports — no mautrix bridge emits `LOGGED_OUT` at all.
#[tokio::test]
async fn each_mautrix_state_becomes_the_contract_state_the_spec_names() -> Result<()> {
    let fixture = Fixture::start("mapping").await?;

    let script: [(&str, &str); 9] = [
        ("CONNECTING", "starting"),
        ("CONNECTED", "connected"),
        ("TRANSIENT_DISCONNECT", "degraded"),
        ("BACKFILLING", "starting"),
        ("CONNECTED", "connected"),
        ("BAD_CREDENTIALS", "session_expired"),
        ("UNKNOWN_ERROR", "disconnected"),
        ("CONNECTED", "connected"),
        ("LOGGED_OUT", "disconnected"),
    ];
    for (step, (reported, _)) in script.iter().enumerate() {
        fixture
            .push(state(reported, 1_789_000_000 + step as u64))
            .await?;
    }

    let events = fixture.events_until(script.len()).await?;
    assert_eq!(
        events.len(),
        script.len(),
        "one event per change, and no more"
    );
    // The first transition comes from `disconnected`: a bridge nobody has
    // heard from is not connected, which is the prior a user can act on.
    let mut previous = "disconnected";
    for (event, (reported, expected)) in events.iter().zip(script.iter()) {
        assert_well_formed(event, &fixture.bridge_id)?;
        assert_eq!(
            event["data"]["to_state"],
            json!(expected),
            "{reported} must map to {expected}: {event}"
        );
        assert_eq!(
            event["data"]["from_state"],
            json!(previous),
            "each event says the state it came from: {event}"
        );
        assert_eq!(
            event["data"]["network"],
            json!("whatsapp"),
            "the network is the instance's, from configuration"
        );
        previous = expected;
    }

    fixture.stop().await;
    Ok(())
}

/// The bridge's own message becomes the event's `reason`, and its own
/// `timestamp` becomes `occurred_at` — the instant of the change, not of the
/// push.
#[tokio::test]
async fn a_pushed_state_carries_the_bridges_reason_and_its_own_instant() -> Result<()> {
    let fixture = Fixture::start("reason").await?;

    fixture
        .push(json!({
            "state_event": "BAD_CREDENTIALS",
            "error": "whatsapp-logged-out",
            "message": "You were logged out from another device",
            "user_action": "RELOGIN",
            "timestamp": 1_789_000_000u64,
            "info": { "last_message_at": 1_788_999_000u64 },
        }))
        .await?;

    let events = fixture.events_until(1).await?;
    let event = &events[0];
    assert_well_formed(event, &fixture.bridge_id)?;
    assert_eq!(event["data"]["to_state"], json!("session_expired"));
    assert_eq!(
        event["data"]["reason"],
        json!("You were logged out from another device"),
        "the bridge's message is the cause a human reads"
    );
    assert_eq!(
        event["data"]["occurred_at"].as_str().unwrap_or_default(),
        "2026-09-10T00:26:40Z",
        "occurred_at is the bridge's own timestamp: {event}"
    );
    assert_eq!(
        event["data"]["last_message_at"]
            .as_str()
            .unwrap_or_default(),
        "2026-09-10T00:10:00Z",
        "last_message_at is filled when the bridge says one: {event}"
    );
    // mautrix's user_action has no field in the contract, and a producer
    // does not invent one.
    assert!(event["data"].get("user_action").is_none(), "{event}");

    fixture.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// De-duplication
// ---------------------------------------------------------------------------

/// A bridge re-pushing the state it is already in publishes nothing. Mautrix
/// pushes on its own schedule and retries with backoff, so this is the
/// normal case and not an edge one.
#[tokio::test]
async fn a_repeated_identical_state_produces_no_second_event() -> Result<()> {
    let fixture = Fixture::start("dedup").await?;

    fixture.push(state("CONNECTED", 1_789_000_000)).await?;
    let first = fixture.events_until(1).await?;
    assert_eq!(first[0]["data"]["to_state"], json!("connected"));

    // The same state again, at a later instant — a periodic re-push — and
    // then byte-for-byte the same body, which is what a retry sends.
    fixture.push(state("CONNECTED", 1_789_000_030)).await?;
    fixture.push(state("CONNECTED", 1_789_000_030)).await?;
    // BACKFILLING maps to `starting`, so it *is* a change; then CONNECTED
    // again. Two more events, and the count is what proves the three pushes
    // above produced none.
    fixture.push(state("BACKFILLING", 1_789_000_060)).await?;
    fixture.push(state("CONNECTED", 1_789_000_090)).await?;

    let events = fixture.events_until(3).await?;
    let states: Vec<&str> = events
        .iter()
        .map(|event| event["data"]["to_state"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        states,
        vec!["connected", "starting", "connected"],
        "only the changes are published: {events:?}"
    );

    // And every id is distinct: the bus deduplicates on the contract's id,
    // so two events that were meant to be one would not even be visible
    // here — the count above is the assertion, and this is the belt.
    let mut ids: Vec<&str> = events
        .iter()
        .map(|event| event["id"].as_str().unwrap_or_default())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 3, "{events:?}");

    fixture.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Startup reconciliation
// ---------------------------------------------------------------------------

/// A Gateway that starts asks each bridge what it is doing, and publishes
/// what it finds — the point being that a restart does not carry a stale
/// state forward, and does not invent a healthy one either.
#[tokio::test]
async fn startup_reconciliation_publishes_what_whoami_reports() -> Result<()> {
    // The stub has to hold the login *before* the Gateway starts, so the
    // fixture's own gateway is stopped and replaced: this is the one test
    // whose subject is what happens at startup.
    let fixture = Fixture::start("reconcile").await?;
    fixture
        .stub
        .add_existing_login("33612345678", "the stub's account");
    fixture
        .stub
        .set_login_state("33612345678", json!({ "state_event": "CONNECTED" }));

    let fixture = fixture.restart().await?;
    let events = fixture.events_until(1).await?;
    assert_well_formed(&events[0], &fixture.bridge_id)?;
    assert_eq!(
        events[0]["data"]["from_state"],
        json!("disconnected"),
        "nothing was known before, and nothing known is not connected"
    );
    assert_eq!(
        events[0]["data"]["to_state"],
        json!("connected"),
        "whoami said connected: {}",
        events[0]
    );

    // Now the session breaks while the Gateway is down. The next start finds
    // it, which is the whole reason reconciliation exists: a webhook pushed
    // at a process that was not running is a push nobody heard.
    fixture.stub.set_login_state(
        "33612345678",
        json!({ "state_event": "BAD_CREDENTIALS", "message": "session revoked by network" }),
    );
    let fixture = fixture.restart().await?;
    let events = fixture.events_until(2).await?;
    assert_eq!(
        events[1]["data"]["from_state"],
        json!("connected"),
        "the state the store remembered across the restart: {}",
        events[1]
    );
    assert_eq!(events[1]["data"]["to_state"], json!("session_expired"));
    assert_eq!(
        events[1]["data"]["reason"],
        json!("session revoked by network")
    );

    // A third start with nothing changed says nothing: reconciliation is a
    // comparison, not a heartbeat.
    let fixture = fixture.restart().await?;
    fixture
        .wait_for_log("startup reconciliation agreed with the stored state")
        .await?;
    assert_eq!(
        fixture.events().await?.len(),
        2,
        "a reconciliation that agrees with the store publishes nothing"
    );

    fixture.stop().await;
    Ok(())
}

/// A bridge that holds no login is `disconnected`, and a Gateway that cannot
/// reach a bridge at all keeps that bridge's last known state rather than
/// inventing one. In a compose stack the Gateway and the bridges start
/// together, so "I could not ask" must not become "it is down".
#[tokio::test]
async fn a_bridge_it_cannot_reach_keeps_its_last_known_state() -> Result<()> {
    let fixture = Fixture::start("unreachable").await?;
    // The second configured bridge points at a port nothing listens on, and
    // it never publishes anything at all.
    fixture.push(state("CONNECTED", 1_789_000_000)).await?;
    fixture.events_until(1).await?;

    let dead: Vec<Value> = fixture
        .bus
        .fetch_all(CONSENT_STREAM, BRIDGE_STATUS_SUBJECT)
        .await?
        .into_iter()
        .filter(|event| event["data"]["bridge_id"] == json!(fixture.unverifiable_bridge_id))
        .collect();
    assert!(
        dead.is_empty(),
        "a bridge that could not be asked has nothing to say: {dead:?}"
    );
    // And the operator is told why, rather than being shown a state the
    // Gateway made up.
    fixture
        .wait_for_log("could not reconcile a bridge's status at startup")
        .await?;

    fixture.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

/// The push is verified against the bridge's own `as_token`, and nothing
/// else opens it. Trusting the compose network was explicitly refused: every
/// container on it can reach this port, and a forged push could tell the user
/// a dead session was healthy.
#[tokio::test]
async fn an_unverified_push_is_refused() -> Result<()> {
    let fixture = Fixture::start("unverified").await?;
    let body = state("CONNECTED", 1_789_000_000);

    // No credential at all.
    let none = push_bridge_status(&fixture.base, &fixture.bridge_id, None, &body).await?;
    assert_eq!(none.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(
        none.json::<Value>().await?["error"],
        json!("unauthenticated")
    );

    // Another bridge's token — which is what a compromised neighbour on the
    // same network would have.
    let wrong = push_bridge_status(
        &fixture.base,
        &fixture.bridge_id,
        Some("not-this-bridges-as-token"),
        &body,
    )
    .await?;
    assert_eq!(wrong.status(), reqwest::StatusCode::UNAUTHORIZED);

    // A prefix of the real one, so the refusal cannot be read as a
    // length check.
    let truncated = push_bridge_status(
        &fixture.base,
        &fixture.bridge_id,
        Some(&STUB_AS_TOKEN[..12]),
        &body,
    )
    .await?;
    assert_eq!(truncated.status(), reqwest::StatusCode::UNAUTHORIZED);

    // The owner's own device token, which opens every other endpoint of this
    // origin and must not open this one.
    let device_token = signed_in_device_token(&fixture.base).await?;
    let as_a_browser = reqwest::Client::new()
        .post(format!(
            "{}{}",
            fixture.base,
            bridge_status_path(&fixture.bridge_id)
        ))
        .header("cookie", format!("twalk_device={device_token}"))
        .json(&body)
        .send()
        .await
        .context("the status webhook did not answer")?;
    assert_eq!(
        as_a_browser.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a device token is not a bridge credential"
    );

    // A bridge the Gateway holds no as_token for: refused, never trusted.
    let unverifiable = push_bridge_status(
        &fixture.base,
        &fixture.unverifiable_bridge_id,
        Some(STUB_AS_TOKEN),
        &body,
    )
    .await?;
    assert_eq!(
        unverifiable.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        unverifiable.json::<Value>().await?["error"],
        json!("as_token_not_configured")
    );

    // A bridge nobody configured.
    let unknown = push_bridge_status(
        &fixture.base,
        "bridge-nobody-configured",
        Some(STUB_AS_TOKEN),
        &body,
    )
    .await?;
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(
        unknown.json::<Value>().await?["error"],
        json!("unknown_bridge")
    );

    // Nothing above reached the bus. Then one verified push does, which is
    // what proves the assertion is about the credential and not about the
    // plumbing being broken.
    assert!(fixture.events().await?.is_empty());
    fixture.push(body).await?;
    assert_eq!(fixture.events_until(1).await?.len(), 1);

    fixture.stop().await;
    Ok(())
}

/// A body that is not a mautrix `BridgeState` is refused rather than guessed
/// at: the one thing this endpoint exists to carry is the state.
#[tokio::test]
async fn a_push_without_a_state_is_refused() -> Result<()> {
    let fixture = Fixture::start("invalid").await?;

    for body in [
        json!({}),
        json!({ "state_event": "" }),
        json!({ "message": "hello" }),
    ] {
        let response = push_bridge_status(
            &fixture.base,
            &fixture.bridge_id,
            Some(STUB_AS_TOKEN),
            &body,
        )
        .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{body} is not a bridge state"
        );
        assert_eq!(
            response.json::<Value>().await?["error"],
            json!("invalid_request")
        );
    }
    // A state this build does not know is *not* invalid: it is reported, and
    // read as disconnected rather than as fine. Which only shows as an event
    // once the bridge has been something else first — `disconnected` is
    // where a bridge nobody has heard from already is.
    fixture.push(state("CONNECTED", 1_789_000_000)).await?;
    fixture
        .push(json!({ "state_event": "SOME_FUTURE_STATE", "timestamp": 1_789_000_030u64 }))
        .await?;
    let events = fixture.events_until(2).await?;
    assert_eq!(events[0]["data"]["to_state"], json!("connected"));
    assert_eq!(
        events[1]["data"]["to_state"],
        json!("disconnected"),
        "a state nobody understands is not a state that is fine: {}",
        events[1]
    );

    fixture.stop().await;
    Ok(())
}

/// The webhook's own id namespace: the configured `bridge_id` is what the
/// events carry, and it is not the instance id the Companion's API uses. The
/// two are different names for different things, and this is the test that
/// says so out loud.
#[tokio::test]
async fn the_events_bridge_id_is_the_contracts_not_the_instances() -> Result<()> {
    let fixture = Fixture::start("ids").await?;

    // The instance id, which `/api/bridges` lists, is not a webhook id.
    let by_instance = push_bridge_status(
        &fixture.base,
        STUB_BRIDGE_ID,
        Some(STUB_AS_TOKEN),
        &state("CONNECTED", 1_789_000_000),
    )
    .await?;
    assert_eq!(by_instance.status(), reqwest::StatusCode::NOT_FOUND);

    fixture.push(state("CONNECTED", 1_789_000_000)).await?;
    let events = fixture.events_until(1).await?;
    assert_eq!(events[0]["data"]["bridge_id"], json!(fixture.bridge_id));
    assert!(
        fixture.bridge_id.starts_with("bridge-"),
        "the contract's pattern: {}",
        fixture.bridge_id
    );
    // And the Companion's own list still speaks the instance's name.
    let device_token = signed_in_device_token(&fixture.base).await?;
    let listed: Value = reqwest::Client::new()
        .get(format!("{}/api/bridges", fixture.base))
        .header("cookie", format!("twalk_device={device_token}"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(listed["bridges"][0]["bridge_id"], json!(STUB_BRIDGE_ID));
    assert_eq!(
        listed["bridges"][1]["bridge_id"],
        json!(UNREACHABLE_BRIDGE_ID)
    );

    fixture.stop().await;
    Ok(())
}
