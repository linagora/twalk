//! The collector at its process boundary (issue #274): started with a grant
//! against the fake SSO and the test stack's bus, it says on the bus what
//! state each connection is in, and each transition is the contract's
//! `connection.status.changed.v1`.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::process::Command;
use twalk_collector::oidc::{Client, Settings};
use twalk_test_harness::sso::{CLIENT_ID, CLIENT_SECRET};
use twalk_test_harness::{
    ensure_stack, nats_url, poll_until, validate_against_contract, Bus, FakeSso,
};

const OWNER: &str = "michel@example.com";
const STREAM: &str = "twalk";
const STATUS_SUBJECT: &str = "twalk.connection.status.changed.v1";

/// A collector started for one run of a test: its own state dir, its own
/// connection ids so runs on the shared bus do not read each other's events.
struct Run {
    sso: FakeSso,
    dir: tempfile::TempDir,
    mail: String,
    calendar: String,
}

impl Run {
    async fn prepare(name: &str) -> Result<Self> {
        let sso = FakeSso::start(OWNER).await?;
        let dir = tempfile::tempdir()?;
        std::fs::write(
            dir.path().join("client-secret"),
            format!("{CLIENT_SECRET}\n"),
        )?;
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
            % 1_000_000;
        Ok(Self {
            sso,
            dir,
            mail: format!("mail-{name}-{unique}"),
            calendar: format!("calendar-{name}-{unique}"),
        })
    }

    fn settings(&self) -> Settings {
        Settings {
            issuer: self.sso.issuer(),
            client_id: CLIENT_ID.to_owned(),
            client_secret_file: self.dir.path().join("client-secret"),
            redirect_uri: "http://localhost:1/callback".to_owned(),
            scopes: vec!["openid".to_owned(), "offline_access".to_owned()],
            grant_file: self.dir.path().join("oidc").join("grant.json"),
        }
    }

    /// The operator's consent, done through the library the binary uses.
    async fn consent(&self) -> Result<()> {
        let client = Client::discover(self.settings()).await?;
        let started = client.begin_consent()?;
        let callback = self.sso.sign_in(&started.authorization_url)?;
        client.complete_consent(&started, &callback).await?;
        Ok(())
    }

    fn env(&self) -> Vec<(String, String)> {
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
            (
                "COLLECTOR_LOG_LEVEL",
                "info,twalk_collector=debug".to_owned(),
            ),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
    }

    fn start(&self) -> Result<CollectorProc> {
        CollectorProc::start(&self.env())
    }
}

struct CollectorProc {
    child: tokio::process::Child,
    log_lines: std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl CollectorProc {
    fn start(env: &[(String, String)]) -> Result<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_twalk-collector"))
            .envs(env.iter().cloned())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start the collector binary")?;
        let log_lines = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
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
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let mut lines = tokio::io::BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    eprintln!("{line}");
                    store.lock().await.push(line);
                }
            });
        }
        Ok(Self { child, log_lines })
    }

    async fn logs(&self) -> Vec<String> {
        self.log_lines.lock().await.clone()
    }

    async fn stop(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }

    async fn exit_status(&mut self) -> Result<std::process::ExitStatus> {
        Ok(tokio::time::timeout(Duration::from_secs(20), self.child.wait()).await??)
    }
}

/// The status events about one connection on the bus, in order.
async fn states_of(bus: &Bus, connection: &str) -> Result<Vec<Value>> {
    Ok(bus
        .fetch_all(STREAM, STATUS_SUBJECT)
        .await?
        .into_iter()
        .filter(|event| event["subject"].as_str() == Some(connection))
        .collect())
}

async fn wait_for_state(bus: &Bus, connection: &str, state: &str) -> Result<Value> {
    let connection = connection.to_owned();
    let state = state.to_owned();
    poll_until(
        || async {
            states_of(bus, &connection)
                .await
                .ok()?
                .into_iter()
                .find(|event| event["data"]["to_state"].as_str() == Some(state.as_str()))
        },
        &format!("{connection} to reach {state} on the bus"),
    )
    .await
}

#[tokio::test]
async fn a_connection_says_connected_then_reconnect_required_when_the_grant_is_revoked(
) -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("revoked").await?;
    run.consent().await?;
    let collector = run.start()?;

    let connected = wait_for_state(&bus, &run.mail, "connected").await?;
    validate_against_contract(&connected, "connection.status.changed")?;
    assert_eq!(
        connected["data"]["from_state"], "unknown",
        "the first event of a run"
    );
    assert_eq!(connected["data"]["kind"], "email");
    assert_eq!(connected["connection"], run.mail);
    assert_eq!(
        connected["source"],
        format!("collector://collector.test/connections/{}", run.mail)
    );
    // The calendar connection of the same grant: its own event.
    let calendar = wait_for_state(&bus, &run.calendar, "connected").await?;
    assert_eq!(calendar["data"]["kind"], "calendar");

    // The SSO revokes the grant; the access token is still fresh, so the
    // collector learns it at its next renewal — forced here by a short
    // token life in the fake? No: the fake's tokens live an hour, so the
    // collector is told by the services instead. Both roads lead to the same
    // word once the token is gone; a revocation the services notice first
    // reads as pending_operator until the SSO refuses the renewal. So the
    // test revokes, then restarts the collector, which renews at start.
    run.sso.revoke();
    collector.stop().await;
    let collector = run.start()?;
    let refused = wait_for_state(&bus, &run.mail, "reconnect_required").await?;
    validate_against_contract(&refused, "connection.status.changed")?;
    assert_eq!(
        refused["data"]["from_state"], "unknown",
        "a new run starts from unknown"
    );
    assert_eq!(refused["data"]["service"], "sso");
    let hint = refused["data"]["hint"].as_str().unwrap_or_default();
    assert!(hint.contains("consent --renew"), "{hint}");
    for line in collector.logs().await {
        assert!(
            !line.contains("refresh-") && !line.contains("access-"),
            "a token reached the log: {line}"
        );
    }
    collector.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_service_refusing_a_fresh_token_is_pending_operator_on_its_own_connection_only(
) -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let run = Run::prepare("refusing").await?;
    run.consent().await?;
    run.sso.refuse("caldav");
    let collector = run.start()?;

    let calendar = wait_for_state(&bus, &run.calendar, "pending_operator").await?;
    validate_against_contract(&calendar, "connection.status.changed")?;
    assert_eq!(calendar["data"]["service"], "caldav");
    assert!(
        calendar["data"]["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("caldav"),
        "{calendar}"
    );
    // The mail connection is fine: the grant stands and JMAP takes it.
    let mail = wait_for_state(&bus, &run.mail, "connected").await?;
    assert!(mail["data"].get("hint").is_none());
    assert!(
        states_of(&bus, &run.mail)
            .await?
            .iter()
            .all(|event| event["data"]["to_state"] != "pending_operator"),
        "the calendar's refusal is not the mailbox's state"
    );

    // Restored: the transition back is published, once.
    run.sso.restore("caldav");
    wait_for_state(&bus, &run.calendar, "connected").await?;
    // And a service that does not answer is unreachable, which is neither.
    run.sso.silence("jmap");
    let silent = wait_for_state(&bus, &run.mail, "unreachable").await?;
    assert_eq!(silent["data"]["service"], "jmap");
    collector.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_grant_for_another_account_publishes_nothing_and_names_the_account() -> Result<()> {
    ensure_stack().await?;
    let bus = Bus::connect().await?;
    let mut run = Run::prepare("stranger").await?;
    // The grant is obtained at an SSO whose account is not the owner's.
    run.sso = FakeSso::start("somebody@example.com").await?;
    run.consent().await?;
    let collector = run.start()?;

    let pending = wait_for_state(&bus, &run.mail, "pending_operator").await?;
    assert!(
        pending["data"]["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("another account"),
        "{pending}"
    );
    assert!(
        collector
            .logs()
            .await
            .iter()
            .any(|line| line.contains("somebody@example.com")
                && line.contains("nothing is published")),
        "the account is named in the log"
    );
    collector.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_connection_the_registry_does_not_know_is_refused_at_start() -> Result<()> {
    ensure_stack().await?;
    let run = Run::prepare("unregistered").await?;
    run.consent().await?;
    // A Gateway that answers a registry without this connection: the fake
    // SSO stands in for the Gateway's snapshot route with an empty registry
    // — it answers 404 there, which the collector reads as a refusal to
    // read the registry at all; a wrong registry is asserted in the unit
    // test of `Config::refuse_unknown_connections`.
    let mut env = run.env();
    env.push(("COLLECTOR_GATEWAY_URL".to_owned(), run.sso.issuer()));
    env.push((
        "COLLECTOR_GATEWAY_SERVICE_TOKEN".to_owned(),
        "token".to_owned(),
    ));
    let mut collector = CollectorProc::start(&env)?;
    let status = collector.exit_status().await?;
    assert!(
        !status.success(),
        "a registry that cannot be read is not a start"
    );
    assert!(
        collector
            .logs()
            .await
            .iter()
            .any(|line| line.contains("registry")),
        "the refusal names the registry"
    );
    Ok(())
}
