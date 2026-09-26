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

use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use harness::{
    companion_build, ensure_stack, event_facts_query, event_facts_signature,
    freebusy_query as query, freebusy_signature as signature, gateway_env_with, gateway_state_dir,
    nats_url, poll_until, validate_against_contract, Bus, GatewayProc, HERMES_ANSWER_SECRET,
    HERMES_DOMAIN, SERVICE_TOKEN,
};
use serde_json::{json, Value};

const STREAM: &str = "twalk";
const CONNECTION_STATUS_SUBJECT: &str = "twalk.connection.status.changed.v1";
const FREEBUSY_PATH: &str = "/_twalk/hermes/freebusy";
const EVENT_FACTS_PATH: &str = "/_twalk/hermes/event-facts";

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

/// One read of what an event carries, signed over **its own** path.
async fn signed_event_read(
    base: &str,
    connection: &str,
    uid: &str,
    delivery: Option<&str>,
) -> Result<(u16, Value)> {
    let query = event_facts_query(connection, uid);
    let timestamp = in_seconds(0);
    let mut request = reqwest::Client::new()
        .get(format!("{base}{EVENT_FACTS_PATH}?{query}"))
        .header("X-Hermes-Timestamp", &timestamp)
        .header(
            "X-Hermes-Signature-256",
            event_facts_signature(&query, &timestamp),
        );
    if let Some(delivery) = delivery {
        request = request.header("X-Hermes-Delivery", delivery);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let body = response.json().await.unwrap_or(Value::Null);
    Ok((status, body))
}

/// The `hermes_event_read` rows, newest first (#355). Its own table, so its
/// own reader: the record is the acceptance criterion here too.
fn recorded_event_reads(
    state_dir: &std::path::Path,
) -> Result<Vec<(String, String, Option<String>, String, Option<i64>)>> {
    let connection = rusqlite::Connection::open_with_flags(
        state_dir.join("consent.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut statement = connection.prepare(
        "SELECT connection, uid, delivery, outcome, found
         FROM hermes_event_read ORDER BY sequence DESC",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

async fn event_metric(base: &str, outcome: &str) -> Result<u64> {
    let text = reqwest::get(format!("{base}/metrics"))
        .await?
        .text()
        .await?;
    let needle =
        format!("twalk_companion_gateway_hermes_event_reads_total{{outcome=\"{outcome}\"}} ");
    Ok(text
        .lines()
        .find_map(|line| line.strip_prefix(&needle))
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0))
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
            // The owner's own time, as the collector answers it (#369), and
            // the gaps spelled in it (#379).
            "timezone": "Europe/Paris",
            "timezone_source": "calendar",
            "now": "2026-09-24T20:36:26+02:00",
            "free": [{
                "start": "2026-09-24T10:30:00Z",
                "end": "2026-09-25T14:00:00Z",
                "start_local": "2026-09-24T12:30:00+02:00",
                "end_local": "2026-09-25T16:00:00+02:00",
                "minutes": 1650,
            }],
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
    // The owner's own time is relayed, unaltered and with its source, because
    // the module that relays it adds nothing to what the collector said
    // (#369). A draft that has to write "jeudi à 12h30" converts with these.
    assert_eq!(body["timezone"], "Europe/Paris");
    assert_eq!(body["timezone_source"], "calendar");
    assert_eq!(body["now"], "2026-09-24T20:36:26+02:00");
    // The gaps are relayed as the collector spelled them, local pair and all:
    // this module adds nothing, and an agent that copies them cannot write an
    // hour in the wrong zone (#379).
    assert_eq!(
        body["free"][0]["start_local"], "2026-09-24T12:30:00+02:00",
        "the gaps did not reach the agent: {body}"
    );
    assert_eq!(body["free"][0]["minutes"], 1650);
    assert_eq!(
        body.as_object().map(|object| object.len()),
        Some(8),
        "connection, from, to, busy, the gaps, the three of the owner's own \
         time, and nothing else: {body}"
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

/// #355: a persona asks what one event carries, and is answered with
/// facts. The point of the test is as much what comes back as what does
/// not: the stub collector is given a description's **length**, because
/// the collector has no route that would give its text, and this asserts
/// that nothing on the way adds one.
#[tokio::test]
async fn a_signed_read_says_what_an_event_carries_and_never_what_it_says() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let connection = unique("calendar");
    let uid = "8f3a2b1c-4d5e-6f70-8192-a3b4c5d6e7f8";
    let collector = StubCollector::answering(
        200,
        json!({
            "connection": connection,
            "uid": uid,
            "found": true,
            "conference": "https://meet.example/abc-def",
            "description_characters": 340,
            "attachments": 1,
        }),
    )
    .await?;
    bus.publish_event(
        CONNECTION_STATUS_SUBJECT,
        &calendar_status_event(&connection, "unknown", "connected"),
    )
    .await?;
    let (gateway, base, state_dir) =
        gateway("event-facts-served", &connection, Some(&collector.url())).await?;

    let (status, body) = poll_until(
        || async {
            let answered = signed_event_read(&base, &connection, uid, Some("delivery-1"))
                .await
                .ok()?;
            (answered.0 != 409).then_some(answered)
        },
        "the Gateway to have read the connection's state off the bus",
    )
    .await?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["found"], json!(true));
    assert_eq!(body["conference"], json!("https://meet.example/abc-def"));
    assert_eq!(body["description_characters"], json!(340));
    assert_eq!(body["attachments"], json!(1));
    assert_eq!(body["uid"], json!(uid));

    // What the Gateway relayed: the collector's own route, the uid as
    // asked, and the service token as the bearer.
    let relayed = collector
        .requests()
        .into_iter()
        .find(|(line, _)| line.contains("/event-facts"))
        .expect("the Gateway relayed the read to the collector");
    assert!(relayed.0.contains(&format!("uid={uid}")), "{}", relayed.0);
    assert!(
        relayed.0.contains(&format!("connection={connection}")),
        "{}",
        relayed.0
    );
    assert_eq!(
        relayed.1.as_deref(),
        Some(&*format!("Bearer {SERVICE_TOKEN}"))
    );

    // The record: who asked, for which event, when, how it went — and
    // **not** what the answer said, beyond whether the event was held. A
    // journal that kept the conference URL would be a copy of the thing
    // the pull was governed for.
    let rows = recorded_event_reads(&state_dir)?;
    let row = rows.first().expect("one read recorded");
    assert_eq!(row.0, connection);
    assert_eq!(row.1, uid);
    assert_eq!(row.2.as_deref(), Some("delivery-1"));
    assert_eq!(row.3, "served");
    assert_eq!(row.4, Some(1), "found is kept, as a flag");
    let stored = std::fs::read(state_dir.join("consent.sqlite3"))?;
    let stored = String::from_utf8_lossy(&stored);
    assert!(
        !stored.contains("meet.example"),
        "the conference URL is an answer, not a record"
    );
    assert!(event_metric(&base, "served").await? >= 1);

    // An event this deployment does not hold is an answer of its own, and
    // a persona must be able to tell it from "the meeting carries
    // nothing".
    collector.answer(
        200,
        json!({
            "connection": connection,
            "uid": "nothing-here",
            "found": false,
            "conference": Value::Null,
            "description_characters": Value::Null,
            "attachments": 0,
        }),
    );
    let (status, body) = signed_event_read(&base, &connection, "nothing-here", None).await?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["found"], json!(false));
    assert_eq!(body["conference"], Value::Null);
    // Found by its uid and not by being the newest row: a test that reads
    // "the last line" asserts the order of everything that ran beside it.
    let rows = recorded_event_reads(&state_dir)?;
    let absent = rows
        .iter()
        .find(|row| row.1 == "nothing-here")
        .expect("the read about an event this deployment does not hold is recorded");
    assert_eq!(absent.4, Some(0), "not held is recorded as such");

    // A collector that stops answering is a 502 and a recorded refusal,
    // never a read that quietly says the event carries nothing.
    collector.stop();
    let (status, body) = signed_event_read(&base, &connection, uid, None).await?;
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"], json!("collector_unreachable"));
    let rows = recorded_event_reads(&state_dir)?;
    let refused = rows
        .iter()
        .find(|row| row.1 == uid)
        .expect("the refused read is recorded under the uid it asked about");
    assert_eq!(refused.3, "collector_unreachable");
    assert!(event_metric(&base, "collector_unreachable").await? >= 1);

    gateway.stop().await;
    Ok(())
}

/// The second script the skill ships (#355), against the real route: its
/// encoding, its canonical line and its headers have to agree with the
/// Gateway's byte for byte, and a script nobody runs is a script that
/// agrees with nothing.
#[tokio::test]
async fn the_skills_event_script_makes_a_request_the_route_accepts() -> Result<()> {
    ensure_stack().await?;
    let bus = bus().await?;
    let connection = unique("calendar");
    // A uid with the characters that break an encoding that is not the
    // route's: sabre writes these, and a `+` left bare reaches the Gateway
    // as a space.
    let uid = "8f3a+2b1c/4d5e:6f70";
    let collector = StubCollector::answering(
        200,
        json!({
            "connection": connection,
            "uid": uid,
            "found": true,
            "conference": "https://meet.example/abc-def",
            "description_characters": 12,
            "attachments": 0,
        }),
    )
    .await?;
    bus.publish_event(
        CONNECTION_STATUS_SUBJECT,
        &calendar_status_event(&connection, "unknown", "connected"),
    )
    .await?;
    let (gateway, base, state_dir) =
        gateway("event-facts-skill", &connection, Some(&collector.url())).await?;
    poll_until(
        || async {
            let (status, _) = signed_event_read(&base, &connection, uid, None)
                .await
                .ok()?;
            (status == 200).then_some(())
        },
        "the connection's state to be read off the bus",
    )
    .await?;
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../skills/twalk-calendar/event-facts.sh")
        .canonicalize()
        .context("the skill's script exists")?;
    let output = tokio::process::Command::new(&script)
        .arg(&connection)
        .arg(uid)
        .env("TWALK_GATEWAY_URL", &base)
        .env("TWALK_ANSWER_SECRET", HERMES_ANSWER_SECRET)
        .env("TWALK_DELIVERY_ID", "event-skill-run-1")
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
    assert_eq!(answer["found"], json!(true), "{answer}");
    assert_eq!(answer["conference"], json!("https://meet.example/abc-def"));
    // The uid survived the encoding on both sides: the row records what was
    // asked, and what was asked is what the script sent.
    let rows = recorded_event_reads(&state_dir)?;
    let row = rows
        .iter()
        .find(|row| row.2.as_deref() == Some("event-skill-run-1"))
        .expect("the script's own read is recorded");
    assert_eq!(row.1, uid, "the uid survived the round trip");
    assert_eq!(row.3, "served");

    collector.stop();
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

/// The skill Twalk ships, both ways in, run as Hermes runs them: the script
/// signs and sends a request the route accepts, and so does the tool server
/// an agent with no shell holds instead (#368). The proof that the document,
/// the script, the tool and the route agree — and the one test that fails if
/// any of the four drifts.
#[tokio::test]
async fn the_skill_reaches_the_route_as_a_script_and_as_a_tool() -> Result<()> {
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
    // The connection may be left out, and that is the point of #363: an agent
    // drafting a reply has no connection id to name — the message it woke for
    // carries the kind and never the id (ADR 0033), and the calendar's is a
    // different connection from the mail's — so the deployment names it once
    // and the script reads it from there. Same read, same record, one more
    // line in it.
    let without_a_connection = tokio::process::Command::new(&script)
        .arg("2026-09-24T08:00:00Z")
        .arg("2026-09-26T18:00:00Z")
        .env("TWALK_GATEWAY_URL", &base)
        .env("TWALK_ANSWER_SECRET", HERMES_ANSWER_SECRET)
        .env("TWALK_CALENDAR_CONNECTION", &connection)
        .env("TWALK_DELIVERY_ID", "skill-run-2")
        .output()
        .await
        .context("the skill's script ran with the connection left out")?;
    let stdout = String::from_utf8_lossy(&without_a_connection.stdout);
    let stderr = String::from_utf8_lossy(&without_a_connection.stderr);
    assert!(
        without_a_connection.status.success(),
        "the script was refused with the connection left out:\nstdout: {stdout}\nstderr: {stderr}"
    );
    let answer: Value = serde_json::from_str(stdout.trim())
        .with_context(|| format!("the script printed the answer as JSON: {stdout}"))?;
    assert_eq!(answer["connection"], json!(connection));
    assert_eq!(
        answer["busy"],
        json!([{ "start": "2026-09-24T09:00:00Z", "end": "2026-09-24T10:30:00Z" }]),
        "the read taken from the environment is the read taken from the argument"
    );
    assert!(
        recorded_reads(&state_dir)?
            .iter()
            .any(|read| read.3.as_deref() == Some("skill-run-2") && read.4 == "served"),
        "a read the agent made without naming a connection is in the record like any other"
    );

    // And when the deployment never set it, the script says which variable is
    // missing rather than reading somebody's calendar by accident. Nothing
    // reaches the Gateway on this path, so the message on stderr is the only
    // thing an operator has: it names the variable (#363's review).
    let unconfigured = tokio::process::Command::new(&script)
        .arg("2026-09-24T08:00:00Z")
        .arg("2026-09-26T18:00:00Z")
        .env("TWALK_GATEWAY_URL", &base)
        .env("TWALK_ANSWER_SECRET", HERMES_ANSWER_SECRET)
        .env_remove("TWALK_CALENDAR_CONNECTION")
        .output()
        .await
        .context("the skill's script ran with nothing to name the connection")?;
    assert!(
        !unconfigured.status.success(),
        "the script read a calendar with no connection named anywhere"
    );
    let said = String::from_utf8_lossy(&unconfigured.stderr);
    assert!(
        said.contains("TWALK_CALENDAR_CONNECTION"),
        "the script failed without naming the variable that is missing: {said}"
    );

    // The document's example is the script's own usage line.
    let document = std::fs::read_to_string(script.with_file_name("SKILL.md"))?;
    assert!(document.contains("./freebusy.sh <connection> <from> <to>"));
    assert!(
        document.contains("./freebusy.sh <from> <to>"),
        "the short form the agent is meant to use is not in the document"
    );
    assert!(document.contains("X-Hermes-Signature-256") && document.contains("X-Hermes-Timestamp"));

    // ---- and the same two reads through the tool, which is the way an agent
    // holds them when it has no shell (#368). Grafted onto this test rather
    // than given its own: a sixth Gateway on this host starves the fifth's
    // bus read, and the script and the tool are the same wire by design — so
    // one stack proving both is the honest shape as well as the cheap one.
    let server = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../skills/twalk-calendar/mcp_server.py")
        .canonicalize()
        .context("the skill's tool server exists")?;
    let mut child = tokio::process::Command::new("python3")
        .arg(&server)
        .env("TWALK_GATEWAY_URL", &base)
        .env("TWALK_ANSWER_SECRET", HERMES_ANSWER_SECRET)
        .env("TWALK_CALENDAR_CONNECTION", &connection)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("the tool server started")?;
    let mut stdin = child.stdin.take().expect("a pipe to write requests into");
    let stdout = child.stdout.take().expect("a pipe to read answers from");
    let mut lines = tokio::io::BufReader::new(stdout).lines();

    // One request, one line back: MCP's stdio transport is newline-delimited
    // JSON-RPC, which is the whole reason a server for it can be one file.
    async fn ask(
        stdin: &mut tokio::process::ChildStdin,
        lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
        request: Value,
    ) -> Result<Value> {
        stdin.write_all(format!("{request}\n").as_bytes()).await?;
        stdin.flush().await?;
        let answered = lines
            .next_line()
            .await?
            .context("the tool server answered a line")?;
        Ok(serde_json::from_str(&answered)?)
    }

    // The handshake, then the listing: this is what a client does before it
    // offers anything to a model, so a server that fails here offers nothing.
    let hello = ask(&mut stdin, &mut lines, json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-03-26", "capabilities": {}},
    }))
    .await?;
    assert_eq!(hello["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(hello["result"]["serverInfo"]["name"], "twalk-calendar");

    let listed = ask(&mut stdin, &mut lines, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).await?;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .context("a list of tools")?
        .iter()
        .map(|tool| tool["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        names,
        vec!["freebusy", "event_facts"],
        "the tool offers exactly the two governed reads, in the order the \
         skill teaches them, and nothing else — no third function is the \
         property this whole ticket exists for"
    );

    let answered = ask(&mut stdin, &mut lines, json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {
            "name": "freebusy",
            "arguments": {
                "from": "2026-09-24T08:00:00Z",
                "to": "2026-09-26T18:00:00Z",
                "reference": "TWALK-REF:assistant:tool-run:1",
            },
        },
    }))
    .await?;
    assert!(
        answered["result"]["isError"].as_bool() != Some(true),
        "the read came back as an error: {answered}"
    );
    let payload: Value =
        serde_json::from_str(answered["result"]["content"][0]["text"].as_str().unwrap_or("{}"))?;
    assert_eq!(
        payload["busy"],
        json!([{ "start": "2026-09-24T09:00:00Z", "end": "2026-09-24T10:30:00Z" }]),
        "the tool's answer is the route's answer, unaltered"
    );
    assert!(
        recorded_reads(&state_dir)?.iter().any(|read| {
            read.3.as_deref() == Some("TWALK-REF:assistant:tool-run:1") && read.4 == "served"
        }),
        "a read made through the tool is in the owner's record, named by the \
         message it was made for"
    );

    // A refusal reaches the model as a result it can act on, not as a
    // protocol error that would tell it the tool is broken.
    let refused = ask(&mut stdin, &mut lines, json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "freebusy", "arguments": {"from": "2026-09-24T08:00:00Z", "to": "2026-10-20T08:00:00Z"}},
    }))
    .await?;
    assert_eq!(refused["result"]["isError"], json!(true));
    let said = refused["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        said.contains("window_too_wide"),
        "a refused read must name its code, or the agent cannot tell the \
         owner what to fix: {said}"
    );
    assert!(
        refused["error"].is_null(),
        "a refused read must not arrive as a JSON-RPC error: {refused}"
    );

    // And a function this server does not have is refused by name rather than
    // reached for: the allowlist is the tool's whole security argument.
    let nothing = ask(&mut stdin, &mut lines, json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": {"name": "terminal", "arguments": {"command": "cat /etc/passwd"}},
    }))
    .await?;
    assert_eq!(nothing["result"]["isError"], json!(true));

    drop(stdin);
    let _ = child.kill().await;
    collector.stop();
    gateway.stop().await;
    Ok(())
}
