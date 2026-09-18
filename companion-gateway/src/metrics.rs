//! Metrics: a handful of process counters and gauges rendered in the
//! Prometheus text exposition format, served by the Gateway's own origin at
//! `/metrics`. The conventions are the Sensor's (`sensor/src/metrics.rs`):
//! one `twalk_<component>_` namespace, HELP and TYPE for every metric,
//! integer samples, and no metrics crate — the skeleton needs one counter and
//! one gauge, and a hand-rolled exposition is smaller than any dependency.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// Process-wide health signals. Cheap to clone into every request: all state
/// is shared behind a mutex.
pub struct Metrics {
    /// HTTP requests answered, by route and response status. The route is one
    /// of a closed set (see [`Route`]), so the label cardinality is bounded
    /// however many paths the Companion has.
    http_requests: Mutex<BTreeMap<(Route, u16), u64>>,
    /// Sign-in attempts by outcome (`accepted`, `not_the_owner`, `replayed`,
    /// …). The outcomes are a closed set of static labels — the refusal
    /// labels of `session::SignInRefusal` and `matrix_openid::Refusal` — so
    /// the cardinality is bounded, and a burst of refusals is the one signal
    /// an operator would want to see here: nobody but the owner should ever
    /// be trying.
    sign_ins: Mutex<BTreeMap<&'static str, u64>>,
    /// Registration relay attempts by outcome (`created`, `not_the_owner`,
    /// `already_exists`, `recovery_key_refused`, …). A closed set of static
    /// labels, as above. This is the counter an operator watches: the relay
    /// should succeed exactly once in a deployment's life, and every other
    /// sample is somebody trying something.
    registrations: Mutex<BTreeMap<&'static str, u64>>,
    /// Sensor invitations by outcome — one sample per room asked about
    /// (`invited`, `already_present`, `failed`), or one per refused request
    /// (`token_rejected`, `not_configured`, …).
    sensor_invitations: Mutex<BTreeMap<&'static str, u64>>,
    /// Consent decisions recorded in the journal, by subject type (#49). A
    /// decision the journal already held is not counted again.
    consent_decisions: Mutex<BTreeMap<&'static str, u64>>,
    /// Consent decisions the outbox published on the bus, and how many are
    /// still waiting for it. Together they are the operator's answer to "is
    /// the outbox draining?": the counter climbs, the gauge returns to zero.
    /// The gauge is `None` until consent is configured, so a Gateway that
    /// writes no consent exposes no consent series at all.
    consent_published: Mutex<u64>,
    consent_outbox_pending: Mutex<Option<u64>>,
    /// Bridge states observed (#56), by the channel they arrived on
    /// (`webhook`, `startup`) and what they said: one of the contract's five
    /// states, `unchanged` when the bridge reported the state it was already
    /// in, or `unreachable` when startup reconciliation could not ask. Both
    /// label sets are closed, so the cardinality is bounded by the number of
    /// states and not by anything a bridge sends.
    bridge_statuses: Mutex<BTreeMap<(&'static str, &'static str), u64>>,
    /// Status webhook calls that were refused, by reason
    /// (`unauthenticated`, `unknown_bridge`, …). The one an operator watches:
    /// a bridge whose `as_token` does not match the Gateway's shows up here
    /// and nowhere else.
    bridge_status_refusals: Mutex<BTreeMap<&'static str, u64>>,
    /// Bridge transitions the outbox published, and how many are still
    /// waiting — the same pair as consent's, for the same question.
    bridge_status_published: Mutex<u64>,
    bridge_status_outbox_pending: Mutex<Option<u64>>,
    /// Inbound events the pending-contact projection turned into a sighting
    /// (#54), and how many contacts are currently waiting for a decision.
    /// The gauge is the number screen 5 shows, which is exactly why an
    /// operator wants it: a dashboard that says "3 waiting" and a Gateway
    /// that says nothing is waiting is a bug worth seeing. `None` until the
    /// projection is configured, so a Gateway that consumes nothing exposes
    /// no contact series at all.
    contacts_observed: Mutex<u64>,
    pending_contacts: Mutex<Option<u64>>,
    /// Approved replies published on the bus (#24), and the approvals that
    /// were refused, by the code the caller was given
    /// (`suggestion_expired`, `consent_revoked`, `bus_unreachable`, …).
    ///
    /// The refusal counter is the one an operator reads after "my assistant
    /// stopped answering": it says whether the approvals are being refused
    /// and for which of a dozen different reasons, rather than leaving a
    /// silence to be interpreted. The labels are the refusal codes, a closed
    /// set of static strings, so the cardinality is bounded.
    approvals_published: Mutex<u64>,
    approval_refusals: Mutex<BTreeMap<&'static str, u64>>,
    /// When the process started, in seconds since the epoch: the uptime
    /// gauge is computed against the scrape clock, as the Sensor's sync age
    /// is.
    started_unix_seconds: u64,
}

/// The routes the Gateway answers on. A closed set on purpose: the Companion
/// is a client-side-routed app, so labelling metrics with the request path
/// would grow a new time series per deep link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Route {
    /// The health endpoint.
    Health,
    /// The metrics endpoint itself.
    Metrics,
    /// The Gateway's own HTTP description (`/openapi.yaml`, ticket #63).
    /// Its own label rather than `companion`: the description is the
    /// Gateway's surface, not a file of the Companion's build, and a
    /// generator polling it is worth telling apart from a browser.
    OpenApi,
    /// The Gateway's own API surface (`/api/...`), empty in this skeleton.
    Api,
    /// The bridge status webhook (`/_twalk/bridges/{id}/status`, ticket
    /// #56). Its own label: its caller is a bridge and not a browser, and a
    /// bridge pushing every few seconds would otherwise be indistinguishable
    /// from the Companion's own traffic.
    BridgeStatus,
    /// The Companion's static files, the app shell included.
    Companion,
}

impl Route {
    pub fn label(self) -> &'static str {
        match self {
            Route::Health => "health",
            Route::Metrics => "metrics",
            Route::OpenApi => "openapi",
            Route::Api => "api",
            Route::BridgeStatus => "bridge_status",
            Route::Companion => "companion",
        }
    }
}

impl Metrics {
    /// `now_unix_seconds` is the process start time, passed in so the uptime
    /// gauge is computed against a clock the caller controls.
    pub fn started_at(now_unix_seconds: u64) -> Self {
        Self {
            http_requests: Mutex::new(BTreeMap::new()),
            sign_ins: Mutex::new(BTreeMap::new()),
            registrations: Mutex::new(BTreeMap::new()),
            sensor_invitations: Mutex::new(BTreeMap::new()),
            consent_decisions: Mutex::new(BTreeMap::new()),
            consent_published: Mutex::new(0),
            consent_outbox_pending: Mutex::new(None),
            bridge_statuses: Mutex::new(BTreeMap::new()),
            bridge_status_refusals: Mutex::new(BTreeMap::new()),
            bridge_status_published: Mutex::new(0),
            bridge_status_outbox_pending: Mutex::new(None),
            approvals_published: Mutex::new(0),
            approval_refusals: Mutex::new(BTreeMap::new()),
            contacts_observed: Mutex::new(0),
            pending_contacts: Mutex::new(None),
            started_unix_seconds: now_unix_seconds,
        }
    }

    pub fn record_request(&self, route: Route, status: u16) {
        *self
            .http_requests
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry((route, status))
            .or_insert(0) += 1;
    }

    /// Counts one sign-in attempt. `outcome` is `accepted` or a refusal's
    /// own label — never anything derived from a request, so no caller can
    /// grow the label set.
    pub fn record_sign_in(&self, outcome: &'static str) {
        *self
            .sign_ins
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(outcome)
            .or_insert(0) += 1;
    }

    /// Counts one registration attempt. `outcome` is `created` or a
    /// refusal's own label (ticket #53) — a static string, never anything
    /// derived from a request.
    pub fn record_registration(&self, outcome: &'static str) {
        *self
            .registrations
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(outcome)
            .or_insert(0) += 1;
    }

    /// Counts one room's invitation outcome, or one refused invitation
    /// request. Static labels only, as above.
    pub fn record_sensor_invitation(&self, outcome: &'static str) {
        *self
            .sensor_invitations
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(outcome)
            .or_insert(0) += 1;
    }

    /// One consent decision appended to the journal.
    pub fn record_consent_decision(&self, subject_type: crate::consent::SubjectType) {
        *self
            .consent_decisions
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(subject_type.as_str())
            .or_insert(0) += 1;
    }

    /// One committed decision published on the bus and marked as such. This
    /// counter is how a test — and an operator after a crash — sees that a
    /// decision reached the bus exactly once.
    /// One approved reply reached the bus (#24).
    pub fn record_approval_published(&self) {
        *self
            .approvals_published
            .lock()
            .expect("the metrics mutex is never poisoned") += 1;
    }

    /// One approval was refused, under the code the caller was given — so a
    /// refusal an operator sees here is the same word the user saw.
    pub fn record_approval_refusal(&self, outcome: &'static str) {
        *self
            .approval_refusals
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(outcome)
            .or_insert(0) += 1;
    }

    pub fn record_consent_published(&self) {
        *self
            .consent_published
            .lock()
            .expect("the metrics mutex is never poisoned") += 1;
    }

    /// How many committed decisions are waiting for the bus, as the store
    /// counts them.
    pub fn set_consent_outbox_pending(&self, pending: u64) {
        *self
            .consent_outbox_pending
            .lock()
            .expect("the metrics mutex is never poisoned") = Some(pending);
    }

    /// One bridge state observed (#56). Both labels are static: `channel` is
    /// `webhook` or `startup`, and `state` is one of the contract's five,
    /// `unchanged` or `unreachable` — never anything a bridge sent.
    pub fn record_bridge_status(&self, channel: &'static str, state: &'static str) {
        *self
            .bridge_statuses
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry((channel, state))
            .or_insert(0) += 1;
    }

    /// One status webhook refused, by the refusal's own label.
    pub fn record_bridge_status_refusal(&self, outcome: &'static str) {
        *self
            .bridge_status_refusals
            .lock()
            .expect("the metrics mutex is never poisoned")
            .entry(outcome)
            .or_insert(0) += 1;
    }

    /// One bridge transition published on the bus and marked as such.
    pub fn record_bridge_status_published(&self) {
        *self
            .bridge_status_published
            .lock()
            .expect("the metrics mutex is never poisoned") += 1;
    }

    /// How many bridge transitions are waiting for the bus.
    pub fn set_bridge_status_outbox_pending(&self, pending: u64) {
        *self
            .bridge_status_outbox_pending
            .lock()
            .expect("the metrics mutex is never poisoned") = Some(pending);
    }

    /// Inbound events projected into a contact sighting (#54), a batch at a
    /// time. Counts events read, not contacts: the same contact writing
    /// twice moves this twice and the gauge below not at all.
    pub fn record_contacts_observed(&self, observed: u64) {
        *self
            .contacts_observed
            .lock()
            .expect("the metrics mutex is never poisoned") += observed;
    }

    /// How many contacts are waiting for a decision, as the store counts
    /// them — the number the Companion's dashboard shows.
    pub fn set_pending_contacts(&self, pending: u64) {
        *self
            .pending_contacts
            .lock()
            .expect("the metrics mutex is never poisoned") = Some(pending);
    }

    /// Renders the Prometheus text exposition (format version 0.0.4). `now`
    /// is the scrape time in seconds since the epoch.
    pub fn render(&self, now_unix_seconds: u64) -> String {
        let mut out = String::new();
        out.push_str("# HELP twalk_companion_gateway_http_requests_total HTTP requests answered, by route and response status.\n");
        out.push_str("# TYPE twalk_companion_gateway_http_requests_total counter\n");
        for ((route, status), count) in self
            .http_requests
            .lock()
            .expect("the metrics mutex is never poisoned")
            .iter()
        {
            out.push_str(&format!(
                "twalk_companion_gateway_http_requests_total{{route=\"{}\",status=\"{status}\"}} {count}\n",
                route.label()
            ));
        }
        out.push_str(
            "# HELP twalk_companion_gateway_sign_ins_total Sign-in attempts, by outcome.\n",
        );
        out.push_str("# TYPE twalk_companion_gateway_sign_ins_total counter\n");
        for (outcome, count) in self
            .sign_ins
            .lock()
            .expect("the metrics mutex is never poisoned")
            .iter()
        {
            out.push_str(&format!(
                "twalk_companion_gateway_sign_ins_total{{outcome=\"{outcome}\"}} {count}\n"
            ));
        }
        // The consent series appear once consent is configured: a Gateway
        // that writes no consent exposes none of them, and a scrape of one
        // that does always carries the outbox gauge, even at zero.
        let pending = *self
            .consent_outbox_pending
            .lock()
            .expect("the metrics mutex is never poisoned");
        if let Some(pending) = pending {
            out.push_str("# HELP twalk_companion_gateway_consent_decisions_total Consent decisions recorded in the journal, by subject type.\n");
            out.push_str("# TYPE twalk_companion_gateway_consent_decisions_total counter\n");
            for (subject_type, count) in self
                .consent_decisions
                .lock()
                .expect("the metrics mutex is never poisoned")
                .iter()
            {
                out.push_str(&format!(
                    "twalk_companion_gateway_consent_decisions_total{{subject_type=\"{subject_type}\"}} {count}\n"
                ));
            }
            out.push_str("# HELP twalk_companion_gateway_consent_events_published_total Consent decisions published on the bus by the transactional outbox.\n");
            out.push_str("# TYPE twalk_companion_gateway_consent_events_published_total counter\n");
            out.push_str(&format!(
                "twalk_companion_gateway_consent_events_published_total {}\n",
                self.consent_published
                    .lock()
                    .expect("the metrics mutex is never poisoned")
            ));
            out.push_str("# HELP twalk_companion_gateway_consent_outbox_pending Committed consent decisions still waiting to be published.\n");
            out.push_str("# TYPE twalk_companion_gateway_consent_outbox_pending gauge\n");
            out.push_str(&format!(
                "twalk_companion_gateway_consent_outbox_pending {pending}\n"
            ));
            // The approval series (#24), inside the same guard: approvals
            // are configured exactly when consent is — they read the same
            // store and publish on the same bus — so a Gateway that writes
            // no consent exposes no approval series either.
            //
            // The refusal counter is the one an operator reads after "my
            // assistant stopped answering". Its label is the code the caller
            // was given, so an expired suggestion, a revoked contact and a
            // bus that did not answer are three different samples and not
            // one silence.
            out.push_str("# HELP twalk_companion_gateway_approvals_published_total Approved replies published on the bus.\n");
            out.push_str("# TYPE twalk_companion_gateway_approvals_published_total counter\n");
            out.push_str(&format!(
                "twalk_companion_gateway_approvals_published_total {}\n",
                self.approvals_published
                    .lock()
                    .expect("the metrics mutex is never poisoned")
            ));
            out.push_str("# HELP twalk_companion_gateway_approval_refusals_total Approvals refused, by the code the caller was given.\n");
            out.push_str("# TYPE twalk_companion_gateway_approval_refusals_total counter\n");
            for (outcome, count) in self
                .approval_refusals
                .lock()
                .expect("the metrics mutex is never poisoned")
                .iter()
            {
                out.push_str(&format!(
                    "twalk_companion_gateway_approval_refusals_total{{outcome=\"{outcome}\"}} {count}\n"
                ));
            }
        }
        // The pending-contact series (#54), on the same terms: present once
        // the projection is configured, absent otherwise.
        let waiting = *self
            .pending_contacts
            .lock()
            .expect("the metrics mutex is never poisoned");
        if let Some(waiting) = waiting {
            out.push_str("# HELP twalk_companion_gateway_contacts_observed_total Inbound events projected into a contact sighting.\n");
            out.push_str("# TYPE twalk_companion_gateway_contacts_observed_total counter\n");
            out.push_str(&format!(
                "twalk_companion_gateway_contacts_observed_total {}\n",
                self.contacts_observed
                    .lock()
                    .expect("the metrics mutex is never poisoned")
            ));
            out.push_str("# HELP twalk_companion_gateway_pending_contacts Contacts that have written and that no consent decision covers.\n");
            out.push_str("# TYPE twalk_companion_gateway_pending_contacts gauge\n");
            out.push_str(&format!(
                "twalk_companion_gateway_pending_contacts {waiting}\n"
            ));
        }
        out.push_str(
            "# HELP twalk_companion_gateway_registrations_total Account registration relay attempts, by outcome.\n",
        );
        out.push_str("# TYPE twalk_companion_gateway_registrations_total counter\n");
        for (outcome, count) in self
            .registrations
            .lock()
            .expect("the metrics mutex is never poisoned")
            .iter()
        {
            out.push_str(&format!(
                "twalk_companion_gateway_registrations_total{{outcome=\"{outcome}\"}} {count}\n"
            ));
        }
        out.push_str(
            "# HELP twalk_companion_gateway_sensor_invitations_total Sensor room invitations, by outcome.\n",
        );
        out.push_str("# TYPE twalk_companion_gateway_sensor_invitations_total counter\n");
        for (outcome, count) in self
            .sensor_invitations
            .lock()
            .expect("the metrics mutex is never poisoned")
            .iter()
        {
            out.push_str(&format!(
                "twalk_companion_gateway_sensor_invitations_total{{outcome=\"{outcome}\"}} {count}\n"
            ));
        }
        // The bridge status series (#56), on the same rule as consent's: the
        // gauge is `None` until this Gateway has a store to record a
        // transition in, so a deployment with no bus exposes none of them.
        let bridge_pending = *self
            .bridge_status_outbox_pending
            .lock()
            .expect("the metrics mutex is never poisoned");
        if let Some(bridge_pending) = bridge_pending {
            out.push_str("# HELP twalk_companion_gateway_bridge_statuses_total Bridge states observed, by the channel they arrived on and the state they reported.\n");
            out.push_str("# TYPE twalk_companion_gateway_bridge_statuses_total counter\n");
            for ((channel, state), count) in self
                .bridge_statuses
                .lock()
                .expect("the metrics mutex is never poisoned")
                .iter()
            {
                out.push_str(&format!(
                    "twalk_companion_gateway_bridge_statuses_total{{channel=\"{channel}\",state=\"{state}\"}} {count}\n"
                ));
            }
            out.push_str("# HELP twalk_companion_gateway_bridge_status_refusals_total Bridge status webhook calls refused, by reason.\n");
            out.push_str("# TYPE twalk_companion_gateway_bridge_status_refusals_total counter\n");
            for (outcome, count) in self
                .bridge_status_refusals
                .lock()
                .expect("the metrics mutex is never poisoned")
                .iter()
            {
                out.push_str(&format!(
                    "twalk_companion_gateway_bridge_status_refusals_total{{outcome=\"{outcome}\"}} {count}\n"
                ));
            }
            out.push_str("# HELP twalk_companion_gateway_bridge_status_events_published_total Bridge state changes published on the bus by the transactional outbox.\n");
            out.push_str(
                "# TYPE twalk_companion_gateway_bridge_status_events_published_total counter\n",
            );
            out.push_str(&format!(
                "twalk_companion_gateway_bridge_status_events_published_total {}\n",
                self.bridge_status_published
                    .lock()
                    .expect("the metrics mutex is never poisoned")
            ));
            out.push_str("# HELP twalk_companion_gateway_bridge_status_outbox_pending Recorded bridge state changes still waiting to be published.\n");
            out.push_str("# TYPE twalk_companion_gateway_bridge_status_outbox_pending gauge\n");
            out.push_str(&format!(
                "twalk_companion_gateway_bridge_status_outbox_pending {bridge_pending}\n"
            ));
        }
        out.push_str(
            "# HELP twalk_companion_gateway_uptime_seconds Seconds since the process started.\n",
        );
        out.push_str("# TYPE twalk_companion_gateway_uptime_seconds gauge\n");
        out.push_str(&format!(
            "twalk_companion_gateway_uptime_seconds {}\n",
            now_unix_seconds.saturating_sub(self.started_unix_seconds)
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exposition_is_prometheus_shaped() {
        let metrics = Metrics::started_at(1_000);
        metrics.record_request(Route::Health, 200);
        metrics.record_request(Route::Health, 200);
        metrics.record_request(Route::Companion, 200);
        metrics.record_request(Route::Companion, 404);
        metrics.record_request(Route::Api, 404);
        metrics.record_request(Route::Metrics, 200);

        metrics.record_sign_in("accepted");
        metrics.record_sign_in("not_the_owner");
        metrics.record_sign_in("not_the_owner");

        metrics.record_registration("created");
        metrics.record_registration("already_exists");
        metrics.record_sensor_invitation("invited");
        metrics.record_sensor_invitation("invited");
        metrics.record_sensor_invitation("already_present");

        let body = metrics.render(1_030);
        assert!(
            body.contains("twalk_companion_gateway_registrations_total{outcome=\"created\"} 1\n"),
            "{body}"
        );
        assert!(
            body.contains(
                "twalk_companion_gateway_registrations_total{outcome=\"already_exists\"} 1\n"
            ),
            "{body}"
        );
        assert!(
            body.contains(
                "twalk_companion_gateway_sensor_invitations_total{outcome=\"invited\"} 2\n"
            ),
            "{body}"
        );
        assert!(
            body.contains("twalk_companion_gateway_sign_ins_total{outcome=\"not_the_owner\"} 2\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_companion_gateway_sign_ins_total{outcome=\"accepted\"} 1\n"),
            "{body}"
        );
        assert!(
            body.contains(
                "twalk_companion_gateway_http_requests_total{route=\"health\",status=\"200\"} 2\n"
            ),
            "{body}"
        );
        assert!(
            body.contains(
                "twalk_companion_gateway_http_requests_total{route=\"companion\",status=\"404\"} 1\n"
            ),
            "{body}"
        );
        assert!(
            body.contains(
                "twalk_companion_gateway_http_requests_total{route=\"api\",status=\"404\"} 1\n"
            ),
            "{body}"
        );
        assert!(
            body.contains("twalk_companion_gateway_uptime_seconds 30\n"),
            "{body}"
        );
        // Every sample line is `name[labels] value` with an integer value,
        // under one namespace.
        for line in body.lines().filter(|line| !line.starts_with('#')) {
            let (name, value) = line.rsplit_once(' ').expect("a sample line has a value");
            assert!(name.starts_with("twalk_companion_gateway_"), "{line}");
            value.parse::<u64>().expect("the value is an integer");
        }
    }

    #[test]
    fn the_consent_series_appear_only_once_consent_is_configured() {
        let metrics = Metrics::started_at(1_000);
        metrics.record_consent_decision(crate::consent::SubjectType::Contact);
        assert!(
            !metrics.render(1_000).contains("consent"),
            "a Gateway that writes no consent exposes no consent series"
        );

        metrics.set_consent_outbox_pending(1);
        metrics.record_consent_decision(crate::consent::SubjectType::Network);
        metrics.record_consent_published();
        let body = metrics.render(1_000);
        assert!(
            body.contains(
                "twalk_companion_gateway_consent_decisions_total{subject_type=\"contact\"} 1\n"
            ),
            "{body}"
        );
        assert!(
            body.contains(
                "twalk_companion_gateway_consent_decisions_total{subject_type=\"network\"} 1\n"
            ),
            "{body}"
        );
        assert!(
            body.contains("twalk_companion_gateway_consent_events_published_total 1\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_companion_gateway_consent_outbox_pending 1\n"),
            "{body}"
        );
        metrics.set_consent_outbox_pending(0);
        assert!(
            metrics
                .render(1_000)
                .contains("twalk_companion_gateway_consent_outbox_pending 0\n"),
            "a drained outbox still reports its gauge, at zero"
        );
    }

    #[test]
    fn the_pending_contact_series_appear_only_once_the_projection_is_configured() {
        let metrics = Metrics::started_at(1_000);
        metrics.record_contacts_observed(1);
        assert!(
            !metrics.render(1_000).contains("contact"),
            "a Gateway that consumes no inbound stream exposes no contact series"
        );

        metrics.set_pending_contacts(3);
        metrics.record_contacts_observed(1);
        let body = metrics.render(1_000);
        assert!(
            body.contains("twalk_companion_gateway_contacts_observed_total 2\n"),
            "{body}"
        );
        assert!(
            body.contains("twalk_companion_gateway_pending_contacts 3\n"),
            "{body}"
        );
        // The number screen 5 shows, at zero once everything is decided —
        // still reported, because "nothing waiting" is an answer.
        metrics.set_pending_contacts(0);
        assert!(
            metrics
                .render(1_000)
                .contains("twalk_companion_gateway_pending_contacts 0\n"),
            "a fully decided deployment still reports its gauge, at zero"
        );
    }

    #[test]
    fn a_gateway_that_answered_nothing_still_renders_its_uptime() {
        let body = Metrics::started_at(1_000).render(1_005);
        assert!(
            body.contains("twalk_companion_gateway_uptime_seconds 5\n"),
            "{body}"
        );
        assert_eq!(
            body.lines()
                .filter(|line| !line.starts_with('#'))
                .collect::<Vec<_>>(),
            vec!["twalk_companion_gateway_uptime_seconds 5"],
            "no request counter samples before the first request: {body}"
        );
    }
}
