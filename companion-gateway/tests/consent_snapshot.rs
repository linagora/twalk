//! Ticket #50, the consent snapshot: the Gateway serves the whole current
//! consent state with the JetStream sequence it reflects, authenticated by a
//! service token, and a consumer that applies the snapshot and then starts
//! its stream consumer at that sequence plus one sees every later decision
//! exactly once — no overlap, no gap.
//!
//! That last property is the reason the ticket exists, and it is what
//! [`a_cold_consumer_applies_the_snapshot_then_the_stream_exactly_once`]
//! proves: it is the Sensor's cold start (#51), driven from the test instead
//! of from the Sensor, against a real bus.
//!
//! The seam is the one every suite of this crate uses: a real Synapse (the
//! device that takes the decisions signs in with a real OpenID token, #52),
//! a real NATS JetStream, the real Gateway binary configured through its
//! environment, HTTP calls from the test. The bus is shared with every other
//! suite and run, so every assertion about the stream is about *this run's*
//! own subjects and event ids — as a real consumer's would not have to be,
//! and as a test on a shared bus must.

mod harness;

use std::collections::HashMap;

use anyhow::{Context, Result};
use harness::{
    companion_build, ensure_stack, gateway_env_with, gateway_env_with_consent, nats_url,
    owner_user_id, poll_until, signed_in_device_token, Bus, GatewayProc, StoredMessage,
    CONSENT_STREAM, CONSENT_SUBJECT, SERVER_NAME, SERVICE_TOKEN,
};
use serde_json::{json, Value};

/// A Gateway that writes consent and serves the snapshot, signed in on one
/// device, against the test stack's bus.
struct Fixture {
    gateway: GatewayProc,
    base: String,
    device_token: String,
    bus: Bus,
    /// The environment this Gateway was started with, and the static
    /// directory its state directory is derived from — so that
    /// [`Fixture::restart_with`] can bring another Gateway up **on the same
    /// store**, which is how the upgrade #149 is about is reproduced.
    env: Vec<(String, String)>,
}

impl Fixture {
    async fn start(test_name: &str) -> Result<Self> {
        Self::start_with(test_name, &[]).await
    }

    /// Starts a Gateway with the consent configuration plus per-test
    /// overrides — the cap, for the test that exceeds it; the owner's
    /// confirmed ghosts, for the test that upgrades across #149.
    async fn start_with(test_name: &str, overrides: &[(&str, &str)]) -> Result<Self> {
        ensure_stack().await?;
        let bus = Bus::connect().await?;
        // Whichever component runs first creates the stream, identically.
        bus.ensure_stream(CONSENT_STREAM, &["twalk.>"]).await?;

        let static_dir = companion_build(test_name)?;
        let env = gateway_env_with_consent(&static_dir, &nats_url());
        Self::start_from(env, overrides, bus).await
    }

    async fn start_from(
        mut env: Vec<(String, String)>,
        overrides: &[(&str, &str)],
        bus: Bus,
    ) -> Result<Self> {
        for (key, value) in overrides {
            match env.iter_mut().find(|(existing, _)| existing == key) {
                Some(entry) => entry.1 = (*value).to_owned(),
                None => env.push(((*key).to_owned(), (*value).to_owned())),
            }
        }
        let gateway = GatewayProc::start(&env)?;
        let base = gateway.base_url().await?;
        let device_token = signed_in_device_token(&base).await?;
        Ok(Self {
            gateway,
            base,
            device_token,
            bus,
            env,
        })
    }

    /// Stops this Gateway and starts another one on the same state directory
    /// and the same bus, with these variables added or changed.
    ///
    /// This is an **upgrade**, not a second deployment: the consent journal,
    /// the seen contacts and the session store are the ones the first Gateway
    /// wrote. It is the only way to build the store #149 is about — a row
    /// keyed on an identity the Gateway did not then know was the owner's —
    /// without reaching inside the binary to write one.
    async fn restart_with(self, overrides: &[(&str, &str)]) -> Result<Self> {
        let env = self.env.clone();
        self.gateway.stop().await;
        // A connection of its own rather than the stopped fixture's: the bus
        // is the test stack's, and reconnecting to it is cheaper than making
        // the harness's `Bus` cloneable for one test.
        Self::start_from(env, overrides, Bus::connect().await?).await
    }

    /// The write API's own answer, whatever it is: the status and the body,
    /// for a test asserting a refusal rather than a decision.
    async fn decide_attempt(&self, body: Value) -> Result<(reqwest::StatusCode, Value)> {
        let response = reqwest::Client::new()
            .post(format!("{}/api/consent/decisions", self.base))
            .header("cookie", format!("twalk_device={}", self.device_token))
            .json(&body)
            .send()
            .await
            .context("the write API did not answer")?;
        let status = response.status();
        Ok((status, response.json().await.unwrap_or(Value::Null)))
    }

    /// The contacts the Companion would be told are waiting for a decision.
    async fn pending_contacts(&self) -> Result<Value> {
        let response = reqwest::Client::new()
            .get(format!("{}/api/contacts/pending", self.base))
            .header("cookie", format!("twalk_device={}", self.device_token))
            .send()
            .await
            .context("the pending-contact endpoint did not answer")?;
        Ok(response.json().await?)
    }

    /// Records one decision through the write API and returns the event id
    /// the outbox will publish it under.
    async fn decide(&self, body: Value) -> Result<String> {
        let response = reqwest::Client::new()
            .post(format!("{}/api/consent/decisions", self.base))
            .header("cookie", format!("twalk_device={}", self.device_token))
            .json(&body)
            .send()
            .await
            .context("the write API did not answer")?;
        let status = response.status();
        let recorded: Value = response.json().await.unwrap_or(Value::Null);
        anyhow::ensure!(
            status == reqwest::StatusCode::CREATED,
            "the decision must be committed, got {status}: {recorded}"
        );
        Ok(recorded["event_id"]
            .as_str()
            .context("the answer names the event id")?
            .to_owned())
    }

    /// Reads the snapshot with the service token, as the Sensor does.
    async fn snapshot(&self) -> Result<Value> {
        let (status, body) = self.snapshot_with(Some(SERVICE_TOKEN), &[]).await?;
        anyhow::ensure!(
            status == reqwest::StatusCode::OK,
            "the snapshot must be served, got {status}: {body}"
        );
        Ok(body)
    }

    /// The snapshot request with whatever credentials a test wants to try.
    async fn snapshot_with(
        &self,
        bearer: Option<&str>,
        cookies: &[(&str, &str)],
    ) -> Result<(reqwest::StatusCode, Value)> {
        let mut request = reqwest::Client::new().get(format!("{}/api/consent/snapshot", self.base));
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        if !cookies.is_empty() {
            let header = cookies
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; ");
            request = request.header("cookie", header);
        }
        let response = request
            .send()
            .await
            .context("the snapshot endpoint did not answer")?;
        let status = response.status();
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, body))
    }

    /// The current state as the owner's own read reports it: every recorded
    /// decision, whether or not the bus has heard it.
    async fn recorded_state(&self) -> Result<Value> {
        let response = reqwest::Client::new()
            .get(format!("{}/api/consent/state", self.base))
            .header("cookie", format!("twalk_device={}", self.device_token))
            .send()
            .await?;
        Ok(response.json().await?)
    }

    /// Every consent event stored on the bus, with its stream sequence.
    async fn stored(&self) -> Result<Vec<StoredMessage>> {
        self.bus
            .fetch_all_with_headers(CONSENT_STREAM, CONSENT_SUBJECT)
            .await
    }

    /// Waits until the bus holds each of these event ids, and answers with
    /// the stream sequence of each: the point at which the Gateway's outbox
    /// has drained what this test asked for.
    async fn wait_for_published(&self, ids: &[&str]) -> Result<HashMap<String, u64>> {
        poll_until(
            || async {
                let stored = self.stored().await.ok()?;
                let found: HashMap<String, u64> = stored
                    .iter()
                    .filter_map(|message| {
                        let id = message.payload["id"].as_str()?;
                        ids.contains(&id).then(|| (id.to_owned(), message.sequence))
                    })
                    .collect();
                (found.len() == ids.len()).then_some(found)
            },
            &format!("{} consent event(s) on the bus", ids.len()),
        )
        .await
    }

    async fn stop(self) {
        self.gateway.stop().await;
    }
}

/// A contact id unique to this run: one bus and one consent subject are
/// shared by every suite.
fn contact(test_name: &str) -> String {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    format!("@whatsapp_g50_{test_name}_{unique}:{SERVER_NAME}")
}

fn grant(subject: &str, network: &str) -> Value {
    json!({
        "subject": { "type": "contact", "id": subject },
        "new_state": "granted",
        "scope": { "networks": [network] }
    })
}

fn revoke(subject: &str, network: &str) -> Value {
    json!({
        "subject": { "type": "contact", "id": subject },
        "new_state": "revoked",
        "scope": { "networks": [network] }
    })
}

/// The consent state as a consumer holds it: `(subject id, network)` to a
/// state. Built from a snapshot's entries, then moved on by the events the
/// stream delivers — which is the whole of what a consumer does with either.
type ConsentState = HashMap<(String, String), String>;

fn apply_entries(state: &mut ConsentState, entries: &Value) -> Result<()> {
    for entry in entries
        .as_array()
        .context("the entries are an array")?
        .iter()
    {
        let subject = entry["subject"]["id"]
            .as_str()
            .context("an entry names its subject")?
            .to_owned();
        let network = entry["network"]
            .as_str()
            .context("an entry names its network")?
            .to_owned();
        let state_value = entry["state"]
            .as_str()
            .context("an entry names its state")?
            .to_owned();
        state.insert((subject, network), state_value);
    }
    Ok(())
}

fn apply_event(state: &mut ConsentState, event: &Value) -> Result<()> {
    let subject = event["data"]["subject"]["id"]
        .as_str()
        .context("an event names its subject")?
        .to_owned();
    let new_state = event["data"]["new_state"]
        .as_str()
        .context("an event names the state it moves to")?
        .to_owned();
    for network in event["data"]["scope"]["networks"]
        .as_array()
        .context("an event names its scope")?
    {
        let network = network
            .as_str()
            .context("a scoped network is a string")?
            .to_owned();
        state.insert((subject.clone(), network), new_state.clone());
    }
    Ok(())
}

/// One consent state restricted to the subjects of this run: the bus is
/// shared, so everything else on it is noise this test must not assert on.
fn ours(state: &ConsentState, subjects: &[&str]) -> ConsentState {
    state
        .iter()
        .filter(|((subject, _), _)| subjects.contains(&subject.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// The property the ticket exists for. A consumer with a cold cache reads the
/// snapshot, applies it, creates its stream consumer at the sequence the
/// snapshot names plus one, and must then see every decision taken after the
/// snapshot exactly once — and none of the decisions the snapshot already
/// told it about.
#[tokio::test]
async fn a_cold_consumer_applies_the_snapshot_then_the_stream_exactly_once() -> Result<()> {
    let fixture = Fixture::start("snapshot-handoff").await?;
    let (x, y, z) = (
        contact("handoff-x"),
        contact("handoff-y"),
        contact("handoff-z"),
    );

    // --- before the snapshot: two contacts and a network-wide default.
    let granted_x = fixture.decide(grant(&x, "whatsapp")).await?;
    let granted_y = fixture.decide(grant(&y, "whatsapp")).await?;
    let default_signal = fixture
        .decide(json!({
            "subject": { "type": "network", "id": "signal" },
            "new_state": "granted",
            "scope": { "networks": ["signal"] }
        }))
        .await?;
    let before = fixture
        .wait_for_published(&[
            granted_x.as_str(),
            granted_y.as_str(),
            default_signal.as_str(),
        ])
        .await?;

    // --- the snapshot, as a consumer with nothing in its cache reads it.
    let snapshot = fixture.snapshot().await?;
    let position = snapshot["stream_sequence"]
        .as_u64()
        .context("the snapshot names its stream position")?;
    assert_eq!(
        snapshot["next_stream_sequence"].as_u64(),
        Some(position + 1),
        "the start sequence is spelled out so no consumer computes it wrong: {snapshot}"
    );
    assert_eq!(snapshot["stream"].as_str(), Some(CONSENT_STREAM));
    assert_eq!(snapshot["subject"].as_str(), Some(CONSENT_SUBJECT));
    assert_eq!(
        position, before[&default_signal],
        "the position is where the bus stored the last decision the snapshot reflects: {snapshot}"
    );
    // The network-wide default is in it, as much an entry as a contact's own
    // decision — a consumer that ignored it would ask about every contact.
    assert!(
        snapshot["entries"]
            .as_array()
            .context("the snapshot holds entries")?
            .iter()
            .any(
                |entry| entry["subject"] == json!({ "type": "network", "id": "signal" })
                    && entry["state"] == json!("granted")
            ),
        "the network default is part of the state a consumer starts from: {snapshot}"
    );
    let mut applied = ConsentState::new();
    apply_entries(&mut applied, &snapshot["entries"])?;
    assert_eq!(
        ours(&applied, &[x.as_str(), y.as_str()]),
        ConsentState::from([
            ((x.clone(), "whatsapp".to_owned()), "granted".to_owned()),
            ((y.clone(), "whatsapp".to_owned()), "granted".to_owned()),
        ]),
        "the snapshot is the state as recorded: {snapshot}"
    );

    // --- after the snapshot: one revocation and one new contact. A
    // revocation is the case the hand-off must not lose — an absent subject
    // means "never decided", so a consumer that missed this event would go on
    // processing a contact the user shut out.
    let revoked_x = fixture.decide(revoke(&x, "whatsapp")).await?;
    let granted_z = fixture.decide(grant(&z, "telegram")).await?;
    fixture
        .wait_for_published(&[revoked_x.as_str(), granted_z.as_str()])
        .await?;

    // --- the hand-off: a durable consumer created at the snapshot's
    // sequence plus one, exactly as ADR 0010 describes and as the Sensor
    // will (#51).
    let delivered = fixture
        .bus
        .consume_from(CONSENT_STREAM, CONSENT_SUBJECT, position + 1, 1_000)
        .await?;
    let delivered_ids: Vec<&str> = delivered
        .iter()
        .filter_map(|message| message.payload["id"].as_str())
        .collect();

    // No overlap: nothing the snapshot already accounted for is delivered
    // again.
    for (already_applied, what) in [
        (granted_x.as_str(), "a grant the snapshot held"),
        (granted_y.as_str(), "a grant the snapshot held"),
        (
            default_signal.as_str(),
            "the network default the snapshot held",
        ),
    ] {
        assert!(
            !delivered_ids.contains(&already_applied),
            "{what} was delivered again after the snapshot: the consumer would \
             apply it twice"
        );
    }
    // No gap, and exactly once: each decision taken after the snapshot is
    // delivered, and delivered one single time.
    for (after, what) in [
        (revoked_x.as_str(), "the revocation"),
        (granted_z.as_str(), "the new contact's grant"),
    ] {
        assert_eq!(
            delivered_ids.iter().filter(|id| ***id == *after).count(),
            1,
            "{what} must be delivered exactly once from the snapshot's position \
             plus one; the consumer saw {delivered_ids:?}"
        );
    }
    // Every message the consumer was handed really is after the position: a
    // consumer that created its consumer here read nothing twice, whatever
    // else was on the shared bus.
    for message in &delivered {
        assert!(
            message.sequence > position,
            "a message at {} was delivered from {}: {}",
            message.sequence,
            position + 1,
            message.payload
        );
    }

    // --- and the consumer's state now agrees with the Gateway's own, which
    // is the whole point of the hand-off.
    for message in &delivered {
        apply_event(&mut applied, &message.payload)?;
    }
    let recorded = fixture.recorded_state().await?;
    let mut gateway_state = ConsentState::new();
    apply_entries(&mut gateway_state, &recorded["entries"])?;
    let subjects = [x.as_str(), y.as_str(), z.as_str()];
    assert_eq!(
        ours(&applied, &subjects),
        ours(&gateway_state, &subjects),
        "snapshot then stream must converge on the Gateway's own state:\n\
         consumer: {:?}\ngateway: {:?}",
        ours(&applied, &subjects),
        ours(&gateway_state, &subjects)
    );
    assert_eq!(
        applied.get(&(x.clone(), "whatsapp".to_owned())),
        Some(&"revoked".to_owned()),
        "the revocation taken after the snapshot is what the consumer ends up with"
    );

    fixture.stop().await;
    Ok(())
}

/// The other half of the same property, from the position's side: whatever a
/// snapshot's position is, the snapshot's content is exactly the fold of the
/// stream up to it. Each iteration reads the snapshot immediately after
/// taking a decision — into the window where the outbox may or may not have
/// published it — so the assertion has to hold whichever side of the read the
/// publication fell on.
#[tokio::test]
async fn the_snapshots_position_reflects_exactly_the_stream_prefix_it_names() -> Result<()> {
    let fixture = Fixture::start("snapshot-position").await?;
    let subject = contact("position");

    for (round, state) in ["granted", "revoked", "granted", "pending"]
        .into_iter()
        .enumerate()
    {
        fixture
            .decide(json!({
                "subject": { "type": "contact", "id": &subject },
                "new_state": state,
                "scope": { "networks": ["whatsapp"] }
            }))
            .await?;
        // Deliberately no wait: the decision is committed, and whether it has
        // reached the bus yet is a race this test wants to run into.
        let snapshot = fixture.snapshot().await?;
        let position = snapshot["stream_sequence"]
            .as_u64()
            .context("the snapshot names its stream position")?;

        let mut from_stream = ConsentState::new();
        let mut stored = fixture.stored().await?;
        stored.sort_by_key(|message| message.sequence);
        for message in stored.iter().filter(|message| message.sequence <= position) {
            apply_event(&mut from_stream, &message.payload)?;
        }
        let mut from_snapshot = ConsentState::new();
        apply_entries(&mut from_snapshot, &snapshot["entries"])?;

        assert_eq!(
            ours(&from_snapshot, &[subject.as_str()]),
            ours(&from_stream, &[subject.as_str()]),
            "round {round}: the snapshot's content must be the fold of the stream up \
             to the position it names — a decision committed and published between the \
             two reads would show up here as a disagreement.\nsnapshot: {snapshot}"
        );
    }

    fixture.stop().await;
    Ok(())
}

/// The two credentials are disjoint. The Sensor is not a device and has no
/// OpenID token to sign in with, so the snapshot takes a service token — and
/// a device token, which opens every other endpoint, opens nothing here.
#[tokio::test]
async fn a_device_token_and_a_service_token_open_strictly_different_doors() -> Result<()> {
    let fixture = Fixture::start("snapshot-auth").await?;

    // The service token: in.
    let (status, _) = fixture.snapshot_with(Some(SERVICE_TOKEN), &[]).await?;
    assert_eq!(status, reqwest::StatusCode::OK);

    // A device token, in the cookie every other endpoint takes, and nothing
    // else: out. This is the acceptance criterion spelled out — a device
    // token must not be accepted here.
    let (status, body) = fixture
        .snapshot_with(None, &[("twalk_device", fixture.device_token.as_str())])
        .await?;
    assert_eq!(
        status,
        reqwest::StatusCode::UNAUTHORIZED,
        "a device token must not read the snapshot: {body}"
    );
    assert_eq!(body["error"].as_str(), Some("unauthenticated"));
    // Nor as a bearer credential, where it would be a token of the right
    // shape presented in the right place.
    let (status, _) = fixture
        .snapshot_with(Some(fixture.device_token.as_str()), &[])
        .await?;
    assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
    // No credential at all, a wrong token, and a token that is a prefix of
    // the right one: the same answer to each.
    for bearer in [
        None,
        Some("not-the-service-token-at-all-0123456789"),
        Some(&SERVICE_TOKEN[..16]),
    ] {
        let (status, body) = fixture.snapshot_with(bearer, &[]).await?;
        assert_eq!(
            status,
            reqwest::StatusCode::UNAUTHORIZED,
            "bearer {bearer:?} must be refused: {body}"
        );
        assert_eq!(body["error"].as_str(), Some("unauthenticated"));
    }

    // And the other direction: the service token opens no other endpoint,
    // including the read that returns the same entries behind a device token.
    for path in [
        "/api/consent/state",
        "/api/consent/effective?contact=%40a%3Atest.twalk&network=whatsapp",
        "/api/devices",
        "/api/session",
    ] {
        let response = reqwest::Client::new()
            .get(format!("{}{path}", fixture.base))
            .bearer_auth(SERVICE_TOKEN)
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "the service token must not open {path}"
        );
    }

    fixture.stop().await;
    Ok(())
}

/// The cap fails loudly. There is no pagination, so the only two honest
/// answers to a state larger than the Gateway will serve are the whole thing
/// and an error — and a truncated snapshot would tell a consumer that
/// contacts the user granted were never decided about.
#[tokio::test]
async fn a_state_over_the_cap_is_refused_whole_rather_than_truncated() -> Result<()> {
    let fixture = Fixture::start_with(
        "snapshot-cap",
        &[("GATEWAY_CONSENT_SNAPSHOT_MAX_ENTRIES", "1")],
    )
    .await?;
    let first = contact("cap-first");
    let second = contact("cap-second");

    let first_id = fixture.decide(grant(&first, "whatsapp")).await?;
    fixture.wait_for_published(&[&first_id]).await?;
    // At the cap, the snapshot is served.
    let snapshot = fixture.snapshot().await?;
    assert_eq!(
        snapshot["entries"].as_array().map(Vec::len),
        Some(1),
        "{snapshot}"
    );

    // Over it, refused — with nothing in the body a consumer could mistake
    // for a state.
    let second_id = fixture.decide(grant(&second, "whatsapp")).await?;
    fixture.wait_for_published(&[&second_id]).await?;
    let (status, body) = poll_until(
        || async {
            let (status, body) = fixture.snapshot_with(Some(SERVICE_TOKEN), &[]).await.ok()?;
            (status != reqwest::StatusCode::OK).then_some((status, body))
        },
        "the snapshot to be refused once the state is over the cap",
    )
    .await?;
    assert_eq!(status, reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"].as_str(), Some("snapshot_too_large"), "{body}");
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("GATEWAY_CONSENT_SNAPSHOT_MAX_ENTRIES")),
        "the refusal names the variable an operator would raise: {body}"
    );
    assert!(
        body.get("entries").is_none(),
        "a refusal carries no entries at all: {body}"
    );

    // The decisions themselves were never at risk: the owner's own read
    // still answers, because the cap is the snapshot's and not the store's.
    let recorded = fixture.recorded_state().await?;
    assert_eq!(recorded["entries"].as_array().map(Vec::len), Some(2));

    fixture.stop().await;
    Ok(())
}

/// Each half of the configuration is independently missing, and the answer
/// names the one that is: an operator who set a bus and no service token, or
/// a token and no bus, is told which.
#[tokio::test]
async fn an_unconfigured_snapshot_names_the_variable_that_would_open_it() -> Result<()> {
    ensure_stack().await?;

    // A Gateway that writes consent and serves no snapshot: no service
    // token. Answered before authentication, so there is nothing to present.
    let static_dir = companion_build("snapshot-no-token")?;
    let mut env = gateway_env_with_consent(&static_dir, &nats_url());
    env.retain(|(key, _)| key != "GATEWAY_SERVICE_TOKEN");
    let gateway = GatewayProc::start(&env)?;
    let base = gateway.base_url().await?;
    let response = reqwest::Client::new()
        .get(format!("{base}/api/consent/snapshot"))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await?;
    assert_eq!(
        body["error"].as_str(),
        Some("service_token_not_configured"),
        "{body}"
    );
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("GATEWAY_SERVICE_TOKEN")),
        "{body}"
    );
    gateway.stop().await;

    // A Gateway with the token and no bus: the snapshot authenticates the
    // caller and then says the consent store is what is missing.
    let static_dir = companion_build("snapshot-no-bus")?;
    let gateway = GatewayProc::start(&gateway_env_with(&static_dir, &[]))?;
    let base = gateway.base_url().await?;
    let response = reqwest::Client::new()
        .get(format!("{base}/api/consent/snapshot"))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await?;
    assert_eq!(
        body["error"].as_str(),
        Some("consent_not_configured"),
        "{body}"
    );
    // And an unauthenticated caller learns nothing about that: the refusal
    // comes first.
    let response = reqwest::get(format!("{base}/api/consent/snapshot")).await?;
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    gateway.stop().await;
    Ok(())
}

/// A service token too short to be a secret stops the Gateway, loudly. It is
/// a bearer credential over the user's whole social graph: unlike a missing
/// variable, which is a deployment an operator has not finished, a weak one
/// is a deliberate act with a consequence they cannot see.
#[tokio::test]
async fn a_service_token_too_short_to_be_a_secret_stops_the_gateway() -> Result<()> {
    let static_dir = companion_build("snapshot-weak-token")?;
    let mut gateway = GatewayProc::start(&gateway_env_with(
        &static_dir,
        &[("GATEWAY_SERVICE_TOKEN", "hunter2")],
    ))?;
    let status = gateway.wait_for_exit().await?;
    assert!(
        !status.success(),
        "the Gateway must refuse to start with a guessable service token"
    );
    let logs = gateway.logs().await.join("\n");
    assert!(
        logs.contains("GATEWAY_SERVICE_TOKEN"),
        "the failure names the variable at fault:\n{logs}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Ticket #149: the owner is never a subject of the snapshot
// ---------------------------------------------------------------------------

/// The test the ticket asks for, and it asserts the **snapshot's contents**
/// rather than the handler's behaviour: ask for the snapshot, and the owner is
/// absent from it. A test of the write path alone would leave the read path —
/// the one every consumer uses — unproven, and the read path is where the
/// defect is: #109 and #147 stopped the Sensor *producing* events about the
/// owner, and neither could stop this Gateway *serving* a row about them.
///
/// The row is built the way a real deployment built its own: by a Gateway for
/// which that ghost was an ordinary contact, which is what every Gateway before
/// this ticket was. Then the Gateway is upgraded — the same store, the same bus,
/// one new variable — and asked the same question.
#[tokio::test]
async fn the_snapshot_never_serves_an_owner_identity_as_a_subject() -> Result<()> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    // The shape the reference deployment actually produced (#109): a LID ghost
    // of the owner's own WhatsApp account, indistinguishable from a contact's.
    let owner_ghost = format!("@whatsapp_lid-g149a{unique}:{SERVER_NAME}");
    // A second one the operator has *not* confirmed yet — the honest half of a
    // read-time exclusion: it is still served, because unknown is not the owner.
    let unconfirmed_ghost = format!("@whatsapp_lid-g149b{unique}:{SERVER_NAME}");
    let real_contact = format!("@whatsapp_g149c{unique}:{SERVER_NAME}");

    // --- before: a Gateway that has not been told whose ghost that is.
    let before = Fixture::start("snapshot-owner").await?;
    let owners_row = before.decide(grant(&owner_ghost, "whatsapp")).await?;
    let unconfirmed_row = before.decide(grant(&unconfirmed_ghost, "whatsapp")).await?;
    let contacts_row = before.decide(grant(&real_contact, "whatsapp")).await?;
    // Published, so all three are inside the snapshot's horizon: otherwise the
    // owner's absence below would hold for the wrong reason.
    before
        .wait_for_published(&[
            owners_row.as_str(),
            unconfirmed_row.as_str(),
            contacts_row.as_str(),
        ])
        .await?;
    let served = before.snapshot().await?;
    assert!(
        served["entries"].as_array().is_some_and(|entries| entries
            .iter()
            .any(|entry| entry["subject"]["id"] == json!(owner_ghost))),
        "the deployment this ticket is about: the snapshot serves the owner's own \
         ghost as a subject with a consent state — {served}"
    );

    // --- the upgrade: the same store, the same bus, and the one thing the
    // Gateway did not know.
    let after = before
        .restart_with(&[("GATEWAY_OWNER_IDENTITIES", owner_ghost.as_str())])
        .await?;
    let snapshot = after.snapshot().await?;

    // The assertion the ticket names. Not "the handler refuses a write": the
    // snapshot's own contents.
    let subjects: Vec<&str> = snapshot["entries"]
        .as_array()
        .context("the snapshot lists entries")?
        .iter()
        .filter_map(|entry| entry["subject"]["id"].as_str())
        .collect();
    assert!(
        !subjects.contains(&owner_ghost.as_str()),
        "the owner is not a subject of the consent snapshot: {snapshot}"
    );
    // Said once more over the entries' own bytes, because "absent from the
    // field I looked in" is a weaker claim than "absent". Over `entries` and
    // not the whole document, deliberately: the ghost *is* named in
    // `owner_identities`, which is the point — the snapshot says who has no
    // consent state, and then holds no consent state for them.
    assert!(
        !serde_json::to_string(&snapshot["entries"])?.contains(&owner_ghost),
        "and appears nowhere in the state at all: {snapshot}"
    );
    // A real contact's row is untouched: this is an exclusion, not a switch.
    assert!(
        subjects.contains(&real_contact.as_str()),
        "the contacts the user did decide about are still served: {snapshot}"
    );
    // And the row about a ghost nobody has confirmed is still served — the
    // exclusion is exactly the set the deployment handed over, and unknown is
    // not the owner. This is what an operator has to know: a row about an
    // identity that *was* theirs stays visible until they add it to the
    // variable, and then it disappears with no migration.
    assert!(
        subjects.contains(&unconfirmed_ghost.as_str()),
        "an unconfirmed ghost is a contact like any other: {snapshot}"
    );

    // The set itself, served beside the state: it cannot be derived from a
    // bridge, so the single writer of consent state hands it over.
    let identities = snapshot["owner_identities"]
        .as_array()
        .context("the snapshot names the owner's identities")?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(
        identities.contains(&owner_ghost.as_str()),
        "the confirmed ghost is one of them: {snapshot}"
    );
    let owner = owner_user_id();
    assert!(
        identities.contains(&owner.as_str()),
        "and so is the owner's own Matrix ID, always: {snapshot}"
    );
    assert!(
        !identities.contains(&unconfirmed_ghost.as_str()),
        "and nothing the deployment did not confirm: {snapshot}"
    );

    // The owner's own read of the state agrees with the snapshot — one fact,
    // not two — and the pending list never proposes the owner as a contact
    // awaiting a decision.
    let state = after.recorded_state().await?;
    assert!(
        !serde_json::to_string(&state)?.contains(&owner_ghost),
        "GET /api/consent/state serves no row about the owner either: {state}"
    );
    let pending = after.pending_contacts().await?;
    assert!(
        !serde_json::to_string(&pending)?.contains(&owner_ghost),
        "and the user is never offered a decision about their own ghost: {pending}"
    );

    // Writing one now is refused, with a code and a sentence — and the code is
    // not one a client could read as "no such subject".
    let (status, refusal) = after
        .decide_attempt(revoke(&owner_ghost, "whatsapp"))
        .await?;
    assert_eq!(
        status,
        reqwest::StatusCode::CONFLICT,
        "a decision about the owner is refused, not accepted and not a 404: {refusal}"
    );
    assert_eq!(
        refusal["error"].as_str(),
        Some("subject_is_the_owner"),
        "{refusal}"
    );
    let detail = refusal["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains(&owner_ghost) && detail.contains("consent state"),
        "the refusal says which subject and why: {refusal}"
    );
    // The same refusal on the read that resolves the precedence: `pending`
    // with `decided_by: null` would say "nobody has decided yet", which about
    // the owner is an invitation to decide something that cannot be decided.
    let response = reqwest::Client::new()
        .get(format!(
            "{}/api/consent/effective?contact={}&network=whatsapp",
            after.base,
            owner_ghost.replace('@', "%40").replace(':', "%3A")
        ))
        .header("cookie", format!("twalk_device={}", after.device_token))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: Value = response.json().await?;
    assert_eq!(
        body["error"].as_str(),
        Some("subject_is_the_owner"),
        "{body}"
    );

    // And `consent.state.changed` was never emitted about the owner by the
    // upgraded Gateway: the refusal is at the writer, so nothing reached the
    // journal the outbox drains. The one event about that ghost on the bus is
    // the grant the *old* Gateway published, which is the history this ticket
    // deliberately does not rewrite.
    let events: Vec<Value> = after
        .stored()
        .await?
        .into_iter()
        .filter(|message| message.payload["subject"] == json!(owner_ghost))
        .map(|message| message.payload)
        .collect();
    assert_eq!(
        events.len(),
        1,
        "exactly the one the pre-#149 Gateway published, and no revocation: {events:?}"
    );
    assert_eq!(events[0]["id"].as_str(), Some(owners_row.as_str()));

    // The rows are still there, and the operator is told so rather than left
    // to find out: the journal is append-only, so withholding is the honest
    // treatment and hiding is not.
    let metrics = reqwest::get(format!("{}/metrics", after.base))
        .await?
        .text()
        .await?;
    assert!(
        metrics.contains("twalk_companion_gateway_owner_consent_rows 1"),
        "the withheld row is counted where an operator scrapes: {metrics}"
    );
    assert!(
        metrics.contains("twalk_companion_gateway_owner_decision_refusals_total 1"),
        "and so is the refusal: {metrics}"
    );
    let logs = after.gateway.logs().await.join("\n");
    assert!(
        logs.contains("holds decisions about the owner"),
        "and it is said at startup, where an operator reading a log will meet it:\n{logs}"
    );

    after.stop().await;
    Ok(())
}
