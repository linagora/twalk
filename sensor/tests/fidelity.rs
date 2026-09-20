// matrix-sdk crypto futures overflow the default trait-solver depth when
// spawned (harness::CryptoBot); matrix-sdk itself sets the same limit.
#![recursion_limit = "256"]

//! Ticket 06, message shape fidelity: rich messages arrive contract-complete.
//! A reply carries `reply_to` with the parent's event id and a capped excerpt,
//! a threaded message carries `thread_root`, a media message carries an
//! attachment entry (an `mxc://` reference — the binary never transits the
//! bus), and bridge traffic preserves the original network timestamp the
//! bridge reports through `origin_server_ts`.

mod harness;

use std::time::{Duration, SystemTime};

use anyhow::Result;
use harness::gateway::{contact_entry, sensor_env_granting};
use harness::{
    ensure_stack, make_whatsapp_portal, poll_until, sensor_env, sha256_hex,
    validate_against_contract, Bot, Bus, SensorProc, StoredMessage, SENSOR_USER_ID,
};
use serde_json::{json, Value};

const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";
const STREAM: &str = "twalk";

/// Polls until the bus holds at least `count` events for the room, then
/// returns them all.
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
        &format!("waiting for {count} events from {room_id}"),
    )
    .await
}

/// Finds the stored event produced from one Matrix event, by recomputing the
/// contract's deterministic id independently of the Sensor's own code.
fn find_event<'a>(
    messages: &'a [StoredMessage],
    room_id: &str,
    matrix_event_id: &str,
) -> &'a Value {
    let expected_id = sha256_hex(&format!("{matrix_event_id}:{room_id}"));
    messages
        .iter()
        .map(|message| &message.payload)
        .find(|payload| payload["id"].as_str() == Some(expected_id.as_str()))
        .unwrap_or_else(|| panic!("no stored event for {matrix_event_id} in {room_id}"))
}

/// A portal room the Sensor has joined, ready for `bot_alpha` to speak in.
async fn observed_portal(alpha: &Bot, name: &str) -> Result<String> {
    let room_id = make_whatsapp_portal(alpha, name).await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    Ok(room_id)
}

#[tokio::test]
async fn a_reply_carries_the_parent_id_and_a_capped_excerpt() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let alpha = Bot::login("bot_alpha").await?;
    // An excerpt quotes a message somebody wrote, and is published only once
    // *that* author has granted (issue #110). Here alpha writes both the
    // parent and the reply, so granting alpha is what makes the excerpt part
    // of this test's subject observable at all.
    let (_gateway, env) = sensor_env_granting(
        &bus,
        vec![contact_entry(alpha.user_id(), "whatsapp", "granted")],
        &[],
    )
    .await?;
    let sensor = SensorProc::start(&env)?;

    let room_id = observed_portal(&alpha, "reply-portal").await?;

    // Longer than the contract's 512-char excerpt cap, multi-byte so a
    // byte-wise truncation would split a char and fail validation.
    let parent_body = "é".repeat(600);
    let parent_id = alpha.send_message(&room_id, &parent_body).await?;
    let reply_id = alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.text",
                "body": "oui, d'accord",
                "m.relates_to": { "m.in_reply_to": { "event_id": parent_id } },
            }),
        )
        .await?;

    let messages = wait_for_room_events(&bus, &room_id, 2).await?;

    let reply = find_event(&messages, &room_id, &reply_id);
    validate_against_contract(reply, "inbound.message.received")?;
    assert_eq!(
        reply["data"]["reply_to"]["matrix_event_id"].as_str(),
        Some(parent_id.as_str())
    );
    let expected_excerpt: String = parent_body.chars().take(512).collect();
    assert_eq!(
        reply["data"]["reply_to"]["excerpt"].as_str(),
        Some(expected_excerpt.as_str()),
        "the excerpt quotes the parent, capped at the contract's 512 chars"
    );

    let parent = find_event(&messages, &room_id, &parent_id);
    validate_against_contract(parent, "inbound.message.received")?;
    assert_eq!(
        parent["data"]["reply_to"],
        Value::Null,
        "the parent itself is not a reply"
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_threaded_message_carries_the_thread_root() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let alpha = Bot::login("bot_alpha").await?;
    // Granted for the same reason as the reply test: the in-thread reply's
    // excerpt quotes alpha's own root message (issue #110).
    let (_gateway, env) = sensor_env_granting(
        &bus,
        vec![contact_entry(alpha.user_id(), "whatsapp", "granted")],
        &[],
    )
    .await?;
    let sensor = SensorProc::start(&env)?;

    let room_id = observed_portal(&alpha, "thread-portal").await?;

    let root_id = alpha.send_message(&room_id, "fil de discussion").await?;
    // How thread-unaware clients see a threaded message: the in_reply_to is
    // only a fallback, so it must not become a reply_to.
    let fallback_id = alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.text",
                "body": "dans le fil",
                "m.relates_to": {
                    "rel_type": "m.thread",
                    "event_id": root_id,
                    "m.in_reply_to": { "event_id": root_id },
                    "is_falling_back": true,
                },
            }),
        )
        .await?;
    // A genuine reply to a message inside the thread: both thread_root and
    // reply_to are meaningful.
    let in_thread_reply_id = alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.text",
                "body": "réponse précise",
                "m.relates_to": {
                    "rel_type": "m.thread",
                    "event_id": root_id,
                    "m.in_reply_to": { "event_id": root_id },
                    "is_falling_back": false,
                },
            }),
        )
        .await?;

    let messages = wait_for_room_events(&bus, &room_id, 3).await?;

    let root = find_event(&messages, &room_id, &root_id);
    validate_against_contract(root, "inbound.message.received")?;
    assert!(
        root["data"].get("thread_root").is_none(),
        "the root is not part of a thread"
    );

    let fallback = find_event(&messages, &room_id, &fallback_id);
    validate_against_contract(fallback, "inbound.message.received")?;
    assert_eq!(
        fallback["data"]["thread_root"].as_str(),
        Some(root_id.as_str())
    );
    assert_eq!(
        fallback["data"]["reply_to"],
        Value::Null,
        "a falling-back in_reply_to is not a real reply"
    );

    let in_thread_reply = find_event(&messages, &room_id, &in_thread_reply_id);
    validate_against_contract(in_thread_reply, "inbound.message.received")?;
    assert_eq!(
        in_thread_reply["data"]["thread_root"].as_str(),
        Some(root_id.as_str())
    );
    assert_eq!(
        in_thread_reply["data"]["reply_to"]["matrix_event_id"].as_str(),
        Some(root_id.as_str())
    );
    assert_eq!(
        in_thread_reply["data"]["reply_to"]["excerpt"].as_str(),
        Some("fil de discussion")
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn an_image_message_carries_an_attachment_reference() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = observed_portal(&alpha, "image-portal").await?;

    // A bare image: body is the filename, so there is no caption.
    let image_id = alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.image",
                "body": "vue-sur-mer.png",
                "filename": "vue-sur-mer.png",
                "url": "mxc://test.twalk/AbCdEf0123456789",
                "info": { "mimetype": "image/png", "size": 53201, "w": 800, "h": 600 },
            }),
        )
        .await?;
    // A captioned image: body differs from the filename.
    let captioned_id = alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.image",
                "body": "regarde cette photo",
                "filename": "IMG_2042.jpg",
                "url": "mxc://test.twalk/ZyXwVu0987654321",
                "info": { "mimetype": "image/jpeg", "size": 102400, "w": 1920, "h": 1080 },
            }),
        )
        .await?;

    let messages = wait_for_room_events(&bus, &room_id, 2).await?;

    let image = find_event(&messages, &room_id, &image_id);
    validate_against_contract(image, "inbound.message.received")?;
    assert_eq!(image["data"]["body"].as_str(), Some("vue-sur-mer.png"));
    let attachment = &image["data"]["attachments"][0];
    assert_eq!(attachment["kind"].as_str(), Some("image"));
    assert_eq!(
        attachment["mxc_uri"].as_str(),
        Some("mxc://test.twalk/AbCdEf0123456789")
    );
    assert_eq!(attachment["mime_type"].as_str(), Some("image/png"));
    assert_eq!(attachment["size_bytes"].as_u64(), Some(53201));
    assert_eq!(
        attachment["dimensions"],
        json!({ "width": 800, "height": 600 })
    );
    assert!(
        attachment.get("caption").is_none(),
        "body equal to the filename is not a caption"
    );
    assert!(attachment.get("duration_ms").is_none());
    // The mxc URIs above are fake: the event is complete and schema-valid
    // without the Sensor ever downloading the binary.

    let captioned = find_event(&messages, &room_id, &captioned_id);
    validate_against_contract(captioned, "inbound.message.received")?;
    assert_eq!(
        captioned["data"]["body"].as_str(),
        Some("regarde cette photo")
    );
    let attachment = &captioned["data"]["attachments"][0];
    assert_eq!(attachment["kind"].as_str(), Some("image"));
    assert_eq!(
        attachment["mxc_uri"].as_str(),
        Some("mxc://test.twalk/ZyXwVu0987654321")
    );
    assert_eq!(
        attachment["caption"].as_str(),
        Some("regarde cette photo"),
        "a body distinct from the filename is the caption"
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn media_msgtypes_map_to_attachment_kinds() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = observed_portal(&alpha, "media-portal").await?;

    let video_id = alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.video",
                "body": "clip.mp4",
                "url": "mxc://test.twalk/ViDeO0123456789ab",
                "info": {
                    "mimetype": "video/mp4",
                    "size": 1048576,
                    "w": 1280,
                    "h": 720,
                    "duration": 4200,
                },
            }),
        )
        .await?;
    let audio_id = alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.audio",
                "body": "vocal.ogg",
                "url": "mxc://test.twalk/Audio0123456789ab",
                "info": { "mimetype": "audio/ogg", "size": 8192, "duration": 2500 },
            }),
        )
        .await?;
    let file_id = alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.file",
                "body": "compte-rendu.pdf",
                "url": "mxc://test.twalk/FiLe0123456789abc",
                "info": { "mimetype": "application/pdf", "size": 20480 },
            }),
        )
        .await?;

    let messages = wait_for_room_events(&bus, &room_id, 3).await?;

    let video = find_event(&messages, &room_id, &video_id);
    validate_against_contract(video, "inbound.message.received")?;
    let attachment = &video["data"]["attachments"][0];
    assert_eq!(attachment["kind"].as_str(), Some("video"));
    assert_eq!(
        attachment["mxc_uri"].as_str(),
        Some("mxc://test.twalk/ViDeO0123456789ab")
    );
    assert_eq!(attachment["mime_type"].as_str(), Some("video/mp4"));
    assert_eq!(attachment["size_bytes"].as_u64(), Some(1048576));
    assert_eq!(
        attachment["dimensions"],
        json!({ "width": 1280, "height": 720 })
    );
    assert_eq!(attachment["duration_ms"].as_u64(), Some(4200));

    let audio = find_event(&messages, &room_id, &audio_id);
    validate_against_contract(audio, "inbound.message.received")?;
    let attachment = &audio["data"]["attachments"][0];
    assert_eq!(attachment["kind"].as_str(), Some("audio"));
    assert_eq!(attachment["mime_type"].as_str(), Some("audio/ogg"));
    assert_eq!(attachment["size_bytes"].as_u64(), Some(8192));
    assert_eq!(attachment["duration_ms"].as_u64(), Some(2500));
    assert!(
        attachment.get("dimensions").is_none(),
        "audio has no pixel dimensions"
    );

    let file = find_event(&messages, &room_id, &file_id);
    validate_against_contract(file, "inbound.message.received")?;
    let attachment = &file["data"]["attachments"][0];
    assert_eq!(attachment["kind"].as_str(), Some("file"));
    assert_eq!(attachment["mime_type"].as_str(), Some("application/pdf"));
    assert_eq!(attachment["size_bytes"].as_u64(), Some(20480));
    assert!(attachment.get("dimensions").is_none());
    assert!(attachment.get("duration_ms").is_none());

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_sticker_msgtype_carries_a_sticker_attachment() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = observed_portal(&alpha, "sticker-portal").await?;

    // Some bridges relay stickers as m.room.message with msgtype m.sticker.
    let sticker_id = alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.sticker",
                "body": "😀",
                "url": "mxc://test.twalk/StIcKeR0123456",
                "info": { "mimetype": "image/png", "size": 2048, "w": 512, "h": 512 },
            }),
        )
        .await?;

    let messages = wait_for_room_events(&bus, &room_id, 1).await?;
    let event = find_event(&messages, &room_id, &sticker_id);
    validate_against_contract(event, "inbound.message.received")?;
    assert_eq!(event["data"]["body"].as_str(), Some("😀"));
    let attachment = &event["data"]["attachments"][0];
    assert_eq!(attachment["kind"].as_str(), Some("sticker"));
    assert_eq!(
        attachment["mxc_uri"].as_str(),
        Some("mxc://test.twalk/StIcKeR0123456")
    );
    assert_eq!(attachment["mime_type"].as_str(), Some("image/png"));
    assert_eq!(attachment["size_bytes"].as_u64(), Some(2048));
    assert_eq!(
        attachment["dimensions"],
        json!({ "width": 512, "height": 512 })
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_location_message_carries_its_geo_body_without_an_attachment() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = observed_portal(&alpha, "location-portal").await?;

    let location_id = alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.location",
                "body": "Tour Eiffel geo:48.8583,2.2945",
                "geo_uri": "geo:48.8583,2.2945",
            }),
        )
        .await?;

    let messages = wait_for_room_events(&bus, &room_id, 1).await?;
    let event = find_event(&messages, &room_id, &location_id);
    validate_against_contract(event, "inbound.message.received")?;
    assert_eq!(
        event["data"]["body"].as_str(),
        Some("Tour Eiffel geo:48.8583,2.2945")
    );
    // The contract's attachment shape requires an mxc:// URI, which geo
    // messages do not have: in v1 the location travels as the body text.
    assert_eq!(event["data"]["attachments"], json!([]));

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn bridge_traffic_preserves_the_network_timestamp() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;
    // A mautrix puppet: the ghost user the bridge relays messages as.
    let puppet = Bot::login("whatsapp_33612345678").await?;

    // An m.bridge room: bridge traffic, whoever technically sent the event.
    let portal_id = observed_portal(&alpha, "timestamp-portal").await?;
    // A plain room without m.bridge state where a ghost speaks: the ghost
    // prefix alone marks the traffic as bridged.
    let ghost_room_id = alpha.create_room("ghost-timestamp-portal", false).await?;
    alpha.invite(&ghost_room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&ghost_room_id, SENSOR_USER_ID, "join")
        .await?;
    alpha.invite(&ghost_room_id, puppet.user_id()).await?;
    puppet.join_room(&ghost_room_id).await?;

    let before = SystemTime::now();
    let portal_message_id = alpha.send_message(&portal_id, "bonjour").await?;
    let ghost_message_id = puppet
        .send_message(&ghost_room_id, "hello from a ghost")
        .await?;

    let portal_messages = wait_for_room_events(&bus, &portal_id, 1).await?;
    let portal_event = find_event(&portal_messages, &portal_id, &portal_message_id);
    validate_against_contract(portal_event, "inbound.message.received")?;
    assert_network_timestamp_near(portal_event, before)?;

    let ghost_messages = wait_for_room_events(&bus, &ghost_room_id, 1).await?;
    let ghost_event = find_event(&ghost_messages, &ghost_room_id, &ghost_message_id);
    validate_against_contract(ghost_event, "inbound.message.received")?;
    assert_eq!(ghost_event["network"].as_str(), Some("whatsapp"));
    assert_network_timestamp_near(ghost_event, before)?;

    sensor.stop().await;
    Ok(())
}

/// The bridge reports the original network time through the event's
/// origin_server_ts: assert the field is present, RFC 3339, and consistent
/// with when the message was actually sent (the test bridge does not massage
/// timestamps, so receipt time is the expected value).
fn assert_network_timestamp_near(event: &Value, sent_after: SystemTime) -> Result<()> {
    let raw = event["data"]["network_timestamp"]
        .as_str()
        .expect("bridge traffic carries a network_timestamp");
    let parsed = time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
        .expect("network_timestamp is RFC 3339");
    let timestamp = SystemTime::from(parsed);
    assert!(
        timestamp >= sent_after - Duration::from_secs(10),
        "network_timestamp {raw} predates the send"
    );
    assert!(
        timestamp <= SystemTime::now() + Duration::from_secs(10),
        "network_timestamp {raw} is in the future"
    );
    Ok(())
}

#[tokio::test]
async fn a_native_matrix_room_publishes_with_the_matrix_network() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    // No m.bridge state, no ghost prefix: the user's own Matrix account,
    // which is a network of its own (ADR 0009). Until #18 this was silence.
    let room_id = alpha.create_room("native-matrix-room", false).await?;
    alpha.invite(&room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&room_id, SENSOR_USER_ID, "join")
        .await?;
    // A room some bridge did mark, for a network this Sensor does not
    // support: an unsupported portal is not native traffic, and stays
    // unpublished rather than being mislabelled `matrix`.
    let irc_room_id = alpha.create_room("irc-portal", false).await?;
    let (state_key, content) = harness::bridge_state(alpha.user_id(), "irc", "irc", "#irc-portal");
    alpha
        .send_state_event(&irc_room_id, "m.bridge", &state_key, content)
        .await?;
    alpha.invite(&irc_room_id, SENSOR_USER_ID).await?;
    alpha
        .wait_for_membership(&irc_room_id, SENSOR_USER_ID, "join")
        .await?;

    alpha
        .send_message(&irc_room_id, "depuis un réseau inconnu")
        .await?;
    let before = SystemTime::now();
    let matrix_event_id = alpha.send_message(&room_id, "un message ordinaire").await?;

    let messages = wait_for_room_events(&bus, &room_id, 1).await?;
    let event = find_event(&messages, &room_id, &matrix_event_id);
    validate_against_contract(event, "inbound.message.received")?;
    assert_eq!(
        event["network"].as_str(),
        Some("matrix"),
        "native Matrix traffic is published as its own network, not skipped"
    );
    assert_eq!(event["subject"].as_str(), Some("@bot_alpha:test.twalk"));
    assert_eq!(event["consent"].as_str(), Some("pending"));
    assert_eq!(event["data"]["body"].as_str(), Some("un message ordinaire"));
    assert_eq!(
        messages[0].header("network"),
        Some("matrix"),
        "the bus header carries the network for server-side filtering"
    );
    // #269: a native Matrix room is the `matrix` connection, on the envelope
    // and on the header alike.
    assert_eq!(event["connection"].as_str(), Some("matrix"));
    assert_eq!(messages[0].header("connection"), Some("matrix"));
    assert!(
        event["data"]["contact"].get("network_identifier").is_none(),
        "on the Matrix network the Matrix user id is the identifier, and it is the subject"
    );
    // Matrix is the source network here, so its own event timestamp *is* the
    // network timestamp — no homeserver receive time masquerading as another
    // network's.
    assert_network_timestamp_near(event, before)?;

    // The unsupported portal stayed silent. The event above was sent after
    // the portal's, so the Sensor has already worked past it; the grace
    // covers the two rooms landing in different sync responses.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let irc_messages = bus
        .fetch_room_messages(STREAM, MESSAGE_SUBJECT, &irc_room_id)
        .await?;
    assert!(
        irc_messages.is_empty(),
        "a portal of an unknown network is not native Matrix traffic"
    );

    sensor.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_message_edit_produces_no_event() -> Result<()> {
    ensure_stack().await?;
    let _guard = harness::SENSOR_LOCK.lock().await;
    let bus = Bus::connect().await?;
    let sensor = SensorProc::start(&sensor_env())?;
    let alpha = Bot::login("bot_alpha").await?;

    let room_id = observed_portal(&alpha, "edit-portal").await?;

    let original_id = alpha.send_message(&room_id, "rendez-vous à 10h").await?;
    // How a bridge relays an edit made on the network: a new event with a
    // `* `-prefixed fallback body, an m.replace relation and m.new_content.
    alpha
        .send_event(
            &room_id,
            "m.room.message",
            json!({
                "msgtype": "m.text",
                "body": "* rendez-vous à 11h",
                "m.new_content": { "msgtype": "m.text", "body": "rendez-vous à 11h" },
                "m.relates_to": { "rel_type": "m.replace", "event_id": original_id },
            }),
        )
        .await?;
    // A sentinel after the edit: the Sensor handles a room's timeline in
    // order and awaits each publish, so once the sentinel is on the bus the
    // edit has been handled — no sleep needed to assert its absence.
    let sentinel_id = alpha.send_message(&room_id, "à tout à l'heure").await?;

    let sentinel_event_id = sha256_hex(&format!("{sentinel_id}:{room_id}"));
    let messages = poll_until(
        || async {
            let messages = bus
                .fetch_room_messages(STREAM, MESSAGE_SUBJECT, &room_id)
                .await
                .ok()?;
            messages
                .iter()
                .any(|message| message.payload["id"].as_str() == Some(sentinel_event_id.as_str()))
                .then_some(messages)
        },
        &format!("waiting for the sentinel event from {room_id}"),
    )
    .await?;

    let original = find_event(&messages, &room_id, &original_id);
    validate_against_contract(original, "inbound.message.received")?;
    assert_eq!(original["data"]["body"].as_str(), Some("rendez-vous à 10h"));
    assert_eq!(
        messages.len(),
        2,
        "message edits have no v1 event type: only the original and the sentinel are published"
    );

    sensor.stop().await;
    Ok(())
}
