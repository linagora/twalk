//! Reading suggestions: the projection the approval screen draws from
//! (ticket #97).
//!
//! `CONTEXT.md` defines a suggestion as
//!
//! > A reply a persona proposes and never sends: it exists to be read,
//! > edited or refused by a human.
//!
//! Ticket #24 built the *refused or approved* half and the question that
//! follows it. It left the first verb unimplemented: nothing on this origin
//! could **read** a suggestion, so #100's screen had an approval endpoint and
//! no way to find anything to approve. This module is that read.
//!
//! # A projection of the bus, not a second store
//!
//! The suggestion lives in the stream. This module writes nothing, keeps
//! nothing between requests and has no table — every answer below is
//! computed from what the bus holds at the moment it is asked, plus the
//! Gateway's own approval rows ([`crate::store`]), which are facts about what
//! *this* Gateway did and not a copy of the suggestion.
//!
//! That is a deliberate refusal of the obvious design. A projection kept in
//! SQLite would be faster and would survive the retention window — and the
//! first time a bus replay disagreed with it, nobody could say which one was
//! true. The same reasoning made [`crate::contacts`] keep four values and no
//! content, and made [`crate::approval`] read its suggestion off the stream
//! instead of caching it.
//!
//! The cost is honest and visible: the read is bounded by the same window an
//! approval's lookup is (`GATEWAY_APPROVAL_LOOKUP_WINDOW`), and the answer
//! **says so** — `window.reached_start_of_stream` is false when there may be
//! older suggestions this Gateway did not read, and a single read that the
//! window did not reach is `410 suggestion_out_of_reach` and never `404
//! suggestion_not_found`. A bound nobody can see is a bound that lies.
//!
//! # A suggestion quotes a contact's message, and this endpoint does not
//!
//! A suggestion answers somebody's message. The tempting listing carries an
//! excerpt of that message so the screen can show what is being replied to —
//! and that is exactly the defect #110 fixed one layer down: an excerpt
//! belongs to the **author of the quoted message**, not to the sender of the
//! event carrying it, and a revoked contact's words published inside an event
//! labelled `granted` is a content leak that was found on live data.
//!
//! So this module never opens an inbound event. Not "opens it carefully":
//! there is no code path here that subscribes to, fetches or deserialises
//! `inbound.message.received` at all, and therefore no reduction to get
//! right, no `consent` label to check against the wrong person, and nothing
//! to leak. What the listing says about the trigger is what the *suggestion*
//! says about it — its CloudEvents id and its type, which is identity and
//! not content — and that is enough to answer "for which message".
//!
//! `POST /api/approvals` does resolve the trigger, because it must: the reply
//! has to be addressed to a room and checked against a contact's consent. It
//! does so into a struct with no `data` member, at the one moment it is
//! needed, for one suggestion. A listing has neither of those excuses.
//!
//! What the answer *does* carry is the persona's own proposed text, which is
//! the thing being approved and the reason the screen exists.
//!
//! # Three situations, three answers
//!
//! An expired suggestion, a missing one and an already-approved one are three
//! different facts and a user acts differently on each. This project has lost
//! eight incidents in two days to two failures sharing one signal (#116,
//! #141, #135, #139, #111, #128, #130), so they are kept apart here by
//! construction:
//!
//! - **missing** — `404 suggestion_not_found` from the single read, and
//!   absent from the listing. The whole retained stream was read.
//! - **out of reach** — `410 suggestion_out_of_reach`. Different from
//!   missing: the Gateway did not look that far.
//! - **expired** — `200` with `standing: "expired"`. It exists, it is
//!   visible, and it is no longer approvable (#22's policy, enforced by
//!   [`crate::approval`]).
//! - **approved** — `200` with `standing: "approved"` and the approval
//!   record, whose own `publication` says whether the reply reached the bus.
//!
//! A suggestion is never rendered as a spinner and never as a bare absence.

// The refusals here are [`crate::approval::Refusal`]'s, deliberately: the
// same fact about the same bounded read answers with the same code and the
// same status on both doors. One variant of that enum carries a whole
// approval record, which makes the `Err` side wide — a real cost, and a
// smaller one than a second taxonomy that would drift from the first.
#![allow(clippy::result_large_err)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{debug, warn};

use crate::approval::{
    has_passed, Content, Format, Posted, Refusal, POSTED_AS_HEADER, POSTED_REACH_HEADER,
    REPLY_APPROVED_TYPE, SUGGEST_PRODUCED_TYPE,
};
use crate::consent::{bus_subject, Network, State, STREAM_NAME};
use crate::portals::{Delivery, Portals};
use crate::store::{RecordedApproval, Store};

/// How many suggestions a listing answers with when the caller names no
/// limit, and the most it will answer with when they name a large one.
///
/// A screen draws a page; a client that wants the next one narrows the
/// window rather than asking for ten thousand rows, because every row here
/// costs a JSON document read off the bus.
pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 200;

/// How long a listing's ephemeral consumer survives being forgotten. The read
/// deletes its own; this is what reaps one whose request died. The same value
/// [`crate::approval`] uses, for the same reason.
const LOOKUP_CONSUMER_IDLE: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// What a read answers
// ---------------------------------------------------------------------------

/// Where a suggestion stands, as far as approving it goes.
///
/// Three values and no fourth: this is not the consent state, which is a
/// different question about a different subject and is answered — at the
/// moment it matters — by `POST /api/approvals`. Folding "the contact was
/// revoked" in here would put two independent facts on one signal, which is
/// the shape of every incident this taxonomy exists to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Nothing about the suggestion itself stops it being approved. Whether
    /// the approval will *succeed* is a question for the consent state at
    /// that moment, and only `POST /api/approvals` can answer it.
    Approvable,
    /// Its `expires_at` (#22) has passed. Still readable, no longer
    /// approvable.
    Expired,
    /// This Gateway recorded an approval of it. The record says where the
    /// reply landed, or that it did not.
    Approved,
}

impl Standing {
    pub fn as_str(self) -> &'static str {
        match self {
            Standing::Approvable => "approvable",
            Standing::Expired => "expired",
            Standing::Approved => "approved",
        }
    }
}

/// One suggestion, as this Gateway reads one off the bus.
///
/// Spelled as a type rather than as pointers into a `Value`, for the reason
/// [`crate::contacts::InboundHeader`] is a type: what the struct does not
/// declare, the process does not hold. Note what is not here — nothing of the
/// message being answered beyond its id and its type, and none of the
/// persona's optional `rationale`, which is free prose *about* a contact's
/// message and belongs to the same family as an excerpt.
#[derive(Debug, Clone)]
pub struct Listed {
    pub event_id: String,
    /// `hermes://<domain>/personas/<persona id>`: the persona that proposed
    /// it.
    pub source: String,
    pub persona_id: String,
    pub network: Network,
    /// The consent label the trigger carried when the Sensor observed it —
    /// an audit fact about the past, and never the current state.
    pub consent_label: State,
    /// The event's `time`: when the persona produced it.
    pub produced_at: String,
    pub expires_at: Option<String>,
    pub attempt: Option<u64>,
    /// The message this answers, by identity alone. Its content is not read
    /// here — see the module documentation.
    pub trigger_event_id: String,
    pub trigger_event_type: String,
    /// What the persona proposed: the text the screen draws and the user
    /// approves or edits.
    pub suggestion: Content,
    /// The sentence the reply will disclose itself with, in the language the
    /// persona wrote in (`data.disclosure`, ticket #121) — what the approval
    /// screen shows fixed beside the editable body, or `None` when the
    /// suggestion carries none. Whether it is *appended* is the switch's
    /// business at the moment of approval, not this read's.
    ///
    /// One of the contract's five, verbatim, or `None`: a value on the bus
    /// that is neither is listed as `None` and logged rather than drawn
    /// (see [`Suggestions::settle`]).
    pub disclosure: Option<String>,
    pub stream_sequence: u64,
    pub standing: Standing,
    /// The approval this Gateway recorded, when there is one. `publication`
    /// on it is `published` or `unpublished`, both terminal (#24).
    pub approval: Option<RecordedApproval>,
    /// Whether a reply could reach the contact, as far as the portal
    /// register can tell **before** the approval (issue #216): the owner's
    /// own account in or out of the room the trigger arrived in. Read from
    /// the trigger's *envelope* (`source`), never from its body.
    pub delivery: Delivery,
    /// The Sensor's own report of what the approved reply reached, once it
    /// posted it — `twalk.persona.reply.approved.v1.posted`, with `reach` and
    /// `posted-as` as headers. `None` until the Sensor has posted it, or
    /// when the report is beyond the read window. This is the *after* fact,
    /// and it is never the same member as `approval.publication`: published
    /// on the bus and delivered to the contact are two things (#216).
    pub posted: Option<Posted>,
}

/// The stretch of the stream a read covered, reported so the bound is
/// visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// The first stream position read, and the last.
    pub from_sequence: u64,
    pub to_sequence: u64,
    /// The configured width (`GATEWAY_APPROVAL_LOOKUP_WINDOW`).
    pub sequences: u64,
    /// True when the read began at the stream's first retained message, so
    /// there is nothing older to have missed. False means older suggestions
    /// may exist and were not read — which is a fact about the answer, not
    /// about the bus.
    pub reached_start_of_stream: bool,
}

/// A listing: the suggestions found, and what the read did not cover.
#[derive(Debug, Clone)]
pub struct Listing {
    /// Newest first.
    pub suggestions: Vec<Listed>,
    pub window: Window,
    /// True when the limit cut the list: more suggestions were found inside
    /// the window than were answered with.
    pub truncated: bool,
    /// Suggestions found in the window that this build could not read — an
    /// unknown network, an unknown consent state, a content type the
    /// contract does not name. Counted rather than dropped in silence, so a
    /// screen missing a row has somewhere to look.
    pub unreadable: usize,
}

// ---------------------------------------------------------------------------
// What is read off the bus
// ---------------------------------------------------------------------------

/// What the Gateway reads of a suggestion event.
///
/// `confidence` and `rationale` are absent on purpose, not by oversight: the
/// contract offers both "for oversight display", and `rationale` is a
/// persona's prose about the message it is answering — the same family as the
/// excerpt #110 was about. A later ticket can add it deliberately, with the
/// reduction question asked out loud. Serde ignores what is not declared, so
/// the omission is the enforcement.
#[derive(Debug, Deserialize)]
struct SuggestionDocument {
    id: String,
    source: String,
    time: String,
    network: String,
    consent: String,
    data: SuggestionData,
}

#[derive(Debug, Deserialize)]
struct SuggestionData {
    persona_id: String,
    trigger: TriggerReference,
    suggestion: SuggestionContent,
    #[serde(default)]
    disclosure: Option<String>,
    #[serde(default)]
    attempt: Option<u64>,
    #[serde(default)]
    expires_at: Option<String>,
}

/// The trigger as the *suggestion* names it: two identity values, and no
/// door to the event itself.
#[derive(Debug, Deserialize)]
struct TriggerReference {
    event_id: String,
    event_type: String,
}

#[derive(Debug, Deserialize)]
struct SuggestionContent {
    body: String,
    format: String,
}

// ---------------------------------------------------------------------------
// The half that reads the bus
// ---------------------------------------------------------------------------

/// The suggestion-reading half of the Gateway: the bus the suggestions are
/// on, and the approval rows that say which of them were acted on.
///
/// It shares a store with [`crate::approval`] and a window with it, and holds
/// a bus connection of its own. Its own, because the two are separately
/// failable and separately timed: a listing that cannot reach the bus is a
/// screen that cannot draw, and an approval that cannot reach the bus is a
/// reply that did not go out. Neither should be waiting behind the other.
pub struct Suggestions {
    /// Read-only here: the approval rows, so a listing can say which
    /// suggestions were already acted on. This module writes nothing.
    store: Arc<Store>,
    nats_url: String,
    lookup_window: u64,
    now: fn() -> std::time::SystemTime,
    bus: tokio::sync::OnceCell<async_nats::jetstream::Context>,
    /// The portal register, which answers where the owner's account stands
    /// in a room (#216), attached once both exist — the register is built
    /// after this module's store, which it journals into (#255). Unset on a
    /// deployment with no register, where every suggestion's delivery is
    /// `unknown` and says why.
    portals: std::sync::OnceLock<Arc<Portals>>,
}

impl Suggestions {
    pub fn new(
        store: Arc<Store>,
        nats_url: String,
        lookup_window: u64,
        now: fn() -> std::time::SystemTime,
    ) -> Self {
        Self {
            store,
            nats_url,
            lookup_window,
            now,
            bus: tokio::sync::OnceCell::new(),
            portals: std::sync::OnceLock::new(),
        }
    }

    /// Gives the listing the portal register, once. A second call is ignored:
    /// there is one register per deployment.
    pub fn attach_portals(&self, portals: Arc<Portals>) {
        let _ = self.portals.set(portals);
    }

    pub fn lookup_window(&self) -> u64 {
        self.lookup_window
    }

    /// The bus, connected on first need, and **without**
    /// `retry_on_initial_connect` — the same choice [`crate::approval`]
    /// makes. A human is waiting for a screen to draw; a read that cannot
    /// reach the bus says so now rather than hanging behind a reconnection.
    async fn jetstream(&self) -> Result<&async_nats::jetstream::Context> {
        self.bus
            .get_or_try_init(|| async {
                let client = async_nats::ConnectOptions::new()
                    .connect(&self.nats_url)
                    .await
                    .with_context(|| format!("the bus at {} did not answer", self.nats_url))?;
                Ok(async_nats::jetstream::new(client))
            })
            .await
    }

    /// The suggestions in the read window, newest first.
    pub async fn list(&self, limit: usize) -> Result<Listing, Refusal> {
        let limit = limit.clamp(1, MAX_LIMIT);
        let jetstream = self.jetstream().await.map_err(|error| {
            warn!(%error, "a suggestion listing could not reach the bus");
            Refusal::BusUnreachable(format!("{error:#}"))
        })?;
        let read = self.read(jetstream, None, limit).await.map_err(|error| {
            warn!(%error, "a suggestion listing could not read the bus");
            Refusal::BusUnreachable(format!("{error:#}"))
        })?;
        let mut suggestions = Vec::with_capacity(read.kept.len());
        let mut unreadable = read.unreadable;
        // Newest first: the scan walks the stream forwards, so the rolling
        // buffer ends oldest-first and is reversed once here rather than
        // sorted.
        for (document, sequence) in read.kept.into_iter().rev() {
            match self.settle(document, sequence) {
                Ok(listed) => suggestions.push(listed),
                // One suggestion this build cannot read does not fail the
                // screen: it is counted, and the count is in the answer.
                // A single read of that same suggestion still refuses with
                // `suggestion_unreadable`, because there the caller asked
                // about that one and an empty answer would be a lie.
                Err(Refusal::SuggestionUnreadable(detail)) => {
                    debug!(%detail, "a suggestion in the window could not be read");
                    unreadable += 1;
                }
                Err(refusal) => return Err(refusal),
            }
        }
        self.say_what_a_reply_would_reach(jetstream, &mut suggestions)
            .await;
        Ok(Listing {
            suggestions,
            window: read.window,
            truncated: read.truncated,
            unreadable,
        })
    }

    /// One suggestion by its CloudEvents id.
    ///
    /// The three answers the module documentation names: the suggestion,
    /// `SuggestionNotFound` when the whole retained stream was read without
    /// it, and `SuggestionOutOfReach` when the window gave up first.
    pub async fn one(&self, event_id: &str) -> Result<Listed, Refusal> {
        let jetstream = self.jetstream().await.map_err(|error| {
            warn!(%error, "a suggestion read could not reach the bus");
            Refusal::BusUnreachable(format!("{error:#}"))
        })?;
        let read = self
            .read(jetstream, Some(event_id), 1)
            .await
            .map_err(|error| {
                warn!(%error, "a suggestion read could not read the bus");
                Refusal::BusUnreachable(format!("{error:#}"))
            })?;
        match read.kept.into_iter().next() {
            Some((document, sequence)) => {
                let mut one = vec![self.settle(document, sequence)?];
                self.say_what_a_reply_would_reach(jetstream, &mut one).await;
                Ok(one.remove(0))
            }
            None if read.window.reached_start_of_stream => Err(Refusal::SuggestionNotFound),
            None => Err(Refusal::SuggestionOutOfReach {
                window: self.lookup_window,
            }),
        }
    }

    /// Turns one read document into the answer: the contract's values this
    /// build understands, plus where it stands.
    fn settle(&self, document: SuggestionDocument, sequence: u64) -> Result<Listed, Refusal> {
        let network = Network::parse(&document.network).ok_or_else(|| {
            Refusal::SuggestionUnreadable(format!(
                "it names the network {:?}, which this build does not know",
                document.network
            ))
        })?;
        let consent_label = State::parse(&document.consent).ok_or_else(|| {
            Refusal::SuggestionUnreadable(format!(
                "it names the consent state {:?}, which this build does not know",
                document.consent
            ))
        })?;
        let format = Format::parse(&document.data.suggestion.format).ok_or_else(|| {
            Refusal::SuggestionUnreadable(format!(
                "its body has the format {:?}, which the contract does not name",
                document.data.suggestion.format
            ))
        })?;
        let approval = self.store.approval(&document.id).map_err(|error| {
            warn!(%error, suggestion = %document.id, "a suggestion's approval could not be read");
            Refusal::StoreUnavailable(format!("{error:#}"))
        })?;
        let now = crate::consent::rfc3339_millis((self.now)());
        let standing = standing(approval.as_ref(), document.data.expires_at.as_deref(), &now);
        let disclosure = listed_disclosure(
            &document.id,
            &document.data.persona_id,
            document.data.disclosure,
        );
        Ok(Listed {
            event_id: document.id,
            source: document.source,
            persona_id: document.data.persona_id,
            network,
            consent_label,
            produced_at: document.time,
            expires_at: document.data.expires_at,
            attempt: document.data.attempt,
            trigger_event_id: document.data.trigger.event_id,
            trigger_event_type: document.data.trigger.event_type,
            suggestion: Content {
                body: document.data.suggestion.body,
                format,
            },
            disclosure,
            stream_sequence: sequence,
            standing,
            approval,
            delivery: Delivery::Unknown {
                why: "trigger_out_of_reach",
            },
            posted: None,
        })
    }

    /// Fills in the two facts about delivery that the suggestion event
    /// itself cannot carry (issue #216): what a reply *would* reach, from the
    /// room the trigger arrived in and the owner's membership of it, and —
    /// for an approved suggestion — what the Sensor said the reply *did*
    /// reach.
    ///
    /// Best effort by design: a listing that could be read is answered even
    /// when the second read fails, with `unknown` and the reason, because a
    /// screen that cannot draw is worse than one that says it cannot tell.
    async fn say_what_a_reply_would_reach(
        &self,
        jetstream: &async_nats::jetstream::Context,
        suggestions: &mut [Listed],
    ) {
        if suggestions.is_empty() {
            return;
        }
        // The rooms, from the triggers' envelopes. Every trigger precedes its
        // suggestion on the stream, so the newest suggestion bounds the read.
        let wanted: HashSet<String> = suggestions
            .iter()
            .map(|listed| listed.trigger_event_id.clone())
            .collect();
        let ceiling = suggestions
            .iter()
            .map(|listed| listed.stream_sequence)
            .max()
            .unwrap_or_default();
        let sources = match self
            .collect(
                jetstream,
                &bus_subject(crate::contacts::INBOUND_MESSAGE_TYPE),
                ceiling.saturating_sub(self.lookup_window),
                ceiling,
                &wanted,
                |message| {
                    let envelope: TriggerEnvelope =
                        serde_json::from_slice(&message.payload).ok()?;
                    Some((envelope.id, envelope.source))
                },
            )
            .await
        {
            Ok(sources) => sources,
            Err(error) => {
                warn!(%error, "the triggers of a suggestion listing could not be read, so what a reply would reach is unknown");
                for listed in suggestions.iter_mut() {
                    listed.delivery = Delivery::Unknown {
                        why: "lookup_failed",
                    };
                }
                HashMap::new()
            }
        };
        let mut by_room: HashMap<String, Delivery> = HashMap::new();
        for listed in suggestions.iter_mut() {
            let Some(room) = sources
                .get(&listed.trigger_event_id)
                .and_then(|source| crate::approval::room_from_source(source))
            else {
                continue;
            };
            let delivery = match by_room.get(&room) {
                Some(known) => known.clone(),
                None => {
                    let delivery = match self.portals.get() {
                        Some(portals) => portals.delivery_of(&room).await,
                        None => Delivery::Unknown {
                            why: "no_portal_register",
                        },
                    };
                    by_room.insert(room.clone(), delivery.clone());
                    delivery
                }
            };
            listed.delivery = delivery;
        }

        // The Sensor's reports, for the approvals that were published: from
        // the oldest publication forward, since a report follows its reply.
        let mut wanted: HashSet<String> = HashSet::new();
        let mut start = u64::MAX;
        for listed in suggestions.iter() {
            if let Some(approval) = &listed.approval {
                if let Some(sequence) = approval.stream_sequence {
                    wanted.insert(approval.event_id.clone());
                    start = start.min(sequence);
                }
            }
        }
        if wanted.is_empty() {
            return;
        }
        let reports = match self
            .collect(
                jetstream,
                &format!("{}.posted", bus_subject(REPLY_APPROVED_TYPE)),
                start,
                u64::MAX,
                &wanted,
                |message| {
                    let envelope: PostedEnvelope = serde_json::from_slice(&message.payload).ok()?;
                    let headers = message.headers.as_ref()?;
                    let header =
                        |name: &str| headers.get(name).map(|value| value.as_str().to_owned());
                    let sequence = message.info().map(|info| info.stream_sequence).ok()?;
                    Some((
                        envelope.id,
                        Posted {
                            reach: header(POSTED_REACH_HEADER)?,
                            posted_as: header(POSTED_AS_HEADER)?,
                            stream_sequence: sequence,
                        },
                    ))
                },
            )
            .await
        {
            Ok(reports) => reports,
            Err(error) => {
                warn!(%error, "the Sensor's reports of posted replies could not be read");
                return;
            }
        };
        for listed in suggestions.iter_mut() {
            if let Some(approval) = &listed.approval {
                listed.posted = reports.get(&approval.event_id).cloned();
            }
        }
    }

    /// Reads one subject between two stream positions and keeps, for each of
    /// the `wanted` ids, what `extract` makes of the message that carries it.
    /// Stops as soon as every wanted id has been seen.
    async fn collect<T>(
        &self,
        jetstream: &async_nats::jetstream::Context,
        filter_subject: &str,
        start: u64,
        ceiling: u64,
        wanted: &HashSet<String>,
        mut extract: impl FnMut(&async_nats::jetstream::Message) -> Option<(String, T)>,
    ) -> Result<HashMap<String, T>> {
        let mut found = HashMap::new();
        let mut stream = jetstream
            .get_stream(STREAM_NAME)
            .await
            .context("failed to reach the bus stream")?;
        let (first_sequence, last_sequence) = {
            let info = stream
                .info()
                .await
                .context("failed to read the bus stream's state")?;
            (info.state.first_sequence, info.state.last_sequence)
        };
        let start = start.max(first_sequence.max(1));
        let ceiling = ceiling.min(last_sequence);
        if last_sequence == 0 || ceiling < start {
            return Ok(found);
        }
        let name = format!(
            "gateway-suggestion-facts-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        );
        let consumer = stream
            .create_consumer(async_nats::jetstream::consumer::pull::Config {
                name: Some(name.clone()),
                filter_subject: filter_subject.to_owned(),
                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::ByStartSequence {
                    start_sequence: start,
                },
                ack_policy: async_nats::jetstream::consumer::AckPolicy::None,
                inactive_threshold: LOOKUP_CONSUMER_IDLE,
                ..Default::default()
            })
            .await
            .context("failed to open a read of the bus")?;
        let mut batch = consumer
            .fetch()
            .max_messages(usize::try_from(ceiling - start + 1).unwrap_or(usize::MAX))
            .messages()
            .await
            .context("failed to read from the bus")?;
        {
            use futures::StreamExt;
            while let Some(message) = batch.next().await {
                let Ok(message) = message else { break };
                let sequence = message
                    .info()
                    .map(|info| info.stream_sequence)
                    .unwrap_or_default();
                if sequence > ceiling {
                    break;
                }
                if let Some((id, value)) = extract(&message) {
                    if wanted.contains(&id) {
                        found.insert(id, value);
                        if found.len() == wanted.len() {
                            break;
                        }
                    }
                }
            }
        }
        if let Err(error) = stream.delete_consumer(&name).await {
            debug!(%error, "failed to delete the suggestion facts' consumer");
        }
        Ok(found)
    }

    /// Reads the tail of the suggestion subject and keeps the last `limit`
    /// documents, or stops at `wanted` when one id is being looked for.
    ///
    /// Deliberately not [`crate::approval`]'s `scan`: that one seeks a single
    /// message and stops, this one enumerates and keeps a bounded tail. They
    /// look alike and behave differently at the edges, and one function doing
    /// both would be a boolean parameter naming which half of it is live.
    async fn read(
        &self,
        jetstream: &async_nats::jetstream::Context,
        wanted: Option<&str>,
        limit: usize,
    ) -> Result<Reading> {
        let mut stream = jetstream
            .get_stream(STREAM_NAME)
            .await
            .context("failed to reach the bus stream")?;
        let (first_sequence, last_sequence) = {
            let info = stream
                .info()
                .await
                .context("failed to read the bus stream's state")?;
            (info.state.first_sequence, info.state.last_sequence)
        };
        let first = first_sequence.max(1);
        if last_sequence == 0 || last_sequence < first {
            // An empty stream is read exhaustively: there is nothing older.
            return Ok(Reading {
                kept: Vec::new(),
                window: Window {
                    from_sequence: 0,
                    to_sequence: 0,
                    sequences: self.lookup_window,
                    reached_start_of_stream: true,
                },
                truncated: false,
                unreadable: 0,
            });
        }
        let start = last_sequence.saturating_sub(self.lookup_window).max(first);
        let window = Window {
            from_sequence: start,
            to_sequence: last_sequence,
            sequences: self.lookup_window,
            reached_start_of_stream: start <= first,
        };
        let name = format!(
            "gateway-suggestion-read-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        );
        let consumer = stream
            .create_consumer(async_nats::jetstream::consumer::pull::Config {
                name: Some(name.clone()),
                filter_subject: bus_subject(SUGGEST_PRODUCED_TYPE),
                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::ByStartSequence {
                    start_sequence: start,
                },
                // Nothing is acked and nothing resumes: this consumer exists
                // for the length of one request and is deleted below.
                ack_policy: async_nats::jetstream::consumer::AckPolicy::None,
                inactive_threshold: LOOKUP_CONSUMER_IDLE,
                ..Default::default()
            })
            .await
            .context("failed to open a read of the bus")?;
        let mut batch = consumer
            .fetch()
            .max_messages(usize::try_from(last_sequence - start + 1).unwrap_or(usize::MAX))
            .messages()
            .await
            .context("failed to read from the bus")?;
        let mut kept: VecDeque<(SuggestionDocument, u64)> = VecDeque::with_capacity(limit);
        let mut seen = 0usize;
        let mut unreadable = 0usize;
        {
            use futures::StreamExt;
            while let Some(message) = batch.next().await {
                let Ok(message) = message else { break };
                let sequence = message
                    .info()
                    .map(|info| info.stream_sequence)
                    .unwrap_or_default();
                if sequence > last_sequence {
                    break;
                }
                let Ok(document) = serde_json::from_slice::<SuggestionDocument>(&message.payload)
                else {
                    // On the suggestion subject and not shaped like one: the
                    // listing counts it and moves on rather than failing the
                    // whole screen for one bad message.
                    unreadable += 1;
                    continue;
                };
                if let Some(wanted) = wanted {
                    if document.id != wanted {
                        continue;
                    }
                    kept.push_back((document, sequence));
                    break;
                }
                seen += 1;
                if kept.len() == limit {
                    kept.pop_front();
                }
                kept.push_back((document, sequence));
            }
        }
        if let Err(error) = stream.delete_consumer(&name).await {
            // Harmless: `inactive_threshold` reaps it anyway.
            debug!(%error, "failed to delete the suggestion read's consumer");
        }
        Ok(Reading {
            truncated: wanted.is_none() && seen > kept.len(),
            kept: kept.into(),
            window,
            unreadable,
        })
    }
}

/// The envelope of a trigger, as the delivery question needs it: its id and
/// its `source`, which names the room. **No `data` member**, for the reason
/// every other view of an inbound event in this Gateway has none.
#[derive(Debug, Deserialize)]
struct TriggerEnvelope {
    id: String,
    source: String,
}

/// The Sensor's report of a posted reply: the approval event republished
/// unchanged, so its `id` is the approval's. What the report says is in its
/// headers.
#[derive(Debug, Deserialize)]
struct PostedEnvelope {
    id: String,
}

/// One bounded read of the suggestion subject.
struct Reading {
    /// Oldest first, at most `limit` of them.
    kept: Vec<(SuggestionDocument, u64)>,
    window: Window,
    truncated: bool,
    unreadable: usize,
}

/// Where a suggestion stands, from the two facts that can move it.
///
/// An approval wins over an expiry: once the reply has gone out, when the
/// suggestion would have gone stale is no longer the interesting fact, and a
/// screen that showed "expired" over an approved suggestion would be telling
/// the user their message was not sent.
pub fn standing(
    approval: Option<&RecordedApproval>,
    expires_at: Option<&str>,
    now: &str,
) -> Standing {
    if approval.is_some() {
        return Standing::Approved;
    }
    match expires_at {
        Some(expires_at) if has_passed(expires_at, now) => Standing::Expired,
        // A suggestion with no `expires_at` never goes stale. The SDK always
        // sets one (#22); the contract lets a third-party persona omit it.
        _ => Standing::Approvable,
    }
}

/// The `data.disclosure` a listing draws: the contract's own sentence,
/// verbatim, or `None` (ticket #121, ADR 0031).
///
/// The approval path *refuses* a suggestion whose sentence the contract does
/// not hold (`approval::contract_sentence`, `409 suggestion_unreadable`),
/// and that refusal is the load-bearing one: it is what keeps two hundred
/// characters of a rogue persona's wording from going out after the user's
/// reply. This read does not refuse — one bad message must not blank a
/// screen (the same rule `unreadable` in the window follows) — and it does
/// not pass the value through either, because the approval screen draws
/// this member as *fixed* and tells the user it is the sentence that goes
/// out with every reply, which is exactly the block a forgery would hide
/// in. So a value that is not one of the five is listed as `None`: the row
/// then says the suggestion carries no sentence, which is the truth as far
/// as this Gateway is concerned, since it will never append that one. The
/// occurrence is logged with the persona and the length, never the text —
/// a warning at read is the count, and the approval, when it is pressed,
/// is counted under `suggestion_unreadable` on `/metrics`.
fn listed_disclosure(suggestion: &str, persona: &str, member: Option<String>) -> Option<String> {
    let sentence = member?;
    if crate::disclosure::is_contract_sentence(&sentence) {
        return Some(sentence);
    }
    warn!(
        suggestion,
        persona,
        chars = sentence.chars().count(),
        "this suggestion's disclosure is not one of the contract's sentences \
         (contracts/disclosure/v1/sentences.json), so it is listed with none and would be \
         refused at approval: the sentence a contact is told is the contract's own and never a \
         persona's wording (ADR 0031)"
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded() -> RecordedApproval {
        RecordedApproval {
            event_id: "c".repeat(64),
            suggestion_event_id: "a".repeat(64),
            approved_by: "@michel:example.com".to_owned(),
            persona_id: "assistant".to_owned(),
            network: Network::Whatsapp,
            contact: "@whatsapp_33612345678:example.com".to_owned(),
            edited: false,
            approved_at: "2026-09-17T10:04:37.000Z".to_owned(),
            published_at: Some("2026-09-17T10:04:37.100Z".to_owned()),
            stream_sequence: Some(4242),
        }
    }

    #[test]
    fn expired_missing_and_approved_are_three_answers_and_not_one() {
        let now = "2026-09-17T12:00:00.000Z";
        assert_eq!(
            standing(None, Some("2026-09-17T11:00:00Z"), now),
            Standing::Expired
        );
        assert_eq!(
            standing(None, Some("2026-09-17T13:00:00Z"), now),
            Standing::Approvable
        );
        // Missing is not a standing at all: it is the absence of a row, and
        // `one` answers it with `suggestion_not_found`. That is the point —
        // a screen cannot confuse it with a state.
        assert_eq!(
            [
                Standing::Approvable.as_str(),
                Standing::Expired.as_str(),
                Standing::Approved.as_str()
            ]
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
            3,
            "two standings sharing one name is the defect this enum exists to avoid"
        );
    }

    #[test]
    fn an_approved_suggestion_is_approved_even_after_it_would_have_expired() {
        // The reply went out at 10:04 and the suggestion would have gone
        // stale at 11:00. Rendering "expired" here would tell the user their
        // message was not sent, which is false.
        assert_eq!(
            standing(
                Some(&recorded()),
                Some("2026-09-17T11:00:00Z"),
                "2026-09-17T12:00:00.000Z"
            ),
            Standing::Approved
        );
    }

    #[test]
    fn a_suggestion_with_no_expiry_never_goes_stale() {
        assert_eq!(
            standing(None, None, "2036-09-17T12:00:00.000Z"),
            Standing::Approvable
        );
        // Neither does one whose expiry cannot be read: the direction that
        // refuses nothing wrongly, as `approval::has_passed` decided.
        assert_eq!(
            standing(None, Some("whenever"), "2036-09-17T12:00:00.000Z"),
            Standing::Approvable
        );
    }

    #[test]
    fn what_is_read_of_a_suggestion_holds_nothing_of_the_message_it_answers() {
        // The document type is the enforcement: a suggestion event carrying
        // a trigger with a body, an excerpt and a display name — which no
        // producer publishes, but which is exactly what a helpful one might
        // — parses into a struct with nowhere to put any of it.
        let payload = serde_json::json!({
            "specversion": "1.0",
            "id": "a".repeat(64),
            "source": "hermes://twalk.example.com/personas/assistant",
            "type": "fr.linagora.twalk.persona.suggest.produced.v1",
            "time": "2026-09-17T10:00:00Z",
            "subject": "b".repeat(64),
            "datacontenttype": "application/json",
            "network": "whatsapp",
            "consent": "granted",
            "data": {
                "persona_id": "assistant",
                "trigger": {
                    "event_id": "b".repeat(64),
                    "event_type": "fr.linagora.twalk.inbound.message.received.v1",
                    "body": "ON DÉCALE À 20H",
                    "excerpt": "QUOTED BY SOMEBODY ELSE",
                    "contact": { "display_name": "Aicha Benali" }
                },
                "suggestion": { "body": "Pas de problème, à 20h !", "format": "text/plain" },
                "disclosure": "Rédigé avec mon assistant IA.",
                "rationale": "SHE ASKED TO MOVE THE APPOINTMENT TO 20H",
                "confidence": 0.9,
                "attempt": 1,
                "expires_at": "2026-09-17T11:00:00Z"
            }
        });
        let document: SuggestionDocument =
            serde_json::from_value(payload).expect("the suggestion parses");
        let held = format!("{document:?}");
        for quoted in [
            "ON DÉCALE À 20H",
            "QUOTED BY SOMEBODY ELSE",
            "Aicha Benali",
            "SHE ASKED TO MOVE",
        ] {
            assert!(
                !held.contains(quoted),
                "the suggestion read holds {quoted:?}, which belongs to the author of the message \
                 being answered and not to the persona — the defect #110 fixed, one layer up"
            );
        }
        assert_eq!(document.data.trigger.event_id, "b".repeat(64));
        assert_eq!(document.data.suggestion.body, "Pas de problème, à 20h !");
        assert_eq!(
            document.data.disclosure.as_deref(),
            Some("Rédigé avec mon assistant IA."),
            "the persona's own sentence is read, so the screen can show it (#121)"
        );
    }

    #[test]
    fn a_sentence_the_contract_does_not_hold_is_listed_as_none_and_never_drawn() {
        let contracts = "Rédigé avec mon assistant IA.";
        assert_eq!(
            listed_disclosure("a", "assistant", Some(contracts.to_owned())).as_deref(),
            Some(contracts)
        );
        assert_eq!(listed_disclosure("a", "assistant", None), None);
        // A rogue persona's own wording, contract-valid on the bus: the
        // screen would draw it as the fixed sentence that goes out with
        // every reply. It is listed as none instead — the approval refuses
        // it, so "carries no sentence" is what this Gateway will do with it.
        for forged in [
            "Written by an assistant you can trust.",
            "Rédigé avec mon assistant IA",
            "",
        ] {
            assert_eq!(
                listed_disclosure("a", "assistant", Some(forged.to_owned())),
                None,
                "{forged:?}"
            );
        }
    }

    #[test]
    fn the_limit_is_clamped_rather_than_trusted() {
        assert_eq!(0usize.clamp(1, MAX_LIMIT), 1);
        assert_eq!(usize::MAX.clamp(1, MAX_LIMIT), MAX_LIMIT);
        assert_eq!(DEFAULT_LIMIT.clamp(1, MAX_LIMIT), DEFAULT_LIMIT);
    }
}
