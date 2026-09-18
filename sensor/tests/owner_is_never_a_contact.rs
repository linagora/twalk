// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Issue #147 / ADR 0021: the owner is never a contact, on **any** event.
//!
//! ADR 0018 made that true for messages. It was not true for the rest: the
//! user's own reactions were still published as `inbound.reaction.added` with
//! one of their ghosts as the subject and a `consent` label, and so was their
//! presence.
//!
//! The label was the sharp end of it. `ConsentCache::state` falls back to the
//! **network's default** before `pending`, so on a deployment where the user
//! had granted a whole network — the ordinary case, since nobody decides
//! about hundreds of contacts one at a time — the user's own reaction came
//! out labelled `granted`, and `normalize::contact_entry` attaches
//! `network_identifier` when the label is `granted`. On a phone-based network
//! that identifier is minted from the ghost localpart: the deployment was
//! publishing **the operator's own phone number as a contact's attribute**,
//! under a label asserting that a third party had agreed to it. That is the
//! configuration this file sets up, because it is the one that was wrong.
//!
//! ADR 0021 settles the two shapes, and they are deliberately different:
//!
//! - a reaction the user added is `outbound.reaction.added`, a type of its
//!   own with the operator's Matrix ID as the subject and no `consent`
//!   extension — symmetrical with `outbound.message.sent`, and for the same
//!   reason: the event tells a consumer something (this conversation has been
//!   acknowledged) that ADR 0018's rejected "publish nothing" would destroy;
//! - the user's own presence is **not published at all**, because it tells
//!   nobody anything they do not already know.
//!
//! What this file asserts, from the bus and never from inside the Sensor:
//!
//! - **the enumeration**, first and without a stack: every contract type
//!   whose `subject` is a Matrix user ID — read from the schemas, not listed
//!   here — has a recorded answer to "what happens when that person is the
//!   owner?", so a tenth or eleventh type cannot be added without answering
//!   it, which is the part of this ticket meant to outlive it;
//! - the user's own reaction is published as `outbound.reaction.added`, with
//!   no `consent` on the envelope, no `consent` NATS header, and no `contact`
//!   object — so neither their display name nor their phone number travels;
//! - no `inbound.reaction.added` is published for it, because there is one
//!   event per occurrence and this is the one;
//! - the user's own presence produces no event on any subject;
//! - a **contact's** presence is still published: the rule is #109's, an
//!   exact match against the identities the deployment confirmed, and an
//!   unconfirmed identity is a contact, so failing safe here means
//!   publishing;
//! - and nothing about the owner carries a `consent` extension on any of the
//!   types the enumeration found.
//!
//! Isolation is the suite's: `TWALK_TEST_STACK`, `TWALK_TEST_SYNAPSE_PORT`
//! and `TWALK_TEST_NATS_PORT` move the whole stack aside for a parallel
//! worktree (`sensor/tests/harness/mod.rs`).

mod harness;

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::Result;
use harness::gateway::{network_entry, sensor_env_granting};
use harness::{
    contract_schema, contract_type_allows_consent, contract_types_about_a_person, ensure_stack,
    make_whatsapp_portal, poll_until, sha256_hex, validate_against_contract, Bot, Bus, SensorProc,
    StoredMessage, MATRIX_USER_ID_SUBJECT_PATTERN, SENSOR_USER_ID,
};
use serde_json::Value;
use tokio::time::sleep;

const STREAM: &str = "twalk";

/// The operator, and the two ghosts of their one WhatsApp account, as the
/// reference deployment answers them (ADR 0018).
const OWNER: &str = "@michel:test.twalk";
const OWNER_PHONE_GHOST: &str = "whatsapp_33660469852";
const OWNER_LID_GHOST: &str = "whatsapp_lid-115332874281144";
/// A ghost of the same shape, on the same network, that the deployment has
/// **not** confirmed as the operator's: a contact, and the control for every
/// absence asserted below.
const CONTACT_GHOST: &str = "whatsapp_33612345678";

/// What the Sensor does about the owner on one contract type whose subject
/// is a person. Every such type must have one of these, recorded here by a
/// human who looked, and the test below fails naming any type that has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerRule {
    /// The owner is never the subject of this type. Their traffic of this
    /// kind is published as the named type instead, which must itself be
    /// `TheOwnersOwn` — so "we moved it somewhere" can never be satisfied by
    /// moving it somewhere that still labels them a contact.
    NeverTheSubject { published_as: &'static str },
    /// The owner **is** the subject, and the type carries no `consent`
    /// extension: the schema declares none and closes itself to extras, so a
    /// third-party producer that sets one fails validation.
    TheOwnersOwn,
    /// Nothing is published about the owner on this type at all.
    NotPublishedAboutTheOwner,
}

use OwnerRule::{NeverTheSubject, NotPublishedAboutTheOwner, TheOwnersOwn};

/// The answer for every contract type whose subject is a person.
///
/// This table is checked **against the contract**, not trusted: the test
/// below reads the schemas, and fails if the two disagree in either
/// direction. Adding a tenth type with a Matrix-user-ID subject therefore
/// breaks this test until somebody writes down what it does about the owner,
/// which is the whole point of issue #147 — the alternative is a list that
/// goes quietly stale and an invariant that quietly stops holding.
const OWNER_RULES: &[(&str, OwnerRule)] = &[
    (
        "inbound.message.received",
        NeverTheSubject {
            published_as: "outbound.message.sent",
        },
    ),
    ("inbound.presence.updated", NotPublishedAboutTheOwner),
    (
        "inbound.reaction.added",
        NeverTheSubject {
            published_as: "outbound.reaction.added",
        },
    ),
    ("outbound.message.sent", TheOwnersOwn),
    ("outbound.reaction.added", TheOwnersOwn),
];

fn bus_subject(type_name: &str) -> String {
    format!("twalk.{type_name}.v1")
}

fn attribute<'a>(event: &'a Value, name: &str) -> &'a str {
    event[name]
        .as_str()
        .unwrap_or_else(|| panic!("the event has no string {name}: {event}"))
}

/// The enumeration. No stack, no Sensor: this one is about the contract.
///
/// It is the acceptance criterion that outlives the ticket. "No event about
/// the owner carries a consent extension" is only a property of the system
/// if something knows which events can be about the owner, and the only
/// answer that cannot go stale is the contract's own: a type whose `subject`
/// is a Matrix user ID names a person, and the owner is a person.
#[test]
fn every_type_the_owner_can_be_the_subject_of_has_an_answer() -> Result<()> {
    let from_the_contract: BTreeSet<String> =
        contract_types_about_a_person()?.into_iter().collect();
    let accounted_for: BTreeSet<String> = OWNER_RULES
        .iter()
        .map(|(type_name, _)| (*type_name).to_owned())
        .collect();

    assert!(
        !from_the_contract.is_empty(),
        "the contract enumeration found no type with a Matrix-user-ID subject, which means it \
         stopped working rather than that the contract changed"
    );
    // And the canonical pattern is still the one the contract spells, so the
    // constant that documents the discriminator cannot rot into a comment.
    assert!(
        from_the_contract.iter().any(|type_name| {
            contract_schema(type_name)
                .ok()
                .and_then(|schema| {
                    schema
                        .pointer("/properties/subject/pattern")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .as_deref()
                == Some(MATRIX_USER_ID_SUBJECT_PATTERN)
        }),
        "no schema spells the Matrix-user-ID subject pattern the harness documents"
    );
    let unanswered: Vec<&String> = from_the_contract.difference(&accounted_for).collect();
    assert!(
        unanswered.is_empty(),
        "these contract types have a person as their subject and no recorded answer to \
         \"what happens when that person is the owner?\": {unanswered:?}. Decide it, record it \
         in ADR 0021 and add it to OWNER_RULES — the owner is never a contact (CONTEXT.md), \
         and a type that has not been asked is a type where that silently stops being true"
    );
    let vanished: Vec<&String> = accounted_for.difference(&from_the_contract).collect();
    assert!(
        vanished.is_empty(),
        "these types are answered here but no longer have a person as their subject in the \
         contract: {vanished:?}"
    );

    for (type_name, rule) in OWNER_RULES {
        match rule {
            TheOwnersOwn => assert!(
                !contract_type_allows_consent(type_name)?,
                "{type_name} has the owner as its subject, so its schema must refuse a consent \
                 extension outright — declaring none and closing itself to extras — rather than \
                 leaving a producer free to set one (ADR 0018, ADR 0021)"
            ),
            NeverTheSubject { published_as } => {
                let target = OWNER_RULES
                    .iter()
                    .find(|(name, _)| name == published_as)
                    .unwrap_or_else(|| panic!("{published_as} is not in OWNER_RULES"));
                assert_eq!(
                    target.1, TheOwnersOwn,
                    "{type_name} redirects the owner to {published_as}, which must itself be a \
                     type that carries no consent: moving the event is not a fix if it still \
                     labels the owner a contact"
                );
            }
            NotPublishedAboutTheOwner => {}
        }
    }
    Ok(())
}

/// Drops every room membership this bot still holds from an earlier run.
///
/// Presence in Matrix is not room-scoped: the Sensor picks one observed room
/// the subject shares with it to name the event's source, and a membership
/// left over from another test would make which room that is a coin toss.
async fn forget_earlier_rooms(bot: &Bot) -> Result<()> {
    for stale_room in bot.joined_rooms().await? {
        bot.leave_room(&stale_room).await?;
    }
    Ok(())
}

/// Every stored event of one type for one room, oldest first.
async fn events_for(bus: &Bus, type_name: &str, room_id: &str) -> Result<Vec<StoredMessage>> {
    bus.fetch_room_messages(STREAM, &bus_subject(type_name), room_id)
        .await
}

/// Waits until a room has produced `count` events of a type, then returns
/// them. Waiting for the count rather than for the first is what makes
/// "and nothing else was published" assertable a line later.
async fn wait_for_count(
    bus: &Bus,
    type_name: &str,
    room_id: &str,
    count: usize,
) -> Result<Vec<StoredMessage>> {
    poll_until(
        || async {
            let stored = events_for(bus, type_name, room_id).await.ok()?;
            (stored.len() >= count).then_some(stored)
        },
        &format!("waiting for {count} {type_name} event(s) from {room_id}"),
    )
    .await
}

#[tokio::test]
async fn the_owner_carries_no_consent_on_any_type_they_can_be_the_subject_of() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    // The Sensor creates the stream at startup, but the stub Gateway below
    // reads the consent subject's head before it does: on a cold bus that
    // read is a 404, so the stream is ensured here rather than depending on
    // whichever test in the binary happened to run first.
    bus.ensure_stream(STREAM, &["twalk.>"]).await?;

    // The configuration that made this a leak rather than a modelling debt:
    // the whole of WhatsApp granted, which is how a user with hundreds of
    // contacts actually decides. Before ADR 0021 this is what labelled the
    // operator's own reaction `granted` and attached their phone number to
    // it. A per-contact grant would have hidden the bug behind a `pending`.
    let (_gateway, env) = sensor_env_granting(
        &bus,
        vec![network_entry("whatsapp", "granted")],
        &[
            ("SENSOR_OWNER", OWNER),
            (
                "SENSOR_OWNER_IDENTITIES",
                &format!("@{OWNER_PHONE_GHOST}:test.twalk,@{OWNER_LID_GHOST}:test.twalk"),
            ),
        ],
    )
    .await?;
    let sensor = SensorProc::start(&env)?;

    let bridge = Bot::login("bot_alpha").await?;
    let owner_ghost = Bot::login(OWNER_LID_GHOST).await?;
    let contact = Bot::login(CONTACT_GHOST).await?;
    forget_earlier_rooms(&owner_ghost).await?;
    forget_earlier_rooms(&contact).await?;

    let room_id = make_whatsapp_portal(&bridge, "owner-is-never-a-contact").await?;
    bridge.invite(&room_id, SENSOR_USER_ID).await?;
    bridge
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    for member in [&owner_ghost, &contact] {
        bridge.invite(&room_id, member.user_id()).await?;
        member.join_room(&room_id).await?;
    }

    // The contact writes; the operator answers and then reacts to what the
    // contact wrote. The reaction's target is somebody else's message, which
    // is what makes the excerpt rule (issue #110) live here too.
    let contact_message = contact.send_message(&room_id, "on décale à 20h ?").await?;
    owner_ghost.send_message(&room_id, "oui, 20h").await?;
    let owner_reaction = owner_ghost
        .send_reaction(&room_id, &contact_message, "👍")
        .await?;
    // And the contact reacts to the operator's answer, so the inbound type
    // is exercised in the same room by somebody who really is a contact.
    let owner_message = owner_ghost
        .send_message(&room_id, "j'apporte le dessert")
        .await?;
    contact
        .send_reaction(&room_id, &owner_message, "🎉")
        .await?;

    let outbound_reactions = wait_for_count(&bus, "outbound.reaction.added", &room_id, 1).await?;
    let inbound_reactions = wait_for_count(&bus, "inbound.reaction.added", &room_id, 1).await?;
    wait_for_count(&bus, "outbound.message.sent", &room_id, 2).await?;

    // ---- the operator's own reaction --------------------------------------
    assert_eq!(
        outbound_reactions.len(),
        1,
        "exactly the operator's one reaction is published as outbound.reaction.added"
    );
    let stored = &outbound_reactions[0];
    let event = &stored.payload;
    validate_against_contract(event, "outbound.reaction.added")?;
    assert_eq!(
        attribute(event, "type"),
        "fr.linagora.twalk.outbound.reaction.added.v1"
    );
    assert_eq!(
        attribute(event, "subject"),
        OWNER,
        "the subject is the operator's Matrix ID, never the ghost the reaction arrived under"
    );
    assert_eq!(
        attribute(event, "id"),
        sha256_hex(&format!("{owner_reaction}:{room_id}")),
        "the same natural key as the inbound reaction it would otherwise have been"
    );
    assert_eq!(event["data"]["reaction"], "👍");
    assert_eq!(event["data"]["target"]["matrix_event_id"], contact_message);
    assert_eq!(
        event["data"]["target"]["excerpt"], "on décale à 20h ?",
        "the target's author is granted, so their words may travel (issue #110)"
    );

    // ---- the absence, on the envelope and on the bus ----------------------
    assert!(
        event.get("consent").is_none(),
        "the owner has no consent state, so the envelope carries no consent extension: {event}"
    );
    assert_eq!(
        stored.header("consent"),
        None,
        "and no consent NATS header either — the headers duplicate the envelope's extensions \
         for server-side filtering, so a header the envelope does not have is one the bus must \
         not carry. A consumer filtering on consent is filtering for events about a contact"
    );
    assert!(
        event["data"].get("contact").is_none(),
        "no contact object: the operator's display name and the phone number their ghost \
         localpart carries are exactly what a network-wide grant used to publish here"
    );
    let serialised = serde_json::to_string(event)?;
    assert!(
        !serialised.contains("33660469852") && !serialised.contains("115332874281144"),
        "no identity of the operator's reaches the bus: {serialised}"
    );

    // ---- and it is one event, not two ------------------------------------
    assert_eq!(
        inbound_reactions.len(),
        1,
        "the contact's reaction is the only inbound.reaction.added in this room: the operator's \
         is published once, as its own type, and not also as a contact's"
    );
    let contact_reaction = &inbound_reactions[0].payload;
    validate_against_contract(contact_reaction, "inbound.reaction.added")?;
    assert_eq!(
        attribute(contact_reaction, "subject"),
        format!("@{CONTACT_GHOST}:test.twalk"),
        "an unconfirmed identity is a contact: unknown is not the operator"
    );
    assert!(
        contract_type_allows_consent("inbound.reaction.added")?,
        "a contact's reaction still carries one"
    );
    assert_eq!(contact_reaction["consent"], "granted");

    // ---- nothing about the owner, on any type, carries a consent ---------
    for (type_name, _) in OWNER_RULES {
        for stored in events_for(&bus, type_name, &room_id).await? {
            let event = &stored.payload;
            if attribute(event, "subject") != OWNER {
                continue;
            }
            assert!(
                event.get("consent").is_none(),
                "{type_name} carries a consent extension about the owner: {event}"
            );
            assert_eq!(
                stored.header("consent"),
                None,
                "{type_name} carries a consent NATS header about the owner"
            );
        }
    }

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn the_owners_own_presence_is_not_published_and_a_contacts_still_is() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    bus.ensure_stream(STREAM, &["twalk.>"]).await?;

    let (_gateway, env) = sensor_env_granting(
        &bus,
        vec![network_entry("whatsapp", "granted")],
        &[
            ("SENSOR_OWNER", OWNER),
            (
                "SENSOR_OWNER_IDENTITIES",
                &format!("@{OWNER_PHONE_GHOST}:test.twalk,@{OWNER_LID_GHOST}:test.twalk"),
            ),
        ],
    )
    .await?;
    let sensor = SensorProc::start(&env)?;

    let bridge = Bot::login("bot_alpha").await?;
    let owner_ghost = Bot::login(OWNER_PHONE_GHOST).await?;
    let contact = Bot::login(CONTACT_GHOST).await?;
    forget_earlier_rooms(&owner_ghost).await?;
    forget_earlier_rooms(&contact).await?;

    let room_id = make_whatsapp_portal(&bridge, "owner-presence").await?;
    bridge.invite(&room_id, SENSOR_USER_ID).await?;
    bridge
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    for member in [&owner_ghost, &contact] {
        bridge.invite(&room_id, member.user_id()).await?;
        member.join_room(&room_id).await?;
    }

    // Synapse broadcasts presence *transitions* only, and either bot may be
    // online already from an earlier run: toggle both on every round so a
    // fresh transition is always emitted, and so one racing the presence
    // routing that follows a join is simply retried. The contact's event is
    // what the poll waits for — which is the point of pairing them. If the
    // Sensor had dropped presence wholesale rather than the owner's, this
    // test would time out here rather than pass quietly.
    let contact_events = poll_until(
        || async {
            for state in ["offline", "online"] {
                owner_ghost.set_presence(state).await.ok()?;
                contact.set_presence(state).await.ok()?;
                sleep(Duration::from_millis(250)).await;
            }
            let stored = events_for(&bus, "inbound.presence.updated", &room_id)
                .await
                .ok()?;
            stored
                .iter()
                .any(|message| {
                    message.payload["subject"] == format!("@{CONTACT_GHOST}:test.twalk")
                        && message.payload["data"]["presence"] == "online"
                })
                .then_some(stored)
        },
        "waiting for a contact's presence event",
    )
    .await?;

    for stored in &contact_events {
        validate_against_contract(&stored.payload, "inbound.presence.updated")?;
    }
    assert!(
        contact_events
            .iter()
            .any(|stored| attribute(&stored.payload, "subject")
                == format!("@{CONTACT_GHOST}:test.twalk")),
        "a contact's presence is still published: the rule is an exact match against the \
         identities the deployment confirmed, and an unconfirmed one is a contact, so failing \
         safe here means publishing — this event is the proof that the fix is a filter and not \
         a switch"
    );

    // The absence. Every presence event this room ever produced is the
    // contact's; the operator toggled just as often, and produced none.
    let all = events_for(&bus, "inbound.presence.updated", &room_id).await?;
    let subjects: BTreeSet<&str> = all
        .iter()
        .map(|stored| attribute(&stored.payload, "subject"))
        .collect();
    assert!(
        !subjects.contains(OWNER)
            && !subjects.contains(format!("@{OWNER_PHONE_GHOST}:test.twalk").as_str())
            && !subjects.contains(format!("@{OWNER_LID_GHOST}:test.twalk").as_str()),
        "the operator's own presence is not published at all — no type of its own and no event \
         (ADR 0021). It tells nobody anything they do not already know, and it was the \
         highest-volume event on the reference deployment. Subjects seen: {subjects:?}"
    );

    sensor.stop().await;
    Ok(())
}
