// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Ticket 02, lifecycle: the Sensor joins rooms on invitation from an
//! allowed inviter, ignores everyone else, and stops observing a room
//! after being removed from it.

mod harness;

use anyhow::Result;
use harness::{ensure_stack, sensor_env, Bot, SensorProc, SENSOR_USER_ID};

#[tokio::test]
async fn joins_when_invited_by_an_allowed_inviter_and_ignores_others() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;
    let beta = Bot::login("bot_beta").await?;

    let portal = alpha.create_room("portal-allowed", false).await?;
    alpha.invite(&portal, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&portal, SENSOR_USER_ID, "join")
        .await?;

    // An invitation from anyone else is ignored: still "invite" after the
    // Sensor has had ample time to act on it.
    let not_a_bridge = beta.create_room("not-a-bridge", false).await?;
    beta.invite(&not_a_bridge, SENSOR_USER_ID).await?;
    beta.wait_for_membership(&not_a_bridge, SENSOR_USER_ID, "invite")
        .await?;
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    assert_eq!(
        beta.get_membership(&not_a_bridge, SENSOR_USER_ID).await?,
        "invite",
        "the Sensor must not join a room on a disallowed invitation"
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn leaves_the_room_after_being_removed() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let portal = alpha.create_room("portal-removed", false).await?;
    alpha.invite(&portal, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&portal, SENSOR_USER_ID, "join")
        .await?;

    alpha.kick(&portal, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&portal, SENSOR_USER_ID, "leave")
        .await?;

    sensor.stop().await;
    Ok(())
}
