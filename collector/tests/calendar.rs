//! The owner's calendars at the process boundary (issue #280): the collector
//! started against the fake SSO, the fake side service and the test stack's
//! bus — an event created, moved and removed in a calendar is the three
//! contract types; an unchanged CTag publishes nothing; a participant the
//! owner revoked on the mail connection is withheld, one never decided about
//! is carried.

use std::process::Stdio;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::process::Command;
use twalk_collector::oidc::{Client, Settings};
use twalk_test_harness::sso::{write_client_secret, CLIENT_ID};
use twalk_test_harness::{
    ensure_stack, nats_url, poll_until, validate_against_contract, Bus, FakeSso,
};

const OWNER: &str = "michel@example.com";
const STREAM: &str = "twalk";
const CONSENT_SUBJECT: &str = "twalk.consent.state.changed.v1";

const WEEKLY: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:8f3a2b1c-weekly\r\nSUMMARY:Weekly sync\r\nDESCRIPTION:Notes nobody decided to share\r\nDTSTART;TZID=Europe/Paris:20261005T090000\r\nDTEND;TZID=Europe/Paris:20261005T093000\r\nRRULE:FREQ=WEEKLY;BYDAY=MO\r\nORGANIZER;CN=Michel Maudet:mailto:michel@example.com\r\nATTENDEE;CN=Michel Maudet;ROLE=CHAIR;PARTSTAT=ACCEPTED:mailto:michel@example.com\r\nATTENDEE;CN=Alice Martin;ROLE=REQ-PARTICIPANT;PARTSTAT=ACCEPTED:mailto:alice@example.org\r\nATTENDEE;ROLE=OPT-PARTICIPANT;PARTSTAT=NEEDS-ACTION:mailto:bob@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

struct Run {
    sso: FakeSso,
    dir: tempfile::TempDir,
    mail: String,
    calendar: String,
    /// The bus's head when the run was prepared: what this run publishes is
    /// after it, and the shared bus's history is not walked.
    since: u64,
}

impl Run {
    async fn prepare(name: &str, bus: &Bus) -> Result<Self> {
        let since = bus.head(STREAM).await?;
        let sso = FakeSso::start(OWNER).await?;
        let dir = tempfile::tempdir()?;
        write_client_secret(dir.path())?;
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        Ok(Self {
            sso,
            dir,
            mail: format!("mail-{name}-{unique}"),
            calendar: format!("cal-{name}-{unique}"),
            since,
        })
    }

    async fn authorize(&self) -> Result<()> {
        let client = Client::discover(Settings {
            issuer: self.sso.issuer(),
            client_id: CLIENT_ID.to_owned(),
            client_secret_file: self.dir.path().join("client-secret"),
            redirect_uri: "http://localhost:1/callback".to_owned(),
            scopes: vec!["openid".to_owned(), "offline_access".to_owned()],
            grant_file: self.dir.path().join("oidc").join("grant.json"),
        })
        .await?;
        let started = client.begin_authorization()?;
        let callback = self.sso.sign_in(&started.authorization_url)?;
        client.complete_authorization(&started, &callback).await?;
        Ok(())
    }

    /// The Gateway's snapshot, as this fake stands in for it: both
    /// connections in the registry, the decisions given, the stream to be
    /// followed from just past its current head.
    async fn serve_snapshot(&self, bus: &Bus, entries: Vec<Value>) -> Result<()> {
        let head = bus.last_sequence(STREAM, CONSENT_SUBJECT).await?;
        self.sso.serve_gateway_snapshot(json!({
            "stream": STREAM,
            "subject": CONSENT_SUBJECT,
            "stream_sequence": head,
            "next_stream_sequence": head + 1,
            "decision_sequence": entries.len(),
            "connections": [
                { "id": self.mail, "kind": "email", "network": "email" },
                { "id": self.calendar, "kind": "calendar" },
            ],
            "entries": entries,
        }));
        Ok(())
    }

    fn revoked_on_mail(&self, identity: &str) -> Value {
        json!({
            "subject": { "type": "contact", "id": identity },
            "connection": self.mail,
            "network": "email",
            "state": "revoked",
            "decided_at": "2026-09-20T10:00:00.000Z",
            "decision_sequence": 1
        })
    }

    fn start(&self) -> Result<tokio::process::Child> {
        Command::new(env!("CARGO_BIN_EXE_twalk-collector"))
            .env("COLLECTOR_STATE_DIR", self.dir.path())
            .env("COLLECTOR_OIDC_ISSUER", self.sso.issuer())
            .env("COLLECTOR_OIDC_CLIENT_ID", CLIENT_ID)
            .env(
                "COLLECTOR_OIDC_CLIENT_SECRET_FILE",
                self.dir.path().join("client-secret"),
            )
            .env("COLLECTOR_OIDC_REDIRECT_URI", "http://localhost:1/callback")
            .env("COLLECTOR_JMAP_SESSION_URL", self.sso.jmap_session_url())
            .env("COLLECTOR_CALDAV_URL", self.sso.caldav_url())
            .env("COLLECTOR_OWNER_EMAIL", OWNER)
            .env("COLLECTOR_MAIL_CONNECTION", &self.mail)
            .env("COLLECTOR_CALENDAR_CONNECTION", &self.calendar)
            .env("COLLECTOR_GATEWAY_URL", self.sso.issuer())
            .env("COLLECTOR_GATEWAY_SERVICE_TOKEN", "test-service-token")
            .env("COLLECTOR_NATS_URL", nats_url())
            .env("COLLECTOR_HOST", "collector.test")
            .env("COLLECTOR_HEALTH_INTERVAL_SECONDS", "1")
            .env("COLLECTOR_LOG_LEVEL", "info,twalk_collector=debug")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start the collector binary")
    }
}

/// The calendar events this run's connection published, of one type, in order.
async fn events_of(bus: &Bus, run: &Run, kind: &str) -> Result<Vec<Value>> {
    Ok(bus
        .fetch_since(
            STREAM,
            &format!("twalk.calendar.event.{kind}.v1"),
            run.since,
        )
        .await?
        .into_iter()
        .filter(|event| event["connection"].as_str() == Some(run.calendar.as_str()))
        .collect())
}

async fn wait_for(bus: &Bus, run: &Run, kind: &str, at_least: usize) -> Result<Vec<Value>> {
    let kind = kind.to_owned();
    poll_until(
        || async {
            let events = events_of(bus, run, &kind).await.ok()?;
            (events.len() >= at_least).then_some(events)
        },
        &format!("{at_least} calendar.event.{kind} about {}", run.calendar),
    )
    .await
}

#[tokio::test]
async fn create_change_and_remove_are_the_three_types_and_an_unchanged_ctag_publishes_nothing(
) -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("three", &bus).await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    // The calendar's id on the side service is this run's own, so two runs
    // on the shared bus never produce one href — and so one id.
    let collection = run.sso.create_calendar(&run.calendar, "Mine");
    let first_etag = run.sso.put_event(&run.calendar, "weekly", WEEKLY);
    let _collector = run.start()?;

    // Created: the event whole, as the fixture has it, and nothing of the
    // description.
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
    assert!(!created.to_string().contains("Notes nobody"), "{created}");
    assert_eq!(
        created["id"],
        sha256_hex(&format!("caldav:{collection}weekly.ics:{first_etag}"))
    );

    // Polled again with the same CTag: nothing more.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert_eq!(events_of(&bus, &run, "created").await?.len(), 1);
    assert!(events_of(&bus, &run, "changed").await?.is_empty());

    // Moved by an hour: one `changed`, naming start and end.
    let moved = WEEKLY
        .replace("20261005T090000", "20261005T100000")
        .replace("20261005T093000", "20261005T103000");
    let second_etag = run.sso.put_event(&run.calendar, "weekly", &moved);
    let changed = wait_for(&bus, &run, "changed", 1).await?;
    let changed = &changed[0];
    validate_against_contract(changed, "calendar.event.changed")?;
    assert_eq!(changed["data"]["changed_fields"], json!(["start", "end"]));
    assert_eq!(
        changed["data"]["event"]["start"],
        "2026-10-05T10:00:00+02:00"
    );
    assert_eq!(changed["data"]["event"]["uid"], "8f3a2b1c-weekly");
    assert_eq!(
        changed["id"],
        sha256_hex(&format!("caldav:{collection}weekly.ics:{second_etag}"))
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
    Ok(())
}

#[tokio::test]
async fn a_participant_revoked_on_the_mail_connection_is_withheld_and_one_never_decided_is_carried(
) -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("withheld", &bus).await?;
    run.authorize().await?;
    // Bob was revoked on the mail connection before the collector started:
    // the snapshot says so. Alice was never decided about.
    run.serve_snapshot(&bus, vec![run.revoked_on_mail("mailto:bob@example.org")])
        .await?;
    run.sso.create_calendar(&run.calendar, "Mine");
    run.sso.put_event(&run.calendar, "weekly", WEEKLY);
    let _collector = run.start()?;

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
    // collector runs: the decision arrives over the stream, and the next
    // version of the event carries her no more.
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
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
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
    Ok(())
}

fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(input.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
