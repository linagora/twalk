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

        let body = metrics.render(1_030);
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
