//! The Hermes runtime at its process boundary (ticket #23).
//!
//! [`PersonaRun`](super::PersonaRun) drives a persona directly, with no
//! runtime in the picture. This one drives the **runtime** — the real
//! `twalk-hermes` binary — and lets it start the persona, so that what is
//! under test is the lifecycle: what the runtime spawns, what it hands the
//! process it spawned, what it does when that process dies, and what it
//! does on SIGTERM.
//!
//! The persona is still the container image it ships as. The runtime spawns
//! a process, so the argv it is configured with here is
//! `hermes/tests/run-persona-image.sh`, which runs the image with whatever
//! environment the runtime gave it. Nothing in this harness reaches inside
//! either process: a test publishes on the bus, reads the runtime's logs the
//! way an operator would, asks Docker what is running, and asks the stub LLM
//! what it was sent.
//!
//! Isolation, because the test stack persists across runs and is shared with
//! the Sensor suite: each run gets a JetStream stream and a bus subject
//! prefix of its own (`h23-<test>-<pid>-<nanos>`), its own persona container
//! names and its own stub LLM port, and gives all of them back in
//! [`RuntimeRun::shutdown`] — or, when a test panics before that, in the
//! `Drop` that kills the runtime and removes the containers. Containers left
//! behind because a run was killed outside Rust's control go with
//! `docker rm -f $(docker ps -aq --filter name=h23-)`.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::Mutex;
use twalk_test_harness::{
    ensure_stack, nats_url, poll_until, sha256_hex, validate_against_contract, Bus, StoredMessage,
    StubLlm, StubRequest,
};

use super::{HERMES_DOMAIN, INBOUND_TYPE, LLM_API_KEY, LLM_PARAMS, MODEL, USER_LANGUAGE};

pub const CONSENT_CHANGED_TYPE: &str = "fr.linagora.twalk.consent.state.changed.v1";

/// The tag the persona image is built under for this suite. Built straight
/// from the persona's Dockerfile rather than through the persona suite's
/// compose project, so the two suites share the layer cache (where the time
/// goes) and nothing else.
pub const PERSONA_IMAGE: &str = "h23-persona-assistant:latest";

/// An image that does not exist and never will: the persona a runtime
/// cannot start.
pub const UNSTARTABLE_IMAGE: &str = "h23-no-such-persona-image:never";

/// The Companion Gateway's service token, as the runtime's own environment
/// holds it.
///
/// It is the credential that reads the LLM configuration the user set from the
/// Companion — and the credential that opens the consent snapshot, the list of
/// every contact (ADR 0010, ADR 0015). A persona must never hold it, and a
/// test says so by looking for this exact string in the persona container's
/// environment.
///
/// Since ticket #184 the runtime **uses** it: `HERMES_GATEWAY_SERVICE_TOKEN`
/// is the variable the settings read authenticates with, and the stub Gateway
/// refuses a read without it. Until then this variable was in the runtime's
/// environment as a decoy — a secret the runtime holds and a persona must not,
/// so that the absence assertion had something to be about. It is both now,
/// which is the stronger shape: the token is load-bearing *and* it does not
/// cross.
pub const GATEWAY_SERVICE_TOKEN_VAR: &str = "HERMES_GATEWAY_SERVICE_TOKEN";
pub const GATEWAY_SERVICE_TOKEN: &str = "h23-gateway-service-token-no-persona-may-hold";

/// Patches applied to the **runtime's own** environment, standing in for what
/// an operator did or did not write into `.env`.
///
/// `None` removes the variable entirely, which is the state the reference
/// deployment ships (`deploy/docker-compose/.env.example` leaves every one of
/// them empty) and the one ticket #184 is about: nothing on the host, the
/// Companion the only voice. A patch list rather than a growing set of
/// booleans, because what the tests need to say is exactly "this variable is
/// not set".
pub type HostEnvironment = Vec<(&'static str, Option<String>)>;

/// The runtime's operator-side model configuration, removed: `.env` empty.
pub fn no_operator_model() -> HostEnvironment {
    vec![
        ("HERMES_LLM_BASE_URL", None),
        ("HERMES_LLM_MODEL", None),
        ("HERMES_LLM_API_KEY", None),
        ("HERMES_LLM_PARAMS", None),
        ("HERMES_USER_LANGUAGE", None),
    ]
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("the hermes package sits inside the repository")
}

pub(crate) fn persona_wrapper() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("run-persona-image.sh")
        .to_string_lossy()
        .into_owned()
}

/// An identifier unique to this run of this test.
fn run_id(test_name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    format!(
        "h23-{test_name}-{}-{}",
        std::process::id(),
        nanos % 1_000_000
    )
}

/// Builds the persona image once per test binary.
pub(crate) async fn ensure_persona_image() -> Result<()> {
    static IMAGE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    IMAGE
        .get_or_try_init(|| async {
            let status = Command::new("docker")
                .current_dir(repository_root())
                .args([
                    "build",
                    "-f",
                    "hermes/personas/assistant/Dockerfile",
                    "-t",
                    PERSONA_IMAGE,
                    ".",
                ])
                .stdout(Stdio::null())
                .status()
                .await
                .context("failed to build the persona image")?;
            if !status.success() {
                bail!("docker build for the persona image failed with {status}");
            }
            Ok(())
        })
        .await?;
    Ok(())
}

/// One persona in the runtime's configuration, and the container it runs as.
#[derive(Clone, Debug)]
pub struct PersonaFixture {
    pub id: String,
    pub container: String,
    pub image: String,
}

impl PersonaFixture {
    /// A persona that runs the real `assistant` image.
    pub fn assistant(run: &str) -> Self {
        Self::with_image(run, "assistant", PERSONA_IMAGE)
    }

    /// A persona whose image does not exist: the one a runtime cannot start.
    pub fn unstartable(run: &str, id: &str) -> Self {
        Self::with_image(run, id, UNSTARTABLE_IMAGE)
    }

    fn with_image(run: &str, id: &str, image: &str) -> Self {
        Self {
            id: id.to_owned(),
            container: format!("{run}-{id}"),
            image: image.to_owned(),
        }
    }

    fn command(&self) -> Vec<String> {
        vec![
            persona_wrapper(),
            self.container.clone(),
            self.image.clone(),
        ]
    }
}

/// The real `twalk-hermes` binary, running personas against the shared
/// stack's bus with a stub LLM behind them.
pub struct RuntimeRun {
    /// The bus, connected from the test side.
    pub bus: Bus,
    /// The LLM the personas reason with: a test scripts its answers and asks
    /// what it was sent — or asserts it was sent nothing.
    pub llm: StubLlm,
    /// The JetStream stream this run owns.
    pub stream: String,
    /// The bus subject prefix this run owns (`twalk` in a deployment).
    pub prefix: String,
    pub personas: Vec<PersonaFixture>,
    child: Option<tokio::process::Child>,
    log_lines: Arc<Mutex<Vec<String>>>,
    decisions: std::sync::atomic::AtomicU32,
    host: HostEnvironment,
    stopped: bool,
}

impl RuntimeRun {
    /// Starts the runtime with the given personas, none of them activated:
    /// the state a deployment is in before the user has decided anything
    /// (ADR 0013 — activation never spreads on its own).
    pub async fn start(test_name: &str, personas: Vec<PersonaFixture>) -> Result<Self> {
        Self::start_with(test_name, personas, &[], None).await
    }

    /// Starts the runtime against an **LLM the test already owns**, with an
    /// explicit set of patches to the runtime's own environment.
    ///
    /// Two-phase on purpose (ticket #184): a test that configures a Companion
    /// Gateway has to know the endpoint's URL *before* the runtime reads the
    /// settings that name it, so the stub LLM is started by the test and handed
    /// over rather than created here.
    pub async fn start_configured(
        test_name: &str,
        personas: Vec<PersonaFixture>,
        activated: &[&str],
        llm: StubLlm,
        host: HostEnvironment,
    ) -> Result<Self> {
        Self::bring_up(test_name, personas, activated, llm, host).await
    }

    /// Starts the runtime with the given personas already activated on
    /// `whatsapp`, by publishing the user's decision onto the bus before
    /// Hermes boots — the order a deployment is really in, since the
    /// Companion Gateway is the only writer of consent state and it has been
    /// running all along.
    pub async fn start_activated(
        test_name: &str,
        personas: Vec<PersonaFixture>,
        canned_reply: &str,
    ) -> Result<Self> {
        let activated = personas.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
        let activated = activated.iter().map(String::as_str).collect::<Vec<_>>();
        Self::start_with(test_name, personas, &activated, Some(canned_reply)).await
    }

    async fn start_with(
        test_name: &str,
        personas: Vec<PersonaFixture>,
        activated: &[&str],
        canned_reply: Option<&str>,
    ) -> Result<Self> {
        let llm = match canned_reply {
            Some(reply) => StubLlm::start_with_reply(reply).await?,
            None => StubLlm::start().await?,
        };
        Self::bring_up(test_name, personas, activated, llm, Vec::new()).await
    }

    async fn bring_up(
        test_name: &str,
        personas: Vec<PersonaFixture>,
        activated: &[&str],
        llm: StubLlm,
        host: HostEnvironment,
    ) -> Result<Self> {
        ensure_stack().await?;
        if personas.iter().any(|p| p.image == PERSONA_IMAGE) {
            ensure_persona_image().await?;
        }

        let id = run_id(test_name);
        let bus = Bus::connect().await?;
        bus.ensure_stream(&id, &[&format!("{id}.>")]).await?;

        let mut run = Self {
            bus,
            llm,
            stream: id.clone(),
            prefix: id,
            personas,
            child: None,
            log_lines: Arc::new(Mutex::new(Vec::new())),
            decisions: std::sync::atomic::AtomicU32::new(0),
            host,
            stopped: false,
        };
        for persona_id in activated {
            run.decide(persona_id, "granted", &["whatsapp"]).await?;
        }
        run.spawn_runtime().await?;
        Ok(run)
    }

    /// The environment the runtime itself runs from.
    ///
    /// It deliberately holds [`GATEWAY_SERVICE_TOKEN_VAR`]: the point of the
    /// injection is that the runtime holds credentials a persona must not,
    /// and an absence assertion against an environment that never had the
    /// secret would prove nothing.
    fn runtime_environment(&self) -> Result<Vec<(String, String)>> {
        let personas = self
            .personas
            .iter()
            .map(|persona| json!({ "id": persona.id, "command": persona.command() }))
            .collect::<Vec<_>>();
        let mut environment = vec![
            (
                "HERMES_PERSONAS".to_owned(),
                Value::Array(personas).to_string(),
            ),
            ("HERMES_DOMAIN".to_owned(), HERMES_DOMAIN.to_owned()),
            ("HERMES_NATS_URL".to_owned(), nats_url()),
            ("HERMES_BUS_STREAM".to_owned(), self.stream.clone()),
            ("HERMES_BUS_SUBJECT_PREFIX".to_owned(), self.prefix.clone()),
            ("HERMES_LLM_BASE_URL".to_owned(), self.llm.base_url()),
            ("HERMES_LLM_MODEL".to_owned(), MODEL.to_owned()),
            ("HERMES_LLM_API_KEY".to_owned(), LLM_API_KEY.to_owned()),
            ("HERMES_LLM_PARAMS".to_owned(), LLM_PARAMS.to_owned()),
            // The user's own language (ADR 0016, ticket #164): held by the
            // Gateway, injected by the runtime like the model configuration.
            // Set here so that the injection is asserted on the environment
            // the runtime really built, rather than only in its unit tests.
            ("HERMES_USER_LANGUAGE".to_owned(), USER_LANGUAGE.to_owned()),
            ("HERMES_LOG_LEVEL".to_owned(), "info".to_owned()),
            ("HERMES_PERSONA_LOG_LEVEL".to_owned(), "debug".to_owned()),
            // A test must not sit through a production backoff, and the
            // failure verdict must land inside a poll window.
            (
                "HERMES_RESTART_BACKOFF_BASE_MS".to_owned(),
                "200".to_owned(),
            ),
            (
                "HERMES_RESTART_BACKOFF_MAX_MS".to_owned(),
                "1000".to_owned(),
            ),
            (
                "HERMES_PERSONA_HEALTHY_AFTER_MS".to_owned(),
                "5000".to_owned(),
            ),
            ("HERMES_PERSONA_START_FAILURES".to_owned(), "3".to_owned()),
            ("HERMES_SHUTDOWN_GRACE_MS".to_owned(), "8000".to_owned()),
            // `docker` is what the persona's argv runs; it needs the host's
            // PATH and its own client configuration, and nothing else.
            (
                "HERMES_PERSONA_ENV_PASSTHROUGH".to_owned(),
                "PATH,HOME,DOCKER_HOST,XDG_RUNTIME_DIR".to_owned(),
            ),
            (
                GATEWAY_SERVICE_TOKEN_VAR.to_owned(),
                GATEWAY_SERVICE_TOKEN.to_owned(),
            ),
        ];
        // What the test said the operator did — or did not — write into
        // `.env` (ticket #184). Applied last, and a `None` **removes** the
        // variable: "this is not set on the host" is the state the reference
        // deployment ships and cannot be expressed by adding variables.
        for (name, value) in &self.host {
            environment.retain(|(key, _)| key != name);
            if let Some(value) = value {
                environment.push(((*name).to_owned(), value.clone()));
            }
        }
        Ok(environment)
    }

    async fn spawn_runtime(&mut self) -> Result<()> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_twalk-hermes"))
            .envs(self.runtime_environment()?)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start the hermes binary")?;
        // The personas' own logs arrive here too: the runtime spawns them
        // with its own stdio, the way a supervisor does, so an operator
        // reads one stream. A test reading for "persona ready" is reading
        // the persona's line, forwarded.
        forward(
            child.stdout.take().expect("stdout is piped"),
            false,
            self.log_lines.clone(),
        );
        forward(
            child.stderr.take().expect("stderr is piped"),
            true,
            self.log_lines.clone(),
        );
        self.child = Some(child);
        self.wait_for_log("hermes running").await?;
        Ok(())
    }

    /// Publishes one of the user's consent decisions about a persona, the
    /// way the Companion Gateway does: a schema-valid
    /// `consent.state.changed` with the contract's deterministic id.
    pub async fn decide(&self, persona_id: &str, new_state: &str, networks: &[&str]) -> Result<()> {
        let nth = self
            .decisions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let occurred_at = format!("2026-09-17T10:{:02}:00Z", nth % 60);
        let scope = networks.join(",");
        let event = json!({
            "specversion": "1.0",
            "id": sha256_hex(&format!("persona:{persona_id}:{new_state}:{scope}:{occurred_at}")),
            "source": "gateway://test.twalk/consent",
            "type": CONSENT_CHANGED_TYPE,
            "time": occurred_at,
            "subject": persona_id,
            "datacontenttype": "application/json",
            "dataschema": "https://schemas.twalk.dev/cloudevents/v1/consent.state.changed.schema.json",
            "data": {
                "subject": { "type": "persona", "id": persona_id },
                "old_state": "unset",
                "new_state": new_state,
                "scope": { "networks": networks },
                "occurred_at": occurred_at,
                "actor": "@michel:test.twalk",
            }
        });
        validate_against_contract(&event, "consent.state.changed")?;
        self.bus
            .publish_event(&self.subject(CONSENT_CHANGED_TYPE), &event)
            .await
    }

    /// The bus subject a contract event type travels on, in this run's
    /// namespace.
    pub fn subject(&self, event_type: &str) -> String {
        let suffix = event_type
            .strip_prefix("fr.linagora.twalk.")
            .expect("contract event types carry the fr.linagora.twalk prefix");
        format!("{}.{suffix}", self.prefix)
    }

    /// Publishes an inbound event the way the Sensor does.
    pub async fn publish_inbound(&self, event: &Value) -> Result<()> {
        self.bus
            .publish_event(&self.subject(INBOUND_TYPE), event)
            .await
    }

    /// Every event of a type a persona published in this run, in stream
    /// order.
    pub async fn published(&self, event_type: &str) -> Result<Vec<StoredMessage>> {
        self.bus
            .fetch_all_with_headers(&self.stream, &self.subject(event_type))
            .await
    }

    /// Polls until a persona publishes an event of `event_type` about
    /// `trigger_event_id`, and brings the runtime's logs with the failure —
    /// which is where the answer always is.
    pub async fn wait_for(
        &self,
        event_type: &str,
        trigger_event_id: &str,
    ) -> Result<StoredMessage> {
        let found = poll_until(
            || async {
                self.published(event_type)
                    .await
                    .ok()?
                    .into_iter()
                    .find(|message| message.payload["subject"].as_str() == Some(trigger_event_id))
            },
            &format!("{event_type} about {trigger_event_id}"),
        )
        .await;
        match found {
            Ok(message) => Ok(message),
            Err(error) => bail!("{error}; the runtime's logs were:\n{}", self.logs().await),
        }
    }

    /// The runtime's captured output so far — the personas' forwarded lines
    /// included, exactly as an operator sees them.
    pub async fn logs(&self) -> String {
        self.log_lines.lock().await.join("\n")
    }

    /// Polls until a line containing `needle` has been logged.
    pub async fn wait_for_log(&self, needle: &str) -> Result<()> {
        let found = poll_until(
            || async { self.logs().await.contains(needle).then_some(()) },
            &format!("a log line containing {needle:?}"),
        )
        .await;
        match found {
            Ok(()) => Ok(()),
            Err(error) => bail!("{error}; the runtime's logs were:\n{}", self.logs().await),
        }
    }

    /// Whether the runtime process is still up.
    pub fn is_running(&mut self) -> bool {
        match &mut self.child {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Whether this persona's container is running, asked of Docker rather
    /// than inferred from a log line.
    pub async fn persona_is_running(&self, persona_id: &str) -> Result<bool> {
        let output = Command::new("docker")
            .args([
                "ps",
                "--quiet",
                "--filter",
                &format!("name=^{}$", self.container_of(persona_id)?),
            ])
            .output()
            .await
            .context("failed to ask docker what is running")?;
        Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
    }

    /// Polls until this persona's container is up.
    pub async fn wait_for_persona(&self, persona_id: &str) -> Result<()> {
        poll_until(
            || async {
                self.persona_is_running(persona_id)
                    .await
                    .ok()
                    .filter(|running| *running)
                    .map(|_| ())
            },
            &format!("the persona {persona_id}'s container to be running"),
        )
        .await
    }

    /// The persona container's environment, as Docker records it: what the
    /// runtime actually handed the process it started.
    pub async fn persona_environment(&self, persona_id: &str) -> Result<Vec<String>> {
        let output = Command::new("docker")
            .args([
                "inspect",
                "--format",
                "{{json .Config.Env}}",
                &self.container_of(persona_id)?,
            ])
            .output()
            .await
            .context("failed to inspect the persona container")?;
        if !output.status.success() {
            bail!(
                "docker inspect failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let parsed: Value = serde_json::from_slice(&output.stdout)
            .context("docker inspect did not answer with JSON")?;
        Ok(parsed
            .as_array()
            .context("the container's environment is not an array")?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect())
    }

    /// Kills the persona's container the way a crash does — SIGKILL, no
    /// chance to tidy up.
    pub async fn kill_persona(&self, persona_id: &str) -> Result<()> {
        let status = Command::new("docker")
            .args(["kill", "--signal", "KILL", &self.container_of(persona_id)?])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .context("failed to kill the persona container")?;
        if !status.success() {
            bail!("docker kill failed with {status}");
        }
        Ok(())
    }

    /// The chat-completions requests whose prompt mentions `marker`.
    pub fn llm_requests_mentioning(&self, marker: &str) -> Vec<StubRequest> {
        self.llm
            .requests()
            .into_iter()
            .filter(|request| request.body.to_string().contains(marker))
            .collect()
    }

    /// Sends SIGTERM — as an operator's process manager would — and waits
    /// for the runtime to exit. (`Child::kill` only sends SIGKILL, which
    /// cannot exercise a graceful shutdown.)
    pub async fn terminate(&mut self) -> Result<std::process::ExitStatus> {
        let child = self.child.as_mut().context("the runtime is not running")?;
        let pid = child.id().context("the runtime has already exited")?;
        let status = Command::new("kill")
            .arg(pid.to_string())
            .status()
            .await
            .context("failed to run kill(1)")?;
        anyhow::ensure!(status.success(), "kill(1) failed with {status}");
        let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
            .await
            .context("the runtime did not exit within 30s of SIGTERM")??;
        self.child = None;
        Ok(status)
    }

    fn container_of(&self, persona_id: &str) -> Result<String> {
        self.personas
            .iter()
            .find(|persona| persona.id == persona_id)
            .map(|persona| persona.container.clone())
            .with_context(|| format!("this run hosts no persona {persona_id}"))
    }

    /// Stops the runtime and gives back everything this run claimed on the
    /// shared stack.
    pub async fn shutdown(mut self) -> Result<()> {
        self.stopped = true;
        if self.child.is_some() {
            let _ = self.terminate().await;
        }
        remove_containers(&self.personas);
        self.bus.delete_stream(&self.stream).await?;
        Ok(())
    }
}

impl Drop for RuntimeRun {
    /// A test that panicked never reached `shutdown`; the runtime and its
    /// containers must still go. Blocking, deliberately: a teardown that
    /// runs is worth a second of a test thread.
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        if let Some(child) = &mut self.child {
            if let Some(pid) = child.id() {
                let _ = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status();
            }
        }
        remove_containers(&self.personas);
    }
}

fn remove_containers(personas: &[PersonaFixture]) {
    for persona in personas {
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", &persona.container])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

pub(crate) fn forward<S>(stream: S, is_stderr: bool, store: Arc<Mutex<Vec<String>>>)
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
    });
}
