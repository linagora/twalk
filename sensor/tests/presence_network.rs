// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Issue #150 / ADR 0027: a presence event's network is the **subject's**, not
//! the first shared room's in id order.
//!
//! Presence in Matrix is not room-scoped: it arrives as an EDU about an
//! account, with no room attached. The contract's `source` is a portal room
//! URI, so the Sensor had to name a room — and it named the lexicographically
//! first joined room the subject shared with it that resolved to a bridged
//! network, then took that room's network as the subject's. For a ghost the
//! sort chose nothing, because a bridge puts only its own ghosts in only its
//! own portals and every shared room gives the same answer. For a **native
//! Matrix contact** (ADR 0009's bring-your-own-account path) it chose
//! everything.
//!
//! Why that is not a cosmetic label: **consent is looked up by `(subject,
//! network)`**. So this file stages the consequence rather than the symptom —
//! the user has *revoked* the native Matrix contact on `matrix` and *granted*
//! the whole of WhatsApp, which is how a user with hundreds of contacts
//! actually decides. Under the defect that person's presence came out
//! `network: whatsapp, consent: granted`: the consent model reading a row the
//! user never wrote, about a person they had said no to.
//!
//! Three subjects, one Sensor, and each one is a different rule of ADR 0027:
//!
//! - a **native Matrix contact** who shares a bridged portal *and* a room no
//!   bridge marked: `matrix`, sourced from the unbridged room. Sharing an
//!   unbridged room is positive evidence that the subject is a Matrix account
//!   in its own right, so this inverts `network::resolve`'s precedence — which
//!   stays right for a *room's* traffic and is wrong for a subject;
//! - a **native Matrix contact** in portals of two networks and nothing else:
//!   **no event at all**, warned and counted. One event per network was
//!   rejected on the contract (the deterministic id has no network in it, so
//!   JetStream dedup would keep one of them arbitrarily), and an arbitrary
//!   pick is the defect;
//! - a **ghost** in portals of two networks: its own localpart answers, so the
//!   portal of another network cannot relabel it. Under the defect this one
//!   was a coin toss between the two rooms' ids.
//!
//! Isolation is the suite's: `TWALK_TEST_STACK`, `TWALK_TEST_SYNAPSE_PORT`
//! and `TWALK_TEST_NATS_PORT` move the whole stack aside for a parallel
//! worktree (`sensor/tests/harness/mod.rs`).

mod harness;

use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::Result;
use harness::gateway::{contact_entry, network_entry, sensor_env_granting};
use harness::{
    ensure_stack, make_native_room, make_signal_portal, make_whatsapp_portal, poll_until,
    validate_against_contract, Bot, Bus, SensorProc, StoredMessage, SENSOR_USER_ID,
};
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::time::sleep;

const STREAM: &str = "twalk";
const PRESENCE: &str = "inbound.presence.updated";

/// A native Matrix contact — ADR 0009's bring-your-own-account path — who also
/// shares a bridged portal. Their localpart carries no network prefix, so only
/// the rooms they are in can attribute them.
const NATIVE_WITH_A_HOME_ROOM: &str = "bot_gamma";
/// A native Matrix contact whose only shared rooms are portals of two
/// different networks.
const NATIVE_IN_TWO_PORTALS: &str = "bot_delta";
/// A WhatsApp ghost, whose localpart *is* their identifier on that network.
const GHOST: &str = "whatsapp_33698765432";

/// The metrics endpoint this test binds the Sensor to. Inside this worktree's
/// range (17200–17399) so parallel stacks never collide.
const METRICS_LISTEN: &str = "127.0.0.1:17211";
const METRICS_URL: &str = "http://127.0.0.1:17211/metrics";

fn user_id(localpart: &str) -> String {
    format!("@{localpart}:test.twalk")
}

fn attribute<'a>(event: &'a Value, name: &str) -> &'a str {
    event[name]
        .as_str()
        .unwrap_or_else(|| panic!("the event has no string {name}: {event}"))
}

/// Every presence event about one subject that names one room as its source,
/// produced after `since`.
///
/// Scoped three ways, and each one earns its place. By **subject**, because the
/// test bot standing in for the bridge is a member of every room here and a
/// room-scoped query alone would pick up its presence too. By **room**, because
/// which room is named is half of what is under test. And by **time**, because
/// attribution is a function of what the Sensor currently observes: while the
/// rooms were being built it knew about some of them and not others, and a
/// presence transition arriving in that window is attributed with what was
/// known then — correctly. `since` is taken once the whole arrangement stands,
/// which is the state ADR 0027's rules are about.
async fn presence_of(
    bus: &Bus,
    room_id: &str,
    subject: &str,
    since: OffsetDateTime,
) -> Result<Vec<StoredMessage>> {
    Ok(bus
        .fetch_room_messages(STREAM, &format!("twalk.{PRESENCE}.v1"), room_id)
        .await?
        .into_iter()
        .filter(|stored| stored.payload["subject"] == subject)
        .filter(|stored| {
            stored.payload["time"]
                .as_str()
                .and_then(|time| OffsetDateTime::parse(time, &Rfc3339).ok())
                .is_some_and(|time| time >= since)
        })
        .collect())
}

/// Drops every room membership this bot still holds from an earlier run: which
/// rooms a subject shares with the Sensor *is* the input under test here, so a
/// membership left over from another test would decide the answer.
async fn forget_earlier_rooms(bot: &Bot) -> Result<()> {
    for stale_room in bot.joined_rooms().await? {
        bot.leave_room(&stale_room).await?;
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
async fn a_presence_events_network_is_the_subjects_and_never_a_sort() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    bus.ensure_stream(STREAM, &["twalk.>"]).await?;

    // The decision that makes this a consent defect rather than a label one:
    // the whole of WhatsApp granted, and this one person revoked on Matrix.
    let (_gateway, env) = sensor_env_granting(
        &bus,
        vec![
            network_entry("whatsapp", "granted"),
            contact_entry(&user_id(NATIVE_WITH_A_HOME_ROOM), "matrix", "revoked"),
        ],
        &[("SENSOR_METRICS_LISTEN", METRICS_LISTEN)],
    )
    .await?;
    let sensor = SensorProc::start(&env)?;

    let bridge = Bot::login("bot_alpha").await?;
    let native = Bot::login(NATIVE_WITH_A_HOME_ROOM).await?;
    let ambiguous = Bot::login(NATIVE_IN_TWO_PORTALS).await?;
    let ghost = Bot::login(GHOST).await?;
    for bot in [&native, &ambiguous, &ghost] {
        forget_earlier_rooms(bot).await?;
    }

    // Six rooms, because which rooms a subject shares is the input. The room
    // ids are the homeserver's and therefore unordered with respect to the
    // networks — which is exactly why the old behaviour was a coin toss and
    // why no assertion below depends on their order.
    let native_portal = make_whatsapp_portal(&bridge, "network-native-portal").await?;
    let native_home = make_native_room(&bridge, "network-native-home").await?;
    let ambiguous_whatsapp = make_whatsapp_portal(&bridge, "network-ambiguous-whatsapp").await?;
    let ambiguous_signal = make_signal_portal(&bridge, "network-ambiguous-signal").await?;
    let ghost_whatsapp = make_whatsapp_portal(&bridge, "network-ghost-whatsapp").await?;
    let ghost_signal = make_signal_portal(&bridge, "network-ghost-signal").await?;

    for (room_id, members) in [
        (&native_portal, vec![&native]),
        (&native_home, vec![&native]),
        (&ambiguous_whatsapp, vec![&ambiguous]),
        (&ambiguous_signal, vec![&ambiguous]),
        (&ghost_whatsapp, vec![&ghost]),
        (&ghost_signal, vec![&ghost]),
    ] {
        bridge.invite(room_id, SENSOR_USER_ID).await?;
        bridge
            .wait_for_membership(room_id, SENSOR_USER_ID, "join")
            .await?;
        for member in members {
            bridge.invite(room_id, member.user_id()).await?;
            member.join_room(room_id).await?;
        }
    }

    // Every assertion below is about the arrangement as a whole, so nothing is
    // asserted about the window in which it was still being built: the Sensor
    // attributes a subject from the rooms it knows about at that instant, and
    // while these six were appearing one at a time it knew about some of them.
    // That is the correct answer to a different question, and this is the
    // cutoff that stops it being mistaken for this one.
    sleep(Duration::from_secs(2)).await;
    let arrangement_stands = OffsetDateTime::now_utc();

    // Synapse broadcasts presence *transitions* only, and an account may be
    // online already from an earlier run: toggle all three on every round so a
    // fresh transition is always emitted, and so one racing the presence
    // routing that follows a join is simply retried. The two subjects that
    // *must* produce an event are what the poll waits for, so the absence
    // asserted afterwards is an absence and not a "not yet".
    poll_until(
        || async {
            for state in ["offline", "online"] {
                for bot in [&native, &ambiguous, &ghost] {
                    bot.set_presence(state).await.ok()?;
                }
                sleep(Duration::from_millis(250)).await;
            }
            let native_events = presence_of(
                &bus,
                &native_home,
                &user_id(NATIVE_WITH_A_HOME_ROOM),
                arrangement_stands,
            )
            .await
            .ok()?;
            let ghost_events =
                presence_of(&bus, &ghost_whatsapp, &user_id(GHOST), arrangement_stands)
                    .await
                    .ok()?;
            (!native_events.is_empty() && !ghost_events.is_empty()).then_some(())
        },
        "waiting for the native contact's and the ghost's presence events",
    )
    .await?;

    // ---- a native Matrix contact stays on Matrix ---------------------------
    // Sharing a room no bridge marked is positive evidence that this subject
    // is a Matrix account in its own right: a bridge never puts its ghosts in
    // one. Their membership of a portal is somebody's invitation, not an
    // identity on that network.
    let native_events = presence_of(
        &bus,
        &native_home,
        &user_id(NATIVE_WITH_A_HOME_ROOM),
        arrangement_stands,
    )
    .await?;
    assert!(
        !native_events.is_empty(),
        "the native Matrix contact's presence is published: the fix is an attribution rule, not \
         a new way to drop somebody"
    );
    for stored in &native_events {
        validate_against_contract(&stored.payload, PRESENCE)?;
        assert_eq!(
            attribute(&stored.payload, "subject"),
            user_id(NATIVE_WITH_A_HOME_ROOM)
        );
        assert_eq!(
            attribute(&stored.payload, "network"),
            "matrix",
            "a native Matrix contact's presence is `matrix`, whatever bridged portal they are \
             also a member of"
        );
        // The whole point of the ticket, asserted as a consent outcome and not
        // as a label: the user revoked this person on Matrix, and WhatsApp is
        // granted as a whole. Under the defect this event came out `granted`,
        // because the network it was labelled with was the one the first
        // shared portal happened to name.
        assert_eq!(
            attribute(&stored.payload, "consent"),
            "revoked",
            "consent is looked up by (subject, network): a decision the user took about this \
             person on Matrix governs their Matrix presence. Labelling the event `whatsapp` made \
             the network-wide WhatsApp grant answer instead — the consent model reading a row the \
             user never wrote: {}",
            stored.payload
        );
        assert_eq!(
            stored.header("consent"),
            Some("revoked"),
            "and the NATS header duplicates it, because that is what a consumer filters on"
        );
    }
    assert!(
        presence_of(
            &bus,
            &native_portal,
            &user_id(NATIVE_WITH_A_HOME_ROOM),
            arrangement_stands
        )
        .await?
        .is_empty(),
        "no event is sourced from the bridged portal this contact merely belongs to: that room \
         is where the old behaviour put it, labelled with that portal's network"
    );

    // ---- a ghost is attributed by its own localpart -----------------------
    let ghost_events =
        presence_of(&bus, &ghost_whatsapp, &user_id(GHOST), arrangement_stands).await?;
    assert!(
        !ghost_events.is_empty(),
        "the ghost's presence is published"
    );
    for stored in &ghost_events {
        validate_against_contract(&stored.payload, PRESENCE)?;
        assert_eq!(attribute(&stored.payload, "subject"), user_id(GHOST));
        assert_eq!(
            attribute(&stored.payload, "network"),
            "whatsapp",
            "`@whatsapp_<id>` is this subject's identifier on WhatsApp, so a Signal portal they \
             are also in cannot relabel them — under the defect this was decided by whichever of \
             the two room ids sorted first"
        );
        assert_eq!(attribute(&stored.payload, "consent"), "granted");
    }
    assert!(
        presence_of(&bus, &ghost_signal, &user_id(GHOST), arrangement_stands)
            .await?
            .is_empty(),
        "and nothing is sourced from the Signal portal, which is the room the sort could have \
         picked"
    );

    // ---- two networks and no way to choose is no event -------------------
    for room_id in [&ambiguous_whatsapp, &ambiguous_signal] {
        let stored = presence_of(
            &bus,
            room_id,
            &user_id(NATIVE_IN_TWO_PORTALS),
            arrangement_stands,
        )
        .await?;
        let states: BTreeSet<&str> = stored
            .iter()
            .map(|stored| attribute(&stored.payload, "network"))
            .collect();
        assert!(
            stored.is_empty(),
            "a subject the Sensor cannot attribute to one network is not published under a \
             guessed one (ADR 0027): this account is a member of a WhatsApp portal and a Signal \
             portal, is a ghost of neither, and shares no unbridged room — so naming either \
             network would make the consent model read the row of a network they may not be on. \
             Networks seen in {room_id}: {states:?}"
        );
    }

    // ---- and that absence is loud, not silent ----------------------------
    let logs = sensor.logs().await.join("\n");
    assert!(
        logs.contains("several networks") && logs.contains(&user_id(NATIVE_IN_TWO_PORTALS)),
        "the drop is warned and names the subject: an event that does not exist is invisible by \
         construction, and a real person behind this is a person missing from the bus. \
         Logs:\n{logs}"
    );
    let dropped = poll_until(
        || async {
            let body = reqwest::get(METRICS_URL).await.ok()?.text().await.ok()?;
            parse_exposition(&body)
                .into_iter()
                .find(|(name, _)| {
                    name == "twalk_sensor_events_dropped_total{reason=\"unattributable_subject\"}"
                })
                .filter(|(_, value)| *value >= 1)
        },
        "the unattributable-subject drop counter to record the dropped presence",
    )
    .await?;
    assert!(dropped.1 >= 1, "got {dropped:?}");

    sensor.stop().await;
    Ok(())
}
