//! A test-side Matrix user with a full crypto stack, built on matrix-sdk.
//!
//! [`super::Bot`] speaks the client-server API over raw HTTP and therefore
//! cannot Megolm-encrypt. `CryptoBot` runs the real SDK (already a
//! dependency of the package) with its own store per login, so it can create
//! encrypted portal rooms, send end-to-end-encrypted messages and bootstrap
//! recovery — what real bridges and clients do. The SDK is allowed here:
//! only the Sensor's own implementation and the HTTP `Bot` helpers must stay
//! SDK-free.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use matrix_sdk::config::SyncSettings;
use matrix_sdk::encryption::EncryptionSettings;
use matrix_sdk::ruma::api::client::room::create_room;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::events::AnyInitialStateEvent;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OwnedUserId, RoomId, UserId};
use matrix_sdk::{Client, Room};
use serde_json::Value;
use tokio::task::JoinHandle;

use super::{poll_until, synapse_url, whatsapp_bridge_state, SERVER_NAME};

pub struct CryptoBot {
    client: Client,
    user_id: OwnedUserId,
    store_dir: PathBuf,
    sync_task: JoinHandle<()>,
}

impl CryptoBot {
    /// Logs in with the harness password scheme and starts a background sync
    /// loop: the SDK needs it to learn room state (membership, encryption)
    /// and to drive the to-device traffic Megolm key sharing rides on. Every
    /// login is a fresh device with its own temporary store directory, so a
    /// test run never inherits another's cryptographic identity.
    pub async fn login(localpart: &str) -> Result<Self> {
        Self::login_with(localpart, EncryptionSettings::default()).await
    }

    /// `login` with explicit encryption settings: the recovery-key test needs
    /// cross-signing and backups enabled on the Sensor account's first
    /// device, while plain bridge bots need neither.
    pub async fn login_with(localpart: &str, encryption: EncryptionSettings) -> Result<Self> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let store_dir = std::env::temp_dir().join(format!(
            "twalk-crypto-bot-{localpart}-{}-{unique}",
            std::process::id()
        ));
        let client = Client::builder()
            .homeserver_url(synapse_url())
            .sqlite_store(&store_dir, None)
            .with_encryption_settings(encryption)
            .build()
            .await
            .context("failed to build the crypto bot client")?;
        client
            .matrix_auth()
            .login_username(
                format!("@{localpart}:{SERVER_NAME}"),
                &format!("test-only-password-{localpart}"),
            )
            .initial_device_display_name("twalk-test-crypto-bot")
            .send()
            .await
            .context("crypto bot login failed")?;
        let user_id = client
            .user_id()
            .ok_or_else(|| anyhow!("no user id right after login"))?
            .to_owned();

        let sync_client = client.clone();
        let sync_task = tokio::spawn(async move {
            loop {
                if sync_client.sync(SyncSettings::default()).await.is_err() {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        });

        Ok(Self {
            client,
            user_id,
            store_dir,
            sync_task,
        })
    }

    pub fn user_id(&self) -> &str {
        self.user_id.as_str()
    }

    /// The SDK client, for recovery-test flows that need more than the bot
    /// helpers (e.g. enabling secret storage on the Sensor account).
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn room(&self, room_id: &str) -> Result<Room> {
        let room_id = RoomId::parse(room_id)?;
        self.client
            .get_room(&room_id)
            .ok_or_else(|| anyhow!("the crypto bot does not know room {room_id} yet"))
    }

    /// Creates a room, optionally Megolm-encrypted from creation, like a real
    /// bridge portal room. Encrypted rooms get `history_visibility: shared`,
    /// the mautrix default: members see history from before they joined, but
    /// cannot decrypt it without the keys.
    pub async fn create_room(&self, name: &str, encrypted: bool) -> Result<String> {
        let mut initial_state = Vec::new();
        if encrypted {
            initial_state.push(initial_state_event(
                "m.room.encryption",
                serde_json::json!({ "algorithm": "m.megolm.v1.aes-sha2" }),
            )?);
            initial_state.push(initial_state_event(
                "m.room.history_visibility",
                serde_json::json!({ "history_visibility": "shared" }),
            )?);
        }
        let mut request = create_room::v3::Request::new();
        request.name = Some(name.to_owned());
        request.is_direct = true;
        request.initial_state = initial_state;
        let room = self
            .client
            .create_room(request)
            .await
            .context("createRoom failed")?;
        if encrypted {
            self.wait_until_encrypted(room.room_id()).await?;
        }
        Ok(room.room_id().to_string())
    }

    /// Sends a state event (e.g. the `m.bridge` portal marker). State events
    /// are never Megolm-encrypted, matching real bridge behaviour.
    pub async fn send_state_event(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
        content: Value,
    ) -> Result<()> {
        self.room(room_id)?
            .send_state_event_raw(event_type, state_key, content)
            .await
            .context("send state event failed")?;
        Ok(())
    }

    pub async fn invite(&self, room_id: &str, invitee_user_id: &str) -> Result<()> {
        let user_id = UserId::parse(invitee_user_id)?;
        self.room(room_id)?
            .invite_user_by_id(&user_id)
            .await
            .context("invite failed")?;
        Ok(())
    }

    pub async fn join_room(&self, room_id: &str) -> Result<()> {
        let room_id = RoomId::parse(room_id)?;
        self.client
            .join_room_by_id(&room_id)
            .await
            .context("join failed")?;
        Ok(())
    }

    /// Sends a text message. In an encrypted room the SDK encrypts it and
    /// shares the Megolm session key with the devices of every room member,
    /// the Sensor's included. Returns when the send is queued; delivery is
    /// observed through the room timeline (e.g. via an HTTP `Bot`).
    pub async fn send_message(&self, room_id: &str, body: &str) -> Result<()> {
        self.room(room_id)?
            .send_queue()
            .send(RoomMessageEventContent::text_plain(body).into())
            .await
            .context("failed to queue the message")?;
        Ok(())
    }

    /// Waits until the bot's sync loop has learned that `user_id` is a joined
    /// member of the room: the condition under which the SDK will share the
    /// Megolm session key with that member's devices on the next send.
    pub async fn wait_for_joined_member(&self, room_id: &str, user_id: &str) -> Result<()> {
        let user_id = UserId::parse(user_id)?.to_owned();
        poll_until(
            || async {
                let room = self.room(room_id).ok()?;
                let member = room.get_member(&user_id).await.ok()??;
                (*member.membership() == matrix_sdk::ruma::events::room::member::MembershipState::Join)
                    .then_some(())
            },
            &format!("waiting for the crypto bot to see {user_id} joined in {room_id}"),
        )
        .await
    }

    /// Waits until the bot's sync loop has seen the room's `m.room.encryption`
    /// state event: before that the SDK would send in plaintext.
    async fn wait_until_encrypted(&self, room_id: &RoomId) -> Result<()> {
        poll_until(
            || async {
                self.client
                    .get_room(room_id)?
                    .encryption_state()
                    .is_encrypted()
                    .then_some(())
            },
            &format!("waiting for {room_id} to be known as encrypted"),
        )
        .await
    }
}

impl Drop for CryptoBot {
    fn drop(&mut self) {
        self.sync_task.abort();
        let _ = std::fs::remove_dir_all(&self.store_dir);
    }
}

/// Builds a raw initial state event for room creation (the `m.bridge` marker
/// is a custom event type ruma does not know).
fn initial_state_event(event_type: &str, content: Value) -> Result<Raw<AnyInitialStateEvent>> {
    let event = serde_json::json!({
        "type": event_type,
        "state_key": "",
        "content": content,
    });
    Ok(Raw::from_json(serde_json::value::to_raw_value(&event)?))
}

/// A mautrix-style encrypted portal room: Megolm from creation plus the
/// keyed `m.bridge` state event identifying the network.
pub async fn make_encrypted_whatsapp_portal(bridge: &CryptoBot, name: &str) -> Result<String> {
    let room_id = bridge.create_room(name, true).await?;
    let (state_key, content) = whatsapp_bridge_state(bridge.user_id(), name);
    bridge
        .send_state_event(&room_id, "m.bridge", &state_key, content)
        .await?;
    Ok(room_id)
}

/// Polls `get_member` until the crypto stack reports the device count the
/// room-key sharing path will see: a guard against racing the Sensor's own
/// device-keys upload right after it joins.
pub async fn wait_for_user_devices(client: &Client, user_id: &str) -> Result<()> {
    let user_id = UserId::parse(user_id)?.to_owned();
    poll_until(
        || async {
            let devices = client
                .encryption()
                .get_user_devices(&user_id)
                .await
                .ok()?;
            if devices.devices().next().is_some() {
                Some(())
            } else {
                None
            }
        },
        &format!("waiting for the crypto bot to see devices of {user_id}"),
    )
    .await?;
    Ok(())
}
