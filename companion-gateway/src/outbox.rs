//! The transactional outbox: how a committed decision reaches the bus
//! exactly once (ticket #49).
//!
//! The rule the whole design serves: **commit, then publish, then mark
//! published**. A decision is durable before anything is said about it, so a
//! crash between the commit and the publication republishes rather than
//! losing — and because the event carries the contract's deterministic id as
//! `Nats-Msg-Id`, a republished row is deduplicated by the bus instead of
//! becoming a second decision. The other order (publish, then commit) can
//! announce a decision the store never kept, which on a consent journal is
//! the worse failure: the audit trail would claim a grant the Gateway does
//! not have.
//!
//! The request path never waits for the bus. Recording notifies this
//! module's loop and answers; the loop drains the outbox, and while the bus
//! is unreachable the decisions simply accumulate as unpublished rows. That
//! is what makes a bus outage a delay in publication instead of a refusal to
//! record the user's decision.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::consent::{self, Decision};
use crate::metrics::Metrics;
use crate::session::Device;
use crate::store::{Committed, Store};

/// How often the loop sweeps the outbox without being notified: the interval
/// at which a decision left unpublished by a bus outage is retried. Recording
/// notifies the loop directly, so this is the recovery path, not the happy
/// one.
const SWEEP_INTERVAL: Duration = Duration::from_secs(1);

/// How many decisions one drain publishes before yielding. A batch bound, so
/// a store holding a long backlog after an outage does not hold the loop
/// forever; the next sweep continues where this one stopped.
const DRAIN_BATCH: usize = 256;

/// The consent writer: it stamps a decision, commits it to the journal, and
/// hands it to the publication loop.
pub struct Outbox {
    store: Arc<Store>,
    metrics: Arc<Metrics>,
    /// The domain the Gateway's events name themselves by:
    /// `gateway://<domain>/consent`.
    domain: String,
    /// The Matrix ID of the deployment's single owner, stamped on every
    /// event as `data.actor`. One owner per Gateway (ADR 0011), so the
    /// device a decision arrives from is the owner's by construction — the
    /// device identifies which of their devices, not which human.
    owner: String,
    /// The clock, injected as everywhere else in this crate so the
    /// timestamps are testable.
    now: fn() -> std::time::SystemTime,
    /// Woken by [`Outbox::record`]; awaited by [`publish_until_shutdown`].
    awake: Notify,
}

impl Outbox {
    pub fn new(
        store: Arc<Store>,
        metrics: Arc<Metrics>,
        domain: String,
        owner: String,
        now: fn() -> std::time::SystemTime,
    ) -> Self {
        Self {
            store,
            metrics,
            domain,
            owner,
            now,
            awake: Notify::new(),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Records one decision and wakes the publication loop.
    ///
    /// Both of the contract's timestamps are stamped here: `occurred_at` is
    /// when the user took the decision, which for a decision arriving over
    /// HTTP is now, and it is part of the event's natural key; `time` is when
    /// the Gateway produced the event. They are the same instant on the first
    /// publication and differ on a republish, which is why only the first is
    /// in the id.
    pub fn record(&self, decision: &Decision, device: &Device) -> Result<Committed> {
        let at = consent::rfc3339_millis((self.now)());
        let committed = self
            .store
            .record(decision, &self.owner, &at, &self.domain, &at)
            .context("failed to record the consent decision")?;
        if committed.replayed {
            debug!(
                event_id = %committed.event_id,
                "the identical consent decision was already recorded; keeping the first"
            );
        } else {
            info!(
                event_id = %committed.event_id,
                sequence = committed.sequence,
                device = %device.id,
                subject_type = committed.recorded.decision.subject.kind.as_str(),
                old_state = committed.recorded.old_state.as_str(),
                new_state = committed.recorded.decision.new_state.as_str(),
                scope = %committed.recorded.decision.scope_key(),
                "recorded a consent decision"
            );
            self.metrics
                .record_consent_decision(committed.recorded.decision.subject.kind);
        }
        self.observe_pending();
        self.awake.notify_one();
        Ok(committed)
    }

    /// Republishes the outbox gauge from the store. Cheap (one indexed
    /// count), and it keeps the number an operator scrapes honest whoever
    /// moved the outbox.
    fn observe_pending(&self) {
        match self.store.unpublished_count() {
            Ok(pending) => self.metrics.set_consent_outbox_pending(pending),
            Err(error) => warn!(%error, "failed to count the consent outbox"),
        }
    }
}

/// Publishes committed decisions until the process stops.
///
/// Connects with NATS's own initial-connect retry, so a Gateway that starts
/// before the bus — or without one — still serves, records and accumulates:
/// the outbox is exactly the mechanism that makes that safe. The stream is
/// the Sensor's (`twalk`, `twalk.>`), ensured here as well because the
/// reference deployment must publish consent decisions whether or not a
/// Sensor ever started.
pub async fn publish_until_shutdown(outbox: Arc<Outbox>, nats_url: String) {
    let client = match async_nats::ConnectOptions::new()
        .retry_on_initial_connect()
        .connect(&nats_url)
        .await
    {
        Ok(client) => client,
        // Only a malformed URL reaches here: with retry_on_initial_connect a
        // bus that is merely down connects later, in the background.
        Err(error) => {
            warn!(%error, %nats_url, "the consent outbox cannot use this bus url: committed decisions will not be published");
            return;
        }
    };
    let jetstream = async_nats::jetstream::new(client);
    let subject = consent::bus_subject(consent::CONSENT_CHANGED_TYPE);
    info!(%nats_url, %subject, "the consent outbox is publishing");
    let mut stream_ready = false;
    loop {
        if !stream_ready {
            match jetstream
                .get_or_create_stream(async_nats::jetstream::stream::Config {
                    name: consent::STREAM_NAME.to_owned(),
                    subjects: consent::STREAM_SUBJECTS
                        .iter()
                        .map(|subject| subject.to_string())
                        .collect(),
                    ..Default::default()
                })
                .await
            {
                Ok(_) => {
                    stream_ready = true;
                    debug!(
                        stream = consent::STREAM_NAME,
                        "bus ready for consent events"
                    );
                }
                Err(error) => {
                    debug!(%error, "the bus is not ready yet; the outbox keeps the decisions")
                }
            }
        }
        if stream_ready {
            if let Err(error) = drain(&outbox, &jetstream, &subject).await {
                // Not fatal and not lost: the rows are still unpublished and
                // the next sweep tries again.
                warn!(%error, "the consent outbox could not drain; retrying");
            }
        }
        tokio::select! {
            _ = outbox.awake.notified() => {}
            _ = tokio::time::sleep(SWEEP_INTERVAL) => {}
        }
    }
}

/// Publishes every waiting decision, oldest first, and marks each one as it
/// goes. Stops at the first failure so that the journal's order is also the
/// bus's.
async fn drain(
    outbox: &Arc<Outbox>,
    jetstream: &async_nats::jetstream::Context,
    subject: &str,
) -> Result<()> {
    let waiting = outbox.store.unpublished(DRAIN_BATCH)?;
    if waiting.is_empty() {
        return Ok(());
    }
    for decision in waiting {
        let mut headers = async_nats::HeaderMap::new();
        // The de-duplication key: a row republished after a crash between
        // the publish and the mark is absorbed by the bus instead of
        // becoming a second event.
        headers.insert(
            async_nats::header::NATS_MESSAGE_ID,
            decision.event_id.as_str(),
        );
        let payload = serde_json::to_vec(&decision.envelope)?;
        let ack = jetstream
            .publish_with_headers(subject.to_owned(), headers, payload.into())
            .await
            .with_context(|| format!("failed to publish decision {}", decision.sequence))?;
        let ack = ack
            .await
            .with_context(|| format!("the bus did not ack decision {}", decision.sequence))?;
        outbox
            .store
            .mark_published(decision.sequence, &consent::rfc3339_millis((outbox.now)()))?;
        outbox.metrics.record_consent_published();
        info!(
            event_id = %decision.event_id,
            sequence = decision.sequence,
            stream_sequence = ack.sequence,
            duplicate = ack.duplicate,
            "published a consent decision"
        );
    }
    outbox.observe_pending();
    Ok(())
}
