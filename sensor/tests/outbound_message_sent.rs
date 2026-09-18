// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Issue #109 / ADR 0018: the user's own messages are their own event type,
//! and the user is not a contact.
//!
//! A portal room mirrors a conversation in both directions, and the Sensor
//! had no notion of the operator, so a message the user sent from their own
//! phone went out as `inbound.message.received`, with the user as the subject
//! and `consent: pending` — the deployment holding that its own owner had
//! never decided about themselves. Observed live in Signal's Note to Self, a
//! conversation with no second party, where a persona would have suggested a
//! reply to nobody.
//!
//! What makes this hard is the identity, not the envelope. On the wire the
//! operator's messages come from a **network ghost of their own account**,
//! indistinguishable in shape from a contact's ghost, and there is **more
//! than one per network**: the reference deployment answers both
//! `@whatsapp_33660469852` and `@whatsapp_lid-115332874281144` for one
//! WhatsApp account, and the message the owner sent from their phone arrived
//! under the LID one. So the set is confirmed by the deployment and handed to
//! the Sensor (`SENSOR_OWNER`, `SENSOR_OWNER_IDENTITIES`); it is not derived
//! by string-building from a bridge login id, which would have found the
//! phone ghost and missed the one the messages actually come from.
//!
//! What this file asserts, from the bus and never from inside the Sensor:
//!
//! - a confirmed identity's message is published as `outbound.message.sent`,
//!   with the operator's Matrix ID as the subject, **no `consent` extension
//!   at all** — not on the envelope and not as a NATS header — and no
//!   `contact` object, so the user never enters the consent model;
//! - and no `inbound.message.received` is published for it, because there is
//!   one event per occurrence and this is the one;
//! - a second confirmed ghost on the same network is the same operator: one
//!   subject on the bus, not one per ghost;
//! - an **unconfirmed** ghost stays a contact — unknown is not the operator,
//!   just as unknown is not consent;
//! - a contact who sets the operator's own display name stays a contact, because
//!   the display name is a hint and never an identity: matching by name would
//!   let a contact impersonate the operator into an exemption;
//! - a reply the user sent quotes a contact's message only when the user's
//!   decision about **that** contact is `granted` (issue #110): their own
//!   message is not a way around their own decision about somebody else;
//! - and the configuration refuses to start half-set, because ghosts with no
//!   operator would silently publish the user's messages as a contact's,
//!   which is the bug.
//!
//! Isolation is the suite's: `TWALK_TEST_STACK`, `TWALK_TEST_SYNAPSE_PORT`
//! and `TWALK_TEST_NATS_PORT` move the whole stack aside for a parallel
//! worktree (`sensor/tests/harness/mod.rs`).

mod harness;

use anyhow::Result;
use harness::gateway::{contact_entry, sensor_env_granting};
use harness::{
    ensure_stack, make_whatsapp_portal, poll_until, sensor_env_with, sha256_hex,
    validate_against_contract, Bot, Bus, SensorProc, StoredMessage, SENSOR_USER_ID,
};
use serde_json::Value;

const STREAM: &str = "twalk";
const OUTBOUND_SUBJECT: &str = "twalk.outbound.message.sent.v1";
const INBOUND_SUBJECT: &str = "twalk.inbound.message.received.v1";

/// The operator: a Matrix ID the Sensor never logs in as and never resolves,
/// only publishes. It is the Gateway's `GATEWAY_OWNER` seen from here.
const OWNER: &str = "@michel:test.twalk";

/// The operator's own network ghosts, as the reference deployment answers
/// them: two on one network, and neither derivable from the other.
const OWNER_PHONE_GHOST: &str = "whatsapp_33660469852";
const OWNER_LID_GHOST: &str = "whatsapp_lid-115332874281144";
/// A ghost of the same shape, on the same network, that the deployment has
/// **not** confirmed as the operator's.
const UNCONFIRMED_GHOST: &str = "whatsapp_33612345678";

/// The display name both of the operator's WhatsApp ghosts carry on the
/// reference deployment. A contact may set it too, which is the whole reason
/// it is never what the Sensor matches on.
const OWNER_DISPLAY_NAME: &str = "Michel-Marie MAUDET (WA)";

/// The environment of a Sensor that knows who its operator is.
fn owner_env(identities: &[&str]) -> Vec<(String, String)> {
    let identities = identities
        .iter()
        .map(|localpart| format!("@{localpart}:test.twalk"))
        .collect::<Vec<_>>()
        .join(",");
    sensor_env_with(&[
        ("SENSOR_OWNER", OWNER),
        ("SENSOR_OWNER_IDENTITIES", &identities),
    ])
}

fn attribute<'a>(event: &'a Value, name: &str) -> &'a str {
    event[name]
        .as_str()
        .unwrap_or_else(|| panic!("the event has no string {name}: {event}"))
}

/// Every stored event of one type for one room, oldest first.
async fn events_for(bus: &Bus, subject: &str, room_id: &str) -> Result<Vec<StoredMessage>> {
    bus.fetch_room_messages(STREAM, subject, room_id).await
}

/// Waits until a room has produced `count` events on a subject, then returns
/// them. Waiting for the count rather than for the first one is what makes
/// "and nothing else was published" assertable a line later.
async fn wait_for_count(
    bus: &Bus,
    subject: &str,
    room_id: &str,
    count: usize,
) -> Result<Vec<StoredMessage>> {
    poll_until(
        || async {
            let stored = events_for(bus, subject, room_id).await.ok()?;
            (stored.len() >= count).then_some(stored)
        },
        &format!("waiting for {count} event(s) from {room_id} on {subject}"),
    )
    .await
}

#[tokio::test]
async fn the_users_own_messages_are_published_as_their_own_type() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&owner_env(&[OWNER_PHONE_GHOST, OWNER_LID_GHOST]))?;

    let bridge = Bot::login("bot_alpha").await?;
    let phone_ghost = Bot::login(OWNER_PHONE_GHOST).await?;
    let lid_ghost = Bot::login(OWNER_LID_GHOST).await?;
    let unconfirmed = Bot::login(UNCONFIRMED_GHOST).await?;
    let impostor = Bot::login("bot_beta").await?;

    let room_id = make_whatsapp_portal(&bridge, "own-messages").await?;
    bridge.invite(&room_id, SENSOR_USER_ID).await?;
    bridge
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    for member in [&phone_ghost, &lid_ghost, &unconfirmed, &impostor] {
        bridge.invite(&room_id, member.user_id()).await?;
        member.join_room(&room_id).await?;
    }
    // A contact who calls themselves by the operator's name, in this room.
    // Set before a single message is sent, so the Sensor resolves it from the
    // room's membership exactly as it resolves any other contact's.
    impostor
        .set_room_display_name(&room_id, OWNER_DISPLAY_NAME)
        .await?;

    // The operator, writing from their phone, under each of their two ghosts.
    let phone_event_id = phone_ghost
        .send_message(&room_id, "je confirme pour 20h")
        .await?;
    lid_ghost
        .send_message(&room_id, "et j'apporte le dessert")
        .await?;
    // A contact of the same shape the deployment never confirmed, and a
    // contact wearing the operator's name.
    unconfirmed
        .send_message(&room_id, "moi je suis un contact")
        .await?;
    impostor
        .send_message(&room_id, "et moi aussi, malgré mon nom")
        .await?;

    let outbound = wait_for_count(&bus, OUTBOUND_SUBJECT, &room_id, 2).await?;
    let inbound = wait_for_count(&bus, INBOUND_SUBJECT, &room_id, 2).await?;

    // ---- the operator's two ghosts are one operator -----------------------
    assert_eq!(
        outbound.len(),
        2,
        "exactly the operator's two messages are published as outbound.message.sent"
    );
    let bodies: Vec<&str> = outbound
        .iter()
        .map(|stored| stored.payload["data"]["body"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        bodies,
        vec!["je confirme pour 20h", "et j'apporte le dessert"]
    );

    for stored in &outbound {
        let event = &stored.payload;
        validate_against_contract(event, "outbound.message.sent")?;
        assert_eq!(
            attribute(event, "type"),
            "fr.linagora.twalk.outbound.message.sent.v1"
        );
        assert_eq!(
            attribute(event, "subject"),
            OWNER,
            "the subject is the operator's Matrix ID, never the ghost the message arrived \
             under: one operator on the bus, not one per network and one per ghost"
        );
        assert_eq!(attribute(event, "network"), "whatsapp");

        // ADR 0018's load-bearing absence: the consent extension carries a
        // contact's decision, and there is no contact in this event.
        assert!(
            event.get("consent").is_none(),
            "outbound.message.sent carries no consent extension at all: {event}"
        );
        assert!(
            stored.header("consent").is_none(),
            "and none as a NATS header either — the headers duplicate the envelope's \
             extensions, so a consumer filtering on consent must not match this"
        );
        assert_eq!(
            stored.header("network"),
            Some("whatsapp"),
            "the network header still travels: it is not a decision about anybody"
        );

        // The user is not a contact, so nothing about them is resolved as
        // one — not even their display name, which the inbound path would
        // have published.
        assert!(
            event["data"].get("contact").is_none(),
            "the user is not a contact (ADR 0018): {event}"
        );
        let serialized = serde_json::to_string(event)?;
        assert!(
            !serialized.contains(OWNER_DISPLAY_NAME),
            "no display name of the operator's reaches the bus: {serialized}"
        );
        assert!(
            !serialized.contains(OWNER_PHONE_GHOST) && !serialized.contains(OWNER_LID_GHOST),
            "and neither does the ghost the message arrived under: {serialized}"
        );
    }

    // The natural key is unchanged: one Matrix event still has one id, and it
    // is still what the bus deduplicates on.
    let first = &outbound[0];
    let expected_id = sha256_hex(&format!("{phone_event_id}:{room_id}"));
    assert_eq!(attribute(&first.payload, "id"), expected_id);
    assert_eq!(
        first.header("Nats-Msg-Id"),
        Some(expected_id.as_str()),
        "Nats-Msg-Id must equal the CloudEvents id for JetStream dedup"
    );
    assert_eq!(
        attribute(&first.payload, "source"),
        format!("matrix://test.twalk/{room_id}"),
        "the room is what names the conversation the user answered"
    );
    // ---- and they are published once, not twice ---------------------------
    let inbound_subjects: Vec<&str> = inbound
        .iter()
        .map(|stored| attribute(&stored.payload, "subject"))
        .collect();
    assert_eq!(
        inbound_subjects,
        vec![
            format!("@{UNCONFIRMED_GHOST}:test.twalk"),
            "@bot_beta:test.twalk".to_owned()
        ],
        "only the two contacts are published as inbound: the operator's messages went out \
         once, as their own type, and not also through the inbound door"
    );

    // ---- unknown is not the operator --------------------------------------
    let unconfirmed_event = &inbound[0].payload;
    validate_against_contract(unconfirmed_event, "inbound.message.received")?;
    assert_eq!(
        attribute(unconfirmed_event, "consent"),
        "pending",
        "a ghost of the same shape the deployment did not confirm stays a contact, with the \
         user's decision about them — it is not exempted from the consent model on a guess"
    );

    // ---- a display name is a hint, never an identity ----------------------
    let impostor_event = &inbound[1].payload;
    validate_against_contract(impostor_event, "inbound.message.received")?;
    assert_eq!(
        impostor_event["data"]["contact"]["display_name"].as_str(),
        Some(OWNER_DISPLAY_NAME),
        "the contact really is wearing the operator's name"
    );
    assert_eq!(
        attribute(impostor_event, "consent"),
        "pending",
        "and is still a contact: a contact chooses their own display name, so matching the \
         operator by one would hand them the exemption for the price of a rename"
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_reply_the_user_sent_quotes_a_contact_only_when_granted() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;

    // The user granted one contact and never decided about the other. In the
    // Sensor's cache `pending` is exactly the absence of a decision
    // (CONTEXT.md), so the undecided contact is simply not in the snapshot.
    let granted = Bot::login("bot_beta").await?;
    let undecided = Bot::login(UNCONFIRMED_GHOST).await?;
    let (_gateway, env) = sensor_env_granting(
        &bus,
        vec![contact_entry(granted.user_id(), "whatsapp", "granted")],
        &[
            ("SENSOR_OWNER", OWNER),
            (
                "SENSOR_OWNER_IDENTITIES",
                concat!("@whatsapp_lid-115332874281144", ":test.twalk"),
            ),
        ],
    )
    .await?;
    let sensor = SensorProc::start(&env)?;

    let bridge = Bot::login("bot_alpha").await?;
    let owner_ghost = Bot::login(OWNER_LID_GHOST).await?;
    let room_id = make_whatsapp_portal(&bridge, "own-replies").await?;
    bridge.invite(&room_id, SENSOR_USER_ID).await?;
    bridge
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    for member in [&granted, &undecided, &owner_ghost] {
        bridge.invite(&room_id, member.user_id()).await?;
        member.join_room(&room_id).await?;
    }

    const GRANTED_WORDS: &str = "on décale à 20h ?";
    const UNDECIDED_WORDS: &str = "personne ne m'a rien demandé";
    let granted_parent = granted.send_message(&room_id, GRANTED_WORDS).await?;
    let undecided_parent = undecided.send_message(&room_id, UNDECIDED_WORDS).await?;
    // Both parents are published first, so the replies below cannot race the
    // consent labels they depend on.
    wait_for_count(&bus, INBOUND_SUBJECT, &room_id, 2).await?;

    reply(&owner_ghost, &room_id, &granted_parent, "oui, parfait").await?;
    reply(
        &owner_ghost,
        &room_id,
        &undecided_parent,
        "je réponds quand même",
    )
    .await?;

    let outbound = wait_for_count(&bus, OUTBOUND_SUBJECT, &room_id, 2).await?;
    assert_eq!(outbound.len(), 2);

    let quoting_granted = &outbound[0].payload;
    validate_against_contract(quoting_granted, "outbound.message.sent")?;
    assert_eq!(
        quoting_granted["data"]["reply_to"]["matrix_event_id"].as_str(),
        Some(granted_parent.as_str())
    );
    assert_eq!(
        quoting_granted["data"]["reply_to"]["excerpt"].as_str(),
        Some(GRANTED_WORDS),
        "the user's reply quotes a granted contact as it always did"
    );

    let quoting_undecided = &outbound[1].payload;
    validate_against_contract(quoting_undecided, "outbound.message.sent")?;
    assert_eq!(
        quoting_undecided["data"]["reply_to"]["matrix_event_id"].as_str(),
        Some(undecided_parent.as_str()),
        "the relation is kept: the reply is still a reply"
    );
    assert!(
        quoting_undecided["data"]["reply_to"]
            .get("excerpt")
            .is_none(),
        "and the excerpt is simply absent — this type has no consent label to say \
         'withheld' with, so nothing pretends otherwise: {quoting_undecided}"
    );
    let serialized = serde_json::to_string(quoting_undecided)?;
    assert!(
        !serialized.contains(UNDECIDED_WORDS),
        "the undecided contact's words appear nowhere in the user's own event (#110): \
         {serialized}"
    );

    sensor.stop().await;
    Ok(())
}

/// A reply in the Matrix shape the bridges use: `m.in_reply_to` on the
/// content's `m.relates_to`.
async fn reply(bot: &Bot, room_id: &str, parent_event_id: &str, body: &str) -> Result<String> {
    bot.send_event(
        room_id,
        "m.room.message",
        serde_json::json!({
            "msgtype": "m.text",
            "body": body,
            "m.relates_to": { "m.in_reply_to": { "event_id": parent_event_id } },
        }),
    )
    .await
}

#[tokio::test]
async fn ghosts_without_an_operator_are_refused_on_startup() -> Result<()> {
    ensure_stack().await?;
    // Half a configuration would degrade exactly like none — the user's own
    // messages published as a contact's — but silently, which is the bug this
    // ticket is about. The Sensor refuses it with the name to fix. No Matrix
    // session is ever opened, so this needs no SENSOR_LOCK.
    let mut env = sensor_env_with(&[(
        "SENSOR_OWNER_IDENTITIES",
        "@whatsapp_33660469852:test.twalk",
    )]);
    env.retain(|(key, _)| key != "SENSOR_OWNER");
    let mut sensor = SensorProc::start(&env)?;

    poll_until(
        || async {
            sensor
                .logs()
                .await
                .iter()
                .any(|line| line.contains("SENSOR_OWNER_IDENTITIES is set without SENSOR_OWNER"))
                .then_some(())
        },
        "the Sensor to refuse a half-configured operator",
    )
    .await?;
    assert!(
        !sensor.is_running(),
        "a refused configuration must stop the process, not warn and carry on"
    );
    Ok(())
}
