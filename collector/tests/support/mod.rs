//! What the collector's process-boundary suites share: a run — its own
//! state directory, its own fake SSO, its own connection ids so runs on the
//! shared bus never read each other's events — and the binary as a child
//! whose every printed line is kept, since "never a token, never a
//! description" is asserted on the log as much as on the bus.

#![allow(dead_code)]

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::process::Command;
use twalk_collector::oidc::{Client, Settings};
use twalk_test_harness::sso::{write_client_secret, CLIENT_ID};
use twalk_test_harness::{nats_url, poll_until, Bus, FakeSso};

pub const OWNER: &str = "michel@example.com";
pub const STREAM: &str = "twalk";
pub const CONSENT_SUBJECT: &str = "twalk.consent.state.changed.v1";

pub struct Run {
    pub sso: FakeSso,
    pub dir: tempfile::TempDir,
    pub mail: String,
    pub calendar: String,
    /// The bus's head when the run was prepared: what it publishes is after
    /// it, and the shared bus's history is not walked at every read.
    pub since: u64,
}

impl Run {
    pub async fn prepare(name: &str) -> Result<Self> {
        Self::prepare_as(name, OWNER).await
    }

    /// A run whose SSO holds a grant for `account` — the owner, or a
    /// stranger.
    pub async fn prepare_as(name: &str, account: &str) -> Result<Self> {
        let since = Bus::connect().await?.head(STREAM).await?;
        let sso = FakeSso::start(account).await?;
        let dir = tempfile::tempdir()?;
        write_client_secret(dir.path())?;
        // Nanoseconds since the epoch: runs on the shared bus must not read
        // each other's events, and the id stays within the contract's 64.
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        Ok(Self {
            sso,
            dir,
            mail: format!("mail-{name}-{unique}"),
            calendar: format!("cal-{name}-{unique}"),
            since,
        })
    }

    pub fn settings(&self) -> Settings {
        Settings {
            issuer: self.sso.issuer(),
            client_id: CLIENT_ID.to_owned(),
            client_secret_file: self.dir.path().join("client-secret"),
            redirect_uri: "http://localhost:1/callback".to_owned(),
            scopes: vec!["openid".to_owned(), "offline_access".to_owned()],
            grant_file: self.dir.path().join("oidc").join("grant.json"),
        }
    }

    /// The operator's authorization, done through the library the binary uses.
    pub async fn authorize(&self) -> Result<()> {
        let client = Client::discover(self.settings()).await?;
        let started = client.begin_authorization()?;
        let callback = self.sso.sign_in(&started.authorization_url)?;
        client.complete_authorization(&started, &callback).await?;
        Ok(())
    }

    /// The environment the binary runs with: both connections held, the
    /// health, calendar and mail polls every second.
    pub fn env(&self) -> Vec<(String, String)> {
        [
            (
                "COLLECTOR_STATE_DIR",
                self.dir.path().to_string_lossy().into_owned(),
            ),
            ("COLLECTOR_OIDC_ISSUER", self.sso.issuer()),
            ("COLLECTOR_OIDC_CLIENT_ID", CLIENT_ID.to_owned()),
            (
                "COLLECTOR_OIDC_CLIENT_SECRET_FILE",
                self.dir
                    .path()
                    .join("client-secret")
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "COLLECTOR_OIDC_REDIRECT_URI",
                "http://localhost:1/callback".to_owned(),
            ),
            ("COLLECTOR_JMAP_SESSION_URL", self.sso.jmap_session_url()),
            ("COLLECTOR_CALDAV_URL", self.sso.caldav_url()),
            ("COLLECTOR_OWNER_EMAIL", OWNER.to_owned()),
            ("COLLECTOR_MAIL_CONNECTION", self.mail.clone()),
            ("COLLECTOR_CALENDAR_CONNECTION", self.calendar.clone()),
            ("COLLECTOR_NATS_URL", nats_url()),
            ("COLLECTOR_HOST", "collector.test".to_owned()),
            ("COLLECTOR_HEALTH_INTERVAL_SECONDS", "1".to_owned()),
            ("COLLECTOR_CALENDAR_POLL_SECONDS", "1".to_owned()),
            ("COLLECTOR_MAIL_POLL_SECONDS", "1".to_owned()),
            (
                "COLLECTOR_LOG_LEVEL",
                "info,twalk_collector=debug".to_owned(),
            ),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
    }

    /// The environment with the fake standing in for the Companion Gateway
    /// too (`serve_gateway_snapshot`).
    pub fn env_with_gateway(&self) -> Vec<(String, String)> {
        let mut env = self.env();
        env.push(("COLLECTOR_GATEWAY_URL".to_owned(), self.sso.issuer()));
        env.push((
            "COLLECTOR_GATEWAY_SERVICE_TOKEN".to_owned(),
            "test-service-token".to_owned(),
        ));
        env
    }

    pub fn start(&self) -> Result<CollectorProc> {
        CollectorProc::start(&self.env())
    }

    /// The Companion Gateway's snapshot, as the fake stands in for it: both
    /// connections in the registry, the decisions given, the stream to be
    /// followed from just past its current head.
    pub async fn serve_snapshot(&self, bus: &Bus, entries: Vec<Value>) -> Result<()> {
        let head = bus.last_sequence(STREAM, CONSENT_SUBJECT).await?;
        self.sso.serve_gateway_snapshot(json!({
            "stream": STREAM,
            "subject": CONSENT_SUBJECT,
            "stream_sequence": head,
            "next_stream_sequence": head + 1,
            "decision_sequence": entries.len(),
            "connections": [
                { "id": self.mail, "kind": "email", "network": "email" },
                { "id": self.calendar, "kind": "calendar" },
            ],
            "entries": entries,
        }));
        Ok(())
    }

    /// A snapshot entry: the owner's decision about a `mailto:` on the
    /// mail connection.
    pub fn decided_on_mail(&self, identity: &str, state: &str) -> Value {
        json!({
            "subject": { "type": "contact", "id": identity },
            "connection": self.mail,
            "network": "email",
            "state": state,
            "decided_at": "2026-09-20T10:00:00.000Z",
            "decision_sequence": 1
        })
    }

    /// The events this run published on one subject, since its start, about
    /// one of its connections.
    pub async fn events_of(
        &self,
        bus: &Bus,
        subject: &str,
        connection: &str,
    ) -> Result<Vec<Value>> {
        Ok(bus
            .fetch_since(STREAM, subject, self.since)
            .await?
            .into_iter()
            .filter(|event| event["connection"].as_str() == Some(connection))
            .collect())
    }

    /// Waits until at least `at_least` such events are on the bus.
    pub async fn wait_for_events(
        &self,
        bus: &Bus,
        subject: &str,
        connection: &str,
        at_least: usize,
    ) -> Result<Vec<Value>> {
        poll_until(
            || async {
                let events = self.events_of(bus, subject, connection).await.ok()?;
                (events.len() >= at_least).then_some(events)
            },
            &format!("{at_least} events on {subject} about {connection}"),
        )
        .await
    }

    pub fn start_with_gateway(&self) -> Result<CollectorProc> {
        CollectorProc::start(&self.env_with_gateway())
    }

    /// Every byte the collector wrote under its state directory, the grant
    /// and the cursors alike: what "never on disk" is asserted on.
    pub fn stored_bytes(&self) -> Result<String> {
        fn walk(dir: &std::path::Path, out: &mut String) -> Result<()> {
            for entry in std::fs::read_dir(dir)? {
                let path = entry?.path();
                if path.is_dir() {
                    walk(&path, out)?;
                } else {
                    out.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
                    out.push('\n');
                }
            }
            Ok(())
        }
        let mut out = String::new();
        walk(self.dir.path(), &mut out)?;
        Ok(out)
    }
}

pub struct CollectorProc {
    child: tokio::process::Child,
    log_lines: std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
    /// The two readers, joined when the process ends so that its last
    /// line — the refusal it exited on — is in `logs()` before a test reads
    /// them.
    readers: Vec<tokio::task::JoinHandle<()>>,
}

impl CollectorProc {
    pub fn start(env: &[(String, String)]) -> Result<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_twalk-collector"))
            .envs(env.iter().cloned())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start the collector binary")?;
        let log_lines = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let mut readers = Vec::new();
        for (stream, store) in [
            (
                Box::new(child.stderr.take().expect("stderr is piped"))
                    as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                log_lines.clone(),
            ),
            (
                Box::new(child.stdout.take().expect("stdout is piped")),
                log_lines.clone(),
            ),
        ] {
            readers.push(tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let mut lines = tokio::io::BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    eprintln!("{line}");
                    store.lock().await.push(line);
                }
            }));
        }
        Ok(Self {
            child,
            log_lines,
            readers,
        })
    }

    pub async fn logs(&self) -> Vec<String> {
        self.log_lines.lock().await.clone()
    }

    /// How many log lines contain `needle`.
    pub async fn count_logged(&self, needle: &str) -> usize {
        self.logs()
            .await
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    }

    /// Waits until `needle` has been logged at least `times` times, for
    /// the harness's twenty seconds.
    pub async fn wait_logged(&self, needle: &str, times: usize) -> Result<()> {
        poll_until(
            || async { (self.count_logged(needle).await >= times).then_some(()) },
            &format!("{times}× {needle:?} in the collector's log"),
        )
        .await
    }

    /// The same, for as long as `within`: what a poll interval waited out,
    /// or a reconnect backed off to a minute, needs. The log so far is in
    /// the error, since the wait is the diagnosis.
    pub async fn wait_logged_for(
        &self,
        needle: &str,
        times: usize,
        within: std::time::Duration,
    ) -> Result<()> {
        let started = std::time::Instant::now();
        while self.count_logged(needle).await < times {
            anyhow::ensure!(
                started.elapsed() < within,
                "timed out waiting {times}× {needle:?} within {within:?}\n{}",
                self.logs().await.join("\n")
            );
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        Ok(())
    }

    /// The position in the log of the `nth` (from 1) line containing
    /// `needle`, to tell what the collector did first.
    pub async fn position_logged(&self, needle: &str, nth: usize) -> Option<usize> {
        self.logs()
            .await
            .iter()
            .enumerate()
            .filter(|(_, line)| line.contains(needle))
            .nth(nth.saturating_sub(1))
            .map(|(index, _)| index)
    }

    /// Asserts that no line the collector printed contains any of `words`.
    pub async fn assert_never_logged(&self, words: &[&str]) {
        for line in self.logs().await {
            for word in words {
                assert!(!line.contains(word), "{word:?} reached the log: {line}");
            }
        }
    }

    pub async fn stop(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    pub async fn exit_status(&mut self) -> Result<std::process::ExitStatus> {
        let status = tokio::time::timeout(Duration::from_secs(20), self.child.wait()).await??;
        for reader in self.readers.drain(..) {
            let _ = tokio::time::timeout(Duration::from_secs(5), reader).await;
        }
        Ok(status)
    }
}

pub fn sha256_hex(input: &str) -> String {
    twalk_test_harness::sha256_hex(input)
}
