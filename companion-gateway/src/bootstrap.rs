//! Bootstrap: the registration relay for the one and only account, and the
//! Sensor's invitation into the rooms the user chooses (ticket #53, ADR 0011,
//! wireframe screens 2 and 3d).
//!
//! # Why the Gateway relays registration at all
//!
//! Screen 2 promises a non-technical user an account on their own homeserver.
//! Leaving Synapse's self-service registration open would turn a personal
//! server into a public one, so the reference deployment keeps
//! `enable_registration: false` and the Gateway creates the account with the
//! registration shared secret instead — the one path that works while public
//! registration stays closed.
//!
//! That secret creates accounts, which is the most powerful thing the Gateway
//! holds (`docs/architecture/security-model.md`). ADR 0011 answers it with one
//! narrow mitigation, and this module is where it is enforced: **exactly one
//! account, the owner's**.
//!
//! - The username must be the localpart of `GATEWAY_OWNER`. Any other
//!   username is refused before the secret is used at all — the Gateway is not
//!   a registration service that happens to be limited, it is a relay for one
//!   configured identity.
//! - Once that account exists, every further attempt is refused. The store
//!   remembers the creation (the Gateway's own promise), and the homeserver's
//!   `M_USER_IN_USE` is honoured as the same refusal, so a wiped store cannot
//!   re-open the window.
//!
//! What is left, and is stated rather than hidden: between the moment the
//! operator configures the relay and the moment the owner finishes screen 2,
//! whoever reaches the origin can claim the owner's account with a password of
//! their choosing. Registration is therefore opt-in —
//! `GATEWAY_REGISTRATION_SHARED_SECRET` unset means the endpoint answers 503 —
//! and the window closes for good on the first success. An operator who
//! provisions the account by hand never opens it.
//!
//! # What never reaches the Gateway, and what only passes through
//!
//! **The recovery key.** Screen 2 tells the user "Twalk never sees it", and
//! ADR 0014 makes that a property of where the code runs: the key is generated
//! in the browser and used there to encrypt the cross-signing secrets. This
//! API has no field it could arrive in and none it could leave in — a request
//! that carries one is refused ([`RegistrationRefusal::RecoveryKeyRefused`]),
//! which is the shape of "cannot" that a test can assert.
//!
//! **The Matrix access token.** Synapse's admin registration answers with an
//! access token and a device id, and the browser needs both: it is the session
//! matrix-js-sdk bootstraps the cross-signing identity with (ADR 0014). So the
//! token is *returned* to the Companion and never kept — not in the store, not
//! in a log line, not in this process beyond the response it travels in. The
//! same holds for the token the Companion hands back when it asks for the
//! Sensor's invitation: it is a parameter of one operation, forgotten when
//! that operation ends.
//!
//! # Inviting the Sensor
//!
//! The Sensor observes the rooms it was invited to and nothing else. Screen 3d
//! lets the user pick which of their existing rooms Twalk may watch, and the
//! invitation has to come from an account that is in the room — the user's.
//! The Companion holds that account's access token (it just logged in there,
//! or it just registered); it passes it here with the room list, the Gateway
//! invites `GATEWAY_SENSOR_USER_ID` into each room, and the token is dropped.
//!
//! Using the token rather than an admin credential matters for a homeserver
//! with password login disabled: there is no password to hand the Sensor, and
//! an invitation needs no more than the inviter's own session.
//!
//! The Sensor's own account stays provisioned by the compose stack: nothing
//! here is on its startup path, and it joins whenever the invitation arrives.

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{info, warn};

use crate::matrix_openid::domain_of;

/// How long the Gateway waits for the homeserver on a bootstrap call. The
/// registration nonce expires in 60 seconds and is single-use, so a call that
/// has not answered well inside that is better refused than retried blindly.
const HOMESERVER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// The most rooms one invitation request may name. Screen 3d is a list the
/// user ticks by hand; a cap keeps one request bounded without ever being a
/// limit a real user meets.
pub const MAX_ROOMS_PER_REQUEST: usize = 64;

/// The account the registration relay created, exactly as the homeserver
/// answered it. Everything here travels straight out to the Companion: the
/// access token and device id are the Matrix session the browser bootstraps
/// its cryptographic identity with (ADR 0014), and the Gateway keeps none of
/// it.
#[derive(Debug, Clone)]
pub struct Account {
    pub user_id: String,
    pub device_id: String,
    pub access_token: String,
    pub home_server: String,
}

/// The homeserver could not say whether an account exists: unreachable, or
/// an answer that is neither "here it is" nor `M_NOT_FOUND`. Carries what the
/// homeserver said, never a credential — the read it wraps needs none.
#[derive(Debug)]
pub struct AccountLookupFailure {
    pub detail: String,
}

impl std::fmt::Display for AccountLookupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

/// Why a registration was refused. No variant carries the password, the
/// registration shared secret or an access token.
#[derive(Debug)]
pub enum RegistrationRefusal {
    /// `GATEWAY_REGISTRATION_SHARED_SECRET` is unset: this deployment's
    /// account was provisioned by its operator, and the relay is off.
    NotConfigured,
    /// The username is not the owner's localpart. One deployment, one
    /// identity: the relay creates that account or none.
    NotTheOwner { username: String },
    /// The owner's account already exists — created through this relay before,
    /// or already present on the homeserver. The one account has been created.
    AlreadyExists,
    /// The request carried something that looks like a recovery key. The
    /// Gateway must not see one (ADR 0014), so it refuses rather than ignores:
    /// a client that sent one has a bug worth failing loudly on.
    RecoveryKeyRefused { field: String },
    /// The body was not a registration request.
    InvalidRequest { detail: &'static str },
    /// The homeserver refused, with its own error code (a password policy, a
    /// username it will not accept, a relay it does not have enabled).
    HomeserverRefused { errcode: String },
    /// The homeserver could not be reached, or answered something that is not
    /// a registration answer. An operator problem.
    Unreachable { detail: String },
}

impl RegistrationRefusal {
    /// The stable label for logs and the registration metric's `outcome`
    /// label. A closed set: never anything derived from a request.
    pub fn label(&self) -> &'static str {
        match self {
            RegistrationRefusal::NotConfigured => "not_configured",
            RegistrationRefusal::NotTheOwner { .. } => "not_the_owner",
            RegistrationRefusal::AlreadyExists => "already_exists",
            RegistrationRefusal::RecoveryKeyRefused { .. } => "recovery_key_refused",
            RegistrationRefusal::InvalidRequest { .. } => "invalid_request",
            RegistrationRefusal::HomeserverRefused { .. } => "homeserver_refused",
            RegistrationRefusal::Unreachable { .. } => "unreachable",
        }
    }
}

/// Why a whole invitation request was refused, as opposed to one room of it
/// failing (that is a [`RoomOutcome`]).
#[derive(Debug)]
pub enum InvitationRefusal {
    /// `GATEWAY_SENSOR_USER_ID` is unset: the Gateway does not know who to
    /// invite.
    NotConfigured,
    /// The homeserver rejected the user's access token, so nothing was
    /// attempted in any room.
    TokenRejected,
    /// The body was not an invitation request.
    InvalidRequest { detail: &'static str },
    /// The homeserver could not be reached at all.
    Unreachable { detail: String },
}

impl InvitationRefusal {
    pub fn label(&self) -> &'static str {
        match self {
            InvitationRefusal::NotConfigured => "not_configured",
            InvitationRefusal::TokenRejected => "token_rejected",
            InvitationRefusal::InvalidRequest { .. } => "invalid_request",
            InvitationRefusal::Unreachable { .. } => "unreachable",
        }
    }
}

/// What happened in one room the user selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomOutcome {
    pub room_id: String,
    pub status: RoomStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomStatus {
    /// The Sensor was invited.
    Invited,
    /// The Sensor is already in the room, or already invited to it. Asking
    /// twice is not an error: the user ticked a room Twalk already watches.
    AlreadyPresent,
    /// This one room failed. `reason` is the homeserver's error code
    /// (`M_FORBIDDEN` when the user may not invite there) or the Gateway's own
    /// (`invalid_room_id`, `unreachable`).
    Failed { reason: String },
}

impl RoomStatus {
    pub fn label(&self) -> &'static str {
        match self {
            RoomStatus::Invited => "invited",
            RoomStatus::AlreadyPresent => "already_present",
            RoomStatus::Failed { .. } => "failed",
        }
    }
}

/// The homeserver the Gateway bootstraps against, and the two operator
/// secrets that half of it needs. Each half is independently optional: a
/// deployment may relay registration, invite the Sensor, both, or neither.
#[derive(Debug, Clone)]
pub struct Bootstrap {
    /// Base URL of the homeserver's **client** API, e.g. `http://synapse:8008`
    /// — where both the admin registration endpoint and the invitation
    /// endpoint live. Without a trailing slash.
    homeserver_url: String,
    /// Synapse's registration shared secret, or `None` when the relay is off.
    /// The secret authenticates the admin registration call by itself: it keys
    /// the MAC the homeserver checks, so no admin account and no admin token
    /// is involved.
    registration_shared_secret: Option<String>,
    /// The Matrix ID of the Sensor this deployment runs, or `None` when
    /// inviting it is off.
    sensor_user_id: Option<String>,
    http: reqwest::Client,
}

impl Bootstrap {
    pub fn new(
        homeserver_url: &str,
        registration_shared_secret: Option<String>,
        sensor_user_id: Option<String>,
    ) -> Result<Self> {
        // An unset variable and one compose interpolated to the empty string
        // mean the same thing: that half is off.
        let registration_shared_secret = registration_shared_secret.filter(|s| !s.is_empty());
        let sensor_user_id = sensor_user_id.filter(|s| !s.is_empty());
        if let Some(sensor) = &sensor_user_id {
            anyhow::ensure!(
                domain_of(sensor).is_some(),
                "GATEWAY_SENSOR_USER_ID is not a Matrix user ID \
                 (expected @localpart:server_name, got {sensor:?})"
            );
        }
        let http = reqwest::Client::builder()
            .timeout(HOMESERVER_TIMEOUT)
            .build()
            .map_err(|error| anyhow::anyhow!("{}", error.without_url()))?;
        Ok(Self {
            homeserver_url: homeserver_url.trim_end_matches('/').to_owned(),
            registration_shared_secret,
            sensor_user_id,
            http,
        })
    }

    /// Whether this deployment relays registration at all.
    pub fn registers_accounts(&self) -> bool {
        self.registration_shared_secret.is_some()
    }

    /// The Sensor this deployment invites, or `None` when inviting is off.
    pub fn sensor_user_id(&self) -> Option<&str> {
        self.sensor_user_id.as_deref()
    }

    pub fn homeserver_url(&self) -> &str {
        &self.homeserver_url
    }

    /// Creates the account on the homeserver with the registration shared
    /// secret.
    ///
    /// Synapse's admin registration is authenticated by the MAC itself rather
    /// than by an admin token: ask for a nonce, then send the request with a
    /// hex HMAC-SHA1 over `nonce \0 username \0 password \0 notadmin`, keyed
    /// with the shared secret. It works with `enable_registration: false`,
    /// which is the whole point — public registration stays closed while this
    /// one account can still be created.
    ///
    /// The caller has already checked that `username` is the owner's and that
    /// no account exists: this function does the call and nothing else.
    pub async fn register(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Account, RegistrationRefusal> {
        let Some(secret) = &self.registration_shared_secret else {
            return Err(RegistrationRefusal::NotConfigured);
        };
        let url = self
            .url(&["_synapse", "admin", "v1", "register"])
            .map_err(|detail| RegistrationRefusal::Unreachable { detail })?;

        // The nonce is single-use and expires in 60 seconds, and it lives in
        // one homeserver process's memory: fetched here, spent immediately
        // below, never stored.
        let nonce = self.nonce(&url).await?;
        let mac = registration_mac(secret, &nonce, username, password);
        let response = self
            .http
            .post(url)
            .json(&serde_json::json!({
                "nonce": nonce,
                "username": username,
                "password": password,
                "admin": false,
                "mac": mac,
            }))
            .send()
            .await
            .map_err(|error| RegistrationRefusal::Unreachable {
                detail: error.without_url().to_string(),
            })?;
        let status = response.status();
        if !status.is_success() {
            let (errcode, detail) = matrix_error(response).await;
            // The homeserver's own text goes to the operator's log; the client
            // gets the error code, which is the part it can act on.
            warn!(
                %errcode,
                %detail,
                "the homeserver refused to create the owner's account"
            );
            return Err(RegistrationRefusal::HomeserverRefused { errcode });
        }
        #[derive(Deserialize)]
        struct Registered {
            user_id: String,
            access_token: String,
            device_id: String,
            #[serde(default)]
            home_server: String,
        }
        let registered: Registered =
            response
                .json()
                .await
                .map_err(|error| RegistrationRefusal::Unreachable {
                    detail: format!(
                        "the homeserver's registration answer is not a registration document: {}",
                        error.without_url()
                    ),
                })?;
        Ok(Account {
            user_id: registered.user_id,
            device_id: registered.device_id,
            access_token: registered.access_token,
            home_server: registered.home_server,
        })
    }

    /// Asks the homeserver for a registration nonce.
    async fn nonce(&self, url: &reqwest::Url) -> Result<String, RegistrationRefusal> {
        let response = self.http.get(url.clone()).send().await.map_err(|error| {
            RegistrationRefusal::Unreachable {
                detail: error.without_url().to_string(),
            }
        })?;
        if !response.status().is_success() {
            let (errcode, detail) = matrix_error(response).await;
            warn!(
                %errcode,
                %detail,
                "the homeserver would not mint a registration nonce: is registration_shared_secret set in its configuration?"
            );
            return Err(RegistrationRefusal::HomeserverRefused { errcode });
        }
        #[derive(Deserialize)]
        struct Nonce {
            nonce: String,
        }
        let nonce: Nonce =
            response
                .json()
                .await
                .map_err(|error| RegistrationRefusal::Unreachable {
                    detail: format!(
                        "the homeserver's nonce answer is not a nonce document: {}",
                        error.without_url()
                    ),
                })?;
        Ok(nonce.nonce)
    }

    /// Invites the Sensor into each room, with the user's own Matrix access
    /// token.
    ///
    /// The token is a parameter: it lives in this call's frame, travels in an
    /// `Authorization` header, and is gone when the call returns. Nothing
    /// writes it down, and no refusal quotes it.
    ///
    /// One room failing does not fail the others — the user ticked several and
    /// wants to know which took — but a token the homeserver rejects fails the
    /// whole request, because then nothing was attempted anywhere.
    pub async fn invite_sensor(
        &self,
        user_access_token: &str,
        rooms: &[String],
    ) -> Result<Vec<RoomOutcome>, InvitationRefusal> {
        let Some(sensor) = &self.sensor_user_id else {
            return Err(InvitationRefusal::NotConfigured);
        };
        if rooms.len() > MAX_ROOMS_PER_REQUEST {
            return Err(InvitationRefusal::InvalidRequest {
                detail: "too many rooms in one request",
            });
        }
        let mut outcomes = Vec::with_capacity(rooms.len());
        for room_id in rooms {
            if !is_room_id(room_id) {
                outcomes.push(RoomOutcome {
                    room_id: room_id.clone(),
                    status: RoomStatus::Failed {
                        reason: "invalid_room_id".to_owned(),
                    },
                });
                continue;
            }
            let status = self.invite_into(user_access_token, room_id, sensor).await?;
            outcomes.push(RoomOutcome {
                room_id: room_id.clone(),
                status,
            });
        }
        Ok(outcomes)
    }

    /// One room: is the Sensor already there, and if not, invite it.
    async fn invite_into(
        &self,
        user_access_token: &str,
        room_id: &str,
        sensor: &str,
    ) -> Result<RoomStatus, InvitationRefusal> {
        match self
            .sensor_membership(user_access_token, room_id, sensor)
            .await?
        {
            // Already watching, or already asked: the user ticked a room Twalk
            // is in. Not an error, and not a second invitation.
            Some(membership) if membership == "join" || membership == "invite" => {
                return Ok(RoomStatus::AlreadyPresent)
            }
            _ => {}
        }
        let url = match self.url(&["_matrix", "client", "v3", "rooms", room_id, "invite"]) {
            Ok(url) => url,
            Err(detail) => return Err(InvitationRefusal::Unreachable { detail }),
        };
        let response = self
            .http
            .post(url)
            .bearer_auth(user_access_token)
            .json(&serde_json::json!({ "user_id": sensor }))
            .send()
            .await
            .map_err(|error| InvitationRefusal::Unreachable {
                detail: error.without_url().to_string(),
            })?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(InvitationRefusal::TokenRejected);
        }
        if response.status().is_success() {
            info!(room = %room_id, sensor = %sensor, "invited the Sensor into a room");
            return Ok(RoomStatus::Invited);
        }
        let (errcode, detail) = matrix_error(response).await;
        // `already in the room` is the one refusal that is really a success,
        // and the membership check above normally catches it — this is the
        // race where somebody joined in between.
        if detail.contains("already in the room") {
            return Ok(RoomStatus::AlreadyPresent);
        }
        warn!(
            room = %room_id,
            %errcode,
            %detail,
            "the homeserver refused to invite the Sensor into a room"
        );
        Ok(RoomStatus::Failed { reason: errcode })
    }

    /// The Sensor's membership in a room as the user's own session can read
    /// it, or `None` when there is no member event (or the state is not
    /// readable, which the invitation below will then refuse for a reason
    /// worth reporting).
    async fn sensor_membership(
        &self,
        user_access_token: &str,
        room_id: &str,
        sensor: &str,
    ) -> Result<Option<String>, InvitationRefusal> {
        let url = match self.url(&[
            "_matrix",
            "client",
            "v3",
            "rooms",
            room_id,
            "state",
            "m.room.member",
            sensor,
        ]) {
            Ok(url) => url,
            Err(detail) => return Err(InvitationRefusal::Unreachable { detail }),
        };
        let response = self
            .http
            .get(url)
            .bearer_auth(user_access_token)
            .send()
            .await
            .map_err(|error| InvitationRefusal::Unreachable {
                detail: error.without_url().to_string(),
            })?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(InvitationRefusal::TokenRejected);
        }
        if !response.status().is_success() {
            return Ok(None);
        }
        #[derive(Deserialize)]
        struct Member {
            #[serde(default)]
            membership: Option<String>,
        }
        Ok(response
            .json::<Member>()
            .await
            .ok()
            .and_then(|member| member.membership))
    }

    /// Whether an account exists on the homeserver — asked of the homeserver,
    /// whoever created the account (ticket #133).
    ///
    /// `GET /api/deployment`'s `bootstrapped` used to be the store's memory of
    /// the registration relay succeeding. That is the right source for
    /// refusing a second registration and the wrong one for "can this person
    /// sign in": an account provisioned outside the relay, or a state
    /// directory recreated or restored from before onboarding, is a working
    /// deployment whose owner was told it had no account and shown no way in —
    /// the failure #112 exists to prevent, reached by the screen written to
    /// prevent it.
    ///
    /// The question is asked through the profile endpoint, because it is the
    /// one read that answers for a local account without a token and while
    /// `enable_registration: false` — `register/available`, the obvious one,
    /// is refused outright by Synapse on a homeserver with registration
    /// closed, which is every deployment of this product. A profile answers
    /// `200` for any account that exists, empty or not, and `404` for one that
    /// does not. Nothing of the profile is kept; only which of the two the
    /// homeserver said.
    ///
    /// Every other answer is an error, never an absence: a homeserver that
    /// cannot be reached, or one configured to require a token for profile
    /// reads (`require_auth_for_profile_requests`), must not be reported as
    /// "no account yet" — that would send a returning user to the account
    /// form.
    pub async fn account_exists(&self, user_id: &str) -> Result<bool, AccountLookupFailure> {
        let url = self
            .url(&["_matrix", "client", "v3", "profile", user_id])
            .map_err(|detail| AccountLookupFailure { detail })?;
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|error| AccountLookupFailure {
                detail: format!(
                    "the homeserver could not be reached: {}",
                    error.without_url()
                ),
            })?;
        if response.status().is_success() {
            return Ok(true);
        }
        let status = response.status();
        let (errcode, detail) = matrix_error(response).await;
        // Synapse says a missing local profile two ways depending on the
        // path it took — `M_NOT_FOUND` "Profile was not found", or `M_UNKNOWN`
        // "No row found (profiles)" — and both are the answer "no such
        // account". `M_UNRECOGNIZED` is not: that is a server with no such
        // endpoint, which is not a Matrix homeserver answering the question.
        if status == reqwest::StatusCode::NOT_FOUND && errcode != "M_UNRECOGNIZED" {
            return Ok(false);
        }
        Err(AccountLookupFailure {
            detail: if status == reqwest::StatusCode::FORBIDDEN {
                format!(
                    "the homeserver refused the profile read ({errcode}: {detail}); it is \
                     configured to require a token for profile requests \
                     (require_auth_for_profile_requests), which this check cannot carry"
                )
            } else {
                format!("the homeserver answered {status} ({errcode}: {detail})")
            },
        })
    }

    /// A homeserver URL with each path segment properly escaped. Room ids and
    /// Matrix IDs travel in the path and carry `!`, `@` and `:` — legal in a
    /// path segment, so they survive as themselves — but also `/` in a
    /// malformed one, which the encoder turns into `%2F` instead of a segment
    /// boundary that would address another endpoint entirely.
    fn url(&self, segments: &[&str]) -> Result<reqwest::Url, String> {
        let mut url = reqwest::Url::parse(&self.homeserver_url)
            .map_err(|error| format!("the homeserver URL is not a URL: {error}"))?;
        url.path_segments_mut()
            .map_err(|_| "the homeserver URL cannot carry a path".to_owned())?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }
}

/// Synapse's `errcode` and human message out of an error response, with
/// stand-ins when the body is not a Matrix error at all.
async fn matrix_error(response: reqwest::Response) -> (String, String) {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(document) => (
            document["errcode"]
                .as_str()
                .unwrap_or("M_UNKNOWN")
                .to_owned(),
            document["error"]
                .as_str()
                .unwrap_or(&format!("the homeserver answered {status}"))
                .to_owned(),
        ),
        Err(_) => (
            "M_UNKNOWN".to_owned(),
            format!("the homeserver answered {status} with a body that is not a Matrix error"),
        ),
    }
}

/// The MAC Synapse's admin registration endpoint checks: hex
/// HMAC-**SHA1** over `nonce \0 username \0 password \0 ("admin"|"notadmin")`,
/// keyed with the registration shared secret.
///
/// SHA-1 is not a choice here: it is the endpoint's wire format, and the MAC
/// authenticates a call the caller already holds the secret for — a hash
/// collision would buy an attacker nothing it does not already have. The
/// Gateway only ever registers a non-admin account, so the last field is
/// always `notadmin`; `displayname`, which the endpoint also accepts, is
/// deliberately not part of the MAC and is not sent.
fn registration_mac(secret: &str, nonce: &str, username: &str, password: &str) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts a key of any length");
    for field in [nonce.as_bytes(), username.as_bytes(), password.as_bytes()] {
        mac.update(field);
        mac.update(b"\x00");
    }
    mac.update(b"notadmin");
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether a string is a Matrix room id: `!opaque:server_name`, with both
/// halves non-empty and no path separator to smuggle.
pub fn is_room_id(room_id: &str) -> bool {
    let Some(rest) = room_id.strip_prefix('!') else {
        return false;
    };
    let Some((opaque, server)) = rest.split_once(':') else {
        return false;
    };
    !opaque.is_empty()
        && !server.is_empty()
        && room_id.len() <= 255
        && !room_id.contains('/')
        && !room_id.chars().any(char::is_whitespace)
        && !room_id.chars().any(char::is_control)
}

/// The field names a recovery key would arrive under if a client ever tried to
/// send one. The Gateway refuses the request rather than ignoring the field:
/// screen 2's promise is that the key never reaches here, and a client with
/// that bug should find out at once (ADR 0014).
const RECOVERY_KEY_FIELDS: [&str; 6] = [
    "recovery_key",
    "recoverykey",
    "recovery-key",
    "security_key",
    "securitykey",
    "security-key",
];

/// The recovery-key-shaped field a JSON object carries, if any. Case is
/// ignored, because the promise is about the value and not about a spelling.
pub fn recovery_key_field(body: &serde_json::Value) -> Option<String> {
    let object = body.as_object()?;
    object.keys().find_map(|key| {
        let lowercase = key.to_ascii_lowercase();
        RECOVERY_KEY_FIELDS
            .contains(&lowercase.as_str())
            .then(|| key.clone())
    })
}

/// Reads a bootstrap request body: JSON, an object, and free of any
/// recovery-key-shaped field before anything else looks at it.
pub fn parse_body(bytes: &[u8]) -> Result<serde_json::Value, RecoveryOrInvalid> {
    let body: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| RecoveryOrInvalid::Invalid)?;
    if !body.is_object() {
        return Err(RecoveryOrInvalid::Invalid);
    }
    match recovery_key_field(&body) {
        Some(field) => Err(RecoveryOrInvalid::RecoveryKey { field }),
        None => Ok(body),
    }
}

/// What can be wrong with a bootstrap body before it is even typed.
#[derive(Debug)]
pub enum RecoveryOrInvalid {
    Invalid,
    RecoveryKey { field: String },
}

/// The localpart the relay may create, from the owner's Matrix ID. Errors
/// rather than defaults: a Gateway whose owner is not a Matrix ID never
/// starts (see `config::SignIn`), so this cannot fail in practice.
pub fn owner_localpart(owner: &str) -> Result<&str> {
    owner
        .strip_prefix('@')
        .and_then(|rest| rest.split_once(':'))
        .map(|(localpart, _)| localpart)
        .filter(|localpart| !localpart.is_empty())
        .with_context(|| format!("the owner {owner:?} is not a Matrix user ID"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The MAC is the endpoint's wire format, so it is pinned against a value
    /// computed independently of this code (python's hmac over the same
    /// fields), not against whatever the implementation happens to produce.
    #[test]
    fn the_registration_mac_is_hmac_sha1_over_the_nul_separated_fields() {
        // $ python3 -c 'import hmac,hashlib;print(hmac.new(b"test-only-registration-shared-secret",
        //   b"abc123\x00owner\x00hunter2\x00notadmin", hashlib.sha1).hexdigest())'
        assert_eq!(
            registration_mac(
                "test-only-registration-shared-secret",
                "abc123",
                "owner",
                "hunter2"
            ),
            "9c9558bdd032b59de049f9306324ffc772366347",
            "the MAC is the admin endpoint's wire format, not ours to change"
        );
        // Every field is in the MAC: change any one of them and it moves.
        let baseline = registration_mac("secret", "nonce", "owner", "password");
        for (nonce, username, password) in [
            ("other", "owner", "password"),
            ("nonce", "other", "password"),
            ("nonce", "owner", "other"),
        ] {
            assert_ne!(
                registration_mac("secret", nonce, username, password),
                baseline
            );
        }
        assert_ne!(
            registration_mac("other-secret", "nonce", "owner", "password"),
            baseline,
            "the MAC is keyed with the registration shared secret"
        );
        // The NUL separators are what stop two different field splits from
        // hashing the same: `a\0b` must not equal `ab\0`.
        assert_ne!(
            registration_mac("secret", "nonce", "ow", "nerpassword"),
            registration_mac("secret", "nonce", "owner", "password")
        );
    }

    #[test]
    fn a_room_id_is_a_room_id_and_nothing_else() {
        assert!(is_room_id("!abc:example.com"));
        assert!(is_room_id("!abc:example.com:8448"));
        for not_a_room in [
            "",
            "abc:example.com",
            "!abc",
            "!:example.com",
            "!abc:",
            "@alice:example.com",
            "!abc:example.com/../../secret",
            "!abc def:example.com",
            "!abc\n:example.com",
        ] {
            assert!(!is_room_id(not_a_room), "{not_a_room:?}");
        }
        assert!(
            !is_room_id(&format!("!{}:example.com", "x".repeat(300))),
            "a room id has a length limit"
        );
    }

    #[test]
    fn a_recovery_key_is_refused_however_it_is_spelled() {
        for spelling in [
            "recovery_key",
            "recoveryKey",
            "RECOVERY_KEY",
            "recovery-key",
            "security_key",
        ] {
            let body = serde_json::json!({ "username": "owner", spelling: "EsTx abcd" });
            assert_eq!(
                recovery_key_field(&body).as_deref(),
                Some(spelling),
                "{spelling} must be refused"
            );
        }
        // An ordinary registration carries none.
        assert_eq!(
            recovery_key_field(&serde_json::json!({ "username": "owner", "password": "p" })),
            None
        );
    }

    #[test]
    fn a_body_carrying_a_recovery_key_never_gets_past_parsing() {
        let refused = parse_body(br#"{"username":"owner","recovery_key":"EsTx abcd"}"#)
            .expect_err("a recovery key is refused");
        assert!(
            matches!(&refused, RecoveryOrInvalid::RecoveryKey { field } if field == "recovery_key"),
            "{refused:?}"
        );
        assert!(matches!(
            parse_body(b"not json").expect_err("not JSON"),
            RecoveryOrInvalid::Invalid
        ));
        assert!(matches!(
            parse_body(b"[1,2,3]").expect_err("not an object"),
            RecoveryOrInvalid::Invalid
        ));
        parse_body(br#"{"username":"owner","password":"p"}"#).expect("an ordinary body parses");
    }

    #[test]
    fn the_owners_localpart_is_what_the_relay_may_create() {
        assert_eq!(owner_localpart("@you:example.com").unwrap(), "you");
        assert_eq!(owner_localpart("@you:example.com:8448").unwrap(), "you");
        assert!(owner_localpart("you:example.com").is_err());
        assert!(owner_localpart("@:example.com").is_err());
    }

    #[test]
    fn a_sensor_user_id_that_is_not_a_matrix_id_is_refused_at_construction() {
        assert!(Bootstrap::new("http://127.0.0.1:1", None, Some("sensor".to_owned())).is_err());
        let bootstrap = Bootstrap::new(
            "http://127.0.0.1:1/",
            Some("secret".to_owned()),
            Some("@sensor:example.com".to_owned()),
        )
        .expect("a well-formed configuration builds");
        assert!(bootstrap.registers_accounts());
        assert_eq!(bootstrap.sensor_user_id(), Some("@sensor:example.com"));
        // The trailing slash is normalised away, so the path builder never
        // produces a double slash.
        assert_eq!(bootstrap.homeserver_url(), "http://127.0.0.1:1");
    }

    #[test]
    fn an_empty_secret_is_no_secret_at_all() {
        let bootstrap = Bootstrap::new(
            "http://127.0.0.1:1",
            Some(String::new()),
            Some(String::new()),
        )
        .expect("empty values build");
        assert!(
            !bootstrap.registers_accounts(),
            "an empty GATEWAY_REGISTRATION_SHARED_SECRET leaves the relay off"
        );
        assert_eq!(bootstrap.sensor_user_id(), None);
    }

    #[test]
    fn matrix_ids_and_room_ids_stay_one_path_segment_each() {
        let bootstrap = Bootstrap::new(
            "http://synapse:8008",
            Some("secret".to_owned()),
            Some("@sensor:example.com".to_owned()),
        )
        .expect("a configuration builds");
        let url = bootstrap
            .url(&[
                "_matrix",
                "client",
                "v3",
                "rooms",
                "!abc:example.com",
                "state",
                "m.room.member",
                "@sensor:example.com",
            ])
            .expect("a url is built");
        assert_eq!(
            url.as_str(),
            "http://synapse:8008/_matrix/client/v3/rooms/!abc:example.com/state/m.room.member/@sensor:example.com"
        );
        // A slash inside a segment is escaped rather than becoming a boundary:
        // no room id addresses another endpoint. (`is_room_id` refuses such an
        // id before this is reached; the encoding is the second line.)
        let escaped = bootstrap
            .url(&["_matrix", "client", "v3", "rooms", "!a/../admin:x"])
            .expect("a url is built");
        assert!(
            escaped.as_str().ends_with("/rooms/!a%2F..%2Fadmin:x"),
            "{escaped}"
        );
        // And a base URL with a trailing slash does not grow an empty segment.
        let trailing = Bootstrap::new("http://synapse:8008/", None, None)
            .expect("it builds")
            .url(&["_synapse", "admin", "v1", "register"])
            .expect("a url is built");
        assert_eq!(
            trailing.as_str(),
            "http://synapse:8008/_synapse/admin/v1/register"
        );
    }

    #[tokio::test]
    async fn an_unreachable_homeserver_leaks_neither_the_password_nor_the_secret() {
        let bootstrap = Bootstrap::new(
            "http://127.0.0.1:1",
            Some("a-very-secret-registration-secret".to_owned()),
            Some("@sensor:example.com".to_owned()),
        )
        .expect("a configuration builds");
        let refused = bootstrap
            .register("owner", "a-very-secret-password")
            .await
            .expect_err("an unreachable homeserver refuses");
        let RegistrationRefusal::Unreachable { detail } = &refused else {
            panic!("expected Unreachable, got {refused:?}");
        };
        assert!(!detail.contains("a-very-secret-password"), "{detail}");
        assert!(
            !detail.contains("a-very-secret-registration-secret"),
            "{detail}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_homeserver_leaks_no_user_token_when_inviting() {
        let bootstrap = Bootstrap::new(
            "http://127.0.0.1:1",
            None,
            Some("@sensor:example.com".to_owned()),
        )
        .expect("a configuration builds");
        let refused = bootstrap
            .invite_sensor(
                "syt_a_secret_looking_access_token",
                &["!room:example.com".to_owned()],
            )
            .await
            .expect_err("an unreachable homeserver refuses");
        let InvitationRefusal::Unreachable { detail } = &refused else {
            panic!("expected Unreachable, got {refused:?}");
        };
        assert!(
            !detail.contains("syt_a_secret_looking_access_token"),
            "{detail}"
        );
    }

    #[tokio::test]
    async fn a_malformed_room_id_fails_its_own_room_and_reaches_no_homeserver() {
        let bootstrap = Bootstrap::new(
            "http://127.0.0.1:1",
            None,
            Some("@sensor:example.com".to_owned()),
        )
        .expect("a configuration builds");
        let outcomes = bootstrap
            .invite_sensor("token", &["not-a-room-id".to_owned()])
            .await
            .expect("a malformed room id is one room's problem, not the request's");
        assert_eq!(
            outcomes,
            vec![RoomOutcome {
                room_id: "not-a-room-id".to_owned(),
                status: RoomStatus::Failed {
                    reason: "invalid_room_id".to_owned()
                },
            }]
        );
    }

    #[tokio::test]
    async fn each_half_is_off_until_its_variable_is_set() {
        let bootstrap = Bootstrap::new("http://127.0.0.1:1", None, None).expect("it builds");
        assert!(matches!(
            bootstrap
                .register("owner", "password")
                .await
                .expect_err("registration is off"),
            RegistrationRefusal::NotConfigured
        ));
        assert!(matches!(
            bootstrap
                .invite_sensor("token", &[])
                .await
                .expect_err("inviting is off"),
            InvitationRefusal::NotConfigured
        ));
    }

    #[tokio::test]
    async fn more_rooms_than_the_cap_is_refused_as_one_request() {
        let bootstrap = Bootstrap::new(
            "http://127.0.0.1:1",
            None,
            Some("@sensor:example.com".to_owned()),
        )
        .expect("it builds");
        let rooms: Vec<String> = (0..MAX_ROOMS_PER_REQUEST + 1)
            .map(|index| format!("!room{index}:example.com"))
            .collect();
        assert!(matches!(
            bootstrap
                .invite_sensor("token", &rooms)
                .await
                .expect_err("too many rooms"),
            InvitationRefusal::InvalidRequest { .. }
        ));
    }
}
