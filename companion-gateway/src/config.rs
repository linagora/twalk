//! Environment-driven configuration. Like the Sensor, the Companion Gateway
//! runs from environment variables alone, so it deploys with the rest of the
//! compose stack — same flat struct, same `from_env`, same error messages
//! naming the variable at fault.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    /// Address the Gateway's HTTP origin listens on (GATEWAY_LISTEN, default
    /// `0.0.0.0:8080`). One origin serves everything: the Companion's static
    /// files, the health endpoint and the metrics endpoint. Same origin means
    /// no CORS and, later, an `HttpOnly` device cookie (ADR 0011). Port 0
    /// asks the kernel for a free port — what the test suite uses.
    pub listen: SocketAddr,
    /// Directory the Companion's static files are served from
    /// (GATEWAY_STATIC_DIR, required). The image ships the Companion's build
    /// at `/srv/companion`; an operator serving their own build mounts it
    /// over that path or points this elsewhere. A directory that is absent or
    /// empty is not fatal: the Gateway serves a clear 404 until the build
    /// appears, so health and metrics stay up either way.
    pub static_dir: PathBuf,
    /// Name of the Companion's SPA fallback file inside the static directory
    /// (GATEWAY_FALLBACK_FILE, default `200.html`): what a path matching no
    /// file of the build is answered with, so a deep link reloaded cold loads
    /// the app. `200.html` is SvelteKit's recommended name — an `index.html`
    /// fallback collides with a prerendered homepage — and it is
    /// configurable because the name is the Companion build's to choose.
    pub fallback_file: String,
    /// Log level filter (GATEWAY_LOG_LEVEL), e.g. `info` or
    /// `info,twalk_companion_gateway=debug`. Per-request logs are at debug:
    /// a static origin at info level would be nothing but access logs.
    pub log_level: String,
    /// How the user signs in (ticket #52), or `None` when GATEWAY_OWNER is
    /// unset: the origin then serves the Companion, `/health` and
    /// `/metrics`, and its whole API answers `503 sign_in_not_configured`.
    ///
    /// Why not a hard startup failure, as a missing GATEWAY_STATIC_DIR is:
    /// the reference deployment brings the whole stack up in one
    /// `docker compose up`, and an operator upgrading a Gateway that has no
    /// `GATEWAY_OWNER` in their `.env` yet would lose the origin — and with
    /// it the page that could tell them why. Keeping the origin up while the
    /// API is closed fails in the safe direction: nothing can be
    /// authenticated, so nothing can be decided.
    pub sign_in: Option<SignIn>,
    /// Bootstrap: the registration relay and the Sensor's invitation (ticket
    /// #53). Each half is independently optional — a deployment may relay
    /// registration, invite the Sensor, both, or neither — and the endpoint of
    /// a half that is off answers 503 naming the variable that would open it.
    pub bootstrap: Bootstrap,
    /// Where the consent store publishes its committed decisions (ticket
    /// #49), or `None` when this deployment writes no consent. Everything
    /// else consent needs — the state directory, the owner, the domain its
    /// events name themselves by — is [`SignIn`]'s, so this is the one
    /// variable the ticket adds. See [`Consent`].
    pub consent: Option<Consent>,
    /// The bridges this deployment can drive logins on (ticket #55), in the
    /// order `GATEWAY_BRIDGES` names them. Empty when none is configured:
    /// `GET /api/bridges` then answers an empty list, which is the honest
    /// answer to "what can I connect?" — the origin, the session and consent
    /// are untouched. See [`bridges_from_env`].
    pub bridges: Vec<crate::bridge::BridgeConfig>,
    /// The consent snapshot a cold consumer reads (ticket #50), or `None`
    /// when GATEWAY_SERVICE_TOKEN is unset — in which case the snapshot
    /// endpoint answers 503 naming it. See [`Snapshot`].
    pub snapshot: Option<Snapshot>,
    /// The durable JetStream consumer the pending-contact projection reads
    /// `inbound.message.received` through (GATEWAY_INBOUND_CONSUMER, default
    /// [`crate::contacts::DEFAULT_INBOUND_CONSUMER`], ticket #54).
    ///
    /// One Gateway owns this consumer per deployment, and the default is
    /// right for every deployment that has one. It is configurable for the
    /// operator who points a second Gateway at the same bus — a staging copy
    /// reading production traffic, a migration running two at once — where
    /// sharing a durable name would split the stream between them and leave
    /// each showing half the contacts. The name is also what carries the
    /// "full delivery on first creation, its ack floor afterwards" property:
    /// renaming it makes the next start a first start, which replays the
    /// stream's whole history into a store that already holds it (harmless,
    /// because the sightings are idempotent, and slow).
    pub inbound_consumer: String,
    /// How many stream positions back an approval looks for the suggestion
    /// it names, and for the message that suggestion answers
    /// (GATEWAY_APPROVAL_LOOKUP_WINDOW, default
    /// [`crate::approval::DEFAULT_LOOKUP_WINDOW`], ticket #24).
    ///
    /// The bus has no index from a CloudEvents id to a stream position, so
    /// finding a suggestion means reading the stream, and the read is
    /// bounded. The bound is visible in the answer rather than hidden in it:
    /// a suggestion the window did not reach is refused as
    /// `suggestion_out_of_reach`, never as `suggestion_not_found`. Widen it
    /// on a deployment whose bus carries far more traffic than one person's
    /// conversations; the cost is a longer read on the approval path alone.
    pub approval_lookup_window: u64,
    /// How often the portal register is re-read in the background, in
    /// seconds (GATEWAY_PORTAL_REFRESH_SECONDS, default
    /// [`crate::portals::DEFAULT_REFRESH_SECONDS`], ticket #105). `0` turns
    /// the background read off.
    ///
    /// It decides only how stale `/metrics` may be about how many
    /// conversations the Sensor is outside: `GET /api/portals` always reads
    /// the homeserver there and then, so the Companion never sees a cached
    /// list. Lower it on a deployment that watches the gauge; the cost is one
    /// homeserver call per portal room per interval.
    pub portal_refresh_seconds: u64,
    /// Where the model configuration and the language preference live, and
    /// the operator's credential file if there is one (ticket #98). `None`
    /// on the same terms as [`Self::sign_in`], because the settings store is
    /// a file in `GATEWAY_STATE_DIR` and there is nobody to set a preference
    /// for without an owner. See [`Settings`].
    pub settings: Option<Settings>,
}

/// What the model and language settings need (ticket #98): the state
/// directory [`SignIn`] already names, and — optionally — the path of the
/// credential file that **wins** over whatever the Companion last wrote.
///
/// The precedence is ADR 0015's, and on the reference deployment it is the
/// ordinary combination rather than the exotic one: the model name comes
/// from the browser and the key from a file, so a production stack can lock
/// the credential down while a developer stays in the browser. The same rule
/// exists one process later in the Hermes runtime
/// (`HERMES_LLM_API_KEY_FILE` over `HERMES_LLM_API_KEY`), and the two are
/// spelled the same way on purpose.
#[derive(Debug, Clone)]
pub struct Settings {
    /// From [`SignIn::state_dir`]: `settings.sqlite3` inside it, beside the
    /// session store and the consent journal.
    pub state_dir: PathBuf,
    /// `GATEWAY_LLM_API_KEY_FILE` — a path **on this host**, mounted
    /// read-only into the container, holding the endpoint's credential and
    /// nothing else. Unset: the credential is whatever the Companion set, or
    /// none.
    ///
    /// Read once at startup, as the runtime reads its own, so a rotated file
    /// takes effect at the next restart; an empty one is a startup error
    /// rather than a silent fallback to the browser's value, which would be
    /// the precedence rule failing in the direction it exists to prevent.
    pub credential_file: Option<PathBuf>,
}

impl Settings {
    fn from_env(sign_in: Option<&SignIn>) -> Option<Self> {
        sign_in.map(|sign_in| Self {
            state_dir: sign_in.state_dir.clone(),
            credential_file: env("GATEWAY_LLM_API_KEY_FILE").map(PathBuf::from),
        })
    }
}

/// The bridges from the environment.
///
/// `GATEWAY_BRIDGES` lists the instances by `bridge_id`, comma-separated and
/// in the order the Companion should offer them:
///
/// ```text
/// GATEWAY_BRIDGES=mautrix-whatsapp,mautrix-signal
/// ```
///
/// Each one then takes its own three variables, keyed by the id with every
/// character that cannot appear in a variable name replaced by `_` and the
/// whole thing upper-cased (`mautrix-whatsapp` → `MAUTRIX_WHATSAPP`):
///
/// - `GATEWAY_BRIDGE_<ID>_URL` (required) — the bridge's appservice
///   listener, e.g. `http://bridge-whatsapp:29318`. The provisioning API
///   lives there, not on the homeserver.
/// - `GATEWAY_BRIDGE_<ID>_PROVISIONING_SECRET` (required) — the same value
///   as that bridge's own `provisioning.shared_secret`. It drives logins and
///   logouts on the user's account: the most powerful thing this facade
///   holds, and the reason the browser never talks to a bridge directly
///   (`docs/architecture/security-model.md`).
/// - `GATEWAY_BRIDGE_<ID>_NETWORK` — the network the user experiences
///   (`whatsapp`, `signal`, `sms`). Defaults to the id with a `mautrix-`
///   prefix stripped, which is right for the reference deployment and wrong
///   for `mautrix-gmessages`, whose network is `sms` (CONTEXT.md) — so that
///   one sets it. It has to be one of the contract's networks, because it is
///   what `bridge.status.changed` carries (ticket #56).
/// - `GATEWAY_BRIDGE_<ID>_AS_TOKEN` — the same value as that bridge's own
///   `appservice.as_token`, which is what the bridge authenticates its
///   status pushes with (ticket #56). Unset: that bridge's webhook is
///   **refused**, because an unverified push is not accepted — see
///   [`crate::bridge_status`].
/// - `GATEWAY_BRIDGE_<ID>_STATUS_ID` — the `bridge_id` this instance's
///   events carry, and the segment of its webhook URL. Defaults to
///   [`default_status_bridge_id`], which is what the reference deployment
///   already points its bridges at, so a deployment normally sets nothing.
///
/// A bridge that is named and then left without a URL or a secret is a
/// startup error naming the variable: a facade that silently has no bridge
/// to talk to would be discovered by a user on the QR screen.
/// `acting_as` is the deployment's owner: mautrix takes the acting user on
/// trust with shared-secret auth, and requires it on every call (#106).
pub fn bridges_from_env(acting_as: &str) -> Result<Vec<crate::bridge::BridgeConfig>> {
    let Some(listed) = env("GATEWAY_BRIDGES") else {
        return Ok(Vec::new());
    };
    let mut bridges = Vec::new();
    for bridge_id in listed
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let slug = variable_slug(bridge_id);
        let url_variable = format!("GATEWAY_BRIDGE_{slug}_URL");
        let secret_variable = format!("GATEWAY_BRIDGE_{slug}_PROVISIONING_SECRET");
        let base_url = env(&url_variable).with_context(|| {
            format!(
                "GATEWAY_BRIDGES names the bridge {bridge_id:?}, so {url_variable} is \
                 required: the bridge's appservice listener, where its provisioning API is"
            )
        })?;
        let provisioning_secret = env(&secret_variable).with_context(|| {
            format!(
                "GATEWAY_BRIDGES names the bridge {bridge_id:?}, so {secret_variable} is \
                 required: the same value as that bridge's provisioning.shared_secret"
            )
        })?;
        let network = env(&format!("GATEWAY_BRIDGE_{slug}_NETWORK")).unwrap_or_else(|| {
            bridge_id
                .strip_prefix("mautrix-")
                .unwrap_or(bridge_id)
                .to_owned()
        });
        let status_bridge_id = env(&format!("GATEWAY_BRIDGE_{slug}_STATUS_ID"))
            .map(|value| value.trim().to_owned())
            .unwrap_or_else(|| default_status_bridge_id(bridge_id));
        bridges.push(crate::bridge::BridgeConfig {
            acting_as: acting_as.to_owned(),
            bridge_id: bridge_id.to_owned(),
            status_bridge_id,
            network,
            base_url,
            provisioning_secret,
            as_token: env(&format!("GATEWAY_BRIDGE_{slug}_AS_TOKEN"))
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
        });
    }
    Ok(bridges)
}

/// The contract's `bridge_id` for an instance the operator has not named one
/// for: the instance id with a `mautrix-` prefix stripped, lower-cased,
/// anything outside `[a-z0-9-]` folded to `-`, under a `bridge-` prefix.
///
/// Deterministic, so it is stable across restarts — the property the
/// contract needs from a `bridge_id` — and it is the id the reference
/// deployment's `.env.example` already points each bridge's
/// `status_endpoint` at, so a working deployment sets nothing.
///
/// ```
/// # use twalk_companion_gateway::config::default_status_bridge_id;
/// assert_eq!(default_status_bridge_id("mautrix-whatsapp"), "bridge-whatsapp");
/// assert_eq!(default_status_bridge_id("mautrix-signal"), "bridge-signal");
/// // An id that is already the contract's is kept as it is.
/// assert_eq!(default_status_bridge_id("bridge-sms-android-1"), "bridge-sms-android-1");
/// assert_eq!(default_status_bridge_id("WhatsApp_2"), "bridge-whatsapp-2");
/// ```
pub fn default_status_bridge_id(bridge_id: &str) -> String {
    let slug: String = bridge_id
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.strip_prefix("mautrix-").unwrap_or(&slug);
    match slug.strip_prefix("bridge-") {
        Some(_) => slug.to_owned(),
        None => format!("bridge-{slug}"),
    }
}

/// The variable-name half of a `bridge_id`: upper case, and every character
/// that is not a letter or a digit becomes `_`.
///
/// ```
/// # use twalk_companion_gateway::config::variable_slug;
/// assert_eq!(variable_slug("mautrix-whatsapp"), "MAUTRIX_WHATSAPP");
/// assert_eq!(variable_slug("mautrix-gmessages"), "MAUTRIX_GMESSAGES");
/// // Whatever an operator calls an instance, the variable name is a legal one.
/// assert_eq!(variable_slug("bridge.2/x"), "BRIDGE_2_X");
/// ```
pub fn variable_slug(bridge_id: &str) -> String {
    bridge_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// What bootstrap needs, beyond the owner that [`SignIn`] already names.
#[derive(Debug, Clone)]
pub struct Bootstrap {
    /// Base URL of the homeserver's **client** API
    /// (GATEWAY_HOMESERVER_URL, e.g. `http://synapse:8008`): where both
    /// Synapse's admin registration endpoint and the room-invitation endpoint
    /// live.
    ///
    /// Unset, it falls back to [`SignIn::federation_base_url`], because in the
    /// reference deployment the client API and the one federation endpoint
    /// sign-in uses ride the same port — so a working deployment needs no new
    /// variable. `None` only when there is no sign-in configuration either, in
    /// which case the whole API is closed anyway.
    pub homeserver_url: Option<String>,
    /// Synapse's registration shared secret
    /// (GATEWAY_REGISTRATION_SHARED_SECRET), the same value as the
    /// homeserver's own `registration_shared_secret`. Unset: the relay is off
    /// and `POST /api/bootstrap/account` answers 503.
    ///
    /// Setting it opens an unauthenticated endpoint that can create exactly
    /// one account — the owner's, once. An operator who provisioned that
    /// account by hand leaves this unset; see [`crate::bootstrap`] for what
    /// the window is and when it closes.
    pub registration_shared_secret: Option<String>,
    /// The Matrix ID of the Sensor this deployment runs
    /// (GATEWAY_SENSOR_USER_ID, e.g. `@sensor:example.com`) — who gets
    /// invited into the rooms the user selects on screen 3d. The same value as
    /// the Sensor's own `SENSOR_USER_ID`. Unset: inviting is off.
    ///
    /// The Sensor's account stays provisioned by the compose stack: this is
    /// who to invite, never how to create it, and nothing here is on the
    /// Sensor's startup path.
    pub sensor_user_id: Option<String>,
}

/// Everything sign-in needs. Present as a whole or not at all: GATEWAY_OWNER
/// is what turns it on, and the rest is then required, because a Gateway that
/// knows its owner but not where to verify a token would refuse every
/// sign-in for a reason no error message would make obvious.
#[derive(Debug, Clone)]
pub struct SignIn {
    /// The Matrix ID of the single human this deployment serves
    /// (GATEWAY_OWNER, e.g. `@you:example.com`). Configuration, exactly as
    /// `SENSOR_ALLOWED_INVITERS` is: one owner per deployment, any number of
    /// devices, and multi-user is out of scope (ADR 0011). Any other Matrix
    /// ID is refused, the Sensor's own account included.
    pub owner: String,
    /// The Matrix server name derived from [`Self::owner`]: the domain an
    /// accepted OpenID token must belong to. Derived rather than configured
    /// so the two can never drift — a deployment whose owner is
    /// `@you:example.com` serves `example.com`.
    pub homeserver_name: String,
    /// Base URL of the homeserver's federation API
    /// (GATEWAY_HOMESERVER_FEDERATION_URL, e.g. `http://synapse:8008`),
    /// where the OpenID userinfo endpoint is reached.
    ///
    /// Pinned rather than resolved from the server name: a deployment serves
    /// one homeserver, which the operator already configured, and
    /// implementing Matrix's server-name resolution (`.well-known`, SRV, the
    /// 8448 fallback) would be a federation stack inside a service that does
    /// not federate. It decides which server is asked; the domain check on
    /// the answer stays either way (see [`crate::matrix_openid`]).
    pub federation_base_url: String,
    /// Directory the Gateway keeps its SQLite stores in (GATEWAY_STATE_DIR).
    /// The session store is `sessions.db` inside it.
    pub state_dir: PathBuf,
    /// Lifetime of a device token in seconds (GATEWAY_DEVICE_TOKEN_TTL,
    /// default 15 minutes). Short: it is the credential that travels on
    /// every request, and the Companion refreshes it while in use.
    pub device_token_ttl_seconds: u64,
    /// Lifetime of a refresh token in seconds (GATEWAY_REFRESH_TOKEN_TTL,
    /// default 30 days): how long a device that was left alone can come back
    /// without signing in again.
    pub refresh_token_ttl_seconds: u64,
}

/// What the consent store needs beyond sign-in's own configuration: the bus.
///
/// Consent is on when a deployment has both an owner and a bus, and the
/// rest is *derived* rather than configured again — the state directory and
/// the owner are [`SignIn`]'s, and the domain the events name themselves by
/// (`gateway://<domain>/consent`) is the owner's own server name. Nothing an
/// operator sets twice can drift.
///
/// Without `GATEWAY_NATS_URL` the consent endpoints answer `503
/// consent_not_configured` and name it; the origin and the session are
/// untouched. Without an owner there is nothing to attribute a decision to,
/// and #52's guard already closes the whole API and says so at startup, so
/// consent simply stays off.
#[derive(Debug, Clone)]
pub struct Consent {
    /// NATS server URL the outbox publishes to (GATEWAY_NATS_URL), e.g.
    /// `nats://nats:4222`. A bus that is down is not an error: decisions
    /// commit and wait in the outbox.
    pub nats_url: String,
    /// Directory the decision journal lives in, from
    /// [`SignIn::state_dir`]: `consent.sqlite3` inside it, next to the
    /// session store.
    pub state_dir: PathBuf,
    /// The domain the Gateway's events name themselves by, from
    /// [`SignIn::homeserver_name`].
    pub matrix_domain: String,
    /// The owner every decision is attributed to, from [`SignIn::owner`]:
    /// the event's `data.actor`. One owner per deployment (ADR 0011), so the
    /// device that took the decision is the owner's by construction.
    pub owner: String,
}

/// What the consent snapshot needs (ticket #50): the credential that
/// authenticates its one caller, and the cap that makes an oversized
/// snapshot an error instead of a truncation.
///
/// The caller is the Sensor, which is not a device and has no Matrix OpenID
/// token to sign in with, so the snapshot takes a **service token** from
/// configuration instead of a device cookie — the same value in the
/// Gateway's environment and in the Sensor's. Unset, the snapshot endpoint
/// answers `503 service_token_not_configured` and names the variable; every
/// other endpoint is untouched, and no device token has ever opened this one.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The shared secret a caller presents as `Authorization: Bearer …`
    /// (GATEWAY_SERVICE_TOKEN). It grants the whole consent state — every
    /// contact the user ever decided about — so it is generated, not chosen:
    /// `openssl rand -hex 32`.
    pub service_token: String,
    /// The largest snapshot this Gateway will serve
    /// (GATEWAY_CONSENT_SNAPSHOT_MAX_ENTRIES, default
    /// [`crate::consent_snapshot::DEFAULT_MAX_ENTRIES`]). There is no
    /// pagination: a state over the cap is refused with an error naming it,
    /// because a silently truncated snapshot would tell a consumer that
    /// contacts the user granted were never decided about.
    pub max_entries: usize,
}

/// How short a service token this Gateway refuses to start with. It is a
/// bearer token on a read of the user's whole social graph, so a short one
/// is a guessable one; 32 characters is what `openssl rand -hex 32` gives
/// with room to spare for a passphrase an operator typed.
const MINIMUM_SERVICE_TOKEN_LENGTH: usize = 32;

impl Snapshot {
    /// `Ok(None)` when GATEWAY_SERVICE_TOKEN is unset; an error when it is
    /// set to something too short to be a secret.
    ///
    /// Why an error rather than the "keep the origin up" treatment
    /// [`SignIn`] gets: a missing variable is a deployment an operator has
    /// not finished, and the endpoint says so. A present but weak one is a
    /// deliberate act that would publish the whole consent state to anyone
    /// who guesses it — the loud direction is to refuse to start, with a
    /// message naming the variable and the minimum.
    fn from_env() -> Result<Option<Self>> {
        let Some(service_token) = env("GATEWAY_SERVICE_TOKEN").map(|value| value.trim().to_owned())
        else {
            return Ok(None);
        };
        anyhow::ensure!(
            service_token.chars().count() >= MINIMUM_SERVICE_TOKEN_LENGTH,
            "environment variable GATEWAY_SERVICE_TOKEN is too short to be a secret: \
             it authenticates a read of the whole consent state, so it must be at least \
             {MINIMUM_SERVICE_TOKEN_LENGTH} characters (generate one with `openssl rand -hex 32`)"
        );
        let max_entries: usize = optional(
            "GATEWAY_CONSENT_SNAPSHOT_MAX_ENTRIES",
            &crate::consent_snapshot::DEFAULT_MAX_ENTRIES.to_string(),
        )?;
        anyhow::ensure!(
            max_entries > 0,
            "environment variable GATEWAY_CONSENT_SNAPSHOT_MAX_ENTRIES must be at least 1: \
             a cap of zero would refuse every snapshot, including an empty one"
        );
        Ok(Some(Self {
            service_token,
            max_entries,
        }))
    }
}

impl Consent {
    fn from_env(sign_in: Option<&SignIn>) -> Result<Option<Self>> {
        let nats_url = std::env::var("GATEWAY_NATS_URL")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        match (sign_in, nats_url) {
            (Some(sign_in), Some(nats_url)) => Ok(Some(Self {
                nats_url,
                state_dir: sign_in.state_dir.clone(),
                matrix_domain: sign_in.homeserver_name.clone(),
                owner: sign_in.owner.clone(),
            })),
            // A bus but no owner: not fatal, and nothing extra to say — a
            // decision with no owner to attribute it to cannot be recorded,
            // and without an owner #52's guard already closes the whole API
            // and says so at startup.
            (Some(_), None) | (None, Some(_)) | (None, None) => Ok(None),
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let sign_in = SignIn::from_env()?;
        let bootstrap = Bootstrap::from_env(sign_in.as_ref());
        Ok(Self {
            listen: optional("GATEWAY_LISTEN", "0.0.0.0:8080")?,
            static_dir: PathBuf::from(required("GATEWAY_STATIC_DIR")?),
            fallback_file: std::env::var("GATEWAY_FALLBACK_FILE")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "200.html".to_owned()),
            log_level: std::env::var("GATEWAY_LOG_LEVEL")
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "info".to_owned()),
            // Before `sign_in` is moved below: consent reads the owner, the
            // state directory and the domain from it.
            consent: Consent::from_env(sign_in.as_ref())?,
            // The owner is the acting user for every provisioning call
            // (#106); with no sign-in configured there is no owner, and a
            // bridge login could not be authenticated anyway.
            bridges: bridges_from_env(
                sign_in
                    .as_ref()
                    .map(|sign_in| sign_in.owner.as_str())
                    .unwrap_or_default(),
            )?,
            snapshot: Snapshot::from_env()?,
            inbound_consumer: env("GATEWAY_INBOUND_CONSUMER")
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| crate::contacts::DEFAULT_INBOUND_CONSUMER.to_owned()),
            approval_lookup_window: {
                let window: u64 = optional(
                    "GATEWAY_APPROVAL_LOOKUP_WINDOW",
                    &crate::approval::DEFAULT_LOOKUP_WINDOW.to_string(),
                )?;
                anyhow::ensure!(
                    window > 0,
                    "environment variable GATEWAY_APPROVAL_LOOKUP_WINDOW must be at least 1: \
                     a window of zero would refuse every approval as out of reach"
                );
                window
            },
            portal_refresh_seconds: optional(
                "GATEWAY_PORTAL_REFRESH_SECONDS",
                &crate::portals::DEFAULT_REFRESH_SECONDS.to_string(),
            )?,
            // The settings store (ticket #98) needs what sign-in already
            // names — the state directory — plus, optionally, the
            // operator's credential file.
            settings: Settings::from_env(sign_in.as_ref()),
            sign_in,
            bootstrap,
        })
    }
}

impl Bootstrap {
    /// Never fails: every part is optional, and a half whose variable is
    /// unset is simply off. What could be wrong with the values — a Sensor
    /// Matrix ID that is not one — is refused when
    /// [`crate::bootstrap::Bootstrap`] is built, which is where the error
    /// message belongs.
    fn from_env(sign_in: Option<&SignIn>) -> Self {
        Self {
            homeserver_url: env("GATEWAY_HOMESERVER_URL")
                .or_else(|| sign_in.map(|sign_in| sign_in.federation_base_url.clone())),
            registration_shared_secret: env("GATEWAY_REGISTRATION_SHARED_SECRET"),
            sensor_user_id: env("GATEWAY_SENSOR_USER_ID").map(|value| value.trim().to_owned()),
        }
    }
}

impl SignIn {
    /// `Ok(None)` when GATEWAY_OWNER is unset; an error when it is set and
    /// something it needs is missing or malformed.
    fn from_env() -> Result<Option<Self>> {
        let Some(owner) = std::env::var("GATEWAY_OWNER")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        let homeserver_name = crate::matrix_openid::domain_of(&owner)
            .with_context(|| {
                format!(
                    "environment variable GATEWAY_OWNER is not a Matrix user ID \
                     (expected @localpart:server_name, got {owner:?})"
                )
            })?
            .to_owned();
        Ok(Some(Self {
            owner,
            homeserver_name,
            federation_base_url: required("GATEWAY_HOMESERVER_FEDERATION_URL")?,
            state_dir: PathBuf::from(required("GATEWAY_STATE_DIR")?),
            device_token_ttl_seconds: optional(
                "GATEWAY_DEVICE_TOKEN_TTL",
                &crate::session::DEFAULT_DEVICE_TOKEN_TTL_SECONDS.to_string(),
            )?,
            refresh_token_ttl_seconds: optional(
                "GATEWAY_REFRESH_TOKEN_TTL",
                &crate::session::DEFAULT_REFRESH_TOKEN_TTL_SECONDS.to_string(),
            )?,
        }))
    }
}

/// An environment variable, treating an empty value as unset: compose
/// interpolates an unset `.env` entry to the empty string, so the two must
/// mean the same thing throughout.
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn required(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("missing required environment variable {name}"))
}

fn optional<T>(name: &str, default: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let raw = std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_owned());
    raw.parse()
        .with_context(|| format!("environment variable {name} has an invalid value"))
}
