//! The pending-contact projection: who has written to the user, and which of
//! them is still waiting for a decision (ticket #54).
//!
//! # What this module is, and what it must never become
//!
//! Screen 5 of the wireframes says "3 consent decisions waiting". To count
//! them the Gateway has to know who has written, which makes it a consumer of
//! the bus as well as a producer — and makes this the most exposed corner of
//! the design. **A list of who writes to the user is a social graph with
//! timestamps.** The only thing standing between this module and a
//! surveillance log is what it refuses to keep:
//!
//! - **no message body**, ever, on any path;
//! - **no display name** — a screen that wants one asks for it and the
//!   Gateway reads it from the bus on demand ([`Contacts::display_names`]),
//!   which is a different thing from holding a directory of everyone's name;
//! - **no `network_identifier`** — the Sensor already withholds it until
//!   consent is granted (the contract's `data.contact.network_identifier` is
//!   "only populated when consent state allows it"), and the Gateway must not
//!   undo that by keeping a copy.
//!
//! That is not a convention to be careful about; it is enforced by the shape
//! of the types. The projection deserialises each inbound event into
//! [`InboundHeader`], which has **four envelope attributes and no `data` member at all**,
//! so there is no expression anywhere downstream of it that could reach a
//! body. The raw bytes stay in the NATS message and are dropped with it. The
//! store is a four-column table whose columns are asserted by a test
//! (`store::tests::the_contact_table_has_no_column_for_content`), and another
//! test reads the database file's own bytes and fails if a body or an
//! identifier is anywhere in them (`tests/pending.rs`).
//!
//! # The uncomfortable truth
//!
//! Storing "only the Matrix ID" sounds neutral and is not. A bridged ghost
//! user's Matrix ID conventionally embeds the network identifier —
//! `@whatsapp_33612345678:example.com` is a phone number with a hostname
//! stapled to it — so this table keeps phone numbers although it has no
//! column for one. It is stored anyway, because a consent decision has to
//! name its subject and that ID *is* the subject; what can be done is to hold
//! nothing else, and to say so where an operator will read it:
//! `docs/architecture/security-model.md`, residual risk 5.
//!
//! # The consumer
//!
//! A **durable** JetStream consumer on `twalk.inbound.message.received.v1`,
//! created with `DeliverPolicy::All`, so a Gateway installed after weeks of
//! Sensor traffic builds its list from the stream's history instead of
//! showing an empty inbox. Afterwards the consumer resumes at its own ack
//! floor, which is the bus's business and not a cursor this Gateway keeps:
//! the durable consumer *is* the cursor.
//!
//! Each batch is committed to the store in one transaction and then acked. A
//! crash in between redelivers it, and the store's upsert is idempotent
//! (`first_seen` only ever moves earlier, `last_seen` only ever later), so a
//! redelivery changes nothing — commit-then-ack is the same order the consent
//! outbox uses, for the same reason: the failure that repeats is survivable
//! and the failure that loses is not. One transaction per batch rather than
//! per message is what keeps a first delivery of a long history to seconds:
//! the store is `synchronous=FULL`, so each separate write is an fsync.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::consent::{Network, STREAM_NAME};
use crate::metrics::Metrics;
use crate::owner::Owner;
use crate::store::{SeenContact, Store};

/// The contract type the projection consumes, and the bus subject it rides
/// on.
pub const INBOUND_MESSAGE_TYPE: &str = "fr.linagora.twalk.inbound.message.received.v1";

/// The durable consumer's default name. One Gateway owns it per deployment;
/// `GATEWAY_INBOUND_CONSUMER` renames it for the operator who points two
/// Gateways at one bus, where sharing a durable name would split the stream
/// between them and leave each with half a list.
pub const DEFAULT_INBOUND_CONSUMER: &str = "companion-gateway-pending-contacts";

/// How many messages one drain takes before yielding: a batch bound, so a
/// first delivery of a long history does not hold the task forever.
const DRAIN_BATCH: usize = 512;

/// How long a drain waits for messages before going round again. Short
/// enough that a shutdown is not noticeably delayed, long enough that an
/// idle deployment is not polling the bus in a tight loop.
const DRAIN_IDLE: Duration = Duration::from_secs(2);

/// How far back from the stream's head [`Contacts::display_names`] reads.
///
/// Display names are not stored, so answering "what is this contact called?"
/// means going and looking on the bus. The look is bounded: a contact whose
/// last message is older than this many events on the stream comes back with
/// no name, and the Companion falls back to the Matrix ID. That is the right
/// trade — the alternative to a bound is either an unbounded scan of the
/// whole stream on every screen paint, or a directory of everyone's name kept
/// on disk, which is the thing this ticket exists to not build.
const DISPLAY_NAME_WINDOW: u64 = 1_000;

/// How many contacts one display-name request may ask about. The list on
/// screen is the pending list, so this is sized for a screenful many times
/// over, and it is a bound rather than a page size: an oversized request is
/// refused, not truncated.
pub const MAX_DISPLAY_NAME_LOOKUPS: usize = 200;

/// What the Gateway reads of an inbound event — and, by construction, all it
/// **can** read.
///
/// Four CloudEvents attributes, no `data` member. `serde` fills these and
/// drops the rest of the document on the floor, so the message body, the
/// attachments, the reply excerpt and the contact's `network_identifier`
/// never become values in this process at all: they exist as bytes inside the
/// NATS message and are freed with it.
///
/// This is the module's central safeguard, and it is deliberately a type
/// rather than a rule. Adding a field here is the change a reviewer refuses —
/// the fourth, `connection`, is the envelope's own perimeter attribute (ADR
/// 0033, #270), the key the sighting is held under, and no more content than
/// `network` was.
#[derive(Debug, Clone, Deserialize)]
pub struct InboundHeader {
    /// The observed sender's Matrix user ID (the contract's `subject`).
    pub subject: String,
    /// The network it wrote on (the contract's `network` extension).
    pub network: String,
    /// The connection it wrote on (the contract's `connection` extension,
    /// #269). Absent on an event published before it existed.
    #[serde(default)]
    pub connection: Option<String>,
    /// When the Sensor produced the event (the contract's `time`).
    pub time: String,
}

/// One contact sighting, validated: what [`Store::observe_contact`] takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correspondent {
    pub contact: String,
    /// The perimeter the sighting is held under (#270).
    pub connection: String,
    pub network: Network,
    /// The event's `time`, re-formatted into the Gateway's one canonical
    /// RFC 3339 spelling so that "earliest" and "latest" are comparisons the
    /// database can make on strings.
    pub at: String,
}

/// Why an inbound event is not a sighting. Two of these are the projection
/// working as designed and are logged at `debug`; the third is a deployment
/// that cannot place a contact who wrote, which is a contact the dashboard
/// will never say is waiting — so it is warned about, once per cause, and
/// counted (`twalk_companion_gateway_contacts_unplaced_total`), because a
/// sighting silently not recorded is the class of failure this product has
/// shipped most often without noticing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotASighting {
    /// The owner's own message, or one with no sender at all.
    NotAContact,
    /// A network this build does not know, or an instant that is not RFC 3339.
    Unreadable(String),
    /// The registry could not place the event's connection (#270).
    Unplaced(crate::connections::Unresolved),
}

impl Correspondent {
    /// Reads a sighting out of an event header, or says why this event is not
    /// one.
    ///
    /// Four reasons to refuse:
    ///
    /// - **the sender is the owner** — any of their identities, not only their
    ///   Matrix ID (ticket #149). The user's own messages travel through the
    ///   same portal rooms, and before ADR 0018 the Sensor published them as a
    ///   contact's, so this is the projection that used to offer the user a
    ///   decision about their own ghost. They are not their own correspondent
    ///   and must never appear in their own list of decisions to take. This is
    ///   the cheapest possible restraint: the owner's sightings are not
    ///   filtered out on read, they are never stored — and the rows an older
    ///   build already stored are excluded when the list is read
    ///   ([`crate::store::Store::pending_contacts`]), because a row that exists
    ///   has to be dealt with wherever it is met.
    /// - **the network is not one of the contract's.** A value this build
    ///   does not know is a contract it does not implement; guessing would
    ///   put a row in the store under a network nobody can decide about.
    /// - **the connection is not the registry's** (#270) — or the event
    ///   carries none, being older than #269, and the registry has no single
    ///   connection of its kind to read it as. The stream's whole history is
    ///   replayed through here on a first start, so the old shape is the
    ///   common one, and it resolves the way every old decision was migrated:
    ///   to the kind's one connection, looked up, not spelled.
    /// - **the time is not RFC 3339.** The instant is the whole of what the
    ///   projection records besides the ID, so an unreadable one is not worth
    ///   inventing a substitute for.
    pub fn read(
        header: &InboundHeader,
        owner: &Owner,
        registry: &crate::connections::Registry,
    ) -> Result<Self, NotASighting> {
        if owner.is_owner(&header.subject) || header.subject.is_empty() {
            return Err(NotASighting::NotAContact);
        }
        let network = Network::parse(&header.network).ok_or_else(|| {
            NotASighting::Unreadable(format!(
                "the network {:?} is not the contract's",
                header.network
            ))
        })?;
        let connection = registry
            .resolve(header.connection.as_deref(), network.as_str())
            .map_err(NotASighting::Unplaced)?
            .id
            .clone();
        let at = canonical_instant(&header.time).ok_or_else(|| {
            NotASighting::Unreadable(format!("the time {:?} is not RFC 3339", header.time))
        })?;
        Ok(Self {
            contact: header.subject.clone(),
            connection,
            network,
            at,
        })
    }
}

/// An RFC 3339 instant in the one spelling this projection stores, or `None`
/// when the input is not RFC 3339.
///
/// Fixed width, always UTC, always three subsecond digits — and that is the
/// whole point, not a formatting preference. `first_seen` and `last_seen` are
/// compared by SQLite as strings, so two spellings of one instant, or a `Z`
/// where a `.000Z` should be, would make "earliest" and "latest" wrong. The
/// Sensor stamps whole seconds and the Gateway stamps milliseconds; an event
/// could arrive with an offset. All of them land here.
///
/// Deliberately not [`crate::consent::rfc3339_millis`], which formats through
/// RFC 3339's own rules and drops a zero subsecond — right for an event's
/// `time`, wrong for a value that is ordered by string comparison.
fn canonical_instant(raw: &str) -> Option<String> {
    let at = time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
        .ok()?
        .to_offset(time::UtcOffset::UTC);
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second(),
        at.millisecond(),
    ))
}

/// The pending-contact half of the Gateway: the projection's store, the bus
/// it consumes and the one read that goes back to the bus for a display name.
pub struct Contacts {
    /// The same SQLite file the consent journal lives in: the pending list is
    /// a view over both, so "a decision moves the contact out of the list" is
    /// arithmetic rather than a second projection to keep in step.
    store: Arc<Store>,
    metrics: Arc<Metrics>,
    /// This deployment's owner, and every identity their own traffic arrives
    /// under (#149): never a correspondent of their own, under any of them.
    owner: Arc<Owner>,
    /// The registry a sighting's connection is looked up in (#270).
    connections: Arc<crate::connections::Registry>,
    /// The causes an unplaced sighting has already been warned about, so a
    /// history of thousands of events under one misconfiguration is one line
    /// and a counter, not a log of thousands.
    unplaced_said: std::sync::Mutex<std::collections::BTreeSet<String>>,
    nats_url: String,
    consumer_name: String,
    /// The bus connection, made on first need and shared by the projection
    /// task and the display-name read. NATS reconnects underneath it, so
    /// this is created once and never rebuilt.
    bus: tokio::sync::OnceCell<async_nats::jetstream::Context>,
}

impl Contacts {
    pub fn new(
        store: Arc<Store>,
        metrics: Arc<Metrics>,
        owner: Arc<Owner>,
        connections: Arc<crate::connections::Registry>,
        nats_url: String,
        consumer_name: String,
    ) -> Self {
        Self {
            store,
            metrics,
            owner,
            connections,
            unplaced_said: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            nats_url,
            consumer_name,
            bus: tokio::sync::OnceCell::new(),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn consumer_name(&self) -> &str {
        &self.consumer_name
    }

    /// The contacts waiting for a decision — never the owner, including a
    /// sighting an older build recorded of them (#149).
    pub fn pending(&self) -> Result<Vec<SeenContact>> {
        self.store.pending_contacts()
    }

    /// The bus, connected on first need.
    ///
    /// `retry_on_initial_connect` so a Gateway that starts before the bus
    /// still comes up: the connection is made in the background and the
    /// projection simply has nothing to do until it lands. Only a malformed
    /// URL fails here.
    async fn jetstream(&self) -> Result<&async_nats::jetstream::Context> {
        self.bus
            .get_or_try_init(|| async {
                let client = async_nats::ConnectOptions::new()
                    .retry_on_initial_connect()
                    .connect(&self.nats_url)
                    .await
                    .with_context(|| {
                        format!(
                            "the pending-contact projection cannot use {}",
                            self.nats_url
                        )
                    })?;
                Ok(async_nats::jetstream::new(client))
            })
            .await
    }

    /// The display names of the given contacts, read from the bus and not
    /// written anywhere.
    ///
    /// This is the one place in the Gateway that looks inside an inbound
    /// event's `data`, and it looks at exactly one member of it. It walks the
    /// tail of the stream ([`DISPLAY_NAME_WINDOW`] messages) with an
    /// ephemeral consumer, keeps the most recent name it saw for each contact
    /// that was asked about, and drops everything else — the bodies it walks
    /// past live for the length of one loop iteration and are never written,
    /// logged or cached.
    ///
    /// A contact whose last message fell outside the window comes back as
    /// `None`. That is a real answer, not a failure: the Companion shows the
    /// Matrix ID, which is what it has and what the decision will name
    /// anyway.
    pub async fn display_names(
        &self,
        contacts: &[String],
    ) -> Result<Vec<(String, Option<String>)>> {
        let mut found: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
        let jetstream = self.jetstream().await?;
        // Mutable because reading the stream's own state is what names the
        // head this read counts back from.
        let mut stream = jetstream
            .get_stream(STREAM_NAME)
            .await
            .context("failed to reach the bus stream")?;
        let last = stream
            .info()
            .await
            .context("failed to read the bus stream's state")?
            .state
            .last_sequence;
        if last > 0 && !contacts.is_empty() {
            let start = last.saturating_sub(DISPLAY_NAME_WINDOW).max(1);
            let name = format!(
                "gateway-display-names-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or_default()
            );
            let consumer = stream
                .create_consumer(async_nats::jetstream::consumer::pull::Config {
                    name: Some(name.clone()),
                    filter_subject: crate::consent::bus_subject(INBOUND_MESSAGE_TYPE),
                    deliver_policy:
                        async_nats::jetstream::consumer::DeliverPolicy::ByStartSequence {
                            start_sequence: start,
                        },
                    // Nothing is acked and nothing resumes: this consumer
                    // exists for the length of one request and is deleted
                    // below.
                    ack_policy: async_nats::jetstream::consumer::AckPolicy::None,
                    inactive_threshold: Duration::from_secs(30),
                    ..Default::default()
                })
                .await
                .context("failed to open a read of the bus")?;
            let mut batch = consumer
                .fetch()
                .max_messages(usize::try_from(DISPLAY_NAME_WINDOW).unwrap_or(usize::MAX))
                .messages()
                .await
                .context("failed to read from the bus")?;
            use futures::StreamExt;
            while let Some(message) = batch.next().await {
                let Ok(message) = message else { break };
                let Ok(probe) = serde_json::from_slice::<DisplayNameProbe>(&message.payload) else {
                    continue;
                };
                let Some(display_name) = probe.data.contact.display_name else {
                    continue;
                };
                if let Some(asked) = contacts
                    .iter()
                    .find(|contact| *contact == &probe.subject)
                    .map(String::as_str)
                {
                    // Stream order is publication order, so the last one
                    // seen is the most recent the window holds.
                    found.insert(asked, display_name);
                }
            }
            if let Err(error) = stream.delete_consumer(&name).await {
                // Harmless: `inactive_threshold` reaps it anyway.
                debug!(%error, "failed to delete the display-name read's consumer");
            }
        }
        Ok(contacts
            .iter()
            .map(|contact| (contact.clone(), found.get(contact.as_str()).cloned()))
            .collect())
    }

    /// Republishes the pending gauge from the store — the dashboard's number,
    /// as an operator scrapes it.
    fn observe_pending(&self) {
        match self.store.pending_contact_count() {
            Ok(pending) => self.metrics.set_pending_contacts(pending),
            Err(error) => warn!(%error, "failed to count the pending contacts"),
        }
    }
}

/// The one member of an inbound event's `data` the Gateway ever reads, and
/// only on the display-name path.
///
/// Spelled as a type with exactly this shape rather than as
/// `event["data"]["contact"]["display_name"]` on a `serde_json::Value`, for
/// the same reason [`InboundHeader`] is a type: what the struct does not
/// declare, the process does not hold.
#[derive(Debug, Deserialize)]
struct DisplayNameProbe {
    subject: String,
    data: DisplayNameProbeData,
}

#[derive(Debug, Deserialize)]
struct DisplayNameProbeData {
    contact: DisplayNameProbeContact,
}

#[derive(Debug, Deserialize)]
struct DisplayNameProbeContact {
    display_name: Option<String>,
}

/// Projects `inbound.message.received` into the pending-contact store until
/// the process stops.
///
/// The consumer is durable and created with `DeliverPolicy::All`: the first
/// time this Gateway runs it reads the stream from the beginning, so an
/// installation that follows weeks of Sensor traffic shows the contacts that
/// wrote during those weeks rather than an empty inbox. Every run after that
/// resumes where the acks stopped — the durable consumer's own ack floor,
/// which is why this module keeps no cursor of its own.
pub async fn project_until_shutdown(contacts: Arc<Contacts>) {
    let subject = crate::consent::bus_subject(INBOUND_MESSAGE_TYPE);
    let jetstream = match contacts.jetstream().await {
        Ok(jetstream) => jetstream,
        Err(error) => {
            warn!(%error, "the pending-contact projection is not running: the Companion will show no contact waiting for a decision");
            return;
        }
    };
    info!(
        consumer = %contacts.consumer_name,
        %subject,
        "the pending-contact projection is consuming the inbound stream, from the beginning on its first run"
    );
    // The gauge before the first message, so a restart reports the list it
    // inherited rather than nothing until the next inbound event.
    contacts.observe_pending();

    loop {
        match drain(&contacts, jetstream, &subject).await {
            Ok(projected) if projected > 0 => contacts.observe_pending(),
            Ok(_) => {}
            Err(error) => {
                // Not fatal: nothing was acked that was not committed, so
                // the next attempt sees the same messages.
                warn!(%error, "the pending-contact projection could not read the bus; retrying");
                tokio::time::sleep(DRAIN_IDLE).await;
            }
        }
    }
}

/// One batch: fetch it, commit its sightings in a single transaction, then
/// ack its messages.
///
/// **Commit then ack**, as the consent outbox publishes then marks. A crash
/// between the two redelivers the whole batch, whose rows the store already
/// has, and the upsert absorbs it. The other order would ack a contact the
/// store never heard of, and the user would never be asked about them.
///
/// The batch is one transaction rather than one write per message because the
/// store is opened `synchronous=FULL`: a Gateway's first delivery replays the
/// stream's whole history, and a separate fsync per event turns that into
/// minutes. Reading the batch first also keeps the store's mutex out of the
/// network's way — nothing is held while the bus is being waited on.
///
/// A message this build cannot read is skipped and acked: it is not a
/// contact, and redelivering it forever would stall every contact behind it.
async fn drain(
    contacts: &Arc<Contacts>,
    jetstream: &async_nats::jetstream::Context,
    subject: &str,
) -> Result<usize> {
    let stream = jetstream
        .get_stream(STREAM_NAME)
        .await
        .context("failed to reach the bus stream")?;
    let consumer = stream
        .get_or_create_consumer(
            &contacts.consumer_name,
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some(contacts.consumer_name.clone()),
                filter_subject: subject.to_owned(),
                // The whole history, the first time this consumer is
                // created; its ack floor every time after.
                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::All,
                ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                ..Default::default()
            },
        )
        .await
        .context("failed to create the pending-contact consumer")?;

    let mut batch = consumer
        .fetch()
        .max_messages(DRAIN_BATCH)
        .expires(DRAIN_IDLE)
        .messages()
        .await
        .context("failed to fetch from the pending-contact consumer")?;

    use futures::StreamExt;
    let mut sightings: Vec<(String, String, String)> = Vec::new();
    let mut delivered = Vec::new();
    while let Some(message) = batch.next().await {
        let message =
            message.map_err(|error| anyhow::anyhow!("failed to read a message: {error}"))?;
        // Four attributes out of the event, and the rest of its bytes are
        // never parsed. See `InboundHeader`.
        match serde_json::from_slice::<InboundHeader>(&message.payload) {
            Ok(header) => {
                match Correspondent::read(&header, &contacts.owner, &contacts.connections) {
                    Ok(correspondent) => sightings.push((
                        correspondent.contact,
                        correspondent.connection,
                        correspondent.at,
                    )),
                    // The owner's own message, an unknown network, an
                    // unreadable instant: nothing to record, and nothing to
                    // redeliver either.
                    Err(NotASighting::NotAContact) => debug!(
                        subject = %header.subject,
                        network = %header.network,
                        "an inbound event is not a correspondent's: not recorded"
                    ),
                    Err(NotASighting::Unreadable(why)) => debug!(
                        subject = %header.subject,
                        network = %header.network,
                        "an inbound event could not be read as a sighting ({why}): not recorded"
                    ),
                    // A contact who wrote and cannot be placed under a
                    // connection: not recorded either — a row under a guessed
                    // perimeter would be a decision offered about the wrong
                    // account — but this one is a deployment's fault and not
                    // the projection's design, so it is counted and said,
                    // once per cause.
                    Err(NotASighting::Unplaced(why)) => {
                        contacts.metrics.record_contact_unplaced();
                        let first_time = contacts
                            .unplaced_said
                            .lock()
                            .expect("the unplaced-causes set is never poisoned")
                            .insert(why.to_string());
                        if first_time {
                            warn!(
                                network = %header.network,
                                connection = header.connection.as_deref().unwrap_or("none"),
                                "an inbound event's connection cannot be placed in the registry \
                                 ({why}): its sender is not recorded as waiting for a decision, \
                                 and will not be until GATEWAY_CONNECTIONS names a connection the \
                                 registry can place it under. Said once per cause; every event \
                                 it happens to is counted in \
                                 twalk_companion_gateway_contacts_unplaced_total"
                            );
                        }
                    }
                }
            }
            Err(error) => warn!(%error, "an inbound event could not be read; skipping it"),
        }
        delivered.push(message);
    }
    if delivered.is_empty() {
        return Ok(0);
    }

    let projected = sightings.len();
    contacts
        .store
        .observe_contacts(&sightings)
        .context("failed to record a batch of seen contacts")?;
    for message in delivered {
        message
            .ack()
            .await
            .map_err(|error| anyhow::anyhow!("failed to ack a message: {error}"))?;
    }
    if projected > 0 {
        contacts.metrics.record_contacts_observed(projected as u64);
        debug!(projected, "projected a batch of inbound events");
    }
    Ok(projected)
}

/// One pending contact as the API renders it. Kept here, next to the store
/// that produced it, so that what leaves the Gateway is written beside what
/// it holds — the two are the same four values.
pub fn pending_json(seen: &SeenContact) -> Value {
    serde_json::json!({
        "contact": seen.contact,
        "connection": seen.connection,
        "network": seen.network.as_str(),
        "first_seen": seen.first_seen,
        "last_seen": seen.last_seen,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OWNER: &str = "@michel:example.com";
    /// The ghost the owner's own WhatsApp messages actually arrived under on
    /// the reference deployment (#109) — and the kind of subject a deployment
    /// upgraded across it can hold a consent row about.
    const OWNER_GHOST: &str = "@whatsapp_lid-115332874281144:example.com";

    fn owner() -> Owner {
        Owner::new(OWNER, [OWNER_GHOST.to_owned()])
    }

    /// One WhatsApp, one Signal, and two SMS accounts: the registry under
    /// which a sighting's connection is a lookup, and sometimes has no
    /// answer.
    fn registry() -> crate::connections::Registry {
        crate::connections::Registry::from_config(
            Some("whatsapp=whatsapp,signal=signal,sms=sms,sms-work=sms"),
            &[],
            "example.com",
        )
        .expect("a registry")
    }

    fn header(event: &Value) -> InboundHeader {
        serde_json::from_value(event.clone()).expect("the header reads")
    }

    /// The contract's own fixture, which carries a body, a caption, an
    /// attachment with its decryption key, a display name and a
    /// `network_identifier`.
    fn full_event() -> Value {
        json!({
            "specversion": "1.0",
            "id": "20be32e73506b9104a6a1bf76fc2d2a15cbd2b8a0a421833be01f865ca9886d0",
            "source": "matrix://matrix.example.com/!abcXYZ123:example.com",
            "type": INBOUND_MESSAGE_TYPE,
            "time": "2026-09-17T10:00:00Z",
            "subject": "@whatsapp_33612345678:example.com",
            "datacontenttype": "application/json",
            "network": "whatsapp",
            "consent": "pending",
            "data": {
                "body": "On décale à 20h ?",
                "format": "text/plain",
                "reply_to": null,
                "attachments": [],
                "contact": {
                    "display_name": "Aïcha Benali",
                    "network_identifier": "+33612345678"
                }
            }
        })
    }

    #[test]
    fn the_projection_reads_three_attributes_and_cannot_reach_the_rest() {
        let event = full_event();
        let header = header(&event);
        assert_eq!(header.subject, "@whatsapp_33612345678:example.com");
        assert_eq!(header.network, "whatsapp");
        assert_eq!(header.time, "2026-09-17T10:00:00Z");
        // And the whole of what the projection then hands the store. There
        // is no expression that reaches the body, the display name or the
        // network identifier from here: `InboundHeader` has no `data`
        // member, so this is a statement about the type and not about this
        // test's diligence.
        let correspondent =
            Correspondent::read(&header, &owner(), &registry()).expect("a correspondent");
        assert_eq!(
            correspondent,
            Correspondent {
                contact: "@whatsapp_33612345678:example.com".to_owned(),
                connection: "whatsapp".to_owned(),
                network: Network::Whatsapp,
                at: "2026-09-17T10:00:00.000Z".to_owned(),
            }
        );
        // Said once more where a reader will look for it: what the event
        // carried and the projection did not take.
        let rendered = format!("{correspondent:?}");
        for content in ["On décale", "Aïcha", "+33612345678"] {
            assert!(
                !rendered.contains(content),
                "a correspondent carries no content: {rendered}"
            );
        }
    }

    #[test]
    fn the_owner_is_not_their_own_correspondent_under_any_of_their_identities() {
        // Their Matrix ID, and — the half #149 adds — the network ghost their
        // own traffic actually arrives under, which is the identity a
        // deployment upgraded across #109 holds a row about.
        for identity in [OWNER, OWNER_GHOST] {
            let mut event = full_event();
            event["subject"] = json!(identity);
            assert_eq!(
                Correspondent::read(&header(&event), &owner(), &registry()),
                Err(NotASighting::NotAContact),
                "the user's own messages travel through the same rooms; they are not \
                 decisions the user has to take about themselves ({identity})"
            );
        }
        // And unknown is not the owner: a ghost one digit apart is a contact,
        // and is still recorded as one.
        let mut somebody_else = full_event();
        somebody_else["subject"] = json!("@whatsapp_lid-115332874281145:example.com");
        assert!(Correspondent::read(&header(&somebody_else), &owner(), &registry()).is_ok());
    }

    #[test]
    fn the_sighting_is_held_under_the_events_connection_or_the_kinds_only_one() {
        // #269 stamps the connection; the projection keeps it as the key.
        let mut stamped = full_event();
        stamped["network"] = json!("sms");
        stamped["connection"] = json!("sms-work");
        let read = Correspondent::read(&header(&stamped), &owner(), &registry()).unwrap();
        assert_eq!(
            (read.connection.as_str(), read.network),
            ("sms-work", Network::Sms)
        );
        // An event older than #269 — the stream's history, replayed on a
        // first start — is read as its kind's one connection, from the
        // registry: `whatsapp` has one, `sms` has two here, so an SMS
        // sighting with no connection is not a row to guess at.
        assert_eq!(
            Correspondent::read(&header(&full_event()), &owner(), &registry())
                .unwrap()
                .connection,
            "whatsapp"
        );
        // Both are the registry's failure to place the event, said as such —
        // `drain` warns and counts these, unlike the owner's own message.
        let mut old_sms = full_event();
        old_sms["network"] = json!("sms");
        assert_eq!(
            Correspondent::read(&header(&old_sms), &owner(), &registry()),
            Err(NotASighting::Unplaced(
                crate::connections::Unresolved::SeveralOfKind {
                    kind: "sms".to_owned(),
                    ids: vec!["sms".to_owned(), "sms-work".to_owned()],
                }
            )),
            "two SMS connections and an event naming neither"
        );
        // A connection the registry does not know is not a row either.
        let mut unknown = full_event();
        unknown["connection"] = json!("wa-home");
        assert_eq!(
            Correspondent::read(&header(&unknown), &owner(), &registry()),
            Err(NotASighting::Unplaced(
                crate::connections::Unresolved::UnknownId("wa-home".to_owned())
            ))
        );
    }

    #[test]
    fn an_event_this_build_cannot_read_records_nothing() {
        // A network outside the contract's enum: a contract this build does
        // not implement, not a row to guess at.
        let mut unknown_network = full_event();
        unknown_network["network"] = json!("gmessages");
        assert!(matches!(
            Correspondent::read(&header(&unknown_network), &owner(), &registry()),
            Err(NotASighting::Unreadable(_))
        ));

        // An instant that is not RFC 3339: the instant is half of what the
        // projection records, so there is nothing to substitute for it.
        let mut bad_time = full_event();
        bad_time["time"] = json!("last tuesday");
        assert!(matches!(
            Correspondent::read(&header(&bad_time), &owner(), &registry()),
            Err(NotASighting::Unreadable(_))
        ));

        // And an event with no sender at all.
        let mut no_subject = full_event();
        no_subject["subject"] = json!("");
        assert_eq!(
            Correspondent::read(&header(&no_subject), &owner(), &registry()),
            Err(NotASighting::NotAContact)
        );
    }

    #[test]
    fn every_spelling_of_an_instant_is_stored_the_same_way() {
        // The Sensor stamps seconds; the Gateway stamps milliseconds; an
        // event could carry an offset. All three have to compare as strings
        // in the store, or "earliest" and "latest" would be wrong.
        for (raw, expected) in [
            ("2026-09-17T10:00:00Z", "2026-09-17T10:00:00.000Z"),
            ("2026-09-17T10:00:00.500Z", "2026-09-17T10:00:00.500Z"),
            ("2026-09-17T12:00:00+02:00", "2026-09-17T10:00:00.000Z"),
        ] {
            assert_eq!(canonical_instant(raw).as_deref(), Some(expected), "{raw}");
        }
        assert_eq!(canonical_instant("not an instant"), None);
        // The property that matters: the canonical spellings sort the way
        // the instants do.
        let mut spellings: Vec<String> = ["2026-09-17T10:00:00.500Z", "2026-09-17T10:00:00Z"]
            .into_iter()
            .map(|raw| canonical_instant(raw).unwrap())
            .collect();
        spellings.sort();
        assert_eq!(
            spellings,
            vec!["2026-09-17T10:00:00.000Z", "2026-09-17T10:00:00.500Z"]
        );
    }

    #[test]
    fn a_pending_contact_renders_as_the_values_it_is_and_no_more() {
        assert_eq!(
            pending_json(&SeenContact {
                contact: "@whatsapp_33612345678:example.com".to_owned(),
                connection: "whatsapp".to_owned(),
                network: Network::Whatsapp,
                first_seen: "2026-09-17T10:00:00.000Z".to_owned(),
                last_seen: "2026-09-17T18:30:00.000Z".to_owned(),
            }),
            json!({
                "contact": "@whatsapp_33612345678:example.com",
                "connection": "whatsapp",
                "network": "whatsapp",
                "first_seen": "2026-09-17T10:00:00.000Z",
                "last_seen": "2026-09-17T18:30:00.000Z"
            })
        );
    }

    #[test]
    fn the_subject_the_projection_consumes_is_the_contracts() {
        assert_eq!(
            crate::consent::bus_subject(INBOUND_MESSAGE_TYPE),
            "twalk.inbound.message.received.v1"
        );
    }
}
