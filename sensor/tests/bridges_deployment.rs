//! Ticket #73, the mautrix bridges in the reference deployment: the two-step
//! procedure `deploy/docker-compose` documents — `provision-bridges.sh`, then
//! `docker compose --profile bridges up` — brings `mautrix-whatsapp` and
//! `mautrix-signal` up against the stack's own Synapse, with Synapse
//! accepting each appservice, each bridge process live, and each bridge's
//! provisioning API answering for the shared secret in the environment file.
//!
//! **No network login is attempted.** Logging a bridge in needs a real
//! WhatsApp or Signal account and a phone to scan a QR code with; that is the
//! Companion's job (#68) and a human's. What this test proves is the
//! deployment: the registration Synapse reads, the process that answers, and
//! the provisioning API the Companion Gateway will drive (#55). Each bridge
//! reports `logins: []` here, and that is the expected state.
//!
//! It lives in the Sensor's suite because the Sensor's crate is where the
//! reference deployment's tests already live (`deployment.rs`), and because
//! the bridge bots this test asserts on are what `SENSOR_ALLOWED_INVITERS`
//! has to name for portal rooms to be observed without manual steps. It runs
//! no Sensor, so it takes no `harness::SENSOR_LOCK` and brings up neither the
//! Sensor nor the bus: `up` names Synapse and the two bridges only, which
//! also means it builds no Rust image.
//!
//! The stack runs under its own compose project and host ports, next to the
//! harness's stack and to the other two deploy-stack tests:
//! TWALK_BRIDGES_TEST_STACK (default twalk-bridges-test),
//! TWALK_BRIDGES_TEST_SYNAPSE_PORT (default 18568),
//! TWALK_BRIDGES_TEST_NATS_PORT (default 14778),
//! TWALK_BRIDGES_TEST_GATEWAY_PORT (default 18578),
//! TWALK_BRIDGES_TEST_WHATSAPP_PORT (default 18588) and
//! TWALK_BRIDGES_TEST_SIGNAL_PORT (default 18598) — all distinct from the
//! defaults of `deployment.rs` (18218 / 14418 / 18328) and of
//! `companion-gateway/tests/deployment.rs` (18318), so several
//! default-configured deploy stacks can sit on one Docker daemon. They sit at
//! the top of their ranges on purpose: an ad-hoc stack in another worktree
//! took 18548 while this test was being written, and a default that loses a
//! race to a neighbour is a default worth moving. Every one of them is
//! overridable for exactly that reason. The bridge images are upstream and
//! pinned by tag in `compose.yaml`, so unlike the Sensor's and the Gateway's
//! there is no per-stack image tag to keep apart.
//!
//! The stack stays up between runs: that is what makes a warm run fast. Set
//! TWALK_BRIDGES_TEST_TEARDOWN=1 to drop it at the end of a passing run
//! instead — by hand, `docker compose -p <stack> --profile bridges down -v`.
//!
//! The credentials below are throwaway constants for the local, ephemeral
//! test stack (same category as the test-bot passwords) — the env file they
//! land in is generated in a temp directory, never committed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::process::Command;
use tokio::time::sleep;

const SERVER_NAME: &str = "deploy.twalk";
const OWNER_USER_ID: &str = "@owner:deploy.twalk";

/// The two bridges, with everything that differs between them: the compose
/// service names, the appservice id Synapse knows them by, the bot the
/// Sensor's allowed inviters must name, and the throwaway secrets this test
/// configures them with. The provisioning secrets are deliberately longer
/// than 16 characters — mautrix answers M_FORBIDDEN to the whole provisioning
/// API below that length.
struct Bridge {
    network: &'static str,
    appservice_id: &'static str,
    bot_user_id: &'static str,
    as_token: &'static str,
    hs_token: &'static str,
    provisioning_secret: &'static str,
    status_endpoint: &'static str,
    port_var: &'static str,
    default_port: &'static str,
}

const BRIDGES: [Bridge; 2] = [
    Bridge {
        network: "whatsapp",
        appservice_id: "whatsapp",
        bot_user_id: "@whatsappbot:deploy.twalk",
        as_token: "bridges-test-only-whatsapp-as-token",
        hs_token: "bridges-test-only-whatsapp-hs-token",
        provisioning_secret: "bridges-test-only-whatsapp-provisioning-secret",
        status_endpoint: "http://companion-gateway:8080/_twalk/bridges/bridge-whatsapp/status",
        port_var: "TWALK_BRIDGES_TEST_WHATSAPP_PORT",
        default_port: "18588",
    },
    Bridge {
        network: "signal",
        appservice_id: "signal",
        bot_user_id: "@signalbot:deploy.twalk",
        as_token: "bridges-test-only-signal-as-token",
        hs_token: "bridges-test-only-signal-hs-token",
        provisioning_secret: "bridges-test-only-signal-provisioning-secret",
        status_endpoint: "http://companion-gateway:8080/_twalk/bridges/bridge-signal/status",
        port_var: "TWALK_BRIDGES_TEST_SIGNAL_PORT",
        default_port: "18598",
    },
];

impl Bridge {
    fn host_port(&self) -> String {
        std::env::var(self.port_var).unwrap_or_else(|_| self.default_port.to_owned())
    }

    fn base_url(&self) -> String {
        format!("http://localhost:{}", self.host_port())
    }

    fn service(&self) -> String {
        format!("bridge-{}", self.network)
    }

    fn registration_service(&self) -> String {
        format!("bridge-{}-registration", self.network)
    }
}

fn bridges_stack() -> String {
    std::env::var("TWALK_BRIDGES_TEST_STACK").unwrap_or_else(|_| "twalk-bridges-test".to_owned())
}

fn env_port(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn synapse_url() -> String {
    format!(
        "http://localhost:{}",
        env_port("TWALK_BRIDGES_TEST_SYNAPSE_PORT", "18568")
    )
}

fn deploy_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../deploy/docker-compose")
}

/// Writes the environment file the stack is configured through, with
/// throwaway test values and this test's own host ports.
///
/// It carries what the services this test starts read, plus every variable
/// `compose.yaml` guards with `:?` — compose interpolates the whole file
/// whatever the active profiles are, so a required variable of a service that
/// never starts is still a hard error. Variables that only other services
/// read (the Gateway's session settings, the Sensor's credentials beyond the
/// guarded ones) are deliberately absent: they have defaults, nothing here
/// reads them, and listing them would be a second copy of `.env.example` to
/// keep in step.
fn write_env_file() -> Result<PathBuf> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "twalk-bridges-deploy-test-{}-{unique}.env",
        std::process::id()
    ));
    let synapse_port = env_port("TWALK_BRIDGES_TEST_SYNAPSE_PORT", "18568");
    let nats_port = env_port("TWALK_BRIDGES_TEST_NATS_PORT", "14778");
    let gateway_port = env_port("TWALK_BRIDGES_TEST_GATEWAY_PORT", "18578");
    let registrations = BRIDGES
        .iter()
        .map(|bridge| format!("/registrations/{}.yaml", bridge.network))
        .collect::<Vec<_>>()
        .join(",");
    // The bridge bots, as an operator has to name them for portal rooms to be
    // observed without manual steps.
    let allowed_inviters = BRIDGES
        .iter()
        .map(|bridge| bridge.bot_user_id.to_owned())
        .collect::<Vec<_>>()
        .join(",");
    let mut contents = format!(
        "MATRIX_DOMAIN={SERVER_NAME}\n\
         MATRIX_HTTP_PORT={synapse_port}\n\
         MATRIX_REGISTRATION_SHARED_SECRET=bridges-test-only-registration-shared-secret\n\
         MATRIX_MACAROON_SECRET=bridges-test-only-macaroon-secret\n\
         MATRIX_FORM_SECRET=bridges-test-only-form-secret\n\
         MATRIX_OWNER_USER_ID={OWNER_USER_ID}\n\
         MATRIX_APPSERVICE_REGISTRATIONS={registrations}\n\
         SENSOR_USER_ID=@sensor:{SERVER_NAME}\n\
         SENSOR_PASSWORD=bridges-test-only-password-sensor\n\
         SENSOR_ALLOWED_INVITERS={allowed_inviters}\n\
         SENSOR_STATE_DIR=/data\n\
         SENSOR_LOG_LEVEL=info\n\
         NATS_PORT={nats_port}\n\
         GATEWAY_HTTP_PORT={gateway_port}\n"
    );
    for bridge in &BRIDGES {
        let prefix = bridge.network.to_uppercase();
        contents.push_str(&format!(
            "{prefix}_APPSERVICE_PORT={}\n\
             {prefix}_AS_TOKEN={}\n\
             {prefix}_HS_TOKEN={}\n\
             {prefix}_PROVISIONING_SECRET={}\n\
             {prefix}_STATUS_ENDPOINT={}\n",
            bridge.host_port(),
            bridge.as_token,
            bridge.hs_token,
            bridge.provisioning_secret,
            bridge.status_endpoint,
        ));
    }
    std::fs::write(&path, contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// Runs `docker compose` against this test's stack with the bridge profile
/// on, and returns its stdout.
async fn compose(env_file: &Path, args: &[&str], what: &str) -> Result<String> {
    let output = Command::new("docker")
        .arg("compose")
        .arg("-p")
        .arg(bridges_stack())
        .arg("--env-file")
        .arg(env_file)
        .arg("-f")
        .arg(deploy_dir().join("compose.yaml"))
        .arg("--profile")
        .arg("bridges")
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

/// The stack's containers as `docker compose ps --format json` reports them:
/// one JSON object per line, one line per service.
async fn compose_ps(env_file: &Path) -> Result<Vec<serde_json::Value>> {
    let stdout = compose(env_file, &["ps", "-a", "--format", "json"], "ps").await?;
    Ok(stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect())
}

fn service_entry<'a>(
    entries: &'a [serde_json::Value],
    service: &str,
) -> Result<&'a serde_json::Value> {
    entries
        .iter()
        .find(|entry| entry["Service"].as_str() == Some(service))
        .with_context(|| format!("the {service} service has no container"))
}

/// The documented first step: generate each bridge's registration, install it
/// in Synapse's configuration and restart Synapse. Run before `up`, as the
/// operator runbook says, so that Synapse starts already knowing both
/// appservices.
async fn provision_bridges(env_file: &Path) -> Result<()> {
    let output = Command::new(deploy_dir().join("provision-bridges.sh"))
        .env("TWALK_ENV_FILE", env_file)
        .env("TWALK_COMPOSE_PROJECT", bridges_stack())
        .output()
        .await
        .context("failed to run provision-bridges.sh")?;
    if !output.status.success() {
        bail!(
            "provision-bridges.sh failed with {}:\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

/// Reads one field out of a bridge's *rendered* configuration, with the yq the
/// bridge image ships. What the generator wrote is the only thing the bridge
/// actually runs on, so this is where a claim about its configuration can be
/// checked.
async fn rendered_config_value(
    env_file: &Path,
    bridge: &Bridge,
    expression: &str,
) -> Result<String> {
    let stdout = compose(
        env_file,
        &[
            "exec",
            "-T",
            &bridge.service(),
            "/usr/bin/yq",
            "e",
            expression,
            "/data/config.yaml",
        ],
        "exec yq",
    )
    .await?;
    Ok(stdout.trim().to_owned())
}

/// `TWALK_BRIDGES_TEST_TEARDOWN=1`: drop the stack at the end of a passing
/// run. Off by default, as in the other deploy-stack tests — a stack left up
/// is what makes the next run warm. The bridge images are upstream and
/// shared, so teardown does not remove them.
fn teardown_requested() -> bool {
    matches!(
        std::env::var("TWALK_BRIDGES_TEST_TEARDOWN").as_deref(),
        Ok("1") | Ok("true")
    )
}

async fn teardown(env_file: &Path) -> Result<()> {
    compose(env_file, &["down", "-v"], "down").await?;
    std::fs::remove_file(env_file)
        .with_context(|| format!("failed to remove {}", env_file.display()))?;
    Ok(())
}

/// Retries an HTTP probe until it answers, for the few seconds a just-started
/// listener needs. `up --wait` already waited for the healthchecks; this
/// covers the gap between a healthy container and a ready host-side port.
async fn poll_response<F, Fut>(what: &str, mut probe: F) -> Result<reqwest::Response>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<reqwest::Response>>,
{
    for _ in 0..60 {
        if let Some(response) = probe().await {
            return Ok(response);
        }
        sleep(Duration::from_millis(500)).await;
    }
    bail!("{what} did not answer within 30s")
}

#[tokio::test]
async fn the_bridge_profile_brings_up_whatsapp_and_signal_against_the_stack() -> Result<()> {
    let env_file = write_env_file()?;

    // Step one of the documented procedure. It generates each registration
    // (which needs no homeserver), installs both in Synapse's configuration,
    // and restarts Synapse if it is already running.
    provision_bridges(&env_file).await?;

    // Step two. Only Synapse and the two bridges: this test needs neither the
    // Sensor nor the bus, and naming them keeps it from building Rust images.
    compose(
        &env_file,
        &[
            "up",
            "-d",
            "--wait",
            "synapse",
            "bridge-whatsapp",
            "bridge-signal",
        ],
        "up",
    )
    .await?;

    let entries = compose_ps(&env_file).await?;
    let client = reqwest::Client::new();

    for bridge in &BRIDGES {
        // The registration one-shot ran and succeeded: that is where both the
        // rendered config and the file Synapse reads come from.
        let registration = service_entry(&entries, &bridge.registration_service())?;
        assert_eq!(
            registration["ExitCode"].as_i64(),
            Some(0),
            "the {} registration one-shot must have succeeded: {registration}",
            bridge.network
        );

        // The bridge process is up, and compose agrees: the healthcheck is
        // the bridge's own unauthenticated liveness endpoint.
        let service = service_entry(&entries, &bridge.service())?;
        assert_eq!(
            service["Health"].as_str(),
            Some("healthy"),
            "compose must report the {} bridge healthy: {service}",
            bridge.network
        );

        // Synapse accepted the appservice — and can reach it. MSC2659's ping
        // authenticates with the bridge's as_token (so Synapse must have the
        // registration, with that exact token) and makes Synapse call the
        // bridge's /_matrix/app/v1/ping with the hs_token (so the bridge must
        // be listening, and agree on the other half of the pair). One request
        // proves the whole appservice handshake.
        let ping = client
            .post(format!(
                "{}/_matrix/client/v1/appservice/{}/ping",
                synapse_url(),
                bridge.appservice_id
            ))
            .bearer_auth(bridge.as_token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .with_context(|| format!("failed to ping the {} appservice", bridge.network))?;
        let status = ping.status();
        let body: serde_json::Value = ping.json().await.unwrap_or(serde_json::Value::Null);
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "Synapse must accept and reach the {} appservice: {body}",
            bridge.network
        );
        assert!(
            body["duration_ms"].is_number(),
            "the {} appservice ping must report a round trip: {body}",
            bridge.network
        );

        // Process liveness, straight from the host, unauthenticated.
        let live = poll_response(
            &format!("the {} bridge's liveness endpoint", bridge.network),
            || {
                let client = client.clone();
                let url = format!("{}/_matrix/mau/live", bridge.base_url());
                async move { client.get(url).send().await.ok() }
            },
        )
        .await?;
        assert_eq!(
            live.status(),
            reqwest::StatusCode::OK,
            "the {} bridge must answer /_matrix/mau/live",
            bridge.network
        );

        // The provisioning API the Companion Gateway will drive (#55),
        // authenticated with the shared secret from the environment file.
        // `user_id` is required even under shared-secret auth: the secret says
        // who may call, the parameter says whom the call is about, and mautrix
        // checks that user against bridge.permissions.
        let whoami = client
            .get(format!(
                "{}/_matrix/provision/v3/whoami?user_id={OWNER_USER_ID}",
                bridge.base_url()
            ))
            .bearer_auth(bridge.provisioning_secret)
            .send()
            .await
            .with_context(|| format!("failed to call the {} bridge's whoami", bridge.network))?;
        let status = whoami.status();
        let body: serde_json::Value = whoami.json().await.unwrap_or(serde_json::Value::Null);
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "the {} bridge's provisioning API must answer whoami for its shared secret: {body}",
            bridge.network
        );
        assert_eq!(
            body["network"]["network_id"].as_str(),
            Some(bridge.network),
            "the {} bridge must identify its own network: {body}",
            bridge.network
        );
        // The bot the Sensor's allowed inviters have to name, as the bridge
        // itself spells it — the env file above configured exactly this.
        assert_eq!(
            body["bridge_bot"].as_str(),
            Some(bridge.bot_user_id),
            "the {} bridge's bot must be the one SENSOR_ALLOWED_INVITERS names: {body}",
            bridge.network
        );
        // No login, and none attempted: that needs a phone.
        assert_eq!(
            body["logins"].as_array().map(Vec::len),
            Some(0),
            "no network login is attempted by this test: {body}",
        );

        // Unauthenticated, the same call is refused. Worth asserting: a
        // provisioning API that answered without the secret would hand anyone
        // who reached the port the user's WhatsApp account.
        let unauthenticated = client
            .get(format!(
                "{}/_matrix/provision/v3/whoami?user_id={OWNER_USER_ID}",
                bridge.base_url()
            ))
            .send()
            .await?;
        assert_eq!(
            unauthenticated.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "the {} bridge's provisioning API must refuse an unauthenticated call",
            bridge.network
        );

        // The status webhook is wired at the Companion Gateway's path (#56's
        // producer), and the management-room notices are left at mautrix's
        // default — the facade spec does not parse them, but silencing them
        // would take away the one signal a human has when the Gateway is down.
        assert_eq!(
            rendered_config_value(&env_file, bridge, ".homeserver.status_endpoint").await?,
            bridge.status_endpoint,
            "the {} bridge must push its status to the Companion Gateway",
            bridge.network
        );
        assert_eq!(
            rendered_config_value(&env_file, bridge, ".bridge.bridge_status_notices").await?,
            "errors",
            "the {} bridge's bridge_status_notices must stay at its upstream default",
            bridge.network
        );
        // And the single owner is the only account allowed to drive it.
        assert_eq!(
            rendered_config_value(
                &env_file,
                bridge,
                ".bridge.permissions | keys | join(\",\")"
            )
            .await?,
            OWNER_USER_ID,
            "the {} bridge must grant only this deployment's owner",
            bridge.network
        );
    }

    // Deliberately after the assertions, so a failure leaves the stack and
    // its logs in place to inspect.
    if teardown_requested() {
        teardown(&env_file).await?;
    }
    Ok(())
}
