//! Integration-test harness for the Twalk Sensor (ticket 01).
//!
//! The seam under test is the Sensor's process boundary: a real Synapse and
//! a real NATS JetStream (see `compose.test.yaml`). Bots play the role of
//! bridges, speaking the documented Matrix client-server API over HTTP.
//! Nothing here reaches inside the Sensor process.

// Every test binary compiles this module but uses only a subset of it.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use tokio::process::Command;
use tokio::time::sleep;

/// The harness stack is parameterizable so that parallel worktrees each run
/// their own isolated instance: TWALK_TEST_STACK names the compose project,
/// TWALK_TEST_SYNAPSE_PORT / TWALK_TEST_NATS_PORT move the host ports.
/// Defaults match the main checkout.
pub fn synapse_url() -> String {
    let port =
        std::env::var("TWALK_TEST_SYNAPSE_PORT").unwrap_or_else(|_| "18008".to_owned());
    format!("http://localhost:{port}")
}

pub fn nats_url() -> String {
    let port = std::env::var("TWALK_TEST_NATS_PORT").unwrap_or_else(|_| "14222".to_owned());
    format!("nats://localhost:{port}")
}

fn stack_id() -> String {
    std::env::var("TWALK_TEST_STACK").unwrap_or_else(|_| "twalk-sensor-test".to_owned())
}

pub const SERVER_NAME: &str = "test.twalk";

fn tests_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn contract_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("contracts")
        .join("cloudevents")
        .join("v1")
}

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

/// Brings the compose stack up (idempotent) and provisions the bots.
/// Safe to call at the top of every test: the bootstrap is serialized
/// through a OnceCell (all tests share one process), so the first caller
/// does the work and concurrent callers wait for it instead of racing
/// parallel `docker compose up` invocations on a cold volume.
pub async fn ensure_stack() -> Result<()> {
    static STACK: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    STACK.get_or_try_init(do_ensure_stack).await?;
    Ok(())
}

async fn do_ensure_stack() -> Result<()> {
    let compose = tests_dir().join("compose.test.yaml");
    let status = Command::new("docker")
        .args([
            "compose".to_owned(),
            "-p".to_owned(),
            stack_id(),
            "-f".to_owned(),
            compose.to_string_lossy().into_owned(),
            "up".to_owned(),
            "-d".to_owned(),
            "--wait".to_owned(),
        ])
        .stdout(Stdio::null())
        .status()
        .await
        .context("failed to run docker compose up")?;
    if !status.success() {
        bail!("docker compose up failed with {status}");
    }

    let status = Command::new(tests_dir().join("scripts").join("provision-bots.sh"))
        .status()
        .await
        .context("failed to run provision-bots.sh")?;
    if !status.success() {
        bail!("bot provisioning failed with {status}");
    }
    Ok(())
}

static TXN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Polls `attempt` every 500 ms until it yields `Some`, or fails after ~20 s.
pub async fn poll_until<T, Fut>(mut attempt: impl FnMut() -> Fut, description: &str) -> Result<T>
where
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..40 {
        if let Some(value) = attempt().await {
            return Ok(value);
        }
        sleep(Duration::from_millis(500)).await;
    }
    bail!("timed out {description}")
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
}

impl Bot {
    /// Logs in with the password scheme provisioned by
    /// tests/scripts/provision-bots.sh — keep the two in sync.
    pub async fn login(localpart: &str) -> Result<Self> {
        let http = reqwest::Client::new();
        let response: Value = http
            .post(format!("{}/_matrix/client/v3/login", synapse_url()))
            .json(&serde_json::json!({
                "type": "m.login.password",
                "identifier": {
                    "type": "m.id.user",
                    "user": format!("@{localpart}:{SERVER_NAME}"),
                },
                "password": format!("test-only-password-{localpart}"),
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
        Ok(Self {
            http,
            access_token,
            user_id,
        })
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    fn authed(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", synapse_url()))
            .bearer_auth(&self.access_token)
    }

    /// Sends an authenticated request and parses the response, treating an
    /// empty body as `Value::Null` (several endpoints return no content).
    async fn send_json(&self, method: reqwest::Method, path: &str, body: Option<&Value>, what: &str) -> Result<Value> {
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
            .send_json(reqwest::Method::POST, "/_matrix/client/v3/createRoom", Some(&body), "createRoom")
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

    /// Sends a message-like event and returns its event id.
    pub async fn send_event(&self, room_id: &str, event_type: &str, content: Value) -> Result<String> {
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

    pub async fn send_reaction(&self, room_id: &str, target_event_id: &str, key: &str) -> Result<String> {
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
                    "/_matrix/client/v3/rooms/{}/state/{event_type}/{state_key}",
                    esc(room_id)
                ),
                Some(&content),
                "send state event",
            )
            .await?;
        extract_str(&response, "event_id", "send state event")
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
    pub async fn wait_for_membership(&self, room_id: &str, user_id: &str, expected: &str) -> Result<()> {
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

/// The bus side of the seam: NATS JetStream, as the Sensor will use it.
pub struct Bus {
    jetstream: async_nats::jetstream::Context,
}

impl Bus {
    /// Connects, retrying while the container finishes starting: the nats
    /// image ships no wget or CLI, so the compose stack cannot healthcheck
    /// it and `--wait` does not cover it.
    pub async fn connect() -> Result<Self> {
        for attempt in 0..30 {
            match async_nats::connect(nats_url().as_str()).await {
                Ok(client) => {
                    return Ok(Self {
                        jetstream: async_nats::jetstream::new(client),
                    })
                }
                Err(e) if attempt < 29 => {
                    let _ = e;
                    sleep(Duration::from_millis(500)).await;
                }
                Err(e) => return Err(e).context("failed to connect to NATS"),
            }
        }
        unreachable!("the loop either returns or exhausts attempts")
    }

    /// Creates (or reuses) a JetStream stream capturing the given subjects.
    pub async fn ensure_stream(&self, name: &str, subjects: &[&str]) -> Result<()> {
        let config = async_nats::jetstream::stream::Config {
            name: name.to_owned(),
            subjects: subjects.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        self.jetstream
            .get_or_create_stream(config)
            .await
            .context("failed to create stream")?;
        Ok(())
    }

    pub async fn publish(&self, subject: &str, payload: &Value) -> Result<()> {
        let ack = self
            .jetstream
            .publish(subject.to_owned(), serde_json::to_vec(payload)?.into())
            .await
            .context("publish failed")?;
        ack.await.context("publish ack failed")?;
        Ok(())
    }

    /// Fetches the most recent message stored on a subject, if any.
    pub async fn last_message(&self, stream: &str, subject: &str) -> Result<Option<Value>> {
        use async_nats::jetstream::stream::LastRawMessageErrorKind;
        let stream = self
            .jetstream
            .get_stream(stream)
            .await
            .context("failed to get stream")?;
        match stream.get_last_raw_message_by_subject(subject).await {
            Ok(message) => Ok(Some(serde_json::from_slice(&message.payload)?)),
            Err(e) if e.kind() == LastRawMessageErrorKind::NoMessageFound => Ok(None),
            Err(e) => Err(e).context("failed to fetch last message"),
        }
    }

    /// Fetches every message stored on a subject, in stream order: the
    /// harness's "consume a subject" primitive. Later tickets assert on
    /// whole sequences (e.g. no duplicate ids after a Sensor replay).
    pub async fn fetch_all(&self, stream: &str, subject: &str) -> Result<Vec<Value>> {
        Ok(self
            .fetch_all_with_headers(stream, subject)
            .await?
            .into_iter()
            .map(|message| message.payload)
            .collect())
    }

    /// Like `fetch_all`, but keeps the NATS headers alongside each payload
    /// (needed to assert on `NATS-Msg-Id` and the filtering extensions).
    pub async fn fetch_all_with_headers(&self, stream: &str, subject: &str) -> Result<Vec<StoredMessage>> {
        use async_nats::jetstream::stream::LastRawMessageErrorKind;
        let stream = self
            .jetstream
            .get_stream(stream)
            .await
            .context("failed to get stream")?;
        let last = match stream.get_last_raw_message_by_subject(subject).await {
            Ok(message) => message,
            Err(e) if e.kind() == LastRawMessageErrorKind::NoMessageFound => return Ok(Vec::new()),
            Err(e) => return Err(e).context("failed to fetch last message"),
        };
        let mut out = Vec::new();
        for sequence in 1..=last.sequence {
            match stream.get_raw_message(sequence).await {
                Ok(message) if message.subject.as_str() == subject => {
                    out.push(StoredMessage {
                        headers: message
                            .headers
                            .iter()
                            .flat_map(|(name, values)| {
                                values
                                    .iter()
                                    .map(move |value| (name.to_string(), value.to_string()))
                            })
                            .collect(),
                        payload: serde_json::from_slice(&message.payload)?,
                    })
                }
                Ok(_) => {}
                Err(e) if e.kind() == LastRawMessageErrorKind::NoMessageFound => {}
                Err(e) => return Err(e).context("failed to fetch message"),
            }
        }
        Ok(out)
    }

    /// Fetches every stored event whose `source` identifies the given room.
    /// Several tests share the bus, so consumers filter by room — as real
    /// consumers will.
    pub async fn fetch_room_messages(&self, stream: &str, subject: &str, room_id: &str) -> Result<Vec<StoredMessage>> {
        let expected_source = format!("matrix://{SERVER_NAME}/{room_id}");
        Ok(self
            .fetch_all_with_headers(stream, subject)
            .await?
            .into_iter()
            .filter(|m| m.payload["source"].as_str() == Some(expected_source.as_str()))
            .collect())
    }

    /// Polls until an event whose `source` identifies the given room is
    /// stored on the subject.
    pub async fn wait_for_room_message(&self, stream: &str, subject: &str, room_id: &str) -> Result<StoredMessage> {
        poll_until(
            || async {
                self.fetch_room_messages(stream, subject, room_id)
                    .await
                    .ok()?
                    .into_iter()
                    .next()
            },
            &format!("waiting for an event from {room_id} on {subject}"),
        )
        .await
    }
}

/// A message as stored on the bus: payload plus NATS headers.
pub struct StoredMessage {
    pub headers: Vec<(String, String)>,
    pub payload: Value,
}

impl StoredMessage {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Validates an event against one of the contract schemas by type name,
/// e.g. `validate_against_contract(&event, "inbound.message.received")`.
pub fn validate_against_contract(event: &Value, type_name: &str) -> Result<()> {
    let schema_path = contract_dir().join(format!("{type_name}.schema.json"));
    let schema: Value = serde_json::from_slice(
        &std::fs::read(&schema_path)
            .with_context(|| format!("failed to read schema {}", schema_path.display()))?,
    )?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|e| anyhow!("invalid schema {type_name}: {e}"))?;
    let errors = validator
        .iter_errors(event)
        .map(|e| format!("  - {}: {}", e.instance_path(), e))
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        bail!(
            "event failed contract validation for {type_name}:\n{}",
            errors.join("\n")
        );
    }
    Ok(())
}

/// Loads one of the contract fixtures by type name.
pub fn contract_fixture(type_name: &str) -> Result<Value> {
    let path = contract_dir()
        .join("fixtures")
        .join(format!("{type_name}.json"));
    let fixture = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?,
    )?;
    Ok(fixture)
}

/// Lists every contract type that has a fixture, straight from the fixtures
/// directory: a new fixture is automatically covered, never silently skipped.
pub fn contract_fixture_types() -> Result<Vec<String>> {
    let mut types = Vec::new();
    for entry in std::fs::read_dir(contract_dir().join("fixtures"))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            types.push(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    types.sort();
    Ok(types)
}

/// The Sensor under test, running as the real binary it ships as — the
/// agreed seam is the process boundary.
pub struct SensorProc(tokio::process::Child);

impl SensorProc {
    pub fn start(env: &[(String, String)]) -> Result<Self> {
        let child = Command::new(env!("CARGO_BIN_EXE_twalk-sensor"))
            .envs(env.iter().cloned())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start the sensor binary")?;
        Ok(Self(child))
    }

    pub async fn stop(mut self) {
        let _ = self.0.kill().await;
        let _ = self.0.wait().await;
    }
}

pub const SENSOR_USER_ID: &str = "@sensor:test.twalk";

/// Serializes tests that spawn a Sensor process. Every test logs the Sensor
/// in as the same Matrix account, so two concurrent Sensor processes would
/// join each other's invited rooms and observe each other's traffic —
/// exactly what must NOT happen when asserting a room stopped producing
/// events. Take this lock at the top of any test that starts a Sensor.
pub static SENSOR_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The environment the Sensor runs from in tests. The password follows the
/// provisioning scheme (see tests/scripts/provision-bots.sh).
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
