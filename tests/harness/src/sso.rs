//! A fake SSO, and the two services a grant from it opens (issue #274, ADR
//! 0033): what the collector's `oidc` module and its `authorize` command are
//! tested against, so that no test needs `sso.linagora.com`, a real client
//! secret, or a browser.
//!
//! In Rust and in the shape of the Sensor's stub Companion Gateway
//! (`sensor/tests/harness/gateway.rs`): one loopback listener, one request
//! per connection, a hand-rolled request reader — because the thing under
//! test is an HTTP client, and a fake built on the same client library would
//! agree with it for the wrong reasons.
//!
//! What it fakes, and only that:
//!
//! - **discovery** — `/.well-known/openid-configuration` naming the
//!   authorization and token endpoints;
//! - **the authorization code grant with PKCE** — the browser half is the
//!   test's: [`FakeSso::sign_in`] plays the user, checking the S256
//!   challenge the collector put in the link and handing back the callback
//!   URL the operator would paste;
//! - **the token endpoint** — a code exchanged once, a refresh token that
//!   **rotates** on every renewal (the old one dies), a client secret
//!   checked, and a grant the test can revoke, after which renewal answers
//!   `400 invalid_grant`;
//! - **two services behind the grant** — a JMAP session (`/jmap/session`,
//!   `username`) and the calendar side service (`/api/user`,
//!   `preferredEmail` and the owner's `_id`), each accepting a live access
//!   token and each one the test can make refuse a fresh token with `403`,
//!   which is the `pending_operator` case: the grant stands, the service
//!   wants an audience the client does not have;
//! - **the owner's calendars on the side service** (#280) — the HAL list at
//!   `/dav/calendars/<id>.json`, `PROPFIND` on a collection answering its
//!   CTag and each resource's ETag, and `REPORT calendar-multiget`
//!   answering the VEVENTs asked for. A test puts and removes events
//!   ([`FakeSso::put_event`], [`FakeSso::remove_event`]); the CTag moves
//!   on every change and an ETag on every write, as a sabre/dav does.
//!
//! Every token the fake issues is random and never a secret worth
//! protecting; what the tests assert is that the collector never prints one.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The client the fake knows, as the deployment would configure it.
pub const CLIENT_ID: &str = "twalk-collector-test";
pub const CLIENT_SECRET: &str = "test-only-client-secret";
/// How long an access token lives, in seconds: an hour, what a real SSO
/// issues. A test that wants a renewal asks for one (a stale token, a
/// revocation) rather than waiting.
pub const ACCESS_TOKEN_SECONDS: u64 = 3600;

/// Writes `CLIENT_SECRET` into `dir/client-secret` at mode 0600 — the shape
/// the collector accepts a secret file in — and returns the path.
pub fn write_client_secret(dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("client-secret");
    std::fs::write(&path, format!("{CLIENT_SECRET}\n"))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(path)
}

#[derive(Default)]
struct State {
    /// The account the grant is for: what both `whoami`s answer.
    account: String,
    /// Codes issued by `sign_in`, each bound to its PKCE challenge and the
    /// redirect URI the link named; spent when exchanged.
    codes: HashMap<String, PendingCode>,
    /// Live access tokens.
    access_tokens: HashMap<String, ()>,
    /// The one live refresh token, or `None` before any grant or after a
    /// revocation. Rotation replaces it: the previous value is dead.
    refresh_token: Option<String>,
    revoked: bool,
    /// Services told to refuse a fresh token with 403 (`pending_operator`).
    refusing: Vec<&'static str>,
    /// Services told to refuse **without offering any challenge** (#320):
    /// the shape of a service that is not an OAuth resource server at all,
    /// where a deployment expected one. `401`, no `WWW-Authenticate`.
    refusing_silently: Vec<&'static str>,
    /// Services that take **Basic** and nothing else (#342): the shape the
    /// ESN's sabre showed under #320 — `WWW-Authenticate: Basic
    /// realm="ESN"`, and a bearer is worth nothing there.
    basic: Vec<(&'static str, String, String)>,
    /// Services told not to answer at all (`unreachable`): the connection is
    /// accepted and closed.
    silent: Vec<&'static str>,
    /// The listener's port: the issuer discovery names has to be this fake.
    port: u16,
    /// The owner's calendars on the side service, by calendar id.
    calendars: std::collections::BTreeMap<String, FakeCalendar>,
    /// The owner's mailbox on the fake JMAP server (#276).
    mails: crate::jmap_fake::MailStore,
    /// What the session says about push (#277): the socket and the ticket
    /// endpoint by default, no ticket after [`FakeSso::offer_no_ticket`],
    /// nothing after [`FakeSso::offer_no_push`].
    push_offer: Option<crate::jmap_fake::PushOffer>,
    /// The push half's state: where the ticket endpoint mints tickets, and
    /// where the live bearers are mirrored for a handshake without one.
    push_state: Option<Arc<Mutex<crate::jmap_push::PushState>>>,
    /// A Companion Gateway's consent snapshot to answer at
    /// `/api/consent/snapshot`, when a test stands this fake in for the
    /// Gateway too; `None` answers 404 there, an unreadable registry.
    gateway_snapshot: Option<Value>,
    /// The owner's calendar-location decision (#354), as the Gateway's
    /// `/api/settings/collection` answers it. `false` unless a test opens
    /// it, which is how a deployment ships.
    calendar_location: bool,
    /// One counter for every CTag and ETag the fake ever hands out, so no
    /// two versions of anything share one.
    versions: u64,
    /// What happened, for the assertions: every token request's grant type,
    /// every refresh token presented.
    token_requests: Vec<String>,
    /// Every path this fake was asked for, in order (#321): a test can
    /// assert that a service the collector does not read was **never**
    /// asked, which no log line proves.
    paths: Vec<String>,
    refresh_tokens_presented: Vec<String>,
    counter: u64,
}

/// One calendar collection on the fake side service.
#[derive(Default)]
struct FakeCalendar {
    name: String,
    ctag: String,
    /// Resources by name (`<uid>.ics`): the ETag and the iCalendar text.
    resources: std::collections::BTreeMap<String, (String, String)>,
    /// The zone this collection declares (`CALDAV:calendar-timezone`), or
    /// `None` for a calendar nobody ever told one to — which is the ordinary
    /// state of a collection on a server whose clients never set it, and the
    /// case a read has to answer honestly rather than invent around (#369).
    timezone: Option<String>,
}

/// The owner's id on the side service: what `/api/user` answers as `_id`
/// and what the calendar paths are under.
pub const OWNER_ID: &str = "64f1c0a2e9b1d3f4a5b6c7d8";

struct PendingCode {
    challenge: String,
    redirect_uri: String,
}

pub struct FakeSso {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    accept_task: tokio::task::JoinHandle<()>,
    /// The JMAP push endpoint (#277), on a listener of its own.
    push: crate::jmap_push::Push,
}

impl FakeSso {
    /// Starts the fake on an ephemeral loopback port, holding a grant for
    /// `account` — the email both services answer as the signed-in user.
    pub async fn start(account: &str) -> Result<Self> {
        let listener = TcpListener::bind(format!("{}:0", Ipv4Addr::LOCALHOST))
            .await
            .context("failed to bind the fake SSO")?;
        let addr = listener.local_addr()?;
        let push = crate::jmap_push::Push::start().await?;
        let mut mails = crate::jmap_fake::MailStore::default();
        mails.push = Some(push.changes.clone());
        let state = Arc::new(Mutex::new(State {
            account: account.to_owned(),
            port: addr.port(),
            mails,
            push_offer: Some(crate::jmap_fake::PushOffer {
                url: push.url(),
                with_ticket: true,
            }),
            push_state: Some(push.state.clone()),
            ..State::default()
        }));
        let accept_task = tokio::spawn(accept_loop(listener, state.clone()));
        Ok(Self {
            addr,
            state,
            accept_task,
            push,
        })
    }

    /// The issuer: what `COLLECTOR_OIDC_ISSUER` names, and where discovery is.
    pub fn issuer(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The JMAP session URL, as `COLLECTOR_JMAP_SESSION_URL` names it.
    pub fn jmap_session_url(&self) -> String {
        format!("http://{}/jmap/session", self.addr)
    }

    /// The calendar side service's root, as `COLLECTOR_CALDAV_URL` names it.
    pub fn caldav_url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    /// The user's half of the authorization code flow: given the link the
    /// collector printed, signs in and returns the callback URL the operator
    /// pastes. Refuses a link with no S256 challenge or no `state` — the two
    /// things the collector must never leave out.
    pub fn sign_in(&self, authorization_url: &str) -> Result<String> {
        let (_, query) = authorization_url
            .split_once('?')
            .context("the authorization link has no query")?;
        let params = parse_form(query);
        anyhow::ensure!(
            params.get("response_type").map(String::as_str) == Some("code"),
            "response_type must be code: {authorization_url}"
        );
        anyhow::ensure!(
            params.get("client_id").map(String::as_str) == Some(CLIENT_ID),
            "the link names another client: {authorization_url}"
        );
        anyhow::ensure!(
            params.get("code_challenge_method").map(String::as_str) == Some("S256"),
            "PKCE must be S256: {authorization_url}"
        );
        let challenge = params
            .get("code_challenge")
            .filter(|c| !c.is_empty())
            .context("the link carries no code_challenge")?;
        let state = params
            .get("state")
            .filter(|s| !s.is_empty())
            .context("the link carries no state")?;
        let redirect_uri = params
            .get("redirect_uri")
            .context("the link names no redirect_uri")?;
        anyhow::ensure!(
            params
                .get("scope")
                .is_some_and(|scope| scope.split(' ').any(|s| s == "offline_access")),
            "the link does not ask for offline_access, so no refresh token would be issued: {authorization_url}"
        );
        let code = {
            let mut guard = self.lock();
            guard.counter += 1;
            let code = format!("code-{}", guard.counter);
            guard.codes.insert(
                code.clone(),
                PendingCode {
                    challenge: challenge.clone(),
                    redirect_uri: redirect_uri.clone(),
                },
            );
            code
        };
        Ok(format!("{redirect_uri}?code={code}&state={state}"))
    }

    /// Kills the grant, as an SSO's revocation does: the access tokens it
    /// issued die with it, so a service answers 401 to the next request, and
    /// the renewal that must follow answers `invalid_grant` — which is
    /// `reconnect_required`.
    pub fn revoke(&self) {
        let mut guard = self.lock();
        guard.revoked = true;
        guard.refresh_token = None;
        guard.access_tokens.clear();
        if let Some(push) = &guard.push_state {
            push.lock()
                .expect("the push state is not poisoned")
                .bearers
                .clear();
        }
    }

    /// The session names no push at all (#277): what a JMAP server other
    /// than TMail may answer, and what leaves the collector to its poll.
    /// Set before the collector starts.
    pub fn offer_no_push(&self) {
        self.lock().push_offer = None;
    }

    /// The session names the socket but no ticket endpoint (#277): the
    /// socket is opened with the bearer on the handshake, RFC 8887 §3. Set
    /// before the collector starts.
    pub fn offer_no_ticket(&self) {
        if let Some(offer) = &mut self.lock().push_offer {
            offer.with_ticket = false;
        }
    }

    /// Makes one service (`"jmap"` or `"caldav"`) refuse every token with
    /// 403: the grant is fine, the service wants something else.
    pub fn refuse(&self, service: &'static str) {
        self.lock().refusing.push(service);
    }

    /// Makes one service refuse **without saying how to authenticate**
    /// (#320): `401` and no `WWW-Authenticate`, the way a service that is
    /// not an OAuth resource server answers a bearer it never asked for.
    /// The operator's next step is the URL, not their SSO.
    /// Makes one service take a username and a password, as the ESN's
    /// sabre does, and refuse every bearer (#342).
    pub fn require_basic(&self, service: &'static str, user: &str, password: &str) {
        self.lock()
            .basic
            .push((service, user.to_owned(), password.to_owned()));
    }

    pub fn refuse_without_challenge(&self, service: &'static str) {
        self.lock().refusing_silently.push(service);
    }

    /// Makes one service stop answering: `unreachable`, not a refusal.
    /// `"jmap"`, `"caldav"`, or since #279 `"sso"` — discovery and the token
    /// endpoint alike.
    pub fn silence(&self, service: &'static str) {
        self.lock().silent.push(service);
    }

    /// Restores a service's answers.
    pub fn restore(&self, service: &'static str) {
        let mut guard = self.lock();
        guard.refusing.retain(|s| *s != service);
        guard.basic.retain(|(held, _, _)| *held != service);
        guard.refusing_silently.retain(|s| *s != service);
        guard.silent.retain(|s| *s != service);
    }

    /// The live refresh token, for a test that asserts what is on disk is
    /// what the SSO holds.
    pub fn current_refresh_token(&self) -> Option<String> {
        self.lock().refresh_token.clone()
    }

    /// Creates a calendar on the side service, empty, and returns its
    /// collection path (`/dav/calendars/<owner>/<id>/`).
    pub fn create_calendar(&self, id: &str, name: &str) -> String {
        let mut guard = self.lock();
        guard.versions += 1;
        let ctag = format!("http://sabre.io/ns/sync/{}", guard.versions);
        guard.calendars.insert(
            id.to_owned(),
            FakeCalendar {
                name: name.to_owned(),
                ctag,
                resources: Default::default(),
                // The reference deployment's own calendars declare one, so
                // the fake does too and a test has to ask for the absence.
                timezone: Some("Europe/Paris".to_owned()),
            },
        );
        format!("/dav/calendars/{OWNER_ID}/{id}/")
    }

    /// Puts an event into a calendar, creating or replacing the resource
    /// `<name>.ics`: a new ETag for it, a new CTag for the calendar. Returns
    /// the ETag, quoted, as the server would answer it.
    pub fn put_event(&self, calendar: &str, name: &str, ics: &str) -> String {
        let mut guard = self.lock();
        guard.versions += 1;
        let version = guard.versions;
        let etag = format!("\"{version}\"");
        let entry = guard
            .calendars
            .get_mut(calendar)
            .expect("the test created the calendar before writing into it");
        entry.ctag = format!("http://sabre.io/ns/sync/{version}");
        entry
            .resources
            .insert(format!("{name}.ics"), (etag.clone(), ics.to_owned()));
        etag
    }

    /// Removes an event's resource from a calendar: a new CTag, and the
    /// resource is no longer listed. Returns the new CTag.
    pub fn remove_event(&self, calendar: &str, name: &str) -> String {
        let mut guard = self.lock();
        guard.versions += 1;
        let version = guard.versions;
        let entry = guard
            .calendars
            .get_mut(calendar)
            .expect("the test created the calendar before removing from it");
        entry.ctag = format!("http://sabre.io/ns/sync/{version}");
        entry.resources.remove(&format!("{name}.ics"));
        entry.ctag.clone()
    }

    /// A calendar's current CTag.
    /// Takes the declared zone off a collection, for the test that asks what
    /// a read says when the calendar has never been told one (#369).
    pub fn forget_calendar_timezone(&self, calendar: &str) {
        self.lock()
            .calendars
            .get_mut(calendar)
            .expect("a calendar to forget the zone of")
            .timezone = None;
    }

    pub fn ctag(&self, calendar: &str) -> String {
        self.lock().calendars[calendar].ctag.clone()
    }

    /// Stands this fake in for the Companion Gateway as well: the document
    /// `GET /api/consent/snapshot` answers (`connections[]`, `entries[]`,
    /// `next_stream_sequence`), with any bearer.
    pub fn serve_gateway_snapshot(&self, document: Value) {
        self.lock().gateway_snapshot = Some(document);
    }

    /// The owner's calendar-location decision, as `GET
    /// /api/settings/collection` answers it (#354). Off until a test says
    /// otherwise, which is how a deployment ships.
    pub fn set_calendar_location(&self, enabled: bool) {
        self.lock().calendar_location = enabled;
    }

    /// Delivers a mail into the owner's INBOX on the fake JMAP server:
    /// the Email state moves, and `Email/changes` lists it. Returns the
    /// Email id.
    pub fn deliver(&self, mail: crate::jmap_fake::FakeMail) -> String {
        self.deliver_to(crate::jmap_fake::INBOX_ID, mail)
    }

    /// Delivers a mail into one of the three mailboxes (`INBOX_ID`,
    /// `SENT_ID`, `ARCHIVE_ID`).
    pub fn deliver_to(&self, mailbox: &str, mail: crate::jmap_fake::FakeMail) -> String {
        self.lock().mails.deliver(mailbox, mail)
    }

    /// Stops the push endpoint (#277): every open socket is closed, every
    /// new handshake refused, until [`Self::restore_push`]. Deliveries keep
    /// moving the Email state; only the poll finds them.
    pub fn cut_push(&self) {
        self.push
            .state
            .lock()
            .expect("the push state is not poisoned")
            .up = false;
        // A change that wakes the serving tasks so they close their sockets.
        let _ = self.push.changes.send(self.lock().mails.state());
    }

    pub fn restore_push(&self) {
        self.push
            .state
            .lock()
            .expect("the push state is not poisoned")
            .up = true;
    }

    /// How many `StateChange`s the push endpoint sent so far.
    pub fn pushes(&self) -> u64 {
        self.push
            .state
            .lock()
            .expect("the push state is not poisoned")
            .pushed
    }

    /// Forgets every Email state before `state`: `Email/changes` from an
    /// older one answers `cannotCalculateChanges` (#277).
    pub fn forget_mail_states_before(&self, state: u64) {
        self.lock().mails.forget_states_before(state);
    }

    /// The current Email state, as a number.
    pub fn mail_state(&self) -> u64 {
        self.lock().mails.state().parse().unwrap_or(0)
    }

    /// Every reply the collector submitted through the fake (#278).
    pub fn submissions(&self) -> Vec<crate::jmap_fake::Submission> {
        self.lock().mails.submissions()
    }

    /// The Email ids in one mailbox of the fake JMAP server.
    pub fn mails_in(&self, mailbox: &str) -> Vec<String> {
        self.lock().mails.mails_in(mailbox)
    }

    /// Makes `EmailSubmission/set` refuse every submission — or stops.
    pub fn refuse_submissions(&self, refuse: bool) {
        self.lock().mails.refuse_submissions(refuse);
    }

    /// Makes the mail server's search index hold a `Message-ID` without
    /// its angle brackets (#331), the way TMail's does: a filter written
    /// `<id@host>` then matches nothing, with no refusal to say why.
    pub fn index_message_ids_bare(&self, bare: bool) {
        self.lock().mails.index_message_ids_bare(bare);
    }

    /// Makes the mail server answer an **empty list** to any `header`
    /// filter, which is what TMail does on the reference deployment
    /// (#331) — and what an absent mail looks like.
    pub fn answer_no_header_filter(&self, none: bool) {
        self.lock().mails.answer_no_header_filter(none);
    }

    /// Every Email id whose content the collector read, in order.
    pub fn mails_read(&self) -> Vec<String> {
        self.lock().mails.read_ids()
    }

    /// Every refresh token the collector presented, in order.
    pub fn refresh_tokens_presented(&self) -> Vec<String> {
        self.lock().refresh_tokens_presented.clone()
    }

    /// Every token request's grant type, in order.
    /// The paths asked of this fake since it started, in order.
    pub fn paths(&self) -> Vec<String> {
        self.lock().paths.clone()
    }

    /// Whether any path of this service was asked for at all.
    pub fn was_asked(&self, service: &str) -> bool {
        self.paths()
            .iter()
            .any(|path| service_of(path) == Some(service))
    }

    pub fn token_requests(&self) -> Vec<String> {
        self.lock().token_requests.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("the fake SSO is not poisoned")
    }
}

impl Drop for FakeSso {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

async fn accept_loop(listener: TcpListener, state: Arc<Mutex<State>>) {
    while let Ok((stream, _)) = listener.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            let _ = serve_connection(stream, state).await;
        });
    }
}

struct RawRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    body: String,
}

/// What the fake answers: a status line, a media type, bytes.
struct Response {
    status: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    /// What the refusal offers as `WWW-Authenticate` (#320): a service that
    /// reads bearer tokens says so, one that reads something else says
    /// that, and a service that is not an OAuth resource server at all —
    /// a Cozy instance where a deployment expected an OpenPaaS side
    /// service — says nothing, which is the case the collector used to
    /// report as a missing audience.
    challenge: Option<&'static str>,
}

impl Response {
    fn json(status: &'static str, body: Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: serde_json::to_vec(&body).expect("a JSON value serialises"),
            challenge: None,
        }
    }

    fn challenging(mut self, challenge: &'static str) -> Self {
        self.challenge = Some(challenge);
        self
    }

    fn xml(status: &'static str, body: String) -> Self {
        Self {
            status,
            content_type: "application/xml; charset=utf-8",
            body: body.into_bytes(),
            challenge: None,
        }
    }

    fn calendar(status: &'static str, body: String) -> Self {
        Self {
            status,
            content_type: "text/calendar; charset=utf-8",
            body: body.into_bytes(),
            challenge: None,
        }
    }
}

async fn serve_connection(mut stream: TcpStream, state: Arc<Mutex<State>>) -> Result<()> {
    let Some(request) = read_request(&mut stream).await? else {
        return Ok(());
    };
    let Some(Response {
        status,
        content_type,
        body,
        challenge,
    }) = respond(&request, &state)
    else {
        // A silenced service: the connection closes with nothing said.
        return Ok(());
    };
    let challenge = challenge
        .map(|challenge| format!("www-authenticate: {challenge}\r\n"))
        .unwrap_or_default();
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\n{challenge}content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}

/// Reads one HTTP/1.1 request, head and — when `content-length` says so —
/// body: the token endpoint takes a form.
async fn read_request(stream: &mut TcpStream) -> Result<Option<RawRequest>> {
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break position;
        }
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_owned();
    let path = request_line.next().unwrap_or_default().to_owned();
    let mut authorization = None;
    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_owned());
            } else if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Ok(Some(RawRequest {
        method,
        path,
        authorization,
        body: String::from_utf8_lossy(&body).into_owned(),
    }))
}

/// Routes one request. `None` is a silenced service.
fn respond(request: &RawRequest, state: &Arc<Mutex<State>>) -> Option<Response> {
    let path = request.path.split('?').next().unwrap_or_default();
    let mut guard = state.lock().expect("the fake SSO is not poisoned");
    guard.paths.push(path.to_owned());
    let response = if let Some(rest) = path.strip_prefix("/dav/calendars/") {
        dav(request, rest, &mut guard)?
    } else {
        let (status, body) = respond_json(request, path, &mut guard)?;
        Response::json(status, body)
    };
    // A resource server's refusal says how to authenticate (RFC 9110
    // §11.6.1), and says it on every one of its routes — unless the test
    // told this service to refuse silently (#320), which is the answer the
    // collector must not read as a missing audience.
    if !(response.status.starts_with("401") || response.status.starts_with("403")) {
        return Some(response);
    }
    match service_of(path) {
        Some(service) if guard.basic.iter().any(|(held, _, _)| *held == service) => {
            Some(response.challenging("Basic realm=\"ESN\", charset=\"UTF-8\""))
        }
        Some(service) if !guard.refusing_silently.contains(&service) => {
            Some(response.challenging("Bearer realm=\"twalk\", error=\"insufficient_scope\""))
        }
        _ => Some(response),
    }
}

/// Standard base64, for the Basic credential this fake checks.
fn base64_standard(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut buffer = [0u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let triple = u32::from(buffer[0]) << 16 | u32::from(buffer[1]) << 8 | u32::from(buffer[2]);
        for position in 0..4 {
            if position <= chunk.len() {
                out.push(ALPHABET[((triple >> (18 - 6 * position)) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Which resource service a path belongs to: the mailbox's routes are
/// under `/jmap`, the side service's are `/api/user` and the DAV
/// collections. The SSO's own endpoints are neither — a token endpoint
/// refusing a client is not a resource server challenging a bearer.
fn service_of(path: &str) -> Option<&'static str> {
    if path.starts_with("/jmap") {
        Some("jmap")
    } else if path.starts_with("/api/user") || path.starts_with("/dav/") {
        Some("caldav")
    } else {
        None
    }
}

fn respond_json(
    request: &RawRequest,
    path: &str,
    guard: &mut State,
) -> Option<(&'static str, Value)> {
    // The SSO itself silenced (#279): neither discovery nor the token
    // endpoint answers, the way an SSO a deployment came up before does.
    if guard.silent.contains(&"sso")
        && matches!(path, "/.well-known/openid-configuration" | "/token")
    {
        return None;
    }
    match (request.method.as_str(), path) {
        ("GET", "/.well-known/openid-configuration") => {
            let issuer = format!("http://127.0.0.1:{}", guard.port);
            Some((
                "200 OK",
                json!({
                    "issuer": issuer,
                    "authorization_endpoint": format!("{issuer}/authorize"),
                    "token_endpoint": format!("{issuer}/token"),
                    "code_challenge_methods_supported": ["S256"],
                }),
            ))
        }
        ("POST", "/token") => Some(token(&request.body, guard)),
        // The JMAP session and API (#276): the same bearer rule as every
        // route of the mail service.
        ("GET", "/jmap/session") => {
            let issuer = format!("http://127.0.0.1:{}", guard.port);
            let state = guard.mails.state();
            let push = guard.push_offer.clone();
            service("jmap", request, guard, |account| {
                crate::jmap_fake::session(account, &issuer, &state, push.as_ref())
            })
        }
        // TMail's ticket endpoint (#277): one ticket, good for one socket —
        // minted once the bearer is admitted, so a refused endpoint leaves
        // no ticket behind.
        ("POST", "/jmap/ws/ticket") => {
            let push_state = guard.push_state.clone();
            service("jmap", request, guard, |account| {
                let ticket = push_state
                    .as_ref()
                    .map(|push| crate::jmap_push::mint_ticket(push));
                json!({
                    "value": ticket,
                    "generatedOn": "2026-09-21T08:00:00Z",
                    "validUntil": "2026-09-21T08:01:00Z",
                    "username": account
                })
            })
        }
        ("POST", "/jmap/api") => match admit("jmap", request, guard) {
            Admission::Silent => None,
            Admission::Refused(status, body) => Some((status, body)),
            Admission::Account(account) => Some(crate::jmap_fake::api(
                &request.body,
                &mut guard.mails,
                &account,
            )),
        },
        // The OpenPaaS shape: the owner's id (what the calendar paths are
        // under) and `preferredEmail`.
        ("GET", "/api/consent/snapshot") => Some(match guard.gateway_snapshot.clone() {
            Some(document) => ("200 OK", document),
            None => (
                "404 Not Found",
                json!({ "error": "not_found", "detail": "this fake stands in for no Companion Gateway" }),
            ),
        }),
        // The collection settings (#354): whether a calendar event may
        // carry where the meeting is. Answered whenever this fake stands in
        // for the Gateway at all, because the collector reads it at start
        // and a fake that served the snapshot but not this one would be a
        // Gateway that cannot exist.
        ("GET", "/api/settings/collection") => Some(match guard.gateway_snapshot.is_some() {
            true => (
                "200 OK",
                json!({ "calendar_location": { "enabled": guard.calendar_location } }),
            ),
            false => (
                "404 Not Found",
                json!({ "error": "not_found", "detail": "this fake stands in for no Companion Gateway" }),
            ),
        }),
        ("GET", "/api/user") => service(
            "caldav",
            request,
            guard,
            |account| json!({ "_id": OWNER_ID, "preferredEmail": account, "emails": [account] }),
        ),
        _ => Some((
            "404 Not Found",
            json!({ "error": "not_found", "detail": format!("the fake SSO does not serve {path}") }),
        )),
    }
}

/// The token endpoint: two grant types, one client, rotating refresh tokens.
fn token(body: &str, guard: &mut State) -> (&'static str, Value) {
    let form = parse_form(body);
    if form.get("client_id").map(String::as_str) != Some(CLIENT_ID)
        || form.get("client_secret").map(String::as_str) != Some(CLIENT_SECRET)
    {
        return (
            "401 Unauthorized",
            json!({ "error": "invalid_client", "error_description": "unknown client or wrong secret" }),
        );
    }
    let grant_type = form.get("grant_type").cloned().unwrap_or_default();
    guard.token_requests.push(grant_type.clone());
    match grant_type.as_str() {
        "authorization_code" => {
            let code = form.get("code").cloned().unwrap_or_default();
            let Some(pending) = guard.codes.remove(&code) else {
                return (
                    "400 Bad Request",
                    json!({ "error": "invalid_grant", "error_description": "unknown or spent code" }),
                );
            };
            let verifier = form.get("code_verifier").cloned().unwrap_or_default();
            if s256(&verifier) != pending.challenge {
                return (
                    "400 Bad Request",
                    json!({ "error": "invalid_grant", "error_description": "the code_verifier does not match the challenge" }),
                );
            }
            if form.get("redirect_uri") != Some(&pending.redirect_uri) {
                return (
                    "400 Bad Request",
                    json!({ "error": "invalid_grant", "error_description": "redirect_uri differs from the one the code was issued for" }),
                );
            }
            guard.revoked = false;
            issue(guard)
        }
        "refresh_token" => {
            let presented = form.get("refresh_token").cloned().unwrap_or_default();
            guard.refresh_tokens_presented.push(presented.clone());
            if guard.revoked || guard.refresh_token.as_deref() != Some(presented.as_str()) {
                return (
                    "400 Bad Request",
                    json!({ "error": "invalid_grant", "error_description": "the refresh token is revoked, expired, or was rotated away" }),
                );
            }
            issue(guard)
        }
        other => (
            "400 Bad Request",
            json!({ "error": "unsupported_grant_type", "error_description": format!("{other:?}") }),
        ),
    }
}

/// A fresh access token and a fresh refresh token; the previous refresh
/// token is dead from here on.
fn issue(guard: &mut State) -> (&'static str, Value) {
    guard.counter += 1;
    let access = format!("access-{}", guard.counter);
    guard.counter += 1;
    let refresh = format!("refresh-{}", guard.counter);
    guard.access_tokens.insert(access.clone(), ());
    if let Some(push) = &guard.push_state {
        push.lock()
            .expect("the push state is not poisoned")
            .bearers
            .insert(access.clone());
    }
    guard.refresh_token = Some(refresh.clone());
    (
        "200 OK",
        json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": ACCESS_TOKEN_SECONDS,
            "refresh_token": refresh,
            "scope": "openid email offline_access",
        }),
    )
}

/// A service behind the grant: a live bearer answers the account, a stale
/// one is refused with 401, and a service the test told to refuse answers
/// 403 whatever the token — the shape of an audience the client lacks.
fn service(
    name: &'static str,
    request: &RawRequest,
    guard: &mut State,
    document: impl Fn(&str) -> Value,
) -> Option<(&'static str, Value)> {
    match admit(name, request, guard) {
        Admission::Silent => None,
        Admission::Refused(status, body) => Some((status, body)),
        Admission::Account(account) => Some(("200 OK", document(&account))),
    }
}

enum Admission {
    Silent,
    Refused(&'static str, Value),
    Account(String),
}

/// One bearer check for every route of a service: silenced, refusing,
/// unauthenticated, or the account the token belongs to.
fn admit(name: &'static str, request: &RawRequest, guard: &State) -> Admission {
    if guard.silent.contains(&name) {
        return Admission::Silent;
    }
    if guard.refusing_silently.contains(&name) {
        // The Cozy case (#320): a service that never asked for a bearer
        // refuses one without saying what it would accept instead.
        return Admission::Refused(
            "401 Unauthorized",
            json!({ "error": "unauthenticated", "detail": format!("{name} is not an OAuth resource server here") }),
        );
    }
    if guard.refusing.contains(&name) {
        return Admission::Refused(
            "403 Forbidden",
            json!({ "error": "forbidden", "detail": format!("{name} wants an audience this token does not carry") }),
        );
    }
    // A service the test made Basic takes that and nothing else, with the
    // challenge a real one offers (#342).
    if let Some((_, user, password)) = guard.basic.iter().find(|(held, _, _)| *held == name) {
        let expected = format!(
            "Basic {}",
            base64_standard(format!("{user}:{password}").as_bytes())
        );
        return match request.authorization.as_deref() {
            Some(offered) if offered.trim() == expected => {
                Admission::Account(guard.account.clone())
            }
            _ => Admission::Refused(
                "401 Unauthorized",
                json!({ "error": "unauthenticated", "detail": format!("{name} takes a username and a password") }),
            ),
        };
    }
    let bearer = request
        .authorization
        .as_deref()
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim().to_owned());
    match bearer {
        Some(token) if guard.access_tokens.contains_key(&token) => {
            Admission::Account(guard.account.clone())
        }
        _ => Admission::Refused(
            "401 Unauthorized",
            json!({ "error": "unauthenticated", "detail": "no live access token" }),
        ),
    }
}

/// The owner's calendars on the side service (#280), under
/// `/dav/calendars/`: the HAL list, `PROPFIND` on a collection, `REPORT
/// calendar-multiget` on it. The same bearer rule as `/api/user` — it is
/// the same service — so a token the side service refuses is refused on
/// every one of its routes.
fn dav(request: &RawRequest, rest: &str, guard: &mut State) -> Option<Response> {
    // The same admission as `/api/user`; whose account it is does not
    // change what the calendar routes answer.
    match admit("caldav", request, guard) {
        Admission::Silent => return None,
        Admission::Refused(status, body) => return Some(Response::json(status, body)),
        Admission::Account(_) => {}
    }
    let not_found = || {
        Some(Response::json(
            "404 Not Found",
            json!({ "error": "not_found", "detail": format!("no such calendar resource: /dav/calendars/{rest}") }),
        ))
    };
    // `<owner>.json`: the HAL list of the owner's calendars.
    if let Some(owner) = rest.strip_suffix(".json") {
        if owner != OWNER_ID || request.method != "GET" {
            return not_found();
        }
        let calendars: Vec<Value> = guard
            .calendars
            .iter()
            .map(|(id, calendar)| {
                json!({
                    "_links": { "self": { "href": format!("/dav/calendars/{OWNER_ID}/{id}.json") } },
                    "dav:name": calendar.name,
                    "calendarserver:ctag": calendar.ctag,
                    "apple:color": "#3A87AD",
                })
            })
            .collect();
        return Some(Response::json(
            "200 OK",
            json!({
                "_links": { "self": { "href": format!("/dav/calendars/{OWNER_ID}.json") } },
                "_embedded": { "dav:calendar": calendars },
            }),
        ));
    }
    // `<owner>/<calendar>/`: the collection — or, with a third segment,
    // one resource in it, which a `GET` reads as ordinary HTTP (#355).
    let mut parts = rest.trim_end_matches('/').splitn(3, '/');
    let owner = parts.next().unwrap_or_default();
    let calendar_id = parts.next().unwrap_or_default();
    let resource_name = parts.next().unwrap_or_default().to_owned();
    if owner != OWNER_ID || calendar_id.contains('/') {
        return not_found();
    }
    let Some(calendar) = guard.calendars.get(calendar_id) else {
        return not_found();
    };
    let collection = format!("/dav/calendars/{OWNER_ID}/{calendar_id}/");
    // The hrefs this fake answers with, which are **not** the collection it
    // is asked about. The reference deployment relays `/dav/` to a sabre
    // mounted at `/`, so a listing there returns
    // `/calendars/<owner>/<calendar>/<name>.ics` while the request went to
    // `/dav/calendars/…`. A fake whose hrefs matched its own base let a
    // collector paste one onto the base and get a 404 in production and a
    // pass here (#355). Faithful now, so that cannot happen again.
    let href_root = collection
        .strip_prefix("/dav")
        .expect("the collection is under the DAV mount")
        .to_owned();
    if !resource_name.is_empty() {
        // One calendar object resource. Only `GET` is served: nothing in
        // this project writes to a calendar.
        if request.method != "GET" {
            return Some(Response::json(
                "405 Method Not Allowed",
                json!({ "error": "method_not_allowed", "detail": format!("{} on a calendar resource", request.method) }),
            ));
        }
        return match calendar.resources.get(&resource_name) {
            Some((_, ics)) => Some(Response::calendar("200 OK", ics.clone())),
            None => not_found(),
        };
    }
    match request.method.as_str() {
        "PROPFIND" => {
            let mut body = String::from(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:multistatus xmlns:d=\"DAV:\" xmlns:cs=\"http://calendarserver.org/ns/\" xmlns:cal=\"urn:ietf:params:xml:ns:caldav\">\n",
            );
            // A collection that declares a zone answers the whole VTIMEZONE,
            // escaped, as sabre does; one that declares none answers the
            // property `404` inside the `207`, which is not an error.
            let (declared, undeclared) = match &calendar.timezone {
                Some(zone) => (
                    format!(
                        "<cal:calendar-timezone>{}</cal:calendar-timezone>",
                        xml_escape(&format!(
                            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTIMEZONE\r\nTZID:{zone}\r\nEND:VTIMEZONE\r\nEND:VCALENDAR\r\n"
                        ))
                    ),
                    String::new(),
                ),
                None => (String::new(), "<cal:calendar-timezone/>".to_owned()),
            };
            body.push_str(&format!(
                "<d:response><d:href>{href_root}</d:href><d:propstat><d:prop><cs:getctag>{}</cs:getctag>{declared}<d:resourcetype><d:collection/><cal:calendar/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat><d:propstat><d:prop><d:getetag/>{undeclared}</d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>\n",
                xml_escape(&calendar.ctag)
            ));
            for (name, (etag, _)) in &calendar.resources {
                body.push_str(&format!(
                    "<d:response><d:href>{href_root}{name}</d:href><d:propstat><d:prop><d:getetag>{}</d:getetag><d:resourcetype/></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat><d:propstat><d:prop><cs:getctag/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>\n",
                    xml_escape(etag)
                ));
            }
            body.push_str("</d:multistatus>\n");
            Some(Response::xml("207 Multi-Status", body))
        }
        "REPORT" if request.body.contains("free-busy-query") => {
            // free-busy-query (RFC 4791 §7.10, #281): a VFREEBUSY of the
            // periods the collection's events occupy inside the range —
            // an event `STATUS:CANCELLED` or `TRANSP:TRANSPARENT` occupies
            // none, as the specification says — and not one word of the
            // events, since the answer is periods by construction.
            let attribute = |name: &str| -> String {
                request
                    .body
                    .split(&format!("{name}=\""))
                    .nth(1)
                    .and_then(|rest| rest.split('"').next())
                    .unwrap_or_default()
                    .to_owned()
            };
            let (start, end) = (attribute("start"), attribute("end"));
            let mut periods: Vec<String> = Vec::new();
            for (_, ics) in calendar.resources.values() {
                if let Some(period) = free_busy_period(ics, &start, &end) {
                    periods.push(period);
                }
            }
            let mut body = format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//twalk test harness//free-busy//EN\r\nBEGIN:VFREEBUSY\r\nDTSTART:{start}\r\nDTEND:{end}\r\nDTSTAMP:{start}\r\n"
            );
            for period in periods {
                body.push_str(&format!("FREEBUSY;FBTYPE=BUSY:{period}\r\n"));
            }
            body.push_str("END:VFREEBUSY\r\nEND:VCALENDAR\r\n");
            Some(Response::calendar("200 OK", body))
        }
        "REPORT" if request.body.contains("calendar-query") => {
            // calendar-query with a time range (RFC 4791 §7.8, #348): the
            // etags of the events **inside the window**, and nothing of
            // the ones outside it. A fake that answered the whole
            // collection whatever was asked is what let an unbounded
            // PROPFIND ship: this one holds the window, so a test can put
            // an event a year away and watch it stay out.
            let attribute = |name: &str| -> String {
                request
                    .body
                    .split(&format!("{name}=\""))
                    .nth(1)
                    .and_then(|rest| rest.split('"').next())
                    .unwrap_or_default()
                    .to_owned()
            };
            let (start, end) = (attribute("start"), attribute("end"));
            let mut body = String::from(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:multistatus xmlns:d=\"DAV:\" xmlns:cs=\"http://calendarserver.org/ns/\" xmlns:cal=\"urn:ietf:params:xml:ns:caldav\">\n",
            );
            for (name, (etag, ics)) in &calendar.resources {
                if !occurs_within(ics, &start, &end) {
                    continue;
                }
                body.push_str(&format!(
                    "<d:response><d:href>{href_root}{name}</d:href><d:propstat><d:prop><d:getetag>{}</d:getetag></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>\n",
                    xml_escape(etag)
                ));
            }
            body.push_str("</d:multistatus>\n");
            Some(Response::xml("207 Multi-Status", body))
        }
        "REPORT" => {
            // calendar-multiget: every <d:href> asked for, answered with its
            // ETag and its iCalendar text; one not there answers 404 in its
            // own response, as RFC 4791 §7.9 has it.
            let asked: Vec<String> = request
                .body
                .split("<d:href>")
                .skip(1)
                .filter_map(|part| part.split("</d:href>").next())
                .map(|href| xml_unescape(href.trim()))
                .collect();
            let mut body = String::from(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:multistatus xmlns:d=\"DAV:\" xmlns:cal=\"urn:ietf:params:xml:ns:caldav\">\n",
            );
            for href in asked {
                // Either spelling: the href this fake hands out (relative
                // to sabre's root) or the collection it was asked about.
                let name = href
                    .strip_prefix(href_root.as_str())
                    .or_else(|| href.strip_prefix(collection.as_str()))
                    .unwrap_or_default();
                match calendar.resources.get(name) {
                    Some((etag, ics)) => body.push_str(&format!(
                        "<d:response><d:href>{}</d:href><d:propstat><d:prop><d:getetag>{}</d:getetag><cal:calendar-data>{}</cal:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>\n",
                        xml_escape(&href),
                        xml_escape(etag),
                        xml_escape(ics)
                    )),
                    None => body.push_str(&format!(
                        "<d:response><d:href>{}</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>\n",
                        xml_escape(&href)
                    )),
                }
            }
            body.push_str("</d:multistatus>\n");
            Some(Response::xml("207 Multi-Status", body))
        }
        _ => Some(Response::json(
            "405 Method Not Allowed",
            json!({ "error": "method_not_allowed", "detail": format!("{} on a calendar collection", request.method) }),
        )),
    }
}

/// One event's busy period inside `[start, end)`, as `start/end` in UTC,
/// read from the `DTSTART`/`DTEND` lines of its iCalendar text — UTC
/// instants (`…Z`) and whole dates only, which is what the tests write. A
/// cancelled or transparent event, or one outside the range, is none.
/// Whether an event overlaps a window, as a `calendar-query` time range
/// asks (#348). Cancelled and transparent events still *occur* — that
/// distinction belongs to free/busy, not to what a calendar holds.
fn occurs_within(ics: &str, range_start: &str, range_end: &str) -> bool {
    let mut dtstart = None;
    let mut dtend = None;
    for line in ics.lines() {
        let line = line.trim_end();
        if let Some(value) = line.strip_prefix("DTSTART") {
            dtstart = value.split(':').next_back().map(str::to_owned);
        } else if let Some(value) = line.strip_prefix("DTEND") {
            dtend = value.split(':').next_back().map(str::to_owned);
        }
    }
    let instant = |value: String| -> String {
        if value.len() == 8 {
            format!("{value}T000000Z")
        } else if value.ends_with('Z') {
            value
        } else {
            format!("{value}Z")
        }
    };
    let Some(start) = dtstart.map(instant) else {
        // An event with no start is in no window: the collector would
        // have nothing to publish about it either.
        return false;
    };
    let end = dtend.map(instant).unwrap_or_else(|| start.clone());
    // Overlap, the way a time range is defined: it starts before the
    // window ends and ends after the window starts.
    start.as_str() < range_end && end.as_str() > range_start
}

/// One `DTSTART`/`DTEND` line's value, as the UTC a free/busy answer is in.
///
/// A real service answers `VFREEBUSY` periods in UTC (RFC 5545 §3.8.2.6) and
/// the collector refuses anything else, correctly. An event written in a zone
/// — which most events in a person's calendar are — therefore has to be
/// converted here, with a zone table rather than a fixed offset: a fake that
/// got summer time wrong would teach the collector that summer time works.
///
/// A value this cannot read is passed through unchanged, so the fixture that
/// breaks fails in the test rather than silently becoming another instant.
fn local_instant(after_dtstart: &str) -> String {
    let value = after_dtstart
        .split(':')
        .next_back()
        .unwrap_or_default()
        .to_owned();
    let Some(zone) = after_dtstart
        .split(';')
        .find_map(|parameter| parameter.strip_prefix("TZID="))
        .and_then(|zone| zone.split(':').next())
    else {
        return value;
    };
    let Ok(zone) = zone.parse::<chrono_tz::Tz>() else {
        return value;
    };
    let Ok(naive) = chrono::NaiveDateTime::parse_from_str(&value, "%Y%m%dT%H%M%S") else {
        return value;
    };
    use chrono::TimeZone;
    match zone.from_local_datetime(&naive).earliest() {
        Some(at) => at
            .with_timezone(&chrono::Utc)
            .format("%Y%m%dT%H%M%SZ")
            .to_string(),
        None => value,
    }
}

fn free_busy_period(ics: &str, range_start: &str, range_end: &str) -> Option<String> {
    let mut dtstart = None;
    let mut dtend = None;
    let mut busy = true;
    for line in ics.lines() {
        let line = line.trim_end();
        if let Some(value) = line.strip_prefix("DTSTART") {
            dtstart = Some(local_instant(value));
        } else if let Some(value) = line.strip_prefix("DTEND") {
            dtend = Some(local_instant(value));
        } else if line == "STATUS:CANCELLED" || line == "TRANSP:TRANSPARENT" {
            busy = false;
        }
    }
    if !busy {
        return None;
    }
    let instant = |value: String| -> String {
        if value.len() == 8 {
            format!("{value}T000000Z")
        } else {
            value
        }
    };
    let start = instant(dtstart?);
    let end = instant(dtend?);
    // Lexicographic on the fixed-width UTC form.
    if end.as_str() <= range_start || start.as_str() >= range_end {
        return None;
    }
    Some(format!(
        "{}/{}",
        start.as_str().max(range_start),
        end.as_str().min(range_end)
    ))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// `application/x-www-form-urlencoded`, decoded.
fn parse_form(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                match u8::from_str_radix(&value[index + 1..index + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// PKCE S256: base64url, no padding, of the SHA-256 of the verifier.
fn s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64url(&digest)
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut buffer = [0u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let triple = u32::from(buffer[0]) << 16 | u32::from(buffer[1]) << 8 | u32::from(buffer[2]);
        for position in 0..(chunk.len() + 1) {
            let index = (triple >> (18 - 6 * position)) & 0x3f;
            out.push(ALPHABET[index as usize] as char);
        }
    }
    out
}
