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
//! harness/` does. The skeleton uses only `poll_until`: it talks to no
//! homeserver and no bus yet.
//!
//! What is Gateway-specific stays here: `GatewayProc`, the static directory
//! fixtures and the environment the Gateway runs from in tests.

// Every test binary compiles this module but uses only a subset of it.
#![allow(dead_code)]

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
        Ok(status)
    }

    /// Waits for the process to exit on its own — a misconfigured Gateway
    /// must fail loudly instead of serving nothing. Borrows, so the caller
    /// can read the logs it exited with.
    pub async fn wait_for_exit(&mut self) -> Result<std::process::ExitStatus> {
        let status = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .context("the gateway did not exit within 10s")??;
        Ok(status)
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

/// The environment the Gateway runs from in tests: port 0 (the kernel picks),
/// the given static directory, debug logs for the Gateway's own target.
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
    ]
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
