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

use anyhow::{Context, Result};
use serde_json::{json, Value};
use support::Run;
use twalk_test_harness::{ensure_stack, poll_until, Bus};

const SERVICE_TOKEN: &str = "test-service-token";

/// An event written **in a zone**, which the plain fixtures are not: they are
/// in UTC, so they say nothing about where the owner works. This one is what
/// #369's second source reads — the zone their own agenda is written in.
const IN_A_ZONE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Paris\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:d369-zoned\r\nSUMMARY:Point hebdo\r\nDTSTART;TZID=Europe/Paris:20261007T090000\r\nDTEND;TZID=Europe/Paris:20261007T093000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

/// One event as Outlook publishes it, in the window this suite reads (#350):
/// a Windows zone name, which every corporate calendar is full of and which
/// this collector refused until the CLDR table existed. Here to prove the
/// busy interval it produces is the one a calendar client shows — the number
/// the owner's free/busy is built from.
const OUTLOOK: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Microsoft Corporation//Outlook 16.0 MIMEDIR//EN\r\nBEGIN:VTIMEZONE\r\nTZID:Romance Standard Time\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:d350-outlook\r\nSUMMARY:Comit\u{e9} de direction\r\nDTSTART;TZID=Romance Standard Time:20261009T140000\r\nDTEND;TZID=Romance Standard Time:20261009T150000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

const MEETING: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:d281-meeting\r\nSUMMARY:Point budget CONFIDENTIEL\r\nLOCATION:Salle Ada Lovelace\r\nDTSTART:20261006T080000Z\r\nDTEND:20261006T090000Z\r\nATTENDEE;CN=Alice Martin:mailto:alice@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
const OVERLAPPING: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:d281-overlap\r\nSUMMARY:Entretien\r\nDTSTART:20261006T083000Z\r\nDTEND:20261006T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
const CANCELLED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:d281-cancelled\r\nSUMMARY:Annulé\r\nSTATUS:CANCELLED\r\nDTSTART:20261006T140000Z\r\nDTEND:20261006T150000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
/// An event with everything #355 is about: a join link in the property
/// meant for it, a description, and an attachment.
const CARRIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:d355-carries\r\nSUMMARY:Atelier\r\nDESCRIPTION:Ordre du jour que personne n'a décidé de partager\r\nCONFERENCE;VALUE=URI;FEATURE=VIDEO:https://meet.example/atelier\r\nATTACH:https://files.example/plan.pdf\r\nDTSTART:20261007T080000Z\r\nDTEND:20261007T090000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

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

async fn facts(port: u16, token: &str, connection: &str, uid: &str) -> Result<(u16, Value)> {
    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/event-facts"))
        .query(&[("connection", connection), ("uid", uid)])
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

/// #355: what an event carries, at the collector's process boundary and
/// against the fake side service.
///
/// The reason this test exists rather than the unit test alone: reading
/// one event means **fetching its resource again**, and the URL that
/// fetches it is built from a listing's href. The first implementation
/// pasted the href onto the service's base, which passed against a fake
/// whose hrefs matched its base and answered `404` on the reference
/// deployment, where sabre sits behind a `/dav/` relay and returns hrefs
/// relative to its own root. The fake now answers hrefs the way that
/// service does, so this test fails if that mistake is made again.
#[tokio::test]
async fn a_read_says_what_an_event_carries_and_never_its_words() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("event-facts").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    run.sso.create_calendar(&run.calendar, "Mine");
    run.sso.put_event(&run.calendar, "carries", CARRIES);
    run.sso.put_event(&run.calendar, "meeting", MEETING);
    let port = free_port()?;
    let metrics_port = free_port()?;
    let collector = support::CollectorProc::start(&env_with_endpoint(&run, port, metrics_port))?;
    collector
        .wait_logged("calendar taken as it stands", 1)
        .await?;
    wait_for_endpoint(port).await?;

    let (status, body) = facts(port, SERVICE_TOKEN, &run.calendar, "d355-carries").await?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["found"], json!(true), "{body}");
    assert_eq!(body["conference"], json!("https://meet.example/atelier"));
    assert_eq!(
        body["description_characters"],
        json!("Ordre du jour que personne n'a décidé de partager"
            .chars()
            .count())
    );
    assert_eq!(body["attachments"], json!(1));
    // The promise, asserted on the bytes: a length and a count left the
    // collector, and not one word of the agenda or the file's name.
    let answered = body.to_string();
    assert!(!answered.contains("Ordre du jour"), "{answered}");
    assert!(!answered.contains("plan.pdf"), "{answered}");

    // An event with none of the three says so with absences, not zeroes
    // that read as facts.
    let (status, body) = facts(port, SERVICE_TOKEN, &run.calendar, "d281-meeting").await?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["found"], json!(true));
    assert_eq!(body["conference"], Value::Null);
    assert_eq!(body["description_characters"], Value::Null);
    assert_eq!(body["attachments"], json!(0));

    // An event this collector never published: `found: false`, which is a
    // different answer from "it carries nothing".
    let (status, body) = facts(port, SERVICE_TOKEN, &run.calendar, "never-seen").await?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["found"], json!(false), "{body}");

    // And the refusals this endpoint owes: anybody else, a connection it
    // does not hold, a uid that is not one.
    let (status, body) = facts(port, "not-the-token", &run.calendar, "d355-carries").await?;
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["error"], json!("unauthenticated"));
    let (status, body) = facts(port, SERVICE_TOKEN, "another-connection", "d355-carries").await?;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["error"], json!("connection_unknown"));
    let (status, body) = facts(port, SERVICE_TOKEN, &run.calendar, "").await?;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], json!("invalid_uid"));

    // Counted on its own series, apart from the free/busy one.
    let text = reqwest::get(format!("http://127.0.0.1:{metrics_port}/metrics"))
        .await?
        .text()
        .await?;
    assert!(
        text.contains("twalk_collector_event_fact_reads_total{outcome=\"served\"} 3"),
        "three reads served: {text}"
    );
    assert!(text.contains("twalk_collector_event_fact_reads_total{outcome=\"invalid_uid\"} 1"));

    collector.stop().await;
    Ok(())
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
        body["busy"],
        json!([
            { "start": "2026-10-06T08:00:00Z", "end": "2026-10-06T10:00:00Z" },
            { "start": "2026-10-08T00:00:00Z", "end": "2026-10-09T00:00:00Z" },
        ]),
        "two calendars merged, the cancelled event nothing, the all-day one a day"
    );
    assert_eq!(
        body
            .as_object()
            .context("an answer")?
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec![
            "busy",
            "connection",
            "free",
            "from",
            "now",
            "timezone",
            "timezone_source",
            "to"
        ],
        "the answer is a closed list of members, and this is it"
    );
    // The owner's own time, beside the intervals (#369): the zone their
    // calendar declares, where that came from, and the hour it is there.
    assert_eq!(body["timezone"], json!("Europe/Paris"));
    assert_eq!(body["timezone_source"], json!("calendar"));
    let now = body["now"].as_str().context("the local hour")?;
    assert!(
        now.ends_with("+02:00") || now.ends_with("+01:00"),
        "`now` is the hour in the owner's zone, offset and all, not another \
         spelling of UTC: {now}"
    );
    let text = body.to_string();
    for word in ["CONFIDENTIEL", "Lovelace", "alice", "Entretien", "Annulé"] {
        assert!(!text.contains(word), "{word} left the collector");
    }
    // And a calendar nobody ever told a zone to — the ordinary state of a
    // collection on a server whose clients never set one. The three members
    // go together, and their absence is what tells the agent to speak in UTC
    // and say so rather than guess a zone (#369).
    run.sso.forget_calendar_timezone(&run.calendar);
    run.sso.forget_calendar_timezone(&other);
    let (status, body) = read(
        port,
        SERVICE_TOKEN,
        &run.calendar,
        "2026-10-05T00:00:00Z",
        "2026-10-10T00:00:00Z",
    )
    .await?;
    assert_eq!(status, 200, "{body}");
    assert!(
        !body["busy"].as_array().context("intervals")?.is_empty(),
        "a calendar with no declared zone still answers its intervals"
    );
    for member in ["timezone", "timezone_source", "now"] {
        assert!(
            body.get(member).is_none(),
            "{member} was answered for a calendar that declares no zone and events \
             written in none: {body}"
        );
    }
    // The gaps are still answered — they are UTC facts — and their local pair
    // is absent rather than guessed (#379).
    let free = body["free"].as_array().context("the gaps")?;
    assert!(!free.is_empty(), "{body}");
    assert!(
        free.iter().all(|gap| gap.get("start_local").is_none()),
        "a gap was spelled in a zone nobody declared: {body}"
    );

    // But the owner's own events usually say where they work, and on the
    // reference deployment that is the only thing that does: measured on
    // 2026-09-26, its collections declare no zone at all. So one event
    // written in a zone, one poll to read it, and the read answers that zone
    // — saying `events`, because it is a weaker fact than a declaration and
    // the owner is told which they are looking at (#369).
    run.sso.put_event(&run.calendar, "zoned", IN_A_ZONE);
    let body = poll_until(
        || async {
            let (_, body) = read(
                port,
                SERVICE_TOKEN,
                &run.calendar,
                "2026-10-05T00:00:00Z",
                "2026-10-10T00:00:00Z",
            )
            .await
            .ok()?;
            body.get("timezone").is_some().then_some(body)
        },
        "the poll to read an event written in a zone",
    )
    .await?;
    assert_eq!(body["timezone"], json!("Europe/Paris"));
    assert_eq!(body["timezone_source"], json!("events"));
    let now = body["now"].as_str().context("the local hour")?;
    assert!(
        now.ends_with("+02:00") || now.ends_with("+01:00"),
        "the hour is in that zone, offset and all: {now}"
    );
    // And the gaps, spelled in that zone, so a drafting agent copies rather
    // than converts (#379): the arithmetic it was measured getting wrong.
    let free = body["free"].as_array().context("the gaps")?;
    assert!(!free.is_empty(), "a window with room in it answered no gaps: {body}");
    for gap in free {
        let local = gap["start_local"].as_str().context("a local start")?;
        assert!(
            local.ends_with("+02:00") || local.ends_with("+01:00"),
            "a gap was spelled in UTC where the owner's zone was known: {gap}"
        );
        assert!(gap["minutes"].as_i64().unwrap_or_default() > 0, "{gap}");
    }

    // And an Outlook-shaped event reaches the busy intervals (#350). This is
    // the number that matters: a free/busy built without these sixty events
    // describes a week the owner is not living, and the Gateway's own check of
    // a proposed time (#383) would then agree with it, wrongly.
    //
    // 14:00 Paris in October is 12:00 UTC. The fake converts with its own
    // table, so the assertion is on two sides that were written apart — and
    // the 9th, because the 8th is the all-day fixture above and a window that
    // is busy anyway would prove nothing.
    run.sso.put_event(&run.calendar, "outlook", OUTLOOK);
    let body = poll_until(
        || async {
            let (_, body) = read(
                port,
                SERVICE_TOKEN,
                &run.calendar,
                "2026-10-09T00:00:00Z",
                "2026-10-10T00:00:00Z",
            )
            .await
            .ok()?;
            (!body["busy"].as_array()?.is_empty()).then_some(body)
        },
        "the poll to read an event written in a Windows zone",
    )
    .await?;
    assert_eq!(
        body["busy"],
        json!([{ "start": "2026-10-09T12:00:00Z", "end": "2026-10-09T13:00:00Z" }]),
        "an Outlook-shaped event did not reach the owner's busy intervals: {body}"
    );
    // And no gap overlaps it, which is the property a draft's offer rests on.
    for gap in body["free"].as_array().context("the gaps")? {
        let start = gap["start"].as_str().unwrap_or_default();
        let end = gap["end"].as_str().unwrap_or_default();
        assert!(
            end <= "2026-10-09T12:00:00Z" || start >= "2026-10-09T13:00:00Z",
            "a gap overlaps the meeting Outlook wrote: {gap}"
        );
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
    // At least four served: the window, the window clipped, the one taken
    // after the calendars forgot their zone, and however many the wait for
    // the zoned event's poll took (#369) — a count, not a fixed number,
    // because that wait is a poll and a poll has no fixed length.
    assert!(metric(metrics_port, "served").await? >= 4);
    assert_eq!(metric(metrics_port, "unauthenticated").await?, knocks + 1);
    for refusal in ["connection_unknown", "window_too_wide", "invalid_window"] {
        assert_eq!(metric(metrics_port, refusal).await?, 1, "{refusal}");
    }
    assert!(metric(metrics_port, "connection_not_connected").await? >= 1);
    collector.stop().await;
    Ok(())
}
