//! The clerk's three durable consumers and its sweep: what it reads off the
//! bus, what it does about each message, and what it does when the relay or
//! the bus will not answer (ticket #265, ADR 0035).
//!
//! [`run`] is the whole of it: connect to the bus, open the three consumers,
//! start them and the sweep as tasks, say `clerk running`. Each consumer
//! loop is the Sensor's own shape (`sensor/src/main.rs`,
//! `consume_approved_replies`): one incarnation drains the durable pull
//! consumer until its stream ends or errors, and the loop rebuilds it a
//! second later, for as long as the process runs — because a consumer that
//! stops is a channel the owner reads that has silently gone stale.
//!
//! Four rulings decide what happens to one message, and each is a counted
//! outcome rather than a silence. A message the clerk cannot read is
//! `skipped{unreadable}` and acked: it will not read better on a redelivery.
//! A suggestion already past its `expires_at` is `skipped{expired}` and
//! acked. A suggestion the relay already holds a post for — found by reading
//! the reference line off the clerk's own posts, never a store of its own —
//! is `skipped{duplicate}` and acked, which is what makes a redelivery safe.
//! And a relay failure is one of two things: **transient** (unreachable,
//! `429`, `5xx`), which is `Nak`ed with [`nak_delay`] so the bus redelivers
//! it later, or a **refusal** of the request itself, which is logged once
//! with the relay's own body and acked — a post the relay will refuse again
//! is not retried for ever. Both count as a relay failure on `/metrics`.
//!
//! The suggestions consumer starts from the **beginning** of the stream
//! (`DeliverPolicy::All`): a suggestion that expired while the clerk was
//! away is skipped, one still open is posted, and on a warm restart the
//! durable's own ack floor is where it resumes. The other two start from
//! **now** (`DeliverPolicy::New`): a fresh clerk journals from the moment it
//! exists, and does not replay a month of bridge transitions into `activite`.
//!
//! What the sweep does is the other half of ADR 0035: every
//! `Config::sweep`, it reads the clerk's own posts in `approbations`,
//! deletes each one whose reference line says its suggestion has expired,
//! and says so once in `activite` (#219). A relay error there is a warning
//! and a counted failure, never a stop — the next sweep is a minute away.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use async_nats::jetstream::consumer::{pull, AckPolicy, Consumer, DeliverPolicy};
use async_nats::jetstream::{AckKind, Message};
use futures::StreamExt;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::events::{
    self, BridgeStatus, ConsentChange, Suggestion, BRIDGE_STATUS, CONSENT_CHANGED, REPLY_APPROVED,
    SUGGEST_PRODUCED,
};
use crate::metrics::{Channel, Deleted, Metrics, Skipped};
use crate::reference::{self, Reference};
use crate::relay::{Relay, RelayError, KIND_FORUM_POST};
use crate::text::{self, Lang};

/// The durable consumer names, as `nats consumer ls` shows them: one per
/// channel the clerk writes to, named after what it reads.
pub const SUGGESTIONS_CONSUMER: &str = "clerk-suggestions";
pub const JOURNAL_CONSUMER: &str = "clerk-journal";
pub const ACTIVITY_CONSUMER: &str = "clerk-activity";

/// How long the bus waits for an ack before redelivering, and how many
/// deliveries it makes before giving a message up. Sixty seconds covers a
/// slow relay several times over ([`crate::relay::REQUEST_TIMEOUT`] is ten).
/// Twenty deliveries under [`nak_delay`] — 2 + 4 + 8 + 16 + 32 seconds and
/// then fifteen more a minute apart, about sixteen minutes — is how long a
/// relay may be down before the bus stops asking, which is inside the hour
/// a suggestion lives by default (`TWALK_SUGGESTION_TTL_SECONDS`): the
/// suggestions consumer reads from the beginning of the stream precisely
/// not to lose a suggestion that is still alive, and one that has expired
/// by the time the relay is back is skipped on its redelivery anyway.
pub const ACK_WAIT: Duration = Duration::from_secs(60);
pub const MAX_DELIVER: i64 = 20;

/// The ceiling of [`nak_delay`].
pub const NAK_DELAY_MAX: Duration = Duration::from_secs(60);

/// How many of its own posts the clerk reads back to find out whether a
/// suggestion is already posted or has expired: the relay's own maximum for
/// one query. More open suggestions than that would be a product with a
/// different problem.
pub const OWN_POSTS_LIMIT: u32 = 1000;

/// One durable pull consumer.
type PullConsumer = Consumer<pull::Config>;

/// How long to wait before rebuilding a failed or ended consumer.
const CONSUMER_RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// How long to wait before asking a bus that did not answer again.
const BUS_RETRY_DELAY: Duration = Duration::from_secs(5);

/// Everything a consumer needs, built once by the binary and shared by
/// every task: the configuration, the relay, the counters and the language
/// the clerk writes in.
pub struct Clerk {
    pub config: Config,
    pub relay: Relay,
    pub metrics: Arc<Metrics>,
    pub lang: Lang,
}

/// Whether the relay already holds a post for the suggestion `id`: any of
/// `posts` whose reference line names it. The reference line and nothing
/// else — a suggestion's body could quote another suggestion's id, and
/// [`reference::parse`] reads the last reference line only.
pub fn already_posted(posts: &[nostr::Event], id: &str) -> bool {
    posts
        .iter()
        .filter_map(|post| reference::parse(&post.content))
        .any(|reference| reference.suggestion_id == id)
}

/// How long to ask the bus to hold a message before redelivering it, by
/// how many times it has been delivered: `2^delivered` seconds, capped at
/// a minute. The first delivery is 1, so the first retry is two seconds
/// away, the fifth thirty-two, and everything after that a minute.
pub fn nak_delay(delivered: i64) -> Duration {
    let seconds = match delivered {
        d if d < 0 => 1,
        d if d >= 6 => NAK_DELAY_MAX.as_secs(),
        d => 1u64 << d,
    };
    Duration::from_secs(seconds).min(NAK_DELAY_MAX)
}

/// Connects to the bus — for as long as it takes, because a clerk that
/// exits when the bus is late is one an operator has to notice and restart —
/// opens the three consumers, then hands each one and the sweep to a task
/// of its own and logs `clerk running`. Meant to be spawned beside the HTTP
/// origin, so `/health` answers while the bus is still being waited for,
/// and so a SIGTERM during that wait still stops the process.
pub async fn run(clerk: Arc<Clerk>) {
    let jetstream = connect(&clerk.config).await;
    let consumers = loop {
        match open_all(&clerk, &jetstream).await {
            Ok(consumers) => break consumers,
            Err(error) => {
                error!(
                    %error,
                    nats_url = %clerk.config.nats_url,
                    stream = %clerk.config.stream,
                    "the clerk's consumers could not be opened; retrying in {}s",
                    BUS_RETRY_DELAY.as_secs()
                );
                tokio::time::sleep(BUS_RETRY_DELAY).await;
            }
        }
    };
    for (which, consumer) in consumers {
        tokio::spawn(consume(
            clerk.clone(),
            jetstream.clone(),
            which,
            Some(consumer),
        ));
    }
    tokio::spawn(sweep(clerk.clone()));
    info!(
        sweep_seconds = clerk.config.sweep.as_secs(),
        "clerk running"
    );
}

/// The bus, once it answers: a connection refused is logged and asked
/// again, and a connection that later drops is the client's own to
/// re-establish.
async fn connect(config: &Config) -> async_nats::jetstream::Context {
    loop {
        match async_nats::connect(&config.nats_url).await {
            Ok(client) => return async_nats::jetstream::new(client),
            Err(error) => {
                error!(
                    %error,
                    nats_url = %config.nats_url,
                    "the bus could not be reached; retrying in {}s",
                    BUS_RETRY_DELAY.as_secs()
                );
                tokio::time::sleep(BUS_RETRY_DELAY).await;
            }
        }
    }
}

/// The stream, then the three consumers, in one go: `clerk running` is
/// said only once all three exist, because the two that deliver from `New`
/// miss whatever was published before they were created.
async fn open_all(
    clerk: &Clerk,
    jetstream: &async_nats::jetstream::Context,
) -> Result<Vec<(Which, PullConsumer)>> {
    jetstream
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: clerk.config.stream.clone(),
            subjects: vec![format!("{}.>", clerk.config.subject_prefix)],
            ..Default::default()
        })
        .await
        .with_context(|| format!("failed to ensure the stream {}", clerk.config.stream))?;
    let mut consumers = Vec::with_capacity(3);
    for which in [Which::Suggestions, Which::Journal, Which::Activity] {
        consumers.push((which, open(clerk, jetstream, which).await?));
    }
    Ok(consumers)
}

/// The three consumers, by what each one reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    /// `persona.suggest.produced` → one forum post in `approbations`.
    Suggestions,
    /// `persona.reply.approved.posted` → one line in `journal`.
    Journal,
    /// `bridge.status.changed` and `consent.state.changed` → `activite`.
    Activity,
}

impl Which {
    fn name(self) -> &'static str {
        match self {
            Self::Suggestions => "suggestions",
            Self::Journal => "journal",
            Self::Activity => "activity",
        }
    }

    fn durable(self) -> &'static str {
        match self {
            Self::Suggestions => SUGGESTIONS_CONSUMER,
            Self::Journal => JOURNAL_CONSUMER,
            Self::Activity => ACTIVITY_CONSUMER,
        }
    }

    /// The durable pull consumer's configuration. `filter_subjects` for the
    /// activity consumer, which reads two subjects, and `filter_subject`
    /// for the other two; the bus refuses both on one consumer.
    fn config(self, config: &Config) -> pull::Config {
        let (filter_subject, filter_subjects, deliver_policy) = match self {
            Self::Suggestions => (
                config.bus_subject(SUGGEST_PRODUCED),
                Vec::new(),
                DeliverPolicy::All,
            ),
            Self::Journal => (posted_subject(config), Vec::new(), DeliverPolicy::New),
            Self::Activity => (
                String::new(),
                vec![
                    config.bus_subject(BRIDGE_STATUS),
                    config.bus_subject(CONSENT_CHANGED),
                ],
                DeliverPolicy::New,
            ),
        };
        pull::Config {
            durable_name: Some(self.durable().to_owned()),
            filter_subject,
            filter_subjects,
            deliver_policy,
            ack_policy: AckPolicy::Explicit,
            ack_wait: ACK_WAIT,
            max_deliver: MAX_DELIVER,
            ..Default::default()
        }
    }
}

/// The subject the Sensor republishes an approved reply on once it has
/// posted it, with the `reach` and `posted-as` headers (#216).
fn posted_subject(config: &Config) -> String {
    format!("{}.posted", config.bus_subject(REPLY_APPROVED))
}

/// One durable consumer, created or found.
async fn open(
    clerk: &Clerk,
    jetstream: &async_nats::jetstream::Context,
    which: Which,
) -> Result<PullConsumer> {
    let stream = jetstream
        .get_stream(&clerk.config.stream)
        .await
        .with_context(|| format!("failed to get the stream {}", clerk.config.stream))?;
    let consumer = stream
        .get_or_create_consumer(which.durable(), which.config(&clerk.config))
        .await
        .with_context(|| format!("failed to ensure the {} consumer", which.name()))?;
    info!(
        consumer = which.durable(),
        stream = %clerk.config.stream,
        "consuming {}",
        which.name()
    );
    Ok(consumer)
}

/// Never returns: drains `first` (the incarnation [`run`] opened before it
/// said `clerk running`), then rebuilds the consumer after every end or
/// failure, a second apart.
async fn consume(
    clerk: Arc<Clerk>,
    jetstream: async_nats::jetstream::Context,
    which: Which,
    first: Option<PullConsumer>,
) {
    let mut consumer = first;
    loop {
        let incarnation = match consumer.take() {
            Some(consumer) => Ok(consumer),
            None => open(&clerk, &jetstream, which).await,
        };
        let outcome = match incarnation {
            Ok(consumer) => drain(&clerk, which, consumer).await,
            Err(error) => Err(error),
        };
        match outcome {
            Ok(()) => error!(
                "the {} consumer's message stream ended; rebuilding it",
                which.name()
            ),
            Err(error) => error!(%error, "the {} consumer failed; rebuilding it", which.name()),
        }
        tokio::time::sleep(CONSUMER_RECONNECT_DELAY).await;
    }
}

/// One incarnation: every message until the stream ends.
async fn drain(clerk: &Clerk, which: Which, consumer: PullConsumer) -> Result<()> {
    let mut messages = consumer
        .messages()
        .await
        .with_context(|| format!("failed to open the {} message stream", which.name()))?;
    while let Some(message) = messages.next().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                warn!(%error, "{} stream error, continuing", which.name());
                continue;
            }
        };
        let delivered = match message.info() {
            Ok(info) => info.delivered,
            Err(error) => {
                // The backoff is driven by the delivered count; without it,
                // assume the first attempt — and say so, so the restart of
                // the schedule is never silent.
                warn!(%error, "no delivery info on a {} message, assuming the first attempt", which.name());
                1
            }
        };
        let result = match which {
            Which::Suggestions => handle_suggestion(clerk, &message).await,
            Which::Journal => handle_posted_report(clerk, &message).await,
            Which::Activity => handle_activity(clerk, &message).await,
        };
        settle(clerk, which, &message, delivered, result).await;
    }
    Ok(())
}

/// Acks or naks one message by what handling it came to. A handler's `Ok`
/// is every outcome that is done with — posted, skipped, or refused and
/// logged — and its `Err` is only a relay failure still to be decided here:
/// transient ones are `Nak`ed with [`nak_delay`], the rest are acked, and
/// both are counted.
async fn settle(
    clerk: &Clerk,
    which: Which,
    message: &Message,
    delivered: i64,
    result: Result<(), RelayError>,
) {
    let subject = message.subject.as_str();
    match result {
        Ok(()) => {
            if let Err(error) = message.ack().await {
                warn!(%error, subject, "ack failed on the {} consumer", which.name());
            }
        }
        Err(error) if error.is_transient() => {
            clerk.metrics.record_relay_failure();
            let delay = nak_delay(delivered);
            if delivered >= MAX_DELIVER {
                // The bus stops redelivering after MAX_DELIVER; the last
                // attempt is named as such, so that a message the bus gives
                // up on is not one that vanished.
                error!(
                    %error,
                    subject,
                    delivered,
                    "the relay could not be written to on the last delivery the bus will make; \
                     this {} message is given up",
                    which.name()
                );
            } else {
                warn!(
                    %error,
                    subject,
                    delivered,
                    retry_in_seconds = delay.as_secs(),
                    "the relay could not be written to; the {} message will be redelivered",
                    which.name()
                );
            }
            if let Err(error) = message.ack_with(AckKind::Nak(Some(delay))).await {
                error!(%error, subject, "nak failed, the message will be redelivered at the ack deadline");
            }
        }
        Err(error) => {
            clerk.metrics.record_relay_failure();
            // Logged once, with the relay's own body (`RelayError`'s Display
            // carries it, cut short), and acked: the same request would be
            // refused the same way, and a post refused five times a minute
            // apart is not a post the owner gets.
            warn!(
                %error,
                subject,
                "the relay refused a {} post; it is not retried",
                which.name()
            );
            if let Err(error) = message.ack().await {
                warn!(%error, subject, "ack failed on the {} consumer", which.name());
            }
        }
    }
}

/// One `persona.suggest.produced` event: a forum post in `approbations`
/// and a line in `activite`, unless it is unreadable, expired or already
/// posted.
async fn handle_suggestion(clerk: &Clerk, message: &Message) -> Result<(), RelayError> {
    let suggestion: Suggestion = match serde_json::from_slice(&message.payload) {
        Ok(suggestion) => suggestion,
        Err(error) => {
            skip(
                clerk,
                Skipped::Unreadable,
                "suggestion",
                &parse_failure(&error),
            );
            return Ok(());
        }
    };
    let id = suggestion.id.as_str();
    if let Some(expires_at) = suggestion.data.expires_at.as_deref() {
        if reference::has_expired(expires_at, now_unix()) {
            skip(
                clerk,
                Skipped::Expired,
                "suggestion",
                &format!("id={id} expires_at={expires_at}"),
            );
            return Ok(());
        }
    }
    let approvals = clerk.config.channel_approvals.as_str();
    let posts = clerk
        .relay
        .own_posts(approvals, KIND_FORUM_POST, OWN_POSTS_LIMIT)
        .await?;
    if already_posted(&posts, id) {
        skip(clerk, Skipped::Duplicate, "suggestion", &format!("id={id}"));
        return Ok(());
    }
    let reference = reference::line(&Reference {
        suggestion_id: suggestion.id.clone(),
        expires_at: suggestion.data.expires_at.clone(),
    });
    let post = text::approval_post(
        clerk.lang,
        &suggestion.data.suggestion.body,
        &suggestion.network,
        suggestion.data.expires_at.as_deref(),
        &reference,
    );
    let published = clerk.relay.forum_post(approvals, &post).await?;
    let total = clerk.metrics.record_post(Channel::Approvals);
    info!(
        id,
        network = %suggestion.network,
        persona = %suggestion.data.persona_id,
        event_id = %published.event_id,
        total,
        "posted a suggestion to approbations"
    );
    // The post is on the relay whatever happens to the activity line: a
    // Nak here would only be redelivered into the duplicate branch above,
    // so the line's failure is counted and logged and the message is done.
    let line = text::activity_suggested(clerk.lang, &suggestion.network);
    activity_line(clerk, &line).await;
    Ok(())
}

/// One `.posted` report: a line in `journal`.
async fn handle_posted_report(clerk: &Clerk, message: &Message) -> Result<(), RelayError> {
    let Some(report) = events::posted_report(&message.payload, message.headers.as_ref()) else {
        skip(
            clerk,
            Skipped::Unreadable,
            "posted report",
            "the payload is not an approved reply, or the reach and posted-as headers are missing",
        );
        return Ok(());
    };
    let line = text::journal_line(
        clerk.lang,
        &report.network,
        &report.reach,
        &report.posted_as,
        &report.approval_id,
        &report.time,
    );
    let published = clerk
        .relay
        .stream_message(&clerk.config.channel_journal, &line)
        .await?;
    let total = clerk.metrics.record_post(Channel::Journal);
    info!(
        approval_id = %report.approval_id,
        network = %report.network,
        reach = %report.reach,
        event_id = %published.event_id,
        total,
        "journalled a posted reply"
    );
    Ok(())
}

/// One bridge transition or consent decision: a line in `activite`, by
/// which subject it arrived on.
async fn handle_activity(clerk: &Clerk, message: &Message) -> Result<(), RelayError> {
    let subject = message.subject.as_str();
    let line = if subject == clerk.config.bus_subject(BRIDGE_STATUS) {
        match serde_json::from_slice::<BridgeStatus>(&message.payload) {
            Ok(status) => {
                text::activity_bridge(clerk.lang, &status.data.bridge_id, &status.data.state)
            }
            Err(error) => {
                skip(
                    clerk,
                    Skipped::Unreadable,
                    "bridge status",
                    &parse_failure(&error),
                );
                return Ok(());
            }
        }
    } else if subject == clerk.config.bus_subject(CONSENT_CHANGED) {
        match serde_json::from_slice::<ConsentChange>(&message.payload) {
            Ok(change) => {
                let networks = change
                    .data
                    .scope
                    .as_ref()
                    .map(|scope| scope.networks.as_slice())
                    .unwrap_or(&[]);
                text::activity_consent(
                    clerk.lang,
                    &change.data.subject.kind,
                    &change.data.new_state,
                    networks,
                )
            }
            Err(error) => {
                skip(
                    clerk,
                    Skipped::Unreadable,
                    "consent change",
                    &parse_failure(&error),
                );
                return Ok(());
            }
        }
    } else {
        skip(
            clerk,
            Skipped::Unreadable,
            "activity",
            &format!("subject {subject} is neither a bridge status nor a consent change"),
        );
        return Ok(());
    };
    let published = clerk
        .relay
        .stream_message(&clerk.config.channel_activity, &line)
        .await?;
    let total = clerk.metrics.record_post(Channel::Activity);
    info!(subject, event_id = %published.event_id, total, "posted to activite");
    Ok(())
}

/// A line in `activite` that follows something already done — a post, a
/// deletion — so its own failure is counted and logged and nothing is
/// retried on its account.
async fn activity_line(clerk: &Clerk, line: &str) {
    match clerk
        .relay
        .stream_message(&clerk.config.channel_activity, line)
        .await
    {
        Ok(_) => {
            clerk.metrics.record_post(Channel::Activity);
        }
        Err(error) => {
            clerk.metrics.record_relay_failure();
            warn!(%error, "the activite line could not be posted");
        }
    }
}

/// One counted, logged skip. `detail` is the clerk's own account of the
/// message — an id, a subject, a parse failure's category and position —
/// and never a contact's words, which the log must not carry either.
fn skip(clerk: &Clerk, why: Skipped, what: &str, detail: &str) {
    let total = clerk.metrics.record_skipped(why);
    warn!(
        why = why.as_str(),
        detail,
        total,
        "skipped a {what} that was {}",
        why.as_str()
    );
}

/// Never returns: every `Config::sweep`, starting now, one [`sweep_once`].
pub async fn sweep(clerk: Arc<Clerk>) {
    let mut ticker = tokio::time::interval(clerk.config.sweep);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        sweep_once(&clerk).await;
    }
}

/// Reads the clerk's own posts in `approbations` and deletes each one whose
/// reference line says its suggestion has expired, saying so in `activite`
/// (#219). A relay error is a warning and a counted failure, never a stop.
pub async fn sweep_once(clerk: &Clerk) {
    let now = now_unix();
    let approvals = clerk.config.channel_approvals.as_str();
    let posts = match clerk
        .relay
        .own_posts(approvals, KIND_FORUM_POST, OWN_POSTS_LIMIT)
        .await
    {
        Ok(posts) => posts,
        Err(error) => {
            clerk.metrics.record_relay_failure();
            warn!(
                %error,
                next_in_seconds = clerk.config.sweep.as_secs(),
                "the sweep could not read the clerk's own posts"
            );
            return;
        }
    };
    let mut deleted = 0u64;
    for post in &posts {
        let Some(reference) = reference::parse(&post.content) else {
            continue;
        };
        let Some(expires_at) = reference.expires_at.as_deref() else {
            continue;
        };
        if !reference::has_expired(expires_at, now) {
            continue;
        }
        let event_id = post.id.to_hex();
        match clerk.relay.delete(approvals, &event_id).await {
            Ok(_) => {
                deleted += 1;
                let total = clerk.metrics.record_deleted(Deleted::Expired);
                info!(
                    suggestion_id = %reference.suggestion_id,
                    event_id = %event_id,
                    expires_at,
                    total,
                    "deleted an expired suggestion's post"
                );
                activity_line(clerk, &text::activity_expired(clerk.lang)).await;
            }
            Err(error) => {
                clerk.metrics.record_relay_failure();
                warn!(
                    %error,
                    suggestion_id = %reference.suggestion_id,
                    event_id = %event_id,
                    "an expired suggestion's post could not be deleted; the next sweep will try again"
                );
            }
        }
    }
    clerk.metrics.record_sweep();
    if deleted > 0 {
        info!(read = posts.len(), deleted, "sweep done");
    }
}

/// A parse failure as the log may carry it: serde_json's category and the
/// position in the payload, never its `Display`, which quotes the value it
/// choked on — `invalid type: string "…"` — so a malformed producer would
/// put a contact's words in the clerk's log through it.
fn parse_failure(error: &serde_json::Error) -> String {
    format!(
        "{:?} at line {} column {}",
        error.classify(),
        error.line(),
        error.column()
    )
}

/// Seconds since the epoch, as `reference::has_expired` takes them.
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use nostr::{EventBuilder, Keys, Kind};

    use super::*;

    const ID: &str = "319be8ff15d5dee005c8aa27119b983da8223959987e5dbc639d81e370b5ef9b";
    const OTHER: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn post(content: &str) -> nostr::Event {
        EventBuilder::new(Kind::Custom(KIND_FORUM_POST), content)
            .sign_with_keys(&Keys::generate())
            .unwrap()
    }

    #[test]
    fn already_posted_matches_on_the_reference_line_only() {
        // A post about ID: its reference line names it, whatever the body.
        let about_id = post(&format!(
            "Proposed reply · WhatsApp\n“Sure, 8pm.”\ntwalk:suggestion:{ID} expires 2026-09-17T11:00:00Z"
        ));
        // A post about OTHER whose *body* quotes ID: not a post about ID.
        let quoting_id = post(&format!(
            "Proposed reply · Signal\n“See twalk:suggestion:{ID} in your mail”\ntwalk:suggestion:{OTHER}"
        ));
        // A post with no reference line at all.
        let prose = post("Just prose, nothing the clerk wrote.");

        assert!(already_posted(std::slice::from_ref(&about_id), ID));
        assert!(already_posted(
            &[prose.clone(), quoting_id.clone(), about_id],
            ID
        ));
        assert!(!already_posted(&[quoting_id.clone(), prose.clone()], ID));
        assert!(already_posted(&[quoting_id], OTHER));
        assert!(!already_posted(&[prose], ID));
        assert!(!already_posted(&[], ID));
    }

    #[test]
    fn nak_delay_doubles_and_caps_at_a_minute() {
        assert_eq!(nak_delay(1), Duration::from_secs(2));
        assert_eq!(nak_delay(2), Duration::from_secs(4));
        assert_eq!(nak_delay(3), Duration::from_secs(8));
        assert_eq!(nak_delay(4), Duration::from_secs(16));
        assert_eq!(nak_delay(5), Duration::from_secs(32));
        assert_eq!(nak_delay(6), Duration::from_secs(60));
        assert_eq!(nak_delay(7), Duration::from_secs(60));
        assert_eq!(nak_delay(1_000), Duration::from_secs(60));
        assert_eq!(nak_delay(i64::MAX), Duration::from_secs(60));
        // A delivered count the bus never sends, but the schedule must not
        // panic on one.
        assert_eq!(nak_delay(0), Duration::from_secs(1));
        assert_eq!(nak_delay(-1), Duration::from_secs(1));
    }

    #[test]
    fn a_parse_failure_is_logged_without_the_value_it_choked_on() {
        // A producer that put a string where the clerk expects an object:
        // serde_json's own message would quote that string in full.
        let marker = "MARKER-a-contacts-words-9f3c";
        let payload = format!(r#"{{"id":"x","time":"t","network":"whatsapp","data":"{marker}"}}"#);
        let error = serde_json::from_str::<Suggestion>(&payload).unwrap_err();
        assert!(error.to_string().contains(marker), "{error}");

        let detail = parse_failure(&error);
        assert!(!detail.contains(marker), "{detail}");
        assert!(detail.starts_with("Data at line 1 column"), "{detail}");
    }
}
