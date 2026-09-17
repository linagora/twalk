//! Integration-test harness for the Twalk Sensor (ticket 01).
//!
//! The seam under test is the Sensor's process boundary: a real Synapse and
//! a real NATS JetStream (the shared test stack, brought up by
//! `twalk-test-harness`). Bots play the role of bridges, speaking the
//! documented Matrix client-server API over HTTP — except
//! `crypto::CryptoBot`, which runs matrix-sdk with its crypto stack because
//! raw HTTP cannot Megolm-encrypt (ticket 04).
//! Nothing here reaches inside the Sensor process.
//!
//! What every component's suite needs — the stack's lifecycle, the `Bus`,
//! contract validation, `poll_until` — lives in the shared harness crate
//! (`tests/harness/`, ticket #20) and is re-exported here, so the Sensor's
//! test files keep seeing one flat `harness::` namespace. What is
//! Sensor-specific stays here: the Matrix `Bot`, the portal-room helpers
//! and `SensorProc`.

// Every test binary compiles this module but uses only a subset of it.
#![allow(dead_code)]

pub mod crypto;

pub use twalk_test_harness::*;

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use tokio::process::Command;

static TXN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Percent-encodes a path segment (room ids and user ids contain `!`, `@`, `:`).
fn esc(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'=' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Extracts a required string field from a client-server API response.
fn extract_str(response: &Value, field: &str, what: &str) -> Result<String> {
    response
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("no {field} in {what} response"))
}

/// A test-side Matrix user playing the role of a bridge (or of the operator),
/// speaking the client-server API directly.
pub struct Bot {
    http: reqwest::Client,
    access_token: String,
    user_id: String,
    device_id: String,
    base_url: String,
}

impl Bot {
    /// Logs in with the password scheme provisioned by
    /// tests/harness/scripts/provision-bots.sh — keep the two in sync.
    pub async fn login(localpart: &str) -> Result<Self> {
        Self::login_with(
            &synapse_url(),
            SERVER_NAME,
            localpart,
            &format!("test-only-password-{localpart}"),
        )
        .await
    }

    /// Logs in against an arbitrary homeserver with an explicit password: the
    /// deployment test (deployment.rs) points the harness at the deploy
    /// stack, which has its own server name and credentials.
    pub async fn login_with(
        base_url: &str,
        server_name: &str,
        localpart: &str,
        password: &str,
    ) -> Result<Self> {
        let http = reqwest::Client::new();
        let response: Value = http
            .post(format!("{base_url}/_matrix/client/v3/login"))
            .json(&serde_json::json!({
                "type": "m.login.password",
                "identifier": {
                    "type": "m.id.user",
                    "user": format!("@{localpart}:{server_name}"),
                },
                "password": password,
            }))
            .send()
            .await
            .context("bot login request failed")?
            .error_for_status()
            .context("bot login returned an error status")?
            .json()
            .await?;
        let access_token = extract_str(&response, "access_token", "login")?;
        let user_id = extract_str(&response, "user_id", "login")?;
        let device_id = extract_str(&response, "device_id", "login")?;
        Ok(Self {
            http,
            access_token,
            user_id,
            device_id,
            base_url: base_url.to_owned(),
        })
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// The credentials this login produced, for a test that has to hand them
    /// to the Sensor: a homeserver with password login disabled leaves an
    /// access token as the only way in (#72), so the Sensor must be able to
    /// start from one.
    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    fn authed(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(&self.access_token)
    }

    /// Sends an authenticated request and parses the response, treating an
    /// empty body as `Value::Null` (several endpoints return no content).
    async fn send_json(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
        what: &str,
    ) -> Result<Value> {
        let mut request = self.authed(method, path);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("{what} request failed"))?
            .error_for_status()
            .with_context(|| format!("{what} returned an error status"))?;
        let text = response.text().await?;
        if text.trim().is_empty() {
            Ok(Value::Null)
        } else {
            Ok(serde_json::from_str(&text)?)
        }
    }

    /// Creates a portal-shaped room. With `encrypted = true` the room enables
    /// Megolm from creation, like a real bridge portal room.
    pub async fn create_room(&self, name: &str, encrypted: bool) -> Result<String> {
        let mut body = serde_json::json!({ "name": name, "is_direct": true });
        if encrypted {
            body["initial_state"] = serde_json::json!([{
                "type": "m.room.encryption",
                "state_key": "",
                "content": { "algorithm": "m.megolm.v1.aes-sha2" },
            }]);
        }
        let response = self
            .send_json(
                reqwest::Method::POST,
                "/_matrix/client/v3/createRoom",
                Some(&body),
                "createRoom",
            )
            .await?;
        extract_str(&response, "room_id", "createRoom")
    }

    pub async fn invite(&self, room_id: &str, invitee_user_id: &str) -> Result<()> {
        self.send_json(
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/invite", esc(room_id)),
            Some(&serde_json::json!({ "user_id": invitee_user_id })),
            "invite",
        )
        .await?;
        Ok(())
    }

    pub async fn join_room(&self, room_id: &str) -> Result<()> {
        self.send_json(
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/join/{}", esc(room_id)),
            Some(&serde_json::json!({})),
            "join",
        )
        .await?;
        Ok(())
    }

    /// Lists the rooms the bot is currently joined to. The stack persists
    /// across runs, so tests sensitive to stale memberships (presence is
    /// not room-scoped) use this to drop them.
    pub async fn joined_rooms(&self) -> Result<Vec<String>> {
        let response = self
            .send_json(
                reqwest::Method::GET,
                "/_matrix/client/v3/joined_rooms",
                None,
                "joined rooms",
            )
            .await?;
        response
            .get("joined_rooms")
            .and_then(Value::as_array)
            .map(|rooms| {
                rooms
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .ok_or_else(|| anyhow!("no joined_rooms in joined rooms response"))
    }

    pub async fn leave_room(&self, room_id: &str) -> Result<()> {
        self.send_json(
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/leave", esc(room_id)),
            Some(&serde_json::json!({})),
            "leave",
        )
        .await?;
        Ok(())
    }

    /// Sends a message-like event and returns its event id.
    pub async fn send_event(
        &self,
        room_id: &str,
        event_type: &str,
        content: Value,
    ) -> Result<String> {
        let txn = format!(
            "{}{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos(),
            TXN_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let response = self
            .send_json(
                reqwest::Method::PUT,
                &format!(
                    "/_matrix/client/v3/rooms/{}/send/{event_type}/{txn}",
                    esc(room_id)
                ),
                Some(&content),
                "send event",
            )
            .await?;
        extract_str(&response, "event_id", "send event")
    }

    pub async fn send_message(&self, room_id: &str, body: &str) -> Result<String> {
        self.send_event(
            room_id,
            "m.room.message",
            serde_json::json!({ "msgtype": "m.text", "body": body }),
        )
        .await
    }

    pub async fn send_reaction(
        &self,
        room_id: &str,
        target_event_id: &str,
        key: &str,
    ) -> Result<String> {
        self.send_event(
            room_id,
            "m.reaction",
            serde_json::json!({
                "m.relates_to": {
                    "rel_type": "m.annotation",
                    "event_id": target_event_id,
                    "key": key,
                }
            }),
        )
        .await
    }

    /// Redacts an event (e.g. to remove a reaction). A user may always
    /// redact their own events.
    pub async fn redact(&self, room_id: &str, event_id: &str) -> Result<()> {
        let txn = format!(
            "{}{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos(),
            TXN_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        self.send_json(
            reqwest::Method::PUT,
            &format!(
                "/_matrix/client/v3/rooms/{}/redact/{}/{txn}",
                esc(room_id),
                esc(event_id)
            ),
            Some(&serde_json::json!({})),
            "redact",
        )
        .await?;
        Ok(())
    }

    /// Sets the bot's own presence (online, offline, unavailable).
    pub async fn set_presence(&self, presence: &str) -> Result<()> {
        self.send_json(
            reqwest::Method::PUT,
            &format!("/_matrix/client/v3/presence/{}/status", esc(&self.user_id)),
            Some(&serde_json::json!({ "presence": presence })),
            "set presence",
        )
        .await?;
        Ok(())
    }

    /// Reads another user's presence.
    pub async fn get_presence(&self, user_id: &str) -> Result<String> {
        let response = self
            .send_json(
                reqwest::Method::GET,
                &format!("/_matrix/client/v3/presence/{}/status", esc(user_id)),
                None,
                "get presence",
            )
            .await?;
        extract_str(&response, "presence", "get presence")
    }

    /// Fetches recent timeline events of a room as raw JSON, newest first.
    pub async fn room_events(&self, room_id: &str, limit: u32) -> Result<Vec<Value>> {
        let response = self
            .send_json(
                reqwest::Method::GET,
                &format!(
                    "/_matrix/client/v3/rooms/{}/messages?dir=b&limit={limit}",
                    esc(room_id)
                ),
                None,
                "room messages",
            )
            .await?;
        Ok(chunk_extract(&response)?)
    }

    /// Polls `room_events` until `predicate` matches an event or the
    /// deadline expires. Returns the matching event.
    pub async fn wait_for_event(
        &self,
        room_id: &str,
        predicate: impl Fn(&Value) -> bool,
        description: &str,
    ) -> Result<Value> {
        poll_until(
            || async {
                self.room_events(room_id, 50)
                    .await
                    .ok()?
                    .into_iter()
                    .find(|event| predicate(event))
            },
            &format!("waiting for {description} in {room_id}"),
        )
        .await
    }

    /// Sends a state event (e.g. the `m.bridge` marker mautrix sets on
    /// portal rooms to identify the network).
    pub async fn send_state_event(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
        content: Value,
    ) -> Result<String> {
        let response = self
            .send_json(
                reqwest::Method::PUT,
                &format!(
                    "/_matrix/client/v3/rooms/{}/state/{event_type}/{}",
                    esc(room_id),
                    esc(state_key)
                ),
                Some(&content),
                "send state event",
            )
            .await?;
        extract_str(&response, "event_id", "send state event")
    }

    /// Reads the content of one state event of a room.
    pub async fn get_state_event(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
    ) -> Result<Value> {
        self.send_json(
            reqwest::Method::GET,
            &format!(
                "/_matrix/client/v3/rooms/{}/state/{event_type}/{}",
                esc(room_id),
                esc(state_key)
            ),
            None,
            "get state event",
        )
        .await
    }

    /// Reads the membership of a user in a room (invite, join, leave, ban).
    pub async fn get_membership(&self, room_id: &str, user_id: &str) -> Result<String> {
        let response = self
            .send_json(
                reqwest::Method::GET,
                &format!(
                    "/_matrix/client/v3/rooms/{}/state/m.room.member/{}",
                    esc(room_id),
                    esc(user_id)
                ),
                None,
                "get membership",
            )
            .await?;
        extract_str(&response, "membership", "get membership")
    }

    /// Kicks a user out of a room (requires power, like a bridge admin has).
    pub async fn kick(&self, room_id: &str, user_id: &str) -> Result<()> {
        self.send_json(
            reqwest::Method::POST,
            &format!("/_matrix/client/v3/rooms/{}/kick", esc(room_id)),
            Some(&serde_json::json!({ "user_id": user_id })),
            "kick",
        )
        .await?;
        Ok(())
    }

    /// Polls `get_membership` until it equals `expected` or the deadline
    /// expires.
    pub async fn wait_for_membership(
        &self,
        room_id: &str,
        user_id: &str,
        expected: &str,
    ) -> Result<()> {
        poll_until(
            || async {
                self.get_membership(room_id, user_id)
                    .await
                    .ok()
                    .filter(|membership| membership == expected)
                    .map(|_| ())
            },
            &format!("waiting for membership {expected} of {user_id} in {room_id}"),
        )
        .await
    }
}

fn chunk_extract(response: &Value) -> Result<Vec<Value>> {
    let chunk = response
        .get("chunk")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("no chunk in messages response"))?;
    Ok(chunk.clone())
}

/// An `m.bridge` portal marker as current mautrix bridges (bridgev2 in
/// mautrix-go) write it: the state key is the bridge's unique id,
/// `<homeserver domain>/<appservice id>` (empty only when the operator sets
/// `no_bridge_info_state_key`), `protocol.id` names the bridge and
/// `channel.id` the remote chat. A WhatsApp chat has no `network` section:
/// in bridgev2 that section describes a parent portal (a Discord guild, a
/// Telegram forum), never the network. Returns `(state_key, content)`.
pub fn bridge_state(
    bridge_user_id: &str,
    appservice_id: &str,
    protocol_id: &str,
    chat_id: &str,
) -> (String, Value) {
    let state_key = format!("{SERVER_NAME}/{appservice_id}");
    let content = serde_json::json!({
        "bridgebot": bridge_user_id,
        "creator": bridge_user_id,
        "protocol": { "id": protocol_id, "displayname": protocol_id },
        "channel": { "id": chat_id, "displayname": chat_id },
        "com.beeper.room_type": "dm",
        "com.beeper.room_type.v2": "dm",
    });
    (state_key, content)
}

/// [`bridge_state`] for a mautrix-whatsapp portal.
pub fn whatsapp_bridge_state(bridge_user_id: &str, chat_id: &str) -> (String, Value) {
    bridge_state(bridge_user_id, "whatsapp", "whatsapp", chat_id)
}

/// A mautrix-style portal room: the bridge identifies the network through a
/// keyed m.bridge state event.
pub async fn make_whatsapp_portal(bridge: &Bot, name: &str) -> Result<String> {
    let room_id = bridge.create_room(name, false).await?;
    let (state_key, content) = whatsapp_bridge_state(bridge.user_id(), name);
    bridge
        .send_state_event(&room_id, "m.bridge", &state_key, content)
        .await?;
    Ok(room_id)
}

/// The Sensor under test, running as the real binary it ships as — the
/// agreed seam is the process boundary. Log lines (stdout and stderr) are
/// captured and forwarded to the test's own output, so tests can assert on
/// the Sensor's structured logs without reaching inside the process.
pub struct SensorProc {
    child: tokio::process::Child,
    log_lines: std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl SensorProc {
    pub fn start(env: &[(String, String)]) -> Result<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_twalk-sensor"))
            .envs(env.iter().cloned())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start the sensor binary")?;
        let log_lines = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        fn forward<S>(
            stream: S,
            is_stderr: bool,
            store: std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
        ) where
            S: tokio::io::AsyncRead + Unpin + Send + 'static,
        {
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let mut lines = tokio::io::BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if is_stderr {
                        eprintln!("{line}");
                    } else {
                        println!("{line}");
                    }
                    store.lock().await.push(line);
                }
            });
        }
        forward(
            child.stdout.take().expect("stdout is piped"),
            false,
            log_lines.clone(),
        );
        forward(
            child.stderr.take().expect("stderr is piped"),
            true,
            log_lines.clone(),
        );
        Ok(Self { child, log_lines })
    }

    pub async fn stop(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    /// Sends SIGTERM — as an operator's process manager would — and waits for
    /// the process to exit. (`Child::kill` only sends SIGKILL, which cannot
    /// exercise a graceful shutdown.)
    pub async fn terminate(mut self) -> Result<std::process::ExitStatus> {
        let pid = self.child.id().context("the sensor has already exited")?;
        let status = Command::new("kill")
            .arg(pid.to_string())
            .status()
            .await
            .context("failed to run kill(1)")?;
        anyhow::ensure!(status.success(), "kill(1) failed with {status}");
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .context("the sensor did not exit within 10s of SIGTERM")??;
        Ok(status)
    }

    /// A snapshot of the Sensor's captured log lines so far.
    pub async fn logs(&self) -> Vec<String> {
        self.log_lines.lock().await.clone()
    }

    /// True while the Sensor process is still running (i.e. it has not
    /// crashed or exited): decryption-failure tests assert the pipeline
    /// survives an undecryptable room.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

pub const SENSOR_USER_ID: &str = "@sensor:test.twalk";

/// A fresh state directory path per test: unique per run, and deliberately
/// NOT created beforehand — starting with an empty store must work, so the
/// Sensor creates the directory itself.
pub fn fresh_state_dir(test_name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "twalk-sensor-state-{test_name}-{}-{unique}",
        std::process::id()
    ))
}

/// Serializes tests that spawn a Sensor process. Every test logs the Sensor
/// in as the same Matrix account, so two concurrent Sensor processes would
/// join each other's invited rooms and observe each other's traffic —
/// exactly what must NOT happen when asserting a room stopped producing
/// events. Take this lock at the top of any test that starts a Sensor.
pub static SENSOR_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The environment the Sensor runs from in tests. The password follows the
/// provisioning scheme (see tests/harness/scripts/provision-bots.sh).
pub fn sensor_env() -> Vec<(String, String)> {
    vec![
        ("SENSOR_HOMESERVER".to_owned(), synapse_url()),
        ("SENSOR_USER_ID".to_owned(), SENSOR_USER_ID.to_owned()),
        (
            "SENSOR_PASSWORD".to_owned(),
            "test-only-password-sensor".to_owned(),
        ),
        ("SENSOR_NATS_URL".to_owned(), nats_url()),
        (
            "SENSOR_ALLOWED_INVITERS".to_owned(),
            "@bot_alpha:test.twalk".to_owned(),
        ),
        (
            "SENSOR_LOG_LEVEL".to_owned(),
            "info,twalk_sensor=debug".to_owned(),
        ),
    ]
}

/// `sensor_env` with per-test overrides: an existing key is replaced, a new
/// key is appended (e.g. a per-test SENSOR_STATE_DIR for persistence tests).
pub fn sensor_env_with(overrides: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut env = sensor_env();
    for (key, value) in overrides {
        if let Some(entry) = env.iter_mut().find(|(existing, _)| existing == key) {
            entry.1 = value.to_string();
        } else {
            env.push((key.to_string(), value.to_string()));
        }
    }
    env
}
