//! The Twalk Companion Gateway binary. The decision logic lives in the
//! library modules; this file binds the origin, wires the router and handles
//! the process lifecycle.

use std::future::IntoFuture;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{info, warn};
use twalk_companion_gateway::approval::Approvals;
use twalk_companion_gateway::bootstrap::Bootstrap;
use twalk_companion_gateway::bridge_status::{reconcile, Statuses};
use twalk_companion_gateway::config::{Config, Consent};
use twalk_companion_gateway::consent_snapshot::Snapshots;
use twalk_companion_gateway::contacts::{project_until_shutdown, Contacts};
use twalk_companion_gateway::hermes_answer::Answers;
use twalk_companion_gateway::http::{router, Gateway};
use twalk_companion_gateway::matrix_openid::Verifier;
use twalk_companion_gateway::metrics::Metrics;
use twalk_companion_gateway::outbox::{publish_until_shutdown, Outbox};
use twalk_companion_gateway::owner::Owner;
use twalk_companion_gateway::portals::{refresh_until_shutdown, PortalBridge, Portals};
use twalk_companion_gateway::runtime_presence::RuntimePresence;
use twalk_companion_gateway::session::Sessions;
use twalk_companion_gateway::settings::Settings;
use twalk_companion_gateway::static_files::Resolver;
use twalk_companion_gateway::store::Store;
use twalk_companion_gateway::suggestions::Suggestions;

/// How long in-flight requests get to finish after SIGTERM before the
/// process exits anyway. Static files and a JSON document: a request that
/// has not finished in this long is not going to.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    tracing_subscriber::fmt()
        .with_env_filter(&config.log_level)
        .init();
    info!(
        listen = %config.listen,
        static_dir = %config.static_dir.display(),
        fallback_file = %config.fallback_file,
        "companion gateway starting"
    );
    let companion = Resolver::new(config.static_dir.clone(), config.fallback_file.clone());
    // A directory that is absent or holds no build is not fatal: the origin
    // answers a plain 404 for the Companion until the build appears, while
    // health and metrics stay up. Warn loudly, because for an operator who
    // did not intend it this is the whole of what is wrong.
    if !companion.fallback_path().is_file() {
        warn!(
            static_dir = %config.static_dir.display(),
            fallback_file = %config.fallback_file,
            "no fallback file in the static directory: the Companion origin will answer 404 for any path its build has no file for"
        );
    }

    // The user's session (ticket #52). Absent configuration it stays `None`:
    // the origin serves the Companion, health and metrics, and the whole API
    // answers 503 — see `config::SignIn` for why that is preferred to
    // refusing to start.
    let sessions = match &config.sign_in {
        Some(sign_in) => {
            info!(
                owner = %sign_in.owner,
                homeserver = %sign_in.homeserver_name,
                federation_base_url = %sign_in.federation_base_url,
                state_dir = %sign_in.state_dir.display(),
                device_token_ttl_seconds = sign_in.device_token_ttl_seconds,
                "sign-in configured: this deployment serves one owner"
            );
            let verifier = Verifier::new(&sign_in.federation_base_url, &sign_in.homeserver_name)
                .context("failed to build the OpenID verification client")?;
            Some(Arc::new(
                Sessions::open(
                    &sign_in.state_dir,
                    verifier,
                    sign_in.owner.clone(),
                    sign_in.device_token_ttl_seconds,
                    sign_in.refresh_token_ttl_seconds,
                    now_unix_seconds,
                )
                .context("failed to open the session store")?,
            ))
        }
        None => {
            warn!(
                "GATEWAY_OWNER is not set: nobody can sign in, so every /api endpoint answers 503. \
                 Set GATEWAY_OWNER, GATEWAY_HOMESERVER_FEDERATION_URL and GATEWAY_STATE_DIR to enable sign-in"
            );
            None
        }
    };

    // Bootstrap (ticket #53): the registration relay and the Sensor's
    // invitation. Each half is independently optional, and a half that is off
    // answers 503 naming the variable that would open it — so an operator
    // never has to guess which of the two is missing.
    let bootstrap = match &config.bootstrap.homeserver_url {
        Some(homeserver_url) => {
            let bootstrap = Bootstrap::new(
                homeserver_url,
                config.bootstrap.registration_shared_secret.clone(),
                config.bootstrap.sensor_user_id.clone(),
            )
            .context("failed to build the bootstrap client")?;
            if bootstrap.registers_accounts() {
                info!(
                    homeserver_url = %bootstrap.homeserver_url(),
                    owner = config.sign_in.as_ref().map(|sign_in| sign_in.owner.as_str()).unwrap_or("(none)"),
                    "the registration relay is enabled: it can create this deployment's one account, once, and refuses every other username"
                );
            } else {
                info!(
                    "GATEWAY_REGISTRATION_SHARED_SECRET is not set: the registration relay is off, and POST /api/bootstrap/account answers 503"
                );
            }
            match bootstrap.sensor_user_id() {
                Some(sensor) => info!(
                    sensor = %sensor,
                    "the Sensor can be invited into the rooms the user selects"
                ),
                None => info!(
                    "GATEWAY_SENSOR_USER_ID is not set: the Gateway cannot invite the Sensor, and POST /api/bootstrap/rooms answers 503"
                ),
            }
            Some(Arc::new(bootstrap))
        }
        // No sign-in configuration and no homeserver URL: the whole API is
        // closed already, so there is nothing to warn about twice.
        None => None,
    };

    let metrics = Arc::new(Metrics::started_at(now_unix_seconds()));

    // Consent (ticket #49). Configured, the Gateway opens its decision
    // journal, publishes whatever the last run left unpublished, and serves
    // the write API; unconfigured, the consent endpoints answer a 503 naming
    // what is missing and the rest of the origin is untouched.
    // Consent's two halves are wired together because they share one store
    // and one bus: the outbox that publishes decisions (#49) and the
    // projection that consumes the inbound stream to know who is waiting for
    // one (#54).
    // The registry of connections (ADR 0033, #269): configuration, held once
    // and handed to everything that needs it — the API, the consent snapshot
    // the Sensor stamps events from, the approvals that resolve an old event
    // against it, and the store that keeps it for the migrations to come.
    let connections = Arc::new(config.connections.clone());
    let (consent, contacts, approvals, suggestions, answers, store) = match &config.consent {
        Some(consent) => {
            let (store, outbox, owner) = open_consent(consent, &metrics)?;
            tokio::spawn(publish_until_shutdown(
                outbox.clone(),
                consent.nats_url.clone(),
            ));
            // Approvals (ticket #24). The same store and the same bus: the
            // question an approval asks — "is this contact granted, now?" —
            // is a read of the consent journal's own projection, which is
            // the whole reason this endpoint is here and not on the runtime
            // (ADR 0022).
            let approvals = Arc::new(Approvals::new(
                store.clone(),
                metrics.clone(),
                connections.clone(),
                consent.owner.clone(),
                consent.nats_url.clone(),
                config.approval_lookup_window,
                std::time::SystemTime::now,
            ));
            info!(
                owner = %consent.owner,
                lookup_window = config.approval_lookup_window,
                "approvals are on: POST /api/approvals turns one suggestion into an outbound \
                 reply, refused if the sender's consent is no longer granted at that moment"
            );
            let projection = Arc::new(Contacts::new(
                store.clone(),
                metrics.clone(),
                owner,
                consent.nats_url.clone(),
                config.inbound_consumer.clone(),
            ));
            info!(
                consumer = %projection.consumer_name(),
                owner = %consent.owner,
                "the pending-contact projection is on: this Gateway keeps a contact's Matrix ID, \
                 its network and its first and last sighting — no body, no display name, no \
                 network identifier"
            );
            tokio::spawn(project_until_shutdown(projection.clone()));
            // Reading suggestions (ticket #97): the read side of the same
            // act, on the same bus and the same window. Its own half, and
            // its own bus connection, because a screen that cannot draw and
            // a reply that did not go out must not queue behind each other.
            let suggestions = Arc::new(Suggestions::new(
                store.clone(),
                consent.nats_url.clone(),
                config.approval_lookup_window,
                std::time::SystemTime::now,
            ));
            info!(
                lookup_window = config.approval_lookup_window,
                "suggestion reads are on: GET /api/suggestions projects the bus — nothing is                  stored, and the answer says how far back it looked"
            );
            // Hermes's answers (ticket #206, ADR 0032). Built on the
            // approval half rather than beside it: the trigger lookup, the
            // consent read at that moment, the refusal vocabulary and the
            // in-request publish are the same, and two implementations of
            // any of them would be two vocabularies for one fact.
            let answers = match &config.hermes_answers {
                Some(seam) => {
                    info!(
                        hermes_domain = %seam.domain,
                        suggestion_ttl_seconds = seam.suggestion_ttl_seconds,
                        "the seam to Hermes is on: POST {} turns a signed answer into a \
                         persona.suggest.produced, refused if the sender's consent is no longer \
                         granted at that moment and refused if the answer names no language",
                        twalk_companion_gateway::hermes_answer::ANSWER_PATH
                    );
                    Some(Arc::new(Answers::new(
                        approvals.clone(),
                        metrics.clone(),
                        seam.secret.clone(),
                        seam.domain.clone(),
                        seam.suggestion_ttl_seconds,
                        std::time::SystemTime::now,
                    )))
                }
                None => None,
            };
            (
                Some(outbox),
                Some(projection),
                Some(approvals),
                Some(suggestions),
                answers,
                Some(store),
            )
        }
        None => {
            if config.sign_in.is_some() {
                warn!(
                    "GATEWAY_NATS_URL is not set: the consent endpoints answer 503, because a \
                     decision the bus never hears is a decision no persona can honour — and so \
                     do the pending-contact endpoints, because the list of who is waiting is \
                     read from the bus, and so do the approval endpoints, because a suggestion \
                     is read from the bus and the reply is published on it"
                );
            }
            (None, None, None, None, None, None)
        }
    };

    // The bridge login facade (ticket #55). Built from configuration alone:
    // no bridge is contacted at startup, because a bridge that is down must
    // not keep the Companion's origin from coming up. A misconfigured bridge
    // *is* fatal, though — a base URL that is not one, or two instances
    // sharing an id — because the operator can fix it now and a user cannot
    // diagnose it from the QR screen.
    let bridges = Arc::new(
        twalk_companion_gateway::bridge::Bridges::new(config.bridges.clone())
            .context("failed to build the bridge login facade")?,
    );
    if config.bridges.is_empty() {
        info!(
            "GATEWAY_BRIDGES is not set: no network can be connected from the Companion, and \
             GET /api/bridges answers an empty list"
        );
    } else {
        for bridge in &config.bridges {
            info!(
                bridge = %bridge.bridge_id,
                network = %bridge.network,
                url = %bridge.base_url,
                "a bridge is configured: the Gateway can drive its logins and holds their \
                 blocking steps itself"
            );
        }
    }
    // The consent snapshot (ticket #50): the service token that
    // authenticates the Sensor's read, and the cap that makes an oversized
    // snapshot an error instead of a truncation. Independent of the bus
    // above, so that an operator who set one and not the other gets an
    // answer naming what is actually missing.
    // The bridge status half (ticket #56). It shares the consent store and
    // the consent bus, so it is on exactly when they are: a transition has to
    // be recorded before it is published, and there is nowhere to publish it
    // without a bus. Without them the webhook answers 503 naming what is
    // missing, rather than accepting a push the Gateway would throw away.
    let statuses = match &consent {
        Some(outbox) => {
            let statuses = Arc::new(
                Statuses::new(
                    &config.bridges,
                    outbox.clone(),
                    metrics.clone(),
                    std::time::SystemTime::now,
                )
                .context("failed to build the bridge status half")?,
            );
            for bridge in statuses.bridges() {
                info!(
                    bridge = %bridge.bridge_id,
                    instance = %bridge.instance_id,
                    network = bridge.network.as_str(),
                    webhook = %twalk_companion_gateway::bridge_status::webhook_path(&bridge.bridge_id),
                    verified = bridge.has_as_token(),
                    "a bridge reports its status here"
                );
                if !bridge.has_as_token() {
                    // Loud, because the bridge's own pushes will be refused
                    // until this is fixed — and accepting them unverified is
                    // the one thing this endpoint must not do.
                    warn!(
                        bridge = %bridge.bridge_id,
                        "no as_token is configured for this bridge, so its status pushes will \
                         be refused: set GATEWAY_BRIDGE_{}_AS_TOKEN to the same value as that \
                         bridge's appservice.as_token",
                        twalk_companion_gateway::config::variable_slug(&bridge.instance_id)
                    );
                }
            }
            Some(statuses)
        }
        None => {
            if !config.bridges.is_empty() {
                warn!(
                    "GATEWAY_NATS_URL is not set: the bridge status webhook answers 503, so a \
                     bridge session that breaks stays broken silently"
                );
            }
            None
        }
    };

    let snapshots = match &config.snapshot {
        Some(snapshot) => {
            info!(
                max_entries = snapshot.max_entries,
                "the consent snapshot is enabled: GET /api/consent/snapshot serves the whole \
                 consent state with the stream sequence it reflects, to a caller presenting \
                 the service token"
            );
            Some(Arc::new(Snapshots::new(
                &snapshot.service_token,
                snapshot.max_entries,
            )))
        }
        None => {
            if config.consent.is_some() {
                warn!(
                    "GATEWAY_SERVICE_TOKEN is not set: GET /api/consent/snapshot answers 503, so \
                     a consumer with a cold cache cannot recover consent state and will label \
                     every sender pending until it hears a decision on the bus"
                );
            }
            None
        }
    };

    // The model and language settings (ticket #98). Configured with sign-in
    // rather than with consent: naming a model needs no bus, so an operator
    // who has not set GATEWAY_NATS_URL can still say what their personas
    // will reason with. The credential file is read here, and an empty or
    // unreadable one is fatal — an operator who named a file meant to supply
    // a credential, and falling back to the browser's value would be
    // ADR 0015's precedence failing in the direction it exists to prevent.
    let settings = match &config.settings {
        Some(settings) => {
            let store = Settings::open(
                &settings.state_dir,
                settings.credential_file.as_deref(),
                std::time::SystemTime::now,
            )
            .context("failed to open the settings store")?;
            match store.credential_file_path() {
                Some(path) => info!(
                    store = %store.path().display(),
                    credential_file = %path,
                    "the model configuration is on, and the operator's credential file WINS \
                     over anything set from the Companion (ADR 0015)"
                ),
                None => info!(
                    store = %store.path().display(),
                    "the model configuration is on: GATEWAY_LLM_API_KEY_FILE is unset, so the \
                     endpoint credential is whatever the Companion set, or none"
                ),
            }
            match store.model() {
                Ok(Some(model)) => info!(
                    base_url = %model.base_url,
                    model = %model.model,
                    "a model is configured: personas reason with it once the runtime reads it"
                ),
                Ok(None) => info!(
                    "no model is configured yet: Twalk ships no default and a persona refuses \
                     to start without one (ADR 0015). Set one from the Companion, or with \
                     PUT /api/settings/model"
                ),
                Err(error) => warn!(%error, "failed to read the stored model configuration"),
            }
            Some(Arc::new(store))
        }
        None => None,
    };

    // The portal register (ticket #105): which conversations the bridges
    // have actually built, and where the Sensor stands in each. It needs the
    // homeserver, the Sensor's Matrix ID and each bridge's appservice token
    // — nothing of the store, the bus or the owner's session — so it is
    // built from configuration alone and is on whenever those three exist.
    // A bridge no connection covers is said now, because its portals'
    // traffic will not be published until it is named.
    for uncovered in connections.uncovered_bridges(&config.bridges) {
        warn!(
            bridge = %uncovered.bridge_id,
            network = %uncovered.network,
            "no connection covers this bridge: a second bridge of one network is declared, never \
             derived — name it in GATEWAY_CONNECTIONS after its GATEWAY_BRIDGES id (<bridge_id>=<network>=<label>), or the \
             Sensor publishes nothing from its portals (ADR 0033)"
        );
    }
    info!(
        connections = %connections
            .connections()
            .iter()
            .map(|c| format!("{}:{}", c.id, c.kind))
            .collect::<Vec<_>>()
            .join(","),
        "the registry of connections is what every event is stamped with and every consent \
         decision is scoped to"
    );
    if let Some(store) = &store {
        store
            .record_connections(connections.connections())
            .context("failed to record the connections in the store")?;
    }

    let portals = match Portals::new(
        config.bootstrap.homeserver_url.as_deref(),
        config.bootstrap.sensor_user_id.as_deref(),
        config
            .sign_in
            .as_ref()
            .map(|sign_in| sign_in.owner.as_str()),
        config
            .bridges
            .iter()
            .map(|bridge| PortalBridge {
                bridge_id: bridge.bridge_id.clone(),
                network: bridge.network.clone(),
                as_token: bridge.as_token.clone(),
                bot_user_id: bridge.bot_user_id.clone(),
            })
            .collect(),
        config.crowd_threshold,
        // The register journals the moves it decides on in the Gateway's
        // store (#255); without one they are decided and logged all the same.
        store.clone(),
        metrics.clone(),
    )
    .context("failed to build the portal register")?
    {
        Some(portals) => {
            // The listing asks the register where the owner's account stands
            // in a suggestion's room (#216). The register is built after the
            // store the listing already holds, because it journals its own
            // moves there (#255), so the two are linked here rather than at
            // either one's construction.
            let portals = Arc::new(portals);
            if let Some(suggestions) = &suggestions {
                suggestions.attach_portals(portals.clone());
            }
            info!(
                sensor = %portals.sensor_user_id(),
                refresh_seconds = config.portal_refresh_seconds,
                "the portal register is enabled: GET /api/portals says which conversations the \
                 bridges have built and which of them the Sensor is inside, and it is inside \
                 none of them until the user says so"
            );
            for bridge in &config.bridges {
                if bridge.as_token.is_none() {
                    // Loud, and for a second reason now: without this token
                    // the register cannot see this bridge's conversations at
                    // all, so the user is offered none of them and the
                    // Sensor stays outside every one.
                    warn!(
                        bridge = %bridge.bridge_id,
                        "no as_token is configured for this bridge, so its portal rooms cannot \
                         be read and none of its conversations can be observed: set \
                         GATEWAY_BRIDGE_{}_AS_TOKEN to the same value as that bridge's \
                         appservice.as_token",
                        twalk_companion_gateway::config::variable_slug(&bridge.bridge_id)
                    );
                }
            }
            Some(portals)
        }
        None => {
            if !config.bridges.is_empty() {
                warn!(
                    "GATEWAY_SENSOR_USER_ID is not set: GET /api/portals answers 503, so nothing \
                     can put the Sensor into the conversations your bridges build and a \
                     connected network will publish nothing at all"
                );
            }
            None
        }
    };

    // Binding fails fast and loud — a configured-but-unusable origin is an
    // operator error to fix, not a condition to swallow (the Sensor's
    // metrics endpoint behaves the same way).
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("failed to bind the gateway origin on {}", config.listen))?;
    let address = listener
        .local_addr()
        .context("failed to read the bound address")?;
    // The readiness marker, and the address a test (GATEWAY_LISTEN with port
    // 0) discovers the origin by.
    info!("companion gateway listening on {address}");

    let app = router(
        Gateway::new(companion, metrics, now_unix_seconds)
            .with_sessions(sessions)
            .with_bootstrap(bootstrap)
            .with_consent(consent)
            .with_bridges(bridges.clone())
            .with_snapshots(snapshots)
            .with_statuses(statuses.clone())
            .with_contacts(contacts)
            .with_approvals(approvals)
            .with_suggestions(suggestions)
            .with_runtime_presence(
                config
                    .consent
                    .as_ref()
                    .map(|consent| Arc::new(RuntimePresence::new(consent.nats_url.clone()))),
            )
            .with_settings(settings)
            .with_portals(portals.clone())
            .with_connections(connections.clone())
            .with_answers(answers),
    );
    // Startup reconciliation (ticket #56): one `whoami` per bridge, after
    // the origin is bound so a slow bridge never delays the Companion coming
    // up. It corrects a state the Gateway was holding when it stopped; a
    // bridge it cannot reach keeps its last known state and its own webhook
    // corrects it.
    if let Some(statuses) = statuses {
        let owner = config
            .sign_in
            .as_ref()
            .map(|sign_in| sign_in.owner.clone())
            .unwrap_or_default();
        tokio::spawn(reconcile(statuses, bridges, owner));
    }
    // The portal register's background read (ticket #105), after the bind
    // for the same reason: it is what keeps `/metrics` able to say how many
    // conversations the Sensor is outside without anyone opening the
    // Companion. The API's own read is always live, so this loop is only
    // about the gauges — and a homeserver it cannot reach costs a warning
    // and a counter, never the origin.
    if let Some(portals) = portals {
        tokio::spawn(refresh_until_shutdown(
            portals,
            config.portal_refresh_seconds,
        ));
    }

    let (shutdown, shutdown_requested) = tokio::sync::oneshot::channel::<()>();
    let mut server = tokio::spawn(
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_requested.await;
            })
            .into_future(),
    );

    tokio::select! {
        joined = &mut server => {
            joined.context("the http server task panicked")?.context("the http server failed")?;
        }
        _ = shutdown_signal() => {
            info!("shutdown signal received, draining in-flight requests");
            let _ = shutdown.send(());
            match tokio::time::timeout(DRAIN_TIMEOUT, &mut server).await {
                Ok(joined) => {
                    joined.context("the http server task panicked")?.context("the http server failed")?;
                    info!("in-flight requests drained, shutting down");
                }
                Err(_) => warn!("shutdown timed out with requests still in flight"),
            }
        }
    }
    info!("companion gateway stopped");
    Ok(())
}

/// Opens the consent store and hands back it and the outbox the router and
/// the publication task share. The store is handed back as well because the
/// pending-contact projection (#54) writes into the same file: the pending
/// list is a view over the seen contacts *and* the decision journal, so the
/// two must be one database and not two.
///
/// Opening applies the embedded schema migrations, so an operator upgrades
/// the image and nothing else. The outbox gauge is published here, before
/// the origin binds: a restart that inherits unpublished decisions reports
/// them from its first scrape, not from its first request.
fn open_consent(
    consent: &Consent,
    metrics: &Arc<Metrics>,
) -> Result<(Arc<Store>, Arc<Outbox>, Arc<Owner>)> {
    // Who this deployment's owner is, under every identity their own traffic
    // arrives under (#149). The store holds it, so no read of the consent
    // state can serve a row about them and no route added later can write one.
    let owner = Arc::new(Owner::new(
        consent.owner.clone(),
        consent.owner_identities.clone(),
    ));
    let store = Store::open(&consent.state_dir, owner.clone())
        .context("failed to open the consent store")?;
    info!(
        store = %store.path().display(),
        owner = %consent.owner,
        owner_identities = %owner.identities().join(","),
        source = %format!("gateway://{}/consent", consent.matrix_domain),
        nats_url = %consent.nats_url,
        "consent store ready: this Gateway is the single writer of consent state"
    );
    if consent.owner_identities.is_empty() {
        // Not a failure: a deployment with no bridge has no ghost, and the
        // owner's own Matrix ID is an identity anyway. Said out loud because on
        // a deployment that *does* bridge a network, this is the variable
        // standing between the user and a consent row about their own phone
        // number — and the list cannot be discovered, so nothing else will
        // notice it is missing.
        info!(
            "GATEWAY_OWNER_IDENTITIES is not set: only the owner's own Matrix ID is known to be \
             theirs, so a consent decision about one of their network ghosts would be recorded \
             like a contact's (the same list the Sensor reads as SENSOR_OWNER_IDENTITIES)"
        );
    }
    let store = Arc::new(store);
    // The rows this journal holds about the owner: withheld from every read
    // from here on, and counted so that withholding is not hiding (#149).
    match store.owner_entries() {
        Ok(rows) if rows.is_empty() => metrics.set_owner_consent_rows(0),
        Ok(rows) => {
            metrics.set_owner_consent_rows(rows.len() as u64);
            warn!(
                rows = rows.len(),
                subjects = %rows
                    .iter()
                    .map(|row| row.subject.id.as_str())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(","),
                "this consent journal holds decisions about the owner: the owner is never a \
                 contact and never has a consent state (ADR 0018, ADR 0021), so no read of this \
                 Gateway serves them. They are kept, not deleted — the journal is append-only — \
                 and most likely arrived before #109, when the user's own messages were \
                 published as a contact's"
            );
        }
        Err(error) => warn!(%error, "failed to read the consent rows held about the owner"),
    }
    let pending = store
        .unpublished_count()
        .context("failed to count the consent outbox")?;
    metrics.set_consent_outbox_pending(pending);
    if pending > 0 {
        // The crash-recovery path, stated plainly because it is the one an
        // operator will want to see after an unclean stop.
        info!(
            pending,
            "committed consent decisions were not published before the last stop; publishing them now"
        );
    }
    Ok((
        store.clone(),
        Arc::new(Outbox::new(
            store,
            metrics.clone(),
            consent.matrix_domain.clone(),
            owner.clone(),
            std::time::SystemTime::now,
        )),
        owner,
    ))
}

/// Resolves when the process is asked to stop (SIGTERM, or SIGINT from an
/// interactive operator).
async fn shutdown_signal() {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("installing a SIGTERM handler never fails");
    tokio::select! {
        _ = sigterm.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}
