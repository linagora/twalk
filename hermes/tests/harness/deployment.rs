//! The reference deployment, with Hermes joining it from the host
//! (ticket #25).
//!
//! [`PersonaRun`](super::PersonaRun) drives a persona alone and
//! [`RuntimeRun`](super::RuntimeRun) drives the runtime that starts one;
//! both talk to the shared test stack's bus and to nothing else. Neither can
//! answer the question this ticket asks, because the answer is not on the
//! bus: *does the reply reach the contact?* Only a real Sensor posting into
//! a real homeserver can say so, and only the Companion Gateway can turn a
//! human decision into the event it posts (ADR 0022).
//!
//! So this harness brings up the reference deployment itself —
//! `deploy/docker-compose/compose.yaml`, the same file an operator runs,
//! configured only through an environment file — and starts the real
//! `twalk-hermes` binary on the host beside it, because that compose file
//! has no Hermes service yet. Everything else is the deployment's own: the
//! Sensor that observes the room and posts the approved reply, the Gateway
//! that serves the approval, the bus between them, and Synapse underneath.
//!
//! Nothing here reaches inside any of those processes. A test asks the
//! homeserver what is in the room, asks the bus what was published, asks the
//! Gateway over HTTP, and asks the stub LLM what it was sent.
//!
//! # What this stack costs, and what it gives back
//!
//! One compose project, four containers, one Docker network and four
//! volumes. The host this suite runs on has filled its disk and exhausted
//! its address pools doing less (#128), so the stack is **torn down at the
//! end of every run, passing or failing**, along with the two images this
//! project tags for itself. What a failure needs in order to be diagnosed —
//! the runtime's logs, the deployment's logs — is attached to the failure
//! itself rather than left behind on the host. `TWALK_LOOP_TEST_KEEP=1`
//! keeps the stack up for an operator who would rather poke at it by hand.
//!
//! # Isolation
//!
//! Its own compose project and its own host ports, all in the range this
//! ticket was given so that a parallel worktree, the Sensor's deployment
//! test and the Gateway's never collide with it:
//! `TWALK_LOOP_TEST_STACK` (default `twalk-h25-loop`),
//! `TWALK_LOOP_TEST_SYNAPSE_PORT` (19508), `TWALK_LOOP_TEST_GATEWAY_PORT`
//! (19518), `TWALK_LOOP_TEST_NATS_PORT` (19522). Both local images are
//! tagged per compose project (`twalk/companion-gateway:<stack>`,
//! `twalk/sensor:<stack>`, issue #38), so a build here never overwrites
//! another stack's image or an operator's `:local`.
//!
//! The credentials below are throwaway constants for this ephemeral stack,
//! in the same category as the test bots' passwords; the environment file
//! they land in is generated in a temp directory and removed with the stack.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::sync::Mutex;
use twalk_test_harness::{Bus, StoredMessage, StubLlm};

use super::runtime::{ensure_persona_image, forward, persona_wrapper, PERSONA_IMAGE};
use super::{CONSENT_CHANGED_TYPE, INBOUND_TYPE, LLM_API_KEY, MODEL, PERSONA_ID};

/// The Matrix server name this deployment answers for. Deliberately not the
/// test stack's `test.twalk` nor the other deployment suites' `deploy.twalk`:
/// a homeserver's name is in every event's `source`, so a distinct one makes
/// an event from the wrong stack obvious rather than plausible.
pub const DEPLOY_SERVER_NAME: &str = "loop.twalk";

/// The owner of this deployment: the account the Gateway's registration
/// relay creates, the identity every approval is stamped with, and the only
/// inviter the Sensor honours.
const OWNER_LOCALPART: &str = "owner";
const OWNER_PASSWORD: &str = "loop-test-only-password-owner";
const REGISTRATION_SHARED_SECRET: &str = "loop-test-only-registration-shared-secret";
const CONTACT_PASSWORD: &str = "loop-test-only-password-contact";
/// The credential the Sensor reads this stack's consent snapshot with
/// (ADR 0010) — which is what makes the Sensor label a contact's message
/// with the state the user decided.
const SERVICE_TOKEN: &str = "loop-test-only-gateway-service-token-long-enough";

/// The bus namespace a deployment publishes under: not a per-run prefix, as
/// in the other Hermes suites, because this *is* a deployment and it is torn
/// down whole.
pub const DEPLOY_STREAM: &str = "twalk";

/// The one contract type this ticket adds to the Hermes suite's vocabulary:
/// the event an approval publishes and the Sensor posts. The others
/// (`INBOUND_TYPE`, `THINKING_TYPE`, `SUGGEST_TYPE`, `CONSENT_CHANGED_TYPE`)
/// are the harness's already.
pub const APPROVED_TYPE: &str = "fr.linagora.twalk.persona.reply.approved.v1";

fn stack() -> String {
    std::env::var("TWALK_LOOP_TEST_STACK").unwrap_or_else(|_| "twalk-h25-loop".to_owned())
}

fn synapse_port() -> String {
    std::env::var("TWALK_LOOP_TEST_SYNAPSE_PORT").unwrap_or_else(|_| "19508".to_owned())
}

fn gateway_port() -> String {
    std::env::var("TWALK_LOOP_TEST_GATEWAY_PORT").unwrap_or_else(|_| "19518".to_owned())
}

fn nats_port() -> String {
    std::env::var("TWALK_LOOP_TEST_NATS_PORT").unwrap_or_else(|_| "19522".to_owned())
}

/// Whether the operator asked for the stack to survive the run. Off by
/// default: a stack that outlives its run is a defect (#128), and this one
/// is expensive.
fn keep_requested() -> bool {
    matches!(
        std::env::var("TWALK_LOOP_TEST_KEEP").as_deref(),
        Ok("1") | Ok("true")
    )
}

fn gateway_image() -> String {
    format!("twalk/companion-gateway:{}", stack())
}

fn sensor_image() -> String {
    format!("twalk/sensor:{}", stack())
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("the hermes package sits inside the repository")
}

fn compose_file() -> PathBuf {
    repository_root()
        .join("deploy")
        .join("docker-compose")
        .join("compose.yaml")
}

fn owner_user_id() -> String {
    format!("@{OWNER_LOCALPART}:{DEPLOY_SERVER_NAME}")
}

fn homeserver_url() -> String {
    format!("http://localhost:{}", synapse_port())
}

fn gateway_url() -> String {
    format!("http://localhost:{}", gateway_port())
}

/// [`poll_until`](twalk_test_harness::poll_until) with the patience a cold
/// deployment needs: Synapse initialises a fresh database, the Sensor logs
/// in, bootstraps its crypto store and syncs before it can act on an
/// invitation. That is minutes of container start rather than the twenty
/// seconds a process-boundary test waits for a process that is already up.
pub async fn poll_deploy<T, Fut>(mut attempt: impl FnMut() -> Fut, description: &str) -> Result<T>
where
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..360 {
        if let Some(value) = attempt().await {
            return Ok(value);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!("timed out {description}")
}

/// One account on this deployment's homeserver, with a session of its own:
/// a contact the user's persona may or may not answer, and the pair of eyes
/// a test reads the room with.
#[derive(Clone, Debug)]
pub struct Contact {
    pub user_id: String,
    pub token: String,
}

/// The reference deployment, plus the Hermes runtime on the host.
pub struct Deployment {
    /// The deployment's bus, connected from the test side.
    pub bus: Bus,
    /// The model the persona reasons with: a test scripts its answers and
    /// asks what it was sent — or asserts it was sent nothing.
    pub llm: StubLlm,
    /// The room the Sensor was invited into: where every message of this
    /// test is written and where the approved reply must appear.
    pub room_id: String,
    /// The owner's Matrix session, for the acts a Matrix client performs
    /// (creating the room, inviting).
    pub owner_token: String,
    /// The owner's device cookie at the Gateway: what makes an approval a
    /// human's deliberate act rather than an anonymous call.
    pub device_token: String,
    client: reqwest::Client,
    env_file: PathBuf,
    persona_container: String,
    hermes: Option<tokio::process::Child>,
    hermes_log_lines: Arc<Mutex<Vec<String>>>,
    stopped: bool,
}

impl Deployment {
    /// Brings the deployment up and walks it to the state a loop test starts
    /// from: the owner's account created, a device signed in, a native
    /// Matrix room of the owner's own, and the Sensor inside it.
    ///
    /// Hermes is **not** started here: a test decides which personas the
    /// user activated before the runtime reads that decision, which is the
    /// order a deployment is really in (ADR 0013).
    pub async fn start(canned_reply: &str) -> Result<Self> {
        let env_file = write_env_file()?;
        // Build first, so a build failure is attributed to the build. The
        // image tags are this stack's own; their layers are the daemon's
        // shared cache, so a warm host rebuilds nothing.
        compose_with(
            &env_file,
            &["build", "companion-gateway", "sensor"],
            "build",
        )
        .await?;
        compose_with(
            &env_file,
            &["up", "-d", "--wait", "companion-gateway", "sensor"],
            "up companion-gateway sensor",
        )
        .await?;

        let mut run = Self {
            bus: Bus::connect_to(&format!("nats://localhost:{}", nats_port())).await?,
            llm: StubLlm::start_with_reply(canned_reply).await?,
            room_id: String::new(),
            owner_token: String::new(),
            device_token: String::new(),
            client: reqwest::Client::new(),
            env_file,
            persona_container: format!("h25-{}-{PERSONA_ID}", std::process::id()),
            hermes: None,
            hermes_log_lines: Arc::new(Mutex::new(Vec::new())),
            stopped: false,
        };
        run.owner_token = run.register_owner().await?;
        run.device_token = run.sign_in().await?;
        run.room_id = run.create_room().await?;
        run.invite_the_sensor().await?;
        Ok(run)
    }

    async fn compose(&self, args: &[&str], what: &str) -> Result<String> {
        compose_with(&self.env_file, args, what).await
    }

    /// The deployment's own logs, as an operator reads them: attached to a
    /// failure, because the stack is torn down before anyone could go and
    /// look.
    pub async fn deployment_logs(&self) -> String {
        self.compose(&["logs", "--no-color", "--tail", "80"], "logs")
            .await
            .unwrap_or_else(|error| format!("(the deployment's logs could not be read: {error})"))
    }

    // -- the owner, and the room ------------------------------------------

    /// Creates this deployment's one account through the Gateway's
    /// registration relay, and answers with the owner's Matrix token. A warm
    /// stack (`TWALK_LOOP_TEST_KEEP=1` on a previous run) already has the
    /// account, and the relay refuses a second: the password is this test's
    /// own, so it logs in instead.
    async fn register_owner(&self) -> Result<String> {
        // Polled for a *verdict*, not merely for an answer: a Gateway that is
        // up before its homeserver is answers the relay's call with a 5xx,
        // and reading the first response as the verdict would turn a stack
        // still starting into a failing test.
        let response = poll_deploy(
            || async {
                let response = self
                    .client
                    .post(format!("{}/api/bootstrap/account", gateway_url()))
                    .json(&json!({ "username": OWNER_LOCALPART, "password": OWNER_PASSWORD }))
                    .send()
                    .await
                    .ok()?;
                matches!(
                    response.status(),
                    reqwest::StatusCode::CREATED | reqwest::StatusCode::CONFLICT
                )
                .then_some(response)
            },
            "the deployed Gateway to create this deployment's one account",
        )
        .await?;
        match response.status() {
            reqwest::StatusCode::CREATED => {
                let document: Value = response.json().await?;
                document["access_token"]
                    .as_str()
                    .map(str::to_owned)
                    .context("the registration relay answered no access token")
            }
            reqwest::StatusCode::CONFLICT => self.login(OWNER_LOCALPART, OWNER_PASSWORD).await,
            status => bail!(
                "the registration relay answered {status}: {}",
                response.text().await.unwrap_or_default()
            ),
        }
    }

    /// Signs a device in at the Gateway with a Matrix OpenID token, as the
    /// Companion does, and answers with the device cookie.
    async fn sign_in(&self) -> Result<String> {
        let openid: Value = self
            .client
            .post(format!(
                "{}/_matrix/client/v3/user/{}/openid/request_token",
                homeserver_url(),
                owner_user_id()
            ))
            .bearer_auth(&self.owner_token)
            .json(&json!({}))
            .send()
            .await?
            .json()
            .await?;
        let response = self
            .client
            .post(format!("{}/api/session", gateway_url()))
            .json(&json!({
                "matrix_openid_token": openid,
                "device_name": "the loop test",
            }))
            .send()
            .await?;
        anyhow::ensure!(
            response.status() == reqwest::StatusCode::OK,
            "the owner must be able to sign in at the deployed Gateway: {} {}",
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
                let (key, cookie) = pair.split_once('=')?;
                (key == "twalk_device").then(|| cookie.to_owned())
            })
            .context("the sign-in answered no device cookie")
    }

    /// A private room of the owner's own, with no bridge marker: native
    /// Matrix traffic (ADR 0009), which is the network this deployment's
    /// conversations come from.
    async fn create_room(&self) -> Result<String> {
        let body: Value = self
            .client
            .post(format!("{}/_matrix/client/v3/createRoom", homeserver_url()))
            .bearer_auth(&self.owner_token)
            .json(&json!({ "name": "the loop", "preset": "private_chat" }))
            .send()
            .await?
            .json()
            .await?;
        body["room_id"]
            .as_str()
            .map(str::to_owned)
            .context("the createRoom answer names no room id")
    }

    /// Invites the Sensor through the Gateway — the user's own act, from
    /// screen 3d — and waits until it has joined. The Sensor joins on its
    /// own, because the owner is on its allow-list.
    async fn invite_the_sensor(&self) -> Result<()> {
        let sensor = format!("@sensor:{DEPLOY_SERVER_NAME}");
        let invited = self
            .client
            .post(format!("{}/api/bootstrap/rooms", gateway_url()))
            .header("cookie", format!("twalk_device={}", self.device_token))
            .json(&json!({
                "matrix_access_token": self.owner_token,
                "rooms": [self.room_id.clone()],
            }))
            .send()
            .await?;
        anyhow::ensure!(
            invited.status() == reqwest::StatusCode::OK,
            "the Gateway must invite the Sensor into the selected room: {} {}",
            invited.status(),
            invited.text().await.unwrap_or_default()
        );
        poll_deploy(
            || async {
                let membership = self.membership(&self.room_id, &sensor).await.ok()??;
                (membership == "join").then_some(())
            },
            "the Sensor to join the room it was invited to",
        )
        .await
    }

    async fn membership(&self, room_id: &str, user_id: &str) -> Result<Option<String>> {
        let response = self
            .client
            .get(format!(
                "{}/_matrix/client/v3/rooms/{room_id}/state/m.room.member/{user_id}",
                homeserver_url()
            ))
            .bearer_auth(&self.owner_token)
            .send()
            .await?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let body: Value = response.json().await?;
        Ok(body["membership"].as_str().map(str::to_owned))
    }

    // -- the contacts ------------------------------------------------------

    /// Provisions one contact account on this deployment's homeserver, logs
    /// it in, and puts it in the room.
    ///
    /// Provisioned with the homeserver's own `register_new_matrix_user` and
    /// not through the Gateway's relay, which creates the owner's account
    /// and refuses every other: a second human on this homeserver is the
    /// operator's business.
    pub async fn add_contact(&self, label: &str) -> Result<Contact> {
        let localpart = format!(
            "{label}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the clock is after the epoch")
                .as_nanos()
                % 1_000_000
        );
        self.compose(
            &[
                "exec",
                "-T",
                "synapse",
                "register_new_matrix_user",
                "-u",
                &localpart,
                "-p",
                CONTACT_PASSWORD,
                "--no-admin",
                "-c",
                "/data/homeserver.yaml",
                "http://localhost:8008",
            ],
            "register a contact",
        )
        .await?;
        let contact = Contact {
            user_id: format!("@{localpart}:{DEPLOY_SERVER_NAME}"),
            token: self.login(&localpart, CONTACT_PASSWORD).await?,
        };
        self.invite(&contact.user_id).await?;
        self.join(&contact.token).await?;
        Ok(contact)
    }

    async fn login(&self, localpart: &str, password: &str) -> Result<String> {
        let body: Value = self
            .client
            .post(format!("{}/_matrix/client/v3/login", homeserver_url()))
            .json(&json!({
                "type": "m.login.password",
                "identifier": { "type": "m.id.user", "user": localpart },
                "password": password,
            }))
            .send()
            .await?
            .json()
            .await?;
        body["access_token"]
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("the login as {localpart} answered no access token"))
    }

    async fn invite(&self, user_id: &str) -> Result<()> {
        let response = self
            .client
            .post(format!(
                "{}/_matrix/client/v3/rooms/{}/invite",
                homeserver_url(),
                self.room_id
            ))
            .bearer_auth(&self.owner_token)
            .json(&json!({ "user_id": user_id }))
            .send()
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "the homeserver refused the invitation: {}",
            response.text().await.unwrap_or_default()
        );
        Ok(())
    }

    async fn join(&self, token: &str) -> Result<()> {
        let response = self
            .client
            .post(format!(
                "{}/_matrix/client/v3/rooms/{}/join",
                homeserver_url(),
                self.room_id
            ))
            .bearer_auth(token)
            .json(&json!({}))
            .send()
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "the homeserver refused the join: {}",
            response.text().await.unwrap_or_default()
        );
        Ok(())
    }

    /// Writes one message in the room, as that account's own client does.
    pub async fn says(&self, contact: &Contact, body: &str) -> Result<()> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let response = self
            .client
            .put(format!(
                "{}/_matrix/client/v3/rooms/{}/send/m.room.message/twalk-h25-{unique}",
                homeserver_url(),
                self.room_id
            ))
            .bearer_auth(&contact.token)
            .json(&json!({ "msgtype": "m.text", "body": body }))
            .send()
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "the homeserver refused the message: {}",
            response.text().await.unwrap_or_default()
        );
        Ok(())
    }

    /// The text of the room's recent messages, as an account that is in it
    /// reads them. This is how a test asks the **homeserver** whether a
    /// reply was really posted, rather than believing Twalk's own account of
    /// it — and how it asks whether one was not.
    pub async fn room_bodies(&self, reader: &Contact) -> Result<Vec<String>> {
        let response = self
            .client
            .get(format!(
                "{}/_matrix/client/v3/rooms/{}/messages?dir=b&limit=100",
                homeserver_url(),
                self.room_id
            ))
            .bearer_auth(&reader.token)
            .send()
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "the homeserver refused to list the room's messages: {}",
            response.text().await.unwrap_or_default()
        );
        let document: Value = response.json().await?;
        Ok(document["chunk"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|event| event["content"]["body"].as_str().map(str::to_owned))
            .collect())
    }

    /// The room's recent message events, content and all: what a test reads
    /// to ask whether a reply is a *native Matrix reply* rather than only
    /// whether its text is there.
    pub async fn room_events(&self, reader: &Contact) -> Result<Vec<Value>> {
        let response = self
            .client
            .get(format!(
                "{}/_matrix/client/v3/rooms/{}/messages?dir=b&limit=100",
                homeserver_url(),
                self.room_id
            ))
            .bearer_auth(&reader.token)
            .send()
            .await?;
        let document: Value = response.json().await?;
        Ok(document["chunk"].as_array().cloned().unwrap_or_default())
    }

    // -- consent, and the persona's activation ----------------------------

    /// Takes one of the user's consent decisions through the Gateway's write
    /// API — the only writer of consent state (ADR 0006) — about a contact
    /// or about a persona. Activating a persona is a decision of exactly the
    /// same kind (ADR 0013), which is why there is one method and not two.
    pub async fn decide(
        &self,
        subject_type: &str,
        id: &str,
        new_state: &str,
        networks: &[&str],
    ) -> Result<()> {
        let response = self
            .client
            .post(format!("{}/api/consent/decisions", gateway_url()))
            .header("cookie", format!("twalk_device={}", self.device_token))
            .json(&json!({
                "subject": { "type": subject_type, "id": id },
                "new_state": new_state,
                "scope": { "networks": networks },
            }))
            .send()
            .await?;
        anyhow::ensure!(
            response.status() == reqwest::StatusCode::CREATED,
            "the decision about {subject_type} {id} must be recorded: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
        Ok(())
    }

    /// Waits until a decision about this subject has actually reached the
    /// bus. The Gateway commits a decision and publishes it from its outbox,
    /// so "the API answered 201" and "the runtime can see it" are two
    /// different instants — and Hermes learns activation from the bus alone.
    pub async fn wait_for_decision(&self, subject_id: &str, new_state: &str) -> Result<()> {
        poll_deploy(
            || async {
                self.published(CONSENT_CHANGED_TYPE)
                    .await
                    .ok()?
                    .into_iter()
                    .any(|message| {
                        message.payload["data"]["subject"]["id"].as_str() == Some(subject_id)
                            && message.payload["data"]["new_state"].as_str() == Some(new_state)
                    })
                    .then_some(())
            },
            &format!("the decision {new_state} about {subject_id} to reach the bus"),
        )
        .await
    }

    /// Gives an approval, as the Companion does: `POST /api/approvals` with
    /// the device's own cookie. Answers the status and the document, because
    /// a refusal is as much a result as a `201` here.
    pub async fn approve(
        &self,
        suggestion_event_id: &str,
        edited_body: Option<&str>,
    ) -> Result<(reqwest::StatusCode, Value)> {
        let mut body = json!({ "suggestion_event_id": suggestion_event_id });
        if let Some(edited) = edited_body {
            body["final"] = json!({ "body": edited, "format": "text/plain" });
        }
        let response = self
            .client
            .post(format!("{}/api/approvals", gateway_url()))
            .header("cookie", format!("twalk_device={}", self.device_token))
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let document: Value = serde_json::from_str(&response.text().await?)
            .context("the Gateway's approval answer is not JSON")?;
        Ok((status, document))
    }

    // -- the bus -----------------------------------------------------------

    /// The bus subject a contract event type travels on in a deployment.
    pub fn subject(&self, event_type: &str) -> String {
        let suffix = event_type
            .strip_prefix("fr.linagora.twalk.")
            .expect("contract event types carry the fr.linagora.twalk prefix");
        format!("{DEPLOY_STREAM}.{suffix}")
    }

    /// Every event of a type published on this deployment's bus, in stream
    /// order.
    pub async fn published(&self, event_type: &str) -> Result<Vec<StoredMessage>> {
        self.bus
            .fetch_all_with_headers(DEPLOY_STREAM, &self.subject(event_type))
            .await
    }

    /// Everything a failure of this suite needs in order to be diagnosed.
    ///
    /// It goes into the failure itself rather than being looked up
    /// afterwards, because this suite takes its stack down when the run ends
    /// (#128): by the time anybody reads the failure, the containers whose
    /// logs would answer it are gone.
    pub async fn diagnostics(&self) -> String {
        format!(
            "the runtime's logs were:\n{}\n\nand the deployment's:\n{}",
            self.hermes_logs().await,
            self.deployment_logs().await
        )
    }

    /// Polls until the Sensor publishes an inbound message whose body is
    /// exactly `body` — the event the rest of the loop hangs from.
    pub async fn wait_for_inbound(&self, body: &str) -> Result<StoredMessage> {
        let found = poll_deploy(
            || async {
                self.published(INBOUND_TYPE)
                    .await
                    .ok()?
                    .into_iter()
                    .find(|message| message.payload["data"]["body"].as_str() == Some(body))
            },
            &format!("the Sensor to publish the message {body:?}"),
        )
        .await;
        match found {
            Ok(message) => Ok(message),
            Err(error) => bail!("{error}; {}", self.diagnostics().await),
        }
    }

    /// Polls until an event with this id is on the bus: how a test follows
    /// the approval the Gateway says it published.
    pub async fn wait_for_event(
        &self,
        event_type: &str,
        event_id: &Value,
    ) -> Result<StoredMessage> {
        let found = poll_deploy(
            || async {
                self.published(event_type)
                    .await
                    .ok()?
                    .into_iter()
                    .find(|message| &message.payload["id"] == event_id)
            },
            &format!("{event_type} {event_id} to reach the bus"),
        )
        .await;
        match found {
            Ok(message) => Ok(message),
            Err(error) => bail!("{error}; {}", self.diagnostics().await),
        }
    }

    /// Polls until this exact text is in the room, read as an account that is
    /// in it. The loop's last step, and the only one whose witness is the
    /// homeserver rather than Twalk.
    pub async fn wait_for_room_body(&self, reader: &Contact, body: &str) -> Result<()> {
        let found = poll_deploy(
            || async {
                self.room_bodies(reader)
                    .await
                    .ok()?
                    .into_iter()
                    .any(|posted| posted == body)
                    .then_some(())
            },
            &format!("the message {body:?} to appear in the room"),
        )
        .await;
        match found {
            Ok(()) => Ok(()),
            Err(error) => bail!("{error}; {}", self.diagnostics().await),
        }
    }

    /// Polls until a persona publishes an event of `event_type` about
    /// `trigger_event_id`, and brings the runtime's logs with the failure —
    /// which is where the answer always is.
    pub async fn wait_for_persona_event(
        &self,
        event_type: &str,
        trigger_event_id: &str,
    ) -> Result<StoredMessage> {
        let found = poll_deploy(
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
            Err(error) => bail!(
                "{error}; the runtime's logs were:\n{}\n\nand the deployment's:\n{}",
                self.hermes_logs().await,
                self.deployment_logs().await
            ),
        }
    }

    /// Every persona event of a type that is about this trigger.
    pub async fn persona_events_about(
        &self,
        event_type: &str,
        trigger_event_id: &str,
    ) -> Result<Vec<Value>> {
        Ok(self
            .published(event_type)
            .await?
            .into_iter()
            .filter(|message| message.payload["subject"].as_str() == Some(trigger_event_id))
            .map(|message| message.payload)
            .collect())
    }

    /// The chat-completions requests whose prompt mentions `marker`.
    pub fn llm_requests_mentioning(&self, marker: &str) -> Vec<twalk_test_harness::StubRequest> {
        self.llm
            .requests()
            .into_iter()
            .filter(|request| request.body.to_string().contains(marker))
            .collect()
    }

    // -- Hermes, on the host ----------------------------------------------

    /// Starts the real `twalk-hermes` binary against this deployment: the
    /// bus it publishes on, the stream it publishes to, and the persona
    /// image the deployment would run as a service if the compose file had
    /// one yet.
    ///
    /// Called by a test *after* the user's activation decision has reached
    /// the bus: a persona nobody decided about is paused, and a paused
    /// persona's messages are not replayed to it when it is activated
    /// (`hermes/README.md`), so the order is the deployment's own and not a
    /// convenience.
    pub async fn start_hermes(&mut self) -> Result<()> {
        ensure_persona_image().await?;
        let personas = json!([{
            "id": PERSONA_ID,
            "command": [
                persona_wrapper(),
                self.persona_container.clone(),
                PERSONA_IMAGE.to_owned(),
            ],
        }]);
        let environment: Vec<(String, String)> = vec![
            ("HERMES_PERSONAS".to_owned(), personas.to_string()),
            ("HERMES_DOMAIN".to_owned(), DEPLOY_SERVER_NAME.to_owned()),
            (
                "HERMES_NATS_URL".to_owned(),
                format!("nats://localhost:{}", nats_port()),
            ),
            ("HERMES_BUS_STREAM".to_owned(), DEPLOY_STREAM.to_owned()),
            (
                "HERMES_BUS_SUBJECT_PREFIX".to_owned(),
                DEPLOY_STREAM.to_owned(),
            ),
            ("HERMES_LLM_BASE_URL".to_owned(), self.llm.base_url()),
            ("HERMES_LLM_MODEL".to_owned(), MODEL.to_owned()),
            ("HERMES_LLM_API_KEY".to_owned(), LLM_API_KEY.to_owned()),
            ("HERMES_LOG_LEVEL".to_owned(), "info".to_owned()),
            ("HERMES_PERSONA_LOG_LEVEL".to_owned(), "debug".to_owned()),
            // A test must not sit through a production backoff.
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
            ("HERMES_SHUTDOWN_GRACE_MS".to_owned(), "8000".to_owned()),
            // `docker` is what the persona's argv runs; it needs the host's
            // PATH and its own client configuration, and nothing else.
            (
                "HERMES_PERSONA_ENV_PASSTHROUGH".to_owned(),
                "PATH,HOME,DOCKER_HOST,XDG_RUNTIME_DIR".to_owned(),
            ),
        ];
        let mut child = Command::new(env!("CARGO_BIN_EXE_twalk-hermes"))
            .envs(environment)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("failed to start the hermes binary")?;
        forward(
            child.stdout.take().expect("stdout is piped"),
            false,
            self.hermes_log_lines.clone(),
        );
        forward(
            child.stderr.take().expect("stderr is piped"),
            true,
            self.hermes_log_lines.clone(),
        );
        self.hermes = Some(child);
        self.wait_for_hermes_log("hermes running").await?;
        // The persona's own line, forwarded through the runtime's stdio: it
        // has its durable consumer and is pulling.
        self.wait_for_hermes_log("persona ready").await
    }

    /// The runtime's captured output so far — the persona's forwarded lines
    /// included, exactly as an operator sees them.
    pub async fn hermes_logs(&self) -> String {
        self.hermes_log_lines.lock().await.join("\n")
    }

    async fn wait_for_hermes_log(&self, needle: &str) -> Result<()> {
        let found = poll_deploy(
            || async { self.hermes_logs().await.contains(needle).then_some(()) },
            &format!("a runtime log line containing {needle:?}"),
        )
        .await;
        match found {
            Ok(()) => Ok(()),
            Err(error) => bail!(
                "{error}; the runtime's logs were:\n{}",
                self.hermes_logs().await
            ),
        }
    }

    // -- teardown ----------------------------------------------------------

    /// Stops Hermes, removes the persona's container, and takes the whole
    /// deployment down — containers, volumes, network and this stack's two
    /// images — unless the operator asked to keep it.
    pub async fn shutdown(mut self) -> Result<()> {
        self.stopped = true;
        self.stop_hermes().await;
        if keep_requested() {
            return Ok(());
        }
        self.compose(&["down", "-v", "--remove-orphans"], "down")
            .await?;
        for image in [gateway_image(), sensor_image()] {
            let output = Command::new("docker")
                .args(["image", "rm", &image])
                .output()
                .await
                .context("failed to run docker image rm")?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Tolerate an already-removed image: teardown stays idempotent.
            if !output.status.success() && !stderr.contains("No such image") {
                bail!(
                    "docker image rm {image} failed with {}:\n{stderr}",
                    output.status
                );
            }
        }
        let _ = std::fs::remove_file(&self.env_file);
        Ok(())
    }

    async fn stop_hermes(&mut self) {
        if let Some(child) = &mut self.hermes {
            if let Some(pid) = child.id() {
                let _ = Command::new("kill").arg(pid.to_string()).status().await;
                let _ = tokio::time::timeout(Duration::from_secs(20), child.wait()).await;
            }
        }
        self.hermes = None;
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.persona_container])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

impl Drop for Deployment {
    /// A test that panicked never reached `shutdown`, and a stack that
    /// outlives its run is a defect rather than an inconvenience (#128): the
    /// teardown happens here too. Blocking, deliberately — a teardown that
    /// runs is worth the seconds it takes on a test thread.
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        if let Some(child) = &mut self.hermes {
            if let Some(pid) = child.id() {
                let _ = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status();
            }
        }
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", &self.persona_container])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if keep_requested() {
            return;
        }
        let _ = std::process::Command::new("docker")
            .args(teardown_argv(&self.env_file))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        for image in [gateway_image(), sensor_image()] {
            let _ = std::process::Command::new("docker")
                .args(["image", "rm", &image])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = std::fs::remove_file(&self.env_file);
    }
}

/// Runs `docker compose` against this stack with the run's environment file.
///
/// A failure the **host** caused is named as one rather than reported as a
/// failing test: a stack that could not be created because the daemon is out
/// of address pools or the disk is full is an environment failure (#128),
/// and reading it as a test failure is how an afternoon goes into the wrong
/// code.
async fn compose_with(env_file: &Path, args: &[&str], what: &str) -> Result<String> {
    let output = Command::new("docker")
        .arg("compose")
        .arg("-p")
        .arg(stack())
        .arg("--env-file")
        .arg(env_file)
        .arg("-f")
        .arg(compose_file())
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to run docker compose {what}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("address pools") || stderr.contains("no space left") {
            bail!(
                "this host cannot bring a test stack up — it is out of Docker address pools or \
                 disk, which is an environment failure and not a test one. `docker network prune` \
                 and `docker system prune`, plus taking down the stale twalk-* compose projects, \
                 free it. The daemon said:\n{stderr}"
            );
        }
        bail!(
            "docker compose {what} failed with {}:\n{stderr}",
            output.status
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The `docker compose` argv this stack's teardown runs, as owned strings:
/// `Drop` cannot await, so it runs the blocking command with the same
/// arguments [`compose_with`] would have used.
fn teardown_argv(env_file: &Path) -> Vec<String> {
    vec![
        "compose".to_owned(),
        "-p".to_owned(),
        stack(),
        "--env-file".to_owned(),
        env_file.to_string_lossy().into_owned(),
        "-f".to_owned(),
        compose_file().to_string_lossy().into_owned(),
        "down".to_owned(),
        "-v".to_owned(),
        "--remove-orphans".to_owned(),
    ]
}

/// Writes the environment file this deployment is configured through:
/// exactly what `.env.example` documents, with throwaway test values and
/// this run's own host ports.
fn write_env_file() -> Result<PathBuf> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "twalk-h25-loop-{}-{unique}.env",
        std::process::id()
    ));
    let owner = owner_user_id();
    let (gateway_image, sensor_image) = (gateway_image(), sensor_image());
    let (synapse_port, gateway_port, nats_port) = (synapse_port(), gateway_port(), nats_port());
    let contents = format!(
        "MATRIX_DOMAIN={DEPLOY_SERVER_NAME}\n\
         MATRIX_HTTP_PORT={synapse_port}\n\
         MATRIX_REGISTRATION_SHARED_SECRET={REGISTRATION_SHARED_SECRET}\n\
         MATRIX_MACAROON_SECRET=loop-test-only-macaroon-secret\n\
         MATRIX_FORM_SECRET=loop-test-only-form-secret\n\
         SENSOR_USER_ID=@sensor:{DEPLOY_SERVER_NAME}\n\
         SENSOR_PASSWORD=loop-test-only-password-sensor\n\
         SENSOR_ALLOWED_INVITERS={owner}\n\
         SENSOR_STATE_DIR=/data\n\
         SENSOR_LOG_LEVEL=info\n\
         NATS_PORT={nats_port}\n\
         GATEWAY_HTTP_PORT={gateway_port}\n\
         GATEWAY_FALLBACK_FILE=200.html\n\
         GATEWAY_LOG_LEVEL=info,twalk_companion_gateway=debug\n\
         GATEWAY_OWNER={owner}\n\
         GATEWAY_STATE_DIR=/data\n\
         GATEWAY_REGISTRATION_SHARED_SECRET={REGISTRATION_SHARED_SECRET}\n\
         GATEWAY_SENSOR_USER_ID=@sensor:{DEPLOY_SERVER_NAME}\n\
         GATEWAY_NATS_URL=nats://nats:4222\n\
         GATEWAY_SERVICE_TOKEN={SERVICE_TOKEN}\n\
         TWALK_GATEWAY_IMAGE={gateway_image}\n\
         TWALK_SENSOR_IMAGE={sensor_image}\n"
    );
    std::fs::write(&path, contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// The owner of this deployment, as an approval is stamped with it.
pub fn owner() -> String {
    owner_user_id()
}
