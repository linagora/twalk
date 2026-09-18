//! Ticket #53, bootstrap: the registration relay that creates this
//! deployment's one account and refuses every other, and the Sensor's
//! invitation into the rooms the user selects on screen 3d.
//!
//! The seam is the process boundary, as everywhere else: a real Synapse
//! answers the admin registration call and holds the rooms, the real Gateway
//! binary relays and invites over the network, and every assertion is an HTTP
//! call, a read of the Gateway's store, or a read of its captured log output.
//!
//! Three promises are asserted here rather than documented:
//!
//! - the first registration succeeds and every one after it is refused with a
//!   machine-readable error;
//! - no recovery key can reach the Gateway — the API neither accepts nor
//!   returns one (ADR 0014, screen 2's "Twalk never sees it");
//! - no Matrix access token is written to the store or to a log line
//!   (ADR 0011), asserted against the bytes of the store and the captured
//!   lines, as `signin.rs` does for sign-in.
//!
//! The invited room being *joined* by a real Sensor, and its traffic reaching
//! the bus as `network=matrix`, is the deploy stack's job: `deployment.rs`,
//! which runs a real Sensor next to a real Gateway.

mod harness;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use harness::{
    companion_build, ensure_stack, fresh_owner_user_id, gateway_env_with, gateway_state_dir,
    poll_until, GatewayProc, MatrixUser, OTHER_LOCALPART, REGISTRATION_SHARED_SECRET,
    SENSOR_USER_ID, SERVER_NAME,
};

/// The password a test registers the owner's account with. A throwaway
/// constant for the local, ephemeral stack, like the test bots' own.
const OWNER_PASSWORD: &str = "test-only-password-g53-owner";

/// A Gateway whose owner has no account yet, so that the relay's first
/// registration is a real first. Returns the process, its origin, its state
/// directory and the owner's Matrix ID.
async fn start(test_name: &str) -> Result<(GatewayProc, String, PathBuf, String)> {
    start_with(test_name, &[]).await
}

/// [`start`] with extra environment overrides.
async fn start_with(
    test_name: &str,
    overrides: &[(&str, &str)],
) -> Result<(GatewayProc, String, PathBuf, String)> {
    ensure_stack().await?;
    let static_dir = companion_build(test_name)?;
    let state_dir = gateway_state_dir(&static_dir);
    let owner = fresh_owner_user_id(test_name);
    let mut env = vec![("GATEWAY_OWNER", owner.as_str())];
    env.extend_from_slice(overrides);
    let gateway = GatewayProc::start(&gateway_env_with(&static_dir, &env))?;
    let base = gateway.base_url().await?;
    wait_until_answering(&base).await?;
    Ok((gateway, base, state_dir, owner))
}

async fn wait_until_answering(base: &str) -> Result<()> {
    poll_until(
        || async {
            reqwest::get(format!("{base}/health"))
                .await
                .ok()?
                .error_for_status()
                .ok()
        },
        "the gateway health endpoint",
    )
    .await?;
    Ok(())
}

/// A client that follows no redirects and keeps no cookies: every test drives
/// the session cookie by hand.
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

/// The localpart of a Matrix ID: what the relay takes as its username.
fn localpart(user_id: &str) -> &str {
    user_id
        .trim_start_matches('@')
        .split_once(':')
        .expect("a Matrix ID")
        .0
}

async fn json(response: reqwest::Response) -> Result<serde_json::Value> {
    let body = response.text().await?;
    serde_json::from_str(&body).with_context(|| format!("not JSON: {body}"))
}

/// `POST /api/bootstrap/account` with an arbitrary body: the endpoint under
/// test, driven by the document a client would send.
async fn register_with(
    client: &reqwest::Client,
    base: &str,
    body: &serde_json::Value,
) -> Result<reqwest::Response> {
    client
        .post(format!("{base}/api/bootstrap/account"))
        .json(body)
        .send()
        .await
        .context("failed to post a registration")
}

/// The ordinary registration: a username and a password, and nothing else.
async fn register(
    client: &reqwest::Client,
    base: &str,
    username: &str,
    password: &str,
) -> Result<reqwest::Response> {
    register_with(
        client,
        base,
        &serde_json::json!({ "username": username, "password": password }),
    )
    .await
}

/// Registers the owner's account and returns the Matrix session the relay
/// answered with — the session the Companion continues in the browser.
async fn register_owner(
    client: &reqwest::Client,
    base: &str,
    owner: &str,
) -> Result<(MatrixUser, serde_json::Value)> {
    let response = register(client, base, localpart(owner), OWNER_PASSWORD).await?;
    assert_eq!(
        response.status(),
        reqwest::StatusCode::CREATED,
        "the first registration creates the account: {}",
        response.text().await.unwrap_or_default()
    );
    let document = json(response).await?;
    let user = MatrixUser::with_token(
        document["user_id"].as_str().context("no user id")?,
        document["access_token"]
            .as_str()
            .context("no access token")?,
    );
    Ok((user, document))
}

/// Signs a device in with a freshly minted OpenID token, and returns the
/// device cookie.
async fn sign_in(client: &reqwest::Client, base: &str, user: &MatrixUser) -> Result<String> {
    let token = user.openid_token().await?;
    let response = client
        .post(format!("{base}/api/session"))
        .json(&serde_json::json!({
            "matrix_openid_token": token,
            "device_name": "Onboarding laptop",
        }))
        .send()
        .await?;
    anyhow::ensure!(
        response.status() == reqwest::StatusCode::OK,
        "the owner must be able to sign in: {} {}",
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

/// `POST /api/bootstrap/rooms` with a device cookie: the invitation endpoint
/// as screen 3d calls it.
async fn invite_rooms(
    client: &reqwest::Client,
    base: &str,
    device_token: &str,
    body: &serde_json::Value,
) -> Result<reqwest::Response> {
    client
        .post(format!("{base}/api/bootstrap/rooms"))
        .header("cookie", format!("twalk_device={device_token}"))
        .json(body)
        .send()
        .await
        .context("failed to post an invitation request")
}

/// Everything the Gateway wrote to its state directory, as one string — the
/// store, its write-ahead log and any sibling file (`signin.rs`'s pattern).
fn store_bytes(state_dir: &Path) -> Result<String> {
    let mut contents = String::new();
    for entry in std::fs::read_dir(state_dir)
        .with_context(|| format!("failed to read {}", state_dir.display()))?
    {
        let path = entry?.path();
        if path.is_file() {
            contents.push_str(&String::from_utf8_lossy(&std::fs::read(&path)?));
        }
    }
    Ok(contents)
}

/// The Prometheus exposition of the Gateway under test.
async fn exposition(base: &str) -> Result<String> {
    Ok(reqwest::get(format!("{base}/metrics"))
        .await?
        .text()
        .await?)
}

#[tokio::test]
async fn the_first_registration_succeeds_and_every_one_after_it_is_refused() -> Result<()> {
    let (gateway, base, _state_dir, owner) = start("first-account-only").await?;
    let client = client()?;

    // The account did not exist: this is the deployment's one registration.
    let (user, document) = register_owner(&client, &base, &owner).await?;
    assert_eq!(document["user_id"].as_str(), Some(owner.as_str()));
    assert!(
        document["device_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "the answer carries the device the browser continues in: {document}"
    );
    // And it is a working Matrix session, which is what screen 2 needs next:
    // the browser bootstraps cross-signing with it (ADR 0014).
    assert_eq!(
        user.whoami().await?,
        owner,
        "the token the relay answered with is a real session on the homeserver"
    );

    // The same username again: refused, with an error a client can branch on.
    let again = register(&client, &base, localpart(&owner), OWNER_PASSWORD).await?;
    assert_eq!(
        again.status(),
        reqwest::StatusCode::CONFLICT,
        "one owner per deployment: the account has already been created"
    );
    let refusal = json(again).await?;
    assert_eq!(
        refusal["error"].as_str(),
        Some("account_already_exists"),
        "the refusal is machine-readable: {refusal}"
    );

    // Another username: refused before the registration secret is used at
    // all. `bot_beta` is a real, unrelated account on this homeserver — the
    // case that matters, since a deployment's homeserver has other accounts.
    let other = register(&client, &base, OTHER_LOCALPART, OWNER_PASSWORD).await?;
    assert_eq!(
        other.status(),
        reqwest::StatusCode::FORBIDDEN,
        "the relay creates the owner's account or none"
    );
    let refusal = json(other).await?;
    assert_eq!(refusal["error"].as_str(), Some("not_the_owner"));
    assert!(
        refusal["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("GATEWAY_OWNER")),
        "the refusal says where the one owner is named: {refusal}"
    );

    // A username nobody has: still refused, because it is not the owner's —
    // the relay is not a registration service with a filter in front of it.
    let invented = register(&client, &base, "someone_entirely_new_g53", OWNER_PASSWORD).await?;
    assert_eq!(invented.status(), reqwest::StatusCode::FORBIDDEN);

    // An operator sees all of it in the metric and in the logs.
    let exposition = exposition(&base).await?;
    for sample in [
        "twalk_companion_gateway_registrations_total{outcome=\"created\"} 1",
        "twalk_companion_gateway_registrations_total{outcome=\"already_exists\"} 1",
        "twalk_companion_gateway_registrations_total{outcome=\"not_the_owner\"} 2",
    ] {
        assert!(
            exposition.contains(sample),
            "{sample} missing:\n{exposition}"
        );
    }
    let logs = gateway.logs().await.join("\n");
    assert!(
        logs.contains("created this deployment's one account"),
        "the creation is logged: {logs}"
    );
    assert!(
        logs.contains("already exists"),
        "the refusal is logged: {logs}"
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_restart_still_refuses_a_second_account() -> Result<()> {
    ensure_stack().await?;
    let static_dir = companion_build("first-account-restart")?;
    let owner = fresh_owner_user_id("first-account-restart");
    let env = gateway_env_with(&static_dir, &[("GATEWAY_OWNER", owner.as_str())]);

    let gateway = GatewayProc::start(&env)?;
    let base = gateway.base_url().await?;
    wait_until_answering(&base).await?;
    let client = client()?;
    register_owner(&client, &base, &owner).await?;
    gateway.stop().await;

    // A new process on the same state directory: the refusal survives,
    // because it is a row on disk and not a fact in memory.
    let gateway = GatewayProc::start(&env)?;
    let base = gateway.base_url().await?;
    wait_until_answering(&base).await?;
    let again = register(&client, &base, localpart(&owner), OWNER_PASSWORD).await?;
    assert_eq!(again.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        json(again).await?["error"].as_str(),
        Some("account_already_exists")
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_wiped_store_does_not_reopen_the_window() -> Result<()> {
    let (gateway, base, state_dir, owner) = start("first-account-wiped-store").await?;
    let client = client()?;
    register_owner(&client, &base, &owner).await?;
    gateway.stop().await;

    // The Gateway's own memory of the creation is gone — the operator deleted
    // the volume, say. The account still exists on the homeserver, and the
    // relay honours the homeserver's answer as the same refusal.
    std::fs::remove_dir_all(&state_dir)
        .with_context(|| format!("failed to remove {}", state_dir.display()))?;
    let static_dir = companion_build("first-account-wiped-store-2")?;
    // The same state directory as the first Gateway had, now empty.
    let env = gateway_env_with(
        &static_dir,
        &[
            ("GATEWAY_OWNER", owner.as_str()),
            ("GATEWAY_STATE_DIR", &state_dir.to_string_lossy()),
        ],
    );
    let gateway = GatewayProc::start(&env)?;
    let base = gateway.base_url().await?;
    wait_until_answering(&base).await?;

    let again = register(&client, &base, localpart(&owner), OWNER_PASSWORD).await?;
    assert_eq!(
        again.status(),
        reqwest::StatusCode::CONFLICT,
        "the homeserver already has the account: losing the store must not re-open the window"
    );
    assert_eq!(
        json(again).await?["error"].as_str(),
        Some("account_already_exists")
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn no_recovery_key_is_accepted_or_returned() -> Result<()> {
    // Deliberately not named after the recovery key: the owner localpart is
    // derived from this name, and the assertion below is that the *answer*
    // never mentions one.
    let (gateway, base, _state_dir, owner) = start("no-key-in-or-out").await?;
    let client = client()?;

    // A client that tried to send one is refused, whatever it calls the
    // field: screen 2's "Twalk never sees it" is a property of the API, not
    // of a convention.
    for spelling in ["recovery_key", "recoveryKey", "security_key"] {
        let response = register_with(
            &client,
            &base,
            &serde_json::json!({
                "username": localpart(&owner),
                "password": OWNER_PASSWORD,
                spelling: "EsTx abcd efgh ijkl mnop qrst uvwx yz23 4567",
            }),
        )
        .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "a registration carrying a {spelling} must be refused"
        );
        let refusal = json(response).await?;
        assert_eq!(
            refusal["error"].as_str(),
            Some("recovery_key_refused"),
            "{refusal}"
        );
    }

    // Nothing was created by any of those: the refusal happens before the
    // registration secret is used, so the one registration is still available.
    let (_user, document) = register_owner(&client, &base, &owner).await?;

    // And the answer returns no recovery key either. Asserted on the document
    // as a whole: the fields are exactly the Matrix session, and no key of any
    // recovery-key spelling appears anywhere in it.
    let fields: Vec<&String> = document
        .as_object()
        .context("the answer is an object")?
        .keys()
        .collect();
    assert_eq!(
        fields,
        vec!["access_token", "device_id", "home_server", "user_id"],
        "the registration answer is the Matrix session and nothing else"
    );
    let rendered = serde_json::to_string(&document)?.to_ascii_lowercase();
    for forbidden in ["recovery", "security_key", "securitykey"] {
        assert!(
            !rendered.contains(forbidden),
            "the registration answer must not mention {forbidden}: {document}"
        );
    }

    // The invitation endpoint refuses one too — there is no bootstrap route a
    // recovery key can reach.
    let user = MatrixUser::with_token(
        document["user_id"].as_str().context("no user id")?,
        document["access_token"]
            .as_str()
            .context("no access token")?,
    );
    let device_token = sign_in(&client, &base, &user).await?;
    let response = invite_rooms(
        &client,
        &base,
        &device_token,
        &serde_json::json!({
            "matrix_access_token": user.matrix_access_token(),
            "rooms": [],
            "recovery_key": "EsTx abcd",
        }),
    )
    .await?;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        json(response).await?["error"].as_str(),
        Some("recovery_key_refused")
    );

    // The refusal is loud, because a client with that bug breaks a promise
    // the user was shown in plain language.
    let logs = gateway.logs().await.join("\n");
    assert!(
        logs.contains("which the Gateway must never see"),
        "the refusal is logged: {logs}"
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn the_sensor_is_invited_into_the_rooms_the_user_selects() -> Result<()> {
    let (gateway, base, _state_dir, owner) = start("invite-rooms").await?;
    let client = client()?;
    let (user, _) = register_owner(&client, &base, &owner).await?;
    let device_token = sign_in(&client, &base, &user).await?;

    // Two of the user's own rooms, and one they will not select.
    let selected = user.create_room("g53 selected room").await?;
    let also_selected = user.create_room("g53 second selected room").await?;
    let not_selected = user.create_room("g53 room the user kept private").await?;

    let response = invite_rooms(
        &client,
        &base,
        &device_token,
        &serde_json::json!({
            "matrix_access_token": user.matrix_access_token(),
            "rooms": [selected, also_selected],
        }),
    )
    .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let document = json(response).await?;
    assert_eq!(
        document["sensor"].as_str(),
        Some(SENSOR_USER_ID),
        "the answer names who was invited: {document}"
    );
    let rooms = document["rooms"].as_array().context("a room list")?;
    assert_eq!(rooms.len(), 2, "{document}");
    for room in rooms {
        assert_eq!(room["status"].as_str(), Some("invited"), "{room}");
    }

    // At the homeserver: the Sensor really is invited (or already joined, if
    // another suite's Sensor is running against this shared stack) — and the
    // room the user did not select is untouched.
    for room in [&selected, &also_selected] {
        let membership = user
            .membership(room, SENSOR_USER_ID)
            .await?
            .unwrap_or_default();
        assert!(
            membership == "invite" || membership == "join",
            "the Sensor must be in {room}, found {membership:?}"
        );
    }
    assert_eq!(
        user.membership(&not_selected, SENSOR_USER_ID).await?,
        None,
        "a room the user did not select is never touched"
    );

    // Asking again is not an error: the user ticked a room Twalk already
    // watches.
    let again = invite_rooms(
        &client,
        &base,
        &device_token,
        &serde_json::json!({
            "matrix_access_token": user.matrix_access_token(),
            "rooms": [selected],
        }),
    )
    .await?;
    assert_eq!(again.status(), reqwest::StatusCode::OK);
    let document = json(again).await?;
    assert_eq!(
        document["rooms"][0]["status"].as_str(),
        Some("already_present"),
        "{document}"
    );

    // One room failing does not fail the others: a malformed id and a room
    // the user is not in, beside a good one.
    let stranger = MatrixUser::login(OTHER_LOCALPART).await?;
    let strangers_room = stranger.create_room("g53 somebody else's room").await?;
    let third = user.create_room("g53 third selected room").await?;
    let mixed = invite_rooms(
        &client,
        &base,
        &device_token,
        &serde_json::json!({
            "matrix_access_token": user.matrix_access_token(),
            "rooms": ["not-a-room-id", strangers_room, third],
        }),
    )
    .await?;
    assert_eq!(mixed.status(), reqwest::StatusCode::OK);
    let rooms = json(mixed).await?;
    let rooms = rooms["rooms"].as_array().context("a room list")?.clone();
    assert_eq!(rooms[0]["status"].as_str(), Some("failed"));
    assert_eq!(rooms[0]["reason"].as_str(), Some("invalid_room_id"));
    assert_eq!(
        rooms[1]["status"].as_str(),
        Some("failed"),
        "{:?}",
        rooms[1]
    );
    assert!(
        rooms[1]["reason"]
            .as_str()
            .is_some_and(|reason| reason.starts_with("M_")),
        "a homeserver refusal keeps its own error code: {:?}",
        rooms[1]
    );
    assert_eq!(
        rooms[2]["status"].as_str(),
        Some("invited"),
        "one room failing must not stop the others: {:?}",
        rooms[2]
    );

    // A token the homeserver does not know fails the whole request, because
    // then nothing was attempted anywhere.
    let rejected = invite_rooms(
        &client,
        &base,
        &device_token,
        &serde_json::json!({
            "matrix_access_token": "syt_not_a_token_this_homeserver_minted",
            "rooms": [third],
        }),
    )
    .await?;
    // `400`, not `401`: a `401` from this origin means the caller's own
    // credentials are not good, and a client is entitled to repair that by
    // refreshing. This is a credential the caller put in the body, for a
    // different server — the Companion's central handler refreshed a healthy
    // session twice per click over it (#141).
    assert_eq!(rejected.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        json(rejected).await?["error"].as_str(),
        Some("matrix_token_rejected")
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn inviting_the_sensor_requires_a_device_token() -> Result<()> {
    let (gateway, base, _state_dir, owner) = start("invite-guard").await?;
    let client = client()?;
    let (user, _) = register_owner(&client, &base, &owner).await?;
    let room = user.create_room("g53 guarded room").await?;

    // No cookie: the guard refuses before the handler sees the token. This is
    // the route that added no row to the guard's table and is protected
    // anyway.
    let response = client
        .post(format!("{base}/api/bootstrap/rooms"))
        .json(&serde_json::json!({
            "matrix_access_token": user.matrix_access_token(),
            "rooms": [room.clone()],
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(
        json(response).await?["error"].as_str(),
        Some("unauthenticated")
    );
    assert_eq!(
        user.membership(&room, SENSOR_USER_ID).await?,
        None,
        "an unauthenticated request invites nobody"
    );

    // With a cookie that is not a token either.
    let response = invite_rooms(
        &client,
        &base,
        "not-a-device-token",
        &serde_json::json!({
            "matrix_access_token": user.matrix_access_token(),
            "rooms": [room.clone()],
        }),
    )
    .await?;
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Signed in, a body that is not an invitation request is a 400 the
    // Companion can parse.
    let device_token = sign_in(&client, &base, &user).await?;
    for body in [
        serde_json::json!({ "rooms": [] }),
        serde_json::json!({ "matrix_access_token": "" , "rooms": [] }),
        serde_json::json!({ "matrix_access_token": "t", "rooms": "not-a-list" }),
    ] {
        let response = invite_rooms(&client, &base, &device_token, &body).await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{body} must be refused"
        );
        assert_eq!(
            json(response).await?["error"].as_str(),
            Some("invalid_request")
        );
    }

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn each_bootstrap_half_is_off_until_its_variable_is_set() -> Result<()> {
    // The relay off, the Sensor known: an operator who provisioned the
    // account by hand and never opened the registration window.
    let (gateway, base, _state_dir, owner) = start_with(
        "bootstrap-relay-off",
        &[("GATEWAY_REGISTRATION_SHARED_SECRET", "")],
    )
    .await?;
    let client = client()?;
    let response = register(&client, &base, localpart(&owner), OWNER_PASSWORD).await?;
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let refusal = json(response).await?;
    assert_eq!(
        refusal["error"].as_str(),
        Some("registration_not_configured")
    );
    assert!(
        refusal["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("GATEWAY_REGISTRATION_SHARED_SECRET")),
        "the answer names the variable that would open it: {refusal}"
    );
    let logs = gateway.logs().await.join("\n");
    assert!(
        logs.contains("the registration relay is off"),
        "the operator is told at startup: {logs}"
    );
    gateway.stop().await;

    // The other way round: the relay on, no Sensor to invite. The owner has
    // to sign in to reach that endpoint at all, so the account is registered
    // first.
    let (gateway, base, _state_dir, owner) =
        start_with("bootstrap-sensor-off", &[("GATEWAY_SENSOR_USER_ID", "")]).await?;
    let (user, _) = register_owner(&client, &base, &owner).await?;
    let device_token = sign_in(&client, &base, &user).await?;
    let response = invite_rooms(
        &client,
        &base,
        &device_token,
        &serde_json::json!({
            "matrix_access_token": user.matrix_access_token(),
            "rooms": [],
        }),
    )
    .await?;
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let refusal = json(response).await?;
    assert_eq!(refusal["error"].as_str(), Some("sensor_not_configured"));
    assert!(
        refusal["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("GATEWAY_SENSOR_USER_ID")),
        "the answer names the variable that would open it: {refusal}"
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn no_matrix_token_password_or_registration_secret_reaches_the_store_or_the_logs(
) -> Result<()> {
    let (gateway, base, state_dir, owner) = start("bootstrap-no-secrets").await?;
    let client = client()?;

    // Everything a bootstrap touches: the relay's answer (a Matrix access
    // token the Gateway must not keep), the OpenID token it verified, the
    // password the user chose, and the registration shared secret itself.
    let (user, document) = register_owner(&client, &base, &owner).await?;
    let registered_token = document["access_token"]
        .as_str()
        .context("no access token")?
        .to_owned();
    let openid_token = user.openid_token().await?;
    let openid_access_token = openid_token["access_token"]
        .as_str()
        .context("the OpenID token has an access token")?
        .to_owned();
    let signed_in = client
        .post(format!("{base}/api/session"))
        .json(&serde_json::json!({
            "matrix_openid_token": openid_token,
            "device_name": "Onboarding laptop",
        }))
        .send()
        .await?;
    assert_eq!(signed_in.status(), reqwest::StatusCode::OK);
    let device_token = signed_in
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';').unwrap_or((value, ""));
            let (key, cookie) = pair.split_once('=')?;
            (key == "twalk_device").then(|| cookie.to_owned())
        })
        .context("a device cookie")?;

    // An invitation, so the user's token has travelled through the Gateway.
    let room = user.create_room("g53 room for the secrets test").await?;
    let invited = invite_rooms(
        &client,
        &base,
        &device_token,
        &serde_json::json!({
            "matrix_access_token": user.matrix_access_token(),
            "rooms": [room],
        }),
    )
    .await?;
    assert_eq!(invited.status(), reqwest::StatusCode::OK);

    // And a refused registration, because a refusal path must not log what it
    // refused either.
    register(&client, &base, OTHER_LOCALPART, "another-password-g53").await?;

    let store = store_bytes(&state_dir)?;
    assert!(
        !store.is_empty(),
        "there is a store to inspect in {}",
        state_dir.display()
    );
    let logs = gateway.logs().await.join("\n");
    assert!(
        logs.contains("created this deployment's one account"),
        "the bootstrap was logged at all: {logs}"
    );
    for (what, secret) in [
        (
            "the Matrix access token the relay answered with",
            registered_token.as_str(),
        ),
        ("the verified OpenID token", openid_access_token.as_str()),
        ("the password the user chose", OWNER_PASSWORD),
        (
            "another password a refused registration carried",
            "another-password-g53",
        ),
        (
            "the homeserver's registration shared secret",
            REGISTRATION_SHARED_SECRET,
        ),
    ] {
        assert!(
            !store.contains(secret),
            "{what} must not appear in the Gateway's store"
        );
        assert!(
            !logs.contains(secret),
            "{what} must not appear in the Gateway's logs"
        );
    }
    // The Gateway's own device token is a credential too.
    assert!(
        !logs.contains(&device_token),
        "the issued device token must not appear in the Gateway's logs"
    );
    // What the store does hold about the account is the user id and a date —
    // enough to refuse a second registration, and nothing else.
    assert!(
        store.contains(&owner),
        "the store remembers which account was created: {}",
        state_dir.display()
    );

    gateway.stop().await;
    Ok(())
}

/// Ticket #112: a returning browser can **ask** whether this deployment has
/// its account, instead of offering to create one and reading the refusal.
///
/// The value has to move — a deployment is not bootstrapped and then is — so
/// both sides are asserted around the one registration that changes it. A test
/// that only checked the `true` would pass against a handler that always said
/// `true`, which is the answer that sends a first user to a sign-in screen for
/// an account nobody has created.
#[tokio::test]
async fn the_deployment_says_whether_it_has_its_account() -> Result<()> {
    let (gateway, base, _state_dir, owner) = start("deployment-bootstrapped").await?;
    let client = client()?;

    let before = json(client.get(format!("{base}/api/deployment")).send().await?).await?;
    assert_eq!(
        before["bootstrapped"].as_bool(),
        Some(false),
        "before any registration, this deployment has no account"
    );
    assert_eq!(
        before["homeserver"].as_str(),
        Some(SERVER_NAME),
        "and it names the server its owner would be on"
    );
    assert!(
        before.get("owner").is_none(),
        "the owner's Matrix ID is never published to an unauthenticated caller: \
         naming the human who owns a deployment is a different disclosure, and \
         no screen needs it before sign-in"
    );

    register_owner(&client, &base, &owner).await?;

    let after = json(client.get(format!("{base}/api/deployment")).send().await?).await?;
    assert_eq!(
        after["bootstrapped"].as_bool(),
        Some(true),
        "once the account exists, the deployment says so"
    );

    gateway.stop().await;
    Ok(())
}
