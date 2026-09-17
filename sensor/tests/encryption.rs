// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Ticket 04, Megolm decryption. An encrypted portal room is the norm, not
//! the exception: a message sent there arrives decrypted and schema-valid on
//! the bus; history the Sensor can never decrypt (the Megolm keys predate
//! its membership) is skipped without stalling the other rooms; and with
//! SENSOR_RECOVERY_KEY set, the Sensor bootstraps its cryptographic identity
//! from the operator's recovery key and reads backed-up history a fresh
//! device never saw.
//!
//! The harness's HTTP bots cannot Megolm-encrypt, so the bridge side is
//! played by `CryptoBot`, an SDK-backed test user (see harness/crypto.rs).

mod harness;

use anyhow::Result;
use harness::crypto::{make_encrypted_whatsapp_portal, wait_for_user_devices, CryptoBot};
use harness::{
    ensure_stack, fresh_state_dir, poll_until, sensor_env_with, sha256_hex,
    validate_against_contract, Bot, Bus, SensorProc, StoredMessage, SENSOR_USER_ID,
};
use matrix_sdk::encryption::EncryptionSettings;
use matrix_sdk::ruma::events::room::message::OriginalSyncRoomMessageEvent;

const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";
const STREAM: &str = "twalk";

/// Waits until the bus holds at least `count` events sourced from the room.
async fn wait_for_room_events(
    bus: &Bus,
    room_id: &str,
    count: usize,
) -> Result<Vec<StoredMessage>> {
    poll_until(
        || async {
            let messages = bus
                .fetch_room_messages(STREAM, MESSAGE_SUBJECT, room_id)
                .await
                .ok()?;
            (messages.len() >= count).then_some(messages)
        },
        &format!("waiting for {count} events from {room_id} on {MESSAGE_SUBJECT}"),
    )
    .await
}

/// The event id of the newest `m.room.encrypted` event `sender` sent in the
/// room — the encrypted twin of the plaintext body, from which the
/// contract's deterministic event id derives. `exclude` skips an already
/// seen event id when a room holds several encrypted messages.
async fn wait_for_encrypted_event_id(
    bot: &Bot,
    room_id: &str,
    sender: &str,
    exclude: Option<&str>,
) -> Result<String> {
    let exclude = exclude.map(str::to_owned);
    let event = bot
        .wait_for_event(
            room_id,
            |event| {
                event["type"].as_str() == Some("m.room.encrypted")
                    && event["sender"].as_str() == Some(sender)
                    && event["event_id"].as_str() != exclude.as_deref()
            },
            "an encrypted event",
        )
        .await?;
    Ok(event["event_id"].as_str().unwrap().to_owned())
}

#[tokio::test]
async fn a_message_in_an_encrypted_room_arrives_decrypted_and_schema_valid() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let state_dir = fresh_state_dir("encryption-basic");
    let env = sensor_env_with(&[("SENSOR_STATE_DIR", &state_dir.to_string_lossy())]);
    let mut sensor = SensorProc::start(&env)?;
    let alpha = CryptoBot::login("bot_alpha").await?;
    // A plain HTTP twin of the same account, for membership polling and for
    // reading the room timeline the CryptoBot's own sends land in.
    let alpha_http = Bot::login("bot_alpha").await?;

    let room_id = make_encrypted_whatsapp_portal(&alpha, "encrypted-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha_http
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    // The bot must have seen the Sensor's join — and its device — before
    // sending, so the Megolm session key is shared with it.
    alpha
        .wait_for_joined_member(&room_id, SENSOR_USER_ID)
        .await?;
    wait_for_user_devices(alpha.client(), SENSOR_USER_ID).await?;

    alpha
        .send_message(&room_id, "chiffré de bout en bout")
        .await?;
    let matrix_event_id =
        wait_for_encrypted_event_id(&alpha_http, &room_id, alpha.user_id(), None).await?;

    let stored = bus
        .wait_for_room_message(STREAM, MESSAGE_SUBJECT, &room_id)
        .await?;
    let event = &stored.payload;

    validate_against_contract(event, "inbound.message.received")?;
    assert_eq!(
        event["id"].as_str(),
        Some(sha256_hex(&format!("{matrix_event_id}:{room_id}")).as_str()),
        "the deterministic id derives from the encrypted Matrix event"
    );
    assert_eq!(event["subject"].as_str(), Some(alpha.user_id()));
    assert_eq!(event["network"].as_str(), Some("whatsapp"));
    assert_eq!(
        event["data"]["body"].as_str(),
        Some("chiffré de bout en bout"),
        "the body on the bus is the decrypted plaintext"
    );
    assert_eq!(event["data"]["format"].as_str(), Some("text/plain"));

    assert!(sensor.is_running());
    sensor.stop().await;
    let _ = std::fs::remove_dir_all(&state_dir);
    Ok(())
}

#[tokio::test]
async fn undecryptable_history_is_skipped_and_other_rooms_keep_flowing() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let state_dir = fresh_state_dir("encryption-utd");
    let env = sensor_env_with(&[("SENSOR_STATE_DIR", &state_dir.to_string_lossy())]);
    let mut sensor = SensorProc::start(&env)?;
    let alpha = CryptoBot::login("bot_alpha").await?;
    let alpha_http = Bot::login("bot_alpha").await?;

    // Room A: a message sent BEFORE the Sensor joins can never be decrypted
    // by it — the Megolm session key was shared only with the members at the
    // time. With shared history visibility the Sensor still receives the
    // event; it must log it, count it and skip it.
    let room_a = make_encrypted_whatsapp_portal(&alpha, "utd-history-portal").await?;
    alpha
        .send_message(&room_a, "history the sensor can never read")
        .await?;
    let pre_join_event_id =
        wait_for_encrypted_event_id(&alpha_http, &room_a, alpha.user_id(), None).await?;

    alpha.invite(&room_a, SENSOR_USER_ID).await?;
    alpha_http
        .wait_for_membership(&room_a, SENSOR_USER_ID, "join")
        .await?;
    alpha
        .wait_for_joined_member(&room_a, SENSOR_USER_ID)
        .await?;
    wait_for_user_devices(alpha.client(), SENSOR_USER_ID).await?;

    // Room B: a healthy encrypted room observed at the same time.
    let room_b = make_encrypted_whatsapp_portal(&alpha, "utd-neighbour-portal").await?;
    alpha.invite(&room_b, SENSOR_USER_ID).await?;
    alpha_http
        .wait_for_membership(&room_b, SENSOR_USER_ID, "join")
        .await?;
    alpha
        .wait_for_joined_member(&room_b, SENSOR_USER_ID)
        .await?;

    alpha
        .send_message(&room_b, "fresh secret in room B")
        .await?;
    alpha
        .send_message(&room_a, "fresh secret in room A")
        .await?;

    // Room B flows, undisturbed by room A's undecryptable history.
    let stored_b = bus
        .wait_for_room_message(STREAM, MESSAGE_SUBJECT, &room_b)
        .await?;
    validate_against_contract(&stored_b.payload, "inbound.message.received")?;
    assert_eq!(
        stored_b.payload["data"]["body"].as_str(),
        Some("fresh secret in room B")
    );

    // Room A keeps flowing too: the post-join message decrypts, the pre-join
    // one never reaches the bus.
    let events_a = wait_for_room_events(&bus, &room_a, 1).await?;
    validate_against_contract(&events_a[0].payload, "inbound.message.received")?;
    assert_eq!(
        events_a[0].payload["data"]["body"].as_str(),
        Some("fresh secret in room A")
    );
    let expected_id = sha256_hex(&format!(
        "{}:{room_a}",
        wait_for_encrypted_event_id(
            &alpha_http,
            &room_a,
            alpha.user_id(),
            Some(&pre_join_event_id)
        )
        .await?
    ));
    assert_eq!(
        events_a[0].payload["id"].as_str(),
        Some(expected_id.as_str())
    );

    // Let any erroneous publish of the undecryptable event happen.
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    let events_a = bus
        .fetch_room_messages(STREAM, MESSAGE_SUBJECT, &room_a)
        .await?;
    assert_eq!(
        events_a.len(),
        1,
        "the pre-join message must never be published"
    );
    assert!(
        sensor.is_running(),
        "the Sensor survived an undecryptable room"
    );

    sensor.stop().await;
    let _ = std::fs::remove_dir_all(&state_dir);
    Ok(())
}

#[tokio::test]
async fn the_recovery_key_bootstraps_the_identity_and_restores_history() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;

    // The onboarding device: an SDK client on the Sensor's own account that
    // sets up cross-signing, secret storage and a key backup — and yields
    // the recovery key the operator saves.
    let onboarding = CryptoBot::login_with(
        "sensor",
        EncryptionSettings {
            auto_enable_cross_signing: true,
            ..Default::default()
        },
    )
    .await?;
    // The stack persists across runs: drop any backup a previous run left
    // on the account before creating a fresh one.
    let _ = onboarding
        .client()
        .encryption()
        .backups()
        .disable_and_delete()
        .await;

    let alpha = CryptoBot::login("bot_alpha").await?;
    let room_id = make_encrypted_whatsapp_portal(&alpha, "recovery-portal").await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    onboarding.join_room(&room_id).await?;
    alpha
        .wait_for_joined_member(&room_id, SENSOR_USER_ID)
        .await?;

    // The onboarding device must hold the room key before backups are
    // enabled, so the upload `wait_for_backups_to_upload` waits on covers it:
    // wait until it decrypts the message.
    let (decrypted_tx, decrypted_rx) = tokio::sync::oneshot::channel::<()>();
    let decrypted_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(decrypted_tx)));
    onboarding
        .client()
        .add_event_handler(move |event: OriginalSyncRoomMessageEvent| {
            let decrypted_tx = decrypted_tx.clone();
            async move {
                if event.content.body() == "backed-up history" {
                    if let Some(tx) = decrypted_tx.lock().unwrap().take() {
                        let _ = tx.send(());
                    }
                }
            }
        });
    alpha.send_message(&room_id, "backed-up history").await?;
    tokio::time::timeout(std::time::Duration::from_secs(20), decrypted_rx)
        .await
        .expect("the onboarding device must decrypt the message")?;

    let recovery_key = onboarding
        .client()
        .encryption()
        .recovery()
        .enable()
        .wait_for_backups_to_upload()
        .await
        .expect("enabling recovery must yield the recovery key");
    drop(onboarding);

    // The replacement device: a fresh state directory, the recovery key as
    // the only link to the account's cryptographic identity.
    let state_dir = fresh_state_dir("encryption-recovery");
    let env = sensor_env_with(&[
        ("SENSOR_STATE_DIR", &state_dir.to_string_lossy()),
        ("SENSOR_RECOVERY_KEY", &recovery_key),
    ]);
    let mut sensor = SensorProc::start(&env)?;

    // The message predates this device entirely: only the key backup
    // restored through the recovery key can decrypt it.
    let stored = bus
        .wait_for_room_message(STREAM, MESSAGE_SUBJECT, &room_id)
        .await?;
    validate_against_contract(&stored.payload, "inbound.message.received")?;
    assert_eq!(
        stored.payload["data"]["body"].as_str(),
        Some("backed-up history"),
        "the replacement device decrypted history from the key backup"
    );

    assert!(sensor.is_running());
    sensor.stop().await;
    let _ = std::fs::remove_dir_all(&state_dir);
    Ok(())
}
