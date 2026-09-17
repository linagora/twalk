//! The Twalk Sensor binary. All the decision logic lives in the library
//! modules; this file only wires them to matrix-sdk and NATS JetStream.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_nats::jetstream::AckKind;
use futures::StreamExt;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::deserialized_responses::RawAnySyncOrStrippedState;
use matrix_sdk::ruma::events::presence::PresenceEvent;
use matrix_sdk::ruma::events::reaction::OriginalSyncReactionEvent;
use matrix_sdk::ruma::events::relation::Reply;
use matrix_sdk::ruma::events::room::member::{MembershipState, StrippedRoomMemberEvent};
use matrix_sdk::ruma::events::room::message::{
    MessageType, OriginalSyncRoomMessageEvent, Relation, RoomMessageEventContent,
};
use matrix_sdk::ruma::events::{AnySyncMessageLikeEvent, AnySyncTimelineEvent, SyncMessageLikeEvent};
use matrix_sdk::ruma::{EventId, OwnedUserId};
use matrix_sdk::{Client, Room, RoomState};
use tracing::{error, info, warn};
use twalk_sensor::config::Config;
use twalk_sensor::consent::Consent;
use twalk_sensor::{network, normalize, outbound};

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
                publish_envelope(
                    &jetstream,
                    normalize::MESSAGE_RECEIVED_TYPE,
                    &envelope,
                    network,
                    consent,
                )
                .await;
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
                publish_envelope(
                    &jetstream,
                    normalize::REACTION_ADDED_TYPE,
                    &envelope,
                    network,
                    consent,
                )
                .await;
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
                publish_envelope(
                    &jetstream,
                    normalize::PRESENCE_UPDATED_TYPE,
                    &envelope,
                    network,
                    consent,
                )
                .await;
            }
        });
    }

    // Outbound: approved replies flow back from the bus into the portal
    // rooms. Runs concurrently with the sync loop, which feeds the client
    // the room knowledge the send path needs.
    {
        let client = client.clone();
        let jetstream = jetstream.clone();
        let retry_base = config.send_retry_base;
        let max_attempts = config.send_retry_max_attempts;
        tokio::spawn(async move {
            consume_approved_replies(client, jetstream, retry_base, max_attempts).await;
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

/// Publishes a CloudEvents envelope on the bus with the contract's headers:
/// NATS-Msg-Id (the JetStream dedup anchor) plus the network/consent
/// extensions duplicated for server-side filtering.
async fn publish_envelope(
    jetstream: &async_nats::jetstream::Context,
    event_type: &str,
    envelope: &serde_json::Value,
    network: network::Network,
    consent: Consent,
) {
    let id = envelope["id"].as_str().unwrap().to_owned();
    let mut headers = async_nats::header::HeaderMap::new();
    headers.insert(async_nats::header::NATS_MESSAGE_ID, id.as_str());
    headers.insert("network", network.as_str());
    headers.insert("consent", consent.as_str());
    let payload = serde_json::to_vec(envelope).expect("the envelope is serializable");
    let subject = normalize::bus_subject(event_type);
    match jetstream
        .publish_with_headers(subject, headers, payload.into())
        .await
    {
        Ok(ack) => match ack.await {
            Ok(_) => info!(%id, "published {}", event_type),
            Err(error) => warn!(%id, %error, "publish ack failed"),
        },
        Err(error) => warn!(%id, %error, "publish failed"),
    }
}

/// How long to wait before rebuilding a failed or ended approved-reply
/// consumer.
const CONSUMER_RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// Durably consumes `twalk.persona.reply.approved.v1` and posts each approved
/// reply into its target portal room. A message is acked only after a
/// successful post; a failed send is redelivered with an exponential backoff
/// (NAK with delay, driven by the JetStream delivered count), and once the
/// delivery attempts are exhausted the event moves to the dead-letter
/// subject — an approved reply is never silently dropped.
///
/// Never returns: if the consumer fails to build or its message stream ends,
/// it is rebuilt after a short delay — approved replies must keep flowing
/// for as long as the Sensor runs.
async fn consume_approved_replies(
    client: Client,
    jetstream: async_nats::jetstream::Context,
    retry_base: Duration,
    max_attempts: i64,
) {
    loop {
        match run_approved_reply_consumer(&client, &jetstream, retry_base, max_attempts).await {
            Ok(()) => error!("the approved-reply message stream ended; rebuilding the consumer"),
            Err(error) => error!(%error, "the approved-reply consumer failed; rebuilding it"),
        }
        tokio::time::sleep(CONSUMER_RECONNECT_DELAY).await;
    }
}

/// One incarnation of the approved-reply consumer: builds the durable pull
/// consumer and processes its messages until the stream ends.
async fn run_approved_reply_consumer(
    client: &Client,
    jetstream: &async_nats::jetstream::Context,
    retry_base: Duration,
    max_attempts: i64,
) -> Result<()> {
    let stream = jetstream
        .get_stream(normalize::STREAM_NAME)
        .await
        .context("failed to get the twalk stream")?;
    let consumer = stream
        .get_or_create_consumer(
            outbound::REPLY_CONSUMER,
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some(outbound::REPLY_CONSUMER.to_owned()),
                filter_subject: normalize::bus_subject(outbound::REPLY_APPROVED_TYPE),
                ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                ..Default::default()
            },
        )
        .await
        .context("failed to ensure the approved-reply consumer")?;
    let dead_letter_subject = outbound::dead_letter_subject();
    info!(
        consumer = outbound::REPLY_CONSUMER,
        "consuming approved replies"
    );

    let mut messages = consumer
        .messages()
        .await
        .context("failed to open the approved-reply message stream")?;
    while let Some(message) = messages.next().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                warn!(%error, "approved-reply stream error, continuing");
                continue;
            }
        };
        let delivered = match message.info() {
            Ok(info) => info.delivered,
            Err(error) => {
                // The backoff schedule is driven by the delivered count;
                // without it, assume the first attempt — and say so, so the
                // restart of the schedule is never silent.
                warn!(%error, "no delivery info on an approved reply, assuming the first attempt");
                1
            }
        };
        let job = match serde_json::from_slice::<serde_json::Value>(&message.message.payload)
            .context("payload is not valid JSON")
            .and_then(|event| outbound::ApprovedReply::parse(&event))
        {
            Ok(job) => job,
            Err(error) => {
                // A malformed event can never be delivered: dead-letter it
                // on the spot instead of burning retries.
                error!(%error, "unusable persona.reply.approved event, dead-lettering");
                dead_letter(&jetstream, &dead_letter_subject, &message).await;
                continue;
            }
        };
        match post_approved_reply(&client, &job).await {
            Ok(()) => {
                if let Err(error) = message.ack().await {
                    warn!(id = %job.event_id, %error, "ack failed after a successful post");
                }
                info!(id = %job.event_id, room = %job.room_id, "posted approved reply");
            }
            Err(PostError::Permanent(error)) => {
                error!(id = %job.event_id, room = %job.room_id, %error, "approved reply can never be posted, dead-lettering");
                dead_letter(&jetstream, &dead_letter_subject, &message).await;
            }
            Err(PostError::Transient(error)) if delivered >= max_attempts => {
                error!(id = %job.event_id, room = %job.room_id, %error, %delivered, "approved reply exhausted its retries, dead-lettering");
                dead_letter(&jetstream, &dead_letter_subject, &message).await;
            }
            Err(PostError::Transient(error)) => {
                let delay = outbound::retry_delay(retry_base, delivered);
                warn!(id = %job.event_id, room = %job.room_id, %error, %delivered, ?delay, "approved reply send failed, scheduling a retry");
                if let Err(error) = message.ack_with(AckKind::Nak(Some(delay))).await {
                    error!(id = %job.event_id, %error, "nak failed, the message will be redelivered at the ack deadline");
                }
            }
        }
    }
    Ok(())
}

/// Publishes an undeliverable event to the dead-letter subject and acks the
/// original. The dead-letter copy keeps the event id as `NATS-Msg-Id` so
/// bus-level dedup still works, and duplicates the event's `network` and
/// `consent` extensions as headers, like the inbound path does. If the
/// publish itself fails the message stays unacked, so it is redelivered
/// while attempts remain rather than disappearing.
async fn dead_letter(
    jetstream: &async_nats::jetstream::Context,
    subject: &str,
    message: &async_nats::jetstream::Message,
) {
    let mut headers = async_nats::header::HeaderMap::new();
    if let Ok(event) = serde_json::from_slice::<serde_json::Value>(&message.message.payload) {
        if let Some(id) = event.get("id").and_then(serde_json::Value::as_str) {
            headers.insert(async_nats::header::NATS_MESSAGE_ID, id);
        }
        for extension in ["network", "consent"] {
            if let Some(value) = event.get(extension).and_then(serde_json::Value::as_str) {
                headers.insert(extension, value);
            }
        }
    }
    match jetstream
        .publish_with_headers(subject.to_owned(), headers, message.message.payload.clone())
        .await
    {
        Ok(ack) => match ack.await {
            Ok(_) => {
                if let Err(error) = message.ack().await {
                    error!(%error, "ack failed after dead-lettering, a duplicate may be dead-lettered again");
                }
            }
            Err(error) => error!(%error, "dead-letter publish ack failed, leaving the message unacked"),
        },
        Err(error) => error!(%error, "dead-letter publish failed, leaving the message unacked"),
    }
}

/// A send that can never succeed (malformed target, content the Sensor cannot
/// render) is permanent; anything else may succeed on a later attempt, e.g.
/// once the Sensor has joined the target room.
enum PostError {
    Permanent(anyhow::Error),
    Transient(anyhow::Error),
}

/// Posts one approved reply into its target room, as a native reply to the
/// original message when the approval names one.
async fn post_approved_reply(client: &Client, job: &outbound::ApprovedReply) -> Result<(), PostError> {
    // Deliberate v1 limitation, mirroring the inbound text-only skeleton:
    // only text/plain is posted; markdown and HTML dead-letter as permanent
    // failures until rich formatting is specced for outbound.
    if job.format != "text/plain" {
        return Err(PostError::Permanent(anyhow!(
            "unsupported final format {}",
            job.format
        )));
    }
    let room_id = matrix_sdk::ruma::RoomId::parse(&job.room_id)
        .map_err(|error| PostError::Permanent(anyhow!(error).context("invalid target room id")))?;
    let Some(room) = client.get_room(&room_id) else {
        return Err(PostError::Transient(anyhow!(
            "the sensor is not a member of the target room"
        )));
    };
    if room.state() != RoomState::Joined {
        return Err(PostError::Transient(anyhow!(
            "the sensor has not joined the target room"
        )));
    }
    let mut content = RoomMessageEventContent::text_plain(job.body.clone());
    if let Some(reply_to) = &job.reply_to_event_id {
        let event_id = matrix_sdk::ruma::EventId::parse(reply_to)
            .map_err(|error| PostError::Permanent(anyhow!(error).context("invalid reply target")))?;
        content.relates_to = Some(Relation::Reply(Reply::with_event_id(event_id)));
    }
    room.send_queue()
        .send(content.into())
        .await
        .map_err(|error| PostError::Transient(anyhow!(error).context("matrix send failed")))?;
    Ok(())
}
