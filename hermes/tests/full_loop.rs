//! Ticket #25, the full loop: a contact writes, a persona proposes a reply,
//! the user approves it, and the reply reaches the contact — with nothing
//! sent that the user did not approve.
//!
//! This is the first test that asserts the promise the product makes, and it
//! is the only place in this repository where every part of it is real at
//! once: a homeserver, the Sensor that observes the room and posts, the bus,
//! the Hermes runtime, the `assistant` persona in the image it ships as, the
//! Companion Gateway that serves the approval (ADR 0022), and a contact who
//! is an actual Matrix account reading the room with their own eyes. The one
//! thing stood in for is the model, because a real one answers differently
//! every run.
//!
//! The seam is Hermes's. `companion-gateway/tests/deployment.rs` already
//! closes the second half of this loop — an approval, the bus, the real
//! Sensor, a real room — with the *suggestion* published by the test,
//! because Hermes does not run in that compose stack. What only this test can
//! show is the first half joined to it: that the suggestion the user approves
//! is one a persona really produced, from a message the Sensor really
//! observed.
//!
//! # The absences are the point
//!
//! Three of the four things asserted here are things that must **not**
//! happen, and they are what makes the loop a promise rather than a
//! demonstration:
//!
//! - a message whose consent is not `granted` produces no suggestion and no
//!   completion request — the model is never asked about a contact the user
//!   did not decide about;
//! - a suggestion nobody approved never reaches the room, however long the
//!   loop runs around it;
//! - an approval refused because consent was revoked in the meantime sends
//!   nothing — the check is a read of the state *at that moment*
//!   (`CONTEXT.md`, ADR 0022), not of the label the suggestion was born
//!   with.
//!
//! Each absence is anchored on something that did happen, so it is an
//! assertion and not a wait: the approved reply arriving in the room is the
//! fence that proves the deployment got that far.
//!
//! # Known gaps this test pins rather than fixes
//!
//! - **The approved reply is not a native Matrix reply.** The contract's
//!   inbound event carries no Matrix event ID to thread under, so the
//!   Gateway sends no `target.reply_to_event_id` and the message lands in
//!   the room unthreaded (ADR 0022). Asserted as it is, not as it should be.
//! - **Per-network scoping is not enforced** (#155): a persona activated on
//!   one network receives every network's events. This deployment's traffic
//!   is `matrix` and the persona is activated on `matrix`, so the gap does
//!   not carry the test — it is named here so that a reader does not take
//!   the passing test for enforcement.
//! - **Messages that arrive while a persona is paused are not replayed** on
//!   reactivation, which is why the user's activation decision is taken and
//!   observed on the bus *before* the runtime starts.
//! - **The runtime starts the persona through the host's Docker socket**
//!   (#158, ADR 0023), which is what `docker compose up` now gives an
//!   operator and what it costs them. This test runs the deployment's own
//!   `hermes` service and asserts it, because the loop closing with a
//!   runtime somebody started by hand is a different claim from the one the
//!   product makes.
//!
//! The stack, its ports and its teardown are documented in
//! `hermes/tests/harness/deployment.rs`.

mod harness;

use anyhow::Result;
use harness::{
    owner, validate_against_contract, Contact, Deployment, APPROVED_TYPE, DEPLOY_SERVER_NAME,
    MODEL, PERSONA_ID, SUGGEST_TYPE, THINKING_TYPE,
};
use serde_json::{json, Value};

/// The persona's drafts, one per message it is given. Scripted rather than
/// canned once, because the loop's central absence — *a suggestion nobody
/// approved never reaches the room* — can only be asserted if the two
/// suggestions are told apart by their text.
const UNAPPROVED_DRAFT: &str = "Oui, ça me va pour 20h — mais personne n'a approuvé cette phrase.";
const APPROVED_DRAFT: &str = "Parfait, à demain 14h !";

/// The trace id half of a traceparent: what has to be identical across a
/// message's sensor, persona and approval events for the trace to link them.
fn trace_id(traceparent: &str) -> &str {
    traceparent
        .split('-')
        .nth(1)
        .expect("a traceparent has four dash-separated parts")
}

fn attribute<'a>(event: &'a Value, name: &str) -> &'a str {
    event[name]
        .as_str()
        .unwrap_or_else(|| panic!("the event has no string {name}: {event}"))
}

#[tokio::test]
async fn a_contacts_message_becomes_an_approved_reply_and_nothing_else_reaches_the_room(
) -> Result<()> {
    let mut stack = Deployment::start(UNAPPROVED_DRAFT).await?;

    // The two correspondents. They are the same kind of account and the same
    // kind of participant; the only difference between them is a decision
    // the user took about one and not the other, which is the whole subject
    // of the first absence.
    let decided = stack.add_contact("granted").await?;
    let undecided = stack.add_contact("undecided").await?;
    stack
        .decide("contact", &decided.user_id, "granted", &["matrix"])
        .await?;

    // The user activates the persona. This is a consent decision on the
    // persona and nothing else (ADR 0013), taken through the same write API
    // as the one above — and it has to be *on the bus* before the runtime
    // starts, because a paused persona's messages are not replayed to it
    // when it is activated later.
    stack
        .decide("persona", PERSONA_ID, "granted", &["matrix"])
        .await?;
    stack.wait_for_decision(PERSONA_ID, "granted").await?;
    stack.wait_for_decision(&decided.user_id, "granted").await?;

    stack.start_hermes().await?;

    // The runtime is the deployment's, not this test's (#158). Asked of
    // Docker rather than of the harness, and asserted before anything else
    // happens: every absence below is only worth something if the thing that
    // produced the presences is the thing an operator gets from
    // `docker compose up`.
    assert_eq!(
        stack.hermes_container_state().await?,
        "running",
        "the loop must close on the deployment's own Hermes, with no process started \
         beside the stack"
    );

    // And the deployment's Hermes reads the deployment's Gateway (#184).
    // `compose.yaml` derives HERMES_GATEWAY_URL and
    // HERMES_GATEWAY_SERVICE_TOKEN from the one GATEWAY_SERVICE_TOKEN this
    // run sets, exactly as it derives the Sensor's snapshot pair, and the
    // wiring is only real if the runtime says it read something. What the
    // Gateway holds here is `llm: null` — nobody has named a model in this
    // deployment's Companion — so the model in force is the one this run put
    // in the environment file, which is what `sources=…=operator` says. The
    // test that the *Gateway's* value reaches a persona is
    // `tests/runtime_settings.rs`, at the runtime's own process boundary.
    let hermes_logs = stack.hermes_logs().await;
    assert!(
        hermes_logs.contains("read the Companion Gateway's runtime settings"),
        "the deployment's runtime must read the deployment's Gateway, or a preference set in \
         the Companion reaches nothing; its logs were:\n{hermes_logs}"
    );
    assert!(
        hermes_logs.contains("model=operator") && hermes_logs.contains("language=operator"),
        "and it must attribute each value, because an operator who pinned one in .env is the \
         only person who can tell why the browser's is not in force:\n{hermes_logs}"
    );

    // The Sensor labels a message with the consent state it holds, which it
    // learns from the Gateway's snapshot at startup and from the bus after
    // that. A decision taken a moment ago may not have reached it yet, so
    // the test converges deliberately instead of racing: it writes until a
    // message of this contact's comes back labelled `granted`. Anything else
    // would make the absences below pass for the wrong reason.
    converge_consent(&stack, &decided).await?;

    // --- 1. A contact the user did not decide about -----------------------
    //
    // Published first, so that the granted message below is a fence: the
    // persona consumes its subject in stream order, so a suggestion for the
    // later message proves it has been through this one.
    let unconsented = format!("je passe te voir ? [{}]", marker("undecided"));
    stack.says(&undecided, &unconsented).await?;
    let unconsented_event = stack.wait_for_inbound(&unconsented).await?;
    assert_eq!(
        unconsented_event.payload["consent"].as_str(),
        Some("pending"),
        "a contact nobody decided about is pending, which is what the gate must refuse: {}",
        unconsented_event.payload
    );

    // --- 2. The message the loop is about ---------------------------------
    let asked = format!("On peut décaler à demain 14h ? [{}]", marker("first"));
    stack.says(&decided, &asked).await?;
    let trigger = stack.wait_for_inbound(&asked).await?;
    let trigger_id = attribute(&trigger.payload, "id").to_owned();
    assert_eq!(
        trigger.payload["consent"].as_str(),
        Some("granted"),
        "the message the loop is about is one the user granted: {}",
        trigger.payload
    );
    assert_eq!(trigger.payload["network"].as_str(), Some("matrix"));

    // The persona says it started, on the real message, continuing the real
    // Sensor's trace.
    let thinking = stack
        .wait_for_persona_event(THINKING_TYPE, &trigger_id)
        .await?;
    validate_against_contract(&thinking.payload, "persona.thinking.emitted")?;
    assert_eq!(
        attribute(&thinking.payload, "source"),
        format!("hermes://{DEPLOY_SERVER_NAME}/personas/{PERSONA_ID}"),
        "the thinking event names the persona that is reasoning"
    );
    assert_eq!(thinking.payload["data"]["model"], json!(MODEL));
    assert_eq!(
        trace_id(attribute(&thinking.payload, "traceparent")),
        trace_id(attribute(&trigger.payload, "traceparent")),
        "the persona continues the trace the Sensor originated"
    );

    // And then the draft the user may approve — which is still only a draft.
    let suggestion = stack
        .wait_for_persona_event(SUGGEST_TYPE, &trigger_id)
        .await?;
    validate_against_contract(&suggestion.payload, "persona.suggest.produced")?;
    let unapproved_id = attribute(&suggestion.payload, "id").to_owned();
    assert_eq!(
        suggestion.payload["data"]["suggestion"]["body"].as_str(),
        Some(UNAPPROVED_DRAFT),
        "the suggestion carries what the model drafted: {}",
        suggestion.payload
    );
    assert_eq!(suggestion.payload["network"], json!("matrix"));
    assert_eq!(suggestion.payload["consent"], json!("granted"));
    assert!(
        suggestion.payload["data"]["expires_at"].is_string(),
        "a suggestion ages out rather than staying approvable for ever (#22): {}",
        suggestion.payload
    );

    // The first absence, now that the fence has passed: the message of the
    // contact nobody decided about produced no activity at all, and the
    // model was never asked about it. The gate runs before the handler and
    // before the LLM client is touched, so no word that contact wrote left
    // this deployment.
    let unconsented_id = attribute(&unconsented_event.payload, "id").to_owned();
    for event_type in [THINKING_TYPE, SUGGEST_TYPE] {
        let about = stack
            .persona_events_about(event_type, &unconsented_id)
            .await?;
        assert!(
            about.is_empty(),
            "a message whose consent is not granted must produce no {event_type}, got {about:?}"
        );
    }
    assert!(
        stack
            .llm_requests_mentioning(&marker("undecided"))
            .is_empty(),
        "the model must never be asked about a contact the user did not decide about; it was \
         sent {} request(s) mentioning them",
        stack.llm_requests_mentioning(&marker("undecided")).len()
    );
    let asked_about = stack.llm_requests_mentioning(&marker("first"));
    assert_eq!(
        asked_about.len(),
        1,
        "one granted message, one completion request (v0.1 is a single completion)"
    );
    assert_eq!(
        asked_about[0].last_message_content(),
        Some(asked.as_str()),
        "the message the contact wrote is what the persona asked the model about"
    );
    // And ADR 0016's fallback crossed the whole deployment to get there
    // (ticket #164): `HERMES_USER_LANGUAGE` in this stack's own `.env`, read
    // by the runtime, injected into the persona's container, in the prompt the
    // model was sent. Asserted here because every other test of it configures
    // the runtime directly, and the compose file is the one link they skip.
    let prompt = asked_about[0].body["messages"][0]["content"]
        .as_str()
        .expect("the persona frames the request with a system prompt");
    assert!(
        prompt.contains("write in French"),
        "the user's own language must reach the persona through the deployment's \
         own configuration, as the fallback for a message whose language cannot \
         be told: {prompt}"
    );

    // --- 3. A second message, and the one approval ------------------------
    //
    // The first suggestion stays unapproved for the rest of the test: it is
    // the one that must never reach the room. The second is the one the user
    // decides to send.
    stack.llm.set_reply(APPROVED_DRAFT);
    let asked_again = format!("Et on se retrouve où ? [{}]", marker("second"));
    stack.says(&decided, &asked_again).await?;
    let second_trigger = stack.wait_for_inbound(&asked_again).await?;
    let second_trigger_id = attribute(&second_trigger.payload, "id").to_owned();
    let second_suggestion = stack
        .wait_for_persona_event(SUGGEST_TYPE, &second_trigger_id)
        .await?;
    let approved_suggestion_id = attribute(&second_suggestion.payload, "id").to_owned();
    assert_eq!(
        second_suggestion.payload["data"]["suggestion"]["body"].as_str(),
        Some(APPROVED_DRAFT)
    );

    // The deliberate act, through the Gateway the Companion talks to. No
    // `final`: what goes out is the persona's own text, so the assertion
    // below is about the very thing the user read before approving.
    let (status, approval) = stack.approve(&approved_suggestion_id, None).await?;
    assert_eq!(
        status,
        reqwest::StatusCode::CREATED,
        "the deployed Gateway must approve a granted contact's suggestion: {approval}"
    );
    assert_eq!(approval["publication"].as_str(), Some("published"));
    assert_eq!(
        approval["edited"].as_bool(),
        Some(false),
        "nothing was edited, so what goes out is what the persona wrote: {approval}"
    );

    let published = stack
        .wait_for_event(APPROVED_TYPE, &approval["event_id"])
        .await?;
    validate_against_contract(&published.payload, "persona.reply.approved")?;
    assert_eq!(
        attribute(&published.payload, "source"),
        attribute(&second_suggestion.payload, "source"),
        "the approval's source names the persona whose suggestion was approved, not the \
         component that published it (ADR 0022)"
    );
    assert_eq!(
        published.payload["data"]["approved_by"].as_str(),
        Some(owner().as_str()),
        "the audit trail says who sent it: {}",
        published.payload
    );
    assert_eq!(
        published.payload["data"]["final"]["body"].as_str(),
        Some(APPROVED_DRAFT),
        "what was approved is exactly what is published"
    );
    assert_eq!(
        published.payload["data"]["target"]["room_id"].as_str(),
        Some(stack.room_id.as_str()),
        "the reply is addressed to the room the message it answers came from"
    );
    assert_eq!(
        trace_id(attribute(&published.payload, "traceparent")),
        trace_id(attribute(&second_trigger.payload, "traceparent")),
        "one trace links sensor → persona → approval → outbound"
    );
    // A known gap, asserted as it is rather than as it should be: the
    // contract's inbound event carries no Matrix event ID to thread under,
    // so an approved reply is not a native Matrix reply (ADR 0022).
    assert!(
        published.payload["data"]["target"]
            .get("reply_to_event_id")
            .is_none(),
        "the approved reply carries no reply_to_event_id, and lands in the room unthreaded — \
         the gap ADR 0022 names: {}",
        published.payload
    );

    // --- 4. The loop closes -----------------------------------------------
    //
    // Asked of the homeserver, as the contact themselves, because that is
    // the only witness that matters: the reply is in their conversation.
    stack.wait_for_room_body(&decided, APPROVED_DRAFT).await?;

    // The same read, once more, for what is *not* there. The approved reply
    // arriving is the fence: the Sensor has consumed the approved subject
    // past this point, so a reply for the unapproved suggestion would be in
    // the room by now if anything had sent one.
    let bodies = stack.room_bodies(&decided).await?;
    assert!(
        !bodies.iter().any(|body| body == UNAPPROVED_DRAFT),
        "a suggestion nobody approved must never reach the room, and the room holds: {bodies:?}"
    );
    // And nothing at all was published for the first suggestion.
    let approvals_of_the_unapproved: Vec<Value> = stack
        .published(APPROVED_TYPE)
        .await?
        .into_iter()
        .map(|message| message.payload)
        .filter(|event| event["data"]["suggestion_event_id"].as_str() == Some(&*unapproved_id))
        .collect();
    assert!(
        approvals_of_the_unapproved.is_empty(),
        "no approval event exists for a suggestion nobody approved: {approvals_of_the_unapproved:?}"
    );

    // The reply is in the room, and it is not a native Matrix reply: the
    // other half of ADR 0022's gap, read off the homeserver's own event.
    let posted = stack
        .room_events(&decided)
        .await?
        .into_iter()
        .find(|event| event["content"]["body"].as_str() == Some(APPROVED_DRAFT))
        .expect("the reply that was just found in the room is one of its events");
    assert_eq!(
        posted["sender"].as_str(),
        Some(format!("@sensor:{DEPLOY_SERVER_NAME}").as_str()),
        "the reply is posted by the Sensor, on the user's behalf: {posted}"
    );
    assert!(
        posted["content"].get("m.relates_to").is_none(),
        "the approved reply lands in the room without being a native Matrix reply, which is \
         ADR 0022's named gap: {posted}"
    );

    // --- 5. Consent revoked between the suggestion and the approval -------
    //
    // The last gate. The suggestion was born `granted` and is still on the
    // bus; what changed is the user's mind, and an approval is refused if the
    // sender's consent is no longer granted *at that moment*.
    stack
        .decide("contact", &decided.user_id, "revoked", &["matrix"])
        .await?;
    let (status, refusal) = stack.approve(&unapproved_id, None).await?;
    assert_eq!(
        status,
        reqwest::StatusCode::CONFLICT,
        "an approval for a contact whose consent was revoked must be refused: {refusal}"
    );
    assert_eq!(
        refusal["error"].as_str(),
        Some("consent_revoked"),
        "and the refusal says which of the situations occurred: {refusal}"
    );

    // A refusal publishes nothing — there is no outbox on this path — so the
    // absence is assertable immediately, on the bus and in the room.
    let approvals_after: Vec<Value> = stack
        .published(APPROVED_TYPE)
        .await?
        .into_iter()
        .map(|message| message.payload)
        .filter(|event| event["data"]["suggestion_event_id"].as_str() == Some(&*unapproved_id))
        .collect();
    assert!(
        approvals_after.is_empty(),
        "a refused approval publishes nothing: {approvals_after:?}"
    );
    let bodies = stack.room_bodies(&decided).await?;
    assert!(
        !bodies.iter().any(|body| body == UNAPPROVED_DRAFT),
        "and nothing was sent to the contact whose consent is revoked: {bodies:?}"
    );

    stack.shutdown().await
}

/// A marker unique to this run and this message: assertions about what
/// reached the model — and about what did not — are then about exactly one
/// message, on a bus and in a room several runs have written to.
fn marker(what: &str) -> String {
    format!("h25-{what}-{}", std::process::id())
}

/// Writes as the contact until one of their messages comes back from the
/// Sensor labelled `granted`.
///
/// The Gateway records a decision and publishes it; the Sensor applies it
/// when it reads it. Between the two instants a message of that contact's is
/// still labelled `pending` — correctly, since that is what the Sensor knew
/// — and a loop test that raced it would assert its absences against a
/// message the gate refused for the right reason at the wrong time.
async fn converge_consent(stack: &Deployment, contact: &Contact) -> Result<()> {
    for attempt in 0..20 {
        let body = format!("ping {attempt} [{}]", marker("converge"));
        stack.says(contact, &body).await?;
        if let Ok(event) = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            stack.wait_for_inbound(&body),
        )
        .await
        {
            if event?.payload["consent"].as_str() == Some("granted") {
                return Ok(());
            }
        }
    }
    anyhow::bail!(
        "the Sensor never applied the decision granting this contact; the deployment's logs \
         were:\n{}",
        stack.deployment_logs().await
    )
}
