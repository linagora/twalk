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
