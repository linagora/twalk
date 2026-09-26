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
    /// Renewals of the grant, by outcome: `renewed`, `reconnect_required`, `pending_operator`,
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
    /// Calendar resources read and not published, by reason (#350):
    /// `zone_unknown`, `zone_windows_unmappable`, `unreadable`. The calendar's
    /// half of `mails_dropped` above, and there for the same reason — sixty of
    /// the owner's meetings were refused for two days, in sixty log lines and
    /// no number nobody was watching.
    calendar_refused: Mutex<BTreeMap<&'static str, u64>>,
    /// Free/busy reads served on the internal HTTP endpoint (#281), by
    /// outcome: `served`, or the refusal's code. Every read of the owner's
    /// agenda is counted, since a read nobody can see is the thing #281
    /// exists to prevent.
    freebusy_reads: Mutex<BTreeMap<&'static str, u64>>,
    /// Reads of what one event carries (#355), counted apart from the
    /// free/busy series above: they answer different questions of
    /// different shapes, and one series holding both would make "how often
    /// was my agenda pulled" unanswerable.
    event_fact_reads: Mutex<BTreeMap<&'static str, u64>>,
    /// Whether the push socket to the mail server is open (#277): 1 when a
    /// delivery wakes the poll, 0 when the poll is on its own.
    push_connected: AtomicU64,
    /// How many times the server woke the mail poll.
    push_wakes: AtomicU64,
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
            calendar_refused: Mutex::new(BTreeMap::new()),
            freebusy_reads: Mutex::new(BTreeMap::new()),
            event_fact_reads: Mutex::new(BTreeMap::new()),
            push_connected: AtomicU64::new(0),
            push_wakes: AtomicU64::new(0),
        }
    }

    pub fn record_event_fact_read(&self, outcome: &'static str) {
        *self
            .event_fact_reads
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(outcome)
            .or_insert(0) += 1;
    }

    pub fn record_freebusy_read(&self, outcome: &'static str) {
        *self
            .freebusy_reads
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(outcome)
            .or_insert(0) += 1;
    }

    pub fn set_push_connected(&self, connected: bool) {
        self.push_connected
            .store(u64::from(connected), Ordering::Relaxed);
    }

    pub fn record_push_wake(&self) {
        self.push_wakes.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_mail_dropped(&self, reason: &'static str) {
        *self
            .mails_dropped
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(reason)
            .or_insert(0) += 1;
    }

    pub fn record_calendar_refused(&self, reason: &'static str) {
        *self
            .calendar_refused
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
        for outcome in [
            "renewed",
            "reconnect_required",
            "pending_operator",
            "unreachable",
        ] {
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
        out.push_str("# HELP twalk_collector_events_dropped_total Events the collector did not publish, by reason — the Sensor's `twalk_sensor_events_dropped_total`, on this side: a mail the frontier dropped (`non_human_sender`, `calendar_invitation`, `owner`).\n");
        out.push_str("# TYPE twalk_collector_events_dropped_total counter\n");
        let dropped = self
            .mails_dropped
            .lock()
            .expect("the metrics mutex is never poisoned");
        for reason in crate::jmap::Dropped::ALL.map(crate::jmap::Dropped::as_str) {
            out.push_str(&format!(
                "twalk_collector_events_dropped_total{{reason=\"{reason}\"}} {}\n",
                dropped.get(reason).copied().unwrap_or(0)
            ));
        }
        drop(dropped);
        out.push_str("# HELP twalk_collector_calendar_resources_refused_total Calendar resources read and not published, by reason (#350): a TZID in neither the IANA nor the Windows family (`zone_unknown`), one CLDR names and this build's zone database does not (`zone_windows_unmappable`), or anything else the parser would not take (`unreadable`). A calendar can stop being published almost entirely, and this is the number that says so.\n");
        out.push_str("# TYPE twalk_collector_calendar_resources_refused_total counter\n");
        let refused = self
            .calendar_refused
            .lock()
            .expect("the metrics mutex is never poisoned");
        for reason in crate::calendars::Refused::REASONS {
            out.push_str(&format!(
                "twalk_collector_calendar_resources_refused_total{{reason=\"{reason}\"}} {}\n",
                refused.get(reason).copied().unwrap_or(0)
            ));
        }
        drop(refused);
        out.push_str("# HELP twalk_collector_freebusy_reads_total Free/busy reads asked of the internal endpoint, by outcome: served, or the refusal's code.\n");
        out.push_str("# TYPE twalk_collector_freebusy_reads_total counter\n");
        let reads = self
            .freebusy_reads
            .lock()
            .expect("the metrics mutex is never poisoned");
        for outcome in crate::http::READ_OUTCOMES {
            out.push_str(&format!(
                "twalk_collector_freebusy_reads_total{{outcome=\"{outcome}\"}} {}\n",
                reads.get(outcome).copied().unwrap_or(0)
            ));
        }
        drop(reads);
        out.push_str("# HELP twalk_collector_event_fact_reads_total Reads of what one event carries — a conference link, a description's length, a count of attachments — by outcome: served, or the refusal's code. Never the description itself (#355).\n");
        out.push_str("# TYPE twalk_collector_event_fact_reads_total counter\n");
        let facts = self
            .event_fact_reads
            .lock()
            .expect("the metrics mutex is never poisoned");
        for outcome in crate::http::EVENT_FACT_OUTCOMES {
            out.push_str(&format!(
                "twalk_collector_event_fact_reads_total{{outcome=\"{outcome}\"}} {}\n",
                facts.get(outcome).copied().unwrap_or(0)
            ));
        }
        drop(facts);
        out.push_str("# HELP twalk_collector_push_connected Whether the push socket to the mail server is open: 1 when a delivery wakes the poll, 0 when the poll is on its own.\n");
        out.push_str("# TYPE twalk_collector_push_connected gauge\n");
        out.push_str(&format!(
            "twalk_collector_push_connected {}\n",
            self.push_connected.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP twalk_collector_push_wakes_total Times the mail server's push woke the mail poll.\n");
        out.push_str("# TYPE twalk_collector_push_wakes_total counter\n");
        out.push_str(&format!(
            "twalk_collector_push_wakes_total {}\n",
            self.push_wakes.load(Ordering::Relaxed)
        ));
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
        for outcome in [
            "renewed",
            "reconnect_required",
            "pending_operator",
            "unreachable",
        ] {
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
        assert!(body.contains("twalk_collector_events_dropped_total{reason=\"owner\"} 0\n"));
        // The calendar's half (#350): every reason rendered at zero, so a
        // dashboard has the series before anything goes wrong.
        for reason in crate::calendars::Refused::REASONS {
            assert!(
                body.contains(&format!(
                    "twalk_collector_calendar_resources_refused_total{{reason=\"{reason}\"}} 0\n"
                )),
                "{reason} is missing from {body}"
            );
        }
        assert!(body.contains("twalk_collector_push_connected 0\n"));
        metrics.set_push_connected(true);
        metrics.record_push_wake();
        assert!(metrics
            .render(1_000)
            .contains("twalk_collector_push_connected 1\n"));
        assert!(metrics
            .render(1_000)
            .contains("twalk_collector_push_wakes_total 1\n"));
        metrics.record_calendar_refused("zone_unknown");
        assert!(metrics.render(1_000).contains(
            "twalk_collector_calendar_resources_refused_total{reason=\"zone_unknown\"} 1\n"
        ));
        metrics.record_mail_dropped("non_human_sender");
        assert!(metrics
            .render(1_000)
            .contains("twalk_collector_events_dropped_total{reason=\"non_human_sender\"} 1\n"));
    }
}
