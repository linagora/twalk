//! Metrics: a handful of process counters and gauges rendered in the
//! Prometheus text exposition format, served over HTTP by the binary (see
//! `SENSOR_METRICS_LISTEN`). No metrics crate: the Sensor needs a handful of
//! counters and two gauges, and a hand-rolled exposition is smaller than any
//! dependency.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// Process-wide health signals. Cheap to clone into every task: all state is
/// shared behind atomics and a mutex.
pub struct Metrics {
    /// Events published to the bus (publish acked), by contract event type.
    events_published: Mutex<BTreeMap<String, u64>>,
    /// Events the crypto stack could not decrypt (ticket 04).
    decryption_failures: AtomicU64,
    /// Failed attempts to post an approved reply into its portal room.
    outbound_send_failures: AtomicU64,
    /// Events moved to the dead-letter subject after exhausting retries.
    dead_lettered_events: AtomicU64,
    /// Failed reads of the Companion Gateway's consent snapshot (ADR 0010).
    /// Every failure means the Sensor is labelling senders `pending` that the
    /// user may well have granted, so it is counted and not only logged: a
    /// degraded label is invisible in the events themselves.
    consent_snapshot_failures: AtomicU64,
    /// Entries the last applied consent snapshot held, and whether one has
    /// been applied at all: a Sensor still retrying an unreachable Gateway
    /// must not read as one that recovered an empty state.
    consent_snapshot_entries: AtomicU64,
    consent_snapshot_applied: AtomicBool,
    /// When the last sync response completed, in seconds since the epoch.
    /// Zero until the first sync completes.
    last_sync_unix_seconds: AtomicU64,
    /// Rooms the Sensor is joined to, and what became of the invitations it
    /// was sent (ticket #105).
    ///
    /// Observation scope is invitation-driven and starts empty, which is the
    /// right default and also the shape of the worst defect this product has
    /// had: a deployment sat outside seventeen of its eighteen conversations
    /// while every component reported itself healthy, and nothing anywhere
    /// said so. The gauge is the Sensor's own half of that answer — the
    /// Companion Gateway states how many conversations exist, this states
    /// how many are actually being read.
    ///
    /// The `ignored` counter is the one an operator reads after "I chose a
    /// conversation and nothing happened": an invitation from a user
    /// `SENSOR_ALLOWED_INVITERS` does not name is refused, correctly and
    /// silently, and the silence used to be the only symptom.
    observed_rooms: AtomicU64,
    invites_joined: AtomicU64,
    invites_ignored: AtomicU64,
    invites_failed: AtomicU64,
    /// Observed events the Sensor deliberately did not publish, by why.
    ///
    /// Counted rather than only logged, because both reasons are *silences*
    /// and a silence is the one failure this product has repeatedly shipped
    /// without noticing. `bridge_bot` climbs twice a minute per bridge on a
    /// correctly configured deployment (issue #152) — which is also how an
    /// operator sees that `SENSOR_BRIDGE_BOTS` is doing something, since the
    /// alternative reading of a flat zero is that they misspelled a Matrix ID.
    /// `unattributable_subject` is the one to watch: it means a subject was
    /// dropped because the Sensor could not say which network they are on
    /// (issue #150), and a real person behind it is a person missing from the
    /// bus.
    dropped_bridge_bot: AtomicU64,
    dropped_unattributable_subject: AtomicU64,
    dropped_tombstoned_room: AtomicU64,
    dropped_unknown_connection: AtomicU64,
    /// Consent entries and changes the Sensor refused, by why (#271):
    /// `no_connection` is a Gateway older than #270 serving a state this
    /// Sensor will not guess the perimeter of; `malformed` a document
    /// missing what the contract requires. Named like the dropped events:
    /// a thing the Sensor deliberately did not do, counted so that its
    /// absence is not mistaken for silence.
    consent_refused_no_connection: AtomicU64,
    consent_refused_malformed: AtomicU64,
    /// The owner's own device (ADR 0025, issue #123): whether the deployment
    /// has one at all, how many portal rooms it is joined to, and what became
    /// of the invitations it was sent.
    ///
    /// The gauge renders **only** when the device is configured, so an absent
    /// sample means "this deployment posts approved replies as `@sensor:` and
    /// the bridge ignores them" rather than "the device is in no rooms" — two
    /// facts a flat zero would merge, which is the shape of the defect itself.
    ///
    /// `refused` is the one to read after "the user approved a reply and the
    /// contact got nothing": the owner's device joins a room only when a
    /// configured bridge bot invited it, and a climbing `refused` with a flat
    /// `joined` is `SENSOR_BRIDGE_BOTS` naming the wrong accounts.
    owner_device_present: AtomicBool,
    owner_device_rooms: AtomicU64,
    owner_device_invites_joined: AtomicU64,
    owner_device_invites_refused: AtomicU64,
    owner_device_invites_failed: AtomicU64,
    owner_device_invites_unjoinable: AtomicU64,
    /// Portal rooms the owner's device holds an invitation to and can never
    /// join (issue #237). Each is a conversation the user chose in which an
    /// approved reply cannot be delivered as the user — the fact #216 needs —
    /// and until this gauge it lived in a log line and nowhere else.
    owner_device_unjoinable_portals: AtomicU64,
    /// Approved replies the Sensor posted, by what they reached (issue #216).
    ///
    /// This is the count behind the sentence the approval screen was getting
    /// wrong: a reply is *published on the bus* and *posted into a room*, and
    /// neither of those is *delivered to the contact*. `nobody` climbing is a
    /// deployment whose every reply stops at Synapse.
    replies_reaching_contact: AtomicU64,
    replies_reaching_nobody: AtomicU64,
}

/// Why an observed event was deliberately not published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// The subject is a bridge's own bot: a service identity, neither the
    /// owner nor a contact (issue #152).
    BridgeBot,
    /// The Sensor cannot attribute the subject to one network, so publishing
    /// would name one on a guess — and the connection, which consent is
    /// looked up by, is resolved from it (issue #150, #271).
    UnattributableSubject,
    /// The room carries an `m.room.tombstone`: it was replaced, and what
    /// still arrives in it is stray (issue #254, ADR 0029). Publishing it
    /// would attribute a conversation to a room the register no longer
    /// lists.
    TombstonedRoom,
    /// No connection of the registry covers the event's room (ADR 0033,
    /// #269): a bridge no connection names. Published under a guessed
    /// perimeter, the event would be governed by decisions about another
    /// account, so it is not published at all — and this counter is what
    /// says so.
    UnknownConnection,
}

impl DropReason {
    /// The metric's label value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BridgeBot => "bridge_bot",
            Self::UnattributableSubject => "unattributable_subject",
            Self::TombstonedRoom => "tombstoned_room",
            Self::UnknownConnection => "unknown_connection",
        }
    }
}

/// What became of one invitation sent to the owner's own device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerDeviceInvite {
    /// A portal of a configured bridge: joined, so the owner is a member of the
    /// room their conversation lives in.
    Joined,
    /// Not a portal of a configured bridge — the inviter is somebody else, or
    /// the deployment named no bridge bots at all.
    Refused,
    /// A portal invitation the join request failed on for a reason that can
    /// clear — a throttle, a server error, no answer. Counted per attempt, so
    /// it climbs while the cause lasts and stops when it clears.
    Failed,
    /// A portal invitation the homeserver refused in a way that will not
    /// change (issue #237): the room is gone or unreachable, the invitation is
    /// no longer valid. Counted **once per room**, like a refusal, because a
    /// permanent answer repeated at sync frequency is noise that hides the
    /// next real failure.
    Unjoinable,
}

impl OwnerDeviceInvite {
    /// The metric's label value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Joined => "joined",
            Self::Refused => "refused",
            Self::Failed => "failed",
            Self::Unjoinable => "unjoinable",
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            events_published: Mutex::new(BTreeMap::new()),
            decryption_failures: AtomicU64::new(0),
            outbound_send_failures: AtomicU64::new(0),
            dead_lettered_events: AtomicU64::new(0),
            consent_snapshot_failures: AtomicU64::new(0),
            consent_snapshot_entries: AtomicU64::new(0),
            consent_snapshot_applied: AtomicBool::new(false),
            last_sync_unix_seconds: AtomicU64::new(0),
            observed_rooms: AtomicU64::new(0),
            invites_joined: AtomicU64::new(0),
            invites_ignored: AtomicU64::new(0),
            invites_failed: AtomicU64::new(0),
            dropped_bridge_bot: AtomicU64::new(0),
            dropped_unattributable_subject: AtomicU64::new(0),
            dropped_tombstoned_room: AtomicU64::new(0),
            dropped_unknown_connection: AtomicU64::new(0),
            consent_refused_no_connection: AtomicU64::new(0),
            consent_refused_malformed: AtomicU64::new(0),
            owner_device_present: AtomicBool::new(false),
            owner_device_rooms: AtomicU64::new(0),
            owner_device_invites_joined: AtomicU64::new(0),
            owner_device_invites_refused: AtomicU64::new(0),
            owner_device_invites_failed: AtomicU64::new(0),
            owner_device_invites_unjoinable: AtomicU64::new(0),
            owner_device_unjoinable_portals: AtomicU64::new(0),
            replies_reaching_contact: AtomicU64::new(0),
            replies_reaching_nobody: AtomicU64::new(0),
        }
    }

    /// Counts an observed event the Sensor deliberately did not publish.
    /// Returns the running total, for the log line.
    pub fn record_dropped(&self, reason: DropReason) -> u64 {
        let counter = match reason {
            DropReason::BridgeBot => &self.dropped_bridge_bot,
            DropReason::UnattributableSubject => &self.dropped_unattributable_subject,
            DropReason::TombstonedRoom => &self.dropped_tombstoned_room,
            DropReason::UnknownConnection => &self.dropped_unknown_connection,
        };
        counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Counts a consent entry or change the Sensor refused, or `None` for
    /// one that is not a defect — a `persona` decision is well-formed
    /// traffic that never labels a sender. Returns the running total, for
    /// the log line.
    pub fn record_consent_refused(&self, why: crate::consent::Unusable) -> Option<u64> {
        let counter = match why {
            crate::consent::Unusable::NotAboutASender => return None,
            crate::consent::Unusable::NoConnection => &self.consent_refused_no_connection,
            crate::consent::Unusable::Malformed => &self.consent_refused_malformed,
        };
        Some(counter.fetch_add(1, Ordering::Relaxed) + 1)
    }

    pub fn record_published(&self, event_type: &str) {
        *self
            .events_published
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(event_type.to_owned())
            .or_insert(0) += 1;
    }

    /// Returns the running total, for the failure log line.
    pub fn record_decryption_failure(&self) -> u64 {
        self.decryption_failures.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn record_outbound_send_failure(&self) {
        self.outbound_send_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_dead_lettered(&self) {
        self.dead_lettered_events.fetch_add(1, Ordering::Relaxed);
    }

    /// Returns the running total, for the failure log line.
    pub fn record_consent_snapshot_failure(&self) -> u64 {
        self.consent_snapshot_failures
            .fetch_add(1, Ordering::Relaxed)
            + 1
    }

    pub fn record_consent_snapshot(&self, entries: usize) {
        self.consent_snapshot_entries
            .store(entries as u64, Ordering::Relaxed);
        self.consent_snapshot_applied.store(true, Ordering::Relaxed);
    }

    pub fn record_sync(&self, now_unix_seconds: u64) {
        self.last_sync_unix_seconds
            .store(now_unix_seconds, Ordering::Relaxed);
    }

    /// How many rooms the Sensor is joined to, as the SDK's own state
    /// answers it — recorded on every completed sync, so the gauge follows a
    /// room joined or left without anything else being asked.
    pub fn record_observed_rooms(&self, rooms: u64) {
        self.observed_rooms.store(rooms, Ordering::Relaxed);
    }

    pub fn record_invite_joined(&self) {
        self.invites_joined.fetch_add(1, Ordering::Relaxed);
    }

    /// An invitation from a user `SENSOR_ALLOWED_INVITERS` does not name.
    /// Returns the running total, for the log line: on a deployment whose
    /// bridge bots were left out of that list this climbs once per
    /// conversation the user chose, which is the whole diagnosis.
    pub fn record_invite_ignored(&self) -> u64 {
        self.invites_ignored.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn record_invite_failed(&self) {
        self.invites_failed.fetch_add(1, Ordering::Relaxed);
    }

    /// The deployment holds a device of the owner's own account: recorded once
    /// at startup, and what makes the room gauge below render at all.
    pub fn record_owner_device_present(&self) {
        self.owner_device_present.store(true, Ordering::Relaxed);
    }

    /// How many rooms the owner's own device is joined to, as the SDK's own
    /// state answers it — the number a bridge's relaying depends on.
    pub fn record_owner_device_rooms(&self, rooms: u64) {
        self.owner_device_rooms.store(rooms, Ordering::Relaxed);
    }

    /// What became of one invitation sent to the owner's device. Returns the
    /// running total of that outcome, for the log line.
    pub fn record_owner_device_invite(&self, outcome: OwnerDeviceInvite) -> u64 {
        let counter = match outcome {
            OwnerDeviceInvite::Joined => &self.owner_device_invites_joined,
            OwnerDeviceInvite::Refused => &self.owner_device_invites_refused,
            OwnerDeviceInvite::Failed => &self.owner_device_invites_failed,
            OwnerDeviceInvite::Unjoinable => &self.owner_device_invites_unjoinable,
        };
        counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// How many portals the owner's device has been invited to and given up
    /// joining, because the homeserver's answer cannot change (issue #237).
    pub fn record_owner_device_unjoinable_portals(&self, rooms: u64) {
        self.owner_device_unjoinable_portals
            .store(rooms, Ordering::Relaxed);
    }

    /// What one posted approved reply reached (issue #216).
    pub fn record_reply_reach(&self, reach: crate::owner_device::Reach) {
        let counter = if reach.reaches_the_contact() {
            &self.replies_reaching_contact
        } else {
            &self.replies_reaching_nobody
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Renders the Prometheus text exposition (format version 0.0.4). `now`
    /// is the scrape time in seconds since the epoch, passed in so the sync
    /// age is computed against a clock the caller controls.
    pub fn render(&self, now_unix_seconds: u64) -> String {
        let mut out = String::new();
        out.push_str("# HELP twalk_sensor_events_published_total CloudEvents published to the bus, by contract event type.\n");
        out.push_str("# TYPE twalk_sensor_events_published_total counter\n");
        for (event_type, count) in self
            .events_published
            .lock()
            .expect("the metrics mutex is never poisoned")
            .iter()
        {
            out.push_str(&format!(
                "twalk_sensor_events_published_total{{type=\"{event_type}\"}} {count}\n"
            ));
        }
        out.push_str("# HELP twalk_sensor_decryption_failures_total Events the crypto stack could not decrypt, skipped.\n");
        out.push_str("# TYPE twalk_sensor_decryption_failures_total counter\n");
        out.push_str(&format!(
            "twalk_sensor_decryption_failures_total {}\n",
            self.decryption_failures.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP twalk_sensor_outbound_send_failures_total Failed attempts to post an approved reply into its portal room.\n");
        out.push_str("# TYPE twalk_sensor_outbound_send_failures_total counter\n");
        out.push_str(&format!(
            "twalk_sensor_outbound_send_failures_total {}\n",
            self.outbound_send_failures.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP twalk_sensor_dead_lettered_events_total Events moved to the dead-letter subject after exhausting their retries.\n");
        out.push_str("# TYPE twalk_sensor_dead_lettered_events_total counter\n");
        out.push_str(&format!(
            "twalk_sensor_dead_lettered_events_total {}\n",
            self.dead_lettered_events.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP twalk_sensor_consent_snapshot_failures_total Failed reads of the Companion Gateway's consent snapshot.\n");
        out.push_str("# TYPE twalk_sensor_consent_snapshot_failures_total counter\n");
        out.push_str(&format!(
            "twalk_sensor_consent_snapshot_failures_total {}\n",
            self.consent_snapshot_failures.load(Ordering::Relaxed)
        ));
        // Renders only once a snapshot has been applied, so that an absent
        // sample and an empty state stay distinguishable.
        if self.consent_snapshot_applied.load(Ordering::Relaxed) {
            out.push_str("# HELP twalk_sensor_consent_snapshot_entries (subject, network) entries the applied consent snapshot held.\n");
            out.push_str("# TYPE twalk_sensor_consent_snapshot_entries gauge\n");
            out.push_str(&format!(
                "twalk_sensor_consent_snapshot_entries {}\n",
                self.consent_snapshot_entries.load(Ordering::Relaxed)
            ));
        }
        out.push_str("# HELP twalk_sensor_observed_rooms Rooms the Sensor has joined, and whose traffic therefore reaches the bus.\n");
        out.push_str("# TYPE twalk_sensor_observed_rooms gauge\n");
        out.push_str(&format!(
            "twalk_sensor_observed_rooms {}\n",
            self.observed_rooms.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP twalk_sensor_invites_total Invitations the Sensor received, by what it did with them.\n");
        out.push_str("# TYPE twalk_sensor_invites_total counter\n");
        for (outcome, count) in [
            ("joined", &self.invites_joined),
            ("ignored", &self.invites_ignored),
            ("failed", &self.invites_failed),
        ] {
            out.push_str(&format!(
                "twalk_sensor_invites_total{{outcome=\"{outcome}\"}} {}\n",
                count.load(Ordering::Relaxed)
            ));
        }
        out.push_str("# HELP twalk_sensor_events_dropped_total Observed events the Sensor deliberately did not publish, by why.\n");
        out.push_str("# TYPE twalk_sensor_events_dropped_total counter\n");
        for (reason, count) in [
            (DropReason::BridgeBot, &self.dropped_bridge_bot),
            (
                DropReason::UnattributableSubject,
                &self.dropped_unattributable_subject,
            ),
            (DropReason::TombstonedRoom, &self.dropped_tombstoned_room),
            (
                DropReason::UnknownConnection,
                &self.dropped_unknown_connection,
            ),
        ] {
            out.push_str(&format!(
                "twalk_sensor_events_dropped_total{{reason=\"{}\"}} {}\n",
                reason.as_str(),
                count.load(Ordering::Relaxed)
            ));
        }
        out.push_str("# HELP twalk_sensor_consent_refused_total Consent entries and changes the Sensor refused, by why: no_connection is a Gateway older than #270, whose state this Sensor will not guess the perimeter of; malformed is a document missing what the contract requires.\n");
        out.push_str("# TYPE twalk_sensor_consent_refused_total counter\n");
        for (reason, count) in [
            ("no_connection", &self.consent_refused_no_connection),
            ("malformed", &self.consent_refused_malformed),
        ] {
            out.push_str(&format!(
                "twalk_sensor_consent_refused_total{{reason=\"{reason}\"}} {}\n",
                count.load(Ordering::Relaxed)
            ));
        }
        out.push_str("# HELP twalk_sensor_owner_device_invites_total Invitations the owner's own device received, by what it did with them.\n");
        out.push_str("# TYPE twalk_sensor_owner_device_invites_total counter\n");
        for (outcome, count) in [
            (OwnerDeviceInvite::Joined, &self.owner_device_invites_joined),
            (
                OwnerDeviceInvite::Refused,
                &self.owner_device_invites_refused,
            ),
            (OwnerDeviceInvite::Failed, &self.owner_device_invites_failed),
            (
                OwnerDeviceInvite::Unjoinable,
                &self.owner_device_invites_unjoinable,
            ),
        ] {
            out.push_str(&format!(
                "twalk_sensor_owner_device_invites_total{{outcome=\"{}\"}} {}\n",
                outcome.as_str(),
                count.load(Ordering::Relaxed)
            ));
        }
        out.push_str("# HELP twalk_sensor_outbound_replies_total Approved replies posted, by what they reached: the contact, or nobody.\n");
        out.push_str("# TYPE twalk_sensor_outbound_replies_total counter\n");
        for (reach, count) in [
            (
                crate::owner_device::Reach::Contact,
                &self.replies_reaching_contact,
            ),
            (
                crate::owner_device::Reach::Nobody,
                &self.replies_reaching_nobody,
            ),
        ] {
            out.push_str(&format!(
                "twalk_sensor_outbound_replies_total{{reach=\"{}\"}} {}\n",
                reach.as_str(),
                count.load(Ordering::Relaxed)
            ));
        }
        // Renders only when the deployment holds a device of the owner's own
        // account, so an absent sample says "approved replies go out as
        // @sensor: and no bridge relays them" (issue #123) rather than "the
        // device is in no rooms yet". Two facts a zero would merge.
        if self.owner_device_present.load(Ordering::Relaxed) {
            out.push_str("# HELP twalk_sensor_owner_device_rooms Portal rooms the owner's own device has joined, and whose conversations a bridge will therefore relay its replies to.\n");
            out.push_str("# TYPE twalk_sensor_owner_device_rooms gauge\n");
            out.push_str(&format!(
                "twalk_sensor_owner_device_rooms {}\n",
                self.owner_device_rooms.load(Ordering::Relaxed)
            ));
            out.push_str("# HELP twalk_sensor_owner_device_unjoinable_portals Portal rooms the owner's own device was invited to and can never join, so an approved reply there cannot be delivered as the user.\n");
            out.push_str("# TYPE twalk_sensor_owner_device_unjoinable_portals gauge\n");
            out.push_str(&format!(
                "twalk_sensor_owner_device_unjoinable_portals {}\n",
                self.owner_device_unjoinable_portals.load(Ordering::Relaxed)
            ));
        }
        // The sync age only exists once a sync has completed; a Sensor that
        // never synced renders no sample rather than a misleading zero.
        let last_sync = self.last_sync_unix_seconds.load(Ordering::Relaxed);
        if last_sync > 0 {
            out.push_str("# HELP twalk_sensor_last_sync_age_seconds Seconds since the last completed sync response.\n");
            out.push_str("# TYPE twalk_sensor_last_sync_age_seconds gauge\n");
            out.push_str(&format!(
                "twalk_sensor_last_sync_age_seconds {}\n",
                now_unix_seconds.saturating_sub(last_sync)
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exposition_is_prometheus_shaped() {
        let metrics = Metrics::new();
        metrics.record_published("fr.linagora.twalk.inbound.message.received.v1");
        metrics.record_published("fr.linagora.twalk.inbound.message.received.v1");
        metrics.record_published("fr.linagora.twalk.inbound.reaction.added.v1");
        metrics.record_decryption_failure();
        metrics.record_outbound_send_failure();
        metrics.record_dead_lettered();
        metrics.record_consent_snapshot_failure();
        metrics.record_consent_snapshot(3);
        metrics.record_sync(1_000);
        assert_eq!(metrics.record_dropped(DropReason::BridgeBot), 1);
        assert_eq!(
            metrics.record_dropped(DropReason::BridgeBot),
            2,
            "the running total is what the log line names"
        );
        assert_eq!(metrics.record_dropped(DropReason::UnattributableSubject), 1);

        let body = metrics.render(1_030);
        assert!(
            body.contains("twalk_sensor_events_published_total{type=\"fr.linagora.twalk.inbound.message.received.v1\"} 2\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_sensor_events_published_total{type=\"fr.linagora.twalk.inbound.reaction.added.v1\"} 1\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_sensor_decryption_failures_total 1\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_sensor_outbound_send_failures_total 1\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_sensor_dead_lettered_events_total 1\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_sensor_consent_snapshot_failures_total 1\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_sensor_consent_snapshot_entries 3\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_sensor_last_sync_age_seconds 30\n"),
            "{body}"
        );
        assert!(body.contains("twalk_sensor_observed_rooms 0\n"), "{body}");
        assert!(
            body.contains("twalk_sensor_events_dropped_total{reason=\"bridge_bot\"} 2\n")
                && body.contains(
                    "twalk_sensor_events_dropped_total{reason=\"unattributable_subject\"} 1\n"
                ),
            "{body}"
        );
        assert!(
            body.contains("twalk_sensor_invites_total{outcome=\"ignored\"} 0\n"),
            "every outcome exists at zero, so a Sensor refusing every inviter is \
             distinguishable from one nobody ever invited: {body}"
        );
        // Every sample line is `name[labels] value` with an integer value.
        for line in body.lines().filter(|line| !line.starts_with('#')) {
            let (name, value) = line.rsplit_once(' ').expect("a sample line has a value");
            assert!(name.starts_with("twalk_sensor_"), "{line}");
            value.parse::<u64>().expect("the value is an integer");
        }
    }

    #[test]
    fn the_sync_age_is_omitted_until_the_first_sync() {
        let metrics = Metrics::new();
        assert!(
            !metrics
                .render(1_000)
                .contains("twalk_sensor_last_sync_age_seconds"),
            "a Sensor that never synced renders no sync age"
        );
    }

    #[test]
    fn every_drop_reason_exists_at_zero() {
        // A Sensor that dropped nothing must be distinguishable from one
        // whose SENSOR_BRIDGE_BOTS names an account that does not exist: an
        // absent sample would read as "no bots on this deployment", which is
        // the misreading that leaves the defect in place.
        let body = Metrics::new().render(1_000);
        for reason in [
            "bridge_bot",
            "unattributable_subject",
            "tombstoned_room",
            "unknown_connection",
        ] {
            assert!(
                body.contains(&format!(
                    "twalk_sensor_events_dropped_total{{reason=\"{reason}\"}} 0\n"
                )),
                "{body}"
            );
        }
    }

    #[test]
    fn a_consent_entry_the_sensor_refused_is_counted_by_why_and_a_persona_is_not() {
        // A Gateway older than #270 serves entries with a network and no
        // connection: refused rather than read as the network's connection
        // (#271), and this is the number an operator reads it from. A
        // persona decision is well-formed and counted by nobody.
        let metrics = Metrics::new();
        let body = metrics.render(1_000);
        for reason in ["no_connection", "malformed"] {
            assert!(
                body.contains(&format!(
                    "twalk_sensor_consent_refused_total{{reason=\"{reason}\"}} 0\n"
                )),
                "{body}"
            );
        }
        assert_eq!(
            metrics.record_consent_refused(crate::consent::Unusable::NoConnection),
            Some(1)
        );
        assert_eq!(
            metrics.record_consent_refused(crate::consent::Unusable::NoConnection),
            Some(2)
        );
        assert_eq!(
            metrics.record_consent_refused(crate::consent::Unusable::NotAboutASender),
            None
        );
        let body = metrics.render(1_000);
        assert!(body.contains("twalk_sensor_consent_refused_total{reason=\"no_connection\"} 2\n"));
        assert!(!body.contains("persona"), "{body}");
    }

    #[test]
    fn the_owner_device_gauge_tells_no_device_from_a_device_in_no_rooms() {
        // The absent sample is the point: on a deployment with no device of the
        // owner's account every approved reply is posted by @sensor: and
        // relayed by nobody (issue #123), and a zero would read as "the device
        // is in no rooms yet".
        let metrics = Metrics::new();
        assert!(
            !metrics
                .render(1_000)
                .contains("twalk_sensor_owner_device_rooms"),
            "a deployment with no owner device renders no room gauge"
        );
        metrics.record_owner_device_present();
        assert!(
            metrics
                .render(1_000)
                .contains("twalk_sensor_owner_device_rooms 0\n"),
            "a configured device that has joined nothing yet still renders"
        );
        // The same absence rule for the portals it gave up on (issue #237):
        // no device, no sample; a device, a zero that means "none".
        assert!(!Metrics::new()
            .render(1_000)
            .contains("twalk_sensor_owner_device_unjoinable_portals"),);
        metrics.record_owner_device_unjoinable_portals(1);
        assert!(
            metrics
                .render(1_000)
                .contains("twalk_sensor_owner_device_unjoinable_portals 1\n"),
            "{}",
            metrics.render(1_000)
        );
    }

    #[test]
    fn every_reach_and_every_invite_outcome_exists_at_zero() {
        // A Sensor that has posted no reply must be distinguishable from one
        // whose replies all reached the contact, and one that refused every
        // invitation from one nobody ever invited.
        let body = Metrics::new().render(1_000);
        for reach in ["contact", "nobody"] {
            assert!(
                body.contains(&format!(
                    "twalk_sensor_outbound_replies_total{{reach=\"{reach}\"}} 0\n"
                )),
                "{body}"
            );
        }
        for outcome in ["joined", "refused", "failed", "unjoinable"] {
            assert!(
                body.contains(&format!(
                    "twalk_sensor_owner_device_invites_total{{outcome=\"{outcome}\"}} 0\n"
                )),
                "{body}"
            );
        }
    }

    #[test]
    fn a_reply_that_reached_nobody_is_counted_apart() {
        let metrics = Metrics::new();
        metrics.record_reply_reach(crate::owner_device::Reach::Contact);
        metrics.record_reply_reach(crate::owner_device::Reach::Nobody);
        metrics.record_reply_reach(crate::owner_device::Reach::Nobody);
        assert_eq!(
            metrics.record_owner_device_invite(OwnerDeviceInvite::Refused),
            1
        );
        assert_eq!(
            metrics.record_owner_device_invite(OwnerDeviceInvite::Refused),
            2,
            "the running total is what the log line names"
        );
        metrics.record_owner_device_present();
        metrics.record_owner_device_rooms(33);
        let body = metrics.render(1_000);
        assert!(
            body.contains("twalk_sensor_outbound_replies_total{reach=\"contact\"} 1\n")
                && body.contains("twalk_sensor_outbound_replies_total{reach=\"nobody\"} 2\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_sensor_owner_device_invites_total{outcome=\"refused\"} 2\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_sensor_owner_device_rooms 33\n"),
            "{body}"
        );
    }

    #[test]
    fn the_snapshot_gauge_tells_no_snapshot_from_an_empty_one() {
        let metrics = Metrics::new();
        assert!(
            !metrics
                .render(1_000)
                .contains("twalk_sensor_consent_snapshot_entries"),
            "a Sensor that never read a snapshot renders no entry count"
        );
        metrics.record_consent_snapshot(0);
        assert!(
            metrics
                .render(1_000)
                .contains("twalk_sensor_consent_snapshot_entries 0\n"),
            "an applied snapshot of an undecided deployment still renders"
        );
    }
}
