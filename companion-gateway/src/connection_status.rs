//! What a connection says about itself, read off the bus (issue #275):
//! `connection.status.changed.v1`, the collector's word on whether the
//! account it holds can be reached, whether the grant still stands, and what
//! the operator does next (#274). The Gateway is a **reader** here — a
//! collector holds the connection and publishes its state, as the Gateway
//! itself publishes a bridge's on `bridge.status.changed.v1` — and it keeps
//! the state for two uses:
//!
//! - an approval towards a connection that cannot send is **refused before
//!   it is published** (`connection_not_connected`, a `409` with the state
//!   and the operator's hint), rather than an approved reply left on the bus
//!   for a collector that will not take it — #229's shape for these
//!   connections;
//! - the Companion shows each collector connection's state as one of four
//!   sentences, and the dashboard's activity feed carries the transitions.
//!
//! A connection that never said anything has **no** status here: a bridge's
//! connection, whose state is the bridge's (`bridge_status.rs`), or a
//! collector that has not run yet. An approval towards such a connection is
//! not refused on this ground — nothing said it could not send — which is
//! also why `unknown` is not stored as a state: it is the absence of a row.
//!
//! The consumer is durable and reads from the beginning on its first run:
//! the subject carries a handful of transitions per connection per day, and
//! a Gateway that starts after the collector must not carry `connected`
//! forward from a state it never saw.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::store::Store;

pub const CONNECTION_STATUS_CHANGED_TYPE: &str = "fr.linagora.twalk.connection.status.changed.v1";
/// The durable consumer's name, from the pending-contact consumer's
/// (`GATEWAY_INBOUND_CONSUMER`): the one setting an operator who points a
/// second Gateway at one bus already changes, so the two consumers of one
/// Gateway stay one configuration — and two Gateways never share a durable
/// and take turns at each other's transitions.
pub fn consumer_name(inbound_consumer: &str) -> String {
    format!("{inbound_consumer}-connection-status")
}
/// How many transitions the registry document carries for the feed.
pub const RECENT_TRANSITIONS: usize = 50;

/// One transition, as the contract's `data` has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub event_id: String,
    pub connection: String,
    pub kind: String,
    pub from_state: String,
    pub to_state: String,
    pub occurred_at: String,
    pub service: Option<String>,
    pub hint: Option<String>,
}

impl Change {
    /// Reads a `connection.status.changed.v1` event. What the contract
    /// requires is required here; what it makes optional is.
    pub fn parse(event: &Value) -> Result<Self> {
        anyhow::ensure!(
            event.get("type").and_then(Value::as_str) == Some(CONNECTION_STATUS_CHANGED_TYPE),
            "not a connection.status.changed event"
        );
        let string = |pointer: &str| -> Result<String> {
            event
                .pointer(pointer)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .with_context(|| format!("the event has no string at {pointer}"))
        };
        let optional = |pointer: &str| -> Option<String> {
            event
                .pointer(pointer)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let to_state = string("/data/to_state")?;
        anyhow::ensure!(
            STATES.contains(&to_state.as_str()),
            "the event names the unknown state {to_state:?}"
        );
        Ok(Self {
            event_id: string("/id")?,
            connection: string("/data/connection")?,
            kind: string("/data/kind")?,
            from_state: string("/data/from_state")?,
            to_state,
            occurred_at: string("/data/occurred_at")?,
            service: optional("/data/service"),
            hint: optional("/data/hint").map(|hint| hint.chars().take(1024).collect()),
        })
    }

    /// The transition as the registry document lists it.
    pub fn json(&self) -> Value {
        json!({
            "connection": self.connection,
            "kind": self.kind,
            "from_state": self.from_state,
            "to_state": self.to_state,
            "occurred_at": self.occurred_at,
            "service": self.service,
            "hint": self.hint,
        })
    }
}

/// The four states a connection can be in, the contract's.
pub const STATES: [&str; 4] = [
    "connected",
    "unreachable",
    "reconnect_required",
    "pending_operator",
];

/// A connection's current state, as the store's view answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub connection: String,
    pub kind: String,
    pub state: String,
    pub occurred_at: String,
    pub service: Option<String>,
    pub hint: Option<String>,
}

impl Current {
    pub fn is_connected(&self) -> bool {
        self.state == "connected"
    }

    /// The `status` member of a registry entry.
    pub fn json(&self) -> Value {
        json!({
            "state": self.state,
            "occurred_at": self.occurred_at,
            "service": self.service,
            "hint": self.hint,
        })
    }
}

/// The consumer: the store it records into, and the bus it reads.
pub struct ConnectionStatuses {
    store: Arc<Store>,
    nats_url: String,
    consumer: String,
    bus: tokio::sync::OnceCell<async_nats::jetstream::Context>,
}

impl ConnectionStatuses {
    pub fn new(store: Arc<Store>, nats_url: String, inbound_consumer: &str) -> Self {
        Self {
            store,
            nats_url,
            consumer: consumer_name(inbound_consumer),
            bus: tokio::sync::OnceCell::new(),
        }
    }

    pub fn consumer_name(&self) -> &str {
        &self.consumer
    }

    async fn jetstream(&self) -> Result<&async_nats::jetstream::Context> {
        self.bus
            .get_or_try_init(|| async {
                let client = async_nats::ConnectOptions::new()
                    .retry_on_initial_connect()
                    .connect(&self.nats_url)
                    .await
                    .context("failed to connect to the bus for connection statuses")?;
                Ok(async_nats::jetstream::new(client))
            })
            .await
    }
}

const DRAIN_BATCH: usize = 100;
const DRAIN_IDLE: std::time::Duration = std::time::Duration::from_secs(5);

/// Never returns: reads the subject in batches for as long as the Gateway
/// runs, recording each transition before acking it.
pub async fn consume_until_shutdown(statuses: Arc<ConnectionStatuses>) {
    let subject = crate::consent::bus_subject(CONNECTION_STATUS_CHANGED_TYPE);
    let jetstream = match statuses.jetstream().await {
        Ok(jetstream) => jetstream,
        Err(error) => {
            warn!(%error, "connection statuses are not consumed: the Companion will show no collector state and an approval towards a connection that cannot send will not be refused on that ground");
            return;
        }
    };
    info!(consumer = %statuses.consumer, %subject, "consuming connection statuses");
    loop {
        match drain(&statuses, jetstream, &subject).await {
            Ok(_) => {}
            Err(error) => {
                warn!(%error, "the connection status consumer could not read the bus; retrying");
                tokio::time::sleep(DRAIN_IDLE).await;
            }
        }
    }
}

/// One batch: fetched, each transition recorded, then acked. Record then
/// ack, so a crash between the two redelivers a batch the store's unique id
/// absorbs. A message this build cannot read is acked and skipped.
async fn drain(
    statuses: &ConnectionStatuses,
    jetstream: &async_nats::jetstream::Context,
    subject: &str,
) -> Result<usize> {
    let stream = jetstream
        .get_stream(crate::consent::STREAM_NAME)
        .await
        .context("failed to reach the bus stream")?;
    let consumer = stream
        .get_or_create_consumer(
            &statuses.consumer,
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some(statuses.consumer.clone()),
                filter_subject: subject.to_owned(),
                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::All,
                ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                ..Default::default()
            },
        )
        .await
        .context("failed to create the connection status consumer")?;
    let mut batch = consumer
        .fetch()
        .max_messages(DRAIN_BATCH)
        .expires(DRAIN_IDLE)
        .messages()
        .await
        .context("failed to fetch from the connection status consumer")?;
    use futures::StreamExt;
    let mut recorded = 0usize;
    let mut delivered = Vec::new();
    while let Some(message) = batch.next().await {
        let message =
            message.map_err(|error| anyhow::anyhow!("failed to read a message: {error}"))?;
        match serde_json::from_slice::<Value>(&message.payload)
            .context("not JSON")
            .and_then(|event| Change::parse(&event))
        {
            Ok(change) => {
                let now = crate::consent::rfc3339_millis(std::time::SystemTime::now());
                if statuses
                    .store
                    .record_connection_status_change(&change, &now)
                    .context("failed to record a connection status change")?
                {
                    recorded += 1;
                    info!(
                        connection = %change.connection,
                        from = %change.from_state,
                        to = %change.to_state,
                        service = change.service.as_deref(),
                        "a connection changed state"
                    );
                }
            }
            Err(error) => warn!(%error, "a connection status event could not be read; skipping it"),
        }
        delivered.push(message);
    }
    for message in delivered {
        message
            .ack()
            .await
            .map_err(|error| anyhow::anyhow!("failed to ack a message: {error}"))?;
    }
    if recorded > 0 {
        debug!(recorded, "recorded a batch of connection status changes");
    }
    Ok(recorded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_contracts_fixture_is_read_and_an_unknown_state_is_refused() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../contracts/cloudevents/v1/fixtures/connection.status.changed.json"
        ))
        .unwrap();
        let change = Change::parse(&fixture).unwrap();
        assert_eq!(change.connection, "mail-linagora");
        assert_eq!(change.kind, "email");
        assert_eq!(change.from_state, "connected");
        assert_eq!(change.to_state, "reconnect_required");
        assert_eq!(change.service.as_deref(), Some("sso"));
        assert!(change
            .hint
            .as_deref()
            .unwrap()
            .contains("authorize --renew"));
        let mut odd = fixture.clone();
        odd["data"]["to_state"] = json!("asleep");
        assert!(Change::parse(&odd).is_err());
        let mut other = fixture;
        other["type"] = json!("fr.linagora.twalk.bridge.status.changed.v1");
        assert!(Change::parse(&other).is_err());
    }

    #[test]
    fn the_consumer_is_named_after_the_inbound_one() {
        assert_eq!(
            consumer_name("gateway-inbound-message-received"),
            "gateway-inbound-message-received-connection-status"
        );
    }

    #[test]
    fn the_four_states_are_the_contracts() {
        let schema: Value = serde_json::from_str(include_str!(
            "../../contracts/cloudevents/v1/connection.status.changed.schema.json"
        ))
        .unwrap();
        let contract: Vec<&str> = schema["properties"]["data"]["properties"]["to_state"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(contract, STATES.to_vec());
    }
}
