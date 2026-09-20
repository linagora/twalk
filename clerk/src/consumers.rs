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
//! acked. A message the relay already holds the clerk's answer to is
//! `skipped{duplicate}` and acked, which is what makes a redelivery safe on
//! every channel and never a store of the clerk's own: a suggestion's post
//! is found by reading the reference line off the clerk's own forum posts,
//! and a journal or activity line by the `r` tag every stream message
//! carries, `twalk:event:<bus event id>` — so a SIGTERM between the relay's
//! `2xx` and the ack, a lost ack or an `ACK_WAIT` overrun redelivers a
//! message whose line is then found rather than written twice. (Dating the
//! line from the bus event's `time` so that a redelivery hashed to the same
//! Nostr id was the simpler mechanism and does not work: the relay refuses
//! a `created_at` more than fifteen minutes from its clock, and a
//! redelivery after a relay outage is precisely that old.) And a relay
//! failure is one of two things: **transient** (unreachable, `429`, `5xx`),
//! which is `Nak`ed with [`nak_delay`] so the bus redelivers it later, or a
//! **refusal** of the request itself, which is logged once with the relay's
//! own reason and acked — a post the relay will refuse again is not retried
//! for ever. Both count as a relay failure on `/metrics`.
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
//! and says so once in `activite` (#219). A post it **cannot date** — no
//! `expires_at` it can read, or no reference line it recognises — stands
//! until the relay's own `created_at` on it is seven days old
//! (`reference::UNDATABLE_CEILING`, ADR 0028), then goes as
//! `deleted{undatable}`, and a sweep whose count of them changed says how many at
//! `warn`, so a channel quietly accumulating posts nobody can date is
//! visible before the week is out ([`verdict`]). A relay error there is a
//! warning and a counted failure, never a stop — the next sweep is a
//! minute away.
//!
//! The fourth loop is the write half (#284, [`decisions`]): every
//! `WriteHalf::decision`, it reads the clerk's own open posts in
//! `approbations`, the gestures on them and its own thread answers, and
//! carries the owner's oldest unanswered gesture on each — a ✅ or an
//! edited reply to the Companion Gateway as a device of the owner, a ❌ to
//! a local deletion — and answers every other gesture in the thread, once,
//! keyed on the gesture's id in the answer's `r` tag so that the relay
//! remembers what was answered and this process need not (ADR 0035). What
//! it does with each answer of the Gateway is [`carry`]'s table, and the
//! rulings in it are the ones to argue with: a refusal the Companion would
//! tell the user to retry is **not** answered in the thread but tried again
//! next tick, because an answer spends the gesture and the owner's only
//! way to "try again" would be a second reaction; a Gateway that gave no
//! usable answer is tried again too, until the suggestion is a tick and a
//! Gateway timeout from its expiry ([`not_recorded_window`]), when the
//! thread is told "not recorded" and the post is left
//! to the sweep, so that a ✅ during an outage does not simply vanish with
//! the post; and `approval_published_but_not_recorded` is an approval that
//! went out ([`crate::refusals::sent`]), so the post goes like any other.
//! The loop also refreshes the session **at startup** — a dead one is a
//! startup `ERROR` naming `provision-clerk-device.sh`, not a surprise in a
//! thread a month later — and once a day after that, because the Gateway's
//! refresh token dies after thirty days unused and a deployment whose owner
//! approves from the Companion for a month would otherwise lose its Buzz
//! device without having done anything. A dead session is the one thing
//! the loop remembers between ticks ([`Session`]): it makes no Gateway
//! call while it lasts, tells a ✅ so once without spending it, and carries
//! it when a later refresh succeeds.
//!
//! The write half also gives the suggestions consumer one read (#300,
//! [`read_before_post`]): `GET /api/suggestions/{id}` as the device, once
//! per suggestion at posting time and never per tick, so that the post
//! says whether an approved reply could reach the contact — the
//! Companion's own sentence, so the two doors show one vocabulary — and
//! a suggestion the Gateway already records as approved is not posted at
//! all (`skipped{already_approved}`). The read never blocks the post: a
//! Gateway that does not answer, refuses, or does not hold the suggestion
//! yields the "not read" line naming why, in the same tick. The session
//! cell is shared with the loop ([`Clerk::session`]) so that a `401` met
//! on either side stops the calls on both.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use async_nats::jetstream::consumer::{pull, AckPolicy, Consumer, DeliverPolicy};
use async_nats::jetstream::{AckKind, Message};
use futures::StreamExt;
use tracing::{debug, error, info, warn};

use crate::config::{Config, WriteHalf};
use crate::decision::{self, Decision, Gesture};
use crate::events::{
    self, BridgeStatus, ConsentChange, Suggestion, BRIDGE_STATUS, CONSENT_CHANGED, REPLY_APPROVED,
    SUGGEST_PRODUCED,
};
use crate::gateway::{Gateway, GatewayError, Outcome, Read};
use crate::metrics::{ApprovalOutcome, Channel, Deleted, Metrics, Skipped};
use crate::reference::{self, Reference};
use crate::refusals::{self, Remedy};
use crate::relay::{answered_gesture, Relay, RelayError, KIND_FORUM_POST};
use crate::text::{self, Lang};

/// The durable consumer names, as `nats consumer ls` shows them: one per
/// channel the clerk writes to, named after what it reads.
pub const SUGGESTIONS_CONSUMER: &str = "clerk-suggestions";
pub const JOURNAL_CONSUMER: &str = "clerk-journal";
pub const ACTIVITY_CONSUMER: &str = "clerk-activity";

/// How long the bus waits for an ack before redelivering, and how many
/// deliveries it makes before giving a message up. Sixty seconds covers a
/// slow relay several times over ([`crate::relay::REQUEST_TIMEOUT`] is ten).
/// Sixty-four deliveries is sized to **the hour a suggestion lives by
/// default** (`TWALK_SUGGESTION_TTL_SECONDS`): under [`nak_delay`] the
/// first five retries are 2, 4, 8, 16 and 32 seconds apart and every one
/// after that a minute, so the sixty-three naks sum to 3542 seconds and
/// the sixty-fourth delivery comes fifty-nine minutes after the first —
/// the last one that can still find the suggestion alive, since a
/// sixty-fifth would land past its expiry and be skipped as expired. A
/// relay outage shorter than a suggestion's life therefore loses nothing,
/// which is what the ticket's "retries with backoff" means. The same
/// sizing bounds what a poisoned message can cost: at most that hour of
/// naks, because a suggestion past its `expires_at` is skipped and acked
/// on the delivery that finds it so, and a message the bus has
/// redelivered sixty-four times is announced as given up rather than
/// vanishing.
pub const ACK_WAIT: Duration = Duration::from_secs(60);
pub const MAX_DELIVER: i64 = 64;

/// The ceiling of [`nak_delay`].
pub const NAK_DELAY_MAX: Duration = Duration::from_secs(60);

/// How many of its own posts the clerk reads back to find out whether a
/// suggestion is already posted or has expired, and how many of its own
/// lines it reads back to find out whether a bus event is already
/// journalled: the relay's own maximum for one query. More open
/// suggestions than that would be a product with a different problem, and
/// a redelivery is at most about an hour old ([`MAX_DELIVER`]), so a
/// channel with a thousand newer lines would be one too.
pub const OWN_POSTS_LIMIT: u32 = 1000;

/// One durable pull consumer.
type PullConsumer = Consumer<pull::Config>;

/// How long to wait before rebuilding a failed or ended consumer.
const CONSUMER_RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// How long to wait before asking a bus that did not answer again.
const BUS_RETRY_DELAY: Duration = Duration::from_secs(5);

/// How long the decisions loop lets the session it holds on the Companion
/// Gateway go without a refresh of its own: a day, well inside the thirty
/// the Gateway keeps an unused refresh token for, and after one that
/// failed, an hour — often enough that a device signed in again is picked
/// up within the hour without an approval, rarely enough that a revoked
/// one is not a hot loop of refusals.
pub const SESSION_REFRESH_PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
pub const SESSION_REFRESH_RETRY: Duration = Duration::from_secs(60 * 60);

/// Everything a consumer needs, built once by the binary and shared by
/// every task: the configuration, the relay, the counters, the language
/// the clerk writes in, and — when the write half is configured — the
/// Companion Gateway it approves through, and what is known of the
/// session on it.
pub struct Clerk {
    pub config: Config,
    pub relay: Relay,
    pub metrics: Arc<Metrics>,
    pub lang: Lang,
    pub gateway: Option<Gateway>,
    /// The one thing remembered about the session on the Companion
    /// Gateway ([`Session`]), shared by the two tasks that speak to it:
    /// the decisions loop, which refreshes it and carries approvals as
    /// it, and the suggestions consumer, which reads a suggestion's
    /// delivery as it before posting (#300). One cell, because a dead
    /// session found by either must stop the other's calls too — the hot
    /// loop of refusals `gateway.rs` warns against would otherwise simply
    /// move from the tick to the next suggestion.
    pub session: Mutex<Session>,
}

impl Clerk {
    /// What the last Gateway answer said about the session.
    pub fn session(&self) -> Session {
        *self
            .session
            .lock()
            .expect("the session cell is never poisoned")
    }

    /// Records what the last Gateway answer said about the session.
    pub fn set_session(&self, session: Session) {
        *self
            .session
            .lock()
            .expect("the session cell is never poisoned") = session;
    }
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

/// One durable consumer, created or found. Found means **returned as it
/// is**: the bus keeps a durable's configuration from the day it was
/// created, so a change to [`ACK_WAIT`], [`MAX_DELIVER`] or the filter
/// subjects in this binary does not reach a consumer that already exists —
/// `nats consumer rm <stream> <durable>` and a restart is what applies it.
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
            // Logged once, with the relay's own reason (`RelayError`'s
            // Display carries it, cut short, and never a body's text), and
            // acked: the same request would be refused the same way, and a
            // post refused five times a minute apart is not a post the owner
            // gets.
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
/// and a line in `activite`, unless it is unreadable, expired, already
/// posted — or, when the write half is configured, already approved
/// ([`read_before_post`]).
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
    let delivery_line = match read_before_post(clerk, id).await {
        BeforePost::Post(line) => line,
        BeforePost::AlreadyApproved => {
            let total = clerk.metrics.record_skipped(Skipped::AlreadyApproved);
            info!(
                id,
                why = Skipped::AlreadyApproved.as_str(),
                total,
                "a suggestion the Companion Gateway already records as approved is not posted"
            );
            return Ok(());
        }
    };
    let reference = reference::line(&Reference {
        suggestion_id: suggestion.id.clone(),
        expires_at: suggestion.data.expires_at.clone(),
    });
    let post = text::approval_post(
        clerk.lang,
        &suggestion.data.suggestion.body,
        &suggestion.network,
        suggestion.data.expires_at.as_deref(),
        &delivery_line,
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
    activity_line(clerk, &line, id).await;
    Ok(())
}

/// What the read of a suggestion before its post came to: the delivery
/// line the post carries, or the one answer that means there is no post
/// to make.
enum BeforePost {
    /// Post, with this as the third line.
    Post(String),
    /// The Companion Gateway records the suggestion as `approved`: it was
    /// decided from the approval screen before the clerk got to it, and a
    /// post would ask the owner for a decision already made.
    AlreadyApproved,
}

/// One `GET /api/suggestions/{id}` as the owner's device before the post
/// (#300), when the write half is configured — so that the post says
/// whether the reply can reach the contact, in the Companion's own words
/// ([`refusals::delivery_line`]), and a suggestion already approved is not
/// posted at all. Without a device the line says so
/// ([`refusals::Unread::NoDevice`]) and nothing is asked.
///
/// The read is **one per suggestion, at posting time, and never per
/// tick**: a restart between this read and the next tick finds the post
/// by its reference line and reads nothing again, so what the clerk knows
/// of a suggestion is still only what the relay holds (ADR 0035). And it
/// **never blocks the post**: bounded by the Gateway's request timeout,
/// every way it can fail is a `warn` with the id and the status and code
/// — never a body — and the matching "not read" line
/// ([`refusals::delivery_unread_line`]), and the post goes up in the same
/// tick. `404 suggestion_not_found` and `410 suggestion_out_of_reach` are
/// the Gateway not holding the suggestion ([`refusals::Unread::NotFound`]);
/// any other coded refusal, and a session the Gateway will not have, are
/// [`refusals::Unread::GatewayRefused`]; nothing answered, or an answer
/// that is not the route's, is [`refusals::Unread::GatewayUnreachable`].
/// A `401` is the same breaker as the decisions loop's: the session is
/// marked [`Session::Dead`] and no further call is made, for a suggestion
/// or for a ✅, until a refresh of the loop's own brings it back.
async fn read_before_post(clerk: &Clerk, id: &str) -> BeforePost {
    use refusals::Unread;
    let l = clerk.lang;
    let Some(gateway) = clerk.gateway.as_ref() else {
        return BeforePost::Post(refusals::delivery_unread_line(l, Unread::NoDevice));
    };
    if clerk.session() == Session::Dead {
        warn!(
            id,
            gateway = %gateway.base(),
            "the Companion Gateway will not have the clerk's session, so the suggestion's \
             delivery is not read and it is posted as such; run provision-clerk-device.sh"
        );
        return BeforePost::Post(refusals::delivery_unread_line(l, Unread::GatewayRefused));
    }
    let unread = match gateway.suggestion(id).await {
        Ok(Read::Found(read)) if read.standing == "approved" => {
            return BeforePost::AlreadyApproved;
        }
        Ok(Read::Found(read)) => {
            debug!(
                id,
                standing = %read.standing,
                reach = %read.delivery.reach,
                detail = %read.delivery.detail,
                "read the suggestion's delivery from the Companion Gateway"
            );
            return BeforePost::Post(refusals::delivery_line(l, &read.delivery));
        }
        Ok(Read::Refused { status, code }) if matches!(status, 404 | 410) => {
            warn!(
                id,
                status,
                code,
                "the Companion Gateway does not hold the suggestion; posted as not read"
            );
            Unread::NotFound
        }
        Ok(Read::Refused { status, code }) => {
            warn!(
                id,
                status,
                code,
                "the Companion Gateway refused the suggestion read; posted as not read"
            );
            Unread::GatewayRefused
        }
        Err(GatewayError::Unauthenticated) => {
            clerk.set_session(Session::Dead);
            warn!(
                id,
                gateway = %gateway.base(),
                "the Companion Gateway will not have the clerk's session: the Buzz device was \
                 revoked or its refresh token died; the suggestion is posted as not read, and \
                 no further call is made until a refresh succeeds; run provision-clerk-device.sh"
            );
            Unread::GatewayRefused
        }
        Err(error) => {
            warn!(
                %error,
                id,
                gateway = %gateway.base(),
                "the Companion Gateway gave no usable answer to the suggestion read; posted as \
                 not read"
            );
            Unread::GatewayUnreachable
        }
    };
    BeforePost::Post(refusals::delivery_unread_line(l, unread))
}

/// One `.posted` report: a line in `journal`, unless the relay already
/// holds one for this report — found by the `r` tag, so a redelivery of a
/// report already journalled is `skipped{duplicate}` and not a second line.
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
    let journal = clerk.config.channel_journal.as_str();
    if already_lined(clerk, journal, &report.approval_id).await? {
        skip(
            clerk,
            Skipped::Duplicate,
            "posted report",
            &format!("approval_id={}", report.approval_id),
        );
        return Ok(());
    }
    let line = text::journal_line(
        clerk.lang,
        &report.network,
        &report.reach,
        &report.posted_as,
        &report.approval_id,
        &report.time,
        report.edited,
    );
    let published = clerk
        .relay
        .stream_message(journal, &line, &report.approval_id)
        .await?;
    let total = clerk.metrics.record_post(Channel::Journal);
    info!(
        approval_id = %report.approval_id,
        network = %report.network,
        reach = %report.reach,
        edited = report.edited,
        event_id = %published.event_id,
        total,
        "journalled a posted reply"
    );
    Ok(())
}

/// One bridge transition or consent decision: a line in `activite`, by
/// which subject it arrived on — unless the relay already holds this
/// event's line, found by the `r` tag, in which case `skipped{duplicate}`.
async fn handle_activity(clerk: &Clerk, message: &Message) -> Result<(), RelayError> {
    let subject = message.subject.as_str();
    let (id, line) = if subject == clerk.config.bus_subject(BRIDGE_STATUS) {
        match serde_json::from_slice::<BridgeStatus>(&message.payload) {
            Ok(status) => (
                status.id,
                text::activity_bridge(clerk.lang, &status.data.bridge_id, &status.data.state),
            ),
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
                let line = text::activity_consent(
                    clerk.lang,
                    &change.data.subject.kind,
                    &change.data.new_state,
                    networks,
                );
                (change.id, line)
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
    let activity = clerk.config.channel_activity.as_str();
    if already_lined(clerk, activity, &id).await? {
        skip(clerk, Skipped::Duplicate, "activity", &format!("id={id}"));
        return Ok(());
    }
    let published = clerk.relay.stream_message(activity, &line, &id).await?;
    let total = clerk.metrics.record_post(Channel::Activity);
    info!(subject, id, event_id = %published.event_id, total, "posted to activite");
    Ok(())
}

/// Whether the relay already holds a line of the clerk's in `channel` for
/// the bus event `bus_event_id`: the relay as the clerk's memory for a
/// stream message, the way [`already_posted`] is for a forum post.
async fn already_lined(
    clerk: &Clerk,
    channel: &str,
    bus_event_id: &str,
) -> Result<bool, RelayError> {
    let lines = clerk
        .relay
        .own_lines_about(channel, bus_event_id, OWN_POSTS_LIMIT)
        .await?;
    Ok(!lines.is_empty())
}

/// A line in `activite` that follows something already done — a post, a
/// deletion — so its own failure is counted and logged and nothing is
/// retried on its account. `bus_event_id` is the suggestion the line is
/// about, which is what its `r` tag names — and the "produced" and the
/// "expired" lines of one suggestion therefore share it, so nothing here
/// may ask [`already_lined`] with a suggestion's id on `activite`: these
/// two lines are idempotent through the forum post they follow (its
/// reference line, and its deletion), not through their own tag.
async fn activity_line(clerk: &Clerk, line: &str, bus_event_id: &str) {
    match clerk
        .relay
        .stream_message(&clerk.config.channel_activity, line, bus_event_id)
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
///
/// An expired suggestion is `info`, not `warn`: the suggestions consumer
/// reads from the beginning of the stream on purpose, so a first start
/// against a real stream skips every suggestion that expired in ninety days
/// of history, and that expected replay must not read as ninety days of
/// warnings. The others are `warn`, because an unreadable message is a
/// producer to look at and a duplicate is a redelivery to know about.
fn skip(clerk: &Clerk, why: Skipped, what: &str, detail: &str) {
    let total = clerk.metrics.record_skipped(why);
    match why {
        Skipped::Expired | Skipped::AlreadyApproved => info!(
            why = why.as_str(),
            detail,
            total,
            "skipped a {what} that was {}",
            why.as_str()
        ),
        Skipped::Unreadable | Skipped::Duplicate | Skipped::Unverified => warn!(
            why = why.as_str(),
            detail,
            total,
            "skipped a {what} that was {}",
            why.as_str()
        ),
    }
}

/// Never returns: every `Config::sweep`, starting now, one [`sweep_once`].
pub async fn sweep(clerk: Arc<Clerk>) {
    let mut ticker = tokio::time::interval(clerk.config.sweep);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut undatable_last_time = 0u64;
    loop {
        ticker.tick().await;
        let undatable = sweep_once(&clerk).await;
        // Once per change rather than once a minute for seven days: the
        // `/metrics` row is the durable signal, the line is the alert.
        if undatable != undatable_last_time && undatable > 0 {
            warn!(
                undatable,
                ceiling_days = reference::UNDATABLE_CEILING.as_secs() / (24 * 60 * 60),
                "the sweep read posts it could not date from their reference line; each is deleted \
                 once the relay's own created_at on it is past the ceiling"
            );
        }
        undatable_last_time = undatable;
    }
}

/// What the sweep decides about one of the clerk's own posts in
/// `approbations` ([`verdict`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Dated by its reference line, and not yet expired.
    Stands,
    /// Its reference line has no expiry the sweep can read — or there is
    /// no reference line it recognises — and the relay's own `created_at`
    /// is under the ceiling: it stands, and is counted as undatable.
    StandsUndated,
    /// Its suggestion has expired: deleted as `expired`.
    Expired,
    /// Undatable and seven days old on the relay's own `created_at`
    /// (`reference::UNDATABLE_CEILING`): deleted as `undatable`.
    PastCeiling,
}

/// The sweep's decision about one post: its content (the reference line is
/// read off it), the `created_at` the relay stamped on it, and the time.
/// Pure, so that the ceiling is unit-tested here rather than waited seven
/// days for.
pub fn verdict(content: &str, created_at_unix: i64, now_unix: i64) -> Verdict {
    let expires_at = reference::parse(content)
        .and_then(|reference| reference.expires_at)
        .and_then(|expires_at| reference::expires_at_unix(&expires_at));
    match expires_at {
        Some(expires_at) if expires_at <= now_unix => Verdict::Expired,
        Some(_) => Verdict::Stands,
        None if reference::past_ceiling(created_at_unix, now_unix) => Verdict::PastCeiling,
        None => Verdict::StandsUndated,
    }
}

/// Reads the clerk's own posts in `approbations` and deletes each one whose
/// reference line says its suggestion has expired, saying so in `activite`
/// (#219) — and each one it cannot date once it is seven days old, counted
/// apart. A relay error is a warning and a counted failure, never a stop.
/// Answers how many posts it read and could not date, for [`sweep`] to say.
pub async fn sweep_once(clerk: &Clerk) -> u64 {
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
            return 0;
        }
    };
    let mut deleted = 0u64;
    let mut undatable = 0u64;
    for post in &posts {
        let why = match verdict(&post.content, post.created_at.as_secs() as i64, now) {
            Verdict::Stands => continue,
            Verdict::StandsUndated => {
                undatable += 1;
                continue;
            }
            Verdict::Expired => Deleted::Expired,
            Verdict::PastCeiling => {
                undatable += 1;
                Deleted::Undatable
            }
        };
        let event_id = post.id.to_hex();
        // The suggestion the post is about, for the log line and the
        // activity line's tag; a post with no reference line the sweep
        // recognises has none, and is named by its event id alone.
        let suggestion_id = reference::parse(&post.content)
            .map(|reference| reference.suggestion_id)
            .unwrap_or_default();
        match clerk.relay.delete(approvals, &event_id).await {
            Ok(_) => {
                deleted += 1;
                let total = clerk.metrics.record_deleted(why);
                info!(
                    why = why.as_str(),
                    suggestion_id,
                    event_id = %event_id,
                    created_at = post.created_at.as_secs(),
                    total,
                    "deleted a suggestion's post"
                );
                let about = if suggestion_id.is_empty() {
                    event_id.as_str()
                } else {
                    suggestion_id.as_str()
                };
                activity_line(clerk, &text::activity_expired(clerk.lang), about).await;
            }
            Err(error) => {
                clerk.metrics.record_relay_failure();
                warn!(
                    %error,
                    why = why.as_str(),
                    suggestion_id,
                    event_id = %event_id,
                    "a suggestion's post could not be deleted; the next sweep will try again"
                );
            }
        }
    }
    clerk.metrics.record_sweep();
    if deleted > 0 {
        info!(read = posts.len(), deleted, undatable, "sweep done");
    }
    undatable
}

/// What one tick of the decisions loop did, for its log line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DecisionsTick {
    /// The clerk's own posts in `approbations` still open (not expired by
    /// their reference line).
    pub posts: usize,
    /// The gestures read on them, by anyone but the clerk.
    pub gestures: usize,
    /// Approvals the Companion Gateway accepted (or had already recorded,
    /// or published without recording): the post deleted.
    pub approved: u64,
    /// Posts the owner refused with ❌: deleted, nobody else told.
    pub refused_locally: u64,
    /// Thread answers written: to a stranger, to a refusal, to a revoked
    /// device, to a gesture that could not be carried before the expiry.
    pub answered: u64,
    /// Owner's gestures the Companion Gateway gave no usable answer to this tick,
    /// carried again next tick.
    pub deferred: u64,
    /// Relay calls that failed; each is also a counted relay failure.
    pub failures: u64,
}

impl DecisionsTick {
    /// Whether the tick is worth an `info` line rather than a `debug` one:
    /// a tick that read posts and did nothing about them is the ordinary
    /// case, five seconds apart for ever.
    pub fn happened(&self) -> bool {
        self.approved + self.refused_locally + self.answered + self.deferred + self.failures > 0
    }
}

/// What the clerk knows about its session on the Companion Gateway
/// between ticks ([`Clerk::session`]). **Dead** is the one state that must
/// be remembered: the Gateway answered `401` to a refresh, or to a request
/// made with a token a refresh had just issued, so the `Buzz` device was
/// revoked or its refresh token died, and the way out is an operator
/// signing it in again — asking again every five seconds, or on every
/// suggestion, would be the hot loop of refusals `gateway.rs` warns
/// against, so a dead session makes no Gateway call at all until a refresh
/// of the decisions loop's own succeeds (the hourly retry, or the next
/// start). An owner's ✅ seen meanwhile is told so in the thread, once,
/// and **not spent** by it: the sentence names the script, and the ✅ is
/// carried the moment the session is back, because the owner decided and
/// the impediment was the clerk's. A suggestion posted meanwhile carries
/// the "not read" line ([`read_before_post`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    /// The last refresh succeeded, or none has failed with `401` yet.
    Alive,
    /// The Companion Gateway will not have this session until it is provisioned again.
    Dead,
}

/// Never returns: refreshes the session, then every `WriteHalf::decision`
/// one [`decisions_once`], and once a day the session again (an hour
/// after one that failed). Returns at once, saying so, when the write half
/// is not configured — the binary does not spawn it then, and this is the
/// guard against a caller that does.
pub async fn decisions(clerk: Arc<Clerk>) {
    let (Some(write), Some(gateway)) = (clerk.config.write_half(), clerk.gateway.as_ref()) else {
        debug!("the write half is not configured; the decisions loop is not started");
        return;
    };
    // At startup, so a session that died while the clerk was away is an
    // ERROR now, naming the script, and not a surprise in a thread on the
    // first ✅ a month later.
    let mut next_refresh = Instant::now() + refresh_session(&clerk, gateway).await;
    info!(
        every_seconds = write.decision.as_secs(),
        gateway = %gateway.base(),
        owner = %write.owner_pubkey,
        session = ?clerk.session(),
        "decisions loop running"
    );
    let mut ticker = tokio::time::interval(write.decision);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if Instant::now() >= next_refresh {
            next_refresh = Instant::now() + refresh_session(&clerk, gateway).await;
        }
        let tick = decisions_once(&clerk).await;
        if tick.happened() {
            info!(
                posts = tick.posts,
                gestures = tick.gestures,
                approved = tick.approved,
                refused_locally = tick.refused_locally,
                answered = tick.answered,
                deferred = tick.deferred,
                failures = tick.failures,
                "decisions tick"
            );
        } else {
            debug!(
                posts = tick.posts,
                gestures = tick.gestures,
                "decisions tick"
            );
        }
    }
}

/// One refresh of the session on the Companion Gateway, and how long to
/// wait before the next one of the loop's own: a day after one that
/// succeeded, an hour after one that did not. A `401` is the session dead
/// — an `error` naming the script that issues a new one, and no approval
/// call until a later refresh succeeds; a Gateway that gave no usable
/// answer leaves the session as it was and is a warning, because the next
/// approval refreshes again anyway ([`Gateway::approve`]) and the loop
/// must start without it.
async fn refresh_session(clerk: &Clerk, gateway: &Gateway) -> Duration {
    match gateway.refresh().await {
        Ok(()) => {
            if clerk.session() == Session::Dead {
                info!(
                    gateway = %gateway.base(),
                    "the clerk's session is back: the Buzz device was signed in again"
                );
            }
            clerk.set_session(Session::Alive);
            SESSION_REFRESH_PERIOD
        }
        Err(GatewayError::Unauthenticated) => {
            clerk.set_session(Session::Dead);
            error!(
                gateway = %gateway.base(),
                "the Companion Gateway will not have the clerk's session: the Buzz device was \
                 revoked or its refresh token died; a ✅ on Buzz is told so in its thread and \
                 carried once an operator runs provision-clerk-device.sh (tried again in {}s)",
                SESSION_REFRESH_RETRY.as_secs()
            );
            SESSION_REFRESH_RETRY
        }
        Err(error) if error.is_transient() => {
            warn!(
                %error,
                gateway = %gateway.base(),
                "the clerk's session could not be refreshed; the loop starts anyway and tries \
                 again in {}s and on the next approval",
                SESSION_REFRESH_RETRY.as_secs()
            );
            SESSION_REFRESH_RETRY
        }
        Err(error) => {
            error!(
                %error,
                gateway = %gateway.base(),
                "the clerk's session could not be refreshed; the loop starts anyway and tries \
                 again in {}s and on the next approval",
                SESSION_REFRESH_RETRY.as_secs()
            );
            SESSION_REFRESH_RETRY
        }
    }
}

/// One tick: the open posts, the gestures on them, the answers already
/// given, and for each post the strangers answered and the owner's oldest
/// unanswered gesture carried. Every relay call that fails is a warning
/// and a counted failure, and the tick moves on; nothing stops the loop.
/// Does nothing when the write half is not configured.
pub async fn decisions_once(clerk: &Clerk) -> DecisionsTick {
    let mut tick = DecisionsTick::default();
    let (Some(write), Some(gateway)) = (clerk.config.write_half(), clerk.gateway.as_ref()) else {
        return tick;
    };
    let now = now_unix();
    let approvals = clerk.config.channel_approvals.as_str();

    // (1) The clerk's own posts. An expired one is the sweep's to delete —
    // the Companion Gateway would refuse its approval as
    // `suggestion_expired` — but the owner may have decided on it in its
    // last seconds, and that gesture is answered rather than swept in
    // silence, so expired posts are read for gestures too.
    let posts = match clerk
        .relay
        .own_posts(approvals, KIND_FORUM_POST, OWN_POSTS_LIMIT)
        .await
    {
        Ok(posts) => posts,
        Err(error) => {
            relay_failed(clerk, &mut tick, &error, "read the clerk's own posts");
            return tick;
        }
    };
    let (expired, open): (Vec<&nostr::Event>, Vec<&nostr::Event>) = posts
        .iter()
        .partition(|post| post_has_expired(&post.content, now));
    tick.posts = open.len();
    if open.is_empty() && expired.is_empty() {
        return tick;
    }
    let ids: Vec<String> = open
        .iter()
        .chain(expired.iter())
        .map(|post| post.id.to_hex())
        .collect();

    // (2) The gestures on them, by anyone but the clerk: its own thread
    // answers are direct replies with text, and read as gestures they would
    // be a stranger's edited approval that the clerk answers, for ever.
    let own_pubkey = clerk.relay.public_key_hex();
    let gestures: Vec<nostr::Event> = match clerk.relay.gestures_on(&ids, OWN_POSTS_LIMIT).await {
        Ok(gestures) => gestures
            .into_iter()
            .filter(|gesture| gesture.pubkey.to_hex() != own_pubkey)
            .collect(),
        Err(error) => {
            relay_failed(clerk, &mut tick, &error, "read the gestures on its posts");
            return tick;
        }
    };
    tick.gestures = gestures.len();
    if gestures.is_empty() {
        return tick;
    }
    // …and the gestures the clerk already answered, by the `r` tag on its
    // own replies: the relay as its memory (ADR 0035). Two sets, because
    // one answer does not spend the gesture ([`Session`]).
    let answers = match clerk.relay.own_comments_on(&ids, OWN_POSTS_LIMIT).await {
        Ok(answers) => answers,
        Err(error) => {
            relay_failed(clerk, &mut tick, &error, "read its own thread answers");
            return tick;
        }
    };
    let (answered, told_revoked) = answered_gestures(&answers);

    // (3) Per post: what was decided, minus what was already answered, and
    // then the strangers answered and the owner's oldest carried.
    for post in open {
        let post_id = post.id.to_hex();
        let decisions: Vec<Decision> =
            decision::decisions_on(&post_id, &write.owner_pubkey, &gestures)
                .into_iter()
                .filter(|decision| !answered.contains(&decision.gesture_id))
                .collect();
        let triage = decision::triage(decisions);
        for stranger in &triage.strangers {
            answer_stranger(clerk, &mut tick, &post_id, stranger).await;
        }
        if let Some(act) = triage.act {
            let told = told_revoked.contains(&act.gesture_id);
            carry(clerk, gateway, write, &mut tick, post, act, told).await;
        }
    }
    // (4) The expired posts: a stranger is answered as anywhere, and an
    // owner's ✅ or edited reply that arrived too late is told so, once —
    // the Companion's own sentence for `suggestion_expired`, counted under
    // that code as if the Gateway had said it, which it would have. A ❌
    // on an expired post asks for nothing: the sweep deletes it either way.
    for post in expired {
        let post_id = post.id.to_hex();
        let decisions: Vec<Decision> =
            decision::decisions_on(&post_id, &write.owner_pubkey, &gestures)
                .into_iter()
                .filter(|decision| !answered.contains(&decision.gesture_id))
                .collect();
        let triage = decision::triage(decisions);
        for stranger in &triage.strangers {
            answer_stranger(clerk, &mut tick, &post_id, stranger).await;
        }
        if let Some(act) = triage.act {
            if act.gesture == Gesture::Refuse {
                continue;
            }
            // Counted when the answer is written, as a stranger's is: the
            // count is "answers written", and a relay that refused the
            // comment sees the same gesture answered next tick.
            if answer(
                clerk,
                &mut tick,
                &post_id,
                &act.gesture_id,
                &text::thread_expired(clerk.lang),
            )
            .await
            {
                const CODE: &str = "suggestion_expired";
                let total = clerk
                    .metrics
                    .record_approval(&ApprovalOutcome::Refused(CODE.to_owned()));
                info!(
                    post_id,
                    gesture_id = %act.gesture_id,
                    total,
                    "the owner decided on a suggestion after it expired; answered in the thread"
                );
            }
        }
    }
    tick
}

/// The gestures the clerk's own thread replies answer, in two sets: those
/// **spent** by their answer — a stranger told, a refusal explained, a
/// "not recorded" before an expiry — and those told that the session was
/// revoked ([`text::thread_revoked`], in either language the clerk writes,
/// since the operator may have changed it between runs), which are not
/// spent: the ✅ is carried once the device is signed in again.
pub fn answered_gestures(answers: &[nostr::Event]) -> (HashSet<String>, HashSet<String>) {
    let revoked_sentences = [
        text::thread_revoked(Lang::Fr),
        text::thread_revoked(Lang::En),
    ];
    let mut spent = HashSet::new();
    let mut told_revoked = HashSet::new();
    for answer in answers {
        let Some(gesture_id) = answered_gesture(answer) else {
            continue;
        };
        if revoked_sentences.contains(&answer.content) {
            told_revoked.insert(gesture_id.to_owned());
        } else {
            spent.insert(gesture_id.to_owned());
        }
    }
    (spent, told_revoked)
}

/// Whether one of the clerk's own posts says, on its reference line, that
/// its suggestion has expired at `now_unix`. A post with no expiry it can
/// read is open: the sweep's ceiling is what ends it, and until then the
/// owner may still decide on it.
pub fn post_has_expired(content: &str, now_unix: i64) -> bool {
    reference::parse(content)
        .and_then(|reference| reference.expires_at)
        .is_some_and(|expires_at| reference::has_expired(&expires_at, now_unix))
}

/// How close to its expiry a post must be for an owner's gesture the
/// Companion Gateway has not answered to be told "not recorded" now rather
/// than tried again next tick: one `decision` interval **plus one Gateway
/// request timeout** ([`crate::gateway::REQUEST_TIMEOUT`]). The next tick
/// is `decision` away only when this one ends on time, and a Gateway that
/// hangs rather than refuses holds a tick for the whole timeout — ten
/// seconds, twice the default interval — so a post with seven seconds to
/// live would be "not within a tick" now, expired by the next tick, and
/// deleted by the sweep with no line in its thread: the silence this line
/// exists to prevent. Over-approximating costs a "not recorded" written
/// one tick early on a post that would have expired anyway.
pub fn not_recorded_window(decision: Duration) -> Duration {
    decision + crate::gateway::REQUEST_TIMEOUT
}

/// Whether a post's suggestion expires within `window` of `now_unix` — at
/// the last tick, an owner's gesture the Companion Gateway has not
/// answered can still be told so in the thread before the sweep deletes
/// the post. A post with no expiry it can read never does: it stands
/// until the sweep's ceiling, and the gesture is simply tried again.
pub fn expires_within(content: &str, now_unix: i64, window: Duration) -> bool {
    reference::parse(content)
        .and_then(|reference| reference.expires_at)
        .and_then(|expires_at| reference::expires_at_unix(&expires_at))
        .is_some_and(|expires_at| expires_at.saturating_sub(now_unix) <= window.as_secs() as i64)
}

/// A gesture by a key that is not the owner's: answered in the thread, once
/// — the next tick finds the answer by the gesture's id — and counted. The
/// relay let a member react and the clerk did nothing with it, and a
/// member who ticked a post and saw nothing happen would conclude the
/// clerk is broken. The log names the key by its first eight characters:
/// enough to recognise, and a public key is not a contact.
async fn answer_stranger(
    clerk: &Clerk,
    tick: &mut DecisionsTick,
    post_id: &str,
    stranger: &Decision,
) {
    let by = stranger.by.get(..8).unwrap_or(&stranger.by);
    warn!(
        post_id,
        gesture_id = %stranger.gesture_id,
        by,
        "a gesture by a key that is not the owner's"
    );
    if answer(
        clerk,
        tick,
        post_id,
        &stranger.gesture_id,
        &text::thread_not_the_owner(clerk.lang),
    )
    .await
    {
        clerk.metrics.record_approval(&ApprovalOutcome::NotTheOwner);
    }
}

/// The owner's oldest unanswered gesture on one post, carried: a ❌ is the
/// post deleted and a line in `activite`; a ✅ or an edited reply is
/// `POST /api/approvals` as the owner's device, and then, by what the
/// Companion Gateway answered:
///
/// - accepted, or published-but-not-recorded ([`refusals::sent`]): the
///   reply went out — the post deleted, a line in `activite` saying edited
///   or not, the outcome counted (the last under its own code, so the
///   number says what happened);
/// - already recorded (`409 already_approved`): the post deleted and
///   `already_approved` counted, but **no** `activite` line — the decision
///   was taken elsewhere (the approval screen, or an earlier tick whose
///   line already exists) and "approved from Buzz" would misattribute it;
///   the journal line from the `.posted` report records what went out;
/// - refused with a code whose remedy is *retry* ([`Remedy::Retry`]: the
///   bus or the store behind the Gateway was out): tried again next tick,
///   like a Gateway that did not answer — an answer in the thread would
///   spend the gesture, and the owner's only way to "try again" would be
///   a second reaction;
/// - refused with any other code: the Companion's own sentence for it in
///   the thread ([`text::thread_refused`]), once; the post stays, for the
///   sweep or a later gesture;
/// - the Gateway will not have the session: "revoked" in the thread, once
///   (`told` says whether it already was), an `error` naming
///   `provision-clerk-device.sh`, and the session marked [`Session::Dead`]
///   — after which a ✅ makes no call and is told the same, once, until a
///   refresh of the loop's own brings the session back and it is carried;
/// - no usable answer (nothing answered, `429`/`5xx`, a shape that is not
///   the route's, a session file that could not be read): counted as
///   `gateway_unreachable`, logged with the URL, tried again next tick —
///   and when the suggestion is within [`not_recorded_window`] of its
///   expiry, "not recorded" in the thread, keyed on the gesture, and the
///   post left to the sweep.
#[allow(clippy::too_many_arguments)]
async fn carry(
    clerk: &Clerk,
    gateway: &Gateway,
    write: &WriteHalf,
    tick: &mut DecisionsTick,
    post: &nostr::Event,
    act: Decision,
    told: bool,
) {
    let post_id = post.id.to_hex();
    let gesture_id = act.gesture_id.as_str();
    // The network as the post names it, for the `activite` line; a post
    // that does not name one gets a line that names none.
    let network = text::network_off_post(&post.content).unwrap_or("");
    let Some(reference) = reference::parse(&post.content) else {
        // One of the clerk's own posts with no reference line it recognises:
        // there is no suggestion to approve or refuse. The sweep already
        // counts and warns about it; the gesture waits with the post.
        debug!(
            post_id,
            gesture_id, "a gesture on a post with no readable reference line"
        );
        return;
    };
    let suggestion_id = reference.suggestion_id.as_str();

    if act.gesture == Gesture::Refuse {
        if delete_post(clerk, tick, &post_id, suggestion_id, "refused").await {
            let total = clerk
                .metrics
                .record_approval(&ApprovalOutcome::RefusedLocally);
            tick.refused_locally += 1;
            info!(
                suggestion_id,
                post_id,
                gesture_id,
                total,
                "the owner refused a suggestion from Buzz; nobody else is told"
            );
            activity_line(
                clerk,
                &text::activity_refused_locally(clerk.lang, network),
                suggestion_id,
            )
            .await;
        }
        return;
    }

    if clerk.session() == Session::Dead {
        // No call: the answer is known, and asking would be the hot loop of
        // refusals. The gesture waits, told once, for the session to come
        // back.
        if !told {
            session_revoked(clerk, gateway, tick, &post_id, suggestion_id, gesture_id).await;
        } else {
            debug!(
                suggestion_id,
                gesture_id, "the session is dead; the owner's gesture waits for it"
            );
        }
        return;
    }

    let edited = act.gesture.edited_text().is_some();
    let answered = gateway
        .approve(suggestion_id, act.gesture.edited_text())
        .await;
    match answered {
        Ok(Outcome::Approved {
            event_id,
            edited,
            already,
        }) => {
            let outcome = if already {
                ApprovalOutcome::AlreadyApproved
            } else {
                ApprovalOutcome::Approved
            };
            let total = clerk.metrics.record_approval(&outcome);
            info!(
                suggestion_id,
                approval_id = %event_id,
                edited,
                already,
                outcome = outcome.as_str(),
                total,
                "the Companion Gateway accepted an approval from Buzz"
            );
            // An approval the Gateway already held was decided elsewhere —
            // the approval screen, or an earlier tick whose line exists —
            // so no "approved from Buzz" line: it would attribute the
            // decision to this gesture. The journal line from the `.posted`
            // report still records what went out.
            let line = (!already).then(|| text::activity_approved(clerk.lang, network, edited));
            went_out(clerk, tick, &post_id, suggestion_id, line.as_deref()).await;
        }
        Ok(Outcome::Refused { status, code }) if refusals::sent(&code) => {
            // The reply went out and the Companion Gateway could not write that down:
            // an approval for every purpose here, counted under its code.
            let total = clerk
                .metrics
                .record_approval(&ApprovalOutcome::Refused(code.clone()));
            warn!(
                suggestion_id,
                status,
                code,
                total,
                "the reply went out but the Companion Gateway could not record the approval"
            );
            let line = text::activity_approved(clerk.lang, network, edited);
            went_out(clerk, tick, &post_id, suggestion_id, Some(&line)).await;
        }
        Ok(Outcome::Refused { status, code }) if refusals::remedy(&code) == Remedy::Retry => {
            let total = clerk
                .metrics
                .record_approval(&ApprovalOutcome::Refused(code.clone()));
            warn!(
                suggestion_id,
                gesture_id,
                status,
                code,
                total,
                gateway = %gateway.base(),
                "the Companion Gateway could not carry an approval right now; tried again next tick"
            );
            defer(clerk, write, tick, post, gesture_id).await;
        }
        Ok(Outcome::Refused { status, code }) => {
            let total = clerk
                .metrics
                .record_approval(&ApprovalOutcome::Refused(code.clone()));
            info!(
                suggestion_id,
                gesture_id,
                status,
                code,
                total,
                "the Companion Gateway refused an approval from Buzz; answered in the thread"
            );
            answer(
                clerk,
                tick,
                &post_id,
                gesture_id,
                &text::thread_refused(clerk.lang, &code),
            )
            .await;
        }
        Err(GatewayError::Unauthenticated) => {
            clerk.set_session(Session::Dead);
            if told {
                // Told already, on an earlier death of the session; the
                // count says the call was made and refused.
                let total = clerk
                    .metrics
                    .record_approval(&ApprovalOutcome::Unauthenticated);
                error!(
                    suggestion_id,
                    gesture_id,
                    total,
                    gateway = %gateway.base(),
                    "the Companion Gateway will not have the clerk's session again; run \
                     provision-clerk-device.sh"
                );
            } else {
                session_revoked(clerk, gateway, tick, &post_id, suggestion_id, gesture_id).await;
            }
        }
        Err(error) => {
            let total = clerk
                .metrics
                .record_approval(&ApprovalOutcome::GatewayUnreachable);
            if error.is_transient() {
                warn!(
                    %error,
                    suggestion_id,
                    gesture_id,
                    total,
                    gateway = %gateway.base(),
                    "the Companion Gateway gave no answer to an approval; tried again next tick"
                );
            } else {
                error!(
                    %error,
                    suggestion_id,
                    gesture_id,
                    total,
                    gateway = %gateway.base(),
                    "the Companion Gateway gave no usable answer to an approval; tried again next \
                     tick"
                );
            }
            defer(clerk, write, tick, post, gesture_id).await;
        }
    }
}

/// The owner's gesture met a dead session: counted as `unauthenticated`,
/// an `error` naming the script, and the thread told — with
/// [`text::thread_revoked`], the one answer that does not spend the
/// gesture ([`answered_gestures`]).
async fn session_revoked(
    clerk: &Clerk,
    gateway: &Gateway,
    tick: &mut DecisionsTick,
    post_id: &str,
    suggestion_id: &str,
    gesture_id: &str,
) {
    let total = clerk
        .metrics
        .record_approval(&ApprovalOutcome::Unauthenticated);
    error!(
        suggestion_id,
        gesture_id,
        total,
        gateway = %gateway.base(),
        "the Companion Gateway will not have the clerk's session: the Buzz device was revoked \
         or its refresh token died, so the owner's gesture waits; run provision-clerk-device.sh"
    );
    answer(
        clerk,
        tick,
        post_id,
        gesture_id,
        &text::thread_revoked(clerk.lang),
    )
    .await;
}

/// An approval that went out: the post deleted, and — once it is — `line`
/// in `activite`, when there is one to write (`None` for an approval the
/// Companion Gateway already held: decided elsewhere, not "from Buzz").
/// The line follows the deletion, not the Gateway's answer. That rule has
/// an accepted cost, and it is **zero lines rather than two**: a `201`
/// whose deletion the relay refuses is carried again next tick, answered
/// `409 already_approved`, deleted, and asked for no line — so an approval
/// this clerk made a tick ago goes unannounced in `activite`. The clerk
/// cannot tell its own last-tick approval from the screen's, because the
/// `Approval` record carries no device id; and a line written on the
/// Gateway's answer instead would be doubled whenever the deletion failed
/// and misattributed whenever the screen approved first. The `.posted`
/// journal line is the record of what went out either way.
async fn went_out(
    clerk: &Clerk,
    tick: &mut DecisionsTick,
    post_id: &str,
    suggestion_id: &str,
    line: Option<&str>,
) {
    if delete_post(clerk, tick, post_id, suggestion_id, "approved").await {
        tick.approved += 1;
        if let Some(line) = line {
            activity_line(clerk, line, suggestion_id).await;
        }
    }
}

/// The Companion Gateway gave no usable answer to the owner's gesture this
/// tick: counted as deferred, and — when the suggestion is within
/// [`not_recorded_window`] of its expiry — told in the thread as "not
/// recorded", keyed on the gesture, so that the sweep deleting the post a
/// moment later is not the owner's only news of their ✅. Nothing is
/// written otherwise; the next tick tries again. The clock is read here
/// and not at the tick's start, because the Gateway call that just failed
/// may have taken its whole timeout, and several deferred posts in one
/// tick each wait their own.
async fn defer(
    clerk: &Clerk,
    write: &WriteHalf,
    tick: &mut DecisionsTick,
    post: &nostr::Event,
    gesture_id: &str,
) {
    tick.deferred += 1;
    if !expires_within(
        &post.content,
        now_unix(),
        not_recorded_window(write.decision),
    ) {
        return;
    }
    let post_id = post.id.to_hex();
    info!(
        post_id,
        gesture_id,
        "the suggestion expires before the next tick; the thread is told the gesture was not \
         recorded"
    );
    answer(
        clerk,
        tick,
        &post_id,
        gesture_id,
        &text::thread_not_recorded(clerk.lang),
    )
    .await;
}

/// One reply in the thread of `post_id`, answering `gesture_id`. Not
/// counted under `twalk_clerk_posts_total{channel="approbations"}`, which
/// has meant "one forum post per suggestion" since #265 and keeps meaning
/// it; every answer is already a row of `twalk_clerk_approvals_total`. A
/// relay failure is a warning and a counted failure, and `false`: the next
/// tick, finding no answer keyed on the gesture, writes it then.
async fn answer(
    clerk: &Clerk,
    tick: &mut DecisionsTick,
    post_id: &str,
    gesture_id: &str,
    content: &str,
) -> bool {
    match clerk
        .relay
        .comment(
            &clerk.config.channel_approvals,
            post_id,
            gesture_id,
            content,
        )
        .await
    {
        Ok(published) => {
            tick.answered += 1;
            debug!(post_id, gesture_id, event_id = %published.event_id, "answered in the thread");
            true
        }
        Err(error) => {
            relay_failed(clerk, tick, &error, "answer in a post's thread");
            false
        }
    }
}

/// One of the clerk's own posts deleted because the owner decided on it
/// (`why` for the log: `approved` or `refused`). `false` on a relay
/// failure, which is counted and warned about; the next tick decides the
/// same post again, and the Companion Gateway's `already_approved` makes
/// that safe.
async fn delete_post(
    clerk: &Clerk,
    tick: &mut DecisionsTick,
    post_id: &str,
    suggestion_id: &str,
    why: &str,
) -> bool {
    match clerk
        .relay
        .delete(&clerk.config.channel_approvals, post_id)
        .await
    {
        Ok(_) => {
            info!(
                why,
                suggestion_id, post_id, "deleted a suggestion's post: the owner decided"
            );
            true
        }
        Err(error) => {
            relay_failed(clerk, tick, &error, "delete a decided post");
            false
        }
    }
}

/// One relay call the decisions loop could not make: a warning naming what
/// it tried (`did` completes "could not …"), a counted relay failure, and
/// the tick's own count of them.
fn relay_failed(clerk: &Clerk, tick: &mut DecisionsTick, error: &RelayError, did: &str) {
    clerk.metrics.record_relay_failure();
    tick.failures += 1;
    warn!(%error, "the decisions loop could not {did}; the next tick will try again");
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
    fn the_sweep_dates_a_post_by_its_reference_line_and_then_by_the_ceiling() {
        const WEEK: i64 = 7 * 24 * 60 * 60;
        // 2026-09-17T11:00:00Z is 1789642800 seconds after the epoch.
        let expires = 1789642800;
        let created = expires - 3600;
        let dated = format!(
            "Réponse proposée\n« Oui »\ntwalk:suggestion:{ID} expires 2026-09-17T11:00:00Z"
        );

        // Dated: the reference line decides, and the ceiling never enters
        // into it — a dated post a month old that has not expired stands.
        assert_eq!(verdict(&dated, created, expires - 1), Verdict::Stands);
        assert_eq!(verdict(&dated, created, expires), Verdict::Expired);
        assert_eq!(
            verdict(&dated, created - WEEK * 4, expires - 1),
            Verdict::Stands
        );

        // Undatable, three ways: no expiry in the reference line (the
        // contract's `expires_at` is optional), one that does not parse,
        // and no reference line the sweep recognises at all. Each stands
        // while the relay's `created_at` is under the ceiling, and goes
        // once it is seven days old.
        for undatable in [
            format!("« Oui »\ntwalk:suggestion:{ID}"),
            format!("« Oui »\ntwalk:suggestion:{ID} expires tomorrow"),
            "« Oui »\nno reference line".to_owned(),
        ] {
            assert_eq!(
                verdict(&undatable, created, created + WEEK - 1),
                Verdict::StandsUndated,
                "{undatable}"
            );
            assert_eq!(
                verdict(&undatable, created, created + WEEK),
                Verdict::PastCeiling,
                "{undatable}"
            );
        }
    }

    #[test]
    fn the_decisions_loop_reads_expiry_off_the_reference_line() {
        // 2026-09-17T11:00:00Z is 1789642800 seconds after the epoch.
        let expires = 1789642800;
        let tick = Duration::from_secs(5);
        let dated = format!(
            "Proposed reply · WhatsApp\n“Oui”\ntwalk:suggestion:{ID} expires 2026-09-17T11:00:00Z"
        );

        // Open until the expiry, expired from it: the same line the sweep
        // draws, so the loop never carries a gesture the sweep is deleting
        // the post of.
        assert!(!post_has_expired(&dated, expires - 1));
        assert!(post_has_expired(&dated, expires));
        assert!(post_has_expired(&dated, expires + 3600));

        // "Within the window": from five seconds before the expiry, not six.
        assert!(!expires_within(&dated, expires - 6, tick));
        assert!(expires_within(&dated, expires - 5, tick));
        assert!(expires_within(&dated, expires - 1, tick));
        assert!(expires_within(&dated, expires, tick));

        // The window the loop really uses is a tick plus a Gateway timeout:
        // a Gateway that hangs holds the tick for the whole timeout, so a
        // post with seven seconds to live at a five-second tick — "not
        // within a tick" — would be expired by the next one and swept
        // in silence. At the default tick that is fifteen seconds.
        let window = not_recorded_window(tick);
        assert_eq!(window, tick + crate::gateway::REQUEST_TIMEOUT, "{window:?}");
        assert_eq!(window, Duration::from_secs(15));
        assert!(expires_within(&dated, expires - 7, window));
        assert!(expires_within(&dated, expires - 15, window));
        assert!(!expires_within(&dated, expires - 16, window));

        // A post the loop cannot date is open and never "within a tick":
        // the gesture on it is tried again for as long as the sweep's
        // ceiling lets the post stand.
        for undatable in [
            format!("“Oui”\ntwalk:suggestion:{ID}"),
            format!("“Oui”\ntwalk:suggestion:{ID} expires tomorrow"),
            "“Oui”\nno reference line".to_owned(),
        ] {
            assert!(!post_has_expired(&undatable, expires + 3600), "{undatable}");
            assert!(!expires_within(&undatable, expires, tick), "{undatable}");
        }
    }

    #[test]
    fn a_revoked_answer_does_not_spend_the_gesture_and_every_other_does() {
        fn answer(gesture: &str, content: &str) -> nostr::Event {
            EventBuilder::new(Kind::Custom(crate::relay::KIND_FORUM_COMMENT), content)
                .tags([
                    nostr::Tag::parse(["e", ID, "", "reply"]).unwrap(),
                    nostr::Tag::parse(["r", &format!("twalk:gesture:{gesture}")]).unwrap(),
                ])
                .sign_with_keys(&Keys::generate())
                .unwrap()
        }
        let g = |n: usize| format!("{:0>64x}", n + 1);
        let answers = [
            answer(&g(0), &text::thread_not_the_owner(Lang::Fr)),
            answer(&g(1), &text::thread_refused(Lang::En, "consent_revoked")),
            answer(&g(2), &text::thread_not_recorded(Lang::Fr)),
            answer(&g(3), &text::thread_revoked(Lang::Fr)),
            answer(&g(4), &text::thread_revoked(Lang::En)),
            // A reply of the clerk's with no gesture tag answers nothing.
            EventBuilder::new(Kind::Custom(crate::relay::KIND_FORUM_COMMENT), "…")
                .sign_with_keys(&Keys::generate())
                .unwrap(),
        ];
        let (spent, told_revoked) = answered_gestures(&answers);
        assert_eq!(
            spent,
            [g(0), g(1), g(2)].into_iter().collect::<HashSet<_>>()
        );
        assert_eq!(
            told_revoked,
            [g(3), g(4)].into_iter().collect::<HashSet<_>>()
        );
    }

    #[test]
    fn a_tick_that_did_nothing_is_debug_and_one_that_did_is_info() {
        assert!(!DecisionsTick::default().happened());
        assert!(!DecisionsTick {
            posts: 12,
            gestures: 3,
            ..DecisionsTick::default()
        }
        .happened());
        for did in [
            DecisionsTick {
                approved: 1,
                ..DecisionsTick::default()
            },
            DecisionsTick {
                refused_locally: 1,
                ..DecisionsTick::default()
            },
            DecisionsTick {
                answered: 1,
                ..DecisionsTick::default()
            },
            DecisionsTick {
                deferred: 1,
                ..DecisionsTick::default()
            },
            DecisionsTick {
                failures: 1,
                ..DecisionsTick::default()
            },
        ] {
            assert!(did.happened(), "{did:?}");
        }
    }

    #[test]
    fn max_deliver_retries_for_the_default_suggestion_life_and_not_past_it() {
        // The retry schedule the bus follows under `nak_delay`, summed over
        // the naks MAX_DELIVER deliveries allow, lands the last delivery
        // within the last minute of TWALK_SUGGESTION_TTL_SECONDS's default
        // hour: still alive, and one more would be past it. A relay outage
        // shorter than a suggestion's life loses nothing, and a poisoned
        // message costs at most that hour.
        let hour = Duration::from_secs(3600);
        let retried_for: Duration = (1..MAX_DELIVER).map(nak_delay).sum();
        assert_eq!(retried_for, Duration::from_secs(3542));
        assert!(retried_for >= hour - NAK_DELAY_MAX, "{retried_for:?}");
        assert!(retried_for < hour, "{retried_for:?}");
        assert!(retried_for + nak_delay(MAX_DELIVER) >= hour);
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
