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
//! operator needs when nothing seems to be happening at all. The one
//! exception is deliberate: `twalk_clerk_approvals_total` has six fixed
//! outcomes at zero from the start and one row **per Companion Gateway
//! refusal code as it occurs** ([`ApprovalOutcome::Refused`]), because the
//! codes are the Gateway's vocabulary (`refusals.rs` holds the table) and a
//! list frozen here would be one more copy of it to keep in step.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
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
    /// The sweep could not date it — no `expires_at` it could read in the
    /// reference line — and it had stood for `reference::UNDATABLE_CEILING`
    /// on the relay's own `created_at` (ADR 0028's seven days).
    Undatable,
}

impl Deleted {
    pub const ALL: [Deleted; 2] = [Deleted::Expired, Deleted::Undatable];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Expired => "expired",
            Self::Undatable => "undatable",
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
    /// A post already exists for this suggestion, or a line for this bus
    /// event (the relay is the clerk's own memory — see `lib.rs` — and a
    /// redelivery must not double-post on any channel).
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

/// What one owner's (or stranger's) gesture on an `approbations` post came
/// to, once the decisions loop had carried it (#284): the label of
/// `twalk_clerk_approvals_total{outcome}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// The Companion Gateway accepted the approval: the reply went out.
    Approved,
    /// The Companion Gateway had already recorded it (`409 already_approved`): a
    /// redelivered gesture, or a clerk restarted between the approval and
    /// the post's deletion. Success, and counted apart so a rising number
    /// here says the loop is repeating itself.
    AlreadyApproved,
    /// A ❌ by the owner: the post deleted, nobody else told.
    RefusedLocally,
    /// A gesture by a key that is not the owner's, answered in the thread.
    NotTheOwner,
    /// The Companion Gateway gave no usable answer this tick, and the
    /// gesture is carried again next tick. One per attempt, so an outage
    /// reads as a slope. **One label for two kinds of cause**, by
    /// decision: the transient ones (nothing answered, a `429` or `5xx`)
    /// and the ones that will be the same next time (an answer that is not
    /// the route's shape, a session file that could not be read or
    /// rewritten) — the six outcomes are fixed, and a seventh would be one
    /// more thing for an operator's dashboard to know. The log tells them
    /// apart: `warn` for the first kind, `error` for the second, each
    /// naming the cause and the URL.
    GatewayUnreachable,
    /// The Companion Gateway will not have the clerk's session: the `Buzz` device was
    /// revoked, or its refresh token died. Answered in the thread once.
    Unauthenticated,
    /// The Companion Gateway refused with this code (`refusals.rs` has the sentence).
    /// Rendered as `outcome="<code>"`, one row per code seen.
    Refused(String),
}

impl ApprovalOutcome {
    /// The six fixed outcomes, rendered at zero from the start.
    pub const FIXED: [ApprovalOutcome; 6] = [
        ApprovalOutcome::Approved,
        ApprovalOutcome::AlreadyApproved,
        ApprovalOutcome::RefusedLocally,
        ApprovalOutcome::NotTheOwner,
        ApprovalOutcome::GatewayUnreachable,
        ApprovalOutcome::Unauthenticated,
    ];

    /// The label value: the fixed outcomes by name, a refusal by its code.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Approved => "approved",
            Self::AlreadyApproved => "already_approved",
            Self::RefusedLocally => "refused_locally",
            Self::NotTheOwner => "not_the_owner",
            Self::GatewayUnreachable => "gateway_unreachable",
            Self::Unauthenticated => "unauthenticated",
            Self::Refused(code) => code,
        }
    }
}

/// Process-wide health signals. Cheap to clone into every task: all state is
/// shared behind atomics, except the per-code approval counters, which are
/// a map behind a mutex because the set of codes is the Companion Gateway's and grows
/// as they occur.
pub struct Metrics {
    posts_approbations: AtomicU64,
    posts_activite: AtomicU64,
    posts_journal: AtomicU64,
    deleted_expired: AtomicU64,
    deleted_undatable: AtomicU64,
    skipped_expired: AtomicU64,
    skipped_unreadable: AtomicU64,
    skipped_duplicate: AtomicU64,
    /// Failed writes to the relay — a post, a delete, a query that did not
    /// come back with a `2xx`.
    relay_failures: AtomicU64,
    /// Completed sweeps for an expired suggestion's post.
    sweeps: AtomicU64,
    /// Gestures carried, by outcome: the fixed ones seeded at zero, a
    /// Companion Gateway refusal code added the first time it occurs.
    approvals: Mutex<BTreeMap<String, u64>>,
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
            deleted_undatable: AtomicU64::new(0),
            skipped_expired: AtomicU64::new(0),
            skipped_unreadable: AtomicU64::new(0),
            skipped_duplicate: AtomicU64::new(0),
            relay_failures: AtomicU64::new(0),
            sweeps: AtomicU64::new(0),
            approvals: Mutex::new(
                ApprovalOutcome::FIXED
                    .iter()
                    .map(|outcome| (outcome.as_str().to_owned(), 0))
                    .collect(),
            ),
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

    fn deleted_counter(&self, why: Deleted) -> &AtomicU64 {
        match why {
            Deleted::Expired => &self.deleted_expired,
            Deleted::Undatable => &self.deleted_undatable,
        }
    }

    /// One post deleted, and why. Returns the running total.
    pub fn record_deleted(&self, why: Deleted) -> u64 {
        self.deleted_counter(why).fetch_add(1, Ordering::Relaxed) + 1
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

    /// One gesture carried to its outcome. Returns the running total for
    /// that outcome, for the log line. A poisoned mutex is recovered rather
    /// than propagated: a counter is not worth a panic in the loop.
    pub fn record_approval(&self, outcome: &ApprovalOutcome) -> u64 {
        let mut approvals = self
            .approvals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let total = approvals.entry(outcome.as_str().to_owned()).or_insert(0);
        *total += 1;
        *total
    }

    /// Renders the Prometheus text exposition (format version 0.0.4).
    /// `now_unix_seconds` is taken the way the Sensor's `render` takes it —
    /// a scrape time the caller controls, rather than a clock this module
    /// reads for itself — so a gauge that needs one later costs nothing to
    /// add; no sample uses it today, and that is said here rather than
    /// hidden behind an underscore.
    pub fn render(&self, now_unix_seconds: u64) -> String {
        let _ = now_unix_seconds;
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
        for why in Deleted::ALL {
            out.push_str(&format!(
                "twalk_clerk_deleted_total{{why=\"{}\"}} {}\n",
                why.as_str(),
                self.deleted_counter(why).load(Ordering::Relaxed)
            ));
        }

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

        out.push_str(
            "# HELP twalk_clerk_approvals_total Gestures on approbations posts carried to their outcome (#284): the fixed outcomes, and one row per Companion Gateway refusal code as it occurs.\n",
        );
        out.push_str("# TYPE twalk_clerk_approvals_total counter\n");
        let approvals = self
            .approvals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (outcome, total) in approvals.iter() {
            out.push_str(&format!(
                "twalk_clerk_approvals_total{{outcome=\"{outcome}\"}} {total}\n"
            ));
        }
        drop(approvals);

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
        for why in ["expired", "undatable"] {
            assert!(
                body.contains(&format!("twalk_clerk_deleted_total{{why=\"{why}\"}} 0\n")),
                "why {why} should exist at zero: {body}"
            );
        }
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
    fn approvals_total_renders_fixed_outcomes_at_zero_and_codes_as_they_occur() {
        let metrics = Metrics::new();
        let body = metrics.render(1_000);
        for outcome in [
            "approved",
            "already_approved",
            "refused_locally",
            "not_the_owner",
            "gateway_unreachable",
            "unauthenticated",
        ] {
            assert!(
                body.contains(&format!(
                    "twalk_clerk_approvals_total{{outcome=\"{outcome}\"}} 0\n"
                )),
                "outcome {outcome} should exist at zero: {body}"
            );
        }
        // No code has occurred, so no code row exists: a code the Gateway
        // never answered with is not a fact to render.
        assert!(!body.contains("consent_revoked"), "{body}");

        assert_eq!(metrics.record_approval(&ApprovalOutcome::Approved), 1);
        assert_eq!(metrics.record_approval(&ApprovalOutcome::Approved), 2);
        assert_eq!(
            metrics.record_approval(&ApprovalOutcome::Refused("consent_revoked".to_owned())),
            1
        );
        assert_eq!(
            metrics.record_approval(&ApprovalOutcome::Refused("consent_revoked".to_owned())),
            2
        );
        assert_eq!(
            metrics.record_approval(&ApprovalOutcome::Refused("suggestion_expired".to_owned())),
            1
        );
        assert_eq!(metrics.record_approval(&ApprovalOutcome::NotTheOwner), 1);

        let body = metrics.render(1_000);
        assert!(
            body.contains("twalk_clerk_approvals_total{outcome=\"approved\"} 2\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_clerk_approvals_total{outcome=\"consent_revoked\"} 2\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_clerk_approvals_total{outcome=\"suggestion_expired\"} 1\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_clerk_approvals_total{outcome=\"not_the_owner\"} 1\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_clerk_approvals_total{outcome=\"unauthenticated\"} 0\n"),
            "{body}"
        );
        // One HELP and one TYPE line for the family, however many rows.
        assert_eq!(
            body.matches("# TYPE twalk_clerk_approvals_total counter\n")
                .count(),
            1
        );
    }

    #[test]
    fn the_exposition_is_prometheus_shaped() {
        let metrics = Metrics::new();
        assert_eq!(metrics.record_post(Channel::Approvals), 1);
        assert_eq!(metrics.record_post(Channel::Approvals), 2);
        assert_eq!(metrics.record_post(Channel::Journal), 1);
        assert_eq!(metrics.record_deleted(Deleted::Expired), 1);
        assert_eq!(metrics.record_deleted(Deleted::Undatable), 1);
        assert_eq!(metrics.record_deleted(Deleted::Undatable), 2);
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
            body.contains("twalk_clerk_deleted_total{why=\"undatable\"} 2\n"),
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
