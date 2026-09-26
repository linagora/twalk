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

pub mod bridge_fixtures;
pub mod stub_bridge;

pub use bridge_fixtures::Bridge as FixtureBridge;
pub use stub_bridge::{StubBridge, STUB_AS_TOKEN, STUB_PROVISIONING_SECRET};
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
    // The build id SvelteKit writes, and the worker's script: both are names
    // that outlive a build, so both are the shell's policy and never the
    // immutable assets' (#222).
    std::fs::write(
        dir.join("_app/version.json"),
        format!("{{\"version\":\"{COMPANION_BUILD_ID}\"}}"),
    )?;
    std::fs::write(dir.join("service-worker.js"), SERVICE_WORKER_JS)?;
    Ok(dir)
}

/// The build id the fake export carries, which `/health` reports as
/// `companion_build` (#222).
pub const COMPANION_BUILD_ID: &str = "1789839442194-test";
pub const SERVICE_WORKER_JS: &str = "// the fake worker\n";

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

/// The `m.room.create` type the Companion gives the handover room, so that
/// nothing counts it as a conversation (ticket #226,
/// `companion/src/lib/matrix/handover.ts`).
pub const HANDOVER_ROOM_TYPE: &str = "fr.linagora.twalk.handover";

/// The `events_default` that room is created with: one above the 100 a room's
/// creator holds, so the homeserver refuses every message event from every
/// member.
pub const HANDOVER_SEND_LEVEL_NOBODY_HAS: u64 = 101;

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

/// One connection per network a suite decides on, named after it.
pub const TEST_CONNECTIONS: &str =
    "whatsapp=whatsapp,signal=signal,telegram=telegram,discord=discord,sms=sms";

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
        // The registry of connections (#269, #270): a Gateway derives one
        // per bridge, and this Gateway has no bridge, so the networks the
        // suites decide on are declared — named after their network, the
        // reference deployment's shape. `gateway_env_with_bridges` clears it
        // so the bridged Gateway runs the derived path.
        (
            "GATEWAY_CONNECTIONS".to_owned(),
            TEST_CONNECTIONS.to_owned(),
        ),
        // The pending-contact projection's durable consumer (ticket #54).
        // A deployment has one Gateway and uses the default name; the test
        // stack has one bus shared by every suite and every run, so each
        // test's Gateway gets its own — two Gateways on one durable name
        // would split the inbound stream between them, and each would see
        // half the contacts.
        //
        // Derived from the static directory, which is unique per test: a
        // Gateway restarted on the same directory keeps the same consumer
        // and therefore the same ack floor, which is exactly the property
        // `tests/pending.rs` restarts one to assert.
        (
            "GATEWAY_INBOUND_CONSUMER".to_owned(),
            inbound_consumer_name(static_dir),
        ),
    ]
}

/// A durable consumer name for a test's Gateway, from its static directory.
/// NATS refuses `.`, `*`, `>`, `/`, `\` and whitespace in a durable name, so
/// everything but letters, digits and `-` becomes `-`.
pub fn inbound_consumer_name(static_dir: &Path) -> String {
    let slug: String = static_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    format!("g54-{slug}")
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
/// Percent-encodes one path segment. mautrix's `m.bridge` state key is
/// `<server_name>/<appservice_id>`, and a raw slash there is a different URL.
fn path_segment(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

pub struct MatrixUser {
    pub user_id: String,
    access_token: String,
    /// `Some(user_id)` when [`Self::access_token`] is an **appservice** token
    /// and every call must name the user it acts as in `?user_id=`.
    ///
    /// This is the whole of what an appservice handle is, and the reason it
    /// exists in the harness at all (#171): Synapse honours that parameter
    /// only for a token that belongs to an appservice, so the parameter the
    /// portal register depends on cannot be exercised by any handle built on
    /// an ordinary access token. `None` is an ordinary session and behaves
    /// exactly as it always did.
    masquerade: Option<String>,
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
            masquerade: None,
            http,
        })
    }

    /// A fresh OpenID token, exactly as the homeserver answers it
    /// (`access_token`, `token_type`, `matrix_server_name`, `expires_in`) —
    /// the document the Companion forwards to the Gateway unchanged.
    pub async fn openid_token(&self) -> Result<serde_json::Value> {
        let token: serde_json::Value = self
            .http
            .post(self.url(&format!(
                "/_matrix/client/v3/user/{}/openid/request_token",
                self.user_id
            )))
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
            masquerade: None,
            http: reqwest::Client::new(),
        }
    }

    /// A handle that acts as `localpart` **through the test stack's appservice
    /// token**, the way a real bridge acts as its own bot.
    ///
    /// The account needs no registration and no password: an appservice may
    /// act as any user its namespace covers, and Synapse creates the profile
    /// on first use. `tests/harness/synapse/appservice-portals.yaml` is the
    /// registration, and its namespace is `@portalbot…`.
    ///
    /// This is the only handle in the harness for which `?user_id=` does
    /// anything at all, which is exactly why the portal register's suite needs
    /// it: the appservice's own `sender_localpart` is an account in no rooms,
    /// so a register that forgets the parameter reads nothing here for the
    /// same reason it read nothing on the reference deployment (#171).
    pub async fn as_appservice(localpart: &str) -> Result<Self> {
        let user_id = format!("@{localpart}:{SERVER_NAME}");
        let http = reqwest::Client::new();
        // Synapse refuses to let an appservice act as a user it has not
        // registered, even one its namespace covers ("Application service has
        // not registered this user"). A real mautrix bridge registers its bot
        // and each ghost exactly like this, so the fixture does too.
        let response = http
            .post(format!("{}/_matrix/client/v3/register", synapse_url()))
            .bearer_auth(PORTALS_APPSERVICE_AS_TOKEN)
            .json(&serde_json::json!({
                "type": "m.login.application_service",
                "username": localpart,
            }))
            .send()
            .await
            .context("failed to register an appservice user")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        // Already registered is success: this is idempotent across runs, and
        // the stack outlives a run.
        anyhow::ensure!(
            status.is_success() || body.contains("M_USER_IN_USE"),
            "the homeserver refused to register the appservice user {user_id}: {status} {body}. \
             The registration in tests/harness/synapse/appservice-portals.yaml has to cover this \
             localpart, and the stack has to have loaded it."
        );
        Ok(Self {
            user_id: user_id.clone(),
            access_token: PORTALS_APPSERVICE_AS_TOKEN.to_owned(),
            masquerade: Some(user_id),
            http,
        })
    }

    /// The same, with a localpart nobody else's rooms are in.
    ///
    /// A bridge bot's whole answer to "which conversations exist?" is the list
    /// of rooms it is joined to, so a bot shared between tests would carry one
    /// test's portals into another's register.
    pub async fn as_fresh_appservice(prefix: &str) -> Result<Self> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        Self::as_appservice(&format!("{prefix}_{unique}")).await
    }

    /// The appservice token itself — what a bridge configures the Gateway with
    /// as `GATEWAY_BRIDGE_<ID>_AS_TOKEN`, and which by itself acts as an
    /// account in no rooms.
    pub fn appservice_token(&self) -> &str {
        &self.access_token
    }

    /// A client-server URL for this handle, carrying the masquerade when it has
    /// one. Every call below goes through it, so a method added later cannot
    /// quietly act as the wrong account — the mistake #171 was.
    fn url(&self, path: &str) -> String {
        match &self.masquerade {
            Some(user_id) => format!("{}{path}?user_id={}", synapse_url(), path_segment(user_id)),
            None => format!("{}{path}", synapse_url()),
        }
    }

    /// The Matrix ID the homeserver says this token belongs to: proof that a
    /// token the relay handed back is a working session.
    pub async fn whoami(&self) -> Result<String> {
        let body: serde_json::Value = self
            .http
            .get(self.url("/_matrix/client/v3/account/whoami"))
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
            .post(self.url("/_matrix/client/v3/createRoom"))
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
            .get(self.url(&format!(
                "/_matrix/client/v3/rooms/{room_id}/state/m.room.member/{user_id}"
            )))
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
            .put(self.url(&format!(
                "/_matrix/client/v3/rooms/{room_id}/send/m.room.message/twalk-g53-{unique}"
            )))
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

    /// Tries to send a text message and answers what the homeserver said: the
    /// `errcode` of a refusal, or `None` when it was accepted.
    ///
    /// The sibling of [`Self::send_message`] for a room where a refusal is the
    /// property under test — the handover room of ticket #226, where nobody may
    /// post at all.
    pub async fn refusal_to_send(&self, room_id: &str, body: &str) -> Result<Option<String>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let response = self
            .http
            .put(self.url(&format!(
                "/_matrix/client/v3/rooms/{room_id}/send/m.room.message/twalk-g226-{unique}"
            )))
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({ "msgtype": "m.text", "body": body }))
            .send()
            .await
            .context("failed to send a message")?;
        if response.status().is_success() {
            return Ok(None);
        }
        let document: serde_json::Value = response
            .json()
            .await
            .context("the refusal is not a Matrix error")?;
        Ok(Some(
            document["errcode"]
                .as_str()
                .unwrap_or("M_UNKNOWN")
                .to_owned(),
        ))
    }

    /// The **handover room** of ticket #226, as the Companion creates one in
    /// the browser: encrypted, typed as something other than a conversation,
    /// the Sensor invited, and `events_default` above any power level a member
    /// can hold so that nobody — the creator included — may post in it.
    ///
    /// This mirrors `companion/src/lib/matrix/handover.ts`'s
    /// `handoverRoomCreation`, which is the authority on the shape and pins it
    /// in its own test. Two spellings of one body is the cost of the room being
    /// created by a browser and asserted about by a Rust suite; what keeps them
    /// honest is that each side asserts the *properties* rather than the JSON.
    pub async fn make_handover_room(&self, name: &str, sensor_user_id: &str) -> Result<String> {
        let body: serde_json::Value = self
            .http
            .post(self.url("/_matrix/client/v3/createRoom"))
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({
                "preset": "private_chat",
                "visibility": "private",
                "name": name,
                "invite": [sensor_user_id],
                "is_direct": false,
                "creation_content": { "type": HANDOVER_ROOM_TYPE },
                "initial_state": [{
                    "type": "m.room.encryption",
                    "state_key": "",
                    "content": { "algorithm": "m.megolm.v1.aes-sha2" },
                }],
                "power_level_content_override": {
                    "events_default": HANDOVER_SEND_LEVEL_NOBODY_HAS,
                    "invite": 100,
                    "kick": 100,
                    "redact": 100,
                },
            }))
            .send()
            .await
            .context("failed to create the handover room")?
            .error_for_status()
            .context("the homeserver refused to create the handover room")?
            .json()
            .await
            .context("the createRoom answer is not JSON")?;
        Ok(body["room_id"]
            .as_str()
            .context("the createRoom answer names no room id")?
            .to_owned())
    }

    /// A brand-new account on the test stack, with a session.
    ///
    /// The portal tests need an account nobody else's rooms are in: a bridge
    /// bot's whole answer to "which conversations exist?" is the list of
    /// rooms it is joined to, so a shared bot would carry every previous
    /// test's portals into this one's register. The test stack's Synapse has
    /// open registration (`tests/harness/synapse/homeserver.yaml`), which is
    /// what makes this one call rather than an admin credential.
    pub async fn register_fresh(prefix: &str) -> Result<Self> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let localpart = format!("{prefix}_{unique}");
        let http = reqwest::Client::new();
        let body: serde_json::Value = http
            .post(format!("{}/_matrix/client/v3/register", synapse_url()))
            .json(&serde_json::json!({
                "username": localpart,
                "password": format!("test-only-password-{localpart}"),
                "auth": { "type": "m.login.dummy" },
                "inhibit_login": false,
            }))
            .send()
            .await
            .context("failed to register a test user")?
            .error_for_status()
            .context("the homeserver refused the registration")?
            .json()
            .await
            .context("the registration answer is not JSON")?;
        Ok(Self {
            user_id: body["user_id"]
                .as_str()
                .context("the registration answer names no user id")?
                .to_owned(),
            access_token: body["access_token"]
                .as_str()
                .context("the registration answer carries no access token")?
                .to_owned(),
            masquerade: None,
            http,
        })
    }

    /// Invites another account into a room.
    pub async fn invite(&self, room_id: &str, user_id: &str) -> Result<()> {
        self.http
            .post(self.url(&format!("/_matrix/client/v3/rooms/{room_id}/invite")))
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({ "user_id": user_id }))
            .send()
            .await
            .context("failed to invite into a room")?
            .error_for_status()
            .context("the homeserver refused the invitation")?;
        Ok(())
    }

    /// Leaves a room.
    pub async fn leave(&self, room_id: &str) -> Result<()> {
        self.http
            .post(self.url(&format!("/_matrix/client/v3/rooms/{room_id}/leave")))
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .context("failed to leave a room")?
            .error_for_status()
            .context("the homeserver refused the leave")?;
        Ok(())
    }

    /// Joins a room this account was invited to — what the Sensor does on its
    /// own, played here by a test account.
    pub async fn join(&self, room_id: &str) -> Result<()> {
        self.http
            .post(self.url(&format!("/_matrix/client/v3/rooms/{room_id}/join")))
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .context("failed to join a room")?
            .error_for_status()
            .context("the homeserver refused the join")?;
        Ok(())
    }

    /// Sets one state event.
    pub async fn send_state_event(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
        content: serde_json::Value,
    ) -> Result<()> {
        self.http
            .put(self.url(&format!(
                "/_matrix/client/v3/rooms/{room_id}/state/{event_type}/{}",
                path_segment(state_key)
            )))
            .bearer_auth(&self.access_token)
            .json(&content)
            .send()
            .await
            .context("failed to send a state event")?
            .error_for_status()
            .context("the homeserver refused the state event")?;
        Ok(())
    }

    /// A portal room, as a bridge builds one: this account creates it (so it
    /// is the room's admin, as a bridge bot is), marks it with the `m.bridge`
    /// state event the Sensor attributes a network by
    /// (`sensor/src/network.rs`), and pulls the named accounts in as the
    /// conversation's members.
    ///
    /// The marker's `bridgebot` names this account, which is how the register
    /// knows not to count the bot as somebody in the conversation.
    pub async fn make_portal(&self, name: &str, protocol_id: &str) -> Result<String> {
        let room_id = self.create_room(name).await?;
        self.mark_as_portal(&room_id, protocol_id, name).await?;
        Ok(room_id)
    }

    /// Writes the `m.bridge` marker into a room that already exists — what a
    /// bridge does to a successor it re-creates after a migration, since the
    /// homeserver's room upgrade copies no custom state.
    pub async fn mark_as_portal(&self, room_id: &str, protocol_id: &str, name: &str) -> Result<()> {
        self.send_state_event(
            room_id,
            "m.bridge",
            &format!("test.twalk/{protocol_id}"),
            serde_json::json!({
                "bridgebot": self.user_id,
                "protocol": { "id": protocol_id, "displayname": protocol_id },
                "channel": { "id": format!("{protocol_id}-{name}"), "displayname": name },
            }),
        )
        .await
    }

    /// Replaces a room with a new one, as the homeserver does it: the old
    /// room gets an `m.room.tombstone` naming the successor, the successor's
    /// `m.room.create` names its predecessor. Returns the successor's id.
    ///
    /// This is a Matrix room upgrade and not a bridge's migration, on
    /// purpose: `m.room.tombstone` predates every bridge, and ADR 0029 wants
    /// what follows from it to hold for a native room exactly as for a
    /// portal. The homeserver copies the room's name and power levels but
    /// **not** its `m.bridge` marker and not its members — a bridge re-marks
    /// and re-invites, so a test that wants a portal successor does the same.
    pub async fn upgrade_room(&self, room_id: &str) -> Result<String> {
        let body: serde_json::Value = self
            .http
            .post(self.url(&format!("/_matrix/client/v3/rooms/{room_id}/upgrade")))
            .bearer_auth(&self.access_token)
            .json(&serde_json::json!({ "new_version": "10" }))
            .send()
            .await
            .context("failed to upgrade a room")?
            .error_for_status()
            .context("the homeserver refused the room upgrade")?
            .json()
            .await
            .context("the upgrade answer is not JSON")?;
        body["replacement_room"]
            .as_str()
            .map(str::to_owned)
            .context("the upgrade answer names no replacement_room")
    }

    /// The accounts joined to a room, as this account can read them.
    pub async fn joined_members(&self, room_id: &str) -> Result<Vec<String>> {
        let body: serde_json::Value = self
            .http
            .get(self.url(&format!(
                "/_matrix/client/v3/rooms/{room_id}/joined_members"
            )))
            .bearer_auth(&self.access_token)
            .send()
            .await
            .context("failed to read a room's members")?
            .error_for_status()
            .context("the homeserver refused to list the members")?
            .json()
            .await
            .context("the joined_members answer is not JSON")?;
        Ok(body["joined"]
            .as_object()
            .map(|joined| joined.keys().cloned().collect())
            .unwrap_or_default())
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
    gateway_env_with(
        static_dir,
        &[
            ("GATEWAY_NATS_URL", nats_url),
            // The test stack's bus is shared by every suite and lives for
            // days, so "not on the bus" must be a search of the whole
            // stream, not of the default 20000 positions: a long-lived
            // stack would otherwise turn every `*_not_found` into
            // `*_out_of_reach`. A suite about the window sets its own.
            ("GATEWAY_APPROVAL_LOOKUP_WINDOW", "100000000"),
        ],
    )
}

/// The two bridge instances the bridge suite configures the Gateway with
/// (ticket #55): one pointed at a running stub, one pointed at a port
/// nothing listens on — a bridge that is down is a state an operator has,
/// and the facade has to answer for it.
pub const STUB_BRIDGE_ID: &str = "mautrix-stub";
pub const UNREACHABLE_BRIDGE_ID: &str = "mautrix-unreachable";

/// The same two bridges as the **event contract** names them (ticket #56):
/// `^bridge-[a-z0-9-]+$`, which is what `bridge.status.changed.v1` carries
/// and what the status webhook's URL has in it. Neither is configured
/// explicitly — these are `config::default_status_bridge_id` of the ids
/// above, which is the whole point of the default.
pub const STUB_STATUS_BRIDGE_ID: &str = "bridge-stub";
pub const UNREACHABLE_STATUS_BRIDGE_ID: &str = "bridge-unreachable";

/// The bus subject `bridge.status.changed.v1` is published on, and its
/// contract type.
pub const BRIDGE_STATUS_SUBJECT: &str = "twalk.bridge.status.changed.v1";
pub const BRIDGE_STATUS_TYPE: &str = "fr.linagora.twalk.bridge.status.changed.v1";

/// The status webhook's path for one bridge, as an operator writes it into
/// that bridge's `homeserver.status_endpoint`.
pub fn bridge_status_path(status_bridge_id: &str) -> String {
    format!("/_twalk/bridges/{status_bridge_id}/status")
}

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
            // Derived from the bridges, not declared (#269).
            ("GATEWAY_CONNECTIONS", ""),
            ("GATEWAY_BRIDGE_MAUTRIX_STUB_URL", stub_base_url),
            (
                "GATEWAY_BRIDGE_MAUTRIX_STUB_PROVISIONING_SECRET",
                STUB_PROVISIONING_SECRET,
            ),
            ("GATEWAY_BRIDGE_MAUTRIX_STUB_NETWORK", "whatsapp"),
            // The appservice token this bridge's status pushes are verified
            // against (ticket #56). The unreachable bridge deliberately has
            // none: a bridge an operator wired up without giving the Gateway
            // its token is a state that must be refused, not trusted.
            ("GATEWAY_BRIDGE_MAUTRIX_STUB_AS_TOKEN", STUB_AS_TOKEN),
            ("GATEWAY_BRIDGE_MAUTRIX_UNREACHABLE_URL", &dead),
            (
                "GATEWAY_BRIDGE_MAUTRIX_UNREACHABLE_PROVISIONING_SECRET",
                STUB_PROVISIONING_SECRET,
            ),
            ("GATEWAY_BRIDGE_MAUTRIX_UNREACHABLE_NETWORK", "signal"),
        ],
    )
}

/// The bridge instances the portal register's suite configures (tickets #105
/// and #171).
///
/// # Why the credential is a real appservice token (#171)
///
/// It used to be an ordinary account's access token, on the reasoning that
/// the register only speaks the client-server API and Synapse answers the
/// same calls for both. That reasoning was sound and the fixture it produced
/// could not fail on the defect it needed to catch: acting as the bridge bot
/// takes `?user_id=`, Synapse honours that parameter **only** for an
/// appservice token, and for an ordinary token the asker *is* the sender — so
/// whether the register sent the parameter was unobservable, in either
/// direction, and a register that never sent it passed.
///
/// So this fixture is the deployment's own shape. The credential is the test
/// stack's appservice token (`tests/harness/synapse/appservice-portals.yaml`),
/// whose `sender_localpart` is an account in **no rooms** — as a generated
/// mautrix registration's sender is — and the account that is in every portal
/// is the bot named in `GATEWAY_BRIDGE_<ID>_BOT_USER_ID`. A register that
/// forgets `?user_id=` therefore reads zero rooms here for exactly the reason
/// it read zero of 32 on the reference deployment.
///
/// Three bridge instances, and each one is a distinct fact about the register:
///
/// - [`PORTAL_BRIDGE_ID`] — the working shape: appservice token plus the
///   bot's Matrix ID.
/// - [`PORTAL_TOKENLESS_BRIDGE_ID`] — no credential at all. Reported as
///   unreadable with the variable that would open it, never silently missing:
///   "the Sensor is outside 17 of your 18 conversations" must never quietly
///   mean "…of the 18 I could see".
/// - [`PORTAL_SENDER_BRIDGE_ID`] — the **defect's own configuration**: the
///   appservice token with no bot named, so the register falls back to the
///   token's own identity and acts as an account in no rooms. It must answer
///   `readable: true` with `joined_rooms: 0` and name that account, because
///   the whole cost of #171 was a zero nobody could attribute.
pub const PORTAL_BRIDGE_ID: &str = "mautrix-portal";
pub const PORTAL_TOKENLESS_BRIDGE_ID: &str = "mautrix-tokenless";
pub const PORTAL_SENDER_BRIDGE_ID: &str = "mautrix-sender";

pub fn gateway_env_with_portals(
    static_dir: &Path,
    bridge_bot: &MatrixUser,
) -> Vec<(String, String)> {
    let dead = unreachable_http_url().expect("the kernel can hand out a free port");
    gateway_env_with(
        static_dir,
        &[
            (
                "GATEWAY_BRIDGES",
                &format!(
                    "{PORTAL_BRIDGE_ID},{PORTAL_TOKENLESS_BRIDGE_ID},{PORTAL_SENDER_BRIDGE_ID}"
                ),
            ),
            // The register never calls a bridge's provisioning API — it asks
            // the homeserver — so these two instances need no listener, and
            // the dead port is the proof that it does not.
            ("GATEWAY_BRIDGE_MAUTRIX_PORTAL_URL", &dead),
            (
                "GATEWAY_BRIDGE_MAUTRIX_PORTAL_PROVISIONING_SECRET",
                "test-only-unused-provisioning-secret",
            ),
            ("GATEWAY_BRIDGE_MAUTRIX_PORTAL_NETWORK", "whatsapp"),
            (
                "GATEWAY_BRIDGE_MAUTRIX_PORTAL_AS_TOKEN",
                bridge_bot.appservice_token(),
            ),
            // The account the register must act as. Configured, never derived
            // from the bridge id (#171, and ADR 0018's reason).
            (
                "GATEWAY_BRIDGE_MAUTRIX_PORTAL_BOT_USER_ID",
                &bridge_bot.user_id,
            ),
            ("GATEWAY_BRIDGE_MAUTRIX_TOKENLESS_URL", &dead),
            (
                "GATEWAY_BRIDGE_MAUTRIX_TOKENLESS_PROVISIONING_SECRET",
                "test-only-unused-provisioning-secret",
            ),
            ("GATEWAY_BRIDGE_MAUTRIX_TOKENLESS_NETWORK", "signal"),
            // #171's own configuration: a credential, and nobody named to act
            // as. The register falls back to the token's own identity, which
            // for an appservice token is its `sender_localpart` — an account
            // in no rooms.
            ("GATEWAY_BRIDGE_MAUTRIX_SENDER_URL", &dead),
            (
                "GATEWAY_BRIDGE_MAUTRIX_SENDER_PROVISIONING_SECRET",
                "test-only-unused-provisioning-secret",
            ),
            ("GATEWAY_BRIDGE_MAUTRIX_SENDER_NETWORK", "telegram"),
            (
                "GATEWAY_BRIDGE_MAUTRIX_SENDER_AS_TOKEN",
                bridge_bot.appservice_token(),
            ),
            // The background refresh is off: every number this suite asserts
            // must come from a read it made itself, so that a passing test is
            // never a timer that happened to fire.
            ("GATEWAY_PORTAL_REFRESH_SECONDS", "0"),
        ],
    )
}

/// [`gateway_env_with_bridges`] plus the bus: what a deployment that reports
/// bridge status has (ticket #56). The status half shares the consent
/// store's journal and the consent bus, so it is on exactly when they are.
pub fn gateway_env_with_bridges_and_consent(
    static_dir: &Path,
    stub_base_url: &str,
    nats_url: &str,
) -> Vec<(String, String)> {
    let mut env = gateway_env_with_bridges(static_dir, stub_base_url);
    env.push(("GATEWAY_NATS_URL".to_owned(), nats_url.to_owned()));
    env
}

/// The secret the Gateway and Hermes share for the answer webhook (ticket
/// #206). Long enough to pass the Gateway's own minimum, which is the point:
/// a suite that used a short one would be testing a Gateway that refuses to
/// start.
pub const HERMES_ANSWER_SECRET: &str = "a-throwaway-hermes-answer-secret-for-the-test-stack-only";

/// The domain the Gateway names a published suggestion's persona under
/// (`GATEWAY_HERMES_DOMAIN`), and therefore the authority of every `source`
/// this suite reads back off the bus.
pub const HERMES_DOMAIN: &str = "twalk.test";

/// [`gateway_env_with_consent`] plus the two variables the seam to Hermes
/// adds. Everything else it needs — the store, the bus, the owner — it takes
/// from the consent configuration, because it reuses the approval half.
pub fn gateway_env_with_hermes(static_dir: &Path, nats_url: &str) -> Vec<(String, String)> {
    // A calendar connection no collector ever speaks for (#281): what a
    // free/busy read on it is refused with, without any bus state.
    let connections = format!("{TEST_CONNECTIONS},{UNSPOKEN_CALENDAR_CONNECTION}=calendar");
    gateway_env_with(
        static_dir,
        &[
            ("GATEWAY_NATS_URL", nats_url),
            ("GATEWAY_HERMES_ANSWER_SECRET", HERMES_ANSWER_SECRET),
            ("GATEWAY_HERMES_DOMAIN", HERMES_DOMAIN),
            // See `gateway_env_with_consent`.
            ("GATEWAY_APPROVAL_LOOKUP_WINDOW", "100000000"),
            ("GATEWAY_CONNECTIONS", connections.as_str()),
        ],
    )
}

/// A calendar connection the Hermes Gateway declares and no collector has
/// reported (#281).
pub const UNSPOKEN_CALENDAR_CONNECTION: &str = "calendar-unspoken";

/// The signature Hermes's outbound hook puts on a push: hex HMAC-SHA256 over
/// the raw body, prefixed `sha256=`.
///
/// Computed here from the wire format rather than from the Gateway's own code,
/// so the test states the contract instead of agreeing with the
/// implementation.
pub fn hermes_signature(body: &str) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(HERMES_ANSWER_SECRET.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(body.as_bytes());
    format!("sha256={:x}", mac.finalize().into_bytes())
}

/// The query string of a free/busy read (#281), encoded the way the skill's
/// script encodes it — `:` and `+` percent-encoded — since the signature
/// covers the query as sent and the test signs what it sends.
pub fn freebusy_query(connection: &str, from: &str, to: &str) -> String {
    let encode = |value: &str| {
        value
            .replace('%', "%25")
            .replace(':', "%3A")
            .replace('+', "%2B")
            .replace('&', "%26")
            .replace('=', "%3D")
    };
    format!(
        "connection={}&from={}&to={}",
        encode(connection),
        encode(from),
        encode(to)
    )
}

/// The signature of a free/busy read (#281): hex HMAC-SHA256 over the
/// canonical line `GET`, the path, the query as sent and the timestamp,
/// newline-separated, prefixed `sha256=`. Computed from the wire format in
/// the skill's document rather than from the Gateway's code, so the test
/// states the contract.
pub fn freebusy_signature(query: &str, timestamp: &str) -> String {
    hermes_read_signature("/_twalk/hermes/freebusy", query, timestamp)
}

/// The query of a read of what one event carries (#355), encoded as the
/// skill encodes it — a uid is opaque and may hold anything.
pub fn event_facts_query(connection: &str, uid: &str) -> String {
    let encode = |value: &str| {
        value
            .replace('%', "%25")
            .replace(':', "%3A")
            .replace('+', "%2B")
            .replace('&', "%26")
            .replace('=', "%3D")
            .replace('/', "%2F")
    };
    format!("connection={}&uid={}", encode(connection), encode(uid))
}

pub fn event_facts_signature(query: &str, timestamp: &str) -> String {
    hermes_read_signature("/_twalk/hermes/event-facts", query, timestamp)
}

/// The signature of one of Hermes's reads. **The path is inside the signed
/// line**, so a signature made for one read does not open the other — which
/// is a property this helper's shape makes a test able to check rather than
/// assume.
pub fn hermes_read_signature(path: &str, query: &str, timestamp: &str) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(HERMES_ANSWER_SECRET.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(format!("GET\n{path}\n{query}\n{timestamp}").as_bytes());
    format!("sha256={:x}", mac.finalize().into_bytes())
}

/// One of Hermes's `transform_llm_output` pushes, carrying whatever the model
/// is said to have written, stamped now.
///
/// A string and not a `Value`, because the signature covers the bytes: a test
/// that serialised a `Value` twice would sign one body and send another, which
/// is the mistake this helper exists to make impossible.
pub fn hermes_push(response_text: &str) -> String {
    hermes_push_at(response_text, &rfc3339_now())
}

/// The same push, stamped at a moment the caller chooses — for the replay
/// assertion, where the timestamp is the thing under test.
pub fn hermes_push_at(response_text: &str, timestamp: &str) -> String {
    serde_json::json!({
        "hook_event_name": "transform_llm_output",
        "tool_name": null,
        "tool_input": null,
        "session_id": "",
        "cwd": "/home/hermes",
        "extra": {
            "response_text": response_text,
            "session_id": "agent:main:webhook:webhook:webhook:twalk-messages:deadbeef",
            "platform": "webhook",
            "model": "a-model-the-operator-named",
        },
        "delivery_id": "1f2e3d4c5b6a79887766554433221100",
        "timestamp": timestamp,
    })
    .to_string()
}

/// The answer a route's prompt asks Hermes for: a reference, a reply, and the
/// language the reply is written in.
pub fn hermes_answer(reference: &str, reply: &str, language: Option<&str>) -> String {
    hermes_answer_saying(reference, reply, language, None)
}

/// The same, with what Hermes says the message asks (#360). `None` is a
/// Hermes older than that member, which is a case this deployment must keep
/// answering.
pub fn hermes_answer_saying(
    reference: &str,
    reply: &str,
    language: Option<&str>,
    summary: Option<&str>,
) -> String {
    let mut answer = serde_json::json!({ "reference": reference, "reply": reply });
    if let Some(language) = language {
        answer["language"] = serde_json::Value::String(language.to_owned());
    }
    if let Some(summary) = summary {
        answer["summary"] = serde_json::Value::String(summary.to_owned());
    }
    answer.to_string()
}

/// A reply that offers times, saying which instants they are (#383).
pub fn hermes_answer_proposing(reference: &str, reply: &str, proposed: &[&str]) -> String {
    serde_json::json!({
        "reference": reference,
        "reply": reply,
        "language": "fr",
        "proposed": proposed,
    })
    .to_string()
}

/// A wake that ends in a question to the owner instead of a draft (#367):
/// the reference, and what the agent says it needs, and no reply at all.
pub fn hermes_deferral(reference: &str, asked: &str) -> String {
    serde_json::json!({ "reference": reference, "deferred": asked }).to_string()
}

/// The `TWALK-REF:` token a persona puts in a wake and Hermes copies back.
pub fn hermes_reference(persona_id: &str, trigger_event_id: &str, attempt: u64) -> String {
    format!("TWALK-REF:{persona_id}:{trigger_event_id}:{attempt}")
}

/// Now, as the contract spells an instant.
pub fn rfc3339_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_secs();
    rfc3339_of(seconds)
}

/// An instant, as the contract spells one, from unix seconds.
pub fn rfc3339_of(seconds: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp(seconds as i64)
        .expect("a representable instant")
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC 3339 formatting never fails for a valid instant")
        .replace(".000000000", "")
}

/// Posts one push at the Gateway's answer webhook, as Hermes does — with
/// whatever signature the caller chooses, so a test can send the right one,
/// the wrong one, or none.
pub async fn post_hermes_answer(
    base: &str,
    signature: Option<&str>,
    body: &str,
) -> Result<reqwest::Response> {
    let mut request = reqwest::Client::new()
        .post(format!("{base}/_twalk/hermes/answers"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_owned());
    if let Some(signature) = signature {
        request = request.header("X-Hermes-Signature-256", signature);
    }
    request
        .send()
        .await
        .context("the Hermes answer webhook did not answer")
}

/// Pushes one mautrix `BridgeState` at the Gateway's status webhook, as a
/// bridge does — with a bearer token the caller chooses, so a test can send
/// the right one, the wrong one, or none.
pub async fn push_bridge_status(
    base: &str,
    status_bridge_id: &str,
    as_token: Option<&str>,
    state: &serde_json::Value,
) -> Result<reqwest::Response> {
    let mut request = reqwest::Client::new()
        .post(format!("{base}{}", bridge_status_path(status_bridge_id)))
        .json(state);
    if let Some(as_token) = as_token {
        request = request.bearer_auth(as_token);
    }
    request
        .send()
        .await
        .context("the status webhook did not answer")
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

/// An HTTP endpoint that is **not** an OpenAI-compatible one, answering
/// whatever a test tells it to (ticket #98).
///
/// The shared harness's `StubLlm` is the endpoint that works; this is every
/// other kind an operator actually points Twalk at, and it exists because
/// `POST /api/settings/model/probe`'s whole purpose is telling them apart:
/// an endpoint that refuses the request (a wrong credential, a model it does
/// not serve) and an endpoint that answers a perfectly good `200` for
/// something that is not a completion (the wrong port) are two different
/// problems with two different fixes. Canned rather than derived from a real
/// service, so the test asserts the Gateway's classification and not
/// somebody else's error handling.
pub struct StubEndpoint {
    addr: std::net::SocketAddr,
    accept_task: tokio::task::JoinHandle<()>,
}

impl StubEndpoint {
    /// Answers every request with this status and this JSON body.
    pub async fn answering(status: u16, reason: &str, body: serde_json::Value) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .context("failed to bind the stub endpoint")?;
        let addr = listener.local_addr()?;
        let head = format!("{status} {reason}");
        let body = serde_json::to_vec(&body)?;
        let accept_task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let head = head.clone();
                let body = body.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    // Read whatever the caller sends until it stops or the
                    // head is complete; the answer does not depend on it.
                    let mut buffer = [0_u8; 4096];
                    let _ = stream.read(&mut buffer).await;
                    let response = format!(
                        "HTTP/1.1 {head}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                    let _ = stream.flush().await;
                });
            }
        });
        Ok(Self { addr, accept_task })
    }

    /// The OpenAI-compatible base URL an operator would configure.
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }
}

impl Drop for StubEndpoint {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}
