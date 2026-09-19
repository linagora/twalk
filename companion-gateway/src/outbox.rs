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
//! Marking a row published also records **where** on the bus it landed: the
//! ack's stream sequence goes into the journal beside it, because that is the
//! position the consent snapshot names for a cold consumer to start from
//! (ticket #50, [`crate::store::Store::snapshot`]). A decision is therefore
//! either unpublished, or published with a known position — never published
//! and nowhere.
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

use crate::consent::{self, Decision, Invalid};
use crate::metrics::Metrics;
use crate::owner::Owner;
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
    /// The deployment's single owner: the Matrix ID stamped on every event as
    /// `data.actor`, and every identity their own traffic arrives under. One
    /// owner per Gateway (ADR 0011), so the device a decision arrives from is
    /// the owner's by construction — the device identifies which of their
    /// devices, not which human.
    ///
    /// The identities are here, at the writer, because this is the one place a
    /// consent row can be created: the owner is never a contact and never has
    /// a consent state (ADR 0018, ADR 0021), so the writer is what refuses one
    /// (ticket #149).
    owner: Arc<Owner>,
    /// The clock, injected as everywhere else in this crate so the
    /// timestamps are testable.
    now: fn() -> std::time::SystemTime,
    /// Woken by [`Outbox::record`]; awaited by [`publish_until_shutdown`].
    awake: Notify,
}

/// Why the writer refused a decision (ticket #149).
///
/// Two failures that must not be answered the same way. [`Self::Invalid`] is
/// the decision itself: the store was never asked, nothing is wrong with this
/// Gateway, and the caller gets a 4xx with a code it can branch on.
/// [`Self::Store`] is this Gateway failing to keep a decision the user took,
/// which is a 500 and an operator's problem.
#[derive(Debug)]
pub enum Refused {
    /// The decision cannot be recorded, whoever asks and however often —
    /// today, only a subject that is one of the owner's own identities. It is
    /// not a transient failure and retrying it changes nothing.
    Invalid(Invalid),
    /// The journal could not be written.
    Store(anyhow::Error),
}

impl Outbox {
    pub fn new(
        store: Arc<Store>,
        metrics: Arc<Metrics>,
        domain: String,
        owner: Arc<Owner>,
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

    /// This deployment's owner and their confirmed identities: who this writer
    /// refuses to record a decision about (ticket #149, [`crate::owner`]).
    pub fn owner(&self) -> &Owner {
        &self.owner
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
    /// A decision whose subject is one of the owner's own identities is
    /// refused here, before the journal is touched: the owner is never a
    /// contact and never has a consent state (ADR 0018, ADR 0021), so there is
    /// nothing to record, and saying so is the point — a decision silently
    /// dropped would leave the user's screen claiming a state that no store
    /// holds. The refusal carries its own code, `subject_is_the_owner`, which
    /// is deliberately not "no such subject" (ticket #149).
    ///
    /// It is checked at the writer rather than at the route, because a route
    /// is something a later ticket adds: `consent.state.changed` is never
    /// emitted about an owner identity because no owner identity ever reaches
    /// the journal the outbox drains.
    pub fn record(
        &self,
        decision: &Decision,
        device: &Device,
    ) -> std::result::Result<Committed, Refused> {
        if let Err(invalid) = decision.refuse_if_owner(&self.owner) {
            warn!(
                subject = %decision.subject.id,
                code = invalid.code(),
                "refused a consent decision about the owner: the owner is never a contact and \
                 has no consent state, so there is nothing to decide"
            );
            self.metrics.record_owner_decision_refused();
            return Err(Refused::Invalid(invalid));
        }
        let at = consent::rfc3339_millis((self.now)());
        let committed = self
            .store
            .record(decision, self.owner.matrix_id(), &at, &self.domain, &at)
            .context("failed to record the consent decision")
            .map_err(Refused::Store)?;
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

    /// Records one bridge state change and wakes the publication loop
    /// (ticket #56).
    ///
    /// The same three steps as a decision, for the same reason: commit,
    /// publish, mark published. A bridge transition is not an audit trail,
    /// but it is a fact the dashboard is about to be told, and a crash
    /// between the commit and the publication must republish rather than
    /// lose it — the contract's deterministic id makes the bus absorb the
    /// duplicate.
    ///
    /// This is deliberately the *same* outbox: one loop, one connection, one
    /// set of exactly-once rules. See [`crate::bridge_status`] for why the
    /// Gateway is the only producer of these events.
    pub fn record_bridge_status(
        &self,
        transition: &crate::bridge_status::Transition,
    ) -> Result<crate::store::BridgeStatusCommitted> {
        let at = consent::rfc3339_millis((self.now)());
        let committed = self
            .store
            .record_bridge_status(transition, &self.domain, &at)
            .context("failed to record the bridge status change")?;
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
        match self.store.unpublished_bridge_status_count() {
            Ok(pending) => self.metrics.set_bridge_status_outbox_pending(pending),
            Err(error) => warn!(%error, "failed to count the bridge status outbox"),
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
    let bridge_subject = consent::bus_subject(crate::bridge_status::BRIDGE_STATUS_CHANGED_TYPE);
    info!(
        %nats_url,
        %subject,
        %bridge_subject,
        "the gateway outbox is publishing"
    );
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
            // The bridge transitions ride the same loop and the same
            // connection (ticket #56). Drained after the decisions and in
            // their own call, so a bus that refuses one kind still delivers
            // the other — a bridge outage must not stall a consent decision,
            // and the reverse would be just as wrong.
            if let Err(error) = drain_bridge_status(&outbox, &jetstream, &bridge_subject).await {
                warn!(%error, "the bridge status outbox could not drain; retrying");
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
        // The ack's sequence is where the bus stored the event, and it is
        // what the snapshot names as its position (#50). On a republish the
        // bus deduplicates and answers with the sequence of the message it
        // already holds, so the position recorded is the same whichever
        // attempt won.
        outbox.store.mark_published(
            decision.sequence,
            &consent::rfc3339_millis((outbox.now)()),
            ack.sequence,
        )?;
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

/// Publishes every waiting bridge transition, oldest first (ticket #56).
///
/// The same shape as [`drain`], with one difference worth naming: a bridge
/// transition has no snapshot to be consistent with, so the stream sequence
/// it records is bookkeeping an operator can read and nothing depends on it.
/// What it shares is the part that matters — the `Nats-Msg-Id` header, which
/// is the contract's deterministic id, so a row republished after a crash
/// between the publish and the mark is absorbed by the bus instead of
/// becoming a second state change on somebody's dashboard.
async fn drain_bridge_status(
    outbox: &Arc<Outbox>,
    jetstream: &async_nats::jetstream::Context,
    subject: &str,
) -> Result<()> {
    let waiting = outbox.store.unpublished_bridge_status(DRAIN_BATCH)?;
    if waiting.is_empty() {
        return Ok(());
    }
    for change in waiting {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(
            async_nats::header::NATS_MESSAGE_ID,
            change.event_id.as_str(),
        );
        let payload = serde_json::to_vec(&change.envelope)?;
        let ack = jetstream
            .publish_with_headers(subject.to_owned(), headers, payload.into())
            .await
            .with_context(|| {
                format!("failed to publish bridge status change {}", change.sequence)
            })?;
        let ack = ack.await.with_context(|| {
            format!(
                "the bus did not ack bridge status change {}",
                change.sequence
            )
        })?;
        outbox.store.mark_bridge_status_published(
            change.sequence,
            &consent::rfc3339_millis((outbox.now)()),
            ack.sequence,
        )?;
        outbox.metrics.record_bridge_status_published();
        info!(
            event_id = %change.event_id,
            sequence = change.sequence,
            stream_sequence = ack.sequence,
            duplicate = ack.duplicate,
            "published a bridge status change"
        );
    }
    outbox.observe_pending();
    Ok(())
}
