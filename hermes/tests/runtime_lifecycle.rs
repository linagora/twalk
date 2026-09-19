//! Ticket #23, the Hermes runtime's lifecycle, at the spec's seam 2: the
//! real `twalk-hermes` binary, starting the real persona image against the
//! shared stack's bus, with the stub LLM behind it.
//!
//! Everything is asserted from outside both processes — what appeared on the
//! bus, what Docker says is running, what the runtime logged, what the model
//! was asked. Nothing reaches inside either.
//!
//! Three of these tests are absences, and they are the ones that carry the
//! promises:
//!
//! - **a paused persona runs and receives nothing** (ADR 0013). Activation
//!   is a consent decision, not a control API, and a paused persona is *not*
//!   stopped — so the test asserts both halves: the container is up, and no
//!   `thinking`, no `suggest` and no completion request exist for a message
//!   published while it was paused.
//! - **a persona is handed nothing it does not need** (ADR 0008, ADR 0015).
//!   The runtime holds the Companion Gateway's service token, which also
//!   opens the consent snapshot — the list of every contact. The test looks
//!   for it in the persona container's own environment and requires it to be
//!   absent.
//! - **a runtime that cannot start something says so.** A persona whose
//!   image does not exist must be announced as failed, not retried quietly
//!   in a loop that looks from the outside like a runtime still starting up.

mod harness;

use anyhow::Result;
use harness::{
    contract_fixture, sha256_hex, traceparent_for, validate_against_contract, PersonaFixture,
    RuntimeRun, GATEWAY_SERVICE_TOKEN, GATEWAY_SERVICE_TOKEN_VAR, MODEL, SUGGEST_TYPE,
    THINKING_TYPE, USER_LANGUAGE,
};
use serde_json::{json, Value};

/// The `inbound.message.received` fixture, re-keyed onto a trigger of this
/// run. The body carries the run's marker so that an assertion about what
/// reached the model — or about what did not — is about exactly this
/// message.
fn inbound_message(marker: &str, body: &str) -> Result<Value> {
    let mut event = contract_fixture("inbound.message.received")?;
    let id = sha256_hex(marker);
    event["id"] = json!(id);
    event["traceparent"] = json!(traceparent_for(&id));
    event["consent"] = json!("granted");
    event["data"]["body"] = json!(format!("{body} [{marker}]"));
    event["data"]["attachments"][0]["caption"] = event["data"]["body"].clone();
    validate_against_contract(&event, "inbound.message.received")?;
    Ok(event)
}

fn event_id(event: &Value) -> String {
    event["id"]
        .as_str()
        .expect("a patched fixture has a string id")
        .to_owned()
}

/// The acceptance criterion: starting the runtime with an assistant
/// configuration spawns the persona process and it consumes.
///
/// It carries the injection with it (ADR 0015): the same container's
/// environment is read back from Docker, and it must hold the model the
/// operator named and none of the runtime's own credentials.
#[tokio::test]
async fn an_activated_persona_is_started_and_consumes() -> Result<()> {
    const REPLY: &str = "Pas de problème, à 20h !";
    let run = run_id("started");
    let mut hermes =
        RuntimeRun::start_activated("started", vec![PersonaFixture::assistant(&run)], REPLY)
            .await?;

    hermes.wait_for_persona("assistant").await?;
    hermes.wait_for_log("persona ready").await?;

    let marker = format!("started-{}", hermes.prefix);
    let trigger = inbound_message(&marker, "On décale à 20h ?")?;
    let trigger_id = event_id(&trigger);
    hermes.publish_inbound(&trigger).await?;

    let thinking = hermes.wait_for(THINKING_TYPE, &trigger_id).await?;
    validate_against_contract(&thinking.payload, "persona.thinking.emitted")?;
    let suggest = hermes.wait_for(SUGGEST_TYPE, &trigger_id).await?;
    validate_against_contract(&suggest.payload, "persona.suggest.produced")?;
    assert_eq!(
        suggest.payload["data"]["suggestion"]["body"],
        json!(REPLY),
        "the suggestion is the answer the model the runtime configured gave"
    );

    // The injection, read back from the process the runtime started.
    let environment = hermes.persona_environment("assistant").await?;
    for expected in [
        format!("TWALK_LLM_MODEL={MODEL}"),
        format!("TWALK_LLM_BASE_URL={}", hermes.llm.base_url()),
        "TWALK_PERSONA_ID=assistant".to_owned(),
        "TWALK_PERSONA_CONSUMER=persona-assistant".to_owned(),
        format!("TWALK_BUS_STREAM={}", hermes.stream),
        // ADR 0016's fallback needs a value, and this is the channel it
        // arrives on: beside the model configuration, in the closed list, on
        // the environment the runtime really built (ticket #164).
        format!("TWALK_USER_LANGUAGE={USER_LANGUAGE}"),
    ] {
        assert!(
            environment.contains(&expected),
            "the runtime must inject {expected:?}; the persona's environment was {environment:?}"
        );
    }
    // The absence ADR 0015 exists for: the Gateway's service token opens the
    // consent snapshot — the list of every contact — and a persona has no
    // business holding it to learn which model to call.
    for leaked in environment.iter().filter(|variable| {
        variable.contains(GATEWAY_SERVICE_TOKEN) || variable.starts_with(GATEWAY_SERVICE_TOKEN_VAR)
    }) {
        panic!(
            "the runtime handed the persona {leaked:?}: the Companion Gateway's service token \
             must not cross into a persona's environment (ADR 0015)"
        );
    }
    assert!(
        environment
            .iter()
            .all(|variable| !variable.starts_with("HERMES_")),
        "the runtime's own namespace must not cross into a persona: {environment:?}"
    );

    // The endpoint in this suite is the stub on this host's loopback, which
    // works only because the persona's container shares the host's network
    // namespace. The runtime is the one component that holds the URL, so it
    // is the one that names the trap — before a message ever arrives, and
    // in words that are neither "the model refused" nor "no model
    // configured".
    assert!(
        hermes.logs().await.contains("is on this host's loopback"),
        "the runtime must warn about a loopback endpoint a containerised persona would dial as \
         itself; its logs were:\n{}",
        hermes.logs().await
    );

    assert!(hermes.is_running(), "the runtime is still up");
    hermes.shutdown().await
}

/// The acceptance criterion: killing the persona process makes the runtime
/// restart it, and a second `thinking` event proves it.
#[tokio::test]
async fn killing_the_persona_makes_the_runtime_restart_it() -> Result<()> {
    const REPLY: &str = "Je regarde et je te dis.";
    let run = run_id("restart");
    let hermes =
        RuntimeRun::start_activated("restart", vec![PersonaFixture::assistant(&run)], REPLY)
            .await?;

    hermes.wait_for_persona("assistant").await?;
    hermes.wait_for_log("persona ready").await?;

    let marker = format!("restart-before-{}", hermes.prefix);
    let before = inbound_message(&marker, "Tu es libre jeudi ?")?;
    let before_id = event_id(&before);
    hermes.publish_inbound(&before).await?;
    hermes.wait_for(THINKING_TYPE, &before_id).await?;

    // The crash.
    hermes.kill_persona("assistant").await?;
    hermes.wait_for_log("persona exited").await?;
    hermes.wait_for_persona("assistant").await?;
    hermes.wait_for_log("persona ready").await?;

    let marker = format!("restart-after-{}", hermes.prefix);
    let after = inbound_message(&marker, "Et vendredi ?")?;
    let after_id = event_id(&after);
    hermes.publish_inbound(&after).await?;

    // The second thinking event: it can only exist if a persona process is
    // consuming again.
    let thinking = hermes.wait_for(THINKING_TYPE, &after_id).await?;
    validate_against_contract(&thinking.payload, "persona.thinking.emitted")?;
    let thinking_events = hermes.published(THINKING_TYPE).await?;
    assert_eq!(
        thinking_events.len(),
        2,
        "one thinking event per message, before and after the restart"
    );

    hermes.shutdown().await
}

/// ADR 0013, asserted as the absence it is: a paused persona **runs** and
/// receives nothing. Nobody has decided about this persona, which is the
/// state every persona starts in — activation never spreads on its own.
#[tokio::test]
async fn a_paused_persona_runs_and_receives_nothing() -> Result<()> {
    let run = run_id("paused");
    let hermes = RuntimeRun::start("paused", vec![PersonaFixture::assistant(&run)]).await?;

    hermes.wait_for_persona("assistant").await?;
    hermes.wait_for_log("persona ready").await?;

    let marker = format!("paused-{}", hermes.prefix);
    let trigger = inbound_message(&marker, "Tu peux me rappeler ?")?;
    hermes.publish_inbound(&trigger).await?;

    // Long enough for the persona to have pulled several times: its fetch
    // timeout is 2s.
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;

    assert!(
        hermes.published(THINKING_TYPE).await?.is_empty(),
        "a paused persona is handed nothing, so it thinks about nothing"
    );
    assert!(hermes.published(SUGGEST_TYPE).await?.is_empty());
    assert_eq!(
        hermes.llm_requests_mentioning(&marker).len(),
        0,
        "no message reached the model"
    );
    // The half that makes it a pause rather than a stop.
    assert!(
        hermes.persona_is_running("assistant").await?,
        "a paused persona still runs (ADR 0013): it is not stopped, it is handed nothing"
    );

    hermes.shutdown().await
}

/// A persona whose image does not run must **say so**. The failure this
/// guards against is the opposite: a runtime restarting something forever,
/// quietly, indistinguishable from a runtime that is still starting up.
#[tokio::test]
async fn a_persona_whose_image_does_not_run_is_announced_as_failed() -> Result<()> {
    let run = run_id("unstartable");
    let mut hermes = RuntimeRun::start(
        "unstartable",
        vec![PersonaFixture::unstartable(&run, "assistant")],
    )
    .await?;

    hermes.wait_for_log("persona failed to start").await?;

    let logs = hermes.logs().await;
    assert!(
        logs.contains("assistant"),
        "the verdict names the persona: {logs}"
    );
    assert!(
        !logs.contains("hermes stopped"),
        "one persona that cannot start does not take the runtime down with it: {logs}"
    );
    assert!(
        hermes.is_running(),
        "the runtime keeps supervising its other personas"
    );
    assert!(
        !hermes.persona_is_running("assistant").await?,
        "nothing is running under that persona's name"
    );

    hermes.shutdown().await
}

/// The acceptance criterion: the runtime shuts down cleanly on SIGTERM —
/// and takes its personas with it, which is the part an operator only finds
/// out about when a second Hermes will not start.
#[tokio::test]
async fn the_runtime_shuts_down_cleanly_on_sigterm() -> Result<()> {
    let run = run_id("sigterm");
    let mut hermes = RuntimeRun::start_activated(
        "sigterm",
        vec![PersonaFixture::assistant(&run)],
        "À tout à l'heure.",
    )
    .await?;

    hermes.wait_for_persona("assistant").await?;
    hermes.wait_for_log("persona ready").await?;

    let status = hermes.terminate().await?;
    assert!(
        status.success(),
        "SIGTERM is a clean stop, not a failure: the runtime exited with {status}"
    );

    let logs = hermes.logs().await;
    for expected in [
        "asking the persona to stop",
        "persona stopped",
        "hermes stopped",
    ] {
        assert!(
            logs.contains(expected),
            "the shutdown says what it did: {expected:?} is missing from {logs}"
        );
    }
    assert!(
        !hermes.persona_is_running("assistant").await?,
        "a supervisor that exits must not leave its personas running: an orphan still holds the \
         durable consumer and would answer messages nobody is supervising"
    );

    hermes.shutdown().await
}

/// A container name unique to this run of this test: the persona containers
/// live on the host's Docker, which is shared with every other suite.
fn run_id(test_name: &str) -> String {
    format!(
        "h23-{test_name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos()
            % 1_000_000
    )
}
