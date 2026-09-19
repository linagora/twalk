//! Issue #184: the runtime reads the Companion Gateway's settings, at the
//! runtime's own process boundary.
//!
//! > A language set in the Companion reaches a persona's prompt on the
//! > reference deployment, **with nothing written into `.env`**.
//!
//! That is the ticket's first acceptance criterion and the first test below,
//! and the emphasis is the whole of it. `#98` built the Gateway's half and
//! `#164` built the runtime's and the SDK's, and nothing joined them:
//! `twalk-hermes` had no HTTP client at all, so a user who changed their
//! language saw the Companion confirm it and nothing happen. A test that
//! configured the value on both sides would have passed on the broken code.
//!
//! What "nothing written into `.env`" means at this seam is exact and
//! asserted: the runtime's own environment carries **no**
//! `HERMES_LLM_BASE_URL`, `HERMES_LLM_MODEL`, `HERMES_LLM_API_KEY`,
//! `HERMES_LLM_PARAMS` or `HERMES_USER_LANGUAGE`
//! (`harness::no_operator_model`). The only voice is a stub Companion Gateway
//! serving `GET /api/settings/runtime`, which refuses a read that does not
//! carry the deployment's service token.
//!
//! What a stub LLM can be asked is what it was sent, so that is what is
//! asserted — including what it was *not* sent. The four tests are the four
//! decisions the ticket had to take:
//!
//! 1. the Companion's stored preference reaches a persona's prompt, and the
//!    token that fetched it does not reach the persona at all;
//! 2. the precedence, in the other direction: what the operator pinned on the
//!    host wins, for the model **and** for the language, and the runtime says
//!    which voice each field came from;
//! 3. a Gateway that does not answer does not stop the runtime, and the
//!    deployment says what it fell back to;
//! 4. a deployment nobody has named a model for hosts nothing, says that, and
//!    starts its personas the moment a model is named — no restart, because
//!    the retry exists to end an outage.

mod harness;

use anyhow::Result;
use harness::{
    contract_fixture, no_operator_model, sha256_hex, traceparent_for, validate_against_contract,
    PersonaFixture, RuntimeRun, StubGateway, StubLlm, SUGGEST_TYPE, THINKING_TYPE,
};
use serde_json::{json, Value};

/// The model name the Companion holds, distinctive enough that finding it in a
/// chat-completions request can only mean it travelled from the Gateway.
const GATEWAY_MODEL: &str = "a-model-chosen-in-the-companion";

/// The model an operator pinned in `.env`, for the other direction.
const PINNED_MODEL: &str = "a-model-the-operator-pinned";

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

/// A granted inbound message whose body is the ambiguous case ADR 0016
/// legislates for: `test` is the same word in French and in English, so the
/// model has to be told what to fall back to.
fn ambiguous_message(marker: &str) -> Result<Value> {
    let mut event = contract_fixture("inbound.message.received")?;
    let id = sha256_hex(marker);
    event["id"] = json!(id);
    event["traceparent"] = json!(traceparent_for(&id));
    event["consent"] = json!("granted");
    event["data"]["body"] = json!(format!("test [{marker}]"));
    event["data"]["attachments"][0]["caption"] = event["data"]["body"].clone();
    validate_against_contract(&event, "inbound.message.received")?;
    Ok(event)
}

/// The ticket's first acceptance criterion, and the one that fails on the code
/// as it stood: a language set in the Companion reaches a persona's prompt with
/// nothing on the host.
#[tokio::test]
async fn a_language_set_in_the_companion_reaches_a_personas_prompt() -> Result<()> {
    const REPLY: &str = "Bien reçu !";
    let run = run_id("gwlang");

    // The endpoint has to exist before the document that names it, which is
    // why the stub LLM is the test's here and not the harness's.
    let llm = StubLlm::start_with_reply(REPLY).await?;
    let gateway = StubGateway::start(StubGateway::runtime_document(
        &llm.base_url(),
        GATEWAY_MODEL,
        Some("fr"),
    ))
    .await?;

    let mut host = no_operator_model();
    host.push(("HERMES_GATEWAY_URL", Some(gateway.base_url())));
    let hermes = RuntimeRun::start_configured(
        "gwlang",
        vec![PersonaFixture::assistant(&run)],
        &["assistant"],
        llm,
        host,
    )
    .await?;

    hermes.wait_for_persona("assistant").await?;
    hermes.wait_for_log("persona ready").await?;

    let marker = format!("gwlang-{}", hermes.prefix);
    let trigger = ambiguous_message(&marker)?;
    let trigger_id = trigger["id"].as_str().expect("a string id").to_owned();
    hermes.publish_inbound(&trigger).await?;

    let suggest = hermes.wait_for(SUGGEST_TYPE, &trigger_id).await?;
    validate_against_contract(&suggest.payload, "persona.suggest.produced")?;

    let requests = hermes.llm_requests_mentioning(&marker);
    assert_eq!(requests.len(), 1, "one message, one completion request");
    let request = &requests[0];
    let prompt = request.body["messages"][0]["content"]
        .as_str()
        .expect("the persona frames the request with a system prompt")
        .to_owned();

    // The criterion itself. The preference exists only in the Gateway's
    // document, and it is in the prompt the model was actually sent.
    assert!(
        prompt.contains("write in French"),
        "the language the user set in the Companion must reach the prompt, and nothing was \
         written into the runtime's environment: {prompt}"
    );
    assert!(
        prompt.contains("the user's own language"),
        "and it must arrive as the fallback rather than as the subject (ADR 0016): {prompt}"
    );
    // ADR 0016 is not inverted on the way through: the message's own language
    // still governs, and the fallback is still conditional.
    let by_the_message = prompt
        .find("same language as the message")
        .unwrap_or_else(|| panic!("the message's own language must still govern: {prompt}"));
    assert!(
        by_the_message
            < prompt
                .find("write in French")
                .expect("the fallback was just asserted"),
        "a prompt that named the user's language first would invert ADR 0016: {prompt}"
    );
    assert_eq!(
        prompt.matches("French").count(),
        1,
        "the user's language is named once, in the fallback, and nowhere else: {prompt}"
    );

    // The second acceptance criterion, in the same request: the model
    // configuration travelled the same way.
    assert_eq!(
        request.body["model"].as_str(),
        Some(GATEWAY_MODEL),
        "the model the user chose in the Companion is the one the persona called: {:?}",
        request.body
    );
    assert_eq!(
        request.authorization.as_deref(),
        Some(format!("Bearer {}", harness::LLM_API_KEY).as_str()),
        "and the endpoint credential the Gateway served is the one it was called with"
    );

    // The rule that must not be broken. The Gateway's service token opens the
    // consent snapshot — the list of every contact — as well as these settings
    // (ADR 0010, ADR 0015). The runtime holds it and has just used it; the
    // persona must hold nothing of the kind. `run-persona-image.sh` forwards
    // every variable it is given, so a leak would show up here.
    let environment = hermes.persona_environment("assistant").await?;
    for leaked in environment.iter().filter(|variable| {
        variable.contains(harness::GATEWAY_SERVICE_TOKEN)
            || variable.starts_with(harness::GATEWAY_SERVICE_TOKEN_VAR)
            || variable.starts_with("HERMES_")
    }) {
        panic!(
            "the runtime handed the persona {leaked:?}: reading the Gateway's settings must add \
             no credential to a persona's environment (ADR 0015)"
        );
    }
    assert!(
        environment.contains(&"TWALK_USER_LANGUAGE=fr".to_owned()),
        "what crosses is the answer, not the credential that fetched it: {environment:?}"
    );

    // And the read is one read: the decision this ticket took is that the
    // settings are read at startup, not polled for the life of the deployment.
    assert_eq!(
        gateway.reads(),
        1,
        "the runtime reads the settings once, at startup"
    );
    assert_eq!(
        gateway.unauthenticated_reads(),
        0,
        "and it authenticates that read the way the Gateway requires"
    );

    // Which the operator is told, because a restart-to-apply nobody knows
    // about is a silent trap.
    let logs = hermes.logs().await;
    assert!(
        logs.contains("takes effect when this runtime is restarted"),
        "the runtime must say that a preference changed from now on needs a restart; its logs \
         were:\n{logs}"
    );
    assert!(
        logs.contains("language=gateway") && logs.contains("model=gateway"),
        "and it must say which voice each value came from:\n{logs}"
    );

    hermes.shutdown().await
}

/// The precedence, in the other direction, and the same order for the model as
/// for the language: ADR 0015 puts what the operator supplied on the host above
/// what was set from the browser, so a value pinned in `.env` wins and the
/// runtime says so.
#[tokio::test]
async fn the_operators_own_configuration_wins_over_the_companions() -> Result<()> {
    let run = run_id("pinned");
    let llm = StubLlm::start().await?;
    // The Gateway holds a different model and a different language: if either
    // won, this test would see it.
    let gateway = StubGateway::start(StubGateway::runtime_document(
        &llm.base_url(),
        GATEWAY_MODEL,
        Some("fr"),
    ))
    .await?;

    let host = vec![
        ("HERMES_GATEWAY_URL", Some(gateway.base_url())),
        ("HERMES_LLM_MODEL", Some(PINNED_MODEL.to_owned())),
        ("HERMES_USER_LANGUAGE", Some("es".to_owned())),
    ];
    let hermes = RuntimeRun::start_configured(
        "pinned",
        vec![PersonaFixture::assistant(&run)],
        &["assistant"],
        llm,
        host,
    )
    .await?;

    hermes.wait_for_persona("assistant").await?;
    hermes.wait_for_log("persona ready").await?;

    // Asserted on the environment the runtime really built, read back from
    // Docker: no message is needed to know which model a persona was handed.
    let environment = hermes.persona_environment("assistant").await?;
    assert!(
        environment.contains(&format!("TWALK_LLM_MODEL={PINNED_MODEL}")),
        "the model the operator pinned on the host wins over the one set from the browser \
         (ADR 0015): {environment:?}"
    );
    assert!(
        !environment
            .iter()
            .any(|variable| variable.contains(GATEWAY_MODEL)),
        "and the browser's model must not reach the persona at all: {environment:?}"
    );
    assert!(
        environment.contains(&"TWALK_USER_LANGUAGE=es".to_owned()),
        "the language sits in the same order as the model, not a different one: {environment:?}"
    );

    let logs = hermes.logs().await;
    assert!(
        logs.contains("model=operator") && logs.contains("language=operator"),
        "the runtime must attribute both fields to the host, so an operator can tell why what \
         they set in the Companion is not in force:\n{logs}"
    );

    hermes.shutdown().await
}

/// The third acceptance criterion. The Sensor's answer to this exact question,
/// copied rather than reinvented: start anyway, say so loudly at `ERROR`
/// naming the URL, and carry on. A runtime that refused to start because a
/// settings endpoint was slow would take a whole deployment down for a
/// preference.
#[tokio::test]
async fn a_gateway_that_does_not_answer_does_not_stop_the_runtime() -> Result<()> {
    let run = run_id("gwdown");
    let llm = StubLlm::start().await?;
    // A port this suite reserves and no stub ever binds: the runtime's read
    // fails as a transport failure rather than as a refusal, and no parallel
    // test can accidentally answer it.
    let unreachable = harness::UNREACHABLE_GATEWAY_URL.to_owned();

    let host = vec![
        ("HERMES_GATEWAY_URL", Some(unreachable.clone())),
        ("HERMES_LLM_MODEL", Some(PINNED_MODEL.to_owned())),
        ("HERMES_USER_LANGUAGE", None),
    ];
    let mut hermes = RuntimeRun::start_configured(
        "gwdown",
        vec![PersonaFixture::assistant(&run)],
        &["assistant"],
        llm,
        host,
    )
    .await?;

    // It started, and it hosts what the host's own configuration named.
    hermes.wait_for_persona("assistant").await?;
    hermes.wait_for_log("persona ready").await?;
    assert!(hermes.is_running(), "the runtime is still up");

    let logs = hermes.logs().await;
    assert!(
        logs.contains(&format!("{unreachable}/api/settings/runtime")),
        "the failure must name the URL it could not read, which is the one thing an operator \
         needs:\n{logs}"
    );
    assert!(
        logs.contains("fell_back_to") && logs.contains("this host's own configuration"),
        "and it must say what it fell back to:\n{logs}"
    );

    // The persona runs, on the operator's model, with no language to fall back
    // to — which is a supported state and not an outage (ADR 0016).
    let environment = hermes.persona_environment("assistant").await?;
    assert!(
        environment.contains(&format!("TWALK_LLM_MODEL={PINNED_MODEL}")),
        "{environment:?}"
    );
    assert!(
        !environment
            .iter()
            .any(|variable| variable.starts_with("TWALK_USER_LANGUAGE")),
        "a preference that could not be read is absent, not empty: {environment:?}"
    );

    hermes.shutdown().await
}

/// The other half of the third criterion, and the shape the whole platform
/// already uses for "allowed to exist, handed nothing" (ADR 0013's paused
/// persona): with no model from either voice there is nothing to host, so the
/// runtime runs, says that, and starts its personas the moment a model is
/// named — no restart, because **the retry exists to end an outage, never to
/// apply a preference**.
#[tokio::test]
async fn a_deployment_nobody_named_a_model_for_hosts_nothing_until_one_is_named() -> Result<()> {
    const REPLY: &str = "Je regarde et je te dis.";
    let run = run_id("nomodel");
    let llm = StubLlm::start_with_reply(REPLY).await?;
    let llm_base_url = llm.base_url();
    // The Gateway is up and its user has named no model: `llm: null`, which the
    // Gateway's own documentation insists is a different fact from a Gateway
    // that could not be reached.
    let gateway = StubGateway::start(StubGateway::no_model_document(Some("fr"))).await?;

    let mut host = no_operator_model();
    host.push(("HERMES_GATEWAY_URL", Some(gateway.base_url())));
    let persona = PersonaFixture::assistant(&run);
    let mut hermes =
        RuntimeRun::start_configured("nomodel", vec![persona], &["assistant"], llm, host).await?;

    hermes
        .wait_for_log("This runtime is up and hosts nothing")
        .await?;
    assert!(
        hermes.is_running(),
        "a deployment with no model named is not an outage of the runtime"
    );
    assert!(
        !hermes.persona_is_running("assistant").await?,
        "and there is nothing to start a persona with: a persona handed no model would die on \
         its first line, for ever (ADR 0015)"
    );

    // The user names a model in the Companion.
    gateway.set_document(StubGateway::runtime_document(
        &llm_base_url,
        GATEWAY_MODEL,
        Some("fr"),
    ));

    hermes
        .wait_for_log("there is a model to reason with now")
        .await?;
    hermes.wait_for_persona("assistant").await?;
    hermes.wait_for_log("persona ready").await?;

    // And it works: the persona reasons with the model that was named, without
    // the runtime having been restarted.
    let marker = format!("nomodel-{}", hermes.prefix);
    let trigger = ambiguous_message(&marker)?;
    let trigger_id = trigger["id"].as_str().expect("a string id").to_owned();
    hermes.publish_inbound(&trigger).await?;
    let thinking = hermes.wait_for(THINKING_TYPE, &trigger_id).await?;
    validate_against_contract(&thinking.payload, "persona.thinking.emitted")?;

    let requests = hermes.llm_requests_mentioning(&marker);
    assert_eq!(requests.len(), 1, "one message, one completion request");
    assert_eq!(
        requests[0].body["model"].as_str(),
        Some(GATEWAY_MODEL),
        "the model the user named after the runtime was already up: {:?}",
        requests[0].body
    );

    assert!(
        gateway.reads() >= 2,
        "the retry asked again until there was something to host; it answered {} reads",
        gateway.reads()
    );

    hermes.shutdown().await
}
