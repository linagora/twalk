//! The mail collector listens rather than polls (issue #277): with the fake
//! server offering push, a delivery reaches the bus without the poll
//! interval elapsing; with the socket cut, the next poll catches it and the
//! socket comes back on its own; a state the server forgot is recovered
//! from the look-back window without a duplicate and without a loss; the
//! cursor survives a restart. Beside the criteria: a session offering no
//! push leaves the collector to its poll, and a session naming no ticket
//! endpoint opens the socket with the bearer.

mod support;

use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;
use support::{CollectorProc, Run, OWNER};
use twalk_test_harness::jmap_fake::FakeMail;
use twalk_test_harness::{ensure_stack, Bus};

const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";
/// The poll interval of the push runs: long enough that a mail arriving well
/// inside it can only have come through the push.
const POLL: Duration = Duration::from_secs(12);
/// How long a wake may take to show in the log after a delivery: the
/// socket's latency, generously.
const WAKE_WITHIN: Duration = Duration::from_secs(5);
/// How long the socket may take to come back once restored: the listener
/// backs off doubling up to a minute while the fake refuses it.
const RECONNECT_WITHIN: Duration = Duration::from_secs(90);

async fn messages_of(bus: &Bus, run: &Run) -> Result<Vec<Value>> {
    run.events_of(bus, MESSAGE_SUBJECT, &run.mail).await
}

async fn wait_for_messages(bus: &Bus, run: &Run, at_least: usize) -> Result<Vec<Value>> {
    run.wait_for_events(bus, MESSAGE_SUBJECT, &run.mail, at_least)
        .await
}

fn slow_polls(run: &Run) -> Vec<(String, String)> {
    let mut env = run.env_with_gateway();
    for (key, value) in env.iter_mut() {
        if key == "COLLECTOR_MAIL_POLL_SECONDS" || key == "COLLECTOR_HEALTH_INTERVAL_SECONDS" {
            *value = POLL.as_secs().to_string();
        }
    }
    env
}

fn from_alice(subject: &str, text: &str) -> FakeMail {
    FakeMail::from_person("Alice Martin", "alice@example.org", OWNER, subject, text)
}

fn titles(messages: &[Value]) -> Vec<&str> {
    messages
        .iter()
        .filter_map(|message| message["data"]["title"].as_str())
        .collect()
}

/// A delivery, and the proof that the poll which published it was the
/// woken one: the wake is in the log within seconds, and before the poll
/// that followed the delivery — not an interval poll landing by chance.
async fn delivered_by_push(
    bus: &Bus,
    run: &Run,
    collector: &CollectorProc,
    mail: FakeMail,
    nth_wake: usize,
    messages_so_far: usize,
) -> Result<Vec<Value>> {
    let polls_before = collector.count_logged("mailbox polled").await;
    let delivered = Instant::now();
    run.sso.deliver(mail);
    collector
        .wait_logged_for("waking the mail poll", nth_wake, WAKE_WITHIN)
        .await?;
    let messages = wait_for_messages(bus, run, messages_so_far + 1).await?;
    let took = delivered.elapsed();
    assert!(
        took < POLL,
        "the mail took {took:?} to reach the bus, which is not push with a {POLL:?} poll"
    );
    let wake = collector
        .position_logged("waking the mail poll", nth_wake)
        .await;
    let poll = collector
        .position_logged("mailbox polled", polls_before + 1)
        .await;
    assert!(
        wake < poll,
        "the poll that published it ran before the wake, so it was the interval's"
    );
    Ok(messages)
}

#[tokio::test]
async fn a_delivery_wakes_the_poll_through_push_and_the_poll_catches_it_when_the_socket_is_cut(
) -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("push").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    let collector = CollectorProc::start(&slow_polls(&run))?;
    collector
        .wait_logged("mailbox taken as it stands", 1)
        .await?;
    collector.wait_logged("push is on", 1).await?;

    // Pushed: the mail is on the bus in a fraction of the poll interval.
    let messages = delivered_by_push(
        &bus,
        &run,
        &collector,
        from_alice("Pushed", "Arrived by push"),
        1,
        0,
    )
    .await?;
    assert_eq!(titles(&messages), ["Pushed"]);
    assert!(run.sso.pushes() >= 1, "the server pushed a StateChange");

    // Cut: the socket closes, the collector says so and polls; the next
    // mail arrives all the same — an interval later, and without a push.
    run.sso.cut_push();
    collector.wait_logged("polling until it is back", 1).await?;
    let pushes_before = run.sso.pushes();
    let polls_before = collector.count_logged("mailbox polled").await;
    run.sso.deliver(from_alice("Polled", "Arrived by poll"));
    collector
        .wait_logged_for(
            "mailbox polled",
            polls_before + 1,
            POLL + Duration::from_secs(10),
        )
        .await?;
    let messages = wait_for_messages(&bus, &run, 2).await?;
    assert_eq!(titles(&messages), ["Pushed", "Polled"]);
    assert_eq!(
        run.sso.pushes(),
        pushes_before,
        "nothing was pushed while cut"
    );
    assert_eq!(
        collector.count_logged("waking the mail poll").await,
        1,
        "no wake while cut"
    );

    // Back: the socket reopens on its own and rings the poll once for what
    // arrived meanwhile; the next delivery is pushed again.
    run.sso.restore_push();
    collector
        .wait_logged_for("push is on", 2, RECONNECT_WITHIN)
        .await?;
    collector
        .wait_logged("reading what arrived while it was down", 1)
        .await?;
    let messages = delivered_by_push(
        &bus,
        &run,
        &collector,
        from_alice("Pushed again", "Back by push"),
        2,
        2,
    )
    .await?;
    assert_eq!(titles(&messages), ["Pushed", "Polled", "Pushed again"]);
    collector
        .assert_never_logged(&["Arrived by", "Back by"])
        .await;
    collector.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_session_offering_no_push_leaves_the_collector_to_its_poll() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("nopush").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    run.sso.offer_no_push();
    let collector = run.start_with_gateway()?;
    collector
        .wait_logged("mailbox taken as it stands", 1)
        .await?;
    collector
        .wait_logged("the JMAP server offers no push", 1)
        .await?;
    run.sso.deliver(from_alice("Polled", "By the poll alone"));
    let messages = wait_for_messages(&bus, &run, 1).await?;
    assert_eq!(titles(&messages), ["Polled"]);
    assert_eq!(run.sso.pushes(), 0);
    collector.wait_logged("mailbox polled", 4).await?;
    assert_eq!(
        collector
            .count_logged("the JMAP server offers no push")
            .await,
        1,
        "said once"
    );
    collector
        .assert_never_logged(&["push is on", "waking the mail poll", "By the poll"])
        .await;
    collector.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_session_naming_no_ticket_endpoint_opens_the_socket_with_the_bearer() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("bearer").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    run.sso.offer_no_ticket();
    let collector = CollectorProc::start(&slow_polls(&run))?;
    collector
        .wait_logged("mailbox taken as it stands", 1)
        .await?;
    collector.wait_logged("push is on", 1).await?;
    let messages = delivered_by_push(
        &bus,
        &run,
        &collector,
        from_alice("Pushed", "With the bearer"),
        1,
        0,
    )
    .await?;
    assert_eq!(titles(&messages), ["Pushed"]);
    collector.assert_never_logged(&["With the bearer"]).await;
    collector.stop().await;
    Ok(())
}

/// An instant `seconds` before now, as a JMAP `receivedAt`.
fn seconds_ago(seconds: u64) -> String {
    let at = time::OffsetDateTime::now_utc() - Duration::from_secs(seconds);
    at.replace_nanosecond(0)
        .unwrap_or(at)
        .format(&time::format_description::well_known::Rfc3339)
        .expect("an RFC 3339 instant")
}

#[tokio::test]
async fn a_forgotten_state_is_recovered_without_a_duplicate_and_the_cursor_survives_a_restart(
) -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("recover").await?;
    run.authorize().await?;
    run.serve_snapshot(&bus, Vec::new()).await?;
    let collector = run.start_with_gateway()?;
    collector
        .wait_logged("mailbox taken as it stands", 1)
        .await?;
    run.sso.deliver(from_alice("First", "Before the loss"));
    wait_for_messages(&bus, &run, 1).await?;
    collector.wait_logged("mailbox polled", 2).await?;

    // The server forgets every state up to now: the next `Email/changes`
    // from the persisted one is refused, the look-back lists what the
    // INBOX received since five minutes before the last read — the first
    // mail among it, set aside as published — and what the window does
    // not reach is not the recovery's to find.
    run.sso.forget_mail_states_before(run.sso.mail_state() + 1);
    run.sso.deliver(
        FakeMail::from_person("Bob", "bob@example.org", OWNER, "Second", "After the loss")
            .received(&seconds_ago(4 * 60)),
    );
    run.sso.deliver(
        FakeMail::from_person(
            "Eve",
            "eve@example.org",
            OWNER,
            "Before the window",
            "Too old",
        )
        .received(&seconds_ago(6 * 60)),
    );
    run.sso.deliver(FakeMail::from_person(
        "Carol",
        "carol@example.org",
        OWNER,
        "Third",
        "Also after",
    ));
    let messages = wait_for_messages(&bus, &run, 3).await?;
    collector
        .wait_logged("recovered from the look-back window", 1)
        .await?;
    assert_eq!(titles(&messages), ["First", "Second", "Third"]);
    // Two more polls with a state the server serves again: nothing twice,
    // and nothing from before the window.
    collector.wait_logged("mailbox polled", 5).await?;
    assert_eq!(
        titles(&messages_of(&bus, &run).await?),
        ["First", "Second", "Third"],
        "no duplicate after the recovery"
    );

    // The cursor survives a restart: what was read is not read again, what
    // arrived while the collector was down is.
    collector.stop().await;
    run.sso.deliver(FakeMail::from_person(
        "Dave",
        "dave@example.org",
        OWNER,
        "Fourth",
        "While down",
    ));
    let collector = run.start_with_gateway()?;
    let messages = wait_for_messages(&bus, &run, 4).await?;
    assert_eq!(titles(&messages), ["First", "Second", "Third", "Fourth"]);
    collector.wait_logged("mailbox polled", 2).await?;
    assert_eq!(messages_of(&bus, &run).await?.len(), 4);
    assert_eq!(
        collector.count_logged("mailbox taken as it stands").await,
        0,
        "a restart resumes from the cursor, it does not start over"
    );
    collector
        .assert_never_logged(&["Before the loss", "After the loss", "Too old", "While down"])
        .await;
    let stored = run.stored_bytes()?;
    assert!(
        !stored.contains("loss") && !stored.contains("Too old"),
        "the state holds ids, never words"
    );
    collector.stop().await;
    Ok(())
}
