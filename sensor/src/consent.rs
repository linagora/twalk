//! The Sensor's consent I/O (ADR 0033, issue #273): the HTTP read of the
//! Companion Gateway's snapshot, and what the snapshot carries that only the
//! Sensor needs — the registry of connections. Every *decision* about
//! consent is the shared crate's (`twalk_consent_cache`, re-exported here),
//! so the Sensor and the mail collector label a sender with one code and
//! cannot disagree; the durable `consent.state.changed` consumer lives in
//! `main.rs` beside the sync loop it is ordered against (ADR 0010).

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

pub use twalk_consent_cache::{
    Consent, ConsentCache, ConsentChange, ConsentEntry, ConsentSubject, Snapshot, Unusable,
    CONSENT_CHANGED_TYPE,
};

/// Durable name of the JetStream pull consumer feeding the consent cache:
/// the consumer survives Sensor restarts, so no recorded decision is lost
/// before it has been applied.
pub const CONSENT_CONSUMER: &str = "sensor-consent-state-changed";

/// The Companion Gateway route serving the snapshot, appended to the base URL
/// the Sensor is configured with (`companion-gateway/openapi.yaml`).
pub const SNAPSHOT_PATH: &str = "/api/consent/snapshot";

/// How long one snapshot read may take before it counts as a failure. The
/// first read happens before the sync loop starts, so it is deliberately
/// short: a Gateway that does not answer promptly is a Gateway to retry
/// against in the background, not one to wait for.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

/// The Gateway's consent snapshot as the Sensor reads it: the shared crate's
/// [`Snapshot`], and the registry of connections the Gateway serves with it
/// (ADR 0033, #269), which is the Sensor's alone — a collector has one
/// connection by construction and nothing to resolve a room against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentSnapshot {
    pub state: Snapshot,
    /// `None` for a deployment with no Gateway, which has the implicit
    /// registry, one connection per network named after it.
    pub connections: Option<crate::connection::Registry>,
}

impl ConsentSnapshot {
    /// The snapshot of a deployment that has decided nothing: start at the
    /// beginning of the stream.
    pub fn empty() -> Self {
        Self {
            state: Snapshot::empty(),
            connections: None,
        }
    }

    /// Parses the `ConsentSnapshot` document of `companion-gateway/
    /// openapi.yaml`: the crate reads the state, the Sensor reads the
    /// registry off the same document.
    pub fn parse(document: &Value) -> Result<Self> {
        Ok(Self {
            state: Snapshot::parse(document)?,
            connections: Some(crate::connection::Registry::from_snapshot(document)),
        })
    }
}

/// The initial consent snapshot, priming the cache at startup: the durable
/// consumer only delivers decisions recorded after its last ack, so without a
/// snapshot a restarted Sensor relabels every sender `pending` (issue #16).
/// [`GatewaySnapshot`] reads it from the Companion Gateway, the single writer
/// of consent state (ADR 0006); [`NoConsentSnapshot`] is what a deployment
/// without a Gateway — and a test that runs without one — gets instead.
pub trait ConsentSnapshotSource: Send + Sync {
    fn fetch_snapshot(&self) -> impl std::future::Future<Output = Result<ConsentSnapshot>> + Send;
}

/// The empty snapshot: nothing decided, follow the stream from its
/// beginning. For deployments and tests with no Companion Gateway in front
/// of the Sensor.
#[derive(Debug, Default)]
pub struct NoConsentSnapshot;

impl ConsentSnapshotSource for NoConsentSnapshot {
    async fn fetch_snapshot(&self) -> Result<ConsentSnapshot> {
        Ok(ConsentSnapshot::empty())
    }
}

/// The real source: `GET /api/consent/snapshot` on the Companion Gateway.
///
/// Authenticated by a **service token** from the Sensor's own environment,
/// as an `Authorization: Bearer` credential. The Sensor is a service and not
/// one of the owner's browsers: it has no Matrix OpenID token to sign in with
/// and must never be given a device token (ADR 0011). The token is the same
/// secret the Gateway is configured with, and it grants a read of every
/// contact the user ever decided about — so it is never logged, and no error
/// this module raises carries it.
pub struct GatewaySnapshot {
    client: reqwest::Client,
    url: String,
    service_token: String,
}

impl GatewaySnapshot {
    /// `base_url` is the Gateway's origin (e.g. `http://companion-gateway:8080`);
    /// the snapshot route is appended to it.
    pub fn new(base_url: &str, service_token: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(SNAPSHOT_TIMEOUT)
            .build()
            .context("failed to build the Companion Gateway HTTP client")?;
        Ok(Self {
            client,
            url: format!("{}{SNAPSHOT_PATH}", base_url.trim_end_matches('/')),
            service_token: service_token.to_owned(),
        })
    }

    /// Where the snapshot is read from — safe to log, unlike the token.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl ConsentSnapshotSource for GatewaySnapshot {
    async fn fetch_snapshot(&self) -> Result<ConsentSnapshot> {
        let response = self
            .client
            .get(&self.url)
            .bearer_auth(&self.service_token)
            .send()
            .await
            .with_context(|| format!("the Companion Gateway at {} is unreachable", self.url))?;
        let status = response.status();
        if !status.is_success() {
            // The body carries the Gateway's own error code (`unauthenticated`,
            // `consent_not_configured`, `snapshot_too_large`, …), which is
            // what an operator needs to see; it never carries the token.
            let detail = response.text().await.unwrap_or_default();
            let detail: String = detail.chars().take(500).collect();
            anyhow::bail!(
                "the Companion Gateway at {} answered {status} to the consent snapshot: {detail}",
                self.url
            );
        }
        let document: Value = response
            .json()
            .await
            .context("the Companion Gateway's consent snapshot is not JSON")?;
        ConsentSnapshot::parse(&document)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The Gateway's document, as `companion-gateway/openapi.yaml` describes
    /// it and `consent_snapshot.rs` renders it.
    fn snapshot_document(stream_sequence: u64, entries: Value) -> Value {
        json!({
            "stream": "twalk",
            "subject": "twalk.consent.state.changed.v1",
            "stream_sequence": stream_sequence,
            "next_stream_sequence": stream_sequence + 1,
            "decision_sequence": 7,
            "entries": entries,
        })
    }

    #[test]
    fn the_sensors_snapshot_is_the_crates_state_and_the_gateways_registry() {
        // The decisions are the shared crate's (#273); the registry is read
        // off the same document, and a Gateway's snapshot hands one over even
        // when it is an old Gateway's — read as the implicit one (#269). A
        // deployment with no Gateway has none to hand.
        let snapshot = ConsentSnapshot::parse(&snapshot_document(
            41,
            json!([{
                "subject": { "type": "contact", "id": "@a:example.com" },
                "connection": "whatsapp", "network": "whatsapp", "state": "granted",
                "decided_at": "2026-09-17T10:00:00.000Z", "decision_sequence": 3
            }]),
        ))
        .unwrap();
        assert_eq!(snapshot.state.entries.len(), 1);
        assert_eq!(snapshot.state.next_stream_sequence, 42);
        assert!(snapshot.connections.is_some());
        assert!(ConsentSnapshot::empty().connections.is_none());
        assert_eq!(ConsentSnapshot::empty().state, Snapshot::empty());
    }

    #[test]
    fn the_snapshot_url_is_the_gateways_origin_plus_the_route() {
        let source = GatewaySnapshot::new("http://companion-gateway:8080", "t").unwrap();
        assert_eq!(
            source.url(),
            "http://companion-gateway:8080/api/consent/snapshot"
        );
        let trailing = GatewaySnapshot::new("http://companion-gateway:8080/", "t").unwrap();
        assert_eq!(
            trailing.url(),
            "http://companion-gateway:8080/api/consent/snapshot"
        );
    }

    #[tokio::test]
    async fn without_a_gateway_the_cache_stays_cold_and_the_stream_is_read_whole() {
        let snapshot = NoConsentSnapshot.fetch_snapshot().await.unwrap();
        assert!(snapshot.state.entries.is_empty());
        assert_eq!(snapshot.state.next_stream_sequence, 1);
    }
}
