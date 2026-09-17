//! Metrics: a handful of process counters and gauges rendered in the
//! Prometheus text exposition format, served over HTTP by the binary (see
//! `SENSOR_METRICS_LISTEN`). No metrics crate: the Sensor needs four counters
//! and a gauge, and a hand-rolled exposition is smaller than any dependency.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// When the last sync response completed, in seconds since the epoch.
    /// Zero until the first sync completes.
    last_sync_unix_seconds: AtomicU64,
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
            last_sync_unix_seconds: AtomicU64::new(0),
        }
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

    pub fn record_sync(&self, now_unix_seconds: u64) {
        self.last_sync_unix_seconds
            .store(now_unix_seconds, Ordering::Relaxed);
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
        metrics.record_sync(1_000);

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
            body.contains("twalk_sensor_last_sync_age_seconds 30\n"),
            "{body}"
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
}
