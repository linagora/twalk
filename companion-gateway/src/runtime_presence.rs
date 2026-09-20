//! Whether a persona runtime is present on this deployment — read off the bus,
//! stored nowhere (ticket #189).
//!
//! # The question, and why nothing could answer it
//!
//! The Companion's dashboard and personas screens told the user no agent
//! runtime was deployed while one was running (#177). They had to say
//! *something*, and there was no fact to read: the Gateway's route table has
//! nothing about the runtime, `Health` and `GET /api/deployment` are closed
//! objects about this process, the runtime has no HTTP server and no
//! healthcheck, and it publishes nothing about itself on the bus. ADR 0013
//! wants it that way — *"There is no control API, no 'start' and no 'stop'"* —
//! and that decision is about **control**. This module is about
//! **observation**, and it does not reopen it: reading here starts, stops and
//! configures nothing.
//!
//! # What is read
//!
//! The runtime's one observable trace is the **durable consumer it creates per
//! persona** — `persona-<id>` on the `twalk` stream, moved between the inbound
//! subject and a paused subject as the user activates and pauses the persona
//! (`hermes/src/main.rs`, ADR 0013). The persona binds to that consumer by name
//! and pulls from it in a loop. So this is a projection of the stream's
//! consumer list: a live read, nothing written, the shape ADR 0024 gave the
//! portal register and for the same reason — a record the deployment keeps
//! about itself can drift from the deployment, and a live read cannot.
//!
//! # The two ways this signal would rot, and what is done about each
//!
//! **A durable consumer outlives the process that created it.** That a
//! `persona-*` consumer exists proves a runtime *was once* here; liveness needs
//! the consumer's own account of who is on the other end. A pull consumer with
//! a fetch outstanding (`num_waiting > 0`) has a process waiting on it *right
//! now*; one with a message delivered and not yet acknowledged
//! (`num_ack_pending > 0`) has a process working on it — a persona spends
//! seconds in a model call between two fetches, and during those seconds no
//! pull is outstanding. Either is [`Liveness::Live`]. A consumer with neither
//! is [`Liveness::Idle`]: the process that pulled from it is gone, or has not
//! pulled in the moment this was read. The persona's fetch loop leaves a gap of
//! milliseconds between one pull expiring and the next being issued, so the
//! read samples more than once ([`SAMPLES`], [`SAMPLE_GAP`]) before it calls a
//! consumer idle. The residual: a persona that crashed mid-message reads as
//! live until the bus redelivers, which is the consumer's `ack_wait` (thirty
//! seconds in the runtime's configuration) — stated rather than hidden.
//!
//! **"The hermes container is up" is not "a runtime is hosting personas."**
//! The reference deployment's entrypoint makes an unconfigured runtime `exec
//! sleep infinity` on purpose, so a stack with no model still comes up (ADR
//! 0015, ADR 0023). Nothing here looks at a container, a process or a port —
//! only at consumers, which exist only when a runtime configured with personas
//! created them.
//!
//! # The three answers
//!
//! - [`Presence::Never`]: no `persona-*` consumer on the stream. No runtime
//!   configured with a persona has ever run against this bus.
//! - [`Presence::Gone`]: consumers exist and none is live. A runtime was here
//!   and is not now — stopped, crashed, or its personas with it.
//! - [`Presence::Present`]: at least one consumer is live. A runtime is here
//!   and hosting personas.
//!
//! Distinguishable by a consumer of the read, not only in a log: the defect
//! class this project has closed a dozen times is two situations behind one
//! signal. Each persona is listed with its own liveness and whether the runtime
//! currently has it active or paused (from the consumer's filter subject), so
//! "a runtime is here and this persona is paused" is not "no runtime".

use std::time::Duration;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use serde::Serialize;

use crate::consent::STREAM_NAME;

/// The prefix of every persona consumer's durable name. Set by the runtime
/// (`PersonaSpec::consumer_name` in `hermes/src/config.rs`) and bound to by the
/// SDK (`Config::durable_name` in `sdk/python/twalk_sdk/config.py`): the one
/// convention the two already share, read here rather than invented.
pub const PERSONA_CONSUMER_PREFIX: &str = "persona-";

/// The subject a paused persona's consumer is filtered to, by the runtime:
/// `<prefix>.hermes.paused.<persona id>`, which nothing ever publishes on
/// (`hermes/src/activation.rs`). Recognised here by its middle segment.
const PAUSED_SEGMENT: &str = ".hermes.paused.";

/// How many times the consumer list is read before a consumer with no pull
/// outstanding and nothing awaiting an ack is called idle, and how long
/// between reads. The persona's loop re-issues its pull the moment the
/// previous one expires; the gap is milliseconds, and three reads a third of a
/// second apart cover it with room to spare while keeping the whole read
/// under a second for a screen that polls.
pub const SAMPLES: u32 = 3;
pub const SAMPLE_GAP: Duration = Duration::from_millis(350);

/// Whether a runtime is present, in the three words the screen needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    /// No persona consumer exists: no runtime configured with a persona has
    /// ever run against this bus.
    Never,
    /// Persona consumers exist and none is live: a runtime was here and is
    /// not now.
    Gone,
    /// At least one persona consumer is live: a runtime is here, hosting
    /// personas.
    Present,
}

/// One persona consumer's liveness, from the consumer's own counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// A pull is outstanding or a delivered message awaits its ack: a
    /// process is on the other end.
    Live,
    /// Neither, across every sample: nothing is pulling from it.
    Idle,
}

/// Whether the runtime currently has this persona active or paused — the
/// filter subject the runtime set on the consumer (ADR 0013), not the consent
/// journal's row, so it is the runtime's own account of what it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Activation {
    Active,
    Paused,
}

/// One persona the runtime created a consumer for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonaPresence {
    /// The persona's id, as the consumer's name carries it.
    pub persona_id: String,
    /// The consumer's durable name.
    pub consumer: String,
    pub liveness: Liveness,
    pub activation: Activation,
    /// Pull requests outstanding at the last sample.
    pub waiting_pulls: usize,
    /// Messages delivered and not yet acknowledged at the last sample.
    pub ack_pending: usize,
}

/// The whole answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reading {
    pub presence: Presence,
    pub personas: Vec<PersonaPresence>,
}

/// The pure half: the three answers from a set of consumer observations.
///
/// Each observation is the *best* of the samples taken for one consumer — a
/// consumer seen with a pull outstanding in any sample is live. Exposed so the
/// rule is tested against values rather than against a bus.
pub fn reading(observations: Vec<Observation>) -> Reading {
    let personas: Vec<PersonaPresence> = observations
        .into_iter()
        .filter_map(|observation| {
            let persona_id = observation
                .consumer
                .strip_prefix(PERSONA_CONSUMER_PREFIX)?
                .to_owned();
            if persona_id.is_empty() {
                return None;
            }
            let liveness = if observation.waiting_pulls > 0 || observation.ack_pending > 0 {
                Liveness::Live
            } else {
                Liveness::Idle
            };
            let activation = if observation.filter_subject.contains(PAUSED_SEGMENT) {
                Activation::Paused
            } else {
                Activation::Active
            };
            Some(PersonaPresence {
                persona_id,
                consumer: observation.consumer,
                liveness,
                activation,
                waiting_pulls: observation.waiting_pulls,
                ack_pending: observation.ack_pending,
            })
        })
        .collect();
    let presence = if personas.is_empty() {
        Presence::Never
    } else if personas
        .iter()
        .any(|persona| persona.liveness == Liveness::Live)
    {
        Presence::Present
    } else {
        Presence::Gone
    };
    Reading { presence, personas }
}

/// What one sample of one consumer said. Only what the rule needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub consumer: String,
    pub filter_subject: String,
    pub waiting_pulls: usize,
    pub ack_pending: usize,
}

/// The live read over the bus.
pub struct RuntimePresence {
    nats_url: String,
    bus: tokio::sync::OnceCell<async_nats::jetstream::Context>,
}

impl RuntimePresence {
    pub fn new(nats_url: String) -> Self {
        Self {
            nats_url,
            bus: tokio::sync::OnceCell::new(),
        }
    }

    /// The bus, connected on first need, and **without**
    /// `retry_on_initial_connect` — the choice [`crate::suggestions`] and
    /// [`crate::approval`] make: a screen is waiting, and a read that cannot
    /// reach the bus says so now rather than hanging behind a reconnection.
    async fn jetstream(&self) -> Result<&async_nats::jetstream::Context> {
        self.bus
            .get_or_try_init(|| async {
                let client = async_nats::ConnectOptions::new()
                    .connect(&self.nats_url)
                    .await
                    .with_context(|| format!("the bus at {} did not answer", self.nats_url))?;
                Ok(async_nats::jetstream::new(client))
            })
            .await
    }

    /// One read: the consumer list sampled [`SAMPLES`] times, the best
    /// sample kept per consumer, and the rule applied.
    ///
    /// A stream that does not exist yet is a bus nothing has published on,
    /// which is [`Presence::Never`] and not an error — a fresh deployment's
    /// first minute.
    pub async fn read(&self) -> Result<Reading> {
        let jetstream = self.jetstream().await?;
        let mut best: std::collections::BTreeMap<String, Observation> = Default::default();
        for sample in 0..SAMPLES {
            if sample > 0 {
                tokio::time::sleep(SAMPLE_GAP).await;
            }
            let stream = match jetstream.get_stream(STREAM_NAME).await {
                Ok(stream) => stream,
                Err(error) if is_stream_not_found(&error) => {
                    return Ok(reading(Vec::new()));
                }
                Err(error) => {
                    return Err(anyhow::anyhow!("the stream could not be read: {error}"));
                }
            };
            let mut consumers = stream.consumers();
            while let Some(info) = consumers
                .try_next()
                .await
                .map_err(|error| anyhow::anyhow!("the consumer list could not be read: {error}"))?
            {
                if !info.name.starts_with(PERSONA_CONSUMER_PREFIX) {
                    continue;
                }
                let observation = Observation {
                    consumer: info.name.clone(),
                    filter_subject: info.config.filter_subject.clone(),
                    waiting_pulls: info.num_waiting,
                    ack_pending: info.num_ack_pending,
                };
                match best.get(&info.name) {
                    Some(seen) if seen.waiting_pulls + seen.ack_pending > 0 => {
                        // Already seen live; a later idle sample does not
                        // undo it. Keep the live one.
                    }
                    _ => {
                        best.insert(info.name.clone(), observation);
                    }
                }
            }
            if !best.is_empty()
                && best
                    .values()
                    .all(|seen| seen.waiting_pulls + seen.ack_pending > 0)
            {
                // Every consumer seen live already: nothing a further sample
                // could change, so the screen gets its answer now.
                break;
            }
        }
        if best.is_empty() {
            // Said once per read at debug rather than warn: a deployment that
            // runs no runtime is a supported state (ADR 0013, ADR 0015).
            tracing::debug!(
                "no persona consumer on the stream: no runtime has ever hosted a persona here"
            );
        }
        Ok(reading(best.into_values().collect()))
    }
}

/// Whether a `get_stream` error is "no such stream" rather than "the bus did
/// not answer" — the former is a fresh bus and an honest [`Presence::Never`],
/// the latter is an error the caller must not turn into an answer.
fn is_stream_not_found(error: &async_nats::jetstream::context::GetStreamError) -> bool {
    use async_nats::jetstream::context::GetStreamErrorKind;
    match error.kind() {
        GetStreamErrorKind::JetStream(inner) => {
            inner.error_code() == async_nats::jetstream::ErrorCode::STREAM_NOT_FOUND
        }
        _ => false,
    }
}

impl std::fmt::Debug for RuntimePresence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimePresence")
            .field("nats_url", &self.nats_url)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(consumer: &str, filter: &str, waiting: usize, pending: usize) -> Observation {
        Observation {
            consumer: consumer.to_owned(),
            filter_subject: filter.to_owned(),
            waiting_pulls: waiting,
            ack_pending: pending,
        }
    }

    #[test]
    fn no_persona_consumer_means_no_runtime_has_ever_been_here() {
        assert_eq!(reading(Vec::new()).presence, Presence::Never);
        // Other consumers on the stream — the Gateway's own projection, the
        // runtime's consent follower — are not personas and say nothing.
        let others = vec![
            observed(
                "gateway-contacts",
                "twalk.inbound.message.received.v1",
                1,
                0,
            ),
            observed("hermes-activation", "twalk.consent.state.changed.v1", 1, 0),
        ];
        let read = reading(others);
        assert_eq!(read.presence, Presence::Never);
        assert!(read.personas.is_empty());
    }

    #[test]
    fn a_consumer_nobody_pulls_from_is_a_runtime_that_was_here() {
        let read = reading(vec![observed(
            "persona-assistant",
            "twalk.inbound.message.received.v1",
            0,
            0,
        )]);
        assert_eq!(read.presence, Presence::Gone);
        assert_eq!(read.personas[0].liveness, Liveness::Idle);
        assert_eq!(read.personas[0].persona_id, "assistant");
    }

    #[test]
    fn a_pull_outstanding_is_a_runtime_that_is_here() {
        let read = reading(vec![observed(
            "persona-assistant",
            "twalk.inbound.message.received.v1",
            1,
            0,
        )]);
        assert_eq!(read.presence, Presence::Present);
        assert_eq!(read.personas[0].liveness, Liveness::Live);
        assert_eq!(read.personas[0].activation, Activation::Active);
    }

    #[test]
    fn a_message_being_worked_on_is_live_even_with_no_pull_outstanding() {
        // A persona in a model call has taken its message and not yet issued
        // the next fetch: nothing waits, one ack is pending.
        let read = reading(vec![observed(
            "persona-assistant",
            "twalk.inbound.message.received.v1",
            0,
            1,
        )]);
        assert_eq!(read.presence, Presence::Present);
    }

    #[test]
    fn a_paused_persona_is_a_runtime_that_is_here_and_says_paused() {
        let read = reading(vec![observed(
            "persona-assistant",
            "twalk.hermes.paused.assistant",
            1,
            0,
        )]);
        assert_eq!(read.presence, Presence::Present);
        assert_eq!(read.personas[0].activation, Activation::Paused);
    }

    #[test]
    fn one_live_persona_among_idle_ones_is_present() {
        let read = reading(vec![
            observed("persona-archive", "twalk.inbound.message.received.v1", 0, 0),
            observed(
                "persona-assistant",
                "twalk.inbound.message.received.v1",
                1,
                0,
            ),
        ]);
        assert_eq!(read.presence, Presence::Present);
        assert_eq!(read.personas.len(), 2);
    }

    #[test]
    fn the_prefix_alone_is_not_a_persona() {
        let read = reading(vec![observed(
            "persona-",
            "twalk.inbound.message.received.v1",
            1,
            0,
        )]);
        assert_eq!(read.presence, Presence::Never);
    }
}
