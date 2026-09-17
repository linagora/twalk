//! Ticket #52, sign-in: the Companion sends a Matrix OpenID token, the
//! Gateway verifies it at the homeserver, checks the identity against the
//! single owner from its configuration, and issues its own per-device token.
//!
//! The seam is the process boundary, as everywhere else in this repository:
//! a real Synapse mints the OpenID tokens, the real Gateway binary verifies
//! them over the network, and every assertion below is an HTTP call, a read
//! of the Gateway's store, or a read of its log output. Nothing reaches
//! inside the process — in particular, "no Matrix access token is stored or
//! logged" is asserted against the bytes of the store and the captured log
//! lines, not by reading the source.

mod harness;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use harness::{
    companion_build, ensure_stack, gateway_env, gateway_env_without_sign_in, gateway_state_dir,
    owner_user_id, poll_until, GatewayProc, MatrixUser, OTHER_LOCALPART, OWNER_LOCALPART,
    SERVER_NAME,
};

/// The Gateway under test, its origin, and the directory it keeps its store
/// in. The stack is up before it starts: signing in is a call to the
/// homeserver.
async fn start(test_name: &str) -> Result<(GatewayProc, String, PathBuf)> {
    ensure_stack().await?;
    let static_dir = companion_build(test_name)?;
    let state_dir = gateway_state_dir(&static_dir);
    let gateway = GatewayProc::start(&gateway_env(&static_dir))?;
    let base = gateway.base_url().await?;
    wait_until_answering(&base).await?;
    Ok((gateway, base, state_dir))
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
/// the session cookie by hand, which is also how it asserts on its
/// attributes.
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

/// The `Set-Cookie` values of a response, whole.
fn set_cookies(response: &reqwest::Response) -> Vec<String> {
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|value| value.to_owned())
        .collect()
}

/// One named cookie's value out of a response's `Set-Cookie` headers.
fn cookie(response: &reqwest::Response, name: &str) -> Option<String> {
    set_cookies(response).into_iter().find_map(|value| {
        let (pair, _) = value.split_once(';').unwrap_or((value.as_str(), ""));
        let (key, cookie_value) = pair.split_once('=')?;
        (key == name).then(|| cookie_value.to_owned())
    })
}

/// The whole `Set-Cookie` header line for a named cookie: what the attribute
/// assertions read.
fn set_cookie_line(response: &reqwest::Response, name: &str) -> Option<String> {
    set_cookies(response)
        .into_iter()
        .find(|value| value.starts_with(&format!("{name}=")))
}

/// Signs a device in with a freshly minted OpenID token for `user`.
async fn sign_in(
    client: &reqwest::Client,
    base: &str,
    user: &MatrixUser,
    device_name: &str,
) -> Result<reqwest::Response> {
    let token = user.openid_token().await?;
    sign_in_with(client, base, &token, device_name).await
}

/// Signs in with a given OpenID token document — the same call, with the
/// token under the test's control (a replay, a foreign homeserver).
async fn sign_in_with(
    client: &reqwest::Client,
    base: &str,
    token: &serde_json::Value,
    device_name: &str,
) -> Result<reqwest::Response> {
    client
        .post(format!("{base}/api/session"))
        .json(&serde_json::json!({
            "matrix_openid_token": token,
            "device_name": device_name,
        }))
        .send()
        .await
        .context("failed to post a sign-in")
}

/// A GET carrying a device cookie.
async fn get_with_device(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    device_token: &str,
) -> Result<reqwest::Response> {
    Ok(client
        .get(format!("{base}{path}"))
        .header("cookie", format!("twalk_device={device_token}"))
        .send()
        .await?)
}

async fn json(response: reqwest::Response) -> Result<serde_json::Value> {
    let body = response.text().await?;
    serde_json::from_str(&body).with_context(|| format!("not JSON: {body}"))
}

/// Everything the Gateway wrote to its state directory, as one string: the
/// store, its write-ahead log and any sibling file. WAL mode means a row can
/// live in `sessions.db-wal` for a while, so the assertion has to cover the
/// directory rather than the database file alone.
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

#[tokio::test]
async fn the_owners_openid_token_signs_in_and_the_cookie_authenticates() -> Result<()> {
    let (gateway, base, _state_dir) = start("signin-owner").await?;
    let client = client()?;
    let owner = MatrixUser::login(OWNER_LOCALPART).await?;

    let response = sign_in(&client, &base, &owner, "Owner's laptop").await?;
    assert_eq!(
        response.status(),
        reqwest::StatusCode::OK,
        "the owner's OpenID token signs in"
    );

    // The credential is a cookie the Companion's own JavaScript cannot read,
    // on the same origin that serves it.
    let line = set_cookie_line(&response, "twalk_device")
        .unwrap_or_else(|| panic!("no device cookie in {:?}", set_cookies(&response)));
    for attribute in ["HttpOnly", "SameSite=Lax", "Path=/", "Max-Age="] {
        assert!(line.contains(attribute), "{attribute} missing from {line}");
    }
    assert!(
        !line.contains("Secure"),
        "on a localhost origin the Secure flag would stop the cookie working: {line}"
    );
    let device_token = cookie(&response, "twalk_device").expect("a device token");
    let refresh_line = set_cookie_line(&response, "twalk_refresh").expect("a refresh cookie");
    assert!(
        refresh_line.contains("Path=/api/session"),
        "the refresh cookie is scoped to the refresh endpoint: {refresh_line}"
    );

    let document = json(response).await?;
    assert_eq!(document["owner"].as_str(), Some(owner_user_id().as_str()));
    assert_eq!(document["homeserver"].as_str(), Some(SERVER_NAME));
    assert_eq!(document["device"]["name"].as_str(), Some("Owner's laptop"));
    assert!(
        document["device"]["created_unix_seconds"]
            .as_u64()
            .is_some_and(|created| created > 1_700_000_000),
        "the device is dated: {document}"
    );
    assert!(
        document["expires_in"].as_u64().is_some_and(|ttl| ttl > 0),
        "the Companion is told when to refresh: {document}"
    );

    // The device token is what every other endpoint takes.
    let session = get_with_device(&client, &base, "/api/session", &device_token).await?;
    assert_eq!(session.status(), reqwest::StatusCode::OK);
    let session = json(session).await?;
    assert_eq!(
        session["device"]["current"].as_bool(),
        Some(true),
        "the session names the device that asked: {session}"
    );

    let devices =
        json(get_with_device(&client, &base, "/api/devices", &device_token).await?).await?;
    let devices = devices["devices"]
        .as_array()
        .expect("the device list is an array")
        .clone();
    assert_eq!(devices.len(), 1, "one device signed in: {devices:?}");
    assert_eq!(devices[0]["name"].as_str(), Some("Owner's laptop"));
    assert!(devices[0]["last_seen_unix_seconds"].as_u64().is_some());
    assert_eq!(devices[0]["revoked_unix_seconds"], serde_json::Value::Null);

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn another_users_openid_token_is_refused() -> Result<()> {
    let (gateway, base, _state_dir) = start("signin-other-user").await?;
    let client = client()?;
    let other = MatrixUser::login(OTHER_LOCALPART).await?;

    // A perfectly valid token from a perfectly real account of the same
    // homeserver — the case that matters, because the homeserver has other
    // accounts (the Sensor's, for one).
    let response = sign_in(&client, &base, &other, "Someone else's phone").await?;
    assert_eq!(
        response.status(),
        reqwest::StatusCode::FORBIDDEN,
        "only the configured owner signs in"
    );
    assert_eq!(
        json(response).await?["error"].as_str(),
        Some("not_the_owner")
    );

    // And it left nothing behind: the owner's own device list is empty.
    let owner = MatrixUser::login(OWNER_LOCALPART).await?;
    let signed_in = sign_in(&client, &base, &owner, "Owner's laptop").await?;
    let device_token = cookie(&signed_in, "twalk_device").expect("a device token");
    let devices =
        json(get_with_device(&client, &base, "/api/devices", &device_token).await?).await?;
    assert_eq!(
        devices["devices"].as_array().map(Vec::len),
        Some(1),
        "the refused sign-in created no device: {devices}"
    );

    // The refusal is visible to an operator, in the logs and in the metric.
    let logs = gateway.logs().await.join("\n");
    assert!(
        logs.contains("not this deployment's owner"),
        "the refusal is logged: {logs}"
    );
    let exposition = reqwest::get(format!("{base}/metrics"))
        .await?
        .text()
        .await?;
    assert!(
        exposition.contains("twalk_companion_gateway_sign_ins_total{outcome=\"not_the_owner\"} 1"),
        "the refusal is counted:\n{exposition}"
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_replayed_openid_token_is_refused() -> Result<()> {
    let (gateway, base, _state_dir) = start("signin-replay").await?;
    let client = client()?;
    let owner = MatrixUser::login(OWNER_LOCALPART).await?;

    // The homeserver's verification does not consume the token: it answers
    // the same for an hour. Proving that here is the reason the ledger
    // exists — a captured token must not be a second sign-in.
    let token = owner.openid_token().await?;
    let first = sign_in_with(&client, &base, &token, "First device").await?;
    assert_eq!(first.status(), reqwest::StatusCode::OK);

    let replayed = sign_in_with(&client, &base, &token, "Replay").await?;
    assert_eq!(
        replayed.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "the same OpenID token cannot sign in twice"
    );
    assert_eq!(
        json(replayed).await?["error"].as_str(),
        Some("openid_token_replayed")
    );

    // The homeserver still accepts the token, which is the point: the refusal
    // is the Gateway's own.
    let still_valid = reqwest::get(format!(
        "{}/_matrix/federation/v1/openid/userinfo?access_token={}",
        harness::synapse_url(),
        token["access_token"].as_str().expect("a token")
    ))
    .await?;
    assert_eq!(
        still_valid.status(),
        reqwest::StatusCode::OK,
        "the token is still valid at the homeserver: replay protection is the Gateway's"
    );

    // And a fresh token signs in as usual.
    let fresh = sign_in(&client, &base, &owner, "Second device").await?;
    assert_eq!(fresh.status(), reqwest::StatusCode::OK);

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_revoked_devices_cookie_is_refused_immediately() -> Result<()> {
    let (gateway, base, _state_dir) = start("signin-revoke").await?;
    let client = client()?;
    let owner = MatrixUser::login(OWNER_LOCALPART).await?;

    let kept = sign_in(&client, &base, &owner, "Kept laptop").await?;
    let kept_token = cookie(&kept, "twalk_device").expect("a device token");
    let lost = sign_in(&client, &base, &owner, "Lost phone").await?;
    let lost_token = cookie(&lost, "twalk_device").expect("a device token");
    let lost_refresh = cookie(&lost, "twalk_refresh").expect("a refresh token");
    let lost_id = json(lost).await?["device"]["id"]
        .as_str()
        .expect("the device has an id")
        .to_owned();

    // The lost phone works right up to the moment it is revoked.
    assert_eq!(
        get_with_device(&client, &base, "/api/session", &lost_token)
            .await?
            .status(),
        reqwest::StatusCode::OK
    );

    let revoked = client
        .delete(format!("{base}/api/devices/{lost_id}"))
        .header("cookie", format!("twalk_device={kept_token}"))
        .send()
        .await?;
    assert_eq!(revoked.status(), reqwest::StatusCode::NO_CONTENT);

    // Immediately: the very next request.
    let after = get_with_device(&client, &base, "/api/session", &lost_token).await?;
    assert_eq!(
        after.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a revoked device's token stops working at once"
    );
    assert_eq!(
        json(after).await?["error"].as_str(),
        Some("unauthenticated")
    );

    // And it cannot refresh its way back in.
    let refreshed = client
        .post(format!("{base}/api/session/refresh"))
        .header("cookie", format!("twalk_refresh={lost_refresh}"))
        .send()
        .await?;
    assert_eq!(
        refreshed.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a revoked device's refresh token is not a way back"
    );

    // The kept laptop is untouched, and the list keeps the revoked device
    // with the date it was revoked on.
    let devices = json(get_with_device(&client, &base, "/api/devices", &kept_token).await?).await?;
    let devices = devices["devices"].as_array().expect("an array").clone();
    assert_eq!(devices.len(), 2, "{devices:?}");
    let lost_entry = devices
        .iter()
        .find(|device| device["id"].as_str() == Some(&lost_id))
        .expect("the revoked device is still listed");
    assert!(
        lost_entry["revoked_unix_seconds"].as_u64().is_some(),
        "the revocation is dated: {lost_entry}"
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn the_device_token_is_short_lived_and_the_refresh_token_rotates_it() -> Result<()> {
    ensure_stack().await?;
    let static_dir = companion_build("signin-refresh")?;
    // A one-second device token, so the expiry is observable at the process
    // boundary rather than only in a unit test.
    let env = harness::gateway_env_with(&static_dir, &[("GATEWAY_DEVICE_TOKEN_TTL", "1")]);
    let gateway = GatewayProc::start(&env)?;
    let base = gateway.base_url().await?;
    wait_until_answering(&base).await?;
    let client = client()?;
    let owner = MatrixUser::login(OWNER_LOCALPART).await?;

    let signed_in = sign_in(&client, &base, &owner, "Phone").await?;
    let first_token = cookie(&signed_in, "twalk_device").expect("a device token");
    let refresh_token = cookie(&signed_in, "twalk_refresh").expect("a refresh token");
    let document = json(signed_in).await?;
    assert_eq!(
        document["expires_in"].as_u64(),
        Some(1),
        "the response states the short lifetime: {document}"
    );

    let expired = poll_until(
        || async {
            let response = get_with_device(&client, &base, "/api/session", &first_token)
                .await
                .ok()?;
            (response.status() == reqwest::StatusCode::UNAUTHORIZED).then_some(())
        },
        "the device token to expire",
    )
    .await;
    expired.context("a short-lived device token must stop working")?;

    // Refresh: same device, new tokens.
    let refreshed = client
        .post(format!("{base}/api/session/refresh"))
        .header("cookie", format!("twalk_refresh={refresh_token}"))
        .send()
        .await?;
    assert_eq!(refreshed.status(), reqwest::StatusCode::OK);
    let next_token = cookie(&refreshed, "twalk_device").expect("a new device token");
    let next_refresh = cookie(&refreshed, "twalk_refresh").expect("a new refresh token");
    assert_ne!(next_token, first_token, "the device token is rotated");
    assert_ne!(next_refresh, refresh_token, "the refresh token is rotated");
    let refreshed = json(refreshed).await?;
    assert_eq!(
        refreshed["device"]["id"].as_str(),
        document["device"]["id"].as_str(),
        "refreshing keeps the device's identity in the list"
    );
    assert_eq!(
        get_with_device(&client, &base, "/api/session", &next_token)
            .await?
            .status(),
        reqwest::StatusCode::OK,
        "the refreshed token authenticates"
    );

    // The rotated-away refresh token is spent.
    let reused = client
        .post(format!("{base}/api/session/refresh"))
        .header("cookie", format!("twalk_refresh={refresh_token}"))
        .send()
        .await?;
    assert_eq!(
        reused.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a refresh token is good once"
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn no_matrix_access_token_reaches_the_gateways_store_or_its_logs() -> Result<()> {
    let (gateway, base, state_dir) = start("signin-no-matrix-token").await?;
    let client = client()?;
    let owner = MatrixUser::login(OWNER_LOCALPART).await?;

    let openid_token = owner.openid_token().await?;
    let openid_access_token = openid_token["access_token"]
        .as_str()
        .expect("the OpenID token has an access token")
        .to_owned();
    let signed_in = sign_in_with(&client, &base, &openid_token, "Laptop").await?;
    assert_eq!(signed_in.status(), reqwest::StatusCode::OK);
    let device_token = cookie(&signed_in, "twalk_device").expect("a device token");
    // Some traffic, so there is something to have written it down.
    get_with_device(&client, &base, "/api/session", &device_token).await?;
    get_with_device(&client, &base, "/api/devices", &device_token).await?;
    // A refused sign-in too: a refusal path must not log what it refused.
    let other = MatrixUser::login(OTHER_LOCALPART).await?;
    sign_in(&client, &base, &other, "Someone else").await?;

    let store = store_bytes(&state_dir)?;
    assert!(
        !store.is_empty(),
        "there is a store to inspect in {}",
        state_dir.display()
    );
    for (what, secret) in [
        // The user's Matrix access token: the Gateway is never even given
        // it, and this asserts the test itself did not leak it in.
        (
            "the owner's Matrix access token",
            owner.matrix_access_token(),
        ),
        (
            "the other user's Matrix access token",
            other.matrix_access_token(),
        ),
        // The OpenID token it did verify: the replay ledger keeps a digest.
        ("the verified OpenID token", openid_access_token.as_str()),
    ] {
        assert!(
            !store.contains(secret),
            "{what} must not appear in the Gateway's store"
        );
    }

    let logs = gateway.logs().await.join("\n");
    assert!(
        logs.contains("a device signed in"),
        "the sign-in was logged at all: {logs}"
    );
    for (what, secret) in [
        (
            "the owner's Matrix access token",
            owner.matrix_access_token(),
        ),
        (
            "the other user's Matrix access token",
            other.matrix_access_token(),
        ),
        ("the verified OpenID token", openid_access_token.as_str()),
        // The Gateway's own device token is a credential too.
        ("the issued device token", device_token.as_str()),
    ] {
        assert!(
            !logs.contains(secret),
            "{what} must not appear in the Gateway's logs"
        );
    }

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn every_api_endpoint_requires_the_device_token() -> Result<()> {
    let (gateway, base, _state_dir) = start("signin-guard").await?;
    let client = client()?;

    // Without a cookie: 401 on everything, including a route that does not
    // exist — an unauthenticated caller learns nothing about the API's shape.
    for (method, path) in [
        ("GET", "/api/session"),
        ("DELETE", "/api/session"),
        ("GET", "/api/devices"),
        ("DELETE", "/api/devices/whatever"),
        // The one route that takes a service token instead (#50) refuses a
        // caller with no credential the same way, and with the same code.
        ("GET", "/api/consent/snapshot"),
        ("GET", "/api/not/a/route"),
    ] {
        let response = client
            .request(method.parse()?, format!("{base}{path}"))
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "{method} {path} must require a credential"
        );
        assert_eq!(
            json(response).await?["error"].as_str(),
            Some("unauthenticated"),
            "{method} {path}"
        );
    }

    // A made-up cookie is not a token, and neither is a stale-looking one.
    for token in ["", "not-a-token", &"0".repeat(64)] {
        assert_eq!(
            get_with_device(&client, &base, "/api/devices", token)
                .await?
                .status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "cookie {token:?}"
        );
    }

    // Signed in, an unknown API path is a JSON 404 again: the Companion's
    // fetch must never be handed the app shell.
    let owner = MatrixUser::login(OWNER_LOCALPART).await?;
    let signed_in = sign_in(&client, &base, &owner, "Laptop").await?;
    let device_token = cookie(&signed_in, "twalk_device").expect("a device token");
    let unknown = get_with_device(&client, &base, "/api/not/a/route", &device_token).await?;
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(json(unknown).await?["error"].as_str(), Some("not_found"));

    // The Companion's own files stay public: the app shell has to load
    // before anyone can sign in.
    assert_eq!(
        reqwest::get(format!("{base}/")).await?.status(),
        reqwest::StatusCode::OK
    );

    // Signing out revokes this device, and its cookie stops working.
    let signed_out = client
        .delete(format!("{base}/api/session"))
        .header("cookie", format!("twalk_device={device_token}"))
        .send()
        .await?;
    assert_eq!(signed_out.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(
        set_cookies(&signed_out)
            .iter()
            .any(|line| line.starts_with("twalk_device=;") && line.contains("Max-Age=0")),
        "signing out clears the cookie: {:?}",
        set_cookies(&signed_out)
    );
    assert_eq!(
        get_with_device(&client, &base, "/api/session", &device_token)
            .await?
            .status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a signed-out device is a revoked device"
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn a_token_from_another_homeserver_is_refused() -> Result<()> {
    let (gateway, base, _state_dir) = start("signin-foreign-homeserver").await?;
    let client = client()?;
    let owner = MatrixUser::login(OWNER_LOCALPART).await?;

    // A real token, relabelled as another homeserver's: the Gateway serves
    // one homeserver and refuses before it asks anyone anything.
    let mut token = owner.openid_token().await?;
    token["matrix_server_name"] = serde_json::json!("evil.example");
    let response = sign_in_with(&client, &base, &token, "Laptop").await?;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        json(response).await?["error"].as_str(),
        Some("foreign_homeserver")
    );

    // A token the homeserver never minted.
    let response = sign_in_with(
        &client,
        &base,
        &serde_json::json!({
            "access_token": "not-a-token-this-homeserver-ever-minted",
            "matrix_server_name": SERVER_NAME,
        }),
        "Laptop",
    )
    .await?;
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(
        json(response).await?["error"].as_str(),
        Some("openid_token_rejected")
    );

    // A body that is not a sign-in at all.
    let response = client
        .post(format!("{base}/api/session"))
        .json(&serde_json::json!({ "device_name": "Laptop" }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        json(response).await?["error"].as_str(),
        Some("invalid_request")
    );

    gateway.stop().await;
    Ok(())
}

#[tokio::test]
async fn without_an_owner_the_origin_serves_the_companion_and_closes_its_api() -> Result<()> {
    let static_dir = companion_build("signin-unconfigured")?;
    let gateway = GatewayProc::start(&gateway_env_without_sign_in(&static_dir))?;
    let base = gateway.base_url().await?;
    wait_until_answering(&base).await?;

    // The origin is up — an operator who forgot GATEWAY_OWNER still gets the
    // page that can say so, and the health endpoint a process manager wants.
    assert_eq!(
        reqwest::get(format!("{base}/")).await?.status(),
        reqwest::StatusCode::OK
    );

    // And the API is closed, in the safe direction: nothing can be
    // authenticated, so nothing can be decided.
    for path in ["/api/session", "/api/devices", "/api/consent/snapshot"] {
        let response = reqwest::get(format!("{base}{path}")).await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "{path} with no owner configured"
        );
        let body = json(response).await?;
        assert_eq!(body["error"].as_str(), Some("sign_in_not_configured"));
        assert!(
            body["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("GATEWAY_OWNER")),
            "the answer names the variable that would open it: {body}"
        );
    }

    let logs = gateway.logs().await.join("\n");
    assert!(
        logs.contains("GATEWAY_OWNER is not set"),
        "the operator is warned loudly at startup: {logs}"
    );

    gateway.stop().await;
    Ok(())
}
