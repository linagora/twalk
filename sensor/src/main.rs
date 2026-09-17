//! The Twalk Sensor binary. All the decision logic lives in the library
//! modules; this file only wires them to matrix-sdk and NATS JetStream.

use anyhow::{Context, Result};
use matrix_sdk::config::SyncSettings;
use matrix_sdk::deserialized_responses::RawAnySyncOrStrippedState;
use matrix_sdk::ruma::events::presence::PresenceEvent;
use matrix_sdk::ruma::events::room::member::{MembershipState, StrippedRoomMemberEvent};
use matrix_sdk::ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent};
use matrix_sdk::ruma::OwnedUserId;
use matrix_sdk::{Client, Room};
use tracing::{info, warn};
use twalk_sensor::config::Config;
use twalk_sensor::consent::Consent;
use twalk_sensor::{network, normalize};

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    tracing_subscriber::fmt()
        .with_env_filter(&config.log_level)
        .init();
    info!(homeserver = %config.homeserver_url, user = %config.user_id, "sensor starting");

    let client = Client::builder()
        .homeserver_url(&config.homeserver_url)
        .build()
        .await?;
    client
        .matrix_auth()
        .login_username(&config.user_id, &config.password)
        .initial_device_display_name("twalk-sensor")
        .send()
        .await
        .context("matrix login failed")?;
    info!("logged in to the homeserver");

    let nats = async_nats::connect(&config.nats_url)
        .await
        .context("failed to connect to NATS")?;
    let jetstream = async_nats::jetstream::new(nats);
    jetstream
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: normalize::STREAM_NAME.to_owned(),
            subjects: normalize::STREAM_SUBJECTS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ..Default::default()
        })
        .await
        .context("failed to ensure the twalk stream")?;
    info!(stream = normalize::STREAM_NAME, "bus ready");

    let own_user = client.user_id().unwrap().to_owned();

    // Observation scope is invitation-driven: join when the inviter is a
    // configured bridge provisioning user or the operator, ignore everyone
    // else. No room is observed by default.
    {
        let allowed = config.allowed_inviters.clone();
        let own_user = own_user.clone();
        client.add_event_handler(move |event: StrippedRoomMemberEvent, room: Room, _client: Client| {
            let allowed = allowed.clone();
            let own_user = own_user.clone();
            async move {
                if event.state_key != own_user {
                    return;
                }
                if event.content.membership != MembershipState::Invite {
                    return;
                }
                let inviter = event.sender.to_string();
                if allowed.contains(&inviter) {
                    info!(room = %room.room_id(), %inviter, "joining observed room");
                    if let Err(error) = room.join().await {
                        warn!(room = %room.room_id(), %error, "failed to join invited room");
                    }
                } else {
                    info!(room = %room.room_id(), %inviter, "ignoring invite from disallowed inviter");
                }
            }
        });
    }

    // Inbound messages: normalize and publish.
    {
        let jetstream = jetstream.clone();
        let own_user = own_user.clone();
        client.add_event_handler(move |event: OriginalSyncRoomMessageEvent, room: Room, _client: Client| {
            let jetstream = jetstream.clone();
            let own_user = own_user.clone();
            async move {
                if event.sender == own_user {
                    return; // never loop on our own outbound traffic
                }
                let body = match &event.content.msgtype {
                    MessageType::Text(text) => text.body.clone(),
                    _ => return, // v1 skeleton: plain text only
                };
                let sender: OwnedUserId = event.sender.clone();
                let Some(network) = resolve_network(&room, &sender).await else {
                    warn!(room = %room.room_id(), %sender, "cannot determine the network, skipping event");
                    return;
                };
                let display_name = room
                    .get_member(&sender)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|member| member.display_name().map(str::to_owned))
                    .unwrap_or_else(|| sender.localpart().to_owned());
                // Consent labelling arrives with ticket 05; unknown senders
                // default to pending, per the contract.
                let consent = Consent::Pending;
                let input = normalize::InboundMessage {
                    matrix_event_id: event.event_id.to_string(),
                    matrix_room_id: room.room_id().to_string(),
                    server_name: own_user.server_name().as_str().to_owned(),
                    sender: sender.to_string(),
                    body,
                    network,
                    consent,
                    display_name,
                    produced_at: rfc3339(std::time::SystemTime::now()),
                    // Bridges report network timestamps in bridge-specific
                    // fields; the homeserver's origin_server_ts is not one.
                    // Mapping bridge fields arrives with the enrichment work.
                    network_timestamp: None,
                };
                let envelope = normalize::build_message_received(&input);
                let id = envelope["id"].as_str().unwrap().to_owned();
                let mut headers = async_nats::header::HeaderMap::new();
                headers.insert(async_nats::header::NATS_MESSAGE_ID, id.as_str());
                headers.insert("network", network.as_str());
                headers.insert("consent", consent.as_str());
                let payload = serde_json::to_vec(&envelope).expect("the envelope is serializable");
                let subject = normalize::bus_subject(normalize::MESSAGE_RECEIVED_TYPE);
                match jetstream
                    .publish_with_headers(subject, headers, payload.into())
                    .await
                {
                    Ok(ack) => match ack.await {
                        Ok(_) => info!(%id, room = %room.room_id(), "published inbound.message.received.v1"),
                        Err(error) => warn!(%id, %error, "publish ack failed"),
                    },
                    Err(error) => warn!(%id, %error, "publish failed"),
                }
            }
        });
    }

    // Bridge-puppet presence. Presence updates in Matrix are NOT
    // room-scoped: they arrive in the sync response's presence list for
    // every user sharing a room with the Sensor (matrix-sdk dispatches
    // them as `PresenceEvent`s with no room context). The contract's
    // `source` is a portal room URI, so one observed room is picked
    // deterministically: the lexicographically first joined room the user
    // is also joined to. As for messages, only users attributable to a
    // network (portal `m.bridge` state or ghost prefix) are published —
    // never the Sensor itself. Presence is best-effort: every failure mode
    // logs and returns, so a bridge without presence support can neither
    // break nor slow the rest of the pipeline.
    {
        let jetstream = jetstream.clone();
        let own_user = own_user.clone();
        client.add_event_handler(move |event: PresenceEvent, client: Client| {
            let jetstream = jetstream.clone();
            let own_user = own_user.clone();
            async move {
                let sender: OwnedUserId = event.sender.clone();
                if sender == own_user {
                    return; // never loop on our own presence
                }
                let presence = match event.content.presence.as_str() {
                    "online" => normalize::Presence::Online,
                    "offline" => normalize::Presence::Offline,
                    "unavailable" => normalize::Presence::Unavailable,
                    other => {
                        warn!(%sender, presence = %other, "unknown presence state, skipping event");
                        return;
                    }
                };
                // Presence EDUs carry no homeserver timestamp: the Sensor's
                // receipt instant is the natural key's receipt timestamp.
                // `time` and the id derive from this one instant (truncated
                // to milliseconds) so consumers can recompute the id.
                let receipt_timestamp_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
                    .unwrap_or_default();

                let mut shared_room_ids = Vec::new();
                for room in client.joined_rooms() {
                    let shares_room = room
                        .get_member(&sender)
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|member| *member.membership() == MembershipState::Join);
                    if shares_room {
                        shared_room_ids.push(room.room_id().to_owned());
                    }
                }
                shared_room_ids.sort_unstable();
                // Pick the first shared room whose network resolves: a
                // contact may share non-portal rooms (no m.bridge state)
                // with the Sensor; those must not shadow a real portal
                // room further down the list.
                let mut resolved = None;
                for room_id in &shared_room_ids {
                    let Some(candidate) = client.get_room(room_id) else {
                        continue;
                    };
                    if let Some(network) = resolve_network(&candidate, &sender).await {
                        resolved = Some((candidate, network));
                        break;
                    }
                }
                let Some((room, network)) = resolved else {
                    if shared_room_ids.is_empty() {
                        return; // not a portal contact: shares no observed room
                    }
                    warn!(%sender, "cannot determine the network in any shared room, skipping event");
                    return;
                };
                let display_name = room
                    .get_member(&sender)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|member| member.display_name().map(str::to_owned))
                    .unwrap_or_else(|| sender.localpart().to_owned());
                // Consent labelling arrives with ticket 05; unknown contacts
                // default to pending, per the contract.
                let consent = Consent::Pending;
                let last_active_at = event
                    .content
                    .last_active_ago
                    .map(|ago| rfc3339_ms(receipt_timestamp_ms.saturating_sub(u64::from(ago))));
                let input = normalize::InboundPresence {
                    matrix_user_id: sender.to_string(),
                    presence,
                    server_name: own_user.server_name().as_str().to_owned(),
                    matrix_room_id: room.room_id().to_string(),
                    network,
                    consent,
                    display_name,
                    produced_at: rfc3339_ms(receipt_timestamp_ms),
                    receipt_timestamp_ms,
                    last_active_at,
                };
                let envelope = normalize::build_presence_updated(&input);
                let id = envelope["id"].as_str().unwrap().to_owned();
                let mut headers = async_nats::header::HeaderMap::new();
                headers.insert(async_nats::header::NATS_MESSAGE_ID, id.as_str());
                headers.insert("network", network.as_str());
                headers.insert("consent", consent.as_str());
                let payload = serde_json::to_vec(&envelope).expect("the envelope is serializable");
                let subject = normalize::bus_subject(normalize::PRESENCE_UPDATED_TYPE);
                match jetstream
                    .publish_with_headers(subject, headers, payload.into())
                    .await
                {
                    Ok(ack) => match ack.await {
                        Ok(_) => info!(%id, %sender, "published inbound.presence.updated.v1"),
                        Err(error) => warn!(%id, %error, "publish ack failed"),
                    },
                    Err(error) => warn!(%id, %error, "publish failed"),
                }
            }
        });
    }

    info!("sensor running");
    client
        .sync(SyncSettings::default())
        .await
        .context("sync loop failed")?;
    Ok(())
}

fn rfc3339(time: std::time::SystemTime) -> String {
    time::OffsetDateTime::from(time)
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC 3339 formatting is infallible")
}

/// Formats a milliseconds-since-epoch timestamp as RFC 3339 with exact
/// millisecond precision: parsing the result back yields the same number,
/// which the presence id's natural key includes.
fn rfc3339_ms(ms: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .expect("millisecond timestamps are in range")
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC 3339 formatting is infallible")
}

/// Reads the room's `m.bridge` state event (the IO) and defers to the pure
/// attribution policy in `network::resolve`.
async fn resolve_network(room: &Room, sender: &OwnedUserId) -> Option<network::Network> {
    let bridge_content = room
        .get_state_event("m.bridge".into(), "")
        .await
        .ok()
        .flatten()
        .and_then(|raw| {
            let json = match &raw {
                RawAnySyncOrStrippedState::Sync(raw) => raw.json().get(),
                RawAnySyncOrStrippedState::Stripped(raw) => raw.json().get(),
            };
            serde_json::from_str::<serde_json::Value>(json).ok()
        })
        .and_then(|event| event.get("content").cloned());
    network::resolve(bridge_content.as_ref(), sender.localpart())
}
