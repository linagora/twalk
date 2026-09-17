//! Integration-test harness for the Companion Gateway (ticket #48).
//!
//! The seam under test is the Gateway's process boundary: the real binary it
//! ships as, configured through its environment, answered over HTTP. Nothing
//! here reaches inside the Gateway process.
//!
//! What every component's suite needs — the test stack's lifecycle, the
//! `Bus`, contract validation, `poll_until` — lives in the shared harness
//! crate (`tests/harness/`, ticket #20) and is re-exported here, so this
//! suite sees one flat `harness::` namespace, exactly as `sensor/tests/
//! harness/` does. The origin's own suite needs only `poll_until`; the
//! sign-in suite (ticket #52) also brings the stack up, because a real
//! homeserver is what mints the OpenID tokens. No bus yet.
//!
//! What is Gateway-specific stays here: `GatewayProc`, the static directory
//! fixtures, the environment the Gateway runs from in tests, and `MatrixUser`
//! — a Matrix account standing in for the user's own browser.

// Every test binary compiles this module but uses only a subset of it.
#![allow(dead_code, unused_imports)]

pub mod stub_bridge;

pub use stub_bridge::{StubBridge, STUB_PROVISIONING_SECRET};
pub use twalk_test_harness::*;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::Command;

/// The Companion Gateway under test, running as the real binary it ships as.
/// Log lines (stdout and stderr) are captured and forwarded to the test's own
/// output, so tests can assert on the Gateway's structured logs without
/// reaching inside the process.
pub struct GatewayProc {
    child: tokio::process::Child,
    log_lines: std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
    /// The tasks draining stdout and stderr. Joined once the process has
    /// exited, so that a test reading the logs of a Gateway that died sees
    /// the line it died with: the process exiting and its last line reaching
    /// the store are two different events, and under load the second can
    /// lose the race.
    forwarders: Vec<tokio::task::JoinHandle<()>>,
}

impl GatewayProc {
    pub fn start(env: &[(String, String)]) -> Result<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_twalk-companion-gateway"))
            .envs(env.iter().cloned())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start the companion gateway binary")?;
        let log_lines = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        fn forward<S>(
            stream: S,
            is_stderr: bool,
            store: std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
        ) -> tokio::task::JoinHandle<()>
        where
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
            })
        }
        let forwarders = vec![
            forward(
                child.stdout.take().expect("stdout is piped"),
                false,
                log_lines.clone(),
            ),
            forward(
                child.stderr.take().expect("stderr is piped"),
                true,
                log_lines.clone(),
            ),
        ];
        Ok(Self {
            child,
            log_lines,
            forwarders,
        })
    }

    /// The origin the Gateway ended up listening on, read from its own
    /// startup log line. Tests ask for port 0, so the kernel picks a free
    /// port and two tests — or two worktrees — never fight over one.
    pub async fn base_url(&self) -> Result<String> {
        let address = poll_until(
            || async {
                self.logs().await.iter().find_map(|line| {
                    line.split_once("listening on ")
                        .map(|(_, rest)| rest.split_whitespace().next().unwrap_or("").to_owned())
                        .filter(|address| !address.is_empty())
                })
            },
            "the gateway to log its listen address",
        )
        .await?;
        Ok(format!("http://{address}"))
    }

    pub async fn stop(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    /// Sends SIGTERM — as an operator's process manager would — and waits for
    /// the process to exit. (`Child::kill` only sends SIGKILL, which cannot
    /// exercise a graceful shutdown.)
    pub async fn terminate(mut self) -> Result<std::process::ExitStatus> {
        let pid = self.child.id().context("the gateway has already exited")?;
        let status = Command::new("kill")
            .arg(pid.to_string())
            .status()
            .await
            .context("failed to run kill(1)")?;
        anyhow::ensure!(status.success(), "kill(1) failed with {status}");
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .context("the gateway did not exit within 10s of SIGTERM")??;
        self.drain_logs().await;
        Ok(status)
    }

    /// Waits for the process to exit on its own — a misconfigured Gateway
    /// must fail loudly instead of serving nothing. Borrows, so the caller
    /// can read the logs it exited with.
    pub async fn wait_for_exit(&mut self) -> Result<std::process::ExitStatus> {
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .context("the gateway did not exit within 10s")??;
        self.drain_logs().await;
        Ok(status)
    }

    /// Waits for the log forwarders to reach end of stream, so every line the
    /// exited process wrote is in the store before a test reads it.
    async fn drain_logs(&mut self) {
        for forwarder in std::mem::take(&mut self.forwarders) {
            let _ = tokio::time::timeout(Duration::from_secs(5), forwarder).await;
        }
    }

    /// A snapshot of the Gateway's captured log lines so far.
    pub async fn logs(&self) -> Vec<String> {
        self.log_lines.lock().await.clone()
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

/// A directory shaped like a built Companion — a SvelteKit static export —
/// in a fresh temp directory per test: a prerendered homepage, the SPA
/// fallback, a prerendered nested page under each trailing-slash spelling,
/// an asset, and the Matrix crypto WebAssembly with its brotli sibling.
pub fn companion_build(test_name: &str) -> Result<PathBuf> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "twalk-gateway-static-{test_name}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("onboarding/signal"))?;
    std::fs::create_dir_all(dir.join("_app/immutable"))?;
    std::fs::write(dir.join("index.html"), INDEX_HTML)?;
    std::fs::write(dir.join("200.html"), FALLBACK_HTML)?;
    std::fs::write(dir.join("onboarding/whatsapp.html"), WHATSAPP_HTML)?;
    std::fs::write(dir.join("onboarding/signal/index.html"), SIGNAL_HTML)?;
    std::fs::write(dir.join("app.css"), "body { color: rebeccapurple }\n")?;
    std::fs::write(dir.join("_app/immutable/crypto.wasm"), WASM)?;
    std::fs::write(dir.join("_app/immutable/crypto.wasm.br"), WASM_BROTLI)?;
    Ok(dir)
}

/// The markers the static fixtures carry, asserted on by the tests.
pub const INDEX_HTML: &str = "<!doctype html>\n<title>Companion home</title>\n";
pub const FALLBACK_HTML: &str = "<!doctype html>\n<title>Companion shell</title>\n";
pub const WHATSAPP_HTML: &str = "<!doctype html>\n<title>WhatsApp onboarding</title>\n";
pub const SIGNAL_HTML: &str = "<!doctype html>\n<title>Signal onboarding</title>\n";
/// A WebAssembly module header — enough to be a distinct file; nothing here
/// instantiates it.
pub const WASM: &[u8] = b"\0asm\x01\0\0\0";
/// Stands in for the brotli-compressed sibling of the module above. Not real
/// brotli: the test asserts the `Content-Encoding` the Gateway chose, and
/// never decodes the body.
pub const WASM_BROTLI: &[u8] = b"brotli-compressed-crypto-wasm";

/// A path inside the temp directory that deliberately does not exist.
pub fn missing_static_dir(test_name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "twalk-gateway-absent-{test_name}-{}-{unique}",
        std::process::id()
    ))
}

/// The local part of the test stack's bot the Gateway is configured with as
/// its owner, and of the bot standing in for everybody else. Any Matrix
/// account other than the owner must be refused — on this homeserver the
/// Sensor has one too.
pub const OWNER_LOCALPART: &str = "bot_alpha";
pub const OTHER_LOCALPART: &str = "bot_beta";

/// The Matrix ID of the owner the Gateway is configured with in tests.
pub fn owner_user_id() -> String {
    format!("@{OWNER_LOCALPART}:{SERVER_NAME}")
}

/// The test stack's registration shared secret, from
/// `tests/harness/synapse/homeserver.yaml` — a throwaway constant for the
/// local, ephemeral stack. What the Gateway's registration relay is
/// configured with (ticket #53).
pub const REGISTRATION_SHARED_SECRET: &str = "test-only-registration-shared-secret";

/// The service token the Gateway is configured with in tests, and the one a
/// test presents to read the consent snapshot (ticket #50). A throwaway
/// constant for the local test stack, long enough to satisfy the Gateway's
/// own minimum — it refuses to start with a service token under 32
/// characters, because that token authenticates a read of the whole consent
/// state.
pub const SERVICE_TOKEN: &str = "test-only-gateway-service-token-g50";

/// The Matrix ID of the Sensor's account on the test stack, provisioned by
/// `provision-bots.sh`: who the Gateway invites into the rooms the user
/// selects.
pub const SENSOR_USER_ID: &str = "@sensor:test.twalk";

/// A Matrix ID nobody has yet, for a test of the registration relay: the
/// relay creates the owner's account exactly once, so every such test needs
/// an owner whose account does not exist on the shared stack.
pub fn fresh_owner_user_id(test_name: &str) -> String {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    // Synapse's localpart grammar is narrow; keep to lowercase and
    // underscores whatever the test is called.
    let slug: String = test_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("@g53_{slug}_{}_{unique}:{SERVER_NAME}", std::process::id())
}

/// The directory the Gateway keeps its stores in for a test, derived from its
/// static directory so the two are unique together — and deliberately *not*
/// inside it: a store under the static directory would be a file the origin
/// serves.
pub fn gateway_state_dir(static_dir: &Path) -> PathBuf {
    let mut path = static_dir.as_os_str().to_owned();
    path.push("-state");
    PathBuf::from(path)
}

/// The environment the Gateway runs from in tests: port 0 (the kernel picks),
/// the given static directory, debug logs for the Gateway's own target, and
/// the sign-in configuration (ticket #52) — the owner, the homeserver's
/// federation API and a state directory of its own. A test that never signs
/// in still runs a fully configured Gateway, and never contacts the
/// homeserver.
pub fn gateway_env(static_dir: &Path) -> Vec<(String, String)> {
    vec![
        ("GATEWAY_LISTEN".to_owned(), "127.0.0.1:0".to_owned()),
        (
            "GATEWAY_STATIC_DIR".to_owned(),
            static_dir.to_string_lossy().into_owned(),
        ),
        (
            "GATEWAY_LOG_LEVEL".to_owned(),
            "info,twalk_companion_gateway=debug".to_owned(),
        ),
        ("GATEWAY_OWNER".to_owned(), owner_user_id()),
        (
            "GATEWAY_HOMESERVER_FEDERATION_URL".to_owned(),
            synapse_url(),
        ),
        (
            "GATEWAY_STATE_DIR".to_owned(),
            gateway_state_dir(static_dir).to_string_lossy().into_owned(),
        ),
        // Bootstrap (ticket #53): the registration relay and the Sensor the
        // Gateway invites. Both halves on, as a deployment that onboards a
        // user through screens 2 and 3d has them.
        (
            "GATEWAY_REGISTRATION_SHARED_SECRET".to_owned(),
            REGISTRATION_SHARED_SECRET.to_owned(),
        ),
        (
            "GATEWAY_SENSOR_USER_ID".to_owned(),
            SENSOR_USER_ID.to_owned(),
        ),
        // The consent snapshot (ticket #50): the service token its one
        // caller presents. On, as a deployment that runs a Sensor has it —
        // and independently of the bus, so that a Gateway with a token and
        // no bus answers `consent_not_configured` rather than pretending
        // the snapshot is the thing that is missing.
        ("GATEWAY_SERVICE_TOKEN".to_owned(), SERVICE_TOKEN.to_owned()),
    ]
}

/// [`gateway_env`] with sign-in unconfigured: what an operator gets who has
/// not set `GATEWAY_OWNER`. The origin still serves the Companion; its API
/// is closed.
pub fn gateway_env_without_sign_in(static_dir: &Path) -> Vec<(String, String)> {
    gateway_env(static_dir)
        .into_iter()
        .filter(|(key, _)| key != "GATEWAY_OWNER")
        .collect()
}

/// A Matrix account on the test stack, standing in for the user's own
/// browser: it logs in with a password and mints OpenID tokens the way the
/// Companion does.
///
/// The account's Matrix access token stays on this side of the seam. That is
/// the property ADR 0011 is about: the Gateway is handed an OpenID token and
/// never the access token that minted it.
pub struct MatrixUser {
    pub user_id: String,
    access_token: String,
    http: reqwest::Client,
}

impl MatrixUser {
    /// Logs one of the test stack's provisioned bots in (the password scheme
    /// is `provision-bots.sh`'s).
    pub async fn login(localpart: &str) -> Result<Self> {
        let http = reqwest::Client::new();
        let body: serde_json::Value = http
            .post(format!("{}/_matrix/client/v3/login", synapse_url()))
            .json(&serde_json::json!({
                "type": "m.login.password",
                "identifier": { "type": "m.id.user", "user": localpart },
                "password": format!("test-only-password-{localpart}"),
            }))
            .send()
            .await
            .context("failed to log a test user in")?
            .error_for_status()
            .context("the homeserver refused the login")?
            .json()
            .await
            .context("the login answer is not JSON")?;
        Ok(Self {
            user_id: body["user_id"]
                .as_str()
                .context("the login answer names no user id")?
                .to_owned(),
            access_token: body["access_token"]
                .as_str()
                .context("the login answer carries no access token")?
                .to_owned(),
            http,
        })
    }

    /// A fresh OpenID token, exactly as the homeserver answers it
    /// (`access_token`, `token_type`, `matrix_server_name`, `expires_in`) —
    /// the document the Companion forwards to the Gateway unchanged.
    pub async fn openid_token(&self) -> Result<serde_json::Value> {
        let token: serde_json::Value = self
            .http
            .post(format!(
                "{}/_matrix/client/v3/user/{}/openid/request_token",
                synapse_url(),
                self.user_id
            ))
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .context("failed to ask for an OpenID token")?
            .error_for_status()
            .context("the homeserver refused to mint an OpenID token")?
            .json()
            .await
            .context("the OpenID token answer is not JSON")?;
        Ok(token)
    }

    /// An account the test already has a token for — what the registration
    /// relay answers with (ticket #53), which is exactly the session the
    /// Companion continues in the browser. No password involved, which is the
    /// point: a homeserver may have password login disabled entirely.
    pub fn with_token(user_id: &str, access_token: &str) -> Self {
        Self {
            user_id: user_id.to_owned(),
            access_token: access_token.to_owned(),
            http: reqwest::Client::new(),
        }
    }

    /// The Matrix ID the homeserver says this token belongs to: proof that a
    /// token the relay handed back is a working session.
    pub async fn whoami(&self) -> Result<String> {
        let body: serde_json::Value = self
            .http
            .get(format!(
                "{}/_matrix/client/v3/account/whoami",
                synapse_url()
            ))
            .bearer_auth(&self.access_token)
            .send()
            .await
            .context("failed to call whoami")?
            .error_for_status()
            .context("the homeserver rejected the token")?
            .json()
            .await
            .context("the whoami answer is not JSON")?;
        Ok(body["user_id"]
            .as_str()
            .context("the whoami answer names no user id")?
            .to_owned())
    }

    /// Creates a private, unencrypted room owned by this account: a native
    /// Matrix room, with no bridge marker, which is what makes its traffic
    /// resolve to `network=matrix` (ADR 0009, ticket #18).
    pub async fn create_room(&self, name: &str) -> Result<String> {
        let body: serde_json::Value = self
            .http
            .post(format!("{}/_matrix/client/v3/createRoom", synapse_url()))
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({ "name": name, "preset": "private_chat" }))
            .send()
            .await
            .context("failed to create a room")?
            .error_for_status()
            .context("the homeserver refused to create the room")?
            .json()
            .await
            .context("the createRoom answer is not JSON")?;
        Ok(body["room_id"]
            .as_str()
            .context("the createRoom answer names no room id")?
            .to_owned())
    }

    /// Another user's membership in a room, as this account can read it, or
    /// `None` when there is no member event: how a test checks that the
    /// Gateway really invited the Sensor.
    pub async fn membership(&self, room_id: &str, user_id: &str) -> Result<Option<String>> {
        let response = self
            .http
            .get(format!(
                "{}/_matrix/client/v3/rooms/{room_id}/state/m.room.member/{user_id}",
                synapse_url()
            ))
            .bearer_auth(&self.access_token)
            .send()
            .await
            .context("failed to read a membership")?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let body: serde_json::Value = response
            .json()
            .await
            .context("the membership answer is not JSON")?;
        Ok(body["membership"].as_str().map(str::to_owned))
    }

    /// Sends a text message, as the user typing in their own client does.
    pub async fn send_message(&self, room_id: &str, body: &str) -> Result<String> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let answer: serde_json::Value = self
            .http
            .put(format!(
                "{}/_matrix/client/v3/rooms/{room_id}/send/m.room.message/twalk-g53-{unique}",
                synapse_url()
            ))
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({ "msgtype": "m.text", "body": body }))
            .send()
            .await
            .context("failed to send a message")?
            .error_for_status()
            .context("the homeserver refused the message")?
            .json()
            .await
            .context("the send answer is not JSON")?;
        Ok(answer["event_id"]
            .as_str()
            .context("the send answer names no event id")?
            .to_owned())
    }

    /// The account's Matrix access token: what the Gateway must never hold,
    /// and what the store and the logs are asserted against.
    pub fn matrix_access_token(&self) -> &str {
        &self.access_token
    }
}

/// The bus stream and subject the Gateway publishes consent decisions on
/// (ticket #49) — the Sensor's stream, the contract's subject.
pub const CONSENT_STREAM: &str = "twalk";
pub const CONSENT_SUBJECT: &str = "twalk.consent.state.changed.v1";

/// [`gateway_env`] plus the one variable the consent store adds: the bus its
/// outbox publishes to. Everything else consent needs — the state directory,
/// the owner, the domain its events name themselves by — it takes from the
/// sign-in configuration already in [`gateway_env`].
pub fn gateway_env_with_consent(static_dir: &Path, nats_url: &str) -> Vec<(String, String)> {
    gateway_env_with(static_dir, &[("GATEWAY_NATS_URL", nats_url)])
}

/// The two bridge instances the bridge suite configures the Gateway with
/// (ticket #55): one pointed at a running stub, one pointed at a port
/// nothing listens on — a bridge that is down is a state an operator has,
/// and the facade has to answer for it.
pub const STUB_BRIDGE_ID: &str = "mautrix-stub";
pub const UNREACHABLE_BRIDGE_ID: &str = "mautrix-unreachable";

/// [`gateway_env`] plus the bridge facade's configuration: the stub bridge
/// above, and an instance whose listener is dead.
///
/// The variable names are the ones an operator writes: `GATEWAY_BRIDGES`
/// lists the instances by `bridge_id`, and each id becomes the middle of its
/// own three variables (`config::variable_slug`).
pub fn gateway_env_with_bridges(static_dir: &Path, stub_base_url: &str) -> Vec<(String, String)> {
    let dead = unreachable_http_url().expect("the kernel can hand out a free port");
    gateway_env_with(
        static_dir,
        &[
            (
                "GATEWAY_BRIDGES",
                &format!("{STUB_BRIDGE_ID},{UNREACHABLE_BRIDGE_ID}"),
            ),
            ("GATEWAY_BRIDGE_MAUTRIX_STUB_URL", stub_base_url),
            (
                "GATEWAY_BRIDGE_MAUTRIX_STUB_PROVISIONING_SECRET",
                STUB_PROVISIONING_SECRET,
            ),
            ("GATEWAY_BRIDGE_MAUTRIX_STUB_NETWORK", "whatsapp"),
            ("GATEWAY_BRIDGE_MAUTRIX_UNREACHABLE_URL", &dead),
            (
                "GATEWAY_BRIDGE_MAUTRIX_UNREACHABLE_PROVISIONING_SECRET",
                STUB_PROVISIONING_SECRET,
            ),
            ("GATEWAY_BRIDGE_MAUTRIX_UNREACHABLE_NETWORK", "signal"),
        ],
    )
}

/// An HTTP URL nothing listens on: asked of the kernel, then released.
pub fn unreachable_http_url() -> Result<String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .context("failed to ask the kernel for a free port")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(format!("http://127.0.0.1:{port}"))
}

/// A TCP port nothing listens on: asked of the kernel, then released. The
/// crash test points a Gateway at it to record decisions the bus cannot have
/// heard.
pub fn unreachable_nats_url() -> Result<String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .context("failed to ask the kernel for a free port")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(format!("nats://127.0.0.1:{port}"))
}

/// Signs the owner's device in and returns its device token: what every
/// endpoint behind #52's guard needs.
///
/// The sign-in suite drives this flow by hand, because the flow is what it
/// tests; every other suite only needs a device that is signed in, and says
/// so in one line.
pub async fn signed_in_device_token(base: &str) -> Result<String> {
    let owner = MatrixUser::login(OWNER_LOCALPART).await?;
    let response = reqwest::Client::new()
        .post(format!("{base}/api/session"))
        .json(&serde_json::json!({
            "matrix_openid_token": owner.openid_token().await?,
            "device_name": "the test's device",
        }))
        .send()
        .await
        .context("failed to post a sign-in")?;
    anyhow::ensure!(
        response.status() == reqwest::StatusCode::OK,
        "the owner's sign-in was refused with {}: {}",
        response.status(),
        response.text().await.unwrap_or_default()
    );
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';').unwrap_or((value, ""));
            let (name, token) = pair.split_once('=')?;
            (name == "twalk_device").then(|| token.to_owned())
        })
        .context("the sign-in set no device cookie")
}

/// `gateway_env` with per-test overrides: an existing key is replaced, a new
/// key is appended. An override with an empty value removes the variable, so
/// a test can exercise a missing required variable.
pub fn gateway_env_with(static_dir: &Path, overrides: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut env = gateway_env(static_dir);
    for (key, value) in overrides {
        if value.is_empty() {
            env.retain(|(existing, _)| existing != key);
            continue;
        }
        if let Some(entry) = env.iter_mut().find(|(existing, _)| existing == key) {
            entry.1 = value.to_string();
        } else {
            env.push((key.to_string(), value.to_string()));
        }
    }
    env
}

/// The metric sample names with their values, parsed from the Prometheus text
/// exposition — the same parser the Sensor's observability suite uses.
pub fn parse_exposition(body: &str) -> Vec<(String, u64)> {
    body.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (name, value) = line
                .rsplit_once(' ')
                .unwrap_or_else(|| panic!("metric line has no value: {line:?}"));
            let value = value
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("metric value is not an integer: {line:?}"));
            (name.to_owned(), value)
        })
        .collect()
}

/// The stricter W3C origin form the Gateway originates and accepts:
/// `00-<32 lowercase hex>-<16 lowercase hex>-<2 hex>`.
pub fn assert_valid_traceparent(value: &str) {
    let parts: Vec<&str> = value.split('-').collect();
    assert_eq!(parts.len(), 4, "a traceparent has four parts: {value}");
    assert_eq!(parts[0], "00", "traceparent version: {value}");
    assert_eq!(parts[1].len(), 32, "trace id length: {value}");
    assert_eq!(parts[2].len(), 16, "span id length: {value}");
    assert_eq!(parts[3].len(), 2, "trace flags length: {value}");
    for hex in [&parts[1], &parts[2], &parts[3]] {
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "ids are lowercase hex: {value}"
        );
    }
}
