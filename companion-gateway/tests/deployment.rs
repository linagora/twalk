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
//! `network=matrix`. It brings the `sensor` service up beside the Gateway,
//! which is also where "the Sensor's startup does not depend on the Gateway"
//! is asserted — from compose's own resolved configuration.
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
/// The service token the deployed Gateway serves its consent snapshot to
/// (ticket #50) — long enough for the Gateway's own minimum, and throwaway
/// like the rest of this stack's credentials.
const SERVICE_TOKEN: &str = "deploy-test-only-gateway-service-token";

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
    let contents = format!(
        "MATRIX_DOMAIN={SERVER_NAME}\n\
         MATRIX_HTTP_PORT={synapse_port}\n\
         MATRIX_REGISTRATION_SHARED_SECRET={REGISTRATION_SHARED_SECRET}\n\
         MATRIX_MACAROON_SECRET=deploy-test-only-macaroon-secret\n\
         MATRIX_FORM_SECRET=deploy-test-only-form-secret\n\
         SENSOR_USER_ID=@sensor:{SERVER_NAME}\n\
         SENSOR_PASSWORD=deploy-test-only-password-sensor\n\
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
         GATEWAY_SENSOR_USER_ID=@sensor:{SERVER_NAME}\n\
         GATEWAY_NATS_URL=nats://nats:4222\n\
         GATEWAY_SERVICE_TOKEN={SERVICE_TOKEN}\n\
         TWALK_GATEWAY_IMAGE={gateway_image}\n\
         TWALK_SENSOR_IMAGE={sensor_image}\n"
    );
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
            bus.fetch_room_messages_on(SERVER_NAME, STREAM, MESSAGE_SUBJECT, &room_id)
                .await
                .ok()?
                .into_iter()
                .next()
        },
        "the invited room's traffic to reach the bus",
    )
    .await?;
    let event = &stored.payload;
    validate_against_contract(event, "inbound.message.received")?;
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
