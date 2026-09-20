// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Issue #110: an excerpt is the **quoted author's** content, and the event
//! carrying it is labelled by whoever quoted them.
//!
//! `#58` reduced publication on the sender's label alone (ADR 0012), which is
//! right for everything the sender wrote and wrong for the one field they did
//! not: `reply_to.excerpt` and a reaction's `target.excerpt` quote somebody
//! else's message. In a group those are two different contacts, so a granted
//! contact reacting to a revoked one's message published the revoked one's
//! words in cleartext inside an event labelled `granted` — past every
//! consumer gate, which decides on that one label (`sdk/python`, ADR 0012).
//! It was observed live on a real WhatsApp group through the reference
//! deployment.
//!
//! What this file asserts, from the bus and never from inside the Sensor:
//!
//! - a granted contact's reaction on a **revoked** contact's message carries
//!   the reaction, its emoji and its target reference, and no excerpt — the
//!   reduction removes the quoted content, not the event;
//! - the same for a reply quoting that message;
//! - the same for an author the user never decided about, because in the
//!   Sensor's cache `pending` *is* the absence of a decision (CONTEXT.md) and
//!   unknown is not consent;
//! - with the quoted author granted, the excerpt is published as before;
//! - and the user's own messages keep their excerpts whoever quotes them,
//!   which is what the deployment's own account is for.
//!
//! The consent state comes from the stub Companion Gateway's snapshot (ADR
//! 0010), applied before the sync loop starts, so each contact's label is
//! settled on its very first event and nothing here races.
//!
//! Isolation is the suite's: `TWALK_TEST_STACK`, `TWALK_TEST_SYNAPSE_PORT`
//! and `TWALK_TEST_NATS_PORT` move the whole stack aside for a parallel
//! worktree (`sensor/tests/harness/mod.rs`).

mod harness;

use anyhow::Result;
use harness::gateway::{contact_entry, sensor_env_granting, StubGateway};
use harness::{
    contract_fixture, ensure_stack, make_whatsapp_portal, poll_until, sha256_hex,
    validate_against_contract, Bot, Bus, SensorProc, StoredMessage, SENSOR_USER_ID,
};
use serde_json::{json, Value};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const STREAM: &str = "twalk";
const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";
const REACTION_SUBJECT: &str = "twalk.inbound.reaction.added.v1";
const CONSENT_SUBJECT: &str = "twalk.consent.state.changed.v1";

/// The words of the contact the user revoked. They are the point of the whole
/// file: every assertion below that says "no excerpt" is also asked whether
/// these characters appear anywhere in the published envelope.
const REVOKED_WORDS: &str = "je ne veux pas que ça sorte d'ici";
/// The words of a contact the user never decided about.
const UNDECIDED_WORDS: &str = "et moi personne ne m'a rien demandé";
/// The words of the same contact once the user has granted them.
const GRANTED_WORDS: &str = "d'accord, tu peux répéter ça";
/// The user's own words, quoted by their contacts.
const OWN_WORDS: &str = "je confirme pour 20h";

/// A fresh CloudEvents id per consent event: the bus persists across runs, so
/// reusing the fixture id would collide in JetStream dedup.
fn unique_event_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    sha256_hex(&format!(
        "{nanos}:{}",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// A contact-scoped `consent.state.changed` event patched from the contract
/// fixture — what the Companion Gateway, the single writer of consent state
/// (ADR 0006), publishes; the test bus stands in for it.
fn consent_change(
    subject_id: &str,
    network: &str,
    old_state: &str,
    new_state: &str,
) -> Result<Value> {
    let mut event = contract_fixture("consent.state.changed")?;
    let now = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
    event["id"] = json!(unique_event_id());
    event["time"] = json!(now);
    event["subject"] = json!(subject_id);
    event["consent"] = json!(new_state);
    event["network"] = json!(network);
    event["data"]["subject"] = json!({ "type": "contact", "id": subject_id });
    event["data"]["old_state"] = json!(old_state);
    event["data"]["new_state"] = json!(new_state);
    event["data"]["scope"]["connections"] = json!([network]);
    event["data"]["scope"]["networks"] = json!([network]);
    event["data"]["occurred_at"] = json!(now);
    validate_against_contract(&event, "consent.state.changed")?;
    Ok(event)
}

/// The Sensor, with a Companion Gateway serving `entries` as the whole
/// current consent state at the bus's current position.
async fn sensor_with_consent(bus: &Bus, entries: Vec<Value>) -> Result<(StubGateway, SensorProc)> {
    let (gateway, env) = sensor_env_granting(bus, entries, &[]).await?;
    let sensor = SensorProc::start(&env)?;
    Ok((gateway, sensor))
}

/// A portal room the Sensor observes, with every `member` joined to it.
async fn observed_portal(bridge: &Bot, name: &str, members: &[&Bot]) -> Result<String> {
    let room_id = make_whatsapp_portal(bridge, name).await?;
    bridge.invite(&room_id, SENSOR_USER_ID).await?;
    bridge
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    for member in members {
        bridge.invite(&room_id, member.user_id()).await?;
        member.join_room(&room_id).await?;
    }
    Ok(room_id)
}

/// Sends a reply to `parent_id`, the Matrix way the Sensor reads as the
/// contract's `reply_to`.
async fn send_reply(bot: &Bot, room_id: &str, body: &str, parent_id: &str) -> Result<String> {
    bot.send_event(
        room_id,
        "m.room.message",
        json!({
            "msgtype": "m.text",
            "body": body,
            "m.relates_to": { "m.in_reply_to": { "event_id": parent_id } },
        }),
    )
    .await
}

/// Polls until the bus holds the event produced from one Matrix event, found
/// by recomputing the contract's deterministic id independently of the
/// Sensor's own code.
async fn wait_for_event(
    bus: &Bus,
    subject: &str,
    room_id: &str,
    matrix_event_id: &str,
) -> Result<StoredMessage> {
    let expected_id = sha256_hex(&format!("{matrix_event_id}:{room_id}"));
    poll_until(
        || async {
            bus.fetch_room_messages(STREAM, subject, room_id)
                .await
                .ok()?
                .into_iter()
                .find(|m| m.payload["id"].as_str() == Some(expected_id.as_str()))
        },
        &format!("the stored event for {matrix_event_id}"),
    )
    .await
}

/// Sends a message repeatedly until an event for this sender carries `label`:
/// the consent consumer and the sync loop race, so the first message after a
/// decision may still be labelled with the previous state (the established
/// harness pattern for racing producers).
async fn wait_for_label(
    bus: &Bus,
    sender: &Bot,
    room_id: &str,
    body: &str,
    label: &str,
) -> Result<()> {
    poll_until(
        || async {
            sender.send_message(room_id, body).await.ok()?;
            bus.fetch_room_messages(STREAM, MESSAGE_SUBJECT, room_id)
                .await
                .ok()?
                .into_iter()
                .find(|m| {
                    m.payload["subject"].as_str() == Some(sender.user_id())
                        && m.payload["consent"].as_str() == Some(label)
                })
        },
        &format!("an event relabelled {label}"),
    )
    .await?;
    Ok(())
}

/// The whole published envelope as text. An excerpt is the one field that
/// could carry another person's words, but the promise is about the event,
/// not about a field: so the words are looked for everywhere in it.
fn says(event: &Value, words: &str) -> bool {
    serde_json::to_string(event)
        .expect("a published envelope is JSON")
        .contains(words)
}

/// The acceptance criteria of #110, in one room with three authors: one the
/// user revoked, one they never decided about, and — after they change their
/// mind — one they granted. The contact quoting them all is granted
/// throughout, so every event below is labelled `granted` and the only thing
/// that varies is whose words are being quoted.
#[tokio::test]
async fn an_excerpt_is_published_only_when_the_quoted_author_is_granted() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;

    // alpha plays the bridge and the contact the user revoked; beta is the
    // granted contact quoting them; the puppet is a contact the user never
    // decided about.
    let alpha = Bot::login("bot_alpha").await?;
    let beta = Bot::login("bot_beta").await?;
    let puppet = Bot::login("whatsapp_33612345678").await?;

    let (_gateway, sensor) = sensor_with_consent(
        &bus,
        vec![
            contact_entry(alpha.user_id(), "whatsapp", "revoked"),
            contact_entry(beta.user_id(), "whatsapp", "granted"),
        ],
    )
    .await?;

    let room_id = observed_portal(&alpha, "excerpt-consent-portal", &[&beta, &puppet]).await?;

    // --- The revoked author -------------------------------------------------
    //
    // Their own message is already reduced, which is #58 working: no body
    // reaches the bus. The words below exist only inside Matrix.
    let revoked_message = alpha.send_message(&room_id, REVOKED_WORDS).await?;
    let reduced = wait_for_event(&bus, MESSAGE_SUBJECT, &room_id, &revoked_message).await?;
    validate_against_contract(&reduced.payload, "inbound.message.received")?;
    assert_eq!(reduced.payload["consent"].as_str(), Some("revoked"));
    assert!(
        reduced.payload["data"].get("body").is_none() && !says(&reduced.payload, REVOKED_WORDS),
        "the premise: a revoked contact's own words never reach the bus (ADR 0012)"
    );

    // A granted contact reacts to it. This is the live observation of #110:
    // the event is labelled `granted` — the reactor's state — and used to
    // carry the revoked author's message in cleartext.
    let reaction_id = beta.send_reaction(&room_id, &revoked_message, "😂").await?;
    let reaction = wait_for_event(&bus, REACTION_SUBJECT, &room_id, &reaction_id).await?;
    validate_against_contract(&reaction.payload, "inbound.reaction.added")?;
    assert_eq!(
        reaction.payload["consent"].as_str(),
        Some("granted"),
        "the label is the reactor's, which is exactly why it cannot vouch for the author"
    );
    assert_eq!(reaction.payload["subject"].as_str(), Some(beta.user_id()));
    assert_eq!(
        reaction.payload["data"]["reaction"].as_str(),
        Some("😂"),
        "the reaction is the reactor's own gesture and still goes out"
    );
    assert_eq!(
        reaction.payload["data"]["target"]["matrix_event_id"].as_str(),
        Some(revoked_message.as_str()),
        "so does the reference to what it points at: the reduction removes the quoted \
         content, not the event"
    );
    assert!(
        reaction.payload["data"]["target"].get("excerpt").is_none(),
        "and the excerpt does not: it is the revoked author's content"
    );
    assert!(
        !says(&reaction.payload, REVOKED_WORDS),
        "the revoked author's words appear nowhere in the published event: {}",
        reaction.payload
    );

    // The same message, quoted by a reply this time.
    let reply_id = send_reply(&beta, &room_id, "mdr", &revoked_message).await?;
    let reply = wait_for_event(&bus, MESSAGE_SUBJECT, &room_id, &reply_id).await?;
    validate_against_contract(&reply.payload, "inbound.message.received")?;
    assert_eq!(reply.payload["consent"].as_str(), Some("granted"));
    assert_eq!(
        reply.payload["data"]["body"].as_str(),
        Some("mdr"),
        "the granted contact's own words are their own to publish"
    );
    assert_eq!(
        reply.payload["data"]["reply_to"]["matrix_event_id"].as_str(),
        Some(revoked_message.as_str()),
        "the relation survives: only the quotation is withheld"
    );
    assert_eq!(
        reply.payload["data"]["reply_to"]["excerpt"].as_str(),
        Some(""),
        "the contract requires the field on a non-revoked sender's reply, so what is \
         withheld is the quoted text: the empty excerpt the Sensor has always published \
         for a parent it could not read"
    );
    assert!(
        !says(&reply.payload, REVOKED_WORDS),
        "the revoked author's words appear nowhere in the published event: {}",
        reply.payload
    );

    // --- The author the user never decided about ----------------------------
    //
    // `pending` in the Sensor's cache is the absence of a decision, not a
    // weaker kind of yes. Their own message is labelled and published in full
    // — `pending` is unchanged (ADR 0012) — and a consumer refuses it on the
    // label. Quoted inside somebody else's `granted` event, no label would
    // protect it, so it is withheld.
    let undecided_message = puppet.send_message(&room_id, UNDECIDED_WORDS).await?;
    let undecided = wait_for_event(&bus, MESSAGE_SUBJECT, &room_id, &undecided_message).await?;
    validate_against_contract(&undecided.payload, "inbound.message.received")?;
    assert_eq!(undecided.payload["consent"].as_str(), Some("pending"));
    assert_eq!(
        undecided.payload["data"]["body"].as_str(),
        Some(UNDECIDED_WORDS),
        "pending is unchanged: the Sensor labels, and consumers refuse"
    );

    let quoting_reaction = beta
        .send_reaction(&room_id, &undecided_message, "👍")
        .await?;
    let quoting = wait_for_event(&bus, REACTION_SUBJECT, &room_id, &quoting_reaction).await?;
    validate_against_contract(&quoting.payload, "inbound.reaction.added")?;
    assert_eq!(quoting.payload["consent"].as_str(), Some("granted"));
    assert!(
        quoting.payload["data"]["target"].get("excerpt").is_none()
            && !says(&quoting.payload, UNDECIDED_WORDS),
        "unknown is not consent: {}",
        quoting.payload
    );

    // --- The author the user grants -----------------------------------------
    //
    // The user changes their mind about alpha. From then on the excerpt is
    // published, because both decisions the event depends on are now grants.
    bus.publish(
        CONSENT_SUBJECT,
        &consent_change(alpha.user_id(), "whatsapp", "revoked", "granted")?,
    )
    .await?;
    wait_for_label(&bus, &alpha, &room_id, "je change d'avis", "granted").await?;

    let granted_message = alpha.send_message(&room_id, GRANTED_WORDS).await?;
    let granted_reaction = beta.send_reaction(&room_id, &granted_message, "🎉").await?;
    let published = wait_for_event(&bus, REACTION_SUBJECT, &room_id, &granted_reaction).await?;
    validate_against_contract(&published.payload, "inbound.reaction.added")?;
    assert_eq!(
        published.payload["data"]["target"]["excerpt"].as_str(),
        Some(GRANTED_WORDS),
        "with both contacts granted the excerpt is published, as it always was"
    );

    let granted_reply = send_reply(&beta, &room_id, "noté", &granted_message).await?;
    let published_reply = wait_for_event(&bus, MESSAGE_SUBJECT, &room_id, &granted_reply).await?;
    validate_against_contract(&published_reply.payload, "inbound.message.received")?;
    assert_eq!(
        published_reply.payload["data"]["reply_to"]["excerpt"].as_str(),
        Some(GRANTED_WORDS),
        "and the reply quotes its parent again"
    );

    // The bus outlives this run and the decision above would label alpha for
    // every later test in it. The user changes their mind back — an ordinary
    // decision, published the ordinary way.
    bus.publish(
        CONSENT_SUBJECT,
        &consent_change(alpha.user_id(), "whatsapp", "granted", "pending")?,
    )
    .await?;

    sensor.stop().await;
    Ok(())
}

/// The other half of the rule: the deployment's own account. The user's own
/// messages — and the replies the Sensor sends for them — are not a third
/// party's content and there is no decision about the user to consult, so
/// quoting them keeps working exactly as it did, whoever is quoting and
/// whatever the user decided about *them*.
#[tokio::test]
async fn the_users_own_message_is_still_quoted_by_their_contacts() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;

    let alpha = Bot::login("bot_alpha").await?;
    let beta = Bot::login("bot_beta").await?;
    // The owner's own account, as another of its devices: the same Matrix user
    // the Sensor runs as, which is what makes these messages the user's own.
    let owner = Bot::login("sensor").await?;

    let (_gateway, sensor) = sensor_with_consent(
        &bus,
        vec![contact_entry(beta.user_id(), "whatsapp", "granted")],
    )
    .await?;

    let room_id = observed_portal(&alpha, "excerpt-owner-portal", &[&beta]).await?;
    let own_message = owner.send_message(&room_id, OWN_WORDS).await?;

    // A granted contact reacts to the user's own message.
    let reaction_id = beta.send_reaction(&room_id, &own_message, "👌").await?;
    let reaction = wait_for_event(&bus, REACTION_SUBJECT, &room_id, &reaction_id).await?;
    validate_against_contract(&reaction.payload, "inbound.reaction.added")?;
    assert_eq!(
        reaction.payload["data"]["target"]["excerpt"].as_str(),
        Some(OWN_WORDS),
        "the user's own words need no decision at all"
    );

    // And so does a contact the user never decided about: the rule keys on
    // the author of the quoted message, never on the one quoting it, so a
    // one-to-one conversation keeps its excerpts from the first message on.
    let reply_id = send_reply(&alpha, &room_id, "parfait", &own_message).await?;
    let reply = wait_for_event(&bus, MESSAGE_SUBJECT, &room_id, &reply_id).await?;
    validate_against_contract(&reply.payload, "inbound.message.received")?;
    assert_eq!(reply.payload["consent"].as_str(), Some("pending"));
    assert_eq!(
        reply.payload["data"]["reply_to"]["excerpt"].as_str(),
        Some(OWN_WORDS),
        "a pending contact quoting the user still quotes them"
    );

    sensor.stop().await;
    Ok(())
}
