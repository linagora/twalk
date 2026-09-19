//! The portal register at the process boundary (ticket #105, ADR 0024): the
//! real Gateway binary, a real Synapse, real portal rooms, and every
//! assertion made against the homeserver's own state rather than against the
//! Gateway's answer about it.
//!
//! What these tests are for is one sentence from the ticket: *a fix that
//! invites the Sensor into the rooms that exist when a network is connected
//! would be correct for an hour and wrong by the evening.* A bridge builds a
//! portal room when a conversation becomes active — eighteen of them appeared
//! on the reference deployment between 04:54 and 13:24 — so the assertion
//! that matters is not that a room the Gateway started with is handled, it is
//! that a room created **after** the Gateway was running is offered too.
//! That is `a_portal_built_after_the_gateway_started_is_offered_too`.
//!
//! The second thing asserted here is an absence, in the project's habit: the
//! Sensor is in **no** conversation until the user says so, and choosing one
//! conversation leaves the others alone. Those eighteen rooms held about
//! 1,300 memberships; a mechanism that put the Sensor in all of them by
//! default would be a worse defect than the one it fixed (#122).
//!
//! The real Sensor process is not here — it is in `tests/deployment.rs`,
//! where the compose stack runs one and a message in a newly observed portal
//! is asserted on the bus. Here the Sensor's account is a test session that
//! joins when invited, which is exactly what the real one does.

mod harness;

use anyhow::{Context, Result};
use harness::{
    companion_build, ensure_stack, gateway_env_with_portals, parse_exposition,
    signed_in_device_token, GatewayProc, MatrixUser, OWNER_LOCALPART, PORTALS_APPSERVICE_SENDER,
    PORTAL_BRIDGE_ID, PORTAL_SENDER_BRIDGE_ID, PORTAL_TOKENLESS_BRIDGE_ID, SENSOR_USER_ID,
};
use serde_json::{json, Value};

/// A Gateway with a portal register, the account playing the bridge bot, the
/// owner's session and the Sensor's.
struct Running {
    gateway: GatewayProc,
    base: String,
    cookie: String,
    /// The account that creates the portal rooms and is in every one of them,
    /// exactly as a bridge bot is. Its access token is what the Gateway holds
    /// as that bridge's appservice token.
    bridge_bot: MatrixUser,
    owner: MatrixUser,
    sensor: MatrixUser,
}

impl Running {
    async fn start(test_name: &str) -> Result<Self> {
        ensure_stack().await?;
        // The bridge bot, acting through the test stack's **appservice**
        // token, exactly as a mautrix bot does. Not an ordinary account: the
        // register names this bot in `?user_id=`, and Synapse honours that
        // parameter only for an appservice token, so an ordinary session
        // cannot exercise it at all (#171).
        let bridge_bot = MatrixUser::as_fresh_appservice("portalbot").await?;
        let owner = MatrixUser::login(OWNER_LOCALPART).await?;
        let sensor = MatrixUser::login("sensor").await?;
        let static_dir = companion_build(test_name)?;
        let gateway = GatewayProc::start(&gateway_env_with_portals(&static_dir, &bridge_bot))?;
        let base = gateway.base_url().await?;
        let cookie = signed_in_device_token(&base).await?;
        Ok(Self {
            gateway,
            base,
            cookie,
            bridge_bot,
            owner,
            sensor,
        })
    }

    /// A portal room, as the bridge builds one when a conversation becomes
    /// active: the bot creates it, marks it, and the two correspondents join.
    async fn build_portal(&self, name: &str) -> Result<String> {
        let room_id = self.bridge_bot.make_portal(name, "whatsapp").await?;
        for member in [&self.owner] {
            self.bridge_bot.invite(&room_id, &member.user_id).await?;
            member.join(&room_id).await?;
        }
        Ok(room_id)
    }

    async fn register(&self) -> Result<Value> {
        let body: Value = reqwest::Client::new()
            .get(format!("{}/api/portals", self.base))
            .header(
                reqwest::header::COOKIE,
                format!("twalk_device={}", self.cookie),
            )
            .send()
            .await
            .context("failed to read the portal register")?
            .error_for_status()
            .context("the Gateway refused to serve the portal register")?
            .json()
            .await
            .context("the register is not JSON")?;
        Ok(body)
    }

    async fn set_observation(&self, rooms: &[&str], observed: bool) -> Result<Value> {
        let body: Value = reqwest::Client::new()
            .post(format!("{}/api/portals/observation", self.base))
            .header(
                reqwest::header::COOKIE,
                format!("twalk_device={}", self.cookie),
            )
            .json(&json!({ "rooms": rooms, "observed": observed }))
            .send()
            .await
            .context("failed to set an observation")?
            .error_for_status()
            .context("the Gateway refused the observation")?
            .json()
            .await
            .context("the observation answer is not JSON")?;
        Ok(body)
    }

    /// The Sensor's membership as the **homeserver** answers it, which is the
    /// only claim worth asserting: the Gateway's own answer about it is what
    /// is under test.
    async fn sensor_membership(&self, room_id: &str) -> Result<Option<String>> {
        self.bridge_bot.membership(room_id, SENSOR_USER_ID).await
    }

    async fn metrics(&self) -> Result<Vec<(String, u64)>> {
        let body = reqwest::get(format!("{}/metrics", self.base))
            .await?
            .text()
            .await?;
        Ok(parse_exposition(&body))
    }

    async fn stop(self) {
        self.gateway.stop().await;
    }
}

/// One portal in the register's `portals` array, by room id.
fn portal<'a>(register: &'a Value, room_id: &str) -> Option<&'a Value> {
    register["portals"]
        .as_array()?
        .iter()
        .find(|portal| portal["room_id"] == room_id)
}

fn observation_of(register: &Value, room_id: &str) -> Option<String> {
    portal(register, room_id)?["observation"]
        .as_str()
        .map(str::to_owned)
}

fn outcome_for(answer: &Value, room_id: &str) -> Option<String> {
    answer["outcomes"]
        .as_array()?
        .iter()
        .find(|outcome| outcome["room_id"] == room_id)?["status"]
        .as_str()
        .map(str::to_owned)
}

fn sample(metrics: &[(String, u64)], name: &str) -> Option<u64> {
    metrics
        .iter()
        .find(|(series, _)| series == name)
        .map(|(_, value)| *value)
}

// ---------------------------------------------------------------------------

/// The register finds the conversations a bridge has built, the Sensor is
/// outside every one of them, and the deployment can say so — as a list, as
/// a summary and as a gauge.
#[tokio::test]
async fn every_conversation_is_offered_and_the_sensor_is_inside_none_of_them() -> Result<()> {
    let running = Running::start("portals-offered").await?;
    let maria = running.build_portal("maria (WA)").await?;
    let chess = running.build_portal("Échecs en Yvelines").await?;

    let register = running.register().await?;
    assert_eq!(
        register["summary"]["total"], 2,
        "both conversations the bridge built are in the register: {register}"
    );
    assert_eq!(
        register["summary"]["observing"], 0,
        "and the Sensor is inside none of them, which is the default and not an accident: \
         {register}"
    );
    assert_eq!(register["summary"]["absent"], 2, "{register}");
    assert_eq!(observation_of(&register, &maria).as_deref(), Some("absent"));
    assert_eq!(observation_of(&register, &chess).as_deref(), Some("absent"));

    // Asked of the homeserver rather than of the Gateway: the Sensor really
    // is in neither room.
    assert_eq!(running.sensor_membership(&maria).await?, None);
    assert_eq!(running.sensor_membership(&chess).await?, None);

    // The conversation is nameable and countable, which is what makes the
    // decision in #143 a real one rather than a row of room ids. The bridge
    // bot is not a member of the conversation, and neither is the Sensor.
    let maria_portal = portal(&register, &maria).context("maria's portal is in the register")?;
    assert_eq!(maria_portal["name"], "maria (WA)", "{register}");
    assert_eq!(maria_portal["network"], "whatsapp", "{register}");
    assert_eq!(maria_portal["bridge_id"], PORTAL_BRIDGE_ID, "{register}");
    assert_eq!(
        maria_portal["members"], 1,
        "the member count is the conversation's, not the room's: the bridge bot is in every \
         portal and is nobody's correspondent — {register}"
    );

    // A bridge the Gateway has no appservice token for is reported, never
    // skipped: a total that quietly covered fewer bridges than the user has
    // connected would reproduce this very defect in another form.
    let bridges = register["bridges"]
        .as_array()
        .context("every configured bridge is in the answer")?;
    assert_eq!(bridges.len(), 3, "{register}");
    let tokenless = bridges
        .iter()
        .find(|bridge| bridge["bridge_id"] == PORTAL_TOKENLESS_BRIDGE_ID)
        .context("the bridge with no token is named")?;
    assert_eq!(tokenless["readable"], false, "{register}");
    assert!(
        tokenless["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("GATEWAY_BRIDGE_MAUTRIX_TOKENLESS_AS_TOKEN"),
        "and the reason names the variable that would open it: {register}"
    );

    // And the bridge that *was* read says which account read it, which is the
    // half of #171 that is not the fix. `asked_as` is the configured bot and
    // not the appservice's sender, and it is joined to the two portals it
    // built — so a zero here would have a name attached to it.
    let working = bridges
        .iter()
        .find(|bridge| bridge["bridge_id"] == PORTAL_BRIDGE_ID)
        .context("the readable bridge is named")?;
    assert_eq!(working["readable"], true, "{register}");
    assert_eq!(
        working["asked_as"], running.bridge_bot.user_id,
        "the register acts as the bridge bot the operator configured, and says so: {register}"
    );
    assert_eq!(
        working["joined_rooms"], 2,
        "and says how many rooms that account is in at all: {register}"
    );

    // And the same fact on /metrics, so a deployment can say it without a
    // browser. This is the criterion the ticket cares most about: the number
    // of conversations the Sensor is *not* in used to be knowable only by
    // asking the homeserver by hand with an appservice token.
    let metrics = running.metrics().await?;
    assert_eq!(
        sample(
            &metrics,
            "twalk_companion_gateway_portal_rooms{observation=\"absent\"}"
        ),
        Some(2),
        "the gauge states how many conversations the Sensor is outside: {metrics:?}"
    );
    assert_eq!(
        sample(
            &metrics,
            "twalk_companion_gateway_portal_rooms{observation=\"observing\"}"
        ),
        Some(0),
        "and the series exists at zero rather than being absent: {metrics:?}"
    );
    assert_eq!(
        sample(
            &metrics,
            "twalk_companion_gateway_portal_bridges_unreadable"
        ),
        Some(1),
        "and the gauge says how many bridges the first number does not cover: {metrics:?}"
    );

    running.stop().await;
    Ok(())
}

/// The register acts as the **bridge bot**, and a register that did not could
/// not see a single conversation (ticket #171).
///
/// This is the test #105's suite could not contain. That suite handed the
/// register an ordinary account's access token, and for an ordinary token
/// Synapse ignores `?user_id=` — `_get_appservice_user` returns early when the
/// token belongs to no appservice, and normal token auth then acts as the
/// token's own user. So the parameter that decides which account reads a
/// bridge's rooms was unobservable in both directions: the register never sent
/// it, every assertion passed, and the reference deployment reported
/// `observing=0 invited=0 absent=0 unreadable_bridges=0` against 32 portal
/// rooms.
///
/// Here the credential is the stack's real appservice token, whose
/// `sender_localpart` is an account in no rooms, and the bot is a different
/// account named in configuration. Two facts are asserted, and the first fails
/// on the code as it was:
///
/// 1. the conversations are found — which they are only if `?user_id=` names
///    the bot;
/// 2. and the bridge configured **without** a bot named — #171's own
///    configuration — answers `readable: true` with `joined_rooms: 0` and
///    names the account it asked as, so that a zero can be told apart from the
///    truthful zero of a deployment whose bridges have built nothing yet.
/// Issue #253 / ADR 0029: a conversation outlives its room, but the room stays
/// the key — and the tombstone chain is what reconciles the two.
///
/// A room the Sensor was observing is replaced. The register follows
/// `m.room.tombstone` to the successor and lists the conversation **once**,
/// there, saying where it came from. The Sensor is not in the successor, and
/// that is the fourth observation state, `moved`: distinct from a portal
/// nobody chose, so the deployment can say *"this conversation moved and the
/// Sensor is not in the new room"* instead of a silent zero — which is #105's
/// failure happening to a decision already on record. A room the Sensor was
/// never in moves too, and stays what it was: `absent`.
#[tokio::test]
async fn a_conversation_whose_room_was_replaced_appears_once_at_its_successor() -> Result<()> {
    let it = Running::start("portals-moved").await?;

    let chosen = it.build_portal("Maria (moves)").await?;
    let ignored = it.build_portal("Chess club (moves)").await?;
    // The user chose one of them: the Gateway invites the Sensor, which joins.
    it.set_observation(&[&chosen], true).await?;
    it.sensor.join(&chosen).await?;
    let before = it.register().await?;
    assert_eq!(
        observation_of(&before, &chosen).as_deref(),
        Some("observing")
    );
    assert_eq!(observation_of(&before, &ignored).as_deref(), Some("absent"));

    // Both rooms are replaced — a supergroup migration, a room upgrade. The
    // bridge re-marks the successors and pulls the conversation's members
    // back in; nobody invites the Sensor anywhere.
    let mut successors = Vec::new();
    for old in [&chosen, &ignored] {
        let new = it.bridge_bot.upgrade_room(old).await?;
        let (state_key, marker) = harness_marker(&it.bridge_bot.user_id, "whatsapp", "moved");
        it.bridge_bot
            .send_state_event(&new, "m.bridge", &state_key, marker)
            .await?;
        it.bridge_bot.invite(&new, &it.owner.user_id).await?;
        it.owner.join(&new).await?;
        successors.push(new);
    }
    let (chosen_successor, ignored_successor) = (&successors[0], &successors[1]);

    let register = it.register().await?;
    // Once, at the successor: two rows would make "the Sensor is outside 30
    // of your 32 conversations" false by one at every migration.
    assert!(
        portal(&register, &chosen).is_none(),
        "the dead room is not a row: {register}"
    );
    assert!(
        portal(&register, &ignored).is_none(),
        "the dead room is not a row: {register}"
    );
    let moved = portal(&register, chosen_successor).context("the successor is listed")?;
    assert_eq!(moved["observation"], json!("moved"), "{moved}");
    assert_eq!(moved["moved_from"], json!(chosen), "{moved}");
    assert_eq!(
        moved["members"],
        json!(1),
        "the owner, and nobody else: {moved}"
    );
    let untouched = portal(&register, ignored_successor).context("the successor is listed")?;
    assert_eq!(
        untouched["observation"],
        json!("absent"),
        "a conversation the Sensor was never in moves and stays absent: {untouched}"
    );
    assert_eq!(untouched["moved_from"], json!(ignored), "{untouched}");
    assert_eq!(register["summary"]["moved"], json!(1), "{register}");

    // The fact is a number too.
    let metrics = it.metrics().await?;
    assert_eq!(
        sample(
            &metrics,
            "twalk_companion_gateway_portal_rooms{observation=\"moved\"}"
        ),
        Some(1),
        "{metrics:?}"
    );

    // The homeserver agrees about where the Sensor is, which is the only
    // thing the register was allowed to derive `moved` from.
    assert_eq!(
        it.sensor_membership(&chosen).await?.as_deref(),
        Some("join")
    );
    assert_eq!(it.sensor_membership(chosen_successor).await?, None);

    it.stop().await;
    Ok(())
}

/// A chain of two replacements resolves to the last room, and a successor the
/// bridge bot cannot read is still named — a tombstone that points into the
/// dark is a fact, not a reason to fold the conversation into nothing.
#[tokio::test]
async fn a_chain_of_replacements_ends_at_the_last_room_and_an_unreadable_one_is_named() -> Result<()>
{
    let it = Running::start("portals-moved-chain").await?;

    let first = it.build_portal("Maria (chain)").await?;
    it.set_observation(&[&first], true).await?;
    it.sensor.join(&first).await?;

    // First → second → third, each re-marked as the bridge would.
    let second = it.bridge_bot.upgrade_room(&first).await?;
    let (key, marker) = harness_marker(&it.bridge_bot.user_id, "whatsapp", "chain-2");
    it.bridge_bot
        .send_state_event(&second, "m.bridge", &key, marker)
        .await?;
    let third = it.bridge_bot.upgrade_room(&second).await?;
    let (key, marker) = harness_marker(&it.bridge_bot.user_id, "whatsapp", "chain-3");
    it.bridge_bot
        .send_state_event(&third, "m.bridge", &key, marker)
        .await?;
    it.bridge_bot.invite(&third, &it.owner.user_id).await?;
    it.owner.join(&third).await?;

    let register = it.register().await?;
    assert!(portal(&register, &first).is_none(), "{register}");
    assert!(portal(&register, &second).is_none(), "{register}");
    let last = portal(&register, &third).context("the last room is the row")?;
    assert_eq!(last["observation"], json!("moved"), "{last}");
    assert_eq!(
        last["moved_from"],
        json!(second),
        "the immediate predecessor: {last}"
    );
    assert_eq!(register["summary"]["moved"], json!(1), "{register}");

    // A successor the bot is not in: the bot leaves it, so the register can
    // read the tombstone but not the room it points at.
    let dark = it.build_portal("Maria (dark)").await?;
    it.set_observation(&[&dark], true).await?;
    it.sensor.join(&dark).await?;
    let beyond = it.bridge_bot.upgrade_room(&dark).await?;
    it.bridge_bot.leave(&beyond).await?;

    let register = it.register().await?;
    assert!(portal(&register, &dark).is_none(), "{register}");
    let named = portal(&register, &beyond).context("the unreadable successor is still a row")?;
    assert_eq!(named["observation"], json!("moved"), "{named}");
    assert_eq!(named["moved_from"], json!(dark), "{named}");
    assert!(
        named["unreadable"]
            .as_str()
            .is_some_and(|why| !why.is_empty()),
        "the row says the successor could not be read: {named}"
    );

    it.stop().await;
    Ok(())
}

/// The `m.bridge` marker a bridge writes into a successor it re-creates:
/// the same shape `make_portal` writes, for a room that already exists.
fn harness_marker(bot: &str, protocol_id: &str, name: &str) -> (String, Value) {
    (
        format!("test.twalk/{protocol_id}"),
        json!({
            "bridgebot": bot,
            "protocol": { "id": protocol_id, "displayname": protocol_id },
            "channel": { "id": format!("{protocol_id}-{name}"), "displayname": name },
        }),
    )
}

#[tokio::test]
async fn the_register_reads_as_the_bridge_bot_and_not_as_the_appservices_sender() -> Result<()> {
    let running = Running::start("portals-asks-as-the-bot").await?;
    let chess = running.build_portal("Échecs en Yvelines").await?;

    // The premise, established against the homeserver rather than assumed: the
    // bot is in the conversation and the appservice's sender is in nothing. If
    // this ever stops holding, the test below stops meaning anything.
    assert_eq!(
        running
            .bridge_bot
            .membership(&chess, &running.bridge_bot.user_id)
            .await?
            .as_deref(),
        Some("join"),
        "the bridge bot is in the portal it built"
    );
    assert_ne!(
        running.bridge_bot.user_id, PORTALS_APPSERVICE_SENDER,
        "the asker and the account in the rooms must be different accounts, or this test proves          nothing"
    );

    let register = running.register().await?;

    // 1. Found — which takes `?user_id=`. On the code #105 shipped this is
    //    zero, because the appservice's sender is joined to nothing.
    assert_eq!(
        register["summary"]["total"], 1,
        "the register found the conversation, which it can only do by asking as the bridge bot:          {register}"
    );
    assert_eq!(observation_of(&register, &chess).as_deref(), Some("absent"));

    let bridges = register["bridges"]
        .as_array()
        .context("every configured bridge is in the answer")?;

    // 2. And the bridge with no bot named is #171 reproduced, reported rather
    //    than silent. Not `unreadable`: nothing failed, and the Gateway cannot
    //    know whether this network has simply built no conversation yet. What
    //    it can do — and could not before — is name the account and the count.
    let sender_only = bridges
        .iter()
        .find(|bridge| bridge["bridge_id"] == PORTAL_SENDER_BRIDGE_ID)
        .context("the bridge configured with no bot is named")?;
    assert_eq!(
        sender_only["readable"], true,
        "nothing failed: the token worked and the homeserver answered — {register}"
    );
    assert_eq!(
        sender_only["asked_as"], PORTALS_APPSERVICE_SENDER,
        "and the answer names the account it fell back to, which is the appservice's own sender          and not any bridge's bot: {register}"
    );
    assert_eq!(
        sender_only["joined_rooms"], 0,
        "which is in no rooms at all — the fact that makes this zero attributable: {register}"
    );
    assert!(
        !register["portals"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .any(|portal| portal["bridge_id"] == PORTAL_SENDER_BRIDGE_ID),
        "and it contributes no conversation, so the list is not quietly wrong either: {register}"
    );

    running.stop().await;
    Ok(())
}

/// Observing a conversation is done **as the bridge bot** too, not only read
/// as it (ticket #171).
///
/// The read and the write are two different calls and only one of them was
/// wrong on the reference deployment — because the read failed first, nothing
/// ever reached the write. It would have failed too: the appservice's sender
/// has no power level in a room it is not in, so the invitation would have
/// been `M_FORBIDDEN` for a conversation the Gateway had somehow found. This
/// asserts the whole path against the homeserver: the Sensor's membership,
/// which no answer of the Gateway's can fake.
#[tokio::test]
async fn observing_a_conversation_acts_as_the_bridge_bot_as_well() -> Result<()> {
    let running = Running::start("portals-invites-as-the-bot").await?;
    let chess = running.build_portal("Échecs en Yvelines").await?;

    let answer = running.set_observation(&[&chess], true).await?;
    assert_eq!(
        outcome_for(&answer, &chess).as_deref(),
        Some("invited"),
        "the bot can invite in the room it created: {answer}"
    );
    assert_eq!(
        running.sensor_membership(&chess).await?.as_deref(),
        Some("invite"),
        "and the homeserver — not the Gateway's own answer — is where that is true"
    );

    running.stop().await;
    Ok(())
}

/// The whole point of the ticket: the bridge builds portals **lazily**, as
/// conversations become active, so a mechanism that only handles the rooms
/// that existed at connection time is wrong by the evening. A room created
/// after the Gateway started is offered, and can be observed, exactly like
/// one that was there first.
#[tokio::test]
async fn a_portal_built_after_the_gateway_started_is_offered_too() -> Result<()> {
    let running = Running::start("portals-later").await?;
    let morning = running.build_portal("maria (WA)").await?;

    let before = running.register().await?;
    assert_eq!(before["summary"]["total"], 1, "{before}");

    // 13:24, and somebody writes in a conversation that has never been
    // bridged before. Nothing was reconnected and nothing was restarted.
    let afternoon = running.build_portal("RAG I Infos").await?;

    let after = running.register().await?;
    assert_eq!(
        after["summary"]["total"], 2,
        "a portal the bridge built while the Gateway was running is in the register: {after}"
    );
    assert_eq!(
        observation_of(&after, &afternoon).as_deref(),
        Some("absent"),
        "offered for observation, and observed by nobody yet: {after}"
    );

    // And it can be observed, which is what makes "offered" mean something.
    let answer = running.set_observation(&[&afternoon], true).await?;
    assert_eq!(outcome_for(&answer, &afternoon).as_deref(), Some("invited"));
    assert_eq!(
        running.sensor_membership(&afternoon).await?.as_deref(),
        Some("invite"),
        "the homeserver holds the invitation, whatever the Gateway says about it"
    );
    // The Sensor joins on its own — here, a test session doing what the real
    // one does when the inviter is an allowed one.
    running.sensor.join(&afternoon).await?;
    let observed = running.register().await?;
    assert_eq!(
        observation_of(&observed, &afternoon).as_deref(),
        Some("observing"),
        "{observed}"
    );
    assert_eq!(
        observation_of(&observed, &morning).as_deref(),
        Some("absent"),
        "and the conversation the user did not choose is still untouched: {observed}"
    );
    assert_eq!(
        running.sensor_membership(&morning).await?,
        None,
        "which the homeserver confirms"
    );

    running.stop().await;
    Ok(())
}

/// Observing is a decision the user can take back. Stopping takes the Sensor
/// out of the room, so the conversation stops reaching the bus — the inverse
/// of starting, through the same credential and the same mechanism.
#[tokio::test]
async fn a_conversation_can_stop_being_observed() -> Result<()> {
    let running = Running::start("portals-stop").await?;
    let room = running
        .build_portal("Association « Vivre à Chapet »")
        .await?;

    running.set_observation(&[&room], true).await?;
    running.sensor.join(&room).await?;
    assert!(running
        .bridge_bot
        .joined_members(&room)
        .await?
        .contains(&SENSOR_USER_ID.to_owned()));

    let answer = running.set_observation(&[&room], false).await?;
    assert_eq!(outcome_for(&answer, &room).as_deref(), Some("removed"));
    assert_eq!(
        running.sensor_membership(&room).await?.as_deref(),
        Some("leave"),
        "the homeserver has the Sensor out of the room, which is what makes it stop observing"
    );
    assert!(
        !running
            .bridge_bot
            .joined_members(&room)
            .await?
            .contains(&SENSOR_USER_ID.to_owned()),
        "and it is not among the room's members"
    );
    let register = running.register().await?;
    assert_eq!(observation_of(&register, &room).as_deref(), Some("absent"));

    // Asking again is not an error, in either direction.
    let again = running.set_observation(&[&room], false).await?;
    assert_eq!(outcome_for(&again, &room).as_deref(), Some("not_observed"));
    let back = running.set_observation(&[&room], true).await?;
    assert_eq!(outcome_for(&back, &room).as_deref(), Some("invited"));
    let twice = running.set_observation(&[&room], true).await?;
    assert_eq!(
        outcome_for(&twice, &room).as_deref(),
        Some("already_observed")
    );

    running.stop().await;
    Ok(())
}

/// The appservice credential's reach is its own bot's portal rooms, and a
/// room id in a request is never a reason to widen it. A room the bot is in
/// that no bridge marked is not a conversation either: it is the bridge's
/// management room, and it is neither offered nor entered.
#[tokio::test]
async fn nothing_but_a_portal_is_offered_and_nothing_else_is_entered() -> Result<()> {
    let running = Running::start("portals-scope").await?;
    let portal_room = running.build_portal("Linagora : Team Clean").await?;

    // The bridge's management room: the bot and the user, and no marker.
    let management = running.bridge_bot.create_room("WhatsApp bridge").await?;
    running
        .bridge_bot
        .invite(&management, &running.owner.user_id)
        .await?;
    running.owner.join(&management).await?;

    // A room of the user's own that the bot is not in at all.
    let private = running.owner.create_room("notes to self").await?;

    let register = running.register().await?;
    assert_eq!(
        register["summary"]["total"], 1,
        "only the room a bridge marked is a conversation: {register}"
    );
    assert!(portal(&register, &portal_room).is_some(), "{register}");
    assert!(
        portal(&register, &management).is_none(),
        "the bridge's management room is not a conversation: {register}"
    );

    let answer = running
        .set_observation(&[&management, &private], true)
        .await?;
    assert_eq!(
        outcome_for(&answer, &management).as_deref(),
        Some("unknown_portal")
    );
    assert_eq!(
        outcome_for(&answer, &private).as_deref(),
        Some("unknown_portal")
    );
    assert_eq!(
        running.sensor_membership(&management).await?,
        None,
        "and nothing was attempted with the appservice credential"
    );
    assert_eq!(
        running.owner.membership(&private, SENSOR_USER_ID).await?,
        None
    );

    running.stop().await;
    Ok(())
}
