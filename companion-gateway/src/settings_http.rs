//! The settings' HTTP surface: the model configuration, the user's native
//! language, the probe that says why an endpoint is not working, the one
//! read the Hermes runtime makes (ticket #98) — and, since ticket #121, the
//! disclosure switch.
//!
//! # Who is calling
//!
//! All but one of the operations are the owner's browser and are behind
//! #52's guard like everything else under `/api/`. The exception,
//! `GET /api/settings/runtime`, is the **Hermes runtime** — a service, not a
//! device, with no Matrix OpenID token to sign in with — so it presents the
//! same service token the consent snapshot takes (#50), which this module
//! verifies through that module's own comparison rather than writing a
//! second one. That is deliberate in both directions: the runtime already
//! needs that token, and a persona is never given it, because it is also the
//! token that opens the list of every contact (ADR 0015).
//!
//! # What never comes back
//!
//! The endpoint credential. `GET /api/settings/model` says whether one is
//! configured, which of the two sources is in force and its last four
//! characters, and that is the whole of what a browser is told. The only
//! answer that carries the credential itself is the runtime's, and its
//! caller is the process that injects it into a persona's environment.
//!
//! # Why there is a probe at all
//!
//! Because "it does not work" has four causes here and they have four
//! different fixes, and this project has produced nine incidents in two days
//! from failures that shared one signal ([#116](https://github.com/linagora/twalk/issues/116),
//! [#141](https://github.com/linagora/twalk/issues/141)):
//!
//! - nothing is configured — `409 model_not_configured`;
//! - the endpoint cannot be reached — `502 endpoint_unreachable`. On the
//!   reference deployment this is the loopback trap: a proxy published on
//!   `127.0.0.1` is a perfectly good address that a container of its own
//!   network namespace dials as itself;
//! - the endpoint answered and refused — `502 endpoint_refused`, carrying
//!   its own status: a wrong credential, a model name it does not serve, a
//!   rate limit;
//! - the endpoint answered a success that is not a chat completion —
//!   `502 endpoint_not_compatible`: the wrong port, almost always.
//!
//! The probe is the **only** endpoint on this origin that spends the
//! operator's money: it sends one completion of one token to the configured
//! endpoint, and it does so only when a human asks. It sends `ping` and no
//! message content ever reaches it.
//!
//! # The disclosure switch is not a setting
//!
//! `GET`/`PUT /api/settings/disclosure` (ticket #121) live on this surface
//! because a settings screen is where a user looks for them, and they are
//! deliberately **not** in the settings store: ADR 0019 requires that
//! turning the disclosure off be a recorded deliberate act, so a `PUT`
//! appends a dated, attributed row to an append-only journal beside the
//! consent journal ([`crate::store`]) and the `GET` reads its last row — or
//! answers **on** when there is none, which is the default and the state of
//! every deployment that never touched it. The `actor` is the deployment's
//! owner, stamped here from configuration exactly as a consent decision's
//! is, never from the body. That journal is in the consent store, so on a
//! deployment with no bus these two answer `503 consent_not_configured`:
//! there is no approval path for the switch to govern.

use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::{json, Value};
use tracing::{debug, error, info, warn};

use crate::http::Gateway;
use crate::session::Device;
use crate::settings::{
    chat_completions_url, parse_language_update, parse_model_update, probe_payload, Credential,
    Language, ModelConfiguration, ProbeOutcome, Settings,
};

/// How long the probe waits for the configured endpoint before calling it
/// unreachable. Long enough for a cold model on a busy host, short enough
/// that a human pressing a button in a browser gets an answer: what is being
/// tested is that something is there and willing, not how fast it thinks.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The settings routes. Merged into the Gateway's router, so registering
/// them is additive — and behind #52's guard without asking, except for the
/// runtime's read, which that guard's own table declares as a service-token
/// route.
pub fn routes() -> Router<Gateway> {
    Router::new()
        .route(
            "/api/settings/model",
            get(read_model).put(write_model).delete(clear_model),
        )
        .route("/api/settings/model/probe", post(probe_model))
        .route(
            "/api/settings/language",
            get(read_language).put(write_language),
        )
        .route(
            "/api/settings/disclosure",
            get(read_disclosure).put(write_disclosure),
        )
        .route(
            "/api/settings/calendar-location",
            get(read_calendar_location).put(write_calendar_location),
        )
        .route(
            "/api/settings/working-day",
            get(read_working_day).put(write_working_day),
        )
        .route("/api/settings/collection", get(collection_settings))
        .route("/api/settings/runtime", get(runtime_settings))
}

/// `GET /api/settings/model` — the model configuration, without the
/// credential.
///
/// ```json
/// {
///   "configured": true,
///   "base_url": "http://127.0.0.1:4000/v1",
///   "model": "qwen",
///   "params": null,
///   "credential": {
///     "configured": true,
///     "source": "file",
///     "hint": "-key",
///     "file": "/run/secrets/litellm-master.key",
///     "companion_credential_stored": true
///   },
///   "updated_at": "2026-09-18T16:24:21.000Z",
///   "personas": {}
/// }
/// ```
///
/// `configured: false` is a state and not a refusal: a deployment with no
/// model named is exactly what ADR 0015 leaves an operator with until they
/// name one, and a settings screen needs to draw it without an error branch.
///
/// `credential.source` is what makes "the file wins" visible. A deployment
/// with a file and a browser-set value shows `source: "file"` and
/// `companion_credential_stored: true`, because "why is the key I pasted not
/// being used?" has to be answerable from the interface.
async fn read_model(State(gateway): State<Gateway>) -> Response {
    let Some(settings) = gateway.settings() else {
        return not_configured();
    };
    match model_json(&settings) {
        Ok(document) => Json(document).into_response(),
        Err(error) => {
            error!(%error, "failed to read the model configuration");
            store_unavailable()
        }
    }
}

/// `PUT /api/settings/model` — name the endpoint and the model.
///
/// Replaces the configuration whole. `credential` absent keeps the stored
/// one, `credential: null` forgets it, and a string replaces it; the
/// distinction exists because the credential is write-only, so a screen that
/// only renamed the model has nothing to send back.
///
/// The answer is what `GET` would now return — the credential still absent
/// from it.
async fn write_model(
    State(gateway): State<Gateway>,
    Extension(device): Extension<Device>,
    body: String,
) -> Response {
    let Some(settings) = gateway.settings() else {
        return not_configured();
    };
    let body: Value = match serde_json::from_str(&body) {
        Ok(body) => body,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "malformed_request",
                &format!("the request body is not JSON: {error}"),
            )
        }
    };
    let update = match parse_model_update(&body) {
        Ok(update) => update,
        Err(invalid) => {
            debug!(
                code = invalid.code(),
                device = %device.id,
                "refused a model configuration"
            );
            return api_error(StatusCode::BAD_REQUEST, invalid.code(), &invalid.message());
        }
    };
    if let Err(error) = settings.set_model(&update) {
        error!(%error, "failed to write the model configuration");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store_unavailable",
            "the model configuration could not be written; nothing was changed",
        );
    }
    // The endpoint and the model, never the credential: a log line is the
    // easiest place in a service to leak a secret into.
    info!(
        base_url = %update.base_url,
        model = %update.model,
        passthrough = update.params.is_some(),
        device = %device.id,
        "the model configuration was set from the Companion"
    );
    if settings.credential_file_path().is_some() {
        info!(
            "a credential file is configured, so it stays the one in force: a value set from \
             the browser is stored and not used (ADR 0015)"
        );
    }
    match model_json(&settings) {
        Ok(document) => Json(document).into_response(),
        Err(error) => {
            error!(%error, "failed to read back the model configuration");
            store_unavailable()
        }
    }
}

/// `DELETE /api/settings/model` — forget the endpoint, the model and the
/// credential set from the browser.
///
/// `204` whether or not there was anything to forget: the caller asked for a
/// deployment with no model configured and that is what it has. The
/// operator's credential file is untouched — it is not the Companion's to
/// remove, and a deployment with a key and no model is an ordinary state on
/// the way to naming another one.
async fn clear_model(
    State(gateway): State<Gateway>,
    Extension(device): Extension<Device>,
) -> Response {
    let Some(settings) = gateway.settings() else {
        return not_configured();
    };
    match settings.clear_model() {
        Ok(removed) => {
            info!(
                removed,
                device = %device.id,
                "the model configuration was cleared: personas have no model to reason with \
                 until one is named again (ADR 0015)"
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            error!(%error, "failed to clear the model configuration");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_unavailable",
                "the model configuration could not be cleared; nothing was changed",
            )
        }
    }
}

/// `POST /api/settings/model/probe` — ask the configured endpoint whether it
/// is there, whether it accepts this credential and whether it knows this
/// model.
///
/// No request body. One chat completion of one token, with the operator's
/// provider parameters merged in exactly as a persona would merge them, so
/// what this proves is what a persona will do.
///
/// Four answers, because there are four causes:
///
/// | | |
/// | --- | --- |
/// | `200` `outcome: "ok"` | the endpoint answered a chat completion |
/// | `409 model_not_configured` | nothing is configured to probe |
/// | `502 endpoint_unreachable` | nothing answered — DNS, connection, TLS, the deadline |
/// | `502 endpoint_refused` | it answered and said no, with its own status |
/// | `502 endpoint_not_compatible` | it answered a success that is not a chat completion |
async fn probe_model(
    State(gateway): State<Gateway>,
    Extension(device): Extension<Device>,
) -> Response {
    let Some(settings) = gateway.settings() else {
        return not_configured();
    };
    let model = match settings.model() {
        Ok(Some(model)) => model,
        Ok(None) => {
            // Distinct from every endpoint failure below, and deliberately
            // not a 404: the route exists, the deployment is fine, and
            // nothing was named.
            return api_error(
                StatusCode::CONFLICT,
                "model_not_configured",
                "there is no model to probe: set one with PUT /api/settings/model. Twalk ships \
                 no default and nothing is sent to a model the operator did not name \
                 (ADR 0015)",
            );
        }
        Err(error) => {
            error!(%error, "failed to read the model configuration");
            return store_unavailable();
        }
    };
    let credential = match settings.credential_in_force() {
        Ok(credential) => credential,
        Err(error) => {
            error!(%error, "failed to resolve the endpoint credential");
            return store_unavailable();
        }
    };
    let outcome = probe(&model, credential.as_ref()).await;
    match &outcome {
        ProbeOutcome::Ok { .. } => info!(
            base_url = %model.base_url,
            model = %model.model,
            credential = credential.as_ref().map(|credential| credential.source.as_str()).unwrap_or("none"),
            device = %device.id,
            "the configured endpoint answered a completion"
        ),
        failed => warn!(
            base_url = %model.base_url,
            model = %model.model,
            outcome = failed.code(),
            device = %device.id,
            "the configured endpoint did not answer a completion"
        ),
    }
    probe_response(&model, &outcome)
}

/// Sends the probe. Everything it decides from the answer is
/// [`ProbeOutcome::from_answer`]'s, which is pure and tested; this function
/// is the socket.
async fn probe(model: &ModelConfiguration, credential: Option<&Credential>) -> ProbeOutcome {
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(client) => client,
        Err(error) => {
            return ProbeOutcome::Unreachable {
                detail: format!("the Gateway could not build an HTTP client: {error}"),
            }
        }
    };
    let mut request = client
        .post(chat_completions_url(&model.base_url))
        .json(&probe_payload(&model.model, model.params.as_ref()));
    if let Some(credential) = credential {
        request = request.bearer_auth(&credential.value);
    }
    match request.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            ProbeOutcome::from_answer(status, &body)
        }
        // Every transport failure is this one: DNS, a refused connection, a
        // TLS handshake, and the deadline above. They are all "nothing
        // answered", which is the distinction that matters against the
        // status-carrying refusal below.
        Err(error) => ProbeOutcome::Unreachable {
            detail: format!(
                "the chat-completions endpoint at {} could not be reached: {error}",
                chat_completions_url(&model.base_url)
            ),
        },
    }
}

fn probe_response(model: &ModelConfiguration, outcome: &ProbeOutcome) -> Response {
    match outcome {
        ProbeOutcome::Ok {
            endpoint_status,
            model: answered,
        } => Json(json!({
            "outcome": "ok",
            "endpoint_status": endpoint_status,
            "base_url": model.base_url,
            "model": model.model,
            "endpoint_model": answered,
        }))
        .into_response(),
        ProbeOutcome::Unreachable { detail } => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": outcome.code(),
                "detail": detail,
                "endpoint_status": Value::Null,
            })),
        )
            .into_response(),
        ProbeOutcome::Refused {
            endpoint_status,
            detail,
        }
        | ProbeOutcome::NotCompatible {
            endpoint_status,
            detail,
        } => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": outcome.code(),
                "detail": detail,
                "endpoint_status": endpoint_status,
            })),
        )
            .into_response(),
    }
}

/// `GET /api/settings/language` — the user's native language, and the five
/// the Companion ships.
///
/// `language: null` is "no preference", which is not the same as English: a
/// persona whose fallback is unset writes in the language of the message it
/// is answering and nothing else. Defaulting silently to English is what
/// [#164](https://github.com/linagora/twalk/issues/164) found in production,
/// against a French speaker.
async fn read_language(State(gateway): State<Gateway>) -> Response {
    let Some(settings) = gateway.settings() else {
        return not_configured();
    };
    match settings.language() {
        Ok(language) => Json(language_json(language)).into_response(),
        Err(error) => {
            error!(%error, "failed to read the language preference");
            store_unavailable()
        }
    }
}

/// `PUT /api/settings/language` — `{"language": "fr"}`, or
/// `{"language": null}` for no preference.
async fn write_language(
    State(gateway): State<Gateway>,
    Extension(device): Extension<Device>,
    body: String,
) -> Response {
    let Some(settings) = gateway.settings() else {
        return not_configured();
    };
    let body: Value = match serde_json::from_str(&body) {
        Ok(body) => body,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "malformed_request",
                &format!("the request body is not JSON: {error}"),
            )
        }
    };
    let language = match parse_language_update(&body) {
        Ok(language) => language,
        Err(invalid) => {
            debug!(
                code = invalid.code(),
                device = %device.id,
                "refused a language preference"
            );
            return api_error(StatusCode::BAD_REQUEST, invalid.code(), &invalid.message());
        }
    };
    if let Err(error) = settings.set_language(language) {
        error!(%error, "failed to write the language preference");
        return api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store_unavailable",
            "the language preference could not be written; nothing was changed",
        );
    }
    info!(
        language = language.map(|language| language.as_str()).unwrap_or("none"),
        device = %device.id,
        "the native language was set from the Companion: it governs the interface and a \
         persona's fallback, never the text sent to a contact (ADR 0016)"
    );
    Json(language_json(language)).into_response()
}

/// `GET /api/settings/disclosure` — the switch as the journal answers it.
///
/// ```json
/// {
///   "enabled": false,
///   "since": "2026-09-20T10:04:37.000Z",
///   "actor": "@michel:example.com",
///   "reason": "a test"
/// }
/// ```
///
/// `since`, `actor` and `reason` are `null` while nobody has decided, which
/// is the default state — `enabled: true` — and not a gap: the record
/// answers "since when and who" for a decision, and there was none.
async fn read_disclosure(State(gateway): State<Gateway>) -> Response {
    read_switch(&gateway, Kind::Disclosure).await
}

/// `PUT /api/settings/disclosure` — `{"enabled": false, "reason": "…"}`.
///
/// Appends a row and answers the state it left behind. Every call appends,
/// including one restating the current state: the journal is the record of
/// what the user decided and when, not a projection to keep tidy.
async fn write_disclosure(
    State(gateway): State<Gateway>,
    Extension(device): Extension<Device>,
    body: String,
) -> Response {
    write_switch(&gateway, &device, Kind::Disclosure, &body).await
}

/// `GET /api/settings/calendar-location` — whether a calendar event may
/// carry where the meeting is (#354, #351).
///
/// The disclosure's shape, member for member, because it is the same kind
/// of thing: one decision for the deployment, recorded with who and when.
/// The default is the other way — `enabled: false`, and `since`, `actor`
/// and `reason` `null`, which says "as it shipped" and not "somebody turned
/// it off".
async fn read_calendar_location(State(gateway): State<Gateway>) -> Response {
    read_switch(&gateway, Kind::CalendarLocation).await
}

/// `PUT /api/settings/calendar-location` — `{"enabled": true, "reason": "…"}`.
///
/// Appends a row and answers the state it left behind, as the disclosure
/// does. Opening it is the noisier direction here: what it lets leave the
/// machine is a place, and on an invitation a place somebody else wrote.
async fn write_calendar_location(
    State(gateway): State<Gateway>,
    Extension(device): Extension<Device>,
    body: String,
) -> Response {
    write_switch(&gateway, &device, Kind::CalendarLocation, &body).await
}

/// `GET /api/settings/working-day` — the days the owner accepts meetings on
/// and how wide those days are (#381).
///
/// ```json
/// {
///   "day": { "days": [1, 2, 3, 4, 5], "starts_at": "09:00", "ends_at": "18:00" },
///   "since": "2026-09-26T15:12:04.000Z",
///   "actor": "@michel:twalk.localhost",
///   "reason": null
/// }
/// ```
///
/// `day: null` with `since: null` is a deployment where nobody ever said, and
/// `day: null` with a `since` is one where somebody said and then cleared it.
/// Both mean every free gap is offered; the screen can tell them apart, which
/// is the only reason the difference is visible here.
async fn read_working_day(State(gateway): State<Gateway>) -> Response {
    let Some(consent) = gateway.consent() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "consent_not_configured",
            "this Gateway keeps no working day: it has no consent store to journal one in",
        );
    };
    match consent.store().working_day_state() {
        Ok(state) => Json(working_day_json(&state)).into_response(),
        Err(error) => {
            error!(%error, "failed to read the working day journal");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_unavailable",
                "the working day journal could not be read",
            )
        }
    }
}

/// `PUT /api/settings/working-day` —
/// `{"days": [1,2,3,4,5], "starts_at": "09:00", "ends_at": "18:00"}`, or
/// `{"days": null}` to say nothing again.
///
/// Appends a row and answers the state it left behind, as the two switches
/// do, and for the same reason: the journal is the record of what the owner
/// decided and when, not a projection to keep tidy. Nothing is repaired on
/// the way in — an hour of somebody's evening must not be given away by a
/// parser being helpful.
async fn write_working_day(
    State(gateway): State<Gateway>,
    Extension(device): Extension<Device>,
    body: String,
) -> Response {
    let Some(consent) = gateway.consent() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "consent_not_configured",
            "this Gateway keeps no working day: it has no consent store to journal one in",
        );
    };
    let update = match crate::working_day::parse_update(&body) {
        Ok(update) => update,
        Err(invalid) => {
            return api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                invalid.code(),
                &invalid.message(),
            )
        }
    };
    let occurred_at = crate::consent::rfc3339_millis(std::time::SystemTime::now());
    // The actor is the owner from configuration, as it is for the two
    // switches: the device says a request is theirs, and configuration says
    // who they are. See `write_switch` for the argument.
    let _ = &device;
    let actor = gateway.owner();
    match consent
        .store()
        .record_working_day_decision(&update, &occurred_at, &actor)
    {
        Ok(state) => {
            match &state.day {
                Some(day) => info!(
                    %actor,
                    days = ?day.days,
                    starts_at = %day.starts_at,
                    ends_at = %day.ends_at,
                    "the owner set their working day"
                ),
                None => info!(
                    %actor,
                    "the owner cleared their working day; every free gap is offered again"
                ),
            }
            Json(working_day_json(&state)).into_response()
        }
        Err(error) => {
            error!(%error, "failed to record a working day decision");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_unavailable",
                "the working day journal could not be written",
            )
        }
    }
}

/// The working day as both of its routes answer it.
fn working_day_json(state: &crate::working_day::State) -> Value {
    json!({
        "day": state.day.as_ref().map(|day| json!({
            "days": day.days,
            "starts_at": day.starts_at,
            "ends_at": day.ends_at,
        })),
        "since": state.since,
        "actor": state.actor,
        "reason": state.reason,
    })
}

/// `GET /api/settings/collection` — what a service that **collects** the
/// owner's own data needs to know before it publishes (#354).
///
/// The collector's seam, and it takes the service token, as the runtime
/// settings and the consent snapshot do. A route of its own rather than a
/// member on `/api/settings/runtime`: that document carries the model's API
/// key, and a collector has no business holding one. Answering the question
/// it asks means handing it the answer and nothing else.
///
/// ```json
/// {
///   "calendar_location": { "enabled": false },
///   "working_day": { "days": [1, 2, 3, 4, 5], "starts_at": "09:00", "ends_at": "18:00" }
/// }
/// ```
///
/// Only what a decision *is*, never who decided it or when: that belongs to
/// the owner's screen, and a collector reading it would be a collector
/// holding a fact about the owner it has no use for.
///
/// `working_day` is `null` when the owner never said, or said and cleared it
/// (#381) — which means the read answers every free gap, as it did before
/// that decision existed. A collector that finds `null` does not filter.
async fn collection_settings(State(gateway): State<Gateway>, headers: HeaderMap) -> Response {
    let Some(service_token) = gateway.snapshots() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_token_not_configured",
            "this Gateway serves no collection settings: set GATEWAY_SERVICE_TOKEN to the same \
             value the collector is configured with",
        );
    };
    if !service_token.authenticates(&headers) {
        warn!("refused a collection settings read: the service token is missing or wrong");
        return api_error(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "the collection settings take this Gateway's service token as an \
             Authorization: Bearer credential; a device token is not accepted here",
        );
    }
    let Some(consent) = gateway.consent() else {
        return switch_not_configured(Kind::CalendarLocation);
    };
    let location = match consent.store().calendar_location_state() {
        Ok(state) => state.enabled,
        Err(error) => {
            error!(%error, "failed to read the calendar location journal");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_unavailable",
                "the calendar location journal could not be read",
            );
        }
    };
    // The working day, or `null` (#381). A journal that cannot be read
    // answers `null` and says so in the log rather than refusing the whole
    // document: a collector that cannot learn the amplitude must still learn
    // whether a location may travel, and not filtering is the safe way to be
    // wrong about an amplitude — every gap is offered, as before.
    let working_day = match consent.store().working_day_state() {
        Ok(state) => state.day,
        Err(error) => {
            error!(%error, "failed to read the working day journal; answering none");
            None
        }
    };
    Json(json!({
        "calendar_location": { "enabled": location },
        "working_day": working_day.map(|day| json!({
            "days": day.days,
            "starts_at": day.starts_at,
            "ends_at": day.ends_at,
        })),
    }))
    .into_response()
}

/// Which of the owner's two recorded switches a request is about (#121,
/// #354). The routes differ in their journal, their parser, their gauge and
/// the two sentences the log says either way; everything else — reading the
/// store, stamping the actor, appending, answering — is one piece of code,
/// because the decision is the same kind of act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Disclosure,
    CalendarLocation,
}

impl Kind {
    /// What a failure names the journal by.
    fn journal(self) -> &'static str {
        match self {
            Self::Disclosure => "disclosure",
            Self::CalendarLocation => "calendar location",
        }
    }

    fn state(self, store: &crate::store::Store) -> anyhow::Result<crate::switch::State> {
        match self {
            Self::Disclosure => store.disclosure_state(),
            Self::CalendarLocation => store.calendar_location_state(),
        }
    }

    fn parse(self, body: &Value) -> Result<crate::switch::Update, crate::switch::Invalid> {
        match self {
            Self::Disclosure => crate::disclosure::parse_update(body),
            Self::CalendarLocation => crate::calendar_location::parse_update(body),
        }
    }

    fn record(
        self,
        store: &crate::store::Store,
        update: &crate::switch::Update,
        occurred_at: &str,
        actor: &str,
    ) -> anyhow::Result<crate::switch::State> {
        let reason = update.reason.as_deref();
        match self {
            Self::Disclosure => {
                store.record_disclosure_decision(update.enabled, occurred_at, actor, reason)
            }
            Self::CalendarLocation => {
                store.record_calendar_location_decision(update.enabled, occurred_at, actor, reason)
            }
        }
    }

    fn observe(self, metrics: &crate::metrics::Metrics, enabled: bool) {
        match self {
            Self::Disclosure => metrics.set_disclosure_enabled(enabled),
            Self::CalendarLocation => metrics.set_calendar_location_enabled(enabled),
        }
    }

    /// The one line the log says, and whether it is the noteworthy
    /// direction. They are opposite directions: a reply going out
    /// undisclosed is what somebody should notice, and so is a location
    /// starting to leave the machine.
    fn said(self, enabled: bool) -> (bool, &'static str) {
        match (self, enabled) {
            (Self::Disclosure, true) => (
                false,
                "the disclosure is on: every approved reply carries the sentence ADR 0019 \
                 requires, after the body, in the language the reply was written in",
            ),
            (Self::Disclosure, false) => (
                true,
                "the disclosure was turned OFF: approved replies go out without the sentence \
                 ADR 0019 requires until it is turned on again. Recorded in the disclosure \
                 journal with who decided and when",
            ),
            (Self::CalendarLocation, true) => (
                true,
                "the calendar location is ON: calendar events now carry where a meeting is, \
                 including meetings a third party organised. Recorded in the calendar location \
                 journal with who decided and when",
            ),
            (Self::CalendarLocation, false) => (
                false,
                "the calendar location is off: no calendar event carries where a meeting is",
            ),
        }
    }
}

/// One switch, read.
async fn read_switch(gateway: &Gateway, kind: Kind) -> Response {
    let Some(consent) = gateway.consent() else {
        return switch_not_configured(kind);
    };
    match kind.state(consent.store()) {
        Ok(state) => Json(switch_json(&state)).into_response(),
        Err(error) => {
            error!(%error, journal = kind.journal(), "failed to read a switch journal");
            switch_store_unavailable(kind, "read")
        }
    }
}

/// One decision, appended, and the state it left behind.
async fn write_switch(gateway: &Gateway, device: &Device, kind: Kind, body: &str) -> Response {
    let Some(consent) = gateway.consent() else {
        return switch_not_configured(kind);
    };
    let body: Value = match serde_json::from_str(body) {
        Ok(body) => body,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "malformed_request",
                &format!("the request body is not JSON: {error}"),
            )
        }
    };
    let update = match kind.parse(&body) {
        Ok(update) => update,
        Err(invalid) => {
            debug!(
                code = invalid.code(),
                device = %device.id,
                journal = kind.journal(),
                "refused a switch decision"
            );
            return api_error(StatusCode::BAD_REQUEST, invalid.code(), invalid.message());
        }
    };
    // The actor is the owner, from configuration: the one person who can
    // take this decision (ADR 0011), and never a name the body supplies.
    let actor = gateway.owner();
    let occurred_at = crate::consent::rfc3339_millis(std::time::SystemTime::now());
    let state = match kind.record(consent.store(), &update, &occurred_at, &actor) {
        Ok(state) => state,
        Err(error) => {
            error!(%error, journal = kind.journal(), "failed to record a switch decision");
            return switch_store_unavailable(kind, "record");
        }
    };
    kind.observe(gateway.metrics(), state.enabled);
    // `warn` for the direction somebody should notice, `info` for the other.
    let (noteworthy, sentence) = kind.said(state.enabled);
    if noteworthy {
        warn!(actor = %actor, device = %device.id, reason = update.reason.is_some(), "{sentence}");
    } else {
        info!(actor = %actor, device = %device.id, "{sentence}");
    }
    Json(switch_json(&state)).into_response()
}

/// `GET /api/settings/runtime` — everything the Hermes runtime injects into
/// a persona's environment, in one read.
///
/// ```json
/// {
///   "llm": {
///     "base_url": "http://127.0.0.1:4000/v1",
///     "model": "qwen",
///     "api_key": "sk-…",
///     "credential_source": "file",
///     "params": null
///   },
///   "language": "fr",
///   "personas": {}
/// }
/// ```
///
/// `llm: null` when no model is configured. That is the third of the three
/// causes the acceptance criteria are about, one level up: a runtime that
/// cannot reach this Gateway sees a transport failure, one whose token is
/// wrong sees `401`, and one that is told `llm: null` knows the operator has
/// named no model and refuses to start saying so (ADR 0015). Three facts,
/// three signals, none of them an empty document that could be mistaken for
/// another.
///
/// `personas` is always `{}` in v0.1 — see [`crate::settings`].
async fn runtime_settings(State(gateway): State<Gateway>, headers: HeaderMap) -> Response {
    // The same service token the consent snapshot takes (#50), verified by
    // that module's own constant-time, length-free comparison rather than by
    // a second copy of it. The runtime already holds this token; a persona
    // never does, because it also opens the list of every contact
    // (ADR 0015).
    let Some(service_token) = gateway.snapshots() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_token_not_configured",
            "this Gateway serves no runtime settings: set GATEWAY_SERVICE_TOKEN to the same \
             value the Hermes runtime is configured with",
        );
    };
    if !service_token.authenticates(&headers) {
        warn!("refused a runtime settings read: the service token is missing or wrong");
        return api_error(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "the runtime settings take this Gateway's service token as an \
             Authorization: Bearer credential; a device token is not accepted here",
        );
    }
    let Some(settings) = gateway.settings() else {
        return not_configured();
    };
    match runtime_json(&settings) {
        Ok(document) => Json(document).into_response(),
        Err(error) => {
            error!(%error, "failed to read the runtime settings");
            store_unavailable()
        }
    }
}

// ---------------------------------------------------------------------------
// The documents
// ---------------------------------------------------------------------------

/// The browser-facing model document. The credential is described and never
/// carried.
fn model_json(settings: &Settings) -> anyhow::Result<Value> {
    let model = settings.model()?;
    let credential = settings.credential_in_force()?;
    let companion_credential_stored = settings.companion_credential_stored()?;
    Ok(json!({
        "configured": model.is_some(),
        "base_url": model.as_ref().map(|model| model.base_url.clone()),
        "model": model.as_ref().map(|model| model.model.clone()),
        "params": model.as_ref().and_then(|model| model.params.clone()),
        "updated_at": model.as_ref().map(|model| model.updated_at.clone()),
        "credential": {
            "configured": credential.is_some(),
            "source": credential.as_ref().map(|credential| credential.source.as_str()),
            "hint": credential.as_ref().and_then(Credential::hint),
            "file": settings.credential_file_path(),
            "companion_credential_stored": companion_credential_stored,
        },
        "personas": {},
    }))
}

fn language_json(language: Option<Language>) -> Value {
    json!({
        "language": language.map(|language| language.as_str()),
        "available": Language::available(),
    })
}

/// The switch, as both routes answer it: `openapi.yaml`'s `DisclosureState`.
/// A switch as both of them are answered: the state, and the decision that
/// left it — `null` for all three when nobody decided, which is a fact and
/// not a gap.
fn switch_json(state: &crate::switch::State) -> Value {
    json!({
        "enabled": state.enabled,
        "since": state.since,
        "actor": state.actor,
        "reason": state.reason,
    })
}

/// The runtime's document: the one read on this origin that carries the
/// credential, because its caller is what puts it in a persona's
/// environment.
fn runtime_json(settings: &Settings) -> anyhow::Result<Value> {
    let model = settings.model()?;
    let credential = settings.credential_in_force()?;
    let llm = match model {
        Some(model) => json!({
            "base_url": model.base_url,
            "model": model.model,
            "api_key": credential.as_ref().map(|credential| credential.value.clone()),
            "credential_source": credential.as_ref().map(|credential| credential.source.as_str()),
            "params": model.params,
        }),
        None => Value::Null,
    };
    Ok(json!({
        "llm": llm,
        "language": settings.language()?.map(|language| language.as_str()),
        "personas": {},
    }))
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// This Gateway keeps no settings. It is configured exactly when sign-in is
/// — both need `GATEWAY_STATE_DIR` and the owner — so in practice #52's
/// guard has already answered `sign_in_not_configured`; this is the same
/// fact said by the handler, so the surface has no silent corner.
fn not_configured() -> Response {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "settings_not_configured",
        "this Gateway keeps no settings: set GATEWAY_OWNER and GATEWAY_STATE_DIR",
    )
}

fn store_unavailable() -> Response {
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store_unavailable",
        "the settings could not be read",
    )
}

/// The disclosure journal lives in the consent store, which exists exactly
/// when the bus is configured — and with no bus there is no approval path
/// for the switch to govern. The consent routes' own code, so a client
/// learns one fact under one word.
/// A switch this deployment cannot hold, named by its journal (#121, #354).
fn switch_not_configured(kind: Kind) -> Response {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "consent_not_configured",
        &format!(
            "this Gateway has no consent store, so it keeps no {} journal and nothing the \
             switch could govern: set GATEWAY_NATS_URL (and GATEWAY_OWNER)",
            kind.journal()
        ),
    )
}

/// A journal that could not be read, or a decision that could not be
/// written. `what` is the verb, so the sentence says which half failed.
fn switch_store_unavailable(kind: Kind, what: &str) -> Response {
    let journal = kind.journal();
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store_unavailable",
        &match what {
            "record" => {
                format!("the {journal} decision could not be recorded; nothing was changed")
            }
            _ => format!("the {journal} journal could not be read"),
        },
    )
}

fn api_error(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({ "error": code, "detail": detail }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{parse_model_update, CredentialSource};

    fn settings(name: &str, credential_file: Option<&std::path::Path>) -> Settings {
        let directory = std::env::temp_dir().join(format!(
            "twalk-gateway-settings-http-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        Settings::open(&directory, credential_file, std::time::SystemTime::now)
            .expect("the settings store opens")
    }

    fn configure(settings: &Settings) {
        let update = parse_model_update(&json!({
            "base_url": "http://127.0.0.1:4000/v1",
            "model": "qwen",
            "credential": "sk-a-key-set-from-the-browser",
        }))
        .expect("the reference deployment's configuration parses");
        settings.set_model(&update).expect("the model is written");
    }

    /// The write-only rule, asserted the only way it can be: against every
    /// byte of the document.
    #[test]
    fn no_browser_facing_document_carries_the_credential() {
        let settings = settings("write-only", None);
        configure(&settings);
        let document = model_json(&settings).expect("the document renders");
        let rendered = document.to_string();
        assert!(
            !rendered.contains("sk-a-key-set-from-the-browser"),
            "the credential is write-only and must not appear anywhere in a browser's read: \
             {rendered}"
        );
        assert_eq!(document["credential"]["configured"], json!(true));
        assert_eq!(document["credential"]["source"], json!("companion"));
        assert_eq!(
            document["credential"]["hint"],
            json!("wser"),
            "the last four characters, which is enough to recognise a key and not enough to \
             use one"
        );
        assert_eq!(
            document["personas"],
            json!({}),
            "the per-persona override exists and is empty in v0.1, so adding the first one \
             later is not a change of shape"
        );
        assert_eq!(document["base_url"], json!("http://127.0.0.1:4000/v1"));
        assert_eq!(document["model"], json!("qwen"));
    }

    #[test]
    fn an_unconfigured_deployment_says_so_rather_than_answering_an_empty_model() {
        let settings = settings("unconfigured", None);
        let document = model_json(&settings).expect("the document renders");
        assert_eq!(document["configured"], json!(false));
        assert_eq!(document["base_url"], Value::Null);
        assert_eq!(document["model"], Value::Null);
        assert_eq!(document["credential"]["configured"], json!(false));
        assert_eq!(document["credential"]["source"], Value::Null);

        let runtime = runtime_json(&settings).expect("the runtime document renders");
        assert_eq!(
            runtime["llm"],
            Value::Null,
            "the runtime is told there is no model, which is a different fact from a Gateway \
             it cannot reach and from one that refused its token (ADR 0015)"
        );
        assert_eq!(runtime["language"], Value::Null);
    }

    #[test]
    fn the_runtime_is_handed_the_credential_in_force_and_its_source() {
        let file = std::env::temp_dir().join(format!(
            "twalk-gateway-http-litellm-{}.key",
            std::process::id()
        ));
        std::fs::write(&file, "sk-the-operators-key\n").expect("the test can write a file");
        let settings = settings("runtime", Some(&file));
        configure(&settings);
        settings
            .set_language(Some(Language::Fr))
            .expect("the preference is written");

        let runtime = runtime_json(&settings).expect("the runtime document renders");
        assert_eq!(
            runtime["llm"]["api_key"],
            json!("sk-the-operators-key"),
            "the file wins over the browser's value (ADR 0015)"
        );
        assert_eq!(
            runtime["llm"]["credential_source"],
            json!(CredentialSource::File.as_str())
        );
        assert_eq!(
            runtime["llm"]["base_url"],
            json!("http://127.0.0.1:4000/v1")
        );
        assert_eq!(runtime["llm"]["model"], json!("qwen"));
        assert_eq!(
            runtime["llm"]["params"],
            Value::Null,
            "an empty passthrough is the expected shape with a proxy in front"
        );
        assert_eq!(
            runtime["language"],
            json!("fr"),
            "ADR 0016's fallback finally has a value to fall back to (#164)"
        );

        // And the browser still learns which source won, without learning
        // either credential.
        let document = model_json(&settings).expect("the document renders");
        assert_eq!(document["credential"]["source"], json!("file"));
        assert_eq!(
            document["credential"]["companion_credential_stored"],
            json!(true),
            "both exist, and the interface has to be able to explain why one is ignored"
        );
        assert!(!model_json(&settings)
            .expect("the document renders")
            .to_string()
            .contains("sk-the-operators-key"));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn the_disclosure_document_says_on_with_nobody_deciding_and_off_with_who_and_when() {
        use crate::switch::State as SwitchState;
        assert_eq!(
            switch_json(&crate::disclosure::DEFAULT),
            json!({ "enabled": true, "since": null, "actor": null, "reason": null }),
            "the default is on, and the nulls say nobody decided rather than that nothing is known"
        );
        assert_eq!(
            switch_json(&SwitchState {
                enabled: false,
                since: Some("2026-09-20T10:04:37.000Z".to_owned()),
                actor: Some("@michel:example.com".to_owned()),
                reason: None,
            }),
            json!({
                "enabled": false,
                "since": "2026-09-20T10:04:37.000Z",
                "actor": "@michel:example.com",
                "reason": null
            })
        );
    }

    #[test]
    fn the_language_document_lists_the_five_the_companion_ships() {
        assert_eq!(
            language_json(None),
            json!({ "language": null, "available": ["en", "fr", "it", "es", "de"] })
        );
        assert_eq!(language_json(Some(Language::It))["language"], json!("it"));
    }

    /// The probe's answers, as a client reads them: four codes, and only the
    /// two that had an answer carry the endpoint's status.
    #[test]
    fn each_probe_outcome_is_its_own_answer() {
        let model = ModelConfiguration {
            base_url: "http://127.0.0.1:4000/v1".to_owned(),
            model: "qwen".to_owned(),
            params: None,
            updated_at: "2026-09-18T16:24:21.000Z".to_owned(),
        };
        let ok = probe_response(
            &model,
            &ProbeOutcome::Ok {
                endpoint_status: 200,
                model: Some("qwen".to_owned()),
            },
        );
        assert_eq!(ok.status(), StatusCode::OK);

        for (outcome, code) in [
            (
                ProbeOutcome::Unreachable {
                    detail: "connection refused".to_owned(),
                },
                "endpoint_unreachable",
            ),
            (
                ProbeOutcome::Refused {
                    endpoint_status: 401,
                    detail: "bad key".to_owned(),
                },
                "endpoint_refused",
            ),
            (
                ProbeOutcome::NotCompatible {
                    endpoint_status: 200,
                    detail: "an index page".to_owned(),
                },
                "endpoint_not_compatible",
            ),
        ] {
            let response = probe_response(&model, &outcome);
            assert_eq!(
                response.status(),
                StatusCode::BAD_GATEWAY,
                "a probe that did not get a completion is not a success"
            );
            assert_eq!(outcome.code(), code);
        }
    }
}
