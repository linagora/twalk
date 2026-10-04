//! The owner's mailbox, polled (issue #276): the I/O the pure `jmap` module
//! is wrapped in — the session read, the API asked, the Email state kept in
//! the state directory, the consent decision read on the mail connection —
//! kept apart from publishing so the state moves only after the bus took
//! the events.
//!
//! No backfill (#251): the first poll takes the current Email state and
//! publishes nothing; from then on `Email/changes` from the persisted state
//! says what arrived, the INBOX among the changed mails is read — a mail in
//! Sent or Archive is not read at all — and each one the frontier lets
//! through is published in the shape its sender's consent selects. A state
//! the server no longer serves is #277's to recover; here it is said and
//! the poll stops until then.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, warn};
use twalk_consent_cache::ConsentCache;

use crate::jmap::{self, Changes, Dropped, Envelopes, Mail, Session};
use crate::outbound::{self, ApprovedReply, SendError};
use crate::side::{self, SideError};

/// The mail connection this process holds, and what publishing about it
/// needs.
pub struct Mailbox {
    pub connection: String,
    /// Every address the owner holds (#322), which is what the frontier, the
    /// audience and the reply path each ask about.
    pub owner: crate::owner::Owner,
    pub session_url: String,
    pub state_dir: PathBuf,
    pub consent: ConsentCache,
    http: reqwest::Client,
    /// What the Companion Gateway tells this mailbox about triage (#416-#418).
    pub governed: FromTheGateway,
    /// When the inbox was last swept for mail old enough for an
    /// `older_than_days` rule. `None` until the first sweep, which is why one
    /// happens at start.
    swept_at: std::sync::Mutex<Option<std::time::Instant>>,
}

/// What the Companion Gateway tells this mailbox: the owner's triage rules,
/// the undos they asked for, and where to report what moved.
///
/// One value rather than three parameters because they travel together and
/// come from one place — the collection seam, re-read before each round. The
/// two shared cells are shared rather than passed for the reason the working
/// day is (#381): a decision taken on the settings screen reaches the next
/// round rather than the next restart.
#[derive(Clone, Default)]
pub struct FromTheGateway {
    pub triage: crate::triage::SharedTriage,
    pub undos: crate::triage::SharedUndos,
    /// Where to report what was moved, and with what. `None` on a deployment
    /// with no Gateway: it still files, and the record is what it loses.
    pub report_to: Option<(String, String)>,
}

/// What the collector holds about the mailbox between polls: the account,
/// the INBOX, and the Email state it last read up to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MailState {
    pub account_id: String,
    pub inbox_id: String,
    pub state: String,
    /// When the mailbox was last read up to `state` (#277): where the
    /// look-back window starts from when the state is lost. Absent on a
    /// state an earlier build wrote: a recovery from such a state has no
    /// window to look back over and takes the mailbox as it stands, as a
    /// first start does, rather than list everything the INBOX holds.
    #[serde(default)]
    pub last_read_at: String,
    /// The Email ids read most recently, newest last, capped at
    /// [`PUBLISHED_RING`] (#277): what a recovery sets aside as already
    /// published. Ids, never a word of a mail.
    #[serde(default)]
    pub published: Vec<String>,
}

/// How many read ids the state remembers: what a look-back window can hold
/// at any plausible rate, and nothing a person could be identified by.
pub const PUBLISHED_RING: usize = 500;

/// How far before the last read a recovery looks (#251: five minutes).
pub const LOOK_BACK: std::time::Duration = std::time::Duration::from_secs(300);

impl MailState {
    fn remember(&mut self, id: &str) {
        if self.published.iter().any(|known| known == id) {
            return;
        }
        self.published.push(id.to_owned());
        if self.published.len() > PUBLISHED_RING {
            let excess = self.published.len() - PUBLISHED_RING;
            self.published.drain(..excess);
        }
    }
}

/// The `state` of an `Email/get` for no ids: the account's current one.
fn email_state_of(result: &Value) -> String {
    result
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The instant a recovery lists mail from: the last read, less the
/// look-back; `None` when no read was ever recorded.
fn look_back_from(last_read_at: &str) -> Option<String> {
    let at =
        time::OffsetDateTime::parse(last_read_at, &time::format_description::well_known::Rfc3339)
            .ok()?
            - LOOK_BACK;
    at.replace_nanosecond(0)
        .unwrap_or(at)
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

/// How often the inbox is swept for mail old enough for an `older_than_days`
/// rule (#417).
///
/// Not every poll: the poll runs every few seconds and a sweep is a query over
/// the whole inbox, so sweeping at that rate would ask the owner's server for
/// the same answer hundreds of times an hour to move nothing. An hour is the
/// grain the rule itself works at — a rule about *days* does not need
/// minutes — and the first sweep happens at start, so a deployment that has
/// just been given a rule does not wait an hour to honour it.
const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(3600);

/// One mail the owner's rules filed: what moved, by which rule, and from
/// where (#417).
///
/// The mailbox it came from is the whole point of recording this: an undo is a
/// second move rather than a recovery (#418, ADR 0042). No subject, no sender
/// — the identity of the mail and the mailboxes, and nothing a contact wrote.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filed {
    pub email_id: String,
    pub rule_id: String,
    pub from_mailbox_id: String,
    pub from_mailbox_name: String,
    pub to_mailbox_id: String,
    pub to_mailbox_name: String,
    /// The move this one reverses, when the owner asked for it back (#418).
    pub undoes: Option<i64>,
}

/// What one poll found. No `Debug`: the envelopes hold the senders' words.
#[derive(Default)]
pub struct MailPoll {
    pub envelopes: Vec<Value>,
    /// L'`accountId` JMAP sous lequel ce poll a lu — local à `poll()`
    /// (`session.account_id`) et remonté ici pour que l'appelant puisse
    /// indexer les mails (lot 3a, #XXX) : `MailSource::new` ne peut pas s'en
    /// passer, et l'id du document en dépend (§3.3).
    pub account: String,
    /// Les mails publiés de ce poll, dans l'ordre des `envelopes` : le
    /// collecteur les indexe pour la recherche (lot 3a, #XXX). Même ensemble
    /// que les enveloppes — ce que la frontière a laissé passer — pour que
    /// l'index et le bus ne puissent pas diverger.
    pub mails: Vec<Mail>,
    /// The mails the frontier dropped, by reason — counted, never named.
    pub dropped: Vec<Dropped>,
    /// What the owner's rules filed, and where from (#417). Reported to the
    /// Gateway so the owner can read it back and undo it (#418).
    pub filed: Vec<Filed>,
    /// A rule that matched and could not be applied, with the reason. Counted
    /// and logged rather than swallowed: a rule that silently does nothing is
    /// the one failure an owner cannot see.
    pub unusable: Vec<(String, crate::triage::Unusable)>,
    /// The state to write once the envelopes are on the bus; `None` when
    /// nothing moved.
    pub state: Option<MailState>,
}

impl Mailbox {
    pub fn new(
        connection: &str,
        owner: &crate::owner::Owner,
        session_url: &str,
        state_dir: &std::path::Path,
        governed: FromTheGateway,
        consent: ConsentCache,
    ) -> Result<Self> {
        Ok(Self {
            connection: connection.to_owned(),
            owner: owner.clone(),
            session_url: session_url.to_owned(),
            state_dir: state_dir.to_owned(),
            consent,
            governed,
            swept_at: std::sync::Mutex::new(None),
            http: side::client()?,
        })
    }

    fn state_path(&self) -> PathBuf {
        self.state_dir
            .join("jmap")
            .join(format!("{}.json", self.connection))
    }

    pub fn read_state(&self) -> Result<Option<MailState>> {
        let path = self.state_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(serde_json::from_str(&text).with_context(|| {
                format!(
                    "{} is not a mail state this collector wrote",
                    path.display()
                )
            })?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    /// Writes the state a poll handed back, after its envelopes were
    /// published.
    pub fn commit(&self, state: &MailState) -> Result<()> {
        crate::fs::write_json_private(&self.state_path(), state)
    }

    /// The mail host, for `source`.
    fn host(&self) -> String {
        side::host_of(&self.session_url)
    }

    /// One poll: the session, then either the first state (published as
    /// nothing) or the changes since the persisted one, read and published.
    /// A state the server no longer serves changes from is recovered by the
    /// look-back window (#277).
    pub async fn poll(
        &self,
        credential: &crate::side::Credential,
        now: &str,
    ) -> Result<MailPoll, SideError> {
        let session =
            Session::parse(&self.get(&self.session_url, credential).await?).map_err(|error| {
                SideError::Unreachable {
                    detail: format!("the JMAP session cannot be read: {error:#}"),
                }
            })?;
        let account = session.account_id.as_str();
        let mut poll = MailPoll::default();
        // The JMAP account this round reads under, carried out for the
        // index (lot 3a) — set before any early return, so a poll that
        // publishes nothing still tells the caller which account it read.
        poll.account = account.to_owned();
        let previous = match self.read_state() {
            Ok(state) => state,
            Err(error) => {
                warn!(error = %format!("{error:#}"), "the mail state cannot be read; the mailbox is not polled");
                return Ok(poll);
            }
        };
        let Some(previous) = previous.filter(|state| state.account_id == account) else {
            // The first start, or another account's state: the current
            // state is taken and nothing before it is published.
            let response = self
                .call(
                    &session.api_url,
                    credential,
                    vec![jmap::mailbox_get(account), jmap::email_state(account)],
                )
                .await?;
            let inbox_id =
                jmap::inbox_id(&method(&response, 0)?).ok_or_else(|| SideError::Unreachable {
                    detail: "the JMAP server lists no INBOX".to_owned(),
                })?;
            let state = method(&response, 1)?
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            info!(account, inbox = %inbox_id, state = %state, "mailbox taken as it stands; nothing of it published");
            poll.state = Some(MailState {
                account_id: account.to_owned(),
                inbox_id,
                state,
                last_read_at: now.to_owned(),
                published: Vec::new(),
            });
            return Ok(poll);
        };
        let response = self
            .call(
                &session.api_url,
                credential,
                vec![jmap::email_changes(account, &previous.state)],
            )
            .await?;
        let (created, new_state, recovered) = match method(&response, 0) {
            Ok(result) => {
                let changes = Changes::parse(&result);
                // Only a mail newly created is a message received; an
                // update (a flag, a move) is not a second delivery.
                (changes.created, changes.new_state, false)
            }
            Err(SideError::Unreachable { detail })
                if detail.contains("cannotCalculateChanges")
                    || detail.contains("invalidArguments") =>
            {
                // The server forgot the state (#277): everything delivered
                // to the INBOX since a little before the last read is
                // listed, what was already published is set aside by its
                // id, and the current state becomes the cursor again. Said,
                // because a resynchronisation is a fact the operator reads.
                let since = match look_back_from(&previous.last_read_at) {
                    Some(since) => since,
                    None => {
                        let response = self
                            .call(
                                &session.api_url,
                                credential,
                                vec![jmap::email_state(account)],
                            )
                            .await?;
                        let state = email_state_of(&method(&response, 0)?);
                        warn!(
                            lost_state = %previous.state,
                            "the JMAP server no longer serves changes from the persisted state and no last read is recorded: mailbox taken as it stands again"
                        );
                        return Ok(MailPoll {
                            state: Some(MailState {
                                state,
                                last_read_at: now.to_owned(),
                                ..previous.clone()
                            }),
                            ..poll
                        });
                    }
                };
                // The state first, then the window page by page: a mail
                // that arrives between the two is in the window and after
                // the state, read now and not again.
                let response = self
                    .call(
                        &session.api_url,
                        credential,
                        vec![jmap::email_state(account)],
                    )
                    .await?;
                let state = email_state_of(&method(&response, 0)?);
                let mut ids: Vec<String> = Vec::new();
                loop {
                    let response = self
                        .call(
                            &session.api_url,
                            credential,
                            vec![jmap::email_received_after(
                                account,
                                &previous.inbox_id,
                                &since,
                                ids.len(),
                            )],
                        )
                        .await?;
                    let page: Vec<String> = method(&response, 0)?
                        .get("ids")
                        .and_then(Value::as_array)
                        .map(|ids| {
                            ids.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default();
                    let short = page.len() < jmap::QUERY_PAGE;
                    ids.extend(page);
                    if short {
                        break;
                    }
                }
                let unseen: Vec<String> = ids
                    .into_iter()
                    .filter(|id| !previous.published.contains(id))
                    .collect();
                warn!(
                    lost_state = %previous.state,
                    since = %since,
                    to_read = unseen.len(),
                    "the JMAP server no longer serves changes from the persisted state: recovered from the look-back window"
                );
                (unseen, state, true)
            }
            Err(error) => return Err(error),
        };
        let mut next = MailState {
            state: new_state,
            last_read_at: now.to_owned(),
            ..previous.clone()
        };
        // A round where nothing new arrived still sweeps and still files: an
        // `older_than_days` rule matters most on a quiet mailbox, and an early
        // return here would have meant it never ran there at all (found by the
        // end-to-end test, which is what it is for).
        let nothing_new = created.is_empty();
        let in_inbox = if nothing_new {
            Vec::new()
        } else if recovered {
            // The query was already the INBOX's.
            created
        } else {
            let response = self
                .call(
                    &session.api_url,
                    credential,
                    vec![jmap::email_mailboxes(account, &created)],
                )
                .await?;
            jmap::in_mailbox(&method(&response, 0)?, &previous.inbox_id)
        };
        let envelopes = Envelopes::new(
            &self.connection,
            &self.host(),
            account,
            &previous.inbox_id,
            &self.owner,
        );
        // Every mail this round read, for the rules to be asked about after
        // the envelopes are built.
        let mut triaged: Vec<Mail> = Vec::new();
        if !in_inbox.is_empty() {
            let response = self
                .call(
                    &session.api_url,
                    credential,
                    vec![jmap::email_get(account, &in_inbox)],
                )
                .await?;
            let list = method(&response, 0)?;
            for email in list
                .get("list")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let mail = match Mail::parse(email) {
                    Ok(mail) => mail,
                    Err(error) => {
                        warn!(error = %format!("{error:#}"), "a mail could not be read; nothing published");
                        continue;
                    }
                };
                // Remembered whatever the frontier says, so a recovery
                // never re-reads it either.
                next.remember(&mail.id);
                match jmap::frontier(&mail, &self.owner) {
                    Ok(()) => {
                        let consent = self.consent.state(&mail.from.mailto(), &self.connection);
                        poll.envelopes
                            .push(envelopes.message_received(&mail, consent, now));
                        // Le même mail, gardé pour l'index (C22) : ce que la
                        // frontière laisse passer est indexé comme il est publié.
                        poll.mails.push(mail.clone());
                    }
                    Err(why) => poll.dropped.push(why),
                }
                // Triage is asked about **every** mail of the inbox, including
                // the ones the frontier dropped (#417). That is the point: a
                // newsletter is exactly what the owner wants filed, and it is
                // exactly what the frontier refuses to publish. The two
                // decisions are about different things — what reaches the bus,
                // and where the mail lives — and conflating them would make
                // triage useless on the mail it exists for.
                triaged.push(mail);
            }
        }
        // The sweep: mail already in the inbox and old enough for an
        // `older_than_days` rule (#417). Added to what this round triages and
        // **never** to what it publishes — `poll.envelopes` is built above and
        // is not touched here.
        //
        // That separation is the whole of it. The collector does no backfill
        // (#251): what the inbox held before it started is the past and is not
        // news. Reading an old mail to decide where it lives is a different
        // act from announcing it, and conflating the two would put months of
        // the owner's old mail on the bus the first time they wrote a rule
        // about age.
        triaged.extend(
            self.sweep(&session, credential, account, &previous.inbox_id, now)
                .await,
        );
        // Filed after the envelopes are built, never before: a mail is
        // published as a trigger from the inbox it arrived in, and a move that
        // raced the read would publish a source that had already changed.
        self.file(&session, credential, account, &triaged, &mut poll, now)
            .await;
        // On a quiet round the state is written only when it actually moved,
        // which is what the early return used to do: rewriting it every second
        // for nothing is a write the owner's disk does not need.
        if !nothing_new || next.state != previous.state || recovered {
            poll.state = Some(next);
        }
        Ok(poll)
    }

    /// The inbox's mail old enough for an `older_than_days` rule, for this
    /// round to triage (#417).
    ///
    /// Empty — and not one request — unless a rule is actually about age, and
    /// at most once an hour ([`SWEEP_EVERY`]): a poll runs every few seconds
    /// and this is a query over the whole inbox.
    ///
    /// **Nothing read here is ever published.** The caller adds these to what
    /// it triages, never to what it publishes: the collector does no backfill
    /// (#251), and a rule about age must not put months of the owner's old
    /// mail on the bus the first time they write one.
    ///
    /// A failure gives up on this round and says so at debug. The mail is
    /// still in the inbox and still old; the next sweep finds it.
    async fn sweep(
        &self,
        session: &Session,
        credential: &crate::side::Credential,
        account: &str,
        inbox_id: &str,
        now: &str,
    ) -> Vec<Mail> {
        let triage = match self.governed.triage.lock() {
            Ok(triage) => triage.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        let Some(days) = crate::triage::oldest_age_wanted(&triage) else {
            return Vec::new();
        };
        {
            let mut swept = match self.swept_at.lock() {
                Ok(swept) => swept,
                Err(poisoned) => poisoned.into_inner(),
            };
            if swept.is_some_and(|last| last.elapsed() < SWEEP_EVERY) {
                return Vec::new();
            }
            *swept = Some(std::time::Instant::now());
        }
        let Some(before) = crate::triage::days_before(now, days) else {
            warn!(%now, "this round's instant cannot be read; the inbox is not swept");
            return Vec::new();
        };
        let ids = match self
            .call(
                &session.api_url,
                credential,
                vec![jmap::email_received_before(account, inbox_id, &before)],
            )
            .await
            .ok()
            .as_ref()
            .and_then(|response| method(response, 0).ok())
            .map(|query| {
                query
                    .get("ids")
                    .and_then(Value::as_array)
                    .map(|ids| {
                        ids.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            }) {
            Some(ids) if !ids.is_empty() => ids,
            _ => return Vec::new(),
        };
        let Ok(response) = self
            .call(
                &session.api_url,
                credential,
                vec![jmap::email_get(account, &ids)],
            )
            .await
        else {
            debug!(
                wanted = ids.len(),
                "the swept mails could not be read this round"
            );
            return Vec::new();
        };
        let Ok(list) = method(&response, 0) else {
            return Vec::new();
        };
        let swept: Vec<Mail> = list
            .get("list")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|email| Mail::parse(email).ok())
            .collect();
        if !swept.is_empty() {
            info!(
                swept = swept.len(),
                older_than_days = days,
                "swept the inbox for mail old enough for a rule about age; nothing read here is \
                 published"
            );
        }
        swept
    }

    /// Files what the owner's rules match, and records what moved (#417).
    ///
    /// Nothing happens on a deployment with no rules, which is every
    /// deployment until somebody writes one — not one request, not one log
    /// line. The mailboxes are read only when there is a rule to resolve.
    ///
    /// **A failure here never fails the poll.** The envelopes are already
    /// built and the bus is what the deployment is for; a mailbox that cannot
    /// be moved is a counted warning, and the next round tries again because
    /// the mail is still in the inbox and still matches.
    async fn file(
        &self,
        session: &Session,
        credential: &crate::side::Credential,
        account: &str,
        mails: &[Mail],
        poll: &mut MailPoll,
        now: &str,
    ) {
        let triage = match self.governed.triage.lock() {
            Ok(triage) => triage.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        let undos = match self.governed.undos.lock() {
            Ok(undos) => undos.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        // An undo is performed even on a deployment whose rules were since
        // cleared: the owner asked for a mail to come back, and withdrawing
        // the rule that filed it does not withdraw that.
        let undos: Vec<_> = undos
            .into_iter()
            .filter(|undo| undo.connection == self.connection)
            .collect();
        if (triage.is_empty() || mails.is_empty()) && undos.is_empty() {
            return;
        }
        // The mailboxes, for this round. Read per round rather than cached:
        // the owner renames and creates mailboxes from their own client, and
        // a cached id is how a rule quietly stops working.
        let mailboxes = match self
            .call(
                &session.api_url,
                credential,
                vec![jmap::mailbox_get(account)],
            )
            .await
            .ok()
            .as_ref()
            .and_then(|response| method(response, 0).ok())
            .and_then(|list| {
                serde_json::from_value::<Vec<crate::triage::Mailbox>>(list.get("list")?.clone())
                    .ok()
            }) {
            Some(mailboxes) => mailboxes,
            None => {
                warn!("the mailboxes could not be read; nothing is filed this round");
                return;
            }
        };
        let at = time::OffsetDateTime::parse(now, &time::format_description::well_known::Rfc3339)
            .map(std::time::SystemTime::from)
            .unwrap_or_else(|_| std::time::SystemTime::now());
        let inbox = mailboxes
            .iter()
            .find(|mailbox| mailbox.role.as_deref() == Some("inbox"));
        let mut moves: Vec<(String, String)> = Vec::new();
        let mut filed: Vec<Filed> = Vec::new();
        // The undos first: a mail the owner asked back must not be filed
        // again by the same rule in the same round, and putting it back first
        // then re-reading it next round is the behaviour they would expect to
        // see argued. It is not — see the note on `undone` below.
        for undo in &undos {
            moves.push((undo.email_id.clone(), undo.from_mailbox_id.clone()));
            filed.push(Filed {
                email_id: undo.email_id.clone(),
                rule_id: crate::triage::UNDO_RULE.to_owned(),
                from_mailbox_id: undo.to_mailbox_id.clone(),
                from_mailbox_name: undo.to_mailbox_name.clone(),
                to_mailbox_id: undo.from_mailbox_id.clone(),
                to_mailbox_name: undo.from_mailbox_name.clone(),
                undoes: Some(undo.sequence),
            });
        }
        let undone: std::collections::BTreeSet<&str> =
            undos.iter().map(|undo| undo.email_id.as_str()).collect();
        for mail in mails {
            // A mail the owner just asked back is left alone this round. Not
            // for ever — if a rule still matches it, the next round files it
            // again, and that is the honest behaviour: the rule is still the
            // owner's. What this prevents is undoing and re-filing in one
            // round, which would make the undo look like it did nothing.
            if undone.contains(mail.id.as_str()) {
                continue;
            }
            match crate::triage::file(&triage, &mailboxes, mail, at) {
                Ok(Some(filing)) => {
                    moves.push((mail.id.clone(), filing.mailbox_id.clone()));
                    // Where this mail actually is, not "the inbox": a
                    // deployment whose inbox has no `inbox` role, or a mail
                    // the owner had already filed somewhere, would otherwise
                    // record an origin that is not the one an undo must
                    // restore — and an empty one makes the move un-undoable
                    // (#418, found in review).
                    let (from_id, from_name) = mail
                        .mailbox_ids
                        .first()
                        .and_then(|id| mailboxes.iter().find(|mb| &mb.id == id))
                        .map(|mb| (mb.id.clone(), mb.name.clone()))
                        .or_else(|| inbox.map(|mb| (mb.id.clone(), mb.name.clone())))
                        .unwrap_or_default();
                    filed.push(Filed {
                        email_id: mail.id.clone(),
                        rule_id: filing.rule_id,
                        from_mailbox_id: from_id,
                        from_mailbox_name: from_name,
                        to_mailbox_id: filing.mailbox_id,
                        to_mailbox_name: filing.mailbox_name,
                        undoes: None,
                    });
                }
                Ok(None) => {}
                Err((rule, why)) => {
                    // Said once per rule per round, not once per mail: a
                    // renamed mailbox would otherwise print a line for every
                    // message that matched it.
                    if !poll.unusable.iter().any(|(held, _)| held == &rule) {
                        warn!(
                            rule = %rule,
                            why = why.as_str(),
                            "a triage rule matched and could not be applied; the mail stays where \
                             it is"
                        );
                        poll.unusable.push((rule, why));
                    }
                }
            }
        }
        if moves.is_empty() {
            return;
        }
        let response = match self
            .call(
                &session.api_url,
                credential,
                vec![jmap::email_moves(account, &moves)],
            )
            .await
        {
            Ok(response) => response,
            Err(error) => {
                warn!(
                    moves = moves.len(),
                    error = %format!("{error:#}"),
                    "the owner's rules matched and the mailbox refused the move; the mail stays \
                     where it is and the next round tries again"
                );
                return;
            }
        };
        // `Email/set` answers which updates it took and which it refused;
        // only what the server says it moved is reported as moved.
        let updated = method(&response, 0)
            .ok()
            .and_then(|set| set.get("updated").cloned())
            .and_then(|updated| {
                updated
                    .as_object()
                    .map(|taken| taken.keys().cloned().collect::<Vec<_>>())
            })
            .unwrap_or_default();
        for one in filed {
            if updated.contains(&one.email_id) {
                info!(
                    rule = %one.rule_id,
                    to = %one.to_mailbox_name,
                    "the owner's rule filed a mail"
                );
                poll.filed.push(one);
            } else {
                warn!(
                    rule = %one.rule_id,
                    "the mailbox did not take this move; the mail stays where it is"
                );
            }
        }
        self.report(&poll.filed, now).await;
    }

    /// Tells the Gateway what moved (#418).
    ///
    /// **Never fails the poll.** The mail has already been filed; what a
    /// failure here costs is the owner's record of it, which the next round
    /// does not recover — stated rather than hidden, and the reason the
    /// warning names how many moves went unrecorded.
    ///
    /// Reported in one request rather than one per move: a round that files
    /// forty newsletters must not make forty calls.
    async fn report(&self, filed: &[Filed], now: &str) {
        let Some((gateway_url, token)) = &self.governed.report_to else {
            return;
        };
        if filed.is_empty() {
            return;
        }
        let moves: Vec<Value> = filed
            .iter()
            .map(|one| {
                serde_json::json!({
                    "connection": self.connection,
                    "email_id": one.email_id,
                    "rule_id": one.rule_id,
                    "from_mailbox_id": one.from_mailbox_id,
                    "from_mailbox_name": one.from_mailbox_name,
                    "to_mailbox_id": one.to_mailbox_id,
                    "to_mailbox_name": one.to_mailbox_name,
                    "occurred_at": now,
                    "undoes": one.undoes,
                })
            })
            .collect();
        let url = format!(
            "{}/api/internal/mail-moves",
            gateway_url.trim_end_matches('/')
        );
        match self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&serde_json::json!({ "moves": moves }))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => warn!(
                status = %response.status(),
                unrecorded = filed.len(),
                "the Companion Gateway refused the move report: the mail moved and the owner's \
                 record of it is missing"
            ),
            Err(error) => warn!(
                error = %error.without_url(),
                unrecorded = filed.len(),
                "the Companion Gateway did not answer the move report: the mail moved and the \
                 owner's record of it is missing"
            ),
        }
    }

    /// Sends an approved reply from the owner's mailbox (#278). The mailbox
    /// is first asked for a reply already carrying this approval's id — a
    /// redelivery after a lost response sends nothing twice — then the mail
    /// answered is found again by its Message-ID, the owner's identity and
    /// the Sent and Drafts mailboxes looked up, and `Email/set` +
    /// `EmailSubmission/set` go in one request. What a retry may fix is
    /// `Transient`; what it cannot — the original gone, an address the
    /// server calls invalid, no identity of the owner's — is `Permanent`. A
    /// draft the submission left behind is destroyed on either.
    pub async fn send_reply(
        &self,
        reply: &ApprovedReply,
        credential: &crate::side::Credential,
    ) -> Result<Sent, SendError> {
        let transient = |error: SideError| SendError::Transient(error.to_string());
        let session = Session::parse(
            &self
                .get(&self.session_url, credential)
                .await
                .map_err(transient)?,
        )
        .map_err(|error| {
            SendError::Transient(format!("the JMAP session cannot be read: {error:#}"))
        })?;
        let account = session.account_id.as_str();
        // `Identity/get` belongs to the submission capability (RFC 8621
        // §6), not the mail one, and this batch used to go out under the
        // mail capability alone: TMail answered `unknownMethod`, as RFC
        // 8620 §3.2 requires, and no approved reply could leave the
        // mailbox at all (#328). The `using` list is now derived from the
        // calls, so this batch asks for what it needs by carrying it.
        let response = self
            .batch(
                &session.api_url,
                credential,
                vec![
                    jmap::mailbox_get(account),
                    jmap::identity_get(account),
                    jmap::email_by_message_id(account, &reply.in_reply_to),
                    jmap::email_by_bare_message_id(account, &reply.in_reply_to),
                ],
            )
            .await?;
        let mailboxes = response.result(0)?;
        let sent_id = jmap::mailbox_with_role(&mailboxes, "sent").ok_or_else(|| {
            SendError::Permanent("the JMAP server lists no Sent mailbox".to_owned())
        })?;
        let drafts_id =
            jmap::mailbox_with_role(&mailboxes, "drafts").unwrap_or_else(|| sent_id.clone());
        // Has this approval already been answered? JMAP has no transaction
        // id, so a redelivered approval — the process stopped between the
        // submission and the acknowledgement — would send the owner's
        // words twice. Every reply carries the approval's id in
        // `X-Twalk-Approval`, and Sent is asked for it by **reading** its
        // newest mails rather than by a filter the server may not answer
        // (#331).
        if self
            .already_answered(&session, credential, account, &sent_id, &reply.event_id)
            .await?
        {
            return Ok(Sent {
                already_sent: true,
                posted_as: None,
            });
        }
        let sending = jmap::identity_for(&response.result(1)?, &self.owner).ok_or_else(|| {
            SendError::Permanent(format!(
                "the JMAP server offers no sending identity for any address of the owner's ({}): \
                 the reply cannot leave as them",
                self.owner.addresses().collect::<Vec<_>>().join(", ")
            ))
        })?;
        if sending.email != self.owner.primary() {
            // Said once per reply and worth it: the reply leaves as an address
            // the owner holds and not the one they are named by, which is what
            // the contact will see and what an operator would otherwise have to
            // deduce from the mailbox.
            tracing::info!(
                identity = %sending.email,
                primary = self.owner.primary(),
                "the mailbox cannot send as the owner's primary address; the reply leaves as this \
                 address of theirs (#322)"
            );
        }
        // The mail the reply answers, by the Message-ID as written and by
        // the same without its brackets (#331). A server that indexes one
        // form answers nothing to the other, and answers it with an empty
        // list rather than a refusal: on the reference deployment this
        // read the owner's own inbox, found nothing, and the collector
        // reported a mail that was sitting there unread as gone. Which
        // form matched is logged, because that is the measurement.
        let written = first_id(&response.result(2)?);
        let bare = first_id(&response.result(3)?);
        if written.is_none() && bare.is_some() {
            info!(
                "this server indexes a Message-ID without its angle brackets: the thread was \
                 found by the bare form and not by the written one"
            );
        }
        let candidates = match written.or(bare) {
            Some(id) => vec![id],
            // Neither form: this server answers no header filter for a
            // Message-ID at all — measured against TMail on the reference
            // deployment, where the mail was in the owner's inbox, unread,
            // while both queries came back empty (#331).
            None => {
                self.thread_by_reading_the_mailbox(&session, credential, account, reply)
                    .await?
            }
        };
        let response = self
            .batch(
                &session.api_url,
                credential,
                vec![jmap::email_get(account, &candidates)],
            )
            .await?;
        let read: Vec<Mail> = response
            .result(0)?
            .get("list")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|mail| Mail::parse(mail).ok())
                    .collect()
            })
            .unwrap_or_default();
        // The Message-ID found the thread; the address is the approval's,
        // and an original that is not from it is a mail somebody else sent
        // under that Message-ID — refused for good, never answered. A
        // Message-ID is the sender's to choose, so two mails may carry
        // one: the contact's is the one answered, and the refusal is for
        // when none of them is theirs (#331).
        let original = read
            .iter()
            .find(|mail| outbound::original_is_from_recipient(reply, mail).is_ok())
            .or(read.first())
            .cloned()
            .ok_or_else(|| {
                SendError::Permanent("the mail answered could not be read back".to_owned())
            })?;
        outbound::original_is_from_recipient(reply, &original).map_err(SendError::Permanent)?;
        let posted_as = sending.email.clone();
        let sender = outbound::Sender {
            account_id: account.to_owned(),
            sending,
            drafts_id,
            sent_id,
        };
        let response = self
            .batch(
                &session.api_url,
                credential,
                outbound::reply_calls(reply, &original, &sender),
            )
            .await?;
        let created = response.result(0)?;
        let Some(draft_id) = created
            .pointer("/created/reply/id")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Err(SendError::Permanent(format!(
                "the JMAP server did not create the reply: {}",
                refusal_type(created.pointer("/notCreated/reply"))
            )));
        };
        let submitted = response.result(1)?;
        if submitted.pointer("/created/submission").is_some() {
            return Ok(Sent {
                already_sent: false,
                posted_as: Some(posted_as),
            });
        }
        // The submission was refused: the draft is not left behind, and the
        // refusal is retried unless its type says a retry cannot change it
        // (RFC 8621 §7.5) — the Sensor's rule for a forbidden post, run on
        // the mail side: a rate limit or a policy clears, an invalid address
        // does not.
        let kind = refusal_type(submitted.pointer("/notCreated/submission"));
        if let Err(error) = self
            .call(
                &session.api_url,
                credential,
                vec![outbound::destroy_draft(account, &draft_id)],
            )
            .await
        {
            warn!(%error, "the draft of a refused reply could not be destroyed");
        }
        let why = format!("the JMAP server refused the submission: {kind}");
        Err(if REFUSALS_A_RETRY_CANNOT_CHANGE.contains(&kind.as_str()) {
            SendError::Permanent(why)
        } else {
            SendError::Transient(why)
        })
    }

    async fn get(
        &self,
        url: &str,
        credential: &crate::side::Credential,
    ) -> Result<Value, SideError> {
        json_of(
            side::send(
                self.http.get(url).header("accept", "application/json"),
                credential,
                "jmap",
            )
            .await?,
        )
        .await
    }

    /// Whether a reply for this approval is already in Sent (#331). A
    /// listing and a get: the ids of Sent's newest mails, then which
    /// approval each was sent for, matched here. No filter, because the
    /// server this runs against answers none; one page, because a
    /// redelivery arrives in minutes and not a mailbox later.
    async fn already_answered(
        &self,
        session: &Session,
        credential: &crate::side::Credential,
        account: &str,
        sent_id: &str,
        approval_id: &str,
    ) -> Result<bool, SendError> {
        let listing = self
            .batch(
                &session.api_url,
                credential,
                vec![jmap::newest_in_mailbox(account, sent_id, 0)],
            )
            .await?;
        let ids = jmap::queried_ids(&listing.result(0)?);
        if ids.is_empty() {
            return Ok(false);
        }
        let mails = self
            .batch(
                &session.api_url,
                credential,
                vec![jmap::approvals_of(account, &ids)],
            )
            .await?;
        let already = jmap::holds_approval(&mails.result(0)?, approval_id);
        if already {
            info!(
                approval = approval_id,
                "this approval's reply is already in Sent: nothing is sent again"
            );
        }
        Ok(already)
    }

    /// The mails a reply might answer, found the way a mail client finds
    /// anything: the account's newest, asked what their Message-ID is,
    /// matched here (#331). Ids and Message-IDs only — no body, no
    /// subject, no address is read — and page by page, since the mail
    /// answered may be older than one page of a busy mailbox.
    async fn thread_by_reading_the_mailbox(
        &self,
        session: &Session,
        credential: &crate::side::Credential,
        account: &str,
        reply: &ApprovedReply,
    ) -> Result<Vec<String>, SendError> {
        let mut scanned = 0usize;
        for page in 0..jmap::THREAD_SCAN_PAGES {
            let listing = self
                .batch(
                    &session.api_url,
                    credential,
                    vec![jmap::newest_in_account(account, scanned)],
                )
                .await?;
            let ids = jmap::queried_ids(&listing.result(0)?);
            if ids.is_empty() {
                break;
            }
            scanned += ids.len();
            let last_page = ids.len() < jmap::QUERY_PAGE;
            let mails = self
                .batch(
                    &session.api_url,
                    credential,
                    vec![jmap::message_ids_of(account, &ids)],
                )
                .await?;
            let found = jmap::ids_with_message_id(&mails.result(0)?, &reply.in_reply_to);
            if !found.is_empty() {
                info!(
                    scanned,
                    pages = page + 1,
                    "this server answers no header filter for a Message-ID: the thread was \
                     found by reading the mailbox instead"
                );
                return Ok(found);
            }
            if last_page {
                break;
            }
        }
        Err(SendError::Permanent(format!(
            "no mail carries the Message-ID this reply answers: no header filter answered it, \
             and it is in none of the {scanned} newest mails of the mailbox"
        )))
    }

    /// One batch of calls, and what the server answered them, paired with
    /// what was asked: the send path reads its results through [`Batch`],
    /// so a refusal names the method it was refused on (#328).
    async fn batch(
        &self,
        api_url: &str,
        credential: &crate::side::Credential,
        calls: Vec<(&'static str, Value)>,
    ) -> Result<Batch, SendError> {
        let methods = calls.iter().map(|(method, _)| *method).collect();
        let response = self
            .call(api_url, credential, calls)
            .await
            .map_err(|error| SendError::Transient(error.to_string()))?;
        Ok(Batch { methods, response })
    }

    async fn call(
        &self,
        api_url: &str,
        credential: &crate::side::Credential,
        calls: Vec<(&str, Value)>,
    ) -> Result<Value, SideError> {
        json_of(
            side::send(
                self.http.post(api_url).json(&jmap::request(calls)),
                credential,
                "jmap",
            )
            .await?,
        )
        .await
    }
}

/// A method's result on a **read** path. The type alone, for the reason
/// [`Batch::result`] gives on the send path: a method error's
/// `description` is the server's own words about what it did not like, and
/// those words may echo an address or a subject.
fn method(response: &Value, index: usize) -> Result<Value, SideError> {
    jmap::method_result(response, index).map_err(|error| SideError::Unreachable {
        detail: format!("the JMAP server answered {}", error.kind),
    })
}

async fn json_of(response: reqwest::Response) -> Result<Value, SideError> {
    response
        .json()
        .await
        .map_err(|error| SideError::Unreachable {
            detail: format!("the JMAP server's answer is not JSON: {error}"),
        })
}

/// What `send_reply` came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    /// The mailbox already held a reply for this approval: nothing was sent
    /// again, and the report says it reached the contact all the same.
    pub already_sent: bool,
    /// The address the reply actually left as, when this run is what sent it
    /// (#322). `None` for a reply already in Sent: this run chose no identity,
    /// and the report then names the address the owner is known by rather than
    /// guessing at what an earlier run used.
    pub posted_as: Option<String>,
}

/// `EmailSubmission/set` refusal types (RFC 8621 §7.5) a retry cannot
/// change. Everything else — `rateLimit`, `forbiddenFrom`, `tooManyRecipients`
/// on a policy that may relax, a server error — is retried and then given
/// up on.
const REFUSALS_A_RETRY_CANNOT_CHANGE: &[&str] = &[
    "invalidEmail",
    "invalidProperties",
    "invalidRecipients",
    "noRecipients",
    "tooLarge",
    "emailNotFound",
];

/// JMAP method error types (RFC 8620 §3.6.2) a retry cannot change: the
/// request itself is wrong. The server's own failures are transient.
const METHOD_ERRORS_A_RETRY_CANNOT_CHANGE: &[&str] = &[
    "invalidArguments",
    "invalidResultReference",
    "forbidden",
    "accountNotFound",
    "accountNotSupportedByMethod",
    "accountReadOnly",
    "unknownMethod",
    "requestTooLarge",
];

/// One batch of method calls and what the server answered, which remembers
/// **what it asked**: a refusal then names the method it was refused on
/// (#328, where `unknownMethod` said nothing about which of four calls the
/// server did not know), without quoting a `description` that may echo the
/// server's own words.
struct Batch {
    methods: Vec<&'static str>,
    response: Value,
}

impl Batch {
    /// The `index`th result, the error classed for the retry policy — and,
    /// since a method error's `description` may quote what the server did
    /// not like, the type and the method are what the reason carries.
    fn result(&self, index: usize) -> Result<Value, SendError> {
        let method = self
            .methods
            .get(index)
            .copied()
            .unwrap_or("a call this batch never made");
        jmap::method_result(&self.response, index).map_err(|error| {
            let why = format!("the JMAP server answered {} to {method}", error.kind);
            if METHOD_ERRORS_A_RETRY_CANNOT_CHANGE.contains(&error.kind.as_str()) {
                SendError::Permanent(why)
            } else {
                SendError::Transient(why)
            }
        })
    }
}

/// The `type` of a `notCreated` refusal, and nothing else of it: its
/// `description` may echo an address.
fn refusal_type(refusal: Option<&Value>) -> String {
    refusal
        .and_then(|refusal| refusal.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned()
}

/// The first id an `Email/query` answered.
fn first_id(query: &Value) -> Option<String> {
    query
        .get("ids")
        .and_then(Value::as_array)
        .and_then(|ids| ids.first())
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{look_back_from, Batch, MailState, SendError, PUBLISHED_RING};
    use serde_json::json;

    /// #328: `unknownMethod` said nothing about which of four calls the
    /// server did not know. A batch remembers what it asked, so the reason
    /// a reply is dead-lettered names the method — and still quotes no
    /// `description`, which may echo the server's own words.
    #[test]
    fn a_refusal_names_the_method_it_was_refused_on_and_nothing_the_server_said() {
        let batch = Batch {
            methods: vec!["Email/query", "Identity/get"],
            response: json!({ "methodResponses": [
                ["Email/query", { "ids": [] }, "c0"],
                ["error", { "type": "unknownMethod", "description": "no such thing as <mm@example.com>" }, "c1"],
            ]}),
        };
        assert!(batch.result(0).is_ok());
        let SendError::Permanent(why) = batch.result(1).unwrap_err() else {
            panic!("unknownMethod is a refusal a retry cannot change");
        };
        assert_eq!(
            why,
            "the JMAP server answered unknownMethod to Identity/get"
        );
    }

    #[test]
    fn the_look_back_starts_five_minutes_before_the_last_read_and_nowhere_without_one() {
        assert_eq!(
            look_back_from("2026-09-20T15:09:12.873Z").as_deref(),
            Some("2026-09-20T15:04:12Z")
        );
        assert_eq!(look_back_from(""), None, "a state an earlier build wrote");
        assert_eq!(look_back_from("yesterday"), None);
    }

    #[test]
    fn the_ring_keeps_the_newest_ids_once_each() {
        let mut state = MailState {
            account_id: "u1".to_owned(),
            inbox_id: "inbox-1".to_owned(),
            state: "7".to_owned(),
            last_read_at: String::new(),
            published: Vec::new(),
        };
        for n in 0..(PUBLISHED_RING + 3) {
            state.remember(&format!("M{n}"));
        }
        state.remember("M4");
        assert_eq!(state.published.len(), PUBLISHED_RING);
        assert_eq!(state.published.first().map(String::as_str), Some("M3"));
        assert_eq!(
            state.published.last().map(String::as_str),
            Some(&*format!("M{}", PUBLISHED_RING + 2))
        );
        assert_eq!(state.published.iter().filter(|id| *id == "M4").count(), 1);
    }
}
