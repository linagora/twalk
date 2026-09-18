//! Integration-test harness for Hermes (ticket #21).
//!
//! What every component's suite needs — the test stack's lifecycle, the
//! `Bus`, contract validation, `poll_until`, the stub LLM — lives in the
//! shared harness crate (`tests/harness/`, ticket #20) and is re-exported
//! here, so the Hermes test files see one flat `harness::` namespace, the
//! way the Sensor's do.
//!
//! What is Hermes-specific is the persona: a persona ships as a **container
//! image** (ADR 0008, and the host has no pip), so the seam this suite
//! drives is that container's process boundary. [`PersonaRun`] is the whole
//! of it — one call brings up the real `assistant` image against the shared
//! stack's bus, with a stub LLM behind it, on a bus namespace of this run's
//! own.
//!
//! Nothing here reaches inside the persona process: a test publishes
//! fixtures on the bus, reads what comes back, and asks the stub LLM what
//! it was sent.
//!
//! Isolation, because the test stack persists across runs and is shared
//! with the Sensor suite: each run gets its own JetStream stream and
//! subject prefix (`h21-<test>-<pid>-<nanos>`), its own durable consumer
//! and its own compose project, and gives all of them back in
//! [`PersonaRun::shutdown`] — or, when a test panics before that, in the
//! `Drop` that removes the container. A container left behind because a run
//! was killed outside Rust's control goes with
//! `docker compose -p <project> -f hermes/tests/compose.assistant.yaml down -v`.

// Every test binary compiles this module but uses only a subset of it.
#![allow(dead_code, unused_imports)]

/// The runtime's own half of the harness (ticket #23): [`RuntimeRun`], which
/// drives the real `twalk-hermes` binary and lets *it* start the persona.
mod runtime;

/// The deployment's own half of the harness (ticket #25): [`Deployment`],
/// which brings up the reference deployment — a real Sensor, a real
/// Companion Gateway, a real homeserver — and starts the runtime beside it,
/// because only that stack can answer whether a reply reached the contact.
mod deployment;

pub use deployment::*;
pub use runtime::*;
pub use twalk_test_harness::*;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::process::Command;

/// The compose service the persona runs as, in
/// `hermes/tests/compose.assistant.yaml`.
const SERVICE: &str = "assistant";

/// The persona id the reference persona ships with — and the first half of
/// every deterministic id it produces.
pub const PERSONA_ID: &str = "assistant";

/// The Hermes domain the persona is configured with in tests, hence the
/// `source` of every event it publishes.
pub const HERMES_DOMAIN: &str = "test.twalk";

/// The model name the persona reports in its `thinking` events (the stub
/// LLM answers whatever model it is asked for).
pub const MODEL: &str = "stub-model";

/// The LLM credentials the persona is configured with: the stub records the
/// `Authorization` header, so a test can prove the operator's credentials
/// reached the endpoint they configured.
pub const LLM_API_KEY: &str = "test-only-llm-key";

/// The provider parameters the persona is configured with, as an operator
/// sets them (ADR 0015): one field the endpoint wants, and one the client
/// would otherwise send that this provider rejects. A test asserts both
/// halves in the request the stub actually received.
pub const LLM_PARAMS: &str = r#"{"top_p": 0.9, "temperature": null}"#;

/// The suggestion policy's default window, in seconds: how long a
/// suggestion stays approvable when the operator has named no window of
/// their own (`sdk/python/twalk_sdk/policy.py`). Restated here rather than
/// read from the SDK, because a test that took the persona's own constant
/// would agree with it whatever it became.
pub const DEFAULT_SUGGESTION_TTL_SECONDS: i64 = 3600;

pub const INBOUND_TYPE: &str = "fr.linagora.twalk.inbound.message.received.v1";
pub const OUTBOUND_TYPE: &str = "fr.linagora.twalk.outbound.message.sent.v1";
pub const THINKING_TYPE: &str = "fr.linagora.twalk.persona.thinking.emitted.v1";
pub const SUGGEST_TYPE: &str = "fr.linagora.twalk.persona.suggest.produced.v1";

fn compose_file() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("compose.assistant.yaml")
        .to_string_lossy()
        .into_owned()
}

/// An identifier unique to this run of this test: the stack persists across
/// runs and is shared with the Sensor suite, so every name a test claims on
/// it carries one.
fn run_id(test_name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    format!(
        "h21-{test_name}-{}-{}",
        std::process::id(),
        nanos % 1_000_000
    )
}

/// One compose project, with the variables its file requires.
///
/// The compose file declares every value that identifies a run as required
/// (`${VAR:?}`), so a missing one fails loudly instead of leaking events
/// between runs — which means even a command that does not care about them
/// (`build`, `down`) has to pass them.
struct Compose {
    project: String,
    env: Vec<(&'static str, String)>,
}

impl Compose {
    fn new(project: String, env: Vec<(&'static str, String)>) -> Self {
        Self { project, env }
    }

    /// The same project with placeholder variables: enough to parse the
    /// compose file for a command that touches no run of its own.
    fn placeholders(project: String) -> Self {
        Self::new(
            project,
            vec![
                ("TWALK_TEST_PERSONA_STREAM", "unused".to_owned()),
                ("TWALK_TEST_PERSONA_PREFIX", "unused".to_owned()),
                ("TWALK_TEST_PERSONA_CONSUMER", "unused".to_owned()),
                ("TWALK_TEST_PERSONA_NATS_URL", nats_url()),
                ("TWALK_TEST_PERSONA_LLM_URL", "http://unused/v1".to_owned()),
            ],
        )
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("docker");
        command.args(["compose", "-p", &self.project, "-f", &compose_file()]);
        command.args(args);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command
    }

    async fn run(&self, args: &[&str], what: &str) -> Result<()> {
        let status = self
            .command(args)
            .stdout(Stdio::null())
            .status()
            .await
            .with_context(|| format!("failed to run {what}"))?;
        if !status.success() {
            bail!("{what} failed with {status}");
        }
        Ok(())
    }
}

/// Builds the persona image once per test binary.
///
/// Each test runs its own compose project, so each has its own image tag;
/// what this shares is the layer cache, which is where the time goes (the
/// image installs the SDK's dependencies). Without it, the tests of one
/// binary race to `pip install` the same two packages in parallel.
///
/// This one tag (`h21-persona-image-assistant`) is deliberately left
/// behind, unlike the per-run ones: it is what holds the built layers
/// between runs, so a later run rebuilds nothing unchanged.
async fn ensure_image() -> Result<()> {
    static IMAGE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    IMAGE
        .get_or_try_init(|| async {
            Compose::placeholders("h21-persona-image".to_owned())
                .run(&["build", SERVICE], "docker compose build for the persona")
                .await
        })
        .await?;
    Ok(())
}

/// The real `assistant` persona, running as the container image it ships
/// as, against the shared stack's bus and a stub LLM.
pub struct PersonaRun {
    /// The bus, connected from the test side.
    pub bus: Bus,
    /// The LLM the persona reasons with: a test scripts its answers and
    /// asks what it was sent — or, for the consent gate, asserts it was
    /// sent nothing.
    pub llm: StubLlm,
    /// The JetStream stream this run owns.
    pub stream: String,
    /// The bus subject prefix this run owns (`twalk` in a deployment).
    pub prefix: String,
    compose: Compose,
    stopped: bool,
}

impl PersonaRun {
    /// Brings up the persona: an isolated stream and subject prefix, a stub
    /// LLM answering `canned_reply`, and the container, waited for until it
    /// reports itself ready.
    ///
    /// The suggestion window is left unset, so the persona runs on the
    /// policy's own default — which is what a deployment that configured
    /// nothing gets.
    pub async fn start(test_name: &str, canned_reply: &str) -> Result<Self> {
        Self::start_with(test_name, canned_reply, None).await
    }

    /// [`start`](Self::start) with the operator's suggestion window named
    /// explicitly, in seconds: the expiry is configuration, so a test has
    /// to be able to set it rather than only observe the default.
    pub async fn start_with_suggestion_ttl(
        test_name: &str,
        canned_reply: &str,
        ttl_seconds: i64,
    ) -> Result<Self> {
        Self::start_with(test_name, canned_reply, Some(ttl_seconds)).await
    }

    async fn start_with(
        test_name: &str,
        canned_reply: &str,
        ttl_seconds: Option<i64>,
    ) -> Result<Self> {
        ensure_stack().await?;
        ensure_image().await?;

        let id = run_id(test_name);
        let bus = Bus::connect().await?;
        bus.ensure_stream(&id, &[&format!("{id}.>")]).await?;

        // On loopback, where the persona's container reaches it: the
        // container runs on the host's network namespace, so the two
        // endpoints it is pointed at — the bus on the port the stack
        // publishes, and this stub — are the same addresses the test uses.
        let llm = StubLlm::start_with_reply(canned_reply).await?;

        let compose = Compose::new(
            id.clone(),
            vec![
                ("TWALK_TEST_PERSONA_STREAM", id.clone()),
                ("TWALK_TEST_PERSONA_PREFIX", id.clone()),
                ("TWALK_TEST_PERSONA_CONSUMER", format!("persona-{id}")),
                ("TWALK_TEST_PERSONA_NATS_URL", nats_url()),
                ("TWALK_TEST_PERSONA_LLM_URL", llm.base_url()),
                ("TWALK_TEST_PERSONA_DOMAIN", HERMES_DOMAIN.to_owned()),
                ("TWALK_TEST_PERSONA_MODEL", MODEL.to_owned()),
                ("TWALK_TEST_PERSONA_LLM_KEY", LLM_API_KEY.to_owned()),
                ("TWALK_TEST_PERSONA_LLM_PARAMS", LLM_PARAMS.to_owned()),
                (
                    "TWALK_TEST_PERSONA_SUGGESTION_TTL",
                    // Empty is how compose passes on "the operator named no
                    // window", which is the case the default has to cover.
                    ttl_seconds.map(|ttl| ttl.to_string()).unwrap_or_default(),
                ),
            ],
        );
        let run = PersonaRun {
            bus,
            llm,
            stream: id.clone(),
            prefix: id,
            compose,
            stopped: false,
        };
        run.compose
            .run(
                &["up", "-d", "--force-recreate", "--remove-orphans", SERVICE],
                "docker compose up for the persona",
            )
            .await?;
        run.wait_until_ready().await?;
        Ok(run)
    }

    /// The persona's own logs, as an operator would read them.
    pub async fn logs(&self) -> Result<String> {
        let output = self
            .compose
            .command(&["logs", "--no-color", SERVICE])
            .output()
            .await
            .context("failed to read the persona's logs")?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Waits until the persona has its durable consumer and says so. A
    /// persona that died on startup fails here, with its logs.
    async fn wait_until_ready(&self) -> Result<()> {
        let ready = poll_until(
            || async {
                self.logs()
                    .await
                    .ok()?
                    .contains("persona ready")
                    .then_some(())
            },
            "the persona to report itself ready",
        )
        .await;
        if ready.is_err() {
            bail!(
                "the persona never became ready; its logs were:\n{}",
                self.logs().await.unwrap_or_default()
            );
        }
        Ok(())
    }

    /// The bus subject a contract event type travels on, in this run's
    /// namespace.
    pub fn subject(&self, event_type: &str) -> String {
        let suffix = event_type
            .strip_prefix("fr.linagora.twalk.")
            .expect("contract event types carry the fr.linagora.twalk prefix");
        format!("{}.{suffix}", self.prefix)
    }

    /// Publishes an inbound event the way the Sensor does: `Nats-Msg-Id`
    /// set to the event id, on this run's own subject.
    pub async fn publish_inbound(&self, event: &Value) -> Result<()> {
        self.bus
            .publish_event(&self.subject(INBOUND_TYPE), event)
            .await
    }

    /// Publishes an inbound event **again**, without the `Nats-Msg-Id` the
    /// Sensor sets on a first publish.
    ///
    /// That header is what makes the bus collapse a producer's replay, so
    /// leaving it off is the only way a test can put the *consumer's* own
    /// at-least-once path under the persona's process boundary: the stream
    /// stores the same CloudEvent twice and the persona is delivered it
    /// twice, exactly as it would be after a crash between processing and
    /// acking.
    pub async fn redeliver_inbound(&self, event: &Value) -> Result<()> {
        self.bus.publish(&self.subject(INBOUND_TYPE), event).await
    }

    /// Publishes an event onto the subject its **own type** maps to, the way
    /// a producer other than the Sensor's inbound path does. The persona's
    /// consumer is filtered to the inbound subject, so nothing published
    /// here reaches it — which is itself a property worth asserting, beside
    /// the SDK gate that would refuse it if it did.
    pub async fn publish_typed(&self, event: &Value) -> Result<()> {
        let event_type = event["type"]
            .as_str()
            .expect("an event to publish has a string type");
        self.bus
            .publish_event(&self.subject(event_type), event)
            .await
    }

    /// Every event of a type the persona published in this run, in stream
    /// order.
    pub async fn published(&self, event_type: &str) -> Result<Vec<StoredMessage>> {
        self.bus
            .fetch_all_with_headers(&self.stream, &self.subject(event_type))
            .await
    }

    /// Polls until the persona publishes an event of `event_type` about
    /// `trigger_event_id`.
    ///
    /// An event that never comes is the failure a persona test has to
    /// diagnose most often, and the answer is always in the persona's own
    /// logs — so they come with the failure rather than being looked up
    /// after the container is gone.
    pub async fn wait_for(
        &self,
        event_type: &str,
        trigger_event_id: &str,
    ) -> Result<StoredMessage> {
        match self.poll_for(event_type, trigger_event_id).await {
            Ok(message) => Ok(message),
            Err(error) => bail!(
                "{error}; the persona's logs were:\n{}",
                self.logs().await.unwrap_or_default()
            ),
        }
    }

    async fn poll_for(&self, event_type: &str, trigger_event_id: &str) -> Result<StoredMessage> {
        poll_until(
            || async {
                self.published(event_type)
                    .await
                    .ok()?
                    .into_iter()
                    .find(|message| message.payload["subject"].as_str() == Some(trigger_event_id))
            },
            &format!("{event_type} about {trigger_event_id}"),
        )
        .await
    }

    /// The chat-completions requests the persona sent whose prompt mentions
    /// `marker`. Tests mark each message they publish, so an assertion
    /// about what reached the model — or about what did not — is about
    /// exactly the message under test.
    pub fn llm_requests_mentioning(&self, marker: &str) -> Vec<StubRequest> {
        self.llm
            .requests()
            .into_iter()
            .filter(|request| request.body.to_string().contains(marker))
            .collect()
    }

    /// Stops the persona and gives back everything this run claimed on the
    /// shared stack.
    pub async fn shutdown(mut self) -> Result<()> {
        self.stopped = true;
        // `--rmi local`: the image this project built carries no tag of its
        // own, so removing it leaves the layer cache (which is where the
        // build time is) and takes away the per-run tag that would
        // otherwise accumulate one per test.
        self.compose
            .run(
                &["down", "-v", "--rmi", "local", "--remove-orphans"],
                "docker compose down for the persona",
            )
            .await?;
        self.bus.delete_stream(&self.stream).await?;
        Ok(())
    }
}

impl Drop for PersonaRun {
    /// A test that panicked never reached `shutdown`; the container must
    /// still go. Blocking, deliberately: a teardown that runs is worth a
    /// second of a test thread.
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        let mut command = std::process::Command::new("docker");
        command
            .args([
                "compose",
                "-p",
                &self.compose.project,
                "-f",
                &compose_file(),
                "down",
                "-v",
                "--rmi",
                "local",
                "--remove-orphans",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (key, value) in &self.compose.env {
            command.env(key, value);
        }
        let _ = command.status();
    }
}

/// A valid W3C traceparent derived from an event id, the way the Sensor
/// originates one (`sensor/src/normalize.rs`): the trace a persona's events
/// must continue.
pub fn traceparent_for(event_id: &str) -> String {
    format!("00-{}-{}-01", &event_id[0..32], &event_id[32..48])
}

/// The trace id half of a traceparent: what has to be identical across a
/// message's sensor, persona and approval events for the trace to link
/// them.
pub fn trace_id(traceparent: &str) -> &str {
    traceparent
        .split('-')
        .nth(1)
        .expect("a traceparent has four dash-separated parts")
}
