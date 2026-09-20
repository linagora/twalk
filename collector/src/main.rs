//! The collector's binary (issue #274): `twalk-collector authorize [--renew]`
//! gives it an OIDC grant with the operator in the loop; `twalk-collector`
//! alone runs — renews the grant, checks whose it is, and says what state
//! each connection is in on the bus.
//!
//! What this binary does **not** do yet is collect anything: #276 (mail) and
//! #280 (calendar) plug into the loop below with the access token it keeps
//! fresh. What it does do is refuse to start on a connection the Gateway's
//! registry does not know, and refuse to publish anything when the grant
//! turns out to be for another account — both said in words, once.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use tracing::{error, info, warn};
use twalk_collector::config::Config;
use twalk_collector::metrics::Metrics;
use twalk_collector::oidc::{Client, Grant, Identities, Renewal, ServiceRefusal};
use twalk_collector::status::{self, Observation, State, Tracker};

/// Renew when the access token has less than this left.
const RENEWAL_MARGIN: Duration = Duration::from_secs(120);

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    tracing_subscriber::fmt()
        .with_env_filter(&config.log_level)
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("authorize") => {
            let renew = args.any(|arg| arg == "--renew");
            authorize(&config, renew).await
        }
        Some(other) => {
            anyhow::bail!("unknown command {other:?}: twalk-collector [authorize [--renew]]")
        }
        None => run(config).await,
    }
}

/// The operator gives the collector a grant. Idempotent: a grant already on
/// disk is left alone unless `--renew` says to replace it. Prints the link
/// to open and reads the callback URL from stdin; prints the two `whoami`s
/// and never a token.
async fn authorize(config: &Config, renew: bool) -> Result<()> {
    let client = Client::discover(config.oidc.clone()).await?;
    if let Some(existing) = Grant::read(&config.oidc.grant_file)? {
        if !renew {
            eprintln!(
                "A grant for {} at {} is already in {} (obtained {}). Nothing to do; pass --renew to replace it.",
                existing.client_id,
                existing.issuer,
                config.oidc.grant_file.display(),
                existing.obtained_at
            );
            return Ok(());
        }
        eprintln!(
            "Replacing the grant in {} (--renew).",
            config.oidc.grant_file.display()
        );
    }
    let started = client.begin_authorization()?;
    eprintln!();
    eprintln!(
        "1. Open this link in a browser and sign in as {}:",
        config.owner_email
    );
    eprintln!();
    eprintln!("   {}", started.authorization_url);
    eprintln!();
    eprintln!(
        "2. The SSO will redirect the browser to {} — nothing listens there. Copy the whole \
         address from the address bar and paste it here, then press Enter:",
        config.oidc.redirect_uri
    );
    eprintln!();
    let mut callback = String::new();
    std::io::stdin()
        .read_line(&mut callback)
        .context("failed to read the callback URL from stdin")?;
    let grant = client
        .complete_authorization(&started, callback.trim())
        .await?;
    eprintln!();
    eprintln!(
        "Grant written to {} (mode 0600).",
        config.oidc.grant_file.display()
    );

    // The two whoamis: the grant must be the owner's, or the collector will
    // refuse to publish anything from it.
    let access = match client.renew(&grant).await? {
        Renewal::Renewed { access, .. } => access,
        Renewal::ReconnectRequired { detail }
        | Renewal::PendingOperator { detail }
        | Renewal::Unreachable { detail } => {
            anyhow::bail!("the grant just obtained could not be renewed: {detail}");
        }
    };
    let identities = config.services.whoami(&access).await?;
    let mut unanswered = Vec::new();
    for (service, identity) in identities.by_service() {
        match identity {
            Ok(account) => eprintln!("   {service} answers as {account}"),
            Err(refusal) => {
                eprintln!("   {service}: {}", refusal.detail());
                unanswered.push((service, refusal));
            }
        }
    }
    let mismatched = identities.owner_mismatch(&config.owner_email);
    if !mismatched.is_empty() {
        // Not kept: a stranger's grant on disk would be protected by the
        // next run's idempotence, and would be renewed for as long as the
        // collector ran. The SSO still holds it until its owner revokes it.
        std::fs::remove_file(&config.oidc.grant_file).with_context(|| {
            format!(
                "failed to remove the refused grant {}",
                config.oidc.grant_file.display()
            )
        })?;
        anyhow::bail!(
            "the grant is not {}'s: {} — the collector would publish nothing from it, so it was \
             not kept ({} removed; the SSO still holds the grant until that account revokes \
             it). Sign in as the owner and run authorize again.",
            config.owner_email,
            mismatched
                .iter()
                .map(|(service, account)| format!("{service} answers as {account}"))
                .collect::<Vec<_>>()
                .join(", "),
            config.oidc.grant_file.display()
        );
    }
    if unanswered.is_empty() {
        eprintln!(
            "Both services answer as {}. The collector can start.",
            config.owner_email
        );
    } else {
        // The grant is the owner's as far as anyone answered; what did not
        // answer is said, with the state the collector will report for it.
        for (service, refusal) in &unanswered {
            let state = if refusal.pending_operator() {
                State::PendingOperator
            } else {
                State::Unreachable
            };
            eprintln!(
                "   The collector will report the {service} connection as {} until {service} \
                 answers as {}.",
                state.as_str(),
                config.owner_email
            );
        }
        eprintln!("The collector can start.");
    }
    Ok(())
}

/// The run loop: the grant kept fresh, the services asked whose it is, and
/// each connection's state on the bus.
async fn run(config: Config) -> Result<()> {
    info!(issuer = %config.oidc.issuer, connections = ?config.connections.iter().map(|c| &c.id).collect::<Vec<_>>(), "collector starting");
    let metrics = Arc::new(Metrics::new());
    if let Some(listen) = config.metrics_listen {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .with_context(|| format!("failed to bind the metrics endpoint on {listen}"))?;
        info!(%listen, "serving metrics");
        tokio::spawn(serve_metrics(listener, metrics.clone()));
    }

    // The registry: a connection this process holds must be one the Gateway
    // names, or every event it published would be about a perimeter no
    // decision governs (ADR 0033). Refused at start, in words.
    if let (Some(url), Some(token)) = (&config.gateway_url, &config.gateway_service_token) {
        let known = registry_ids(url, token).await?;
        config.refuse_unknown_connections(&known)?;
    } else {
        warn!("COLLECTOR_GATEWAY_URL is not set: the connections are not checked against the registry");
    }

    let nats = async_nats::connect(&config.nats_url)
        .await
        .context("failed to connect to NATS")?;
    let jetstream = async_nats::jetstream::new(nats);
    jetstream
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: "twalk".to_owned(),
            subjects: vec!["twalk.>".to_owned()],
            ..Default::default()
        })
        .await
        .context("failed to ensure the twalk stream")?;

    let client = Client::discover(config.oidc.clone()).await?;
    let mut trackers: Vec<Tracker> = config
        .connections
        .iter()
        .map(|held| Tracker::new(&held.id, held.kind, &config.host))
        .collect();
    let mut grant = Grant::read(&config.oidc.grant_file)?;
    let mut access: Option<twalk_collector::oidc::AccessToken> = None;
    // Why the SSO last refused to renew, when it did: the grant's fault
    // (`reconnect_required`) or the client's (`pending_operator`) are two
    // different sentences to the operator.
    let mut sso_refusal: Option<Observation> = None;

    loop {
        let now = SystemTime::now();
        let mut renewed_this_round = false;
        let observation = match &grant {
            None => Observation {
                state: State::ReconnectRequired,
                service: Some("sso"),
                hint: Some(format!(
                    "No grant in {}. Run `twalk-collector authorize` on the host and sign in as {}.",
                    config.oidc.grant_file.display(),
                    config.owner_email
                )),
            },
            Some(current) => {
                if access
                    .as_ref()
                    .is_none_or(|token| !token.is_fresh(RENEWAL_MARGIN))
                {
                    match client.renew(current).await? {
                        Renewal::Renewed {
                            grant: rotated,
                            access: fresh,
                        } => {
                            metrics.record_renewal("renewed", unix_seconds(now));
                            grant = Some(rotated);
                            access = Some(fresh);
                            renewed_this_round = true;
                            sso_refusal = None;
                        }
                        Renewal::ReconnectRequired { detail } => {
                            metrics.record_renewal("reconnect_required", unix_seconds(now));
                            access = None;
                            sso_refusal = Some(reconnect_required(&config));
                            warn!(%detail, "the grant could not be renewed");
                        }
                        Renewal::PendingOperator { detail } => {
                            metrics.record_renewal("pending_operator", unix_seconds(now));
                            access = None;
                            sso_refusal = Some(client_refused(&detail));
                            warn!(%detail, "the SSO refused the client");
                        }
                        Renewal::Unreachable { detail } => {
                            metrics.record_renewal("unreachable", unix_seconds(now));
                            warn!(%detail, "the SSO could not be reached");
                        }
                    }
                }
                match &access {
                    None if grant.is_some() => sso_refusal
                        .clone()
                        .unwrap_or_else(|| reconnect_required(&config)),
                    None => Observation {
                        state: State::Unreachable,
                        service: Some("sso"),
                        hint: Some("The SSO did not answer; the collector retries on its own.".to_owned()),
                    },
                    Some(token) => {
                        let mut identities = config.services.whoami(token).await;
                        // A 401 on a token this process believes fresh is
                        // not yet the operator's problem: the SSO may have
                        // revoked the grant under it. Renew first; the SSO's
                        // refusal is `reconnect_required`, and only a
                        // service refusing a token the SSO just issued is
                        // `pending_operator` (issue #274: two refusals).
                        let stale = matches!(&identities, Ok(ids) if ids.unauthenticated() && !renewed_this_round);
                        if stale {
                            match client.renew(grant.as_ref().expect("a token comes from a grant")).await? {
                                Renewal::Renewed {
                                    grant: rotated,
                                    access: fresh,
                                } => {
                                    metrics.record_renewal("renewed", unix_seconds(now));
                                    identities = config.services.whoami(&fresh).await;
                                    grant = Some(rotated);
                                    access = Some(fresh);
                                }
                                Renewal::ReconnectRequired { detail } => {
                                    metrics.record_renewal("reconnect_required", unix_seconds(now));
                                    warn!(%detail, "a service refused the token and the SSO refused to renew the grant");
                                    access = None;
                                    sso_refusal = Some(reconnect_required(&config));
                                    identities = Err(anyhow::anyhow!("revoked"));
                                }
                                Renewal::PendingOperator { detail } => {
                                    metrics.record_renewal("pending_operator", unix_seconds(now));
                                    warn!(%detail, "a service refused the token and the SSO refused the client");
                                    access = None;
                                    sso_refusal = Some(client_refused(&detail));
                                    identities = Err(anyhow::anyhow!("client refused"));
                                }
                                Renewal::Unreachable { detail } => {
                                    metrics.record_renewal("unreachable", unix_seconds(now));
                                    warn!(%detail, "a service refused the token and the SSO could not be reached");
                                }
                            }
                        }
                        match (&access, identities) {
                            (None, _) => sso_refusal
                                .clone()
                                .unwrap_or_else(|| reconnect_required(&config)),
                            (Some(_), Ok(identities)) => observe_services(&config, &identities),
                            (Some(_), Err(error)) => Observation {
                                state: State::Unreachable,
                                service: None,
                                hint: Some(format!("the services could not be asked: {error:#}")),
                            },
                        }
                    }
                }
            }
        };
        // One observation of the grant, published per connection it holds:
        // a calendar connection whose service refused gets its own words.
        let occurred_at = twalk_collector::oidc::now_rfc3339();
        for tracker in &mut trackers {
            let per_connection = per_connection(&observation, tracker.kind());
            metrics.set_connection_state(tracker.connection(), per_connection.state);
            if let Some(envelope) = tracker.observe(&per_connection, &occurred_at) {
                publish(&jetstream, &envelope, &metrics).await;
            }
        }
        tokio::time::sleep(config.health_interval).await;
    }
}

/// The SSO refused the grant: only the operator can give a new one.
fn reconnect_required(config: &Config) -> Observation {
    Observation {
        state: State::ReconnectRequired,
        service: Some("sso"),
        hint: Some(format!(
            "The SSO refused to renew the grant. Run `twalk-collector authorize --renew` \
             on the host and sign in again as {}; nothing is published until then.",
            config.owner_email
        )),
    }
}

/// The SSO refused the client, not the grant: the operator's, at the SSO.
fn client_refused(detail: &str) -> Observation {
    Observation {
        state: State::PendingOperator,
        service: Some("sso"),
        hint: Some(detail.to_owned()),
    }
}

/// What the services said about a token the SSO just issued: whose grant
/// this is and whether each service takes it — the observation that becomes
/// each connection's state.
fn observe_services(config: &Config, identities: &Identities) -> Observation {
    let mismatched = identities.owner_mismatch(&config.owner_email);
    if !mismatched.is_empty() {
        // Named in the log — the account, not a token — and nothing
        // published from it: the grant is somebody else's.
        for (service, account) in &mismatched {
            error!(
                service,
                account,
                owner = %config.owner_email,
                "the grant is not the owner's: nothing is published from this connection"
            );
        }
        return Observation {
            state: State::PendingOperator,
            service: mismatched.first().map(|(service, _)| *service),
            hint: Some(format!(
                "The grant belongs to another account, not {}. Run `twalk-collector authorize --renew` \
                 and sign in as the owner.",
                config.owner_email
            )),
        };
    }
    // Per service, in the order a refusal is reported: the first service
    // that refused names the state; `per_connection` re-labels per kind.
    for (service, identity) in identities.by_service() {
        if let Err(refusal) = identity {
            let state = match refusal {
                ServiceRefusal::PendingOperator { .. } => State::PendingOperator,
                ServiceRefusal::Unreachable { .. } => State::Unreachable,
            };
            return Observation {
                state,
                service: Some(service),
                hint: Some(refusal.detail().to_owned()),
            };
        }
    }
    Observation::connected()
}

/// The observation as one connection experiences it: a service's refusal is
/// that service's connection's state and not the other's — the mail
/// connection is `connected` while the calendar's side service refuses.
fn per_connection(observation: &Observation, kind: &str) -> Observation {
    match observation.service {
        Some("jmap") if kind != "email" => Observation::connected(),
        Some("caldav") if kind != "calendar" => Observation::connected(),
        _ => observation.clone(),
    }
}

async fn publish(
    jetstream: &async_nats::jetstream::Context,
    envelope: &serde_json::Value,
    metrics: &Metrics,
) {
    let id = envelope["id"].as_str().unwrap_or_default().to_owned();
    let mut headers = async_nats::header::HeaderMap::new();
    headers.insert(async_nats::header::NATS_MESSAGE_ID, id.as_str());
    if let Some(connection) = envelope.get("connection").and_then(|v| v.as_str()) {
        headers.insert("connection", connection);
    }
    let payload = serde_json::to_vec(envelope).expect("the envelope is serializable");
    match jetstream
        .publish_with_headers(
            status::bus_subject(status::STATUS_CHANGED_TYPE),
            headers,
            payload.into(),
        )
        .await
    {
        Ok(ack) => match ack.await {
            Ok(_) => {
                metrics.record_published(status::STATUS_CHANGED_TYPE);
                info!(
                    %id,
                    connection = %envelope["subject"],
                    state = %envelope["data"]["to_state"],
                    "published {}",
                    status::STATUS_CHANGED_TYPE
                );
            }
            Err(error) => warn!(%id, %error, "publish ack failed"),
        },
        Err(error) => warn!(%id, %error, "publish failed"),
    }
}

/// The Gateway's registry as `(id, kind)`, off the consent snapshot the
/// Sensor reads too (`GET /api/consent/snapshot`, `connections[]`, as
/// `companion-gateway/openapi.yaml` describes it), with the same service
/// token.
async fn registry_ids(gateway_url: &str, service_token: &str) -> Result<Vec<(String, String)>> {
    let url = format!("{}/api/consent/snapshot", gateway_url.trim_end_matches('/'));
    let document: serde_json::Value = reqwest::Client::new()
        .get(&url)
        .bearer_auth(service_token)
        .send()
        .await
        .with_context(|| format!("the Companion Gateway did not answer at {url}"))?
        .error_for_status()
        .with_context(|| format!("the Companion Gateway refused the registry read at {url}"))?
        .json()
        .await
        .context("the Companion Gateway's snapshot is not JSON")?;
    Ok(document["connections"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    Some((
                        entry["id"].as_str()?.to_owned(),
                        entry["kind"].as_str().unwrap_or_default().to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default())
}

async fn serve_metrics(listener: tokio::net::TcpListener, metrics: Arc<Metrics>) {
    loop {
        match listener.accept().await {
            Ok((mut socket, _peer)) => {
                let metrics = metrics.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut request = Vec::with_capacity(1024);
                    let mut chunk = [0u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n")
                        && request.len() < 8192
                    {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(read) => request.extend_from_slice(&chunk[..read]),
                        }
                    }
                    let body = metrics.render(unix_seconds(SystemTime::now()));
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/plain; version=0.0.4; charset=utf-8\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
            Err(error) => warn!(%error, "metrics accept failed"),
        }
    }
}

fn unix_seconds(at: SystemTime) -> u64 {
    at.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
