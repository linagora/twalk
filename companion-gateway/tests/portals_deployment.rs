//! Ticket #105 end to end on the reference deployment: a real Synapse, a
//! real bus, the deployed Companion Gateway and a **real Sensor process**,
//! and a portal room created after all of them were already running.
//!
//! This is the test the ticket asks for, and the shape of it is the point.
//! The defect was not that the Sensor was missing from rooms that existed at
//! connection time — it was that a bridge builds a portal room *when a
//! conversation becomes active*, so the rooms keep arriving all day and a
//! one-off repair is wrong by the evening. So nothing here is prepared
//! before the stack is up: the whole deployment starts first, the
//! conversation happens afterwards, and the assertion is that it is offered,
//! that choosing it puts the real Sensor inside it, and that a message in it
//! reaches the bus.
//!
//! The chain this proves has four links, and every one of them was broken or
//! absent before:
//!
//! 1. the Gateway can see a conversation the bridge built (it reads the
//!    bridge bot's rooms with that bridge's appservice token),
//! 2. it offers it rather than entering it (nothing is observed by default),
//! 3. the invitation it issues as the bridge bot is one the Sensor accepts
//!    (`SENSOR_ALLOWED_INVITERS`, which until now settled only what the
//!    Sensor would accept if anyone ever asked),
//! 4. and the traffic then reaches the bus.
//!
//! What stands in for a real mautrix bridge is one account: the "bridge bot"
//! is an ordinary Matrix account that creates the portal room, marks it with
//! the `m.bridge` state event, and whose access token the Gateway holds as
//! that bridge's `as_token`. A real bridge needs a phone and a live WhatsApp
//! account (`sensor/tests/bridges_deployment.rs` is where the real ones run,
//! and it cannot log one in either); what the register needs from a bridge
//! bot is membership of the portal rooms and the power to invite in them,
//! and this account has exactly that. Synapse answers the client-server API
//! the same way for an appservice token and a user token, which is why the
//! register speaks nothing else.
//!
//! This stack is its own: TWALK_PORTALS_TEST_STACK (default
//! twalk-portals-test) with TWALK_PORTALS_TEST_SYNAPSE_PORT (17309),
//! TWALK_PORTALS_TEST_GATEWAY_PORT (17319) and TWALK_PORTALS_TEST_NATS_PORT
//! (17329), so it never collides with the deploy-test stack the other suites
//! share. **It tears itself down at the end of every run, passing or
//! failing**, images included — this host has filled its disk twice
//! ([#128](https://github.com/linagora/twalk/issues/128)). Set
//! TWALK_PORTALS_TEST_KEEP=1 to keep the stack for inspection.
//!
//! The credentials below are throwaway constants for a local, ephemeral
//! stack whose env file is generated in a temp directory and never
//! committed.

mod harness;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use harness::{parse_exposition, validate_against_contract, Bus};
use tokio::process::Command;

const SERVER_NAME: &str = "deploy.twalk";
const OWNER_LOCALPART: &str = "owner";
const OWNER_PASSWORD: &str = "portals-test-only-password-owner";
const REGISTRATION_SHARED_SECRET: &str = "portals-test-only-registration-shared-secret";
const SERVICE_TOKEN: &str = "portals-test-only-gateway-service-token";
/// The account playing the bridge's bot: it creates the portal rooms, is in
/// every one of them, and its access token is what the Gateway holds as that
/// bridge's `as_token`.
const BRIDGE_BOT_LOCALPART: &str = "whatsappbot";
const BRIDGE_BOT_PASSWORD: &str = "portals-test-only-password-whatsappbot";
/// A network ghost, named the way mautrix names one, so the Sensor
/// attributes its message to WhatsApp exactly as it would in production.
const GHOST_LOCALPART: &str = "whatsapp_33612345678";
const GHOST_PASSWORD: &str = "portals-test-only-password-ghost";

const STREAM: &str = "twalk";
const MESSAGE_SUBJECT: &str = "twalk.inbound.message.received.v1";

fn owner_user_id() -> String {
    format!("@{OWNER_LOCALPART}:{SERVER_NAME}")
}

fn sensor_user_id() -> String {
    format!("@sensor:{SERVER_NAME}")
}

fn bridge_bot_user_id() -> String {
    format!("@{BRIDGE_BOT_LOCALPART}:{SERVER_NAME}")
}

fn ghost_user_id() -> String {
    format!("@{GHOST_LOCALPART}:{SERVER_NAME}")
}

fn port(variable: &str, fallback: &str) -> String {
    std::env::var(variable)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

fn stack() -> String {
    std::env::var("TWALK_PORTALS_TEST_STACK")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "twalk-portals-test".to_owned())
}

fn synapse_port() -> String {
    port("TWALK_PORTALS_TEST_SYNAPSE_PORT", "17309")
}

fn gateway_port() -> String {
    port("TWALK_PORTALS_TEST_GATEWAY_PORT", "17319")
}

fn nats_port() -> String {
    port("TWALK_PORTALS_TEST_NATS_PORT", "17329")
}

fn homeserver_url() -> String {
    format!("http://localhost:{}", synapse_port())
}

fn gateway_base_url() -> String {
    format!("http://localhost:{}", gateway_port())
}

fn gateway_image() -> String {
    format!("twalk/companion-gateway:{}", stack())
}

fn sensor_image() -> String {
    format!("twalk/sensor:{}", stack())
}

fn deploy_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the package has a parent directory")
        .join("deploy/docker-compose")
}

/// Kept only when an operator asks: the default is that this run leaves
/// nothing behind.
fn keep_requested() -> bool {
    matches!(
        std::env::var("TWALK_PORTALS_TEST_KEEP").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// The stack's environment. `bridge_as_token` is `None` on the first write,
/// when the homeserver is not up yet and the bridge bot has no session: the
/// register is then off, which is also worth having gone through, because it
/// is the state every deployment starts in.
fn write_env_file(bridge_as_token: Option<&str>) -> Result<PathBuf> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "twalk-portals-deploy-test-{}-{unique}.env",
        std::process::id()
    ));
    let (gateway_image, sensor_image) = (gateway_image(), sensor_image());
    let (gateway_port, synapse_port, nats_port) = (gateway_port(), synapse_port(), nats_port());
    let owner = owner_user_id();
    let bridge_bot = bridge_bot_user_id();
    // The Sensor accepts an invitation from the bridge's bot. That is what
    // `deploy/README.md` has always told operators to configure, and what
    // nothing ever used, because nothing invited.
    let mut contents = format!(
        "MATRIX_DOMAIN={SERVER_NAME}\n\
         MATRIX_HTTP_PORT={synapse_port}\n\
         MATRIX_REGISTRATION_SHARED_SECRET={REGISTRATION_SHARED_SECRET}\n\
         MATRIX_MACAROON_SECRET=portals-test-only-macaroon-secret\n\
         MATRIX_FORM_SECRET=portals-test-only-form-secret\n\
         SENSOR_USER_ID={}\n\
         SENSOR_PASSWORD=portals-test-only-password-sensor\n\
         SENSOR_ALLOWED_INVITERS={owner},{bridge_bot}\n\
         SENSOR_STATE_DIR=/data\n\
         SENSOR_LOG_LEVEL=info\n\
         NATS_PORT={nats_port}\n\
         GATEWAY_HTTP_PORT={gateway_port}\n\
         GATEWAY_FALLBACK_FILE=200.html\n\
         GATEWAY_LOG_LEVEL=info,twalk_companion_gateway=debug\n\
         GATEWAY_OWNER={owner}\n\
         GATEWAY_STATE_DIR=/data\n\
         GATEWAY_REGISTRATION_SHARED_SECRET={REGISTRATION_SHARED_SECRET}\n\
         GATEWAY_SENSOR_USER_ID={}\n\
         GATEWAY_NATS_URL=nats://nats:4222\n\
         GATEWAY_SERVICE_TOKEN={SERVICE_TOKEN}\n\
         TWALK_GATEWAY_IMAGE={gateway_image}\n\
         TWALK_SENSOR_IMAGE={sensor_image}\n",
        sensor_user_id(),
        sensor_user_id(),
    );
    if let Some(as_token) = bridge_as_token {
        // One bridge instance, configured the way `.env.example` documents.
        // Its provisioning listener is never reached: the register asks the
        // homeserver, not the bridge, and this stack runs no bridge at all —
        // which is itself the assertion that the two are independent.
        contents.push_str(&format!(
            "GATEWAY_BRIDGES=mautrix-whatsapp\n\
             WHATSAPP_PROVISIONING_SECRET=portals-test-only-provisioning-secret\n\
             WHATSAPP_AS_TOKEN={as_token}\n\
             GATEWAY_PORTAL_REFRESH_SECONDS=5\n"
        ));
    }
    std::fs::write(&path, contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

async fn compose(env_file: &Path, args: &[&str], what: &str) -> Result<String> {
    let output = Command::new("docker")
        .arg("compose")
        .arg("-p")
        .arg(stack())
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

/// Everything this run created: the stack, its volumes, its two per-stack
/// images and both env files. Runs whether the assertions passed or not,
/// which is the only version of this that keeps a shared host's disk
/// (#128).
async fn teardown(env_files: &[PathBuf]) {
    if keep_requested() {
        eprintln!("TWALK_PORTALS_TEST_KEEP is set: leaving the stack up");
        return;
    }
    let Some(env_file) = env_files.last() else {
        return;
    };
    if let Err(error) = compose(env_file, &["down", "-v"], "down -v").await {
        eprintln!("teardown: {error:?}");
    }
    for image in [gateway_image(), sensor_image()] {
        let output = Command::new("docker")
            .args(["image", "rm", &image])
            .output()
            .await;
        match output {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.contains("No such image") {
                    eprintln!("teardown: docker image rm {image}: {stderr}");
                }
            }
            Err(error) => eprintln!("teardown: docker image rm {image}: {error}"),
            _ => {}
        }
    }
    for env_file in env_files {
        let _ = std::fs::remove_file(env_file);
    }
}

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

async fn provision_account(env_file: &Path, localpart: &str, password: &str) -> Result<()> {
    compose(
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
        &format!("register {localpart}"),
    )
    .await?;
    Ok(())
}

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

/// A portal room, as the bridge builds one when a conversation becomes
/// active: the bot creates it, marks it with `m.bridge` (which is what the
/// Sensor attributes a network by), and pulls the correspondent in.
async fn build_portal(client: &reqwest::Client, bot_token: &str, name: &str) -> Result<String> {
    let created: serde_json::Value = serde_json::from_str(
        &client
            .post(format!("{}/_matrix/client/v3/createRoom", homeserver_url()))
            .bearer_auth(bot_token)
            .json(&serde_json::json!({ "name": name, "preset": "private_chat" }))
            .send()
            .await?
            .text()
            .await?,
    )?;
    let room_id = created["room_id"]
        .as_str()
        .context("the createRoom answer names no room id")?
        .to_owned();
    let state_key = percent(&format!("{SERVER_NAME}/whatsapp"));
    let marked = client
        .put(format!(
            "{}/_matrix/client/v3/rooms/{room_id}/state/m.bridge/{state_key}",
            homeserver_url()
        ))
        .bearer_auth(bot_token)
        .json(&serde_json::json!({
            "bridgebot": bridge_bot_user_id(),
            "protocol": { "id": "whatsapp", "displayname": "WhatsApp" },
            "channel": { "id": "whatsapp-portals-test", "displayname": name },
        }))
        .send()
        .await?;
    anyhow::ensure!(
        marked.status().is_success(),
        "the homeserver refused the bridge marker: {}",
        marked.text().await.unwrap_or_default()
    );
    Ok(room_id)
}

fn percent(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

async fn invite(client: &reqwest::Client, token: &str, room_id: &str, user_id: &str) -> Result<()> {
    let response = client
        .post(format!(
            "{}/_matrix/client/v3/rooms/{room_id}/invite",
            homeserver_url()
        ))
        .bearer_auth(token)
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

async fn membership(
    client: &reqwest::Client,
    token: &str,
    room_id: &str,
    user_id: &str,
) -> Result<Option<String>> {
    let response = client
        .get(format!(
            "{}/_matrix/client/v3/rooms/{room_id}/state/m.room.member/{user_id}",
            homeserver_url()
        ))
        .bearer_auth(token)
        .send()
        .await?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let body: serde_json::Value = serde_json::from_str(&response.text().await?)?;
    Ok(body["membership"].as_str().map(str::to_owned))
}

async fn send_message(
    client: &reqwest::Client,
    token: &str,
    room_id: &str,
    body: &str,
) -> Result<()> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let response = client
        .put(format!(
            "{}/_matrix/client/v3/rooms/{room_id}/send/m.room.message/twalk-g105-{unique}",
            homeserver_url()
        ))
        .bearer_auth(token)
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

// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_conversation_the_bridge_builds_while_the_deployment_runs_reaches_the_bus_when_chosen(
) -> Result<()> {
    let mut env_files = Vec::new();
    let result = run(&mut env_files).await;
    teardown(&env_files).await;
    result
}

async fn run(env_files: &mut Vec<PathBuf>) -> Result<()> {
    let cold = write_env_file(None)?;
    env_files.push(cold.clone());
    compose(&cold, &["build", "companion-gateway", "sensor"], "build").await?;
    compose(&cold, &["up", "-d", "--wait", "synapse"], "up synapse").await?;

    // The accounts this deployment has: the owner, the bridge's bot, and one
    // network ghost standing in for the person on the other side.
    for (localpart, password) in [
        (OWNER_LOCALPART, OWNER_PASSWORD),
        (BRIDGE_BOT_LOCALPART, BRIDGE_BOT_PASSWORD),
        (GHOST_LOCALPART, GHOST_PASSWORD),
    ] {
        provision_account(&cold, localpart, password).await?;
    }
    let client = reqwest::Client::new();
    let bot_token = login_as(&client, BRIDGE_BOT_LOCALPART, BRIDGE_BOT_PASSWORD).await?;
    let owner_token = login_as(&client, OWNER_LOCALPART, OWNER_PASSWORD).await?;
    let ghost_token = login_as(&client, GHOST_LOCALPART, GHOST_PASSWORD).await?;

    // Now the deployment an operator actually runs: the Gateway holding that
    // bridge's appservice credential, and the Sensor beside it.
    let warm = write_env_file(Some(&bot_token))?;
    env_files.push(warm.clone());
    compose(
        &warm,
        &["up", "-d", "--wait", "companion-gateway", "sensor"],
        "up companion-gateway sensor",
    )
    .await?;

    let base = gateway_base_url();
    poll_deploy(
        || async {
            let response = client.get(format!("{base}/health")).send().await.ok()?;
            response.status().is_success().then_some(())
        },
        "the deployed Gateway to answer",
    )
    .await?;
    let cookie = sign_in(&client, &base, &owner_token).await?;
    let register = |cookie: String| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = serde_json::from_str(
                &client
                    .get(format!("{base}/api/portals"))
                    .header(reqwest::header::COOKIE, format!("twalk_device={cookie}"))
                    .send()
                    .await?
                    .text()
                    .await?,
            )?;
            Ok::<_, anyhow::Error>(body)
        }
    };

    // Nothing has happened on WhatsApp yet, so there is nothing to observe —
    // and the answer says that rather than refusing.
    let empty = register(cookie.clone()).await?;
    assert_eq!(
        empty["summary"]["total"], 0,
        "a connected network with no active conversation has no portal rooms: {empty}"
    );
    assert_eq!(
        empty["bridges"][0]["readable"], true,
        "and the bridge itself is readable, which is the difference between \
         'no conversations' and 'I cannot see them': {empty}"
    );

    // 13:24. Somebody writes, and the bridge builds a portal for it — while
    // the Gateway, the Sensor, the homeserver and the bus have all been
    // running for some time. This is the case no connection-time fix covers.
    let room = build_portal(&client, &bot_token, "Échecs en Yvelines").await?;
    invite(&client, &bot_token, &room, &owner_user_id()).await?;
    join(&client, &owner_token, &room).await?;
    invite(&client, &bot_token, &room, &ghost_user_id()).await?;
    join(&client, &ghost_token, &room).await?;

    let offered = poll_deploy(
        || {
            let register = register(cookie.clone());
            async move {
                let body = register.await.ok()?;
                (body["summary"]["total"] == 1).then_some(body)
            }
        },
        "the new conversation to appear in the portal register",
    )
    .await?;
    assert_eq!(
        offered["portals"][0]["room_id"].as_str(),
        Some(room.as_str()),
        "{offered}"
    );
    assert_eq!(
        offered["portals"][0]["observation"], "absent",
        "offered for observation, and entered by nobody: {offered}"
    );
    assert_eq!(offered["portals"][0]["network"], "whatsapp", "{offered}");
    assert_eq!(
        membership(&client, &bot_token, &room, &sensor_user_id()).await?,
        None,
        "which the homeserver confirms: the Sensor was not put anywhere on its own"
    );

    // The user chooses this conversation. Everything after this line is the
    // deployment's own: the Gateway invites as the bridge bot, and the real
    // Sensor decides for itself whether to accept.
    let chosen: serde_json::Value = serde_json::from_str(
        &client
            .post(format!("{base}/api/portals/observation"))
            .header(reqwest::header::COOKIE, format!("twalk_device={cookie}"))
            .json(&serde_json::json!({ "rooms": [room], "observed": true }))
            .send()
            .await?
            .text()
            .await?,
    )?;
    assert_eq!(chosen["outcomes"][0]["status"], "invited", "{chosen}");

    let joined = poll_deploy(
        || async {
            membership(&client, &bot_token, &room, &sensor_user_id())
                .await
                .ok()
                .flatten()
                .filter(|membership| membership == "join")
        },
        "the deployed Sensor to join the portal room it was invited to",
    )
    .await?;
    assert_eq!(joined, "join");

    // And the conversation reaches the bus. The Sensor attributes it to
    // WhatsApp from the room's own bridge marker, which is what makes this a
    // conversation and not a Matrix room.
    let body = format!("a message in a portal the bridge built at 13:24 — {}", {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    });
    send_message(&client, &ghost_token, &room, &body).await?;
    let bus = Bus::connect_to(&format!("nats://localhost:{}", nats_port())).await?;
    let event = poll_deploy(
        || async {
            bus.fetch_room_messages_on(SERVER_NAME, STREAM, MESSAGE_SUBJECT, &room)
                .await
                .ok()?
                .into_iter()
                .map(|stored| stored.payload)
                .find(|event| event["data"]["body"] == body)
        },
        "the chosen conversation's message to reach the bus",
    )
    .await?;
    validate_against_contract(&event, "inbound.message.received")?;
    assert_eq!(event["network"], "whatsapp", "{event}");
    assert_eq!(
        event["subject"].as_str(),
        Some(ghost_user_id().as_str()),
        "{event}"
    );

    // The register now says so, and so does /metrics — which is the fact the
    // deployment could not previously state at all.
    let observing = register(cookie.clone()).await?;
    assert_eq!(observing["portals"][0]["observation"], "observing");
    assert_eq!(observing["summary"]["observing"], 1);
    assert_eq!(observing["summary"]["absent"], 0);

    let exposition = client
        .get(format!("{base}/metrics"))
        .send()
        .await?
        .text()
        .await?;
    let samples = parse_exposition(&exposition);
    let sample = |name: &str| {
        samples
            .iter()
            .find(|(series, _)| series == name)
            .map(|(_, value)| *value)
    };
    // The background read is on a five-second interval here, so this is the
    // gauge the deployment would show an operator who never opened a browser.
    assert_eq!(
        sample("twalk_companion_gateway_portal_rooms{observation=\"observing\"}"),
        Some(1),
        "{exposition}"
    );
    assert_eq!(
        sample("twalk_companion_gateway_portal_rooms{observation=\"absent\"}"),
        Some(0),
        "{exposition}"
    );

    // The Sensor's own half of that answer — it is reading one room, and it
    // ignored no invitation on the way — is asserted where the Sensor's
    // metrics endpoint is reachable: `sensor/tests/observability.rs`.

    Ok(())
}
