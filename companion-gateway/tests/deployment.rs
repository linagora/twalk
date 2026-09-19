//! Ticket #48, the Companion Gateway in the reference deployment: a
//! `docker compose up` of deploy/docker-compose brings the Gateway up
//! configured only through the environment file, healthy, answering its
//! health endpoint, exposing its metrics and serving the Companion's index
//! file on its own origin.
//!
//! The Gateway needs no homeserver to come up (it talks to one only while
//! somebody signs in), and the bus its consent outbox publishes to comes up
//! with it, so the first test brings up the `companion-gateway` service and
//! the `nats` it depends on — compose still interpolates the whole file, so
//! the env file below carries every documented variable, exactly as
//! `.env.example` does. The second test is
//! about the deployment's Synapse configuration: the `openid` resource that
//! sign-in verifies tokens at (ticket #52), which nothing else in the
//! repository would notice the loss of.
//!
//! The third test (ticket #53) is the one that needs the whole stack: the
//! registration relay creating this deployment's one account on a homeserver
//! whose own registration is closed, and the Sensor joining a room the Gateway
//! invited it to and publishing that room's traffic to the bus as
//! `network=matrix`. It is also where ticket #54's own end of the chain is
//! asserted, because this is the only place a **real Sensor** publishes into
//! a bus a real Gateway consumes: a correspondent who writes in that room
//! appears in the Gateway's pending list, and leaves it when the user
//! decides. It brings the `sensor` service up beside the Gateway,
//! which is also where "the Sensor's startup does not depend on the Gateway"
//! is asserted — from compose's own resolved configuration.
//!
//! Since ticket #24 the same test closes the loop: a suggestion is published
//! against the correspondent's **real** message, the deployed Gateway is
//! asked to approve it, and the approved reply is asserted to appear in the
//! real Matrix room — posted by the real Sensor, read back from the
//! homeserver. The persona is the one part stood in for, because Hermes does
//! not run in this compose stack yet; everything downstream of the suggestion
//! is the deployment's own. The owner's own message, in the same test, is
//! `outbound.message.sent.v1` with no consent extension at all: the owner is
//! not a contact (tickets #109 and #147, ADR 0018 and ADR 0021), and this
//! test had gone on expecting `inbound.message.received` for it.
//!
//! The deploy stack runs under its own compose project and host ports, next
//! to the harness's own stack: TWALK_DEPLOY_TEST_STACK (default
//! twalk-deploy-test), TWALK_DEPLOY_TEST_GATEWAY_PORT (default 18318 —
//! deliberately distinct from the 18328 default of
//! sensor/tests/deployment.rs, so two default-configured deploy stacks do
//! not collide on it), TWALK_DEPLOY_TEST_SYNAPSE_PORT and
//! TWALK_DEPLOY_TEST_NATS_PORT. Both local images are tagged per compose
//! project (`twalk/companion-gateway:<stack>`, `twalk/sensor:<stack>`, issue
//! #38) so that parallel worktrees never overwrite each other's build; the
//! operator defaults, `:local`, are untouched.
//!
//! The stack and its images stay up between runs: that is what makes a warm
//! run fast. Set TWALK_DEPLOY_TEST_TEARDOWN=1 to drop them at the end of a
//! passing run instead — by hand, `docker compose -p <stack> down -v`
//! followed by `docker image rm twalk/companion-gateway:<stack>
//! twalk/sensor:<stack>`.
//!
//! The credentials below are throwaway constants for the local, ephemeral
//! deploy-test stack (same category as the test-bot passwords) — the env
//! file they land in is generated in a temp directory, never committed.

mod harness;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use harness::{parse_exposition, poll_until, validate_against_contract, Bus};
use tokio::process::Command;

const SERVER_NAME: &str = "deploy.twalk";

/// The owner of the deploy-test stack: the account the registration relay
/// creates, and the only inviter the Sensor honours there.
const OWNER_LOCALPART: &str = "owner";
/// Throwaway constants for the local, ephemeral deploy-test stack.
const OWNER_PASSWORD: &str = "deploy-test-only-password-owner";
const REGISTRATION_SHARED_SECRET: &str = "deploy-test-only-registration-shared-secret";
/// A throwaway password for the per-run correspondent account #54's half of
/// the bootstrap test provisions.
const CORRESPONDENT_PASSWORD: &str = "deploy-test-only-password-correspondent";

/// The service token the deployed Gateway serves its consent snapshot to
/// (ticket #50) and its runtime settings to (ticket #98) — long enough for
/// the Gateway's own minimum, and throwaway like the rest of this stack's
/// credentials.
const SERVICE_TOKEN: &str = "deploy-test-only-gateway-service-token";

/// The LLM endpoint credential this stack supplies as a **file**, which is
/// what the reference deployment does (ADR 0015, ticket #98). Throwaway, and
/// distinctive, so that a test can search an answer for it.
const FILE_CREDENTIAL: &str = "deploy-test-only-llm-endpoint-key";

/// The host path that file lives at. Stable per compose project rather than
/// per run: a path that changed every time would recreate the gateway
/// container on every run, and a warm stack is what makes the next run fast.
fn credential_file() -> PathBuf {
    std::env::temp_dir().join(format!("twalk-gateway-deploy-test-{}.key", deploy_stack()))
}

fn owner_user_id() -> String {
    format!("@{OWNER_LOCALPART}:{SERVER_NAME}")
}

fn sensor_user_id() -> String {
    format!("@sensor:{SERVER_NAME}")
}

fn homeserver_url() -> String {
    format!("http://localhost:{}", synapse_port())
}

/// The bus as the deploy stack publishes to it (`sensor/src/normalize.rs`).
const STREAM: &str = "twalk";
const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";
/// Where the **owner's own** message lands. The deploy stack's Sensor takes
/// its operator from `SENSOR_OWNER`, which defaults to `GATEWAY_OWNER`, so a
/// message the owner sends in the room they invited the Sensor into is
/// published here and not on [`MESSAGE_SUBJECT`] — the owner is not a contact
/// and never has a consent state (tickets #109 and #147, ADR 0018 and
/// ADR 0021).
const OWN_MESSAGE_SUBJECT: &str = "twalk.outbound.message.sent.v1";
/// The two subjects ticket #24's half of this test uses: the suggestion a
/// persona would have published, and the approval the deployed Gateway
/// publishes and the deployed Sensor posts.
const SUGGEST_SUBJECT: &str = "twalk.persona.suggest.produced.v1";
const APPROVED_SUBJECT: &str = "twalk.persona.reply.approved.v1";

/// [`poll_until`] with the patience a cold deploy stack needs: the Sensor has
/// to log in, bootstrap its crypto store and sync before it can act on an
/// invitation, which is minutes of container start rather than the 20 seconds
/// a process-boundary test waits.
async fn poll_deploy<T, Fut>(mut attempt: impl FnMut() -> Fut, description: &str) -> Result<T>
where
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..240 {
        if let Some(value) = attempt().await {
            return Ok(value);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    bail!("timed out {description}")
}

fn deploy_stack() -> String {
    std::env::var("TWALK_DEPLOY_TEST_STACK").unwrap_or_else(|_| "twalk-deploy-test".to_owned())
}

fn gateway_port() -> String {
    std::env::var("TWALK_DEPLOY_TEST_GATEWAY_PORT").unwrap_or_else(|_| "18318".to_owned())
}

fn synapse_port() -> String {
    std::env::var("TWALK_DEPLOY_TEST_SYNAPSE_PORT").unwrap_or_else(|_| "18218".to_owned())
}

fn nats_port() -> String {
    std::env::var("TWALK_DEPLOY_TEST_NATS_PORT").unwrap_or_else(|_| "14418".to_owned())
}

fn gateway_base_url() -> String {
    format!("http://localhost:{}", gateway_port())
}

/// The tag the deploy stack's Gateway image is built and run under. One tag
/// per compose project, so two worktrees on two stacks each rebuild their own
/// image instead of overwriting a shared one (issue #38); compose project
/// names are already restricted to the characters a Docker tag accepts.
/// TWALK_GATEWAY_IMAGE overrides it, as it does for an operator.
fn gateway_image() -> String {
    std::env::var("TWALK_GATEWAY_IMAGE")
        .ok()
        .filter(|image| !image.is_empty())
        .unwrap_or_else(|| format!("twalk/companion-gateway:{}", deploy_stack()))
}

/// Same rule for the Sensor's image, which the bootstrap test below needs
/// running next to the Gateway: one tag per compose project, so this stack
/// never overwrites the Sensor suite's build or an operator's `:local`.
fn sensor_image() -> String {
    std::env::var("TWALK_SENSOR_IMAGE")
        .ok()
        .filter(|image| !image.is_empty())
        .unwrap_or_else(|| format!("twalk/sensor:{}", deploy_stack()))
}

fn deploy_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../deploy/docker-compose")
}

/// Writes the environment file the deploy stack is configured through:
/// exactly what `.env.example` documents, with throwaway test values and the
/// test's own host ports.
fn write_env_file() -> Result<PathBuf> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "twalk-gateway-deploy-test-{}-{unique}.env",
        std::process::id()
    ));
    let (gateway_image, sensor_image) = (gateway_image(), sensor_image());
    let (gateway_port, synapse_port, nats_port) = (gateway_port(), synapse_port(), nats_port());
    let owner = owner_user_id();
    let credential_path = credential_file();
    let credential_file = credential_path.display().to_string();
    let contents = format!(
        "MATRIX_DOMAIN={SERVER_NAME}\n\
         MATRIX_HTTP_PORT={synapse_port}\n\
         MATRIX_REGISTRATION_SHARED_SECRET={REGISTRATION_SHARED_SECRET}\n\
         MATRIX_MACAROON_SECRET=deploy-test-only-macaroon-secret\n\
         MATRIX_FORM_SECRET=deploy-test-only-form-secret\n\
         SENSOR_USER_ID=@sensor:{SERVER_NAME}\n\
         SENSOR_PASSWORD=deploy-test-only-password-sensor\n\
         SENSOR_ALLOWED_INVITERS={owner}\n\
         SENSOR_OWNER_IDENTITIES={OWNER_GHOST}\n\
         SENSOR_STATE_DIR=/data\n\
         SENSOR_LOG_LEVEL=info\n\
         NATS_PORT={nats_port}\n\
         GATEWAY_HTTP_PORT={gateway_port}\n\
         GATEWAY_FALLBACK_FILE=200.html\n\
         GATEWAY_LOG_LEVEL=info,twalk_companion_gateway=debug\n\
         GATEWAY_OWNER={owner}\n\
         GATEWAY_STATE_DIR=/data\n\
         GATEWAY_REGISTRATION_SHARED_SECRET={REGISTRATION_SHARED_SECRET}\n\
         GATEWAY_SENSOR_USER_ID=@sensor:{SERVER_NAME}\n\
         GATEWAY_NATS_URL=nats://nats:4222\n\
         GATEWAY_SERVICE_TOKEN={SERVICE_TOKEN}\n\
         HERMES_LLM_API_KEY_FILE={credential_file}\n\
         TWALK_GATEWAY_IMAGE={gateway_image}\n\
         TWALK_SENSOR_IMAGE={sensor_image}\n"
    );
    // The credential the operator supplies as a file (#98). compose mounts
    // this same host path into the Gateway and into the Hermes runtime, so
    // writing it here is what makes "the file wins" a property of the
    // deployment and not only of the code.
    std::fs::write(&credential_path, format!("{FILE_CREDENTIAL}\n"))
        .with_context(|| format!("failed to write {}", credential_path.display()))?;
    std::fs::write(&path, contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// Runs `docker compose` against the deploy stack with the test's env file.
async fn compose(env_file: &Path, args: &[&str], what: &str) -> Result<String> {
    let output = Command::new("docker")
        .arg("compose")
        .arg("-p")
        .arg(deploy_stack())
        .arg("--env-file")
        .arg(env_file)
        .arg("-f")
        .arg(deploy_dir().join("compose.yaml"))
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to run docker compose {what}"))?;
    if !output.status.success() {
        bail!(
            "docker compose {what} failed with {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// [`compose`] for a command that *changes* the stack — a build, an up, a
/// removal. The tests of this binary run in parallel and share one compose
/// project, so two of them creating the same container or building the same
/// image at the same moment is a race none of them needs. Reads (`ps`,
/// `config`, `logs`) do not take the lock.
async fn compose_change(env_file: &Path, args: &[&str], what: &str) -> Result<String> {
    static CHANGES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = CHANGES.lock().await;
    compose(env_file, args, what).await
}

/// Builds the stack's two local images, once per test binary: both the
/// origin test and the bootstrap test want the Gateway's, and the bootstrap
/// test also wants the Sensor's. Warm, this is a cache hit; the `OnceCell`
/// is `ensure_stack`'s own pattern, so concurrent callers wait for the first
/// instead of racing it.
async fn ensure_images(env_file: &Path) -> Result<()> {
    static BUILT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    BUILT
        .get_or_try_init(|| async {
            compose_change(
                env_file,
                &["build", "companion-gateway", "sensor"],
                "build companion-gateway sensor",
            )
            .await
            .map(|_| ())
        })
        .await?;
    Ok(())
}

/// Whether the run asked for the stack to be torn down at the end
/// (TWALK_DEPLOY_TEST_TEARDOWN=1). Off by default, as in the Sensor's
/// deployment test: the stack and its image stay up, and the next run reuses
/// them warm.
fn teardown_requested() -> bool {
    matches!(
        std::env::var("TWALK_DEPLOY_TEST_TEARDOWN").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Removes what this run created: the Gateway service and the Sensor the
/// bootstrap test brings up next to it, their per-stack images and the
/// generated env file. The rest of the stack (Synapse, the bus) is left to the
/// Sensor's deployment test, which owns it.
async fn teardown(env_file: &Path) -> Result<()> {
    compose_change(
        env_file,
        &["rm", "-sfv", "companion-gateway", "sensor"],
        "rm companion-gateway sensor",
    )
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
    std::fs::remove_file(env_file)
        .with_context(|| format!("failed to remove {}", env_file.display()))?;
    Ok(())
}

/// The stack's containers as `docker compose ps --format json` reports them:
/// one JSON object per line, one line per service.
async fn compose_ps(env_file: &Path) -> Result<Vec<serde_json::Value>> {
    let stdout = compose(env_file, &["ps", "-a", "--format", "json"], "ps").await?;
    Ok(stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect())
}

/// A network ghost of the owner's own account, as the deployment lists it
/// (ticket #109, #149). Nothing sends a message as it in these tests — what it
/// is here for is the wiring: one list in `.env`, read by the Sensor and by the
/// Gateway.
const OWNER_GHOST: &str = "@whatsapp_lid-115332874281144:test.twalk";

/// Ticket #149's deployment half: the owner's confirmed identities are listed
/// **once** and reach both components that need them.
///
/// The Gateway needs the set because the owner has no consent state and the
/// Gateway is the single writer of it; the Sensor needs it because the owner is
/// not a contact on any event. Neither can derive it — a bridge's provisioning
/// API exposes no ghost Matrix ID at all — so the risk this test exists for is
/// two variables drifting apart on a live deployment, which is the outcome the
/// ticket names. Asserted against compose's own interpolation, which is the
/// thing that would be wrong.
#[tokio::test]
async fn one_list_of_the_owners_identities_reaches_both_components() -> Result<()> {
    let env_file = write_env_file()?;
    let resolved = compose(&env_file, &["config"], "config").await?;
    let configured: Vec<&str> = resolved
        .lines()
        .map(str::trim)
        .filter(|line| line.contains("OWNER_IDENTITIES:"))
        .collect();
    assert_eq!(
        configured,
        vec![
            // compose quotes a value starting with `@`; the value is the point.
            format!("GATEWAY_OWNER_IDENTITIES: '{OWNER_GHOST}'"),
            format!("SENSOR_OWNER_IDENTITIES: '{OWNER_GHOST}'"),
        ],
        "one value in .env reaches both services; an operator who set only the          Sensor's variable — every existing deployment — needs to change nothing:\n{resolved}"
    );
    Ok(())
}

#[tokio::test]
async fn the_compose_stack_serves_the_companion_with_health_and_metrics() -> Result<()> {
    let env_file = write_env_file()?;

    // Build first so a build failure is attributed to the build; warm, this
    // is a cache hit. Then bring the service up exactly as an operator
    // would — the skeleton Gateway needs nothing else in the stack.
    ensure_images(&env_file).await?;
    compose_change(
        &env_file,
        &["up", "-d", "--wait", "companion-gateway"],
        "up companion-gateway",
    )
    .await?;

    // The container runs this stack's own image, not a shared tag another
    // worktree's checkout may have rebuilt under it (issue #38), and compose
    // reports it healthy — `up --wait` returned, and the healthcheck is the
    // Gateway's own health endpoint.
    let entries = compose_ps(&env_file).await?;
    let entry = entries
        .iter()
        .find(|entry| entry["Service"].as_str() == Some("companion-gateway"))
        .context("the companion-gateway service has no container")?;
    assert_eq!(
        entry["Image"].as_str(),
        Some(gateway_image().as_str()),
        "the Gateway container must run this stack's own image: {entry}"
    );
    assert_eq!(
        entry["Health"].as_str(),
        Some("healthy"),
        "compose must report the Gateway healthy: {entry}"
    );

    let base = gateway_base_url();
    let health = poll_until(
        || async {
            let response = reqwest::get(format!("{base}/health")).await.ok()?;
            (response.status() == reqwest::StatusCode::OK)
                .then_some(response)?
                .text()
                .await
                .ok()
        },
        "the deployed Gateway's health endpoint",
    )
    .await?;
    let health: serde_json::Value = serde_json::from_str(&health)?;
    assert_eq!(health["status"].as_str(), Some("ok"));

    // Metrics, in the Sensor's exposition conventions.
    let metrics = reqwest::get(format!("{base}/metrics"))
        .await?
        .text()
        .await?;
    let samples = parse_exposition(&metrics);
    assert!(
        samples
            .iter()
            .any(|(name, _)| name == "twalk_companion_gateway_uptime_seconds"),
        "the deployed Gateway must expose its metrics:\n{metrics}"
    );

    // The Companion's origin: the image ships a holding page until the PWA
    // is built, and the operator mounts their own build over it.
    let index = reqwest::get(format!("{base}/")).await?;
    assert_eq!(index.status(), reqwest::StatusCode::OK);
    assert_eq!(
        index
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.starts_with("text/html")),
        Some(true),
        "the origin root serves an HTML index file"
    );
    assert!(
        index.text().await?.contains("Companion"),
        "the index file served is the Companion's"
    );

    // And a client-side route — a deep link reloaded cold — gets the SPA
    // fallback with a 200 from the deployed image, not a 404.
    let deep_link = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?
        .get(format!("{base}/onboarding/whatsapp"))
        .send()
        .await?;
    assert_eq!(
        deep_link.status(),
        reqwest::StatusCode::OK,
        "the deployed image serves the SPA fallback for a client-side route"
    );
    assert_eq!(
        deep_link
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );

    // The image carries its own HTTP description (ticket #63): the Companion
    // lot generates its client from it, and a build that shipped without it
    // — a `.dockerignore` rule, a build context narrowed to `src/` — would
    // leave a deployed Gateway no client can be generated against.
    let described = reqwest::get(format!("{base}/openapi.yaml")).await?;
    assert_eq!(described.status(), reqwest::StatusCode::OK);
    assert_eq!(
        described
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/yaml"),
    );
    let described = described.text().await?;
    assert_eq!(
        described,
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("openapi.yaml"))?,
        "the deployed image must serve the description committed in the repository"
    );

    // Sign-in is configured from the environment file, and the guard is live
    // in the deployed image: every API endpoint refuses a caller with no
    // device token. (The sign-in flow itself is exercised against a real
    // homeserver in tests/signin.rs; here the question is only whether the
    // deployed container is wired for it.)
    let unauthenticated = reqwest::get(format!("{base}/api/devices")).await?;
    assert_eq!(
        unauthenticated.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "the deployed Gateway's API must require a device token"
    );
    let body: serde_json::Value = serde_json::from_str(&unauthenticated.text().await?)?;
    assert_eq!(
        body["error"].as_str(),
        Some("unauthenticated"),
        "a 503 here would mean GATEWAY_OWNER never reached the container: {body}"
    );

    // Consent (#49) is wired in the deployed image: the same guard answers
    // for its routes, and the consent store opened on the gateway-data
    // volume with the bus behind it — which is what the outbox gauge being
    // present at all says. (A decision taken end to end needs an owner
    // account on the deployment's homeserver, which this stack does not
    // provision; tests/consent.rs takes that path against the test stack.)
    let unauthenticated = reqwest::get(format!("{base}/api/consent/state")).await?;
    assert_eq!(
        unauthenticated.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "the deployed Gateway's consent routes must require a device token"
    );
    let samples = parse_exposition(
        &reqwest::get(format!("{base}/metrics"))
            .await?
            .text()
            .await?,
    );
    assert_eq!(
        samples
            .iter()
            .find(|(name, _)| name == "twalk_companion_gateway_consent_outbox_pending")
            .map(|(_, value)| *value),
        Some(0),
        "the deployed Gateway must have opened its consent journal and drained its outbox: \
         no gauge at all means GATEWAY_NATS_URL or GATEWAY_STATE_DIR never reached the container"
    );

    // The consent snapshot (#50) is wired in the deployed image, and its
    // credential is not the device cookie: the service token from the
    // environment file opens it, and nothing else does. This is what a
    // Sensor with a cold cache will call (#51), so the variable reaching the
    // container is the property worth pinning here — an empty state is the
    // right answer on a stack where nobody has decided anything.
    let client = reqwest::Client::new();
    let snapshot = client
        .get(format!("{base}/api/consent/snapshot"))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await?;
    assert_eq!(
        snapshot.status(),
        reqwest::StatusCode::OK,
        "the deployed Gateway must serve its consent snapshot to the service token: \
         a 503 here means GATEWAY_SERVICE_TOKEN never reached the container"
    );
    let snapshot: serde_json::Value = snapshot.json().await?;
    assert_eq!(snapshot["stream"].as_str(), Some("twalk"));
    assert_eq!(
        snapshot["next_stream_sequence"].as_u64(),
        snapshot["stream_sequence"]
            .as_u64()
            .map(|sequence| sequence + 1),
        "the snapshot names the sequence a consumer starts at: {snapshot}"
    );
    let unauthenticated = reqwest::get(format!("{base}/api/consent/snapshot")).await?;
    assert_eq!(
        unauthenticated.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "the deployed Gateway's snapshot must refuse a caller with no service token"
    );

    // The model and language settings (#98), on the same credential: this is
    // what the Hermes runtime reads before it starts a persona. Nothing has
    // been configured on this stack, so `llm` is null — which is the point:
    // the runtime is TOLD there is no model rather than left to guess
    // between that, a Gateway it cannot reach, and one that refused its
    // token (ADR 0015).
    let runtime = client
        .get(format!("{base}/api/settings/runtime"))
        .bearer_auth(SERVICE_TOKEN)
        .send()
        .await?;
    assert_eq!(
        runtime.status(),
        reqwest::StatusCode::OK,
        "the deployed Gateway must serve its runtime settings to the service token"
    );
    let runtime: serde_json::Value = runtime.json().await?;
    assert!(
        runtime["llm"].is_null(),
        "nothing has named a model on this stack: {runtime}"
    );
    assert!(runtime["language"].is_null(), "{runtime}");
    let unauthenticated = reqwest::get(format!("{base}/api/settings/runtime")).await?;
    assert_eq!(
        unauthenticated.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "and it must refuse a caller with no service token — the endpoint that carries the \
         LLM credential is not one a device cookie opens"
    );

    // Only on request: a service left up (with its image) is what makes the
    // next run warm. Deliberately after the assertions, so a failure leaves
    // the container and its logs in place to inspect.
    if teardown_requested() {
        teardown(&env_file).await?;
    }
    Ok(())
}

/// The homeserver of the reference deployment must serve the one federation
/// endpoint sign-in needs — and nothing more of the federation API.
///
/// This is a test about `deploy/docker-compose/synapse/homeserver.yaml`: the
/// listener's `openid` resource is what
/// `GET /_matrix/federation/v1/openid/userinfo` rides on, and without it every
/// sign-in against the reference stack fails at the verification step while
/// nothing else in the repository notices. The `federation` resource stays
/// absent, which the second assertion pins: a personal hub does not federate.
///
/// Synapse belongs to the whole deploy stack rather than to the Gateway, so
/// this test brings it up and leaves it up, as the Sensor's deployment test
/// does — a warm stack is what makes the next run fast.
#[tokio::test]
async fn the_deployments_homeserver_serves_openid_userinfo_and_no_more_federation() -> Result<()> {
    let env_file = write_env_file()?;
    compose_change(&env_file, &["up", "-d", "--wait", "synapse"], "up synapse").await?;
    let homeserver = format!("http://localhost:{}", synapse_port());

    // A token this homeserver never minted: a 401 proves the endpoint is
    // routed, which is all the Gateway needs of it here (tests/signin.rs
    // covers what a real token does).
    let userinfo = poll_until(
        || async {
            reqwest::get(format!(
                "{homeserver}/_matrix/federation/v1/openid/userinfo?access_token=never-minted"
            ))
            .await
            .ok()
        },
        "the deployed homeserver's OpenID userinfo endpoint",
    )
    .await?;
    assert_eq!(
        userinfo.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "the deployment must route /_matrix/federation/v1/openid/userinfo, or no sign-in can work"
    );

    let federation = reqwest::get(format!("{homeserver}/_matrix/federation/v1/version")).await?;
    assert_eq!(
        federation.status(),
        reqwest::StatusCode::NOT_FOUND,
        "the rest of the federation API stays absent: this hub does not federate"
    );
    Ok(())
}

/// Ticket #53, end to end on the reference deployment: the Gateway creates
/// this deployment's one account on a homeserver whose own registration is
/// closed, refuses a second, and invites the Sensor into a room the user
/// selects — after which the Sensor joins it and that room's traffic reaches
/// the bus as `network=matrix`.
///
/// This is the only test in the repository where the whole chain is real at
/// once: Synapse with `enable_registration` off, the Gateway relaying with the
/// registration shared secret, a Sensor provisioned by the stack's own
/// one-shot rather than by the Gateway, and NATS JetStream carrying the event.
/// The pieces are each covered at their own process boundary elsewhere
/// (`tests/bootstrap.rs`, `sensor/tests/fidelity.rs`); what only this test can
/// show is that they fit.
#[tokio::test]
async fn the_deployed_gateway_creates_the_one_account_and_the_sensor_joins_what_it_is_invited_to(
) -> Result<()> {
    let env_file = write_env_file()?;
    ensure_images(&env_file).await?;
    compose_change(
        &env_file,
        &["up", "-d", "--wait", "companion-gateway", "sensor"],
        "up companion-gateway sensor",
    )
    .await?;

    // The Sensor's startup depends on the homeserver, the bus and the account
    // provisioning one-shot — never on the Gateway. Asserted from compose's
    // own resolved configuration, so adding such an edge fails here rather
    // than being noticed the first time a Gateway is down at boot.
    let configuration = compose(&env_file, &["config", "--format", "json"], "config").await?;
    let configuration: serde_json::Value = serde_json::from_str(&configuration)?;
    let sensor_dependencies: Vec<&str> = configuration["services"]["sensor"]["depends_on"]
        .as_object()
        .map(|dependencies| dependencies.keys().map(String::as_str).collect())
        .unwrap_or_default();
    assert!(
        !sensor_dependencies.contains(&"companion-gateway"),
        "the Sensor must not depend on the Gateway to start: {sensor_dependencies:?}"
    );

    // The homeserver's own self-service registration is closed, which is the
    // whole reason the relay exists: a personal server must not become a
    // public one.
    let self_service = reqwest::Client::new()
        .post(format!("{}/_matrix/client/v3/register", homeserver_url()))
        .json(&serde_json::json!({
            "username": "a_stranger",
            "password": "a-password-a-stranger-chose",
            "auth": { "type": "m.login.dummy" },
        }))
        .send()
        .await?;
    assert_eq!(
        self_service.status(),
        reqwest::StatusCode::FORBIDDEN,
        "the reference deployment must keep Synapse's open registration off"
    );

    let base = gateway_base_url();
    let client = reqwest::Client::new();
    let register = || async {
        client
            .post(format!("{base}/api/bootstrap/account"))
            .json(&serde_json::json!({
                "username": OWNER_LOCALPART,
                "password": OWNER_PASSWORD,
            }))
            .send()
            .await
    };

    // Cold stack: this is the deployment's one registration. Warm stack: the
    // account is already there from a previous run, and the relay refuses —
    // which is the same assertion made twice.
    let first = poll_deploy(
        || async { register().await.ok() },
        "the deployed Gateway to answer a registration",
    )
    .await?;
    let owner_token = match first.status() {
        reqwest::StatusCode::CREATED => {
            let document: serde_json::Value = serde_json::from_str(&first.text().await?)?;
            assert_eq!(document["user_id"].as_str(), Some(owner_user_id().as_str()));
            assert!(
                document.get("recovery_key").is_none(),
                "the Gateway never returns a recovery key: {document}"
            );
            document["access_token"]
                .as_str()
                .context("the relay answered no access token")?
                .to_owned()
        }
        reqwest::StatusCode::CONFLICT => {
            let document: serde_json::Value = serde_json::from_str(&first.text().await?)?;
            assert_eq!(
                document["error"].as_str(),
                Some("account_already_exists"),
                "a warm stack already has the account: {document}"
            );
            // The password is this test's own, so a warm stack can log in.
            login(&client).await?
        }
        status => bail!("unexpected registration status {status}"),
    };

    // Either way, the next registration is refused: one owner per deployment.
    let second = register().await?;
    assert_eq!(
        second.status(),
        reqwest::StatusCode::CONFLICT,
        "the deployed relay must refuse any account after the first"
    );
    let refusal: serde_json::Value = serde_json::from_str(&second.text().await?)?;
    assert_eq!(refusal["error"].as_str(), Some("account_already_exists"));

    // Sign in with that account, as the Companion does after screen 2.
    let device_token = sign_in(&client, &base, &owner_token).await?;

    // The operator's credential file reached the gateway container, and it
    // is the one in force although nobody has typed one into the Companion
    // (#98). That is the reference deployment's own combination — the model
    // name from the browser, the key from a file (ADR 0015) — and this is
    // where the compose wiring for it is asserted rather than assumed.
    let model: serde_json::Value = client
        .get(format!("{base}/api/settings/model"))
        .header("cookie", format!("twalk_device={device_token}"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        model["credential"]["source"].as_str(),
        Some("file"),
        "a 'companion' or a null source here means HERMES_LLM_API_KEY_FILE never reached the \
         gateway container: {model}"
    );
    assert_eq!(
        model["credential"]["configured"].as_bool(),
        Some(true),
        "{model}"
    );
    assert!(
        !model.to_string().contains(FILE_CREDENTIAL),
        "the credential is write-only: no browser-facing read returns it. {model}"
    );

    // A native Matrix room of the user's own: no bridge marker, which is what
    // makes its traffic resolve to `network=matrix` (ADR 0009, ticket #18).
    let room_id = create_room(&client, &owner_token).await?;

    // Screen 3d: the user selects that room, and the Gateway invites the
    // Sensor with the user's own token — no password anywhere in the request.
    let invited = client
        .post(format!("{base}/api/bootstrap/rooms"))
        .header("cookie", format!("twalk_device={device_token}"))
        .json(&serde_json::json!({
            "matrix_access_token": owner_token,
            "rooms": [room_id.clone()],
        }))
        .send()
        .await?;
    assert_eq!(invited.status(), reqwest::StatusCode::OK);
    let document: serde_json::Value = serde_json::from_str(&invited.text().await?)?;
    assert_eq!(document["sensor"].as_str(), Some(sensor_user_id().as_str()));
    assert!(
        matches!(
            document["rooms"][0]["status"].as_str(),
            Some("invited") | Some("already_present")
        ),
        "the Sensor must be invited into the selected room: {document}"
    );

    // The Sensor joins on its own, because the owner is on its allow-list.
    // Nothing of the Gateway is involved in this step.
    poll_deploy(
        || async {
            let membership = membership(&client, &owner_token, &room_id, &sensor_user_id())
                .await
                .ok()??;
            (membership == "join").then_some(())
        },
        "the Sensor to join the room it was invited to",
    )
    .await?;

    // And that room's traffic reaches the bus, as Matrix.
    let bus = Bus::connect_to(&format!("nats://localhost:{}", nats_port())).await?;
    let body = format!("un message ordinaire {}", std::process::id());
    send_message(&client, &owner_token, &room_id, &body).await?;
    let stored = poll_deploy(
        || async {
            bus.fetch_room_messages_on(SERVER_NAME, STREAM, OWN_MESSAGE_SUBJECT, &room_id)
                .await
                .ok()?
                .into_iter()
                .next()
        },
        "the invited room's traffic to reach the bus",
    )
    .await?;
    let event = &stored.payload;
    validate_against_contract(event, "outbound.message.sent")?;
    assert_eq!(
        event["network"].as_str(),
        Some("matrix"),
        "a room with no bridge marker is native Matrix traffic: {event}"
    );
    assert_eq!(
        stored.header("network"),
        Some("matrix"),
        "and the bus header says so too, for server-side filtering"
    );
    assert_eq!(event["subject"].as_str(), Some(owner_user_id().as_str()));
    assert_eq!(event["data"]["body"].as_str(), Some(body.as_str()));
    // The owner is not a contact, so their own message carries no consent
    // state at all — not `granted`, not `pending`, nothing (ADR 0018,
    // ADR 0021). Asserted here because this is the only place in this
    // repository where a real Sensor publishes the real owner's own traffic
    // into a bus a real Gateway consumes.
    assert!(
        event.get("consent").is_none(),
        "the owner's own message carries no consent extension: {event}"
    );
    assert_eq!(stored.header("consent"), None);

    // --- ticket #54, at the only seam where the whole chain is real: a
    // correspondent writes in that room, the Sensor publishes it, and the
    // Gateway's projection turns it into a decision waiting to be taken.
    //
    // A fresh account per run, because the Gateway's journal lives on a
    // volume that survives between runs: a correspondent this deployment has
    // already been asked about would not be waiting for anything.
    let correspondent = fresh_correspondent();
    provision_account(&env_file, &correspondent, CORRESPONDENT_PASSWORD).await?;
    let correspondent_id = format!("@{correspondent}:{SERVER_NAME}");
    let correspondent_token = login_as(&client, &correspondent, CORRESPONDENT_PASSWORD).await?;
    invite(&client, &owner_token, &room_id, &correspondent_id).await?;
    join(&client, &correspondent_token, &room_id).await?;
    send_message(
        &client,
        &correspondent_token,
        &room_id,
        "bonjour, c'est moi qui écris",
    )
    .await?;

    // The Sensor publishes it, with the correspondent as its subject — the
    // event the projection reads three attributes of.
    let written = poll_deploy(
        || async {
            bus.fetch_room_messages_on(SERVER_NAME, STREAM, MESSAGE_SUBJECT, &room_id)
                .await
                .ok()?
                .into_iter()
                .find(|stored| stored.payload["subject"] == serde_json::json!(correspondent_id))
        },
        "the correspondent's message to reach the bus",
    )
    .await?;
    validate_against_contract(&written.payload, "inbound.message.received")?;

    // And the deployed Gateway puts them in the list of decisions waiting.
    let pending = |contact: String| {
        let client = client.clone();
        let base = base.clone();
        let device_token = device_token.clone();
        async move {
            let listed: serde_json::Value = client
                .get(format!("{base}/api/contacts/pending"))
                .header("cookie", format!("twalk_device={device_token}"))
                .send()
                .await
                .ok()?
                .json()
                .await
                .ok()?;
            let entries = listed["contacts"].as_array()?.clone();
            Some((
                entries
                    .iter()
                    .find(|entry| entry["contact"] == serde_json::json!(contact))
                    .cloned(),
                entries,
            ))
        }
    };
    let (entry, entries) = poll_deploy(
        || async {
            let (entry, entries) = pending(correspondent_id.clone()).await?;
            entry.map(|entry| (entry, entries))
        },
        "the deployed Gateway to list the correspondent as waiting for a decision",
    )
    .await?;
    assert_eq!(
        entry["network"].as_str(),
        Some("matrix"),
        "a room with no bridge marker is native Matrix traffic, here too: {entry}"
    );
    let mut members: Vec<&String> = entry
        .as_object()
        .context("a pending contact is an object")?
        .keys()
        .collect();
    members.sort();
    assert_eq!(
        members,
        vec!["contact", "first_seen", "last_seen", "network"],
        "the deployed Gateway hands out an ID, a network and two instants — no body,          no display name, no network identifier: {entry}"
    );
    assert!(
        !entries
            .iter()
            .any(|entry| entry["contact"] == serde_json::json!(owner_user_id())),
        "the owner's own messages travel through the same room and must not become          decisions the owner has to take about themselves: {entries:?}"
    );

    // The user decides, through the write API, and the correspondent stops
    // waiting.
    let decided = client
        .post(format!("{base}/api/consent/decisions"))
        .header("cookie", format!("twalk_device={device_token}"))
        .json(&serde_json::json!({
            "subject": { "type": "contact", "id": correspondent_id },
            "new_state": "granted",
            "scope": { "networks": ["matrix"] }
        }))
        .send()
        .await?;
    assert_eq!(
        decided.status(),
        reqwest::StatusCode::CREATED,
        "the decision must be recorded: {}",
        decided.text().await.unwrap_or_default()
    );
    let (still_waiting, _) = pending(correspondent_id.clone())
        .await
        .context("the pending list must answer after a decision")?;
    assert!(
        still_waiting.is_none(),
        "a contact the user decided about is not waiting for a decision: {still_waiting:?}"
    );

    // --- ticket #24, the loop closing in the reference deployment: a
    // suggestion on the bus, an approval through the deployed Gateway, and
    // the reply appearing in the real Matrix room, posted by the real Sensor.
    //
    // The persona is the one part stood in for — Hermes does not run in this
    // compose stack yet — so the test publishes the `persona.suggest.produced`
    // a persona would, against the correspondent's **real** message above.
    // Everything after that is the deployment: the Gateway reads the
    // suggestion and its trigger off the bus, checks that the correspondent's
    // consent is still granted (it was decided `granted` a few lines up),
    // publishes `persona.reply.approved.v1`, and the Sensor's outbound
    // consumer posts it into the room.
    let trigger_id = written.payload["id"]
        .as_str()
        .context("the correspondent's event has an id")?
        .to_owned();
    let suggestion = suggestion_event(&trigger_id);
    let suggestion_id = suggestion["id"]
        .as_str()
        .context("the suggestion has an id")?
        .to_owned();
    validate_against_contract(&suggestion, "persona.suggest.produced")?;
    bus.publish_event(SUGGEST_SUBJECT, &suggestion).await?;

    let reply_body = format!("bonjour — réponse approuvée {}", std::process::id());
    let approval = client
        .post(format!("{base}/api/approvals"))
        .header("cookie", format!("twalk_device={device_token}"))
        .json(&serde_json::json!({
            "suggestion_event_id": suggestion_id,
            "final": { "body": reply_body, "format": "text/plain" }
        }))
        .send()
        .await?;
    let status = approval.status();
    let approved: serde_json::Value = serde_json::from_str(&approval.text().await?)?;
    assert_eq!(
        status,
        reqwest::StatusCode::CREATED,
        "the deployed Gateway must approve a granted contact's suggestion: {approved}"
    );
    assert_eq!(approved["publication"].as_str(), Some("published"));
    assert_eq!(approved["edited"].as_bool(), Some(true));

    // The event the Sensor consumes, schema-valid on the deployment's own bus.
    let published = poll_deploy(
        || async {
            bus.fetch_all(STREAM, APPROVED_SUBJECT)
                .await
                .ok()?
                .into_iter()
                .find(|event| event["id"] == approved["event_id"])
        },
        "the approved reply to reach the deployment's bus",
    )
    .await?;
    validate_against_contract(&published, "persona.reply.approved")?;
    assert_eq!(
        published["data"]["target"]["room_id"].as_str(),
        Some(room_id.as_str()),
        "the reply is addressed to the room the message it answers came from: {published}"
    );

    // And the loop closes: the Sensor posts it, so the correspondent sees it
    // in the room. Asserted against the homeserver's own state rather than
    // against anything Twalk says about itself.
    poll_deploy(
        || async {
            room_bodies(&client, &correspondent_token, &room_id)
                .await
                .ok()?
                .into_iter()
                .any(|body| body == reply_body)
                .then_some(())
        },
        "the approved reply to appear in the room, posted by the Sensor",
    )
    .await?;

    // The deployed Gateway logged the bootstrap and none of its credentials.
    // (The store is asserted at the process boundary, in tests/bootstrap.rs,
    // where the test can read the file the Gateway writes.)
    let logs = compose(
        &env_file,
        &["logs", "--no-log-prefix", "companion-gateway"],
        "logs companion-gateway",
    )
    .await?;
    for (what, secret) in [
        ("the owner's Matrix access token", owner_token.as_str()),
        ("the owner's password", OWNER_PASSWORD),
        (
            "the homeserver's registration shared secret",
            REGISTRATION_SHARED_SECRET,
        ),
        ("the issued device token", device_token.as_str()),
        // And #54's own invariant, at the deployment seam: the projection
        // reads three attributes of an inbound event and never its body.
        (
            "the correspondent's message",
            "bonjour, c'est moi qui écris",
        ),
    ] {
        assert!(
            !logs.contains(secret),
            "{what} must not appear in the deployed Gateway's logs"
        );
    }

    if teardown_requested() {
        teardown(&env_file).await?;
    }
    Ok(())
}

/// A correspondent localpart no previous run has used. The Gateway's decision
/// journal lives on a volume that survives between runs, so a correspondent
/// this deployment has already been asked about would not be waiting for a
/// decision — and the test would pass for the wrong reason.
fn fresh_correspondent() -> String {
    format!(
        "g54_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos()
    )
}

/// Provisions one account on the deployment's Synapse the way `provision.sh`
/// does — `register_new_matrix_user` with the registration shared secret the
/// homeserver's own configuration carries. Not through the Gateway's
/// registration relay, which creates the owner's account and refuses every
/// other: a second human on this homeserver is the operator's business.
async fn provision_account(env_file: &Path, localpart: &str, password: &str) -> Result<()> {
    compose_change(
        env_file,
        &[
            "exec",
            "-T",
            "synapse",
            "register_new_matrix_user",
            "-u",
            localpart,
            "-p",
            password,
            "--no-admin",
            "-c",
            "/data/homeserver.yaml",
            "http://localhost:8008",
        ],
        "register a correspondent",
    )
    .await?;
    Ok(())
}

/// Invites a user into a room, as the room's creator.
async fn invite(
    client: &reqwest::Client,
    owner_token: &str,
    room_id: &str,
    user_id: &str,
) -> Result<()> {
    let response = client
        .post(format!(
            "{}/_matrix/client/v3/rooms/{room_id}/invite",
            homeserver_url()
        ))
        .bearer_auth(owner_token)
        .json(&serde_json::json!({ "user_id": user_id }))
        .send()
        .await?;
    anyhow::ensure!(
        response.status().is_success(),
        "the homeserver refused the invitation: {}",
        response.text().await.unwrap_or_default()
    );
    Ok(())
}

/// Accepts an invitation.
async fn join(client: &reqwest::Client, token: &str, room_id: &str) -> Result<()> {
    let response = client
        .post(format!(
            "{}/_matrix/client/v3/rooms/{room_id}/join",
            homeserver_url()
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({}))
        .send()
        .await?;
    anyhow::ensure!(
        response.status().is_success(),
        "the homeserver refused the join: {}",
        response.text().await.unwrap_or_default()
    );
    Ok(())
}

/// Logs any provisioned account in with its password.
async fn login_as(client: &reqwest::Client, localpart: &str, password: &str) -> Result<String> {
    let body: serde_json::Value = serde_json::from_str(
        &client
            .post(format!("{}/_matrix/client/v3/login", homeserver_url()))
            .json(&serde_json::json!({
                "type": "m.login.password",
                "identifier": { "type": "m.id.user", "user": localpart },
                "password": password,
            }))
            .send()
            .await?
            .text()
            .await?,
    )?;
    body["access_token"]
        .as_str()
        .map(str::to_owned)
        .context("the login answered no access token")
}

/// Logs the owner in with a password: only for a warm stack whose account a
/// previous run created. The registration relay's own answer carries a token,
/// so the onboarding path never needs this.
async fn login(client: &reqwest::Client) -> Result<String> {
    let body: serde_json::Value = serde_json::from_str(
        &client
            .post(format!("{}/_matrix/client/v3/login", homeserver_url()))
            .json(&serde_json::json!({
                "type": "m.login.password",
                "identifier": { "type": "m.id.user", "user": OWNER_LOCALPART },
                "password": OWNER_PASSWORD,
            }))
            .send()
            .await?
            .text()
            .await?,
    )?;
    Ok(body["access_token"]
        .as_str()
        .context("the login answered no access token")?
        .to_owned())
}

/// Signs a device in at the deployed Gateway with a Matrix OpenID token, and
/// returns the device cookie.
async fn sign_in(client: &reqwest::Client, base: &str, owner_token: &str) -> Result<String> {
    let openid: serde_json::Value = serde_json::from_str(
        &client
            .post(format!(
                "{}/_matrix/client/v3/user/{}/openid/request_token",
                homeserver_url(),
                owner_user_id()
            ))
            .bearer_auth(owner_token)
            .json(&serde_json::json!({}))
            .send()
            .await?
            .text()
            .await?,
    )?;
    let response = client
        .post(format!("{base}/api/session"))
        .json(&serde_json::json!({
            "matrix_openid_token": openid,
            "device_name": "Onboarding laptop",
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

/// A private, unencrypted room owned by the user: native Matrix traffic.
async fn create_room(client: &reqwest::Client, owner_token: &str) -> Result<String> {
    let body: serde_json::Value = serde_json::from_str(
        &client
            .post(format!("{}/_matrix/client/v3/createRoom", homeserver_url()))
            .bearer_auth(owner_token)
            .json(&serde_json::json!({
                "name": "g53 deployed room",
                "preset": "private_chat",
            }))
            .send()
            .await?
            .text()
            .await?,
    )?;
    Ok(body["room_id"]
        .as_str()
        .context("the createRoom answer names no room id")?
        .to_owned())
}

/// A user's membership in a room, or `None` when there is no member event.
async fn membership(
    client: &reqwest::Client,
    owner_token: &str,
    room_id: &str,
    user_id: &str,
) -> Result<Option<String>> {
    let response = client
        .get(format!(
            "{}/_matrix/client/v3/rooms/{room_id}/state/m.room.member/{user_id}",
            homeserver_url()
        ))
        .bearer_auth(owner_token)
        .send()
        .await?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let body: serde_json::Value = serde_json::from_str(&response.text().await?)?;
    Ok(body["membership"].as_str().map(str::to_owned))
}

/// Sends a text message as the user typing in their own client does.
/// One `persona.suggest.produced.v1` for a real trigger event, as the
/// assistant would publish it (ticket #24). Hermes does not run in this
/// compose stack yet, so this is the one part of the loop the test stands in
/// for; everything downstream of it is the deployment's own.
fn suggestion_event(trigger_id: &str) -> serde_json::Value {
    let now = std::time::SystemTime::now();
    let produced_at = format_rfc3339(now);
    let expires_at = format_rfc3339(now + std::time::Duration::from_secs(3600));
    serde_json::json!({
        "specversion": "1.0",
        "id": harness::sha256_hex(&format!("assistant:{trigger_id}:1")),
        "source": format!("hermes://{SERVER_NAME}/personas/assistant"),
        "type": "fr.linagora.twalk.persona.suggest.produced.v1",
        "time": produced_at,
        "subject": trigger_id,
        "datacontenttype": "application/json",
        "network": "matrix",
        "consent": "granted",
        "data": {
            "persona_id": "assistant",
            "trigger": {
                "event_id": trigger_id,
                "event_type": "fr.linagora.twalk.inbound.message.received.v1"
            },
            "suggestion": { "body": "bonjour !", "format": "text/plain" },
            "attempt": 1,
            "expires_at": expires_at
        }
    })
}

/// RFC 3339, whole seconds, UTC — the spelling every producer in this
/// repository stamps.
fn format_rfc3339(at: std::time::SystemTime) -> String {
    let seconds = at
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs();
    time::OffsetDateTime::from_unix_timestamp(seconds as i64)
        .expect("a unix timestamp is an instant")
        .format(&time::format_description::well_known::Rfc3339)
        .expect("an instant formats as RFC 3339")
}

/// The text of the recent messages in a room, as an account that is in it
/// reads them. How this test asks the **homeserver** whether the approved
/// reply was really posted, rather than believing Twalk's own account of it.
async fn room_bodies(client: &reqwest::Client, token: &str, room_id: &str) -> Result<Vec<String>> {
    let response = client
        .get(format!(
            "{}/_matrix/client/v3/rooms/{room_id}/messages?dir=b&limit=50",
            homeserver_url()
        ))
        .bearer_auth(token)
        .send()
        .await?;
    anyhow::ensure!(
        response.status().is_success(),
        "the homeserver refused to list the room's messages: {}",
        response.text().await.unwrap_or_default()
    );
    let document: serde_json::Value = serde_json::from_str(&response.text().await?)?;
    Ok(document["chunk"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|event| event["content"]["body"].as_str().map(str::to_owned))
        .collect())
}

async fn send_message(
    client: &reqwest::Client,
    owner_token: &str,
    room_id: &str,
    body: &str,
) -> Result<()> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let response = client
        .put(format!(
            "{}/_matrix/client/v3/rooms/{room_id}/send/m.room.message/twalk-g53-{unique}",
            homeserver_url()
        ))
        .bearer_auth(owner_token)
        .json(&serde_json::json!({ "msgtype": "m.text", "body": body }))
        .send()
        .await?;
    anyhow::ensure!(
        response.status().is_success(),
        "the homeserver refused the message: {}",
        response.text().await.unwrap_or_default()
    );
    Ok(())
}
