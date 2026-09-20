//! The bus side of the seam: NATS JetStream, as the components use it.

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::time::sleep;

use crate::stack::{nats_url, SERVER_NAME};
use crate::wait::poll_until;

pub struct Bus {
    client: async_nats::Client,
    jetstream: async_nats::jetstream::Context,
}

impl Bus {
    /// Connects, retrying while the container finishes starting: the nats
    /// image ships no wget or CLI, so the compose stack cannot healthcheck
    /// it and `--wait` does not cover it.
    pub async fn connect() -> Result<Self> {
        Self::connect_to(&nats_url()).await
    }

    /// Connects to an arbitrary NATS URL: the deployment test targets the
    /// deploy stack's bus, not the harness's own.
    pub async fn connect_to(url: &str) -> Result<Self> {
        for attempt in 0..30 {
            match async_nats::connect(url).await {
                Ok(client) => {
                    return Ok(Self {
                        jetstream: async_nats::jetstream::new(client.clone()),
                        client,
                    })
                }
                Err(e) if attempt < 29 => {
                    let _ = e;
                    sleep(Duration::from_millis(500)).await;
                }
                Err(e) => return Err(e).context("failed to connect to NATS"),
            }
        }
        unreachable!("the loop either returns or exhausts attempts")
    }

    /// Creates (or reuses) a JetStream stream capturing the given subjects.
    pub async fn ensure_stream(&self, name: &str, subjects: &[&str]) -> Result<()> {
        let config = async_nats::jetstream::stream::Config {
            name: name.to_owned(),
            subjects: subjects.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        self.jetstream
            .get_or_create_stream(config)
            .await
            .context("failed to create stream")?;
        Ok(())
    }

    /// Removes a stream and everything it holds. The test stack persists
    /// across runs, so a suite that gives itself a stream of its own — the
    /// way a persona test isolates a whole subject namespace from the
    /// Sensor's `twalk.>` — takes it away again instead of leaving one
    /// behind per run.
    ///
    /// Removing one that is not there is not an error: the postcondition is
    /// that it is gone.
    pub async fn delete_stream(&self, name: &str) -> Result<()> {
        let _ = self.jetstream.delete_stream(name).await;
        Ok(())
    }

    pub async fn publish(&self, subject: &str, payload: &Value) -> Result<()> {
        let ack = self
            .jetstream
            .publish(subject.to_owned(), serde_json::to_vec(payload)?.into())
            .await
            .context("publish failed")?;
        ack.await.context("publish ack failed")?;
        Ok(())
    }

    /// Publishes a CloudEvent the way a component does: with `Nats-Msg-Id`
    /// set to the event's `id`, so the bus de-duplicates a re-published
    /// event.
    pub async fn publish_event(&self, subject: &str, event: &Value) -> Result<()> {
        let id = event["id"].as_str().context("the event has no string id")?;
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(async_nats::header::NATS_MESSAGE_ID, id);
        let ack = self
            .jetstream
            .publish_with_headers(
                subject.to_owned(),
                headers,
                serde_json::to_vec(event)?.into(),
            )
            .await
            .context("publish failed")?;
        ack.await.context("publish ack failed")?;
        Ok(())
    }

    /// Publishes a CloudEvent with headers of the caller's own — the shape of
    /// the Sensor's report of a posted reply (`reach`, `posted-as` on
    /// `…reply.approved.v1.posted`), for a suite that runs no Sensor and plays
    /// its part. `Nats-Msg-Id` is `<id>:<suffix>`, as the Sensor sets it
    /// (`outbound::posted_msg_id`): the report carries the approval's own
    /// `id`, and under that id alone the bus would deduplicate it against the
    /// approval and drop it.
    pub async fn publish_event_with_headers(
        &self,
        subject: &str,
        msg_id_suffix: &str,
        mut headers: async_nats::HeaderMap,
        event: &Value,
    ) -> Result<()> {
        let id = event["id"].as_str().context("the event has no string id")?;
        headers.insert(
            async_nats::header::NATS_MESSAGE_ID,
            format!("{id}:{msg_id_suffix}").as_str(),
        );
        let ack = self
            .jetstream
            .publish_with_headers(
                subject.to_owned(),
                headers,
                serde_json::to_vec(event)?.into(),
            )
            .await
            .context("publish failed")?;
        ack.await.context("publish ack failed")?;
        Ok(())
    }

    /// Fetches the most recent message stored on a subject, if any.
    pub async fn last_message(&self, stream: &str, subject: &str) -> Result<Option<Value>> {
        use async_nats::jetstream::stream::LastRawMessageErrorKind;
        let stream = self
            .jetstream
            .get_stream(stream)
            .await
            .context("failed to get stream")?;
        match stream.get_last_raw_message_by_subject(subject).await {
            Ok(message) => Ok(Some(serde_json::from_slice(&message.payload)?)),
            Err(e) if e.kind() == LastRawMessageErrorKind::NoMessageFound => Ok(None),
            Err(e) => Err(e).context("failed to fetch last message"),
        }
    }

    /// The stream sequence of the last message stored on a subject, or `0`
    /// when none was: what a Companion Gateway that has published every decision it
    /// holds would name as its position, read from the stream's index
    /// rather than by replaying the subject.
    pub async fn last_sequence(&self, stream: &str, subject: &str) -> Result<u64> {
        use async_nats::jetstream::stream::LastRawMessageErrorKind;
        let stream = self
            .jetstream
            .get_stream(stream)
            .await
            .context("failed to get stream")?;
        match stream.get_last_raw_message_by_subject(subject).await {
            Ok(message) => Ok(message.sequence),
            Err(e) if e.kind() == LastRawMessageErrorKind::NoMessageFound => Ok(0),
            Err(e) => Err(e).context("failed to fetch the last message"),
        }
    }

    /// The stream's last sequence, whatever the subject: the position a test
    /// notes before it starts the process under test, so that `fetch_since`
    /// reads only what that process published — on a long-lived bus,
    /// walking a subject from sequence 1 is a minute per read.
    pub async fn head(&self, stream: &str) -> Result<u64> {
        let mut stream = self
            .jetstream
            .get_stream(stream)
            .await
            .context("failed to get stream")?;
        Ok(stream
            .info()
            .await
            .context("failed to read the stream's info")?
            .state
            .last_sequence)
    }

    /// Every message stored on a subject after `after` (a sequence from
    /// `head`), in stream order.
    pub async fn fetch_since(&self, stream: &str, subject: &str, after: u64) -> Result<Vec<Value>> {
        use async_nats::jetstream::stream::LastRawMessageErrorKind;
        let stream = self
            .jetstream
            .get_stream(stream)
            .await
            .context("failed to get stream")?;
        let last = match stream.get_last_raw_message_by_subject(subject).await {
            Ok(message) => message.sequence,
            Err(e) if e.kind() == LastRawMessageErrorKind::NoMessageFound => return Ok(Vec::new()),
            Err(e) => return Err(e).context("failed to fetch last message"),
        };
        let mut out = Vec::new();
        for sequence in (after + 1)..=last {
            match stream.get_raw_message(sequence).await {
                Ok(message) if message.subject.as_str() == subject => {
                    out.push(serde_json::from_slice(&message.payload)?)
                }
                Ok(_) => {}
                Err(e) if e.kind() == LastRawMessageErrorKind::NoMessageFound => {}
                Err(e) => return Err(e).context("failed to fetch message"),
            }
        }
        Ok(out)
    }

    /// Fetches every message stored on a subject, in stream order: the
    /// harness's "consume a subject" primitive. Tests assert on whole
    /// sequences (e.g. no duplicate ids after a replay).
    pub async fn fetch_all(&self, stream: &str, subject: &str) -> Result<Vec<Value>> {
        Ok(self
            .fetch_all_with_headers(stream, subject)
            .await?
            .into_iter()
            .map(|message| message.payload)
            .collect())
    }

    /// Like `fetch_all`, but keeps the NATS headers alongside each payload
    /// (needed to assert on `NATS-Msg-Id` and the filtering extensions).
    pub async fn fetch_all_with_headers(
        &self,
        stream: &str,
        subject: &str,
    ) -> Result<Vec<StoredMessage>> {
        use async_nats::jetstream::stream::LastRawMessageErrorKind;
        let stream = self
            .jetstream
            .get_stream(stream)
            .await
            .context("failed to get stream")?;
        let last = match stream.get_last_raw_message_by_subject(subject).await {
            Ok(message) => message,
            Err(e) if e.kind() == LastRawMessageErrorKind::NoMessageFound => return Ok(Vec::new()),
            Err(e) => return Err(e).context("failed to fetch last message"),
        };
        let mut out = Vec::new();
        for sequence in 1..=last.sequence {
            match stream.get_raw_message(sequence).await {
                Ok(message) if message.subject.as_str() == subject => out.push(StoredMessage {
                    headers: message
                        .headers
                        .iter()
                        .flat_map(|(name, values)| {
                            values
                                .iter()
                                .map(move |value| (name.to_string(), value.to_string()))
                        })
                        .collect(),
                    payload: serde_json::from_slice(&message.payload)?,
                    sequence: message.sequence,
                }),
                Ok(_) => {}
                Err(e) if e.kind() == LastRawMessageErrorKind::NoMessageFound => {}
                Err(e) => return Err(e).context("failed to fetch message"),
            }
        }
        Ok(out)
    }

    /// Fetches every stored event whose `source` identifies the given room.
    /// Several tests share the bus, so consumers filter by room — as real
    /// consumers will.
    pub async fn fetch_room_messages(
        &self,
        stream: &str,
        subject: &str,
        room_id: &str,
    ) -> Result<Vec<StoredMessage>> {
        self.fetch_room_messages_on(SERVER_NAME, stream, subject, room_id)
            .await
    }

    /// Same as `fetch_room_messages`, against an arbitrary server name: the
    /// deploy stack has its own (`deploy.twalk` in deployment.rs).
    pub async fn fetch_room_messages_on(
        &self,
        server_name: &str,
        stream: &str,
        subject: &str,
        room_id: &str,
    ) -> Result<Vec<StoredMessage>> {
        let expected_source = format!("matrix://{server_name}/{room_id}");
        Ok(self
            .fetch_all_with_headers(stream, subject)
            .await?
            .into_iter()
            .filter(|m| m.payload["source"].as_str() == Some(expected_source.as_str()))
            .collect())
    }

    /// Polls until an event whose `source` identifies the given room is
    /// stored on the subject.
    pub async fn wait_for_room_message(
        &self,
        stream: &str,
        subject: &str,
        room_id: &str,
    ) -> Result<StoredMessage> {
        self.wait_for_room_message_on(SERVER_NAME, stream, subject, room_id)
            .await
    }

    /// Same as `wait_for_room_message`, against an arbitrary server name.
    pub async fn wait_for_room_message_on(
        &self,
        server_name: &str,
        stream: &str,
        subject: &str,
        room_id: &str,
    ) -> Result<StoredMessage> {
        poll_until(
            || async {
                self.fetch_room_messages_on(server_name, stream, subject, room_id)
                    .await
                    .ok()?
                    .into_iter()
                    .next()
            },
            &format!("waiting for an event from {room_id} on {subject}"),
        )
        .await
    }

    /// What a cold consumer sees when it starts at a given stream sequence:
    /// a durable pull consumer created with
    /// `DeliverPolicy::ByStartSequence`, filtered to one subject, drained
    /// once and deleted.
    ///
    /// This is the consumer side of the Companion Gateway's consent snapshot
    /// hand-off (ADR 0010): the snapshot names the sequence it reflects, and
    /// a consumer applying it then starts *at that sequence plus one*. A
    /// test asserting that hand-off has to create the consumer exactly as a
    /// real one would, which is what this does — the durable name is unique
    /// per call, because the bus is shared by every suite and run.
    ///
    /// Deliberately a single drain rather than a subscription: the messages
    /// are acked as they arrive, the batch ends as soon as the stream has no
    /// more, and the caller gets one deterministic list to assert on. Wait
    /// for what you expect to be *stored* (`fetch_all_with_headers`) before
    /// calling this, or a race makes the list short.
    pub async fn consume_from(
        &self,
        stream: &str,
        subject: &str,
        start_sequence: u64,
        max_messages: usize,
    ) -> Result<Vec<StoredMessage>> {
        use futures::StreamExt;

        let stream = self
            .jetstream
            .get_stream(stream)
            .await
            .context("failed to get stream")?;
        let name = format!(
            "harness-from-{start_sequence}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the clock is after the epoch")
                .as_nanos()
        );
        let consumer = stream
            .create_consumer(async_nats::jetstream::consumer::pull::Config {
                durable_name: Some(name.clone()),
                filter_subject: subject.to_owned(),
                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::ByStartSequence {
                    start_sequence,
                },
                ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                ..Default::default()
            })
            .await
            .context("failed to create the consumer")?;
        let mut batch = consumer
            .fetch()
            .max_messages(max_messages)
            .messages()
            .await
            .context("failed to fetch from the consumer")?;
        let mut out = Vec::new();
        while let Some(message) = batch.next().await {
            let message =
                message.map_err(|error| anyhow::anyhow!("failed to read a message: {error}"))?;
            let sequence = message
                .info()
                .map(|info| info.stream_sequence)
                .unwrap_or_default();
            out.push(StoredMessage {
                headers: message
                    .headers
                    .iter()
                    .flat_map(|headers| {
                        headers.iter().flat_map(|(name, values)| {
                            values
                                .iter()
                                .map(move |value| (name.to_string(), value.to_string()))
                        })
                    })
                    .collect(),
                payload: serde_json::from_slice(&message.payload)?,
                sequence,
            });
            message
                .ack()
                .await
                .map_err(|error| anyhow::anyhow!("failed to ack a message: {error}"))?;
        }
        stream
            .delete_consumer(&name)
            .await
            .context("failed to delete the consumer")?;
        Ok(out)
    }

    /// Removes a durable consumer, so the next component that asks for it
    /// creates it — a **cold** start, which is the side of the consent
    /// snapshot hand-off that only exists once (ADR 0010): a durable consumer
    /// outlives the process that made it, so without this a suite only ever
    /// tests the warm path after its first run.
    ///
    /// Deleting one that is not there is not an error here: the postcondition
    /// is that it is gone.
    pub async fn delete_consumer(&self, stream: &str, consumer: &str) -> Result<()> {
        let stream = self
            .jetstream
            .get_stream(stream)
            .await
            .context("failed to get stream")?;
        let _ = stream.delete_consumer(consumer).await;
        Ok(())
    }

    /// What a durable consumer still owes: how many messages it has not been
    /// handed yet, how many it has been handed and not acked, and how many it
    /// has been handed more than once.
    ///
    /// This is the bus's own account of a consumer, not a component's, and it
    /// is the only way to tell "this trigger was dealt with" from "this
    /// trigger is in flight and will come back" — the difference a persona
    /// that cannot produce a suggestion has to make visible rather than
    /// leaving an event apparently unprocessed for ever (issue #162).
    pub async fn consumer_state(&self, stream: &str, consumer: &str) -> Result<ConsumerState> {
        let stream = self
            .jetstream
            .get_stream(stream)
            .await
            .context("failed to get stream")?;
        let mut consumer = stream
            .get_consumer::<async_nats::jetstream::consumer::pull::Config>(consumer)
            .await
            .map_err(|error| anyhow::anyhow!("failed to get consumer: {error}"))?;
        let info = consumer
            .info()
            .await
            .context("failed to read consumer info")?;
        Ok(ConsumerState {
            pending: info.num_pending,
            awaiting_ack: info.num_ack_pending,
            redelivered: info.num_redelivered,
        })
    }

    /// Subscribes to a subject with core NATS, bypassing JetStream dedup:
    /// the returned receiver observes EVERY publish, including a republish
    /// that the stream later deduplicates on storage. This is how tests
    /// prove a component never re-emits an event, instead of relying on the
    /// bus to absorb replays. Subscribe before the traffic under test.
    pub async fn subscribe_raw(
        &self,
        subject: &str,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<Value>> {
        let mut subscription = self
            .client
            .subscribe(subject.to_owned())
            .await
            .context("subscribe failed")?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            use futures::StreamExt;
            while let Some(message) = subscription.next().await {
                if let Ok(payload) = serde_json::from_slice::<Value>(&message.payload) {
                    let _ = tx.send(payload);
                }
            }
        });
        Ok(rx)
    }
}

/// A durable consumer's outstanding work, as the bus sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsumerState {
    /// Messages matching its filter that it has not been handed yet.
    pub pending: u64,
    /// Messages it has been handed and has neither acked nor terminated.
    pub awaiting_ack: usize,
    /// Messages it has been handed more than once.
    pub redelivered: usize,
}

/// A message as stored on the bus: payload plus NATS headers, and the stream
/// sequence it is stored at — which is what a consumer resumes from, and what
/// the Gateway's consent snapshot names (ADR 0010).
pub struct StoredMessage {
    pub headers: Vec<(String, String)>,
    pub payload: Value,
    pub sequence: u64,
}

impl StoredMessage {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}
