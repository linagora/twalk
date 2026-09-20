//! The relay: the clerk's one voice, a Buzz relay's two HTTP routes, spoken
//! with the clerk's own Nostr key and nothing else (ticket #265, ADR 0035).
//!
//! A Buzz relay is a Nostr relay with an HTTP front: `POST /events` takes one
//! signed event and `POST /query` takes a list of filters, and each request is
//! authenticated by NIP-98 — a throwaway kind-27235 event that names the exact
//! URL hit, the method and the SHA-256 of the body, signed by the same key the
//! posts are. The relay is multi-tenant by host and keeps a replay guard on
//! that event's id, which is why the URL is the one the relay **announces**
//! ([`crate::config::Config::relay_url`]) and why every request signs a fresh
//! event with a nonce of its own: two posts with one body are still two
//! requests. The recipe is Buzz's own CLI's (`buzz-cli/src/client.rs`),
//! mirrored rather than reinvented, because a header the relay's own tooling
//! builds is one the relay is known to accept.
//!
//! Five things this module decides and three it deliberately leaves alone.
//! It **decides** what crosses the wire: the event's kind and tags as the
//! caller gave them, the signature, and the header. It decides that
//! **nothing read back is believed until its signature is** ([`verified`],
//! applied to every `/query` answer in [`Relay::query`]): the relay is a
//! surface the clerk writes through and reads through, never an authority
//! over what was said (ADR 0032), and since #284 what it serves decides
//! whether a reply goes out — a `pubkey` field in a JSON object is the
//! relay's word, a Schnorr signature over the id is the key holder's, and
//! only the second makes a ✅ the owner's. An event that does not verify is
//! dropped, warned about by id and kind, and counted
//! (`twalk_clerk_skipped_total{why="unverified"}`), so a relay that starts
//! forging is visible rather than obeyed. It decides how a failure
//! is **named**: nothing answered ([`RelayError::Unreachable`]), the relay
//! answered and said no ([`RelayError::Refused`], with the status and the
//! relay's own short reason), or the relay answered something that is not a
//! relay's answer ([`RelayError::Malformed`]) — and which of those a caller
//! may sensibly try again ([`RelayError::is_transient`]: a relay that could
//! not be reached, a `429` and a `5xx`, never a `4xx` that says the request
//! itself was wrong). It decides that **no error carries a body's text**: a
//! `2xx` from `/query` that does not deserialise is the clerk's own posts,
//! which quote suggestions, and the error is what `settle` logs — so
//! [`RelayError::Malformed`] names the parse failure's category, its line
//! and column and the body's length, and [`RelayError::Refused`] keeps only
//! the `error` or `message` field of a JSON refusal, cut to
//! [`REASON_CHARS`], or nothing. And it decides that the clerk's **key file
//! is nobody else's to read**: [`load_keys`] refuses a file another account
//! on the host could open, naming the `chmod` that fixes it, because the key
//! signs everything the clerk says.
//!
//! It **does not retry**: a caller knows whether a post is worth a second try
//! and a second later, and this module does not — a retry loop here would turn
//! one rate limit into a burst. It **holds no memory** of what it published:
//! the relay is the clerk's memory (ADR 0035), read back through
//! [`Relay::own_posts`] for a forum post and [`Relay::own_lines_about`] for a
//! stream message — every stream message carries the bus event it was
//! written for as a tag (`["r", "twalk:event:<id>"]`, [`event_reference`]),
//! because the relay refuses a `created_at` more than fifteen minutes from
//! its own clock (`buzz-relay`'s ingest, `MAX_TIMESTAMP_DRIFT_SECS`), so a
//! line cannot be made to hash to one Nostr id across a redelivery by dating
//! it from the bus event's `time`. And it **knows no kinds beyond the six
//! the clerk uses** — a forum post, a stream message, a delete and its own
//! posts back, and since ticket #284 the two the owner answers a post with:
//! a reaction (kind 7) and a thread reply (kind 45003), which the clerk
//! reads off its own posts ([`Relay::gestures_on`]) and answers in the
//! post's thread ([`Relay::comment`]), because the write half of the loop
//! runs on Buzz and Buzz's word for "decided" is a gesture on the post —
//! so a seventh would be a decision made here, in the open, rather than a
//! tag list assembled somewhere else. Two facts about the relay are load-
//! bearing there and are spelled out once, in the readers rather than in a
//! caller: Buzz resolves a reaction's target from the **last** `e` tag of
//! the event ([`targets`]), and a direct reply to a post carries **one** `e`
//! tag, marked `reply`, while a nested reply carries two, the first marked
//! `root` ([`is_direct_reply`]) — a nested reply is a conversation under the
//! post, not a decision on it. And what the clerk already answered it
//! finds the way it finds everything else, by reading the relay back: each
//! of its replies carries the gesture it answers as a tag
//! (`["r", "twalk:gesture:<id>"]`, [`answered_gesture`]), so a redelivery
//! and a restart are one read ([`Relay::own_comments_on`]).

use std::collections::HashSet;
use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use nostr::{Event, EventBuilder, JsonUtil, Keys, Kind, Tag};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::metrics::{Metrics, Skipped};

/// NIP-98's own kind: an HTTP authorisation event, never stored by a relay.
pub const NIP98_KIND: u16 = 27235;
/// A Buzz forum post, the shape one suggestion becomes in `approbations`.
pub const KIND_FORUM_POST: u16 = 45001;
/// A Buzz stream message, one line in a channel's running feed.
pub const KIND_STREAM_MESSAGE: u16 = 9;
/// A Buzz deletion of one of one's own events in a channel.
pub const KIND_DELETE: u16 = 9005;
/// A reaction (NIP-25): the owner's tick or cross on one of the clerk's
/// posts. Buzz's own shape is one `["e", post_id]` tag and the emoji as the
/// content — no `h`, the channel is the post's.
pub const KIND_REACTION: u16 = 7;
/// A Buzz forum comment: a reply in a post's thread, the owner's or the
/// clerk's own.
pub const KIND_FORUM_COMMENT: u16 = 45003;

/// What every clerk reply's `r` tag starts with: the gesture it answers,
/// `twalk:gesture:<event id>` ([`gesture_reference`]) — the reply's
/// counterpart of a stream message's [`EVENT_REFERENCE_PREFIX`].
pub const GESTURE_PREFIX: &str = "twalk:gesture:";

/// How many post ids one `#e` filter names. The relay pushes `#e` down to
/// its index whatever the count, so the bound is on the request and not on
/// the answer: a clerk with a year of posts behind it must not put every
/// id it ever wrote into one body, and one filter's `limit` is per filter.
pub const GESTURE_QUERY_IDS: usize = 100;

/// How long one request may take, end to end. The relay is on the same host
/// or the same private network as the clerk on the reference deployment, and
/// NIP-98 gives a request sixty seconds of validity either way, so a request
/// still in flight after ten is one that will not succeed.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// How much of a refusal's reason an error carries. The relay's own reasons
/// (`{"error": "invalid: …"}`) are one short sentence; a longer one is cut,
/// and a body that is not a JSON object with an `error` or `message` string
/// — a proxy's HTML page — contributes nothing at all, because a body the
/// clerk did not expect is a body it must not repeat.
pub const REASON_CHARS: usize = 120;

/// What every stream message's `r` tag starts with: the bus event the line
/// was written for, `twalk:event:<CloudEvents id>` ([`event_reference`]).
pub const EVENT_REFERENCE_PREFIX: &str = "twalk:event:";

/// The variable `deploy/docker-compose/provision-nostr-key.sh` writes the
/// key under, in the env-style file Hermes reads the same key from.
pub const KEY_FILE_VARIABLE: &str = "BUZZ_PRIVATE_KEY";

/// What the relay answered to one accepted `POST /events`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Published {
    /// The event's id, hex, as the relay computed it — the same id the event
    /// carried, which is what a later delete or a later query names.
    pub event_id: String,
    /// Whether the relay took the event. A `200` with `accepted: false` is the
    /// relay's way of saying "seen before" (`duplicate:` in `message`), which
    /// is not a refusal: the post is there.
    pub accepted: bool,
    #[serde(default)]
    pub message: String,
}

/// Why one request to the relay did not produce what was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayError {
    /// Nothing answered: connection refused, DNS, TLS, or the request timed
    /// out. The relay may be down or may be starting.
    Unreachable(String),
    /// The relay answered with a status outside `2xx`. `reason` is the
    /// `error` or `message` string of its JSON body, cut to [`REASON_CHARS`],
    /// or empty when the body was not that shape — never the body itself.
    Refused { status: u16, reason: String },
    /// Either this side could not build the request (a tag the `nostr` crate
    /// would not parse, a signature that failed) or the relay answered `2xx`
    /// with something that is not the answer this route gives — described by
    /// the parse failure's category and position and the body's length,
    /// never its text ([`malformed_answer`]).
    Malformed(String),
}

impl RelayError {
    /// Whether the same request, made again later, could succeed: the relay
    /// could not be reached, or it answered `429` or `5xx`. A `4xx` other than
    /// `429` is the relay's verdict on the request itself — a bad signature, a
    /// replayed nonce, a channel the clerk is not a member of — and asking
    /// again would only be refused again.
    pub fn is_transient(&self) -> bool {
        match self {
            RelayError::Unreachable(_) => true,
            RelayError::Refused { status, .. } => *status == 429 || *status >= 500,
            RelayError::Malformed(_) => false,
        }
    }
}

impl fmt::Display for RelayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RelayError::Unreachable(why) => write!(f, "the relay could not be reached: {why}"),
            RelayError::Refused { status, reason } if reason.is_empty() => {
                write!(f, "the relay refused the request with HTTP {status}")
            }
            RelayError::Refused { status, reason } => {
                write!(
                    f,
                    "the relay refused the request with HTTP {status}: {reason}"
                )
            }
            RelayError::Malformed(why) => write!(f, "the relay's answer could not be used: {why}"),
        }
    }
}

impl std::error::Error for RelayError {}

/// A refusal as the error carries it: the `error` or `message` string of a
/// JSON object body, cut to [`REASON_CHARS`] (marked when cut), and nothing
/// from any other shape of body.
fn refusal(status: u16, body: &str) -> RelayError {
    let reason = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            ["error", "message"]
                .into_iter()
                .find_map(|field| value.get(field)?.as_str().map(str::to_owned))
        })
        .map(|reason| {
            let reason = reason.trim();
            if reason.chars().count() <= REASON_CHARS {
                reason.to_owned()
            } else {
                let shown: String = reason.chars().take(REASON_CHARS).collect();
                format!("{shown}… [cut]")
            }
        })
        .unwrap_or_default();
    RelayError::Refused { status, reason }
}

/// A `2xx` whose body is not the answer the route gives, as the error
/// carries it: the status, the URL, serde_json's category of failure and
/// the line and column it stopped at, and how long the body was — and none
/// of the body, because for `/query` that body is the clerk's own posts,
/// which quote suggestions, and the error is what ends up in a log line.
fn malformed_answer(status: u16, url: &str, body: &str, error: &serde_json::Error) -> RelayError {
    RelayError::Malformed(format!(
        "HTTP {status} from {url} is not the answer that route gives: {:?} at line {} column {} \
         of a body of {} bytes",
        error.classify(),
        error.line(),
        error.column(),
        body.len()
    ))
}

/// The value of a stream message's `r` tag for the bus event `bus_event_id`:
/// what [`Relay::own_lines_about`] finds a line by, and what makes a
/// redelivery of that event a line already written rather than a second one.
pub fn event_reference(bus_event_id: &str) -> String {
    format!("{EVENT_REFERENCE_PREFIX}{bus_event_id}")
}

/// Whether `event` carries the `r` tag for `bus_event_id`. Checked here as
/// well as asked of the relay, because the relay answers a `#r` filter by
/// reading its newest `limit` rows and filtering them itself, and a client
/// that took the answer on trust would take the newest line for the right
/// one if that ever changed.
pub fn references_event(event: &Event, bus_event_id: &str) -> bool {
    let wanted = event_reference(bus_event_id);
    event
        .tags
        .iter()
        .map(|tag| tag.as_slice())
        .any(|tag| tag.len() >= 2 && tag[0] == "r" && tag[1] == wanted)
}

/// The value of a clerk reply's `r` tag for the gesture `gesture_id`: what
/// [`answered_gesture`] reads back, and what makes a redelivery of the bus
/// event that carried the gesture's outcome a reply already written.
pub fn gesture_reference(gesture_id: &str) -> String {
    format!("{GESTURE_PREFIX}{gesture_id}")
}

/// Whether `event`, a reaction or a thread reply, targets `post_id`: the
/// second element of its **last** `e` tag is `post_id`. The last one,
/// because that is where Buzz's ingest reads a reaction's target from
/// (`.tags.iter().rev().find_map(…)`), and a reader that took the first
/// would count a reaction whose client quoted another post as a decision
/// on that other post. For a direct reply the one `e` tag is the last; for
/// a nested reply the last is the parent comment, so a nested reply never
/// targets the post itself — which is right, it answers a comment.
///
/// A kind that is not a gesture — a delete names its target in an `e` tag
/// too — targets nothing, whatever its tags say.
pub fn targets(event: &Event, post_id: &str) -> bool {
    let kind = event.kind.as_u16();
    if kind != KIND_REACTION && kind != KIND_FORUM_COMMENT {
        return false;
    }
    e_tags(event).last().is_some_and(|tag| tag[1] == post_id)
}

/// Whether `event` is a **direct** reply to `post_id`: a thread reply
/// (kind 45003) with exactly one `e` tag, `["e", post_id, "", "reply"]` as
/// Buzz's own builder writes it or `["e", post_id]` as a client that writes
/// no marker does. A nested reply carries two `e` tags (the root and the
/// parent), and one `e` tag marked `root` is the shape of a reply whose
/// parent was left out — neither is a reply *to the post*, and only a reply
/// to the post is the owner deciding on it.
pub fn is_direct_reply(event: &Event, post_id: &str) -> bool {
    if event.kind.as_u16() != KIND_FORUM_COMMENT {
        return false;
    }
    let tags = e_tags(event);
    let [tag] = tags.as_slice() else {
        return false;
    };
    tag[1] == post_id
        && matches!(
            tag.get(3).map(String::as_str),
            None | Some("") | Some("reply")
        )
}

/// The gesture a clerk reply answers, read from its `r` tag
/// (`twalk:gesture:<id>`), if it carries one. An `r` tag of another prefix
/// — a stream message's bus event — is not a gesture, and a content that
/// spells the reference is not a tag.
pub fn answered_gesture(event: &Event) -> Option<&str> {
    event
        .tags
        .iter()
        .map(|tag| tag.as_slice())
        .filter(|tag| tag.len() >= 2 && tag[0] == "r")
        .find_map(|tag| tag[1].strip_prefix(GESTURE_PREFIX))
        .filter(|id| !id.is_empty())
}

/// One event the relay served that the clerk will not believe: which, of
/// what kind, and why ([`nostr::event::Error::InvalidId`] for an id that
/// is not the hash of the event, [`nostr::event::Error::InvalidSignature`]
/// for a signature that is not the `pubkey`'s over that id). No content:
/// it may be a forged post quoting who knows what.
#[derive(Debug, PartialEq)]
pub struct Rejected {
    pub id: nostr::EventId,
    pub kind: u16,
    pub error: nostr::event::Error,
}

/// The events of `served` whose id and Schnorr signature verify
/// (`nostr::Event::verify`), in their order, and the ones that did not,
/// each with the reason. Applied to every `/query` answer
/// ([`Relay::query`]), because a `pubkey` the relay serves is the relay's
/// claim and only the signature is the key holder's: a compromised relay
/// that answers `{"pubkey": <owner>, "content": "✅", …}` with a signature
/// it cannot make must not get a reply sent in the owner's name — and one
/// that recomputes the id over the owner's pubkey has only moved the
/// failure from the id check to the signature check.
pub fn verified(served: Vec<Event>) -> (Vec<Event>, Vec<Rejected>) {
    let mut rejected = Vec::new();
    let events = served
        .into_iter()
        .filter(|event| match event.verify() {
            Ok(()) => true,
            Err(error) => {
                rejected.push(Rejected {
                    id: event.id,
                    kind: event.kind.as_u16(),
                    error,
                });
                false
            }
        })
        .collect();
    (events, rejected)
}

/// The `e` tags of `event`, in order, each with at least the id cell.
fn e_tags(event: &Event) -> Vec<&[String]> {
    event
        .tags
        .iter()
        .map(|tag| tag.as_slice())
        .filter(|tag| tag.len() >= 2 && tag[0] == "e")
        .collect()
}

/// A Buzz relay, reached over HTTP under the clerk's own key.
pub struct Relay {
    /// The URL the relay announces, with no trailing slash, so `{base}/events`
    /// is exactly the `u` tag the relay compares against.
    base: String,
    keys: Keys,
    http: reqwest::Client,
    /// Where an event dropped as unverified is counted, when the binary
    /// handed its counters over ([`Relay::with_metrics`]); a relay built
    /// without them (a test) still drops and warns.
    metrics: Option<Arc<Metrics>>,
    /// The ids already warned about as unverified, so that a forged event
    /// the relay serves on every tick is one warning and not one a tick.
    /// A log deduplication and nothing more — not a memory of what the
    /// clerk did (ADR 0035 is about that), lost at every restart and worth
    /// nothing kept: after a restart the event is warned about once more.
    warned_unverified: Mutex<HashSet<nostr::EventId>>,
}

impl fmt::Debug for Relay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Relay")
            .field("base", &self.base)
            .field("public_key", &self.public_key_hex())
            .finish_non_exhaustive()
    }
}

impl Relay {
    /// `base` is the URL the relay announces; a trailing slash is dropped so
    /// the route paths below compose. The HTTP client is built once, with
    /// [`REQUEST_TIMEOUT`] on every request.
    pub fn new(base: &str, keys: Keys) -> Result<Self> {
        let base = base.trim_end_matches('/').to_owned();
        if base.is_empty() {
            bail!("the relay URL is empty");
        }
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("building the HTTP client for the relay")?;
        Ok(Self {
            base,
            keys,
            http,
            metrics: None,
            warned_unverified: Mutex::new(HashSet::new()),
        })
    }

    /// The counters an unverified event is counted in
    /// (`twalk_clerk_skipped_total{why="unverified"}`).
    pub fn with_metrics(mut self, metrics: Arc<Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// The announced URL as stored: no trailing slash.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The clerk's own public key, hex — the `authors` of its own posts.
    pub fn public_key_hex(&self) -> String {
        self.keys.public_key().to_hex()
    }

    /// Signs and publishes one event. `kind` is the raw kind number and
    /// `tags` are the tag vectors as Nostr writes them (`["h", channel]`).
    pub async fn publish(
        &self,
        kind: u16,
        tags: Vec<Vec<String>>,
        content: &str,
    ) -> Result<Published, RelayError> {
        let tags = tags
            .into_iter()
            .map(|tag| {
                Tag::parse(tag.iter().map(String::as_str)).map_err(|e| {
                    RelayError::Malformed(format!("tag {tag:?} is not a Nostr tag: {e}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let event = EventBuilder::new(Kind::Custom(kind), content)
            .tags(tags)
            .sign_with_keys(&self.keys)
            .map_err(|e| RelayError::Malformed(format!("signing a kind {kind} event: {e}")))?;
        let body = event.as_json().into_bytes();
        let published: Published = self.post("/events", body).await?;
        debug!(
            kind,
            event_id = %published.event_id,
            accepted = published.accepted,
            message = %published.message,
            "published to the relay"
        );
        Ok(published)
    }

    /// `POST /query` with one or more Nostr filters, as JSON. The relay
    /// answers the matching events newest first, at most the `limit` each
    /// filter names (and never more than 1000).
    pub async fn query(&self, filters: Vec<Value>) -> Result<Vec<Event>, RelayError> {
        let body = serde_json::to_vec(&filters)
            .map_err(|e| RelayError::Malformed(format!("serialising the filters: {e}")))?;
        let served: Vec<Event> = self.post("/query", body).await?;
        let (events, rejected) = verified(served);
        for rejected in rejected {
            // Counted every time it is served and dropped — the counter is
            // the slope of a relay that keeps serving it — but warned about
            // once per process: the same forged event comes back on every
            // tick of the sweep and the loop, and a warning a tick for ever
            // is a log nobody reads.
            if let Some(metrics) = &self.metrics {
                metrics.record_skipped(Skipped::Unverified);
            }
            let first_time = self
                .warned_unverified
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(rejected.id);
            if first_time {
                warn!(
                    event_id = %rejected.id.to_hex(),
                    kind = rejected.kind,
                    error = %rejected.error,
                    "dropped an event the relay served whose signature does not verify; counted \
                     on every tick it is served again, warned about once"
                );
            }
        }
        Ok(events)
    }

    /// A forum post (kind 45001) in `channel`: what one suggestion becomes.
    pub async fn forum_post(&self, channel: &str, content: &str) -> Result<Published, RelayError> {
        self.publish(KIND_FORUM_POST, vec![tag("h", channel)], content)
            .await
    }

    /// A stream message (kind 9) in `channel`: one line in a running feed,
    /// tagged with the bus event it was written for
    /// (`["r", "twalk:event:<bus_event_id>"]`), which is how the line is
    /// found again on a redelivery ([`Relay::own_lines_about`]).
    pub async fn stream_message(
        &self,
        channel: &str,
        content: &str,
        bus_event_id: &str,
    ) -> Result<Published, RelayError> {
        self.publish(
            KIND_STREAM_MESSAGE,
            vec![tag("h", channel), tag("r", &event_reference(bus_event_id))],
            content,
        )
        .await
    }

    /// A delete (kind 9005) of `event_id`, one of the clerk's own events in
    /// `channel`. The relay refuses a delete of somebody else's.
    pub async fn delete(&self, channel: &str, event_id: &str) -> Result<Published, RelayError> {
        self.publish(KIND_DELETE, vec![tag("h", channel), tag("e", event_id)], "")
            .await
    }

    /// A reply (kind 45003) in the thread of `post_id`, one of the clerk's
    /// own posts in `channel`, answering the gesture `gesture_id` — the
    /// owner's reaction or reply that the answer is about. A **direct**
    /// reply to the post, the way Buzz's own builder writes one
    /// (`["e", post_id, "", "reply"]`, one `e` tag), so the owner reads it
    /// under the post and not under their own comment; and tagged with the
    /// gesture ([`gesture_reference`]) so [`Relay::own_comments_on`] finds
    /// it again on a redelivery.
    pub async fn comment(
        &self,
        channel: &str,
        post_id: &str,
        gesture_id: &str,
        content: &str,
    ) -> Result<Published, RelayError> {
        self.publish(
            KIND_FORUM_COMMENT,
            comment_tags(channel, post_id, gesture_id),
            content,
        )
        .await
    }

    /// The reactions (kind 7) and thread replies (kind 45003) on any of
    /// `post_ids`, by anyone — the owner's gestures on the clerk's posts —
    /// at most `limit` per query, one query per [`GESTURE_QUERY_IDS`] ids
    /// ([`gestures_filters`]), in the relay's order (newest first) **within
    /// each chunk** and chunk-major across them, so a caller that needs an
    /// order sorts (`decision::decisions_on` does). No ids is no
    /// query. Each event is answered once even when two of its `e` tags
    /// name two of the ids (a nested reply names its root and its parent),
    /// because a caller counts what it is given; and which post an event
    /// is *on* is [`targets`]'s to say, never the filter's.
    pub async fn gestures_on(
        &self,
        post_ids: &[String],
        limit: u32,
    ) -> Result<Vec<Event>, RelayError> {
        self.query_each(gestures_filters(post_ids, limit)).await
    }

    /// The clerk's own thread replies (kind 45003) on any of `post_ids`:
    /// the relay as the clerk's memory of which gestures it has answered
    /// ([`answered_gesture`] on each), chunked like [`Relay::gestures_on`].
    pub async fn own_comments_on(
        &self,
        post_ids: &[String],
        limit: u32,
    ) -> Result<Vec<Event>, RelayError> {
        self.query_each(own_comments_filters(
            &self.public_key_hex(),
            post_ids,
            limit,
        ))
        .await
    }

    /// One `POST /query` per filter, the answers concatenated and each
    /// event kept once by id.
    async fn query_each(&self, filters: Vec<Value>) -> Result<Vec<Event>, RelayError> {
        let mut seen = std::collections::HashSet::new();
        let mut events = Vec::new();
        for filter in filters {
            for event in self.query(vec![filter]).await? {
                if seen.insert(event.id) {
                    events.push(event);
                }
            }
        }
        Ok(events)
    }

    /// The clerk's own events of `kind` in `channel`, newest first, at most
    /// `limit`: the relay as the clerk's memory (ADR 0035).
    pub async fn own_posts(
        &self,
        channel: &str,
        kind: u16,
        limit: u32,
    ) -> Result<Vec<Event>, RelayError> {
        let filter = own_posts_filter(&self.public_key_hex(), channel, kind, limit);
        self.query(vec![filter]).await
    }

    /// The clerk's own stream messages in `channel` written for the bus
    /// event `bus_event_id`: the relay as the clerk's memory for a line, the
    /// way [`Relay::own_posts`] and the reference line are for a post. The
    /// relay is asked with a `#r` filter over its newest `limit` lines by
    /// this key, and the answer is checked tag by tag
    /// ([`references_event`]) before it is believed.
    pub async fn own_lines_about(
        &self,
        channel: &str,
        bus_event_id: &str,
        limit: u32,
    ) -> Result<Vec<Event>, RelayError> {
        let filter = own_lines_filter(&self.public_key_hex(), channel, bus_event_id, limit);
        let lines = self.query(vec![filter]).await?;
        Ok(lines
            .into_iter()
            .filter(|line| references_event(line, bus_event_id))
            .collect())
    }

    /// One authenticated `POST` to `path`, the body signed into a fresh
    /// NIP-98 header, the `2xx` answer read as `T`.
    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: Vec<u8>,
    ) -> Result<T, RelayError> {
        let url = format!("{}{path}", self.base);
        let authorization = nip98_authorization(&self.keys, &url, &body)?;
        let response = self
            .http
            .post(&url)
            .header(reqwest::header::AUTHORIZATION, authorization)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| RelayError::Unreachable(e.to_string()))?;
        let status = response.status();
        // A body that could not be read after a status was received: the
        // connection went while the answer was in flight. `Unreachable` is a
        // loose word for it, but it is the right retry class — transient
        // either way — and the message says what actually happened.
        let text = response
            .text()
            .await
            .map_err(|e| RelayError::Unreachable(format!("reading the answer to {url}: {e}")))?;
        if !status.is_success() {
            let error = refusal(status.as_u16(), &text);
            warn!(url = %url, %error, "the relay refused the request");
            return Err(error);
        }
        serde_json::from_str(&text).map_err(|e| malformed_answer(status.as_u16(), &url, &text, &e))
    }
}

fn tag(name: &str, value: &str) -> Vec<String> {
    vec![name.to_owned(), value.to_owned()]
}

/// The filter that finds the clerk's own events of one kind in one channel:
/// `{"kinds":[kind],"#h":[channel],"authors":[pubkey],"limit":limit}`.
pub fn own_posts_filter(pubkey_hex: &str, channel: &str, kind: u16, limit: u32) -> Value {
    serde_json::json!({
        "kinds": [kind],
        "#h": [channel],
        "authors": [pubkey_hex],
        "limit": limit,
    })
}

/// The filter that finds the clerk's own stream messages written for one
/// bus event: [`own_posts_filter`] for kind 9 plus `"#r": ["twalk:event:<id>"]`.
pub fn own_lines_filter(pubkey_hex: &str, channel: &str, bus_event_id: &str, limit: u32) -> Value {
    serde_json::json!({
        "kinds": [KIND_STREAM_MESSAGE],
        "#h": [channel],
        "#r": [event_reference(bus_event_id)],
        "authors": [pubkey_hex],
        "limit": limit,
    })
}

/// The filters that find the gestures on `post_ids`, by anyone: one per
/// [`GESTURE_QUERY_IDS`] ids, each
/// `{"kinds":[7,45003],"#e":[…],"limit":limit}` — no `authors`, because
/// the reactor is the owner and not the clerk, and no `#h`, because a
/// reaction carries no `h` tag at all (the relay derives its channel from
/// the post). Empty for no ids: an empty `#e` would ask the relay for
/// whatever it takes that to mean.
pub fn gestures_filters(post_ids: &[String], limit: u32) -> Vec<Value> {
    post_ids
        .chunks(GESTURE_QUERY_IDS)
        .map(|ids| {
            serde_json::json!({
                "kinds": [KIND_REACTION, KIND_FORUM_COMMENT],
                "#e": ids,
                "limit": limit,
            })
        })
        .collect()
}

/// The filters that find the clerk's own thread replies on `post_ids`:
/// [`gestures_filters`]'s shape for kind 45003 alone, by `pubkey_hex`.
pub fn own_comments_filters(pubkey_hex: &str, post_ids: &[String], limit: u32) -> Vec<Value> {
    post_ids
        .chunks(GESTURE_QUERY_IDS)
        .map(|ids| {
            serde_json::json!({
                "kinds": [KIND_FORUM_COMMENT],
                "authors": [pubkey_hex],
                "#e": ids,
                "limit": limit,
            })
        })
        .collect()
}

/// The tags of a clerk reply in the thread of `post_id`, answering
/// `gesture_id`: `["h", channel]`, `["e", post_id, "", "reply"]` — Buzz's
/// own direct-reply shape, root and parent being the same event — and
/// `["r", "twalk:gesture:<gesture_id>"]`.
fn comment_tags(channel: &str, post_id: &str, gesture_id: &str) -> Vec<Vec<String>> {
    vec![
        tag("h", channel),
        vec![
            "e".to_owned(),
            post_id.to_owned(),
            String::new(),
            "reply".to_owned(),
        ],
        tag("r", &gesture_reference(gesture_id)),
    ]
}

/// The `Authorization` header value for one `POST` of `body` to `url`:
/// [`nip98_event`], serialised, base64 (standard, padded) after the `Nostr`
/// scheme.
fn nip98_authorization(keys: &Keys, url: &str, body: &[u8]) -> Result<String, RelayError> {
    let event = nip98_event(keys, url, body)?;
    Ok(format!("Nostr {}", B64.encode(event.as_json().as_bytes())))
}

/// A fresh kind-27235 event for one `POST` of `body` to `url`, exactly as
/// Buzz's own CLI builds it: `u` the full URL, `method`, a `nonce` so two
/// requests with one body are two events for the relay's replay guard,
/// `payload` the hex SHA-256 of the body — signed with `keys`.
fn nip98_event(keys: &Keys, url: &str, body: &[u8]) -> Result<Event, RelayError> {
    let payload = format!("{:x}", Sha256::digest(body));
    let nonce = uuid::Uuid::new_v4().to_string();
    let tags = [
        ["u", url],
        ["method", "POST"],
        ["nonce", nonce.as_str()],
        ["payload", payload.as_str()],
    ]
    .into_iter()
    .map(|tag| {
        Tag::parse(tag).map_err(|e| RelayError::Malformed(format!("NIP-98 tag {tag:?}: {e}")))
    })
    .collect::<Result<Vec<_>, _>>()?;
    EventBuilder::new(Kind::Custom(NIP98_KIND), "")
        .tags(tags)
        .sign_with_keys(keys)
        .map_err(|e| RelayError::Malformed(format!("signing the NIP-98 event: {e}")))
}

/// Reads the clerk's key from `path`, in either of the two shapes an operator
/// has: a bare key on the first line that is neither empty nor a `#` comment
/// (64 hex characters or `nsec1…`), or the env-style file
/// `deploy/docker-compose/provision-nostr-key.sh` writes and Hermes reads,
/// with `BUZZ_PRIVATE_KEY=<key>` on a line of its own among other lines.
///
/// Refused before it is read when group or others can read it: the key
/// signs everything the clerk says, and a file the whole host can open is
/// one every other container mounting the same directory can too. The
/// refusal names the `chmod` that fixes it.
pub fn load_keys(path: &Path) -> Result<Keys> {
    let mode = std::fs::metadata(path)
        .with_context(|| format!("reading the clerk's key file {}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        bail!(
            "the clerk's key file {} is readable by group or others (mode {:04o}); it signs \
             everything the clerk posts, so run `chmod 0600 {}` and start again",
            path.display(),
            mode & 0o7777,
            path.display()
        );
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading the clerk's key file {}", path.display()))?;
    let Some(raw) = key_in(&contents) else {
        bail!(
            "the clerk's key file {} holds neither a bare key (64 hex characters or nsec1…, on \
             its first line that is not empty or a # comment) nor a line \
             {KEY_FILE_VARIABLE}=<key> the way deploy/docker-compose/provision-nostr-key.sh \
             writes one",
            path.display()
        );
    };
    Keys::parse(&raw).with_context(|| {
        format!(
            "the clerk's key file {} does not hold a Nostr secret key: expected 64 hex \
             characters or nsec1…, as a bare line or as {KEY_FILE_VARIABLE}=<key>",
            path.display()
        )
    })
}

/// The key `contents` holds, unparsed: a `BUZZ_PRIVATE_KEY=` line anywhere
/// (an `export` allowed, quotes dropped) wins, otherwise the first line that
/// is neither empty nor a comment — provided it is not some other variable's
/// assignment, which is a file of the env shape that simply lacks the key.
fn key_in(contents: &str) -> Option<String> {
    let lines = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let mut first = None;
    for line in lines {
        let assignment = line
            .strip_prefix("export ")
            .map(str::trim_start)
            .unwrap_or(line);
        if let Some(value) = assignment
            .strip_prefix(KEY_FILE_VARIABLE)
            .and_then(|rest| rest.trim_start().strip_prefix('='))
        {
            return Some(unquote(value.trim()).to_owned());
        }
        first.get_or_insert(line);
    }
    first.filter(|line| !line.contains('=')).map(str::to_owned)
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use nostr::nips::nip98::{verify_auth_header, HttpMethod};
    use nostr::{Keys, Kind, Timestamp, ToBech32, Url};
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn nip98_header_signs_the_url_method_and_payload() {
        let keys = Keys::generate();
        let url = "http://127.0.0.1:17800/events";
        let body = br#"{"kind":45001,"content":"a post"}"#;

        // The header, checked the way a relay checks it: `nostr`'s own
        // NIP-98 verifier decodes the base64, parses the event, matches the
        // URL and the method, hashes the body against `payload`, bounds
        // `created_at` and verifies the signature.
        let header = nip98_authorization(&keys, url, body).unwrap();
        assert!(header.starts_with("Nostr "), "{header}");
        let signer = verify_auth_header(
            &header,
            &Url::parse(url).unwrap(),
            HttpMethod::POST,
            Timestamp::now(),
            Some(body),
        )
        .expect("a header the relay would accept");
        assert_eq!(signer, keys.public_key());
        assert!(
            verify_auth_header(
                &header,
                &Url::parse("http://127.0.0.1:17800/query").unwrap(),
                HttpMethod::POST,
                Timestamp::now(),
                Some(body),
            )
            .is_err(),
            "the header is bound to the URL it was made for"
        );

        // The event itself, tag by tag.
        let event = nip98_event(&keys, url, body).unwrap();
        assert_eq!(event.kind, Kind::Custom(27235));
        assert_eq!(event.content, "");
        assert_eq!(event.pubkey, keys.public_key());
        event.verify().expect("signed with the clerk's own keys");

        let tags: Vec<&[String]> = event.tags.iter().map(|t| t.as_slice()).collect();
        assert!(
            tags.contains(&&["u".to_owned(), url.to_owned()][..]),
            "{tags:?}"
        );
        assert!(
            tags.contains(&&["method".to_owned(), "POST".to_owned()][..]),
            "{tags:?}"
        );
        let payload = tags
            .iter()
            .find(|t| t[0] == "payload")
            .map(|t| t[1].as_str())
            .expect("a payload tag");
        assert_eq!(payload.len(), 64);
        assert_eq!(payload, format!("{:x}", Sha256::digest(body)));
        let nonce = tags
            .iter()
            .find(|t| t[0] == "nonce")
            .map(|t| t[1].as_str())
            .expect("a nonce tag, so two requests with one body are two events");
        uuid::Uuid::parse_str(nonce).expect("the nonce is a UUID");

        // Every request is a fresh event: the relay keeps a replay guard on
        // the event id, so the same body signed twice must not collide.
        let again = nip98_authorization(&keys, url, body).unwrap();
        assert_ne!(header, again);
    }

    /// A directory under the temp dir, removed when dropped — on a failing
    /// assertion as much as on a passing test, so a red run does not leave
    /// `twalk-clerk-relay-*` behind.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("twalk-clerk-relay-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl std::ops::Deref for ScratchDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    fn write_key(dir: &Path, name: &str, contents: &str, mode: u32) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn load_keys_reads_hex_and_nsec_and_refuses_a_readable_file() {
        let keys = Keys::generate();
        let hex = keys.secret_key().to_secret_hex();
        let nsec = keys.secret_key().to_bech32().unwrap();
        let dir = ScratchDir::new();

        let hex_path = write_key(&dir, "hex.key", &format!("{hex}\n"), 0o600);
        assert_eq!(
            load_keys(&hex_path).unwrap().public_key(),
            keys.public_key()
        );

        let nsec_path = write_key(&dir, "nsec.key", &format!("  {nsec}  \n\n"), 0o600);
        assert_eq!(
            load_keys(&nsec_path).unwrap().public_key(),
            keys.public_key()
        );

        for mode in [0o640, 0o604, 0o644, 0o660] {
            let path = write_key(&dir, &format!("open-{mode:o}.key"), &hex, mode);
            let err = load_keys(&path).unwrap_err().to_string();
            assert!(
                err.contains(&format!("chmod 0600 {}", path.display())),
                "{mode:o}: {err}"
            );
        }

        let garbage = write_key(&dir, "garbage.key", "not a key\n", 0o600);
        let err = load_keys(&garbage).unwrap_err().to_string();
        assert!(err.contains(&garbage.display().to_string()), "{err}");

        let empty = write_key(&dir, "empty.key", "", 0o600);
        assert!(load_keys(&empty).is_err());

        let err = load_keys(&dir.join("missing.key")).unwrap_err().to_string();
        assert!(err.contains("missing.key"), "{err}");
    }

    #[test]
    #[allow(non_snake_case)]
    fn load_keys_reads_an_env_file_holding_BUZZ_PRIVATE_KEY() {
        let keys = Keys::generate();
        let hex = keys.secret_key().to_secret_hex();
        let nsec = keys.secret_key().to_bech32().unwrap();
        let dir = ScratchDir::new();

        // The shape `deploy/docker-compose/provision-nostr-key.sh` writes,
        // and the one Hermes reads: comments, other variables, and the key
        // on a line of its own, not necessarily the first.
        let env_file = format!(
            "# written by provision-nostr-key.sh\nBUZZ_RELAY_URL=http://127.0.0.1:17800\n\n\
             BUZZ_PRIVATE_KEY={hex}\nBUZZ_PUBLIC_KEY={}\n",
            keys.public_key().to_hex()
        );
        let path = write_key(&dir, "hermes.env", &env_file, 0o600);
        assert_eq!(load_keys(&path).unwrap().public_key(), keys.public_key());

        for line in [
            format!("BUZZ_PRIVATE_KEY=\"{nsec}\""),
            format!("BUZZ_PRIVATE_KEY='{hex}'"),
            format!("export BUZZ_PRIVATE_KEY={hex}"),
        ] {
            let path = write_key(&dir, "quoted.env", &format!("{line}\n"), 0o600);
            assert_eq!(
                load_keys(&path).unwrap().public_key(),
                keys.public_key(),
                "{line}"
            );
        }

        // A comment above a bare key is still a bare key.
        let commented = write_key(
            &dir,
            "commented.key",
            &format!("# the clerk's key\n{hex}\n"),
            0o600,
        );
        assert_eq!(
            load_keys(&commented).unwrap().public_key(),
            keys.public_key()
        );

        // An env file that names every variable but the key is refused with
        // both accepted shapes spelled out, so the operator can see which
        // one they meant to write.
        let without = write_key(
            &dir,
            "without.env",
            "BUZZ_RELAY_URL=http://127.0.0.1:17800\nBUZZ_PUBLIC_KEY=abc\n",
            0o600,
        );
        let err = load_keys(&without).unwrap_err().to_string();
        assert!(
            err.contains("BUZZ_PRIVATE_KEY=") && err.contains("nsec"),
            "{err}"
        );
        assert!(err.contains(&without.display().to_string()), "{err}");
    }

    #[test]
    fn own_posts_filter_names_the_author_and_channel() {
        let filter = own_posts_filter(
            "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d",
            "9b1ba94a-38c3-49fe-9eb0-ffaafa62571a",
            45001,
            200,
        );
        assert_eq!(
            filter,
            serde_json::json!({
                "kinds": [45001],
                "#h": ["9b1ba94a-38c3-49fe-9eb0-ffaafa62571a"],
                "authors": ["3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"],
                "limit": 200,
            })
        );
    }

    #[test]
    fn the_base_url_is_stored_without_a_trailing_slash() {
        let keys = Keys::generate();
        let relay = Relay::new("http://127.0.0.1:17800/", keys.clone()).unwrap();
        assert_eq!(relay.base(), "http://127.0.0.1:17800");
        assert_eq!(relay.public_key_hex(), keys.public_key().to_hex());
    }

    #[test]
    fn own_lines_filter_names_the_bus_event_as_well() {
        let filter = own_lines_filter(
            "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d",
            "29a57768-7513-43bc-9cc2-6915453467f4",
            "57f0e4d352d1ba5e6bf0e92223634253cd852e3d7d018ea91025dc098c1a564a",
            1000,
        );
        assert_eq!(
            filter,
            serde_json::json!({
                "kinds": [9],
                "#h": ["29a57768-7513-43bc-9cc2-6915453467f4"],
                "#r": ["twalk:event:57f0e4d352d1ba5e6bf0e92223634253cd852e3d7d018ea91025dc098c1a564a"],
                "authors": ["3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"],
                "limit": 1000,
            })
        );
    }

    #[test]
    fn a_line_is_recognised_by_its_r_tag_and_not_by_its_content() {
        let keys = Keys::generate();
        let id = "57f0e4d352d1ba5e6bf0e92223634253cd852e3d7d018ea91025dc098c1a564a";
        let other = "0000000000000000000000000000000000000000000000000000000000000000";
        let line = |tags: Vec<Vec<String>>, content: &str| {
            EventBuilder::new(Kind::Custom(KIND_STREAM_MESSAGE), content)
                .tags(
                    tags.iter()
                        .map(|t| Tag::parse(t.iter().map(String::as_str)).unwrap()),
                )
                .sign_with_keys(&keys)
                .unwrap()
        };
        let about_id = line(
            vec![tag("h", "chan"), tag("r", &event_reference(id))],
            "Sent · WhatsApp",
        );
        // A line about another event whose *content* names this one is not
        // a line about this one: the tag decides, never the text.
        let about_other = line(
            vec![tag("h", "chan"), tag("r", &event_reference(other))],
            &format!("Sent · WhatsApp · twalk:event:{id}"),
        );
        let untagged = line(vec![tag("h", "chan")], &format!("twalk:event:{id}"));
        assert!(references_event(&about_id, id));
        assert!(!references_event(&about_other, id));
        assert!(references_event(&about_other, other));
        assert!(!references_event(&untagged, id));
    }

    /// Sixty-four hex characters that differ in their last two: a post id
    /// the `nostr` crate standardises as an `e` tag, which is what the relay
    /// will be handed.
    fn post_id(n: usize) -> String {
        format!("{:0>64x}", n + 1)
    }

    /// A signed event of `kind` with `tags`, for the pure readers below —
    /// signed because [`Event`] cannot be built any other way, by a key
    /// that is nobody's.
    fn event_of(kind: u16, tags: Vec<Vec<String>>, content: &str) -> Event {
        EventBuilder::new(Kind::Custom(kind), content)
            .tags(
                tags.iter()
                    .map(|t| Tag::parse(t.iter().map(String::as_str)).unwrap()),
            )
            .sign_with_keys(&Keys::generate())
            .unwrap()
    }

    #[test]
    fn gestures_filter_names_both_kinds_and_every_id() {
        let ids = vec![post_id(0), post_id(1), post_id(2)];
        let filters = gestures_filters(&ids, 500);
        assert_eq!(
            filters,
            vec![serde_json::json!({
                "kinds": [7, 45003],
                "#e": [post_id(0), post_id(1), post_id(2)],
                "limit": 500,
            })]
        );

        // The clerk's own replies: the same `#e`, one kind, and its key.
        let own = own_comments_filters(
            "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d",
            &ids,
            500,
        );
        assert_eq!(
            own,
            vec![serde_json::json!({
                "kinds": [45003],
                "authors": ["3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"],
                "#e": [post_id(0), post_id(1), post_id(2)],
                "limit": 500,
            })]
        );

        // No ids is no filter at all, never a filter with an empty `#e`,
        // whose meaning is the relay's to choose.
        assert!(gestures_filters(&[], 500).is_empty());
        assert!(own_comments_filters("ab", &[], 500).is_empty());
    }

    #[test]
    fn gestures_are_queried_in_chunks_of_100() {
        let ids: Vec<String> = (0..250).map(post_id).collect();
        let filters = gestures_filters(&ids, 1000);
        assert_eq!(filters.len(), 3, "{filters:?}");
        let sizes: Vec<usize> = filters
            .iter()
            .map(|f| f["#e"].as_array().unwrap().len())
            .collect();
        assert_eq!(sizes, vec![100, 100, 50]);
        // Every id, once, in order; every filter both kinds and the limit.
        let named: Vec<String> = filters
            .iter()
            .flat_map(|f| f["#e"].as_array().unwrap().iter())
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(named, ids);
        for filter in &filters {
            assert_eq!(filter["kinds"], serde_json::json!([7, 45003]));
            assert_eq!(filter["limit"], serde_json::json!(1000));
        }
        // Exactly 100 is one chunk, 101 is two.
        assert_eq!(gestures_filters(&ids[..100], 10).len(), 1);
        assert_eq!(gestures_filters(&ids[..101], 10).len(), 2);
    }

    #[test]
    fn comment_tags_are_h_e_reply_r() {
        let tags = comment_tags(
            "9b1ba94a-38c3-49fe-9eb0-ffaafa62571a",
            &post_id(0),
            &post_id(7),
        );
        assert_eq!(
            tags,
            vec![
                vec![
                    "h".to_owned(),
                    "9b1ba94a-38c3-49fe-9eb0-ffaafa62571a".to_owned()
                ],
                vec![
                    "e".to_owned(),
                    post_id(0),
                    String::new(),
                    "reply".to_owned()
                ],
                vec!["r".to_owned(), format!("twalk:gesture:{}", post_id(7))],
            ]
        );
        // What the tags say, read back by the same module's readers: a
        // direct reply to the post, answering that gesture.
        let comment = event_of(KIND_FORUM_COMMENT, tags, "Envoyé.");
        assert!(is_direct_reply(&comment, &post_id(0)));
        assert!(targets(&comment, &post_id(0)));
        assert_eq!(answered_gesture(&comment), Some(post_id(7).as_str()));
    }

    #[test]
    fn targets_reads_the_last_e_tag() {
        let post = post_id(0);
        let other = post_id(1);
        let reaction = event_of(KIND_REACTION, vec![tag("e", &post)], "✅");
        assert!(targets(&reaction, &post));
        assert!(!targets(&reaction, &other));

        // A reaction carrying two `e` tags (a client quoting what it reacts
        // to): Buzz resolves the target from the last one, so this module
        // does the same, or the two would disagree about which post was
        // reacted to.
        let two = event_of(KIND_REACTION, vec![tag("e", &other), tag("e", &post)], "✅");
        assert!(targets(&two, &post));
        assert!(!targets(&two, &other));

        // A nested reply's last `e` is its parent, marked `reply`; its first
        // is the root. The reply targets the parent.
        let nested = event_of(
            KIND_FORUM_COMMENT,
            vec![
                tag("h", "chan"),
                vec![
                    "e".to_owned(),
                    post.clone(),
                    String::new(),
                    "root".to_owned(),
                ],
                vec![
                    "e".to_owned(),
                    other.clone(),
                    String::new(),
                    "reply".to_owned(),
                ],
            ],
            "…",
        );
        assert!(targets(&nested, &other));
        assert!(!targets(&nested, &post));

        // No `e` tag targets nothing; and a kind that is not a gesture — a
        // delete names a post in an `e` tag too — is not one.
        let untagged = event_of(KIND_REACTION, vec![], "✅");
        assert!(!targets(&untagged, &post));
        let delete = event_of(KIND_DELETE, vec![tag("h", "chan"), tag("e", &post)], "");
        assert!(!targets(&delete, &post));
    }

    #[test]
    fn is_direct_reply_refuses_a_nested_reply() {
        let post = post_id(0);
        let parent = post_id(1);
        let e = |id: &str, marker: &str| {
            vec![
                "e".to_owned(),
                id.to_owned(),
                String::new(),
                marker.to_owned(),
            ]
        };
        let direct = event_of(
            KIND_FORUM_COMMENT,
            vec![tag("h", "chan"), e(&post, "reply")],
            "…",
        );
        assert!(is_direct_reply(&direct, &post));
        assert!(!is_direct_reply(&direct, &parent));

        // The bare shape a client that writes no marker produces.
        let bare = event_of(
            KIND_FORUM_COMMENT,
            vec![tag("h", "chan"), tag("e", &post)],
            "…",
        );
        assert!(is_direct_reply(&bare, &post));

        // A nested reply under the post — root marked, parent marked — is
        // a reply to the parent, not to the post, and not "direct" to
        // either: two `e` tags.
        let nested = event_of(
            KIND_FORUM_COMMENT,
            vec![tag("h", "chan"), e(&post, "root"), e(&parent, "reply")],
            "…",
        );
        assert!(!is_direct_reply(&nested, &post));
        assert!(!is_direct_reply(&nested, &parent));

        // One `e` tag marked `root` alone is not a direct reply either.
        let rooted = event_of(
            KIND_FORUM_COMMENT,
            vec![tag("h", "chan"), e(&post, "root")],
            "…",
        );
        assert!(!is_direct_reply(&rooted, &post));

        // A reaction with the very tag a bare reply has is not a reply.
        let reaction = event_of(KIND_REACTION, vec![tag("e", &post)], "✅");
        assert!(!is_direct_reply(&reaction, &post));
    }

    #[test]
    fn an_event_whose_signature_does_not_verify_is_dropped_and_counted() {
        let post = post_id(0);
        let genuine = event_of(KIND_REACTION, vec![tag("e", &post)], "✅");
        assert!(genuine.verify().is_ok());

        // The same JSON object with one byte of its signature changed: the
        // relay could serve this, and nothing but the signature says no.
        let mut value: Value = serde_json::from_str(&genuine.as_json()).unwrap();
        let sig = value["sig"].as_str().unwrap().to_owned();
        let flipped = if sig.starts_with('0') { "1" } else { "0" };
        value["sig"] = Value::String(format!("{flipped}{}", &sig[1..]));
        let tampered_sig = Event::from_json(value.to_string()).unwrap();

        // …with its `pubkey` swapped for another key's and nothing else:
        // the id, a hash over the pubkey, no longer matches — a stranger's ✅
        // served as the owner's, lazily.
        let owner = Keys::generate().public_key();
        let mut value: Value = serde_json::from_str(&genuine.as_json()).unwrap();
        value["pubkey"] = Value::String(owner.to_hex());
        let stale_id = Event::from_json(value.to_string()).unwrap();

        // …and the intelligent forgery: the pubkey swapped **and** the id
        // recomputed over it, so the id check passes and only the
        // signature — which the forger cannot make without the owner's
        // secret key — says no.
        let recomputed = nostr::EventId::new(
            &owner,
            &genuine.created_at,
            &genuine.kind,
            &genuine.tags,
            &genuine.content,
        );
        value["id"] = Value::String(recomputed.to_hex());
        let forged = Event::from_json(value.to_string()).unwrap();
        assert!(forged.verify_id(), "the forged id is consistent");

        let (kept, rejected) = verified(vec![
            tampered_sig.clone(),
            genuine.clone(),
            stale_id.clone(),
            forged.clone(),
        ]);
        assert_eq!(kept, vec![genuine.clone()]);
        assert_eq!(
            rejected,
            vec![
                Rejected {
                    id: tampered_sig.id,
                    kind: KIND_REACTION,
                    error: nostr::event::Error::InvalidSignature,
                },
                Rejected {
                    id: stale_id.id,
                    kind: KIND_REACTION,
                    error: nostr::event::Error::InvalidId,
                },
                Rejected {
                    id: forged.id,
                    kind: KIND_REACTION,
                    error: nostr::event::Error::InvalidSignature,
                },
            ]
        );

        let (kept, rejected) = verified(vec![genuine.clone()]);
        assert!(rejected.is_empty());
        assert_eq!(kept, vec![genuine]);
        let (kept, rejected) = verified(Vec::new());
        assert!(rejected.is_empty());
        assert!(kept.is_empty());
    }

    #[test]
    fn answered_gesture_reads_the_r_tag() {
        let gesture = post_id(7);
        let answer = event_of(
            KIND_FORUM_COMMENT,
            comment_tags("chan", &post_id(0), &gesture),
            "…",
        );
        assert_eq!(answered_gesture(&answer), Some(gesture.as_str()));

        // The owner's own reply carries no `r` tag; a line about a bus
        // event carries an `r` tag of another prefix; a content that
        // spells the reference is not a tag.
        let owner = event_of(
            KIND_FORUM_COMMENT,
            vec![tag("h", "chan"), tag("e", &post_id(0))],
            &format!("twalk:gesture:{gesture}"),
        );
        assert_eq!(answered_gesture(&owner), None);
        let line = event_of(
            KIND_STREAM_MESSAGE,
            vec![tag("h", "chan"), tag("r", &event_reference(&gesture))],
            "…",
        );
        assert_eq!(answered_gesture(&line), None);
        let empty = event_of(
            KIND_FORUM_COMMENT,
            vec![tag("h", "chan"), tag("r", GESTURE_PREFIX)],
            "…",
        );
        assert_eq!(answered_gesture(&empty), None);
    }

    #[test]
    fn a_rate_limit_and_a_server_error_are_transient_and_a_refusal_is_not() {
        let refused = |status| RelayError::Refused {
            status,
            reason: String::new(),
        };
        assert!(RelayError::Unreachable("connection refused".into()).is_transient());
        assert!(refused(429).is_transient());
        assert!(refused(500).is_transient());
        assert!(refused(503).is_transient());
        assert!(!refused(400).is_transient());
        assert!(!refused(401).is_transient());
        assert!(!refused(403).is_transient());
        assert!(!RelayError::Malformed("not JSON".into()).is_transient());
    }

    #[test]
    fn a_refusal_keeps_the_relays_reason_cut_short_and_nothing_of_another_body() {
        // The relay's own shape: `{"error": "…"}` (`api_error`), or the
        // `message` of a `/events` answer.
        let err = refusal(
            400,
            r#"{"error":"invalid: event timestamp too far from server time"}"#,
        );
        assert_eq!(
            err.to_string(),
            "the relay refused the request with HTTP 400: invalid: event timestamp too far from \
             server time"
        );
        let err = refusal(429, r#"{"message":"rate limited: retry in 12s"}"#);
        assert_eq!(
            err.to_string(),
            "the relay refused the request with HTTP 429: rate limited: retry in 12s"
        );

        // A long reason is cut at REASON_CHARS.
        let long = "x".repeat(1000);
        let err = refusal(400, &format!(r#"{{"error":"{long}"}}"#));
        let shown = err.to_string();
        assert!(shown.contains(&"x".repeat(120)), "{shown}");
        assert!(!shown.contains(&"x".repeat(121)), "{shown}");
        assert!(shown.ends_with("… [cut]"), "{shown}");

        // A body of any other shape — a proxy's HTML page, a bare string,
        // a JSON object whose reason is not a string — contributes nothing.
        for body in [
            "<html><body>MARKER-502 Bad Gateway</body></html>",
            "MARKER-plain",
            r#"{"error":{"nested":"MARKER-nested"}}"#,
            r#"["MARKER-array"]"#,
            "",
        ] {
            let err = refusal(502, body);
            assert_eq!(
                err.to_string(),
                "the relay refused the request with HTTP 502",
                "{body}"
            );
        }
    }

    #[test]
    fn a_2xx_body_that_is_not_the_answer_is_described_and_never_quoted() {
        // What `/query` answers with is the clerk's own posts, which quote
        // suggestions: a body that fails to parse must not end up in the
        // error, and through it in the log.
        let marker = "MARKER-a-suggestions-words-7c1e";
        let body = format!(r#"[{{"content":"{marker}","kind":9}}, not json"#);
        let parse_error = serde_json::from_str::<Vec<Event>>(&body).unwrap_err();
        let err = malformed_answer(200, "http://127.0.0.1:17800/query", &body, &parse_error);
        let shown = err.to_string();
        assert!(!shown.contains(marker), "{shown}");
        assert!(!shown.contains("content"), "{shown}");
        assert!(
            shown.contains("HTTP 200 from http://127.0.0.1:17800/query"),
            "{shown}"
        );
        assert!(shown.contains("at line 1 column"), "{shown}");
        assert!(
            shown.contains(&format!("a body of {} bytes", body.len())),
            "{shown}"
        );
        assert!(!err.is_transient());
    }
}
