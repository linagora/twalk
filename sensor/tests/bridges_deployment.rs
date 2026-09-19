//! Ticket #73, extended by #172: the mautrix bridges in the reference
//! deployment. The two-step procedure `deploy/docker-compose` documents —
//! `provision-bridges.sh`, then `docker compose --profile bridges up` —
//! brings `mautrix-whatsapp`, `mautrix-signal` and `mautrix-gmessages` up
//! against the stack's own Synapse, with Synapse accepting each appservice,
//! each bridge process live, and each bridge's provisioning API answering for
//! the shared secret in the environment file.
//!
//! A bridge is identified by its `bridge_id` and is never a network value
//! (ADR 0005). `gmessages` appears throughout this file as mautrix's own name
//! for that bridge — its binary, its appservice id, its registration file,
//! its compose service and the variables carrying its secrets — and nowhere
//! as a network: the network it serves is `sms`, which is the Companion
//! Gateway's `GATEWAY_BRIDGE_MAUTRIX_GMESSAGES_NETWORK` and not this test's
//! business. `Bridge::mautrix_id` is named for exactly that reason.
//!
//! **No network login is attempted.** Logging a bridge in needs a real
//! account and a human: a phone to scan a QR code with for WhatsApp and
//! Signal, and for Google Messages seven Google session cookies copied out of
//! a private browsing window followed by an emoji match on the phone (#57).
//! That is the Companion's job (#68) and a human's, and no automation may do
//! it. What this test proves is the deployment: the registration Synapse
//! reads, the process that answers, and the provisioning API the Companion
//! Gateway will drive (#55). Each bridge reports `logins: []` here, and that
//! is the expected state.
//!
//! The second test is the other half of #172's acceptance: a stack with **no**
//! bridges comes up, and is never asked for a bridge token. Compose
//! interpolates the whole file whatever the active profiles are, so every
//! bridge variable in `compose.yaml` has an empty default — the day one of
//! them grows a `:?` guard, a deployment that runs no bridge at all stops
//! starting, and that is a defect this file can catch cheaply.
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
//! TWALK_BRIDGES_TEST_WHATSAPP_PORT (default 18588),
//! TWALK_BRIDGES_TEST_SIGNAL_PORT (default 18598) and
//! TWALK_BRIDGES_TEST_GMESSAGES_PORT (default 17736) — all distinct from the
//! defaults of `deployment.rs` (18218 / 14418 / 18328) and of
//! `companion-gateway/tests/deployment.rs` (18318), so several
//! default-configured deploy stacks can sit on one Docker daemon. The first
//! five sit at the top of their ranges on purpose: an ad-hoc stack in another
//! worktree took 18548 while this test was being written, and a default that
//! loses a race to a neighbour is a default worth moving. The two defaults
//! #172 added are in the 17700–17899 block that ticket was given, which is
//! also why the existing five did not move: an operator's `.env` and a warm
//! stack both keep working. Every one of them is overridable. The bridge
//! images are upstream and pinned by tag in `compose.yaml`, so unlike the
//! Sensor's and the Gateway's there is no per-stack image tag to keep apart.
//!
//! The no-bridge test has a project and ports of its own —
//! TWALK_NO_BRIDGES_TEST_STACK (default twalk-no-bridges-test),
//! TWALK_NO_BRIDGES_TEST_SYNAPSE_PORT (default 17708) and
//! TWALK_NO_BRIDGES_TEST_NATS_PORT (default 17718) — because it must not
//! share a project with a stack that *has* bridges: the property it asserts
//! is about a deployment where no bridge variable is set at all.
//!
//! The three-bridge stack stays up between runs: that is what makes a warm
//! run fast. Set TWALK_BRIDGES_TEST_TEARDOWN=1 to drop it at the end of a
//! passing run instead — by hand, `docker compose -p <stack> --profile
//! bridges down -v`. The no-bridge stack always tears itself down, passing or
//! failing: it is two containers and there is nothing to keep warm.
//!
//! The credentials below are throwaway constants for the local, ephemeral
//! test stack (same category as the test-bot passwords) — the env file they
//! land in is generated in a temp directory, never committed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use tokio::process::Command;
use tokio::time::sleep;

const SERVER_NAME: &str = "deploy.twalk";
const OWNER_USER_ID: &str = "@owner:deploy.twalk";

/// The three bridges, with everything that differs between them: the compose
/// service names, the appservice id Synapse knows them by, the bot the
/// Sensor's allowed inviters must name, and the throwaway secrets this test
/// configures them with. The provisioning secrets are deliberately longer
/// than 16 characters — mautrix answers M_FORBIDDEN to the whole provisioning
/// API below that length.
///
/// `mautrix_id` is mautrix's own name for the bridge and is deliberately not
/// called a network: it selects the binary, the appservice id, the compose
/// service, the registration's filename and the `.env` prefix carrying that
/// bridge's secrets, and for the third bridge its value is `gmessages` —
/// a bridge id, never a network (ADR 0005). The network each one serves is
/// the Companion Gateway's configuration and appears nowhere in this file.
struct Bridge {
    mautrix_id: &'static str,
    appservice_id: &'static str,
    bot_user_id: &'static str,
    as_token: &'static str,
    hs_token: &'static str,
    provisioning_secret: &'static str,
    status_endpoint: &'static str,
    port_var: &'static str,
    default_port: &'static str,
}

const BRIDGES: [Bridge; 3] = [
    Bridge {
        mautrix_id: "whatsapp",
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
        mautrix_id: "signal",
        appservice_id: "signal",
        bot_user_id: "@signalbot:deploy.twalk",
        as_token: "bridges-test-only-signal-as-token",
        hs_token: "bridges-test-only-signal-hs-token",
        provisioning_secret: "bridges-test-only-signal-provisioning-secret",
        status_endpoint: "http://companion-gateway:8080/_twalk/bridges/bridge-signal/status",
        port_var: "TWALK_BRIDGES_TEST_SIGNAL_PORT",
        default_port: "18598",
    },
    // SMS, through Google Messages (#172). The bridge is `mautrix-gmessages`
    // and the network it serves is `sms`; the id below is the bridge's, and
    // `bridge-gmessages` is the `bridge_id` its status pushes carry.
    Bridge {
        mautrix_id: "gmessages",
        appservice_id: "gmessages",
        bot_user_id: "@gmessagesbot:deploy.twalk",
        as_token: "bridges-test-only-gmessages-as-token",
        hs_token: "bridges-test-only-gmessages-hs-token",
        provisioning_secret: "bridges-test-only-gmessages-provisioning-secret",
        status_endpoint: "http://companion-gateway:8080/_twalk/bridges/bridge-gmessages/status",
        port_var: "TWALK_BRIDGES_TEST_GMESSAGES_PORT",
        default_port: "17736",
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
        format!("bridge-{}", self.mautrix_id)
    }

    fn registration_service(&self) -> String {
        format!("bridge-{}-registration", self.mautrix_id)
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

/// Writes the environment file the three-bridge stack is configured through,
/// with throwaway test values and this test's own host ports.
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
        .map(|bridge| format!("/registrations/{}.yaml", bridge.mautrix_id))
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
        let prefix = bridge.mautrix_id.to_uppercase();
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
async fn the_bridge_profile_brings_up_all_three_bridges_against_the_stack() -> Result<()> {
    let env_file = write_env_file()?;

    // Step one of the documented procedure. It generates every registration
    // (which needs no homeserver), installs them in Synapse's configuration,
    // and restarts Synapse if it is already running.
    provision_bridges(&env_file).await?;

    // Step two. Only Synapse and the bridges: this test needs neither the
    // Sensor nor the bus, and naming them keeps it from building Rust images.
    let mut up = vec!["up", "-d", "--wait", "synapse"];
    let services: Vec<String> = BRIDGES.iter().map(Bridge::service).collect();
    up.extend(services.iter().map(String::as_str));
    compose(&env_file, &up, "up").await?;

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
            bridge.mautrix_id
        );

        // The bridge process is up, and compose agrees: the healthcheck is
        // the bridge's own unauthenticated liveness endpoint.
        let service = service_entry(&entries, &bridge.service())?;
        assert_eq!(
            service["Health"].as_str(),
            Some("healthy"),
            "compose must report the {} bridge healthy: {service}",
            bridge.mautrix_id
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
            .with_context(|| format!("failed to ping the {} appservice", bridge.mautrix_id))?;
        let status = ping.status();
        let body: serde_json::Value = ping.json().await.unwrap_or(serde_json::Value::Null);
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "Synapse must accept and reach the {} appservice: {body}",
            bridge.mautrix_id
        );
        assert!(
            body["duration_ms"].is_number(),
            "the {} appservice ping must report a round trip: {body}",
            bridge.mautrix_id
        );

        // Process liveness, straight from the host, unauthenticated.
        let live = poll_response(
            &format!("the {} bridge's liveness endpoint", bridge.mautrix_id),
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
            bridge.mautrix_id
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
            .with_context(|| format!("failed to call the {} bridge's whoami", bridge.mautrix_id))?;
        let status = whoami.status();
        let body: serde_json::Value = whoami.json().await.unwrap_or(serde_json::Value::Null);
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "the {} bridge's provisioning API must answer whoami for its shared secret: {body}",
            bridge.mautrix_id
        );
        // mautrix's own `network_id`, which is its name for the bridge and
        // not the contract's `network`: this is where `gmessages` is a
        // legitimate value and `sms` would be wrong (ADR 0005). The Sensor
        // folds these ids into networks in `network::Network::from_bridge_id`
        // and refuses `gmessages` as a network value; the mapping from this
        // bridge to `sms` lives in the Companion Gateway's configuration.
        assert_eq!(
            body["network"]["network_id"].as_str(),
            Some(bridge.mautrix_id),
            "the {} bridge must identify itself by its own mautrix id: {body}",
            bridge.mautrix_id
        );
        // The bot the Sensor's allowed inviters have to name, as the bridge
        // itself spells it — the env file above configured exactly this.
        assert_eq!(
            body["bridge_bot"].as_str(),
            Some(bridge.bot_user_id),
            "the {} bridge's bot must be the one SENSOR_ALLOWED_INVITERS names: {body}",
            bridge.mautrix_id
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
            bridge.mautrix_id
        );

        // The status webhook is wired at the Companion Gateway's path (#56's
        // producer), and the management-room notices are left at mautrix's
        // default — the facade spec does not parse them, but silencing them
        // would take away the one signal a human has when the Gateway is down.
        assert_eq!(
            rendered_config_value(&env_file, bridge, ".homeserver.status_endpoint").await?,
            bridge.status_endpoint,
            "the {} bridge must push its status to the Companion Gateway",
            bridge.mautrix_id
        );
        assert_eq!(
            rendered_config_value(&env_file, bridge, ".bridge.bridge_status_notices").await?,
            "errors",
            "the {} bridge's bridge_status_notices must stay at its upstream default",
            bridge.mautrix_id
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
            bridge.mautrix_id
        );
    }

    // Deliberately after the assertions, so a failure leaves the stack and
    // its logs in place to inspect.
    if teardown_requested() {
        teardown(&env_file).await?;
    }
    Ok(())
}

// --- A deployment with no bridges at all ------------------------------------

fn no_bridges_stack() -> String {
    std::env::var("TWALK_NO_BRIDGES_TEST_STACK")
        .unwrap_or_else(|_| "twalk-no-bridges-test".to_owned())
}

/// Runs `docker compose` against the no-bridge stack and returns its stdout
/// on success, or its stderr on failure. A failure is a value here rather
/// than an error: whether interpolation succeeds is what the first assertion
/// below is about, and compose's own message is the evidence worth printing.
///
/// `profile` is normally `false` — no profile is the whole point. One
/// assertion turns it on deliberately, to *render* the bridge services of an
/// environment that configures none of them: `config` applies the active
/// profiles, so a service behind an inactive one is not in the output at all
/// and could not be inspected.
async fn compose_no_bridges(
    env_file: &Path,
    profile: bool,
    args: &[&str],
    what: &str,
) -> Result<std::result::Result<String, String>> {
    let mut command = Command::new("docker");
    command
        .arg("compose")
        .arg("-p")
        .arg(no_bridges_stack())
        .arg("--env-file")
        .arg(env_file)
        .arg("-f")
        .arg(deploy_dir().join("compose.yaml"));
    if profile {
        command.arg("--profile").arg("bridges");
    }
    let output = command
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to run docker compose {what}"))?;
    if output.status.success() {
        Ok(Ok(String::from_utf8_lossy(&output.stdout).into_owned()))
    } else {
        Ok(Err(String::from_utf8_lossy(&output.stderr).into_owned()))
    }
}

/// The environment file of a deployment that runs no bridge: exactly the
/// variables `compose.yaml` guards with `:?` plus the two host ports this
/// stack publishes, and **not one bridge variable**. That absence is the
/// fixture — the file is the `.env` of an operator who never enabled the
/// profile.
fn write_no_bridges_env_file() -> Result<PathBuf> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "twalk-no-bridges-deploy-test-{}-{unique}.env",
        std::process::id()
    ));
    let synapse_port = env_port("TWALK_NO_BRIDGES_TEST_SYNAPSE_PORT", "17708");
    let nats_port = env_port("TWALK_NO_BRIDGES_TEST_NATS_PORT", "17718");
    std::fs::write(
        &path,
        format!(
            "MATRIX_DOMAIN={SERVER_NAME}\n\
             MATRIX_HTTP_PORT={synapse_port}\n\
             MATRIX_REGISTRATION_SHARED_SECRET=no-bridges-test-only-registration-shared-secret\n\
             MATRIX_MACAROON_SECRET=no-bridges-test-only-macaroon-secret\n\
             MATRIX_FORM_SECRET=no-bridges-test-only-form-secret\n\
             MATRIX_OWNER_USER_ID={OWNER_USER_ID}\n\
             MATRIX_APPSERVICE_REGISTRATIONS=\n\
             SENSOR_USER_ID=@sensor:{SERVER_NAME}\n\
             SENSOR_PASSWORD=no-bridges-test-only-password-sensor\n\
             SENSOR_ALLOWED_INVITERS={OWNER_USER_ID}\n\
             SENSOR_STATE_DIR=/data\n\
             SENSOR_LOG_LEVEL=info\n\
             NATS_PORT={nats_port}\n"
        ),
    )
    .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// The assertions, as a fallible function rather than a body full of
/// `assert!`: the caller tears the stack down whether this succeeds or fails,
/// and a panic here would skip that.
async fn check_a_bridgeless_stack(env_file: &Path) -> Result<()> {
    // 1. The file interpolates at all with no bridge variable set. This is
    //    the assertion #172 is really about: compose interpolates the whole
    //    file whatever the active profiles are, so a `:?` guard on a bridge
    //    token — or a `gmessages` one demanded where WhatsApp's and Signal's
    //    are optional — would stop a bridgeless deployment dead, with an
    //    error about a network its operator does not use.
    if let Err(stderr) = compose_no_bridges(env_file, false, &["config"], "config").await? {
        bail!(
            "a deployment with no bridge variable set must still interpolate; \
             compose refused it:\n{stderr}"
        );
    }

    // 2. No bridge service is planned. `config --services` applies the active
    //    profiles, and none is active here.
    let services = compose_no_bridges(
        env_file,
        false,
        &["config", "--services"],
        "config --services",
    )
    .await?
    .map_err(|stderr| anyhow::anyhow!("docker compose config --services failed:\n{stderr}"))?;
    let planned: Vec<&str> = services
        .lines()
        .map(str::trim)
        .filter(|service| service.starts_with("bridge-"))
        .collect();
    ensure!(
        planned.is_empty(),
        "a default `up` must plan no bridge service, got {planned:?}"
    );

    // 3. And every bridge secret renders EMPTY rather than failing — even
    //    with the profile turned on, which is the stronger statement and the
    //    one that says where the missing-value error is supposed to come
    //    from. An operator who enables the profile without setting a token
    //    gets a one-shot that stops with a sentence naming the variable
    //    (`bridges/generate-registration.sh`), never an interpolation error
    //    about a network they may not even be trying to run.
    let rendered = compose_no_bridges(
        env_file,
        true,
        &["config", "--format", "json"],
        "config --format json",
    )
    .await?
    .map_err(|stderr| {
        anyhow::anyhow!(
            "the bridge profile must interpolate even when no bridge token is set:\n{stderr}"
        )
    })?;
    let rendered: serde_json::Value =
        serde_json::from_str(&rendered).context("docker compose config --format json")?;
    let rendered_services = rendered["services"]
        .as_object()
        .context("the rendered configuration has no services")?;
    for bridge in &BRIDGES {
        let service = format!("bridge-{}-registration", bridge.mautrix_id);
        let environment = rendered_services
            .get(&service)
            .and_then(|service| service["environment"].as_object())
            .with_context(|| format!("{service} is missing from the rendered configuration"))?;
        for variable in [
            "BRIDGE_AS_TOKEN",
            "BRIDGE_HS_TOKEN",
            "BRIDGE_PROVISIONING_SECRET",
            "BRIDGE_STATUS_ENDPOINT",
        ] {
            let value = environment
                .get(variable)
                .with_context(|| format!("{service} does not set {variable}"))?;
            ensure!(
                value.as_str() == Some(""),
                "{service}'s {variable} must render empty on a stack that sets no bridge \
                 variable, got {value}"
            );
        }
    }

    // 4. And the stack comes up. Synapse is where an appservice registration
    //    is installed, so it is the service that would refuse to start on a
    //    registration path left behind by a bridge this stack does not run.
    //    The Sensor and the Gateway are deliberately not started: building
    //    their images proves nothing this test is about.
    compose_no_bridges(
        env_file,
        false,
        &["up", "-d", "--wait", "synapse", "nats"],
        "up",
    )
    .await?
    .map_err(|stderr| anyhow::anyhow!("a stack with no bridges must come up:\n{stderr}"))?;
    Ok(())
}

/// #172's other half: adding a third bridge must not make a deployment that
/// runs none of them harder to start.
///
/// A stack with no bridges must never be asked for a gmessages token. Every
/// bridge variable in `compose.yaml` therefore has an empty default, and the
/// missing-value errors come from `bridges/generate-registration.sh`, which
/// can say what is missing and why.
///
/// It tears its stack down at the end of every run, passing or failing: two
/// containers, nothing worth keeping warm, and a stack that outlived the test
/// would hold this project's name and ports against the next one.
#[tokio::test]
async fn a_stack_with_no_bridges_comes_up_and_is_asked_for_no_bridge_token() -> Result<()> {
    let env_file = write_no_bridges_env_file()?;
    let outcome = check_a_bridgeless_stack(&env_file).await;
    let teardown = compose_no_bridges(&env_file, false, &["down", "-v"], "down").await;
    let _ = std::fs::remove_file(&env_file);

    outcome?;
    teardown?
        .map_err(|stderr| anyhow::anyhow!("failed to tear the no-bridge stack down:\n{stderr}"))?;
    Ok(())
}
