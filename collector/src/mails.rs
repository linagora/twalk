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
use tracing::{info, warn};
use twalk_consent_cache::ConsentCache;

use crate::jmap::{self, Changes, Dropped, Envelopes, Mail, Session};
use crate::outbound::{self, ApprovedReply, SendError};
use crate::side::{self, SideError};

/// The mail connection this process holds, and what publishing about it
/// needs.
pub struct Mailbox {
    pub connection: String,
    pub owner_email: String,
    pub session_url: String,
    pub state_dir: PathBuf,
    pub consent: ConsentCache,
    http: reqwest::Client,
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

/// What one poll found. No `Debug`: the envelopes hold the senders' words.
#[derive(Default)]
pub struct MailPoll {
    pub envelopes: Vec<Value>,
    /// The mails the frontier dropped, by reason — counted, never named.
    pub dropped: Vec<Dropped>,
    /// The state to write once the envelopes are on the bus; `None` when
    /// nothing moved.
    pub state: Option<MailState>,
}

impl Mailbox {
    pub fn new(
        connection: &str,
        owner_email: &str,
        session_url: &str,
        state_dir: &std::path::Path,
        consent: ConsentCache,
    ) -> Result<Self> {
        Ok(Self {
            connection: connection.to_owned(),
            owner_email: owner_email.to_owned(),
            session_url: session_url.to_owned(),
            state_dir: state_dir.to_owned(),
            consent,
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
    pub async fn poll(&self, token: &str, now: &str) -> Result<MailPoll, SideError> {
        let session =
            Session::parse(&self.get(&self.session_url, token).await?).map_err(|error| {
                SideError::Unreachable {
                    detail: format!("the JMAP session cannot be read: {error:#}"),
                }
            })?;
        let account = session.account_id.as_str();
        let mut poll = MailPoll::default();
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
                    token,
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
                token,
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
                            .call(&session.api_url, token, vec![jmap::email_state(account)])
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
                    .call(&session.api_url, token, vec![jmap::email_state(account)])
                    .await?;
                let state = email_state_of(&method(&response, 0)?);
                let mut ids: Vec<String> = Vec::new();
                loop {
                    let response = self
                        .call(
                            &session.api_url,
                            token,
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
        if created.is_empty() {
            if next.state != previous.state || recovered {
                poll.state = Some(next);
            }
            return Ok(poll);
        }
        let in_inbox = if recovered {
            // The query was already the INBOX's.
            created
        } else {
            let response = self
                .call(
                    &session.api_url,
                    token,
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
            &self.owner_email,
        );
        if !in_inbox.is_empty() {
            let response = self
                .call(
                    &session.api_url,
                    token,
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
                match jmap::frontier(&mail, &self.owner_email) {
                    Ok(()) => {
                        let consent = self.consent.state(&mail.from.mailto(), &self.connection);
                        poll.envelopes
                            .push(envelopes.message_received(&mail, consent, now));
                    }
                    Err(why) => poll.dropped.push(why),
                }
            }
        }
        poll.state = Some(next);
        Ok(poll)
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
    pub async fn send_reply(&self, reply: &ApprovedReply, token: &str) -> Result<Sent, SendError> {
        let transient = |error: SideError| SendError::Transient(error.to_string());
        let session = Session::parse(
            &self
                .get(&self.session_url, token)
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
                token,
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
            .already_answered(&session, token, account, &sent_id, &reply.event_id)
            .await?
        {
            return Ok(Sent { already_sent: true });
        }
        let identity_id = jmap::identity_for(&response.result(1)?, &self.owner_email)
            .ok_or_else(|| {
                SendError::Permanent(format!(
                    "the JMAP server offers no sending identity for {}: the reply cannot leave as the owner",
                    self.owner_email
                ))
            })?;
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
                self.thread_by_reading_the_mailbox(&session, token, account, reply)
                    .await?
            }
        };
        let response = self
            .batch(
                &session.api_url,
                token,
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
        let sender = outbound::Sender {
            account_id: account.to_owned(),
            identity_id,
            owner_email: self.owner_email.clone(),
            drafts_id,
            sent_id,
        };
        let response = self
            .batch(
                &session.api_url,
                token,
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
                token,
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

    async fn get(&self, url: &str, token: &str) -> Result<Value, SideError> {
        json_of(
            side::send(
                self.http.get(url).header("accept", "application/json"),
                token,
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
        token: &str,
        account: &str,
        sent_id: &str,
        approval_id: &str,
    ) -> Result<bool, SendError> {
        let listing = self
            .batch(
                &session.api_url,
                token,
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
                token,
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
        token: &str,
        account: &str,
        reply: &ApprovedReply,
    ) -> Result<Vec<String>, SendError> {
        let mut scanned = 0usize;
        for page in 0..jmap::THREAD_SCAN_PAGES {
            let listing = self
                .batch(
                    &session.api_url,
                    token,
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
                    token,
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
        token: &str,
        calls: Vec<(&'static str, Value)>,
    ) -> Result<Batch, SendError> {
        let methods = calls.iter().map(|(method, _)| *method).collect();
        let response = self
            .call(api_url, token, calls)
            .await
            .map_err(|error| SendError::Transient(error.to_string()))?;
        Ok(Batch { methods, response })
    }

    async fn call(
        &self,
        api_url: &str,
        token: &str,
        calls: Vec<(&str, Value)>,
    ) -> Result<Value, SideError> {
        json_of(
            side::send(
                self.http.post(api_url).json(&jmap::request(calls)),
                token,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sent {
    /// The mailbox already held a reply for this approval: nothing was sent
    /// again, and the report says it reached the contact all the same.
    pub already_sent: bool,
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
