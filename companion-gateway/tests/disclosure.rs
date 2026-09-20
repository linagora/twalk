//! Ticket #121 at the Gateway's process boundary: the disclosure switch, and
//! what it does to an approval. The real binary, the real NATS JetStream,
//! real contract events, and HTTP calls from the test.
//!
//! ADR 0019 asks for one thing and ADR 0031 places it, and both are the test
//! plan:
//!
//! > Every reply a persona drafted reaches the contact with one sentence
//! > after it, on a line of its own, in the language the reply was written
//! > in […] on by default; turning it off is a dated, attributed, append-only
//! > record.
//!
//! So, in order:
//!
//! 1. **on by default**, and the answer says nobody decided rather than
//!    inventing a decision;
//! 2. **turning it off is a record**: `PUT` answers the state with who
//!    decided and when, `GET` reads the same, and the journal it lives in
//!    refuses every `UPDATE` and `DELETE` — asserted on the store's own file,
//!    because "append-only" is a property of the schema and not of the
//!    handler;
//! 3. **an approval while off** publishes a reply that carries neither the
//!    line nor the `data.disclosure` member — a bus that recorded a
//!    disclosure nobody received would be lying about what the contact got;
//! 4. **an approval while on** carries both, and `edited` stays `false` when
//!    the body was untouched, because the comparison is the body alone and
//!    appending the sentence is not an edit;
//! 5. **a suggestion with no sentence** goes out undisclosed even while the
//!    switch is on: the Gateway composes nothing (ADR 0031), and the
//!    contract allows a suggestion without the member.
//!
//! The switch is global and so is the store it lives in, so each test here
//! runs its own Gateway on its own state directory: a decision taken by one
//! must not be read by another. The bus is shared with every suite and every
//! run, so each test invents its own contact, room and suggestion, and asks
//! about those.

mod harness;

use std::path::PathBuf;

use anyhow::{Context, Result};
use harness::{
    companion_build, ensure_stack, gateway_env_with_consent, nats_url, owner_user_id, poll_until,
    validate_against_contract, Bus, GatewayProc, SERVER_NAME,
};
use serde_json::{json, Value};

const STREAM: &str = "twalk";
const INBOUND_SUBJECT: &str = "twalk.inbound.message.received.v1";
const INBOUND_TYPE: &str = "fr.linagora.twalk.inbound.message.received.v1";
const SUGGEST_SUBJECT: &str = "twalk.persona.suggest.produced.v1";
const SUGGEST_TYPE: &str = "fr.linagora.twalk.persona.suggest.produced.v1";
const APPROVED_SUBJECT: &str = "twalk.persona.reply.approved.v1";

/// The contract's French sentence (`contracts/disclosure/v1/sentences.json`):
/// what a persona answering a French message selects.
const DISCLOSURE: &str = "Rédigé avec mon assistant IA.";

// ---------------------------------------------------------------------------
// Driving the seam
// ---------------------------------------------------------------------------

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

fn ghost(label: &str) -> String {
    format!("@whatsapp_{}:{SERVER_NAME}", unique(label))
}

fn portal_room(label: &str) -> String {
    let opaque: String = harness::sha256_hex(&unique(label))
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(18)
        .collect();
    format!("!{opaque}:{SERVER_NAME}")
}

fn in_seconds(seconds: i64) -> String {
    (time::OffsetDateTime::now_utc() + time::Duration::seconds(seconds))
        .replace_nanosecond(0)
        .expect("a whole second is a valid instant")
        .format(&time::format_description::well_known::Rfc3339)
        .expect("an instant formats as RFC 3339")
}

/// One `inbound.message.received.v1` as the Sensor publishes it.
fn inbound_event(sender: &str, room_id: &str) -> Value {
    let at = in_seconds(-120);
    let event = json!({
        "specversion": "1.0",
        "id": harness::sha256_hex(&format!("d121-inbound:{sender}:{room_id}:{at}")),
        "source": format!("matrix://{SERVER_NAME}/{room_id}"),
        "type": INBOUND_TYPE,
        "time": at,
        "subject": sender,
        "datacontenttype": "application/json",
        "dataschema": "https://schemas.twalk.dev/cloudevents/v1/inbound.message.received.schema.json",
        "network": "whatsapp",
        "connection": "whatsapp",
        "consent": "granted",
        "data": {
            "body": "On décale à 20h ?",
            "format": "text/plain",
            "reply_to": null,
            "attachments": [],
            "contact": { "display_name": "Aicha Benali D121" }
        }
    });
    validate_against_contract(&event, "inbound.message.received")
        .expect("the fixture is an event the contract allows");
    event
}

/// One `persona.suggest.produced.v1`, with or without the sentence a
/// persona selects — the contract allows both, and the second is what a
/// suggestion published before #121 looks like.
fn suggest_event(trigger: &Value, body: &str, disclosure: Option<&str>) -> Value {
    let trigger_id = trigger["id"].as_str().expect("the trigger has an id");
    let mut data = json!({
        "persona_id": "assistant",
        "trigger": { "event_id": trigger_id, "event_type": INBOUND_TYPE },
        "suggestion": { "body": body, "format": "text/plain" },
        "attempt": 1,
        "expires_at": in_seconds(3600)
    });
    if let Some(disclosure) = disclosure {
        data["disclosure"] = json!(disclosure);
    }
    let event = json!({
        "specversion": "1.0",
        "id": harness::sha256_hex(&format!("assistant:{trigger_id}:1")),
        "source": format!("hermes://{SERVER_NAME}/personas/assistant"),
        "type": SUGGEST_TYPE,
        "time": in_seconds(-60),
        "subject": trigger_id,
        "datacontenttype": "application/json",
        "dataschema": "https://schemas.twalk.dev/cloudevents/v1/persona.suggest.produced.schema.json",
        "network": "whatsapp",
        "connection": "whatsapp",
        "consent": "granted",
        "data": data
    });
    validate_against_contract(&event, "persona.suggest.produced")
        .expect("the fixture is an event the contract allows");
    event
}

async fn bus() -> Result<Bus> {
    let bus = Bus::connect().await?;
    bus.ensure_stream(STREAM, &["twalk.>"]).await?;
    Ok(bus)
}

/// A Gateway with a bus and a consent store of its own, signed in.
struct Running {
    #[allow(dead_code)]
    gateway: GatewayProc,
    base: String,
    static_dir: PathBuf,
    device: String,
    http: reqwest::Client,
}

impl Running {
    async fn start(test_name: &str) -> Result<Self> {
        let static_dir = companion_build(test_name)?;
        let gateway = GatewayProc::start(&gateway_env_with_consent(&static_dir, &nats_url()))?;
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

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(reqwest::StatusCode, Value)> {
        let mut request = self
            .http
            .request(method.clone(), format!("{}{path}", self.base))
            .header(
                reqwest::header::COOKIE,
                format!("twalk_device={}", self.device),
            );
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("failed to call {method} {path}"))?;
        let status = response.status();
        let text = response.text().await?;
        let body = serde_json::from_str(&text)
            .with_context(|| format!("{method} {path} answered {status} with non-JSON: {text}"))?;
        Ok((status, body))
    }

    async fn get(&self, path: &str) -> Result<(reqwest::StatusCode, Value)> {
        self.call(reqwest::Method::GET, path, None).await
    }

    async fn put(&self, path: &str, body: &Value) -> Result<(reqwest::StatusCode, Value)> {
        self.call(reqwest::Method::PUT, path, Some(body)).await
    }

    async fn post(&self, path: &str, body: &Value) -> Result<(reqwest::StatusCode, Value)> {
        self.call(reqwest::Method::POST, path, Some(body)).await
    }

    /// The switch, as the settings screen reads it.
    async fn switch(&self) -> Result<Value> {
        let (status, body) = self.get("/api/settings/disclosure").await?;
        anyhow::ensure!(status == reqwest::StatusCode::OK, "{status}: {body}");
        Ok(body)
    }

    /// One decision about the switch, as the settings screen takes it.
    async fn decide_disclosure(&self, enabled: bool, reason: Option<&str>) -> Result<Value> {
        let (status, body) = self
            .put(
                "/api/settings/disclosure",
                &json!({ "enabled": enabled, "reason": reason }),
            )
            .await?;
        anyhow::ensure!(status == reqwest::StatusCode::OK, "{status}: {body}");
        Ok(body)
    }

    /// Records one consent decision through #49's write API.
    async fn grant(&self, contact: &str) -> Result<()> {
        let (status, body) = self
            .post(
                "/api/consent/decisions",
                &json!({
                    "subject": { "type": "contact", "id": contact },
                    "new_state": "granted",
                    "scope": { "networks": ["whatsapp"] }
                }),
            )
            .await?;
        anyhow::ensure!(
            status == reqwest::StatusCode::CREATED,
            "the decision was refused with {status}: {body}"
        );
        Ok(())
    }

    /// Approves one suggestion and reads the published event back from the
    /// bus at the position the answer named.
    async fn approve(&self, bus: &Bus, suggestion_id: &str) -> Result<(Value, Value)> {
        let (status, answer) = self
            .post(
                "/api/approvals",
                &json!({ "suggestion_event_id": suggestion_id }),
            )
            .await?;
        anyhow::ensure!(
            status == reqwest::StatusCode::CREATED,
            "the approval was refused with {status}: {answer}"
        );
        let sequence = answer["stream_sequence"]
            .as_u64()
            .context("the answer names no stream position")?;
        let stored = bus
            .consume_from(STREAM, APPROVED_SUBJECT, sequence, 1)
            .await?
            .into_iter()
            .next()
            .with_context(|| format!("no approved reply is stored at sequence {sequence}"))?;
        validate_against_contract(&stored.payload, "persona.reply.approved")?;
        Ok((answer, stored.payload))
    }

    /// The Gateway's own metrics, as a scrape reads them.
    async fn metric(&self, name: &str) -> Result<Option<u64>> {
        let text = reqwest::get(format!("{}/metrics", self.base))
            .await?
            .text()
            .await?;
        Ok(harness::parse_exposition(&text)
            .into_iter()
            .find(|(metric, _)| metric == name)
            .map(|(_, value)| value))
    }

    fn store_file(&self) -> PathBuf {
        harness::gateway_state_dir(&self.static_dir).join("consent.sqlite3")
    }

    /// Every byte of every file in the Gateway's state directory — the
    /// store **and** its WAL, as `tests/pending.rs` reads it. The store runs
    /// in WAL mode and the Gateway is still up, so what this run wrote is in
    /// `consent.sqlite3-wal` and an assertion over the main file alone would
    /// pass for a reason that has nothing to do with the schema.
    fn state_bytes(&self) -> Result<Vec<u8>> {
        let state_dir = harness::gateway_state_dir(&self.static_dir);
        let mut bytes = Vec::new();
        for entry in std::fs::read_dir(&state_dir)
            .with_context(|| format!("failed to read {}", state_dir.display()))?
        {
            let path = entry?.path();
            if path.is_file() {
                bytes.extend(std::fs::read(&path)?);
            }
        }
        Ok(bytes)
    }
}

/// A granted contact, their message, and a persona's suggestion for it —
/// with or without the sentence.
async fn conversation(
    running: &Running,
    bus: &Bus,
    label: &str,
    disclosure: Option<&str>,
) -> Result<(String, String)> {
    let contact = ghost(label);
    let room_id = portal_room(label);
    running.grant(&contact).await?;
    let trigger = inbound_event(&contact, &room_id);
    bus.publish_event(INBOUND_SUBJECT, &trigger).await?;
    let body = format!("Pas de problème, à 20h ! ({label})");
    let suggestion = suggest_event(&trigger, &body, disclosure);
    bus.publish_event(SUGGEST_SUBJECT, &suggestion).await?;
    Ok((suggestion["id"].as_str().unwrap().to_owned(), body))
}

// ---------------------------------------------------------------------------
// 1 and 2. On by default; off as a record; the record cannot be rewritten
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_disclosure_is_on_by_default_and_turning_it_off_is_a_dated_attributed_record(
) -> Result<()> {
    ensure_stack().await?;
    let running = Running::start("disclosure-switch").await?;

    // Nobody decided, and the answer says so rather than inventing a
    // decision: three nulls, not a seeded row.
    assert_eq!(
        running.switch().await?,
        json!({ "enabled": true, "since": null, "actor": null, "reason": null }),
        "the disclosure ships on by default (ADR 0031)"
    );
    assert_eq!(
        running
            .metric("twalk_companion_gateway_disclosure_enabled")
            .await?,
        Some(1),
        "and /metrics says so without anyone opening the Companion"
    );

    // Off, with a reason. The actor is the owner, stamped by the Gateway
    // from configuration — the body never names one.
    let before = in_seconds(-1);
    let off = running
        .decide_disclosure(false, Some("testing what the contact sees"))
        .await?;
    assert_eq!(off["enabled"], json!(false));
    assert_eq!(
        off["actor"],
        json!(owner_user_id()),
        "who decided is the deployment's owner, as a consent decision's actor is"
    );
    let since = off["since"].as_str().context("the record is dated")?;
    assert!(
        since >= before.as_str(),
        "the decision is dated now, not when the row was read: {since} < {before}"
    );
    assert_eq!(off["reason"], json!("testing what the contact sees"));
    assert_eq!(
        running.switch().await?,
        off,
        "GET reads back exactly the state PUT answered"
    );
    assert_eq!(
        running
            .metric("twalk_companion_gateway_disclosure_enabled")
            .await?,
        Some(0)
    );

    // The journal is append-only, asserted on the store's own file: the
    // property is the schema's triggers, not the handler's manners. A
    // second connection to the same SQLite file, as an operator with a
    // shell would open one.
    let store = rusqlite::Connection::open(running.store_file())
        .context("the consent store opens from outside the process")?;
    for statement in [
        "DELETE FROM disclosure_decision",
        "UPDATE disclosure_decision SET new_state = 'on'",
        "UPDATE disclosure_decision SET actor = '@somebody-else:example.com'",
        "UPDATE disclosure_decision SET reason = NULL",
    ] {
        let refused = store.execute(statement, []);
        assert!(
            refused.is_err(),
            "the disclosure journal accepted {statement:?}: it is the record of a deliberate act \
             and must be append-only (ADR 0019)"
        );
    }
    drop(store);

    // Back on: a second row, and the first stays. The record now says
    // "on since …, by …" and the reason it carries is this decision's own.
    let on = running.decide_disclosure(true, None).await?;
    assert_eq!(on["enabled"], json!(true));
    assert_eq!(on["actor"], json!(owner_user_id()));
    assert!(
        on["since"].as_str().context("dated")? >= since,
        "the second decision is not earlier than the first"
    );
    assert_eq!(on["reason"], Value::Null);
    let store = rusqlite::Connection::open(running.store_file())?;
    let rows: i64 = store.query_row("SELECT COUNT(*) FROM disclosure_decision", [], |row| {
        row.get(0)
    })?;
    assert_eq!(
        rows, 2,
        "every decision is a row; nothing is collapsed or replaced"
    );

    // And a request that is not one decision is refused with the member
    // named, so a client that sent the wrong shape learns nothing happened.
    let (status, refusal) = running
        .put("/api/settings/disclosure", &json!({ "enabled": "off" }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{refusal}");
    assert_eq!(refusal["error"], json!("malformed_request"));
    assert!(
        refusal["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("enabled"),
        "{refusal}"
    );
    assert_eq!(
        running.switch().await?["enabled"],
        json!(true),
        "a refused request changed nothing"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 3 and 4. What an approval carries, while off and while on
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_approval_while_off_carries_neither_the_line_nor_the_member_and_while_on_carries_both(
) -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("disclosure-approval").await?;

    // Off first. The suggestion carries the sentence — the persona selected
    // it, as every first-party suggestion does — and the switch is what
    // decides whether it is sent.
    running
        .decide_disclosure(false, Some("a test while off"))
        .await?;
    let (undisclosed_id, undisclosed_body) =
        conversation(&running, &bus, "off", Some(DISCLOSURE)).await?;
    let (answer, event) = running.approve(&bus, &undisclosed_id).await?;
    assert_eq!(
        event["data"]["final"]["body"],
        json!(undisclosed_body),
        "with the switch off, the body goes out alone: no line after it"
    );
    assert!(
        event["data"].get("disclosure").is_none(),
        "and no member either: a bus that recorded a disclosure nobody received would be \
         lying about what the contact got: {event}"
    );
    assert_eq!(answer["edited"], json!(false));
    assert_eq!(event["data"]["edited"], json!(false));

    // On again — not retroactive: the reply above went out as it went out.
    running.decide_disclosure(true, None).await?;
    let (disclosed_id, disclosed_body) =
        conversation(&running, &bus, "on", Some(DISCLOSURE)).await?;
    let (answer, event) = running.approve(&bus, &disclosed_id).await?;
    assert_eq!(
        event["data"]["final"]["body"],
        json!(format!("{disclosed_body}\n{DISCLOSURE}")),
        "with the switch on, the sentence follows the body on a line of its own (ADR 0031)"
    );
    assert_eq!(
        event["data"]["disclosure"],
        json!(DISCLOSURE),
        "and is named again as a member, so a consumer need not parse the body"
    );
    assert_eq!(
        answer["edited"],
        json!(false),
        "the body was untouched: appending the sentence is not an edit, because the comparison \
         is the body alone and the sentence is not in the field the user edits"
    );
    assert_eq!(event["data"]["edited"], json!(false));

    // What the Gateway keeps of either: no text at all. The sentence is on
    // the bus with the reply, where the retention is declared. Every file
    // in the state directory, because the rows this test wrote are in the
    // WAL and not yet in the main file. The approval rows themselves are
    // there — the search is not vacuous — which the id check proves first.
    let bytes = running.state_bytes()?;
    assert!(
        bytes
            .windows(disclosed_id.len())
            .any(|window| window == disclosed_id.as_bytes()),
        "the approval row was not found in the state directory, so the search below would \
         prove nothing"
    );
    for text in [
        undisclosed_body.as_str(),
        disclosed_body.as_str(),
        DISCLOSURE,
    ] {
        assert!(
            !bytes
                .windows(text.len())
                .any(|window| window == text.as_bytes()),
            "the Gateway's store holds {text:?}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 5. A suggestion with no sentence: the Gateway composes nothing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_suggestion_with_no_sentence_goes_out_undisclosed_because_the_gateway_composes_nothing(
) -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let running = Running::start("disclosure-absent").await?;
    assert_eq!(
        running.switch().await?["enabled"],
        json!(true),
        "the switch is on: what is being tested is the absence of a sentence, not a decision"
    );

    // A suggestion as one published before #121 looks like, or as a
    // third-party persona that set no sentence publishes one: the contract
    // allows it, and ADR 0031 forbids the Gateway from inventing a sentence
    // — it holds no language to select one by.
    let (suggestion_id, body) = conversation(&running, &bus, "absent", None).await?;
    let (answer, event) = running.approve(&bus, &suggestion_id).await?;
    assert_eq!(
        event["data"]["final"]["body"],
        json!(body),
        "no sentence to append, so none is: the Gateway composes no disclosure of its own"
    );
    assert!(
        event["data"].get("disclosure").is_none(),
        "and no member is invented: {event}"
    );
    assert_eq!(answer["edited"], json!(false));
    Ok(())
}
