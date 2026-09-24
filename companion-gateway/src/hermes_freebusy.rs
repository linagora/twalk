//! Hermes reads the owner's free/busy (ticket #281, ADR 0032's founding
//! example: a WhatsApp message that needs a calendar looked at before three
//! times are proposed). **The one governed pull.** Everything else Hermes
//! knows was pushed to it through a persona's wake, after the consent gate;
//! this is the one thing it asks for, and the reasons it may are the shape
//! of this module.
//!
//! # Who may ask
//!
//! Hermes, with the **same secret as its answers**
//! (`GATEWAY_HERMES_ANSWER_SECRET`): one fact for the operator to hold about
//! the seam, and one credential that opens exactly two routes on this origin.
//! A `GET` has no body to sign, so the signature is HMAC-SHA256 over a
//! canonical line — the method, the path, the query string as sent and a
//! timestamp, newline-separated — carried as `X-Hermes-Signature-256:
//! sha256=<hex>` beside `X-Hermes-Timestamp`. The timestamp is inside the
//! signed bytes and must be within five minutes of this clock
//! ([`crate::hermes_answer::CLOCK_SKEW_SECONDS`], the same number as the
//! answers'), which is what stops a captured request from being replayed
//! next week. The signature authenticates the sender and nothing more: the
//! window is checked here, the connection's state is read from this
//! Gateway's own store, and the agenda is read by the collector.
//!
//! # What may be asked
//!
//! A window of at most **fourteen days** (`window_too_wide` otherwise): a
//! persona proposing times needs the next few days, and a reader that could
//! ask for a year would be reading the owner's life, one call at a time. A
//! connection that is not `connected` — or that no collector has spoken for
//! — is refused with its state, the same refusal an approval towards it
//! gets (#275), because a read of an agenda the collector cannot reach is a
//! read of nothing dressed as a read of something.
//!
//! # What comes back
//!
//! Busy intervals and nothing else: `[{start, end}]`, clipped to the window,
//! merged. No title, no participant, no location, because the collector
//! runs a `free-busy-query` that carries none, and this module relays the
//! collector's answer without adding to it. The relay is over internal HTTP
//! to the collector (`GATEWAY_COLLECTOR_URL`, with this Gateway's own service
//! token as the bearer — the snapshot seam, in the other direction), so the
//! one process that holds the owner's grant is the one that reads the
//! agenda; this Gateway never sees the side service.
//!
//! # What is kept
//!
//! **Every read is recorded**, served or refused: the connection, the window,
//! when, which delivery, and the outcome (`hermes_read` in the store,
//! `twalk_companion_gateway_hermes_reads_total{outcome}` on `/metrics`). A
//! pull that left no trace would be the one thing in this deployment the
//! owner could not audit, and the Companion shows the record on the Hermes
//! card in a later lot.
//!
//! The pure half is here — the canonical line, the signature, the window,
//! the refusals — and the I/O in [`Reads`]: the store, the relay, the clock.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::{info, warn};

use crate::approval::{connection_can_send, Refusal};
use crate::hermes_answer::{is_fresh, signature_matches, CLOCK_SKEW_SECONDS, SIGNATURE_HEADER};
use crate::metrics::Metrics;
use crate::store::Store;

/// The path this module answers on, beside the answers' under the reserved
/// `/_twalk/` prefix.
pub const FREEBUSY_PATH: &str = "/_twalk/hermes/freebusy";

/// Where a persona asks what one event carries (#355), beside the read
/// above and under the same reserved prefix.
pub const EVENT_FACTS_PATH: &str = "/_twalk/hermes/event-facts";

/// The header the request's timestamp travels in, RFC 3339; inside the
/// signed line.
pub const TIMESTAMP_HEADER: &str = "X-Hermes-Timestamp";

/// The header naming one attempt, so the record can tell a retry from a
/// second read. Optional: a read without one is recorded with none.
pub const DELIVERY_HEADER: &str = "X-Hermes-Delivery";

/// The widest window a read may ask for.
pub const MAX_WINDOW_SECONDS: i64 = 14 * 86_400;

/// How long the relay waits for the collector.
pub const COLLECTOR_TIMEOUT: Duration = Duration::from_secs(20);

/// The most of any member of an unverified request the record keeps: the
/// record holds what was asked, bad signature or not, and what was asked
/// is whatever reached the port, so it is cut before it is stored.
pub const RECORDED_MEMBER_LENGTH: usize = 256;

/// The line the signature covers: method, path, query as sent, timestamp.
/// The query is signed **as sent** and not re-encoded, since a canonical
/// re-encoding on either side is a second implementation to keep equal.
pub fn canonical(method: &str, path: &str, query: &str, timestamp: &str) -> String {
    format!("{method}\n{path}\n{query}\n{timestamp}")
}

/// `sha256=<hex>` over the canonical line.
pub fn sign(secret: &str, canonical_line: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(canonical_line.as_bytes());
    format!("sha256={:x}", mac.finalize().into_bytes())
}

/// One request as the route received it: the query string as sent, its
/// parsed members, and the headers this module reads.
#[derive(Debug, Clone, Default)]
pub struct ReadRequest {
    pub query: String,
    pub connection: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub timestamp: Option<String>,
    pub signature: Option<String>,
    pub delivery: Option<String>,
    /// The event a read of #355's kind names. `None` on a free/busy read,
    /// which asks about a window instead.
    pub uid: Option<String>,
}

/// One busy interval, as the collector answers it and as Hermes reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Busy {
    pub start: String,
    pub end: String,
}

/// A window checked: two instants in order, at most [`MAX_WINDOW_SECONDS`]
/// apart. Kept as the strings the caller sent, since the collector parses
/// them again and the record keeps them as asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub from: String,
    pub to: String,
}

impl Window {
    pub fn parse(from: Option<&str>, to: Option<&str>) -> Result<Self, ReadRefusal> {
        let (Some(from), Some(to)) = (from, to) else {
            return Err(ReadRefusal::InvalidWindow(
                "`from` and `to` are both required, as RFC 3339 instants".to_owned(),
            ));
        };
        // A space is what a `+` becomes when a client leaves it unencoded
        // in the query; read as an instant it would be silently shifted by
        // its own offset, so it is refused instead.
        if from.contains(' ') || to.contains(' ') {
            return Err(ReadRefusal::InvalidWindow(
                "`from` and `to` must be percent-encoded: a `+` in an offset reaches this \
                 Gateway as a space"
                    .to_owned(),
            ));
        }
        let start = crate::hermes_answer::parse_rfc3339_seconds(from).ok_or_else(|| {
            ReadRefusal::InvalidWindow(format!("`from` is not an RFC 3339 instant: {from:?}"))
        })?;
        let end = crate::hermes_answer::parse_rfc3339_seconds(to).ok_or_else(|| {
            ReadRefusal::InvalidWindow(format!("`to` is not an RFC 3339 instant: {to:?}"))
        })?;
        if end <= start {
            return Err(ReadRefusal::InvalidWindow(
                "`to` must be after `from`".to_owned(),
            ));
        }
        if end - start > MAX_WINDOW_SECONDS {
            return Err(ReadRefusal::WindowTooWide);
        }
        Ok(Self {
            from: from.to_owned(),
            to: to.to_owned(),
        })
    }
}

/// Why a read was refused: the code the caller is given, the status, and
/// the sentence. Every variant is also the outcome the read is recorded and
/// counted under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadRefusal {
    /// The Gateway has no seam to Hermes: no secret to verify a read with.
    SeamNotConfigured,
    /// The Gateway has no collector to relay to.
    CollectorNotConfigured,
    /// No signature or no timestamp on the request.
    Unsigned,
    /// The signature does not match the canonical line.
    BadSignature,
    /// The timestamp is outside the clock-skew window.
    Stale { timestamp: String },
    /// `connection` is missing.
    NoConnection,
    /// `connection` names no calendar connection of this deployment.
    ConnectionUnknown(String),
    /// The window is not two instants in order.
    InvalidWindow(String),
    /// `uid` is missing, empty, or longer than an iCalendar UID can be.
    InvalidUid,
    /// Wider than [`MAX_WINDOW_SECONDS`].
    WindowTooWide,
    /// The connection is not `connected`, or no collector has spoken for it.
    ConnectionNotConnected {
        connection: String,
        state: String,
        hint: Option<String>,
    },
    /// The store could not be read.
    StoreUnavailable(String),
    /// The collector did not answer, or answered something that is not a
    /// free/busy answer — one code, since either way the relay found no
    /// collector to speak with.
    CollectorUnreachable(String),
    /// The collector answered a refusal of its own.
    CollectorRefused { status: u16, code: String },
}

impl ReadRefusal {
    pub fn code(&self) -> &'static str {
        match self {
            Self::SeamNotConfigured => "hermes_answers_not_configured",
            Self::CollectorNotConfigured => "collector_not_configured",
            Self::Unsigned => "unsigned",
            Self::BadSignature => "bad_signature",
            Self::Stale { .. } => "stale_timestamp",
            Self::NoConnection => "invalid_request",
            Self::ConnectionUnknown(_) => "connection_unknown",
            Self::InvalidWindow(_) => "invalid_window",
            Self::InvalidUid => "invalid_uid",
            Self::WindowTooWide => "window_too_wide",
            Self::ConnectionNotConnected { .. } => "connection_not_connected",
            Self::StoreUnavailable(_) => "store_unavailable",
            Self::CollectorUnreachable(_) => "collector_unreachable",
            Self::CollectorRefused { .. } => "collector_refused",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            Self::SeamNotConfigured | Self::CollectorNotConfigured | Self::StoreUnavailable(_) => {
                503
            }
            Self::Unsigned | Self::BadSignature | Self::Stale { .. } => 401,
            Self::NoConnection
            | Self::InvalidWindow(_)
            | Self::InvalidUid
            | Self::WindowTooWide => 400,
            Self::ConnectionUnknown(_) => 404,
            Self::ConnectionNotConnected { .. } => 409,
            Self::CollectorUnreachable(_) | Self::CollectorRefused { .. } => 502,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::SeamNotConfigured => {
                "this Gateway has no seam to Hermes: set GATEWAY_HERMES_ANSWER_SECRET, which \
                 signs Hermes's answers and its free/busy reads alike"
                    .to_owned()
            }
            Self::CollectorNotConfigured => {
                "this Gateway relays no free/busy read: set GATEWAY_COLLECTOR_URL to the \
                 collector's internal endpoint (COLLECTOR_HTTP_LISTEN on its side) and \
                 GATEWAY_SERVICE_TOKEN, which the collector accepts"
                    .to_owned()
            }
            Self::Unsigned => format!(
                "the read carries no {SIGNATURE_HEADER} or no {TIMESTAMP_HEADER}. It is \
                 HMAC-SHA256 over `GET\\n{FREEBUSY_PATH}\\n<query as sent>\\n<timestamp>` \
                 with the secret Hermes's answers are signed with"
            ),
            Self::BadSignature => format!(
                "the {SIGNATURE_HEADER} does not match: the line signed is \
                 `GET\\n{FREEBUSY_PATH}\\n<query as sent>\\n<timestamp>`, with the query \
                 string exactly as it was sent"
            ),
            Self::Stale { timestamp } => format!(
                "the {TIMESTAMP_HEADER} {timestamp:?} is not within {CLOCK_SKEW_SECONDS}s of \
                 this clock; a read is signed at the moment it is made"
            ),
            Self::NoConnection => {
                "`connection` is required: the calendar connection to read".to_owned()
            }
            Self::ConnectionUnknown(connection) => format!(
                "{connection:?} is not a calendar connection this deployment declares \
                 (GATEWAY_CONNECTIONS, kind `calendar`); an agenda is read on a calendar \
                 connection and on nothing else"
            ),
            Self::InvalidWindow(detail) => detail.clone(),
            Self::InvalidUid => "uid is the event's iCalendar UID, between 1 and 512 characters, as `calendar.event.*` carried it".to_owned(),
            Self::WindowTooWide => format!(
                "the window is wider than the {} days a free/busy read may ask for",
                MAX_WINDOW_SECONDS / 86_400
            ),
            Self::ConnectionNotConnected {
                connection,
                state,
                hint,
            } => {
                let hint = hint
                    .as_deref()
                    .map(|hint| format!(" {hint}"))
                    .unwrap_or_default();
                format!(
                    "the connection {connection:?} is {state}, so its agenda cannot be read.{hint}"
                )
            }
            Self::StoreUnavailable(detail) => {
                format!("the connection's state could not be read: {detail}")
            }
            // "the read" and not "the free/busy read": since #355 two reads
            // share these refusals, and a sentence naming the wrong one sends
            // an operator to the wrong log. Found by running the new read
            // against the live deployment, where the Gateway answered that a
            // free/busy read had been refused for a read of an event.
            Self::CollectorUnreachable(detail) => {
                format!("the collector did not answer the read: {detail}")
            }
            Self::CollectorRefused { status, code } => {
                format!("the collector refused the read with HTTP {status} {code}")
            }
        }
    }

    /// The `state` a `connection_not_connected` refusal carries, for the
    /// caller to show.
    pub fn state(&self) -> Option<&str> {
        match self {
            Self::ConnectionNotConnected { state, .. } => Some(state),
            _ => None,
        }
    }
}

/// One read, as the store records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HermesRead {
    pub connection: String,
    pub window_from: String,
    pub window_to: String,
    pub requested_at: String,
    pub delivery: Option<String>,
    /// `served`, or the refusal's code.
    pub outcome: String,
    /// How many intervals were answered; `None` on a refusal.
    pub intervals: Option<u64>,
}

/// What one event carries, as the collector answered (#355). Counts, a
/// flag's worth of knowledge and at most one URL — never the description,
/// never an attachment's name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFacts {
    /// Whether this collector holds that event at all. `false` is an
    /// answer, and a different one from "the meeting carries nothing": a
    /// persona learns the deployment has nothing about that uid.
    pub found: bool,
    pub conference: Option<String>,
    pub description_characters: Option<u64>,
    pub attachments: u64,
}

/// One read of what an event carries, as the journal keeps it (#355).
///
/// A sibling of [`HermesRead`] rather than a row in it: the two reads ask
/// different questions — a window of the agenda, one event by its uid — and
/// one table holding both would carry a discriminator and two half-empty
/// groups of columns. What is kept is the same in spirit: who asked, for
/// what, when, and how it went. Never what the answer said, beyond whether
/// the event was held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HermesEventRead {
    pub connection: String,
    /// The uid as asked, cut to [`RECORDED_MEMBER_LENGTH`].
    pub uid: String,
    pub requested_at: String,
    pub delivery: Option<String>,
    /// `served`, or the refusal's code.
    pub outcome: String,
    /// Whether the collector held the event; `None` on a refusal.
    pub found: Option<bool>,
}

/// The I/O half: the secret, the store the connection's state and the
/// record live in, the relay to the collector, and the clock.
pub struct Reads {
    secret: String,
    store: Arc<Store>,
    connections: Arc<crate::connections::Registry>,
    /// The collector's endpoint and the bearer it accepts; `None` when
    /// `GATEWAY_COLLECTOR_URL` is unset.
    collector: Option<(String, String)>,
    http: reqwest::Client,
    metrics: Arc<Metrics>,
    now: fn() -> SystemTime,
}

impl Reads {
    pub fn new(
        secret: String,
        store: Arc<Store>,
        connections: Arc<crate::connections::Registry>,
        collector: Option<(String, String)>,
        metrics: Arc<Metrics>,
        now: fn() -> SystemTime,
    ) -> Self {
        Self {
            secret,
            store,
            connections,
            collector,
            http: reqwest::Client::builder()
                .timeout(COLLECTOR_TIMEOUT)
                .build()
                .expect("a client with a timeout builds"),
            metrics,
            now,
        }
    }

    /// One read: verified, checked, relayed, recorded — the record written
    /// whatever the outcome, since the point of the record is the reads
    /// that were refused as much as the ones that were served.
    pub async fn read(&self, request: &ReadRequest) -> Result<Vec<Busy>, ReadRefusal> {
        let outcome = self.serve(request).await;
        let (label, intervals) = match &outcome {
            Ok(busy) => ("served".to_owned(), Some(busy.len() as u64)),
            Err(refusal) => (refusal.code().to_owned(), None),
        };
        self.metrics.record_hermes_read(match &outcome {
            Ok(_) => "served",
            Err(refusal) => refusal.code(),
        });
        let cut = |member: &Option<String>| {
            member.as_deref().map(|value| {
                value
                    .chars()
                    .take(RECORDED_MEMBER_LENGTH)
                    .collect::<String>()
            })
        };
        let record = HermesRead {
            connection: cut(&request.connection).unwrap_or_default(),
            window_from: cut(&request.from).unwrap_or_default(),
            window_to: cut(&request.to).unwrap_or_default(),
            requested_at: crate::hermes_answer::rfc3339_seconds((self.now)()),
            delivery: cut(&request.delivery),
            outcome: label,
            intervals,
        };
        if let Err(error) = self.store.record_hermes_read(&record) {
            // Said loudly, and the read still answered: a record that could
            // not be written is an operator's problem to see in the log,
            // not a reason to tell Hermes the agenda is unreadable.
            warn!(%error, "a free/busy read could not be recorded");
        }
        match &outcome {
            Ok(busy) => info!(
                connection = %record.connection,
                from = %record.window_from,
                to = %record.window_to,
                delivery = record.delivery.as_deref(),
                intervals = busy.len(),
                "hermes read the owner's free/busy"
            ),
            Err(refusal) => warn!(
                code = refusal.code(),
                status = refusal.status(),
                connection = %record.connection,
                delivery = record.delivery.as_deref(),
                detail = %refusal.message(),
                "a free/busy read was refused"
            ),
        }
        outcome
    }

    /// What both of Hermes's reads must carry before anything else is
    /// looked at: a signature over **this read's own** canonical line, a
    /// fresh timestamp, and the name of a connection. Answers that name.
    ///
    /// One function because a second copy of this is a second place for the
    /// signature check to drift, and a signature check that is right in one
    /// of two copies is worth nothing.
    fn signed_for(&self, request: &ReadRequest, path: &str) -> Result<String, ReadRefusal> {
        let (Some(timestamp), Some(signature)) = (&request.timestamp, &request.signature) else {
            return Err(ReadRefusal::Unsigned);
        };
        let line = canonical("GET", path, &request.query, timestamp);
        if !signature_matches(&self.secret, Some(signature), line.as_bytes()) {
            return Err(ReadRefusal::BadSignature);
        }
        if !is_fresh(timestamp, (self.now)()) {
            return Err(ReadRefusal::Stale {
                timestamp: timestamp.clone(),
            });
        }
        let connection = request
            .connection
            .as_deref()
            .filter(|connection| !connection.is_empty())
            .ok_or(ReadRefusal::NoConnection)?;
        Ok(connection.to_owned())
    }

    /// The connection itself, judged **after** the read's own parameters
    /// have been: it must be a calendar of this deployment, it must be
    /// connected, and there must be a collector to relay to.
    ///
    /// The order is deliberate and is not free. A caller who asked for a
    /// fortnight and a second is told their window is impossible whichever
    /// connection they named; telling them instead that the connection is
    /// unknown sends them to fix the wrong thing. The conformance suite
    /// holds this order, which is how a refactor that quietly reversed it
    /// was caught.
    fn connection_ready(&self, connection: &str) -> Result<(String, String), ReadRefusal> {
        // A calendar connection of this deployment, and no other: an agenda
        // is what a calendar connection is, and a read on a mail or a
        // bridged connection would be a read of nothing — or, worse, of
        // whatever the collector answered for a name it did not check.
        let a_calendar = self
            .connections
            .get(connection)
            .is_some_and(|known| known.kind == "calendar");
        if !a_calendar {
            return Err(ReadRefusal::ConnectionUnknown(connection.to_owned()));
        }
        match connection_can_send(&self.store, &self.connections, connection) {
            Ok(()) => {}
            Err(Refusal::ConnectionNotConnected {
                connection,
                state,
                hint,
            }) => {
                return Err(ReadRefusal::ConnectionNotConnected {
                    connection,
                    state,
                    hint,
                })
            }
            Err(Refusal::StoreUnavailable(detail)) => {
                return Err(ReadRefusal::StoreUnavailable(detail))
            }
            // `connection_can_send` produces the two refusals above and no
            // other; a third would be a store this module cannot read.
            Err(other) => return Err(ReadRefusal::StoreUnavailable(other.message())),
        }
        let Some((collector_url, service_token)) = &self.collector else {
            return Err(ReadRefusal::CollectorNotConfigured);
        };
        Ok((collector_url.clone(), service_token.clone()))
    }

    /// One relayed read of the collector's internal endpoint: the route,
    /// the query it takes, and the answer as JSON. The status and the
    /// collector's own error code come back as this module's refusals, so
    /// a persona is never handed the collector's shape.
    async fn relay(
        &self,
        collector_url: &str,
        service_token: &str,
        route: &str,
        query: &[(&str, &str)],
    ) -> Result<serde_json::Value, ReadRefusal> {
        let response = self
            .http
            .get(format!("{}/{route}", collector_url.trim_end_matches('/')))
            .query(query)
            .bearer_auth(service_token)
            .send()
            .await
            .map_err(|error| ReadRefusal::CollectorUnreachable(error.to_string()))?;
        let status = response.status().as_u16();
        let body: serde_json::Value = response.json().await.map_err(|error| {
            ReadRefusal::CollectorUnreachable(format!("its answer is not JSON: {error}"))
        })?;
        if status != 200 {
            return Err(ReadRefusal::CollectorRefused {
                status,
                code: body["error"].as_str().unwrap_or("unknown").to_owned(),
            });
        }
        Ok(body)
    }

    /// What one of the owner's events carries (#355): relayed to the
    /// collector, recorded, counted, and answered.
    ///
    /// The same governance as the free/busy read above, because it is the
    /// same kind of pull — a persona reaching into the owner's own data
    /// through a seam the owner can audit. What it may learn is narrower:
    /// facts about an event, never its words.
    pub async fn event_facts(&self, request: &ReadRequest) -> Result<EventFacts, ReadRefusal> {
        let outcome = self.serve_event_facts(request).await;
        self.metrics.record_hermes_event_read(match &outcome {
            Ok(_) => "served",
            Err(refusal) => refusal.code(),
        });
        let cut = |member: &Option<String>| {
            member.as_deref().map(|value| {
                value
                    .chars()
                    .take(RECORDED_MEMBER_LENGTH)
                    .collect::<String>()
            })
        };
        let record = HermesEventRead {
            connection: cut(&request.connection).unwrap_or_default(),
            uid: cut(&request.uid).unwrap_or_default(),
            requested_at: crate::hermes_answer::rfc3339_seconds((self.now)()),
            delivery: cut(&request.delivery),
            outcome: match &outcome {
                Ok(_) => "served".to_owned(),
                Err(refusal) => refusal.code().to_owned(),
            },
            found: outcome.as_ref().ok().map(|facts| facts.found),
        };
        if let Err(error) = self.store.record_hermes_event_read(&record) {
            // As the free/busy read's: said loudly, and the read still
            // answered. A record that could not be written is an operator's
            // problem to see, not a reason to tell Hermes nothing.
            warn!(%error, "a read of an event's facts could not be recorded");
        }
        match &outcome {
            Ok(facts) => info!(
                connection = %record.connection,
                uid = %record.uid,
                delivery = record.delivery.as_deref(),
                found = facts.found,
                conference = facts.conference.is_some(),
                attachments = facts.attachments,
                "hermes read what one of the owner's events carries"
            ),
            Err(refusal) => warn!(
                code = refusal.code(),
                status = refusal.status(),
                connection = %record.connection,
                uid = %record.uid,
                delivery = record.delivery.as_deref(),
                detail = %refusal.message(),
                "a read of an event's facts was refused"
            ),
        }
        outcome
    }

    async fn serve_event_facts(&self, request: &ReadRequest) -> Result<EventFacts, ReadRefusal> {
        let connection = self.signed_for(request, EVENT_FACTS_PATH)?;
        let uid = request
            .uid
            .as_deref()
            .map(str::trim)
            .filter(|uid| !uid.is_empty() && uid.chars().count() <= 512)
            .ok_or(ReadRefusal::InvalidUid)?;
        let (collector_url, service_token) = self.connection_ready(&connection)?;
        let body = self
            .relay(
                &collector_url,
                &service_token,
                "event-facts",
                &[("connection", connection.as_str()), ("uid", uid)],
            )
            .await?;
        serde_json::from_value(body.clone()).map_err(|error| {
            ReadRefusal::CollectorUnreachable(format!(
                "its answer carries no facts about the event: {error}"
            ))
        })
    }

    async fn serve(&self, request: &ReadRequest) -> Result<Vec<Busy>, ReadRefusal> {
        let connection = self.signed_for(request, FREEBUSY_PATH)?;
        let window = Window::parse(request.from.as_deref(), request.to.as_deref())?;
        let (collector_url, service_token) = self.connection_ready(&connection)?;
        let connection = connection.as_str();
        let body = self
            .relay(
                &collector_url,
                &service_token,
                "freebusy",
                &[
                    ("connection", connection),
                    ("from", window.from.as_str()),
                    ("to", window.to.as_str()),
                ],
            )
            .await?;
        serde_json::from_value(body["busy"].clone()).map_err(|error| {
            ReadRefusal::CollectorUnreachable(format!(
                "its answer carries no busy intervals: {error}"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{canonical, sign, ReadRefusal, Window, FREEBUSY_PATH};

    #[test]
    fn the_signature_is_hmac_sha256_over_the_canonical_line() {
        // $ printf 'GET\n/_twalk/hermes/freebusy\nconnection=calendar&from=2026-09-24T08:00:00Z&to=2026-09-26T08:00:00Z\n2026-09-20T16:00:00Z' \
        //     | openssl dgst -sha256 -hmac 'test-only-hermes-answer-secret-0123456789' | cut -d' ' -f2
        let line = canonical(
            "GET",
            FREEBUSY_PATH,
            "connection=calendar&from=2026-09-24T08:00:00Z&to=2026-09-26T08:00:00Z",
            "2026-09-20T16:00:00Z",
        );
        assert_eq!(
            line,
            "GET\n/_twalk/hermes/freebusy\nconnection=calendar&from=2026-09-24T08:00:00Z&to=2026-09-26T08:00:00Z\n2026-09-20T16:00:00Z"
        );
        assert_eq!(
            sign("test-only-hermes-answer-secret-0123456789", &line),
            "sha256=0f55f6ee82cc89ac96b111112c842cc0dc2ad4faf00858ac311a902197a7b604"
        );
    }

    #[test]
    fn a_window_is_two_instants_in_order_and_at_most_fourteen_days() {
        assert!(Window::parse(Some("2026-09-24T08:00:00Z"), Some("2026-10-08T08:00:00Z")).is_ok());
        assert_eq!(
            Window::parse(Some("2026-09-24T08:00:00Z"), Some("2026-10-08T08:00:01Z")),
            Err(ReadRefusal::WindowTooWide)
        );
        assert!(matches!(
            Window::parse(Some("2026-09-24T08:00:00Z"), Some("2026-09-24T08:00:00Z")),
            Err(ReadRefusal::InvalidWindow(_))
        ));
        assert!(matches!(
            Window::parse(None, Some("2026-09-24T08:00:00Z")),
            Err(ReadRefusal::InvalidWindow(_))
        ));
        assert!(matches!(
            Window::parse(Some("jeudi"), Some("2026-09-24T08:00:00Z")),
            Err(ReadRefusal::InvalidWindow(_))
        ));
        assert_eq!(ReadRefusal::WindowTooWide.status(), 400);
        assert_eq!(ReadRefusal::BadSignature.status(), 401);
        assert_eq!(
            ReadRefusal::ConnectionNotConnected {
                connection: "calendar".to_owned(),
                state: "unreachable".to_owned(),
                hint: None
            }
            .status(),
            409
        );
    }
}
