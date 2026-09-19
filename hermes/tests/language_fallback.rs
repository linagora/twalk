//! Issue #164: ADR 0016's fallback, at the persona's process boundary.
//!
//! > A persona writes its suggestion in the language of the message it
//! > answers, and falls back to **the user's own language** only when it
//! > cannot tell.
//!
//! The first real suggestion this product produced answered *"Test
//! received."* to a French-speaking contact who had written `test` — a word
//! identical in both languages, which is exactly the ambiguous case the ADR
//! legislates for. There was nothing to fall back to: the preference existed
//! in the ADR and in nobody's code.
//!
//! Two assertions matter here, and the second is the one that keeps the
//! ADR's actual decision from being quietly inverted:
//!
//! 1. with the preference set, the ambiguous case has an answer — the
//!    persona tells the model which language to write in when it cannot
//!    tell;
//! 2. an **unambiguous** message still follows the message. The preference
//!    governs the fallback and nothing else: "write in the user's language"
//!    is the intuitive reading, the one an i18n habit produces, and the one
//!    ADR 0016 exists to refuse.
//!
//! What a stub LLM can be asked is what it was sent, so that is what is
//! asserted: the instruction the persona actually put in front of the model,
//! and — in `assistant.rs`, where no preference is configured — the absence
//! of any instruction naming a language. A test cannot assert the language a
//! real model answers in without a real model; what it can assert is that
//! the model was told the right thing, in the right order, and nothing else.

mod harness;

use anyhow::Result;
use harness::{
    contract_fixture, sha256_hex, traceparent_for, validate_against_contract, PersonaRun,
    SUGGEST_TYPE,
};
use serde_json::{json, Value};

/// A granted inbound message whose body is the ambiguous case itself: `test`
/// is the same word in French and in English. The run's marker is what makes
/// the assertion about this message rather than another; the stub judges
/// neither.
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

#[tokio::test]
async fn the_users_language_is_the_fallback_and_never_the_rule() -> Result<()> {
    const REPLY: &str = "Bien reçu !";
    // The preference the Companion stores and the runtime injects. A French
    // user is the case that was got wrong live.
    let run = PersonaRun::start_with_user_language("language", REPLY, "fr").await?;

    let marker = format!("language-{}", run.prefix);
    let trigger = ambiguous_message(&marker)?;
    let trigger_id = trigger["id"].as_str().expect("a string id").to_owned();
    run.publish_inbound(&trigger).await?;

    let suggest = run.wait_for(SUGGEST_TYPE, &trigger_id).await?;
    validate_against_contract(&suggest.payload, "persona.suggest.produced")?;

    let requests = run.llm_requests_mentioning(&marker);
    assert_eq!(requests.len(), 1, "one message, one completion request");
    let prompt = requests[0].body["messages"][0]["content"]
        .as_str()
        .expect("the persona frames the request with a system prompt")
        .to_owned();

    // 1. The fallback exists, and it names the user's own language by its
    //    English name: prompts stay in English (ADR 0016), and a bare `fr` is
    //    not something a model reliably reads as a language.
    assert!(
        prompt.contains("write in French"),
        "an ambiguous message must have something to fall back to, named: {prompt}"
    );
    assert!(
        prompt.contains("the user's own language"),
        "and the prompt must say what that language is to the user, so the model \
         knows it is a fallback and not the subject: {prompt}"
    );

    // 2. The decision ADR 0016 actually makes, which the fallback must not
    //    invert: the message's own language comes first, and the fallback is
    //    conditional on not being able to tell.
    let by_the_message = prompt
        .find("same language as the message")
        .unwrap_or_else(|| panic!("the message's own language must still govern: {prompt}"));
    let fallback = prompt
        .find("write in French")
        .expect("the fallback was just asserted");
    assert!(
        by_the_message < fallback,
        "the instruction to follow the message must come first; a prompt that \
         named the user's language first would invert ADR 0016: {prompt}"
    );
    assert!(
        prompt.contains("too short or ambiguous to tell"),
        "the fallback must be conditional in the prompt, not an instruction to \
         always write French: {prompt}"
    );
    assert_eq!(
        prompt.matches("French").count(),
        1,
        "the user's language is named once, in the fallback, and nowhere else: \
         {prompt}"
    );

    // And the persona said, at startup, what it would do with an ambiguous
    // message — the same fact the operator can read without a suggestion in
    // front of them.
    let logs = run.logs().await?;
    assert!(
        logs.contains("TWALK_USER_LANGUAGE=fr"),
        "the persona must state the preference it is running with; its logs \
         were:\n{logs}"
    );
    assert!(
        !logs.contains("no user language is configured"),
        "and must not warn about a preference it was given: \n{logs}"
    );

    run.shutdown().await
}
