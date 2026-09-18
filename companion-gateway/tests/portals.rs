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
    signed_in_device_token, GatewayProc, MatrixUser, OWNER_LOCALPART, PORTAL_BRIDGE_ID,
    PORTAL_TOKENLESS_BRIDGE_ID, SENSOR_USER_ID,
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
        let bridge_bot = MatrixUser::register_fresh("portalbot").await?;
        let owner = MatrixUser::login(OWNER_LOCALPART).await?;
        let sensor = MatrixUser::login("sensor").await?;
        let static_dir = companion_build(test_name)?;
        let gateway = GatewayProc::start(&gateway_env_with_portals(
            &static_dir,
            bridge_bot.matrix_access_token(),
        ))?;
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
    assert_eq!(bridges.len(), 2, "{register}");
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
