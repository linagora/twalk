//! The one governed pull (issue #281), at the collector's process boundary:
//! the internal HTTP endpoint the Companion Gateway relays a free/busy read
//! to, against the fake side service. A read with the Gateway's service
//! token answers the busy intervals of every calendar of the owner, merged,
//! and not a title, a participant or a location; a cancelled event
//! occupies nothing; the endpoint refuses anybody else, a connection it
//! does not hold, a window wider than fourteen days, and a calendar
//! connection that is not connected — each with a code, each counted on
//! `/metrics`.

mod support;

use anyhow::Result;
use serde_json::{json, Value};
use support::Run;
use twalk_test_harness::{ensure_stack, poll_until, Bus};

const SERVICE_TOKEN: &str = "test-service-token";

const MEETING: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:d281-meeting\r\nSUMMARY:Point budget CONFIDENTIEL\r\nLOCATION:Salle Ada Lovelace\r\nDTSTART:20261006T080000Z\r\nDTEND:20261006T090000Z\r\nATTENDEE;CN=Alice Martin:mailto:alice@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
const OVERLAPPING: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:d281-overlap\r\nSUMMARY:Entretien\r\nDTSTART:20261006T083000Z\r\nDTEND:20261006T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
const CANCELLED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:d281-cancelled\r\nSUMMARY:Annulé\r\nSTATUS:CANCELLED\r\nDTSTART:20261006T140000Z\r\nDTEND:20261006T150000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
const ALL_DAY: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:d281-allday\r\nSUMMARY:Déplacement\r\nDTSTART;VALUE=DATE:20261008\r\nDTEND;VALUE=DATE:20261009\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

/// The environment with the endpoint served on a free port, and the
/// metrics on another, so the counts can be read.
fn env_with_endpoint(run: &Run, port: u16, metrics_port: u16) -> Vec<(String, String)> {
    let mut env = run.env_with_gateway();
    env.push((
        "COLLECTOR_HTTP_LISTEN".to_owned(),
        format!("127.0.0.1:{port}"),
    ));
    env.push((
        "COLLECTOR_METRICS_LISTEN".to_owned(),
        format!("127.0.0.1:{metrics_port}"),
    ));
    env
}

async fn metric(metrics_port: u16, outcome: &str) -> Result<u64> {
    let text = reqwest::get(format!("http://127.0.0.1:{metrics_port}/metrics"))
        .await?
        .text()
        .await?;
    let needle = format!("twalk_collector_freebusy_reads_total{{outcome=\"{outcome}\"}} ");
    Ok(text
        .lines()
        .find_map(|line| line.strip_prefix(&needle))
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0))
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

async fn read(
    port: u16,
    token: &str,
    connection: &str,
    from: &str,
    to: &str,
) -> Result<(u16, Value)> {
    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/freebusy"))
        .query(&[("connection", connection), ("from", from), ("to", to)])
        .bearer_auth(token)
        .send()
        .await?;
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    Ok((status, body))
}

async fn wait_for_endpoint(port: u16) -> Result<()> {
    poll_until(
        || async {
            reqwest::Client::new()
                .get(format!("http://127.0.0.1:{port}/freebusy"))
                .send()
                .await
                .ok()
                .map(|_| ())
        },
        "the free/busy endpoint to answer",
    )
    .await
}

#[tokio::test]
async fn a_read_with_the_service_token_answers_busy_intervals_and_nothing_else() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("freebusy").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    run.sso.create_calendar(&run.calendar, "Mine");
    run.sso.put_event(&run.calendar, "meeting", MEETING);
    run.sso.put_event(&run.calendar, "cancelled", CANCELLED);
    run.sso.put_event(&run.calendar, "allday", ALL_DAY);
    // A second calendar: the answer is the owner's agenda, not one calendar's.
    let other = format!("{}-perso", run.calendar);
    run.sso.create_calendar(&other, "Perso");
    run.sso.put_event(&other, "overlap", OVERLAPPING);
    let port = free_port()?;
    let metrics_port = free_port()?;
    let collector = support::CollectorProc::start(&env_with_endpoint(&run, port, metrics_port))?;
    collector
        .wait_logged("calendar taken as it stands", 1)
        .await?;
    wait_for_endpoint(port).await?;
    // The wait above knocked without a token: counted, as every read is.
    let knocks = metric(metrics_port, "unauthenticated").await?;

    let (status, body) = read(
        port,
        SERVICE_TOKEN,
        &run.calendar,
        "2026-10-05T00:00:00Z",
        "2026-10-10T00:00:00Z",
    )
    .await?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        json!({
            "connection": run.calendar,
            "from": "2026-10-05T00:00:00Z",
            "to": "2026-10-10T00:00:00Z",
            "busy": [
                { "start": "2026-10-06T08:00:00Z", "end": "2026-10-06T10:00:00Z" },
                { "start": "2026-10-08T00:00:00Z", "end": "2026-10-09T00:00:00Z" },
            ]
        }),
        "two calendars merged, the cancelled event nothing, the all-day one a day"
    );
    let text = body.to_string();
    for word in ["CONFIDENTIEL", "Lovelace", "alice", "Entretien", "Annulé"] {
        assert!(!text.contains(word), "{word} left the collector");
    }
    // A window clipped: the meeting is cut at the window's edge.
    let (status, body) = read(
        port,
        SERVICE_TOKEN,
        &run.calendar,
        "2026-10-06T08:30:00Z",
        "2026-10-06T09:00:00Z",
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(
        body["busy"],
        json!([{ "start": "2026-10-06T08:30:00Z", "end": "2026-10-06T09:00:00Z" }])
    );

    // Refused, each with its code.
    let (status, body) = read(
        port,
        "not-the-service-token",
        &run.calendar,
        "2026-10-05T00:00:00Z",
        "2026-10-10T00:00:00Z",
    )
    .await?;
    assert_eq!(
        (status, body["error"].as_str()),
        (401, Some("unauthenticated"))
    );
    let (status, body) = read(
        port,
        SERVICE_TOKEN,
        &run.mail,
        "2026-10-05T00:00:00Z",
        "2026-10-10T00:00:00Z",
    )
    .await?;
    assert_eq!(
        (status, body["error"].as_str()),
        (404, Some("connection_unknown")),
        "the mail connection has no agenda"
    );
    let (status, body) = read(
        port,
        SERVICE_TOKEN,
        &run.calendar,
        "2026-10-05T00:00:00Z",
        "2026-10-19T00:00:01Z",
    )
    .await?;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("window_too_wide"))
    );
    let (status, body) = read(port, SERVICE_TOKEN, &run.calendar, "jeudi", "vendredi").await?;
    assert_eq!(
        (status, body["error"].as_str()),
        (400, Some("invalid_window"))
    );

    // The side service refuses the token: the connection is
    // pending_operator on the next round, and the read says so rather
    // than asking a service that would refuse it.
    run.sso.refuse("caldav");
    let refused = poll_until(
        || async {
            let (status, body) = read(
                port,
                SERVICE_TOKEN,
                &run.calendar,
                "2026-10-05T00:00:00Z",
                "2026-10-10T00:00:00Z",
            )
            .await
            .ok()?;
            (status == 409).then_some(body)
        },
        "the read to be refused once the connection is not connected",
    )
    .await?;
    assert_eq!(refused["error"], "connection_not_connected");
    assert_eq!(refused["state"], "pending_operator");

    // Each counted under its code: two served, and one of each refusal —
    // the `409` polled for above may have been counted more than once, and
    // a `caldav_refused` may have slipped in before the state moved.
    assert_eq!(metric(metrics_port, "served").await?, 2);
    assert_eq!(metric(metrics_port, "unauthenticated").await?, knocks + 1);
    for refusal in ["connection_unknown", "window_too_wide", "invalid_window"] {
        assert_eq!(metric(metrics_port, refusal).await?, 1, "{refusal}");
    }
    assert!(metric(metrics_port, "connection_not_connected").await? >= 1);
    collector.stop().await;
    Ok(())
}
