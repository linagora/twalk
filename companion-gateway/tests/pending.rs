//! Ticket #54, the pending-contact projection at the Gateway's process
//! boundary: the real binary, the real NATS JetStream the Sensor publishes
//! to, real inbound events, and HTTP calls from the test.
//!
//! Five properties, one per test:
//!
//! 1. **a Gateway installed after weeks of traffic is not empty.** The
//!    durable consumer is created with full delivery, so the events published
//!    before this Gateway existed build its first list.
//! 2. **a decision moves the contact out of the list** — recorded through
//!    #49's write API, which is how a user answers.
//! 3. **the store holds no body and no identifier.** Asserted against the
//!    bytes of the database file and the captured logs, in the spirit of
//!    #52's "no Matrix access token is stored or logged".
//! 4. **a restart resumes at the ack floor** instead of replaying the stream
//!    into a store that already holds it.
//! 5. **display names come from the bus and are written nowhere.**
//!
//! The events here are published by the test rather than by a Sensor process:
//! the Gateway's seam is the bus, and what crosses it is a contract-valid
//! CloudEvent, which every event below is checked to be. The Sensor's own
//! half — that a real Sensor in the reference deployment produces events this
//! projection turns into pending entries — is
//! `tests/deployment.rs::the_deployed_gateway_creates_the_one_account_and_the_sensor_joins_what_it_is_invited_to`,
//! where a real Sensor, a real Synapse and a real Gateway run in one compose
//! stack.
//!
//! The bus is shared by every suite and every run, so nothing here asserts on
//! totals: each test invents its own contacts and asks about those.

mod harness;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use harness::{
    companion_build, ensure_stack, gateway_env_with_consent, gateway_state_dir, nats_url,
    owner_user_id, poll_until, unreachable_nats_url, validate_against_contract, Bus, GatewayProc,
    SERVER_NAME,
};
use serde_json::{json, Value};

/// The bus the Sensor publishes inbound messages to
/// (`sensor/src/normalize.rs`).
const STREAM: &str = "twalk";
const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";
const MESSAGE_TYPE: &str = "fr.linagora.twalk.inbound.message.received.v1";

/// The strings that must never reach the Gateway's store or its logs. They
/// are deliberately unmistakable: a substring search over a binary file finds
/// them if they are there, whatever encoded them.
const SECRET_BODY: &str = "on-decale-a-20h-le-corps-du-message-g54";
const SECRET_IDENTIFIER: &str = "+33600000054";
const SECRET_DISPLAY_NAME: &str = "Aicha Benali G54";

// ---------------------------------------------------------------------------
// Driving the seam
// ---------------------------------------------------------------------------

/// A suffix no other run of this suite shares: the bus keeps every event
/// every run published, so a test that reused a contact ID would be asserting
/// on somebody else's traffic.
fn unique(label: &str) -> String {
    format!(
        "{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos()
    )
}

/// A ghost user's Matrix ID, spelled the way a bridge materialises one — and
/// the reason residual risk 5 exists: the network identifier is in the
/// localpart.
fn ghost(network: &str, label: &str) -> String {
    format!("@{network}_{}:{SERVER_NAME}", unique(label))
}

/// One `inbound.message.received.v1` as the Sensor publishes it: the full
/// shape, carrying a body, a display name and a `network_identifier` — all
/// the things the Gateway must be shown and must not keep.
fn inbound_event(subject: &str, network: &str, at: &str, body: &str, display_name: &str) -> Value {
    let id = harness::sha256_hex(&format!("{subject}:{network}:{at}:{body}"));
    let room: String = harness::sha256_hex(&format!("room:{subject}:{at}"))
        .chars()
        .take(18)
        .collect();
    json!({
        "specversion": "1.0",
        "id": id,
        "source": format!("matrix://{SERVER_NAME}/!{room}:{SERVER_NAME}"),
        "type": MESSAGE_TYPE,
        "time": at,
        "subject": subject,
        "datacontenttype": "application/json",
        "dataschema": "https://schemas.twalk.dev/cloudevents/v1/inbound.message.received.schema.json",
        "network": network,
        "connection": network,
        "consent": "pending",
        "data": {
            "body": body,
            "format": "text/plain",
            "reply_to": null,
            "attachments": [],
            "contact": {
                "display_name": display_name,
                "network_identifier": SECRET_IDENTIFIER
            }
        }
    })
}

/// Publishes one inbound event, after checking it is one the contract allows:
/// a projection asserted against a fixture the contract would reject proves
/// nothing.
async fn publish(bus: &Bus, event: &Value) -> Result<()> {
    validate_against_contract(event, "inbound.message.received")?;
    bus.publish_event(MESSAGE_SUBJECT, event).await
}

/// The bus, with the stream the Sensor declares — ensured here because a cold
/// test stack has no stream until something publishes.
async fn bus() -> Result<Bus> {
    let bus = Bus::connect().await?;
    bus.ensure_stream(STREAM, &["twalk.>"]).await?;
    Ok(bus)
}

struct Running {
    gateway: GatewayProc,
    base: String,
    static_dir: PathBuf,
    device: String,
    http: reqwest::Client,
}

impl Running {
    /// A fully configured Gateway with a bus, signed in.
    async fn start(test_name: &str) -> Result<Self> {
        let static_dir = companion_build(test_name)?;
        Self::start_on(static_dir, &nats_url()).await
    }

    /// The same, on a static directory the caller already has — which is what
    /// makes a restart a restart: the state directory and the durable
    /// consumer name both derive from it.
    async fn start_on(static_dir: PathBuf, nats_url: &str) -> Result<Self> {
        let gateway = GatewayProc::start(&gateway_env_with_consent(&static_dir, nats_url))?;
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
        let device = harness::signed_in_device_token(&base).await?;
        Ok(Self {
            gateway,
            base,
            static_dir,
            device,
            http: reqwest::Client::new(),
        })
    }

    fn state_dir(&self) -> PathBuf {
        gateway_state_dir(&self.static_dir)
    }

    async fn get(&self, path: &str) -> Result<reqwest::Response> {
        self.http
            .get(format!("{}{path}", self.base))
            .header(
                reqwest::header::COOKIE,
                format!("twalk_device={}", self.device),
            )
            .send()
            .await
            .with_context(|| format!("failed to call GET {path}"))
    }

    async fn pending(&self) -> Result<Value> {
        let response = self.get("/api/contacts/pending").await?;
        anyhow::ensure!(
            response.status().as_u16() == 200,
            "the pending list was refused with {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
        Ok(response.json().await?)
    }

    /// Whether the pending list holds this contact, and on which networks.
    async fn waiting_networks(&self, contact: &str) -> Result<Vec<String>> {
        Ok(self.pending().await?["contacts"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter(|entry| entry["contact"].as_str() == Some(contact))
            .filter_map(|entry| entry["network"].as_str().map(str::to_owned))
            .collect())
    }

    /// Polls until this contact is waiting for a decision on this network.
    async fn wait_until_pending(&self, contact: &str, network: &str) -> Result<Value> {
        poll_until(
            || async {
                self.pending()
                    .await
                    .ok()?
                    .get("contacts")?
                    .as_array()?
                    .iter()
                    .find(|entry| {
                        entry["contact"].as_str() == Some(contact)
                            && entry["network"].as_str() == Some(network)
                    })
                    .cloned()
            },
            &format!("{contact} to appear in the pending list on {network}"),
        )
        .await
    }

    /// Records a consent decision the way the user does (#49).
    async fn decide(&self, contact: &str, state: &str, network: &str) -> Result<()> {
        let response = self
            .http
            .post(format!("{}/api/consent/decisions", self.base))
            .header(
                reqwest::header::COOKIE,
                format!("twalk_device={}", self.device),
            )
            .json(&json!({
                "subject": { "type": "contact", "id": contact },
                "new_state": state,
                "scope": { "networks": [network] }
            }))
            .send()
            .await?;
        anyhow::ensure!(
            response.status().as_u16() == 201,
            "the decision was refused with {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
        Ok(())
    }

    async fn metric(&self, name: &str) -> Result<u64> {
        let body = reqwest::get(format!("{}/metrics", self.base))
            .await?
            .text()
            .await?;
        Ok(harness::parse_exposition(&body)
            .into_iter()
            .find(|(sample, _)| sample == name)
            .map(|(_, value)| value)
            .unwrap_or_default())
    }

    async fn stop(self) {
        self.gateway.stop().await;
    }
}

/// Everything the Gateway wrote to its state directory, as one string — the
/// database, its write-ahead log and any sibling. The same helper
/// `tests/signin.rs` asserts "no Matrix access token is stored" with, for the
/// same reason: WAL mode means a row can live outside the database file for a
/// while, so the assertion covers the directory.
fn store_bytes(state_dir: &Path) -> Result<String> {
    let mut contents = String::new();
    for entry in std::fs::read_dir(state_dir)
        .with_context(|| format!("failed to read {}", state_dir.display()))?
    {
        let path = entry?.path();
        if path.is_file() {
            contents.push_str(&String::from_utf8_lossy(&std::fs::read(&path)?));
        }
    }
    Ok(contents)
}

// ---------------------------------------------------------------------------
// 1. A Gateway installed after weeks of traffic shows what it missed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_gateway_started_against_a_stream_with_history_builds_its_list_from_it() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;

    // Weeks of Sensor traffic, before this Gateway exists at all: one
    // contact that wrote twice on WhatsApp, one that wrote on Signal, and
    // the user's own messages in the same rooms.
    let talkative = ghost("whatsapp", "history_talkative");
    let quiet = ghost("signal", "history_quiet");
    publish(
        &bus,
        &inbound_event(
            &talkative,
            "whatsapp",
            "2026-09-01T08:00:00Z",
            "the first message",
            SECRET_DISPLAY_NAME,
        ),
    )
    .await?;
    publish(
        &bus,
        &inbound_event(
            &talkative,
            "whatsapp",
            "2026-09-08T19:30:00Z",
            "the last message",
            SECRET_DISPLAY_NAME,
        ),
    )
    .await?;
    publish(
        &bus,
        &inbound_event(
            &quiet,
            "signal",
            "2026-09-05T12:00:00Z",
            "hello",
            SECRET_DISPLAY_NAME,
        ),
    )
    .await?;
    publish(
        &bus,
        &inbound_event(
            &owner_user_id(),
            "whatsapp",
            "2026-09-06T12:00:00Z",
            "what the user said themselves",
            "The owner",
        ),
    )
    .await?;

    // Only now does the Gateway exist. Its durable consumer is created with
    // full delivery, so the weeks above are its first list rather than an
    // empty inbox.
    let gateway = Running::start("pending-history").await?;

    // Polled on the *second* message's instant: the two are separate
    // deliveries, so the contact appears when the first is projected and its
    // latest sighting moves when the second is.
    let entry = poll_until(
        || async {
            let entry = gateway
                .wait_until_pending(&talkative, "whatsapp")
                .await
                .ok()?;
            (entry["last_seen"].as_str() == Some("2026-09-08T19:30:00.000Z")).then_some(entry)
        },
        "the latest sighting to move with the second message",
    )
    .await?;
    assert_eq!(
        entry["first_seen"].as_str(),
        Some("2026-09-01T08:00:00.000Z"),
        "the first sighting is the event's own time, not the Gateway's clock: {entry}"
    );
    // Four values and no fifth, at the API as in the store.
    let mut members: Vec<&String> = entry.as_object().context("an object")?.keys().collect();
    members.sort();
    assert_eq!(
        members,
        vec!["contact", "first_seen", "last_seen", "network"],
        "a pending contact is an ID, a network and two instants: {entry}"
    );

    gateway.wait_until_pending(&quiet, "signal").await?;

    // The user is not their own correspondent: their messages travel through
    // the same rooms and the Sensor publishes them like any other, and the
    // Gateway never records them.
    assert_eq!(
        gateway.waiting_networks(&owner_user_id()).await?,
        Vec::<String>::new(),
        "the owner must never be a decision the owner has to take"
    );

    // The dashboard's numbers: the total counts the whole list, and the
    // per-network breakdown counts the two this test put there among
    // whatever else the shared bus holds.
    let pending = gateway.pending().await?;
    let total = pending["total"].as_u64().context("a total")?;
    assert_eq!(
        total,
        pending["contacts"].as_array().map(Vec::len).unwrap_or(0) as u64,
        "unfiltered, the total is the list's length: {pending}"
    );
    let counted: u64 = pending["networks"]
        .as_array()
        .context("the per-network counts")?
        .iter()
        .filter_map(|entry| entry["count"].as_u64())
        .sum();
    assert_eq!(
        counted, total,
        "the breakdown adds up to the total: {pending}"
    );

    // And the filter narrows the list without touching the numbers beside
    // it: a badge and the list under it must never disagree. Asserted inside
    // one answer, because the shared bus is still growing this list between
    // any two of them.
    let filtered: Value = gateway
        .get("/api/contacts/pending?network=signal")
        .await?
        .json()
        .await?;
    let listed = filtered["contacts"].as_array().context("the contacts")?;
    assert!(
        listed
            .iter()
            .all(|entry| entry["network"] == json!("signal")),
        "the filter narrows the contacts: {filtered}"
    );
    assert!(
        listed
            .iter()
            .any(|entry| entry["contact"] == json!(quiet.clone())),
        "and keeps the one this test put on signal: {filtered}"
    );
    let filtered_total = filtered["total"].as_u64().context("a total")?;
    assert!(
        filtered_total > listed.len() as u64,
        "the total counts everything waiting, not what this call showed: {filtered}"
    );
    assert_eq!(
        filtered["networks"]
            .as_array()
            .context("the per-network counts")?
            .iter()
            .filter_map(|entry| entry["count"].as_u64())
            .sum::<u64>(),
        filtered_total,
        "and the breakdown beside it still adds up to it: {filtered}"
    );

    gateway.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// 2. A decision moves the contact out of the list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_decision_recorded_through_the_write_api_stops_the_contact_waiting() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let gateway = Running::start("pending-decided").await?;

    let decided = ghost("whatsapp", "decided");
    let untouched = ghost("whatsapp", "untouched");
    for contact in [&decided, &untouched] {
        publish(
            &bus,
            &inbound_event(
                contact,
                "whatsapp",
                "2026-09-17T10:00:00Z",
                "a message",
                SECRET_DISPLAY_NAME,
            ),
        )
        .await?;
    }
    gateway.wait_until_pending(&decided, "whatsapp").await?;
    gateway.wait_until_pending(&untouched, "whatsapp").await?;

    // The user answers, through the same write API the Companion uses.
    gateway.decide(&decided, "granted", "whatsapp").await?;

    assert_eq!(
        gateway.waiting_networks(&decided).await?,
        Vec::<String>::new(),
        "a contact the user decided about is not waiting for a decision"
    );
    assert_eq!(
        gateway.waiting_networks(&untouched).await?,
        vec!["whatsapp".to_owned()],
        "and the decision moved nobody else"
    );
    // The dashboard's number and the list under it are the same answer. (An
    // exact delta is not assertable here: this Gateway reads the whole
    // shared stream, which every other test of this suite is publishing to
    // at the same time.)
    let after = gateway.pending().await?;
    assert_eq!(
        after["total"].as_u64(),
        after["contacts"]
            .as_array()
            .map(|contacts| contacts.len() as u64),
        "the badge counts what the list holds: {after}"
    );

    // A revocation is an answer too: the user said no, and is not asked
    // again.
    gateway.decide(&untouched, "revoked", "whatsapp").await?;
    assert_eq!(
        gateway.waiting_networks(&untouched).await?,
        Vec::<String>::new(),
        "a revoked contact is decided, not waiting"
    );

    // But a contact that writes on a second network is waiting again there:
    // consent has a perimeter, and so does the list.
    publish(
        &bus,
        &inbound_event(
            &decided,
            "signal",
            "2026-09-17T11:00:00Z",
            "a message on another network",
            SECRET_DISPLAY_NAME,
        ),
    )
    .await?;
    gateway.wait_until_pending(&decided, "signal").await?;
    assert_eq!(
        gateway.waiting_networks(&decided).await?,
        vec!["signal".to_owned()],
        "the whatsapp decision did not answer for signal"
    );

    gateway.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. The store holds no body and no identifier
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_message_body_and_no_network_identifier_reaches_the_gateways_store_or_its_logs(
) -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let gateway = Running::start("pending-no-content").await?;

    let contact = ghost("whatsapp", "no_content");
    publish(
        &bus,
        &inbound_event(
            &contact,
            "whatsapp",
            "2026-09-17T10:00:00Z",
            SECRET_BODY,
            SECRET_DISPLAY_NAME,
        ),
    )
    .await?;
    gateway.wait_until_pending(&contact, "whatsapp").await?;
    // A decision as well, so the journal has been written to and the WAL
    // checkpointed with the contact in it.
    gateway.decide(&contact, "granted", "whatsapp").await?;

    let store = store_bytes(&gateway.state_dir())?;
    assert!(
        store.contains(&contact),
        "the test is looking at the right files: the contact the Gateway did store is in {}",
        gateway.state_dir().display()
    );
    for (what, secret) in [
        // The message itself. The projection never parses `data`, so this
        // is not a field that was filtered out — it is a value the process
        // never held.
        ("the message body", SECRET_BODY),
        // The network identifier the Sensor withholds until consent is
        // granted. The Gateway must not undo that by keeping a copy.
        ("the contact's network identifier", SECRET_IDENTIFIER),
        // The display name, which is read from the bus on demand and never
        // written down.
        ("the contact's display name", SECRET_DISPLAY_NAME),
    ] {
        assert!(
            !store.contains(secret),
            "{what} must not appear in the Gateway's store: this store is a list of who \
             writes to the user, and its restraint is the only thing that keeps it from \
             being a surveillance log"
        );
    }

    // And the same of the logs, which are the other place a projection
    // leaks what it read.
    let logs = gateway.gateway.logs().await.join("\n");
    assert!(
        logs.contains("pending-contact projection"),
        "the projection was running at all: {logs}"
    );
    for (what, secret) in [
        ("the message body", SECRET_BODY),
        ("the contact's network identifier", SECRET_IDENTIFIER),
        ("the contact's display name", SECRET_DISPLAY_NAME),
    ] {
        assert!(
            !logs.contains(secret),
            "{what} must not appear in the Gateway's logs"
        );
    }

    gateway.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. A restart resumes at the ack floor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_restarted_gateway_resumes_at_its_ack_floor_instead_of_replaying_the_stream() -> Result<()>
{
    ensure_stack().await?;
    let bus = bus().await?;

    // A history big enough that replaying it would be unmistakable — and
    // big enough to stay unmistakable next to whatever the other tests of
    // this suite are publishing onto the same shared stream at the same
    // moment.
    let history: Vec<String> = (0..40)
        .map(|index| ghost("whatsapp", &format!("ackfloor_{index}")))
        .collect();
    for contact in &history {
        publish(
            &bus,
            &inbound_event(
                contact,
                "whatsapp",
                "2026-09-10T10:00:00Z",
                "a message from before the restart",
                SECRET_DISPLAY_NAME,
            ),
        )
        .await?;
    }

    let static_dir = companion_build("pending-ack-floor")?;
    let first = Running::start_on(static_dir.clone(), &nats_url()).await?;
    // The projection reads the stream in order, so the last one published
    // arriving means all of them have.
    first
        .wait_until_pending(history.last().expect("a history"), "whatsapp")
        .await?;
    let observed_first = first
        .metric("twalk_companion_gateway_contacts_observed_total")
        .await?;
    assert!(
        observed_first >= history.len() as u64,
        "the first run read the history: {observed_first}"
    );
    // The precondition the rest of this test rests on, asserted rather than
    // inferred from the last contact having arrived: the whole history is in
    // the store the restart will inherit, with the sightings the events
    // carried.
    let inherited = first.pending().await?;
    for contact in &history {
        let entry = inherited["contacts"]
            .as_array()
            .context("the contacts")?
            .iter()
            .find(|entry| entry["contact"] == json!(contact.clone()))
            .with_context(|| format!("{contact} is in the first run's list"))?;
        assert_eq!(
            entry["first_seen"].as_str(),
            Some("2026-09-10T10:00:00.000Z"),
            "{contact} was recorded at the instant its event carried: {entry}"
        );
    }
    first.stop().await;

    // Traffic while the Gateway is down, and one of the old contacts writing
    // again — which is what the store's "latest sighting" has to pick up.
    let newcomer = ghost("signal", "ackfloor_newcomer");
    publish(
        &bus,
        &inbound_event(
            &newcomer,
            "signal",
            "2026-09-12T09:00:00Z",
            "a message while the gateway was down",
            SECRET_DISPLAY_NAME,
        ),
    )
    .await?;
    publish(
        &bus,
        &inbound_event(
            &history[0],
            "whatsapp",
            "2026-09-12T18:00:00Z",
            "and an old contact writing again",
            SECRET_DISPLAY_NAME,
        ),
    )
    .await?;

    // The same static directory, so the same state directory and the same
    // durable consumer: this is a restart, not a second deployment.
    let second = Running::start_on(static_dir, &nats_url()).await?;
    second.wait_until_pending(&newcomer, "signal").await?;
    let moved = second.wait_until_pending(&history[0], "whatsapp").await?;
    assert_eq!(
        moved["first_seen"].as_str(),
        Some("2026-09-10T10:00:00.000Z"),
        "the first sighting survived the restart: {moved}"
    );
    assert_eq!(
        moved["last_seen"].as_str(),
        Some("2026-09-12T18:00:00.000Z"),
        "and the latest one moved: {moved}"
    );
    // The whole inherited list is still there — read from the store, not
    // rebuilt from the stream.
    for contact in &history {
        assert_eq!(
            second.waiting_networks(contact).await?,
            vec!["whatsapp".to_owned()],
            "{contact} was inherited from the store"
        );
    }

    // And the property the test is named for: the second run read the
    // handful of events published while it was down, not the forty-odd it
    // had already acked. The consumer resumed at its ack floor, which is the
    // bus's business — this Gateway keeps no cursor of its own.
    //
    // A margin rather than an exact count, because this Gateway consumes the
    // stack's one shared stream and the other tests of this suite are
    // publishing onto it while this one runs. Replaying would put the whole
    // history back through the projection, which is an order of magnitude
    // above that noise.
    let observed_second = second
        .metric("twalk_companion_gateway_contacts_observed_total")
        .await?;
    assert!(
        observed_second < history.len() as u64 / 2,
        "the restart replayed the stream it had already applied: it read {observed_second} \
         events, and the history alone is {}",
        history.len()
    );
    assert!(
        second
            .metric("twalk_companion_gateway_pending_contacts")
            .await?
            > history.len() as u64,
        "while the list it serves still holds the history and the newcomer"
    );

    second.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// 5. Display names come from the bus, and are written nowhere
// ---------------------------------------------------------------------------

#[tokio::test]
async fn display_names_are_read_from_the_bus_on_demand_and_never_stored() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let gateway = Running::start("pending-display-names").await?;

    let named = ghost("whatsapp", "named");
    let renamed_to = format!("Renamed {}", unique("later"));
    publish(
        &bus,
        &inbound_event(
            &named,
            "whatsapp",
            "2026-09-17T10:00:00Z",
            "a message",
            SECRET_DISPLAY_NAME,
        ),
    )
    .await?;
    gateway.wait_until_pending(&named, "whatsapp").await?;

    let encoded = |contact: &str| contact.replace('@', "%40").replace(':', "%3A");
    let names: Value = gateway
        .get(&format!(
            "/api/contacts/display-names?contact={}",
            encoded(&named)
        ))
        .await?
        .json()
        .await?;
    assert_eq!(
        names["contacts"][0]["contact"].as_str(),
        Some(named.as_str())
    );
    assert_eq!(
        names["contacts"][0]["display_name"].as_str(),
        Some(SECRET_DISPLAY_NAME),
        "the name comes from the bus: {names}"
    );

    // The most recent one wins: the Gateway asks the bus every time rather
    // than remembering an answer, so a contact that changed its name is not
    // stale.
    publish(
        &bus,
        &inbound_event(
            &named,
            "whatsapp",
            "2026-09-17T11:00:00Z",
            "a later message",
            &renamed_to,
        ),
    )
    .await?;
    let refreshed = poll_until(
        || async {
            let names: Value = gateway
                .get(&format!(
                    "/api/contacts/display-names?contact={}",
                    encoded(&named)
                ))
                .await
                .ok()?
                .json()
                .await
                .ok()?;
            let name = names["contacts"][0]["display_name"].as_str()?.to_owned();
            (name == renamed_to).then_some(name)
        },
        "the display-name read to answer with the newest name on the bus",
    )
    .await?;
    assert_eq!(refreshed, renamed_to);

    // A contact nobody ever wrote as: `null`, which is an answer and not a
    // failure — the Companion shows the Matrix ID.
    let unknown = ghost("telegram", "never_wrote");
    let none: Value = gateway
        .get(&format!(
            "/api/contacts/display-names?contact={}",
            encoded(&unknown)
        ))
        .await?
        .json()
        .await?;
    assert_eq!(
        none["contacts"][0]["contact"].as_str(),
        Some(unknown.as_str())
    );
    assert_eq!(
        none["contacts"][0]["display_name"],
        Value::Null,
        "a name the bus does not carry is null, not an error: {none}"
    );

    // And nothing that was read was kept: neither name is in the store.
    let store = store_bytes(&gateway.state_dir())?;
    assert!(store.contains(&named), "the contact itself is stored");
    for name in [SECRET_DISPLAY_NAME, renamed_to.as_str()] {
        assert!(
            !store.contains(name),
            "a display name the Gateway read must not be written down: {name}"
        );
    }

    gateway.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// The two refusals a deployment can produce
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_gateway_with_no_bus_says_so_instead_of_answering_an_empty_list() -> Result<()> {
    ensure_stack().await?;
    // `gateway_env` configures sign-in and no bus, which is a deployment an
    // operator can have.
    let static_dir = companion_build("pending-no-bus")?;
    let gateway = GatewayProc::start(&harness::gateway_env(&static_dir))?;
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
    let device = harness::signed_in_device_token(&base).await?;
    let http = reqwest::Client::new();

    for path in [
        "/api/contacts/pending",
        "/api/contacts/display-names?contact=%40a%3Atest.twalk",
    ] {
        let response = http
            .get(format!("{base}{path}"))
            .header(reqwest::header::COOKIE, format!("twalk_device={device}"))
            .send()
            .await?;
        assert_eq!(
            response.status().as_u16(),
            503,
            "a Gateway that watches no stream must not answer an empty list: {path}"
        );
        let body: Value = response.json().await?;
        assert_eq!(
            body["error"].as_str(),
            Some("contacts_not_configured"),
            "\"nobody has written to you\" and \"this Gateway is not watching\" are \
             different claims: {body}"
        );
    }

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_bus_that_cannot_be_reached_refuses_the_names_and_serves_the_list() -> Result<()> {
    ensure_stack().await?;
    let static_dir = companion_build("pending-dead-bus")?;
    let gateway = Running::start_on(static_dir, &unreachable_nats_url()?).await?;

    // The list comes from the Gateway's own store, so a bus that is down
    // does not take it away — it is simply not growing.
    let pending = gateway.pending().await?;
    assert_eq!(pending["total"].as_u64(), Some(0));

    // The names do come from the bus, and their refusal says which half
    // failed.
    let response = gateway
        .get("/api/contacts/display-names?contact=%40a%3Atest.twalk")
        .await?;
    assert_eq!(response.status().as_u16(), 502);
    let body: Value = response.json().await?;
    assert_eq!(body["error"].as_str(), Some("bus_unreachable"), "{body}");

    gateway.stop().await;
    Ok(())
}
