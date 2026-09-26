//! The clerk's write half on the reference deployment (ticket #284,
//! ADR 0036): the deployed Companion Gateway, the clerk's own compose
//! service really started from `deploy/docker-compose/compose.yaml`, and
//! the operator's own `provision-clerk-device.sh` — the three things
//! `decisions.rs` stands in for with a stub Gateway and a process the suite
//! starts by hand.
//!
//! What only this test can show is the seam the stub cannot: that the
//! session the script writes is one the **real** Gateway issued and keeps
//! alive, that the container the operator gets from `docker compose up`
//! holds no credential in its environment and still approves, and that a
//! device revoked from the dashboard is answered in the thread and restored
//! by running the script again — as root this time, because the container
//! has taken the session directory over, which is the route the script's
//! own message names. And (#300) that the delivery line a post carries is
//! what the **real** `GET /api/suggestions/{id}` answers for that
//! suggestion — asserted as equality with the Gateway's own answer, never
//! as a literal, because on this stack no bridge runs and what the
//! Gateway says of a room no bridge marked is the Gateway's to say — read
//! once per suggestion and not again across a restart.
//!
//! # What is real and what is not
//!
//! Real: Synapse, the bus, the Gateway image and the clerk image built from
//! this checkout, the compose file an operator runs, the script, the Buzz
//! relay (the clerk suite's own, `clerk/tests/compose.relay.yaml`, which the
//! required suite already keeps up), and the owner — an account the
//! Gateway's registration relay creates, whose OpenID token signs the test's
//! own device in and whose password the script reads from a file.
//!
//! Stood in for: the persona and the Sensor. The suggestion is published by
//! the test, against a message the test publishes, because the clerk's seam
//! is the bus and a contract-valid event is what crosses it; the Sensor
//! does not run, so the approved reply is on the bus and posted nowhere,
//! which is also why nothing reaches `journal` here. `hermes/tests/full_loop.rs`
//! is where the whole loop closes.
//!
//! # The stack, its ports and its teardown
//!
//! Its own compose project, `TWALK_CLERK_DEPLOY_TEST_STACK` (default
//! `twalk-clerk-deploy`), on `TWALK_CLERK_DEPLOY_TEST_SYNAPSE_PORT` (18380),
//! `TWALK_CLERK_DEPLOY_TEST_NATS_PORT` (18381),
//! `TWALK_CLERK_DEPLOY_TEST_GATEWAY_PORT` (18382) and
//! `TWALK_CLERK_DEPLOY_TEST_CLERK_PORT` (18383) — the last one a host port,
//! because the clerk runs in the host's network namespace and 8084 is the
//! reference deployment's own. The relay is the clerk suite's
//! (`TWALK_CLERK_TEST_STACK`, `TWALK_CLERK_TEST_RELAY_PORT`), reached by
//! the URL it announces. Two images are built and tagged per stack
//! (`twalk/companion-gateway:<stack>`, `twalk/clerk:<stack>`).
//!
//! **Torn down at the end of every run, passing or failing** — containers,
//! volumes, network, the two images, the run's directory — because this host
//! has filled its disk doing less (#128); what a failure needs is attached
//! to the failure. `TWALK_CLERK_DEPLOY_TEST_KEEP=1` keeps it all. The two
//! scenarios share one stack and one test, in order, because the second
//! revokes the device the first provisioned (and because a binary's tests
//! run in parallel and there is no "after the last one" otherwise, #199).
//!
//! The credentials below are throwaway constants for this ephemeral stack,
//! in the same category as the test bots' passwords.

mod harness;

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures::FutureExt;
use harness::{
    contract_fixture, reaction, reader_in, reference_names, seed, sha256_hex,
    validate_against_contract, wait_on_relay, wait_on_relay_for, Bus, Channels, RelayStack,
    TEST_OWNER_PUBKEY_HEX,
};
use nostr::{Event, Keys};
use serde_json::{json, Value};
use tokio::process::Command;
use twalk_clerk::gateway::Delivery;
use twalk_clerk::refusals::{delivery_line, delivery_unread_line, Unread};
use twalk_clerk::text::{activity_approved, thread_revoked, Lang};

/// The Matrix server name this deployment answers for: not the test
/// stack's `test.twalk` nor the other deployment suites' names, so an event
/// from the wrong stack is obvious rather than plausible.
const DEPLOY_SERVER_NAME: &str = "clerk.twalk";

const OWNER_LOCALPART: &str = "owner";
const OWNER_PASSWORD: &str = "clerk-deploy-test-only-password-owner";
const REGISTRATION_SHARED_SECRET: &str = "clerk-deploy-test-only-registration-shared-secret";
const MACAROON_SECRET: &str = "clerk-deploy-test-only-macaroon-secret";
const FORM_SECRET: &str = "clerk-deploy-test-only-form-secret";
const SENSOR_PASSWORD: &str = "clerk-deploy-test-only-password-sensor";

/// The deployment's bus namespace and stream: `twalk`, as in every
/// deployment, because this *is* one and it is torn down whole.
const STREAM: &str = "twalk";
const INBOUND_SUBJECT: &str = "twalk.inbound.message.received.v1";
const SUGGEST_SUBJECT: &str = "twalk.persona.suggest.produced.v1";
const APPROVED_SUBJECT: &str = "twalk.persona.reply.approved.v1";

/// The network every conversation here is on: the fixture's, and the one
/// the clerk's `activite` line names.
const NETWORK: &str = "whatsapp";

/// The deployment's language, `CLERK_USER_LANGUAGE` in its `.env`: the
/// sentences asserted are the clerk's own for it.
const LANG: Lang = Lang::Fr;

/// How long a suggestion lives here: well past every wait in this file,
/// so no post ever nears the loop's "not recorded" window while a scenario
/// is still asserting on it.
const LIFE_SECONDS: i64 = 3600;

/// How long after the owner's ✅ its post may still stand when the Gateway
/// answers at once: ticks to read the gesture, the approval's round trip,
/// the delete, the relay's margin — and here the Gateway is a container
/// rather than a stub. Twenty seconds is twenty ticks.
const GONE_WITHIN: Duration = Duration::from_secs(20);

fn stack() -> String {
    std::env::var("TWALK_CLERK_DEPLOY_TEST_STACK")
        .unwrap_or_else(|_| "twalk-clerk-deploy".to_owned())
}

fn synapse_port() -> String {
    std::env::var("TWALK_CLERK_DEPLOY_TEST_SYNAPSE_PORT").unwrap_or_else(|_| "18380".to_owned())
}

fn nats_port() -> String {
    std::env::var("TWALK_CLERK_DEPLOY_TEST_NATS_PORT").unwrap_or_else(|_| "18381".to_owned())
}

fn gateway_port() -> String {
    std::env::var("TWALK_CLERK_DEPLOY_TEST_GATEWAY_PORT").unwrap_or_else(|_| "18382".to_owned())
}

fn clerk_port() -> String {
    std::env::var("TWALK_CLERK_DEPLOY_TEST_CLERK_PORT").unwrap_or_else(|_| "18383".to_owned())
}

fn keep_requested() -> bool {
    matches!(
        std::env::var("TWALK_CLERK_DEPLOY_TEST_KEEP").as_deref(),
        Ok("1") | Ok("true")
    )
}

fn gateway_image() -> String {
    format!("twalk/companion-gateway:{}", stack())
}

fn clerk_image() -> String {
    format!("twalk/clerk:{}", stack())
}

/// The clerk's container, named as compose names it and as the brief
/// reads it: `docker inspect <stack>-clerk-1`.
fn clerk_container() -> String {
    format!("{}-clerk-1", stack())
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("the clerk package sits inside the repository")
}

fn deploy_dir() -> PathBuf {
    repository_root().join("deploy").join("docker-compose")
}

fn compose_file() -> PathBuf {
    deploy_dir().join("compose.yaml")
}

fn provision_script() -> PathBuf {
    deploy_dir().join("provision-clerk-device.sh")
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

fn clerk_url() -> String {
    format!("http://127.0.0.1:{}", clerk_port())
}

/// `poll_until` with the patience a cold deployment needs — minutes of
/// container start rather than the twenty seconds a process-boundary test
/// waits for a process that is already up.
async fn poll_deploy<T, Fut>(mut attempt: impl FnMut() -> Fut, description: &str) -> Result<T>
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

// ---------------------------------------------------------------------------
// The events a scenario publishes
// ---------------------------------------------------------------------------

/// A contact of this run's own, on a portal room of this run's own, with
/// the message a persona answered and the suggestion it produced: what a
/// scenario stages on the deployment's bus.
struct Conversation {
    contact: String,
    trigger: Value,
    suggestion: Value,
}

impl Conversation {
    fn suggestion_id(&self) -> &str {
        self.suggestion["id"]
            .as_str()
            .expect("a suggestion has an id")
    }

    fn suggestion_body(&self) -> &str {
        self.suggestion["data"]["suggestion"]["body"]
            .as_str()
            .expect("a suggestion has a body")
    }
}

fn now_rfc3339() -> String {
    in_seconds(0)
}

fn in_seconds(seconds: i64) -> String {
    (time::OffsetDateTime::now_utc() + time::Duration::seconds(seconds))
        .replace_nanosecond(0)
        .expect("zero is a nanosecond")
        .format(&time::format_description::well_known::Rfc3339)
        .expect("UTC formats as RFC 3339")
}

/// One conversation, from the contract's own fixtures: the inbound message
/// rewritten to a ghost and a room of this run's own on this deployment's
/// homeserver, and the suggestion rewritten to answer it, expiring
/// [`LIFE_SECONDS`] from now. Everything the fixture says about the
/// contact — the display name, the `network_identifier`, the body — stays,
/// because those are the markers the post is searched for.
fn conversation(label: &str) -> Result<Conversation> {
    let unique = format!(
        "{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos()
    );
    let contact = format!("@whatsapp_{unique}:{DEPLOY_SERVER_NAME}");
    let room: String = sha256_hex(&unique).chars().take(18).collect();

    let mut trigger = contract_fixture("inbound.message.received")?;
    trigger["id"] = json!(sha256_hex(&format!("c284-inbound:{unique}")));
    trigger["time"] = json!(now_rfc3339());
    trigger["subject"] = json!(contact);
    trigger["source"] = json!(format!(
        "matrix://{DEPLOY_SERVER_NAME}/!{room}:{DEPLOY_SERVER_NAME}"
    ));
    validate_against_contract(&trigger, "inbound.message.received")?;

    let trigger_id = trigger["id"].as_str().unwrap().to_owned();
    let mut suggestion = contract_fixture("persona.suggest.produced")?;
    suggestion["id"] = json!(sha256_hex(&format!("assistant:{trigger_id}:1")));
    suggestion["time"] = json!(now_rfc3339());
    suggestion["subject"] = json!(trigger_id);
    suggestion["source"] = json!(format!("hermes://{DEPLOY_SERVER_NAME}/personas/assistant"));
    suggestion["data"]["trigger"]["event_id"] = json!(trigger_id);
    suggestion["data"]["suggestion"]["body"] =
        json!(format!("Pas de problème, à 20h ! [{unique}]"));
    suggestion["data"]["expires_at"] = json!(in_seconds(LIFE_SECONDS));
    validate_against_contract(&suggestion, "persona.suggest.produced")?;

    Ok(Conversation {
        contact,
        trigger,
        suggestion,
    })
}

// ---------------------------------------------------------------------------
// The deployment
// ---------------------------------------------------------------------------

/// The reference deployment from the clerk's side, and the relay it writes
/// to.
struct Deployment {
    bus: Bus,
    relay: RelayStack,
    channels: Channels,
    /// The deployed clerk's public key: what its posts and thread answers
    /// are signed with.
    clerk_pubkey: String,
    /// The key this run reads the relay with (a member of the three
    /// channels and nothing else), so the reads draw on a quota of their
    /// own and not on the owner's, which the seed and every ✅ need.
    reader: Keys,
    /// The owner's Matrix session, for the OpenID token the test's own
    /// device signs in with.
    owner_token: String,
    /// The test's own device at the Gateway — "the clerk deployment test",
    /// beside the `Buzz` the script signs in: what grants the contact,
    /// reads the approval table and the device list, and revokes `Buzz`
    /// the way the dashboard does.
    device_token: String,
    client: reqwest::Client,
    /// The run's directory: the clerk's key, the owner's password file, the
    /// generated `.env`, and the session directory the script creates and
    /// the container takes over.
    dir: PathBuf,
    env_file: PathBuf,
    stopped: bool,
}

impl Deployment {
    /// Brings the deployment up to the point the scenarios start from: the
    /// relay seeded for a clerk of this run's own, the two images built,
    /// Synapse and the Gateway up, the owner's account created, a device of
    /// the test's own signed in. The clerk is **not** started here: the
    /// first scenario provisions its device and starts it, in the
    /// operator's order.
    async fn start() -> Result<Self> {
        let relay = RelayStack::ensure().await?;
        let run = harness::run_id("deploy");
        let dir = std::env::temp_dir().join(&run);
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
        let (key_file, clerk_pubkey, channels) = seed(&relay, &run, &dir).await?;
        let reader = reader_in(&relay, &channels).await?;
        let env_file = write_env_file(&dir, &key_file, &channels, &relay.url)?;
        write_secret_file(&dir.join("owner-password"), OWNER_PASSWORD)?;

        // Build first, so a build failure is attributed to the build; both
        // images now, so the clerk's build is not in the middle of a
        // scenario. Their layers are the daemon's shared cache.
        compose_with(&env_file, &["build", "companion-gateway", "clerk"], "build").await?;
        // Synapse is named: the Gateway deliberately does not depend on it,
        // and the clerk depends on the bus alone.
        compose_with(
            &env_file,
            &["up", "-d", "--wait", "synapse", "companion-gateway"],
            "up synapse companion-gateway",
        )
        .await?;

        let bus = Bus::connect_to(&format!("nats://localhost:{}", nats_port())).await?;
        bus.ensure_stream(STREAM, &["twalk.>"]).await?;
        let mut deployment = Self {
            bus,
            relay,
            channels,
            clerk_pubkey,
            reader,
            owner_token: String::new(),
            device_token: String::new(),
            client: reqwest::Client::new(),
            dir,
            env_file,
            stopped: false,
        };
        deployment.owner_token = deployment.register_owner().await?;
        deployment.device_token = deployment.sign_in().await?;
        Ok(deployment)
    }

    fn session_dir(&self) -> PathBuf {
        self.dir.join("session")
    }

    fn password_file(&self) -> PathBuf {
        self.dir.join("owner-password")
    }

    async fn compose(&self, args: &[&str], what: &str) -> Result<String> {
        compose_with(&self.env_file, args, what).await
    }

    // -- the owner, and the test's own device ------------------------------

    /// Creates the deployment's one account through the Gateway's
    /// registration relay and answers the owner's Matrix token. A kept
    /// stack already has the account and the relay refuses a second, so it
    /// logs in instead. Polled for a verdict, because a Gateway up before
    /// its homeserver answers the relay's call with a 5xx.
    async fn register_owner(&self) -> Result<String> {
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

    /// Signs the test's own device in at the Gateway with a Matrix OpenID
    /// token, as the Companion does, and answers the device cookie.
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
                "device_name": "the clerk deployment test",
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

    fn cookie(&self) -> String {
        format!("twalk_device={}", self.device_token)
    }

    async fn get(&self, path: &str) -> Result<(reqwest::StatusCode, Value)> {
        let response = self
            .client
            .get(format!("{}{path}", gateway_url()))
            .header(reqwest::header::COOKIE, self.cookie())
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        let status = response.status();
        let text = response.text().await?;
        let body = serde_json::from_str(&text)
            .with_context(|| format!("GET {path} answered {status} with non-JSON: {text}"))?;
        Ok((status, body))
    }

    /// Grants a contact on [`NETWORK`] through the Gateway's write API —
    /// the only writer of consent state — as the user does from the
    /// consent screen.
    async fn grant(&self, contact: &str) -> Result<()> {
        let response = self
            .client
            .post(format!("{}/api/consent/decisions", gateway_url()))
            .header(reqwest::header::COOKIE, self.cookie())
            .json(&json!({
                "subject": { "type": "contact", "id": contact },
                "new_state": "granted",
                "scope": { "networks": [NETWORK] },
            }))
            .send()
            .await?;
        anyhow::ensure!(
            response.status() == reqwest::StatusCode::CREATED,
            "the decision about {contact} must be recorded: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
        Ok(())
    }

    /// The Gateway's device list, as the dashboard reads it.
    async fn devices(&self) -> Result<Vec<Value>> {
        let (status, body) = self.get("/api/devices").await?;
        anyhow::ensure!(
            status == reqwest::StatusCode::OK,
            "GET /api/devices answered {status}: {body}"
        );
        body["devices"]
            .as_array()
            .cloned()
            .context("the device list has a devices array")
    }

    /// The unrevoked devices named `Buzz` — what the dashboard shows as
    /// signed in under that name. The ticket's criterion is that there is
    /// exactly one.
    async fn unrevoked_buzz(&self) -> Result<Vec<Value>> {
        Ok(self
            .devices()
            .await?
            .into_iter()
            .filter(|device| {
                device["name"].as_str() == Some("Buzz") && device["revoked_unix_seconds"].is_null()
            })
            .collect())
    }

    /// Revokes a device, as the dashboard's button does.
    async fn revoke(&self, device_id: &str) -> Result<()> {
        let response = self
            .client
            .delete(format!("{}/api/devices/{device_id}", gateway_url()))
            .header(reqwest::header::COOKIE, self.cookie())
            .send()
            .await?;
        anyhow::ensure!(
            response.status() == reqwest::StatusCode::NO_CONTENT,
            "revoking the device {device_id} must answer 204: {} {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
        Ok(())
    }

    /// `GET /api/approvals/{id}`: the Gateway's own record of whether a
    /// suggestion was approved, status and document, because a `404` is
    /// as much an answer as a `200` here.
    async fn approval(&self, suggestion_id: &str) -> Result<(reqwest::StatusCode, Value)> {
        self.get(&format!("/api/approvals/{suggestion_id}")).await
    }

    /// `GET /api/suggestions/{id}` with the test's own device — the read
    /// the clerk makes as its device before a post (#300), made here by a
    /// second device of the same owner so that the post can be compared
    /// with what the Gateway itself says.
    async fn suggestion(&self, suggestion_id: &str) -> Result<Value> {
        let (status, body) = self
            .get(&format!("/api/suggestions/{suggestion_id}"))
            .await?;
        anyhow::ensure!(
            status == reqwest::StatusCode::OK,
            "GET /api/suggestions/{suggestion_id} answered {status}: {body}"
        );
        Ok(body)
    }

    /// The `delivery` the Gateway answers for a suggestion, as the clerk
    /// deserialises it.
    async fn delivery_of(&self, suggestion_id: &str) -> Result<Delivery> {
        let answer = self.suggestion(suggestion_id).await?;
        serde_json::from_value(answer["delivery"].clone())
            .with_context(|| format!("the answer carries a delivery: {answer}"))
    }

    /// Polls the approval table until it holds a row for this suggestion.
    async fn wait_for_approval(&self, suggestion_id: &str) -> Result<Value> {
        let found = wait_on_relay(
            || async {
                let (status, body) = self.approval(suggestion_id).await.ok()?;
                (status == reqwest::StatusCode::OK).then_some(body)
            },
            &format!("the deployed Gateway to record an approval of {suggestion_id}"),
        )
        .await;
        match found {
            Ok(body) => Ok(body),
            Err(error) => bail!("{error}; {}", self.diagnostics().await),
        }
    }

    // -- the bus -----------------------------------------------------------

    /// Grants the contact, then publishes the message and the suggestion
    /// onto the deployment's bus, in that order: the Gateway looks the
    /// trigger up when the clerk approves, and the clerk's suggestions
    /// consumer delivers from `New`, so the clerk must be running before
    /// this is called.
    async fn stage(&self, label: &str) -> Result<Conversation> {
        let talk = conversation(label)?;
        self.grant(&talk.contact).await?;
        self.bus
            .publish_event(INBOUND_SUBJECT, &talk.trigger)
            .await?;
        self.bus
            .publish_event(SUGGEST_SUBJECT, &talk.suggestion)
            .await?;
        Ok(talk)
    }

    /// The `persona.reply.approved.v1` with this id on the deployment's
    /// bus, once it is there.
    async fn wait_for_published(&self, event_id: &str) -> Result<Value> {
        let found = wait_on_relay(
            || async {
                self.bus
                    .fetch_all(STREAM, APPROVED_SUBJECT)
                    .await
                    .ok()?
                    .into_iter()
                    .find(|event| event["id"].as_str() == Some(event_id))
            },
            &format!("the approval {event_id} to reach the deployment's bus"),
        )
        .await;
        match found {
            Ok(event) => Ok(event),
            Err(error) => bail!("{error}; {}", self.diagnostics().await),
        }
    }

    // -- the relay ---------------------------------------------------------

    async fn read(&self, filters: Value) -> Result<Vec<Event>> {
        self.relay.query_as(&self.reader, filters).await
    }

    /// The forum posts in `approbations` whose reference line names the
    /// suggestion `id`.
    async fn posts_about(&self, id: &str) -> Result<Vec<Event>> {
        let posts = self
            .read(json!([{ "kinds": [45001], "#h": [self.channels.approvals], "limit": 1000 }]))
            .await?;
        Ok(posts
            .into_iter()
            .filter(|post| reference_names(&post.content, id))
            .collect())
    }

    /// Polls `approbations` until the deployed clerk has posted about the
    /// suggestion `id`.
    async fn wait_for_post(&self, id: &str) -> Result<Event> {
        let posts = wait_on_relay(
            || async {
                let posts = self.posts_about(id).await.ok()?;
                (!posts.is_empty()).then_some(posts)
            },
            &format!("a post about suggestion {id} in approbations"),
        )
        .await;
        match posts {
            Ok(mut posts) => Ok(posts.remove(0)),
            Err(error) => bail!("{error}; {}", self.diagnostics().await),
        }
    }

    async fn wait_until_gone(&self, id: &str) -> Result<()> {
        let gone = wait_on_relay_for(
            || async { self.posts_about(id).await.ok()?.is_empty().then_some(()) },
            &format!("the post about suggestion {id} to be gone from approbations"),
            GONE_WITHIN,
        )
        .await;
        match gone {
            Ok(()) => Ok(()),
            Err(error) => bail!("{error}; {}", self.diagnostics().await),
        }
    }

    /// The owner reacts `emoji` to the post — signed by the relay owner's
    /// key, the one the deployment's `CLERK_OWNER_PUBKEY` names.
    async fn react_as_owner(&self, post_id: &str, emoji: &str) -> Result<Event> {
        let event = reaction(&self.relay.owner, post_id, emoji)?;
        self.relay.submit(&event).await?;
        Ok(event)
    }

    /// The deployed clerk's own replies in the thread of `post_id`,
    /// newest first.
    async fn clerk_thread_of(&self, post_id: &str) -> Result<Vec<Event>> {
        Ok(self
            .read(json!([{ "kinds": [45003], "#e": [post_id], "limit": 1000 }]))
            .await?
            .into_iter()
            .filter(|reply| reply.pubkey.to_hex() == self.clerk_pubkey)
            .collect())
    }

    async fn wait_for_thread_line(&self, post_id: &str, needle: &str) -> Result<Event> {
        let line = wait_on_relay(
            || async {
                self.clerk_thread_of(post_id)
                    .await
                    .ok()?
                    .into_iter()
                    .rev()
                    .find(|reply| reply.content.contains(needle))
            },
            &format!("a reply by the clerk containing {needle:?} in the thread of {post_id}"),
        )
        .await;
        match line {
            Ok(line) => Ok(line),
            Err(error) => bail!("{error}; {}", self.diagnostics().await),
        }
    }

    /// Polls `activite` until a line containing `needle` is there.
    async fn wait_for_activity_line(&self, needle: &str) -> Result<Event> {
        let line = wait_on_relay(
            || async {
                self.read(json!([{ "kinds": [9], "#h": [self.channels.activity], "limit": 1000 }]))
                    .await
                    .ok()?
                    .into_iter()
                    .rev()
                    .find(|line| line.content.contains(needle))
            },
            &format!("a line containing {needle:?} in activite"),
        )
        .await;
        match line {
            Ok(line) => Ok(line),
            Err(error) => bail!("{error}; {}", self.diagnostics().await),
        }
    }

    // -- the script, and the clerk's container ------------------------------

    /// Runs `provision-clerk-device.sh` the way an operator does, against
    /// this run's `.env` (`TWALK_ENV_FILE`) with the owner's password in a
    /// file (`TWALK_OWNER_PASSWORD_FILE`), and answers its stdout.
    ///
    /// The first time, the session directory does not exist and the
    /// script creates it as the operator's own account. Once the clerk has
    /// started with it mounted, the container owns it under its own
    /// unprivileged account, and the script's own message says to come
    /// back with `sudo --preserve-env=TWALK_OWNER_PASSWORD_FILE`: that is
    /// what this does then, non-interactively (`sudo -n`), and a host that
    /// cannot is a failure naming the route rather than a test that
    /// silently took another.
    async fn provision_device(&self) -> Result<String> {
        let session_dir = self.session_dir();
        let taken_over = session_dir.exists() && !is_writable(&session_dir);
        let mut command = if taken_over {
            let probe = Command::new("sudo")
                .args(["-n", "true"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
            if !matches!(probe, Ok(status) if status.success()) {
                bail!(
                    "{} is owned by the clerk's container now, so re-running \
                     provision-clerk-device.sh into it is the script's documented sudo route — \
                     and this host has no passwordless sudo for it (`sudo -n true` failed)",
                    session_dir.display()
                );
            }
            let mut command = Command::new("sudo");
            command.args([
                "-n",
                "--preserve-env=TWALK_OWNER_PASSWORD_FILE,TWALK_ENV_FILE",
                provision_script().to_str().expect("a UTF-8 path"),
            ]);
            command
        } else {
            Command::new(provision_script())
        };
        let output = command
            .env("TWALK_ENV_FILE", &self.env_file)
            .env("TWALK_OWNER_PASSWORD_FILE", self.password_file())
            .stdin(Stdio::null())
            .output()
            .await
            .context("failed to run provision-clerk-device.sh")?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::ensure!(
            output.status.success(),
            "provision-clerk-device.sh failed with {}{}:\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status,
            if taken_over { " (under sudo)" } else { "" }
        );
        // The script prints no token, by its own contract: whatever the
        // Gateway issued went from the response into the file — which is
        // what makes its report safe to echo into this run's log.
        for (what, secret) in [
            ("the owner's password", OWNER_PASSWORD),
            ("the owner's Matrix token", self.owner_token.as_str()),
        ] {
            anyhow::ensure!(
                !stdout.contains(secret) && !stderr.contains(secret),
                "provision-clerk-device.sh printed {what}"
            );
        }
        eprintln!(
            "    provision-clerk-device.sh{} said:\n{}",
            if taken_over { " (under sudo)" } else { "" },
            stdout
                .lines()
                .map(|line| format!("      | {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        Ok(stdout)
    }

    /// Starts the deployment's own `clerk` service, as an operator does,
    /// and waits for the binary to say it is running and its decisions loop
    /// is too.
    async fn start_clerk(&self) -> Result<()> {
        self.compose(&["up", "-d", "--wait", "clerk"], "up clerk")
            .await?;
        self.wait_for_clerk_log("decisions loop running", 1).await
    }

    /// `docker compose restart clerk`: what the script tells an operator to
    /// run after signing the device in again. The container's log keeps
    /// the previous run's lines, so the line is waited for a second time.
    async fn restart_clerk(&self) -> Result<()> {
        self.compose(&["restart", "clerk"], "restart clerk").await?;
        self.wait_for_clerk_log("decisions loop running", 2).await
    }

    async fn clerk_logs(&self) -> String {
        self.compose(
            &["logs", "--no-color", "--no-log-prefix", "clerk"],
            "logs clerk",
        )
        .await
        .unwrap_or_else(|error| format!("(the clerk's logs could not be read: {error})"))
    }

    async fn wait_for_clerk_log(&self, needle: &str, at_least: usize) -> Result<()> {
        let found = poll_deploy(
            || async {
                (self.clerk_logs().await.matches(needle).count() >= at_least).then_some(())
            },
            &format!("{at_least} clerk log line(s) containing {needle:?}"),
        )
        .await;
        match found {
            Ok(()) => Ok(()),
            Err(error) => bail!("{error}; {}", self.diagnostics().await),
        }
    }

    /// The clerk container's environment, as `docker inspect` reports it:
    /// one `NAME=value` per entry.
    async fn clerk_environment(&self) -> Result<Vec<String>> {
        let output = Command::new("docker")
            .args([
                "inspect",
                "--format",
                "{{json .Config.Env}}",
                &clerk_container(),
            ])
            .output()
            .await
            .context("failed to run docker inspect")?;
        anyhow::ensure!(
            output.status.success(),
            "docker inspect {} failed: {}",
            clerk_container(),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).context("docker inspect's Env is a JSON array")
    }

    async fn clerk_container_state(&self) -> Result<String> {
        Ok(self
            .compose(&["ps", "--format", "{{.Service}} {{.State}}"], "ps")
            .await?
            .lines()
            .find(|line| line.starts_with("clerk "))
            .map(|line| line["clerk ".len()..].trim().to_owned())
            .unwrap_or_default())
    }

    /// The clerk's `/metrics`, from the port this stack's `.env` gives it.
    async fn clerk_metrics(&self) -> Result<String> {
        Ok(self
            .client
            .get(format!("{}/metrics", clerk_url()))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?)
    }

    async fn wait_for_metric(&self, sample: &str) -> Result<()> {
        let found = wait_on_relay(
            || async {
                let metrics = self.clerk_metrics().await.ok()?;
                metrics.lines().any(|line| line == sample).then_some(())
            },
            &format!("the metrics line {sample:?}"),
        )
        .await;
        match found {
            Ok(()) => Ok(()),
            Err(error) => bail!(
                "{error}; /metrics was:\n{}",
                self.clerk_metrics().await.unwrap_or_default()
            ),
        }
    }

    /// What a failure needs: the deployment's logs, attached to the failure
    /// because the stack is gone before anybody reads it.
    async fn diagnostics(&self) -> String {
        format!(
            "the deployment's logs were:\n{}",
            self.compose(&["logs", "--no-color", "--tail", "80"], "logs")
                .await
                .unwrap_or_else(|error| format!("(could not be read: {error})"))
        )
    }

    // -- teardown ----------------------------------------------------------

    async fn shutdown(mut self) -> Result<()> {
        self.stopped = true;
        if keep_requested() {
            eprintln!(
                "TWALK_CLERK_DEPLOY_TEST_KEEP is set: the stack {} and {} stay",
                stack(),
                self.dir.display()
            );
            return Ok(());
        }
        self.compose(&["down", "-v", "--remove-orphans"], "down")
            .await?;
        give_back_the_session_dir(&self.dir);
        for image in [gateway_image(), clerk_image()] {
            let output = Command::new("docker")
                .args(["image", "rm", &image])
                .output()
                .await
                .context("failed to run docker image rm")?;
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !output.status.success() && !stderr.contains("No such image") {
                bail!(
                    "docker image rm {image} failed with {}:\n{stderr}",
                    output.status
                );
            }
        }
        std::fs::remove_dir_all(&self.dir)
            .with_context(|| format!("removing {}", self.dir.display()))?;
        Ok(())
    }
}

impl Drop for Deployment {
    /// A test that failed never reached `shutdown`, and a stack that
    /// outlives its run is a defect (#128): the teardown happens here too,
    /// blocking, with the same steps.
    fn drop(&mut self) {
        if self.stopped || keep_requested() {
            return;
        }
        let _ = std::process::Command::new("docker")
            .args(compose_argv(
                &self.env_file,
                &["down", "-v", "--remove-orphans"],
            ))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        give_back_the_session_dir(&self.dir);
        for image in [gateway_image(), clerk_image()] {
            let _ = std::process::Command::new("docker")
                .args(["image", "rm", &image])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The session directory is the clerk container's once it has started with
/// it mounted (`clerk-entrypoint.sh` hands it to the image's `clerk`
/// account), so the test's own account can neither list nor remove it.
/// The account that took it over gives it back: a throwaway container from
/// this stack's clerk image, as root, removes it — before the image goes.
/// Tolerant of everything, because it runs on the way out.
fn give_back_the_session_dir(dir: &Path) {
    let _ = std::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "--entrypoint",
            "rm",
            "-v",
            &format!("{}:/mnt", dir.display()),
            &clerk_image(),
            "-rf",
            "/mnt/session",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn is_writable(path: &Path) -> bool {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path.join(".c284-writable-probe"))
        .map(|_| {
            let _ = std::fs::remove_file(path.join(".c284-writable-probe"));
        })
        .is_ok()
}

fn compose_argv(env_file: &Path, args: &[&str]) -> Vec<String> {
    let mut argv = vec![
        "compose".to_owned(),
        "-p".to_owned(),
        stack(),
        "--env-file".to_owned(),
        env_file.to_string_lossy().into_owned(),
        "-f".to_owned(),
        compose_file().to_string_lossy().into_owned(),
    ];
    argv.extend(args.iter().map(|arg| (*arg).to_owned()));
    argv
}

/// Runs `docker compose` against this stack with the run's environment
/// file. A failure the host caused is named as one (#128).
async fn compose_with(env_file: &Path, args: &[&str], what: &str) -> Result<String> {
    let output = Command::new("docker")
        .args(compose_argv(env_file, args))
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

/// A file only its owner can read, created with that mode: the owner's
/// password, for `TWALK_OWNER_PASSWORD_FILE`.
fn write_secret_file(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

/// The environment file this deployment is configured through: what
/// `.env.example` documents, with throwaway values, this run's own ports
/// and images, and the clerk's whole configuration — the relay the clerk
/// suite keeps up, the key and the three channels seeded for this run, the
/// harness owner's key as `CLERK_OWNER_PUBKEY`, and a session directory
/// inside the run's directory. `CLERK_GATEWAY_URL` is deliberately left
/// out: the script and the compose file both derive it from
/// `GATEWAY_HTTP_PORT`, and the test is about their agreeing.
///
/// The Sensor's three required variables are here because compose
/// interpolates the whole file whatever is started; the Sensor itself is
/// never started.
fn write_env_file(
    dir: &Path,
    key_file: &Path,
    channels: &Channels,
    relay_url: &str,
) -> Result<PathBuf> {
    let path = dir.join("deployment.env");
    let owner = owner_user_id();
    let (synapse_port, nats_port) = (synapse_port(), nats_port());
    let (gateway_port, clerk_port) = (gateway_port(), clerk_port());
    let (gateway_image, clerk_image) = (gateway_image(), clerk_image());
    let key_file = key_file.display();
    // The registry of connections (#269, #270): a Gateway derives one per
    // bridge, and this deployment runs no bridge, so the one network the
    // scenarios decide on is declared, named after itself — the id every
    // existing decision was migrated onto, and the Gateway suite's own shape.
    let session_dir = dir.join("session");
    let session_dir = session_dir.display();
    let contents = format!(
        "MATRIX_DOMAIN={DEPLOY_SERVER_NAME}\n\
         MATRIX_HTTP_PORT={synapse_port}\n\
         MATRIX_REGISTRATION_SHARED_SECRET={REGISTRATION_SHARED_SECRET}\n\
         MATRIX_MACAROON_SECRET={MACAROON_SECRET}\n\
         MATRIX_FORM_SECRET={FORM_SECRET}\n\
         SENSOR_USER_ID=@sensor:{DEPLOY_SERVER_NAME}\n\
         SENSOR_PASSWORD={SENSOR_PASSWORD}\n\
         SENSOR_ALLOWED_INVITERS={owner}\n\
         SENSOR_STATE_DIR=/data\n\
         NATS_PORT={nats_port}\n\
         GATEWAY_HTTP_PORT={gateway_port}\n\
         GATEWAY_FALLBACK_FILE=200.html\n\
         GATEWAY_LOG_LEVEL=info,twalk_companion_gateway=debug\n\
         GATEWAY_OWNER={owner}\n\
         GATEWAY_STATE_DIR=/data\n\
         GATEWAY_REGISTRATION_SHARED_SECRET={REGISTRATION_SHARED_SECRET}\n\
         GATEWAY_SENSOR_USER_ID=@sensor:{DEPLOY_SERVER_NAME}\n\
         GATEWAY_CONNECTIONS={NETWORK}={NETWORK}\n\
         CLERK_RELAY_URL={relay_url}\n\
         CLERK_NOSTR_KEY_FILE={key_file}\n\
         CLERK_CHANNEL_APPROVALS={}\n\
         CLERK_CHANNEL_ACTIVITY={}\n\
         CLERK_CHANNEL_JOURNAL={}\n\
         CLERK_USER_LANGUAGE=fr\n\
         CLERK_OWNER_PUBKEY={TEST_OWNER_PUBKEY_HEX}\n\
         CLERK_GATEWAY_SESSION_DIR={session_dir}\n\
         CLERK_DECISION_SECONDS=1\n\
         CLERK_LISTEN_PORT={clerk_port}\n\
         CLERK_LOG_LEVEL=info\n\
         TWALK_GATEWAY_IMAGE={gateway_image}\n\
         TWALK_CLERK_IMAGE={clerk_image}\n",
        channels.approvals, channels.activity, channels.journal
    );
    std::fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// The scenarios
// ---------------------------------------------------------------------------

/// Runs one scenario and turns a panicked assertion into an error with the
/// deployment's logs attached, so the stack can go and the failure still
/// says what it saw.
async fn scenario<Fut>(stack: &Deployment, what: &str, body: Fut) -> Result<()>
where
    Fut: std::future::Future<Output = Result<()>>,
{
    let outcome = match AssertUnwindSafe(body).catch_unwind().await {
        Ok(outcome) => outcome,
        Err(_) => Err(anyhow::anyhow!("panicked; its assertion is printed above")),
    };
    match outcome {
        Ok(()) => Ok(()),
        Err(error) => bail!(
            "scenario: {what}: {error:?}\n\n{}",
            stack.diagnostics().await
        ),
    }
}

/// One `twalk_clerk_approvals_total` sample line, as `/metrics` renders it.
fn approvals_sample(outcome: &str, total: u64) -> String {
    format!("twalk_clerk_approvals_total{{outcome=\"{outcome}\"}} {total}")
}

/// One `twalk_clerk_delivery_reads_total` sample line (#300).
fn delivery_reads_sample(outcome: &str, total: u64) -> String {
    format!("twalk_clerk_delivery_reads_total{{outcome=\"{outcome}\"}} {total}")
}

/// The delivery line of one `approbations` post: the third line, where
/// `text::approval_post` puts it (the staged body is one line).
/// The delivery line of a post: **third from the end**, and not third from
/// the start.
///
/// Counted from the tail because the tail is what has not moved. The post ends
/// with the delivery line, the gestures the owner may make, and the reference —
/// three lines, in that order, since the post existed. Its head has grown
/// twice: #335 put what the reply answers above the text, and #367 the path
/// the draft took, and #383 whether the hours it names were checked. A fixed
/// index from the start was right once and has been wrong at every addition
/// since; a body with a newline in it would break it too.
fn delivery_line_of(post: &Event) -> Result<String> {
    let lines: Vec<&str> = post.content.lines().collect();
    lines
        .len()
        .checked_sub(3)
        .and_then(|third_from_the_end| lines.get(third_from_the_end))
        .map(|line| (*line).to_owned())
        .with_context(|| {
            format!(
                "a post ends with a delivery line, the gestures and the reference:\n{}",
                post.content
            )
        })
}

#[tokio::test]
async fn the_clerk_approves_as_the_owners_buzz_device_on_the_reference_deployment() -> Result<()> {
    let started = std::time::Instant::now();
    let stack = Deployment::start().await?;
    eprintln!("    the stack is up, {:.0?}", started.elapsed());

    scenario(
        &stack,
        "the owner's ✅ is one row in the Gateway's approval table, through the device Buzz",
        the_owners_check_is_one_row_in_the_gateways_approval_table_through_the_device_buzz(&stack),
    )
    .await?;
    eprintln!("    first scenario done, {:.0?}", started.elapsed());

    scenario(
        &stack,
        "revoking Buzz on the dashboard makes the next ✅ a thread answer, and the script restores it",
        revoking_buzz_on_the_dashboard_makes_the_next_check_a_thread_answer_and_the_script_restores_it(
            &stack,
        ),
    )
    .await?;
    eprintln!("    both scenarios done, {:.0?}", started.elapsed());

    stack.shutdown().await
}

/// The operator's order: the script signs `Buzz` in, the clerk is started
/// with the session it wrote, a suggestion is posted, the owner ticks it on
/// Buzz, and the deployed Gateway's own record says the owner approved it
/// and nothing was edited.
async fn the_owners_check_is_one_row_in_the_gateways_approval_table_through_the_device_buzz(
    stack: &Deployment,
) -> Result<()> {
    // The script, first, on a directory that does not exist yet.
    let said = stack.provision_device().await?;
    anyhow::ensure!(
        said.contains("device     \"Buzz\""),
        "the script must report the device it signed in:\n{said}"
    );

    stack.start_clerk().await?;
    anyhow::ensure!(
        stack.clerk_container_state().await? == "running",
        "the clerk must be running as a container of this deployment"
    );
    let logs = stack.clerk_logs().await;
    anyhow::ensure!(
        !logs.contains("will not have the clerk's session"),
        "the session the script wrote must be one the deployed Gateway accepts at startup:\n{logs}"
    );

    // The criterion carried over from #265, read off the container the
    // operator got: the clerk's environment names no credential. Not the
    // Gateway's service token, not a device token, nothing the compose file
    // could have handed it — and none of this run's own secrets by value.
    let environment = stack.clerk_environment().await?;
    for entry in &environment {
        let (name, value) = entry.split_once('=').unwrap_or((entry, ""));
        anyhow::ensure!(
            !name.ends_with("_TOKEN")
                && !name.ends_with("_SECRET")
                && name != "GATEWAY_SERVICE_TOKEN",
            "the clerk's container environment holds a credential-shaped variable {name}: \
             {environment:?}"
        );
        for (what, secret) in [
            ("the registration shared secret", REGISTRATION_SHARED_SECRET),
            ("the owner's password", OWNER_PASSWORD),
            ("the owner's Matrix token", stack.owner_token.as_str()),
            ("the test's own device token", stack.device_token.as_str()),
        ] {
            anyhow::ensure!(
                !value.contains(secret),
                "the clerk's container environment carries {what} in {name}"
            );
        }
    }
    anyhow::ensure!(
        environment
            .iter()
            .any(|entry| entry == "CLERK_GATEWAY_SESSION_FILE=/var/lib/clerk/session"),
        "the compose file must point the clerk at the mounted session: {environment:?}"
    );

    // One `Buzz`, as the dashboard would list it, beside the test's own
    // device.
    let buzz = stack.unrevoked_buzz().await?;
    anyhow::ensure!(
        buzz.len() == 1,
        "exactly one unrevoked device named Buzz after the script: {:?}",
        stack.devices().await?
    );

    // A message and a suggestion the Gateway can find, from a contact the
    // user granted; the clerk posts it.
    let talk = stack.stage("first").await?;
    let post = stack.wait_for_post(talk.suggestion_id()).await?;
    anyhow::ensure!(
        post.pubkey.to_hex() == stack.clerk_pubkey,
        "the post is signed by the deployed clerk's own key"
    );
    anyhow::ensure!(
        post.content.contains(talk.suggestion_body()),
        "the post carries the suggestion's body verbatim: {}",
        post.content
    );
    // What reaches Buzz, and what does not, as #335 decided it — and both
    // halves, because a test that only asserts absences cannot notice the day
    // something starts leaving.
    //
    // The line this draws is not "nothing about the contact". It is the one
    // the owner took: *the contact's name and the persona's summary, on a line
    // above the proposed reply, and nothing else of the conversation.* The
    // relay is somebody else's server (ADR 0012), so what is withheld is what
    // would let that server read the conversation — the identity, the number
    // behind it, the words the contact wrote, the room they were written in —
    // and not the two sentences the owner asked to see before deciding.
    let display_name = talk.trigger["data"]["contact"]["display_name"]
        .as_str()
        .unwrap_or_default();
    let network_identifier = talk.trigger["data"]["contact"]["network_identifier"]
        .as_str()
        .unwrap_or_default();
    let contact_body = talk.trigger["data"]["body"].as_str().unwrap_or_default();
    // The portal room, as the trigger's own source names it: the last
    // identifier of the conversation, and the one nothing on this surface has
    // ever needed.
    let portal_room = talk.trigger["source"]
        .as_str()
        .unwrap_or_default()
        .rsplit_once('/')
        .map(|(_, room)| room.to_owned())
        .unwrap_or_default();
    for (what, marker) in [
        ("the contact's Matrix ID", talk.contact.as_str()),
        ("the contact's network identifier", network_identifier),
        ("the contact's own words", contact_body),
        ("the portal room", portal_room.as_str()),
    ] {
        anyhow::ensure!(
            !marker.is_empty(),
            "{what} is not in the fixture, so its absence from the post proves nothing"
        );
        anyhow::ensure!(
            !post.content.contains(marker),
            "the post names {what}, which this surface must not carry (#160, ADR 0012): {}",
            post.content
        );
    }
    let summary = talk.suggestion["data"]["context"]["summary"]
        .as_str()
        .unwrap_or_default();
    anyhow::ensure!(
        post.content.contains(summary),
        "the post does not say what the reply answers, which is half of what #335 sends to \
         this surface: {}",
        post.content
    );
    // Once, not twice: the clerk prefixes the name only when the summary does
    // not already carry it, and a persona writing two sentences about who
    // asked what usually names them. Counting is how that rule is asserted
    // rather than assumed — and it is also what catches the opposite
    // regression, a post that has stopped naming the contact at all.
    anyhow::ensure!(
        post.content.matches(display_name).count() == 1,
        "the contact is named {} times and #334 puts them on this surface exactly once: {}",
        post.content.matches(display_name).count(),
        post.content
    );
    anyhow::ensure!(
        stack.approval(talk.suggestion_id()).await?.0 == reqwest::StatusCode::NOT_FOUND,
        "nothing is approved before the owner decides"
    );

    // The post says whether the reply can reach the contact, in the
    // Companion's words, and it is what the deployed Gateway itself
    // answers for this suggestion (#300): equality with that answer, read
    // with the test's own device, and not a literal — on this stack no
    // bridge runs, and what the Gateway says of the trigger's room is the
    // Gateway's to say.
    let delivery = stack.delivery_of(talk.suggestion_id()).await?;
    eprintln!(
        "    the deployed Gateway answers delivery reach={} detail={}",
        delivery.reach, delivery.detail
    );
    anyhow::ensure!(
        delivery_line_of(&post)? == delivery_line(LANG, &delivery),
        "the post's delivery line is the Companion's sentence for what the Gateway answers \
         ({delivery:?}):\n{}",
        post.content
    );
    // Read once, and counted as such: the read is the only way the line
    // above could hold what the Gateway said.
    stack
        .wait_for_metric(&delivery_reads_sample("found", 1))
        .await?;

    // The owner's ✅, as the relay owner's key.
    let post_id = post.id.to_hex();
    stack.react_as_owner(&post_id, "✅").await?;

    // The Gateway's own record: who approved, and that nothing was edited.
    let approval = stack.wait_for_approval(talk.suggestion_id()).await?;
    anyhow::ensure!(
        approval["approved_by"].as_str() == Some(owner_user_id().as_str()),
        "the approval is stamped with the owner: {approval}"
    );
    anyhow::ensure!(
        approval["edited"].as_bool() == Some(false),
        "a ✅ sends the persona's own words: {approval}"
    );
    anyhow::ensure!(
        approval["publication"].as_str() == Some("published"),
        "the approval was published inside its own request: {approval}"
    );
    // And the event it published is on the deployment's bus, schema-valid,
    // naming the persona whose suggestion it was.
    let event_id = approval["event_id"]
        .as_str()
        .context("the approval names its event")?;
    let published = stack.wait_for_published(event_id).await?;
    validate_against_contract(&published, "persona.reply.approved")?;
    anyhow::ensure!(
        published["data"]["suggestion_event_id"].as_str() == Some(talk.suggestion_id()),
        "the published approval names the suggestion: {published}"
    );
    // What went out is what the persona wrote, and the one line the owner
    // cannot edit out of it: the disclosure, appended at approval by the
    // Gateway and never part of the body (#121, ADR 0019, ADR 0031). Asserted
    // as "the persona's words, then the sentence the event itself names" —
    // rather than against a literal — because which sentence it is depends on
    // the language the reply was written in, and the event is where that
    // decision is recorded.
    let sent = published["data"]["final"]["body"].as_str().unwrap_or_default();
    let disclosure = published["data"]["disclosure"].as_str();
    let expected = match disclosure {
        Some(sentence) => format!("{}\n{sentence}", talk.suggestion_body()),
        None => talk.suggestion_body().to_owned(),
    };
    anyhow::ensure!(
        sent == expected,
        "what went out is the persona's words plus the disclosure and nothing else: {published}"
    );

    // The post is gone, the activity feed says so, and the clerk counted it.
    stack.wait_until_gone(talk.suggestion_id()).await?;
    stack
        .wait_for_activity_line(&activity_approved(LANG, NETWORK, false))
        .await?;
    stack
        .wait_for_metric(&approvals_sample("approved", 1))
        .await?;
    anyhow::ensure!(
        stack.clerk_thread_of(&post_id).await?.is_empty(),
        "an approval that went out is not answered in the thread"
    );
    Ok(())
}

/// The device is revoked from the dashboard: the next ✅ is told so in its
/// thread and carried nowhere; the operator runs the script again — with
/// sudo, because the container owns the session directory now — restarts
/// the clerk, and the ✅ still on the relay is carried.
async fn revoking_buzz_on_the_dashboard_makes_the_next_check_a_thread_answer_and_the_script_restores_it(
    stack: &Deployment,
) -> Result<()> {
    let buzz = stack.unrevoked_buzz().await?;
    anyhow::ensure!(buzz.len() == 1, "one Buzz to revoke: {buzz:?}");
    let first_buzz = buzz[0]["id"]
        .as_str()
        .context("a device has an id")?
        .to_owned();
    stack.revoke(&first_buzz).await?;
    anyhow::ensure!(
        stack.unrevoked_buzz().await?.is_empty(),
        "no unrevoked Buzz after the dashboard's revocation"
    );

    let talk = stack.stage("second").await?;
    let post = stack.wait_for_post(talk.suggestion_id()).await?;
    // The read before this post met the revoked device: the post still
    // went up in the same tick, saying the delivery was not read and why,
    // and the read is counted under that reason (#300).
    anyhow::ensure!(
        delivery_line_of(&post)? == delivery_unread_line(LANG, Unread::GatewayRefused),
        "a post made while the device is revoked says its delivery was not read:\n{}",
        post.content
    );
    stack
        .wait_for_metric(&delivery_reads_sample("refused", 1))
        .await?;
    let post_id = post.id.to_hex();
    let check = stack.react_as_owner(&post_id, "✅").await?;

    let told = stack
        .wait_for_thread_line(&post_id, &thread_revoked(LANG))
        .await?;
    anyhow::ensure!(told.content == thread_revoked(LANG), "{}", told.content);
    anyhow::ensure!(
        told.tags.iter().any(|tag| {
            let tag = tag.as_slice();
            tag.len() >= 2
                && tag[0] == "r"
                && tag[1] == format!("twalk:gesture:{}", check.id.to_hex())
        }),
        "the thread answer is keyed on the owner's gesture: {told:?}"
    );
    anyhow::ensure!(
        stack.approval(talk.suggestion_id()).await?.0 == reqwest::StatusCode::NOT_FOUND,
        "nothing got past the door while the device was revoked"
    );
    anyhow::ensure!(
        stack.posts_about(talk.suggestion_id()).await?.len() == 1,
        "the post waits for the session to come back"
    );
    stack
        .wait_for_clerk_log("provision-clerk-device.sh", 1)
        .await?;

    // The operator route, again: the session in the directory is dead,
    // the script signs a new Buzz in — one unrevoked, the revoked one
    // still listed with its date — and says to restart the clerk.
    let said = stack.provision_device().await?;
    anyhow::ensure!(
        said.contains("is dead") && said.contains("device     \"Buzz\""),
        "the script must find the session dead and sign a new device in:\n{said}"
    );
    let devices = stack.devices().await?;
    let unrevoked = stack.unrevoked_buzz().await?;
    anyhow::ensure!(
        unrevoked.len() == 1,
        "exactly one unrevoked Buzz after the script: {devices:?}"
    );
    anyhow::ensure!(
        unrevoked[0]["id"].as_str() != Some(first_buzz.as_str()),
        "the new Buzz is a new device: {devices:?}"
    );
    anyhow::ensure!(
        devices
            .iter()
            .any(|device| device["id"].as_str() == Some(first_buzz.as_str())
                && device["revoked_unix_seconds"].is_number()),
        "the revoked Buzz stays listed as revoked: {devices:?}"
    );

    stack.restart_clerk().await?;
    let approval = stack.wait_for_approval(talk.suggestion_id()).await?;
    anyhow::ensure!(
        approval["approved_by"].as_str() == Some(owner_user_id().as_str())
            && approval["edited"].as_bool() == Some(false),
        "the ✅ made while the device was revoked is carried once it is back: {approval}"
    );
    stack.wait_until_gone(talk.suggestion_id()).await?;
    let thread = stack.clerk_thread_of(&post_id).await?;
    anyhow::ensure!(
        thread.len() == 1 && thread[0].content == thread_revoked(LANG),
        "the revoked line stays and nothing is added: {thread:?}"
    );
    // The restarted clerk found both posts by their reference lines and
    // read neither suggestion again: one read per suggestion, at posting
    // time, and the relay is the memory (ADR 0035). A restart resets the
    // counters, so the proof is that both stand at zero.
    let metrics = stack.clerk_metrics().await?;
    for outcome in ["found", "refused"] {
        anyhow::ensure!(
            metrics
                .lines()
                .any(|line| line == delivery_reads_sample(outcome, 0)),
            "the restarted clerk read no suggestion again ({outcome}); /metrics was:\n{metrics}"
        );
    }
    Ok(())
}
