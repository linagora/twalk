//! Integration-test harness for the Twalk Sensor (ticket 01).
//!
//! The seam under test is the Sensor's process boundary: a real Synapse and
//! a real NATS JetStream (see `compose.test.yaml`). Bots play the role of
//! bridges, speaking the documented Matrix client-server API over HTTP.
//! Nothing here reaches inside the Sensor process.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use tokio::process::Command;
use tokio::time::sleep;

pub const SYNAPSE_URL: &str = "http://localhost:18008";
pub const NATS_URL: &str = "nats://localhost:14222";
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
            "compose",
            "-p",
            "twalk-sensor-test",
            "-f",
            compose.to_str().unwrap(),
            "up",
            "-d",
            "--wait",
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
            .post(format!("{SYNAPSE_URL}/_matrix/client/v3/login"))
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
            .request(method, format!("{SYNAPSE_URL}{path}"))
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
        let chunk = response
            .get("chunk")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("no chunk in messages response"))?;
        Ok(chunk.clone())
    }

    /// Polls `room_events` until `predicate` matches an event or the
    /// deadline expires. Returns the matching event.
    pub async fn wait_for_event(
        &self,
        room_id: &str,
        predicate: impl Fn(&Value) -> bool,
        description: &str,
    ) -> Result<Value> {
        for _ in 0..30 {
            for event in self.room_events(room_id, 50).await? {
                if predicate(&event) {
                    return Ok(event);
                }
            }
            sleep(Duration::from_millis(500)).await;
        }
        bail!("timed out waiting for {description} in {room_id}")
    }
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
            match async_nats::connect(NATS_URL).await {
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
                    out.push(serde_json::from_slice(&message.payload)?)
                }
                Ok(_) => {}
                Err(e) if e.kind() == LastRawMessageErrorKind::NoMessageFound => {}
                Err(e) => return Err(e).context("failed to fetch message"),
            }
        }
        Ok(out)
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
