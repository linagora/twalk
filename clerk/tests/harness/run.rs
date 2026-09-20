//! One run of the clerk under test: everything a process-boundary suite
//! sets up, claimed once and given back once.
//!
//! [`Run::start`] is what every test in `approbations.rs`, `journal.rs`,
//! `activite.rs` and `sweep.rs` begins with: the shared stack (for the
//! bus), the relay stack, a bus connection, a stream **and** a subject
//! prefix that are both this run's id (the Hermes suite's shape, so a
//! durable consumer never replays another run's history), a fresh clerk
//! key added to the relay, three fresh channels with that key in them, and
//! the clerk binary started on all of it and waited for until it says
//! `clerk running` — because the two consumers that deliver from `New`
//! miss what was published before they existed, and a test that published
//! first would be asserting on a race.
//!
//! What the run gives back is the same list in reverse: the clerk is
//! stopped, the stream deleted, the key's directory removed. [`shutdown`]
//! is the clean way; [`Drop`] does the same for a test that panicked, so a
//! failing test leaves neither a clerk process nor a stream behind on the
//! persistent stack. The channels stay — the relay stack is `down -v`ed as
//! a whole, and a channel named after its run says where it came from.
//!
//! [`shutdown`]: Run::shutdown

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use nostr::Event;
use serde_json::Value;
use twalk_test_harness::{ensure_stack, nats_url, poll_until, validate_against_contract, Bus};

use super::clerk::ClerkProc;
use super::relay::{fresh_clerk_key, relay_env, Channels, RelayStack};
use super::run_id;

/// How often the relay is asked again while a test waits for something to
/// appear on it — once a second, deliberately slower than `poll_until`'s
/// half-second: the relay rate-limits each key to 300 requests a minute,
/// every test in a binary reads as the one owner key, and the tests of a
/// binary run in parallel.
pub const RELAY_POLL: Duration = Duration::from_secs(1);

/// How long a test waits for the relay to show something before failing.
/// The bus delivers in milliseconds and the clerk posts in one round trip,
/// so half a minute is a clerk that will never post, not a slow one.
pub const RELAY_WAIT: Duration = Duration::from_secs(30);

/// One run: the stack, the bus, the relay, the channels and the clerk.
pub struct Run {
    /// The run's id: the stream's name, the subject prefix, and what every
    /// channel and temporary directory is named after.
    pub id: String,
    pub bus: Bus,
    pub stack: RelayStack,
    pub channels: Channels,
    /// The clerk under test. Replaced by [`restart_clerk`](Self::restart_clerk).
    pub clerk: ClerkProc,
    /// The clerk's public key: what its posts are signed with.
    pub clerk_pubkey: String,
    /// The environment the clerk was started with, without a listen
    /// address — [`ClerkProc::start`] picks a fresh one each time.
    env: Vec<(String, String)>,
    dir: PathBuf,
    stopped: bool,
}

impl Run {
    /// A run in French, the reference deployment's language.
    pub async fn start(test_name: &str) -> Result<Self> {
        Self::start_in(test_name, "fr").await
    }

    /// A run whose clerk writes in `language` (`fr` or `en`).
    pub async fn start_in(test_name: &str, language: &str) -> Result<Self> {
        ensure_stack().await?;
        let stack = RelayStack::ensure().await?;
        let id = run_id(test_name);
        let bus = Bus::connect().await?;
        bus.ensure_stream(&id, &[&format!("{id}.>")]).await?;

        let dir = std::env::temp_dir().join(&id);
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating {}", dir.display()))?;
        let (key_file, clerk_pubkey, channels) = seed(&stack, &id, &dir).await?;

        let mut env = relay_env(&stack, &key_file, &channels, &id);
        let language_entry = env
            .iter_mut()
            .find(|(name, _)| name == "CLERK_USER_LANGUAGE");
        anyhow::ensure!(
            language_entry.is_some(),
            "relay_env no longer sets CLERK_USER_LANGUAGE, so a run cannot choose its language"
        );
        if let Some((_, value)) = language_entry {
            *value = language.to_owned();
        }
        let clerk = start_clerk(&env).await?;
        Ok(Self {
            id,
            bus,
            stack,
            channels,
            clerk,
            clerk_pubkey,
            env,
            dir,
            stopped: false,
        })
    }

    /// The bus subject of one contract type in this run:
    /// `<id>.persona.suggest.produced.v1` for `persona.suggest.produced`.
    pub fn subject(&self, type_name: &str) -> String {
        format!("{}.{type_name}.v1", self.id)
    }

    /// The subject the Sensor reports a posted reply on (#216):
    /// `<id>.persona.reply.approved.v1.posted`.
    pub fn posted_subject(&self) -> String {
        format!("{}.posted", self.subject("persona.reply.approved"))
    }

    /// Validates `event` against the contract's schema for `type_name` and
    /// publishes it on that type's subject, `Nats-Msg-Id` its own id — the
    /// way a producer does. An event that fails the contract is a test
    /// bug, and never reaches the bus.
    pub async fn publish(&self, type_name: &str, event: &Value) -> Result<()> {
        validate_against_contract(event, type_name)?;
        self.bus
            .publish_event(&self.subject(type_name), event)
            .await
    }

    /// The same event a second time, under a **different** `Nats-Msg-Id`
    /// (`<id>:again`): the bus deduplicates on that header for two minutes,
    /// so a plain republish would never reach a consumer, and what a
    /// redelivery looks like to the clerk is the same CloudEvent id twice.
    pub async fn publish_again(&self, type_name: &str, event: &Value) -> Result<()> {
        validate_against_contract(event, type_name)?;
        self.bus
            .publish_event_with_headers(
                &self.subject(type_name),
                "again",
                async_nats::HeaderMap::new(),
                event,
            )
            .await
    }

    /// The Sensor's report of a posted reply (#216): the approval event
    /// republished unchanged on the `.posted` subject with the `reach` and
    /// `posted-as` headers, under `Nats-Msg-Id` `<id>:posted` as the Sensor
    /// sets it.
    pub async fn publish_posted_report(
        &self,
        approval: &Value,
        reach: &str,
        posted_as: &str,
    ) -> Result<()> {
        self.publish_posted_report_as(approval, reach, posted_as, "posted")
            .await
    }

    /// The same report a second time, under a **different** `Nats-Msg-Id`
    /// (`<id>:posted-again`), for the same reason as
    /// [`publish_again`](Self::publish_again): what a redelivery looks
    /// like to the clerk is the same approval id and headers twice.
    pub async fn publish_posted_report_again(
        &self,
        approval: &Value,
        reach: &str,
        posted_as: &str,
    ) -> Result<()> {
        self.publish_posted_report_as(approval, reach, posted_as, "posted-again")
            .await
    }

    async fn publish_posted_report_as(
        &self,
        approval: &Value,
        reach: &str,
        posted_as: &str,
        msg_id_suffix: &str,
    ) -> Result<()> {
        validate_against_contract(approval, "persona.reply.approved")?;
        let mut headers = async_nats::HeaderMap::new();
        headers.insert("reach", reach);
        headers.insert("posted-as", posted_as);
        self.bus
            .publish_event_with_headers(&self.posted_subject(), msg_id_suffix, headers, approval)
            .await
    }

    /// The forum posts (kind 45001) in `approbations` whose reference line
    /// names the suggestion `id` — the clerk's own way of recognising a
    /// post, read back from outside.
    pub async fn posts_about(&self, id: &str) -> Result<Vec<Event>> {
        let posts = self
            .stack
            .events_in(&self.channels.approvals, &[45001])
            .await?;
        Ok(posts
            .into_iter()
            .filter(|post| reference_names(&post.content, id))
            .collect())
    }

    /// Polls `approbations` until a post about the suggestion `id` is there
    /// and returns it. Fails with what the channel held, because "no post"
    /// and "a post about something else" are different failures.
    pub async fn wait_for_post(&self, id: &str) -> Result<Event> {
        let posts = wait_on_relay(
            || async {
                let posts = self.posts_about(id).await.ok()?;
                (!posts.is_empty()).then_some(posts)
            },
            &format!("a post about suggestion {id} in approbations"),
        )
        .await;
        match posts {
            Ok(mut posts) => Ok(posts.remove(0)),
            Err(error) => {
                let held = self
                    .stack
                    .events_in(&self.channels.approvals, &[45001])
                    .await;
                anyhow::bail!("{error}; approbations held {held:?}")
            }
        }
    }

    /// The stream messages (kind 9) in `channel` — `activite` or `journal`
    /// — newest first.
    pub async fn lines_in(&self, channel: &str) -> Result<Vec<Event>> {
        self.stack.events_in(channel, &[9]).await
    }

    /// Polls `channel` until a stream message containing `needle` is there
    /// and returns it, oldest first among those that match.
    pub async fn wait_for_line(&self, channel: &str, needle: &str) -> Result<Event> {
        let line = wait_on_relay(
            || async {
                self.lines_in(channel)
                    .await
                    .ok()?
                    .into_iter()
                    .rev()
                    .find(|line| line.content.contains(needle))
            },
            &format!("a line containing {needle:?}"),
        )
        .await;
        match line {
            Ok(line) => Ok(line),
            Err(error) => {
                let held = self.lines_in(channel).await;
                anyhow::bail!("{error}; the channel held {held:?}")
            }
        }
    }

    /// Polls `channel` until at least `at_least` stream messages containing
    /// `needle` are there, and returns every one that matches, oldest
    /// first — for a test that then asserts the exact count.
    pub async fn wait_for_lines(
        &self,
        channel: &str,
        needle: &str,
        at_least: usize,
    ) -> Result<Vec<Event>> {
        let lines = wait_on_relay(
            || async {
                let matching = self
                    .lines_in(channel)
                    .await
                    .ok()?
                    .into_iter()
                    .rev()
                    .filter(|line| line.content.contains(needle))
                    .collect::<Vec<_>>();
                (matching.len() >= at_least).then_some(matching)
            },
            &format!("{at_least} lines containing {needle:?}"),
        )
        .await;
        match lines {
            Ok(lines) => Ok(lines),
            Err(error) => {
                let held = self.lines_in(channel).await;
                anyhow::bail!("{error}; the channel held {held:?}")
            }
        }
    }

    /// Asserts that the clerk's `/metrics` carries `sample` as a **whole**
    /// line — `twalk_clerk_posts_total{channel="approbations"} 1` — so an
    /// assertion on a counter cannot match a longer number by prefix.
    ///
    /// Polls (`poll_until`, up to ~20 s), because a counter is bumped
    /// *after* the relay has answered the write it counts: a test that
    /// has just seen the post on the relay can still read the previous
    /// value for a moment, and a suite that fails on that moment is a
    /// flake. For a value ordering already guarantees — a counter that
    /// must still be at zero — [`assert_metric_now`](Self::assert_metric_now)
    /// reads once, since polling for a zero would pass before anything
    /// could have moved it. The failure carries the whole exposition.
    pub async fn assert_metric(&self, sample: &str) -> Result<()> {
        let found = poll_until(
            || async {
                let metrics = self.clerk.metrics().await.ok()?;
                metrics.lines().any(|line| line == sample).then_some(())
            },
            &format!("the metrics line {sample:?}"),
        )
        .await;
        match found {
            Ok(()) => Ok(()),
            Err(error) => {
                let metrics = self.clerk.metrics().await?;
                anyhow::bail!("{error}; /metrics was:\n{metrics}")
            }
        }
    }

    /// One read of `/metrics`, asserted as [`assert_metric`](Self::assert_metric)
    /// does but without polling: for a line whose value is guaranteed by
    /// what the test has already seen, typically a counter still at zero.
    pub async fn assert_metric_now(&self, sample: &str) -> Result<()> {
        let metrics = self.clerk.metrics().await?;
        anyhow::ensure!(
            metrics.lines().any(|line| line == sample),
            "expected the metrics line {sample:?}; /metrics was:\n{metrics}"
        );
        Ok(())
    }

    /// Polls `approbations` until no post about the suggestion `id` is
    /// there any more, for at most `within`. A transient refusal of the
    /// query is asked again, not failed on; the timeout's failure names
    /// what still stands.
    pub async fn wait_until_gone(&self, id: &str, within: Duration) -> Result<()> {
        let gone = wait_on_relay_for(
            || async { self.posts_about(id).await.ok()?.is_empty().then_some(()) },
            &format!("the post about suggestion {id} to be gone from approbations"),
            within,
        )
        .await;
        match gone {
            Ok(()) => Ok(()),
            Err(error) => {
                let standing = self.posts_about(id).await;
                anyhow::bail!("{error}; still standing: {standing:?}")
            }
        }
    }

    /// Stops the clerk with SIGTERM and starts a fresh process on the same
    /// key, channels, stream and subject prefix — a warm restart, as an
    /// operator's process manager does one — and waits for `clerk running`.
    pub async fn restart_clerk(&mut self) -> Result<()> {
        let status = self.clerk.stop().await?;
        anyhow::ensure!(status.success(), "the clerk did not exit cleanly: {status}");
        self.clerk = start_clerk(&self.env).await?;
        Ok(())
    }

    /// Stops the clerk and gives back everything this run claimed on the
    /// shared stacks.
    pub async fn shutdown(mut self) -> Result<()> {
        let status = self.clerk.stop().await?;
        anyhow::ensure!(status.success(), "the clerk did not exit cleanly: {status}");
        // Only now: a stop that failed above returns early, and `Drop` must
        // still terminate the process and delete the stream.
        self.stopped = true;
        self.bus.delete_stream(&self.id).await?;
        tokio::fs::remove_dir_all(&self.dir)
            .await
            .with_context(|| format!("removing {}", self.dir.display()))?;
        Ok(())
    }
}

impl Drop for Run {
    /// A test that panicked never reached `shutdown`. The clerk is
    /// terminated first — a field is dropped after this body runs, and the
    /// stream must not go while a process still consumes it — then the
    /// stream is deleted from a thread of its own, because a test's runtime
    /// cannot be blocked on from inside it, and a stream left behind is
    /// one the next run's clerk would find consumers on.
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        self.clerk.terminate();
        let stream = self.id.clone();
        let url = nats_url();
        let deleted = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            runtime.block_on(async {
                let client =
                    tokio::time::timeout(Duration::from_secs(5), async_nats::connect(&url))
                        .await
                        .ok()?
                        .ok()?;
                let jetstream = async_nats::jetstream::new(client);
                tokio::time::timeout(Duration::from_secs(5), jetstream.delete_stream(&stream))
                    .await
                    .ok()?
                    .ok()
            })
        })
        .join();
        if !matches!(deleted, Ok(Some(_))) {
            eprintln!("the run's stream {} could not be deleted on drop", self.id);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A fresh clerk key on the relay and the three channels it will write to,
/// named after the run so a leftover on the persistent stack says where it
/// came from. Returns the key file, the public key and the channels.
pub async fn seed(
    stack: &RelayStack,
    run: &str,
    dir: &Path,
) -> Result<(PathBuf, String, Channels)> {
    let (key_file, clerk_pubkey) = fresh_clerk_key(dir).await?;
    stack.add_member(&clerk_pubkey).await?;
    let channels = Channels {
        approvals: stack
            .create_channel(&format!("approbations-{run}"), "forum", &clerk_pubkey)
            .await?,
        activity: stack
            .create_channel(&format!("activite-{run}"), "stream", &clerk_pubkey)
            .await?,
        journal: stack
            .create_channel(&format!("journal-{run}"), "stream", &clerk_pubkey)
            .await?,
    };
    Ok((key_file, clerk_pubkey, channels))
}

/// Starts the clerk on `env` and waits for `clerk running`.
async fn start_clerk(env: &[(String, String)]) -> Result<ClerkProc> {
    let clerk = ClerkProc::start(env.to_vec()).await?;
    clerk.wait_for_log("clerk running").await?;
    Ok(clerk)
}

/// Whether the last reference line of `content` names the suggestion
/// `id`: the clerk's own rule (`reference::parse`), spelled again here so
/// that a suite reading the relay back does not borrow the component's
/// reading of it.
pub fn reference_names(content: &str, id: &str) -> bool {
    content
        .lines()
        .rev()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("twalk:suggestion:"))
        .and_then(|rest| rest.split_whitespace().next())
        == Some(id)
}

/// Whether a stream message carries the tag the clerk writes for the bus
/// event `id` — `["r", "twalk:event:<id>"]` — spelled again here for the
/// same reason as [`reference_names`]: the suite reads the relay back on
/// its own terms.
pub fn line_references_event(line: &Event, id: &str) -> bool {
    let wanted = format!("twalk:event:{id}");
    line.tags
        .iter()
        .map(|tag| tag.as_slice())
        .any(|tag| tag.len() >= 2 && tag[0] == "r" && tag[1] == wanted)
}

/// Polls `attempt` every [`RELAY_POLL`] until it yields `Some`, for at
/// most [`RELAY_WAIT`]. A query the relay refused (its rate limit, a
/// moment of unavailability) is a `None` here, not a failure: what the
/// test asserts is what the relay holds, and it is asked again.
pub async fn wait_on_relay<T, Fut>(mut attempt: impl FnMut() -> Fut, description: &str) -> Result<T>
where
    Fut: std::future::Future<Output = Option<T>>,
{
    wait_on_relay_for(&mut attempt, description, RELAY_WAIT).await
}

/// [`wait_on_relay`] with a bound of the test's own: for a fact that must
/// hold by a given time — an expired post gone within so many sweeps.
/// Each attempt is bounded by what is left of `within`, so a relay that
/// hangs on one query fails the test at its deadline with its diagnosis
/// rather than hanging it for ever.
pub async fn wait_on_relay_for<T, Fut>(
    mut attempt: impl FnMut() -> Fut,
    description: &str,
    within: Duration,
) -> Result<T>
where
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if let Ok(Some(value)) = tokio::time::timeout_at(deadline, attempt()).await {
            return Ok(value);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out after {}s waiting for {description}",
                within.as_secs()
            );
        }
        tokio::time::sleep(RELAY_POLL).await;
    }
}
