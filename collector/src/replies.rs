//! The approval consumer (issue #278): `persona.reply.approved.v1` read
//! durably, the approvals whose target is the mail connection this process
//! holds sent from the owner's mailbox, and the Sensor's outbound policy on
//! every outcome — a transient failure redelivered with a growing delay, a
//! permanent one or the last allowed attempt dead-lettered with the reason
//! in a header, a sent reply reported on `.posted` with `reach=contact`
//! (#216). The pure half is `outbound.rs`; the JMAP calls are
//! `Mailbox::send_reply`.
//!
//! Two things the Sensor's shape does not have, because a mailbox is not a
//! room. A lost response after a successful submission would send twice on
//! redelivery — Matrix deduplicates on a transaction id, JMAP on nothing —
//! so every reply carries the approval's id in a header of its own
//! (`X-Twalk-Approval`) and the mailbox is asked for it before anything is
//! created: a reply already in Sent is reported, not sent again. And an
//! `Email/set` that succeeded before an `EmailSubmission/set` that did not
//! leaves a draft on the server; the draft is destroyed on the way to the
//! retry, so a refusal never accumulates the approved text in Drafts.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::StreamExt;
use tracing::{debug, error, info, warn};

use crate::mails::Mailbox;
use crate::metrics::Metrics;
use crate::oidc::AccessToken;
use crate::outbound::{self, Parsed, SendError};

/// The Sensor's variables, on this side.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub base: Duration,
    pub max_attempts: i64,
}

/// The access token the run loop keeps fresh, read by the consumer.
pub type SharedAccess = Arc<tokio::sync::RwLock<Option<AccessToken>>>;

/// What this process proves itself with, shared by every task that asks a
/// service (#342). An OIDC connection replaces it at every renewal; a
/// `Basic` one sets it once at start and never again, because there is
/// nothing to renew.
pub type SharedCredential = Arc<tokio::sync::RwLock<Option<crate::side::Credential>>>;

/// Never returns: a consumer that fails is rebuilt after a short delay.
pub async fn consume_approvals(
    jetstream: async_nats::jetstream::Context,
    mailbox: Arc<Mailbox>,
    held: Vec<String>,
    owner_email: String,
    access: SharedCredential,
    metrics: Arc<Metrics>,
    retry: RetryPolicy,
) {
    loop {
        match run(
            &jetstream,
            &mailbox,
            &held,
            &owner_email,
            &access,
            &metrics,
            retry,
        )
        .await
        {
            Ok(()) => error!("the approval stream ended; rebuilding the consumer"),
            Err(error) => error!(%error, "the approval consumer failed; rebuilding it"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn run(
    jetstream: &async_nats::jetstream::Context,
    mailbox: &Mailbox,
    held: &[String],
    owner_email: &str,
    access: &SharedCredential,
    metrics: &Metrics,
    retry: RetryPolicy,
) -> Result<()> {
    let stream = jetstream
        .get_stream("twalk")
        .await
        .context("failed to get the twalk stream")?;
    // One durable per mail connection, created at `New`: an approval
    // published before this collector first ran cannot target a connection
    // it publishes for, since the trigger it answers came from this
    // collector. A durable created earlier resumes at its own ack floor.
    // Renaming `COLLECTOR_MAIL_CONNECTION` orphans the old durable and the
    // new one starts at `New` — a rename is a new connection, and an
    // approval towards the old id would not be this collector's anyway.
    let name = outbound::reply_consumer(&mailbox.connection);
    let consumer = stream
        .get_or_create_consumer(
            &name,
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some(name.clone()),
                filter_subject: crate::status::bus_subject(outbound::REPLY_APPROVED_TYPE),
                ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::New,
                ..Default::default()
            },
        )
        .await
        .context("failed to ensure the approval consumer")?;
    info!(consumer = %name, "consuming approved replies");
    let mut messages = consumer
        .messages()
        .await
        .context("failed to open the approval stream")?;
    while let Some(message) = messages.next().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                warn!(%error, "approval stream error, continuing");
                continue;
            }
        };
        let delivered = message.info().map(|info| info.delivered).unwrap_or(1);
        let event: serde_json::Value = match serde_json::from_slice(&message.payload) {
            Ok(event) => event,
            Err(error) => {
                error!(%error, "an approval that is not JSON; dead-lettering");
                dead_letter(jetstream, &message, "not JSON", metrics).await;
                continue;
            }
        };
        let reply = match outbound::ApprovedReply::parse(&event, held) {
            Ok(Parsed::Ours(reply)) => reply,
            Ok(Parsed::AnotherComponents { why }) => {
                debug!(
                    why = why.as_str(),
                    "an approval that is not this collector's"
                );
                if let Err(error) = message.ack().await {
                    warn!(%error, "ack failed on another component's approval");
                }
                continue;
            }
            Err(error) => {
                error!(%error, "an unusable approval; dead-lettering");
                dead_letter(jetstream, &message, &format!("{error:#}"), metrics).await;
                continue;
            }
        };
        // No token is not an attempt: the grant is renewing, or gone until
        // the operator authorizes again. Waited for a while with the bus
        // told the message is in progress, rather than spending its retry
        // budget on rounds nothing was tried in; past the wait, redelivered.
        let Some(token) = wait_for_token(access, &message, &reply.event_id).await else {
            if let Err(error) = message
                .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                    outbound::MAX_RETRY_BACKOFF,
                )))
                .await
            {
                warn!(%error, "nak failed");
            }
            continue;
        };
        match mailbox.send_reply(&reply, &token).await {
            Ok(sent) => {
                metrics.record_published("persona.reply.approved.posted");
                report_posted(jetstream, &message, &reply.event_id, owner_email).await;
                if let Err(error) = message.ack().await {
                    warn!(id = %reply.event_id, %error, "ack failed after a sent reply");
                }
                info!(
                    id = %reply.event_id,
                    connection = %reply.connection,
                    already = sent.already_sent,
                    traceparent = reply.traceparent.as_deref(),
                    "an approved reply left from the owner's mailbox"
                );
            }
            Err(SendError::Permanent(why)) => {
                error!(id = %reply.event_id, %why, "an approved reply can never be sent; dead-lettering");
                dead_letter(jetstream, &message, &why, metrics).await;
            }
            Err(SendError::Transient(why)) if delivered >= retry.max_attempts => {
                error!(id = %reply.event_id, %why, delivered, "an approved reply exhausted its retries; dead-lettering");
                dead_letter(jetstream, &message, &why, metrics).await;
            }
            Err(SendError::Transient(why)) => {
                let delay = outbound::retry_delay(retry.base, delivered);
                warn!(id = %reply.event_id, %why, delivered, ?delay, "an approved reply could not be sent; retrying");
                if let Err(error) = message
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(delay)))
                    .await
                {
                    warn!(%error, "nak failed");
                }
            }
        }
    }
    Ok(())
}

/// The credential, waited for up to a minute with the bus told every few
/// seconds that the message is being worked on.
async fn wait_for_token(
    access: &SharedCredential,
    message: &async_nats::jetstream::Message,
    event_id: &str,
) -> Option<crate::side::Credential> {
    const WAIT: Duration = Duration::from_secs(60);
    const PROGRESS_EVERY: Duration = Duration::from_secs(5);
    let started = std::time::Instant::now();
    loop {
        if let Some(credential) = access.read().await.clone() {
            return Some(credential);
        }
        if started.elapsed() >= WAIT {
            warn!(id = %event_id, "no credential to send the reply with after a minute; the approval waits for the grant");
            return None;
        }
        if let Err(error) = message
            .ack_with(async_nats::jetstream::AckKind::Progress)
            .await
        {
            warn!(%error, "progress ack failed");
        }
        tokio::time::sleep(PROGRESS_EVERY).await;
    }
}

/// The dead-letter copy of an approval, on `<subject>.dead`, the event
/// unchanged and the reason in a header. The original is acked only once
/// its copy is on the bus: a copy the bus did not take leaves the original
/// to be redelivered, so an approved reply is never lost between the two
/// (the Sensor's rule).
async fn dead_letter(
    jetstream: &async_nats::jetstream::Context,
    message: &async_nats::jetstream::Message,
    reason: &str,
    metrics: &Metrics,
) {
    let event = serde_json::from_slice::<serde_json::Value>(&message.payload).ok();
    let event_id = event
        .as_ref()
        .and_then(|event| {
            event
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let mut headers = async_nats::header::HeaderMap::new();
    headers.insert(
        async_nats::header::NATS_MESSAGE_ID,
        outbound::dead_letter_msg_id(&event_id).as_str(),
    );
    headers.insert(outbound::EVENT_ID_HEADER, event_id.as_str());
    headers.insert(
        outbound::REASON_HEADER,
        reason.chars().take(512).collect::<String>().as_str(),
    );
    if let Some(event) = &event {
        duplicate_extensions(event, &mut headers);
    }
    let stored = match jetstream
        .publish_with_headers(
            outbound::dead_letter_subject(),
            headers,
            message.payload.clone(),
        )
        .await
    {
        Ok(ack) => match ack.await {
            Ok(_) => {
                metrics.record_published("persona.reply.approved.dead");
                true
            }
            Err(error) => {
                warn!(%event_id, %error, "dead-letter publish was not acked; the original stays for redelivery");
                false
            }
        },
        Err(error) => {
            warn!(%event_id, %error, "dead-letter publish failed; the original stays for redelivery");
            false
        }
    };
    if stored {
        if let Err(error) = message.ack().await {
            warn!(%event_id, %error, "ack failed after dead-lettering");
        }
    }
}

/// The reach report (#216): the approval republished unchanged on
/// `<subject>.posted`, `reach=contact` and `posted-as` the owner's own
/// address — a mail from the owner's mailbox reaches the recipient by
/// construction, which is the whole point of ADR 0038.
async fn report_posted(
    jetstream: &async_nats::jetstream::Context,
    message: &async_nats::jetstream::Message,
    event_id: &str,
    owner_email: &str,
) {
    let mut headers = async_nats::header::HeaderMap::new();
    headers.insert(
        async_nats::header::NATS_MESSAGE_ID,
        outbound::posted_msg_id(event_id).as_str(),
    );
    headers.insert(outbound::EVENT_ID_HEADER, event_id);
    headers.insert(outbound::POSTED_REACH_HEADER, "contact");
    headers.insert(
        outbound::POSTED_AS_HEADER,
        crate::side::owner_mailto(owner_email).as_str(),
    );
    if let Ok(event) = serde_json::from_slice::<serde_json::Value>(&message.payload) {
        duplicate_extensions(&event, &mut headers);
    }
    match jetstream
        .publish_with_headers(outbound::posted_subject(), headers, message.payload.clone())
        .await
    {
        Ok(ack) => {
            if let Err(error) = ack.await {
                warn!(%event_id, %error, "the reach report was not acked");
            }
        }
        Err(error) => warn!(%event_id, %error, "the reach report could not be published"),
    }
}

/// The envelope's filtering extensions as headers on a copy the collector
/// publishes of its own accord, as the Sensor sets them: a consumer of the
/// `.posted` or `.dead` subject filters on them without deserialising.
fn duplicate_extensions(event: &serde_json::Value, headers: &mut async_nats::header::HeaderMap) {
    for extension in ["network", "connection", "consent", "traceparent"] {
        if let Some(value) = event.get(extension).and_then(serde_json::Value::as_str) {
            headers.insert(extension, value);
        }
    }
}
