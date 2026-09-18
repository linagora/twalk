//! The settings' HTTP surface: the model configuration, the user's native
//! language, the probe that says why an endpoint is not working, and the one
//! read the Hermes runtime makes (ticket #98).
//!
//! # Who is calling
//!
//! Six of the seven operations are the owner's browser and are behind #52's
//! guard like everything else under `/api/`. The seventh,
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
