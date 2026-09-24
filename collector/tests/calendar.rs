//! The owner's calendars at the process boundary (issue #280): the collector
//! started against the fake SSO, the fake side service and the test stack's
//! bus — what the calendar already held is taken as the state and not
//! published; an event created, moved and removed afterwards is the three
//! contract types; an unchanged CTag publishes nothing; a participant the
//! owner revoked on the mail connection is withheld, one never decided about
//! is carried — and nothing of a description or a withheld person reaches
//! the bus, the log, or the state directory.

mod support;

use anyhow::Result;
use serde_json::{json, Value};
use support::{sha256_hex, Run, CONSENT_SUBJECT, OWNER};
use twalk_test_harness::{ensure_stack, validate_against_contract, Bus};

const DESCRIPTION: &str = "Notes nobody decided to share";

const WEEKLY: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:8f3a2b1c-weekly\r\nSUMMARY:Weekly sync\r\nDESCRIPTION:Notes nobody decided to share\r\nDTSTART;TZID=Europe/Paris:20261005T090000\r\nDTEND;TZID=Europe/Paris:20261005T093000\r\nRRULE:FREQ=WEEKLY;BYDAY=MO\r\nORGANIZER;CN=Michel Maudet:mailto:michel@example.com\r\nATTENDEE;CN=Michel Maudet;ROLE=CHAIR;PARTSTAT=ACCEPTED:mailto:michel@example.com\r\nATTENDEE;CN=Alice Martin;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:alice@example.org\r\nATTENDEE;ROLE=OPT-PARTICIPANT;PARTSTAT=NEEDS-ACTION:mailto:bob@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

/// A standing meeting the calendar already held before the collector ever
/// ran: the past, which is not published.
const STANDING: &str = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:standing-1\r\nSUMMARY:Standing meeting\r\nDTSTART;TZID=Europe/Paris:20261001T140000\r\nDTEND;TZID=Europe/Paris:20261001T150000\r\nATTENDEE;CN=Carol:mailto:carol@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

/// The calendar events this run's connection published, of one type.
async fn events_of(bus: &Bus, run: &Run, kind: &str) -> Result<Vec<Value>> {
    run.events_of(
        bus,
        &format!("twalk.calendar.event.{kind}.v1"),
        &run.calendar,
    )
    .await
}

async fn wait_for(bus: &Bus, run: &Run, kind: &str, at_least: usize) -> Result<Vec<Value>> {
    run.wait_for_events(
        bus,
        &format!("twalk.calendar.event.{kind}.v1"),
        &run.calendar,
        at_least,
    )
    .await
}

#[tokio::test]
async fn what_the_calendar_held_is_not_published_and_create_change_remove_are_the_three_types(
) -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("three").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    // The calendar's id on the side service is this run's own, so two runs
    // on the shared bus never produce one href — and so one id.
    let collection = run.sso.create_calendar(&run.calendar, "Mine");
    run.sso.put_event(&run.calendar, "standing", STANDING);
    let collector = run.start_with_gateway()?;

    // The first poll takes the calendar as it stands: nothing published,
    // and said. Two more polls with the same CTag: nothing read, nothing
    // published.
    collector
        .wait_logged("calendar taken as it stands", 1)
        .await?;
    collector.wait_logged("calendars polled", 3).await?;
    assert!(events_of(&bus, &run, "created").await?.is_empty());

    // Created, after the collector was watching: the event whole, and
    // nothing of the description.
    let first_etag = run.sso.put_event(&run.calendar, "weekly", WEEKLY);
    let created = wait_for(&bus, &run, "created", 1).await?;
    let created = &created[0];
    validate_against_contract(created, "calendar.event.created")?;
    assert_eq!(created["subject"], format!("mailto:{OWNER}"));
    assert_eq!(created["connection"], run.calendar);
    assert!(created.get("consent").is_none() && created.get("network").is_none());
    assert_eq!(
        created["source"],
        format!(
            "caldav://{}{collection}",
            run.sso
                .caldav_url()
                .trim_start_matches("http://")
                .trim_end_matches('/')
        )
    );
    assert_eq!(created["data"]["title"], "Weekly sync");
    assert_eq!(created["data"]["start"], "2026-10-05T09:00:00+02:00");
    assert_eq!(created["data"]["participants"].as_array().unwrap().len(), 3);
    assert_eq!(created["data"]["participants_withheld"], 0);
    assert!(!created.to_string().contains(DESCRIPTION), "{created}");
    assert_eq!(
        created["id"],
        sha256_hex(&format!("caldav:{collection}weekly.ics:{first_etag}"))
    );
    assert_eq!(
        events_of(&bus, &run, "created").await?.len(),
        1,
        "the standing meeting was never published"
    );

    // Moved by an hour: one `changed`, naming start and end. A standing
    // meeting renamed is a `changed` too — the cursor knew it.
    let moved = WEEKLY
        .replace("20261005T090000", "20261005T100000")
        .replace("20261005T093000", "20261005T103000");
    let second_etag = run.sso.put_event(&run.calendar, "weekly", &moved);
    run.sso.put_event(
        &run.calendar,
        "standing",
        &STANDING.replace(
            "SUMMARY:Standing meeting",
            "SUMMARY:Standing meeting, renamed",
        ),
    );
    let changed = wait_for(&bus, &run, "changed", 2).await?;
    let weekly = changed
        .iter()
        .find(|event| event["data"]["event"]["uid"] == "8f3a2b1c-weekly")
        .expect("the weekly's change");
    validate_against_contract(weekly, "calendar.event.changed")?;
    assert_eq!(weekly["data"]["changed_fields"], json!(["start", "end"]));
    assert_eq!(
        weekly["data"]["event"]["start"],
        "2026-10-05T10:00:00+02:00"
    );
    assert_eq!(
        weekly["id"],
        sha256_hex(&format!("caldav:{collection}weekly.ics:{second_etag}"))
    );
    let standing = changed
        .iter()
        .find(|event| event["data"]["event"]["uid"] == "standing-1")
        .expect("the standing meeting's change");
    assert_eq!(standing["data"]["changed_fields"], json!(["title"]));
    assert_eq!(
        events_of(&bus, &run, "created").await?.len(),
        1,
        "a standing meeting that changes is not created out of nowhere"
    );

    // Removed: the uid and the last title, and nobody in it.
    let ctag = run.sso.remove_event(&run.calendar, "weekly");
    let removed = wait_for(&bus, &run, "removed", 1).await?;
    let removed = &removed[0];
    validate_against_contract(removed, "calendar.event.removed")?;
    assert_eq!(
        removed["data"],
        json!({ "uid": "8f3a2b1c-weekly", "title": "Weekly sync" })
    );
    assert_eq!(
        removed["id"],
        sha256_hex(&format!("caldav:{collection}weekly.ics:removed:{ctag}"))
    );
    assert!(!removed.to_string().contains("alice"), "{removed}");

    // The description is in no log line and in no stored byte.
    collector.assert_never_logged(&[DESCRIPTION]).await;
    let stored = run.stored_bytes()?;
    assert!(
        !stored.contains(DESCRIPTION),
        "the description reached the state directory"
    );
    assert!(
        stored.contains("standing-1") && stored.contains("Standing meeting, renamed"),
        "the cursor holds the event as published"
    );
    collector.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_participant_revoked_on_the_mail_connection_is_withheld_and_one_never_decided_is_carried(
) -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("withheld").await?;
    run.authorize().await?;
    // Bob was revoked on the mail connection before the collector started:
    // the snapshot says so. Alice was never decided about.
    run.serve_snapshot(
        &bus,
        vec![run.decided_on_mail("mailto:bob@example.org", "revoked")],
    )
    .await?;
    run.sso.create_calendar(&run.calendar, "Mine");
    let collector = run.start_with_gateway()?;
    collector
        .wait_logged("calendar taken as it stands", 1)
        .await?;
    run.sso.put_event(&run.calendar, "weekly", WEEKLY);

    let created = wait_for(&bus, &run, "created", 1).await?;
    let created = &created[0];
    validate_against_contract(created, "calendar.event.created")?;
    let participants = created["data"]["participants"].as_array().unwrap();
    assert_eq!(participants.len(), 2, "{created}");
    assert!(participants
        .iter()
        .any(|p| p["identity"] == "mailto:alice@example.org"));
    assert!(participants
        .iter()
        .any(|p| p["identity"] == format!("mailto:{OWNER}")));
    assert_eq!(created["data"]["participants_withheld"], 1);
    assert!(!created.to_string().contains("bob"), "{created}");

    // Then the owner revokes Alice, on the mail connection, while the
    // collector runs: the decision arrives over the stream — applied, the
    // log says — and the next version of the event carries her no more.
    let occurred_at = "2026-09-20T11:00:00Z";
    let decision = json!({
        "specversion": "1.0",
        "id": sha256_hex(&format!("contact:mailto:alice@example.org:revoked:{}:{occurred_at}", run.mail)),
        "source": "gateway://test/consent",
        "type": "fr.linagora.twalk.consent.state.changed.v1",
        "time": occurred_at,
        "subject": "mailto:alice@example.org",
        "datacontenttype": "application/json",
        "network": "email",
        "data": {
            "subject": { "type": "contact", "id": "mailto:alice@example.org" },
            "old_state": "pending",
            "new_state": "revoked",
            "scope": { "connections": [run.mail], "networks": ["email"] },
            "occurred_at": occurred_at,
            "actor": "@michel:example.com"
        }
    });
    bus.publish_event(CONSENT_SUBJECT, &decision).await?;
    collector.wait_logged("applied a consent change", 1).await?;
    let renamed = WEEKLY.replace("SUMMARY:Weekly sync", "SUMMARY:Weekly sync (moved room)");
    run.sso.put_event(&run.calendar, "weekly", &renamed);
    let changed = wait_for(&bus, &run, "changed", 1).await?;
    let changed = &changed[0];
    validate_against_contract(changed, "calendar.event.changed")?;
    let participants = changed["data"]["event"]["participants"].as_array().unwrap();
    assert_eq!(participants.len(), 1, "{changed}");
    assert_eq!(participants[0]["identity"], format!("mailto:{OWNER}"));
    assert_eq!(changed["data"]["event"]["participants_withheld"], 2);
    let fields = changed["data"]["changed_fields"].as_array().unwrap();
    assert!(
        fields.contains(&json!("title")) && fields.contains(&json!("participants")),
        "{changed}"
    );

    // Bob — withheld from the first publication — is in no log line and
    // in no stored byte; the description neither.
    collector
        .assert_never_logged(&["bob@example.org", DESCRIPTION])
        .await;
    let stored = run.stored_bytes()?;
    assert!(
        !stored.contains("bob@example.org"),
        "a withheld participant reached the state directory"
    );
    assert!(!stored.contains(DESCRIPTION));
    collector.stop().await;
    Ok(())
}

/// #348: the question a collector asks a calendar is bounded.
///
/// `PROPFIND Depth: 1` asks for every resource a collection has ever
/// held. On the reference deployment's real calendar that did not answer
/// inside thirty seconds, while a bounded question on the same server and
/// the same credential answered in the same second — and the fake, which
/// held a handful of events and answered instantly, is what let it ship.
///
/// So the listing is a `calendar-query` over a window, and this test puts
/// the two things that matter side by side: a calendar holding hundreds
/// of events outside the window, and one inside it. Only the second is
/// ever seen — not published, not even remembered in the cursor.
#[tokio::test]
async fn a_calendar_is_asked_about_a_window_and_the_rest_is_never_read() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("window").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, vec![]).await?;

    run.sso.create_calendar(&run.calendar, "Mine");
    // Five hundred events years away: the history a real calendar has and
    // a fake never had.
    for index in 0..500 {
        let far = format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:far-{index}\r\nSUMMARY:Far away {index}\r\nDTSTART:2029{:02}{:02}T090000Z\r\nDTEND:2029{:02}{:02}T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            (index % 12) + 1,
            (index % 27) + 1,
            (index % 12) + 1,
            (index % 27) + 1
        );
        run.sso
            .put_event(&run.calendar, &format!("far-{index}"), &far);
    }
    let collector = run.start()?;
    collector
        .wait_logged("calendar taken as it stands", 1)
        .await?;

    // One event inside the window, created after the collector started.
    let soon_start = twalk_test_harness::jmap_fake::now_rfc3339();
    let soon = format!(
        "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:soon-1\r\nSUMMARY:Point de la semaine\r\nDTSTART:{}\r\nDTEND:{}\r\nATTENDEE;CN=Alice:mailto:alice@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        soon_start.replace(['-', ':'], "").split('.').next().unwrap_or_default(),
        soon_start.replace(['-', ':'], "").split('.').next().unwrap_or_default()
    );
    run.sso.put_event(&run.calendar, "soon-1", &soon);

    let created = wait_for(&bus, &run, "created", 1).await?;
    assert_eq!(created.len(), 1, "one event, the one in the window");
    assert_eq!(created[0]["data"]["title"], json!("Point de la semaine"));

    // The five hundred are not published, and not remembered either: a
    // cursor that held them would be the same unbounded thing one layer
    // down.
    let stored = run.stored_bytes()?;
    assert!(
        !stored.contains("far-"),
        "the cursor remembers a calendar's history: it should hold only its window"
    );
    assert!(
        events_of(&bus, &run, "changed").await?.is_empty()
            && events_of(&bus, &run, "removed").await?.is_empty()
    );
    collector.stop().await;
    Ok(())
}
