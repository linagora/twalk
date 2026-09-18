//! Ticket #49, the consent store and its write API: a decision taken over
//! HTTP is committed to an append-only journal, reaches the bus exactly once
//! as a schema-valid `consent.state.changed.v1`, and reads back with the
//! network-default-versus-contact-override precedence applied.
//!
//! The seam is the process boundary, as it is for the Sensor and for #48:
//! a real Synapse (the device signs in with a real OpenID token, #52), a real
//! NATS JetStream, the real Gateway binary configured through its
//! environment, and HTTP calls from the test. Nothing here reaches inside the
//! Gateway process — the crash property is driven by killing it.
//!
//! Every test runs its own Gateway with its own consent store, and one bus
//! shared with every other suite: a test reads back the events it caused by
//! the ids the write API gave it, and the decisions it takes are about
//! contacts whose Matrix IDs are unique to the run.

mod harness;

use anyhow::{Context, Result};
use harness::{
    companion_build, ensure_stack, gateway_env, gateway_env_with, gateway_env_with_consent,
    nats_url, owner_user_id, parse_exposition, poll_until, sha256_hex, signed_in_device_token,
    unreachable_nats_url, validate_against_contract, Bus, GatewayProc, StoredMessage,
    CONSENT_STREAM, CONSENT_SUBJECT, SERVER_NAME,
};
use serde_json::{json, Value};

const CONSENT_TYPE: &str = "fr.linagora.twalk.consent.state.changed.v1";

/// The source every Gateway in this suite names its events by: the owner's
/// own server name, as the Gateway derives it (`gateway://<domain>/consent`).
fn source() -> String {
    format!("gateway://{SERVER_NAME}/consent")
}

/// One Gateway with a consent store, signed in on one device, against the
/// test stack's bus.
struct Fixture {
    gateway: GatewayProc,
    base: String,
    device_token: String,
    /// Kept so a test can restart the Gateway on the same journal and the
    /// same static build.
    static_dir: std::path::PathBuf,
    bus: Bus,
}

impl Fixture {
    async fn start(test_name: &str) -> Result<Self> {
        let bus = Self::bus().await?;
        Self::start_on(test_name, &nats_url(), bus).await
    }

    /// The test stack's bus, with the stream the Sensor and the Gateway both
    /// declare — whichever component runs first creates it, identically.
    async fn bus() -> Result<Bus> {
        ensure_stack().await?;
        let bus = Bus::connect().await?;
        bus.ensure_stream(CONSENT_STREAM, &["twalk.>"]).await?;
        Ok(bus)
    }

    /// Starts a Gateway pointed at an arbitrary bus — the crash test points
    /// its first one at a port nothing listens on.
    async fn start_on(test_name: &str, url: &str, bus: Bus) -> Result<Self> {
        let static_dir = companion_build(test_name)?;
        let gateway = GatewayProc::start(&gateway_env_with_consent(&static_dir, url))?;
        let base = gateway.base_url().await?;
        let device_token = signed_in_device_token(&base).await?;
        Ok(Self {
            gateway,
            base,
            device_token,
            static_dir,
            bus,
        })
    }

    /// Restarts the Gateway on the same stores, against the given bus: what
    /// an operator's `docker compose up` does after an unclean stop. The
    /// device stays signed in — the session store is on the same volume.
    async fn restart_on(self, url: &str) -> Result<Self> {
        let Self {
            gateway,
            static_dir,
            device_token,
            bus,
            ..
        } = self;
        // SIGKILL: no draining, no graceful shutdown, no chance to publish
        // what the journal is holding.
        gateway.stop().await;
        let gateway = GatewayProc::start(&gateway_env_with_consent(&static_dir, url))?;
        let base = gateway.base_url().await?;
        Ok(Self {
            gateway,
            base,
            device_token,
            static_dir,
            bus,
        })
    }

    /// Records a decision through the write API, with the device's token in
    /// the cookie the Companion sends.
    async fn decide(&self, body: Value) -> Result<(reqwest::StatusCode, Value)> {
        let response = reqwest::Client::new()
            .post(format!("{}/api/consent/decisions", self.base))
            .header("cookie", format!("twalk_device={}", self.device_token))
            .json(&body)
            .send()
            .await
            .context("the write API did not answer")?;
        let status = response.status();
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, body))
    }

    /// Records a decision and fails the test if it was not committed.
    async fn decide_ok(&self, body: Value) -> Result<Value> {
        let (status, recorded) = self.decide(body).await?;
        assert_eq!(
            status,
            reqwest::StatusCode::CREATED,
            "the decision must be committed: {recorded}"
        );
        Ok(recorded)
    }

    async fn get(&self, path: &str) -> Result<(reqwest::StatusCode, Value)> {
        let response = reqwest::Client::new()
            .get(format!("{}{path}", self.base))
            .header("cookie", format!("twalk_device={}", self.device_token))
            .send()
            .await
            .context("the read API did not answer")?;
        let status = response.status();
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, body))
    }

    /// The events on the consent subject carrying one of these ids. The bus
    /// is shared with every other suite and run, so the ids the write API
    /// returned are what identifies this test's own events.
    async fn published(&self, ids: &[&str]) -> Result<Vec<StoredMessage>> {
        Ok(self
            .bus
            .fetch_all_with_headers(CONSENT_STREAM, CONSENT_SUBJECT)
            .await?
            .into_iter()
            .filter(|message| {
                message.payload["id"]
                    .as_str()
                    .is_some_and(|id| ids.contains(&id))
            })
            .collect())
    }

    /// The events published about one subject, whatever their ids: how a test
    /// asserts that a refused decision published *nothing*.
    async fn published_about(&self, subject_id: &str) -> Result<Vec<StoredMessage>> {
        Ok(self
            .bus
            .fetch_all_with_headers(CONSENT_STREAM, CONSENT_SUBJECT)
            .await?
            .into_iter()
            .filter(|message| message.payload["data"]["subject"]["id"].as_str() == Some(subject_id))
            .collect())
    }

    /// Waits for every one of these ids to be on the bus.
    async fn wait_for_published(&self, ids: &[&str]) -> Result<Vec<StoredMessage>> {
        poll_until(
            || async {
                let published = self.published(ids).await.ok()?;
                (published.len() >= ids.len()).then_some(published)
            },
            &format!("{} consent event(s) on the bus", ids.len()),
        )
        .await
    }

    /// One metric sample of this Gateway's exposition.
    async fn metric(&self, name: &str) -> Result<Option<u64>> {
        let body = reqwest::get(format!("{}/metrics", self.base))
            .await?
            .text()
            .await?;
        Ok(parse_exposition(&body)
            .into_iter()
            .find(|(sample, _)| sample == name)
            .map(|(_, value)| value))
    }

    async fn stop(self) {
        self.gateway.stop().await;
    }
}

/// A contact id unique to this test run: one bus and one consent subject are
/// shared by every suite.
fn contact(test_name: &str) -> String {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    format!("@whatsapp_{test_name}_{unique}:{SERVER_NAME}")
}

/// The `event_id` of a recorded decision.
fn event_id(recorded: &Value) -> Result<String> {
    Ok(recorded["event_id"]
        .as_str()
        .context("the answer names the event id")?
        .to_owned())
}

#[tokio::test]
async fn a_decision_taken_over_http_appears_on_the_bus_schema_valid() -> Result<()> {
    let fixture = Fixture::start("recorded").await?;
    let subject = contact("recorded");

    let recorded = fixture
        .decide_ok(json!({
            "subject": { "type": "contact", "id": subject },
            "new_state": "granted",
            "scope": { "networks": ["whatsapp"] },
            "reason": "she asked me to"
        }))
        .await?;
    // The answer names the event the outbox will publish, and the state the
    // subject came from: nothing had ever been decided about her.
    assert_eq!(recorded["old_state"].as_str(), Some("unset"), "{recorded}");
    assert_eq!(recorded["new_state"].as_str(), Some("granted"));
    // The decision is the owner's, whichever of their devices it arrived
    // from (ADR 0011: one owner per deployment).
    assert_eq!(recorded["actor"].as_str(), Some(owner_user_id().as_str()));
    let id = event_id(&recorded)?;
    let occurred_at = recorded["occurred_at"]
        .as_str()
        .context("the answer names when the decision was taken")?
        .to_owned();

    let published = fixture.wait_for_published(&[&id]).await?;
    let event = &published[0].payload;

    // The contract itself is the assertion that matters.
    validate_against_contract(event, "consent.state.changed")?;

    // The deterministic id, recomputed here from the schema's recipe rather
    // than trusted from the Gateway: a third party reproducing this string
    // byte for byte must get the same id.
    let expected_id = sha256_hex(&format!("contact:{subject}:granted:whatsapp:{occurred_at}"));
    assert_eq!(
        id, expected_id,
        "the id is sha256 of subject type, id, state, sorted scope and occurred_at"
    );

    assert_eq!(event["type"].as_str(), Some(CONSENT_TYPE));
    assert_eq!(event["source"].as_str(), Some(source().as_str()));
    assert_eq!(event["specversion"].as_str(), Some("1.0"));
    assert_eq!(event["datacontenttype"].as_str(), Some("application/json"));
    assert_eq!(event["subject"].as_str(), Some(subject.as_str()));
    // A single-network scope, so the filtering extension is set.
    assert_eq!(event["network"].as_str(), Some("whatsapp"));
    // And the consent extension never is, on the Gateway's own events.
    assert!(
        event.get("consent").is_none(),
        "consent is never set on a consent change: {event}"
    );
    assert_eq!(
        event["data"]["subject"],
        json!({"type": "contact", "id": subject})
    );
    assert_eq!(event["data"]["old_state"].as_str(), Some("unset"));
    assert_eq!(event["data"]["new_state"].as_str(), Some("granted"));
    assert_eq!(event["data"]["scope"]["networks"], json!(["whatsapp"]));
    assert_eq!(
        event["data"]["occurred_at"].as_str(),
        Some(occurred_at.as_str())
    );
    assert_eq!(
        event["data"]["actor"].as_str(),
        Some(owner_user_id().as_str())
    );
    assert_eq!(event["data"]["reason"].as_str(), Some("she asked me to"));

    // The de-duplication key the bus needs to absorb a republished outbox
    // row.
    assert_eq!(
        published[0].header("Nats-Msg-Id"),
        Some(expected_id.as_str()),
        "the event is published under its own id as the message id"
    );

    // The outbox drained, and says so.
    assert_eq!(
        fixture
            .metric("twalk_companion_gateway_consent_outbox_pending")
            .await?,
        Some(0)
    );
    assert_eq!(
        fixture
            .metric("twalk_companion_gateway_consent_events_published_total")
            .await?,
        Some(1)
    );

    fixture.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_decision_over_several_networks_carries_the_sorted_scope_and_no_network_extension(
) -> Result<()> {
    let fixture = Fixture::start("multi-network").await?;
    let subject = contact("multi");

    // Deliberately not in the order the id recipe wants them.
    let recorded = fixture
        .decide_ok(json!({
            "subject": { "type": "contact", "id": subject },
            "new_state": "revoked",
            "scope": { "networks": ["whatsapp", "matrix", "signal"] }
        }))
        .await?;
    let id = event_id(&recorded)?;
    let occurred_at = recorded["occurred_at"].as_str().unwrap().to_owned();

    let published = fixture.wait_for_published(&[&id]).await?;
    let event = &published[0].payload;
    validate_against_contract(event, "consent.state.changed")?;

    // The scope is the natural key's third part, sorted ascending.
    assert_eq!(
        event["data"]["scope"]["networks"],
        json!(["matrix", "signal", "whatsapp"])
    );
    assert_eq!(
        id,
        sha256_hex(&format!(
            "contact:{subject}:revoked:matrix,signal,whatsapp:{occurred_at}"
        )),
        "the id includes the sorted scope"
    );
    // Several networks: no single network to filter on, so the extension is
    // left out and data.scope.networks is the authority.
    assert!(
        event.get("network").is_none(),
        "a multi-network change sets no network extension: {event}"
    );

    fixture.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_network_default_is_overridden_by_a_contact_decision() -> Result<()> {
    let fixture = Fixture::start("precedence").await?;
    let loud = contact("loud");
    let quiet = contact("quiet");

    // The user grants WhatsApp as a whole — the default for every contact on
    // it — then revokes one contact.
    let default_decision = fixture
        .decide_ok(json!({
            "subject": { "type": "network", "id": "whatsapp" },
            "new_state": "granted",
            "scope": { "networks": ["whatsapp"] }
        }))
        .await?;
    let override_decision = fixture
        .decide_ok(json!({
            "subject": { "type": "contact", "id": loud },
            "new_state": "revoked",
            "scope": { "networks": ["whatsapp"] }
        }))
        .await?;
    // The contact-scoped decision saw no earlier decision *about her*: the
    // network default is a default, not a state she was in.
    assert_eq!(override_decision["old_state"].as_str(), Some("unset"));

    // Read back: the override wins for her, the default answers for everyone
    // else, and a network nobody decided about stays pending.
    let (status, resolved) = fixture
        .get(&format!(
            "/api/consent/effective?contact={}&network=whatsapp",
            urlencoding(&loud)
        ))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK, "{resolved}");
    assert_eq!(resolved["state"].as_str(), Some("revoked"), "{resolved}");
    assert_eq!(
        resolved["decided_by"],
        json!({"type": "contact", "id": loud}),
        "the answer names the decision that won"
    );

    let (_, defaulted) = fixture
        .get(&format!(
            "/api/consent/effective?contact={}&network=whatsapp",
            urlencoding(&quiet)
        ))
        .await?;
    assert_eq!(defaulted["state"].as_str(), Some("granted"), "{defaulted}");
    assert_eq!(
        defaulted["decided_by"],
        json!({"type": "network", "id": "whatsapp"}),
        "the answer names the network default that decided it"
    );

    let (_, undecided) = fixture
        .get(&format!(
            "/api/consent/effective?contact={}&network=telegram",
            urlencoding(&loud)
        ))
        .await?;
    assert_eq!(undecided["state"].as_str(), Some("pending"), "{undecided}");
    assert_eq!(
        undecided["decided_by"],
        Value::Null,
        "never decided is not revoked, and names no decision"
    );

    // The projection holds both decisions, the revocation as explicit as the
    // grant.
    let (_, state) = fixture.get("/api/consent/state").await?;
    let entries = state["entries"].as_array().context("entries")?;
    let network_entry = entries
        .iter()
        .find(|entry| entry["subject"] == json!({"type": "network", "id": "whatsapp"}))
        .context("the network default is in the current state")?;
    assert_eq!(network_entry["state"].as_str(), Some("granted"));
    assert_eq!(network_entry["network"].as_str(), Some("whatsapp"));
    let contact_entry = entries
        .iter()
        .find(|entry| entry["subject"]["id"] == json!(loud))
        .context("the contact override is in the current state")?;
    assert_eq!(contact_entry["state"].as_str(), Some("revoked"));

    // Both decisions are on the bus, schema-valid, and the network one is a
    // network subject scoped to itself.
    let published = fixture
        .wait_for_published(&[
            &event_id(&default_decision)?,
            &event_id(&override_decision)?,
        ])
        .await?;
    for message in &published {
        validate_against_contract(&message.payload, "consent.state.changed")?;
    }
    let network_event = published
        .iter()
        .find(|message| message.payload["data"]["subject"]["type"] == json!("network"))
        .context("the network decision was published")?;
    assert_eq!(
        network_event.payload["data"]["subject"]["id"].as_str(),
        Some("whatsapp")
    );
    assert_eq!(
        network_event.payload["data"]["scope"]["networks"],
        json!(["whatsapp"])
    );

    fixture.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_crash_between_commit_and_publish_publishes_exactly_once_on_restart() -> Result<()> {
    let bus = Fixture::bus().await?;
    // Every publish on the subject, JetStream's own de-duplication included:
    // this is what proves the Gateway publishes once, rather than relying on
    // the bus to absorb a second attempt.
    let mut raw = bus.subscribe_raw(CONSENT_SUBJECT).await?;

    // A Gateway whose bus does not exist: the decision commits and stays in
    // the outbox.
    let fixture = Fixture::start_on("crash", &unreachable_nats_url()?, bus).await?;
    let subject = contact("crash");
    let recorded = fixture
        .decide_ok(json!({
            "subject": { "type": "contact", "id": subject },
            "new_state": "granted",
            "scope": { "networks": ["signal"] }
        }))
        .await?;
    let id = event_id(&recorded)?;
    // Committed, unpublished: the state the crash has to survive.
    let pending = poll_until(
        || async {
            fixture
                .metric("twalk_companion_gateway_consent_outbox_pending")
                .await
                .ok()?
                .filter(|pending| *pending == 1)
        },
        "the decision to be committed and waiting in the outbox",
    )
    .await?;
    assert_eq!(pending, 1);
    assert_eq!(
        fixture
            .metric("twalk_companion_gateway_consent_events_published_total")
            .await?,
        Some(0),
        "nothing was published: the bus does not exist"
    );
    assert!(
        fixture.published(&[&id]).await?.is_empty(),
        "and the bus has nothing from this Gateway"
    );

    // Killed with the journal row unpublished, then restarted against the
    // real bus — the operator's `docker compose up` after an unclean stop.
    let fixture = fixture.restart_on(&nats_url()).await?;

    let published = fixture.wait_for_published(&[&id]).await?;
    assert_eq!(published.len(), 1, "the decision the crash interrupted");
    let event = &published[0].payload;
    validate_against_contract(event, "consent.state.changed")?;
    assert_eq!(
        event["data"]["subject"]["id"].as_str(),
        Some(subject.as_str())
    );
    assert_eq!(
        published[0].header("Nats-Msg-Id"),
        Some(id.as_str()),
        "a row republished after a crash is deduplicated on the bus by this header"
    );

    // Exactly once, counted three ways: the Gateway's own counter, the
    // stream's contents, and every publish the bus saw — the outbox sweeps
    // once a second, so a loop that republished would show up here.
    assert_eq!(
        fixture
            .metric("twalk_companion_gateway_consent_events_published_total")
            .await?,
        Some(1)
    );
    assert_eq!(
        fixture
            .metric("twalk_companion_gateway_consent_outbox_pending")
            .await?,
        Some(0),
        "the outbox marked the decision published"
    );
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert_eq!(
        fixture.published(&[&id]).await?.len(),
        1,
        "three outbox sweeps later, still one event"
    );
    let mut raw_publishes = 0;
    while let Ok(event) = raw.try_recv() {
        if event["id"].as_str() == Some(id.as_str()) {
            raw_publishes += 1;
        }
    }
    assert_eq!(
        raw_publishes, 1,
        "the Gateway published the decision once, not once per sweep"
    );

    fixture.stop().await;
    Ok(())
}

#[tokio::test]
async fn activating_a_persona_is_a_decision_on_this_same_write_path() -> Result<()> {
    // ADR 0013: activating or pausing a persona *is* a consent decision, on
    // the same journal and the same bus subject, with `scope.networks` naming
    // the networks that persona may read. The Companion's screen 4 writes
    // exactly this (#69), and Hermes learns that a persona is active by
    // reading the event off the bus (#60) — there is no control API to add.
    let fixture = Fixture::start("persona").await?;
    // Unique to the run, like every other subject here: one bus is shared by
    // every suite.
    let persona = format!(
        "assistant-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos()
    );

    let activated = fixture
        .decide_ok(json!({
            "subject": { "type": "persona", "id": persona },
            "new_state": "granted",
            "scope": { "networks": ["whatsapp", "signal"] }
        }))
        .await?;
    assert_eq!(
        activated["old_state"].as_str(),
        Some("unset"),
        "{activated}"
    );
    // Sorted, because the scope is part of the event's deterministic id.
    assert_eq!(
        activated["scope"]["networks"],
        json!(["signal", "whatsapp"])
    );
    let granted_id = event_id(&activated)?;

    let event = &fixture.wait_for_published(&[&granted_id]).await?[0].payload;
    validate_against_contract(event, "consent.state.changed")?;
    assert_eq!(
        event["data"]["subject"],
        json!({"type": "persona", "id": persona})
    );
    assert_eq!(event["data"]["new_state"].as_str(), Some("granted"));
    assert_eq!(
        event["data"]["scope"]["networks"],
        json!(["signal", "whatsapp"])
    );

    // The owner's own read shows it, which is how the dashboard knows which
    // personas are active and on which networks.
    let (status, state) = fixture.get("/api/consent/state").await?;
    assert_eq!(status, reqwest::StatusCode::OK, "{state}");
    let entries: Vec<&Value> = state["entries"]
        .as_array()
        .context("the state names its entries")?
        .iter()
        .filter(|entry| entry["subject"]["id"].as_str() == Some(persona.as_str()))
        .collect();
    assert_eq!(entries.len(), 2, "one entry per network: {state}");
    assert!(entries.iter().all(|entry| entry["state"] == "granted"));

    // And the consumer snapshot leaves it out: a persona is not consent state
    // a Sensor labels senders by (ticket #50).
    let snapshot = reqwest::Client::new()
        .get(format!("{}/api/consent/snapshot", fixture.base))
        .header(
            "authorization",
            format!("Bearer {}", harness::SERVICE_TOKEN),
        )
        .send()
        .await
        .context("the snapshot did not answer")?
        .json::<Value>()
        .await?;
    assert!(
        !snapshot["entries"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .any(|entry| entry["subject"]["type"] == "persona"),
        "no persona reaches the snapshot: {snapshot}"
    );

    // Pausing it is the same call with `revoked`. It is starved, not stopped:
    // nothing here manages a process, and nothing can.
    let paused = fixture
        .decide_ok(json!({
            "subject": { "type": "persona", "id": persona },
            "new_state": "revoked",
            "scope": { "networks": ["whatsapp", "signal"] }
        }))
        .await?;
    assert_eq!(paused["old_state"].as_str(), Some("granted"), "{paused}");
    let paused_event = &fixture.wait_for_published(&[&event_id(&paused)?]).await?[0].payload;
    assert_eq!(paused_event["data"]["new_state"].as_str(), Some("revoked"));

    fixture.stop().await;
    Ok(())
}

#[tokio::test]
async fn the_write_api_refuses_a_caller_without_a_device_token() -> Result<()> {
    let fixture = Fixture::start("unauthenticated").await?;
    let subject = contact("unauthenticated");
    let decision = json!({
        "subject": { "type": "contact", "id": subject },
        "new_state": "granted",
        "scope": { "networks": ["whatsapp"] }
    });
    let client = reqwest::Client::new();

    for (what, request) in [
        (
            "no token at all",
            client.post(format!("{}/api/consent/decisions", fixture.base)),
        ),
        (
            "a token the Gateway never issued",
            client
                .post(format!("{}/api/consent/decisions", fixture.base))
                .header("cookie", "twalk_device=not-a-token-this-gateway-issued"),
        ),
    ] {
        let response = request.json(&decision).send().await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "{what} must not be able to write consent"
        );
        let body = response.json::<Value>().await?;
        assert_eq!(body["error"].as_str(), Some("unauthenticated"), "{body}");
    }

    // Reading is authenticated too — the guard protects every consent route.
    let unauthenticated = client
        .get(format!("{}/api/consent/state", fixture.base))
        .send()
        .await?;
    assert_eq!(
        unauthenticated.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "the current state is not public either"
    );

    // And nothing was recorded by any of it.
    let (_, state) = fixture.get("/api/consent/state").await?;
    assert_eq!(state["entries"], json!([]), "{state}");
    assert!(fixture.published_about(&subject).await?.is_empty());

    fixture.stop().await;
    Ok(())
}

#[tokio::test]
async fn the_write_api_refuses_what_the_contract_does_not_allow() -> Result<()> {
    let fixture = Fixture::start("refusals").await?;
    let subject = contact("refusals");

    for (what, body, expected) in [
        (
            "a subject type the contract does not have",
            json!({
                "subject": { "type": "device", "id": "assistant" },
                "new_state": "granted",
                "scope": { "networks": ["whatsapp"] }
            }),
            "unknown_value",
        ),
        (
            "a network subject whose scope is not its own network",
            json!({
                "subject": { "type": "network", "id": "whatsapp" },
                "new_state": "granted",
                "scope": { "networks": ["signal"] }
            }),
            "scope_contradicts_subject",
        ),
        (
            "a network that is not one of the contract's",
            json!({
                "subject": { "type": "contact", "id": subject },
                "new_state": "granted",
                "scope": { "networks": ["gmessages"] }
            }),
            "unknown_value",
        ),
        (
            "a state that is not one of the contract's",
            json!({
                "subject": { "type": "contact", "id": subject },
                "new_state": "unset",
                "scope": { "networks": ["whatsapp"] }
            }),
            "unknown_value",
        ),
        (
            "an empty scope",
            json!({
                "subject": { "type": "contact", "id": subject },
                "new_state": "granted",
                "scope": { "networks": [] }
            }),
            "malformed_request",
        ),
        (
            "no subject at all",
            json!({ "new_state": "granted", "scope": { "networks": ["whatsapp"] } }),
            "malformed_request",
        ),
    ] {
        let (status, answer) = fixture.decide(body).await?;
        assert_eq!(
            status,
            reqwest::StatusCode::BAD_REQUEST,
            "{what} must be refused: {answer}"
        );
        assert_eq!(
            answer["error"].as_str(),
            Some(expected),
            "{what}: the answer carries a code a client can branch on"
        );
        assert!(
            answer["detail"].as_str().is_some_and(|m| !m.is_empty()),
            "{what}: and a detail an operator can read"
        );
    }

    // Nothing refused was recorded, and nothing reached the bus.
    let (_, state) = fixture.get("/api/consent/state").await?;
    assert_eq!(state["entries"], json!([]), "{state}");
    assert!(fixture.published_about(&subject).await?.is_empty());

    fixture.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_gateway_without_a_bus_says_consent_is_not_configured() -> Result<()> {
    // Sign-in configured (#52's own environment), no GATEWAY_NATS_URL: a
    // Gateway that cannot publish a decision must not pretend to record one.
    ensure_stack().await?;
    let static_dir = companion_build("no-bus")?;
    let gateway = GatewayProc::start(&gateway_env(&static_dir))?;
    let base = gateway.base_url().await?;
    let device_token = signed_in_device_token(&base).await?;

    let response = reqwest::Client::new()
        .post(format!("{base}/api/consent/decisions"))
        .header("cookie", format!("twalk_device={device_token}"))
        .json(&json!({
            "subject": { "type": "contact", "id": contact("no-bus") },
            "new_state": "granted",
            "scope": { "networks": ["whatsapp"] }
        }))
        .send()
        .await?;
    assert_eq!(
        response.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "an unconfigured consent store is a 503, not a 404 and not a silent success"
    );
    let body = response.json::<Value>().await?;
    assert_eq!(body["error"].as_str(), Some("consent_not_configured"));
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("GATEWAY_NATS_URL")),
        "the answer names what is missing: {body}"
    );

    // The Companion, the session, health and metrics are unaffected, and no
    // consent series is exposed.
    let session = reqwest::Client::new()
        .get(format!("{base}/api/session"))
        .header("cookie", format!("twalk_device={device_token}"))
        .send()
        .await?;
    assert_eq!(
        session.status(),
        reqwest::StatusCode::OK,
        "sign-in still works without a bus"
    );
    let metrics = reqwest::get(format!("{base}/metrics"))
        .await?
        .text()
        .await?;
    assert!(
        !metrics.contains("consent"),
        "a Gateway that writes no consent exposes no consent metrics:\n{metrics}"
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn consent_stays_off_without_an_owner_to_attribute_a_decision_to() -> Result<()> {
    // A bus but no GATEWAY_OWNER: there is nobody to attribute a decision
    // to, so the consent store never opens — and #52's guard closes the
    // whole API anyway, which is the answer the caller gets.
    let static_dir = companion_build("no-owner")?;
    let gateway = GatewayProc::start(&gateway_env_with(
        &static_dir,
        &[("GATEWAY_OWNER", ""), ("GATEWAY_NATS_URL", &nats_url())],
    ))?;
    let base = gateway.base_url().await?;

    let response = reqwest::get(format!("{base}/api/consent/state")).await?;
    assert_eq!(
        response.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "without an owner nobody can sign in, so nothing under /api answers"
    );
    let logs = poll_until(
        || async {
            let logs = gateway.logs().await.join("\n");
            logs.contains("GATEWAY_OWNER").then_some(logs)
        },
        "the Gateway to say that nobody can sign in",
    )
    .await?;
    assert!(
        !logs.contains("consent store ready"),
        "the consent store must not open without an owner:\n{logs}"
    );
    // And no consent series, because no consent store: an operator scraping
    // this Gateway sees that it writes no consent at all.
    let metrics = reqwest::get(format!("{base}/metrics"))
        .await?
        .text()
        .await?;
    assert!(!metrics.contains("consent"), "{metrics}");

    gateway.stop().await;
    Ok(())
}

/// Percent-encodes a query parameter value. Hand-rolled over the handful of
/// characters a Matrix user ID needs, so the suite adds no dependency for it.
fn urlencoding(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '@' => "%40".to_owned(),
            ':' => "%3A".to_owned(),
            '+' => "%2B".to_owned(),
            '/' => "%2F".to_owned(),
            other => other.to_string(),
        })
        .collect()
}
