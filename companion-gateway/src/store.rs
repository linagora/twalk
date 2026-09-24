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
//! Ticket #50 added a fourth, which is what makes a cold consumer's hand-off
//! safe: **the snapshot's position is consistent with its content**
//! ([`Store::snapshot`]). The journal remembers where on the bus each
//! published decision landed (`stream_sequence`), and the snapshot is the
//! state of the *published prefix* of the journal, read in one transaction
//! with the position of its last decision. See that method for why the
//! prefix, and not the journal's head, is the only honest answer.
//!
//! Ticket #54 added a table on the same terms as #56's, and it is governed by
//! one rule rather than three: **`contact_seen` holds four columns and will
//! hold four columns**. It is the list of who has written to the user, kept
//! so the Companion can show what is waiting for a decision, and its
//! restraint is the only thing between it and a surveillance log — no body,
//! no display name, no `network_identifier`. See the migration's own comment
//! and [`crate::contacts`].
//!
//! Ticket #24 added `approval`, and it is the table where the rule above was
//! most tempting to break: an approval is a message being sent, so the
//! obvious row would carry the text. It does not. An approval here is the
//! suggestion's id, the owner who approved it, a boolean saying whether they
//! edited it, and the position the publication landed at on the bus. The text
//! lives on the bus, where the retention is declared, and is read back from
//! there — never from this file. See the migration's own comment and
//! [`crate::approval`].
//!
//! Ticket #121 added `disclosure_decision`, and it is the one table here
//! that is a journal *because* it is not a setting: ADR 0019 requires that
//! turning off the sentence a persona's reply discloses itself with be a
//! recorded deliberate act, so it is append-only by the consent journal's own
//! triggers, carries the same `occurred_at`/`actor`/`reason`, and its current
//! state is its last row — no row meaning **on**. It is deliberately neither
//! the consent journal itself nor the settings table (ADR 0031); see
//! [`crate::disclosure`].
//!
//! The schema migrations are embedded in the binary ([`MIGRATIONS`]) and
//! applied at open, so an operator upgrades the image and nothing else.
//!
//! No message content is stored here, ever: a decision is a subject, a state,
//! a perimeter, two timestamps and the owner who took it, and an approval is
//! an id, an owner, a boolean and a position.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::bridge_status::ContractState;
use crate::consent::{
    Decision, Effective, Network, OldState, Recorded, State, Subject, SubjectType,
};
use crate::disclosure::DisclosureState;
use crate::owner::Owner;
use crate::switch::State as SwitchState;

/// The store's file inside the Gateway's state directory. A companion `-wal`
/// and `-shm` sit next to it: the journal is opened in WAL mode, so a reader
/// never blocks the writer that is recording a decision.
const DATABASE_FILE: &str = "consent.sqlite3";

/// The schema, one statement batch per version. The applied version is the
/// database's `user_version`; a new migration is appended to this array and
/// never edited in place, so an existing store upgrades by applying exactly
/// the tail it has not seen.
pub const MIGRATIONS: [&str; 13] = [
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
    // v2 — where on the bus each published decision landed, and the snapshot
    // a cold consumer reads (ticket #50).
    r#"
    -- The JetStream sequence the bus stored this decision at, written by the
    -- outbox when the publication is acked; NULL until then. It is not part
    -- of the decision — the append-only trigger above deliberately does not
    -- list it — but it is what lets the snapshot name a position a consumer
    -- can start a stream consumer at (ADR 0010).
    ALTER TABLE consent_decision ADD COLUMN stream_sequence INTEGER;

    CREATE INDEX consent_decision_unpositioned
        ON consent_decision (sequence) WHERE stream_sequence IS NULL;

    -- How far into the journal the snapshot may read: the last decision such
    -- that every decision up to it has a known position on the bus.
    --
    -- The outbox publishes in journal order and awaits each ack before the
    -- next, so the positioned rows are a prefix of the journal and the bus's
    -- order is the journal's. This horizon is that prefix's end: the first
    -- row with no position, minus one; the whole journal when every row has
    -- one; zero when the journal is empty or nothing has reached the bus yet.
    --
    -- A row an older build marked published without recording a position
    -- (schema v1 kept only `published_at`) counts as unpositioned, which is
    -- the safe direction: it holds the horizon back rather than naming a
    -- sequence nobody knows.
    CREATE VIEW consent_snapshot_horizon AS
    SELECT COALESCE(
        (SELECT MIN(sequence) - 1 FROM consent_decision WHERE stream_sequence IS NULL),
        (SELECT MAX(sequence) FROM consent_decision),
        0
    ) AS decision_sequence;

    -- The snapshot: the current state of the journal's published prefix, one
    -- entry per (subject, network), revocations as explicit as grants,
    -- network defaults included, `persona` subjects excluded — persona
    -- activation is a consent decision (ADR 0013) but it is not part of the
    -- consent state a Sensor labels senders by, and #60 is what writes those.
    --
    -- Excluded in SQL rather than in Rust so that the snapshot never even
    -- reads a persona row: this is the projection a consumer's whole cold
    -- start rests on.
    CREATE VIEW consent_snapshot AS
    SELECT subject_type, subject_id, network, state, decided_at, decision_sequence
    FROM (
        SELECT d.subject_type, d.subject_id, n.network, d.new_state AS state,
               d.occurred_at AS decided_at, d.sequence AS decision_sequence,
               ROW_NUMBER() OVER (
                   PARTITION BY d.subject_type, d.subject_id, n.network
                   ORDER BY d.sequence DESC
               ) AS recency
        FROM consent_decision d
        JOIN consent_decision_network n ON n.sequence = d.sequence
        WHERE d.subject_type <> 'persona'
          AND d.sequence <= (SELECT decision_sequence FROM consent_snapshot_horizon)
    )
    WHERE recency = 1;
    "#,
    // v3 — bridge state changes and their own outbox rows (ticket #56).
    //
    // In the same store, and drained by the same publication loop, on
    // purpose: the outbox is the one mechanism in this service that gets
    // exactly-once right, and a second publisher would be a second thing to
    // get it wrong in. What this table is *not* is a second consent journal —
    // it is operational history, so it carries no append-only trigger and
    // nothing here is an audit trail of a promise to the user.
    //
    // The row is also the memory that makes de-duplication survive a
    // restart: `from_state` after a Gateway restart is the state the bridge
    // was really in, not whatever the first push after the restart assumed.
    r#"
    CREATE TABLE bridge_status_change (
        sequence        INTEGER PRIMARY KEY AUTOINCREMENT,
        -- The contract's deterministic id. UNIQUE makes recording the
        -- identical transition twice idempotent instead of doubling it.
        event_id        TEXT NOT NULL UNIQUE,
        -- The contract's bridge_id (^bridge-[a-z0-9-]+$), from configuration.
        bridge_id       TEXT NOT NULL,
        network         TEXT NOT NULL CHECK (network IN ('whatsapp', 'telegram', 'signal', 'discord', 'sms', 'matrix')),
        from_state      TEXT NOT NULL CHECK (from_state IN ('starting', 'connected', 'degraded', 'disconnected', 'session_expired')),
        to_state        TEXT NOT NULL CHECK (to_state IN ('starting', 'connected', 'degraded', 'disconnected', 'session_expired')),
        occurred_at     TEXT NOT NULL,
        reason          TEXT,
        last_message_at TEXT,
        -- The rendered CloudEvent, published verbatim.
        envelope        TEXT NOT NULL,
        published_at    TEXT,
        stream_sequence INTEGER
    );

    CREATE INDEX bridge_status_change_bridge
        ON bridge_status_change (bridge_id, sequence);
    CREATE INDEX bridge_status_change_unpublished
        ON bridge_status_change (sequence) WHERE published_at IS NULL;

    -- Each bridge's last known state: what a new observation is compared
    -- with, and the `from_state` of the next transition.
    CREATE VIEW bridge_status_current AS
    SELECT bridge_id, network, to_state AS state, occurred_at, reason,
           last_message_at, sequence
    FROM (
        SELECT c.*, ROW_NUMBER() OVER (
                   PARTITION BY c.bridge_id ORDER BY c.sequence DESC
               ) AS recency
        FROM bridge_status_change c
    )
    WHERE recency = 1;
    "#,
    // v4 — who has written to the user, and the list of them nobody has
    // decided about yet (ticket #54).
    r#"
    -- The pending-contact projection's whole store. Four columns, and it is
    -- meant to stay four columns.
    --
    -- This table is a list of the people who write to the user: a social
    -- graph with timestamps. The only thing keeping it from being a
    -- surveillance log is what it refuses to hold — no message body, no
    -- display name, no `network_identifier`. The Sensor withholds the
    -- network identifier until consent is granted (the contract's
    -- `data.contact.network_identifier`), and the Gateway must not undo that
    -- by keeping a copy of its own. A display name is read from the bus when
    -- a screen needs one and is never written here
    -- (`crate::contacts::Contacts::display_names`).
    --
    -- The uncomfortable part, stated where the columns are rather than only
    -- in a document: a bridged ghost user's Matrix ID conventionally embeds
    -- the network identifier (`@whatsapp_33612345678:example.com`), so
    -- storing the ID is not as neutral as "an opaque handle" sounds — this
    -- table keeps the phone number even though it has no column for one. It
    -- is stored anyway because a consent decision has to name its subject
    -- and that ID *is* the subject; what the schema can do is hold nothing
    -- else. `docs/architecture/security-model.md`, residual risk 5, says the
    -- same thing to an operator deciding whether to run this.
    CREATE TABLE contact_seen (
        -- The sender's Matrix user ID, as the bridge materialised it: the
        -- contract's `subject`, and the id a decision about this contact
        -- will name.
        contact_id TEXT NOT NULL,
        -- The network it wrote on, as the user experiences it (ADR 0005),
        -- `matrix` included (ADR 0009) — the contract's `network`
        -- extension, never a bridge id.
        network    TEXT NOT NULL CHECK (network IN ('whatsapp', 'telegram', 'signal', 'discord', 'sms', 'matrix')),
        -- When this contact first and last wrote, from the event's own
        -- `time` and never from the Gateway's clock: a Gateway installed
        -- after weeks of Sensor traffic builds this table from the stream's
        -- history, and stamping "now" on all of it would make every old
        -- contact look new.
        first_seen TEXT NOT NULL,
        last_seen  TEXT NOT NULL,
        PRIMARY KEY (contact_id, network)
    ) WITHOUT ROWID;

    -- The pending list: every seen contact that no decision covers on the
    -- network it wrote on.
    --
    -- "No decision" is the same question `GET /api/consent/effective`
    -- answers with `decided_by: null` — neither the contact's own decision
    -- nor the network's default exists — so a network default the user set
    -- takes every contact on that network out of this list at once, which is
    -- exactly what the default is for. A contact decided `pending`
    -- explicitly is *not* in this list: the user answered, and the answer
    -- was "not yet".
    --
    -- A view rather than a table, for the same reason `consent_state` is
    -- one: there is no second copy to keep in step, and a decision recorded
    -- through the write API moves the contact out of the list by arithmetic
    -- rather than by someone remembering to update a projection.
    CREATE VIEW pending_contact AS
    SELECT s.contact_id, s.network, s.first_seen, s.last_seen
    FROM contact_seen s
    WHERE NOT EXISTS (
        SELECT 1 FROM consent_state c
        WHERE c.network = s.network
          AND ((c.subject_type = 'contact' AND c.subject_id = s.contact_id)
            OR (c.subject_type = 'network' AND c.subject_id = s.network))
    );
    "#,
    // v5 — the approvals this Gateway published (ticket #24).
    //
    // Read the columns before reading anything else about this table: there
    // is **no column for the reply's text**, and there is not going to be
    // one. An approval is the act of sending a message, and the message
    // itself belongs on the bus, which is where the audit trail lives and
    // where the retention is declared. What this table answers is the one
    // question the bus answers slowly: "the reply I approved — did it
    // actually go out, and where?" It holds the suggestion's id, the person
    // who approved it, whether they edited what the persona wrote, and the
    // stream position the publication landed at.
    //
    // This is **not** an outbox. `consent_decision` and
    // `bridge_status_change` are written before they are published and
    // drained later, because a decision the user took must survive a bus
    // outage. An approval must not: a send held for later publication is a
    // message the Gateway has promised to deliver after the consent it
    // checked may have been revoked. So a row here is written unpublished,
    // published inside the same request, and marked — and a row left
    // unmarked by a crash is repaired by the next approval of the same
    // suggestion, which republishes under the contract's deterministic id
    // and is deduplicated by the bus rather than sent twice.
    //
    // No append-only trigger: this is operational bookkeeping, not the
    // journal of a promise to the user. The promise's audit trail is
    // `persona.reply.approved.v1` on the bus.
    r#"
    CREATE TABLE approval (
        sequence            INTEGER PRIMARY KEY AUTOINCREMENT,
        -- The contract's deterministic id,
        -- sha256(suggestion_event_id:approved_by). UNIQUE is the rule "a
        -- given suggestion is approved at most once by a given user", in the
        -- schema rather than in a comment.
        event_id            TEXT NOT NULL UNIQUE,
        -- The persona.suggest.produced event that was approved.
        suggestion_event_id TEXT NOT NULL,
        -- The Matrix ID of the human who approved it: this deployment's
        -- owner, from configuration and never from the request body.
        approved_by         TEXT NOT NULL,
        -- Which persona proposed it, for an operator reading the table.
        persona_id          TEXT NOT NULL,
        network             TEXT NOT NULL CHECK (network IN ('whatsapp', 'telegram', 'signal', 'discord', 'sms', 'matrix')),
        -- The contact the reply goes to. Already in `contact_seen` — this
        -- table adds no new category of data — and kept here because it is
        -- what the consent check was made against.
        contact             TEXT NOT NULL,
        -- Whether the final content differed from what the persona wrote.
        -- A boolean, not the text: "how often do I correct my assistant?"
        -- is answerable without keeping a word of what was said.
        edited              INTEGER NOT NULL CHECK (edited IN (0, 1)),
        approved_at         TEXT NOT NULL,
        -- NULL until the bus acknowledged the publication, and then the
        -- instant and the position it landed at. A row that stays NULL is a
        -- crash between the write and the publish, and it is visible on
        -- purpose.
        published_at        TEXT,
        stream_sequence     INTEGER,
        UNIQUE (suggestion_event_id, approved_by)
    );

    CREATE INDEX approval_suggestion ON approval (suggestion_event_id);
    "#,
    // v6 — the moves the portal register decided on (issue #255, ADR 0029).
    //
    // One row per conversation whose room was replaced while the Sensor was
    // in it, keyed on the successor: what the register did about it —
    // followed the decision there, or returned it to the chooser because the
    // successor's audience crossed the threshold — and the numbers it did it
    // on. A deployment that changed rooms under the user without being able
    // to say so is one whose history they cannot check; this is where it is
    // said. Keyed on the successor so that a move is decided once, however
    // many times the register reads the same tombstone.
    r#"
    CREATE TABLE portal_move (
        successor       TEXT NOT NULL PRIMARY KEY,
        predecessor     TEXT NOT NULL,
        bridge_id       TEXT NOT NULL,
        members         INTEGER NOT NULL,
        crowd_threshold INTEGER NOT NULL,
        followed        INTEGER NOT NULL CHECK (followed IN (0, 1)),
        decided_at      TEXT NOT NULL
    ) WITHOUT ROWID;

    CREATE INDEX portal_move_decided_at ON portal_move (decided_at);
    "#,
    // v7 — the registry of connections (ADR 0033, issue #269).
    //
    // What the Gateway is configured with, written at every start so the
    // store always holds the registry the running Gateway serves: the next
    // migration (#270) keys consent on it, and a decision must be able to
    // reference a connection the store knows. `kind` is not CHECKed: the
    // contract's definition is the authority and the Gateway refuses an
    // unknown kind at startup, before anything reaches this table.
    r#"
    CREATE TABLE connection (
        id         TEXT NOT NULL PRIMARY KEY,
        kind       TEXT NOT NULL,
        label      TEXT NOT NULL,
        bridge_id  TEXT,
        created_at TEXT NOT NULL
    ) WITHOUT ROWID;
    "#,
    // v8 — consent is keyed on the connection (ADR 0033, issue #270): the
    // expand step of the wide refactor #251 sequences.
    //
    // A decision's scope becomes a set of connections, held in a table of
    // the same shape as the network one, and every projection is rewritten
    // over it: `consent_state`, `consent_snapshot`, `pending_contact`, and
    // the `contact_seen` table they join. `network` stays on every one of
    // them as a derived column — the connection's kind, from the registry
    // table v7 made — so that a consumer not yet migrated reads what it read
    // before. `consent_decision_network` is not dropped: it is part of the
    // journal's history, append-only by its own triggers, and after this
    // migration nothing writes it.
    //
    // **The migration of the decisions already taken.** Every scope row is
    // copied onto the connection whose id is its network's name — the id the
    // registry derives for a deployment with one bridge per network, and the
    // id #269 fixed on purpose so that this copy is a rename and not a
    // guess. It is correct only because it is done now, while every
    // deployment has exactly one connection per network (ADR 0033); a second
    // account of one network declared later starts with no decision, which
    // is the safe direction. The registry rows those ids need are inserted
    // here too, kind and label both the network's name, so a store migrated
    // before its Gateway starts still joins — the Gateway's own registry
    // refreshes the label at its next start.
    //
    // The `CHECK`s on `network` were copies of the contract frozen at v1 and
    // would refuse `email` (#268). SQLite cannot alter a constraint, so the
    // two tables that still carry one and are still written, `approval` and
    // `bridge_status_change`, are rebuilt with the contract's current list;
    // `contact_seen` is rebuilt anyway, keyed on the connection, and carries
    // no network column at all any more — four columns, still.
    r#"
    CREATE TABLE consent_decision_connection (
        sequence   INTEGER NOT NULL REFERENCES consent_decision(sequence),
        connection TEXT NOT NULL REFERENCES connection(id),
        PRIMARY KEY (sequence, connection)
    ) WITHOUT ROWID;
    CREATE TRIGGER consent_decision_connection_no_delete
    BEFORE DELETE ON consent_decision_connection
    BEGIN
        SELECT RAISE(ABORT, 'the consent decision journal is append-only');
    END;
    CREATE TRIGGER consent_decision_connection_no_update
    BEFORE UPDATE ON consent_decision_connection
    BEGIN
        SELECT RAISE(ABORT, 'the consent decision journal is append-only');
    END;

    INSERT OR IGNORE INTO connection (id, kind, label, created_at)
    SELECT network, network, network, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
    FROM (SELECT network FROM consent_decision_network
          UNION SELECT network FROM contact_seen);
    INSERT INTO consent_decision_connection (sequence, connection)
    SELECT sequence, network FROM consent_decision_network;

    DROP VIEW pending_contact;
    DROP VIEW consent_snapshot;
    DROP VIEW consent_state;

    CREATE VIEW consent_state AS
    SELECT subject_type, subject_id, connection, network, new_state AS state,
           occurred_at AS decided_at, sequence AS decision_sequence
    FROM (
        SELECT d.subject_type, d.subject_id, c.connection, k.kind AS network,
               d.new_state, d.occurred_at, d.sequence,
               ROW_NUMBER() OVER (
                   PARTITION BY d.subject_type, d.subject_id, c.connection
                   ORDER BY d.sequence DESC
               ) AS recency
        FROM consent_decision d
        JOIN consent_decision_connection c ON c.sequence = d.sequence
        JOIN connection k ON k.id = c.connection
    )
    WHERE recency = 1;

    CREATE VIEW consent_snapshot AS
    SELECT subject_type, subject_id, connection, network, state, decided_at, decision_sequence
    FROM (
        SELECT d.subject_type, d.subject_id, c.connection, k.kind AS network,
               d.new_state AS state, d.occurred_at AS decided_at,
               d.sequence AS decision_sequence,
               ROW_NUMBER() OVER (
                   PARTITION BY d.subject_type, d.subject_id, c.connection
                   ORDER BY d.sequence DESC
               ) AS recency
        FROM consent_decision d
        JOIN consent_decision_connection c ON c.sequence = d.sequence
        JOIN connection k ON k.id = c.connection
        WHERE d.subject_type <> 'persona'
          AND d.sequence <= (SELECT decision_sequence FROM consent_snapshot_horizon)
    )
    WHERE recency = 1;

    CREATE TABLE contact_seen_v8 (
        contact_id TEXT NOT NULL,
        -- The connection it wrote on (ADR 0033): the perimeter a decision
        -- about this contact is scoped to. Its kind is the network.
        connection TEXT NOT NULL REFERENCES connection(id),
        first_seen TEXT NOT NULL,
        last_seen  TEXT NOT NULL,
        PRIMARY KEY (contact_id, connection)
    ) WITHOUT ROWID;
    INSERT INTO contact_seen_v8 (contact_id, connection, first_seen, last_seen)
    SELECT contact_id, network, first_seen, last_seen FROM contact_seen;
    DROP TABLE contact_seen;
    ALTER TABLE contact_seen_v8 RENAME TO contact_seen;

    -- The pending list, keyed on the connection: a network default taken
    -- on that connection covers every contact on it, a contact's own
    -- decision on it covers the contact.
    CREATE VIEW pending_contact AS
    SELECT s.contact_id, s.connection, k.kind AS network, s.first_seen, s.last_seen
    FROM contact_seen s
    JOIN connection k ON k.id = s.connection
    WHERE NOT EXISTS (
        SELECT 1 FROM consent_state c
        WHERE c.connection = s.connection
          AND ((c.subject_type = 'contact' AND c.subject_id = s.contact_id)
            OR (c.subject_type = 'network' AND c.subject_id = k.kind))
    );

    CREATE TABLE approval_v8 (
        sequence            INTEGER PRIMARY KEY AUTOINCREMENT,
        event_id            TEXT NOT NULL UNIQUE,
        suggestion_event_id TEXT NOT NULL,
        approved_by         TEXT NOT NULL,
        persona_id          TEXT NOT NULL,
        network             TEXT NOT NULL CHECK (network IN ('whatsapp', 'telegram', 'signal', 'discord', 'sms', 'matrix', 'email')),
        contact             TEXT NOT NULL,
        edited              INTEGER NOT NULL CHECK (edited IN (0, 1)),
        approved_at         TEXT NOT NULL,
        published_at        TEXT,
        stream_sequence     INTEGER,
        UNIQUE (suggestion_event_id, approved_by)
    );
    INSERT INTO approval_v8 SELECT * FROM approval;
    DROP TABLE approval;
    ALTER TABLE approval_v8 RENAME TO approval;
    CREATE INDEX approval_suggestion ON approval (suggestion_event_id);

    DROP VIEW bridge_status_current;
    CREATE TABLE bridge_status_change_v8 (
        sequence        INTEGER PRIMARY KEY AUTOINCREMENT,
        event_id        TEXT NOT NULL UNIQUE,
        bridge_id       TEXT NOT NULL,
        network         TEXT NOT NULL CHECK (network IN ('whatsapp', 'telegram', 'signal', 'discord', 'sms', 'matrix', 'email')),
        from_state      TEXT NOT NULL CHECK (from_state IN ('starting', 'connected', 'degraded', 'disconnected', 'session_expired')),
        to_state        TEXT NOT NULL CHECK (to_state IN ('starting', 'connected', 'degraded', 'disconnected', 'session_expired')),
        occurred_at     TEXT NOT NULL,
        reason          TEXT,
        last_message_at TEXT,
        envelope        TEXT NOT NULL,
        published_at    TEXT,
        stream_sequence INTEGER
    );
    INSERT INTO bridge_status_change_v8 SELECT * FROM bridge_status_change;
    DROP TABLE bridge_status_change;
    ALTER TABLE bridge_status_change_v8 RENAME TO bridge_status_change;
    CREATE INDEX bridge_status_change_bridge
        ON bridge_status_change (bridge_id, sequence);
    CREATE INDEX bridge_status_change_unpublished
        ON bridge_status_change (sequence) WHERE published_at IS NULL;
    CREATE VIEW bridge_status_current AS
    SELECT bridge_id, network, to_state AS state, occurred_at, reason,
           last_message_at, sequence
    FROM (
        SELECT c.*, ROW_NUMBER() OVER (
                   PARTITION BY c.bridge_id ORDER BY c.sequence DESC
               ) AS recency
        FROM bridge_status_change c
    )
    WHERE recency = 1;
    "#,
    // v9 — what a connection said about itself (issue #275): every
    // `connection.status.changed.v1` the collector published, consumed off
    // the bus, and the current state as a view over them. The Gateway is a
    // reader here, not the producer: a collector holds the connection and
    // says its state; the Gateway keeps it so an approval towards a
    // connection that cannot send is refused before it is published, and
    // the Companion shows the state and its hint.
    r#"
    CREATE TABLE connection_status_change (
        sequence     INTEGER PRIMARY KEY AUTOINCREMENT,
        -- The contract's deterministic id: recording a redelivered
        -- transition twice is idempotent.
        event_id     TEXT NOT NULL UNIQUE,
        connection   TEXT NOT NULL,
        kind         TEXT NOT NULL CHECK (kind IN ('whatsapp', 'telegram', 'signal', 'discord', 'sms', 'matrix', 'email', 'calendar')),
        from_state   TEXT NOT NULL CHECK (from_state IN ('unknown', 'connected', 'unreachable', 'reconnect_required', 'pending_operator')),
        to_state     TEXT NOT NULL CHECK (to_state IN ('connected', 'unreachable', 'reconnect_required', 'pending_operator')),
        occurred_at  TEXT NOT NULL,
        service      TEXT CHECK (service IS NULL OR service IN ('sso', 'jmap', 'caldav')),
        hint         TEXT,
        recorded_at  TEXT NOT NULL
    );
    CREATE INDEX connection_status_change_connection
        ON connection_status_change (connection, sequence);
    CREATE VIEW connection_status_current AS
    SELECT connection, kind, to_state AS state, occurred_at, service, hint, sequence
    FROM (
        SELECT c.*, ROW_NUMBER() OVER (
                   PARTITION BY c.connection ORDER BY c.sequence DESC
               ) AS recency
        FROM connection_status_change c
    )
    WHERE recency = 1;
    "#,
    // v10 — the disclosure switch (ticket #121, ADR 0019, ADR 0031).
    //
    // A journal, not a setting. ADR 0019 requires that removing the sentence
    // a persona's reply discloses itself with be "a recorded deliberate act",
    // and a single-row upsert with an `updated_at` — the settings table's
    // shape — forgets who decided and what the state was before. So this is
    // a sibling of `consent_decision`: append-only by the same triggers,
    // the same `occurred_at`/`actor`/`reason` columns, and the current state
    // is its last row. Deliberately **not** the consent journal itself: the
    // disclosure is not a decision about a subject, has no scope and no
    // envelope, and folding it in would give it a consent event it is not.
    //
    // No row means **on**. The disclosure ships on by default (ADR 0031), and
    // what this table records is every decision to turn it off and back —
    // so a deployment that never touched it has an empty table and a true
    // answer, not a seeded row pretending somebody decided.
    r#"
    CREATE TABLE disclosure_decision (
        sequence    INTEGER PRIMARY KEY AUTOINCREMENT,
        new_state   TEXT NOT NULL CHECK (new_state IN ('on', 'off')),
        occurred_at TEXT NOT NULL,
        -- The deployment's owner, as a consent decision's actor is: the one
        -- person who can take this decision (ADR 0011).
        actor       TEXT NOT NULL,
        reason      TEXT
    );

    CREATE TRIGGER disclosure_decision_no_delete BEFORE DELETE ON disclosure_decision
    BEGIN
        SELECT RAISE(ABORT, 'the disclosure decision journal is append-only');
    END;
    CREATE TRIGGER disclosure_decision_no_update BEFORE UPDATE ON disclosure_decision
    BEGIN
        SELECT RAISE(ABORT, 'the disclosure decision journal is append-only');
    END;
    "#,
    // v11 — the one governed pull (issue #281): every free/busy read Hermes
    // made through this Gateway, served or refused. The record is the point:
    // a pull that left no trace would be the one thing in this deployment
    // the owner could not audit. What was read is not kept — intervals are
    // the owner's agenda — only how many.
    r#"
    CREATE TABLE hermes_read (
        sequence     INTEGER PRIMARY KEY AUTOINCREMENT,
        connection   TEXT NOT NULL,
        window_from  TEXT NOT NULL,
        window_to    TEXT NOT NULL,
        requested_at TEXT NOT NULL,
        -- The X-Hermes-Delivery the read carried, when it did: a retry and
        -- a second read are two rows with one delivery or two.
        delivery     TEXT,
        -- 'served', or the code the read was refused with.
        outcome      TEXT NOT NULL,
        intervals    INTEGER
    );
    CREATE INDEX hermes_read_connection ON hermes_read (connection, sequence);
    "#,
    // v12 — whose words went out (issue #327). `edited` says a body
    // differed from the draft; it never said whether the owner corrected a
    // word or threw the draft away and wrote their own reply, and those are
    // the two cases ADR 0019 treats differently — the second carries no
    // disclosure. Rows written before this one are the persona's, which is
    // what they were: until #327 nothing else could be sent.
    r#"
    ALTER TABLE approval ADD COLUMN written_by TEXT NOT NULL DEFAULT 'persona';
    "#,
    // v13 — whether a calendar event may carry where the meeting is
    // (#354, #351). A sibling of `disclosure_decision` down to its
    // triggers, and for the same reason: this is a decision the owner
    // takes, not a preference they hold, so what it leaves behind is a
    // dated act with an actor.
    //
    // A table of its own rather than a `kind` column on the disclosure's,
    // because an append-only journal with a discriminator is a place where
    // one switch's migration breaks the other's, and because the two
    // default the opposite way — which a shared table would have to carry
    // as data instead of as code.
    //
    // No row means **off**, and that is the difference from v10 worth
    // stating: the disclosure ships on because ADR 0019 wants a reply to
    // disclose itself unless somebody decided otherwise, and this ships off
    // because a location can be a home address and, on an invitation,
    // belongs to whoever organised the meeting (ADR 0012, ADR 0028). A
    // deployment that never opened the settings screen sends no location.
    r#"
    CREATE TABLE calendar_location_decision (
        sequence    INTEGER PRIMARY KEY AUTOINCREMENT,
        new_state   TEXT NOT NULL CHECK (new_state IN ('on', 'off')),
        occurred_at TEXT NOT NULL,
        actor       TEXT NOT NULL,
        reason      TEXT
    );

    CREATE TRIGGER calendar_location_decision_no_delete BEFORE DELETE ON calendar_location_decision
    BEGIN
        SELECT RAISE(ABORT, 'the calendar location decision journal is append-only');
    END;
    CREATE TRIGGER calendar_location_decision_no_update BEFORE UPDATE ON calendar_location_decision
    BEGIN
        SELECT RAISE(ABORT, 'the calendar location decision journal is append-only');
    END;
    "#,
];

/// One switch's journal: the table its rows live in, the noun a failure
/// names it by, and what it ships as. Not a string a caller passes — the
/// two constants below are every value this type takes, which is what lets
/// [`Store::switch_state`] interpolate the table name into its SQL.
#[derive(Debug, Clone, Copy)]
struct Switch {
    table: &'static str,
    name: &'static str,
    shipped_enabled: bool,
}

/// On unless the owner turned it off (ADR 0031).
const DISCLOSURE: Switch = Switch {
    table: "disclosure_decision",
    name: "disclosure",
    shipped_enabled: true,
};

/// Off unless the owner turned it on (#351): a location can be a home
/// address, and on an invitation it belongs to whoever organised it.
const CALENDAR_LOCATION: Switch = Switch {
    table: "calendar_location_decision",
    name: "calendar location",
    shipped_enabled: false,
};

/// One conversation's move, as the register decided it (issue #255).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PortalMove {
    /// The room the conversation lives in now.
    pub successor: String,
    /// The room it left — the one the user's decision named.
    pub predecessor: String,
    pub bridge_id: String,
    /// People in the successor when the register decided, bridge bot and
    /// Sensor excluded — the number the threshold was applied to.
    pub members: u64,
    /// The threshold applied, so the entry stays readable after the operator
    /// changes it.
    pub crowd_threshold: u64,
    /// Whether the Sensor was invited into the successor (the decision
    /// followed the conversation), or the conversation went back to the
    /// chooser as a crowd.
    pub followed: bool,
    /// RFC 3339.
    pub decided_at: String,
}

/// The consent store. One connection behind a mutex: a decision is a handful
/// of small local statements, so the contention a pool would relieve does not
/// exist, and one writer is what SQLite wants anyway.
pub struct Store {
    connection: Mutex<Connection>,
    path: PathBuf,
    /// Who this deployment's owner is, and under which Matrix IDs their own
    /// traffic arrives (ticket #149, [`crate::owner`]).
    ///
    /// Held by the store rather than passed to each read, and that is the
    /// point: the owner is never a contact and never has a consent state, so
    /// "the read that forgot to exclude them" must not be expressible. A
    /// caller cannot supply the wrong set, omit it, or add a read that does
    /// not ask — the same reason the `persona` exclusion lives in the
    /// `consent_snapshot` view's own SQL rather than in a Rust filter over it.
    ///
    /// It is configuration, and immutable for the life of the process: the
    /// operator adds a ghost to `GATEWAY_OWNER_IDENTITIES` and the deployment
    /// restarts the Gateway.
    owner: Arc<Owner>,
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

/// One entry of the current state: a subject, a connection, and the state
/// the most recent decision covering them left behind. `network` is the
/// connection's kind (#270).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub subject: Subject,
    pub connection: String,
    pub network: Network,
    pub state: State,
    pub decided_at: String,
    pub decision_sequence: i64,
}

/// One bridge transition as committed (ticket #56): where it landed in the
/// journal, the id the outbox will publish it under, and whether the
/// identical transition had already been recorded.
#[derive(Debug, Clone)]
pub struct BridgeStatusCommitted {
    pub sequence: i64,
    pub event_id: String,
    /// True when this exact transition — same bridge, same state, same
    /// instant — was already in the store. Nothing was recorded and the id is
    /// the first one's.
    pub replayed: bool,
}

/// One contact the Gateway has seen write, on one network (ticket #54).
///
/// The whole of what the Gateway keeps about a correspondent, and the whole
/// of what any read of this store can hand out: an ID, a network and two
/// instants. There is no field here for a body, a display name or a network
/// identifier, and adding one would be the change a reviewer refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenContact {
    /// The contact's Matrix user ID, as the bridge materialised it.
    pub contact: String,
    /// The connection it wrote on (#270); `network` is its kind.
    pub connection: String,
    pub network: Network,
    /// RFC 3339, from the event's own `time` — the instant the Sensor
    /// produced the first and the last event this contact was the subject
    /// of.
    pub first_seen: String,
    pub last_seen: String,
}

/// One approval this Gateway recorded (ticket #24): what a client reads back
/// to learn whether the reply it approved actually went out.
///
/// There is no `body` here, and there is none in the table it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedApproval {
    /// The contract's deterministic id of the `persona.reply.approved.v1`.
    pub event_id: String,
    pub suggestion_event_id: String,
    pub approved_by: String,
    pub persona_id: String,
    pub network: Network,
    pub contact: String,
    pub edited: bool,
    /// Whose words went out (#327): `persona` — the draft, corrections
    /// included — or `owner`, the reply written in its place. What the
    /// disclosure followed, and the half of the trail `edited` could not
    /// carry.
    pub written_by: String,
    pub approved_at: String,
    /// `None` until the bus acknowledged the publication. A recorded
    /// approval that is not published is a crash between the two, not a
    /// queue: nothing retries it but another approval of the same
    /// suggestion.
    pub published_at: Option<String>,
    /// Where on the bus the reply landed. `None` for the same reason.
    pub stream_sequence: Option<u64>,
}

impl RecordedApproval {
    /// The one word a client branches on: `published` when the bus
    /// acknowledged the reply, `unpublished` when this Gateway wrote the row
    /// and does not know where the reply went.
    ///
    /// Both are terminal. Neither is "in flight": an approval is published
    /// inside its own request or it is refused, so there is no state here
    /// that a screen should render as a spinner.
    pub fn publication(&self) -> &'static str {
        if self.published_at.is_some() {
            "published"
        } else {
            "unpublished"
        }
    }
}

/// A committed decision the outbox has not published yet.
#[derive(Debug, Clone)]
pub struct Unpublished {
    pub sequence: i64,
    pub event_id: String,
    pub envelope: Value,
}

/// The consent state a cold consumer starts from, and the bus position it
/// reflects (ticket #50, ADR 0010). Read as one unit — see
/// [`Store::snapshot`].
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// One entry per (subject, network), `persona` subjects excluded.
    pub entries: Vec<Entry>,
    /// The journal position the snapshot reflects — the end of the published
    /// prefix. `0` when no decision has reached the bus yet.
    pub decision_sequence: i64,
    /// The JetStream sequence of that decision: what a consumer adds one to.
    /// `0` when no decision has reached the bus yet, so that a consumer
    /// starts at `1` and sees the whole stream.
    pub stream_sequence: u64,
}

/// Why a snapshot could not be served. Two very different failures, kept
/// apart because the answers a client branches on are different codes: one
/// is a store the Gateway could not read, the other is a snapshot the
/// operator has capped below its own size.
#[derive(Debug)]
pub enum SnapshotRefusal {
    /// The current state holds more entries than the configured cap. The
    /// snapshot is refused whole: truncating it silently would hand a
    /// consumer a state in which contacts the user granted look never
    /// decided, which is exactly the confusion ADR 0010's explicit
    /// revocations exist to prevent.
    TooLarge {
        max_entries: usize,
    },
    Store(anyhow::Error),
}

impl std::fmt::Display for SnapshotRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotRefusal::TooLarge { max_entries } => write!(
                formatter,
                "the consent state holds more than {max_entries} entries"
            ),
            SnapshotRefusal::Store(error) => write!(formatter, "{error}"),
        }
    }
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
    pub fn open(state_dir: &Path, owner: Arc<Owner>) -> Result<Self> {
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
            owner,
        };
        store.migrate()?;
        Ok(store)
    }

    /// This deployment's owner and their confirmed identities: who no read of
    /// this store will serve a consent state for (ticket #149).
    pub fn owner(&self) -> &Owner {
        &self.owner
    }

    /// The SQL that keeps the owner's own rows out of a read of the consent
    /// state, and the parameters it binds.
    ///
    /// A clause built here and bound as parameters, rather than folded into
    /// the `consent_state` and `consent_snapshot` views: a view cannot take a
    /// parameter, and the alternative — writing the configured identities into
    /// a table for the views to join against — would make this file a second
    /// holder of a truth that lives in the environment, with a stale copy the
    /// first time a Gateway started with a different one.
    ///
    /// Scoped to `contact` rows on purpose. A `network` subject's id is a
    /// network value and a `persona` subject's id is a persona's name; neither
    /// is a Matrix user ID, so a collision there is not the owner (see
    /// [`Decision::refuse_if_owner`]).
    fn owner_exclusion(&self) -> (String, Vec<&str>) {
        let identities = self.owner.as_sql_params();
        if identities.is_empty() {
            return (String::new(), identities);
        }
        let placeholders = vec!["?"; identities.len()].join(", ");
        (
            format!("WHERE NOT (subject_type = 'contact' AND subject_id IN ({placeholders}))"),
            identities,
        )
    }

    /// The mirror image of [`Self::owner_exclusion`]: only the owner's own
    /// rows. What [`Self::owner_entries`] reads, so that a row this Gateway
    /// withholds is still a row an operator can be told about.
    fn owner_only(&self) -> (String, Vec<&str>) {
        let identities = self.owner.as_sql_params();
        if identities.is_empty() {
            // No identity can match, and `IN ()` is not SQL. A predicate that
            // is false for every row says the same thing and stays one query.
            return (String::from("WHERE 0"), identities);
        }
        let placeholders = vec!["?"; identities.len()].join(", ");
        (
            format!("WHERE subject_type = 'contact' AND subject_id IN ({placeholders})"),
            identities,
        )
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
    ///
    /// A decision about the owner is refused here as well as at
    /// [`crate::outbox::Outbox::record`], which is where the caller gets a code
    /// and a sentence for it. This one is the floor: the store is the only
    /// thing in this process that can append to the journal, so a route added
    /// later that reached for it directly would fail loudly instead of writing
    /// a row every read then has to withhold (ticket #149).
    pub fn record(
        &self,
        decision: &Decision,
        actor: &str,
        occurred_at: &str,
        domain: &str,
        produced_at: &str,
    ) -> Result<Committed> {
        if let Err(refusal) = decision.refuse_if_owner(&self.owner) {
            anyhow::bail!(
                "refusing to append a consent decision about the owner to the journal: {}",
                refusal.message()
            );
        }
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("failed to open the decision transaction")?;

        let placeholders = vec!["?"; decision.connections.len()].join(", ");
        let mut parameters: Vec<String> = vec![
            decision.subject.kind.as_str().to_owned(),
            decision.subject.id.clone(),
        ];
        parameters.extend(decision.connections.iter().cloned());
        let previous: Option<String> = transaction
            .query_row(
                &format!(
                    "SELECT state FROM consent_state \
                     WHERE subject_type = ? AND subject_id = ? AND connection IN ({placeholders}) \
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
        // The scope, one row per connection (#270). `consent_decision_network`
        // is no longer written: `network` is read off the registry's kind.
        for connection_id in &decision.connections {
            transaction
                .execute(
                    "INSERT INTO consent_decision_connection (sequence, connection) \
                     VALUES (?, ?)",
                    rusqlite::params![sequence, connection_id],
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
    /// is the projection #50 serves as a snapshot alongside the stream
    /// position it reflects.
    ///
    /// The owner's own rows are not in it (ticket #149): the owner is never a
    /// contact and never has a consent state, so this read serves none —
    /// including a row recorded before that was enforced. The rows themselves
    /// are still there, and [`Self::owner_entries`] is what says so.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let (exclusion, identities) = self.owner_exclusion();
        self.state_rows(&exclusion, &identities)
    }

    /// The consent rows this store holds **about the owner** — the ones every
    /// other read withholds (ticket #149).
    ///
    /// Nothing serves these to a consumer. They exist so that withholding a
    /// row is not the same thing as hiding it: the Gateway counts them at
    /// startup and on every snapshot read, names them in a warning, and
    /// publishes the number on `/metrics`. A deployment upgraded across #109
    /// can hold one — before ADR 0018 the user's own messages were published
    /// as a contact's and fed the pending-contact projection — and an operator
    /// is entitled to know that their journal has one rather than discovering
    /// it in a SQL client.
    ///
    /// They are not deleted, and this is the deliberate half of the answer:
    /// `consent_decision` is append-only, enforced by its own triggers,
    /// because it is the audit trail of a confidentiality promise. A migration
    /// would also only ever catch the identities configured on the day it ran,
    /// and the set grows after the fact — a LID appeared mid-conversation on
    /// the reference deployment — so an identity confirmed next month would
    /// need another migration, while a read-time exclusion covers it the
    /// moment the operator confirms it.
    pub fn owner_entries(&self) -> Result<Vec<Entry>> {
        let (only, identities) = self.owner_only();
        self.state_rows(&only, &identities)
    }

    /// One read of the `consent_state` view under a caller-built predicate.
    /// The two callers above are its whole vocabulary: everything, minus the
    /// owner; or the owner's alone.
    fn state_rows(&self, predicate: &str, params: &[&str]) -> Result<Vec<Entry>> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(&format!(
                "SELECT subject_type, subject_id, connection, network, state, decided_at, \
                        decision_sequence \
                 FROM consent_state {predicate} ORDER BY subject_type, subject_id, connection"
            ))
            .context("failed to prepare the current-state query")?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .context("failed to read the current state")?;
        let mut entries = Vec::new();
        for row in rows {
            let (kind, id, connection, network, state, decided_at, decision_sequence) =
                row.context("failed to read a current-state row")?;
            entries.push(entry(
                kind,
                id,
                connection,
                network,
                state,
                decided_at,
                decision_sequence,
            )?);
        }
        Ok(entries)
    }

    /// The snapshot a consumer with a cold cache starts from: the current
    /// state, and the JetStream sequence it reflects (ticket #50, ADR 0010).
    ///
    /// # Why the published prefix, and not the journal's head
    ///
    /// Only a decision that has reached the bus has a position on it. A
    /// snapshot that included the decisions still waiting in the outbox
    /// would have to name the position of the last *published* one — and a
    /// consumer starting there would then be handed those decisions a second
    /// time when the outbox drains, applying them twice. So the snapshot
    /// stops where the bus's knowledge stops: `decision_sequence` is the end
    /// of the positioned prefix (`consent_snapshot_horizon`), the entries
    /// are that prefix's state, and every decision the snapshot does not
    /// know about is, by construction, a decision the consumer will be told
    /// about from `stream_sequence + 1`. No overlap, no gap.
    ///
    /// A bus outage therefore delays the snapshot's content rather than
    /// corrupting it: the decisions accumulate as unpositioned rows, the
    /// snapshot keeps naming the last position it can vouch for, and the
    /// consumer receives the backlog in order once the outbox drains.
    ///
    /// # Why the position cannot disagree with the content
    ///
    /// Both are read inside one transaction, and every write goes through
    /// the same single connection behind this store's mutex — so no decision
    /// can be committed, and none can be marked published, between the read
    /// of the state and the read of the position. Whatever a concurrent
    /// caller does, it lands entirely inside this snapshot or entirely after
    /// it.
    ///
    /// # The cap
    ///
    /// `max_entries` is a bound, not a page size: there is no pagination,
    /// and a state larger than the cap is refused with
    /// [`SnapshotRefusal::TooLarge`] rather than truncated. The query asks
    /// for one row more than the cap, so a refusal costs one extra row and
    /// never materialises a state nobody may have.
    ///
    /// # The owner is not in it
    ///
    /// Not as a subject, not as a state, not even as a row this query reads
    /// (ticket #149, ADR 0018, ADR 0021). The owner is never a contact and
    /// never has a consent state, and a snapshot that served one would tell
    /// every consumer otherwise — which is the whole defect, since a consumer
    /// without the Sensor's own filter (#147) would apply it. A row recorded
    /// before this was enforced is covered too, because the exclusion is at
    /// read time; [`Self::owner_entries`] is what reports that such a row
    /// exists.
    pub fn snapshot(&self, max_entries: usize) -> Result<Snapshot, SnapshotRefusal> {
        let mut connection = self.connection();
        // Deferred: the whole of this is a read, and in WAL mode the first
        // statement fixes the snapshot every later one sees. The mutex above
        // already serialises this against a decision being recorded; the
        // transaction is what keeps that true if this store ever grows a
        // second connection.
        let transaction = connection
            .transaction()
            .context("failed to open the snapshot transaction")
            .map_err(SnapshotRefusal::Store)?;

        let (decision_sequence, stream_sequence) = transaction
            .query_row(
                "SELECT h.decision_sequence, \
                        COALESCE((SELECT d.stream_sequence FROM consent_decision d \
                                  WHERE d.sequence = h.decision_sequence), 0) \
                 FROM consent_snapshot_horizon h",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .context("failed to read the snapshot's stream position")
            .map_err(SnapshotRefusal::Store)?;

        // The owner's own rows are excluded here, in the snapshot's own SQL
        // and before the cap, exactly as `persona` rows are excluded in the
        // view (ticket #149): this is the projection a consumer's whole cold
        // start rests on, and the invariant is that it never *reads* a row
        // about the owner rather than that it remembers to drop one.
        let (exclusion, identities) = self.owner_exclusion();
        let mut statement = transaction
            .prepare(&format!(
                "SELECT subject_type, subject_id, connection, network, state, decided_at, \
                        decision_sequence \
                 FROM consent_snapshot {exclusion} \
                 ORDER BY subject_type, subject_id, connection LIMIT ?"
            ))
            .context("failed to prepare the snapshot query")
            .map_err(SnapshotRefusal::Store)?;
        // One more than the cap: enough to know the state is over it, and
        // never the whole of an oversized state. Counted over the rows this
        // snapshot would serve, so a withheld owner row never pushes a state
        // over the cap it is not part of.
        let ceiling = i64::try_from(max_entries.saturating_add(1)).unwrap_or(i64::MAX);
        let mut bound: Vec<rusqlite::types::Value> = identities
            .iter()
            .map(|identity| rusqlite::types::Value::from((*identity).to_owned()))
            .collect();
        bound.push(rusqlite::types::Value::from(ceiling));
        let rows = statement
            .query_map(rusqlite::params_from_iter(bound.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .context("failed to read the snapshot")
            .map_err(SnapshotRefusal::Store)?;
        let mut entries = Vec::new();
        for row in rows {
            let (kind, id, connection, network, state, decided_at, decision_sequence) = row
                .context("failed to read a snapshot row")
                .map_err(SnapshotRefusal::Store)?;
            if entries.len() == max_entries {
                return Err(SnapshotRefusal::TooLarge { max_entries });
            }
            entries.push(
                entry(
                    kind,
                    id,
                    connection,
                    network,
                    state,
                    decided_at,
                    decision_sequence,
                )
                .map_err(SnapshotRefusal::Store)?,
            );
        }
        Ok(Snapshot {
            entries,
            decision_sequence,
            stream_sequence: u64::try_from(stream_sequence).unwrap_or(0),
        })
    }

    /// The effective consent state of a contact on one connection (#270),
    /// with the precedence applied: the contact's own decision on it if
    /// there is one, the network's default on it otherwise, and `pending`
    /// when neither exists.
    ///
    /// An owner identity resolves to `pending` with no decision named, and no
    /// row is read for it — not the owner's own, and not the network's default
    /// either, because a default is a default *for contacts* and the owner is
    /// not one (ticket #149). That is the safe internal answer: the approval
    /// path reads this to ask "is this sender granted, now?" and must never be
    /// told yes about a subject that cannot be decided. The question itself is
    /// refused at the HTTP surface with its own code, so a caller who asks it
    /// is told why rather than handed a `pending` it would read as "not yet
    /// decided".
    pub fn effective(
        &self,
        contact: &str,
        connection_id: &str,
        network: Network,
    ) -> Result<Effective> {
        if self.owner.is_owner(contact) {
            return Ok(Effective::resolve(contact, network, None, None));
        }
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT state FROM consent_state \
                 WHERE subject_type = ? AND subject_id = ? AND connection = ?",
            )
            .context("failed to prepare the precedence query")?;
        let mut read = |kind: SubjectType, id: &str| -> Result<Option<State>> {
            let state: Option<String> = statement
                .query_row(rusqlite::params![kind.as_str(), id, connection_id], |row| {
                    row.get(0)
                })
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

    /// Marks a decision as published, at the position the bus stored it at.
    /// The only mutation the journal allows, and the one whose loss is
    /// survivable: a row published but not marked is republished, and the bus
    /// deduplicates it on `Nats-Msg-Id` — answering with the sequence of the
    /// message it already holds, so the position this records is the same one
    /// either way.
    ///
    /// The position is what the snapshot names ([`Store::snapshot`]), so it
    /// is written in the same statement as the mark: a row that counts as
    /// published and has no position would hold the snapshot's horizon back
    /// forever.
    pub fn mark_published(
        &self,
        sequence: i64,
        published_at: &str,
        stream_sequence: u64,
    ) -> Result<()> {
        self.connection()
            .execute(
                "UPDATE consent_decision SET published_at = ?1, stream_sequence = ?2 \
                 WHERE sequence = ?3 AND published_at IS NULL",
                rusqlite::params![
                    published_at,
                    i64::try_from(stream_sequence).unwrap_or(i64::MAX),
                    sequence
                ],
            )
            .with_context(|| format!("failed to mark decision {sequence} published"))?;
        Ok(())
    }

    // -- Bridge status (ticket #56) -----------------------------------------

    /// The last state recorded for one bridge, or `None` when nothing has
    /// ever been recorded for it — which the caller reads as
    /// [`crate::bridge_status::ContractState::INITIAL`].
    ///
    /// This is what makes de-duplication survive a restart: the comparison a
    /// push is made against comes from the store, not from memory.
    pub fn bridge_status(&self, bridge_id: &str) -> Result<Option<ContractState>> {
        let connection = self.connection();
        let state: Option<String> = connection
            .query_row(
                "SELECT state FROM bridge_status_current WHERE bridge_id = ?",
                [bridge_id],
                |row| row.get(0),
            )
            .optional()
            .context("failed to read the bridge's current status")?;
        match state.as_deref() {
            Some(state) => Ok(Some(ContractState::parse(state).with_context(|| {
                format!("the store holds the unknown bridge state {state:?}")
            })?)),
            None => Ok(None),
        }
    }

    // -- Connection status (issue #275) ----------------------------------

    /// Records one `connection.status.changed.v1` transition, idempotently:
    /// the event's id is unique, so a redelivery records nothing twice.
    /// Returns whether the row was new.
    pub fn record_connection_status_change(
        &self,
        change: &crate::connection_status::Change,
        recorded_at: &str,
    ) -> Result<bool> {
        let connection = self.connection();
        let inserted = connection
            .execute(
                "INSERT OR IGNORE INTO connection_status_change
                    (event_id, connection, kind, from_state, to_state, occurred_at, service, hint, recorded_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    change.event_id,
                    change.connection,
                    change.kind,
                    change.from_state,
                    change.to_state,
                    change.occurred_at,
                    change.service,
                    change.hint,
                    recorded_at,
                ],
            )
            .context("failed to record a connection status change")?;
        Ok(inserted > 0)
    }

    /// The state a connection last said it was in, or `None` when it never
    /// said — a bridge's connection, or a collector that has not run.
    pub fn connection_status(
        &self,
        connection_id: &str,
    ) -> Result<Option<crate::connection_status::Current>> {
        let connection = self.connection();
        connection
            .query_row(
                "SELECT connection, kind, state, occurred_at, service, hint
                 FROM connection_status_current WHERE connection = ?",
                [connection_id],
                current_status_row,
            )
            .optional()
            .context("failed to read the connection's current status")
    }

    /// Every connection's current state, for the registry document.
    pub fn connection_statuses(&self) -> Result<Vec<crate::connection_status::Current>> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT connection, kind, state, occurred_at, service, hint
                 FROM connection_status_current ORDER BY connection",
            )
            .context("failed to prepare the connection statuses read")?;
        let rows = statement
            .query_map([], current_status_row)
            .context("failed to read the connection statuses")?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("failed to read a connection status row")
    }

    /// Records one free/busy read (issue #281), whatever its outcome.
    pub fn record_hermes_read(&self, read: &crate::hermes_freebusy::HermesRead) -> Result<()> {
        let connection = self.connection();
        connection
            .execute(
                "INSERT INTO hermes_read
                 (connection, window_from, window_to, requested_at, delivery, outcome, intervals)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    read.connection,
                    read.window_from,
                    read.window_to,
                    read.requested_at,
                    read.delivery,
                    read.outcome,
                    read.intervals.map(|count| count as i64),
                ],
            )
            .context("failed to record a free/busy read")?;
        Ok(())
    }

    /// The most recent transitions, newest first, for the dashboard's feed.
    pub fn connection_status_changes(
        &self,
        limit: usize,
    ) -> Result<Vec<crate::connection_status::Change>> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT event_id, connection, kind, from_state, to_state, occurred_at, service, hint
                 FROM connection_status_change ORDER BY sequence DESC LIMIT ?",
            )
            .context("failed to prepare the connection status changes read")?;
        let rows = statement
            .query_map([limit as i64], |row| {
                Ok(crate::connection_status::Change {
                    event_id: row.get(0)?,
                    connection: row.get(1)?,
                    kind: row.get(2)?,
                    from_state: row.get(3)?,
                    to_state: row.get(4)?,
                    occurred_at: row.get(5)?,
                    service: row.get(6)?,
                    hint: row.get(7)?,
                })
            })
            .context("failed to read the connection status changes")?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("failed to read a connection status change row")
    }

    /// Records the registry of connections as configured (#269): inserted
    /// when new, kind and label refreshed when known. Never deleted here — a
    /// connection that left the configuration may still be what a recorded
    /// decision is scoped to, and forgetting it would orphan the decision.
    ///
    /// Returns the ids the store holds that the registry does **not** name:
    /// a connection that left the configuration, or the one the migration
    /// (#270) attached every earlier decision to on a deployment whose
    /// declared ids are not the networks' names. A decision scoped to such
    /// an id governs no live connection, and the Gateway says so at
    /// startup rather than letting the user's earlier answers go quiet.
    pub fn record_connections(
        &self,
        connections: &[crate::connections::Connection],
    ) -> Result<Vec<String>> {
        let now = crate::consent::rfc3339_millis(std::time::SystemTime::now());
        let connection = self.connection();
        for entry in connections {
            connection
                .execute(
                    "INSERT INTO connection (id, kind, label, bridge_id, created_at) \
                     VALUES (?, ?, ?, ?, ?) \
                     ON CONFLICT (id) DO UPDATE SET kind = excluded.kind, \
                     label = excluded.label, bridge_id = excluded.bridge_id",
                    rusqlite::params![entry.id, entry.kind, entry.label, entry.bridge_id, now],
                )
                .context("failed to record a connection")?;
        }
        let mut statement = connection
            .prepare("SELECT id FROM connection ORDER BY id")
            .context("failed to prepare the connections query")?;
        let known = statement
            .query_map([], |row| row.get::<_, String>(0))
            .context("failed to read the connections")?
            .collect::<Result<Vec<_>, _>>()
            .context("failed to read a connection row")?;
        Ok(known
            .into_iter()
            .filter(|id| !connections.iter().any(|entry| &entry.id == id))
            .collect())
    }

    /// Journals one move, once: a second record about the same successor is
    /// a replay of the same decision and changes nothing. Returns whether
    /// this call was the one that recorded it.
    pub fn record_portal_move(&self, portal_move: &PortalMove) -> Result<bool> {
        let inserted = self
            .connection()
            .execute(
                "INSERT INTO portal_move                  (successor, predecessor, bridge_id, members, crowd_threshold, followed,                   decided_at)                  VALUES (?, ?, ?, ?, ?, ?, ?)                  ON CONFLICT (successor) DO NOTHING",
                rusqlite::params![
                    portal_move.successor,
                    portal_move.predecessor,
                    portal_move.bridge_id,
                    portal_move.members as i64,
                    portal_move.crowd_threshold as i64,
                    i64::from(portal_move.followed),
                    portal_move.decided_at,
                ],
            )
            .context("failed to record a portal move")?;
        Ok(inserted == 1)
    }

    /// Whether a move onto this successor has already been decided.
    pub fn portal_move_decided(&self, successor: &str) -> Result<bool> {
        let count: i64 = self
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM portal_move WHERE successor = ?",
                [successor],
                |row| row.get(0),
            )
            .context("failed to look up a portal move")?;
        Ok(count > 0)
    }

    /// The most recent moves, newest first.
    pub fn portal_moves(&self, limit: usize) -> Result<Vec<PortalMove>> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT successor, predecessor, bridge_id, members, crowd_threshold, followed,                  decided_at FROM portal_move ORDER BY decided_at DESC, successor LIMIT ?",
            )
            .context("failed to prepare the portal moves query")?;
        let rows = statement
            .query_map([limit as i64], |row| {
                Ok(PortalMove {
                    successor: row.get(0)?,
                    predecessor: row.get(1)?,
                    bridge_id: row.get(2)?,
                    members: row.get::<_, i64>(3)? as u64,
                    crowd_threshold: row.get::<_, i64>(4)? as u64,
                    followed: row.get::<_, i64>(5)? == 1,
                    decided_at: row.get(6)?,
                })
            })
            .context("failed to read the portal moves")?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("failed to read a portal move row")
    }

    /// Records one bridge transition and its rendered envelope, in one
    /// statement: the row is durable before anything is published, as a
    /// consent decision is.
    ///
    /// The identical transition arriving twice — the same bridge, the same
    /// state, the same instant — records nothing and returns the first row's
    /// id, exactly as a replayed decision does. That is the second line of
    /// defence behind the state comparison: mautrix retries a push with
    /// backoff, so the same body genuinely does arrive twice.
    pub fn record_bridge_status(
        &self,
        transition: &crate::bridge_status::Transition,
        domain: &str,
        produced_at: &str,
    ) -> Result<BridgeStatusCommitted> {
        let event_id = transition.event_id();
        let envelope = transition.envelope(domain, produced_at);
        let connection = self.connection();
        let inserted = connection
            .execute(
                "INSERT INTO bridge_status_change \
                 (event_id, bridge_id, network, from_state, to_state, occurred_at, reason, \
                  last_message_at, envelope) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (event_id) DO NOTHING",
                rusqlite::params![
                    event_id,
                    transition.bridge_id,
                    transition.network.as_str(),
                    transition.from_state.as_str(),
                    transition.to_state.as_str(),
                    transition.occurred_at,
                    transition.reason,
                    transition.last_message_at,
                    serde_json::to_string(&envelope)?,
                ],
            )
            .context("failed to record the bridge status change")?;
        if inserted == 0 {
            let sequence: i64 = connection
                .query_row(
                    "SELECT sequence FROM bridge_status_change WHERE event_id = ?",
                    [&event_id],
                    |row| row.get(0),
                )
                .context("failed to read the already-recorded bridge status change")?;
            return Ok(BridgeStatusCommitted {
                sequence,
                event_id,
                replayed: true,
            });
        }
        Ok(BridgeStatusCommitted {
            sequence: connection.last_insert_rowid(),
            event_id,
            replayed: false,
        })
    }

    /// The bridge transitions the outbox has not published yet, oldest
    /// first.
    pub fn unpublished_bridge_status(&self, limit: usize) -> Result<Vec<Unpublished>> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT sequence, event_id, envelope FROM bridge_status_change \
                 WHERE published_at IS NULL ORDER BY sequence LIMIT ?",
            )
            .context("failed to prepare the bridge status outbox query")?;
        let rows = statement
            .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .context("failed to read the bridge status outbox")?;
        let mut pending = Vec::new();
        for row in rows {
            let (sequence, event_id, envelope) = row.context("failed to read an outbox row")?;
            pending.push(Unpublished {
                sequence,
                event_id,
                envelope: serde_json::from_str(&envelope).with_context(|| {
                    format!("bridge status change {sequence} holds an unreadable envelope")
                })?,
            });
        }
        Ok(pending)
    }

    /// How many bridge transitions are still waiting for the bus.
    pub fn unpublished_bridge_status_count(&self) -> Result<u64> {
        let connection = self.connection();
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM bridge_status_change WHERE published_at IS NULL",
                [],
                |row| row.get(0),
            )
            .context("failed to count the bridge status outbox")?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// Marks a bridge transition published, at the position the bus stored
    /// it at — the same bookkeeping [`Store::mark_published`] does for a
    /// decision, and survivable the same way.
    pub fn mark_bridge_status_published(
        &self,
        sequence: i64,
        published_at: &str,
        stream_sequence: u64,
    ) -> Result<()> {
        self.connection()
            .execute(
                "UPDATE bridge_status_change SET published_at = ?1, stream_sequence = ?2 \
                 WHERE sequence = ?3 AND published_at IS NULL",
                rusqlite::params![
                    published_at,
                    i64::try_from(stream_sequence).unwrap_or(i64::MAX),
                    sequence
                ],
            )
            .with_context(|| format!("failed to mark bridge status change {sequence} published"))?;
        Ok(())
    }

    // -----------------------------------------------------------------
    // Approvals (ticket #24)
    // -----------------------------------------------------------------

    /// Records one approval, before it is published.
    ///
    /// Nothing about the reply's content is passed to this method, so there
    /// is nothing here to have forgotten to drop: the signature could not
    /// write a message body if it wanted to.
    ///
    /// Recording the same approval twice — the same suggestion, the same
    /// approver — writes nothing and is not an error. That is the repair
    /// path for a crash between this write and the publication: the next
    /// attempt finds the row, republishes under the contract's id (which the
    /// bus deduplicates) and marks it.
    #[allow(clippy::too_many_arguments)]
    pub fn record_approval(
        &self,
        event_id: &str,
        suggestion_event_id: &str,
        approved_by: &str,
        persona_id: &str,
        network: Network,
        contact: &str,
        edited: bool,
        written_by: &str,
        approved_at: &str,
    ) -> Result<()> {
        self.connection()
            .execute(
                "INSERT INTO approval \
                 (event_id, suggestion_event_id, approved_by, persona_id, network, contact, \
                  edited, written_by, approved_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (event_id) DO NOTHING",
                rusqlite::params![
                    event_id,
                    suggestion_event_id,
                    approved_by,
                    persona_id,
                    network.as_str(),
                    contact,
                    i64::from(edited),
                    written_by,
                    approved_at,
                ],
            )
            .context("failed to record the approval")?;
        Ok(())
    }

    /// Marks an approval published, at the position the bus stored it at.
    ///
    /// `published_at IS NULL` in the predicate for the same reason the
    /// consent outbox has it: a republish that the bus deduplicated must not
    /// move a position that was already recorded.
    pub fn mark_approval_published(
        &self,
        event_id: &str,
        published_at: &str,
        stream_sequence: u64,
    ) -> Result<()> {
        self.connection()
            .execute(
                "UPDATE approval SET published_at = ?1, stream_sequence = ?2 \
                 WHERE event_id = ?3 AND published_at IS NULL",
                rusqlite::params![
                    published_at,
                    i64::try_from(stream_sequence).unwrap_or(i64::MAX),
                    event_id
                ],
            )
            .with_context(|| format!("failed to mark approval {event_id} published"))?;
        Ok(())
    }

    /// The approval of one suggestion, or `None` when it was never approved.
    ///
    /// One row per suggestion because one owner takes every decision on this
    /// deployment (ADR 0011); the table's key is nevertheless (suggestion,
    /// approver), which is the contract's own natural key.
    pub fn approval(&self, suggestion_event_id: &str) -> Result<Option<RecordedApproval>> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT event_id, suggestion_event_id, approved_by, persona_id, network, \
                        contact, edited, written_by, approved_at, published_at, \
                        stream_sequence \
                 FROM approval WHERE suggestion_event_id = ? ORDER BY sequence LIMIT 1",
            )
            .context("failed to prepare the approval query")?;
        let row = statement
            .query_row([suggestion_event_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                ))
            })
            .optional()
            .context("failed to read the approval")?;
        let Some((
            event_id,
            suggestion_event_id,
            approved_by,
            persona_id,
            network,
            contact,
            edited,
            written_by,
            approved_at,
            published_at,
            stream_sequence,
        )) = row
        else {
            return Ok(None);
        };
        Ok(Some(RecordedApproval {
            event_id,
            suggestion_event_id,
            approved_by,
            persona_id,
            network: Network::parse(&network)
                .with_context(|| format!("the store holds the unknown network {network:?}"))?,
            contact,
            edited: edited != 0,
            written_by,
            approved_at,
            published_at,
            stream_sequence: stream_sequence.map(|sequence| sequence as u64),
        }))
    }

    // -----------------------------------------------------------------
    // The disclosure switch (ticket #121)
    // -----------------------------------------------------------------

    /// Appends one decision to the disclosure journal and answers the state
    /// it leaves behind.
    ///
    /// Every call appends, including one that restates the current state:
    /// "the user confirmed it is on, on this date" is a fact the journal may
    /// hold, and collapsing it would make the journal a projection of itself.
    pub fn record_disclosure_decision(
        &self,
        enabled: bool,
        occurred_at: &str,
        actor: &str,
        reason: Option<&str>,
    ) -> Result<DisclosureState> {
        self.record_switch_decision(DISCLOSURE, enabled, occurred_at, actor, reason)
    }

    /// The switch as it stands: the journal's last row, or the default when
    /// the journal is empty — **on**, since nobody decided otherwise
    /// (ADR 0031).
    pub fn disclosure_state(&self) -> Result<DisclosureState> {
        self.switch_state(DISCLOSURE)
    }

    // -----------------------------------------------------------------
    // Whether a calendar event carries where the meeting is (#354)
    // -----------------------------------------------------------------

    /// Appends one decision to the calendar-location journal and answers
    /// the state it leaves behind.
    pub fn record_calendar_location_decision(
        &self,
        enabled: bool,
        occurred_at: &str,
        actor: &str,
        reason: Option<&str>,
    ) -> Result<SwitchState> {
        self.record_switch_decision(CALENDAR_LOCATION, enabled, occurred_at, actor, reason)
    }

    /// The switch as it stands: the journal's last row, or **off**, since a
    /// deployment where nobody decided sends no location (#351).
    pub fn calendar_location_state(&self) -> Result<SwitchState> {
        self.switch_state(CALENDAR_LOCATION)
    }

    // -----------------------------------------------------------------
    // What the two switches share
    // -----------------------------------------------------------------

    /// Appends one decision to a switch's journal and answers the state it
    /// leaves behind.
    ///
    /// Every call appends, including one that restates the current state:
    /// "the user confirmed it is on, on this date" is a fact the journal may
    /// hold, and collapsing it would make the journal a projection of itself.
    fn record_switch_decision(
        &self,
        switch: Switch,
        enabled: bool,
        occurred_at: &str,
        actor: &str,
        reason: Option<&str>,
    ) -> Result<SwitchState> {
        self.connection()
            .execute(
                // The table name is this binary's, never a caller's: the
                // two constants below are the only values `switch` takes.
                &format!(
                    "INSERT INTO {} (new_state, occurred_at, actor, reason) VALUES (?, ?, ?, ?)",
                    switch.table
                ),
                rusqlite::params![SwitchState::word(enabled), occurred_at, actor, reason],
            )
            .with_context(|| format!("failed to record the {} decision", switch.name))?;
        self.switch_state(switch)
    }

    /// A switch as it stands: its journal's last row, or what it ships as.
    fn switch_state(&self, switch: Switch) -> Result<SwitchState> {
        let connection = self.connection();
        let row = connection
            .query_row(
                &format!(
                    "SELECT new_state, occurred_at, actor, reason FROM {} \
                     ORDER BY sequence DESC LIMIT 1",
                    switch.table
                ),
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()
            .with_context(|| format!("failed to read the {} journal", switch.name))?;
        let Some((new_state, occurred_at, actor, reason)) = row else {
            return Ok(SwitchState::shipped_as(switch.shipped_enabled));
        };
        let enabled = SwitchState::enabled_from(&new_state).ok_or_else(|| {
            anyhow::anyhow!("the {} journal holds the state {new_state:?}", switch.name)
        })?;
        Ok(SwitchState {
            enabled,
            since: Some(occurred_at),
            actor: Some(actor),
            reason,
        })
    }

    // -----------------------------------------------------------------
    // The pending-contact projection (ticket #54)
    // -----------------------------------------------------------------

    /// Records that a contact wrote on a network at an instant.
    ///
    /// Three values in, three values stored. The caller
    /// ([`crate::contacts`]) never even parses the rest of the event, so
    /// there is nothing here to have dropped: this method could not write a
    /// message body if it wanted to.
    ///
    /// Idempotent by construction, which is what makes the consumer's
    /// commit-then-ack safe: the same event delivered twice moves neither
    /// timestamp, because `first_seen` only ever goes earlier and
    /// `last_seen` only ever goes later. The stream is not necessarily in
    /// timestamp order — a full first delivery replays weeks of history at
    /// once — so the extremes are taken rather than the last write winning.
    pub fn observe_contact(&self, contact: &str, connection: &str, at: &str) -> Result<()> {
        self.observe_contacts(&[(contact.to_owned(), connection.to_owned(), at.to_owned())])
    }

    /// The same, for a whole batch of sightings, in **one** transaction.
    ///
    /// This is what the projection calls, and the transaction is not an
    /// optimisation detail: the store is opened `synchronous=FULL`, so every
    /// separate write costs an fsync, and a Gateway's first delivery replays
    /// the stream's whole history through this method. One fsync per batch
    /// instead of one per message is the difference between a first start
    /// that takes seconds and one that takes minutes.
    ///
    /// It is also the right transactional boundary for the consumer: the
    /// batch is committed, and only then are its messages acked. A crash in
    /// between redelivers the whole batch, which changes nothing — the upsert
    /// only ever moves `first_seen` earlier and `last_seen` later, so
    /// applying a sighting twice is applying it once.
    pub fn observe_contacts(&self, sightings: &[(String, String, String)]) -> Result<()> {
        if sightings.is_empty() {
            return Ok(());
        }
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("failed to open the sightings transaction")?;
        {
            let mut statement = transaction
                .prepare(
                    "INSERT INTO contact_seen (contact_id, connection, first_seen, last_seen) \
                     VALUES (?1, ?2, ?3, ?3) \
                     ON CONFLICT (contact_id, connection) DO UPDATE SET \
                         first_seen = MIN(first_seen, excluded.first_seen), \
                         last_seen  = MAX(last_seen,  excluded.last_seen)",
                )
                .context("failed to prepare the sighting statement")?;
            for (contact, connection_id, at) in sightings {
                statement
                    .execute(rusqlite::params![contact, connection_id, at])
                    .context("failed to record a seen contact")?;
            }
        }
        transaction
            .commit()
            .context("failed to commit the sightings")?;
        Ok(())
    }

    /// The contacts waiting for a decision: every seen contact no decision
    /// covers on the network it wrote on, oldest first sighting first.
    ///
    /// Ordered by `first_seen` because that is the order the user met them
    /// in, and because a stable order is what lets the Companion render a
    /// list that does not jump between polls.
    ///
    /// The owner is never in it (ticket #149). [`crate::contacts`] does not
    /// even record a sighting of them, so for anything this build observed
    /// there is nothing to exclude; the exclusion here is for the rows a
    /// deployment upgraded across #109 already holds, because the user's own
    /// messages used to arrive as a contact's. Offering somebody a decision
    /// about their own ghost is the visible half of the defect, and a
    /// `revoked` taken on it would silence their own traffic for any consumer
    /// without the Sensor's filter.
    pub fn pending_contacts(&self) -> Result<Vec<SeenContact>> {
        let (exclusion, identities) = self.pending_owner_exclusion();
        let connection = self.connection();
        let mut statement = connection
            .prepare(&format!(
                "SELECT contact_id, connection, network, first_seen, last_seen \
                 FROM pending_contact {exclusion} ORDER BY first_seen, contact_id, connection"
            ))
            .context("failed to prepare the pending-contact query")?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(identities.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .context("failed to read the pending contacts")?;
        let mut pending = Vec::new();
        for row in rows {
            let (contact, connection, network, first_seen, last_seen) =
                row.context("failed to read a pending-contact row")?;
            pending.push(SeenContact {
                contact,
                connection,
                network: Network::parse(&network)
                    .with_context(|| format!("the store holds the network {network:?}"))?,
                first_seen,
                last_seen,
            });
        }
        Ok(pending)
    }

    /// How many contacts are waiting for a decision — the dashboard's one
    /// number, and the operator's gauge. The owner is excluded exactly as in
    /// [`Self::pending_contacts`], so the number and the list cannot disagree.
    pub fn pending_contact_count(&self) -> Result<u64> {
        let (exclusion, identities) = self.pending_owner_exclusion();
        let count: i64 = self
            .connection()
            .query_row(
                &format!("SELECT COUNT(*) FROM pending_contact {exclusion}"),
                rusqlite::params_from_iter(identities.iter()),
                |row| row.get(0),
            )
            .context("failed to count the pending contacts")?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// [`Self::owner_exclusion`] for the pending list, whose view names its
    /// subject `contact_id` and holds contacts only — so there is no subject
    /// type to qualify.
    fn pending_owner_exclusion(&self) -> (String, Vec<&str>) {
        let identities = self.owner.as_sql_params();
        if identities.is_empty() {
            return (String::new(), identities);
        }
        let placeholders = vec!["?"; identities.len()].join(", ");
        (
            format!("WHERE contact_id NOT IN ({placeholders})"),
            identities,
        )
    }

    fn connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .expect("the consent store mutex is never poisoned")
    }
}

/// One state row as the domain reads it. Shared by the current-state
/// projection and the snapshot, so the two can never disagree about what a
/// stored value means — and so an unknown value is the same loud failure in
/// both: the journal is the record of truth, and a row nobody can read is a
/// bug to see, not a row to skip.
fn entry(
    kind: String,
    id: String,
    connection: String,
    network: String,
    state: String,
    decided_at: String,
    decision_sequence: i64,
) -> Result<Entry> {
    Ok(Entry {
        subject: Subject {
            kind: SubjectType::parse(&kind)
                .with_context(|| format!("the journal holds the subject type {kind:?}"))?,
            id,
        },
        connection,
        network: Network::parse(&network)
            .with_context(|| format!("the journal holds the network {network:?}"))?,
        state: State::parse(&state)
            .with_context(|| format!("the journal holds the state {state:?}"))?,
        decided_at,
        decision_sequence,
    })
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

/// What the two test modules below share.
#[cfg(test)]
mod test_support {
    use super::Network;

    /// One connection per network, named after it — the reference
    /// deployment's registry, and the id every migrated decision landed on
    /// (#270). Recorded on every test store, because the views join on it.
    pub fn implicit_registry() -> Vec<crate::connections::Connection> {
        Network::ALL
            .iter()
            .map(|network| crate::connections::Connection {
                id: network.as_str().to_owned(),
                kind: network.as_str().to_owned(),
                label: network.as_str().to_owned(),
                bridge_id: None,
                bridge_bot: None,
            })
            .collect()
    }
}

/// One row of `connection_status_current`, in the order its columns are
/// selected everywhere it is read.
fn current_status_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<crate::connection_status::Current> {
    Ok(crate::connection_status::Current {
        connection: row.get(0)?,
        kind: row.get(1)?,
        state: row.get(2)?,
        occurred_at: row.get(3)?,
        service: row.get(4)?,
        hint: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every `network IN (…)` a migration ever wrote, as the set it admits.
    fn admitted_by_the_checks() -> Vec<(usize, Vec<String>)> {
        let mut checks = Vec::new();
        for (version, migration) in MIGRATIONS.iter().enumerate() {
            let mut rest = *migration;
            while let Some(at) = rest.find("network IN (") {
                let after = &rest[at + "network IN (".len()..];
                let close = after.find(')').expect("a closed IN list");
                let values: Vec<String> = after[..close]
                    .split(',')
                    .map(|value| value.trim().trim_matches('\'').to_owned())
                    .collect();
                checks.push((version + 1, values));
                rest = &after[close..];
            }
        }
        checks
    }

    /// The contract is the one authority for the network values (ADR 0033,
    /// #268), and the store's `CHECK` constraints are copies of it — frozen
    /// ones, since a migration is never edited in place. So the live schema
    /// is what this test holds to the contract: the constraints the **latest**
    /// constraining migration writes admit exactly the contract's values,
    /// because that migration rebuilt every table still carrying one (#270).
    /// Every earlier copy is history and admits a prefix of the contract —
    /// never a value the contract does not have.
    #[test]
    fn the_latest_checks_admit_exactly_the_contracts_networks() {
        let authority = twalk_test_harness::contract_definition_values("network")
            .expect("the contract's network definition");
        let checks = admitted_by_the_checks();
        let latest = checks
            .iter()
            .map(|(version, _)| *version)
            .max()
            .expect("the migrations constrain network somewhere");
        // A network the contract gains later fails this: the answer is a
        // migration that rebuilds the constrained tables, as v8 did.
        for (version, admitted) in checks {
            if version == latest {
                assert_eq!(
                    admitted, authority,
                    "migration v{version}'s CHECK disagrees with the contract"
                );
            } else {
                assert!(
                    authority.starts_with(&admitted),
                    "migration v{version}'s CHECK ({admitted:?}) is not a prefix of the contract"
                );
            }
        }
    }

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
        let store = Store::open(&dir, Arc::new(test_owner())).expect("the store opens");
        // The registry every Gateway records at startup (#269), one
        // connection per network named after it: what the views join on.
        store
            .record_connections(&test_support::implicit_registry())
            .expect("the registry records");
        store
    }

    /// The owner every store in these tests belongs to: their Matrix ID, and
    /// the WhatsApp LID ghost the reference deployment's own messages arrive
    /// under (#109). Every read of a store excludes them, so a test that wants
    /// to prove it names one of these.
    pub(super) fn test_owner() -> Owner {
        Owner::new(OWNER, [OWNER_GHOST.to_owned()])
    }

    /// The one ghost these deployments have confirmed as the owner's.
    const OWNER_GHOST: &str = "@whatsapp_lid-115332874281144:example.com";

    fn decision(kind: SubjectType, id: &str, state: State, networks: &[Network]) -> Decision {
        Decision {
            subject: Subject {
                kind,
                id: id.to_owned(),
            },
            new_state: state,
            connections: networks
                .iter()
                .map(|network| network.as_str().to_owned())
                .collect(),
            networks: networks.to_vec(),
            reason: None,
        }
    }

    fn record(store: &Store, decision: &Decision, occurred_at: &str) -> Committed {
        store
            .record(decision, OWNER, occurred_at, DOMAIN, occurred_at)
            .expect("the decision records")
    }

    /// Ticket #149, the hard half: a row that already exists.
    ///
    /// Built the way a real deployment built its own — by a Gateway for which
    /// that ghost was an ordinary contact, which is exactly what every Gateway
    /// before #109 was — and then read by one that has been told whose ghost it
    /// is. Nothing migrates and nothing is deleted: the journal is append-only,
    /// and the exclusion is at read time, so the row is served to nobody from
    /// the moment the operator confirms the identity.
    #[test]
    fn a_consent_row_about_the_owner_is_kept_and_served_to_nobody() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "twalk-consent-store-owner-row-{}-{unique}",
            std::process::id()
        ));

        // A Gateway that has not been told about the ghost: it is a contact,
        // and the user's decision about them is recorded like anybody's.
        let before = Store::open(&dir, Arc::new(Owner::new(OWNER, []))).expect("the store opens");
        before
            .record_connections(&test_support::implicit_registry())
            .expect("the registry records");
        let owners_row = record(
            &before,
            &decision(
                SubjectType::Contact,
                OWNER_GHOST,
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:00:00.000Z",
        );
        let contacts_row = record(
            &before,
            &decision(
                SubjectType::Contact,
                "@whatsapp_33612345678:example.com",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:01:00.000Z",
        );
        // Published, so both are inside the snapshot's horizon — otherwise the
        // assertion below that the owner's row is not in the snapshot would
        // hold for the wrong reason.
        for (position, committed) in [owners_row, contacts_row].iter().enumerate() {
            before
                .mark_published(
                    committed.sequence,
                    "2026-09-17T10:05:00.000Z",
                    41 + position as u64,
                )
                .expect("the outbox marks it published");
        }
        before
            .observe_contact(OWNER_GHOST, "signal", "2026-09-17T10:02:00.000Z")
            .expect("a sighting an older build recorded");
        assert!(
            before
                .entries()
                .unwrap()
                .iter()
                .any(|entry| entry.subject.id == OWNER_GHOST),
            "the row exists: this is the deployment #149 is about"
        );
        drop(before);

        // The same file, opened by a Gateway whose operator has confirmed that
        // ghost as their own.
        let after = Store::open(&dir, Arc::new(test_owner())).expect("the store reopens");
        for id in after
            .entries()
            .unwrap()
            .iter()
            .map(|entry| &entry.subject.id)
        {
            assert_ne!(
                id, OWNER_GHOST,
                "GET /api/consent/state serves no owner row"
            );
        }
        let snapshot = after.snapshot(100).unwrap();
        assert!(
            !snapshot.entries.is_empty(),
            "the snapshot has reached the published rows, so its silence about the \
             owner means something"
        );
        for id in snapshot.entries.iter().map(|entry| &entry.subject.id) {
            assert_ne!(id, OWNER_GHOST, "and neither does the snapshot");
        }
        assert!(
            after
                .pending_contacts()
                .unwrap()
                .iter()
                .all(|seen| seen.contact != OWNER_GHOST),
            "and the user is not offered a decision about their own ghost"
        );
        assert_eq!(
            after.pending_contact_count().unwrap(),
            after.pending_contacts().unwrap().len() as u64,
            "the number and the list cannot disagree"
        );
        // The precedence question has no answer about the owner, and the
        // network's default is not one either: `granted` on WhatsApp for
        // everybody would otherwise become `granted` for the user's own ghost.
        record(
            &after,
            &decision(
                SubjectType::Network,
                "whatsapp",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T11:00:00.000Z",
        );
        let effective = after
            .effective(OWNER_GHOST, "whatsapp", Network::Whatsapp)
            .unwrap();
        assert_eq!(effective.state, State::Pending);
        assert_eq!(effective.decided_by, None);
        assert_eq!(
            after
                .effective(
                    "@whatsapp_33612345678:example.com",
                    "whatsapp",
                    Network::Whatsapp
                )
                .unwrap()
                .state,
            State::Granted,
            "a real contact is unaffected: this is an exclusion, not a switch"
        );

        // Nothing was destroyed, and the one read that says so is the
        // operator's.
        let withheld = after.owner_entries().unwrap();
        assert_eq!(withheld.len(), 1);
        assert_eq!(withheld[0].subject.id, OWNER_GHOST);
        assert_eq!(withheld[0].state, State::Granted);

        // And the writer refuses to add another: the store is the floor under
        // `Outbox::record`, so a caller that reached past it fails loudly.
        let refused = after.record(
            &decision(
                SubjectType::Contact,
                OWNER_GHOST,
                State::Revoked,
                &[Network::Whatsapp],
            ),
            OWNER,
            "2026-09-17T12:00:00.000Z",
            DOMAIN,
            "2026-09-17T12:00:00.000Z",
        );
        assert!(refused.is_err(), "the journal refuses the append");
        assert_eq!(
            after.owner_entries().unwrap().len(),
            1,
            "and nothing was appended"
        );
    }

    /// What migration v8 (#270) promises: every `(subject, network)` state
    /// a store held before it is the same `(subject, connection)` state
    /// after it, on the connection named after the network — and the
    /// pending list, the journal's length, the bridge and approval rows all
    /// come through untouched.
    ///
    /// The check is written once and run twice: on a v7 store built by hand
    /// here, and on a **copy of the reference deployment's store** when
    /// `TWALK_REFERENCE_STORE` names one (never committed: it holds the
    /// user's contacts). The copy is what makes the promise about the real
    /// journal and not about a fixture's idea of it.
    fn assert_v8_keeps_every_state(dir: &Path) {
        let path = dir.join(DATABASE_FILE);
        // Before: the store at v7 — brought there from wherever the copy
        // was, by the migrations up to and excluding v8 — read through the
        // views v8 rewrites.
        let before = {
            let raw = Connection::open(&path).expect("the store opens raw");
            let applied: i64 = raw
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert!(
                applied <= 7,
                "a store already at v{applied} has nothing to migrate"
            );
            for (index, migration) in MIGRATIONS.iter().enumerate().take(7).skip(applied as usize) {
                raw.execute_batch(migration)
                    .expect("an earlier migration applies");
                raw.pragma_update(None, "user_version", index as i64 + 1)
                    .unwrap();
            }
            let states = read_rows(
                &raw,
                "SELECT subject_type, subject_id, network, state FROM consent_state \
                 ORDER BY subject_type, subject_id, network",
            );
            let pending = read_rows(
                &raw,
                "SELECT contact_id, network, first_seen, last_seen FROM pending_contact \
                 ORDER BY contact_id, network",
            );
            let counts: Vec<i64> = [
                "consent_decision",
                "consent_decision_network",
                "contact_seen",
                "approval",
                "bridge_status_change",
            ]
            .iter()
            .map(|table| {
                raw.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
            })
            .collect();
            (states, pending, counts)
        };
        assert!(
            !before.0.is_empty(),
            "a store with no decision proves nothing about the migration"
        );

        // After: opened by this build, which applies v8.
        let store = Store::open(dir, Arc::new(Owner::new(OWNER, []))).expect("the store migrates");
        let version: i64 = store
            .connection()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version as usize, MIGRATIONS.len());
        let after_states = read_rows(
            &store.connection(),
            "SELECT subject_type, subject_id, connection, state FROM consent_state \
             ORDER BY subject_type, subject_id, connection",
        );
        assert_eq!(
            after_states, before.0,
            "every (subject, network) state before is the (subject, connection) state after, \
             on the connection named after the network"
        );
        // And `network` is still on every row, derived, the same value.
        let after_networks = read_rows(
            &store.connection(),
            "SELECT subject_type, subject_id, network, state FROM consent_state \
             ORDER BY subject_type, subject_id, network",
        );
        assert_eq!(after_networks, before.0);
        let after_pending = read_rows(
            &store.connection(),
            "SELECT contact_id, connection, first_seen, last_seen FROM pending_contact \
             ORDER BY contact_id, connection",
        );
        assert_eq!(after_pending, before.1, "the pending list is the same list");
        let after_counts: Vec<i64> = [
            "consent_decision",
            "consent_decision_connection",
            "contact_seen",
            "approval",
            "bridge_status_change",
        ]
        .iter()
        .map(|table| {
            store
                .connection()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        })
        .collect();
        assert_eq!(
            after_counts, before.2,
            "the journal, one scope row per network row, the sightings, the approvals and \
             the bridge transitions all come through"
        );
        // The registry rows the migration had to make: one per network the
        // journal or the sightings named, kind and id the same word.
        let registry = read_rows(
            &store.connection(),
            "SELECT id, kind FROM connection WHERE id = kind ORDER BY id",
        );
        let named: std::collections::BTreeSet<&String> = before
            .0
            .iter()
            .map(|row| &row[2])
            .chain(before.1.iter().map(|row| &row[1]))
            .collect();
        for network in named {
            assert!(
                registry.iter().any(|row| &row[0] == network),
                "the connection {network} the migration needed is registered: {registry:?}"
            );
        }
    }

    fn read_rows(connection: &Connection, sql: &str) -> Vec<Vec<String>> {
        let mut statement = connection.prepare(sql).expect("the query prepares");
        let width = statement.column_count();
        statement
            .query_map([], |row| {
                (0..width)
                    .map(|column| {
                        row.get::<_, rusqlite::types::Value>(column)
                            .map(|v| match v {
                                rusqlite::types::Value::Null => String::new(),
                                rusqlite::types::Value::Integer(i) => i.to_string(),
                                rusqlite::types::Value::Real(r) => r.to_string(),
                                rusqlite::types::Value::Text(t) => t,
                                rusqlite::types::Value::Blob(_) => "<blob>".to_owned(),
                            })
                    })
                    .collect()
            })
            .expect("the query runs")
            .map(|row| row.expect("a row reads"))
            .collect()
    }

    #[test]
    fn every_decision_taken_before_the_connection_holds_the_same_state_on_its_networks_one() {
        // A v7 store, built the way a Gateway before #270 built its own:
        // decisions with their scope in `consent_decision_network`, contacts
        // seen per network, an approval and a bridge transition.
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "twalk-consent-store-v7-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let raw = Connection::open(dir.join(DATABASE_FILE)).unwrap();
            for (index, migration) in MIGRATIONS.iter().enumerate().take(7) {
                raw.execute_batch(migration).unwrap();
                raw.pragma_update(None, "user_version", index as i64 + 1)
                    .unwrap();
            }
            let mut sequence = 0;
            let mut decide =
                |kind: &str, id: &str, old: &str, new: &str, networks: &[&str], at: &str| {
                    sequence += 1;
                    raw.execute(
                    "INSERT INTO consent_decision (sequence, event_id, subject_type, subject_id, \
                     old_state, new_state, scope_key, occurred_at, actor, reason, envelope, \
                     published_at, stream_sequence) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, '{}', ?, ?)",
                    rusqlite::params![
                        sequence,
                        format!("event-{sequence}"),
                        kind,
                        id,
                        old,
                        new,
                        networks.join(","),
                        at,
                        OWNER,
                        at,
                        sequence * 10
                    ],
                )
                .unwrap();
                    for network in networks {
                        raw.execute(
                        "INSERT INTO consent_decision_network (sequence, network) VALUES (?, ?)",
                        rusqlite::params![sequence, network],
                    )
                    .unwrap();
                    }
                };
            decide(
                "contact",
                "@a:example.com",
                "unset",
                "granted",
                &["whatsapp"],
                "2026-09-01T10:00:00.000Z",
            );
            decide(
                "contact",
                "@b:example.com",
                "unset",
                "pending",
                &["whatsapp", "signal"],
                "2026-09-02T10:00:00.000Z",
            );
            decide(
                "contact",
                "@b:example.com",
                "pending",
                "revoked",
                &["signal"],
                "2026-09-03T10:00:00.000Z",
            );
            decide(
                "network",
                "telegram",
                "unset",
                "granted",
                &["telegram"],
                "2026-09-04T10:00:00.000Z",
            );
            decide(
                "persona",
                "assistant",
                "unset",
                "granted",
                &["whatsapp", "signal"],
                "2026-09-05T10:00:00.000Z",
            );
            decide(
                "contact",
                "@a:example.com",
                "granted",
                "revoked",
                &["whatsapp"],
                "2026-09-06T10:00:00.000Z",
            );
            decide(
                "contact",
                "@e:example.com",
                "unset",
                "granted",
                &["whatsapp"],
                "2026-09-07T10:00:00.000Z",
            );
            for (contact, network, at) in [
                ("@a:example.com", "whatsapp", "2026-09-01T09:00:00.000Z"),
                ("@c:example.com", "whatsapp", "2026-09-02T09:00:00.000Z"),
                ("@c:example.com", "signal", "2026-09-02T09:30:00.000Z"),
                ("@d:example.com", "telegram", "2026-09-03T09:00:00.000Z"),
            ] {
                raw.execute(
                    "INSERT INTO contact_seen (contact_id, network, first_seen, last_seen) \
                     VALUES (?, ?, ?, ?)",
                    rusqlite::params![contact, network, at, at],
                )
                .unwrap();
            }
            raw.execute(
                "INSERT INTO approval (event_id, suggestion_event_id, approved_by, persona_id, \
                 network, contact, edited, approved_at, published_at, stream_sequence) \
                 VALUES ('approval-1', 'suggestion-1', ?, 'assistant', 'whatsapp', \
                         '@a:example.com', 0, '2026-09-01T11:00:00.000Z', \
                         '2026-09-01T11:00:00.100Z', 77)",
                [OWNER],
            )
            .unwrap();
            raw.execute(
                "INSERT INTO bridge_status_change (event_id, bridge_id, network, from_state, \
                 to_state, occurred_at, envelope, published_at, stream_sequence) \
                 VALUES ('bridge-1', 'bridge-whatsapp', 'whatsapp', 'starting', 'connected', \
                         '2026-09-01T08:00:00.000Z', '{}', '2026-09-01T08:00:00.100Z', 5)",
                [],
            )
            .unwrap();
        }
        assert_v8_keeps_every_state(&dir);

        // The acceptance criterion, spelled out: a decision recorded before
        // the migration is `granted` on `whatsapp` after it — read the way
        // an approval reads it, by connection.
        let store = Store::open(&dir, Arc::new(Owner::new(OWNER, []))).unwrap();
        let effective = store
            .effective("@e:example.com", "whatsapp", Network::Whatsapp)
            .unwrap();
        assert_eq!(
            (effective.state, effective.decided_by.map(|s| s.id)),
            (State::Granted, Some("@e:example.com".to_owned())),
            "granted on whatsapp before, granted on whatsapp after"
        );
        let effective = store
            .effective("@a:example.com", "whatsapp", Network::Whatsapp)
            .unwrap();
        assert_eq!(effective.state, State::Revoked, "the last decision on @a");
        let effective = store
            .effective("@d:example.com", "telegram", Network::Telegram)
            .unwrap();
        assert_eq!(
            (effective.state, effective.decided_by.map(|s| s.id)),
            (State::Granted, Some("telegram".to_owned())),
            "the network default, on the network's connection"
        );
        assert_eq!(
            store.bridge_status("bridge-whatsapp").unwrap(),
            Some(ContractState::Connected),
            "the rebuilt bridge table still answers"
        );
        assert!(
            store.approval("suggestion-1").unwrap().is_some(),
            "the rebuilt approval table still answers"
        );
        // And an `email` decision is no longer refused by a frozen CHECK:
        // the rebuilt tables admit the contract's whole list.
        store
            .record_connections(&[crate::connections::Connection {
                id: "mail-linagora".to_owned(),
                kind: "email".to_owned(),
                label: "Twake Mail".to_owned(),
                bridge_id: None,
                bridge_bot: None,
            }])
            .unwrap();
        store
            .observe_contact(
                "@mail_someone:example.com",
                "mail-linagora",
                "2026-09-07T10:00:00.000Z",
            )
            .expect("an email sighting is admitted");
    }

    /// The same promise, on a copy of the reference deployment's store —
    /// run by hand with `TWALK_REFERENCE_STORE=<dir holding consent.sqlite3>`
    /// (a copy; the migration writes), and skipped without it.
    #[test]
    fn the_reference_deployments_store_migrates_with_every_state_kept() {
        let Some(dir) = std::env::var_os("TWALK_REFERENCE_STORE") else {
            eprintln!("TWALK_REFERENCE_STORE is not set: the reference store is not checked");
            return;
        };
        assert_v8_keeps_every_state(Path::new(&dir));
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
        let reopened = Store::open(&path, Arc::new(test_owner())).expect("the store reopens");
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
    fn the_disclosure_is_on_until_somebody_decides_and_the_journal_keeps_every_decision() {
        let store = store("disclosure");
        assert_eq!(
            store.disclosure_state().unwrap(),
            crate::disclosure::DEFAULT,
            "no row is on, and nobody decided (ADR 0031)"
        );

        let off = store
            .record_disclosure_decision(
                false,
                "2026-09-20T10:00:00.000Z",
                OWNER,
                Some("a test of the switch"),
            )
            .unwrap();
        assert_eq!(
            off,
            DisclosureState {
                enabled: false,
                since: Some("2026-09-20T10:00:00.000Z".to_owned()),
                actor: Some(OWNER.to_owned()),
                reason: Some("a test of the switch".to_owned()),
            }
        );
        assert_eq!(store.disclosure_state().unwrap(), off);

        // Back on: a new row, and the earlier one stays. The record answers
        // "since when and who decided" for the current state, and the
        // journal answers it for every state there ever was.
        let on = store
            .record_disclosure_decision(true, "2026-09-20T11:00:00.000Z", OWNER, None)
            .unwrap();
        assert!(on.enabled);
        assert_eq!(on.since.as_deref(), Some("2026-09-20T11:00:00.000Z"));
        assert_eq!(on.reason, None);
        let rows: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM disclosure_decision", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 2);
    }

    #[test]
    fn the_calendar_location_ships_off_and_is_a_journal_like_the_disclosure() {
        let store = store("calendar-location");
        // #351: a deployment nobody decided on sends no location. The
        // difference from the disclosure is the default and nothing else,
        // and "off because it shipped off" is not "off because somebody
        // turned it off" — a screen says those differently.
        let shipped = store.calendar_location_state().unwrap();
        assert!(!shipped.enabled);
        assert_eq!(shipped.since, None);
        assert_eq!(shipped.actor, None);

        let on = store
            .record_calendar_location_decision(
                true,
                "2026-09-24T10:00:00.000Z",
                OWNER,
                Some("I want travel time"),
            )
            .unwrap();
        assert!(on.enabled);
        assert_eq!(on.since.as_deref(), Some("2026-09-24T10:00:00.000Z"));
        assert_eq!(on.actor.as_deref(), Some(OWNER));
        assert_eq!(on.reason.as_deref(), Some("I want travel time"));
        assert_eq!(store.calendar_location_state().unwrap(), on);

        // Two journals, not one table with a kind: a decision about one
        // switch says nothing about the other.
        assert!(
            store.disclosure_state().unwrap().enabled,
            "opening the location left the disclosure as it shipped"
        );
        store
            .record_disclosure_decision(false, "2026-09-24T10:01:00.000Z", OWNER, None)
            .unwrap();
        assert!(
            store.calendar_location_state().unwrap().enabled,
            "turning the disclosure off left the location where the owner put it"
        );

        let connection = store.connection();
        assert!(
            connection
                .execute("DELETE FROM calendar_location_decision", [])
                .is_err(),
            "a calendar location decision cannot be deleted"
        );
        assert!(
            connection
                .execute(
                    "UPDATE calendar_location_decision SET new_state = 'off'",
                    []
                )
                .is_err(),
            "a calendar location decision cannot be rewritten"
        );
    }

    #[test]
    fn the_disclosure_journal_is_append_only() {
        let store = store("disclosure-append-only");
        store
            .record_disclosure_decision(false, "2026-09-20T10:00:00.000Z", OWNER, None)
            .unwrap();
        let connection = store.connection();
        assert!(
            connection
                .execute("DELETE FROM disclosure_decision", [])
                .is_err(),
            "a disclosure decision cannot be deleted"
        );
        assert!(
            connection
                .execute(
                    "UPDATE disclosure_decision SET new_state = 'on' WHERE sequence = 1",
                    []
                )
                .is_err(),
            "a disclosure decision cannot be rewritten"
        );
        assert!(
            connection
                .execute("UPDATE disclosure_decision SET reason = 'edited'", [])
                .is_err(),
            "not even its reason: every column of this journal is the decision"
        );
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
            .effective("@loud:example.com", "whatsapp", Network::Whatsapp)
            .unwrap();
        assert_eq!(overridden.state, State::Revoked);
        assert_eq!(
            overridden.decided_by.unwrap().kind,
            SubjectType::Contact,
            "the contact's own decision is what answered"
        );
        let defaulted = store
            .effective("@quiet:example.com", "whatsapp", Network::Whatsapp)
            .unwrap();
        assert_eq!(defaulted.state, State::Granted);
        assert_eq!(defaulted.decided_by.unwrap().kind, SubjectType::Network);
        // Another network the user never decided about stays pending, and
        // names no decision: "never decided" is not "revoked".
        let undecided = store
            .effective("@loud:example.com", "telegram", Network::Telegram)
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
            .mark_published(committed.sequence, "2026-09-17T10:00:01.000Z", 42)
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
        let reopened = Store::open(&dir, Arc::new(test_owner())).unwrap();
        assert_eq!(
            reopened
                .effective("@someone:example.com", "matrix", Network::Matrix)
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

    // -----------------------------------------------------------------
    // The snapshot (ticket #50)
    // -----------------------------------------------------------------

    /// Publishes a committed decision the way the outbox does, at the bus
    /// position the bus would have answered with.
    fn publish(store: &Store, sequence: i64, stream_sequence: u64) {
        store
            .mark_published(sequence, "2026-09-17T11:00:00.000Z", stream_sequence)
            .expect("the decision is marked published");
    }

    #[test]
    fn an_empty_journal_snapshots_to_nothing_at_position_zero() {
        let snapshot = store("snapshot-empty").snapshot(100).unwrap();
        assert!(snapshot.entries.is_empty());
        assert_eq!(snapshot.decision_sequence, 0);
        // Zero, so that a consumer's "position plus one" is the start of the
        // stream: with nothing decided there is nothing to have missed.
        assert_eq!(snapshot.stream_sequence, 0);
    }

    #[test]
    fn the_snapshot_reflects_the_published_prefix_and_names_its_position() {
        let store = store("snapshot-prefix");
        let granted = record(
            &store,
            &decision(
                SubjectType::Contact,
                "@a:example.com",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:00:00.000Z",
        );
        let default = record(
            &store,
            &decision(
                SubjectType::Network,
                "signal",
                State::Granted,
                &[Network::Signal],
            ),
            "2026-09-17T10:01:00.000Z",
        );
        publish(&store, granted.sequence, 100);
        publish(&store, default.sequence, 101);

        let snapshot = store.snapshot(100).unwrap();
        assert_eq!(snapshot.decision_sequence, default.sequence);
        assert_eq!(snapshot.stream_sequence, 101);
        // The network default is in it, as much an entry as the contact's
        // own decision.
        assert_eq!(snapshot.entries.len(), 2, "{:?}", snapshot.entries);
        assert!(snapshot.entries.iter().any(
            |entry| entry.subject.kind == SubjectType::Network && entry.subject.id == "signal"
        ));

        // A third decision, committed but not yet on the bus: the snapshot
        // keeps naming the position it can vouch for, and keeps the state
        // that position produced. Anything else would hand a consumer a
        // revocation it is about to be told about again.
        let revoked = record(
            &store,
            &decision(
                SubjectType::Contact,
                "@a:example.com",
                State::Revoked,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:02:00.000Z",
        );
        let waiting = store.snapshot(100).unwrap();
        assert_eq!(waiting.decision_sequence, default.sequence);
        assert_eq!(waiting.stream_sequence, 101);
        assert_eq!(
            waiting
                .entries
                .iter()
                .find(|entry| entry.subject.id == "@a:example.com")
                .map(|entry| entry.state),
            Some(State::Granted),
            "an unpublished revocation is not in the snapshot: the consumer \
             will hear it on the bus after this position"
        );

        // Once it reaches the bus, the snapshot moves with it — and the
        // revocation is explicit, not an absence.
        publish(&store, revoked.sequence, 107);
        let moved = store.snapshot(100).unwrap();
        assert_eq!(moved.decision_sequence, revoked.sequence);
        assert_eq!(moved.stream_sequence, 107);
        assert_eq!(
            moved
                .entries
                .iter()
                .find(|entry| entry.subject.id == "@a:example.com")
                .map(|entry| entry.state),
            Some(State::Revoked)
        );
    }

    #[test]
    fn the_horizon_stops_at_the_first_decision_the_bus_has_not_taken() {
        let store = store("snapshot-horizon");
        let first = record(
            &store,
            &decision(
                SubjectType::Contact,
                "@a:example.com",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:00:00.000Z",
        );
        let second = record(
            &store,
            &decision(
                SubjectType::Contact,
                "@b:example.com",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:01:00.000Z",
        );
        let third = record(
            &store,
            &decision(
                SubjectType::Contact,
                "@c:example.com",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:02:00.000Z",
        );
        // The outbox publishes in order, so this cannot happen — but if a
        // future change ever left a hole, the snapshot must stop at it
        // rather than name a position with a decision missing underneath.
        publish(&store, first.sequence, 10);
        publish(&store, third.sequence, 12);

        let snapshot = store.snapshot(100).unwrap();
        assert_eq!(snapshot.decision_sequence, first.sequence);
        assert_eq!(snapshot.stream_sequence, 10);
        assert_eq!(
            snapshot.entries.len(),
            1,
            "only the decisions before the hole: {:?}",
            snapshot.entries
        );
        let _ = second;
    }

    #[test]
    fn a_persona_decision_is_kept_in_the_journal_and_left_out_of_the_snapshot() {
        let store = store("snapshot-persona");
        let contact = record(
            &store,
            &decision(
                SubjectType::Contact,
                "@a:example.com",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T10:00:00.000Z",
        );
        publish(&store, contact.sequence, 5);
        // A persona activation, written straight into the journal: the write
        // API refuses `persona` until #60 opens it (ADR 0013), and this test
        // is what says the snapshot is ready for it — the exclusion is in
        // SQL, so the snapshot never even reads the row.
        {
            let connection = store.connection();
            connection
                .execute(
                    "INSERT INTO consent_decision \
                     (event_id, subject_type, subject_id, old_state, new_state, scope_key, \
                      occurred_at, actor, reason, envelope, published_at, stream_sequence) \
                     VALUES ('f0'||hex(randomblob(31)), 'persona', 'assistant', 'unset', \
                             'granted', 'whatsapp', '2026-09-17T10:03:00.000Z', ?1, NULL, '{}', \
                             '2026-09-17T11:00:00.000Z', 6)",
                    [OWNER],
                )
                .expect("a persona decision is appended");
            let sequence = connection.last_insert_rowid();
            connection
                .execute(
                    "INSERT INTO consent_decision_network (sequence, network) VALUES (?1, 'whatsapp')",
                    [sequence],
                )
                .expect("its scope is appended");
        }

        let snapshot = store.snapshot(100).unwrap();
        assert_eq!(
            snapshot.entries.len(),
            1,
            "the persona is not consent state a consumer labels senders by: {:?}",
            snapshot.entries
        );
        assert_eq!(snapshot.entries[0].subject.kind, SubjectType::Contact);
        // Its position still counts: the persona decision is on the bus, so
        // a consumer starting after it is not told about it twice.
        assert_eq!(snapshot.stream_sequence, 6);
    }

    #[test]
    fn a_state_over_the_cap_is_refused_whole_rather_than_truncated() {
        let store = store("snapshot-cap");
        for (index, contact) in ["@a:example.com", "@b:example.com", "@c:example.com"]
            .into_iter()
            .enumerate()
        {
            let committed = record(
                &store,
                &decision(
                    SubjectType::Contact,
                    contact,
                    State::Granted,
                    &[Network::Whatsapp],
                ),
                &format!("2026-09-17T10:0{index}:00.000Z"),
            );
            publish(&store, committed.sequence, 20 + index as u64);
        }
        // At the cap: served.
        assert_eq!(store.snapshot(3).unwrap().entries.len(), 3);
        // Over it: refused, and the refusal names the cap rather than
        // handing back the first two entries.
        match store.snapshot(2) {
            Err(SnapshotRefusal::TooLarge { max_entries }) => assert_eq!(max_entries, 2),
            other => panic!("an oversized snapshot must be refused: {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // The pending-contact projection (ticket #54)
    // -----------------------------------------------------------------

    #[test]
    fn a_seen_contact_keeps_its_earliest_and_its_latest_sighting() {
        let store = store("seen-extremes");
        // Deliberately out of order: a full first delivery replays the
        // stream's history, and the stream is not sorted by the Sensor's
        // clock.
        for at in [
            "2026-09-17T12:00:00.000Z",
            "2026-09-17T09:00:00.000Z",
            "2026-09-17T18:00:00.000Z",
        ] {
            store
                .observe_contact("@whatsapp_33612345678:example.com", "whatsapp", at)
                .expect("the sighting records");
        }
        let pending = store.pending_contacts().unwrap();
        assert_eq!(pending.len(), 1, "one contact, one network: {pending:?}");
        assert_eq!(pending[0].first_seen, "2026-09-17T09:00:00.000Z");
        assert_eq!(pending[0].last_seen, "2026-09-17T18:00:00.000Z");
        // The same event delivered twice changes nothing, which is what
        // makes the consumer's commit-then-ack safe.
        store
            .observe_contact(
                "@whatsapp_33612345678:example.com",
                "whatsapp",
                "2026-09-17T12:00:00.000Z",
            )
            .unwrap();
        assert_eq!(store.pending_contacts().unwrap(), pending);
    }

    #[test]
    fn a_contact_is_pending_per_network_it_wrote_on() {
        let store = store("seen-per-network");
        store
            .observe_contact("@a:example.com", "whatsapp", "2026-09-17T10:00:00.000Z")
            .unwrap();
        store
            .observe_contact("@a:example.com", "signal", "2026-09-17T10:01:00.000Z")
            .unwrap();
        store
            .observe_contact("@b:example.com", "whatsapp", "2026-09-17T10:02:00.000Z")
            .unwrap();
        assert_eq!(store.pending_contact_count().unwrap(), 3);
        // Oldest first sighting first: the order the user met them in.
        let pending = store.pending_contacts().unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|seen| (seen.contact.as_str(), seen.network))
                .collect::<Vec<_>>(),
            vec![
                ("@a:example.com", Network::Whatsapp),
                ("@a:example.com", Network::Signal),
                ("@b:example.com", Network::Whatsapp),
            ]
        );
    }

    #[test]
    fn a_decision_takes_a_contact_out_of_the_pending_list() {
        let store = store("seen-decided");
        store
            .observe_contact("@a:example.com", "whatsapp", "2026-09-17T10:00:00.000Z")
            .unwrap();
        store
            .observe_contact("@a:example.com", "signal", "2026-09-17T10:00:00.000Z")
            .unwrap();
        assert_eq!(store.pending_contact_count().unwrap(), 2);

        record(
            &store,
            &decision(
                SubjectType::Contact,
                "@a:example.com",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T11:00:00.000Z",
        );
        let pending = store.pending_contacts().unwrap();
        assert_eq!(
            pending.iter().map(|seen| seen.network).collect::<Vec<_>>(),
            vec![Network::Signal],
            "the decision covered whatsapp and left the other network waiting: {pending:?}"
        );

        // A revocation is a decision too: the contact is answered, so it is
        // not waiting.
        record(
            &store,
            &decision(
                SubjectType::Contact,
                "@a:example.com",
                State::Revoked,
                &[Network::Signal],
            ),
            "2026-09-17T11:01:00.000Z",
        );
        assert_eq!(store.pending_contact_count().unwrap(), 0);
    }

    #[test]
    fn a_network_default_answers_for_every_contact_on_it() {
        let store = store("seen-default");
        for contact in ["@a:example.com", "@b:example.com", "@c:example.com"] {
            store
                .observe_contact(contact, "whatsapp", "2026-09-17T10:00:00.000Z")
                .unwrap();
        }
        store
            .observe_contact("@d:example.com", "telegram", "2026-09-17T10:00:00.000Z")
            .unwrap();
        assert_eq!(store.pending_contact_count().unwrap(), 4);

        record(
            &store,
            &decision(
                SubjectType::Network,
                "whatsapp",
                State::Granted,
                &[Network::Whatsapp],
            ),
            "2026-09-17T11:00:00.000Z",
        );
        let pending = store.pending_contacts().unwrap();
        assert_eq!(
            pending.len(),
            1,
            "one default answered for three contacts at once: {pending:?}"
        );
        assert_eq!(pending[0].contact, "@d:example.com");
    }

    #[test]
    fn a_contact_the_user_decided_pending_is_not_waiting_for_a_decision() {
        let store = store("seen-decided-pending");
        store
            .observe_contact("@a:example.com", "whatsapp", "2026-09-17T10:00:00.000Z")
            .unwrap();
        record(
            &store,
            &decision(
                SubjectType::Contact,
                "@a:example.com",
                State::Pending,
                &[Network::Whatsapp],
            ),
            "2026-09-17T11:00:00.000Z",
        );
        assert_eq!(
            store.pending_contact_count().unwrap(),
            0,
            "the user answered, and the answer was \"not yet\": an absent decision \
             and a decision to wait are not the same thing (ADR 0010)"
        );
    }

    #[test]
    fn the_contact_table_has_no_column_for_content() {
        // The restraint is the feature, so it is asserted rather than
        // trusted: this test fails the day somebody adds a `body`, a
        // `display_name` or a `network_identifier` column to the store of
        // who writes to the user.
        let store = store("seen-columns");
        let connection = store.connection();
        let mut statement = connection
            .prepare("SELECT name FROM pragma_table_info('contact_seen') ORDER BY cid")
            .unwrap();
        let columns: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|column| column.unwrap())
            .collect();
        assert_eq!(
            columns,
            vec!["contact_id", "connection", "first_seen", "last_seen"],
            "the pending-contact store holds a Matrix ID, a connection and two instants, \
             and nothing else: see the migration's own comment and \
             docs/architecture/security-model.md"
        );
    }

    #[test]
    fn the_approval_table_has_no_column_for_the_reply_that_was_sent() {
        // An approval is the act of sending a message, so this is the table
        // where holding the message would have been the natural thing to do.
        // It is not held: the text goes on the bus and is read back from
        // there. This test fails the day a `body`, a `final` or a `suggestion`
        // column appears.
        let store = store("approval-columns");
        let connection = store.connection();
        let mut statement = connection
            .prepare("SELECT name FROM pragma_table_info('approval') ORDER BY cid")
            .unwrap();
        let columns: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|column| column.unwrap())
            .collect();
        assert_eq!(
            columns,
            vec![
                "sequence",
                "event_id",
                "suggestion_event_id",
                "approved_by",
                "persona_id",
                "network",
                "contact",
                "edited",
                "approved_at",
                "published_at",
                "stream_sequence",
                // #327: whose words went out. A boolean's worth of fact
                // about authorship, added by a migration and therefore
                // last — never the text itself, which is the rule this
                // test exists for.
                "written_by",
            ],
            "an approval is an id, an owner, a boolean and a position — never the text that \
             was approved"
        );
    }

    #[test]
    fn an_approval_is_recorded_unpublished_and_then_marked() {
        let store = store("approval-lifecycle");
        let suggestion = "a".repeat(64);
        let event = "c".repeat(64);
        store
            .record_approval(
                &event,
                &suggestion,
                "@michel:example.com",
                "assistant",
                Network::Whatsapp,
                "@whatsapp_336:example.com",
                true,
                "persona",
                "2026-09-17T10:04:37.000Z",
            )
            .unwrap();
        let recorded = store.approval(&suggestion).unwrap().expect("recorded");
        assert_eq!(recorded.event_id, event);
        assert_eq!(recorded.publication(), "unpublished");
        assert!(recorded.edited);
        assert_eq!(recorded.stream_sequence, None);

        store
            .mark_approval_published(&event, "2026-09-17T10:04:37.100Z", 4242)
            .unwrap();
        let published = store.approval(&suggestion).unwrap().expect("recorded");
        assert_eq!(published.publication(), "published");
        assert_eq!(published.stream_sequence, Some(4242));

        // Recording it again writes nothing and does not move the position:
        // that is what makes the crash-repair path safe to run twice.
        store
            .record_approval(
                &event,
                &suggestion,
                "@michel:example.com",
                "assistant",
                Network::Whatsapp,
                "@whatsapp_336:example.com",
                false,
                "persona",
                "2026-09-17T11:00:00.000Z",
            )
            .unwrap();
        store
            .mark_approval_published(&event, "2026-09-17T11:00:00.000Z", 9999)
            .unwrap();
        let again = store.approval(&suggestion).unwrap().expect("recorded");
        assert_eq!(again, published, "a replay changes nothing");
    }

    #[test]
    fn a_suggestion_nobody_approved_has_no_approval() {
        let store = store("approval-absent");
        assert_eq!(store.approval(&"f".repeat(64)).unwrap(), None);
    }

    #[test]
    fn a_store_from_the_previous_schema_holds_its_horizon_back() {
        let store = store("snapshot-migrated");
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
        // What schema v1 left behind: published, with no position recorded.
        store
            .connection()
            .execute(
                "UPDATE consent_decision SET published_at = '2026-09-17T11:00:00.000Z', \
                 stream_sequence = NULL WHERE sequence = ?1",
                [committed.sequence],
            )
            .unwrap();
        let snapshot = store.snapshot(100).unwrap();
        assert_eq!(
            snapshot.decision_sequence, 0,
            "a decision whose position nobody recorded cannot be vouched for"
        );
        assert!(snapshot.entries.is_empty());
        // And the safe direction it fails in: the consumer starts at the
        // beginning of the stream and applies that decision from the bus.
        assert_eq!(snapshot.stream_sequence, 0);
    }
}

#[cfg(test)]
mod bridge_status_tests {
    use super::tests::test_owner;
    use super::*;
    use crate::bridge_status::Transition;

    const DOMAIN: &str = "example.com";

    fn store(test_name: &str) -> Store {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "twalk-bridge-status-store-{test_name}-{}-{unique}",
            std::process::id()
        ));
        let store = Store::open(&dir, Arc::new(test_owner())).expect("the store opens");
        // The registry every Gateway records at startup (#269), one
        // connection per network named after it: what the views join on.
        store
            .record_connections(&test_support::implicit_registry())
            .expect("the registry records");
        store
    }

    fn transition(from: ContractState, to: ContractState, occurred_at: &str) -> Transition {
        Transition {
            bridge_id: "bridge-whatsapp".to_owned(),
            network: Network::Whatsapp,
            from_state: from,
            to_state: to,
            occurred_at: occurred_at.to_owned(),
            reason: None,
            last_message_at: None,
        }
    }

    #[test]
    fn a_bridge_nothing_is_known_about_has_no_recorded_state() {
        let store = store("unknown");
        assert_eq!(store.bridge_status("bridge-whatsapp").unwrap(), None);
    }

    #[test]
    fn the_last_recorded_transition_is_the_bridges_current_state() {
        let store = store("current");
        for (from, to, at) in [
            (
                ContractState::Disconnected,
                ContractState::Starting,
                "2026-09-17T10:00:00.000Z",
            ),
            (
                ContractState::Starting,
                ContractState::Connected,
                "2026-09-17T10:00:05.000Z",
            ),
            (
                ContractState::Connected,
                ContractState::Degraded,
                "2026-09-17T10:10:00.000Z",
            ),
        ] {
            store
                .record_bridge_status(&transition(from, to, at), DOMAIN, at)
                .expect("the transition is recorded");
        }
        assert_eq!(
            store.bridge_status("bridge-whatsapp").unwrap(),
            Some(ContractState::Degraded)
        );
        // Another bridge's history is its own: one row per bridge, and no
        // bridge inherits a neighbour's state.
        assert_eq!(store.bridge_status("bridge-signal").unwrap(), None);
    }

    #[test]
    fn the_identical_transition_arriving_twice_records_once() {
        let store = store("replay");
        let change = transition(
            ContractState::Connected,
            ContractState::SessionExpired,
            "2026-09-17T10:00:00.000Z",
        );
        let first = store
            .record_bridge_status(&change, DOMAIN, "2026-09-17T10:00:00.000Z")
            .expect("recorded");
        // A retried push: mautrix retries with backoff, so the same body
        // genuinely arrives twice.
        let second = store
            .record_bridge_status(&change, DOMAIN, "2026-09-17T10:00:09.000Z")
            .expect("recorded");
        assert!(!first.replayed);
        assert!(second.replayed);
        assert_eq!(first.sequence, second.sequence);
        assert_eq!(first.event_id, second.event_id);
        assert_eq!(
            store.unpublished_bridge_status(10).unwrap().len(),
            1,
            "one row, so one event"
        );
    }

    #[test]
    fn a_recorded_transition_waits_in_the_outbox_until_it_is_marked() {
        let store = store("outbox");
        let change = transition(
            ContractState::Disconnected,
            ContractState::Connected,
            "2026-09-17T10:00:00.000Z",
        );
        let committed = store
            .record_bridge_status(&change, DOMAIN, "2026-09-17T10:00:00.000Z")
            .expect("recorded");
        assert_eq!(store.unpublished_bridge_status_count().unwrap(), 1);
        let waiting = store.unpublished_bridge_status(10).unwrap();
        assert_eq!(waiting[0].event_id, committed.event_id);
        assert_eq!(
            waiting[0].envelope["type"],
            serde_json::json!("fr.linagora.twalk.bridge.status.changed.v1")
        );
        assert_eq!(
            waiting[0].envelope["source"],
            serde_json::json!("gateway://example.com/bridges/bridge-whatsapp")
        );
        store
            .mark_bridge_status_published(committed.sequence, "2026-09-17T10:00:01.000Z", 42)
            .expect("marked");
        assert_eq!(store.unpublished_bridge_status_count().unwrap(), 0);
        assert!(store.unpublished_bridge_status(10).unwrap().is_empty());
    }

    #[test]
    fn the_registry_is_recorded_once_refreshed_when_known_and_never_forgotten() {
        // #269: the table a consent decision will be scoped to (#270). A
        // connection that left the configuration stays — a decision may
        // still name it — and one that is back is the same row, with its
        // first `created_at`, not a new one.
        // A bare store: `store()` records a registry of its own, and this
        // test is about what recording does.
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "twalk-connections-recorded-{}-{unique}",
            std::process::id()
        ));
        let store = Store::open(&dir, Arc::new(test_owner())).expect("the store opens");
        let connection = |id: &str, kind: &str, label: &str, bridge: Option<&str>| {
            crate::connections::Connection {
                id: id.to_owned(),
                kind: kind.to_owned(),
                label: label.to_owned(),
                bridge_id: bridge.map(str::to_owned),
                bridge_bot: None,
            }
        };
        store
            .record_connections(&[
                connection(
                    "whatsapp",
                    "whatsapp",
                    "mautrix-whatsapp",
                    Some("mautrix-whatsapp"),
                ),
                connection("matrix", "matrix", "example.com", None),
            ])
            .expect("recorded");
        let rows = |store: &Store| -> Vec<(String, String, String, Option<String>, String)> {
            let connection = store.connection();
            let mut statement = connection
                .prepare(
                    "SELECT id, kind, label, bridge_id, created_at FROM connection ORDER BY id",
                )
                .unwrap();
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .unwrap();
            rows.map(Result::unwrap).collect()
        };
        let first = rows(&store);
        assert_eq!(first.len(), 2);
        assert_eq!(
            (
                first[1].0.as_str(),
                first[1].2.as_str(),
                first[1].3.as_deref()
            ),
            ("whatsapp", "mautrix-whatsapp", Some("mautrix-whatsapp"))
        );

        // The next start: WhatsApp relabelled, Matrix gone from the
        // configuration, Signal new.
        let stale = store
            .record_connections(&[
                connection("whatsapp", "whatsapp", "Home", Some("mautrix-whatsapp")),
                connection("signal", "signal", "mautrix-signal", Some("mautrix-signal")),
            ])
            .expect("recorded again");
        assert_eq!(
            stale,
            ["matrix"],
            "the connection the registry no longer names is kept, and named back"
        );
        let second = rows(&store);
        let ids: Vec<&str> = second.iter().map(|row| row.0.as_str()).collect();
        assert_eq!(ids, ["matrix", "signal", "whatsapp"], "never deleted here");
        assert_eq!(second[2].2, "Home", "the label follows the configuration");
        assert_eq!(
            second[2].4, first[1].4,
            "a connection recorded again keeps the instant it was first recorded"
        );
    }

    #[test]
    fn a_bridges_state_survives_the_store_being_reopened() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "twalk-bridge-status-reopen-{}-{unique}",
            std::process::id()
        ));
        {
            let store = Store::open(&dir, Arc::new(test_owner())).expect("the store opens");
            store
                .record_bridge_status(
                    &transition(
                        ContractState::Disconnected,
                        ContractState::Connected,
                        "2026-09-17T10:00:00.000Z",
                    ),
                    DOMAIN,
                    "2026-09-17T10:00:00.000Z",
                )
                .expect("recorded");
        }
        // This is what makes de-duplication survive a Gateway restart: the
        // state a push is compared with comes back from disk, so the first
        // push after a restart does not become a transition out of nowhere.
        let reopened = Store::open(&dir, Arc::new(test_owner())).expect("the store reopens");
        assert_eq!(
            reopened.bridge_status("bridge-whatsapp").unwrap(),
            Some(ContractState::Connected)
        );
    }

    /// #275: a transition recorded once whatever the redeliveries, the
    /// current view the latest, the changes newest first.
    #[test]
    fn a_connections_transitions_are_recorded_once_and_the_latest_is_its_state() {
        let store = store("connection-status");
        let change = |id: &str, from: &str, to: &str, at: &str| crate::connection_status::Change {
            event_id: id.to_owned(),
            connection: "mail-linagora".to_owned(),
            kind: "email".to_owned(),
            from_state: from.to_owned(),
            to_state: to.to_owned(),
            occurred_at: at.to_owned(),
            service: Some("sso".to_owned()),
            hint: Some("Run `twalk-collector authorize --renew`.".to_owned()),
        };
        let first = change("e1", "unknown", "connected", "2026-09-20T09:00:00Z");
        assert!(store
            .record_connection_status_change(&first, "now")
            .unwrap());
        assert!(
            !store
                .record_connection_status_change(&first, "now")
                .unwrap(),
            "a redelivered transition records nothing twice"
        );
        let second = change(
            "e2",
            "connected",
            "reconnect_required",
            "2026-09-20T10:00:00Z",
        );
        assert!(store
            .record_connection_status_change(&second, "now")
            .unwrap());
        let current = store.connection_status("mail-linagora").unwrap().unwrap();
        assert_eq!(current.state, "reconnect_required");
        assert_eq!(current.kind, "email");
        assert!(store
            .connection_status("agenda-linagora")
            .unwrap()
            .is_none());
        assert_eq!(store.connection_statuses().unwrap().len(), 1);
        let changes = store.connection_status_changes(10).unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].event_id, "e2", "newest first");
        assert_eq!(store.connection_status_changes(1).unwrap().len(), 1);
    }
}
