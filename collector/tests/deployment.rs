//! Ticket #279: the collector in the reference deployment. The two-step
//! procedure `deploy/README.md` documents — `provision-connection.sh`, then
//! `docker compose --profile collector up -d --wait` — has a first step no
//! automation may take: an operator signs in at their organisation's SSO, in
//! a browser, as the owner. So this test proves the deployment **around**
//! that step: the image builds, the service starts on a `.env` filled as
//! the README says, and — with no grant in its volume — it reaches the
//! stack's bus and says so, one `connection.status.changed.v1` per
//! connection it holds, `reconnect_required` with the command to run in the
//! hint. That is the state a fresh deployment is in the minute before the
//! operator runs step 1, and the state the Companion shows them.
//!
//! The entrypoint's own refusal is proved too: a client secret readable by
//! group or others is refused before the binary runs, naming `chmod 0600` —
//! the one check between an operator's file and a token request.
//!
//! **No SSO is contacted.** With no grant there is nothing to renew, so the
//! issuer and the two services in the `.env` are names nothing resolves;
//! the collector's process suites prove the grant, the services and the
//! registry against the harness's fakes. **No Companion Gateway runs**: the
//! `.env` carries no `GATEWAY_SERVICE_TOKEN`, so the collector reads no
//! registry and says so at start — a real deployment's `.env` has the token,
//! and the collector then refuses a connection the Gateway does not name
//! (`status.rs` proves that refusal at the process boundary). The stack this
//! test brings up is `nats` and `collector`, named explicitly.
//!
//! The stack runs under its own compose project and host ports, next to the
//! harness's stack and the other deploy-stack tests:
//! TWALK_COLLECTOR_DEPLOY_TEST_STACK (default twalk-collector-deploy-test),
//! TWALK_COLLECTOR_DEPLOY_TEST_NATS_PORT (default 17608). Synapse and the
//! Gateway are not started, but compose interpolates the whole file whatever
//! the active profiles are, so their required variables are set to throwaway
//! values and their ports to ones in the 17600 block nothing listens on. The
//! image is tagged per compose project (`twalk/collector:<stack>`), so two
//! worktrees never overwrite each other's build. The stack stays up between
//! runs; set TWALK_COLLECTOR_DEPLOY_TEST_TEARDOWN=1 to drop it at the end of
//! a passing run — by hand, `docker compose -p <stack> --profile collector
//! down -v`.
//!
//! The credentials below are throwaway constants for the local, ephemeral
//! test stack; the env file and the secret file they land in are generated
//! in a temp directory, never committed.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;
use tokio::process::Command;
use tokio::time::sleep;
use twalk_test_harness::{validate_against_contract, Bus};

const STREAM: &str = "twalk";
const STATUS_SUBJECT: &str = "twalk.connection.status.changed.v1";
const OWNER_EMAIL: &str = "owner@deploy.twalk";
const MAIL_CONNECTION: &str = "mail-deploy";
const CALENDAR_CONNECTION: &str = "calendar-deploy";

fn deploy_stack() -> String {
    std::env::var("TWALK_COLLECTOR_DEPLOY_TEST_STACK")
        .unwrap_or_else(|_| "twalk-collector-deploy-test".to_owned())
}

fn nats_port() -> String {
    std::env::var("TWALK_COLLECTOR_DEPLOY_TEST_NATS_PORT").unwrap_or_else(|_| "17608".to_owned())
}

fn nats_url() -> String {
    format!("nats://localhost:{}", nats_port())
}

/// The tag the stack's collector image is built and run under: one per
/// compose project (issue #38's rule), the operator's `twalk/collector:local`
/// untouched. TWALK_COLLECTOR_IMAGE overrides it, as it does for an operator.
fn collector_image() -> String {
    std::env::var("TWALK_COLLECTOR_IMAGE")
        .ok()
        .filter(|image| !image.is_empty())
        .unwrap_or_else(|| format!("twalk/collector:{}", deploy_stack()))
}

fn deploy_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../deploy/docker-compose")
}

fn temp_path(name: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "twalk-collector-deploy-test-{}-{unique}-{name}",
        std::process::id()
    ))
}

/// The client secret as an operator keeps it: a file of their own, the mode
/// the entrypoint requires — or, for the refusal, one it does not.
fn write_secret_file(mode: u32) -> Result<PathBuf> {
    let path = temp_path("client-secret");
    std::fs::write(&path, "deploy-test-only-client-secret\n")
        .with_context(|| format!("failed to write {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
    Ok(path)
}

/// The environment file the stack is configured through: the collector's
/// section of `.env.example` filled in as the README says, with names
/// nothing resolves where a real deployment has its SSO and its services,
/// plus every variable `compose.yaml` guards with `:?` — compose
/// interpolates the whole file whatever the active profiles are.
fn write_env_file(secret_file: &Path) -> Result<PathBuf> {
    let path = temp_path("stack.env");
    let contents = format!(
        "MATRIX_DOMAIN=deploy.twalk\n\
         MATRIX_HTTP_PORT=17618\n\
         MATRIX_REGISTRATION_SHARED_SECRET=deploy-test-only-registration-shared-secret\n\
         MATRIX_MACAROON_SECRET=deploy-test-only-macaroon-secret\n\
         MATRIX_FORM_SECRET=deploy-test-only-form-secret\n\
         SENSOR_USER_ID=@sensor:deploy.twalk\n\
         SENSOR_PASSWORD=deploy-test-only-password-sensor\n\
         SENSOR_ALLOWED_INVITERS=@owner:deploy.twalk\n\
         SENSOR_STATE_DIR=/data\n\
         NATS_PORT={}\n\
         GATEWAY_HTTP_PORT=17628\n\
         TWALK_COLLECTOR_IMAGE={}\n\
         COLLECTOR_OIDC_ISSUER=https://sso.deploy.twalk.invalid\n\
         COLLECTOR_OIDC_CLIENT_ID=twalk-collector\n\
         COLLECTOR_OIDC_CLIENT_SECRET_FILE={}\n\
         COLLECTOR_OIDC_REDIRECT_URI=http://localhost:1/callback\n\
         COLLECTOR_JMAP_SESSION_URL=https://mail.deploy.twalk.invalid/jmap/session\n\
         COLLECTOR_CALDAV_URL=https://calendar.deploy.twalk.invalid\n\
         COLLECTOR_OWNER_EMAIL={OWNER_EMAIL}\n\
         COLLECTOR_MAIL_CONNECTION={MAIL_CONNECTION}\n\
         COLLECTOR_CALENDAR_CONNECTION={CALENDAR_CONNECTION}\n\
         COLLECTOR_HEALTH_INTERVAL_SECONDS=2\n\
         COLLECTOR_LOG_LEVEL=info\n",
        nats_port(),
        collector_image(),
        secret_file.display(),
    );
    std::fs::write(&path, contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn compose_command(env_file: &Path) -> Command {
    let mut command = Command::new("docker");
    command
        .arg("compose")
        .arg("-p")
        .arg(deploy_stack())
        .arg("--env-file")
        .arg(env_file)
        .arg("-f")
        .arg(deploy_dir().join("compose.yaml"))
        .arg("--profile")
        .arg("collector");
    command
}

/// Runs `docker compose` against this test's stack and returns its stdout.
async fn compose(env_file: &Path, args: &[&str], what: &str) -> Result<String> {
    let output = compose_command(env_file)
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

/// The same, with a failure handed back as its stderr rather than treated
/// as an error: one assertion is about a start that is *supposed* to be
/// refused, and its message is the evidence.
async fn compose_allowing_failure(
    env_file: &Path,
    args: &[&str],
    what: &str,
) -> Result<std::result::Result<String, String>> {
    let output = compose_command(env_file)
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to run docker compose {what}"))?;
    Ok(if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    })
}

async fn compose_ps(env_file: &Path) -> Result<Vec<Value>> {
    let stdout = compose(env_file, &["ps", "-a", "--format", "json"], "ps").await?;
    Ok(stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect())
}

fn teardown_requested() -> bool {
    std::env::var("TWALK_COLLECTOR_DEPLOY_TEST_TEARDOWN")
        .map(|value| value == "1")
        .unwrap_or(false)
}

async fn teardown(env_file: &Path) -> Result<()> {
    compose(env_file, &["down", "-v", "--remove-orphans"], "down").await?;
    let _ = Command::new("docker")
        .args(["image", "rm", &collector_image()])
        .output()
        .await;
    Ok(())
}

#[tokio::test]
async fn the_collector_profile_builds_starts_and_says_on_the_bus_that_it_has_no_grant() -> Result<()>
{
    let secret_file = write_secret_file(0o600)?;
    let env_file = write_env_file(&secret_file)?;

    // The README's second step, on the two services this test needs, the
    // collector's image built for this stack. `--force-recreate` so a warm
    // stack's collector starts again and says its first state again.
    compose(&env_file, &["build", "collector"], "build collector").await?;
    compose(
        &env_file,
        &[
            "up",
            "-d",
            "--wait",
            "--force-recreate",
            "nats",
            "collector",
        ],
        "up",
    )
    .await?;

    // Started, and stayed started: the collector refuses configuration it
    // cannot run on by exiting, and compose would restart it in a loop —
    // so "running twice in a row, a few seconds apart" is the assertion.
    for _ in 0..2 {
        sleep(Duration::from_secs(3)).await;
        let entries = compose_ps(&env_file).await?;
        let collector = entries
            .iter()
            .find(|entry| entry["Service"].as_str() == Some("collector"))
            .context("the collector service has no container")?;
        ensure!(
            collector["State"].as_str() == Some("running"),
            "the collector is not running: {collector}"
        );
        ensure!(
            collector["Image"].as_str() == Some(collector_image().as_str()),
            "the collector runs another image than this stack's: {collector}"
        );
    }

    // On the bus: one transition per connection, from `unknown` to
    // `reconnect_required`, the SSO the service in question, and the
    // command to run in the hint — contract-valid, from this collector.
    let bus = Bus::connect_to(&nats_url())
        .await
        .context("the stack's bus is not reachable on the published port")?;
    let mut said: Vec<Value> = Vec::new();
    for _ in 0..30 {
        said = bus
            .fetch_all(STREAM, STATUS_SUBJECT)
            .await?
            .into_iter()
            .filter(|event| {
                event["data"]["to_state"] == "reconnect_required"
                    && [MAIL_CONNECTION, CALENDAR_CONNECTION]
                        .contains(&event["connection"].as_str().unwrap_or_default())
            })
            .collect();
        if [MAIL_CONNECTION, CALENDAR_CONNECTION]
            .iter()
            .all(|connection| said.iter().any(|event| event["connection"] == *connection))
        {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    for (connection, kind) in [
        (MAIL_CONNECTION, "email"),
        (CALENDAR_CONNECTION, "calendar"),
    ] {
        let event = said
            .iter()
            .rfind(|event| event["connection"] == connection)
            .with_context(|| format!("no reconnect_required on the bus for {connection}"))?;
        validate_against_contract(event, "connection.status.changed")?;
        assert_eq!(
            event["source"],
            format!("collector://collector/connections/{connection}")
        );
        assert_eq!(event["data"]["kind"], kind);
        assert_eq!(event["data"]["service"], "sso");
        let hint = event["data"]["hint"].as_str().unwrap_or_default();
        assert!(
            hint.contains("authorize") && hint.contains("provision-connection.sh"),
            "the hint names the command to run: {hint}"
        );
        assert!(
            hint.contains(OWNER_EMAIL),
            "the hint names who to sign in as: {hint}"
        );
    }

    // And in the log, for an operator reading `docker compose logs`.
    let logs = compose(&env_file, &["logs", "--no-color", "collector"], "logs").await?;
    assert!(
        logs.contains("No grant in") && logs.contains("provision-connection.sh"),
        "the log says the grant is missing and what to run:\n{logs}"
    );
    assert!(
        logs.contains("COLLECTOR_GATEWAY_URL is not set"),
        "the log says the registry was not read, since this stack runs no Gateway:\n{logs}"
    );

    // The entrypoint's refusal: a client secret readable by others never
    // reaches the binary. A one-shot with `--no-deps`, so nothing else
    // starts, on an env file naming the loose file.
    let loose = write_secret_file(0o644)?;
    let loose_env = write_env_file(&loose)?;
    let refused = compose_allowing_failure(
        &loose_env,
        &["run", "--rm", "--no-deps", "collector", "authorize"],
        "run collector with a loose secret",
    )
    .await?;
    match refused {
        Ok(stdout) => bail!("a world-readable client secret was accepted:\n{stdout}"),
        Err(stderr) => assert!(
            stderr.contains("readable by group or") && stderr.contains("chmod 0600"),
            "the refusal names the mode and the fix:\n{stderr}"
        ),
    }

    if teardown_requested() {
        teardown(&env_file).await?;
    }
    let _ = std::fs::remove_file(&secret_file);
    let _ = std::fs::remove_file(&loose);
    let _ = std::fs::remove_file(&env_file);
    let _ = std::fs::remove_file(&loose_env);
    Ok(())
}
