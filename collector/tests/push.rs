//! The mail collector listens rather than polls (issue #277): with the fake
//! server offering push, a delivery reaches the bus without the poll
//! interval elapsing; with the socket cut, the next poll catches it and the
//! socket comes back on its own; a state the server forgot is recovered
//! from the look-back window without a duplicate and without a loss; the
//! cursor survives a restart.

mod support;

use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;
use support::{CollectorProc, Run, OWNER};
use twalk_test_harness::jmap_fake::FakeMail;
use twalk_test_harness::{ensure_stack, Bus};

const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";
/// The poll interval of these runs: long enough that a mail arriving well
/// inside it can only have come through the push.
const POLL: Duration = Duration::from_secs(12);

async fn messages_of(bus: &Bus, run: &Run) -> Result<Vec<Value>> {
    run.events_of(bus, MESSAGE_SUBJECT, &run.mail).await
}

async fn wait_for_messages(bus: &Bus, run: &Run, at_least: usize) -> Result<Vec<Value>> {
    run.wait_for_events(bus, MESSAGE_SUBJECT, &run.mail, at_least)
        .await
}

/// The push listener backs off doubling up to a minute between attempts
/// while the socket refuses it, so its return after a cut is waited for
/// longer than the harness's twenty seconds.
async fn wait_logged_within(
    collector: &CollectorProc,
    needle: &str,
    times: usize,
    within: Duration,
) -> Result<()> {
    let started = Instant::now();
    while collector.count_logged(needle).await < times {
        anyhow::ensure!(
            started.elapsed() < within,
            "timed out waiting {times}× {needle:?} within {within:?}\n{}",
            collector.logs().await.join("\n")
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(())
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
    let delivered = Instant::now();
    run.sso.deliver(FakeMail::from_person(
        "Alice Martin",
        "alice@example.org",
        OWNER,
        "Pushed",
        "Arrived by push",
    ));
    // The wake is in the log within seconds — the socket's latency, not
    // the bus's — and the mail is on the bus before the interval is up.
    wait_logged_within(
        &collector,
        "waking the mail poll",
        1,
        Duration::from_secs(5),
    )
    .await?;
    let woke = delivered.elapsed();
    let messages = wait_for_messages(&bus, &run, 1).await?;
    let took = delivered.elapsed();
    assert!(
        took < POLL,
        "the mail took {took:?} to reach the bus (woken after {woke:?}), which is not push with a {POLL:?} poll"
    );
    assert_eq!(messages[0]["data"]["title"], "Pushed");
    assert!(run.sso.pushes() >= 1, "the server pushed a StateChange");

    // Cut: the socket closes, the collector says so and polls; the next
    // mail arrives all the same — later, and without a push.
    run.sso.cut_push();
    collector.wait_logged("polling until it is back", 1).await?;
    let pushes_before = run.sso.pushes();
    let polls_before = collector.count_logged("mailbox polled").await;
    run.sso.deliver(FakeMail::from_person(
        "Alice Martin",
        "alice@example.org",
        OWNER,
        "Polled",
        "Arrived by poll",
    ));
    // The poll interval, then the bus.
    wait_logged_within(
        &collector,
        "mailbox polled",
        polls_before + 1,
        POLL + Duration::from_secs(10),
    )
    .await?;
    let messages = wait_for_messages(&bus, &run, 2).await?;
    assert_eq!(messages[1]["data"]["title"], "Polled");
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

    // Back: the socket reopens on its own, and the next delivery is pushed
    // again.
    run.sso.restore_push();
    wait_logged_within(&collector, "push is on", 2, Duration::from_secs(90)).await?;
    let delivered = Instant::now();
    run.sso.deliver(FakeMail::from_person(
        "Alice Martin",
        "alice@example.org",
        OWNER,
        "Pushed again",
        "Back by push",
    ));
    wait_logged_within(
        &collector,
        "waking the mail poll",
        2,
        Duration::from_secs(5),
    )
    .await?;
    wait_for_messages(&bus, &run, 3).await?;
    assert!(delivered.elapsed() < POLL);
    collector
        .assert_never_logged(&["Arrived by", "Back by"])
        .await;
    collector.stop().await;
    Ok(())
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
    run.sso.deliver(FakeMail::from_person(
        "Alice Martin",
        "alice@example.org",
        OWNER,
        "First",
        "Before the loss",
    ));
    wait_for_messages(&bus, &run, 1).await?;
    collector.wait_logged("mailbox polled", 2).await?;

    // The server forgets every state up to now: the next `Email/changes`
    // from the persisted one is refused, the look-back lists everything
    // the INBOX received since — the first mail among it — and only what
    // was never published is published.
    run.sso.forget_mail_states_before(run.sso.mail_state() + 1);
    run.sso.deliver(FakeMail::from_person(
        "Bob",
        "bob@example.org",
        OWNER,
        "Second",
        "After the loss",
    ));
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
    let titles: Vec<&str> = messages
        .iter()
        .filter_map(|m| m["data"]["title"].as_str())
        .collect();
    assert_eq!(titles, ["First", "Second", "Third"]);
    // Two more polls with a state the server serves again: nothing twice.
    collector.wait_logged("mailbox polled", 5).await?;
    assert_eq!(
        messages_of(&bus, &run).await?.len(),
        3,
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
    assert_eq!(messages[3]["data"]["title"], "Fourth");
    collector.wait_logged("mailbox polled", 2).await?;
    assert_eq!(messages_of(&bus, &run).await?.len(), 4);
    assert_eq!(
        collector.count_logged("mailbox taken as it stands").await,
        0,
        "a restart resumes from the cursor, it does not start over"
    );
    collector
        .assert_never_logged(&["Before the loss", "After the loss", "While down"])
        .await;
    let stored = run.stored_bytes()?;
    assert!(!stored.contains("loss"), "the state holds ids, never words");
    collector.stop().await;
    Ok(())
}
