//! A real Buzz relay, and the signed client that seeds and reads it.
//!
//! The relay is the seam's other side (ticket #265): the clerk's whole job
//! is to write onto it what the bus says, under a key of its own, so its
//! suite runs against a real one — `clerk/tests/compose.relay.yaml`, brought
//! up here the way the shared stack is (`up -d --wait` under a `OnceCell`).
//!
//! The client in this file is a **test-side twin** of the clerk's own
//! (`clerk/src/relay.rs`), deliberately not shared with it: it signs as the
//! relay's **owner**, and a suite that borrowed the component's client to
//! read back what the component wrote would agree with it whatever it
//! became. What the two have in common is the relay's own contract, spelled
//! out once more here so a change on either side fails here:
//!
//! - every request is a NIP-98 event (kind 27235) carrying the full URL,
//!   the method, a nonce and the SHA-256 of the body, base64-encoded in
//!   `Authorization: Nostr <…>` — and the relay binds the URL to the `Host`
//!   it was reached on, so the URL signed is `http://127.0.0.1:<port>/…`
//!   and never `localhost`, which would be a tenant that does not exist;
//! - `POST /events` takes one signed event and answers
//!   `{"event_id","accepted","message"}`; `accepted: false` is a refusal
//!   with a reason, not a transport failure;
//! - `POST /query` takes a JSON array of NIP-01 filters and answers the
//!   events, newest first, at most 1000 (`limitation.max_limit`);
//! - a relay member is added by kind 9030 (`["p", hex]`, `["role", …]`),
//!   signed by an owner or admin — a NIP-43 command the relay executes and
//!   never stores; a channel is created by kind 9007 (`["h", uuid]`,
//!   `["name", …]`, `["visibility", …]`, `["channel_type", …]`), and a
//!   member is put into it by kind 9000 (`["h", uuid]`, `["p", hex]`,
//!   `["role", …]`), NIP-29's put-user — exactly what `buzz-sdk`'s
//!   `build_add_member` builds;
//! - an event is accepted only from the key that signed the request
//!   (`event pubkey does not match authenticated identity` otherwise), so
//!   the owner's gestures go up as the owner and a stranger's as the
//!   stranger ([`RelayStack::submit_as`]); a reaction (kind 7) carries one
//!   `["e", post]` tag and no `h`, a thread reply (kind 45003) carries
//!   `["h", channel]` and `["e", post, "", "reply"]` — `buzz-sdk`'s
//!   `build_reaction` and `build_forum_comment` ([`reaction`],
//!   [`thread_reply`]) — and the same signed event a second time is a
//!   duplicate, not a second event ([`RelayStack::submit_again_as`]).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use nostr::{Event, EventBuilder, JsonUtil, Keys, Kind, Tag};
use serde_json::{json, Value};
use tokio::process::Command;
use twalk_test_harness::{nats_url, poll_until, sha256_hex};

/// The relay owner's secret key: the secp256k1 scalar **1**, whose public
/// key is the curve's generator point,
/// `79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798` —
/// the `RELAY_OWNER_PUBKEY` the compose file boots the relay with.
///
/// Test-only, on a relay that listens on loopback and is torn down with
/// `down -v`; never reuse it anywhere.
pub const TEST_OWNER_SECRET_HEX: &str =
    "0000000000000000000000000000000000000000000000000000000000000001";

/// The public key of [`TEST_OWNER_SECRET_HEX`], as the compose file has it.
pub const TEST_OWNER_PUBKEY_HEX: &str =
    "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

/// How long one request to the relay may take before the test gives up on
/// it and asks again (or fails with its diagnosis): a relay on loopback
/// answers in milliseconds, and a request still open after ten seconds is
/// a hung relay, which must fail a test rather than hang it for ever.
pub const RELAY_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The sweep interval every clerk under test runs with
/// (`CLERK_SWEEP_SECONDS` in [`relay_env`]): two seconds, so a test can
/// watch an expired post go and derive its own bound from this number.
pub const SWEEP_SECONDS: u64 = 2;

/// A reqwest client with [`RELAY_REQUEST_TIMEOUT`] on every request.
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(RELAY_REQUEST_TIMEOUT)
        .build()
        .expect("a reqwest client with a timeout builds")
}

/// Every kind the clerk writes or could write, for a search that must find
/// **nothing** of a contact on any channel (`buzz-core`'s names): a forum
/// post (45001), a stream message (9), a forum comment (45003), a reaction
/// (7) and a stream message edit (40003).
pub const CLERK_KINDS: [u16; 5] = [9, 45001, 45003, 7, 40003];

/// The three channels a clerk is configured with, by UUID — the same shape
/// `deploy/docker-compose/provision-buzz-channels.sh` prints.
#[derive(Debug, Clone)]
pub struct Channels {
    pub approvals: String,
    pub activity: String,
    pub journal: String,
}

/// The port the relay is published on: `TWALK_CLERK_TEST_RELAY_PORT`
/// moves it aside for a parallel worktree, exactly as `TWALK_TEST_*` move
/// the shared stack.
pub fn relay_port() -> u16 {
    std::env::var("TWALK_CLERK_TEST_RELAY_PORT")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(17800)
}

/// The URL the relay announces (`RELAY_URL` in the compose file): the one
/// NIP-98 binds every signed request to, and therefore the one the clerk
/// under test is handed as `CLERK_RELAY_URL`.
pub fn relay_url() -> String {
    format!("http://127.0.0.1:{}", relay_port())
}

/// The compose project the relay runs under.
fn stack_id() -> String {
    std::env::var("TWALK_CLERK_TEST_STACK").unwrap_or_else(|_| "twalk-clerk-test".to_owned())
}

fn compose_file() -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("compose.relay.yaml")
        .to_string_lossy()
        .into_owned()
}

/// Brings the relay stack up (idempotent) and waits for its NIP-11
/// document. Serialised through a `OnceCell`, so the first test of a
/// binary does the work and the others wait rather than racing `docker
/// compose up` on a cold volume.
async fn ensure_relay() -> Result<()> {
    static RELAY: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    RELAY.get_or_try_init(do_ensure_relay).await?;
    Ok(())
}

async fn do_ensure_relay() -> Result<()> {
    let status = Command::new("docker")
        .args([
            "compose",
            "-p",
            &stack_id(),
            "-f",
            &compose_file(),
            "up",
            "-d",
            "--wait",
        ])
        .env("TWALK_CLERK_TEST_STACK", stack_id())
        .env("TWALK_CLERK_TEST_RELAY_PORT", relay_port().to_string())
        .stdout(Stdio::null())
        .status()
        .await
        .context("failed to run docker compose up for the relay")?;
    if !status.success() {
        bail!("docker compose up for the relay failed with {status}");
    }
    // Healthy is the health listener answering; the public listener is
    // what the tests speak to, so wait for its NIP-11 document.
    let http = http_client();
    let url = relay_url();
    poll_until(
        || async {
            http.get(format!("{url}/"))
                .header("Accept", "application/nostr+json")
                .send()
                .await
                .ok()?
                .json::<Value>()
                .await
                .ok()
        },
        "the relay's NIP-11 document",
    )
    .await?;
    Ok(())
}

/// The real Buzz relay, spoken to as its owner.
pub struct RelayStack {
    /// The URL the relay announces, `http://127.0.0.1:<port>`.
    pub url: String,
    /// The owner's keys ([`TEST_OWNER_SECRET_HEX`]): what seeds the relay
    /// and reads it back.
    pub owner: Keys,
    project: String,
    http: reqwest::Client,
}

impl RelayStack {
    /// The relay, up and answering.
    pub async fn ensure() -> Result<Self> {
        ensure_relay().await?;
        Ok(Self {
            url: relay_url(),
            owner: Keys::parse(TEST_OWNER_SECRET_HEX).context("the owner's test key")?,
            project: stack_id(),
            http: http_client(),
        })
    }

    /// The compose project the relay runs under, for a test that wants to
    /// name it in a failure.
    pub fn project(&self) -> &str {
        &self.project
    }

    /// The relay's NIP-11 document.
    pub async fn nip11(&self) -> Result<Value> {
        self.http
            .get(format!("{}/", self.url))
            .header("Accept", "application/nostr+json")
            .send()
            .await
            .context("GET / for the NIP-11 document")?
            .json()
            .await
            .context("the NIP-11 document is JSON")
    }

    /// One signed `POST`, as the owner: the NIP-98 header over exactly this
    /// URL and this body. A non-2xx answer is a failure naming the status
    /// and the body, because "unauthorized" and "no such tenant" are the two
    /// mistakes this client can make and both are in the body.
    async fn signed_post(&self, path: &str, body: String) -> Result<Value> {
        self.signed_post_as(&self.owner, path, body).await
    }

    /// [`signed_post`](Self::signed_post) as `keys` rather than the owner:
    /// the relay refuses an event whose author is not the key that signed
    /// the request (`event pubkey does not match authenticated identity`),
    /// so a stranger's reaction must be posted as the stranger.
    async fn signed_post_as(&self, keys: &Keys, path: &str, body: String) -> Result<Value> {
        let url = format!("{}{path}", self.url);
        let response = self
            .http
            .post(&url)
            .header("Authorization", authorization(keys, &url, "POST", &body)?)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("POST {url} answered {status}: {text}");
        }
        serde_json::from_str(&text).with_context(|| format!("POST {url} answered non-JSON: {text}"))
    }

    /// Submits one signed event and requires the relay to accept it: a
    /// refusal is a failure carrying the relay's own reason.
    pub async fn submit(&self, event: &Event) -> Result<()> {
        self.submit_as(&self.owner, event).await
    }

    /// [`submit`](Self::submit) with the request signed by `keys`, which
    /// must be the event's own author (a relay member): how a stranger's
    /// gesture reaches the relay.
    pub async fn submit_as(&self, keys: &Keys, event: &Event) -> Result<()> {
        let answer = self
            .signed_post_as(keys, "/events", event.as_json())
            .await?;
        if answer["accepted"].as_bool() != Some(true) {
            bail!(
                "the relay refused a kind {} event: {answer}",
                event.kind.as_u16()
            );
        }
        Ok(())
    }

    /// Submits an event the relay already holds, as `keys`, and requires the
    /// relay to call it a **duplicate** — what a client that retried a
    /// request looks like from the clerk's side: the same signed event, the
    /// same id, once on the relay. A duplicate is `accepted: true,
    /// message: "duplicate:"` for a comment and `accepted: false, message:
    /// "duplicate: reaction already exists"` for a reaction (the relay's
    /// `ingest.rs`), so both are read as the duplicate they are; an event the
    /// relay did **not** already hold is a failure, because the test meant
    /// to resubmit and did not. Returns the relay's message.
    pub async fn submit_again_as(&self, keys: &Keys, event: &Event) -> Result<String> {
        let answer = self
            .signed_post_as(keys, "/events", event.as_json())
            .await?;
        let message = answer["message"].as_str().unwrap_or_default().to_owned();
        if !message.starts_with("duplicate:") {
            bail!(
                "the relay did not call the resubmitted kind {} event a duplicate: {answer}",
                event.kind.as_u16()
            );
        }
        Ok(message)
    }

    /// Adds `pubkey_hex` as a member of the relay (kind 9030, NIP-43),
    /// which is what lets it write at all: the relay requires membership
    /// and the clerk's key is nobody until this.
    pub async fn add_member(&self, pubkey_hex: &str) -> Result<()> {
        let event = EventBuilder::new(Kind::Custom(9030), "")
            .tags([tag(["p", pubkey_hex])?, tag(["role", "member"])?])
            .sign_with_keys(&self.owner)
            .context("signing the add-member command")?;
        self.submit(&event).await
    }

    /// Creates a private channel of `kind` (`forum` or `stream`) named
    /// `name` (kind 9007, with a fresh UUID as its `h`), then puts `member`
    /// into it (kind 9000, NIP-29's put-user, the way `buzz-sdk`'s
    /// `build_add_member` does). Returns the channel's UUID — what the
    /// clerk is configured with.
    pub async fn create_channel(&self, name: &str, kind: &str, member: &str) -> Result<String> {
        let channel = uuid::Uuid::new_v4().to_string();
        let create = EventBuilder::new(Kind::Custom(9007), "")
            .tags([
                tag(["h", &channel])?,
                tag(["name", name])?,
                tag(["visibility", "private"])?,
                tag(["channel_type", kind])?,
            ])
            .sign_with_keys(&self.owner)
            .context("signing the create-channel event")?;
        self.submit(&create).await?;
        self.add_to_channel(&channel, member).await?;
        Ok(channel)
    }

    /// Puts `pubkey_hex` — already a relay member — into `channel` (kind
    /// 9000, NIP-29's put-user, as `buzz-sdk`'s `build_add_member` builds
    /// it), signed by the owner: what lets a key read the channel and
    /// react in it.
    pub async fn add_to_channel(&self, channel: &str, pubkey_hex: &str) -> Result<()> {
        let put_user = EventBuilder::new(Kind::Custom(9000), "")
            .tags([
                tag(["h", channel])?,
                tag(["p", pubkey_hex])?,
                tag(["role", "member"])?,
            ])
            .sign_with_keys(&self.owner)
            .context("signing the put-user event")?;
        self.submit(&put_user).await
    }

    /// A fresh key that is a member of the relay and of `channel`, and
    /// nothing else — not the owner, not the clerk: a **stranger** whose
    /// gesture on a post must decide nothing.
    pub async fn stranger_in(&self, channel: &str) -> Result<Keys> {
        let keys = Keys::generate();
        let pubkey = keys.public_key().to_hex();
        self.add_member(&pubkey).await?;
        self.add_to_channel(channel, &pubkey).await?;
        Ok(keys)
    }

    /// Every event of one of `kinds` in `channel`, newest first, as the
    /// owner reads them (the relay's cap of 1000, which no test approaches).
    pub async fn events_in(&self, channel: &str, kinds: &[u16]) -> Result<Vec<Event>> {
        self.query(json!([{ "kinds": kinds, "#h": [channel], "limit": 1000 }]))
            .await
    }

    /// One `POST /query` as the owner with `filters` — a JSON array of
    /// NIP-01 filters — answered as events, newest first.
    pub async fn query(&self, filters: Value) -> Result<Vec<Event>> {
        let answer = self.signed_post("/query", filters.to_string()).await?;
        serde_json::from_value(answer).context("the query answered something other than events")
    }

    /// Everything the clerk could have written in `channel`
    /// ([`CLERK_KINDS`]): for a search that must find nothing of a contact.
    pub async fn all_events_in(&self, channel: &str) -> Result<Vec<Event>> {
        self.events_in(channel, &CLERK_KINDS).await
    }
}

/// A reaction (kind 7) on the post `post_id`, signed by `keys`: content
/// the emoji, one `["e", post_id]` tag and nothing else — the shape
/// `buzz-sdk`'s `build_reaction` writes and a Buzz client sends when the
/// owner taps ✅ under a post. No `h` tag: the relay derives the channel
/// from the target.
pub fn reaction(keys: &Keys, post_id: &str, emoji: &str) -> Result<Event> {
    EventBuilder::new(Kind::Custom(7), emoji)
        .tags([tag(["e", post_id])?])
        .sign_with_keys(keys)
        .context("signing the reaction")
}

/// A **direct** reply (kind 45003) in the thread of the post `post_id` in
/// `channel`, signed by `keys`: `["h", channel]` and the one
/// `["e", post_id, "", "reply"]` tag `buzz-sdk`'s `build_forum_comment`
/// writes when root and parent are the post itself — the owner answering
/// the post with the text to send, or a stranger commenting on it.
pub fn thread_reply(keys: &Keys, channel: &str, post_id: &str, text: &str) -> Result<Event> {
    EventBuilder::new(Kind::Custom(45003), text)
        .tags([tag(["h", channel])?, tag(["e", post_id, "", "reply"])?])
        .sign_with_keys(keys)
        .context("signing the thread reply")
}

/// The `Authorization` header of one NIP-98 request: a kind 27235 event
/// over the full URL, the method, a nonce (so two identical requests in
/// the same second are two events, not a replay) and the body's SHA-256,
/// signed by `keys` and base64-encoded.
pub fn authorization(keys: &Keys, url: &str, method: &str, body: &str) -> Result<String> {
    let event = EventBuilder::new(Kind::HttpAuth, "")
        .tags([
            tag(["u", url])?,
            tag(["method", method])?,
            tag(["nonce", &uuid::Uuid::new_v4().to_string()])?,
            tag(["payload", &sha256_hex(body)])?,
        ])
        .sign_with_keys(keys)
        .context("signing the NIP-98 event")?;
    // Standard alphabet, padded: what NIP-98 names.
    Ok(format!("Nostr {}", BASE64.encode(event.as_json())))
}

fn tag<const N: usize>(values: [&str; N]) -> Result<Tag> {
    Tag::parse(values).with_context(|| format!("building the {} tag", values[0]))
}

/// Generates a key for a clerk under test and writes its secret, as one
/// hex line, to `<dir>/clerk.key` with mode 0600 — the shape
/// `deploy/docker-compose/provision-nostr-key.sh` writes and
/// `CLERK_NOSTR_KEY_FILE` names. Returns the path and the public key.
///
/// The file is created with its final mode (`create_new` and `mode(0600)`
/// in one `open`), never written at the umask's default and tightened
/// afterwards: a secret that was world-readable for a moment was
/// world-readable. And `create_new` means a leftover from another run is
/// a failure rather than a key silently overwritten.
pub async fn fresh_clerk_key(dir: &Path) -> Result<(PathBuf, String)> {
    use std::os::unix::fs::OpenOptionsExt;
    use tokio::io::AsyncWriteExt;

    let keys = Keys::generate();
    let path = dir.join("clerk.key");
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .await
        .with_context(|| format!("creating {} with mode 0600", path.display()))?;
    file.write_all(format!("{}\n", keys.secret_key().to_secret_hex()).as_bytes())
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    file.flush().await?;
    Ok((path, keys.public_key().to_hex()))
}

/// The whole `CLERK_*` environment of a clerk under test, except its
/// listen address, which [`super::ClerkProc::start`] picks: the shared
/// stack's bus, this run's own stream and subject prefix (`run`, used as
/// both — the Hermes suite's shape), the relay and the three channels.
///
/// French, because that is the reference deployment's language and the
/// one the clerk's sentences are written in first; a test that wants
/// another sets `CLERK_USER_LANGUAGE` over this. The sweep is two seconds
/// so a test can watch an expired post go.
pub fn relay_env(
    stack: &RelayStack,
    key_file: &Path,
    channels: &Channels,
    run: &str,
) -> Vec<(String, String)> {
    [
        ("CLERK_NATS_URL", nats_url()),
        ("CLERK_STREAM", run.to_owned()),
        ("CLERK_SUBJECT_PREFIX", run.to_owned()),
        ("CLERK_RELAY_URL", stack.url.clone()),
        (
            "CLERK_NOSTR_KEY_FILE",
            key_file.to_string_lossy().into_owned(),
        ),
        ("CLERK_CHANNEL_APPROVALS", channels.approvals.clone()),
        ("CLERK_CHANNEL_ACTIVITY", channels.activity.clone()),
        ("CLERK_CHANNEL_JOURNAL", channels.journal.clone()),
        ("CLERK_USER_LANGUAGE", "fr".to_owned()),
        ("CLERK_SWEEP_SECONDS", SWEEP_SECONDS.to_string()),
        ("CLERK_LOG_LEVEL", "info".to_owned()),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value))
    .collect()
}
