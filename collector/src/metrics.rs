//! Process-wide health signals, in the Prometheus text format, in the
//! Sensor's shape (`sensor/src/metrics.rs`). Small on purpose: what the
//! collector can say about itself in #274 is whether its grant renews and
//! what state each connection is in; the collectors of lot 2 add theirs.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub struct Metrics {
    /// Events published to the bus (publish acked), by contract event type.
    events_published: Mutex<BTreeMap<String, u64>>,
    /// Renewals of the grant, by outcome: `renewed`, `reconnect_required`,
    /// `unreachable`. A flat zero on a running collector means the access
    /// token has never had to be renewed yet, not that renewal works.
    renewals: Mutex<BTreeMap<&'static str, u64>>,
    /// Each connection's current state, as a gauge per state: exactly one
    /// of the four is 1 for a connection, the rest 0 — the shape a dashboard
    /// alerts on without parsing labels.
    connection_state: Mutex<BTreeMap<(String, &'static str), u64>>,
    /// When the grant was last renewed, in seconds since the epoch; zero
    /// until it was.
    last_renewal_unix_seconds: AtomicU64,
    /// Mails the frontier dropped, by reason (#276): `non_human_sender`,
    /// `calendar_invitation`, `owner`. A silence counted, since a silence
    /// is the one failure this product has shipped without noticing.
    mails_dropped: Mutex<BTreeMap<&'static str, u64>>,
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
            renewals: Mutex::new(BTreeMap::new()),
            connection_state: Mutex::new(BTreeMap::new()),
            last_renewal_unix_seconds: AtomicU64::new(0),
            mails_dropped: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn record_mail_dropped(&self, reason: &'static str) {
        *self
            .mails_dropped
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(reason)
            .or_insert(0) += 1;
    }

    pub fn record_published(&self, event_type: &str) {
        *self
            .events_published
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(event_type.to_owned())
            .or_insert(0) += 1;
    }

    pub fn record_renewal(&self, outcome: &'static str, now_unix_seconds: u64) {
        *self
            .renewals
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(outcome)
            .or_insert(0) += 1;
        if outcome == "renewed" {
            self.last_renewal_unix_seconds
                .store(now_unix_seconds, Ordering::Relaxed);
        }
    }

    /// Sets a connection's state: one gauge to 1, the other three to 0.
    pub fn set_connection_state(&self, connection: &str, state: crate::status::State) {
        let mut states = self
            .connection_state
            .lock()
            .expect("the metrics mutex is never poisoned");
        for candidate in [
            crate::status::State::Connected,
            crate::status::State::Unreachable,
            crate::status::State::ReconnectRequired,
            crate::status::State::PendingOperator,
        ] {
            states.insert(
                (connection.to_owned(), candidate.as_str()),
                u64::from(candidate == state),
            );
        }
    }

    pub fn render(&self, now_unix_seconds: u64) -> String {
        let mut out = String::new();
        out.push_str("# HELP twalk_collector_events_published_total CloudEvents published to the bus, by contract event type.\n");
        out.push_str("# TYPE twalk_collector_events_published_total counter\n");
        for (event_type, count) in self
            .events_published
            .lock()
            .expect("the metrics mutex is never poisoned")
            .iter()
        {
            out.push_str(&format!(
                "twalk_collector_events_published_total{{type=\"{event_type}\"}} {count}\n"
            ));
        }
        out.push_str(
            "# HELP twalk_collector_grant_renewals_total Renewals of the OIDC grant, by outcome.\n",
        );
        out.push_str("# TYPE twalk_collector_grant_renewals_total counter\n");
        let renewals = self
            .renewals
            .lock()
            .expect("the metrics mutex is never poisoned");
        for outcome in ["renewed", "reconnect_required", "unreachable"] {
            out.push_str(&format!(
                "twalk_collector_grant_renewals_total{{outcome=\"{outcome}\"}} {}\n",
                renewals.get(outcome).copied().unwrap_or(0)
            ));
        }
        drop(renewals);
        let last = self.last_renewal_unix_seconds.load(Ordering::Relaxed);
        if last > 0 {
            out.push_str("# HELP twalk_collector_grant_age_seconds Seconds since the grant was last renewed.\n");
            out.push_str("# TYPE twalk_collector_grant_age_seconds gauge\n");
            out.push_str(&format!(
                "twalk_collector_grant_age_seconds {}\n",
                now_unix_seconds.saturating_sub(last)
            ));
        }
        out.push_str("# HELP twalk_collector_mails_dropped_total Mails the frontier did not publish, by reason.\n");
        out.push_str("# TYPE twalk_collector_mails_dropped_total counter\n");
        let dropped = self
            .mails_dropped
            .lock()
            .expect("the metrics mutex is never poisoned");
        for reason in ["non_human_sender", "calendar_invitation", "owner"] {
            out.push_str(&format!(
                "twalk_collector_mails_dropped_total{{reason=\"{reason}\"}} {}\n",
                dropped.get(reason).copied().unwrap_or(0)
            ));
        }
        drop(dropped);
        out.push_str("# HELP twalk_collector_connection_state Each connection's state: 1 on the state it is in, 0 on the three it is not.\n");
        out.push_str("# TYPE twalk_collector_connection_state gauge\n");
        for ((connection, state), value) in self
            .connection_state
            .lock()
            .expect("the metrics mutex is never poisoned")
            .iter()
        {
            out.push_str(&format!(
                "twalk_collector_connection_state{{connection=\"{connection}\",state=\"{state}\"}} {value}\n"
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_renewal_outcome_exists_at_zero_and_a_connection_is_in_exactly_one_state() {
        let metrics = Metrics::new();
        let body = metrics.render(1_000);
        for outcome in ["renewed", "reconnect_required", "unreachable"] {
            assert!(body.contains(&format!(
                "twalk_collector_grant_renewals_total{{outcome=\"{outcome}\"}} 0\n"
            )));
        }
        assert!(
            !body.contains("grant_age_seconds"),
            "no renewal yet, no age"
        );
        metrics.set_connection_state("mail-linagora", crate::status::State::PendingOperator);
        metrics.record_renewal("renewed", 900);
        let body = metrics.render(1_000);
        assert!(body.contains(
            "twalk_collector_connection_state{connection=\"mail-linagora\",state=\"pending_operator\"} 1\n"
        ));
        assert!(body.contains(
            "twalk_collector_connection_state{connection=\"mail-linagora\",state=\"connected\"} 0\n"
        ));
        assert!(body.contains("twalk_collector_grant_age_seconds 100\n"));
        assert!(body.contains("twalk_collector_mails_dropped_total{reason=\"owner\"} 0\n"));
        metrics.record_mail_dropped("non_human_sender");
        assert!(metrics
            .render(1_000)
            .contains("twalk_collector_mails_dropped_total{reason=\"non_human_sender\"} 1\n"));
    }
}
