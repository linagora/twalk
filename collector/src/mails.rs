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
use crate::outbound::{ApprovedReply, SendError};
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
        let changes = match method(&response, 0) {
            Ok(result) => Changes::parse(&result),
            Err(SideError::Unreachable { detail })
                if detail.contains("cannotCalculateChanges")
                    || detail.contains("invalidArguments") =>
            {
                // The server forgot the state: #277 recovers it; until then
                // said, and nothing guessed.
                warn!(state = %previous.state, "the JMAP server no longer serves changes from the persisted state; recovery is #277's");
                return Ok(poll);
            }
            Err(error) => return Err(error),
        };
        if changes.created.is_empty() && changes.updated.is_empty() {
            if changes.new_state != previous.state {
                poll.state = Some(MailState {
                    state: changes.new_state,
                    ..previous
                });
            }
            return Ok(poll);
        }
        // Only a mail newly created is a message received; an update (a
        // flag, a move) is not a second delivery.
        let response = self
            .call(
                &session.api_url,
                token,
                vec![jmap::email_mailboxes(account, &changes.created)],
            )
            .await?;
        let in_inbox = jmap::in_mailbox(&method(&response, 0)?, &previous.inbox_id);
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
        poll.state = Some(MailState {
            state: changes.new_state,
            ..previous
        });
        Ok(poll)
    }

    /// Sends an approved reply from the owner's mailbox (#278): the mail
    /// answered is found again by its Message-ID, the owner's identity and
    /// the Sent and Drafts mailboxes looked up, then `Email/set` and
    /// `EmailSubmission/set` in one request. What a retry may fix is
    /// `Transient`; what it cannot — the original gone, the submission
    /// refused, no identity of the owner's — is `Permanent`.
    pub async fn send_reply(&self, reply: &ApprovedReply, token: &str) -> Result<(), SendError> {
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
        let response = self
            .call(
                &session.api_url,
                token,
                vec![
                    jmap::mailbox_get(account),
                    jmap::identity_get(account),
                    jmap::email_by_message_id(account, &reply.in_reply_to),
                ],
            )
            .await
            .map_err(transient)?;
        let mailboxes = method(&response, 0).map_err(transient)?;
        let sent_id = jmap::mailbox_with_role(&mailboxes, "sent").ok_or_else(|| {
            SendError::Permanent("the JMAP server lists no Sent mailbox".to_owned())
        })?;
        let drafts_id =
            jmap::mailbox_with_role(&mailboxes, "drafts").unwrap_or_else(|| sent_id.clone());
        let identity_id = jmap::identity_for(&method(&response, 1).map_err(transient)?, &self.owner_email)
            .ok_or_else(|| {
                SendError::Permanent(format!(
                    "the JMAP server offers no sending identity for {}: the reply cannot leave as the owner",
                    self.owner_email
                ))
            })?;
        let found = method(&response, 2).map_err(transient)?;
        let original_id = found
            .get("ids")
            .and_then(Value::as_array)
            .and_then(|ids| ids.first())
            .and_then(Value::as_str)
            .ok_or_else(|| {
                SendError::Permanent(format!(
                    "the mail {} the reply answers is no longer in the mailbox",
                    reply.in_reply_to
                ))
            })?
            .to_owned();
        let response = self
            .call(
                &session.api_url,
                token,
                vec![jmap::email_get(account, &[original_id])],
            )
            .await
            .map_err(transient)?;
        let original = method(&response, 0)
            .map_err(transient)?
            .get("list")
            .and_then(Value::as_array)
            .and_then(|list| list.first())
            .map(Mail::parse)
            .transpose()
            .map_err(|error| {
                SendError::Permanent(format!("the mail answered cannot be read: {error:#}"))
            })?
            .ok_or_else(|| {
                SendError::Permanent("the mail answered could not be read back".to_owned())
            })?;
        let calls = crate::outbound::reply_calls(
            reply,
            &original,
            account,
            &identity_id,
            &self.owner_email,
            &drafts_id,
            &sent_id,
        );
        let response = self
            .call(&session.api_url, token, calls)
            .await
            .map_err(transient)?;
        let created = method(&response, 0).map_err(transient)?;
        if created.pointer("/created/reply").is_none() {
            return Err(SendError::Permanent(format!(
                "the JMAP server did not create the reply: {}",
                created.get("notCreated").cloned().unwrap_or(Value::Null)
            )));
        }
        let submitted = method(&response, 1).map_err(transient)?;
        if submitted.pointer("/created/submission").is_none() {
            return Err(SendError::Permanent(format!(
                "the JMAP server refused the submission: {}",
                submitted.get("notCreated").cloned().unwrap_or(Value::Null)
            )));
        }
        Ok(())
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

fn method(response: &Value, index: usize) -> Result<Value, SideError> {
    jmap::method_result(response, index).map_err(|error| SideError::Unreachable {
        detail: format!("the JMAP server answered an error: {error}"),
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
