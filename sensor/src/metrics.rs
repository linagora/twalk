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
}

/// Why an observed event was deliberately not published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// The subject is a bridge's own bot: a service identity, neither the
    /// owner nor a contact (issue #152).
    BridgeBot,
    /// The Sensor cannot attribute the subject to one network, so publishing
    /// would name one on a guess — and consent is looked up by
    /// `(subject, network)` (issue #150).
    UnattributableSubject,
}

impl DropReason {
    /// The metric's label value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BridgeBot => "bridge_bot",
            Self::UnattributableSubject => "unattributable_subject",
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
        }
    }

    /// Counts an observed event the Sensor deliberately did not publish.
    /// Returns the running total, for the log line.
    pub fn record_dropped(&self, reason: DropReason) -> u64 {
        let counter = match reason {
            DropReason::BridgeBot => &self.dropped_bridge_bot,
            DropReason::UnattributableSubject => &self.dropped_unattributable_subject,
        };
        counter.fetch_add(1, Ordering::Relaxed) + 1
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
        ] {
            out.push_str(&format!(
                "twalk_sensor_events_dropped_total{{reason=\"{}\"}} {}\n",
                reason.as_str(),
                count.load(Ordering::Relaxed)
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
        for reason in ["bridge_bot", "unattributable_subject"] {
            assert!(
                body.contains(&format!(
                    "twalk_sensor_events_dropped_total{{reason=\"{reason}\"}} 0\n"
                )),
                "{body}"
            );
        }
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
