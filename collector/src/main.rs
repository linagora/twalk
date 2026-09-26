//! The collector's binary (issue #274): `twalk-collector authorize [--renew]`
//! gives it an OIDC grant with the operator in the loop; `twalk-collector`
//! alone runs — renews the grant, checks whose it is, and says what state
//! each connection is in on the bus.
//!
//! What this binary does **not** do yet is collect anything: #276 (mail) and
//! #280 (calendar) plug into the loop below with the access token it keeps
//! fresh. What it does do is refuse to start on a connection the Companion Gateway's
//! registry does not know, and refuse to publish anything when the grant
//! turns out to be for another account — both said in words, once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use tracing::{debug, error, info, warn};
use twalk_collector::config::Config;
use twalk_collector::metrics::Metrics;
use twalk_collector::oidc::{Client, Grant, Identities, Renewal, ServiceRefusal};
use twalk_collector::side::SideError;
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
    // #342: `authorize` obtains a grant, and a `basic` collector has none
    // to obtain. Refused here rather than half-run: the operator's next
    // step is a password file, not a browser.
    let Some(oidc) = config.oidc.clone() else {
        anyhow::bail!(
            "this collector's credential is COLLECTOR_CREDENTIAL=basic: there is no grant to \
             obtain and no SSO to sign in to. Its password lives in \
             COLLECTOR_BASIC_PASSWORD_FILE, and the service is read as COLLECTOR_BASIC_USER"
        );
    };
    let client = Client::discover(oidc.clone()).await?;
    if let Some(existing) = Grant::read(&oidc.grant_file)? {
        if !renew {
            eprintln!(
                "A grant for {} at {} is already in {} (obtained {}). Nothing to do; pass --renew to replace it.",
                existing.client_id,
                existing.issuer,
                oidc.grant_file.display(),
                existing.obtained_at
            );
            return Ok(());
        }
        eprintln!(
            "Replacing the grant in {} (--renew).",
            oidc.grant_file.display()
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
        oidc.redirect_uri
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
        oidc.grant_file.display()
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
    let identities = config
        .services
        .whoami(&twalk_collector::side::Credential::Bearer(
            access.token.clone(),
        ))
        .await?;
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
        std::fs::remove_file(&oidc.grant_file).with_context(|| {
            format!(
                "failed to remove the refused grant {}",
                oidc.grant_file.display()
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
            oidc.grant_file.display()
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
    info!(
        credential = %config
            .oidc
            .as_ref()
            .map(|oidc| format!("a grant at {}", oidc.issuer))
            .unwrap_or_else(|| config
                .basic
                .as_ref()
                .map(|basic| format!("{basic:?}"))
                .unwrap_or_default()),
        connections = ?config.connections.iter().map(|c| &c.id).collect::<Vec<_>>(),
        "collector starting"
    );
    let metrics = Arc::new(Metrics::new());
    if let Some(listen) = config.metrics_listen {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .with_context(|| format!("failed to bind the metrics endpoint on {listen}"))?;
        info!(%listen, "serving metrics");
        tokio::spawn(serve_metrics(listener, metrics.clone()));
    }

    // The registry: a connection this process holds must be one the Companion Gateway
    // names, or every event it published would be about a perimeter no
    // decision governs (ADR 0033). Refused at start, in words.
    // The same document carries the consent state the participants of a
    // meeting are labelled by (#280), so it is read once.
    // Whether a calendar event may carry where a meeting is (#354) is the
    // owner's decision and is read from the same place, with the same
    // token, at the same moment.
    let location_enabled = Arc::new(AtomicBool::new(false));
    // The owner's working day (#381), read from the same document as the
    // switch above and refreshed on the same rounds. `None` until they say.
    let working_day: twalk_collector::calendars::SharedWorkingDay = Default::default();
    let snapshot = match (&config.gateway_url, &config.gateway_service_token) {
        (Some(url), Some(token)) => {
            let document = consent_snapshot(url, token).await?;
            config.refuse_unknown_connections(&registry_ids(&document))?;
            let (enabled, day) = collection_settings(url, token).await?;
            location_enabled.store(enabled, Ordering::Relaxed);
            match &day {
                Some(day) => info!(
                    days = ?day.days,
                    starts_at = %day.starts_at,
                    ends_at = %day.ends_at,
                    "the owner's working day is set: a free/busy read offers only the gaps inside it"
                ),
                None => info!(
                    "no working day is set: a free/busy read offers every gap, night included"
                ),
            }
            *working_day.lock().expect("the working day mutex is never poisoned") = day;
            if enabled {
                warn!("the calendar location is ON: published events carry where a meeting is, by a decision recorded on the Companion Gateway. Turn it off there to stop it");
            } else {
                info!(
                    "the calendar location is off: no published event carries where a meeting is"
                );
            }
            Some(document)
        }
        _ => {
            warn!("COLLECTOR_GATEWAY_URL is not set: the connections are not checked against the registry, no consent decision is known, and no event carries where a meeting is — a decision this process cannot read is not a decision it may act on (#354)");
            None
        }
    };

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

    let consent =
        twalk_collector::consent::follow(jetstream.clone(), snapshot.as_ref(), &config.owner_email)
            .await?;
    // The calendar connection, when this process holds one: polled on every
    // round the grant and the side service allow (#280).
    let calendars = config
        .connections
        .iter()
        .find(|held| held.kind == "calendar")
        .map(|held| {
            Ok::<_, anyhow::Error>(twalk_collector::calendars::Calendars {
                connection: held.id.clone(),
                owner_email: config.owner_email.clone(),
                mail_connection: config
                    .connections
                    .iter()
                    .find(|held| held.kind == "email")
                    .map(|held| held.id.clone()),
                side: twalk_collector::calendars::Side::new(
                    config
                        .services
                        .caldav_url
                        .as_deref()
                        // Config pairs a URL with its connection (#321), so
                        // this cannot fire; it says which pairing broke if
                        // one day it does.
                        .context("a calendar connection is held with no COLLECTOR_CALDAV_URL")?,
                )?,
                state_dir: config.state_dir.clone(),
                consent: consent.clone(),
                locations_may_travel: location_enabled.clone(),
                working_day: working_day.clone(),
            })
        })
        .transpose()?;
    // The mail connection, when this process holds one: the INBOX polled
    // on its own interval (#276), each sender labelled by the decision
    // about them on this connection.
    let mailbox = config
        .connections
        .iter()
        .find(|held| held.kind == "email")
        .map(|held| {
            twalk_collector::mails::Mailbox::new(
                &held.id,
                &config.owner_email,
                config
                    .services
                    .jmap_session_url
                    .as_deref()
                    .context("a mail connection is held with no COLLECTOR_JMAP_SESSION_URL")?,
                &config.state_dir,
                consent.clone(),
            )
        })
        .transpose()?
        .map(Arc::new);
    // The access token, shared with the reply consumer (#278): the run loop
    // keeps it fresh, the consumer sends with whatever is current, and
    // waits when there is none.
    // What every task proves itself with (#342). An OIDC connection
    // replaces it at each renewal below; a `Basic` one sets it once,
    // because there is nothing to renew and no grant to reconnect.
    let shared_access: twalk_collector::replies::SharedCredential = Arc::default();
    // The free/busy endpoint (#281): the calendar connection's state and the
    // owner's id on the side service, as the loop last observed them, for a
    // read to check before it asks.
    let calendar_access: twalk_collector::http::SharedCalendarAccess = Arc::default();
    let calendars = calendars.map(Arc::new);
    if let (Some(listen), Some(token)) = (config.http_listen, &config.gateway_service_token) {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .with_context(|| format!("failed to bind the internal HTTP endpoint on {listen}"))?;
        info!(%listen, "serving the free/busy endpoint to the Companion Gateway");
        tokio::spawn(twalk_collector::http::serve(
            listener,
            twalk_collector::http::Endpoint {
                service_token: token.clone(),
                calendars: calendars.clone(),
                calendar_access: calendar_access.clone(),
                access: shared_access.clone(),
                metrics: metrics.clone(),
            },
        ));
    }
    // Rung by the push listener when the server says the Email state
    // changed: the run loop wakes and the mail poll runs at once.
    let mail_wake: Arc<tokio::sync::Notify> = Arc::default();
    let mut woken_for_mail = false;
    if let Some(mailbox) = &mailbox {
        tokio::spawn(twalk_collector::replies::consume_approvals(
            jetstream.clone(),
            mailbox.clone(),
            config
                .connections
                .iter()
                .map(|held| held.id.clone())
                .collect(),
            config.owner_email.clone(),
            shared_access.clone(),
            metrics.clone(),
            twalk_collector::replies::RetryPolicy {
                base: config.send_retry_base,
                max_attempts: config.send_retry_max_attempts,
            },
        ));
        // Push (#277): the server's doorbell, ringing the mail poll ahead of
        // its interval. The poll stays as the fallback.
        tokio::spawn(twalk_collector::push::listen(
            config
                .services
                .jmap_session_url
                .clone()
                .expect("a mailbox is held, so its session URL is configured (#321)"),
            shared_access.clone(),
            mail_wake.clone(),
            metrics.clone(),
        ));
    }

    // The SSO's discovery document is read the first time a grant has to be
    // renewed, not at start: a deployment brought up before its SSO answers
    // — or with no grant yet, where there is nothing to renew — must say so
    // on the bus and keep running, not exit for compose to restart in a
    // loop. `discover` retries on every round until the SSO answers.
    let mut client: Option<Client> = None;
    let mut trackers: Vec<Tracker> = config
        .connections
        .iter()
        .map(|held| Tracker::new(&held.id, held.kind, &config.host))
        .collect();
    // The grant, when this collector's credential is one (#342).
    let mut grant = match &config.oidc {
        Some(oidc) => Grant::read(&oidc.grant_file)?,
        None => None,
    };
    // The calendars are polled on their own interval (#251: every 60 s),
    // not on every health round.
    let mut last_calendar_poll: Option<std::time::Instant> = None;
    // Whether the calendar-location switch is currently unreadable, so the
    // warning is said once per outage rather than once per round (#354).
    let mut location_unreadable = false;
    let mut last_mail_poll: Option<std::time::Instant> = None;
    let mut access: Option<twalk_collector::oidc::AccessToken> = None;
    // Why the SSO last refused to renew, when it did: the grant's fault
    // (`reconnect_required`) or the client's (`pending_operator`) are two
    // different sentences to the operator.
    let mut sso_refusal: Option<Observation> = None;

    loop {
        let now = SystemTime::now();
        let mut renewed_this_round = false;
        // The SSO did not answer a renewal this round: the grant may well
        // stand, and the state is `unreachable` — not the `reconnect_required`
        // a refusal would be, which would send the operator to sign in
        // again for an SSO that was merely down.
        let mut sso_unreachable = false;
        let mut caldav_owner_id: Option<String> = None;
        let observation = match (&config.basic, &grant) {
            // A credential of the operator's own (#342): no SSO to renew
            // against, so what the services answer **is** the state. A
            // `basic` connection is `connected`, `pending_operator` or
            // `unreachable`; it can never be `reconnect_required`, because
            // there is no grant to reconnect.
            (Some(basic), _) => {
                let credential = twalk_collector::side::Credential::Basic {
                    user: basic.user.clone(),
                    password: basic.password.clone(),
                };
                let identities = config.services.whoami(&credential).await;
                match identities {
                    Ok(identities) => {
                        caldav_owner_id = identities.caldav_owner_id.clone();
                        let mismatched = identities.owner_mismatch(&config.owner_email);
                        if mismatched.is_empty() {
                            observe_services(&config, &identities)
                        } else {
                            for (service, account) in &mismatched {
                                warn!(%service, %account, "the credential is another account's; nothing is published");
                            }
                            Observation {
                                state: State::PendingOperator,
                                service: Some(mismatched[0].0),
                                hint: Some(format!(
                                    "The credential belongs to another account, not {}. Check \
                                     COLLECTOR_BASIC_USER and its password file.",
                                    config.owner_email
                                )),
                            }
                        }
                    }
                    Err(error) => Observation {
                        state: State::Unreachable,
                        service: None,
                        hint: Some(format!("the services could not be asked: {error:#}")),
                    },
                }
            }
            (None, None) => Observation {
                state: State::ReconnectRequired,
                service: Some("sso"),
                hint: Some(format!(
                    "No grant in {}. Run `twalk-collector authorize` — on a compose deployment, \
                     `deploy/docker-compose/provision-connection.sh` — and sign in as {}.",
                    config
                        .oidc
                        .as_ref()
                        .map(|oidc| oidc.grant_file.display().to_string())
                        .unwrap_or_default(),
                    config.owner_email
                )),
            },
            (None, Some(current)) => {
                if access
                    .as_ref()
                    .is_none_or(|token| !token.is_fresh(RENEWAL_MARGIN))
                {
                    match renew(&mut client, &config, current).await? {
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
                            sso_unreachable = true;
                            warn!(%detail, "the SSO could not be reached");
                        }
                    }
                }
                match &access {
                    None if sso_unreachable => Observation {
                        state: State::Unreachable,
                        service: Some("sso"),
                        hint: Some(
                            "The SSO did not answer; the collector retries on its own.".to_owned(),
                        ),
                    },
                    None => sso_refusal
                        .clone()
                        .unwrap_or_else(|| reconnect_required(&config)),
                    Some(token) => {
                        let mut identities = config
                            .services
                            .whoami(&twalk_collector::side::Credential::Bearer(
                                token.token.clone(),
                            ))
                            .await;
                        // A 401 on a token this process believes fresh is
                        // not yet the operator's problem: the SSO may have
                        // revoked the grant under it. Renew first; the SSO's
                        // refusal is `reconnect_required`, and only a
                        // service refusing a token the SSO just issued is
                        // `pending_operator` (issue #274: two refusals).
                        let stale = matches!(&identities, Ok(ids) if ids.unauthenticated() && !renewed_this_round);
                        if stale {
                            match renew(
                                &mut client,
                                &config,
                                grant.as_ref().expect("a token comes from a grant"),
                            )
                            .await?
                            {
                                Renewal::Renewed {
                                    grant: rotated,
                                    access: fresh,
                                } => {
                                    metrics.record_renewal("renewed", unix_seconds(now));
                                    identities = config
                                        .services
                                        .whoami(&twalk_collector::side::Credential::Bearer(
                                            fresh.token.clone(),
                                        ))
                                        .await;
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
                            (Some(_), Ok(identities)) => {
                                caldav_owner_id = identities.caldav_owner_id.clone();
                                observe_services(&config, &identities)
                            }
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
        *shared_access.write().await = match &config.basic {
            Some(basic) => Some(twalk_collector::side::Credential::Basic {
                user: basic.user.clone(),
                password: basic.password.clone(),
            }),
            None => access
                .as_ref()
                .map(|access| twalk_collector::side::Credential::Bearer(access.token.clone())),
        };
        // What this round's reads prove themselves with: the same value
        // every task holds, so a round and a reply never disagree (#342).
        let credential = shared_access.read().await.clone();
        // One observation of the grant, published per connection it holds:
        // a calendar connection whose service refused gets its own words.
        let occurred_at = twalk_collector::oidc::now_rfc3339();
        let mut calendar_is_connected = false;
        let mut mail_is_connected = false;
        for tracker in &mut trackers {
            let per_connection = per_connection(&observation, tracker.kind());
            if per_connection.state == State::Connected {
                match tracker.kind() {
                    "calendar" => calendar_is_connected = true,
                    "email" => mail_is_connected = true,
                    _ => {}
                }
            }
            if tracker.kind() == "calendar" {
                *calendar_access.write().await = twalk_collector::http::CalendarAccess {
                    owner_id: caldav_owner_id.clone(),
                    state: Some(per_connection.state.as_str()),
                };
            }
            metrics.set_connection_state(tracker.connection(), per_connection.state);
            if let Some(envelope) = tracker.observe(&per_connection, &occurred_at) {
                // A transition is said in the log in the operator's words as
                // well as on the bus: `docker compose logs collector` is
                // where they look first, and the hint is the next step.
                match per_connection.state {
                    State::Connected => info!(
                        connection = tracker.connection(),
                        "the connection is connected"
                    ),
                    state => warn!(
                        connection = tracker.connection(),
                        state = state.as_str(),
                        service = per_connection.service,
                        hint = per_connection.hint.as_deref().unwrap_or_default(),
                        "the connection is not connected"
                    ),
                }
                publish(&jetstream, &envelope, &metrics).await;
            }
        }
        // The calendars, on a round where the grant stands and the side
        // service answered as the owner, once the poll interval is up: what
        // changed since the cursor is published, then the cursor moves. A
        // read the side service refuses is the calendar connection's state
        // — the same words the whoami would have found — and a poll that
        // fails otherwise is said and retried next time.
        let poll_is_due =
            last_calendar_poll.is_none_or(|last| last.elapsed() >= config.calendar_poll_interval);
        if let (Some(calendars), Some(credential), Some(owner_id), true, true) = (
            &calendars,
            &credential,
            &caldav_owner_id,
            calendar_is_connected,
            poll_is_due,
        ) {
            last_calendar_poll = Some(std::time::Instant::now());
            // What the owner has decided since the last round (#354). Read
            // before the poll, so the events this round publishes follow the
            // switch as it stands rather than as it stood at start.
            let mut may_publish = true;
            if let (Some(url), Some(token)) = (&config.gateway_url, &config.gateway_service_token) {
                match collection_settings(url, token).await {
                    Ok((enabled, day)) => {
                        location_unreadable = false;
                        // The working day as it stands, beside the switch: an
                        // owner who narrows their hours at noon has narrowed
                        // them for the next read, not for the next restart.
                        *working_day
                            .lock()
                            .expect("the working day mutex is never poisoned") = day;
                        if location_enabled.swap(enabled, Ordering::Relaxed) != enabled {
                            if enabled {
                                warn!("the calendar location was turned ON: published events now carry where a meeting is");
                            } else {
                                info!("the calendar location was turned off: no published event carries where a meeting is");
                            }
                        }
                    }
                    // An unreadable decision is not a decision, so the switch
                    // shuts. But a round that published with it open and now
                    // publishes with it shut would say `location` moved on a
                    // meeting nobody moved — so such a round publishes
                    // nothing at all and the cursor stays where it is.
                    // Nothing is lost: the next round reads both.
                    Err(error) => {
                        let was_open = location_enabled.swap(false, Ordering::Relaxed);
                        may_publish = !was_open;
                        if !location_unreadable {
                            location_unreadable = true;
                            warn!(
                                error = %format!("{error:#}"),
                                skipped_this_round = was_open,
                                "the calendar location switch could not be read: no event carries where a meeting is until it can be. Said once, until it answers again"
                            );
                        }
                    }
                }
            }
            if !may_publish {
                continue;
            }
            // The window this round asks about (#348): bounded, so a real
            // calendar answers and the cursor stays a cursor.
            let window = twalk_collector::freebusy::Window::around(
                std::time::SystemTime::now(),
                config.calendar_window_back_days,
                config.calendar_window_ahead_days,
            );
            match calendars
                .poll(owner_id, credential, &window, &occurred_at)
                .await
            {
                Ok(found) => {
                    debug!(
                        envelopes = found.envelopes.len(),
                        cursors = found.cursors.len(),
                        "calendars polled"
                    );
                    for envelope in &found.envelopes {
                        publish(&jetstream, envelope, &metrics).await;
                    }
                    if let Err(error) = calendars.commit(&found.cursors) {
                        error!(error = %format!("{error:#}"), "the calendar cursor could not be written; the next poll republishes");
                    }
                }
                Err(SideError::Refused { status, challenge }) => {
                    // What the operator is told follows what the service
                    // asked for, here as at `authorize` (#320): this is the
                    // path that reported a Cozy instance as a missing
                    // audience on the reference deployment.
                    let refused = Observation {
                        state: State::PendingOperator,
                        service: Some("caldav"),
                        hint: Some(twalk_collector::oidc::refusal_detail(
                            "caldav",
                            config.services.caldav_url.as_deref().unwrap_or_default(),
                            status,
                            challenge.as_deref(),
                        )),
                    };
                    for tracker in trackers.iter_mut().filter(|t| t.kind() == "calendar") {
                        metrics.set_connection_state(tracker.connection(), refused.state);
                        if let Some(envelope) = tracker.observe(&refused, &occurred_at) {
                            publish(&jetstream, &envelope, &metrics).await;
                        }
                    }
                }
                Err(error) => warn!(%error, "the calendars could not be polled this round"),
            }
        } else {
            debug!(
                calendars = calendars.is_some(),
                token = access.is_some(),
                owner_id = ?caldav_owner_id,
                calendar_is_connected,
                poll_is_due,
                "calendars not polled this round"
            );
        }
        // The mailbox, on the same terms: the grant stands, JMAP answered
        // as the owner, the poll interval is up.
        let mail_poll_is_due = woken_for_mail
            || last_mail_poll.is_none_or(|last| last.elapsed() >= config.mail_poll_interval);
        woken_for_mail = false;
        if let (Some(mailbox), Some(credential), true, true) =
            (&mailbox, &credential, mail_is_connected, mail_poll_is_due)
        {
            last_mail_poll = Some(std::time::Instant::now());
            match mailbox.poll(credential, &occurred_at).await {
                Ok(found) => {
                    debug!(
                        envelopes = found.envelopes.len(),
                        dropped = found.dropped.len(),
                        "mailbox polled"
                    );
                    for why in &found.dropped {
                        metrics.record_mail_dropped(why.as_str());
                        info!(reason = why.as_str(), "a mail was not published");
                    }
                    for envelope in &found.envelopes {
                        publish(&jetstream, envelope, &metrics).await;
                    }
                    if let Some(state) = &found.state {
                        if let Err(error) = mailbox.commit(state) {
                            error!(error = %format!("{error:#}"), "the mail state could not be written; the next poll republishes");
                        }
                    }
                }
                Err(SideError::Refused { status, challenge }) => {
                    let refused = Observation {
                        state: State::PendingOperator,
                        service: Some("jmap"),
                        hint: Some(twalk_collector::oidc::refusal_detail(
                            "jmap",
                            config
                                .services
                                .jmap_session_url
                                .as_deref()
                                .unwrap_or_default(),
                            status,
                            challenge.as_deref(),
                        )),
                    };
                    for tracker in trackers.iter_mut().filter(|t| t.kind() == "email") {
                        metrics.set_connection_state(tracker.connection(), refused.state);
                        if let Some(envelope) = tracker.observe(&refused, &occurred_at) {
                            publish(&jetstream, &envelope, &metrics).await;
                        }
                    }
                }
                Err(error) => warn!(%error, "the mailbox could not be polled this round"),
            }
        }
        // The round ends at the health interval, or the moment the server
        // says a mail arrived — whichever comes first.
        tokio::select! {
            _ = tokio::time::sleep(config.health_interval) => {}
            _ = mail_wake.notified() => {
                woken_for_mail = true;
            }
        }
    }
}

/// One renewal, with the SSO discovered first when it has not been yet: a
/// discovery the SSO does not answer — or answers with something that is
/// not a discovery document — is the same `Unreachable` a renewal it does
/// not answer is, said in the log with the cause, and the next round asks
/// again.
async fn renew(client: &mut Option<Client>, config: &Config, grant: &Grant) -> Result<Renewal> {
    let client = match client {
        Some(client) => client,
        None => match Client::discover(
            config
                .oidc
                .clone()
                .context("a renewal without an OIDC credential (#342)")?,
        )
        .await
        {
            Ok(discovered) => client.insert(discovered),
            Err(error) => {
                return Ok(Renewal::Unreachable {
                    detail: format!("{error:#}"),
                })
            }
        },
    };
    client.renew(grant).await
}

/// The SSO refused the grant: only the operator can give a new one.
fn reconnect_required(config: &Config) -> Observation {
    Observation {
        state: State::ReconnectRequired,
        service: Some("sso"),
        hint: Some(format!(
            "The SSO refused to renew the grant. Run `twalk-collector authorize --renew` — on a \
             compose deployment, `deploy/docker-compose/provision-connection.sh --renew` — and \
             sign in again as {}; nothing is published until then.",
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

/// Publishes one envelope on the subject its `type` names (`twalk.<type
/// minus the prefix>`), with `Nats-Msg-Id` for the bus to deduplicate on
/// and the `connection` header every message-flow type carries.
async fn publish(
    jetstream: &async_nats::jetstream::Context,
    envelope: &serde_json::Value,
    metrics: &Metrics,
) {
    let id = envelope["id"].as_str().unwrap_or_default().to_owned();
    let event_type = envelope["type"].as_str().unwrap_or_default().to_owned();
    let mut headers = async_nats::header::HeaderMap::new();
    headers.insert(async_nats::header::NATS_MESSAGE_ID, id.as_str());
    if let Some(connection) = envelope.get("connection").and_then(|v| v.as_str()) {
        headers.insert("connection", connection);
    }
    let payload = serde_json::to_vec(envelope).expect("the envelope is serializable");
    match jetstream
        .publish_with_headers(status::bus_subject(&event_type), headers, payload.into())
        .await
    {
        Ok(ack) => match ack.await {
            Ok(_) => {
                metrics.record_published(&event_type);
                info!(
                    %id,
                    connection = %envelope["connection"],
                    subject = %envelope["subject"],
                    "published {event_type}"
                );
            }
            Err(error) => warn!(%id, %error, "publish ack failed"),
        },
        Err(error) => warn!(%id, %error, "publish failed"),
    }
}

/// Whether a calendar event may carry where the meeting is, as the owner
/// decided it on the Companion Gateway (#354): `GET /api/settings/collection`,
/// with the service token the registry read already uses.
///
/// Read at start and again before every calendar poll, so a decision taken
/// on the settings screen reaches the next round rather than the next
/// restart.
///
/// **An unreadable decision is not a decision**, either time: the switch
/// shuts and no event carries a location until this answers again. Not
/// fatal, though — unlike the registry read, whose failure makes every
/// event this process would publish wrong. Here withholding is a safe
/// answer, and taking the mail half down over a calendar field would be a
/// blast radius nobody asked for.
///
/// One thing follows that is easy to miss: a round that had been publishing
/// locations and can no longer read the switch publishes **nothing**, and
/// leaves its cursor where it is. Publishing with the switch shut would put
/// `location` in `changed_fields` for a meeting nobody moved, and the
/// owner's journal would say something untrue. The next round reads both.
async fn collection_settings(
    gateway_url: &str,
    service_token: &str,
) -> Result<(bool, Option<twalk_collector::freebusy::WorkingDay>)> {
    let url = format!(
        "{}/api/settings/collection",
        gateway_url.trim_end_matches('/')
    );
    let document: serde_json::Value = reqwest::Client::new()
        .get(&url)
        .bearer_auth(service_token)
        .send()
        .await
        .with_context(|| format!("the Companion Gateway did not answer at {url}"))?
        .error_for_status()
        .with_context(|| format!("the Companion Gateway refused the collection read at {url}"))?
        .json()
        .await
        .context("the Companion Gateway's collection settings are not JSON")?;
    let enabled = document
        .get("calendar_location")
        .and_then(|switch| switch.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .context("the collection settings do not say whether the calendar location is enabled")?;
    // The working day, or nothing (#381). Absent and null are the same
    // absence — a Gateway older than the decision, and one where nobody made
    // it — and both mean every gap is offered. A half-read value is treated
    // as none for the same reason: an amplitude nobody set must not clip
    // somebody's gaps.
    let day = document.get("working_day").and_then(|day| {
        Some(twalk_collector::freebusy::WorkingDay {
            days: day
                .get("days")?
                .as_array()?
                .iter()
                .filter_map(|day| day.as_u64().map(|day| day as u8))
                .collect(),
            starts_at: day.get("starts_at")?.as_str()?.to_owned(),
            ends_at: day.get("ends_at")?.as_str()?.to_owned(),
        })
        .filter(|day| !day.days.is_empty())
    });
    Ok((enabled, day))
}

/// The Companion Gateway's consent snapshot, the document the Sensor reads too
/// (`GET /api/consent/snapshot`, as `companion-gateway/openapi.yaml`
/// describes it), with the same service token: the registry is its
/// `connections[]`, the decisions its `entries[]`.
async fn consent_snapshot(gateway_url: &str, service_token: &str) -> Result<serde_json::Value> {
    let url = format!("{}/api/consent/snapshot", gateway_url.trim_end_matches('/'));
    reqwest::Client::new()
        .get(&url)
        .bearer_auth(service_token)
        .send()
        .await
        .with_context(|| format!("the Companion Gateway did not answer at {url}"))?
        .error_for_status()
        .with_context(|| format!("the Companion Gateway refused the registry read at {url}"))?
        .json()
        .await
        .context("the Companion Gateway's snapshot is not JSON")
}

/// The Companion Gateway's registry as `(id, kind)`, off the snapshot's `connections[]`.
fn registry_ids(document: &serde_json::Value) -> Vec<(String, String)> {
    document["connections"]
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
        .unwrap_or_default()
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
