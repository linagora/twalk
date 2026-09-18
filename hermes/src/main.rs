//! The Hermes runtime binary.
//!
//! All the decisions live in the library modules; this file wires them to
//! NATS JetStream and to the operating system's process table. What it
//! does, in order:
//!
//! 1. reads the configuration ([`twalk_hermes::config`]) — and refuses to
//!    start without a model, because there is no default one (ADR 0015);
//! 2. ensures the stream the deployment publishes on;
//! 3. replays `consent.state.changed` from the beginning of the stream to
//!    learn which personas the user activated (ADR 0013), and keeps
//!    following it;
//! 4. creates each persona's durable consumer — filtered to inbound
//!    messages when the persona is active, and to a subject nothing
//!    publishes on when it is paused;
//! 5. starts one process per persona, with an environment it **constructs**
//!    ([`twalk_hermes::environment`]), and supervises it
//!    ([`twalk_hermes::supervisor`]);
//! 6. on SIGTERM, asks every persona to stop, waits, and exits.
//!
//! Every transition is a structured log line and nothing else: the runtime
//! emits no telemetry of its own, because observability is OTLP and opt-in
//! (ADR 0017) and there is no endpoint configured here to opt into yet. No
//! log line carries message content — the runtime never reads a message.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_nats::jetstream::consumer::{pull, AckPolicy, DeliverPolicy};
use async_nats::jetstream::stream::Stream;
use futures::StreamExt;
use tokio::sync::{watch, Mutex};
use tracing::{error, info, warn};
use twalk_hermes::activation::{
    paused_subject, Activation, PersonaDecision, CONSENT_CHANGED_TYPE, MESSAGE_RECEIVED_TYPE,
};
use twalk_hermes::config::{Config, PersonaSpec};
use twalk_hermes::environment::{passthrough, persona_environment};
use twalk_hermes::supervisor::Restarts;

/// How many redeliveries a persona's durable consumer allows, and how long
/// it waits for an ack. They belong to the consumer, and the runtime owns
/// the consumer, so they are stated here rather than in the SDK (whose
/// `MAX_DELIVER` these mirror: a persona that binds to a consumer it did
/// not create inherits this).
const MAX_DELIVER: i64 = 3;
const ACK_WAIT: Duration = Duration::from_secs(60);

/// How long an ephemeral consumer survives with nobody reading it. The
/// consent follower is ephemeral on purpose — it replays from the beginning
/// of the stream on every start, which is what "follows the consent stream
/// from the beginning" means and why persona activation needs no snapshot
/// (ADR 0013).
const EPHEMERAL_INACTIVE_THRESHOLD: Duration = Duration::from_secs(300);

#[tokio::main]
async fn main() -> Result<()> {
    let config = Arc::new(Config::from_env()?);
    tracing_subscriber::fmt()
        .with_env_filter(&config.log_level)
        .init();
    info!(
        domain = %config.hermes_domain,
        nats = %config.nats_url,
        stream = %config.stream,
        subject_prefix = %config.subject_prefix,
        personas = config.personas.len(),
        model = %config.llm.model,
        endpoint = %config.llm.base_url,
        "hermes starting"
    );
    // Three causes, three messages. "No model configured" is the refusal
    // above, before this line is reached. "The model refused the request"
    // and "the endpoint is unreachable" are told apart by the SDK's client
    // (`sdk/python/twalk_sdk/llm.py`), in the persona's own logs. What
    // neither of them can say early is this one: an endpoint on this host's
    // loopback is reachable from *here* and not from a persona in a
    // container of its own network namespace, which would dial itself. The
    // runtime holds the URL, so it says so at startup rather than letting
    // the first message arrive and time out.
    if config.llm.endpoint_is_loopback() {
        warn!(
            endpoint = %config.llm.base_url,
            "the configured endpoint is on this host's loopback: a persona that does not share \
             this host's network namespace will reach itself, not the model. Give the persona's \
             command host networking, or point HERMES_LLM_BASE_URL at an address its container \
             can resolve (a host-gateway mapping, or the bridge network's gateway)"
        );
    }

    let client = async_nats::connect(&config.nats_url)
        .await
        .with_context(|| format!("failed to connect to the bus at {}", config.nats_url))?;
    let jetstream = async_nats::jetstream::new(client);
    let stream = jetstream
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: config.stream.clone(),
            subjects: config.stream_subjects(),
            ..Default::default()
        })
        .await
        .with_context(|| format!("failed to ensure the stream {}", config.stream))?;

    // Activation, from the beginning of the stream. This happens before any
    // persona is started, so a paused persona is never briefly reading.
    let consent_subject = config.subject(CONSENT_CHANGED_TYPE);
    let consent_consumer = stream
        .create_consumer(pull::Config {
            filter_subject: consent_subject.clone(),
            deliver_policy: DeliverPolicy::All,
            ack_policy: AckPolicy::None,
            inactive_threshold: EPHEMERAL_INACTIVE_THRESHOLD,
            ..Default::default()
        })
        .await
        .with_context(|| format!("failed to follow {consent_subject}"))?;

    let activation = Arc::new(Mutex::new(Activation::new()));
    let replayed = replay_activation(&consent_consumer, &activation).await?;
    info!(
        decisions = replayed,
        subject = %consent_subject,
        "persona activation replayed from the beginning of the stream"
    );

    let inbound_subject = config.subject(MESSAGE_RECEIVED_TYPE);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut supervisors = Vec::with_capacity(config.personas.len());
    for persona in &config.personas {
        let (active, networks) = {
            let activation = activation.lock().await;
            (
                activation.is_active(&persona.id),
                activation.granted_networks(&persona.id),
            )
        };
        ensure_persona_consumer(&stream, &config, persona, active).await?;
        info!(
            persona = %persona.id,
            active,
            ?networks,
            consumer = %persona.consumer_name(),
            "persona activation"
        );
        let mut environment = passthrough(&config);
        // The persona's own variables last: what the runtime computed wins
        // over anything an operator put in the passthrough list.
        environment.extend(persona_environment(&config, persona));
        supervisors.push(tokio::spawn(supervise(
            persona.clone(),
            environment,
            Arc::clone(&config),
            shutdown_rx.clone(),
        )));
    }

    // Keep following: a decision taken while Hermes runs moves the
    // persona's consumer, and never its process (ADR 0013).
    let follower = tokio::spawn(follow_activation(
        consent_consumer,
        stream.clone(),
        Arc::clone(&config),
        Arc::clone(&activation),
        shutdown_rx.clone(),
    ));

    info!(
        personas = config.personas.len(),
        subject = %inbound_subject,
        "hermes running"
    );

    shutdown_signal().await;
    info!("shutdown signal received, stopping personas");
    let _ = shutdown_tx.send(true);
    follower.abort();
    // The grace period is per persona and they stop in parallel; the extra
    // second is for the runtime's own bookkeeping, not for a straggler.
    let deadline = config.shutdown_grace + Duration::from_secs(1);
    for supervisor in supervisors {
        if tokio::time::timeout(deadline, supervisor).await.is_err() {
            warn!("a persona supervisor did not finish within the shutdown grace period");
        }
    }
    info!("hermes stopped");
    Ok(())
}

/// Creates — or moves — one persona's durable consumer.
///
/// This is the whole of "a paused persona runs and receives nothing": an
/// active persona's consumer is filtered to `inbound.message.received`, a
/// paused one's to a subject no producer publishes on. The persona binds to
/// this consumer by name (the SDK's `pull_subscribe` binds to an existing
/// durable rather than redefining it), so the filter is the runtime's to
/// set and the persona's to live with.
async fn ensure_persona_consumer(
    stream: &Stream,
    config: &Config,
    persona: &PersonaSpec,
    active: bool,
) -> Result<()> {
    let name = persona.consumer_name();
    let filter_subject = if active {
        config.subject(MESSAGE_RECEIVED_TYPE)
    } else {
        paused_subject(&config.subject_prefix, &persona.id)
    };
    stream
        .create_consumer(pull::Config {
            durable_name: Some(name.clone()),
            name: Some(name.clone()),
            filter_subject: filter_subject.clone(),
            // Explicit acks: an event is acked once the persona is done with
            // it, so a crash mid-processing redelivers it.
            ack_policy: AckPolicy::Explicit,
            ack_wait: ACK_WAIT,
            // A fresh consumer starts at the beginning of the stream: the
            // events that arrived before this persona was ever activated are
            // still the user's messages.
            deliver_policy: DeliverPolicy::All,
            max_deliver: MAX_DELIVER,
            ..Default::default()
        })
        .await
        .with_context(|| format!("failed to ensure the consumer {name} on {filter_subject}"))?;
    Ok(())
}

/// Drains the consent subject to the end of the stream, applying every
/// persona decision. Returns how many it applied.
async fn replay_activation(
    consumer: &async_nats::jetstream::consumer::Consumer<pull::Config>,
    activation: &Mutex<Activation>,
) -> Result<usize> {
    let mut applied = 0;
    loop {
        let mut batch = consumer
            .fetch()
            .max_messages(256)
            .messages()
            .await
            .context("failed to read the consent history")?;
        let mut received = 0;
        while let Some(message) = batch.next().await {
            let message = message
                .map_err(|error| anyhow::anyhow!("failed to read a consent decision: {error}"))?;
            received += 1;
            if apply_message(&message.payload, activation).await.is_some() {
                applied += 1;
            }
        }
        if received == 0 {
            return Ok(applied);
        }
    }
}

/// Follows the consent subject for as long as Hermes runs, moving each
/// persona's consumer as the user's decisions land.
async fn follow_activation(
    consumer: async_nats::jetstream::consumer::Consumer<pull::Config>,
    stream: Stream,
    config: Arc<Config>,
    activation: Arc<Mutex<Activation>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut messages = match consumer.messages().await {
        Ok(messages) => messages,
        Err(error) => {
            error!(%error, "cannot follow persona activation; personas keep their current state");
            return;
        }
    };
    loop {
        let message = tokio::select! {
            message = messages.next() => message,
            _ = shutdown_requested(&mut shutdown) => return,
        };
        let Some(Ok(message)) = message else {
            // The subscription ended or errored; the personas keep the
            // activation they have, which is the safe direction — a paused
            // persona stays paused.
            warn!("the persona activation subscription ended; activation is now frozen");
            return;
        };
        let Some(decision) = apply_message(&message.payload, &activation).await else {
            continue;
        };
        // A decision names exactly one persona, so only that one's consumer
        // moves. A deployment that does not host it has nothing to do —
        // personas are configured here, activated there, and neither list is
        // the other's.
        let Some(persona) = config
            .personas
            .iter()
            .find(|persona| persona.id == decision.persona_id)
        else {
            continue;
        };
        let (active, networks) = {
            let activation = activation.lock().await;
            (
                activation.is_active(&persona.id),
                activation.granted_networks(&persona.id),
            )
        };
        match ensure_persona_consumer(&stream, &config, persona, active).await {
            Ok(()) => info!(
                persona = %persona.id,
                active,
                ?networks,
                "persona activation changed"
            ),
            Err(error) => error!(
                persona = %persona.id,
                %error,
                "failed to apply the persona's activation to its consumer"
            ),
        }
    }
}

/// Applies one bus message if it is a persona decision, and gives it back.
async fn apply_message(payload: &[u8], activation: &Mutex<Activation>) -> Option<PersonaDecision> {
    let Ok(event) = serde_json::from_slice::<serde_json::Value>(payload) else {
        warn!("a message on the consent subject is not JSON; ignoring it");
        return None;
    };
    let decision = PersonaDecision::parse(&event)?;
    info!(
        persona = %decision.persona_id,
        new_state = %decision.new_state,
        networks = ?decision.networks,
        "persona consent decision"
    );
    activation.lock().await.apply(&decision);
    Some(decision)
}

/// One persona's whole life: start it, watch it, start it again.
///
/// This function cannot see the activation state, and that is deliberate:
/// activation is not a process switch (ADR 0013), so there must be no path
/// from a consent decision to a kill.
async fn supervise(
    persona: PersonaSpec,
    environment: Vec<(String, String)>,
    config: Arc<Config>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut restarts = Restarts::new(config.restart);
    while !*shutdown.borrow() {
        let started = Instant::now();
        let mut child = match spawn(&persona, &environment) {
            Ok(child) => {
                info!(
                    persona = %persona.id,
                    pid = child.id().unwrap_or_default(),
                    command = %persona.command.join(" "),
                    "persona started"
                );
                child
            }
            Err(error) => {
                // Nothing to wait for: the process never existed. It is the
                // same event as a run that ended instantly, and it is
                // recorded as one so the failure verdict below covers it.
                error!(persona = %persona.id, %error, "persona could not be started");
                if !wait_backoff(&persona, &mut restarts, Duration::ZERO, &mut shutdown).await {
                    break;
                }
                continue;
            }
        };

        tokio::select! {
            status = child.wait() => {
                let ran_for = started.elapsed();
                match status {
                    Ok(status) => info!(
                        persona = %persona.id,
                        status = %status,
                        ran_for_ms = ran_for.as_millis() as u64,
                        "persona exited"
                    ),
                    Err(error) => warn!(
                        persona = %persona.id,
                        %error,
                        "failed to wait for the persona; treating it as exited"
                    ),
                }
                if !wait_backoff(&persona, &mut restarts, ran_for, &mut shutdown).await {
                    break;
                }
            }
            _ = shutdown_requested(&mut shutdown) => {
                terminate(&persona, &mut child, config.shutdown_grace).await;
                break;
            }
        }
    }
    info!(persona = %persona.id, "persona stopped");
}

/// Records a finished run, says what it means, and waits out the backoff.
/// Returns false when the wait was cut short by a shutdown.
async fn wait_backoff(
    persona: &PersonaSpec,
    restarts: &mut Restarts,
    ran_for: Duration,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    let restart = restarts.record_run(ran_for);
    if restart.announce_failed {
        // The line that keeps a runtime which cannot start something from
        // looking like one that is still starting it.
        error!(
            persona = %persona.id,
            attempts = restart.consecutive_failures,
            command = %persona.command.join(" "),
            "persona failed to start: it has not stayed up once; still retrying"
        );
    } else if restarts.has_failed() {
        warn!(
            persona = %persona.id,
            attempts = restart.consecutive_failures,
            delay_ms = restart.delay.as_millis() as u64,
            "persona still failing to start, retrying"
        );
    } else {
        info!(
            persona = %persona.id,
            delay_ms = restart.delay.as_millis() as u64,
            "persona restarting"
        );
    }
    tokio::select! {
        _ = tokio::time::sleep(restart.delay) => true,
        _ = shutdown_requested(shutdown) => false,
    }
}

fn spawn(
    persona: &PersonaSpec,
    environment: &[(String, String)],
) -> std::io::Result<tokio::process::Child> {
    let mut command = tokio::process::Command::new(&persona.command[0]);
    command.args(&persona.command[1..]);
    // The child's environment is constructed, not inherited: see
    // `twalk_hermes::environment`. Without this line a persona would be
    // handed every secret the runtime holds.
    command.env_clear();
    command.envs(environment.iter().cloned());
    // A supervisor that dies must not leave its personas running: an
    // orphaned persona still holds the durable consumer and would answer
    // messages nobody is supervising.
    command.kill_on_drop(true);
    command.spawn()
}

/// Asks a persona to stop the way an operator's process manager does —
/// SIGTERM, then a bounded wait, then SIGKILL. The SDK finishes the event
/// in flight and acks it on SIGTERM, so this is what makes a clean shutdown
/// clean rather than a redelivery.
async fn terminate(persona: &PersonaSpec, child: &mut tokio::process::Child, grace: Duration) {
    let Some(pid) = child.id() else {
        return;
    };
    info!(persona = %persona.id, pid, "asking the persona to stop");
    // SAFETY: `kill(2)` with a pid this process owns and has not yet
    // reaped. `tokio::process::Child::kill` only sends SIGKILL, which
    // cannot exercise a graceful stop.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(status)) => info!(persona = %persona.id, status = %status, "persona exited"),
        Ok(Err(error)) => warn!(persona = %persona.id, %error, "failed to wait for the persona"),
        Err(_) => {
            warn!(
                persona = %persona.id,
                grace_ms = grace.as_millis() as u64,
                "persona did not stop within the grace period, killing it"
            );
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

/// Resolves once shutdown has been requested, and immediately on every call
/// afterwards — so it can be used in as many `select!` arms as needed.
async fn shutdown_requested(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stop| *stop).await;
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
