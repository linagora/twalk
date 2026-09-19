//! Ticket #73, extended by #172 and #175: the mautrix bridges in the reference
//! deployment. The two-step procedure `deploy/docker-compose` documents —
//! `provision-bridges.sh`, then `docker compose --profile bridges up` —
//! brings `mautrix-whatsapp`, `mautrix-signal`, `mautrix-gmessages` and
//! `mautrix-telegram` up against the stack's own Synapse, with Synapse
//! accepting each appservice, each bridge process live, and each bridge's
//! provisioning API answering for the shared secret in the environment file.
//!
//! A bridge is identified by its `bridge_id` and is never a network value
//! (ADR 0005). `gmessages` appears throughout this file as mautrix's own name
//! for that bridge — its binary, its appservice id, its registration file,
//! its compose service and the variables carrying its secrets — and nowhere
//! as a network: the network it serves is `sms`, which is the Companion
//! Gateway's `GATEWAY_BRIDGE_MAUTRIX_GMESSAGES_NETWORK` and not this test's
//! business. `Bridge::mautrix_id` is named for exactly that reason. `telegram`
//! is the bridge whose id and network value happen to coincide, and it is read
//! here only as the bridge id; nothing in this file derives one from the other.
//!
//! #175 adds the fourth bridge, and with it the first credential in this
//! deployment that is not a Matrix credential: `network.api_id` and
//! `network.api_hash`, an application the OPERATOR registers at
//! <https://my.telegram.org/apps>. Three things about it are asserted here
//! because they are the deployment's, not the Companion's: that the generator
//! refuses to run without it and says where to get one, that a stack with no
//! bridges is never asked for it, and that `api_id` reaches the bridge as a
//! YAML **integer** — mautrix's config upgrader copies that key with an int
//! type and silently skips a node of any other type, so a quoted value would
//! leave upstream's sample credential in place instead of failing, and the
//! failure would be a flood ban from Telegram rather than a config error.
//!
//! **No network login is attempted.** Logging a bridge in needs a real
//! account and a human: a phone to scan a QR code with for WhatsApp and
//! Signal, for Google Messages seven Google session cookies copied out of a
//! private browsing window followed by an emoji match on the phone (#57), and
//! for Telegram a phone number, the code Telegram sends and a password if the
//! account has two-factor authentication on. That is the Companion's job (#68)
//! and a human's, and no automation may do it. What this test proves is the
//! deployment: the registration Synapse reads, the process that answers, and
//! the provisioning API the Companion Gateway will drive (#55). Each bridge
//! reports `logins: []` here, and that is the expected state.
//!
//! Which is also why the Telegram API credential this test configures is
//! harmless. A bridge contacts Telegram only when a login starts, and no login
//! starts here, so the synthetic pair below never leaves this machine — and
//! neither value could be mistaken for a real one (`1` is not a shape Telegram
//! issues, and the hash is a sentence). A real `api_id` must never be shared or
//! published: Telegram answers `API_ID_PUBLISHED_FLOOD` to one that has been,
//! and then it works for nobody.
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
//! TWALK_BRIDGES_TEST_SIGNAL_PORT (default 18598),
//! TWALK_BRIDGES_TEST_GMESSAGES_PORT (default 17736) and
//! TWALK_BRIDGES_TEST_TELEGRAM_PORT (default 17906) — all distinct from the
//! defaults of `deployment.rs` (18218 / 14418 / 18328) and of
//! `companion-gateway/tests/deployment.rs` (18318), so several
//! default-configured deploy stacks can sit on one Docker daemon. The first
//! five sit at the top of their ranges on purpose: an ad-hoc stack in another
//! worktree took 18548 while this test was being written, and a default that
//! loses a race to a neighbour is a default worth moving. The two defaults
//! #172 added are in the 17700–17899 block that ticket was given, and #175's
//! is in the 17900–18099 block this one was given — which is also why none of
//! the earlier defaults moved: an operator's `.env` and a warm stack both keep
//! working. Every one of them is overridable. The bridge
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
//! The four-bridge stack stays up between runs: that is what makes a warm
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

/// The four bridges, with everything that differs between them: the compose
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
///
/// `extra_env` carries what one bridge needs and the others do not, spelled in
/// full rather than under the `{PREFIX}_` scheme: today that is Telegram's
/// network API credential, which is not a Matrix credential at all.
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
    extra_env: &'static [(&'static str, &'static str)],
}

const BRIDGES: [Bridge; 4] = [
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
        extra_env: &[],
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
        extra_env: &[],
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
        extra_env: &[],
    },
    // Telegram (#175). The bridge is `mautrix-telegram` and the network it
    // serves is `telegram`: the same word, by coincidence rather than by rule,
    // and the id below is the bridge's. `bridge-telegram` is the `bridge_id`
    // its status pushes carry.
    //
    // The last two are the first credential in this deployment that belongs to
    // the external network rather than to Matrix, and they are synthetic in a
    // way no real pair could be: Telegram issues six- to eight-digit ids, and
    // an api_hash is 32 hex characters. Nothing sends them anywhere — a bridge
    // contacts Telegram only when a login starts, and this test starts none —
    // but a value that could be mistaken for a real credential has no business
    // in a repository at all, because Telegram flood-bans a published api_id.
    Bridge {
        mautrix_id: "telegram",
        appservice_id: "telegram",
        bot_user_id: "@telegrambot:deploy.twalk",
        as_token: "bridges-test-only-telegram-as-token",
        hs_token: "bridges-test-only-telegram-hs-token",
        provisioning_secret: "bridges-test-only-telegram-provisioning-secret",
        status_endpoint: "http://companion-gateway:8080/_twalk/bridges/bridge-telegram/status",
        port_var: "TWALK_BRIDGES_TEST_TELEGRAM_PORT",
        default_port: "17906",
        extra_env: &[
            ("TELEGRAM_API_ID", TELEGRAM_TEST_API_ID),
            ("TELEGRAM_API_HASH", TELEGRAM_TEST_API_HASH),
        ],
    },
];

/// The synthetic Telegram API credential above, named so that the assertions
/// about how it is rendered can read the same constants the environment file
/// was written from. `api_id` is the value whose YAML **type** matters: it must
/// reach the bridge as an integer.
const TELEGRAM_TEST_API_ID: &str = "1";
const TELEGRAM_TEST_API_HASH: &str = "bridges-test-only-telegram-api-hash";

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
    // The bridge bots, as an operator has to name them **twice**: in
    // SENSOR_ALLOWED_INVITERS, so portal rooms are observed without manual
    // steps, and in SENSOR_BRIDGE_BOTS, so nothing is published about the bots
    // themselves (#152, ADR 0026). Same value here because this stack has no
    // other allowed inviter; on a real deployment the first list also names
    // the operator and the second must not, which is why they are two lists.
    let bridge_bots = BRIDGES
        .iter()
        .map(|bridge| bridge.bot_user_id.to_owned())
        .collect::<Vec<_>>()
        .join(",");
    let allowed_inviters = bridge_bots.clone();
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
         SENSOR_BRIDGE_BOTS={bridge_bots}\n\
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
        for (name, value) in bridge.extra_env {
            contents.push_str(&format!("{name}={value}\n"));
        }
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

/// Runs `docker compose` against this test's stack and returns its stdout on
/// success or its stderr on failure, rather than treating failure as an error.
/// One assertion is about a one-shot that is *supposed* to stop, and its
/// message is the evidence worth reading.
async fn compose_allowing_failure(
    env_file: &Path,
    args: &[&str],
    what: &str,
) -> Result<std::result::Result<String, String>> {
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
    if output.status.success() {
        Ok(Ok(String::from_utf8_lossy(&output.stdout).into_owned()))
    } else {
        // The generator writes its refusal to stderr; a compose one-shot's own
        // failure lands there too, so both are in the same string.
        Ok(Err(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )))
    }
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
async fn the_bridge_profile_brings_up_all_four_bridges_against_the_stack() -> Result<()> {
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
            "the {} bridge's bot must be the one SENSOR_ALLOWED_INVITERS and \
             SENSOR_BRIDGE_BOTS name: {body}",
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
        // SQLite, for every bridge in the reference stack. mau.fi's Go setup
        // page says PostgreSQL 16 or higher; `sqlite3-fk-wal` is supported on
        // identical terms and is what this deployment runs, which is why the
        // claim is asserted against the running bridge's own configuration
        // rather than believed. It is also #175's fourth open question: adding
        // Telegram did NOT add a database service, and this is what says so.
        assert_eq!(
            rendered_config_value(&env_file, bridge, ".database.type").await?,
            "sqlite3-fk-wal",
            "the {} bridge must run on SQLite like the rest of the stack",
            bridge.mautrix_id
        );
    }

    check_the_telegram_bridges_own_decisions(&env_file).await?;

    // Deliberately after the assertions, so a failure leaves the stack and
    // its logs in place to inspect.
    if teardown_requested() {
        teardown(&env_file).await?;
    }
    Ok(())
}

/// What the Telegram bridge decides that the other three have no equivalent
/// for (#175). Everything above is uniform across the four bridges; this is
/// the part that is Telegram's alone, and every value here is read back from
/// the configuration the running bridge was rendered with.
async fn check_the_telegram_bridges_own_decisions(env_file: &Path) -> Result<()> {
    let telegram = BRIDGES
        .iter()
        .find(|bridge| bridge.mautrix_id == "telegram")
        .context("the telegram bridge is missing from BRIDGES")?;

    // 1. The network API credential arrived, and arrived as an INTEGER.
    //
    //    This is the assertion that earns its place. `network.api_id` is an
    //    int in the bridge's configuration, and mautrix's config upgrader
    //    copies that key with an int type and silently SKIPS a node of any
    //    other type. A generator that rendered it as a quoted string would
    //    therefore leave upstream's sample credential in place, the bridge
    //    would start, and the first login would be answered by Telegram with
    //    a flood ban rather than by the bridge with a configuration error.
    //    The value and the type are two different claims, so both are made.
    assert_eq!(
        rendered_config_value(env_file, telegram, ".network.api_id").await?,
        TELEGRAM_TEST_API_ID,
        "the telegram bridge must be configured with the api_id from the environment"
    );
    assert_eq!(
        rendered_config_value(env_file, telegram, ".network.api_id | tag").await?,
        "!!int",
        "network.api_id must reach the bridge as a YAML integer: mautrix's config \
         upgrader copies that key with an int type and silently skips a node of any \
         other type, so a quoted value would leave upstream's SAMPLE credential in \
         place and Telegram would answer API_ID_PUBLISHED_FLOOD"
    );
    assert_eq!(
        rendered_config_value(env_file, telegram, ".network.api_hash").await?,
        TELEGRAM_TEST_API_HASH,
        "the telegram bridge must be configured with the api_hash from the environment"
    );
    // And the committed base config carries no credential at all — which is a
    // claim about this repository rather than about the running bridge, and the
    // reason the two assertions above had to come from the environment. A
    // plausible-looking placeholder here would be worse than an empty value:
    // Telegram answers API_ID_PUBLISHED_FLOOD to a published or shared api_id,
    // so a committed sample is a credential that works for nobody rather than
    // a configuration error somebody notices.
    let base_config = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../bridges/mautrix-telegram/config.yaml"),
    )
    .context("failed to read the committed telegram base config")?;
    for forbidden in ["api_id:", "api_hash:", "tjyd5yge35lbodk1xwzw2jstp90k55qz"] {
        ensure!(
            !base_config.contains(forbidden),
            "bridges/mautrix-telegram/config.yaml must commit no Telegram API credential, \
             but it contains {forbidden:?}: the operator's own api_id is rendered from the \
             environment, and a shared or published one draws API_ID_PUBLISHED_FLOOD"
        );
    }

    // 2. How big the register's jump is at login. Unlike WhatsApp, which
    //    builds portals lazily as conversations become active, this bridge
    //    syncs the chat list at login: these three values are what decide how
    //    many portal rooms exist the moment a human finishes logging in, and
    //    therefore how long the conversation chooser's list is (#143) the
    //    first time the user sees it. They are pinned rather than inherited
    //    precisely so that an upstream change cannot move that number under a
    //    working deployment, which is what this assertion protects.
    for (expression, expected, why) in [
        (
            ".network.sync.create_limit",
            "15",
            "portals created when the chat list is synced: reviewable in one sitting,              and not 0 (the deafness #105 was about) or -1 (a chooser nobody reads)",
        ),
        (
            ".network.sync.login_sync_limit",
            "15",
            "kept equal to create_limit, so the list the chooser holds is the list that              appeared at login rather than one that grows while it is read",
        ),
        (
            ".network.sync.direct_chats",
            "true",
            "a DM is the conversation Twalk is most about; off, the commonest kind is              invisible until somebody writes",
        ),
        (
            ".network.member_list.max_initial_sync",
            "100",
            "what makes the chooser's member count real rather than decorative, and a              floor and not a count above it",
        ),
        (
            ".network.member_list.sync_broadcast_channels",
            "false",
            "a channel's subscribers are not the user's correspondents, and syncing them              manufactures ghosts wholesale",
        ),
        (
            ".network.max_member_count",
            "-1",
            "a decision AGAINST a cap: a capped portal is never created, so the              conversation never appears in the register at all and nothing says why              (ADR 0024 refuses silent omission)",
        ),
        (
            ".network.bridge_communities",
            "false",
            "a bridged community is itself a portal and carries m.bridge, so the register              would offer a space as a conversation; nothing here reads              com.beeper.room_type_v2 yet",
        ),
        (
            ".network.always_tombstone_on_supergroup_migration",
            "false",
            "true would replace a promoted group's portal with a room the Sensor is not              in, so a conversation the user chose to observe would go deaf silently,              AFTER they decided about it — the register and the Sensor's membership are              both keyed on room id",
        ),
        (
            ".network.device_info.device_model",
            "Twalk",
            "what the user sees in Telegram's own device list: the product, not the              software",
        ),
    ] {
        assert_eq!(
            rendered_config_value(env_file, telegram, expression).await?,
            expected,
            "the telegram bridge's {expression} must be {expected} — {why}"
        );
    }

    // 3. And the generator refuses to run without the credential, with a
    //    message that names it and says where to get one.
    //
    //    This is the other half of the empty-default discipline that
    //    `compose.yaml` states for the bridge tokens and the no-bridge test
    //    below asserts: a stack that runs no bridge must never be asked for a
    //    Telegram credential, so the variable has an empty default and THIS is
    //    where the error has to come from. A one-off `run --rm --no-deps`
    //    disturbs nothing: the refusal happens before the generator copies the
    //    base config, so the rendered configuration the running bridge is
    //    using is not touched.
    let refusal = compose_allowing_failure(
        env_file,
        &[
            "run",
            "--rm",
            "--no-deps",
            "-T",
            "-e",
            "BRIDGE_TELEGRAM_API_ID=",
            &telegram.registration_service(),
        ],
        "run bridge-telegram-registration with no api_id",
    )
    .await?;
    let message = match refusal {
        Ok(stdout) => bail!(
            "the telegram registration one-shot must refuse to run with no api_id,              but it succeeded:\n{stdout}"
        ),
        Err(message) => message,
    };
    assert!(
        message.contains("https://my.telegram.org/apps"),
        "the refusal must say where the operator gets an api_id, because nothing else          in the deployment can tell them: {message}"
    );
    assert!(
        message.contains("TELEGRAM_API_ID"),
        "the refusal must name the variable that is missing: {message}"
    );
    assert!(
        message.contains("NOT a Matrix credential"),
        "the refusal must say what KIND of credential this is: every other secret in          this deployment is a Matrix credential, and an operator who reads 'api_id' as          one will look for it on their homeserver: {message}"
    );
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
    //    are optional, or a Telegram api_id demanded of everybody — would stop
    //    a bridgeless deployment dead, with an error about a network its
    //    operator does not use.
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
        // And the same for whatever that bridge needs on its own. Today this is
        // Telegram's network API credential, and it is the strongest case for
        // the rule: an operator who runs only WhatsApp must never be asked for
        // an application they would have to register with Telegram, and the
        // variable is not even a Matrix credential they could produce from
        // their own homeserver. The error belongs to
        // `bridges/generate-registration.sh`, which knows what it is and where
        // to get one.
        for (variable, _) in bridge.extra_env {
            let variable = format!("BRIDGE_{variable}");
            let value = environment
                .get(&variable)
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

/// #172's other half, which #175 inherits: adding a bridge must not make a
/// deployment that runs none of them harder to start.
///
/// A stack with no bridges must never be asked for a gmessages token, and
/// #175 raises the stakes — it must not be asked for a Telegram API
/// credential either, which is not a Matrix credential at all and which an
/// operator could not produce from their own infrastructure if they wanted
/// to. Every bridge variable in `compose.yaml` therefore has an empty default,
/// and the missing-value errors come from
/// `bridges/generate-registration.sh`, which can say what is missing and
/// why — and, for Telegram, where to get it.
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
