//! The collector's consent state (issue #280): the shared cache
//! (`twalk-consent-cache`, #273) fed the Sensor's way — the Companion
//! Companion Gateway's snapshot first, then the `consent.state.changed` stream from
//! the sequence the snapshot hands over (ADR 0010). What the collector reads
//! it for is one question: is this `mailto:` revoked on the mail connection?
//! — the participant rule of `definitions/calendar-event.schema.json`.
//!
//! The consumer is not durable: the snapshot is the record, and a restart
//! re-reads it and starts the stream again from where it says. A deployment
//! with no Companion Gateway has no snapshot and no decisions, and the cache stays
//! empty: nobody is withheld, because nobody was ever decided about.

use anyhow::{Context, Result};
use futures::StreamExt;
use serde_json::Value;
use tracing::{info, warn};
use twalk_consent_cache::{ConsentCache, ConsentChange, Snapshot, CONSENT_CHANGED_TYPE};

/// The cache, filled from the Companion Gateway's snapshot document (the same one the
/// registry is read off), and a task that keeps it current off the bus.
pub async fn follow(
    jetstream: async_nats::jetstream::Context,
    snapshot_document: Option<&Value>,
    owner_email: &str,
) -> Result<ConsentCache> {
    // The owner has no consent state (ADR 0021): a decision about their own
    // address — from the snapshot or the stream — never enters the cache.
    // Their identity here is the `mailto:` every mail and calendar event
    // spells them by; the collector has no Matrix ID to name them by.
    let owner = twalk_consent_cache::owner::Owner::new(
        crate::side::owner_mailto(owner_email),
        std::iter::empty(),
    );
    let cache = ConsentCache::for_people_only(
        Some(owner),
        twalk_consent_cache::bridge_bot::BridgeBots::default(),
    );
    let Some(document) = snapshot_document else {
        info!("no Companion Gateway: no consent decisions, nobody withheld");
        return Ok(cache);
    };
    let snapshot = Snapshot::parse(document).context("the consent snapshot cannot be read")?;
    for why in &snapshot.unusable {
        warn!(reason = ?why, "a consent entry was not applied: {}", why.explained());
    }
    cache.apply_snapshot(&snapshot);
    info!(
        entries = snapshot.entries.len(),
        from_sequence = snapshot.next_stream_sequence,
        "consent snapshot applied; following the stream"
    );
    let stream_cache = cache.clone();
    let start = snapshot.next_stream_sequence;
    tokio::spawn(async move {
        loop {
            match consume(&jetstream, &stream_cache, start).await {
                Ok(()) => warn!("the consent stream ended; reopening it"),
                Err(error) => warn!(%error, "the consent consumer failed; reopening it"),
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
    Ok(cache)
}

async fn consume(
    jetstream: &async_nats::jetstream::Context,
    cache: &ConsentCache,
    start_sequence: u64,
) -> Result<()> {
    let stream = jetstream
        .get_stream("twalk")
        .await
        .context("failed to get the twalk stream")?;
    let consumer = stream
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            filter_subject: crate::status::bus_subject(CONSENT_CHANGED_TYPE),
            ack_policy: async_nats::jetstream::consumer::AckPolicy::None,
            deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::ByStartSequence {
                start_sequence,
            },
            ..Default::default()
        })
        .await
        .context("failed to create the consent consumer")?;
    let mut messages = consumer
        .messages()
        .await
        .context("failed to open the consent stream")?;
    while let Some(message) = messages.next().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                warn!(%error, "consent stream error, continuing");
                continue;
            }
        };
        let Ok(event) = serde_json::from_slice::<Value>(&message.payload) else {
            warn!("a consent.state.changed payload is not JSON; skipped");
            continue;
        };
        match ConsentChange::parse(&event) {
            Ok(change) => {
                cache.apply(&change);
                info!(
                    subject = change.subject_label(),
                    state = change.new_state.as_str(),
                    connections = ?change.connections,
                    "applied a consent change"
                );
            }
            Err(why) => {
                warn!(reason = ?why, "a consent change was not applied: {}", why.explained())
            }
        }
    }
    Ok(())
}
