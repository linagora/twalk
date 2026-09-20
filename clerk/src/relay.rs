//! The relay: the clerk's one voice, a Buzz relay's two HTTP routes, spoken
//! with the clerk's own Nostr key and nothing else (ticket #265, ADR 0035).
//!
//! A Buzz relay is a Nostr relay with an HTTP front: `POST /events` takes one
//! signed event and `POST /query` takes a list of filters, and each request is
//! authenticated by NIP-98 — a throwaway kind-27235 event that names the exact
//! URL hit, the method and the SHA-256 of the body, signed by the same key the
//! posts are. The relay is multi-tenant by host and keeps a replay guard on
//! that event's id, which is why the URL is the one the relay **announces**
//! ([`crate::config::Config::relay_url`]) and why every request signs a fresh
//! event with a nonce of its own: two posts with one body are still two
//! requests. The recipe is Buzz's own CLI's (`buzz-cli/src/client.rs`),
//! mirrored rather than reinvented, because a header the relay's own tooling
//! builds is one the relay is known to accept.
//!
//! Three things this module decides and three it deliberately leaves alone.
//! It **decides** what crosses the wire: the event's kind and tags as the
//! caller gave them, the signature, and the header. It decides how a failure
//! is **named**: nothing answered ([`RelayError::Unreachable`]), the relay
//! answered and said no ([`RelayError::Refused`], with the status and the body
//! it sent), or the relay answered something that is not a relay's answer
//! ([`RelayError::Malformed`]) — and which of those a caller may sensibly try
//! again ([`RelayError::is_transient`]: a relay that could not be reached, a
//! `429` and a `5xx`, never a `4xx` that says the request itself was wrong).
//! And it decides that the clerk's **key file is nobody else's to read**:
//! [`load_keys`] refuses a file another account on the host could open, naming
//! the `chmod` that fixes it, because the key signs everything the clerk says.
//!
//! It **does not retry**: a caller knows whether a post is worth a second try
//! and a second later, and this module does not — a retry loop here would turn
//! one rate limit into a burst. It **holds no memory** of what it published:
//! the relay is the clerk's memory (ADR 0035), read back through
//! [`Relay::own_posts`]. And it **knows no kinds beyond the four the clerk
//! uses** — a forum post, a stream message, a delete and its own posts back —
//! so a fifth would be a decision made here, in the open, rather than a tag
//! list assembled somewhere else.

use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use nostr::{Event, EventBuilder, JsonUtil, Keys, Kind, Tag};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

/// NIP-98's own kind: an HTTP authorisation event, never stored by a relay.
pub const NIP98_KIND: u16 = 27235;
/// A Buzz forum post, the shape one suggestion becomes in `approbations`.
pub const KIND_FORUM_POST: u16 = 45001;
/// A Buzz stream message, one line in a channel's running feed.
pub const KIND_STREAM_MESSAGE: u16 = 9;
/// A Buzz deletion of one of one's own events in a channel.
pub const KIND_DELETE: u16 = 9005;

/// How long one request may take, end to end. The relay is on the same host
/// or the same private network as the clerk on the reference deployment, and
/// NIP-98 gives a request sixty seconds of validity either way, so a request
/// still in flight after ten is one that will not succeed.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// How much of a refusal's body a log line shows. The relay's own error
/// bodies are one short JSON object; anything longer is a proxy's HTML page,
/// which nobody reads in a log.
const LOGGED_BODY_CHARS: usize = 300;

/// The variable `deploy/docker-compose/provision-nostr-key.sh` writes the
/// key under, in the env-style file Hermes reads the same key from.
pub const KEY_FILE_VARIABLE: &str = "BUZZ_PRIVATE_KEY";

/// What the relay answered to one accepted `POST /events`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Published {
    /// The event's id, hex, as the relay computed it — the same id the event
    /// carried, which is what a later delete or a later query names.
    pub event_id: String,
    /// Whether the relay took the event. A `200` with `accepted: false` is the
    /// relay's way of saying "seen before" (`duplicate:` in `message`), which
    /// is not a refusal: the post is there.
    pub accepted: bool,
    #[serde(default)]
    pub message: String,
}

/// Why one request to the relay did not produce what was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayError {
    /// Nothing answered: connection refused, DNS, TLS, or the request timed
    /// out. The relay may be down or may be starting.
    Unreachable(String),
    /// The relay answered with a status outside `2xx`. `body` is the whole
    /// body it sent, because a `429`'s `retry in Ns` is in there and a caller
    /// may want it; the log line and [`fmt::Display`] cut it short.
    Refused { status: u16, body: String },
    /// Either this side could not build the request (a tag the `nostr` crate
    /// would not parse, a signature that failed) or the relay answered `2xx`
    /// with something that is not the answer this route gives.
    Malformed(String),
}

impl RelayError {
    /// Whether the same request, made again later, could succeed: the relay
    /// could not be reached, or it answered `429` or `5xx`. A `4xx` other than
    /// `429` is the relay's verdict on the request itself — a bad signature, a
    /// replayed nonce, a channel the clerk is not a member of — and asking
    /// again would only be refused again.
    pub fn is_transient(&self) -> bool {
        match self {
            RelayError::Unreachable(_) => true,
            RelayError::Refused { status, .. } => *status == 429 || *status >= 500,
            RelayError::Malformed(_) => false,
        }
    }
}

impl fmt::Display for RelayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RelayError::Unreachable(why) => write!(f, "the relay could not be reached: {why}"),
            RelayError::Refused { status, body } => {
                write!(
                    f,
                    "the relay refused the request with HTTP {status}: {}",
                    cut(body)
                )
            }
            RelayError::Malformed(why) => write!(f, "the relay's answer could not be used: {why}"),
        }
    }
}

impl std::error::Error for RelayError {}

/// The first [`LOGGED_BODY_CHARS`] characters of a body, marked when cut.
fn cut(body: &str) -> String {
    let body = body.trim();
    if body.chars().count() <= LOGGED_BODY_CHARS {
        return body.to_owned();
    }
    let shown: String = body.chars().take(LOGGED_BODY_CHARS).collect();
    format!("{shown}… [cut]")
}

/// A Buzz relay, reached over HTTP under the clerk's own key.
pub struct Relay {
    /// The URL the relay announces, with no trailing slash, so `{base}/events`
    /// is exactly the `u` tag the relay compares against.
    base: String,
    keys: Keys,
    http: reqwest::Client,
}

impl fmt::Debug for Relay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Relay")
            .field("base", &self.base)
            .field("public_key", &self.public_key_hex())
            .finish_non_exhaustive()
    }
}

impl Relay {
    /// `base` is the URL the relay announces; a trailing slash is dropped so
    /// the route paths below compose. The HTTP client is built once, with
    /// [`REQUEST_TIMEOUT`] on every request.
    pub fn new(base: &str, keys: Keys) -> Result<Self> {
        let base = base.trim_end_matches('/').to_owned();
        if base.is_empty() {
            bail!("the relay URL is empty");
        }
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("building the HTTP client for the relay")?;
        Ok(Self { base, keys, http })
    }

    /// The announced URL as stored: no trailing slash.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The clerk's own public key, hex — the `authors` of its own posts.
    pub fn public_key_hex(&self) -> String {
        self.keys.public_key().to_hex()
    }

    /// Signs and publishes one event. `kind` is the raw kind number and
    /// `tags` are the tag vectors as Nostr writes them (`["h", channel]`).
    pub async fn publish(
        &self,
        kind: u16,
        tags: Vec<Vec<String>>,
        content: &str,
    ) -> Result<Published, RelayError> {
        let tags = tags
            .into_iter()
            .map(|tag| {
                Tag::parse(tag.iter().map(String::as_str)).map_err(|e| {
                    RelayError::Malformed(format!("tag {tag:?} is not a Nostr tag: {e}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let event = EventBuilder::new(Kind::Custom(kind), content)
            .tags(tags)
            .sign_with_keys(&self.keys)
            .map_err(|e| RelayError::Malformed(format!("signing a kind {kind} event: {e}")))?;
        let body = event.as_json().into_bytes();
        let published: Published = self.post("/events", body).await?;
        debug!(
            kind,
            event_id = %published.event_id,
            accepted = published.accepted,
            message = %published.message,
            "published to the relay"
        );
        Ok(published)
    }

    /// `POST /query` with one or more Nostr filters, as JSON. The relay
    /// answers the matching events newest first, at most the `limit` each
    /// filter names (and never more than 1000).
    pub async fn query(&self, filters: Vec<Value>) -> Result<Vec<Event>, RelayError> {
        let body = serde_json::to_vec(&filters)
            .map_err(|e| RelayError::Malformed(format!("serialising the filters: {e}")))?;
        self.post("/query", body).await
    }

    /// A forum post (kind 45001) in `channel`: what one suggestion becomes.
    pub async fn forum_post(&self, channel: &str, content: &str) -> Result<Published, RelayError> {
        self.publish(KIND_FORUM_POST, vec![tag("h", channel)], content)
            .await
    }

    /// A stream message (kind 9) in `channel`: one line in a running feed.
    pub async fn stream_message(
        &self,
        channel: &str,
        content: &str,
    ) -> Result<Published, RelayError> {
        self.publish(KIND_STREAM_MESSAGE, vec![tag("h", channel)], content)
            .await
    }

    /// A delete (kind 9005) of `event_id`, one of the clerk's own events in
    /// `channel`. The relay refuses a delete of somebody else's.
    pub async fn delete(&self, channel: &str, event_id: &str) -> Result<Published, RelayError> {
        self.publish(KIND_DELETE, vec![tag("h", channel), tag("e", event_id)], "")
            .await
    }

    /// The clerk's own events of `kind` in `channel`, newest first, at most
    /// `limit`: the relay as the clerk's memory (ADR 0035).
    pub async fn own_posts(
        &self,
        channel: &str,
        kind: u16,
        limit: u32,
    ) -> Result<Vec<Event>, RelayError> {
        let filter = own_posts_filter(&self.public_key_hex(), channel, kind, limit);
        self.query(vec![filter]).await
    }

    /// One authenticated `POST` to `path`, the body signed into a fresh
    /// NIP-98 header, the `2xx` answer read as `T`.
    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: Vec<u8>,
    ) -> Result<T, RelayError> {
        let url = format!("{}{path}", self.base);
        let authorization = nip98_authorization(&self.keys, &url, &body)?;
        let response = self
            .http
            .post(&url)
            .header(reqwest::header::AUTHORIZATION, authorization)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| RelayError::Unreachable(e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| RelayError::Unreachable(format!("reading the answer to {url}: {e}")))?;
        if !status.is_success() {
            warn!(
                url = %url,
                status = status.as_u16(),
                body = %cut(&text),
                "the relay refused the request"
            );
            return Err(RelayError::Refused {
                status: status.as_u16(),
                body: text,
            });
        }
        serde_json::from_str(&text).map_err(|e| {
            RelayError::Malformed(format!(
                "HTTP {} from {url} is not the answer that route gives ({e}): {}",
                status.as_u16(),
                cut(&text)
            ))
        })
    }
}

fn tag(name: &str, value: &str) -> Vec<String> {
    vec![name.to_owned(), value.to_owned()]
}

/// The filter that finds the clerk's own events of one kind in one channel:
/// `{"kinds":[kind],"#h":[channel],"authors":[pubkey],"limit":limit}`.
pub fn own_posts_filter(pubkey_hex: &str, channel: &str, kind: u16, limit: u32) -> Value {
    serde_json::json!({
        "kinds": [kind],
        "#h": [channel],
        "authors": [pubkey_hex],
        "limit": limit,
    })
}

/// The `Authorization` header value for one `POST` of `body` to `url`:
/// [`nip98_event`], serialised, base64 (standard, padded) after the `Nostr`
/// scheme.
fn nip98_authorization(keys: &Keys, url: &str, body: &[u8]) -> Result<String, RelayError> {
    let event = nip98_event(keys, url, body)?;
    Ok(format!("Nostr {}", B64.encode(event.as_json().as_bytes())))
}

/// A fresh kind-27235 event for one `POST` of `body` to `url`, exactly as
/// Buzz's own CLI builds it: `u` the full URL, `method`, a `nonce` so two
/// requests with one body are two events for the relay's replay guard,
/// `payload` the hex SHA-256 of the body — signed with `keys`.
fn nip98_event(keys: &Keys, url: &str, body: &[u8]) -> Result<Event, RelayError> {
    let payload = format!("{:x}", Sha256::digest(body));
    let nonce = uuid::Uuid::new_v4().to_string();
    let tags = [
        ["u", url],
        ["method", "POST"],
        ["nonce", nonce.as_str()],
        ["payload", payload.as_str()],
    ]
    .into_iter()
    .map(|tag| {
        Tag::parse(tag).map_err(|e| RelayError::Malformed(format!("NIP-98 tag {tag:?}: {e}")))
    })
    .collect::<Result<Vec<_>, _>>()?;
    EventBuilder::new(Kind::Custom(NIP98_KIND), "")
        .tags(tags)
        .sign_with_keys(keys)
        .map_err(|e| RelayError::Malformed(format!("signing the NIP-98 event: {e}")))
}

/// Reads the clerk's key from `path`, in either of the two shapes an operator
/// has: a bare key on the first line that is neither empty nor a `#` comment
/// (64 hex characters or `nsec1…`), or the env-style file
/// `deploy/docker-compose/provision-nostr-key.sh` writes and Hermes reads,
/// with `BUZZ_PRIVATE_KEY=<key>` on a line of its own among other lines.
///
/// Refused before it is read when group or others can read it: the key
/// signs everything the clerk says, and a file the whole host can open is
/// one every other container mounting the same directory can too. The
/// refusal names the `chmod` that fixes it.
pub fn load_keys(path: &Path) -> Result<Keys> {
    let mode = std::fs::metadata(path)
        .with_context(|| format!("reading the clerk's key file {}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        bail!(
            "the clerk's key file {} is readable by group or others (mode {:04o}); it signs \
             everything the clerk posts, so run `chmod 0600 {}` and start again",
            path.display(),
            mode & 0o7777,
            path.display()
        );
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("reading the clerk's key file {}", path.display()))?;
    let Some(raw) = key_in(&contents) else {
        bail!(
            "the clerk's key file {} holds neither a bare key (64 hex characters or nsec1…, on \
             its first line that is not empty or a # comment) nor a line \
             {KEY_FILE_VARIABLE}=<key> the way deploy/docker-compose/provision-nostr-key.sh \
             writes one",
            path.display()
        );
    };
    Keys::parse(&raw).with_context(|| {
        format!(
            "the clerk's key file {} does not hold a Nostr secret key: expected 64 hex \
             characters or nsec1…, as a bare line or as {KEY_FILE_VARIABLE}=<key>",
            path.display()
        )
    })
}

/// The key `contents` holds, unparsed: a `BUZZ_PRIVATE_KEY=` line anywhere
/// (an `export` allowed, quotes dropped) wins, otherwise the first line that
/// is neither empty nor a comment — provided it is not some other variable's
/// assignment, which is a file of the env shape that simply lacks the key.
fn key_in(contents: &str) -> Option<String> {
    let lines = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let mut first = None;
    for line in lines {
        let assignment = line
            .strip_prefix("export ")
            .map(str::trim_start)
            .unwrap_or(line);
        if let Some(value) = assignment
            .strip_prefix(KEY_FILE_VARIABLE)
            .and_then(|rest| rest.trim_start().strip_prefix('='))
        {
            return Some(unquote(value.trim()).to_owned());
        }
        first.get_or_insert(line);
    }
    first.filter(|line| !line.contains('=')).map(str::to_owned)
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use nostr::nips::nip98::{verify_auth_header, HttpMethod};
    use nostr::{Keys, Kind, Timestamp, ToBech32, Url};
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn nip98_header_signs_the_url_method_and_payload() {
        let keys = Keys::generate();
        let url = "http://127.0.0.1:17800/events";
        let body = br#"{"kind":45001,"content":"a post"}"#;

        // The header, checked the way a relay checks it: `nostr`'s own
        // NIP-98 verifier decodes the base64, parses the event, matches the
        // URL and the method, hashes the body against `payload`, bounds
        // `created_at` and verifies the signature.
        let header = nip98_authorization(&keys, url, body).unwrap();
        assert!(header.starts_with("Nostr "), "{header}");
        let signer = verify_auth_header(
            &header,
            &Url::parse(url).unwrap(),
            HttpMethod::POST,
            Timestamp::now(),
            Some(body),
        )
        .expect("a header the relay would accept");
        assert_eq!(signer, keys.public_key());
        assert!(
            verify_auth_header(
                &header,
                &Url::parse("http://127.0.0.1:17800/query").unwrap(),
                HttpMethod::POST,
                Timestamp::now(),
                Some(body),
            )
            .is_err(),
            "the header is bound to the URL it was made for"
        );

        // The event itself, tag by tag.
        let event = nip98_event(&keys, url, body).unwrap();
        assert_eq!(event.kind, Kind::Custom(27235));
        assert_eq!(event.content, "");
        assert_eq!(event.pubkey, keys.public_key());
        event.verify().expect("signed with the clerk's own keys");

        let tags: Vec<&[String]> = event.tags.iter().map(|t| t.as_slice()).collect();
        assert!(
            tags.contains(&&["u".to_owned(), url.to_owned()][..]),
            "{tags:?}"
        );
        assert!(
            tags.contains(&&["method".to_owned(), "POST".to_owned()][..]),
            "{tags:?}"
        );
        let payload = tags
            .iter()
            .find(|t| t[0] == "payload")
            .map(|t| t[1].as_str())
            .expect("a payload tag");
        assert_eq!(payload.len(), 64);
        assert_eq!(payload, format!("{:x}", Sha256::digest(body)));
        let nonce = tags
            .iter()
            .find(|t| t[0] == "nonce")
            .map(|t| t[1].as_str())
            .expect("a nonce tag, so two requests with one body are two events");
        uuid::Uuid::parse_str(nonce).expect("the nonce is a UUID");

        // Every request is a fresh event: the relay keeps a replay guard on
        // the event id, so the same body signed twice must not collide.
        let again = nip98_authorization(&keys, url, body).unwrap();
        assert_ne!(header, again);
    }

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("twalk-clerk-relay-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_key(dir: &Path, name: &str, contents: &str, mode: u32) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn load_keys_reads_hex_and_nsec_and_refuses_a_readable_file() {
        let keys = Keys::generate();
        let hex = keys.secret_key().to_secret_hex();
        let nsec = keys.secret_key().to_bech32().unwrap();
        let dir = scratch_dir();

        let hex_path = write_key(&dir, "hex.key", &format!("{hex}\n"), 0o600);
        assert_eq!(
            load_keys(&hex_path).unwrap().public_key(),
            keys.public_key()
        );

        let nsec_path = write_key(&dir, "nsec.key", &format!("  {nsec}  \n\n"), 0o600);
        assert_eq!(
            load_keys(&nsec_path).unwrap().public_key(),
            keys.public_key()
        );

        for mode in [0o640, 0o604, 0o644, 0o660] {
            let path = write_key(&dir, &format!("open-{mode:o}.key"), &hex, mode);
            let err = load_keys(&path).unwrap_err().to_string();
            assert!(
                err.contains(&format!("chmod 0600 {}", path.display())),
                "{mode:o}: {err}"
            );
        }

        let garbage = write_key(&dir, "garbage.key", "not a key\n", 0o600);
        let err = load_keys(&garbage).unwrap_err().to_string();
        assert!(err.contains(&garbage.display().to_string()), "{err}");

        let empty = write_key(&dir, "empty.key", "", 0o600);
        assert!(load_keys(&empty).is_err());

        let err = load_keys(&dir.join("missing.key")).unwrap_err().to_string();
        assert!(err.contains("missing.key"), "{err}");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[allow(non_snake_case)]
    fn load_keys_reads_an_env_file_holding_BUZZ_PRIVATE_KEY() {
        let keys = Keys::generate();
        let hex = keys.secret_key().to_secret_hex();
        let nsec = keys.secret_key().to_bech32().unwrap();
        let dir = scratch_dir();

        // The shape `deploy/docker-compose/provision-nostr-key.sh` writes,
        // and the one Hermes reads: comments, other variables, and the key
        // on a line of its own, not necessarily the first.
        let env_file = format!(
            "# written by provision-nostr-key.sh\nBUZZ_RELAY_URL=http://127.0.0.1:17800\n\n\
             BUZZ_PRIVATE_KEY={hex}\nBUZZ_PUBLIC_KEY={}\n",
            keys.public_key().to_hex()
        );
        let path = write_key(&dir, "hermes.env", &env_file, 0o600);
        assert_eq!(load_keys(&path).unwrap().public_key(), keys.public_key());

        for line in [
            format!("BUZZ_PRIVATE_KEY=\"{nsec}\""),
            format!("BUZZ_PRIVATE_KEY='{hex}'"),
            format!("export BUZZ_PRIVATE_KEY={hex}"),
        ] {
            let path = write_key(&dir, "quoted.env", &format!("{line}\n"), 0o600);
            assert_eq!(
                load_keys(&path).unwrap().public_key(),
                keys.public_key(),
                "{line}"
            );
        }

        // A comment above a bare key is still a bare key.
        let commented = write_key(
            &dir,
            "commented.key",
            &format!("# the clerk's key\n{hex}\n"),
            0o600,
        );
        assert_eq!(
            load_keys(&commented).unwrap().public_key(),
            keys.public_key()
        );

        // An env file that names every variable but the key is refused with
        // both accepted shapes spelled out, so the operator can see which
        // one they meant to write.
        let without = write_key(
            &dir,
            "without.env",
            "BUZZ_RELAY_URL=http://127.0.0.1:17800\nBUZZ_PUBLIC_KEY=abc\n",
            0o600,
        );
        let err = load_keys(&without).unwrap_err().to_string();
        assert!(
            err.contains("BUZZ_PRIVATE_KEY=") && err.contains("nsec"),
            "{err}"
        );
        assert!(err.contains(&without.display().to_string()), "{err}");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn own_posts_filter_names_the_author_and_channel() {
        let filter = own_posts_filter(
            "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d",
            "9b1ba94a-38c3-49fe-9eb0-ffaafa62571a",
            45001,
            200,
        );
        assert_eq!(
            filter,
            serde_json::json!({
                "kinds": [45001],
                "#h": ["9b1ba94a-38c3-49fe-9eb0-ffaafa62571a"],
                "authors": ["3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"],
                "limit": 200,
            })
        );
    }

    #[test]
    fn the_base_url_is_stored_without_a_trailing_slash() {
        let keys = Keys::generate();
        let relay = Relay::new("http://127.0.0.1:17800/", keys.clone()).unwrap();
        assert_eq!(relay.base(), "http://127.0.0.1:17800");
        assert_eq!(relay.public_key_hex(), keys.public_key().to_hex());
    }

    #[test]
    fn a_rate_limit_and_a_server_error_are_transient_and_a_refusal_is_not() {
        let refused = |status| RelayError::Refused {
            status,
            body: String::new(),
        };
        assert!(RelayError::Unreachable("connection refused".into()).is_transient());
        assert!(refused(429).is_transient());
        assert!(refused(500).is_transient());
        assert!(refused(503).is_transient());
        assert!(!refused(400).is_transient());
        assert!(!refused(401).is_transient());
        assert!(!refused(403).is_transient());
        assert!(!RelayError::Malformed("not JSON".into()).is_transient());
    }

    #[test]
    fn a_refusal_is_logged_with_its_body_cut_short() {
        let long = "x".repeat(1000);
        let err = RelayError::Refused {
            status: 400,
            body: long.clone(),
        };
        let shown = err.to_string();
        assert!(shown.contains("400"));
        assert!(shown.len() < 400, "{}", shown.len());
        assert!(shown.contains(&"x".repeat(300)));
        assert!(!shown.contains(&"x".repeat(301)));
        // The error itself keeps the whole body: a caller may need the
        // relay's `retry in Ns` hint, and truncation is for the log line.
        if let RelayError::Refused { body, .. } = err {
            assert_eq!(body, long);
        }
    }
}
