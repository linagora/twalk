//! The collector at its process boundary (issue #274): started with a grant
//! against the fake SSO and the test stack's bus, it says on the bus what
//! state each connection is in, and each transition is the contract's
//! `connection.status.changed.v1`.

mod support;

use anyhow::Result;
use serde_json::Value;
use support::{CollectorProc, Run, STREAM};
use twalk_test_harness::{ensure_stack, poll_until, validate_against_contract, Bus};

const STATUS_SUBJECT: &str = "twalk.connection.status.changed.v1";

/// The status events about one connection on the bus, in order, from this
/// run's start.
async fn states_of(bus: &Bus, run: &Run, connection: &str) -> Result<Vec<Value>> {
    Ok(bus
        .fetch_since(STREAM, STATUS_SUBJECT, run.since)
        .await?
        .into_iter()
        .filter(|event| event["subject"].as_str() == Some(connection))
        .collect())
}

async fn wait_for_state(bus: &Bus, run: &Run, connection: &str, state: &str) -> Result<Value> {
    let connection = connection.to_owned();
    let state = state.to_owned();
    poll_until(
        || async {
            states_of(bus, run, &connection)
                .await
                .ok()?
                .into_iter()
                .find(|event| event["data"]["to_state"].as_str() == Some(state.as_str()))
        },
        &format!("{connection} to reach {state} on the bus"),
    )
    .await
}

/// The same, for a state a connection has already been in once: the last
/// such event, and only when its hint is the one the test is about.
async fn wait_for_hint(
    bus: &Bus,
    run: &Run,
    connection: &str,
    state: &str,
    needle: &str,
) -> Result<Value> {
    let connection = connection.to_owned();
    let state = state.to_owned();
    let needle = needle.to_owned();
    poll_until(
        || async {
            states_of(bus, run, &connection)
                .await
                .ok()?
                .into_iter()
                .rev()
                .find(|event| {
                    event["data"]["to_state"].as_str() == Some(state.as_str())
                        && event["data"]["hint"]
                            .as_str()
                            .is_some_and(|hint| hint.contains(&needle))
                })
        },
        &format!("{connection} to reach {state} saying {needle:?}"),
    )
    .await
}

#[tokio::test]
async fn a_connection_says_connected_then_reconnect_required_when_the_grant_is_revoked(
) -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("revoked").await?;
    run.authorize().await?;
    let collector = run.start()?;

    let connected = wait_for_state(&bus, &run, &run.mail, "connected").await?;
    validate_against_contract(&connected, "connection.status.changed")?;
    assert_eq!(
        connected["data"]["from_state"], "unknown",
        "the first event of a run"
    );
    assert_eq!(connected["data"]["kind"], "email");
    assert_eq!(connected["connection"], run.mail);
    assert_eq!(
        connected["source"],
        format!("collector://collector.test/connections/{}", run.mail)
    );
    // The calendar connection of the same grant: its own event.
    let calendar = wait_for_state(&bus, &run, &run.calendar, "connected").await?;
    assert_eq!(calendar["data"]["kind"], "calendar");

    // The SSO revokes the grant while the collector's access token is
    // still fresh by its own clock. A service answers 401 to it; the
    // collector does not take a service's word for `pending_operator` on a
    // token it has not just renewed — it renews first, the SSO answers
    // `invalid_grant`, and that is `reconnect_required`, from `connected`,
    // in the same run, without a restart.
    run.sso.revoke();
    let refused = wait_for_state(&bus, &run, &run.mail, "reconnect_required").await?;
    validate_against_contract(&refused, "connection.status.changed")?;
    assert_eq!(refused["data"]["from_state"], "connected");
    assert!(
        states_of(&bus, &run, &run.mail)
            .await?
            .iter()
            .all(|event| event["data"]["to_state"] != "pending_operator"),
        "a revocation is never reported as the operator's misconfiguration"
    );
    assert_eq!(refused["data"]["service"], "sso");
    let hint = refused["data"]["hint"].as_str().unwrap_or_default();
    assert!(hint.contains("authorize --renew"), "{hint}");
    collector
        .assert_never_logged(&["refresh-", "access-"])
        .await;
    collector.stop().await;
    Ok(())
}

/// #279: an SSO that does not answer when the collector starts — the
/// deployment came up before it, or its discovery document is not served
/// — is `unreachable` on the bus with the grant left alone, not a
/// `reconnect_required` sending the operator to sign in again for nothing;
/// and the collector keeps running rather than exiting for compose to
/// restart, so it is `connected` the round after the SSO answers.
#[tokio::test]
async fn an_sso_that_does_not_answer_at_start_is_unreachable_and_the_grant_is_kept() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("silent-sso").await?;
    run.authorize().await?;
    run.sso.silence("sso");
    let collector = run.start()?;
    let silent = wait_for_state(&bus, &run, &run.mail, "unreachable").await?;
    validate_against_contract(&silent, "connection.status.changed")?;
    assert_eq!(silent["data"]["from_state"], "unknown");
    assert_eq!(silent["data"]["service"], "sso");
    assert!(
        states_of(&bus, &run, &run.mail)
            .await?
            .iter()
            .all(|event| event["data"]["to_state"] != "reconnect_required"),
        "a silent SSO is not a revoked grant"
    );
    collector
        .wait_logged("the SSO could not be reached", 1)
        .await?;
    run.sso.restore("sso");
    let connected = wait_for_state(&bus, &run, &run.mail, "connected").await?;
    assert_eq!(connected["data"]["from_state"], "unreachable");
    assert_eq!(
        collector.count_logged("collector starting").await,
        1,
        "the collector did not restart"
    );
    collector.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_service_refusing_a_fresh_token_is_pending_operator_on_its_own_connection_only(
) -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("refusing").await?;
    run.authorize().await?;
    run.sso.refuse("caldav");
    let collector = run.start()?;

    let calendar = wait_for_state(&bus, &run, &run.calendar, "pending_operator").await?;
    validate_against_contract(&calendar, "connection.status.changed")?;
    assert_eq!(calendar["data"]["service"], "caldav");
    // The service asked for a bearer and refused this one, so the hint is
    // the one about the client's audience — the operator's work is at the
    // SSO (#320: and only in this case).
    let hint = calendar["data"]["hint"].as_str().unwrap_or_default();
    assert!(hint.contains("caldav"), "{calendar}");
    assert!(
        hint.contains("an audience or a scope the operator has to add to the client at the SSO"),
        "{hint}"
    );
    // The mail connection is fine: the grant stands and JMAP takes it.
    let mail = wait_for_state(&bus, &run, &run.mail, "connected").await?;
    assert!(mail["data"].get("hint").is_none());
    assert!(
        states_of(&bus, &run, &run.mail)
            .await?
            .iter()
            .all(|event| event["data"]["to_state"] != "pending_operator"),
        "the calendar's refusal is not the mailbox's state"
    );

    // A service that refuses **without saying how to authenticate** is the
    // other morning (#320): not an OAuth resource server at all — the URL
    // names something else, a Cozy instance where the deployment expected
    // an OpenPaaS side service. Same state, since the operator still has
    // work to do; a different sentence, because "add an audience at the
    // SSO" would send them to fix something nobody said was wrong. The
    // connection is left in `pending_operator` on purpose: an operator who
    // acted on the first sentence has to be told the second one, so the
    // hint changing is itself a change worth publishing.
    run.sso.refuse_without_challenge("caldav");
    let silent_refusal = wait_for_hint(
        &bus,
        &run,
        &run.calendar,
        "pending_operator",
        "offered no authentication challenge",
    )
    .await?;
    let hint = silent_refusal["data"]["hint"].as_str().unwrap_or_default();
    assert!(
        hint.contains("offered no authentication challenge"),
        "{hint}"
    );
    assert!(
        hint.contains("no scope added at the SSO will change this answer"),
        "{hint}"
    );
    assert!(
        !hint.contains("an audience or a scope the operator has to add"),
        "the two refusals do not share a sentence: {hint}"
    );
    assert_eq!(
        silent_refusal["data"]["from_state"], "pending_operator",
        "the connection did not move; the reason did"
    );
    run.sso.restore("caldav");
    wait_for_state(&bus, &run, &run.calendar, "connected").await?;

    // And a service that does not answer is unreachable, which is neither.
    run.sso.silence("jmap");
    let silent = wait_for_state(&bus, &run, &run.mail, "unreachable").await?;
    assert_eq!(silent["data"]["service"], "jmap");
    collector.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_grant_for_another_account_publishes_nothing_and_names_the_account() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    // The grant is obtained at an SSO whose account is not the owner's.
    let run = Run::prepare_as("stranger", "somebody@example.com").await?;
    run.authorize().await?;
    let collector = run.start()?;

    let pending = wait_for_state(&bus, &run, &run.mail, "pending_operator").await?;
    assert!(
        pending["data"]["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("another account"),
        "{pending}"
    );
    assert!(
        collector
            .logs()
            .await
            .iter()
            .any(|line| line.contains("somebody@example.com")
                && line.contains("nothing is published")),
        "the account is named in the log"
    );
    collector.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_registry_that_cannot_be_read_is_refused_at_start() -> Result<()> {
    ensure_stack().await?;
    let run = Run::prepare("unregistered").await?;
    run.authorize().await?;
    // A Gateway that answers a registry without this connection: the fake
    // SSO stands in for the Gateway's snapshot route with an empty registry
    // — it answers 404 there, which the collector reads as a refusal to
    // read the registry at all; a wrong registry is asserted in the unit
    // test of `Config::refuse_unknown_connections`.
    let mut env = run.env();
    env.push(("COLLECTOR_GATEWAY_URL".to_owned(), run.sso.issuer()));
    env.push((
        "COLLECTOR_GATEWAY_SERVICE_TOKEN".to_owned(),
        "token".to_owned(),
    ));
    let mut collector = CollectorProc::start(&env)?;
    let status = collector.exit_status().await?;
    assert!(
        !status.success(),
        "a registry that cannot be read is not a start"
    );
    assert!(
        collector
            .logs()
            .await
            .iter()
            .any(|line| line.contains("registry")),
        "the refusal names the registry"
    );
    Ok(())
}

/// #321: a collector that holds a mailbox and no calendar asks the
/// mailbox and nothing else.
///
/// The cost of asking anyway was not hypothetical: on the reference
/// deployment the calendar service is somebody else's production host,
/// and the collector was sending it a request every health round and
/// reporting its refusal as a state — for a connection the deployment had
/// decided not to hold. A service nobody reads has no state to be in, and
/// no URL to configure either.
#[tokio::test]
async fn a_collector_holding_one_connection_asks_one_service() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("mail-only").await?;
    run.authorize().await?;
    // The calendar service would refuse if it were asked, which is what
    // makes the silence load-bearing: a request would be a state on the
    // bus, and there is none.
    run.sso.refuse("caldav");
    let mut env = run.env();
    env.retain(|(name, _)| {
        name != "COLLECTOR_CALENDAR_CONNECTION" && name != "COLLECTOR_CALDAV_URL"
    });
    let collector = CollectorProc::start(&env)?;

    let connected = wait_for_state(&bus, &run, &run.mail, "connected").await?;
    assert_eq!(connected["data"]["kind"], "email");
    assert!(
        states_of(&bus, &run, &run.calendar).await?.is_empty(),
        "a connection this process does not hold has no state on the bus"
    );
    assert!(
        !run.sso.was_asked("caldav"),
        "the calendar service was asked for: {:?}",
        run.sso.paths()
    );
    assert!(run.sso.was_asked("jmap"), "{:?}", run.sso.paths());
    collector.stop().await;
    Ok(())
}

/// And the configuration follows the connections: a calendar connection
/// with no URL to read it at is refused by name, rather than a process
/// that starts and asks nothing (#321, and #342 which needs a
/// calendar-only collector to exist at all).
#[tokio::test]
async fn a_connection_without_its_service_url_is_refused_by_name() -> Result<()> {
    ensure_stack().await?;
    let run = Run::prepare("no-url").await?;
    run.authorize().await?;
    let mut env = run.env();
    env.retain(|(name, _)| name != "COLLECTOR_CALDAV_URL");
    let mut collector = CollectorProc::start(&env)?;
    let status = collector.exit_status().await?;
    assert!(!status.success(), "a connection with no URL is not a start");
    let logs = collector.logs().await;
    assert!(
        logs.iter()
            .any(|line| line.contains("COLLECTOR_CALDAV_URL") && line.contains("calendar")),
        "the refusal names the variable and the connection that wants it: {logs:?}"
    );
    Ok(())
}
