//! Ticket #21, the Python persona SDK and the `assistant` skeleton, at the
//! spec's seam 1: the real persona process, driven by publishing contract
//! fixtures to the bus, with the stub LLM answering a canned completion.
//!
//! The persona under test is the container image `assistant` ships as
//! (personas ship as images — ADR 0008, and the host has no pip), talking
//! to the bus itself and never to the test process. Every assertion is
//! made from outside it: what appeared on the bus, and what the model was
//! asked.
//!
//! The consent gate is the one that carries the project's promise. It is
//! asserted here by absence — no event and no completion request for a
//! `pending` or `revoked` message — and case by case in the SDK's own unit
//! tests (`sdk/python/tests/test_consent_gate.py`), because a gate that is
//! "impossible for an author to forget" has to be both.
//!
//! The trigger-type gate is asserted the same way, for the same reason: a
//! message the **user** sent (`outbound.message.sent`, ADR 0018) is on the
//! bus and must wake no persona — answering the operator, or in Signal's
//! Note to Self answering nobody at all — and the consent gate cannot stop
//! it, because that type carries no consent extension to read. Since ADR 0021
//! the same holds for the user's own reaction, `outbound.reaction.added`, and
//! it needed no change to the gate: an allowlist excludes a new type by
//! default, which is the property asserted alongside the absences.
//!
//! This suite configures **no** user language, which is the state ADR 0016's
//! fallback did not exist in at all (issue #164) and still the state of a
//! deployment whose user has not opened the settings screen. So the granted
//! case asserts the absence too: the prompt names no language, nothing is
//! invented, and the persona says at startup what will happen to a message
//! whose language it cannot tell. The preference's own case is
//! `language_fallback.rs`.
//!
//! Since ADR 0031 (issue #121) a suggestion also carries the **disclosure**:
//! the contract's sentence for the language the reply is written in, which
//! the Companion Gateway appends to the reply at approval so the contact
//! reads that it was drafted with an AI assistant. The persona selects the
//! sentence and never writes it, and the language is the model's answer to
//! one more question — so the granted case asserts the second completion
//! request and its shape, and a case of its own asserts what happens when
//! the model answers a language the contract has no sentence for: no
//! suggestion, one `ERROR` line naming the tag, and the delivery terminated
//! rather than retried, the shape `reasoning_budget.rs` set.

mod harness;

use anyhow::Result;
use harness::{
    contract_fixture, contract_variant_fixture, poll_until, sha256_hex, trace_id, traceparent_for,
    validate_against_contract, PersonaRun, HERMES_DOMAIN, INBOUND_TYPE, LANGUAGE_ASK_MARK,
    LLM_API_KEY, MODEL, OUTBOUND_TYPE, PERSONA_ID, SUGGEST_TYPE, THINKING_TYPE,
};
use serde_json::{json, Value};

/// The contract's French sentence (`contracts/disclosure/v1/sentences.json`),
/// which is what the stub's default language answer selects. Spelled out
/// rather than read from the file: the assertion is that *this* sentence
/// reached the bus, verbatim.
const FRENCH_DISCLOSURE: &str = "Rédigé avec mon assistant IA.";

/// The `inbound.message.received` fixture, re-keyed onto a trigger of this
/// run and labelled with a consent state. The body carries the run's marker
/// so that an assertion about what reached the model is about exactly this
/// message. Re-validated, so a patched fixture stays a contract citizen.
fn inbound_message(marker: &str, consent: &str, body: &str) -> Result<Value> {
    let mut event = contract_fixture("inbound.message.received")?;
    let id = sha256_hex(marker);
    event["id"] = json!(id);
    event["traceparent"] = json!(traceparent_for(&id));
    event["consent"] = json!(consent);
    event["data"]["body"] = json!(format!("{body} [{marker}]"));
    // The caption repeats the body in the fixture; leave it consistent.
    event["data"]["attachments"][0]["caption"] = event["data"]["body"].clone();
    validate_against_contract(&event, "inbound.message.received")?;
    Ok(event)
}

/// The contract's reduced shape for a revoked sender: same envelope, no
/// body, no excerpt, no attachment reference (ADR 0012, issue #58). The
/// gate must drop it before that shape could matter — and it carries no
/// text to mark, which is the point.
fn revoked_message(marker: &str) -> Result<Value> {
    let mut event = contract_variant_fixture("inbound.message.received", "revoked-sender")?;
    let id = sha256_hex(marker);
    event["id"] = json!(id);
    event["traceparent"] = json!(traceparent_for(&id));
    validate_against_contract(&event, "inbound.message.received")?;
    Ok(event)
}

fn event_id(event: &Value) -> String {
    event["id"]
        .as_str()
        .expect("a patched fixture has a string id")
        .to_owned()
}

fn attribute<'a>(event: &'a Value, name: &str) -> &'a str {
    event[name]
        .as_str()
        .unwrap_or_else(|| panic!("the event has no string {name}: {event}"))
}

#[tokio::test]
async fn a_granted_message_produces_thinking_then_a_schema_valid_suggestion() -> Result<()> {
    const REPLY: &str = "Pas de problème, à 20h !";
    let run = PersonaRun::start("granted", REPLY).await?;

    let marker = format!("granted-{}", run.prefix);
    let trigger = inbound_message(&marker, "granted", "On décale à 20h ?")?;
    let trigger_id = event_id(&trigger);
    run.publish_inbound(&trigger).await?;

    // 1. thinking: the persona started on this message.
    let thinking = run.wait_for(THINKING_TYPE, &trigger_id).await?;
    validate_against_contract(&thinking.payload, "persona.thinking.emitted")?;
    let event = &thinking.payload;
    assert_eq!(
        attribute(event, "id"),
        sha256_hex(&format!("{PERSONA_ID}:{trigger_id}")),
        "the thinking id is sha256(persona_id:trigger_event_id)"
    );
    assert_eq!(
        attribute(event, "source"),
        format!("hermes://{HERMES_DOMAIN}/personas/{PERSONA_ID}")
    );
    assert_eq!(
        attribute(event, "subject"),
        trigger_id,
        "the subject is the trigger's event id"
    );
    assert_eq!(
        event["data"]["trigger"],
        json!({ "event_id": trigger_id, "event_type": INBOUND_TYPE }),
        "oversight must be able to link the activity to the trigger without a bus lookup"
    );
    assert_eq!(attribute(event, "network"), "whatsapp");
    assert_eq!(attribute(event, "consent"), "granted");
    assert_eq!(
        event["data"]["model"],
        json!(MODEL),
        "the thinking event names the model that will reason"
    );
    assert_eq!(
        trace_id(attribute(event, "traceparent")),
        trace_id(attribute(&trigger, "traceparent")),
        "the persona continues the trigger's trace"
    );
    assert_eq!(
        thinking.header("Nats-Msg-Id"),
        Some(attribute(event, "id")),
        "Nats-Msg-Id is the deterministic id, so a replay collapses on the bus"
    );
    for extension in ["network", "consent", "traceparent"] {
        assert_eq!(
            thinking.header(extension),
            Some(attribute(event, extension)),
            "{extension} is duplicated as a NATS header for server-side filtering"
        );
    }

    // 2. suggest: the reply the user may approve.
    let suggest = run.wait_for(SUGGEST_TYPE, &trigger_id).await?;
    validate_against_contract(&suggest.payload, "persona.suggest.produced")?;
    let event = &suggest.payload;
    assert_eq!(
        attribute(event, "id"),
        sha256_hex(&format!("{PERSONA_ID}:{trigger_id}:1")),
        "the suggest id is sha256(persona_id:trigger_event_id:attempt)"
    );
    assert_eq!(attribute(event, "subject"), trigger_id);
    assert_eq!(
        event["data"]["suggestion"],
        json!({ "body": REPLY, "format": "text/plain" }),
        "the suggestion carries the completion, as text"
    );
    assert_eq!(event["data"]["attempt"], json!(1));
    assert_eq!(
        event["data"]["trigger"],
        json!({ "event_id": trigger_id, "event_type": INBOUND_TYPE })
    );
    assert_eq!(attribute(event, "network"), "whatsapp");
    assert_eq!(attribute(event, "consent"), "granted");
    assert_eq!(
        trace_id(attribute(event, "traceparent")),
        trace_id(attribute(&trigger, "traceparent"))
    );
    assert_eq!(suggest.header("Nats-Msg-Id"), Some(attribute(event, "id")));
    assert!(
        event["data"]["expires_at"].is_string(),
        "a suggestion ages out rather than staying approvable for ever; what \
         the window is, and where it is measured from, is `suggestion.rs` (#22)"
    );
    // The disclosure (ADR 0031, #121): the contract's own sentence for the
    // language the reply is written in, selected by the persona and never
    // composed by it, carried as a member of its own so the Gateway can
    // append it at approval without reading the body. The stub answered
    // `fr` to the language ask below, so this is the French sentence and
    // not a translation of anything.
    assert_eq!(
        event["data"]["disclosure"],
        json!(FRENCH_DISCLOSURE),
        "the suggestion carries the contract's sentence for its language: {event}"
    );

    assert!(
        thinking.sequence < suggest.sequence,
        "thinking must reach the bus before the suggestion it precedes \
         (thinking at {}, suggest at {})",
        thinking.sequence,
        suggest.sequence
    );

    // 3. what the persona asked the model: two requests per message since
    //    ADR 0031 — the reply, then the language ask about the reply. Only
    //    the first mentions the message, because the ask is about what the
    //    persona wrote and not about what the contact did.
    let requests = run.llm_requests_mentioning(&marker);
    assert_eq!(
        requests.len(),
        1,
        "one message, one completion request about it; the language ask is \
         about the reply and is asserted below"
    );
    let request = &requests[0];
    assert_eq!(
        request.last_message_content(),
        Some(
            trigger["data"]["body"]
                .as_str()
                .expect("the fixture has a body")
        ),
        "the message the user received is what the persona asked about"
    );
    assert_eq!(
        request.body["messages"][0]["role"],
        json!("system"),
        "the persona frames the request with a system prompt"
    );
    // What this run configured no preference for, and what the prompt
    // therefore must not say (ADR 0016, issue #164). A persona with no user
    // language answers in the language of the message and invents no
    // fallback: naming a language here — "probably English" — is the failure
    // the ticket is about, arriving from the other direction.
    let prompt = request.body["messages"][0]["content"]
        .as_str()
        .expect("the system prompt is text");
    assert!(
        prompt.contains("same language as the message"),
        "the message's own language governs the reply: {prompt}"
    );
    for language in ["English", "French", "Italian", "Spanish", "German"] {
        assert!(
            !prompt.contains(language),
            "no language is named in the prompt of a deployment whose user set no \
             preference, {language} included: {prompt}"
        );
    }
    // And the gap is stated rather than passed over: the operator reads it in
    // the log, with the variable and where it is set, instead of discovering
    // it in a suggestion sent to a contact.
    let logs = run.logs().await?;
    assert!(
        logs.contains("no user language is configured"),
        "a persona with no fallback must say so at startup; its logs were:\n{logs}"
    );
    assert!(
        logs.contains("HERMES_USER_LANGUAGE"),
        "and must name where it is set: \n{logs}"
    );
    assert_eq!(request.body["model"], json!(MODEL));
    assert_eq!(
        request.authorization.as_deref(),
        Some(format!("Bearer {LLM_API_KEY}").as_str()),
        "the credentials the operator configured reach the endpoint they chose"
    );
    // The operator's provider parameters, in the request the endpoint
    // actually received (ADR 0015): one added, and one the client would
    // otherwise have sent removed — which is what made the first endpoint
    // tried in practice work at all.
    assert_eq!(
        request.body["top_p"],
        json!(0.9),
        "a provider parameter reaches the endpoint untouched"
    );
    assert!(
        request.body.get("temperature").is_none(),
        "a parameter set to null removes a field the provider rejects, got {}",
        request.body
    );

    // 4. the language ask (ADR 0031): the second and last request, shaped
    //    for one token, about the reply and nothing else — it carries no
    //    word the contact wrote, which is what makes it cheap to send to a
    //    model at all.
    let all = run.llm.requests();
    assert_eq!(
        all.len(),
        2,
        "one message, two completion requests: the reply and the language ask, \
         got {}",
        all.len()
    );
    let ask = &all[1];
    assert!(
        ask.is_language_ask(),
        "the second request is the language ask and the first is the reply: {}",
        ask.body
    );
    assert!(
        !all[0].is_language_ask(),
        "the reply is drafted before its language is asked: {}",
        all[0].body
    );
    assert_eq!(
        ask.last_message_content(),
        Some(REPLY),
        "the persona asks which language the reply it drafted is in, and sends \
         the reply alone: {}",
        ask.body
    );
    assert!(
        !ask.body.to_string().contains(&marker),
        "the language ask carries nothing the contact wrote: {}",
        ask.body
    );
    assert_eq!(
        ask.body["max_tokens"],
        json!(5),
        "the ask is shaped for one token, because that is all it needs back: {}",
        ask.body
    );
    let ask_prompt = ask.body["messages"][0]["content"]
        .as_str()
        .expect("the ask has a system prompt");
    assert!(
        ask_prompt.contains(LANGUAGE_ASK_MARK),
        "the ask is the SDK's own wording, which the stub recognises by it: {ask_prompt}"
    );
    assert!(
        ask_prompt.contains("en, fr, it, es, de") && ask_prompt.contains("other"),
        "the ask names the five languages the contract has a sentence for, and the \
         one answer that is none of them: {ask_prompt}"
    );
    assert_eq!(
        ask.body["model"],
        json!(MODEL),
        "the same model answers the ask: no second endpoint, no detection library"
    );
    // And the persona says which language it selected the sentence for, and
    // who said so — the model, here — naming the trigger and not the contact.
    let logs = run.logs().await?;
    let disclosed = logs
        .lines()
        .find(|line| line.contains("disclosure language=") && line.contains(&trigger_id))
        .unwrap_or_else(|| panic!("no disclosure line names this trigger; the logs were:\n{logs}"));
    assert!(
        disclosed.contains("language=fr") && disclosed.contains("declared_by=model"),
        "the log says which language and whose answer it was: {disclosed}"
    );

    run.shutdown().await
}

/// Waits for a line in the persona's logs, so a failure carries the logs
/// rather than a timeout.
async fn wait_for_log(run: &PersonaRun, needle: &str) -> Result<String> {
    let found = poll_until(
        || async {
            let logs = run.logs().await.ok()?;
            logs.contains(needle).then_some(logs)
        },
        &format!("the persona to log {needle:?}"),
    )
    .await;
    match found {
        Ok(logs) => Ok(logs),
        Err(error) => anyhow::bail!(
            "{error}; the persona's logs were:\n{}",
            run.logs().await.unwrap_or_default()
        ),
    }
}

/// ADR 0031's refusal, at the process boundary: the model answers a
/// language the contract has no sentence for, and there is no suggestion.
///
/// Not a fallback to English, and not to the user's language: a disclosure
/// the contact cannot read is one nobody reads. And not a retry either — the
/// model would answer the same language to the same reply, and each ask is
/// billed — so the shape is `reasoning_budget.rs`'s: asked once, one `ERROR`
/// line naming the tag and the remedy, the delivery terminated, and the bus
/// asked (not the persona) that nothing is left in flight.
#[tokio::test]
async fn a_reply_in_a_language_with_no_sentence_is_refused_once_and_never_suggested() -> Result<()>
{
    // The reply is the stub's and its language is the stub's word for it:
    // what the persona does with the answer is the whole test.
    const REPLY: &str = "了解です、20時で大丈夫です。";
    let run = PersonaRun::start("no-sentence", REPLY).await?;
    run.llm.set_language_answer("ja");

    let marker = format!("no-sentence-{}", run.prefix);
    let trigger = inbound_message(&marker, "granted", "On décale à 20h ?")?;
    let trigger_id = event_id(&trigger);
    run.publish_inbound(&trigger).await?;

    // The persona did start on it: `thinking` is published before the model
    // is asked anything, so oversight sees the activity.
    let thinking = run.wait_for(THINKING_TYPE, &trigger_id).await?;
    validate_against_contract(&thinking.payload, "persona.thinking.emitted")?;

    // The refusal, by name: the tag the model answered, and where the
    // sentences are — a contribution request an operator can act on.
    let logs = wait_for_log(&run, "and no retry").await?;
    let refusal = logs
        .lines()
        .find(|line| line.contains("and no retry") && line.contains(&trigger_id))
        .unwrap_or_else(|| panic!("no refusal names this trigger; the logs were:\n{logs}"))
        .to_owned();
    assert!(
        refusal.contains("ERROR"),
        "a suggestion that will never exist is an error, not a note: {refusal}"
    );
    assert!(
        refusal.contains("disclosure has no sentence for the language the model answered: ja"),
        "the refusal names the language the model answered, as it answered it: {refusal}"
    );
    assert!(
        refusal.contains("contracts/disclosure/v1/sentences.json"),
        "and where a sentence for it would go: {refusal}"
    );
    assert!(
        !refusal.contains(&marker) && !refusal.contains(REPLY),
        "the line names the trigger and nothing the contact or the persona wrote: {refusal}"
    );

    // Asked once for the reply and once for its language — and then never
    // again. The persona's own retry delay is five seconds, so this waits
    // past it before counting.
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;
    assert_eq!(
        run.llm_requests_mentioning(&marker).len(),
        1,
        "the reply was drafted once; a retry cannot change which language it is in"
    );
    let all = run.llm.requests();
    assert_eq!(
        all.len(),
        2,
        "the reply and its language ask, once each, got {}",
        all.len()
    );
    assert!(
        all[1].is_language_ask() && all[1].last_message_content() == Some(REPLY),
        "the second request asked the language of the reply: {}",
        all[1].body
    );

    // No suggestion. The absence is the promise: a reply the contact cannot
    // be told was drafted with an AI assistant is no suggestion at all.
    let suggestions: Vec<u64> = run
        .published(SUGGEST_TYPE)
        .await?
        .into_iter()
        .map(|message| message.sequence)
        .collect();
    assert!(
        suggestions.is_empty(),
        "a reply with no disclosure must never be offered to the user, found {suggestions:?}"
    );

    // And the trigger is not left in flight. The bus's own account: nothing
    // pending, nothing awaiting an ack, nothing redelivered.
    let state = run.consumer_state().await?;
    assert_eq!(
        (state.pending, state.awaiting_ack, state.redelivered),
        (0, 0, 0),
        "the delivery must be terminated rather than left apparently unprocessed \
         or handed over again: {state:?}"
    );

    // The refusal is about *this* reply and not about the persona: the next
    // message, whose reply the model calls French, is suggested with the
    // sentence — so an operator reading one refusal knows the persona is
    // still running and what to contribute.
    run.llm.set_reply("Pas de souci, à 20h !");
    run.llm.set_language_answer("fr");
    let next = inbound_message(&format!("next-{}", run.prefix), "granted", "Et 20h ?")?;
    let next_id = event_id(&next);
    run.publish_inbound(&next).await?;
    let suggest = run.wait_for(SUGGEST_TYPE, &next_id).await?;
    validate_against_contract(&suggest.payload, "persona.suggest.produced")?;
    assert_eq!(
        suggest.payload["data"]["disclosure"],
        json!(FRENCH_DISCLOSURE),
        "the persona goes on suggesting, with the sentence, once the language has one"
    );

    run.shutdown().await
}

/// The `outbound.message.sent` fixture — the user's own message — re-keyed
/// onto this run. Re-validated, so what is published stays a contract
/// citizen: this is an event the Sensor really produces, not a fabrication.
fn own_message(marker: &str, body: &str) -> Result<Value> {
    let mut event = contract_fixture("outbound.message.sent")?;
    let id = sha256_hex(marker);
    event["id"] = json!(id);
    event["traceparent"] = json!(traceparent_for(&id));
    event["data"]["body"] = json!(format!("{body} [{marker}]"));
    validate_against_contract(&event, "outbound.message.sent")?;
    Ok(event)
}

#[tokio::test]
async fn the_users_own_traffic_never_triggers_a_persona() -> Result<()> {
    let run = PersonaRun::start("own-message", "this reply must never be drafted").await?;

    // 1. On its own subject, which is where the Sensor publishes it. The
    //    persona's consumer is filtered to the inbound subject, so this
    //    never reaches it — the first of the two things that have to hold.
    let on_its_subject = own_message(
        &format!("own-subject-{}", run.prefix),
        "je confirme pour 20h",
    )?;
    run.publish_typed(&on_its_subject).await?;

    // 2. On the **inbound** subject, which is a deliberate misroute: it is
    //    what a widened consumer filter, a mis-declared stream or a
    //    third-party producer would produce, and it is the only way to put
    //    the SDK's own gate under the container's process boundary. The
    //    event is still the contract's shape.
    let misrouted = own_message(
        &format!("misrouted-{}", run.prefix),
        "et j'apporte le dessert",
    )?;
    run.publish_inbound(&misrouted).await?;

    // 3. The same misroute wearing a `granted` consent extension. This one
    //    is deliberately NOT contract-valid — the type forbids the
    //    extension — and it is here to prove which gate does the work: the
    //    consent gate would wave it through, and the trigger-type gate
    //    refuses it on the type, which is the only attribute that says
    //    "this is the operator writing".
    let mut consent_wearing = own_message(&format!("wearing-{}", run.prefix), "et le café")?;
    consent_wearing["consent"] = json!("granted");
    assert!(
        validate_against_contract(&consent_wearing, "outbound.message.sent").is_err(),
        "the contract itself refuses a consent extension on this type (ADR 0018)"
    );
    run.publish_inbound(&consent_wearing).await?;

    // 4. The user's own **reaction** — the `outbound.*` family's second
    //    member (ADR 0021) — misrouted the same way. It cost the gate no
    //    edit to refuse: the allowlist admits `inbound.message.received` and
    //    nothing else, so a type the contract adds later is excluded before
    //    anybody remembers it exists. That is the property being asserted
    //    here, and it is the one a rule excluding `outbound.*` would lose.
    let own_reaction_marker = format!("own-reaction-{}", run.prefix);
    let mut own_reaction = contract_fixture("outbound.reaction.added")?;
    let own_reaction_id = sha256_hex(&own_reaction_marker);
    own_reaction["id"] = json!(own_reaction_id);
    own_reaction["traceparent"] = json!(traceparent_for(&own_reaction_id));
    own_reaction["data"]["target"]["excerpt"] = json!(own_reaction_marker.clone());
    validate_against_contract(&own_reaction, "outbound.reaction.added")?;
    run.publish_inbound(&own_reaction).await?;

    // The fence: a granted inbound message, published last. Its suggestion
    // proves the persona has been through the three above, which is what
    // turns an absence into an assertion instead of a wait.
    let fence_marker = format!("fence-{}", run.prefix);
    let fence = inbound_message(&fence_marker, "granted", "On décale à 20h ?")?;
    let fence_id = event_id(&fence);
    run.publish_inbound(&fence).await?;
    run.wait_for(SUGGEST_TYPE, &fence_id).await?;

    for (label, event) in [
        ("on its own subject", &on_its_subject),
        ("misrouted onto the inbound subject", &misrouted),
        ("misrouted wearing a granted consent", &consent_wearing),
        ("the user's own reaction, misrouted", &own_reaction),
    ] {
        let trigger_id = event_id(event);
        for event_type in [THINKING_TYPE, SUGGEST_TYPE] {
            let about: Vec<String> = run
                .published(event_type)
                .await?
                .into_iter()
                .filter(|message| message.payload["subject"].as_str() == Some(&trigger_id))
                .map(|message| {
                    message.payload["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect();
            assert!(
                about.is_empty(),
                "the user's own traffic ({label}) must produce no {event_type}, got {about:?}"
            );
        }
    }

    // And the model was never asked about any of them: the gate runs before
    // the handler and before the LLM client is touched, so no word the user
    // wrote was ever sent anywhere. Two requests, both the fence's: its
    // reply, and the language ask about that reply (ADR 0031).
    let requests = run.llm.requests();
    assert_eq!(
        requests.len(),
        2,
        "five events, one of them a persona's business: the model must have been asked \
         about exactly one — its reply and its language — got {} requests",
        requests.len()
    );
    assert!(
        requests[0].body.to_string().contains(&fence_marker),
        "the one reply request must be the inbound message's"
    );
    assert!(
        requests[1].is_language_ask(),
        "and the other is the language ask about its reply: {}",
        requests[1].body
    );
    for request in &requests {
        for marker in ["own-subject-", "misrouted-", "wearing-", "own-reaction-"] {
            assert!(
                !request.body.to_string().contains(marker),
                "nothing the user wrote themselves reached the model: {marker}"
            );
        }
    }

    // Nothing the persona published in this run is about anything but the
    // fence, and its type is still the inbound one.
    for event_type in [THINKING_TYPE, SUGGEST_TYPE] {
        let published = run.published(event_type).await?;
        assert_eq!(
            published.len(),
            1,
            "only the inbound message may produce a {event_type}"
        );
        assert_eq!(published[0].payload["subject"].as_str(), Some(&*fence_id));
    }
    let thinking = run.published(THINKING_TYPE).await?;
    assert_eq!(
        thinking[0].payload["data"]["trigger"]["event_type"],
        json!(INBOUND_TYPE),
        "and the one thing that woke the persona was an inbound message"
    );

    // The user's own event is still on the bus, unconsumed and intact: not
    // triggering a persona is not the same as not being published — a
    // persona that cannot see the user already replied would suggest answers
    // to closed conversations (ADR 0018).
    let own = run.published(OUTBOUND_TYPE).await?;
    assert_eq!(own.len(), 1, "the user's own message stays on the bus");
    validate_against_contract(&own[0].payload, "outbound.message.sent")?;

    run.shutdown().await
}

#[tokio::test]
async fn a_pending_or_revoked_message_produces_no_event_and_no_llm_call() -> Result<()> {
    let run = PersonaRun::start("gate", "this reply must never be drafted").await?;

    let pending = inbound_message(
        &format!("pending-{}", run.prefix),
        "pending",
        "Tu peux répondre ?",
    )?;
    let revoked = revoked_message(&format!("revoked-{}", run.prefix))?;
    // Published first, so that the fence below cannot be processed before
    // them: the persona consumes its subject in stream order.
    run.publish_inbound(&pending).await?;
    run.publish_inbound(&revoked).await?;

    // The fence: a granted message, published last. Its suggestion is the
    // proof that the persona has been through the two above — which is how
    // an absence becomes assertable instead of a wait of arbitrary length.
    let fence_marker = format!("fence-{}", run.prefix);
    let fence = inbound_message(&fence_marker, "granted", "On décale à 20h ?")?;
    let fence_id = event_id(&fence);
    run.publish_inbound(&fence).await?;
    run.wait_for(SUGGEST_TYPE, &fence_id).await?;

    for (label, event) in [("pending", &pending), ("revoked", &revoked)] {
        let trigger_id = event_id(event);
        for event_type in [THINKING_TYPE, SUGGEST_TYPE] {
            let about: Vec<String> = run
                .published(event_type)
                .await?
                .into_iter()
                .filter(|message| message.payload["subject"].as_str() == Some(&trigger_id))
                .map(|message| {
                    message.payload["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect();
            assert!(
                about.is_empty(),
                "a {label} message must produce no {event_type}, got {about:?}"
            );
        }
    }

    // And the model was never asked: the gate runs before any persona code,
    // so nothing about those two messages was ever sent anywhere. Two
    // requests, both the fence's: its reply and the language ask about it.
    let requests = run.llm.requests();
    assert_eq!(
        requests.len(),
        2,
        "three messages, one granted: the model must have been asked about exactly \
         one — its reply and its language — got {} requests",
        requests.len()
    );
    assert!(
        requests[0].body.to_string().contains(&fence_marker),
        "the one reply request must be the granted message's"
    );
    assert!(
        requests[1].is_language_ask(),
        "and the other is the language ask about its reply: {}",
        requests[1].body
    );
    for request in &requests {
        for marker in ["pending-", "revoked-"] {
            assert!(
                !request.body.to_string().contains(marker),
                "nothing a {marker} message carried reached the model: {}",
                request.body
            );
        }
    }

    // Every event the persona did publish in this run is about the fence.
    for event_type in [THINKING_TYPE, SUGGEST_TYPE] {
        let published = run.published(event_type).await?;
        assert_eq!(
            published.len(),
            1,
            "only the granted message may produce a {event_type}"
        );
        assert_eq!(published[0].payload["subject"].as_str(), Some(&*fence_id));
        assert_eq!(published[0].payload["consent"].as_str(), Some("granted"));
    }

    run.shutdown().await
}

#[tokio::test]
async fn a_message_with_no_text_is_skipped_without_a_suggestion() -> Result<()> {
    let run = PersonaRun::start("no-text", "this reply must never be drafted").await?;

    // A granted message whose body is empty: v0.1 answers text, and a
    // persona that guessed at an attachment would be worse than one that
    // says nothing. It still reports that it started — the user sees the
    // activity — and produces no suggestion.
    let mut silent = inbound_message(&format!("no-text-{}", run.prefix), "granted", "")?;
    silent["data"]["body"] = json!("");
    silent["data"]["attachments"][0]["caption"] = json!("");
    validate_against_contract(&silent, "inbound.message.received")?;
    let silent_id = event_id(&silent);
    run.publish_inbound(&silent).await?;

    let fence_marker = format!("fence-{}", run.prefix);
    let fence = inbound_message(&fence_marker, "granted", "On décale à 20h ?")?;
    let fence_id = event_id(&fence);
    run.publish_inbound(&fence).await?;
    run.wait_for(SUGGEST_TYPE, &fence_id).await?;

    run.wait_for(THINKING_TYPE, &silent_id).await?;
    let suggestions: Vec<u64> = run
        .published(SUGGEST_TYPE)
        .await?
        .into_iter()
        .filter(|message| message.payload["subject"].as_str() == Some(&silent_id))
        .map(|message| message.sequence)
        .collect();
    assert!(
        suggestions.is_empty(),
        "a message with no text must produce no suggestion, found {suggestions:?}"
    );
    assert_eq!(
        run.llm.request_count(),
        2,
        "the model is not asked to reply to nothing, nor which language nothing is \
         in: the fence's reply and its language ask are the only requests"
    );

    run.shutdown().await
}
