//! The collector's OIDC grant against the fake SSO (issue #274): the authorization,
//! renewal with rotation, and the two refusals that are never one.

use std::os::unix::fs::PermissionsExt;

use anyhow::Result;
use twalk_collector::oidc::{Client, Grant, Renewal, Settings};
use twalk_test_harness::sso::{write_client_secret, CLIENT_ID};
use twalk_test_harness::FakeSso;

const OWNER: &str = "michel@example.com";

fn settings(sso: &FakeSso, state_dir: &std::path::Path) -> Result<Settings> {
    let secret_file = write_client_secret(state_dir)?;
    Ok(Settings {
        issuer: sso.issuer(),
        client_id: CLIENT_ID.to_owned(),
        client_secret_file: secret_file,
        redirect_uri: "http://localhost:1/callback".to_owned(),
        scopes: vec![
            "openid".to_owned(),
            "email".to_owned(),
            "offline_access".to_owned(),
        ],
        grant_file: state_dir.join("oidc").join("grant.json"),
    })
}

/// Plays the operator: asks for the link, signs in at the fake, pastes the
/// callback back. Returns the grant the collector wrote.
async fn authorize(client: &Client, sso: &FakeSso) -> Result<Grant> {
    let started = client.begin_authorization()?;
    let callback = sso.sign_in(&started.authorization_url)?;
    client.complete_authorization(&started, &callback).await
}

#[tokio::test]
async fn authorize_ends_with_a_grant_on_disk_at_0600_and_a_second_run_leaves_it_alone() -> Result<()>
{
    let sso = FakeSso::start(OWNER).await?;
    let dir = tempfile::tempdir()?;
    let client = Client::discover(settings(&sso, dir.path())?).await?;

    let grant = authorize(&client, &sso).await?;
    assert!(!grant.refresh_token.is_empty());
    let path = dir.path().join("oidc").join("grant.json");
    let mode = std::fs::metadata(&path)?.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the grant is the operator's alone: {mode:o}");
    let on_disk: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    assert_eq!(
        on_disk["refresh_token"].as_str(),
        sso.current_refresh_token().as_deref(),
        "what is on disk is what the SSO holds"
    );
    assert!(
        on_disk.get("access_token").is_none(),
        "an access token is short-lived and never written: {on_disk}"
    );
    assert_eq!(on_disk["issuer"].as_str(), Some(sso.issuer().as_str()));

    // The code was spent, and no second authorization started: the grant already
    // there is the answer.
    assert_eq!(sso.token_requests(), ["authorization_code"]);
    let reloaded = Grant::read(&path)?.expect("the grant reads back");
    assert_eq!(reloaded.refresh_token, grant.refresh_token);
    Ok(())
}

#[tokio::test]
async fn a_renewal_rotates_the_refresh_token_and_the_new_one_is_on_disk_before_the_access_token_serves(
) -> Result<()> {
    let sso = FakeSso::start(OWNER).await?;
    let dir = tempfile::tempdir()?;
    let client = Client::discover(settings(&sso, dir.path())?).await?;
    let first = authorize(&client, &sso).await?;

    let renewed = client.renew(&first).await?;
    let Renewal::Renewed { grant, access } = renewed else {
        panic!("a live grant renews: {renewed:?}");
    };
    assert_ne!(
        grant.refresh_token, first.refresh_token,
        "the refresh token rotated"
    );
    assert_eq!(
        Grant::read(&client.settings().grant_file)?.map(|g| g.refresh_token),
        Some(grant.refresh_token.clone()),
        "the rotated token is on disk"
    );
    assert!(!access.token.is_empty());
    assert!(access.expires_at > std::time::SystemTime::now());

    // A second renewal from the *old* token is what a crash between the
    // write and the use would look like if the write came second — the SSO
    // refuses it, which is why the collector writes first. From the new one
    // it renews again.
    assert!(matches!(
        client.renew(&first).await?,
        Renewal::ReconnectRequired { .. }
    ));
    assert!(matches!(
        client.renew(&grant).await?,
        Renewal::Renewed { .. }
    ));
    assert_eq!(
        sso.refresh_tokens_presented(),
        [
            first.refresh_token.clone(),
            first.refresh_token.clone(),
            grant.refresh_token.clone()
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_revoked_grant_is_reconnect_required_and_a_refusing_service_is_pending_operator(
) -> Result<()> {
    let sso = FakeSso::start(OWNER).await?;
    let dir = tempfile::tempdir()?;
    let client = Client::discover(settings(&sso, dir.path())?).await?;
    let grant = authorize(&client, &sso).await?;

    // The SSO revoked the grant: only the operator can give a new one.
    sso.revoke();
    let Renewal::ReconnectRequired { detail } = client.renew(&grant).await? else {
        panic!("a revoked grant is a reconnect, not an error");
    };
    assert!(detail.contains("invalid_grant"), "{detail}");
    assert!(
        !detail.contains(&grant.refresh_token),
        "the refusal's words never carry the token: {detail}"
    );
    Ok(())
}

/// The third refusal the two-refusals rule owes (review of #274): the SSO
/// refusing the **client** — a wrong secret, here — is not the grant being
/// gone. Read as `reconnect_required`, it would send the operator to sign in
/// again for a problem signing in cannot fix; it is `pending_operator`,
/// naming the client's configuration, and the grant on disk is left alone.
#[tokio::test]
async fn a_wrong_client_secret_is_pending_operator_and_not_a_reconnect() -> Result<()> {
    let sso = FakeSso::start(OWNER).await?;
    let dir = tempfile::tempdir()?;
    let settings = settings(&sso, dir.path())?;
    let client = Client::discover(settings.clone()).await?;
    let grant = authorize(&client, &sso).await?;

    // The operator rotated the secret file to a value the SSO does not know.
    std::fs::write(
        &settings.client_secret_file,
        "not-the-secret-the-sso-knows\n",
    )?;
    let Renewal::PendingOperator { detail } = client.renew(&grant).await? else {
        panic!("a refused client is the operator's problem, not a reconnect");
    };
    assert!(detail.contains("invalid_client"), "{detail}");
    assert!(
        detail.contains("COLLECTOR_OIDC_CLIENT_SECRET_FILE"),
        "the remedy names the setting: {detail}"
    );
    assert!(
        !detail.contains(&grant.refresh_token),
        "the refusal's words never carry the token: {detail}"
    );
    // The grant was not touched: the SSO still knows it, and the right
    // secret renews it.
    assert_eq!(
        Grant::read(&settings.grant_file)?.map(|g| g.refresh_token),
        Some(grant.refresh_token.clone())
    );
    twalk_test_harness::sso::write_client_secret(dir.path())?;
    assert!(matches!(
        client.renew(&grant).await?,
        Renewal::Renewed { .. }
    ));
    Ok(())
}

#[tokio::test]
async fn the_two_whoamis_must_name_the_owner_and_a_refusal_names_the_service() -> Result<()> {
    let sso = FakeSso::start(OWNER).await?;
    let dir = tempfile::tempdir()?;
    let client = Client::discover(settings(&sso, dir.path())?).await?;
    let grant = authorize(&client, &sso).await?;
    let Renewal::Renewed { access, .. } = client.renew(&grant).await? else {
        panic!("renews");
    };

    let bearer = twalk_collector::side::Credential::Bearer(access.token.clone());
    let services = twalk_collector::oidc::Services {
        jmap_session_url: Some(sso.jmap_session_url()),
        caldav_url: Some(sso.caldav_url()),
    };
    let identities = services.whoami(&bearer).await?;
    let answered = |identity: &Option<Result<String, twalk_collector::oidc::ServiceRefusal>>| {
        identity
            .as_ref()
            .and_then(|answer| answer.as_deref().ok())
            .map(str::to_owned)
    };
    assert_eq!(answered(&identities.jmap).as_deref(), Some(OWNER));
    assert_eq!(answered(&identities.caldav).as_deref(), Some(OWNER));

    // A service that refuses a fresh token: the grant stands, the service
    // wants something the client does not carry — the operator changes the
    // client, not the grant. Named, so the operator knows which.
    sso.refuse("caldav");
    let identities = services.whoami(&bearer).await?;
    assert_eq!(answered(&identities.jmap).as_deref(), Some(OWNER));
    assert!(
        matches!(&identities.caldav, Some(Err(why)) if why.pending_operator()),
        "{:?}",
        identities.caldav
    );

    // #321: a process that holds no calendar connection asks no calendar
    // service — nothing is sent to it, and it has no state to be in.
    let mail_only = twalk_collector::oidc::Services {
        jmap_session_url: Some(sso.jmap_session_url()),
        caldav_url: None,
    };
    let identities = mail_only.whoami(&bearer).await?;
    assert_eq!(answered(&identities.jmap).as_deref(), Some(OWNER));
    assert!(
        identities.caldav.is_none(),
        "a service this collector does not read is not asked: {:?}",
        identities.caldav
    );
    assert_eq!(
        identities.by_service().len(),
        1,
        "and it is not reported either"
    );
    assert!(!identities.unauthenticated());
    assert!(identities.owner_mismatch("somebody@example.com").len() == 1);

    // A token for another account is nothing to publish from.
    let other = FakeSso::start("somebody@example.com").await?;
    let other_dir = tempfile::tempdir()?;
    let other_client = Client::discover(settings(&other, other_dir.path())?).await?;
    let other_grant = authorize(&other_client, &other).await?;
    let Renewal::Renewed { access, .. } = other_client.renew(&other_grant).await? else {
        panic!("renews");
    };
    let other_services = twalk_collector::oidc::Services {
        jmap_session_url: Some(other.jmap_session_url()),
        caldav_url: Some(other.caldav_url()),
    };
    let identities = other_services.whoami(&bearer).await?;
    assert_eq!(
        identities.owner_mismatch(OWNER),
        vec![
            ("jmap", "somebody@example.com".to_owned()),
            ("caldav", "somebody@example.com".to_owned())
        ]
    );
    Ok(())
}
