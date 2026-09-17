//! The consent store: an append-only decision journal in SQLite, with the
//! current state as a projection over it and the outbox rows in the journal
//! itself (ticket #49).
//!
//! Three properties are the point of this module, and each is enforced by the
//! schema rather than by discipline:
//!
//! - **The journal is append-only.** Triggers refuse every `DELETE` and every
//!   `UPDATE` of a recorded field. The one mutable column is `published_at`,
//!   which is the outbox's own bookkeeping and says nothing about the
//!   decision.
//! - **The current state is a projection, not a second truth.** It is a SQL
//!   view over the journal (`consent_state`), so it cannot drift from the
//!   decisions it derives from — there is no projection table to rebuild and
//!   no code path that can write one without writing a decision (ADR 0010).
//! - **Commit and publication are one transaction away from each other.** A
//!   decision and its rendered envelope land in the same transaction; the
//!   outbox publishes afterwards and marks the row. A crash in between
//!   republishes (deduplicated on the bus by `Nats-Msg-Id`) rather than
//!   losing the decision.
//!
//! The schema migrations are embedded in the binary ([`MIGRATIONS`]) and
//! applied at open, so an operator upgrades the image and nothing else.
//!
//! No message content is stored here, ever: a decision is a subject, a state,
//! a perimeter, two timestamps and the owner who took it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::consent::{
    Decision, Effective, Network, OldState, Recorded, State, Subject, SubjectType,
};

/// The store's file inside the Gateway's state directory. A companion `-wal`
/// and `-shm` sit next to it: the journal is opened in WAL mode, so a reader
/// never blocks the writer that is recording a decision.
const DATABASE_FILE: &str = "consent.sqlite3";

/// The schema, one statement batch per version. The applied version is the
/// database's `user_version`; a new migration is appended to this array and
/// never edited in place, so an existing store upgrades by applying exactly
/// the tail it has not seen.
pub const MIGRATIONS: [&str; 1] = [
    // v1 — the decision journal, its per-network scope rows, and the current
    // state as a view over both.
    r#"
    CREATE TABLE consent_decision (
        -- The journal's own order, and the only ordering the store trusts:
        -- clocks are not an ordering (ADR 0010).
        sequence     INTEGER PRIMARY KEY AUTOINCREMENT,
        -- The contract's deterministic id. UNIQUE makes re-recording the
        -- identical decision idempotent instead of duplicating it.
        event_id     TEXT NOT NULL UNIQUE,
        subject_type TEXT NOT NULL CHECK (subject_type IN ('contact', 'network', 'persona')),
        subject_id   TEXT NOT NULL,
        old_state    TEXT NOT NULL CHECK (old_state IN ('unset', 'granted', 'pending', 'revoked')),
        new_state    TEXT NOT NULL CHECK (new_state IN ('granted', 'pending', 'revoked')),
        -- The scope as the id recipe renders it: sorted, comma-joined. Kept
        -- verbatim because it is part of the event's natural key.
        scope_key    TEXT NOT NULL,
        occurred_at  TEXT NOT NULL,
        actor        TEXT NOT NULL,
        reason       TEXT,
        -- The rendered CloudEvent, published verbatim: a republished row is
        -- byte-identical to what a crash interrupted, whatever the code has
        -- become since.
        envelope     TEXT NOT NULL,
        -- The outbox's only bookkeeping: NULL until the decision reached the
        -- bus.
        published_at TEXT
    );

    -- One row per (decision, network): the scope, normalised, so that the
    -- current state can be a view instead of a table someone has to keep in
    -- step.
    CREATE TABLE consent_decision_network (
        sequence INTEGER NOT NULL REFERENCES consent_decision(sequence),
        network  TEXT NOT NULL CHECK (network IN ('whatsapp', 'telegram', 'signal', 'discord', 'sms', 'matrix')),
        PRIMARY KEY (sequence, network)
    ) WITHOUT ROWID;

    CREATE INDEX consent_decision_subject
        ON consent_decision (subject_type, subject_id, sequence);
    CREATE INDEX consent_decision_unpublished
        ON consent_decision (sequence) WHERE published_at IS NULL;

    -- The projection: for every (subject, network), the most recent decision
    -- that covered it. One entry per (subject, network), revocations as
    -- explicit as grants, and an absent row meaning "never decided" — never
    -- "revoked".
    CREATE VIEW consent_state AS
    SELECT subject_type, subject_id, network, new_state AS state,
           occurred_at AS decided_at, sequence AS decision_sequence
    FROM (
        SELECT d.subject_type, d.subject_id, n.network, d.new_state, d.occurred_at, d.sequence,
               ROW_NUMBER() OVER (
                   PARTITION BY d.subject_type, d.subject_id, n.network
                   ORDER BY d.sequence DESC
               ) AS recency
        FROM consent_decision d
        JOIN consent_decision_network n ON n.sequence = d.sequence
    )
    WHERE recency = 1;

    -- Append-only, enforced. The journal is the record of truth and the
    -- audit trail of a confidentiality promise: nothing in this service may
    -- rewrite a decision the user took.
    CREATE TRIGGER consent_decision_no_delete BEFORE DELETE ON consent_decision
    BEGIN
        SELECT RAISE(ABORT, 'the consent decision journal is append-only');
    END;
    CREATE TRIGGER consent_decision_no_update
    BEFORE UPDATE OF event_id, subject_type, subject_id, old_state, new_state,
                     scope_key, occurred_at, actor, reason, envelope
    ON consent_decision
    BEGIN
        SELECT RAISE(ABORT, 'the consent decision journal is append-only');
    END;
    CREATE TRIGGER consent_decision_network_no_delete
    BEFORE DELETE ON consent_decision_network
    BEGIN
        SELECT RAISE(ABORT, 'the consent decision journal is append-only');
    END;
    CREATE TRIGGER consent_decision_network_no_update
    BEFORE UPDATE ON consent_decision_network
    BEGIN
        SELECT RAISE(ABORT, 'the consent decision journal is append-only');
    END;
    "#,
];

/// The consent store. One connection behind a mutex: a decision is a handful
/// of small local statements, so the contention a pool would relieve does not
/// exist, and one writer is what SQLite wants anyway.
pub struct Store {
    connection: Mutex<Connection>,
    path: PathBuf,
}

/// A decision as committed: the journal position it took, the event the
/// outbox will publish, and whether this was the identical decision arriving
/// twice.
#[derive(Debug, Clone)]
pub struct Committed {
    pub sequence: i64,
    pub event_id: String,
    pub recorded: Recorded,
    /// True when the journal already held this exact decision (same subject,
    /// state, scope and instant): the store recorded nothing new and the
    /// event id is the one from the first time. The contract's determinism is
    /// what makes this safe — re-emitting a recorded change is idempotent.
    pub replayed: bool,
}

/// One entry of the current state: a subject, a network, and the state the
/// most recent decision covering them left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub subject: Subject,
    pub network: Network,
    pub state: State,
    pub decided_at: String,
    pub decision_sequence: i64,
}

/// A committed decision the outbox has not published yet.
#[derive(Debug, Clone)]
pub struct Unpublished {
    pub sequence: i64,
    pub event_id: String,
    pub envelope: Value,
}

impl Store {
    /// Opens (creating if needed) the store under the Gateway's state
    /// directory and applies every migration the file has not seen.
    ///
    /// The directory is created with owner-only permissions and the database
    /// file narrowed to the same: the journal is a social graph with
    /// timestamps (who the user decided about, on which network, when), and
    /// nothing in the reference deployment encrypts it at rest — see
    /// `docs/architecture/security-model.md`.
    pub fn open(state_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("failed to create {}", state_dir.display()))?;
        restrict_to_owner(state_dir, 0o700);
        let path = state_dir.join(DATABASE_FILE);
        let connection = Connection::open(&path)
            .with_context(|| format!("failed to open the consent store at {}", path.display()))?;
        restrict_to_owner(&path, 0o600);
        // WAL so a snapshot read never blocks the decision being recorded;
        // synchronous=FULL because a decision the user was told was recorded
        // must survive the machine losing power, not only the process dying.
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .context("failed to put the consent store in WAL mode")?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .context("failed to set the consent store's durability")?;
        connection
            .pragma_update(None, "foreign_keys", true)
            .context("failed to enable foreign keys on the consent store")?;
        let store = Self {
            connection: Mutex::new(connection),
            path,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Applies the embedded migrations the store has not seen, tracked by the
    /// database's own `user_version`. Each one runs in its own transaction, so
    /// a failure leaves the store at the last version that applied cleanly
    /// instead of half-migrated.
    fn migrate(&self) -> Result<()> {
        let mut connection = self.connection();
        let applied: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .context("failed to read the consent store's schema version")?;
        let applied = usize::try_from(applied).unwrap_or(0);
        anyhow::ensure!(
            applied <= MIGRATIONS.len(),
            "the consent store is at schema version {applied}, newer than the {} this build knows: \
             a downgrade would have to rewrite the decision journal, so the Gateway refuses to start",
            MIGRATIONS.len()
        );
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(applied) {
            let version = index + 1;
            let transaction = connection
                .transaction()
                .context("failed to open a migration transaction")?;
            transaction
                .execute_batch(migration)
                .with_context(|| format!("consent store migration {version} failed"))?;
            transaction
                .pragma_update(None, "user_version", i64::try_from(version).unwrap())
                .with_context(|| format!("failed to record schema version {version}"))?;
            transaction
                .commit()
                .with_context(|| format!("failed to commit migration {version}"))?;
            tracing::info!(version, "applied a consent store migration");
        }
        Ok(())
    }

    /// Records one decision: the journal row, its scope rows and the rendered
    /// envelope, in a single transaction. What the caller does after it
    /// returns cannot lose the decision — that is what makes the publication
    /// an outbox rather than a hope.
    ///
    /// `old_state` is read inside the same transaction, from the most recent
    /// decision that covered any of this decision's networks: for the usual
    /// single-network decision that is simply the state the subject held on
    /// that network, and for a multi-network scope it is the last thing the
    /// user decided about that perimeter. `unset` means no decision had ever
    /// covered it.
    pub fn record(
        &self,
        decision: &Decision,
        actor: &str,
        occurred_at: &str,
        domain: &str,
        produced_at: &str,
    ) -> Result<Committed> {
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("failed to open the decision transaction")?;

        let placeholders = vec!["?"; decision.networks.len()].join(", ");
        let mut parameters: Vec<String> = vec![
            decision.subject.kind.as_str().to_owned(),
            decision.subject.id.clone(),
        ];
        parameters.extend(
            decision
                .networks
                .iter()
                .map(|network| network.as_str().to_owned()),
        );
        let previous: Option<String> = transaction
            .query_row(
                &format!(
                    "SELECT state FROM consent_state \
                     WHERE subject_type = ? AND subject_id = ? AND network IN ({placeholders}) \
                     ORDER BY decision_sequence DESC LIMIT 1"
                ),
                rusqlite::params_from_iter(parameters.iter()),
                |row| row.get(0),
            )
            .optional()
            .context("failed to read the subject's previous consent state")?;
        let old_state = match previous.as_deref() {
            Some(state) => OldState::parse(state)
                .with_context(|| format!("the journal holds the unknown state {state:?}"))?,
            None => OldState::Unset,
        };

        let recorded = Recorded {
            decision: decision.clone(),
            old_state,
            occurred_at: occurred_at.to_owned(),
            actor: actor.to_owned(),
        };
        let event_id = recorded.event_id();
        let envelope = recorded.envelope(domain, produced_at);

        let inserted = transaction
            .execute(
                "INSERT INTO consent_decision \
                 (event_id, subject_type, subject_id, old_state, new_state, scope_key, \
                  occurred_at, actor, reason, envelope) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (event_id) DO NOTHING",
                rusqlite::params![
                    event_id,
                    decision.subject.kind.as_str(),
                    decision.subject.id,
                    old_state.as_str(),
                    decision.new_state.as_str(),
                    decision.scope_key(),
                    occurred_at,
                    actor,
                    decision.reason,
                    serde_json::to_string(&envelope)?,
                ],
            )
            .context("failed to append the decision to the journal")?;
        if inserted == 0 {
            // The identical decision, arriving twice: same subject, same
            // state, same perimeter, same instant. The journal keeps the
            // first one and the answer names its id, so a retried request
            // neither duplicates the decision nor invents a second event.
            let (sequence, recorded_old_state): (i64, String) = transaction
                .query_row(
                    "SELECT sequence, old_state FROM consent_decision WHERE event_id = ?",
                    [&event_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .context("failed to read the already-recorded decision")?;
            transaction
                .commit()
                .context("failed to commit the decision transaction")?;
            return Ok(Committed {
                sequence,
                event_id,
                recorded: Recorded {
                    old_state: OldState::parse(&recorded_old_state).unwrap_or(OldState::Unset),
                    ..recorded
                },
                replayed: true,
            });
        }
        let sequence = transaction.last_insert_rowid();
        for network in &decision.networks {
            transaction
                .execute(
                    "INSERT INTO consent_decision_network (sequence, network) VALUES (?, ?)",
                    rusqlite::params![sequence, network.as_str()],
                )
                .context("failed to append the decision's scope")?;
        }
        transaction
            .commit()
            .context("failed to commit the decision transaction")?;
        Ok(Committed {
            sequence,
            event_id,
            recorded,
            replayed: false,
        })
    }

    /// The current state: one entry per (subject, network), as recorded.
    /// Network entries are the defaults; contact entries override them. This
    /// is the projection #50 will serve as a snapshot alongside the stream
    /// position it reflects.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT subject_type, subject_id, network, state, decided_at, decision_sequence \
                 FROM consent_state ORDER BY subject_type, subject_id, network",
            )
            .context("failed to prepare the current-state query")?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .context("failed to read the current state")?;
        let mut entries = Vec::new();
        for row in rows {
            let (kind, id, network, state, decided_at, decision_sequence) =
                row.context("failed to read a current-state row")?;
            entries.push(Entry {
                subject: Subject {
                    kind: SubjectType::parse(&kind)
                        .with_context(|| format!("the journal holds the subject type {kind:?}"))?,
                    id,
                },
                network: Network::parse(&network)
                    .with_context(|| format!("the journal holds the network {network:?}"))?,
                state: State::parse(&state)
                    .with_context(|| format!("the journal holds the state {state:?}"))?,
                decided_at,
                decision_sequence,
            });
        }
        Ok(entries)
    }

    /// The effective consent state of a contact on one network, with the
    /// precedence applied: the contact's own decision if it has one, the
    /// network's default otherwise, and `pending` when neither exists.
    pub fn effective(&self, contact: &str, network: Network) -> Result<Effective> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT state FROM consent_state \
                 WHERE subject_type = ? AND subject_id = ? AND network = ?",
            )
            .context("failed to prepare the precedence query")?;
        let mut read = |kind: SubjectType, id: &str| -> Result<Option<State>> {
            let state: Option<String> = statement
                .query_row(
                    rusqlite::params![kind.as_str(), id, network.as_str()],
                    |row| row.get(0),
                )
                .optional()
                .context("failed to read a consent state")?;
            match state {
                Some(state) => {
                    Ok(Some(State::parse(&state).with_context(|| {
                        format!("the journal holds the state {state:?}")
                    })?))
                }
                None => Ok(None),
            }
        };
        let contact_state = read(SubjectType::Contact, contact)?;
        let default_state = read(SubjectType::Network, network.as_str())?;
        Ok(Effective::resolve(
            contact,
            network,
            contact_state,
            default_state,
        ))
    }

    /// The committed decisions the outbox has not published yet, oldest
    /// first: what a restart finds and publishes, and what a bus outage
    /// accumulates.
    pub fn unpublished(&self, limit: usize) -> Result<Vec<Unpublished>> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT sequence, event_id, envelope FROM consent_decision \
                 WHERE published_at IS NULL ORDER BY sequence LIMIT ?",
            )
            .context("failed to prepare the outbox query")?;
        let rows = statement
            .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .context("failed to read the outbox")?;
        let mut pending = Vec::new();
        for row in rows {
            let (sequence, event_id, envelope) = row.context("failed to read an outbox row")?;
            pending.push(Unpublished {
                sequence,
                event_id,
                envelope: serde_json::from_str(&envelope)
                    .with_context(|| format!("decision {sequence} holds an unreadable envelope"))?,
            });
        }
        Ok(pending)
    }

    /// How many committed decisions are still waiting for the bus — the
    /// operator's one number for "is the outbox draining?".
    pub fn unpublished_count(&self) -> Result<u64> {
        let connection = self.connection();
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM consent_decision WHERE published_at IS NULL",
                [],
                |row| row.get(0),
            )
            .context("failed to count the outbox")?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// Marks a decision as published. The only mutation the journal allows,
    /// and the one whose loss is survivable: a row published but not marked
    /// is republished, and the bus deduplicates it on `Nats-Msg-Id`.
    pub fn mark_published(&self, sequence: i64, published_at: &str) -> Result<()> {
        self.connection()
            .execute(
                "UPDATE consent_decision SET published_at = ? \
                 WHERE sequence = ? AND published_at IS NULL",
                rusqlite::params![published_at, sequence],
            )
            .with_context(|| format!("failed to mark decision {sequence} published"))?;
        Ok(())
    }

    fn connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .expect("the consent store mutex is never poisoned")
    }
}

/// Narrows a path to its owner. Best effort: a store on a filesystem that
/// cannot express it (a bind mount from a foreign host) is a warning, not a
/// reason to refuse to serve consent.
fn restrict_to_owner(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
        tracing::warn!(
            path = %path.display(),
            %error,
            "could not restrict the consent store's permissions to its owner"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OWNER: &str = "@michel:example.com";
    const DOMAIN: &str = "example.com";

    fn store(test_name: &str) -> Store {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "twalk-consent-store-{test_name}-{}-{unique}",
            std::process::id()
        ));
        Store::open(&dir).expect("the store opens")
    }

    fn decision(kind: SubjectType, id: &str, state: State, networks: &[Network]) -> Decision {
        Decision {
            subject: Subject {
                kind,
                id: id.to_owned(),
            },
            new_state: state,
            networks: networks.to_vec(),
            reason: None,
        }
    }

    fn record(store: &Store, decision: &Decision, occurred_at: &str) -> Committed {
        store
            .record(decision, OWNER, occurred_at, DOMAIN, occurred_at)
            .expect("the decision records")
    }

    #[test]
    fn a_fresh_store_is_migrated_to_the_current_schema_and_reopens_unchanged() {
        let store = store("migrations");
        let version: i64 = store
            .connection()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version as usize, MIGRATIONS.len());
        let path = store.path().parent().unwrap().to_path_buf();
        drop(store);
        // Reopening applies nothing and loses nothing: the migrations are
        // idempotent against an already-migrated file.
        let reopened = Store::open(&path).expect("the store reopens");
        let version: i64 = reopened
            .connection()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version as usize, MIGRATIONS.len());
    }

    #[test]
    fn the_first_decision_on_a_perimeter_moves_from_unset() {
        let store = store("unset");
        let committed = record(
            &store,
            &decision(
                SubjectType::Contact,
                "@a:example.com",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:00:00.000Z",
        );
        assert_eq!(committed.recorded.old_state, OldState::Unset);
        assert!(!committed.replayed);
        assert_eq!(committed.event_id.len(), 64);
    }

    #[test]
    fn the_journal_is_append_only() {
        let store = store("append-only");
        record(
            &store,
            &decision(
                SubjectType::Contact,
                "@a:example.com",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:00:00.000Z",
        );
        let connection = store.connection();
        let deleted = connection.execute("DELETE FROM consent_decision", []);
        assert!(deleted.is_err(), "a decision cannot be deleted");
        let rewritten = connection.execute(
            "UPDATE consent_decision SET new_state = 'granted' WHERE sequence = 1",
            [],
        );
        assert!(rewritten.is_err(), "a decision cannot be rewritten");
        // The outbox's own column is the exception, and the only one.
        connection
            .execute(
                "UPDATE consent_decision SET published_at = '2026-09-17T10:00:01Z'",
                [],
            )
            .expect("the outbox marks its own progress");
    }

    #[test]
    fn the_current_state_is_the_latest_decision_per_subject_and_network() {
        let store = store("projection");
        let contact = decision(
            SubjectType::Contact,
            "@a:example.com",
            State::Granted,
            &[Network::Whatsapp, Network::Signal],
        );
        record(&store, &contact, "2026-09-17T10:00:00.000Z");
        let revoked = decision(
            SubjectType::Contact,
            "@a:example.com",
            State::Revoked,
            &[Network::Whatsapp],
        );
        let second = record(&store, &revoked, "2026-09-17T10:01:00.000Z");
        // The second decision saw the first: the perimeters overlap.
        assert_eq!(second.recorded.old_state, OldState::Was(State::Granted));

        let entries = store.entries().unwrap();
        assert_eq!(entries.len(), 2, "{entries:?}");
        let whatsapp = entries
            .iter()
            .find(|entry| entry.network == Network::Whatsapp)
            .unwrap();
        assert_eq!(whatsapp.state, State::Revoked);
        let signal = entries
            .iter()
            .find(|entry| entry.network == Network::Signal)
            .unwrap();
        assert_eq!(
            signal.state,
            State::Granted,
            "the narrower decision left the other network alone"
        );
    }

    #[test]
    fn a_contact_decision_overrides_the_network_default_on_read() {
        let store = store("precedence");
        record(
            &store,
            &decision(
                SubjectType::Network,
                "whatsapp",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:00:00.000Z",
        );
        record(
            &store,
            &decision(
                SubjectType::Contact,
                "@loud:example.com",
                State::Revoked,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:01:00.000Z",
        );

        let overridden = store
            .effective("@loud:example.com", Network::Whatsapp)
            .unwrap();
        assert_eq!(overridden.state, State::Revoked);
        assert_eq!(
            overridden.decided_by.unwrap().kind,
            SubjectType::Contact,
            "the contact's own decision is what answered"
        );
        let defaulted = store
            .effective("@quiet:example.com", Network::Whatsapp)
            .unwrap();
        assert_eq!(defaulted.state, State::Granted);
        assert_eq!(defaulted.decided_by.unwrap().kind, SubjectType::Network);
        // Another network the user never decided about stays pending, and
        // names no decision: "never decided" is not "revoked".
        let undecided = store
            .effective("@loud:example.com", Network::Telegram)
            .unwrap();
        assert_eq!(undecided.state, State::Pending);
        assert_eq!(undecided.decided_by, None);
    }

    #[test]
    fn the_identical_decision_arriving_twice_records_once() {
        let store = store("idempotent");
        let same = decision(
            SubjectType::Contact,
            "@a:example.com",
            State::Granted,
            &[Network::Whatsapp],
        );
        let first = record(&store, &same, "2026-09-17T10:00:00.000Z");
        let second = record(&store, &same, "2026-09-17T10:00:00.000Z");
        assert!(!first.replayed);
        assert!(second.replayed);
        assert_eq!(first.event_id, second.event_id);
        assert_eq!(first.sequence, second.sequence);
        assert_eq!(store.unpublished(10).unwrap().len(), 1);
    }

    #[test]
    fn a_committed_decision_waits_in_the_outbox_until_it_is_marked() {
        let store = store("outbox");
        let committed = record(
            &store,
            &decision(
                SubjectType::Contact,
                "@a:example.com",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:00:00.000Z",
        );
        let pending = store.unpublished(10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event_id, committed.event_id);
        // The envelope is stored rendered, so a republish is byte-identical.
        assert_eq!(
            pending[0].envelope["id"].as_str(),
            Some(committed.event_id.as_str())
        );
        assert_eq!(
            pending[0].envelope["source"].as_str(),
            Some("gateway://example.com/consent")
        );
        assert_eq!(store.unpublished_count().unwrap(), 1);

        store
            .mark_published(committed.sequence, "2026-09-17T10:00:01.000Z")
            .unwrap();
        assert!(store.unpublished(10).unwrap().is_empty());
        assert_eq!(store.unpublished_count().unwrap(), 0);
    }

    #[test]
    fn a_decision_survives_the_store_being_reopened() {
        let store = store("durable");
        record(
            &store,
            &decision(
                SubjectType::Network,
                "matrix",
                State::Granted,
                &[Network::Matrix],
            ),
            "2026-09-17T10:00:00.000Z",
        );
        let dir = store.path().parent().unwrap().to_path_buf();
        drop(store);
        let reopened = Store::open(&dir).unwrap();
        assert_eq!(
            reopened
                .effective("@someone:example.com", Network::Matrix)
                .unwrap()
                .state,
            State::Granted
        );
        assert_eq!(
            reopened.unpublished(10).unwrap().len(),
            1,
            "an unpublished decision is still waiting after a restart"
        );
    }

    #[test]
    fn the_stored_envelope_carries_what_the_contract_expects() {
        let store = store("envelope");
        let committed = record(
            &store,
            &decision(
                SubjectType::Contact,
                "@a:example.com",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:00:00.000Z",
        );
        let envelope = store.unpublished(1).unwrap().remove(0).envelope;
        assert_eq!(envelope["data"]["old_state"].as_str(), Some("unset"));
        assert_eq!(envelope["data"]["actor"].as_str(), Some(OWNER));
        assert_eq!(envelope["network"].as_str(), Some("whatsapp"));
        assert_eq!(envelope["data"]["scope"]["networks"], json!(["whatsapp"]));
        assert_eq!(envelope["id"].as_str(), Some(committed.event_id.as_str()));
    }
}
