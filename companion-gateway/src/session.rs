//! The user's session: signing in with a Matrix OpenID token, the device
//! list, and the per-device tokens every other endpoint requires (ticket
//! #52, ADR 0011).
//!
//! What a sign-in does, in order, and why each step is there:
//!
//! 1. **Verify** the OpenID token at the homeserver ([`crate::matrix_openid`]),
//!    which also checks that the answered Matrix ID belongs to the server
//!    that was asked.
//! 2. **Check the owner.** One deployment serves one human, named in
//!    configuration (`GATEWAY_OWNER`) exactly as the Sensor's allowed
//!    inviters are. Any other Matrix ID is refused — the homeserver may well
//!    have other accounts (the Sensor has one), and none of them is the
//!    owner.
//! 3. **Refuse a replay.** Synapse's verification does not consume the
//!    token: the same token answers for its whole lifetime, so anyone who
//!    captured one in flight could sign in with it again. The Gateway keeps
//!    a ledger of the tokens it has already accepted — digests only — and
//!    refuses a second sign-in with the same one.
//! 4. **Issue the Gateway's own tokens**: a short-lived device token and a
//!    long-lived refresh token, both random, both stored as digests, both
//!    belonging to one row in the device list.
//!
//! What this module deliberately never holds: a Matrix access token (the
//! Companion keeps the user's; the Gateway is never given it), and not even
//! the OpenID token it verified — the replay ledger stores a SHA-256 digest,
//! which identifies a token already seen and cannot be presented as one.
//!
//! Revocation is immediate because it is not cached: every authenticated
//! request reads the device row, so a revoked device's very next request is
//! refused, in this process and in any other sharing the store.
//!
//! Ticket #53 added one more row to the same store: that the registration
//! relay has created this deployment's one account. It lives here rather than
//! in a file of its own because it is a fact about the same subject — who this
//! deployment serves — and because the relay's owner check and sign-in's owner
//! check must never be able to disagree.
//!
//! The store is SQLite in the Gateway's state directory, in its own file
//! (`sessions.db`) next to the consent journal's (`consent.db`, ticket #49):
//! the session store is the user's login state and the journal is the record
//! of truth for consent, they have no transaction in common, and keeping
//! them apart lets either be deleted without touching the other — losing
//! every device means signing in again, which the Companion can do silently.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use crate::matrix_openid::{OpenIdToken, Refusal, Verifier};

/// How long the OpenID replay ledger keeps a digest. It has to outlive the
/// tokens themselves: a digest dropped while its token is still valid at the
/// homeserver would re-open exactly the replay this ledger closes. Synapse
/// mints hour-long tokens, and the client's `expires_in` is the client's
/// claim rather than a fact, so the ledger keeps a day — long enough for any
/// plausible homeserver policy, short enough that the table stays a few rows
/// on a personal deployment.
const REPLAY_LEDGER_RETENTION_SECONDS: u64 = 24 * 60 * 60;

/// Default lifetime of a device token: short, because it is the credential
/// that travels on every request. The Companion refreshes it while the app
/// is open; a device that comes back after longer refreshes once and carries
/// on. `GATEWAY_DEVICE_TOKEN_TTL` overrides it.
pub const DEFAULT_DEVICE_TOKEN_TTL_SECONDS: u64 = 15 * 60;

/// Default lifetime of a refresh token: long, because its expiry is what
/// finally logs a device out. `GATEWAY_REFRESH_TOKEN_TTL` overrides it.
pub const DEFAULT_REFRESH_TOKEN_TTL_SECONDS: u64 = 30 * 24 * 60 * 60;

/// The longest device name the store accepts. The name is the user's own
/// label for a device they can see in a list; a cap keeps a mistyped paste
/// out of the store.
const MAX_DEVICE_NAME: usize = 64;

/// What an unnamed device is called in the list.
const UNNAMED_DEVICE: &str = "Companion";

/// One device in the user's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The device's public identifier: what the Companion revokes by. Random,
    /// so it says nothing about the device.
    pub id: String,
    pub name: String,
    pub created_unix_seconds: u64,
    pub last_seen_unix_seconds: u64,
    /// When the device was revoked, or `None` while it is live. Revoked rows
    /// stay in the list: "I revoked that phone last Tuesday" is worth seeing.
    pub revoked_unix_seconds: Option<u64>,
}

/// A freshly minted pair of tokens and the device they belong to. The plain
/// tokens exist only in this value, on their way into the response's
/// cookies: the store keeps digests.
#[derive(Debug, Clone)]
pub struct Issued {
    pub device: Device,
    pub device_token: String,
    pub device_token_ttl_seconds: u64,
    pub refresh_token: String,
    pub refresh_token_ttl_seconds: u64,
}

/// Why a sign-in was refused.
#[derive(Debug)]
pub enum SignInRefusal {
    /// The token did not resolve to an identity at the homeserver.
    Unresolved(Refusal),
    /// It resolved to somebody who is not this deployment's owner.
    NotTheOwner { subject: String },
    /// This exact OpenID token has already been used to sign in.
    Replayed,
    /// The store failed. An operator problem.
    Store(anyhow::Error),
}

impl SignInRefusal {
    /// The stable label for logs and the sign-in metric's `outcome` label.
    pub fn label(&self) -> &'static str {
        match self {
            SignInRefusal::Unresolved(refusal) => refusal.label(),
            SignInRefusal::NotTheOwner { .. } => "not_the_owner",
            SignInRefusal::Replayed => "replayed",
            SignInRefusal::Store(_) => "store_failed",
        }
    }
}

/// The sessions of the one owner this deployment serves.
pub struct Sessions {
    store: Mutex<Connection>,
    verifier: Verifier,
    /// The Matrix ID allowed to sign in, from configuration.
    owner: String,
    /// The localpart of [`Self::owner`]: the only account the registration
    /// relay may create (ticket #53). Derived once at open, so the relay and
    /// the owner check can never disagree about who this deployment serves.
    owner_localpart: String,
    device_token_ttl_seconds: u64,
    refresh_token_ttl_seconds: u64,
    now_unix_seconds: fn() -> u64,
}

impl Sessions {
    /// Opens (creating it if needed) the session store in `state_dir`.
    pub fn open(
        state_dir: &Path,
        verifier: Verifier,
        owner: String,
        device_token_ttl_seconds: u64,
        refresh_token_ttl_seconds: u64,
        now_unix_seconds: fn() -> u64,
    ) -> Result<Self> {
        std::fs::create_dir_all(state_dir).with_context(|| {
            format!(
                "failed to create the gateway state directory {}",
                state_dir.display()
            )
        })?;
        let path = state_dir.join("sessions.db");
        let store = Connection::open(&path)
            .with_context(|| format!("failed to open the session store {}", path.display()))?;
        migrate(&store).with_context(|| format!("failed to migrate {}", path.display()))?;
        let owner_localpart = crate::bootstrap::owner_localpart(&owner)?.to_owned();
        Ok(Self {
            store: Mutex::new(store),
            verifier,
            owner,
            owner_localpart,
            device_token_ttl_seconds,
            refresh_token_ttl_seconds,
            now_unix_seconds,
        })
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The owner's localpart: the one account the registration relay may
    /// create (ticket #53).
    pub fn owner_localpart(&self) -> &str {
        &self.owner_localpart
    }

    pub fn device_token_ttl_seconds(&self) -> u64 {
        self.device_token_ttl_seconds
    }

    /// The Matrix server name this deployment serves: the domain of the
    /// owner's Matrix ID, and the only homeserver whose OpenID tokens are
    /// accepted.
    pub fn homeserver_name(&self) -> &str {
        self.verifier.server_name()
    }

    /// Verifies an OpenID token, checks it against the owner, refuses a
    /// replay, and issues the device's own tokens.
    pub async fn sign_in(
        &self,
        token: &OpenIdToken,
        device_name: Option<&str>,
    ) -> Result<Issued, SignInRefusal> {
        let subject = self
            .verifier
            .resolve(token)
            .await
            .map_err(SignInRefusal::Unresolved)?;
        if subject != self.owner {
            return Err(SignInRefusal::NotTheOwner { subject });
        }
        // Only after the identity is established: a ledger that recorded
        // every token presented to it would be a place for anyone to write.
        let now = (self.now_unix_seconds)();
        self.remember_token(&token.access_token, now)?;
        self.issue(device_name, now).map_err(SignInRefusal::Store)
    }

    /// The device this token belongs to, or `None` when the token is unknown,
    /// expired, or the device has been revoked. Records the device as seen.
    pub fn authenticate(&self, device_token: &str) -> Option<Device> {
        let now = (self.now_unix_seconds)();
        let store = self
            .store
            .lock()
            .expect("the session store is not poisoned");
        let device = store
            .query_row(
                "SELECT id, name, created_unix_seconds, last_seen_unix_seconds, \
                 revoked_unix_seconds \
                 FROM devices \
                 WHERE device_token_sha256 = ?1 \
                   AND revoked_unix_seconds IS NULL \
                   AND device_token_expires_unix_seconds > ?2",
                (digest(device_token), seconds(now)),
                device_from_row,
            )
            .optional()
            .ok()
            .flatten()?;
        // Best effort: a failed last-seen update must not refuse a request
        // that is otherwise authentic.
        let _ = store.execute(
            "UPDATE devices SET last_seen_unix_seconds = ?2 WHERE id = ?1",
            (&device.id, seconds(now)),
        );
        Some(Device {
            last_seen_unix_seconds: now,
            ..device
        })
    }

    /// Exchanges a refresh token for a new pair, rotating both: the device
    /// keeps its identity in the list, and a device token that leaked stops
    /// working at the next refresh instead of living out its lifetime.
    /// `None` when the refresh token is unknown, expired or revoked.
    pub fn refresh(&self, refresh_token: &str) -> Option<Issued> {
        let now = (self.now_unix_seconds)();
        let device_token = random_hex(32);
        let next_refresh_token = random_hex(32);
        let store = self
            .store
            .lock()
            .expect("the session store is not poisoned");
        let device = store
            .query_row(
                "UPDATE devices SET \
                   device_token_sha256 = ?2, \
                   device_token_expires_unix_seconds = ?3, \
                   refresh_token_sha256 = ?4, \
                   refresh_token_expires_unix_seconds = ?5, \
                   last_seen_unix_seconds = ?6 \
                 WHERE refresh_token_sha256 = ?1 \
                   AND revoked_unix_seconds IS NULL \
                   AND refresh_token_expires_unix_seconds > ?6 \
                 RETURNING id, name, created_unix_seconds, last_seen_unix_seconds, \
                   revoked_unix_seconds",
                (
                    digest(refresh_token),
                    digest(&device_token),
                    seconds(now + self.device_token_ttl_seconds),
                    digest(&next_refresh_token),
                    seconds(now + self.refresh_token_ttl_seconds),
                    seconds(now),
                ),
                device_from_row,
            )
            .optional()
            .ok()
            .flatten()?;
        Some(Issued {
            device,
            device_token,
            device_token_ttl_seconds: self.device_token_ttl_seconds,
            refresh_token: next_refresh_token,
            refresh_token_ttl_seconds: self.refresh_token_ttl_seconds,
        })
    }

    /// The device list, newest first.
    pub fn devices(&self) -> Result<Vec<Device>> {
        let store = self
            .store
            .lock()
            .expect("the session store is not poisoned");
        let mut statement = store.prepare(
            "SELECT id, name, created_unix_seconds, last_seen_unix_seconds, \
             revoked_unix_seconds FROM devices ORDER BY created_unix_seconds DESC, id",
        )?;
        let devices = statement
            .query_map([], device_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(devices)
    }

    /// Revokes one device, by its public id. `false` when no live device has
    /// that id — an unknown id and an already revoked one are the same
    /// answer, so revoking twice is not an error.
    ///
    /// The tokens' digests are dropped with the same statement: a revoked
    /// row is a record that the device existed, not a credential in waiting.
    pub fn revoke(&self, device_id: &str) -> Result<bool> {
        let now = (self.now_unix_seconds)();
        let store = self
            .store
            .lock()
            .expect("the session store is not poisoned");
        let changed = store.execute(
            "UPDATE devices SET \
               revoked_unix_seconds = ?2, \
               device_token_sha256 = NULL, \
               refresh_token_sha256 = NULL \
             WHERE id = ?1 AND revoked_unix_seconds IS NULL",
            (device_id, seconds(now)),
        )?;
        Ok(changed > 0)
    }

    /// When this deployment's one account was created through the
    /// registration relay, or `None` when no account has been (ticket #53).
    ///
    /// This is the Gateway's own memory of the refusal ADR 0011 promises:
    /// one owner per deployment, so one account ever. It is deliberately not
    /// the only check — the homeserver's `M_USER_IN_USE` is honoured as the
    /// same refusal, so losing this store cannot re-open the window.
    pub fn owner_account_created(&self) -> Result<Option<u64>> {
        let created = self
            .store
            .lock()
            .expect("the session store is not poisoned")
            .query_row(
                "SELECT created_unix_seconds FROM owner_account WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(|created| created as u64);
        Ok(created)
    }

    /// Records that the relay created the owner's account. `false` when a row
    /// was already there — the table admits exactly one (`CHECK (id = 1)`), so
    /// "one account per deployment" is a constraint of the schema and not only
    /// of the code above it.
    pub fn record_owner_account_created(&self, user_id: &str) -> Result<bool> {
        let now = (self.now_unix_seconds)();
        let inserted = self
            .store
            .lock()
            .expect("the session store is not poisoned")
            .execute(
                "INSERT OR IGNORE INTO owner_account (id, user_id, created_unix_seconds) \
                 VALUES (1, ?1, ?2)",
                (user_id, seconds(now)),
            )
            .context("failed to record the owner's account")?;
        Ok(inserted > 0)
    }

    /// Records an accepted OpenID token's digest, refusing one already in the
    /// ledger. Prunes digests older than the retention window on the way
    /// through: the ledger is a few rows on a personal deployment, and this
    /// is the only place that writes it.
    fn remember_token(&self, access_token: &str, now: u64) -> Result<(), SignInRefusal> {
        let store = self
            .store
            .lock()
            .expect("the session store is not poisoned");
        store
            .execute(
                "DELETE FROM openid_tokens_accepted WHERE accepted_unix_seconds < ?1",
                (seconds(now.saturating_sub(REPLAY_LEDGER_RETENTION_SECONDS)),),
            )
            .map_err(|error| SignInRefusal::Store(error.into()))?;
        let inserted = store
            .execute(
                "INSERT OR IGNORE INTO openid_tokens_accepted \
                 (token_sha256, accepted_unix_seconds) VALUES (?1, ?2)",
                (digest(access_token), seconds(now)),
            )
            .map_err(|error| SignInRefusal::Store(error.into()))?;
        if inserted == 0 {
            return Err(SignInRefusal::Replayed);
        }
        Ok(())
    }

    /// Adds a device to the list with a fresh pair of tokens.
    fn issue(&self, device_name: Option<&str>, now: u64) -> Result<Issued> {
        let device = Device {
            id: random_hex(16),
            name: sanitize_device_name(device_name),
            created_unix_seconds: now,
            last_seen_unix_seconds: now,
            revoked_unix_seconds: None,
        };
        let device_token = random_hex(32);
        let refresh_token = random_hex(32);
        self.store
            .lock()
            .expect("the session store is not poisoned")
            .execute(
                "INSERT INTO devices ( \
                   id, name, created_unix_seconds, last_seen_unix_seconds, \
                   device_token_sha256, device_token_expires_unix_seconds, \
                   refresh_token_sha256, refresh_token_expires_unix_seconds \
                 ) VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7)",
                (
                    &device.id,
                    &device.name,
                    seconds(now),
                    digest(&device_token),
                    seconds(now + self.device_token_ttl_seconds),
                    digest(&refresh_token),
                    seconds(now + self.refresh_token_ttl_seconds),
                ),
            )
            .context("failed to record a new device")?;
        Ok(Issued {
            device,
            device_token,
            device_token_ttl_seconds: self.device_token_ttl_seconds,
            refresh_token,
            refresh_token_ttl_seconds: self.refresh_token_ttl_seconds,
        })
    }
}

fn device_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Device> {
    Ok(Device {
        id: row.get(0)?,
        name: row.get(1)?,
        created_unix_seconds: row.get::<_, i64>(2)? as u64,
        last_seen_unix_seconds: row.get::<_, i64>(3)? as u64,
        revoked_unix_seconds: row.get::<_, Option<i64>>(4)?.map(|at| at as u64),
    })
}

/// A moment as SQLite stores it. SQLite integers are signed 64-bit, and the
/// Gateway counts whole seconds since the epoch: the conversion is lossless
/// in both directions for any clock this side of the year 292 billion, and
/// keeping `u64` in the domain keeps the JSON documents unsigned.
fn seconds(unix_seconds: u64) -> i64 {
    unix_seconds as i64
}

/// The schema, applied on open. Embedded in the binary and versioned through
/// SQLite's own `user_version`, so an upgrade is a code change and never an
/// operator step.
///
/// Note what the columns are: digests, never tokens. Not the device tokens
/// this Gateway minted, and not the OpenID tokens it verified — a store
/// anyone read would let them revoke devices and see when the user last used
/// one, but never sign in, and never act on the homeserver.
fn migrate(store: &Connection) -> Result<()> {
    store.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    let version: u32 = store.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 1 {
        store.execute_batch(
            "BEGIN;
             CREATE TABLE devices (
               id                                  TEXT PRIMARY KEY,
               name                                TEXT NOT NULL,
               created_unix_seconds                INTEGER NOT NULL,
               last_seen_unix_seconds              INTEGER NOT NULL,
               revoked_unix_seconds                INTEGER,
               device_token_sha256                 TEXT,
               device_token_expires_unix_seconds   INTEGER,
               refresh_token_sha256                TEXT,
               refresh_token_expires_unix_seconds  INTEGER
             );
             CREATE UNIQUE INDEX devices_device_token
               ON devices (device_token_sha256);
             CREATE UNIQUE INDEX devices_refresh_token
               ON devices (refresh_token_sha256);
             CREATE TABLE openid_tokens_accepted (
               token_sha256           TEXT PRIMARY KEY,
               accepted_unix_seconds  INTEGER NOT NULL
             );
             PRAGMA user_version = 1;
             COMMIT;",
        )?;
    }
    if version < 2 {
        // Ticket #53: that the registration relay has created this
        // deployment's one account. `CHECK (id = 1)` is the point — the table
        // holds one row by construction, so the "one owner per deployment"
        // rule of ADR 0011 is a constraint of the schema and not only of the
        // code that writes it. No token and no password: the user id and the
        // date, which is all a refusal needs.
        store.execute_batch(
            "BEGIN;
             CREATE TABLE owner_account (
               id                    INTEGER PRIMARY KEY CHECK (id = 1),
               user_id               TEXT NOT NULL,
               created_unix_seconds  INTEGER NOT NULL
             );
             PRAGMA user_version = 2;
             COMMIT;",
        )?;
    }
    Ok(())
}

/// SHA-256 of a token, lowercase hex: what the store keeps instead of the
/// token. The same digest for the same token, so a lookup is an equality
/// test, and nothing in the store can be replayed as a credential.
fn digest(token: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `bytes` bytes from the OS CSPRNG, lowercase hex. Tokens and device ids are
/// the only random values the Gateway mints, and `getrandom` is the OS
/// interface for exactly that.
fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("the operating system's CSPRNG is available");
    buffer.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The device name as it goes into the list: the user's own label, trimmed,
/// capped, and stripped of the control characters that would break a list
/// rendering it. An empty or absent name becomes [`UNNAMED_DEVICE`].
fn sanitize_device_name(name: Option<&str>) -> String {
    let cleaned: String = name
        .unwrap_or_default()
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return UNNAMED_DEVICE.to_owned();
    }
    cleaned.chars().take(MAX_DEVICE_NAME).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store in a throwaway directory, with a clock the test controls and
    /// a verifier pointed at nothing — every test here exercises the store
    /// and the tokens, never the homeserver call (that is
    /// `tests/signin.rs`'s job, against a real Synapse).
    fn sessions(device_ttl: u64, refresh_ttl: u64) -> (Sessions, tempdir::TempDir) {
        let dir = tempdir::TempDir::new();
        let verifier =
            Verifier::new("http://127.0.0.1:1", "test.twalk").expect("a verifier is built");
        let sessions = Sessions::open(
            dir.path(),
            verifier,
            "@owner:test.twalk".to_owned(),
            device_ttl,
            refresh_ttl,
            || 1_000,
        )
        .expect("the store opens");
        (sessions, dir)
    }

    #[test]
    fn an_issued_device_token_authenticates_its_own_device_and_nothing_else() {
        let (sessions, _dir) = sessions(900, 9_000);
        let issued = sessions
            .issue(Some("Pixel 8"), 1_000)
            .expect("a device is issued");
        let authenticated = sessions
            .authenticate(&issued.device_token)
            .expect("the issued token authenticates");
        assert_eq!(authenticated.id, issued.device.id);
        assert_eq!(authenticated.name, "Pixel 8");
        assert!(sessions.authenticate("not-a-token").is_none());
        // The refresh token is not a device token.
        assert!(sessions.authenticate(&issued.refresh_token).is_none());
    }

    #[test]
    fn a_revoked_device_stops_authenticating_at_once() {
        let (sessions, _dir) = sessions(900, 9_000);
        let issued = sessions.issue(None, 1_000).expect("a device is issued");
        assert!(sessions.revoke(&issued.device.id).expect("revocation runs"));
        assert!(
            sessions.authenticate(&issued.device_token).is_none(),
            "a revoked device's token must stop working immediately"
        );
        assert!(
            sessions.refresh(&issued.refresh_token).is_none(),
            "a revoked device must not refresh its way back in"
        );
        // Revoking twice is not an error, and the row stays in the list with
        // the date it was revoked on.
        assert!(!sessions.revoke(&issued.device.id).expect("revocation runs"));
        let devices = sessions.devices().expect("the device list reads");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].revoked_unix_seconds, Some(1_000));
    }

    #[test]
    fn an_expired_device_token_is_refused_but_refreshes_into_a_new_one() {
        let (sessions, _dir) = sessions(0, 9_000);
        let issued = sessions.issue(None, 1_000).expect("a device is issued");
        assert!(
            sessions.authenticate(&issued.device_token).is_none(),
            "a device token past its expiry is not a credential"
        );
        let refreshed = sessions
            .refresh(&issued.refresh_token)
            .expect("the refresh token still works");
        assert_eq!(refreshed.device.id, issued.device.id, "same device");
        assert_ne!(
            refreshed.refresh_token, issued.refresh_token,
            "the refresh token rotates"
        );
        assert!(
            sessions.refresh(&issued.refresh_token).is_none(),
            "the rotated-away refresh token stops working"
        );
    }

    #[test]
    fn refreshing_rotates_the_device_token_so_the_old_one_stops_working() {
        let (sessions, _dir) = sessions(900, 9_000);
        let issued = sessions.issue(None, 1_000).expect("a device is issued");
        let refreshed = sessions
            .refresh(&issued.refresh_token)
            .expect("the refresh token works");
        assert!(sessions.authenticate(&refreshed.device_token).is_some());
        assert!(
            sessions.authenticate(&issued.device_token).is_none(),
            "the previous device token is rotated away"
        );
    }

    #[test]
    fn the_replay_ledger_accepts_a_token_once() {
        let (sessions, _dir) = sessions(900, 9_000);
        sessions
            .remember_token("an-openid-token", 1_000)
            .expect("the first sign-in with a token is accepted");
        let refused = sessions
            .remember_token("an-openid-token", 1_001)
            .expect_err("the same token a second time is a replay");
        assert!(matches!(refused, SignInRefusal::Replayed), "{refused:?}");
        // A different token is unaffected.
        sessions
            .remember_token("another-openid-token", 1_002)
            .expect("a different token is accepted");
    }

    #[test]
    fn the_replay_ledger_stores_a_digest_and_not_the_token() {
        let (sessions, dir) = sessions(900, 9_000);
        sessions
            .remember_token("syt_a_secret_looking_openid_token", 1_000)
            .expect("the token is accepted");
        let held: String = sessions
            .store
            .lock()
            .expect("the store is not poisoned")
            .query_row(
                "SELECT token_sha256 FROM openid_tokens_accepted",
                [],
                |row| row.get(0),
            )
            .expect("one row");
        assert_ne!(held, "syt_a_secret_looking_openid_token");
        assert_eq!(held, digest("syt_a_secret_looking_openid_token"));
        // And nowhere in the file either (the process-boundary test in
        // tests/signin.rs asserts the same thing against a real token).
        let bytes = std::fs::read(dir.path().join("sessions.db")).expect("the store is readable");
        assert!(
            !String::from_utf8_lossy(&bytes).contains("syt_a_secret_looking_openid_token"),
            "the store must not hold the token itself"
        );
    }

    #[test]
    fn a_device_name_is_the_users_label_trimmed_capped_and_defaulted() {
        assert_eq!(sanitize_device_name(Some("  Pixel 8 ")), "Pixel 8");
        assert_eq!(sanitize_device_name(None), UNNAMED_DEVICE);
        assert_eq!(sanitize_device_name(Some("   ")), UNNAMED_DEVICE);
        assert_eq!(
            sanitize_device_name(Some("line\nbreak\u{0}")),
            "linebreak",
            "control characters do not reach the list"
        );
        assert_eq!(
            sanitize_device_name(Some(&"x".repeat(200))).chars().count(),
            MAX_DEVICE_NAME
        );
    }

    #[test]
    fn tokens_are_random_and_long() {
        let first = random_hex(32);
        let second = random_hex(32);
        assert_eq!(first.len(), 64, "32 bytes as hex");
        assert_ne!(first, second);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn the_owners_account_can_be_recorded_once_and_only_once() {
        let (sessions, _dir) = sessions(900, 9_000);
        assert_eq!(
            sessions.owner_account_created().expect("the store reads"),
            None,
            "nothing has been created yet"
        );
        assert!(sessions
            .record_owner_account_created("@owner:test.twalk")
            .expect("the first record is written"));
        assert_eq!(
            sessions.owner_account_created().expect("the store reads"),
            Some(1_000)
        );
        assert!(
            !sessions
                .record_owner_account_created("@somebody_else:test.twalk")
                .expect("a second record is not an error"),
            "the table holds exactly one account: one owner per deployment"
        );
        assert_eq!(
            sessions.owner_account_created().expect("the store reads"),
            Some(1_000),
            "the first account stays the account"
        );
    }

    #[test]
    fn the_owner_localpart_is_derived_from_the_configured_owner() {
        let (sessions, _dir) = sessions(900, 9_000);
        assert_eq!(sessions.owner(), "@owner:test.twalk");
        assert_eq!(sessions.owner_localpart(), "owner");
    }

    #[test]
    fn the_store_survives_being_reopened() {
        let dir = tempdir::TempDir::new();
        let open = || {
            Sessions::open(
                dir.path(),
                Verifier::new("http://127.0.0.1:1", "test.twalk").expect("a verifier"),
                "@owner:test.twalk".to_owned(),
                900,
                9_000,
                || 1_000,
            )
            .expect("the store opens")
        };
        let first = open();
        let issued = first.issue(Some("Laptop"), 1_000).expect("a device");
        first
            .record_owner_account_created("@owner:test.twalk")
            .expect("the account is recorded");
        // A restart keeps the device list and the tokens it issued: the
        // migration is idempotent and the rows are on disk.
        let reopened = open();
        assert!(reopened.authenticate(&issued.device_token).is_some());
        assert_eq!(reopened.devices().expect("the list reads").len(), 1);
        assert_eq!(
            reopened.owner_account_created().expect("the store reads"),
            Some(1_000),
            "a restart still refuses a second account"
        );
    }

    /// A throwaway directory, as `static_files`'s tests use: the crate has no
    /// dev dependency for this and the need is three lines deep.
    mod tempdir {
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        pub struct TempDir(PathBuf);

        impl TempDir {
            pub fn new() -> Self {
                let unique = format!(
                    "twalk-gateway-sessions-{}-{}",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                );
                let path = std::env::temp_dir().join(unique);
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).expect("the temp directory is writable");
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
