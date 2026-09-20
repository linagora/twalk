//! Metrics: a handful of process counters and gauges rendered in the
//! Prometheus text exposition format, served over HTTP by the binary (see
//! `Config::listen`). No metrics crate, the same reasoning as the Sensor's
//! own (`sensor/src/metrics.rs`): the clerk needs a handful of counters and
//! two gauges, and a hand-rolled exposition is smaller than any dependency.
//!
//! Every labelled outcome is rendered **at zero**: a channel the clerk has
//! never posted to and a `why` it has never skipped for must still appear
//! in the exposition, because an absent sample and a sample that is
//! genuinely zero are not the same fact, and the difference is the one an
//! operator needs when nothing seems to be happening at all.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// One of the three Buzz channels the clerk writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// One forum post per suggestion.
    Approvals,
    /// What calls for a look — a bridge or consent change worth a sentence.
    Activity,
    /// One line per posted reply.
    Journal,
}

impl Channel {
    /// The metric's label value — the channel's own French name, the way
    /// `CLERK_CHANNEL_*` and `docs/architecture/adr/0035-*.md` name it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approvals => "approbations",
            Self::Activity => "activite",
            Self::Journal => "journal",
        }
    }
}

/// Why a post was deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deleted {
    /// The suggestion it answered for expired (the sweep, `Config::sweep`).
    Expired,
}

impl Deleted {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Expired => "expired",
        }
    }
}

/// Why an event the clerk read off the bus was not turned into a post.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skipped {
    /// The suggestion had already expired by the time the clerk read it.
    Expired,
    /// The event did not deserialise into the shape the clerk expects.
    Unreadable,
    /// A post already exists for this suggestion (the relay is the clerk's
    /// own memory — see `lib.rs` — and a redelivery must not double-post).
    Duplicate,
}

impl Skipped {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::Unreadable => "unreadable",
            Self::Duplicate => "duplicate",
        }
    }
}

/// Process-wide health signals. Cheap to clone into every task: all state is
/// shared behind atomics.
pub struct Metrics {
    posts_approbations: AtomicU64,
    posts_activite: AtomicU64,
    posts_journal: AtomicU64,
    deleted_expired: AtomicU64,
    skipped_expired: AtomicU64,
    skipped_unreadable: AtomicU64,
    skipped_duplicate: AtomicU64,
    /// Failed writes to the relay — a post, a delete, a query that did not
    /// come back with a `2xx`.
    relay_failures: AtomicU64,
    /// Completed sweeps for an expired suggestion's post.
    sweeps: AtomicU64,
    /// When the process started, in seconds since the epoch: recorded once,
    /// at construction.
    started_at_seconds: u64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        let started_at_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            posts_approbations: AtomicU64::new(0),
            posts_activite: AtomicU64::new(0),
            posts_journal: AtomicU64::new(0),
            deleted_expired: AtomicU64::new(0),
            skipped_expired: AtomicU64::new(0),
            skipped_unreadable: AtomicU64::new(0),
            skipped_duplicate: AtomicU64::new(0),
            relay_failures: AtomicU64::new(0),
            sweeps: AtomicU64::new(0),
            started_at_seconds,
        }
    }

    fn posts_counter(&self, channel: Channel) -> &AtomicU64 {
        match channel {
            Channel::Approvals => &self.posts_approbations,
            Channel::Activity => &self.posts_activite,
            Channel::Journal => &self.posts_journal,
        }
    }

    fn skipped_counter(&self, why: Skipped) -> &AtomicU64 {
        match why {
            Skipped::Expired => &self.skipped_expired,
            Skipped::Unreadable => &self.skipped_unreadable,
            Skipped::Duplicate => &self.skipped_duplicate,
        }
    }

    /// One post written to `channel`. Returns the running total, for the
    /// log line.
    pub fn record_post(&self, channel: Channel) -> u64 {
        self.posts_counter(channel).fetch_add(1, Ordering::Relaxed) + 1
    }

    /// One post deleted, and why. Returns the running total.
    pub fn record_deleted(&self, why: Deleted) -> u64 {
        let counter = match why {
            Deleted::Expired => &self.deleted_expired,
        };
        counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// One event read off the bus that did not become a post, and why.
    /// Returns the running total, for the log line.
    pub fn record_skipped(&self, why: Skipped) -> u64 {
        self.skipped_counter(why).fetch_add(1, Ordering::Relaxed) + 1
    }

    /// One failed write to the relay. Returns the running total.
    pub fn record_relay_failure(&self) -> u64 {
        self.relay_failures.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// One completed sweep for an expired suggestion's post.
    pub fn record_sweep(&self) {
        self.sweeps.fetch_add(1, Ordering::Relaxed);
    }

    /// Renders the Prometheus text exposition (format version 0.0.4).
    /// `now_unix_seconds` is unused by any sample today, but is taken the
    /// way the Sensor's `render` is — a scrape time the caller controls,
    /// rather than a clock this module reads for itself — so a gauge that
    /// needs one later costs nothing to add.
    pub fn render(&self, _now_unix_seconds: u64) -> String {
        let mut out = String::new();

        out.push_str("# HELP twalk_clerk_posts_total Posts written to the relay, by channel.\n");
        out.push_str("# TYPE twalk_clerk_posts_total counter\n");
        for channel in [Channel::Approvals, Channel::Activity, Channel::Journal] {
            out.push_str(&format!(
                "twalk_clerk_posts_total{{channel=\"{}\"}} {}\n",
                channel.as_str(),
                self.posts_counter(channel).load(Ordering::Relaxed)
            ));
        }

        out.push_str("# HELP twalk_clerk_deleted_total Posts deleted, by why.\n");
        out.push_str("# TYPE twalk_clerk_deleted_total counter\n");
        out.push_str(&format!(
            "twalk_clerk_deleted_total{{why=\"{}\"}} {}\n",
            Deleted::Expired.as_str(),
            self.deleted_expired.load(Ordering::Relaxed)
        ));

        out.push_str(
            "# HELP twalk_clerk_skipped_total Events read off the bus that did not become a post, by why.\n",
        );
        out.push_str("# TYPE twalk_clerk_skipped_total counter\n");
        for why in [Skipped::Expired, Skipped::Unreadable, Skipped::Duplicate] {
            out.push_str(&format!(
                "twalk_clerk_skipped_total{{why=\"{}\"}} {}\n",
                why.as_str(),
                self.skipped_counter(why).load(Ordering::Relaxed)
            ));
        }

        out.push_str("# HELP twalk_clerk_relay_failures_total Failed writes to the relay.\n");
        out.push_str("# TYPE twalk_clerk_relay_failures_total counter\n");
        out.push_str(&format!(
            "twalk_clerk_relay_failures_total {}\n",
            self.relay_failures.load(Ordering::Relaxed)
        ));

        out.push_str(
            "# HELP twalk_clerk_sweeps_total Completed sweeps for an expired suggestion's post.\n",
        );
        out.push_str("# TYPE twalk_clerk_sweeps_total counter\n");
        out.push_str(&format!(
            "twalk_clerk_sweeps_total {}\n",
            self.sweeps.load(Ordering::Relaxed)
        ));

        out.push_str("# HELP twalk_clerk_up Whether the clerk process is up. Always 1 while it answers /metrics.\n");
        out.push_str("# TYPE twalk_clerk_up gauge\n");
        out.push_str("twalk_clerk_up 1\n");

        out.push_str(
            "# HELP twalk_clerk_started_at_seconds When the process started, in seconds since the epoch.\n",
        );
        out.push_str("# TYPE twalk_clerk_started_at_seconds gauge\n");
        out.push_str(&format!(
            "twalk_clerk_started_at_seconds {}\n",
            self.started_at_seconds
        ));

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_metric_exists_at_zero() {
        let metrics = Metrics::new();
        let body = metrics.render(1_000);

        for channel in ["approbations", "activite", "journal"] {
            assert!(
                body.contains(&format!(
                    "twalk_clerk_posts_total{{channel=\"{channel}\"}} 0\n"
                )),
                "channel {channel} should exist at zero: {body}"
            );
        }
        assert!(
            body.contains("twalk_clerk_deleted_total{why=\"expired\"} 0\n"),
            "{body}"
        );
        for why in ["expired", "unreadable", "duplicate"] {
            assert!(
                body.contains(&format!("twalk_clerk_skipped_total{{why=\"{why}\"}} 0\n")),
                "why {why} should exist at zero: {body}"
            );
        }
        assert!(
            body.contains("twalk_clerk_relay_failures_total 0\n"),
            "{body}"
        );
        assert!(body.contains("twalk_clerk_sweeps_total 0\n"), "{body}");
        assert!(body.contains("twalk_clerk_up 1\n"), "{body}");
        assert!(body.contains("twalk_clerk_started_at_seconds "), "{body}");
    }

    #[test]
    fn the_exposition_is_prometheus_shaped() {
        let metrics = Metrics::new();
        assert_eq!(metrics.record_post(Channel::Approvals), 1);
        assert_eq!(metrics.record_post(Channel::Approvals), 2);
        assert_eq!(metrics.record_post(Channel::Journal), 1);
        assert_eq!(metrics.record_deleted(Deleted::Expired), 1);
        assert_eq!(metrics.record_skipped(Skipped::Unreadable), 1);
        assert_eq!(metrics.record_relay_failure(), 1);
        metrics.record_sweep();
        metrics.record_sweep();

        let body = metrics.render(1_000);

        assert!(
            body.contains("twalk_clerk_posts_total{channel=\"approbations\"} 2\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_clerk_posts_total{channel=\"journal\"} 1\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_clerk_posts_total{channel=\"activite\"} 0\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_clerk_deleted_total{why=\"expired\"} 1\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_clerk_skipped_total{why=\"unreadable\"} 1\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_clerk_relay_failures_total 1\n"),
            "{body}"
        );
        assert!(body.contains("twalk_clerk_sweeps_total 2\n"), "{body}");

        // Every sample line is `name[labels] value` with an integer value.
        for line in body.lines().filter(|line| !line.starts_with('#')) {
            let (name, value) = line.rsplit_once(' ').expect("a sample line has a value");
            assert!(name.starts_with("twalk_clerk_"), "{line}");
            value.parse::<u64>().expect("the value is an integer");
        }
    }
}
