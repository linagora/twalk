//! Ticket #281, the one governed pull, at the Gateway's process boundary:
//! the real binary, the real bus, a stub standing in for the collector, and
//! signed reads from the test standing in for Hermes.
//!
//! In order:
//!
//! 1. a read signed with the answers' secret, on a calendar connection the
//!    collector said is `connected`, answers the intervals the collector
//!    answered and nothing else, and the relay carried the Gateway's own
//!    service token and the window as asked (that no title, participant
//!    or location ever leaves the collector is `collector/tests/freebusy.rs`'s
//!    to prove, against a fixture that holds all three);
//! 2. the read is a `hermes_read` row in the Gateway's store and a count
//!    on `/metrics`, and so is every refusal;
//! 3. wider than fourteen days is `400`, a bad signature `401`, a
//!    connection that is not `connected` `409` with its state;
//! 4. a collector that refuses with a code of its own, or does not answer,
//!    is `502`, not a `503` that would say the deployment has no seam; a
//!    Gateway with a seam and no collector is that `503`;
//! 5. the skill Twalk ships (`skills/twalk-calendar/freebusy.sh`) makes a
//!    request the route accepts — run as Hermes would run it.
//!
//! What a wrong, unsigned or stale read is answered with is
//! `tests/openapi.rs`'s, because those are reached without bus state.

mod harness;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use harness::{
    companion_build, ensure_stack, freebusy_query as query, freebusy_signature as signature,
    gateway_env_with, gateway_state_dir, nats_url, poll_until, validate_against_contract, Bus,
    GatewayProc, HERMES_ANSWER_SECRET, HERMES_DOMAIN, SERVICE_TOKEN,
};
use serde_json::{json, Value};

const STREAM: &str = "twalk";
const CONNECTION_STATUS_SUBJECT: &str = "twalk.connection.status.changed.v1";
const FREEBUSY_PATH: &str = "/_twalk/hermes/freebusy";

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

fn rfc3339(at: time::OffsetDateTime) -> String {
    at.replace_nanosecond(0)
        .expect("a whole second is a valid instant")
        .format(&time::format_description::well_known::Rfc3339)
        .expect("an instant formats as RFC 3339")
}

fn in_seconds(seconds: i64) -> String {
    rfc3339(time::OffsetDateTime::now_utc() + time::Duration::seconds(seconds))
}

async fn bus() -> Result<Bus> {
    let bus = Bus::connect().await?;
    bus.ensure_stream(STREAM, &["twalk.>"]).await?;
    Ok(bus)
}

/// One `connection.status.changed.v1` as the collector publishes it about
/// a calendar connection (#274): the state #281 refuses a read on.
fn calendar_status_event(connection: &str, from: &str, to: &str) -> Value {
    let at = in_seconds(-30);
    let event = json!({
        "specversion": "1.0",
        "id": harness::sha256_hex(&format!("{connection}:{to}:{at}")),
        "source": format!("collector://collector.test/connections/{connection}"),
        "type": "fr.linagora.twalk.connection.status.changed.v1",
        "time": at,
        "subject": connection,
        "datacontenttype": "application/json",
        "connection": connection,
        "data": {
            "connection": connection,
            "kind": "calendar",
            "from_state": from,
            "to_state": to,
            "occurred_at": at,
            "service": "caldav",
            "hint": "Run `twalk-collector authorize --renew` on the host."
        }
    });
    validate_against_contract(&event, "connection.status.changed")
        .expect("the fixture is an event the contract allows");
    event
}

/// One request the stub collector received: the request line, and the
/// `authorization` header when there was one.
type Relayed = (String, Option<String>);

/// One `hermes_read` row as the test reads it back: connection, window
/// from, window to, delivery, outcome, intervals.
type Recorded = (String, String, String, Option<String>, String, Option<i64>);

/// The collector's internal endpoint, stubbed: answers what a test tells
/// it to, and keeps every request line and bearer it received, so the test
/// asserts what the Gateway relayed and not only what came back.
struct StubCollector {
    addr: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<Relayed>>>,
    answer: Arc<Mutex<(u16, Vec<u8>)>>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl StubCollector {
    async fn answering(status: u16, body: Value) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("failed to bind the stub collector")?;
        let addr = listener.local_addr()?;
        let requests: Arc<Mutex<Vec<Relayed>>> = Arc::default();
        let seen = requests.clone();
        let answer = Arc::new(Mutex::new((status, serde_json::to_vec(&body)?)));
        let canned = answer.clone();
        let accept_task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let seen = seen.clone();
                let (status, body) = canned.lock().expect("not poisoned").clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buffer = vec![0_u8; 8192];
                    let read = stream.read(&mut buffer).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buffer[..read]).into_owned();
                    let line = head.lines().next().unwrap_or_default().to_owned();
                    let bearer = head
                        .lines()
                        .find_map(|line| line.strip_prefix("authorization: "))
                        .map(str::to_owned);
                    seen.lock().expect("not poisoned").push((line, bearer));
                    let response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                    let _ = stream.flush().await;
                });
            }
        });
        Ok(Self {
            addr,
            requests,
            answer,
            accept_task,
        })
    }

    /// What the stub answers from now on: a refusal of its own, say.
    fn answer(&self, status: u16, body: Value) {
        *self.answer.lock().expect("not poisoned") =
            (status, serde_json::to_vec(&body).expect("a JSON body"));
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn requests(&self) -> Vec<Relayed> {
        self.requests.lock().expect("not poisoned").clone()
    }

    fn stop(self) {
        self.accept_task.abort();
    }
}

/// A Gateway with the seam and the collector's URL, and the calendar
/// connection of this run in its registry.
async fn gateway(
    test_name: &str,
    connection: &str,
    collector_url: Option<&str>,
) -> Result<(GatewayProc, String, PathBuf)> {
    let static_dir = companion_build(test_name)?;
    let connections = format!("{},{connection}=calendar", harness::TEST_CONNECTIONS);
    let mut overrides = vec![
        ("GATEWAY_NATS_URL", nats_url()),
        (
            "GATEWAY_HERMES_ANSWER_SECRET",
            HERMES_ANSWER_SECRET.to_owned(),
        ),
        ("GATEWAY_HERMES_DOMAIN", HERMES_DOMAIN.to_owned()),
        ("GATEWAY_APPROVAL_LOOKUP_WINDOW", "100000000".to_owned()),
        ("GATEWAY_CONNECTIONS", connections),
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
    Ok((gateway, base, gateway_state_dir(&static_dir)))
}

/// One read as Hermes makes it: the query string signed as sent.
async fn read(
    base: &str,
    query: &str,
    signature: &str,
    timestamp: &str,
    delivery: Option<&str>,
) -> Result<(u16, Value)> {
    let mut request = reqwest::Client::new()
        .get(format!("{base}{FREEBUSY_PATH}?{query}"))
        .header("X-Hermes-Timestamp", timestamp)
        .header("X-Hermes-Signature-256", signature);
    if let Some(delivery) = delivery {
        request = request.header("X-Hermes-Delivery", delivery);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let body = response.json().await.unwrap_or(Value::Null);
    Ok((status, body))
}

async fn signed_read(
    base: &str,
    connection: &str,
    from: &str,
    to: &str,
    delivery: Option<&str>,
) -> Result<(u16, Value)> {
    let query = query(connection, from, to);
    let timestamp = in_seconds(0);
    read(
        base,
        &query,
        &signature(&query, &timestamp),
        &timestamp,
        delivery,
    )
    .await
}

/// The `hermes_read` rows, newest first, read straight from the Gateway's
/// store: the record is the acceptance criterion, and nothing serves it yet.
fn recorded_reads(state_dir: &std::path::Path) -> Result<Vec<Recorded>> {
    let connection = rusqlite::Connection::open_with_flags(
        state_dir.join("consent.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut statement = connection.prepare(
        "SELECT connection, window_from, window_to, delivery, outcome, intervals
         FROM hermes_read ORDER BY sequence DESC",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

async fn metric(base: &str, outcome: &str) -> Result<u64> {
    let text = reqwest::get(format!("{base}/metrics"))
        .await?
        .text()
        .await?;
    let needle = format!("twalk_companion_gateway_hermes_reads_total{{outcome=\"{outcome}\"}} ");
    Ok(text
        .lines()
        .find_map(|line| line.strip_prefix(&needle))
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0))
}

#[tokio::test]
async fn a_signed_read_on_a_connected_calendar_answers_busy_intervals_and_is_recorded() -> Result<()>
{
    ensure_stack().await?;
    let bus = bus().await?;
    let connection = unique("calendar");
    let busy = json!([
        { "start": "2026-09-24T09:00:00Z", "end": "2026-09-24T10:30:00Z" },
        { "start": "2026-09-25T14:00:00Z", "end": "2026-09-25T15:00:00Z" }
    ]);
    let collector = StubCollector::answering(
        200,
        json!({
            "connection": connection,
            "from": "2026-09-24T08:00:00Z",
            "to": "2026-09-26T18:00:00Z",
            "busy": busy,
        }),
    )
    .await?;
    // The collector spoke before the Gateway started: connected.
    bus.publish_event(
        CONNECTION_STATUS_SUBJECT,
        &calendar_status_event(&connection, "unknown", "connected"),
    )
    .await?;
    let (gateway, base, state_dir) =
        gateway("freebusy-served", &connection, Some(&collector.url())).await?;

    // Served, once the Gateway has read the state off the bus.
    let (status, body) = poll_until(
        || async {
            let answer = signed_read(
                &base,
                &connection,
                "2026-09-24T08:00:00Z",
                "2026-09-26T18:00:00Z",
                Some("delivery-1"),
            )
            .await
            .ok()?;
            (answer.0 != 409).then_some(answer)
        },
        "the connection's state to be read off the bus",
    )
    .await?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["busy"], busy);
    assert_eq!(body["connection"], connection);
    assert_eq!(body["from"], "2026-09-24T08:00:00Z");
    assert_eq!(
        body.as_object().map(|object| object.len()),
        Some(4),
        "connection, from, to, busy and nothing else: {body}"
    );
    // The relay: the Gateway's own service token, the window as asked, the
    // collector's route.
    let relayed = collector.requests();
    assert_eq!(relayed.len(), 1, "{relayed:?}");
    let (line, bearer) = &relayed[0];
    assert!(
        line.starts_with("GET /freebusy?"),
        "the collector's route: {line}"
    );
    assert!(line.contains(&format!("connection={connection}")), "{line}");
    assert!(
        line.contains("from=2026-09-24T08%3A00%3A00Z")
            && line.contains("to=2026-09-26T18%3A00%3A00Z"),
        "the window as asked: {line}"
    );
    assert_eq!(bearer.as_deref(), Some(&*format!("Bearer {SERVICE_TOKEN}")));

    // Recorded and counted — the reads refused while the state was still
    // being read off the bus are rows too, since every read is.
    let reads = recorded_reads(&state_dir)?;
    assert!(
        reads[1..]
            .iter()
            .all(|read| read.4 == "connection_not_connected"),
        "{reads:?}"
    );
    assert_eq!(
        reads[0],
        (
            connection.clone(),
            "2026-09-24T08:00:00Z".to_owned(),
            "2026-09-26T18:00:00Z".to_owned(),
            Some("delivery-1".to_owned()),
            "served".to_owned(),
            Some(2)
        )
    );
    assert_eq!(metric(&base, "served").await?, 1);

    // Wider than fourteen days: refused, recorded, counted — and the
    // collector never asked.
    let (status, body) = signed_read(
        &base,
        &connection,
        "2026-09-24T08:00:00Z",
        "2026-10-08T08:00:01Z",
        None,
    )
    .await?;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("window_too_wide")),
        "{body}"
    );
    // A signature over another query: refused before anything is read.
    let query_sent = query(&connection, "2026-09-24T08:00:00Z", "2026-09-26T18:00:00Z");
    let timestamp = in_seconds(0);
    let over_another = signature(
        &query(&connection, "2026-09-24T08:00:00Z", "2026-09-30T18:00:00Z"),
        &timestamp,
    );
    let (status, body) = read(&base, &query_sent, &over_another, &timestamp, None).await?;
    assert_eq!(
        (status, body["error"].as_str()),
        (401, Some("bad_signature")),
        "{body}"
    );
    assert_eq!(collector.requests().len(), 1, "no relay on a refusal");
    let reads = recorded_reads(&state_dir)?;
    assert_eq!(reads[0].4, "bad_signature");
    assert_eq!(reads[1].4, "window_too_wide");
    assert_eq!(
        reads[1].2, "2026-10-08T08:00:01Z",
        "the window as asked, in the record"
    );
    assert_eq!(metric(&base, "window_too_wide").await?, 1);
    assert_eq!(metric(&base, "bad_signature").await?, 1);

    // The collector says the calendar can no longer be reached: the next
    // read is refused with that state, before the collector is asked.
    bus.publish_event(
        CONNECTION_STATUS_SUBJECT,
        &calendar_status_event(&connection, "connected", "reconnect_required"),
    )
    .await?;
    let body = poll_until(
        || async {
            let (status, body) = signed_read(
                &base,
                &connection,
                "2026-09-24T08:00:00Z",
                "2026-09-26T18:00:00Z",
                None,
            )
            .await
            .ok()?;
            (status == 409).then_some(body)
        },
        "the read to be refused once the connection is not connected",
    )
    .await?;
    assert_eq!(body["error"], "connection_not_connected");
    assert_eq!(body["state"], "reconnect_required");
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("authorize --renew"),
        "the collector's hint travels: {body}"
    );
    // Refused before the collector is asked: one more read, no more relay.
    let relays = collector.requests().len();
    let (status, _) = signed_read(
        &base,
        &connection,
        "2026-09-24T08:00:00Z",
        "2026-09-26T18:00:00Z",
        None,
    )
    .await?;
    assert_eq!(status, 409);
    assert_eq!(collector.requests().len(), relays, "no relay on a refusal");
    assert!(recorded_reads(&state_dir)?
        .iter()
        .any(|read| read.4 == "connection_not_connected"));

    // The collector refusing with a code of its own — its side service
    // refused it — is a 502 that names the code; the collector down is a
    // 502 too, and not a 503 that would say the deployment has no seam.
    bus.publish_event(
        CONNECTION_STATUS_SUBJECT,
        &calendar_status_event(&connection, "reconnect_required", "connected"),
    )
    .await?;
    collector.answer(
        502,
        json!({ "error": "caldav_refused", "detail": "the calendar service refused the free-busy report with HTTP 403" }),
    );
    let body = poll_until(
        || async {
            let (status, body) = signed_read(
                &base,
                &connection,
                "2026-09-24T08:00:00Z",
                "2026-09-26T18:00:00Z",
                None,
            )
            .await
            .ok()?;
            (status == 502).then_some(body)
        },
        "the read to reach the collector and be refused by it",
    )
    .await?;
    assert_eq!(body["error"], "collector_refused");
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("caldav_refused"),
        "the collector's code travels: {body}"
    );
    collector.stop();
    let body = poll_until(
        || async {
            let (status, body) = signed_read(
                &base,
                &connection,
                "2026-09-24T08:00:00Z",
                "2026-09-26T18:00:00Z",
                None,
            )
            .await
            .ok()?;
            (status == 502).then_some(body)
        },
        "the read to reach the collector and find it down",
    )
    .await?;
    assert_eq!(body["error"], "collector_unreachable");
    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_gateway_with_a_seam_and_no_collector_says_so() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let connection = unique("calendar");
    bus.publish_event(
        CONNECTION_STATUS_SUBJECT,
        &calendar_status_event(&connection, "unknown", "connected"),
    )
    .await?;
    let (gateway, base, state_dir) = gateway("freebusy-no-collector", &connection, None).await?;
    let body = poll_until(
        || async {
            let (status, body) = signed_read(
                &base,
                &connection,
                "2026-09-24T08:00:00Z",
                "2026-09-26T18:00:00Z",
                None,
            )
            .await
            .ok()?;
            (status == 503).then_some(body)
        },
        "the read to be refused for want of a collector",
    )
    .await?;
    assert_eq!(body["error"], "collector_not_configured");
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("GATEWAY_COLLECTOR_URL"),
        "the variable that would open it: {body}"
    );
    assert!(recorded_reads(&state_dir)?
        .iter()
        .any(|read| read.4 == "collector_not_configured"));
    gateway.stop().await;
    Ok(())
}

/// The skill Twalk ships, run as Hermes runs it: the script signs and
/// sends a request the route accepts. The proof that the document and the
/// route agree, and the one test that would fail if either drifted.
#[tokio::test]
async fn the_skills_script_makes_a_request_the_route_accepts() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let connection = unique("calendar");
    let collector = StubCollector::answering(
        200,
        json!({
            "connection": connection,
            "from": "2026-09-24T08:00:00Z",
            "to": "2026-09-26T18:00:00Z",
            "busy": [{ "start": "2026-09-24T09:00:00Z", "end": "2026-09-24T10:30:00Z" }],
        }),
    )
    .await?;
    bus.publish_event(
        CONNECTION_STATUS_SUBJECT,
        &calendar_status_event(&connection, "unknown", "connected"),
    )
    .await?;
    let (gateway, base, state_dir) =
        gateway("freebusy-skill", &connection, Some(&collector.url())).await?;
    poll_until(
        || async {
            let (status, _) = signed_read(
                &base,
                &connection,
                "2026-09-24T08:00:00Z",
                "2026-09-26T18:00:00Z",
                None,
            )
            .await
            .ok()?;
            (status == 200).then_some(())
        },
        "the connection's state to be read off the bus",
    )
    .await?;
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../skills/twalk-calendar/freebusy.sh")
        .canonicalize()
        .context("the skill's script exists")?;
    let output = tokio::process::Command::new(&script)
        .arg(&connection)
        .arg("2026-09-24T08:00:00Z")
        .arg("2026-09-26T18:00:00Z")
        .env("TWALK_GATEWAY_URL", &base)
        .env("TWALK_ANSWER_SECRET", HERMES_ANSWER_SECRET)
        .env("TWALK_DELIVERY_ID", "skill-run-1")
        .output()
        .await
        .context("the skill's script ran")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the script was refused:\nstdout: {stdout}\nstderr: {stderr}"
    );
    let answer: Value = serde_json::from_str(stdout.trim())
        .with_context(|| format!("the script printed the answer as JSON: {stdout}"))?;
    assert_eq!(
        answer["busy"],
        json!([{ "start": "2026-09-24T09:00:00Z", "end": "2026-09-24T10:30:00Z" }])
    );
    assert!(
        recorded_reads(&state_dir)?
            .iter()
            .any(|read| read.3.as_deref() == Some("skill-run-1") && read.4 == "served"),
        "the script's delivery is in the record"
    );
    // The document's example is the script's own usage line.
    let document = std::fs::read_to_string(script.with_file_name("SKILL.md"))?;
    assert!(document.contains("./freebusy.sh <connection> <from> <to>"));
    assert!(document.contains("X-Hermes-Signature-256") && document.contains("X-Hermes-Timestamp"));
    collector.stop();
    gateway.stop().await;
    Ok(())
}
