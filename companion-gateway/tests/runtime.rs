//! Whether a persona runtime is present, read off the bus (ticket #189):
//! `GET /api/runtime` at the Gateway's process boundary, against the real
//! bus, with the three states staged the way a runtime produces them.
//!
//! No runtime runs in this suite, and none needs to: the fact the read
//! projects is a **durable pull consumer named `persona-<id>`** and whether
//! anything is pulling from it, which is exactly what `twalk-hermes` creates
//! and what the SDK binds to. So this suite plays the runtime's part on the
//! bus — creates the consumer the runtime would, pulls from it the way the
//! persona does, stops pulling the way a stopped persona would — and asserts
//! the read changes with each. That is the acceptance criterion in #189's own
//! words: "raise a runtime, stop it, and prove the read changes", proven
//! against the bus rather than against a process.
//!
//! The stream is the shared stack's `twalk`, so the suite removes any
//! `persona-*` consumer another run left before it begins: a consumer
//! outlives the process that created it, which is the first way this signal
//! would rot, and the reason the suite cannot assume `never` on entry.

mod harness;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use harness::{
    companion_build, ensure_stack, gateway_env_with, gateway_env_with_consent, nats_url,
    poll_until, unreachable_nats_url, GatewayProc,
};
use serde_json::Value;

const STREAM: &str = "twalk";
const INBOUND_SUBJECT: &str = "twalk.inbound.message.received.v1";
const PERSONA_PREFIX: &str = "persona-";

fn unique(label: &str) -> String {
    format!(
        "{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_nanos()
    )
}

/// A Gateway with a bus, signed in.
struct Running {
    #[allow(dead_code)]
    gateway: GatewayProc,
    base: String,
    device: String,
    http: reqwest::Client,
}

impl Running {
    async fn start(test_name: &str, env: Vec<(String, String)>) -> Result<Self> {
        let gateway = GatewayProc::start(&env)?;
        let base = gateway.base_url().await?;
        poll_until(
            || async {
                reqwest::get(format!("{base}/health"))
                    .await
                    .ok()?
                    .error_for_status()
                    .ok()
            },
            "the gateway health endpoint",
        )
        .await?;
        let _ = test_name;
        let device = harness::signed_in_device_token(&base).await?;
        Ok(Self {
            gateway,
            base,
            device,
            http: reqwest::Client::new(),
        })
    }

    async fn runtime(&self) -> Result<(reqwest::StatusCode, Value)> {
        let response = self
            .http
            .get(format!("{}/api/runtime", self.base))
            .header(
                reqwest::header::COOKIE,
                format!("twalk_device={}", self.device),
            )
            .send()
            .await
            .context("failed to call GET /api/runtime")?;
        let status = response.status();
        let text = response.text().await?;
        let body = serde_json::from_str(&text)
            .with_context(|| format!("GET /api/runtime answered {status} with non-JSON: {text}"))?;
        Ok((status, body))
    }

    /// The read, waited for until `presence` is the expected word — the bus
    /// is asked live and a consumer's counters move a beat after the client
    /// that moved them.
    async fn presence_becomes(&self, expected: &str) -> Result<Value> {
        poll_until(
            || async {
                let (status, body) = self.runtime().await.ok()?;
                if status != reqwest::StatusCode::OK {
                    return None;
                }
                (body["presence"].as_str() == Some(expected)).then_some(body)
            },
            &format!("the runtime's presence to read {expected}"),
        )
        .await
    }
}

/// The stream, with every `persona-*` consumer another run left behind
/// removed. The Gateway's own consumers and the consent follower are not
/// touched: they are not personas and the read does not count them.
async fn stream_without_stale_personas(
    jetstream: &async_nats::jetstream::Context,
) -> Result<async_nats::jetstream::stream::Stream> {
    let stream = jetstream
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: STREAM.to_owned(),
            subjects: vec!["twalk.>".to_owned()],
            ..Default::default()
        })
        .await
        .map_err(|error| anyhow::anyhow!("failed to ensure the stream: {error}"))?;
    let mut names = stream.consumer_names();
    let mut stale = Vec::new();
    while let Some(name) = names
        .try_next()
        .await
        .map_err(|error| anyhow::anyhow!("failed to list consumers: {error}"))?
    {
        if name.starts_with(PERSONA_PREFIX) {
            stale.push(name);
        }
    }
    for name in stale {
        let _ = stream.delete_consumer(&name).await;
    }
    Ok(stream)
}

fn persona_env(static_dir: &PathBuf) -> Vec<(String, String)> {
    gateway_env_with_consent(static_dir, &nats_url())
}

/// The three states, in the order a deployment goes through them, and back.
#[tokio::test]
async fn never_gone_and_present_are_three_answers_the_bus_tells_apart() -> Result<()> {
    ensure_stack().await?;
    let client = async_nats::connect(nats_url())
        .await
        .context("failed to connect to the bus")?;
    let jetstream = async_nats::jetstream::new(client);
    let stream = stream_without_stale_personas(&jetstream).await?;

    let static_dir = companion_build("runtime-presence")?;
    let running = Running::start("runtime-presence", persona_env(&static_dir)).await?;

    // 1. Nothing has ever hosted a persona on this bus: `never`, and the list
    //    is empty rather than absent.
    let read = running.presence_becomes("never").await?;
    assert_eq!(read["personas"], serde_json::json!([]), "{read}");

    // 2. A runtime creates its persona's consumer — exactly the shape
    //    `twalk-hermes` makes (`ensure_persona_consumer`): durable, pull,
    //    filtered to the inbound subject — and nothing pulls from it yet.
    //    That is a runtime that *was* here: `gone`, not `never`.
    let persona_id = unique("assistant");
    let consumer_name = format!("{PERSONA_PREFIX}{persona_id}");
    let consumer = stream
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some(consumer_name.clone()),
            name: Some(consumer_name.clone()),
            filter_subject: INBOUND_SUBJECT.to_owned(),
            ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
            deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::New,
            ..Default::default()
        })
        .await
        .map_err(|error| anyhow::anyhow!("failed to create the persona consumer: {error}"))?;
    let read = running.presence_becomes("gone").await?;
    let row = &read["personas"][0];
    assert_eq!(
        row["persona_id"].as_str(),
        Some(persona_id.as_str()),
        "{read}"
    );
    assert_eq!(row["consumer"].as_str(), Some(consumer_name.as_str()));
    assert_eq!(row["liveness"].as_str(), Some("idle"), "{read}");
    assert_eq!(row["activation"].as_str(), Some("active"), "{read}");
    assert_eq!(row["waiting_pulls"].as_u64(), Some(0), "{read}");

    // 3. A persona pulls from it — the SDK's loop is `fetch(1, timeout)` for
    //    ever, so a pull request is outstanding on the server. That is a
    //    runtime that *is* here: `present`, with the pull counted.
    let pulling = tokio::spawn({
        let consumer = consumer.clone();
        async move {
            loop {
                let mut batch = match consumer
                    .fetch()
                    .max_messages(1)
                    .expires(Duration::from_secs(2))
                    .messages()
                    .await
                {
                    Ok(batch) => batch,
                    Err(_) => return,
                };
                while let Ok(Some(message)) = batch.try_next().await {
                    let _ = message.ack().await;
                }
            }
        }
    });
    let read = running.presence_becomes("present").await?;
    let row = &read["personas"][0];
    assert_eq!(row["liveness"].as_str(), Some("live"), "{read}");
    assert!(
        row["waiting_pulls"]
            .as_u64()
            .is_some_and(|pulls| pulls >= 1),
        "the pull the persona has outstanding is the evidence: {read}"
    );

    // 4. The persona stops — the process is gone, the consumer stays. Once
    //    the last pull it issued has expired, nothing waits on the server and
    //    the read says `gone` again. This is the state a stale consumer
    //    produces, and the one that would have read as "present" from
    //    existence alone.
    pulling.abort();
    let read = running.presence_becomes("gone").await?;
    assert_eq!(
        read["personas"][0]["liveness"].as_str(),
        Some("idle"),
        "{read}"
    );

    // 5. Paused is a runtime that is here and a persona that receives
    //    nothing (ADR 0013): the runtime moves the consumer's filter to the
    //    paused subject, and the persona keeps pulling. `present`, `paused`.
    let paused_name = format!("{PERSONA_PREFIX}{}", unique("archive"));
    let paused = stream
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some(paused_name.clone()),
            name: Some(paused_name.clone()),
            filter_subject: format!("twalk.hermes.paused.{}", unique("archive")),
            ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
            ..Default::default()
        })
        .await
        .map_err(|error| anyhow::anyhow!("failed to create the paused consumer: {error}"))?;
    let pulling_paused = tokio::spawn(async move {
        loop {
            let mut batch = match paused
                .fetch()
                .max_messages(1)
                .expires(Duration::from_secs(2))
                .messages()
                .await
            {
                Ok(batch) => batch,
                Err(_) => return,
            };
            while let Ok(Some(_)) = batch.try_next().await {}
        }
    });
    let read = running.presence_becomes("present").await?;
    let rows = read["personas"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        rows.len(),
        2,
        "both consumers are listed, live or not: {read}"
    );
    let paused_row = rows
        .iter()
        .find(|row| row["consumer"].as_str() == Some(paused_name.as_str()))
        .expect("the paused persona is listed");
    assert_eq!(paused_row["activation"].as_str(), Some("paused"), "{read}");
    assert_eq!(paused_row["liveness"].as_str(), Some("live"), "{read}");
    let idle_row = rows
        .iter()
        .find(|row| row["consumer"].as_str() == Some(consumer_name.as_str()))
        .expect("the stopped persona is still listed");
    assert_eq!(
        idle_row["liveness"].as_str(),
        Some("idle"),
        "one live persona does not make the stopped one live: {read}"
    );
    pulling_paused.abort();

    // 6. Both consumers removed — a runtime whose personas were unconfigured
    //    and their consumers deleted — and it is `never` again: no trace.
    stream
        .delete_consumer(&consumer_name)
        .await
        .map_err(|error| anyhow::anyhow!("failed to delete the persona consumer: {error}"))?;
    stream
        .delete_consumer(&paused_name)
        .await
        .map_err(|error| anyhow::anyhow!("failed to delete the paused consumer: {error}"))?;
    running.presence_becomes("never").await?;
    Ok(())
}

/// A bus that is configured and does not answer is `502 bus_unreachable`,
/// never `never`: unknown is not absent. The same test the suggestion listing
/// has for the same reason.
#[tokio::test]
async fn a_bus_that_does_not_answer_is_unknown_and_not_never() -> Result<()> {
    ensure_stack().await?;
    let static_dir = companion_build("runtime-presence-bus-down")?;
    let env = gateway_env_with(
        &static_dir,
        &[("GATEWAY_NATS_URL", &unreachable_nats_url()?)],
    );
    let running = Running::start("runtime-presence-bus-down", env).await?;
    let (status, body) = running.runtime().await?;
    assert_eq!(status, reqwest::StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["error"].as_str(), Some("bus_unreachable"), "{body}");
    Ok(())
}
