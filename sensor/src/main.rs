//! The Twalk Sensor binary. All the decision logic lives in the library
//! modules; this file only wires them to matrix-sdk and NATS JetStream.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_nats::jetstream::AckKind;
use futures::StreamExt;
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::deserialized_responses::RawAnySyncOrStrippedState;
use matrix_sdk::encryption::{BackupDownloadStrategy, EncryptionSettings};
use matrix_sdk::ruma::api::error::ErrorKind;
use matrix_sdk::ruma::events::presence::PresenceEvent;
use matrix_sdk::ruma::events::reaction::OriginalSyncReactionEvent;
use matrix_sdk::ruma::events::relation::Reply;
use matrix_sdk::ruma::events::room::encrypted::OriginalSyncRoomEncryptedEvent;
use matrix_sdk::ruma::events::room::member::{MembershipState, StrippedRoomMemberEvent};
use matrix_sdk::ruma::events::room::message::{
    MessageType, OriginalSyncRoomMessageEvent, Relation, RoomMessageEventContent,
};
use matrix_sdk::ruma::events::room::MediaSource;
use matrix_sdk::ruma::events::{
    AnySyncMessageLikeEvent, AnySyncTimelineEvent, SyncMessageLikeEvent,
};
use matrix_sdk::ruma::{EventId, OwnedEventId, OwnedTransactionId, OwnedUserId, UInt};
use matrix_sdk::{Client, LoopCtrl, Room, RoomState};
use tracing::{error, info, warn};
use twalk_sensor::config::Config;
use twalk_sensor::consent::{Consent, ConsentCache, ConsentSnapshotSource};
use twalk_sensor::metrics::Metrics;
use twalk_sensor::{consent, network, normalize, outbound};

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    tracing_subscriber::fmt()
        .with_env_filter(&config.log_level)
        .init();
    info!(homeserver = %config.homeserver_url, user = %config.user_id, "sensor starting");

    // Observability (ticket 10): one shared metrics registry, optionally
    // served over HTTP in the Prometheus text format. Binding fails fast and
    // loud — a configured-but-unusable endpoint is an operator error to fix,
    // not a condition to swallow.
    let metrics = Arc::new(Metrics::new());
    if let Some(listen) = config.metrics_listen {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .with_context(|| format!("failed to bind the metrics endpoint on {listen}"))?;
        info!(%listen, "serving metrics");
        tokio::spawn(serve_metrics(listener, metrics.clone()));
    }
    // In-flight publishes are spawned through this tracker: a graceful
    // shutdown drains them before exiting instead of cutting them off.
    let publish_tracker = PublishTracker::default();

    // Persistence (ticket 03): with SENSOR_STATE_DIR set, the SDK's state
    // and crypto stores live in that directory (sqlite), so the sync token
    // survives restarts and the sync loop resumes where it stopped instead
    // of re-syncing (and re-emitting) the recent timeline. The stores are
    // not encrypted at rest: the directory is the operator's to protect
    // (volume permissions, disk encryption). Without SENSOR_STATE_DIR the
    // Sensor keeps the in-memory behaviour: a fresh login and initial sync
    // on every start.
    //
    // Encryption (ticket 04): all cryptography is delegated to the SDK's
    // crypto crate — the Sensor implements no primitive itself.
    // Cross-signing is bootstrapped automatically when the account has none
    // (the first password login carries the UIAA credentials for it); a
    // server-side key backup is created when none exists, so room keys
    // survive a device replacement; and when a backup key is later restored
    // through the recovery key, the backed-up room keys are downloaded in
    // one shot — the Sensor's rooms are few and the download is bounded.
    let encryption_settings = EncryptionSettings {
        auto_enable_cross_signing: true,
        auto_enable_backups: true,
        backup_download_strategy: BackupDownloadStrategy::OneShot,
    };

    // With a persisted store, a fresh password login on every start would
    // mint a new device each time — growing the account's device list and
    // resetting the crypto identity the crypto store was persisted for. The
    // session is therefore kept in `session.json` in the state directory:
    // restore it when present, log in otherwise and persist the new session
    // for the next start. Restoring also reloads the persisted sync token,
    // which `SyncSettings::default()` (SyncToken::ReusePrevious) picks up.
    //
    // Whether the persisted session is usable is decided before the client
    // opens the store (issue #28): the crypto store belongs to the session's
    // device, and matrix-sdk refuses to open it for the new device a fresh
    // login mints. So when the session is missing, unparseable or its token
    // was revoked, the stale crypto store is moved aside first and the client
    // starts on a clean one (the state store and its sync token are kept).
    let session_file = config
        .state_dir
        .as_ref()
        .map(|dir| dir.join("session.json"));
    let mut session = session_file.as_deref().and_then(load_session);
    if let Some(persisted) = &session {
        if access_token_revoked(&config.homeserver_url, &persisted.tokens.access_token).await {
            warn!(
                device_id = %persisted.meta.device_id,
                "the persisted access token was revoked (M_UNKNOWN_TOKEN), falling back to a fresh login"
            );
            session = None;
        }
    }
    if session.is_none() {
        if let Some(state_dir) = &config.state_dir {
            set_stale_store_aside(state_dir).context("failed to move the stale store aside")?;
        }
    }

    let client = match &config.state_dir {
        Some(state_dir) => {
            Client::builder()
                .homeserver_url(&config.homeserver_url)
                .sqlite_store(state_dir, None)
                .with_encryption_settings(encryption_settings)
                .build()
                .await?
        }
        None => {
            Client::builder()
                .homeserver_url(&config.homeserver_url)
                .with_encryption_settings(encryption_settings)
                .build()
                .await?
        }
    };

    // A session that parses but fails to restore is fatal: it points at
    // store corruption the operator should see, and logging in past a
    // half-restored session is not safe (the SDK refuses to set
    // authentication data twice).
    if let Some(session) = session {
        client
            .restore_session(session)
            .await
            .context("failed to restore the persisted session")?;
        info!("restored the persisted session");
    } else {
        client
            .matrix_auth()
            .login_username(&config.user_id, &config.password)
            .initial_device_display_name("twalk-sensor")
            .send()
            .await
            .context("matrix login failed")?;
        info!("logged in to the homeserver");
        if let Some(session_file) = &session_file {
            let session = client
                .matrix_auth()
                .session()
                .expect("a session exists right after login");
            write_private_file(session_file, &serde_json::to_vec(&session)?)
                .context("failed to persist the session")?;
        }
    }

    // Cryptographic identity bootstrap (ticket 04). Let the automatic
    // cross-signing/bootstrap tasks settle first, then, when the operator
    // configured SENSOR_RECOVERY_KEY, open the account's secret storage with
    // it and import what it holds: the cross-signing private keys (so this
    // device is the same identity, not a new one) and the key-backup
    // decryption key, which triggers the one-shot download of the backed-up
    // room keys — this is what lets a replacement device read history. A
    // failed recovery (wrong key, no secret storage on the account) is
    // logged loudly but is not fatal: live traffic still decrypts, senders
    // share Megolm keys with the new device directly.
    client
        .encryption()
        .wait_for_e2ee_initialization_tasks()
        .await;
    if let Some(recovery_key) = &config.recovery_key {
        let recovery = client.encryption().recovery();
        match recovery.recover_and_fix_backup(recovery_key).await {
            Ok(()) => info!(
                state = ?recovery.state(),
                "recovered the cryptographic identity from the recovery key"
            ),
            Err(error) => error!(
                %error,
                "recovery with SENSOR_RECOVERY_KEY failed; continuing with the local device identity only"
            ),
        }
    }

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

    // Consent labelling (ticket 05): every published event carries the
    // sender's current consent state from this cache, fed by the durable
    // consent.state.changed consumer spawned below. The initial snapshot
    // fetch sits behind the ConsentSnapshotSource trait; no Companion
    // Gateway exists yet (ADR 0006), so the no-op source is wired in and
    // every sender starts out pending. The Sensor never writes consent
    // state.
    let consent_cache = ConsentCache::default();
    for change in consent::NoConsentSnapshot.fetch_snapshot().await {
        consent_cache.apply(&change);
    }
    {
        let jetstream = jetstream.clone();
        let consent_cache = consent_cache.clone();
        tokio::spawn(async move {
            consume_consent_changes(jetstream, consent_cache).await;
        });
    }

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

    // Inbound messages: normalize and publish. Text, media (image, video,
    // audio, file), sticker (relayed by some bridges as an m.room.message
    // msgtype) and location shapes produce events; other msgtypes (notices,
    // emotes, verification requests, ...) have no v1 shape and are skipped.
    {
        let jetstream = jetstream.clone();
        let own_user = own_user.clone();
        let consent_cache = consent_cache.clone();
        let publish_tracker = publish_tracker.clone();
        let metrics = metrics.clone();
        client.add_event_handler(move |event: OriginalSyncRoomMessageEvent, room: Room, _client: Client| {
            let jetstream = jetstream.clone();
            let own_user = own_user.clone();
            let consent_cache = consent_cache.clone();
            let publish_tracker = publish_tracker.clone();
            let metrics = metrics.clone();
            async move {
                if event.sender == own_user {
                    return; // never loop on our own outbound traffic
                }
                if let Some(Relation::Replacement(replacement)) = &event.content.relates_to {
                    // An edit is a new event (`* new text` fallback body)
                    // replacing an earlier one: no v1 event type exists for
                    // it, and publishing it would read as a fresh message.
                    tracing::debug!(
                        room = %room.room_id(),
                        event_id = %event.event_id,
                        replaces = %replacement.event_id,
                        "skipping message edit, no v1 event type"
                    );
                    return;
                }
                let Some(attachments) = attachments_for(&event.content.msgtype) else {
                    return; // no v1 shape for this msgtype
                };
                let body = event.content.body().to_owned();
                let sender: OwnedUserId = event.sender.clone();
                let bridge_contents = room_bridge_contents(&room).await;
                let Some(network) = network::resolve(&bridge_contents, sender.localpart()) else {
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
                let consent = consent_cache.state(sender.as_str(), network);
                // The native network identifier is contact PII: derived
                // here, but published only for a granted contact (the
                // builders enforce the gate).
                let network_identifier =
                    network::ghost_network_identifier(network, sender.localpart());
                let (reply_target, thread_root) = relation_targets(&event.content);
                let reply_to = match reply_target {
                    Some(parent_id) => Some(normalize::ReplyTo {
                        matrix_event_id: parent_id.to_string(),
                        // An unreachable parent is not an error: the reply
                        // still publishes, with an empty excerpt.
                        excerpt: target_excerpt(&room, &parent_id).await.unwrap_or_default(),
                    }),
                    None => None,
                };
                let input = normalize::InboundMessage {
                    matrix_event_id: event.event_id.to_string(),
                    matrix_room_id: room.room_id().to_string(),
                    server_name: own_user.server_name().as_str().to_owned(),
                    sender: sender.to_string(),
                    body,
                    network,
                    consent,
                    display_name,
                    network_identifier,
                    reply_to,
                    thread_root: thread_root.map(|event_id| event_id.to_string()),
                    attachments,
                    produced_at: rfc3339(std::time::SystemTime::now()),
                    // Bridge traffic carries the original network time in
                    // origin_server_ts (mautrix massages it through the
                    // appservice ts override). The network only resolves
                    // for bridge traffic — a portal m.bridge state event or
                    // a ghost sender — so reaching this point means the
                    // timestamp is the network's; plain Matrix traffic
                    // never produces an event, and the homeserver's receive
                    // time never masquerades as one.
                    network_timestamp: Some(rfc3339_ms(u64::from(event.origin_server_ts.0))),
                };
                let envelope = normalize::build_message_received(&input);
                publish_tracker
                    .publish(
                        jetstream,
                        normalize::MESSAGE_RECEIVED_TYPE,
                        envelope,
                        network,
                        consent,
                        metrics,
                    )
                    .await;
            }
        });
    }

    // Decryption failures (ticket 04). matrix-sdk-crypto re-types an event
    // it decrypted to its inner type, so an m.room.encrypted event that
    // still reaches the handlers is one the crypto stack could not decrypt.
    // It is logged, counted and skipped — never fatal, never blocking the
    // other rooms. A key that arrives later does not re-dispatch the event
    // (no event cache), so a skipped event stays unpublished; portal rooms
    // share keys at send time, so live traffic does not hit this.
    {
        let metrics = metrics.clone();
        client.add_event_handler(move |event: OriginalSyncRoomEncryptedEvent, room: Room| {
            let metrics = metrics.clone();
            async move {
                let failures = metrics.record_decryption_failure();
                warn!(
                    room = %room.room_id(),
                    event_id = %event.event_id,
                    sender = %event.sender,
                    failures,
                    "cannot decrypt event, skipping it"
                );
            }
        });
    }

    // Inbound reactions: normalize and publish. Reaction removals arrive as
    // redactions, never as m.reaction events, so they produce no event, per
    // the contract.
    {
        let jetstream = jetstream.clone();
        let own_user = own_user.clone();
        let consent_cache = consent_cache.clone();
        let publish_tracker = publish_tracker.clone();
        let metrics = metrics.clone();
        client.add_event_handler(move |event: OriginalSyncReactionEvent, room: Room, _client: Client| {
            let jetstream = jetstream.clone();
            let own_user = own_user.clone();
            let consent_cache = consent_cache.clone();
            let publish_tracker = publish_tracker.clone();
            let metrics = metrics.clone();
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
                let consent = consent_cache.state(reactor.as_str(), network);
                let network_identifier =
                    network::ghost_network_identifier(network, reactor.localpart());
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
                    network_identifier,
                    produced_at: rfc3339(std::time::SystemTime::now()),
                    // Bridges report network timestamps in bridge-specific
                    // fields; mapping them arrives with the enrichment work.
                    network_timestamp: None,
                };
                let envelope = normalize::build_reaction_added(&input);
                publish_tracker
                    .publish(
                        jetstream,
                        normalize::REACTION_ADDED_TYPE,
                        envelope,
                        network,
                        consent,
                        metrics,
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
        let consent_cache = consent_cache.clone();
        let publish_tracker = publish_tracker.clone();
        let metrics = metrics.clone();
        client.add_event_handler(move |event: PresenceEvent, client: Client| {
            let jetstream = jetstream.clone();
            let own_user = own_user.clone();
            let consent_cache = consent_cache.clone();
            let publish_tracker = publish_tracker.clone();
            let metrics = metrics.clone();
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
                let consent = consent_cache.state(sender.as_str(), network);
                let network_identifier =
                    network::ghost_network_identifier(network, sender.localpart());
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
                    network_identifier,
                    produced_at: rfc3339_ms(receipt_timestamp_ms),
                    receipt_timestamp_ms,
                    last_active_at,
                };
                let envelope = normalize::build_presence_updated(&input);
                publish_tracker
                    .publish(
                        jetstream,
                        normalize::PRESENCE_UPDATED_TYPE,
                        envelope,
                        network,
                        consent,
                        metrics,
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
        let metrics = metrics.clone();
        tokio::spawn(async move {
            consume_approved_replies(client, jetstream, retry_base, max_attempts, metrics).await;
        });
    }

    info!("sensor running");
    // The sync callback runs once per completed sync response: it drives the
    // sync-age gauge (the operator's lag signal). Boxed so the shutdown path
    // can drop the loop itself, not just a pinned reference to it.
    let sync_metrics = metrics.clone();
    let mut sync = Box::pin(
        client.sync_with_callback(SyncSettings::default(), move |_response| {
            sync_metrics.record_sync(now_unix_seconds());
            async { LoopCtrl::Continue }
        }),
    );
    tokio::select! {
        result = &mut sync => {
            result.context("sync loop failed")?;
        }
        _ = shutdown_signal() => {
            // Dropping the sync future stops the loop; publishes already in
            // flight live in the tracker (spawned, not awaited inline) and
            // are drained below. The bus consumers need no draining: a
            // message they leave unacked is redelivered after the ack
            // deadline, so at-least-once holds across the restart.
            info!("shutdown signal received, draining in-flight work");
            drop(sync);
            if publish_tracker.wait_for_idle(Duration::from_secs(5)).await {
                info!("in-flight publishes drained, shutting down");
            } else {
                warn!("shutdown timed out with publishes still in flight; the events stay dedup-able on the bus");
            }
        }
    }
    Ok(())
}

/// Resolves when the process is asked to stop (SIGTERM, or SIGINT from an
/// interactive operator).
async fn shutdown_signal() {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("installing a SIGTERM handler never fails");
    tokio::select! {
        _ = sigterm.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// Serves the Prometheus text exposition over HTTP/1.1, one connection at a
/// time, any path — the endpoint has exactly one document.
async fn serve_metrics(listener: tokio::net::TcpListener, metrics: Arc<Metrics>) {
    loop {
        match listener.accept().await {
            Ok((mut socket, _peer)) => {
                let metrics = metrics.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    // Drain the request head first (bounded); responding
                    // without reading risks an RST that discards the answer.
                    let mut request = Vec::with_capacity(1024);
                    let mut chunk = [0u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n")
                        && request.len() < 8192
                    {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(read) => request.extend_from_slice(&chunk[..read]),
                        }
                    }
                    let body = metrics.render(now_unix_seconds());
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/plain; version=0.0.4; charset=utf-8\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
            Err(error) => warn!(%error, "metrics accept failed"),
        }
    }
}

/// Counts publishes that are in flight (spawned, not yet acked) so a graceful
/// shutdown can drain them instead of cutting them off mid-request.
#[derive(Clone, Default)]
struct PublishTracker {
    in_flight: Arc<AtomicU64>,
    idle: Arc<tokio::sync::Notify>,
}

impl PublishTracker {
    /// Publishes through a detached task, awaited here: normal operation
    /// keeps the handler's ordering, while a shutdown that drops the handler
    /// futures leaves the publish running to completion.
    async fn publish(
        &self,
        jetstream: async_nats::jetstream::Context,
        event_type: &'static str,
        envelope: serde_json::Value,
        network: network::Network,
        consent: Consent,
        metrics: Arc<Metrics>,
    ) {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        let tracker = self.clone();
        let task = tokio::spawn(async move {
            publish_envelope(
                &jetstream, event_type, &envelope, network, consent, &metrics,
            )
            .await;
            if tracker.in_flight.fetch_sub(1, Ordering::Relaxed) == 1 {
                tracker.idle.notify_waiters();
            }
        });
        let _ = task.await;
    }

    /// True once no publish is in flight; false when the deadline expired.
    async fn wait_for_idle(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.idle.notified();
            if self.in_flight.load(Ordering::Relaxed) == 0 {
                return true;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.in_flight.load(Ordering::Relaxed) == 0;
            }
        }
    }
}

/// Loads the Matrix session persisted in `session_file` (ticket 03).
/// Returns None — and the caller logs in fresh — when there is no file or
/// the file is unreadable or unparseable.
fn load_session(session_file: &Path) -> Option<MatrixSession> {
    if !session_file.is_file() {
        return None;
    }
    let session = std::fs::read_to_string(session_file)
        .with_context(|| format!("failed to read {}", session_file.display()))
        .and_then(|raw| {
            serde_json::from_str::<MatrixSession>(&raw)
                .with_context(|| format!("failed to parse {}", session_file.display()))
        });
    match session {
        Ok(session) => Some(session),
        Err(error) => {
            warn!(
                error = format!("{error:#}"),
                "persisted session is unusable, falling back to a fresh login"
            );
            None
        }
    }
}

/// True only when the homeserver positively rejects the access token with
/// `M_UNKNOWN_TOKEN` (device deleted, password changed, logged out). Any
/// other outcome — valid token, homeserver unreachable, unexpected answer —
/// keeps the persisted session: a transient failure must never cost the
/// device its crypto store.
async fn access_token_revoked(homeserver_url: &str, access_token: &str) -> bool {
    let url = format!(
        "{}/_matrix/client/v3/account/whoami",
        homeserver_url.trim_end_matches('/')
    );
    let response = matrix_sdk::reqwest::Client::new()
        .get(url)
        .bearer_auth(access_token)
        .timeout(Duration::from_secs(30))
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            warn!(%error, "cannot check the persisted access token, restoring the session as is");
            return false;
        }
    };
    if response.status() != matrix_sdk::reqwest::StatusCode::UNAUTHORIZED {
        return false;
    }
    let body = response.text().await.unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .is_some_and(|error| error["errcode"] == "M_UNKNOWN_TOKEN")
}

/// The crypto store matrix-sdk's sqlite backend keeps in the state
/// directory; its `-wal`/`-shm` companions share this prefix.
const CRYPTO_STORE_FILE: &str = "matrix-sdk-crypto.sqlite3";

/// Moves the previous device's crypto store (and its dead session file)
/// into a timestamped `stale-store-*` subdirectory of `state_dir`, so the
/// new device starts on a clean crypto store (issue #28). Nothing is
/// deleted: the operator decides what to do with the old store.
///
/// Only the crypto store is bound to the device: in matrix-sdk 0.19 the
/// account check (`CryptoStoreError::MismatchedAccount`) lives in the
/// `OlmMachine` alone, while the state and event-cache stores are opened and
/// reloaded (rooms, sync token) on login and restore alike without any
/// user/device check. They are therefore kept, so the sync resumes from the
/// persisted token and the recent timeline is not re-emitted on the bus.
///
/// The subdirectory stays inside `state_dir` because that is typically a
/// volume mount point, which cannot be renamed itself, and a rename within
/// it never crosses filesystems.
fn set_stale_store_aside(state_dir: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(state_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to list {}", state_dir.display()))
        }
    };
    let mut stale = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(CRYPTO_STORE_FILE) || name_str == "session.json" {
            stale.push(name);
        }
    }
    if !stale
        .iter()
        .any(|name| name.to_string_lossy().starts_with(CRYPTO_STORE_FILE))
    {
        return Ok(()); // no crypto store yet: a first start, nothing to set aside
    }
    let stamp = now_unix_seconds();
    let mut aside = state_dir.join(format!("stale-store-{stamp}"));
    let mut suffix = 1;
    while aside.exists() {
        aside = state_dir.join(format!("stale-store-{stamp}-{suffix}"));
        suffix += 1;
    }
    std::fs::create_dir(&aside).with_context(|| format!("failed to create {}", aside.display()))?;
    for name in &stale {
        std::fs::rename(state_dir.join(name), aside.join(name))
            .with_context(|| format!("failed to move {} aside", name.to_string_lossy()))?;
    }
    warn!(
        moved_to = %aside.display(),
        "the persisted session is unusable: moved the previous device's crypto store aside and logging in as a \
         new device on a clean crypto store. The state store and sync token are kept, so nothing is re-emitted. \
         Megolm sessions held only by the old device cannot be decrypted by the new one unless \
         SENSOR_RECOVERY_KEY restores the key backup. Delete the moved directory once it is no longer needed."
    );
    Ok(())
}

/// Writes a file readable by its owner only — the session file holds an
/// access token. The write is atomic: the contents go to a temporary file
/// in the same directory, are flushed to disk, then renamed over `path`, so
/// a crash leaves either the old file or the new one, never a partial one.
fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        // Persist the rename itself (directory entry) where supported.
        if let Ok(dir) = std::fs::File::open(dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
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

/// Reads the contents of every `m.bridge` state event of the room (the IO),
/// the mautrix portal markers identifying the network. Mautrix keys the
/// marker with the bridge's unique id (`<homeserver domain>/<appservice id>`
/// in bridgev2), so it is read regardless of state key; ordering by state
/// key keeps the choice deterministic when several bridges marked the room.
async fn room_bridge_contents(room: &Room) -> Vec<serde_json::Value> {
    let mut markers: Vec<(String, serde_json::Value)> = room
        .get_state_events("m.bridge".into())
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|raw| {
            let json = match raw {
                RawAnySyncOrStrippedState::Sync(raw) => raw.json().get(),
                RawAnySyncOrStrippedState::Stripped(raw) => raw.json().get(),
            };
            let mut event = serde_json::from_str::<serde_json::Value>(json).ok()?;
            let state_key = event.get("state_key")?.as_str()?.to_owned();
            Some((state_key, event.get_mut("content")?.take()))
        })
        .collect();
    markers.sort_by(|(left, _), (right, _)| left.cmp(right));
    markers.into_iter().map(|(_, content)| content).collect()
}

/// Defers to the pure attribution policy in `network::resolve`.
async fn resolve_network(room: &Room, sender: &OwnedUserId) -> Option<network::Network> {
    network::resolve(&room_bridge_contents(room).await, sender.localpart())
}

/// Splits `m.relates_to` into the contract's reply target and thread root.
/// A threaded message's fallback `m.in_reply_to` (`is_falling_back: true`)
/// exists only for thread-unaware clients and is not a real reply.
fn relation_targets(
    content: &RoomMessageEventContent,
) -> (Option<OwnedEventId>, Option<OwnedEventId>) {
    match &content.relates_to {
        Some(Relation::Reply(reply)) => (Some(reply.in_reply_to.event_id.clone()), None),
        Some(Relation::Thread(thread)) => {
            let reply = match (&thread.in_reply_to, thread.is_falling_back) {
                (Some(in_reply_to), false) => Some(in_reply_to.event_id.clone()),
                _ => None,
            };
            (reply, Some(thread.event_id.clone()))
        }
        _ => (None, None),
    }
}

/// The contract requires both pixel dimensions, each at least 1 pixel.
fn dimensions(width: Option<UInt>, height: Option<UInt>) -> Option<(u64, u64)> {
    let (width, height) = (u64::from(width?), u64::from(height?));
    (width >= 1 && height >= 1).then_some((width, height))
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Maps an observed msgtype to its contract attachments, or None when the
/// msgtype produces no v1 event. Attachments are `mxc://` references into
/// Matrix media storage: the binary is never downloaded.
fn attachments_for(msgtype: &MessageType) -> Option<Vec<normalize::Attachment>> {
    let mxc_uri = |source: &MediaSource| match source {
        MediaSource::Plain(mxc) => mxc.to_string(),
        MediaSource::Encrypted(file) => file.url.to_string(),
    };
    let attachment = match msgtype {
        MessageType::Text(_) => return Some(Vec::new()),
        MessageType::Image(image) => normalize::Attachment {
            kind: normalize::AttachmentKind::Image,
            mxc_uri: mxc_uri(&image.source),
            mime_type: image.info.as_deref().and_then(|info| info.mimetype.clone()),
            size_bytes: image
                .info
                .as_deref()
                .and_then(|info| info.size.map(u64::from)),
            caption: image.caption().map(str::to_owned),
            dimensions: image
                .info
                .as_deref()
                .and_then(|info| dimensions(info.width, info.height)),
            duration_ms: None,
        },
        MessageType::Video(video) => normalize::Attachment {
            kind: normalize::AttachmentKind::Video,
            mxc_uri: mxc_uri(&video.source),
            mime_type: video.info.as_deref().and_then(|info| info.mimetype.clone()),
            size_bytes: video
                .info
                .as_deref()
                .and_then(|info| info.size.map(u64::from)),
            caption: video.caption().map(str::to_owned),
            dimensions: video
                .info
                .as_deref()
                .and_then(|info| dimensions(info.width, info.height)),
            duration_ms: video
                .info
                .as_deref()
                .and_then(|info| info.duration.map(duration_ms)),
        },
        MessageType::Audio(audio) => normalize::Attachment {
            kind: normalize::AttachmentKind::Audio,
            mxc_uri: mxc_uri(&audio.source),
            mime_type: audio.info.as_deref().and_then(|info| info.mimetype.clone()),
            size_bytes: audio
                .info
                .as_deref()
                .and_then(|info| info.size.map(u64::from)),
            caption: audio.caption().map(str::to_owned),
            dimensions: None,
            duration_ms: audio
                .info
                .as_deref()
                .and_then(|info| info.duration.map(duration_ms)),
        },
        MessageType::File(file) => normalize::Attachment {
            kind: normalize::AttachmentKind::File,
            mxc_uri: mxc_uri(&file.source),
            mime_type: file.info.as_deref().and_then(|info| info.mimetype.clone()),
            size_bytes: file
                .info
                .as_deref()
                .and_then(|info| info.size.map(u64::from)),
            caption: file.caption().map(str::to_owned),
            dimensions: None,
            duration_ms: None,
        },
        // Geo messages carry no mxc URI and the contract's attachment shape
        // requires one: in v1 a location travels as the message body only.
        MessageType::Location(_) => return Some(Vec::new()),
        // Some bridges relay stickers as m.room.message with an m.sticker
        // msgtype; ruma leaves unknown msgtypes as raw content.
        MessageType::_Custom(_) if msgtype.msgtype() == "m.sticker" => {
            match normalize::attachment_from_sticker_data(msgtype.data().as_ref()) {
                Some(attachment) => attachment,
                // A sticker without a usable mxc URI still publishes as a
                // message (its body is the alt text), without an entry.
                None => return Some(Vec::new()),
            }
        }
        _ => return None,
    };
    Some(vec![attachment])
}

/// Fetches the target of a relation from the homeserver (via the SDK's
/// `Room::event`) and extracts a contract-capped excerpt of its body: the
/// plain text for a text message, the caption or filename for media, the
/// geo description for a location. An unreachable target is not an error —
/// the event still publishes, without the excerpt.
async fn target_excerpt(room: &Room, event_id: &EventId) -> Option<String> {
    let timeline_event = room.event(event_id, None).await.ok()?;
    let AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(
        SyncMessageLikeEvent::Original(message),
    )) = timeline_event.raw().deserialize().ok()?
    else {
        return None;
    };
    Some(normalize::excerpt(message.content.body()))
}

/// Publishes a CloudEvents envelope on the bus with the contract's headers:
/// NATS-Msg-Id (the JetStream dedup anchor) plus the network, consent and
/// traceparent extensions duplicated for server-side filtering.
async fn publish_envelope(
    jetstream: &async_nats::jetstream::Context,
    event_type: &str,
    envelope: &serde_json::Value,
    network: network::Network,
    consent: Consent,
    metrics: &Metrics,
) {
    let id = envelope["id"].as_str().unwrap().to_owned();
    let mut headers = async_nats::header::HeaderMap::new();
    headers.insert(async_nats::header::NATS_MESSAGE_ID, id.as_str());
    headers.insert("network", network.as_str());
    headers.insert("consent", consent.as_str());
    if let Some(traceparent) = envelope
        .get("traceparent")
        .and_then(serde_json::Value::as_str)
    {
        headers.insert("traceparent", traceparent);
    }
    let payload = serde_json::to_vec(envelope).expect("the envelope is serializable");
    let subject = normalize::bus_subject(event_type);
    match jetstream
        .publish_with_headers(subject, headers, payload.into())
        .await
    {
        Ok(ack) => match ack.await {
            Ok(_) => {
                metrics.record_published(event_type);
                info!(%id, "published {}", event_type);
            }
            Err(error) => warn!(%id, %error, "publish ack failed"),
        },
        Err(error) => warn!(%id, %error, "publish failed"),
    }
}

/// How long to wait before rebuilding a failed or ended durable consumer.
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
    metrics: Arc<Metrics>,
) {
    loop {
        match run_approved_reply_consumer(&client, &jetstream, retry_base, max_attempts, &metrics)
            .await
        {
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
    metrics: &Metrics,
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
                dead_letter(&jetstream, &dead_letter_subject, &message, metrics).await;
                continue;
            }
        };
        match post_approved_reply(&client, &job).await {
            Ok(()) => {
                if let Err(error) = message.ack().await {
                    warn!(id = %job.event_id, %error, "ack failed after a successful post");
                }
                info!(
                    id = %job.event_id,
                    room = %job.room_id,
                    traceparent = job.traceparent.as_deref(),
                    "posted approved reply"
                );
            }
            Err(PostError::Permanent(error)) => {
                metrics.record_outbound_send_failure();
                error!(id = %job.event_id, room = %job.room_id, %error, "approved reply can never be posted, dead-lettering");
                dead_letter(&jetstream, &dead_letter_subject, &message, metrics).await;
            }
            Err(PostError::Transient(error)) if delivered >= max_attempts => {
                metrics.record_outbound_send_failure();
                error!(id = %job.event_id, room = %job.room_id, %error, %delivered, "approved reply exhausted its retries, dead-lettering");
                dead_letter(&jetstream, &dead_letter_subject, &message, metrics).await;
            }
            Err(PostError::Transient(error)) => {
                metrics.record_outbound_send_failure();
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

/// Durably consumes `twalk.consent.state.changed.v1` and applies each
/// contact-scoped decision to the consent cache, so subsequent events label
/// the sender with the current state. Applying a decision is idempotent, so
/// an event is acked as soon as it is applied; a malformed or non-contact
/// event is acked and skipped — it can never become applicable, and
/// redelivering it would poison the consumer.
///
/// Never returns: if the consumer fails to build or its message stream
/// ends, it is rebuilt after a short delay — consent changes must keep
/// flowing for as long as the Sensor runs.
async fn consume_consent_changes(
    jetstream: async_nats::jetstream::Context,
    consent_cache: ConsentCache,
) {
    loop {
        match run_consent_consumer(&jetstream, &consent_cache).await {
            Ok(()) => error!("the consent-change message stream ended; rebuilding the consumer"),
            Err(error) => error!(%error, "the consent-change consumer failed; rebuilding it"),
        }
        tokio::time::sleep(CONSUMER_RECONNECT_DELAY).await;
    }
}

/// One incarnation of the consent-change consumer: builds the durable pull
/// consumer and applies its messages until the stream ends. A freshly
/// created consumer replays the whole consent history (the default
/// deliver-all policy); an existing one resumes from its ack position.
async fn run_consent_consumer(
    jetstream: &async_nats::jetstream::Context,
    consent_cache: &ConsentCache,
) -> Result<()> {
    let stream = jetstream
        .get_stream(normalize::STREAM_NAME)
        .await
        .context("failed to get the twalk stream")?;
    let consumer = stream
        .get_or_create_consumer(
            consent::CONSENT_CONSUMER,
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some(consent::CONSENT_CONSUMER.to_owned()),
                filter_subject: normalize::bus_subject(consent::CONSENT_CHANGED_TYPE),
                ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                ..Default::default()
            },
        )
        .await
        .context("failed to ensure the consent-change consumer")?;
    info!(
        consumer = consent::CONSENT_CONSUMER,
        "consuming consent changes"
    );

    let mut messages = consumer
        .messages()
        .await
        .context("failed to open the consent-change message stream")?;
    while let Some(message) = messages.next().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                warn!(%error, "consent-change stream error, continuing");
                continue;
            }
        };
        match serde_json::from_slice::<serde_json::Value>(&message.message.payload) {
            Ok(event) => match consent::ConsentChange::parse(&event) {
                Some(change) => {
                    consent_cache.apply(&change);
                    info!(
                        subject = %change.subject_id,
                        state = change.new_state.as_str(),
                        networks = ?change.networks,
                        "applied a consent change"
                    );
                }
                // A channel- or persona-scoped decision is well-formed
                // traffic that simply never labels a sender; a malformed
                // contact change is worth a warning.
                None => match event
                    .pointer("/data/subject/type")
                    .and_then(serde_json::Value::as_str)
                {
                    Some("contact") | None => warn!(
                        id = event
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("<none>"),
                        "unusable consent.state.changed event, skipping"
                    ),
                    Some(_) => {}
                },
            },
            Err(error) => {
                warn!(%error, "consent.state.changed payload is not valid JSON, skipping")
            }
        }
        if let Err(error) = message.ack().await {
            warn!(%error, "consent-change ack failed, the event will be redelivered");
        }
    }
    Ok(())
}

/// Publishes an undeliverable event to the dead-letter subject and acks the
/// original. The dead-letter copy gets its own stable `Nats-Msg-Id`, derived
/// from the event id (the approved reply itself is published under the event
/// id in the same stream, so reusing it would get the copy dropped as a
/// duplicate); the event id stays visible in the `event-id` header. The
/// event's `network`, `consent` and `traceparent` extensions are duplicated
/// as headers, like the inbound path does. If the publish itself fails the
/// message stays unacked, so it is redelivered while attempts remain rather
/// than disappearing.
async fn dead_letter(
    jetstream: &async_nats::jetstream::Context,
    subject: &str,
    message: &async_nats::jetstream::Message,
    metrics: &Metrics,
) {
    let mut headers = async_nats::header::HeaderMap::new();
    let event = serde_json::from_slice::<serde_json::Value>(&message.message.payload).ok();
    // A malformed event may carry no usable id: fall back to the id it was
    // published under, if any.
    let event_id = event
        .as_ref()
        .and_then(|event| event.get("id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            message
                .message
                .headers
                .as_ref()
                .and_then(|headers| headers.get(async_nats::header::NATS_MESSAGE_ID))
                .map(|id| id.as_str().to_owned())
        });
    if let Some(id) = &event_id {
        headers.insert(
            async_nats::header::NATS_MESSAGE_ID,
            outbound::dead_letter_msg_id(id).as_str(),
        );
        headers.insert(outbound::DEAD_LETTER_EVENT_ID_HEADER, id.as_str());
    }
    if let Some(event) = &event {
        for extension in ["network", "consent", "traceparent"] {
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
            Ok(ack) => {
                if ack.duplicate {
                    // The derived id is unique to this event's dead-letter
                    // copy: a duplicate means an earlier attempt already
                    // stored (and counted) it, but its ack of the original
                    // was lost. The copy is safe; just ack the original.
                    warn!(
                        id = event_id.as_deref(),
                        "dead-letter copy already stored, acking the redelivered original"
                    );
                } else {
                    metrics.record_dead_lettered();
                }
                if let Err(error) = message.ack().await {
                    error!(%error, "ack failed after dead-lettering, a duplicate may be dead-lettered again");
                }
            }
            Err(error) => {
                error!(%error, "dead-letter publish ack failed, leaving the message unacked")
            }
        },
        Err(error) => error!(%error, "dead-letter publish failed, leaving the message unacked"),
    }
}

/// A send that can never succeed (malformed target, content the Sensor cannot
/// render, a request the homeserver rejects as malformed) is permanent;
/// anything else may succeed on a later attempt, e.g. once the Sensor has
/// joined the target room or been granted the power to post in it (see
/// `classify_send_error`).
enum PostError {
    Permanent(anyhow::Error),
    Transient(anyhow::Error),
}

/// Posts one approved reply into its target room, as a native reply to the
/// original message when the approval names one.
async fn post_approved_reply(
    client: &Client,
    job: &outbound::ApprovedReply,
) -> Result<(), PostError> {
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
        let event_id = matrix_sdk::ruma::EventId::parse(reply_to).map_err(|error| {
            PostError::Permanent(anyhow!(error).context("invalid reply target"))
        })?;
        content.relates_to = Some(Relation::Reply(Reply::with_event_id(event_id)));
    }
    // A direct send, not the send queue: the queue only enqueues locally and
    // sends in the background, so an ack after it would not be tied to
    // delivery. Awaiting the homeserver's event id is what makes the ack
    // safe. The transaction id is derived from the approval's event id, so a
    // redelivered approval whose earlier send was accepted but whose response
    // was lost is deduplicated by the homeserver instead of posted twice.
    let transaction_id = OwnedTransactionId::from(format!("twalk-{}", job.event_id));
    room.send(content)
        .with_transaction_id(transaction_id)
        .await
        .map_err(classify_send_error)?;
    Ok(())
}

/// Maps a failed Matrix send onto the outbound retry policy. Only errors
/// saying the request itself is malformed can never succeed and are
/// permanent. Everything else is transient and goes through the bounded retry
/// schedule before dead-lettering — notably `M_FORBIDDEN`, which a portal
/// room's power levels raise and which clears once the Sensor is granted the
/// right to post, and `M_LIMIT_EXCEEDED`, network and server errors.
fn classify_send_error(error: matrix_sdk::Error) -> PostError {
    let permanent = matches!(
        error.client_api_error_kind(),
        Some(
            ErrorKind::BadJson | ErrorKind::NotJson | ErrorKind::TooLarge | ErrorKind::InvalidParam
        )
    );
    let error = anyhow!(error).context("matrix send failed");
    if permanent {
        PostError::Permanent(error)
    } else {
        PostError::Transient(error)
    }
}
