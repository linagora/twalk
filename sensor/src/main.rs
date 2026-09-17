//! The Twalk Sensor binary. All the decision logic lives in the library
//! modules; this file only wires them to matrix-sdk and NATS JetStream.

use anyhow::{Context, Result};
use matrix_sdk::config::SyncSettings;
use matrix_sdk::deserialized_responses::RawAnySyncOrStrippedState;
use matrix_sdk::ruma::events::reaction::OriginalSyncReactionEvent;
use matrix_sdk::ruma::events::room::member::{MembershipState, StrippedRoomMemberEvent};
use matrix_sdk::ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent};
use matrix_sdk::ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent, SyncMessageLikeEvent};
use matrix_sdk::ruma::{EventId, OwnedUserId};
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

    // Inbound reactions: normalize and publish. Reaction removals arrive as
    // redactions, never as m.reaction events, so they produce no event, per
    // the contract.
    {
        let jetstream = jetstream.clone();
        let own_user = own_user.clone();
        client.add_event_handler(move |event: OriginalSyncReactionEvent, room: Room, _client: Client| {
            let jetstream = jetstream.clone();
            let own_user = own_user.clone();
            async move {
                if event.sender == own_user {
                    return; // never loop on our own outbound traffic
                }
                let reactor: OwnedUserId = event.sender.clone();
                let Some(network) = resolve_network(&room, &reactor).await else {
                    warn!(room = %room.room_id(), %reactor, "cannot determine the network, skipping event");
                    return;
                };
                let display_name = room
                    .get_member(&reactor)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|member| member.display_name().map(str::to_owned))
                    .unwrap_or_else(|| reactor.localpart().to_owned());
                // Consent labelling arrives with ticket 05; unknown senders
                // default to pending, per the contract.
                let consent = Consent::Pending;
                let target_event_id = event.content.relates_to.event_id.clone();
                let excerpt = target_excerpt(&room, &target_event_id).await;
                let input = normalize::InboundReaction {
                    matrix_event_id: event.event_id.to_string(),
                    matrix_room_id: room.room_id().to_string(),
                    server_name: own_user.server_name().as_str().to_owned(),
                    reactor: reactor.to_string(),
                    reaction: event.content.relates_to.key.clone(),
                    target_event_id: target_event_id.to_string(),
                    target_excerpt: excerpt,
                    network,
                    consent,
                    display_name,
                    produced_at: rfc3339(std::time::SystemTime::now()),
                    // Bridges report network timestamps in bridge-specific
                    // fields; mapping them arrives with the enrichment work.
                    network_timestamp: None,
                };
                let envelope = normalize::build_reaction_added(&input);
                let id = envelope["id"].as_str().unwrap().to_owned();
                let mut headers = async_nats::header::HeaderMap::new();
                headers.insert(async_nats::header::NATS_MESSAGE_ID, id.as_str());
                headers.insert("network", network.as_str());
                headers.insert("consent", consent.as_str());
                let payload = serde_json::to_vec(&envelope).expect("the envelope is serializable");
                let subject = normalize::bus_subject(normalize::REACTION_ADDED_TYPE);
                match jetstream
                    .publish_with_headers(subject, headers, payload.into())
                    .await
                {
                    Ok(ack) => match ack.await {
                        Ok(_) => info!(%id, room = %room.room_id(), "published inbound.reaction.added.v1"),
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

/// Fetches the target of a reaction from the homeserver (via the SDK's
/// `Room::event`) and extracts a contract-capped excerpt of its body.
/// Only plain text messages yield an excerpt; an unreachable target is not
/// an error — the event still publishes, without the excerpt.
async fn target_excerpt(room: &Room, event_id: &EventId) -> Option<String> {
    let timeline_event = room.event(event_id, None).await.ok()?;
    let AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(
        SyncMessageLikeEvent::Original(message),
    )) = timeline_event.raw().deserialize().ok()?
    else {
        return None;
    };
    match message.content.msgtype {
        MessageType::Text(text) => Some(normalize::excerpt(&text.body)),
        _ => None,
    }
}
