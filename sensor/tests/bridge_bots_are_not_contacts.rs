// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Issue #152 / ADR 0026: a bridge's own bot is not a person, and nothing is
//! published about it.
//!
//! A bridge materialises ghosts for people and one bot for itself — mautrix's
//! `sender_localpart`, `@whatsappbot`, `@signalbot`. The Sensor had no notion
//! of the second kind, so a bot reaching a handler was resolved as a
//! **subject**: looked up in the consent cache, labelled (a network-wide
//! grant answers `granted`, the ordinary case), given a `contact` object with
//! its display name, and published. Measured on the reference deployment the
//! moment #147 made the rest visible:
//!
//! ```text
//! @whatsappbot  680        @signalbot  470        the owner  34
//! ```
//!
//! Two service accounts were 1,150 of 1,216 presence events, and still are,
//! two a minute, for as long as the deployment runs. The volume is the least
//! of it: those events travel through the *contact* machinery, so a
//! deployment's consent state can hold rows about `@whatsappbot` and the user
//! can be offered a decision about a robot.
//!
//! What this file asserts, from the bus and never from inside the Sensor:
//!
//! - **the enumeration**, first and without a stack: every contract type
//!   whose `subject` is a Matrix user ID — read from the schemas, not listed
//!   here — has a recorded answer to "what happens when that account is a
//!   bridge's own bot?", so an eleventh type cannot be added without the
//!   question being asked, exactly as `owner_is_never_a_contact.rs` does for
//!   the owner;
//! - the bot's message, its reaction and its presence produce **no event of
//!   any type**, and no byte of its identity reaches the bus;
//! - a **contact in the same room**, doing the same three things at the same
//!   time, is published as before. This is the assertion the ticket insists
//!   on being separate: the failure to avoid here is not an extra event, it
//!   is a real person vanishing from the bus in silence, and a fix that
//!   dropped everybody would satisfy every absence above;
//! - a consent decision the Gateway serves **about** a bot is refused entry
//!   to the cache and said so at `warn` — the Sensor can refuse to use such a
//!   row, and removing it from the Gateway's store is #149's half;
//! - and the suppression is **counted**, because on a healthy deployment it is
//!   the most frequent thing the Sensor does and a silence is not evidence.
//!
//! Isolation is the suite's: `TWALK_TEST_STACK`, `TWALK_TEST_SYNAPSE_PORT`
//! and `TWALK_TEST_NATS_PORT` move the whole stack aside for a parallel
//! worktree (`sensor/tests/harness/mod.rs`).

mod harness;

use std::collections::BTreeSet;

use anyhow::Result;
use harness::gateway::{contact_entry, network_entry, sensor_env_granting};
use harness::{
    contract_type_allows_consent, contract_types_about_a_person, ensure_stack,
    make_whatsapp_portal, poll_until, toggle_presence_one_account_at_a_time,
    validate_against_contract, Bot, Bus, SensorProc, StoredMessage, SENSOR_USER_ID,
};
use serde_json::Value;

const STREAM: &str = "twalk";

/// The bridges' own bots, as mautrix's `sender_localpart` names them.
const WHATSAPP_BOT: &str = "whatsappbot";
/// A ghost of the same bridge: a person the bridge stands in for, and the
/// control for every absence asserted below.
const CONTACT_GHOST: &str = "whatsapp_33612345678";

/// The metrics endpoint these tests bind the Sensor to. Inside this
/// worktree's range (17200–17399) so parallel stacks never collide.
const METRICS_LISTEN: &str = "127.0.0.1:17210";
const METRICS_URL: &str = "http://127.0.0.1:17210/metrics";

/// What the Sensor does about a bridge's own bot on one contract type whose
/// subject is a person. Every such type must have one of these, recorded here
/// by a human who looked, and the test below fails naming any type that has
/// none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BotRule {
    /// Nothing is published about a bridge bot on this type: the handler drops
    /// the observed event before anything resolves the bot as a subject, so no
    /// consent is read, no display name is fetched and no event exists.
    NotPublished,
    /// A bridge bot can never be this type's subject, because the subject is
    /// the operator's own Matrix ID — and an identity a deployment claims as
    /// both the operator and a bridge bot is refused on startup
    /// (`Config::validate_bridge_bots`) rather than silently given one of the
    /// two incompatible answers.
    NeverItsSubject,
}

use BotRule::{NeverItsSubject, NotPublished};

/// The answer for every contract type whose subject is a person.
///
/// Checked **against the contract**, not trusted: the test below reads the
/// schemas and fails if the two disagree in either direction. Adding an
/// eleventh type with a Matrix-user-ID subject therefore breaks this test
/// until somebody writes down what it does about a bridge bot — the
/// alternative is a list that goes quietly stale and an invariant that quietly
/// stops holding.
const BOT_RULES: &[(&str, BotRule)] = &[
    ("inbound.message.received", NotPublished),
    ("inbound.presence.updated", NotPublished),
    ("inbound.reaction.added", NotPublished),
    ("outbound.message.sent", NeverItsSubject),
    ("outbound.reaction.added", NeverItsSubject),
];

fn bus_subject(type_name: &str) -> String {
    format!("twalk.{type_name}.v1")
}

fn attribute<'a>(event: &'a Value, name: &str) -> &'a str {
    event[name]
        .as_str()
        .unwrap_or_else(|| panic!("the event has no string {name}: {event}"))
}

/// Every stored event of one type for one room, oldest first.
async fn events_for(bus: &Bus, type_name: &str, room_id: &str) -> Result<Vec<StoredMessage>> {
    bus.fetch_room_messages(STREAM, &bus_subject(type_name), room_id)
        .await
}

/// Drops every room membership this bot still holds from an earlier run.
///
/// Presence in Matrix is not room-scoped: the event names one observed room
/// the subject shares with the Sensor, and a membership left over from
/// another test would make which room that is a coin toss — and every
/// assertion here is scoped by room.
async fn forget_earlier_rooms(bot: &Bot) -> Result<()> {
    for stale_room in bot.joined_rooms().await? {
        bot.leave_room(&stale_room).await?;
    }
    Ok(())
}

/// The enumeration. No stack, no Sensor: this one is about the contract.
///
/// It is the acceptance criterion meant to outlive the ticket. "Nothing is
/// published about a bridge bot" is only a property of the system if
/// something knows which events can be about one, and the only answer that
/// cannot go stale is the contract's own: a type whose `subject` is a Matrix
/// user ID names an account, and a bridge bot is an account.
#[test]
fn every_type_a_bridge_bot_could_be_the_subject_of_has_an_answer() -> Result<()> {
    let from_the_contract: BTreeSet<String> =
        contract_types_about_a_person()?.into_iter().collect();
    let accounted_for: BTreeSet<String> = BOT_RULES
        .iter()
        .map(|(type_name, _)| (*type_name).to_owned())
        .collect();

    assert!(
        !from_the_contract.is_empty(),
        "the contract enumeration found no type with a Matrix-user-ID subject, which means it \
         stopped working rather than that the contract changed"
    );
    let unanswered: Vec<&String> = from_the_contract.difference(&accounted_for).collect();
    assert!(
        unanswered.is_empty(),
        "these contract types have an account as their subject and no recorded answer to \
         \"what happens when that account is a bridge's own bot?\": {unanswered:?}. Decide it, \
         record it in ADR 0026 and add it to BOT_RULES — a bridge bot is neither the owner nor \
         a contact (CONTEXT.md), and a type that has not been asked is a type where that \
         silently stops being true"
    );
    let vanished: Vec<&String> = accounted_for.difference(&from_the_contract).collect();
    assert!(
        vanished.is_empty(),
        "these types are answered here but no longer have an account as their subject in the \
         contract: {vanished:?}"
    );

    // And the two answers are checked against what the schema says the type
    // is for, so neither can be recorded against the wrong kind of type.
    for (type_name, rule) in BOT_RULES {
        match rule {
            NotPublished => assert!(
                contract_type_allows_consent(type_name)?,
                "{type_name} is recorded as a type a bridge bot must not be published on, but \
                 its schema carries no consent extension — so it is not a type about a contact \
                 and this is the wrong answer for it"
            ),
            NeverItsSubject => assert!(
                !contract_type_allows_consent(type_name)?,
                "{type_name} is recorded as having the operator as its subject, so its schema \
                 must refuse a consent extension outright (ADR 0018, ADR 0021)"
            ),
        }
    }
    Ok(())
}

/// A metric sample name with its value, parsed from the text exposition.
fn parse_exposition(body: &str) -> Vec<(String, u64)> {
    body.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (name, value) = line
                .rsplit_once(' ')
                .unwrap_or_else(|| panic!("metric line has no value: {line:?}"));
            (
                name.to_owned(),
                value
                    .parse::<u64>()
                    .unwrap_or_else(|_| panic!("metric value is not an integer: {line:?}")),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_bridge_bot_is_dropped_and_a_contact_in_the_same_room_is_published() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    // The Sensor creates the stream at startup, but the stub Gateway below
    // reads the consent subject's head before it does: on a cold bus that
    // read is a 404, so the stream is ensured here rather than depending on
    // whichever test in the binary happened to run first.
    bus.ensure_stream(STREAM, &["twalk.>"]).await?;

    let bot_id = format!("@{WHATSAPP_BOT}:test.twalk");
    let contact_id = format!("@{CONTACT_GHOST}:test.twalk");

    // The configuration that made this a leak rather than a tidiness
    // complaint: the whole of WhatsApp granted, which is how a user with
    // hundreds of contacts actually decides. That is what labelled the bot
    // `granted` and attached a `contact` object to it. And a row the Gateway
    // should never have held, about the bot itself — a deployment can have
    // acquired one, because until now the bot fed the pending-contact
    // projection like anybody else.
    let (_gateway, env) = sensor_env_granting(
        &bus,
        vec![
            network_entry("whatsapp", "granted"),
            contact_entry(&bot_id, "whatsapp", "granted"),
        ],
        &[
            ("SENSOR_BRIDGE_BOTS", &bot_id),
            ("SENSOR_METRICS_LISTEN", METRICS_LISTEN),
        ],
    )
    .await?;
    let sensor = SensorProc::start(&env)?;

    let bridge = Bot::login("bot_alpha").await?;
    let bot = Bot::login(WHATSAPP_BOT).await?;
    let contact = Bot::login(CONTACT_GHOST).await?;
    forget_earlier_rooms(&bot).await?;
    forget_earlier_rooms(&contact).await?;

    let room_id = make_whatsapp_portal(&bridge, "bridge-bot-portal").await?;
    bridge.invite(&room_id, SENSOR_USER_ID).await?;
    bridge
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    for member in [&bot, &contact] {
        bridge.invite(&room_id, member.user_id()).await?;
        member.join_room(&room_id).await?;
    }

    // Both do the same three things, in the same room, at the same time: the
    // whole point is that the two outcomes differ by identity and by nothing
    // else. The bot's message is an `m.text` deliberately — most of what a
    // bridge bot says is an `m.notice`, which has no v1 shape and would be
    // skipped for an unrelated reason, and that would prove nothing.
    let contact_message = contact.send_message(&room_id, "on décale à 20h ?").await?;
    bot.send_message(&room_id, "Connexion WhatsApp rétablie")
        .await?;
    bot.send_reaction(&room_id, &contact_message, "✅").await?;
    let contact_message_two = contact.send_message(&room_id, "et le dessert ?").await?;
    contact
        .send_reaction(&room_id, &contact_message_two, "🎉")
        .await?;

    // Wait for the contact's own traffic, which is what tells us the Sensor
    // has processed this room at all. Waiting for a count rather than for the
    // first event is what makes "and nothing else was published" assertable a
    // line later.
    let messages = poll_until(
        || async {
            let stored = events_for(&bus, "inbound.message.received", &room_id)
                .await
                .ok()?;
            (stored.len() >= 2).then_some(stored)
        },
        "waiting for the contact's two messages",
    )
    .await?;
    let reactions = poll_until(
        || async {
            let stored = events_for(&bus, "inbound.reaction.added", &room_id)
                .await
                .ok()?;
            (!stored.is_empty()).then_some(stored)
        },
        "waiting for the contact's reaction",
    )
    .await?;

    // ---- the contact is published, exactly as before ----------------------
    assert_eq!(
        messages.len(),
        2,
        "the contact's two messages, and only those: the bot wrote in the same room and \
         produced none"
    );
    for stored in &messages {
        validate_against_contract(&stored.payload, "inbound.message.received")?;
        assert_eq!(attribute(&stored.payload, "subject"), contact_id);
        assert_eq!(stored.payload["consent"], "granted");
    }
    assert_eq!(
        reactions.len(),
        1,
        "the contact's one reaction, and only that: the bot reacted to the contact's message in \
         the same room and produced none"
    );
    validate_against_contract(&reactions[0].payload, "inbound.reaction.added")?;
    assert_eq!(attribute(&reactions[0].payload, "subject"), contact_id);

    // Synapse broadcasts presence *transitions* only, and either account may
    // be online already from an earlier run: toggle both on every round so a
    // fresh transition is always emitted, and so one racing the presence
    // routing that follows a join is simply retried. The *contact's* event is
    // what the poll waits for — which is the point of pairing them. If the
    // Sensor had dropped presence wholesale rather than the bot's, this test
    // would time out here rather than pass quietly.
    //
    // The two accounts are toggled **one at a time**, and that is load-bearing
    // rather than tidy (issue #197): toggling them in the same instant made the
    // homeserver report the bot's transitions and swallow the contact's
    // `online` one, every round, so this test timed out having proved exactly
    // the half that is not the point. `toggle_presence_one_account_at_a_time`
    // carries the mechanism. What has not changed is that both accounts do the
    // same thing in the same room in the same round — their messages and
    // reactions are still simultaneous above, which is the controlled
    // comparison ADR 0026 asks for; only the presence transitions are
    // serialised, because presence is the one thing here that is not
    // room-scoped and is delivered as a current state rather than as an event.
    let presence = poll_until(
        || async {
            toggle_presence_one_account_at_a_time(&[&bot, &contact])
                .await
                .ok()?;
            let stored = events_for(&bus, "inbound.presence.updated", &room_id)
                .await
                .ok()?;
            stored
                .iter()
                .any(|message| {
                    message.payload["subject"] == contact_id.as_str()
                        && message.payload["data"]["presence"] == "online"
                })
                .then_some(stored)
        },
        "waiting for a contact's presence event",
    )
    .await?;
    for stored in &presence {
        validate_against_contract(&stored.payload, "inbound.presence.updated")?;
    }

    // ---- and the bot is nowhere, on any type -----------------------------
    for (type_name, _) in BOT_RULES {
        let stored = events_for(&bus, type_name, &room_id).await?;
        let subjects: BTreeSet<&str> = stored
            .iter()
            .map(|stored| attribute(&stored.payload, "subject"))
            .collect();
        assert!(
            !subjects.contains(bot_id.as_str()),
            "{type_name} carries an event about a bridge's own bot. It is the appservice's own \
             service identity: it creates portals and puppets ghosts, and publishing that a \
             robot is online — or what it wrote — tells nobody anything, while routing it \
             through the contact machinery lets a deployment's consent state acquire a row \
             about it (ADR 0026). Subjects seen: {subjects:?}"
        );
        for stored in &stored {
            let serialised = serde_json::to_string(&stored.payload)?;
            assert!(
                !serialised.contains(WHATSAPP_BOT),
                "no byte of the bot's identity reaches the bus — not as a subject, not as a \
                 display name, and not as a quoted author's excerpt inside somebody else's \
                 event: {serialised}"
            );
        }
    }

    // ---- the refusals are visible, not silent ----------------------------
    let logs = sensor.logs().await.join("\n");
    assert!(
        logs.contains("refusing a consent decision about a bridge's own bot"),
        "a decision the Gateway served about the bot is refused entry to the cache and said so \
         at warn: the row should not exist at the Gateway either, and only the Gateway can \
         remove it (#149). Logs:\n{logs}"
    );
    let dropped = poll_until(
        || async {
            let body = reqwest::get(METRICS_URL).await.ok()?.text().await.ok()?;
            parse_exposition(&body)
                .into_iter()
                .find(|(name, _)| {
                    name == "twalk_sensor_events_dropped_total{reason=\"bridge_bot\"}"
                })
                .filter(|(_, value)| *value >= 3)
        },
        "the bridge-bot drop counter to reach the bot's three events",
    )
    .await?;
    assert!(
        dropped.1 >= 3,
        "the message, the reaction and at least one presence transition are counted: a \
         suppression nobody can measure reads the same as a variable nobody set, which is how \
         this defect would survive the fix. Got {dropped:?}"
    );

    sensor.stop().await;
    Ok(())
}
