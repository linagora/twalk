//! The bus's **retention policy** (issue #174, ADR 0037): the one place the
//! `twalk` stream's configuration is decided, with every field named.
//!
//! Five components create the stream if it is missing, and until this
//! module every one of them did so with `..Default::default()` — so the bus
//! kept every event for ever, uncompressed, with no size ceiling, and
//! remembered a `Nats-Msg-Id` for two minutes, and nobody had decided any
//! of it. The reference deployment's host reached a full disk three times
//! in one day. The stream holds **contacts' messages** — portal rooms are
//! end-to-end encrypted, so the bus is the only place in the deployment
//! where the plaintext of other people's words exists at rest — and how
//! long it keeps them is therefore a personal-data decision before it is a
//! disk one. The operator took it on 2026-09-19: ninety days, two GiB,
//! discard the oldest, compress, and a **day** of duplicate tracking.
//!
//! The Sensor is the stream's owner: it is the component that publishes the
//! bulk of what the stream holds, and [`ensure_stream`] is the one call
//! that creates the stream with the policy or **updates an existing one in
//! place**, saying field by field what changed. The other components keep
//! creating a bare stream when none exists, so a deployment that starts the
//! Companion Gateway first still works; the Sensor then reconciles it.
//!
//! What this module refuses to do is the load-bearing part: it never
//! deletes or recreates a stream. An update the bus rejects — JetStream
//! cannot change a stream's storage, retention or name in place — is an
//! `error` naming the field and the operator's options, and the Sensor
//! runs on the stream's existing policy. Expired events are gone for good,
//! and taking them away is the operator's act, never a restart's.
//!
//! The duplicate window is a correctness fix and not a tuning knob. The
//! Sensor publishes with a `Nats-Msg-Id` derived from the event's content
//! so that a republication of the same Matrix event is dropped by the bus,
//! and a Sensor restart re-syncs from Matrix and republishes; JetStream
//! remembers an id for `duplicate_window` only, which was two minutes. Any
//! event republished later than that landed twice, with two sequences, and
//! every consumer saw it twice — a persona triggered twice on one message,
//! a contact counted twice.

use std::fmt;
use std::time::Duration;

use anyhow::{Context, Result};
use async_nats::jetstream::context::{
    CreateStreamErrorKind, GetStreamErrorKind, UpdateStreamError,
};
use async_nats::jetstream::stream::{
    Compression, Config, DiscardPolicy, RetentionPolicy, StorageType,
};
use async_nats::jetstream::ErrorCode;
use tracing::{error, info};

use crate::normalize::{STREAM_NAME, STREAM_SUBJECTS};

/// The three values the operator sets (`SENSOR_BUS_MAX_AGE_DAYS`,
/// `SENSOR_BUS_MAX_BYTES`, `SENSOR_BUS_DUPLICATE_WINDOW_SECONDS`), with the
/// decided ones as defaults. Everything else in the stream's configuration
/// is fixed by [`StreamPolicy::stream_config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamPolicy {
    /// How long the stream keeps an event. NATS reads zero as "for ever",
    /// which is exactly the undecided state this policy replaces, so zero is
    /// refused rather than passed through.
    pub max_age: Duration,
    /// How large the stream may grow before the oldest events are discarded
    /// to make room. The working limit is the age: ninety days of a real
    /// account's traffic is nearer 90 MB than 2 GiB. The ceiling exists so
    /// that the user's disk is never the thing that gives way.
    pub max_bytes: i64,
    /// How long the bus remembers a `Nats-Msg-Id`, so that the same event
    /// published again is one message and not two.
    pub duplicate_window: Duration,
}

impl StreamPolicy {
    /// Ninety days: long enough that a replay means something, short enough
    /// that the bus does not silently become an archive of other people's
    /// messages that nobody chose to keep.
    pub const DEFAULT_MAX_AGE_DAYS: u64 = 90;
    /// Two GiB.
    pub const DEFAULT_MAX_BYTES: i64 = 2_147_483_648;
    /// A day: longer than any re-sync a restart produces.
    pub const DEFAULT_DUPLICATE_WINDOW_SECONDS: u64 = 86_400;

    /// The longest age this policy accepts, a hundred years: past about 292
    /// years the nanoseconds the bus is sent no longer fit the signed integer
    /// it reads them as and wrap to a negative value, which NATS treats as
    /// **no expiry** — the state the zero check refuses, reached from the
    /// other end. A ceiling well short of that, refused by name, is the
    /// plainer answer.
    pub const MAX_AGE_DAYS_CEILING: u64 = 36_500;

    /// Builds the policy from the operator's three values, refusing the ones
    /// NATS would read as "no limit": a zero age keeps every event for ever,
    /// a zero or negative size means no ceiling, and a zero window remembers
    /// no id at all — and an age past [`Self::MAX_AGE_DAYS_CEILING`], which
    /// would wrap to the same "for ever". Each refusal names the variable to
    /// fix. A window longer than the age is refused too, because the bus
    /// refuses it — later, with a less useful message.
    pub fn new(max_age_days: u64, max_bytes: i64, duplicate_window_seconds: u64) -> Result<Self> {
        if max_age_days == 0 {
            anyhow::bail!(
                "SENSOR_BUS_MAX_AGE_DAYS is 0: the bus would keep every event for ever, which is \
                 the undecided policy issue #174 replaced. Set it to at least 1"
            );
        }
        let max_age_secs = max_age_days
            .checked_mul(86_400)
            .filter(|_| max_age_days <= Self::MAX_AGE_DAYS_CEILING)
            .with_context(|| {
                format!(
                    "SENSOR_BUS_MAX_AGE_DAYS is {max_age_days}: past {} days (a hundred years) \
                     the age the bus is sent wraps to a value it reads as \"no expiry\", which is \
                     the undecided policy issue #174 replaced. Set it to {} at most",
                    Self::MAX_AGE_DAYS_CEILING,
                    Self::MAX_AGE_DAYS_CEILING
                )
            })?;
        if max_bytes < 1 {
            anyhow::bail!(
                "SENSOR_BUS_MAX_BYTES is {max_bytes}: the bus would grow without a ceiling until \
                 the disk gives way. Set it to at least 1"
            );
        }
        if duplicate_window_seconds == 0 {
            anyhow::bail!(
                "SENSOR_BUS_DUPLICATE_WINDOW_SECONDS is 0: the bus would remember no Nats-Msg-Id \
                 and every republished event would land twice. Set it to at least 1"
            );
        }
        let max_age = Duration::from_secs(max_age_secs);
        let duplicate_window = Duration::from_secs(duplicate_window_seconds);
        if duplicate_window > max_age {
            anyhow::bail!(
                "SENSOR_BUS_DUPLICATE_WINDOW_SECONDS ({duplicate_window_seconds}) is longer than \
                 SENSOR_BUS_MAX_AGE_DAYS ({max_age_days} days): the bus cannot remember an id \
                 longer than it keeps the message"
            );
        }
        Ok(Self {
            max_age,
            max_bytes,
            duplicate_window,
        })
    }

    /// The `twalk` stream's whole configuration. Every field is named, so a
    /// reader sees the policy without knowing NATS's defaults — and a field
    /// async-nats adds in a later version is a compile error here, which is
    /// a new policy decision surfacing rather than one inherited in silence.
    pub fn stream_config(&self) -> Config {
        Config {
            name: STREAM_NAME.to_owned(),
            subjects: STREAM_SUBJECTS.iter().map(|s| s.to_string()).collect(),
            // The retention policy proper.
            retention: RetentionPolicy::Limits,
            max_age: self.max_age,
            max_bytes: self.max_bytes,
            // No count ceiling, per stream or per subject: the age and the
            // size are the limits, and a count would be a third one nobody
            // decided.
            max_messages: -1,
            max_messages_per_subject: -1,
            // When a limit is reached the oldest events go, so the bus never
            // refuses a new message to keep an old one.
            discard: DiscardPolicy::Old,
            discard_new_per_subject: false,
            // CloudEvents JSON compresses several-fold and no consumer is
            // affected; it applies to new blocks.
            compression: Some(Compression::S2),
            duplicate_window: self.duplicate_window,
            // The storage: files, one copy, on the user's own disk. Neither
            // can change in place, which is what the refusal path is for.
            storage: StorageType::File,
            num_replicas: 1,
            persist_mode: None,
            placement: None,
            // No ceilings on what consumers may do or messages may weigh:
            // the Sensor's own consumers and the projections are the bus's
            // readers, and an attachment's metadata is the largest event.
            max_consumers: -1,
            max_message_size: -1,
            consumer_limits: None,
            // Nothing exotic: every event is acknowledged, nothing is sealed,
            // and an operator's purge or delete stays possible, because that
            // act is theirs and this module exists not to take it.
            no_ack: false,
            sealed: false,
            deny_delete: false,
            deny_purge: false,
            allow_rollup: false,
            allow_direct: false,
            mirror_direct: false,
            mirror: None,
            sources: None,
            republish: None,
            subject_transform: None,
            first_sequence: None,
            template_owner: String::new(),
            description: None,
            metadata: Default::default(),
            pause_until: None,
            allow_message_ttl: false,
            subject_delete_marker_ttl: None,
            allow_atomic_publish: false,
            allow_message_schedules: false,
            allow_message_counter: false,
            allow_batch_publish: false,
        }
    }
}

impl fmt::Display for StreamPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "max_age={} max_bytes={} discard=old compression=s2 duplicate_window={}",
            render_duration(self.max_age),
            self.max_bytes,
            render_duration(self.duplicate_window)
        )
    }
}

/// One field of the stream's configuration that the policy changes: what
/// the bus held and what the policy says, rendered for a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldChange {
    pub field: &'static str,
    pub old: String,
    pub new: String,
}

impl fmt::Display for FieldChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} → {}", self.field, self.old, self.new)
    }
}

/// The fields of `wanted` that differ from `existing`, over the fields the
/// policy decides. The comparison is field by field on purpose: the bus
/// echoes a configuration with defaults filled in, so a whole-struct
/// equality would report a difference that is not one, and a log line
/// saying "the configuration changed" is not a log line saying what did.
pub fn diff(existing: &Config, wanted: &Config) -> Vec<FieldChange> {
    let mut changes = Vec::new();
    let mut compare = |field: &'static str, old: String, new: String| {
        if old != new {
            changes.push(FieldChange { field, old, new });
        }
    };
    compare(
        "subjects",
        existing.subjects.join(","),
        wanted.subjects.join(","),
    );
    compare(
        "retention",
        format!("{:?}", existing.retention).to_lowercase(),
        format!("{:?}", wanted.retention).to_lowercase(),
    );
    compare(
        "storage",
        format!("{:?}", existing.storage).to_lowercase(),
        format!("{:?}", wanted.storage).to_lowercase(),
    );
    compare(
        "num_replicas",
        existing.num_replicas.to_string(),
        wanted.num_replicas.to_string(),
    );
    compare(
        "max_age",
        render_duration(existing.max_age),
        render_duration(wanted.max_age),
    );
    compare(
        "max_bytes",
        existing.max_bytes.to_string(),
        wanted.max_bytes.to_string(),
    );
    compare(
        "max_messages",
        existing.max_messages.to_string(),
        wanted.max_messages.to_string(),
    );
    compare(
        "max_messages_per_subject",
        existing.max_messages_per_subject.to_string(),
        wanted.max_messages_per_subject.to_string(),
    );
    compare(
        "max_consumers",
        existing.max_consumers.to_string(),
        wanted.max_consumers.to_string(),
    );
    compare(
        "max_message_size",
        existing.max_message_size.to_string(),
        wanted.max_message_size.to_string(),
    );
    compare(
        "discard",
        format!("{:?}", existing.discard).to_lowercase(),
        format!("{:?}", wanted.discard).to_lowercase(),
    );
    compare(
        "compression",
        render_compression(existing.compression.as_ref()),
        render_compression(wanted.compression.as_ref()),
    );
    compare(
        "duplicate_window",
        render_duration(existing.duplicate_window),
        render_duration(wanted.duplicate_window),
    );
    changes
}

/// What [`ensure_stream`] did, for the log and for the tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No stream: created with the policy.
    Created,
    /// The stream already carried the policy.
    Unchanged,
    /// The stream existed with another configuration and was updated in
    /// place; these are the fields that changed.
    Updated(Vec<FieldChange>),
    /// The stream existed with another configuration and the bus refused to
    /// change it — a field JetStream cannot change in place. The Sensor runs
    /// on the stream's existing policy.
    Refused {
        changes: Vec<FieldChange>,
        error: String,
    },
}

/// Creates the `twalk` stream with the policy, or reconciles an existing one
/// onto it in place, logging each field that changed. Never deletes or
/// recreates a stream: a refused update is an `error` naming the field and
/// the operator's options, and is not a failure of this call — the Sensor
/// still has a stream to publish on. What fails the call is a bus that
/// cannot be asked at all, on the read and on the update alike.
///
/// The update sends the **whole** configuration [`StreamPolicy::stream_config`]
/// names, and [`diff`] compares the thirteen policy fields: so when a policy
/// field differs, a field the policy does not name that an operator edited
/// out of band (`description`, `allow_direct`, `metadata`, a `republish`…)
/// is reset to the policy's value without a line of its own — and when none
/// differs, that edit survives. That is the accepted cost of the Sensor
/// owning the configuration rather than the thirteen fields alone: the
/// stream has one author, and an edit made behind its back is not one it
/// preserves.
pub async fn ensure_stream(
    jetstream: &async_nats::jetstream::Context,
    policy: &StreamPolicy,
) -> Result<Outcome> {
    let wanted = policy.stream_config();
    let outcome = match jetstream.get_stream(STREAM_NAME).await {
        Ok(stream) => reconcile(jetstream, &stream.cached_info().config, &wanted).await?,
        Err(error) if is_not_found(&error) => match jetstream.create_stream(wanted.clone()).await {
            Ok(_) => Outcome::Created,
            // Another component created a bare stream between the read and
            // the create: reconcile that one rather than fail over a race
            // the deployment's start order makes ordinary.
            Err(create_error) => match jetstream.get_stream(STREAM_NAME).await {
                Ok(stream) => reconcile(jetstream, &stream.cached_info().config, &wanted).await?,
                Err(_) => {
                    return Err(create_error)
                        .with_context(|| format!("failed to create the {STREAM_NAME} stream"))
                }
            },
        },
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read the {STREAM_NAME} stream"))
        }
    };
    match &outcome {
        Outcome::Created => info!(
            stream = STREAM_NAME,
            %policy,
            "created the bus's stream with the retention policy (ADR 0037)"
        ),
        Outcome::Unchanged => info!(
            stream = STREAM_NAME,
            %policy,
            "the bus's stream already carries the retention policy"
        ),
        Outcome::Updated(changes) => {
            for change in changes {
                info!(
                    stream = STREAM_NAME,
                    field = change.field,
                    old = %change.old,
                    new = %change.new,
                    "bus stream policy updated in place: {change}"
                );
            }
            info!(
                stream = STREAM_NAME,
                %policy,
                changed = changes.len(),
                "the bus's stream now carries the retention policy (ADR 0037); events already \
                 past the new age expire from here on"
            );
        }
        Outcome::Refused { changes, error } => error!(
            stream = STREAM_NAME,
            fields = ?changes.iter().map(|c| c.field).collect::<Vec<_>>(),
            %error,
            "the bus refused to update the stream's retention policy in place, so the Sensor \
             runs on the stream's existing policy ({}). A field JetStream cannot change on a \
             live stream (its storage, its retention) needs the operator's act, and Twalk never \
             takes it: either keep the stream as it is, or — knowing that every event on it is \
             then gone for good — remove it (`nats stream rm {STREAM_NAME}` with the nats CLI \
             on the host, or by removing the bus's data volume) and restart the Sensor, which \
             creates it anew with the policy (ADR 0037)",
            changes
                .iter()
                .map(|c| format!("{}={}", c.field, c.old))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    }
    Ok(outcome)
}

async fn reconcile(
    jetstream: &async_nats::jetstream::Context,
    existing: &Config,
    wanted: &Config,
) -> Result<Outcome> {
    let changes = diff(existing, wanted);
    if changes.is_empty() {
        return Ok(Outcome::Unchanged);
    }
    match jetstream.update_stream(wanted).await {
        Ok(_) => Ok(Outcome::Updated(changes)),
        Err(error) => refused_or_not_answered(changes, error),
    }
}

/// What a failed `update_stream` means. Only an answer **from the server** —
/// a JetStream API error, which carries the bus's own reason ("stream
/// configuration update can not change storage type") — is a refusal the
/// Sensor can run on: the stream is there, with a policy, and the operator is
/// told which. A timeout, an unavailable JetStream or a transport failure is
/// a bus that could not be asked, and is an error the way the read path's is:
/// the process does not run on a guess about a policy it could not even ask
/// about, and it must not be told to remove a stream over a timeout.
fn refused_or_not_answered(changes: Vec<FieldChange>, error: UpdateStreamError) -> Result<Outcome> {
    match error.kind() {
        CreateStreamErrorKind::JetStream(_) => Ok(Outcome::Refused {
            changes,
            error: error.to_string(),
        }),
        _ => Err(error).with_context(|| {
            format!(
                "failed to update the {STREAM_NAME} stream's retention policy: the bus did not \
                 answer"
            )
        }),
    }
}

fn is_not_found(error: &async_nats::jetstream::context::GetStreamError) -> bool {
    matches!(
        error.kind(),
        GetStreamErrorKind::JetStream(js) if js.error_code() == ErrorCode::STREAM_NOT_FOUND
    )
}

/// A duration as the bus reads it, in seconds, with the unit a human reads
/// it in when it is a whole one: `7776000s (90d)`, `86400s (1d)`, `120s (2m)`,
/// and `0s (for ever)` for the value NATS reads as no limit.
pub fn render_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs == 0 {
        return "0s (for ever)".to_owned();
    }
    let unit = [(86_400, "d"), (3_600, "h"), (60, "m")]
        .into_iter()
        .find(|(size, _)| secs.is_multiple_of(*size))
        .map(|(size, suffix)| format!("{}{suffix}", secs / size));
    match unit {
        Some(unit) => format!("{secs}s ({unit})"),
        None => format!("{secs}s"),
    }
}

fn render_compression(compression: Option<&Compression>) -> String {
    match compression {
        Some(Compression::S2) => "s2".to_owned(),
        Some(Compression::None) => "none".to_owned(),
        // A stream the bus reports no compression field for, which a server
        // older than 2.10 does: it compresses nothing.
        None => "none".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decided() -> StreamPolicy {
        StreamPolicy::new(
            StreamPolicy::DEFAULT_MAX_AGE_DAYS,
            StreamPolicy::DEFAULT_MAX_BYTES,
            StreamPolicy::DEFAULT_DUPLICATE_WINDOW_SECONDS,
        )
        .expect("the decided values are a valid policy")
    }

    /// The stream a `..Default::default()` deployment left behind, as the
    /// bus echoes it: no age, no size, no compression, two minutes of
    /// duplicate tracking — the state read off the reference deployment's
    /// own `meta.inf` on 2026-09-19.
    fn as_nats_defaults_it() -> Config {
        Config {
            name: STREAM_NAME.to_owned(),
            subjects: vec!["twalk.>".to_owned()],
            max_age: Duration::ZERO,
            max_bytes: -1,
            max_messages: -1,
            max_messages_per_subject: -1,
            max_consumers: -1,
            max_message_size: -1,
            num_replicas: 1,
            compression: Some(Compression::None),
            duplicate_window: Duration::from_secs(120),
            ..Default::default()
        }
    }

    #[test]
    fn the_config_names_the_decided_values() {
        let config = decided().stream_config();
        assert_eq!(config.name, "twalk");
        assert_eq!(config.subjects, vec!["twalk.>".to_owned()]);
        assert_eq!(config.max_age, Duration::from_secs(90 * 86_400));
        assert_eq!(config.max_bytes, 2_147_483_648);
        assert_eq!(config.discard, DiscardPolicy::Old);
        assert_eq!(config.compression, Some(Compression::S2));
        assert_eq!(config.duplicate_window, Duration::from_secs(86_400));
        assert_eq!(config.storage, StorageType::File);
        assert_eq!(config.num_replicas, 1);
        assert_eq!(config.retention, RetentionPolicy::Limits);
        assert_eq!(config.max_messages, -1);
        assert_eq!(config.max_messages_per_subject, -1);
        assert!(
            !config.deny_delete && !config.deny_purge,
            "the operator's purge stays possible: it is their act, not this module's"
        );
    }

    #[test]
    fn the_diff_lists_exactly_what_the_policy_changes_on_a_default_stream() {
        let changes = diff(&as_nats_defaults_it(), &decided().stream_config());
        let fields: Vec<&str> = changes.iter().map(|c| c.field).collect();
        assert_eq!(
            fields,
            vec!["max_age", "max_bytes", "compression", "duplicate_window"],
            "the four fields the operator decided, and nothing the bus filled in by default: \
             {changes:?}"
        );
        let rendered: Vec<String> = changes.iter().map(|c| c.to_string()).collect();
        assert_eq!(
            rendered,
            vec![
                "max_age: 0s (for ever) → 7776000s (90d)",
                "max_bytes: -1 → 2147483648",
                "compression: none → s2",
                "duplicate_window: 120s (2m) → 86400s (1d)",
            ]
        );
    }

    #[test]
    fn a_stream_that_carries_the_policy_has_no_diff() {
        let wanted = decided().stream_config();
        assert!(diff(&wanted, &wanted).is_empty());
        // The bus echoes what it was given plus its own defaults for what
        // was left unset; none of those is a change.
        let echoed = Config {
            description: None,
            metadata: Default::default(),
            ..wanted.clone()
        };
        assert!(diff(&echoed, &wanted).is_empty());
    }

    #[test]
    fn a_field_the_bus_cannot_change_is_still_in_the_diff() {
        // Storage cannot change in place: the diff names it so the refusal
        // can, and the decision of what to do about it is the operator's.
        let existing = Config {
            storage: StorageType::Memory,
            ..decided().stream_config()
        };
        let changes = diff(&existing, &decided().stream_config());
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].to_string(), "storage: memory → file");
    }

    #[test]
    fn the_values_nats_reads_as_no_limit_are_refused_by_name() {
        for (days, bytes, window, variable) in [
            (0, 1, 1, "SENSOR_BUS_MAX_AGE_DAYS"),
            (1, 0, 1, "SENSOR_BUS_MAX_BYTES"),
            (1, -1, 1, "SENSOR_BUS_MAX_BYTES"),
            (1, 1, 0, "SENSOR_BUS_DUPLICATE_WINDOW_SECONDS"),
        ] {
            let error = StreamPolicy::new(days, bytes, window).unwrap_err();
            assert!(
                error.to_string().contains(variable),
                "({days}, {bytes}, {window}) names {variable}: {error}"
            );
        }
    }

    #[test]
    fn an_age_that_would_wrap_to_for_ever_is_refused_by_name() {
        for days in [
            StreamPolicy::MAX_AGE_DAYS_CEILING + 1,
            // 292 years: the nanoseconds no longer fit the bus's signed integer.
            106_752,
            u64::MAX / 86_400 + 1,
            u64::MAX,
        ] {
            let error = StreamPolicy::new(days, 1, 1).unwrap_err();
            assert!(
                error.to_string().contains("SENSOR_BUS_MAX_AGE_DAYS"),
                "{days}: {error}"
            );
        }
        assert!(
            StreamPolicy::new(StreamPolicy::MAX_AGE_DAYS_CEILING, 1, 1).is_ok(),
            "the ceiling itself is allowed"
        );
    }

    #[test]
    fn only_an_answer_from_the_bus_is_a_refusal() {
        use async_nats::jetstream::context::CreateStreamError;

        let changes = || {
            vec![FieldChange {
                field: "storage",
                old: "memory".to_owned(),
                new: "file".to_owned(),
            }]
        };
        // The server's own answer, as it comes off the wire: the bus is
        // there, it has a policy, and it will not change this field.
        let api_error: async_nats::jetstream::Error = serde_json::from_value(serde_json::json!({
            "code": 500,
            "err_code": 10052,
            "description": "stream configuration update can not change storage type"
        }))
        .expect("a JetStream API error deserialises");
        let refused = refused_or_not_answered(changes(), CreateStreamError::from(api_error))
            .expect("a refusal is an outcome, not a failure");
        match refused {
            Outcome::Refused { changes, error } => {
                assert_eq!(changes[0].field, "storage");
                assert!(error.contains("can not change storage type"), "{error}");
            }
            other => panic!("expected Refused, got {other:?}"),
        }

        // No answer at all: the Sensor does not run on a guess about a policy
        // it could not ask about, and is never told to remove a stream over
        // a timeout.
        for kind in [
            CreateStreamErrorKind::TimedOut,
            CreateStreamErrorKind::JetStreamUnavailable,
            CreateStreamErrorKind::Response,
            CreateStreamErrorKind::ResponseParse,
            CreateStreamErrorKind::NotFound,
        ] {
            let error = refused_or_not_answered(changes(), CreateStreamError::new(kind.clone()))
                .expect_err("a bus that did not answer is an error");
            assert!(
                error.to_string().contains("did not answer"),
                "{kind:?}: {error:#}"
            );
        }
    }

    #[test]
    fn a_window_longer_than_the_age_is_refused_before_the_bus_refuses_it() {
        let error = StreamPolicy::new(1, 1, 86_401).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("SENSOR_BUS_DUPLICATE_WINDOW_SECONDS")
                && error.to_string().contains("SENSOR_BUS_MAX_AGE_DAYS"),
            "{error}"
        );
        assert!(StreamPolicy::new(1, 1, 86_400).is_ok(), "equal is allowed");
    }

    #[test]
    fn a_duration_is_rendered_in_the_unit_a_human_reads() {
        assert_eq!(render_duration(Duration::ZERO), "0s (for ever)");
        assert_eq!(render_duration(Duration::from_secs(90)), "90s");
        assert_eq!(render_duration(Duration::from_secs(120)), "120s (2m)");
        assert_eq!(render_duration(Duration::from_secs(7_200)), "7200s (2h)");
        assert_eq!(render_duration(Duration::from_secs(86_400)), "86400s (1d)");
    }
}
