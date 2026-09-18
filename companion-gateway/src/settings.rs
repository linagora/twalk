//! The model configuration and the user's native language: held by the
//! Gateway, set from the Companion, read by the Hermes runtime (ticket #98).
//!
//! # Why this lives here at all
//!
//! Twalk ships no default LLM and nothing is ever sent to a model the
//! operator did not name (ADR 0015). Somebody has to hold the name, and the
//! choice was made deliberately: the Gateway, because the operator must be
//! able to change a model **without shell access**, and because the runtime
//! injects the configuration into each persona rather than letting a persona
//! fetch it — the credential that reads this configuration is the same one
//! that opens the consent snapshot, the list of every contact, and a
//! third-party persona takes the same path as a first-party one (ADR 0008).
//!
//! The user's native language has exactly the same shape and the same
//! reason (ADR 0016): a persona runs in a container and cannot read
//! `navigator.language`, so the preference is stored and injected. Without
//! it ADR 0016 is half-built — the "answer in the incoming message's
//! language" half works from the persona's prompt and the "fall back to the
//! user's language" half has no value to fall back to, which is
//! [#164](https://github.com/linagora/twalk/issues/164): a French speaker
//! writing `test` got an English draft. This module is where that value
//! gets a home.
//!
//! # The shape the reference deployment actually has
//!
//! An OpenAI-compatible **LiteLLM proxy in front of the model**:
//!
//! ```text
//! endpoint : http://127.0.0.1:4000/v1
//! model    : qwen
//! key      : a file on the host
//! ```
//!
//! Two consequences are built in rather than left to taste.
//!
//! **The credential is a file and the model comes from the browser.** A
//! credential the operator supplied as a file wins over one set from the
//! Companion (ADR 0015), and that is not a theoretical precedence: it is how
//! the reference deployment runs, so it is the path that has to work first.
//! [`Settings::credential_in_force`] is where the rule lives, and every read
//! of the API says which source won, because "why is my key not being used?"
//! must be answerable from the interface.
//!
//! **The provider passthrough is the escape hatch, not the norm.** It stays —
//! an operator without a proxy needs it, and OVH's AI Endpoints, the first
//! endpoint tried in practice, rejects fields OpenAI clients send by default
//! (ADR 0015). But with a proxy in front, every provider quirk
//! (`drop_params`, `reasoning_effort`, the provider's real model name) lives
//! in the operator's proxy configuration where it belongs, and Twalk sees a
//! plain endpoint serving a model called `qwen`. An empty passthrough is the
//! healthy shape, and nothing here may depend on it being set.
//!
//! # The credential is write-only
//!
//! It goes in and never comes out. No read of the browser-facing API returns
//! it — only whether one is configured, which source is in force, and its
//! last four characters, which is enough for a human to recognise the key
//! they pasted and not enough for anyone else to use it. The one read that
//! returns the credential itself is [`RuntimeSettings`], behind the service
//! token, whose caller is the Hermes runtime.
//!
//! It also lands in a SQLite file that **nothing encrypts at rest**
//! ([#14](https://github.com/linagora/twalk/issues/14)). It is an outbound
//! secret that bills money and sees message content, so it is in
//! `docs/architecture/security-model.md`'s credential table and in its
//! residual risks, rather than only in this comment.
//!
//! # The per-persona override
//!
//! The served documents carry `personas`, and in v0.1 it is always `{}`.
//! Shipping a working override with one persona would ship an unexercised
//! path; adding the member later would change the shape of an endpoint
//! clients had already generated against. So the member exists and is empty,
//! which is the only one of the three options that costs nothing later.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};

/// The store's file inside the Gateway's state directory, beside
/// `sessions.db` and `consent.sqlite3`.
const DATABASE_FILE: &str = "settings.sqlite3";

/// The interface languages the Companion ships, in the order the Companion
/// offers them (`companion/src/lib/i18n/`, `CONTRIBUTING.md`). English and
/// French are reviewed by native speakers; the other three are not, which is
/// recorded in each catalogue's header and not shown to a user who could do
/// nothing about it (ADR 0016).
pub const LANGUAGES: [Language; 5] = [
    Language::En,
    Language::Fr,
    Language::It,
    Language::Es,
    Language::De,
];

/// The user's native language: the one the interface is drawn in, and the
/// one a persona falls back to when it cannot tell what language the message
/// it is answering was written in (ADR 0016).
///
/// It never governs the text sent to a contact. That is the whole point of
/// ADR 0016 and the mistake an i18n habit produces: a French user answering
/// an English-speaking contact in French has been handed something they
/// cannot send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    En,
    Fr,
    It,
    Es,
    De,
}

impl Language {
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::En => "en",
            Language::Fr => "fr",
            Language::It => "it",
            Language::Es => "es",
            Language::De => "de",
        }
    }

    /// The tag as the Companion writes it, exactly: `fr`, never `fr-FR` and
    /// never `FR`. A closed list is the point — an unrecognised tag is
    /// refused with the five named, rather than stored and discovered later
    /// by a persona that has no catalogue for it.
    pub fn parse(value: &str) -> Option<Self> {
        LANGUAGES
            .into_iter()
            .find(|language| language.as_str() == value)
    }

    /// The five, as the API lists them.
    pub fn available() -> Vec<&'static str> {
        LANGUAGES
            .into_iter()
            .map(|language| language.as_str())
            .collect()
    }
}

/// Where the credential the personas call the endpoint with came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialSource {
    /// A file on the host, named by `GATEWAY_LLM_API_KEY_FILE`. It **wins**
    /// (ADR 0015): a production stack locks the credential down while the
    /// model name still comes from the browser.
    File,
    /// Set from the Companion, through `PUT /api/settings/model`.
    Companion,
}

impl CredentialSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            CredentialSource::File => "file",
            CredentialSource::Companion => "companion",
        }
    }
}

/// The credential actually in force, and which of the two sources supplied
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub source: CredentialSource,
    pub value: String,
}

impl Credential {
    /// The last four characters, for an interface that has to show *which*
    /// key is configured without showing the key.
    ///
    /// `None` for anything under eight characters: four of a six-character
    /// secret is most of it, and a hint that leaks is worse than no hint.
    pub fn hint(&self) -> Option<String> {
        let characters: Vec<char> = self.value.chars().collect();
        (characters.len() >= 8).then(|| characters[characters.len() - 4..].iter().collect())
    }
}

/// The model configuration as it was stored: the endpoint, the model name
/// the endpoint knows it by, and the operator's provider parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelConfiguration {
    /// An OpenAI-compatible chat-completions **base** URL, e.g.
    /// `http://127.0.0.1:4000/v1`. Normalised without a trailing slash, so
    /// the runtime and the probe build the same `…/chat/completions`.
    pub base_url: String,
    /// The model name **the configured endpoint** knows, not a family name:
    /// `qwen` on the reference deployment, because that is what its LiteLLM
    /// serves it as.
    pub model: String,
    /// The operator's provider parameters, merged into every request
    /// untouched (ADR 0015). Always a JSON object when present.
    ///
    /// Routinely `None`, and that is the healthy shape with a proxy in
    /// front: the quirks live in the proxy's configuration.
    pub params: Option<Value>,
    pub updated_at: String,
}

/// What a write to the model configuration asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelUpdate {
    pub base_url: String,
    pub model: String,
    pub params: Option<Value>,
    pub credential: CredentialUpdate,
}

/// What a write says about the credential — three cases, because the
/// credential is write-only and a client therefore *cannot* round-trip it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialUpdate {
    /// The member was absent: whatever is stored stays. This is the ordinary
    /// case for a settings screen changing a model name, which has never
    /// been shown the credential and has nothing to send back.
    Keep,
    /// The member was `null`: forget the credential set from the browser. It
    /// does not touch the operator's file, which is not the Companion's to
    /// remove.
    Clear,
    Set(String),
}

/// Why a write was refused, with the stable code a client branches on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    /// A member the shape does not have. Refused rather than ignored: a
    /// client that sends `credentials` for `credential` would otherwise
    /// believe it had set a key it had not.
    UnknownMember(String),
    MissingMember(&'static str),
    BaseUrl(String),
    Model(String),
    Params(String),
    Credential(String),
    Language(String),
}

impl Invalid {
    pub fn code(&self) -> &'static str {
        match self {
            Invalid::UnknownMember(_) | Invalid::MissingMember(_) => "malformed_request",
            Invalid::BaseUrl(_) => "invalid_base_url",
            Invalid::Model(_) => "invalid_model",
            Invalid::Params(_) => "invalid_params",
            Invalid::Credential(_) => "invalid_credential",
            Invalid::Language(_) => "unsupported_language",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Invalid::UnknownMember(name) => format!(
                "the request body has a member this endpoint does not have: {name:?}. It is \
                 refused rather than ignored, because a misspelled credential member would \
                 otherwise look like a credential that was set"
            ),
            Invalid::MissingMember(name) => {
                format!("the request body is missing the required member {name:?}")
            }
            Invalid::BaseUrl(detail) => detail.clone(),
            Invalid::Model(detail) => detail.clone(),
            Invalid::Params(detail) => detail.clone(),
            Invalid::Credential(detail) => detail.clone(),
            Invalid::Language(detail) => detail.clone(),
        }
    }
}

/// Parses the body of `PUT /api/settings/model`.
///
/// ```json
/// {
///   "base_url": "http://127.0.0.1:4000/v1",
///   "model": "qwen",
///   "params": { "max_tokens": 2000 },
///   "credential": "sk-…"
/// }
/// ```
///
/// `params` absent or `null` means no passthrough. `credential` **absent**
/// keeps what is stored and `credential: null` clears it — the distinction
/// exists because the credential is write-only, so a client editing the
/// model name has nothing to send back.
pub fn parse_model_update(body: &Value) -> Result<ModelUpdate, Invalid> {
    let object = body
        .as_object()
        .ok_or_else(|| Invalid::MissingMember("base_url"))?;
    for name in object.keys() {
        if !matches!(
            name.as_str(),
            "base_url" | "model" | "params" | "credential"
        ) {
            return Err(Invalid::UnknownMember(name.clone()));
        }
    }

    let base_url = object
        .get("base_url")
        .and_then(Value::as_str)
        .ok_or(Invalid::MissingMember("base_url"))?;
    let base_url = normalise_base_url(base_url).ok_or_else(|| {
        Invalid::BaseUrl(format!(
            "base_url must be an absolute http or https URL with a host — the chat-completions \
             *base*, e.g. http://127.0.0.1:4000/v1 — got {base_url:?}"
        ))
    })?;

    let model = object
        .get("model")
        .and_then(Value::as_str)
        .ok_or(Invalid::MissingMember("model"))?
        .trim()
        .to_owned();
    if model.is_empty() {
        return Err(Invalid::Model(
            "model must be the name the configured endpoint knows the model by (`qwen` on a \
             LiteLLM proxy that serves one), and it cannot be empty: there is no default model \
             (ADR 0015)"
                .to_owned(),
        ));
    }

    let params = match object.get("params") {
        None | Some(Value::Null) => None,
        Some(Value::Object(params)) if params.is_empty() => None,
        Some(Value::Object(params)) => Some(Value::Object(params.clone())),
        Some(other) => {
            return Err(Invalid::Params(format!(
                "params is a JSON object of provider parameters merged into every request \
                 untouched, and a member set to null removes a field the request would \
                 otherwise carry (ADR 0015); got {other}"
            )))
        }
    };

    let credential = match object.get("credential") {
        None => CredentialUpdate::Keep,
        Some(Value::Null) => CredentialUpdate::Clear,
        Some(Value::String(credential)) => {
            let credential = credential.trim();
            if credential.is_empty() {
                return Err(Invalid::Credential(
                    "credential is the endpoint's own bearer token: send null to forget the one \
                     set from this browser, never an empty string"
                        .to_owned(),
                ));
            }
            CredentialUpdate::Set(credential.to_owned())
        }
        Some(other) => {
            return Err(Invalid::Credential(format!(
                "credential is a string, or null to forget the one set from this browser; got \
                 {other}"
            )))
        }
    };

    Ok(ModelUpdate {
        base_url,
        model,
        params,
        credential,
    })
}

/// Parses the body of `PUT /api/settings/language`: `{"language": "fr"}`, or
/// `{"language": null}` to go back to having no preference.
pub fn parse_language_update(body: &Value) -> Result<Option<Language>, Invalid> {
    let object = body.as_object().ok_or(Invalid::MissingMember("language"))?;
    for name in object.keys() {
        if name != "language" {
            return Err(Invalid::UnknownMember(name.clone()));
        }
    }
    match object.get("language") {
        Some(Value::Null) => Ok(None),
        Some(Value::String(tag)) => Language::parse(tag).map(Some).ok_or_else(|| {
            Invalid::Language(format!(
                "language must be one of {} — the interface languages the Companion ships — or \
                 null for no preference; got {tag:?}",
                Language::available().join(", ")
            ))
        }),
        Some(other) => Err(Invalid::Language(format!(
            "language is one of {} or null; got {other}",
            Language::available().join(", ")
        ))),
        None => Err(Invalid::MissingMember("language")),
    }
}

/// An OpenAI-compatible base URL, normalised: trimmed, without its trailing
/// slash, and only if it is absolute with a host.
///
/// `None` for anything else. There is no `url` crate in this graph and one
/// is not worth adding for this: what has to be refused is a relative path, a
/// scheme the personas' client cannot speak, and an authority-less URL that
/// would silently become a request to nowhere.
///
/// ```
/// # use twalk_companion_gateway::settings::normalise_base_url;
/// assert_eq!(normalise_base_url(" http://127.0.0.1:4000/v1/ "), Some("http://127.0.0.1:4000/v1".to_owned()));
/// assert_eq!(normalise_base_url("https://oai.endpoints.kepler.ai.cloud.ovh.net/v1"), Some("https://oai.endpoints.kepler.ai.cloud.ovh.net/v1".to_owned()));
/// assert_eq!(normalise_base_url("127.0.0.1:4000/v1"), None);
/// assert_eq!(normalise_base_url("ftp://endpoint/v1"), None);
/// assert_eq!(normalise_base_url("http:///v1"), None);
/// ```
pub fn normalise_base_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let rest = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    let normalised = trimmed.trim_end_matches('/');
    // `http://host/` trimmed to `http://host` is still a base URL; a URL
    // that was nothing *but* slashes is not.
    (!normalised.is_empty()).then(|| normalised.to_owned())
}

/// The chat-completions endpoint of a base URL, built exactly as the SDK
/// builds it (`sdk/python/twalk_sdk/config.py`), so the probe exercises the
/// URL a persona will actually call and not a near-miss.
pub fn chat_completions_url(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

/// The body the probe sends: one completion of one token, with the
/// operator's provider parameters merged in **last** and a member set to
/// `null` removing a field — the SDK's own merge, so what the probe proves
/// is what a persona will do.
///
/// One token rather than none, and no assertion about the text: what is
/// being tested is that the endpoint is there, that it accepts this
/// credential and that it knows this model name. A reasoning model spends
/// the budget thinking and answers with no content
/// ([#162](https://github.com/linagora/twalk/issues/162)), and that is still
/// a reachable, willing endpoint.
pub fn probe_payload(model: &str, params: Option<&Value>) -> Value {
    let mut payload = Map::new();
    payload.insert("model".to_owned(), Value::from(model));
    payload.insert(
        "messages".to_owned(),
        json!([{ "role": "user", "content": "ping" }]),
    );
    payload.insert("max_tokens".to_owned(), Value::from(1));
    payload.insert("stream".to_owned(), Value::from(false));
    if let Some(Value::Object(params)) = params {
        for (name, value) in params {
            if value.is_null() {
                payload.remove(name);
            } else {
                payload.insert(name.clone(), value.clone());
            }
        }
    }
    Value::Object(payload)
}

/// What one probe of the configured endpoint found. Four causes, four
/// answers — the distinction this project got wrong seven times in two days
/// ([#116](https://github.com/linagora/twalk/issues/116),
/// [#141](https://github.com/linagora/twalk/issues/141)), and the fifth
/// cause, "no endpoint configured at all", is a different answer again
/// (`model_not_configured`) rather than a silent version of one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The endpoint answered a chat completion. The credential works and the
    /// model name is one it knows.
    Ok {
        endpoint_status: u16,
        /// The model the endpoint echoed back, when it did.
        model: Option<String>,
    },
    /// Nothing answered: DNS, connection refused, TLS, or the probe's own
    /// deadline. **Not** the endpoint's opinion of the request — an endpoint
    /// that is not there and one that says no are different problems with
    /// different fixes, and the reference deployment has both (a proxy on
    /// the host's loopback that a container of its own network namespace
    /// dials as itself is the first).
    Unreachable { detail: String },
    /// The endpoint answered, and refused: a wrong credential, a model name
    /// it does not serve, a rate limit, its own outage. `endpoint_status` is
    /// its status, unedited.
    Refused {
        endpoint_status: u16,
        detail: String,
    },
    /// The endpoint answered a success that is not an OpenAI chat
    /// completion. Almost always the wrong port or the wrong path — a web
    /// server's index page answers `200` very convincingly.
    NotCompatible {
        endpoint_status: u16,
        detail: String,
    },
}

impl ProbeOutcome {
    /// The stable code a client branches on. `ok` is not an error code and
    /// is answered with `200`; the other three are the `error` member of the
    /// Gateway's one error document.
    pub fn code(&self) -> &'static str {
        match self {
            ProbeOutcome::Ok { .. } => "ok",
            ProbeOutcome::Unreachable { .. } => "endpoint_unreachable",
            ProbeOutcome::Refused { .. } => "endpoint_refused",
            ProbeOutcome::NotCompatible { .. } => "endpoint_not_compatible",
        }
    }

    /// Reads an answer the endpoint gave: its status, and its body.
    ///
    /// A success carrying a `choices` array is a chat completion; a success
    /// carrying anything else is an endpoint speaking another protocol.
    pub fn from_answer(status: u16, body: &str) -> Self {
        if !(200..300).contains(&status) {
            return ProbeOutcome::Refused {
                endpoint_status: status,
                detail: format!(
                    "the chat-completions endpoint answered HTTP {status}: {}",
                    excerpt(body)
                ),
            };
        }
        let parsed: Option<Value> = serde_json::from_str(body).ok();
        let completion = parsed.as_ref().and_then(|body| {
            body.get("choices")
                .and_then(Value::as_array)
                .map(|_| body.clone())
        });
        match completion {
            Some(body) => ProbeOutcome::Ok {
                endpoint_status: status,
                model: body.get("model").and_then(Value::as_str).map(str::to_owned),
            },
            None => ProbeOutcome::NotCompatible {
                endpoint_status: status,
                detail: format!(
                    "the endpoint answered HTTP {status} with something that is not an \
                     OpenAI chat completion (no choices array): {}",
                    excerpt(body)
                ),
            },
        }
    }
}

/// The first 512 characters of an endpoint's answer, for an operator's logs.
/// It is the endpoint's own error message, which is the most useful clue
/// there is, and it is never message content: the probe sends `ping`.
fn excerpt(body: &str) -> String {
    let excerpt: String = body.chars().take(512).collect();
    if excerpt.trim().is_empty() {
        "(an empty body)".to_owned()
    } else {
        excerpt
    }
}

/// The operator's credential file, as it was read at startup.
#[derive(Debug, Clone)]
struct FileCredential {
    path: String,
    value: String,
}

/// The Gateway's settings store: the model configuration and the language
/// preference, plus the operator's credential file if there is one.
///
/// One connection behind a mutex, as the consent store is: these are two
/// single-row tables written by one human from one browser.
pub struct Settings {
    store: Mutex<Connection>,
    /// Read once, at startup, exactly as the Hermes runtime reads its own
    /// (`hermes/src/config.rs`). A rotated file therefore takes effect at
    /// the next restart — which is the same rule on both sides of the
    /// hand-off, and the alternative (re-reading per request) would make a
    /// file deleted under a running Gateway a new, silent failure mode.
    file_credential: Option<FileCredential>,
    path: PathBuf,
    now: fn() -> std::time::SystemTime,
}

impl Settings {
    /// Opens (creating it if needed) the settings store in `state_dir` and
    /// reads the operator's credential file, if one is named.
    ///
    /// An empty credential file is an **error**, not an absence: an operator
    /// who named a file meant to supply a credential, and a Gateway that
    /// quietly fell back to the browser's value would be the precedence rule
    /// failing silently in the direction the rule exists to prevent. The
    /// same refusal the runtime makes (`hermes::config::resolve_api_key`).
    pub fn open(
        state_dir: &Path,
        credential_file: Option<&Path>,
        now: fn() -> std::time::SystemTime,
    ) -> Result<Self> {
        std::fs::create_dir_all(state_dir).with_context(|| {
            format!(
                "failed to create the gateway state directory {}",
                state_dir.display()
            )
        })?;
        let path = state_dir.join(DATABASE_FILE);
        let store = Connection::open(&path)
            .with_context(|| format!("failed to open the settings store {}", path.display()))?;
        restrict_to_owner(&path, 0o600);
        migrate(&store).with_context(|| format!("failed to migrate {}", path.display()))?;
        let file_credential = match credential_file {
            Some(file) => Some(read_credential_file(file)?),
            None => None,
        };
        Ok(Self {
            store: Mutex::new(store),
            file_credential,
            path,
            now,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The path `GATEWAY_LLM_API_KEY_FILE` names, when it names one. The
    /// browser is shown it, because "the file wins" is useless to a human
    /// who cannot see *which* file won.
    pub fn credential_file_path(&self) -> Option<&str> {
        self.file_credential
            .as_ref()
            .map(|credential| credential.path.as_str())
    }

    /// The model configuration the operator set, or `None` when none was —
    /// which is a state, not a failure, and stays distinguishable from
    /// every other one (ADR 0015).
    pub fn model(&self) -> Result<Option<ModelConfiguration>> {
        let store = self.lock();
        store
            .query_row(
                "SELECT base_url, model, params, updated_at FROM model_configuration WHERE id = 1",
                [],
                |row| {
                    let params: Option<String> = row.get(2)?;
                    Ok(ModelConfiguration {
                        base_url: row.get(0)?,
                        model: row.get(1)?,
                        params: params
                            .as_deref()
                            .and_then(|params| serde_json::from_str(params).ok()),
                        updated_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .context("failed to read the model configuration")
    }

    /// Whether a credential was set from the Companion — true even when the
    /// operator's file is the one in force, because that combination is
    /// exactly what a settings screen has to explain.
    pub fn companion_credential_stored(&self) -> Result<bool> {
        Ok(self.companion_credential()?.is_some())
    }

    fn companion_credential(&self) -> Result<Option<String>> {
        let store = self.lock();
        store
            .query_row(
                "SELECT credential FROM model_configuration WHERE id = 1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
            .context("failed to read the stored endpoint credential")
    }

    /// **The file wins** (ADR 0015).
    ///
    /// It is the operator's, on the host, and the other one is whatever the
    /// Companion last wrote — so a production stack can lock the credential
    /// down while a developer stays in the browser. On the reference
    /// deployment that is the ordinary combination rather than the exotic
    /// one: the model name comes from the browser and the key from a file.
    pub fn credential_in_force(&self) -> Result<Option<Credential>> {
        if let Some(file) = &self.file_credential {
            return Ok(Some(Credential {
                source: CredentialSource::File,
                value: file.value.clone(),
            }));
        }
        Ok(self.companion_credential()?.map(|value| Credential {
            source: CredentialSource::Companion,
            value,
        }))
    }

    /// Writes the model configuration, replacing it whole.
    ///
    /// The credential is handled inside the same transaction as the rest:
    /// [`CredentialUpdate::Keep`] reads the stored one and writes it back,
    /// so a screen that only renamed the model cannot lose the key it was
    /// never shown.
    pub fn set_model(&self, update: &ModelUpdate) -> Result<ModelConfiguration> {
        let updated_at = crate::consent::rfc3339_millis((self.now)());
        let params = update.params.as_ref().map(|params| params.to_string());
        let mut store = self.lock();
        let transaction = store
            .transaction()
            .context("failed to open a settings transaction")?;
        let stored: Option<String> = transaction
            .query_row(
                "SELECT credential FROM model_configuration WHERE id = 1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .context("failed to read the stored endpoint credential")?
            .flatten();
        let credential = match &update.credential {
            CredentialUpdate::Keep => stored,
            CredentialUpdate::Clear => None,
            CredentialUpdate::Set(credential) => Some(credential.clone()),
        };
        transaction
            .execute(
                "INSERT INTO model_configuration (id, base_url, model, params, credential, updated_at)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (id) DO UPDATE SET
                   base_url = excluded.base_url,
                   model = excluded.model,
                   params = excluded.params,
                   credential = excluded.credential,
                   updated_at = excluded.updated_at",
                rusqlite::params![
                    update.base_url,
                    update.model,
                    params,
                    credential,
                    updated_at
                ],
            )
            .context("failed to write the model configuration")?;
        transaction
            .commit()
            .context("failed to commit the model configuration")?;
        Ok(ModelConfiguration {
            base_url: update.base_url.clone(),
            model: update.model.clone(),
            params: update.params.clone(),
            updated_at,
        })
    }

    /// Forgets the model configuration and the credential set from the
    /// browser. `false` when there was nothing to forget.
    ///
    /// It does not touch the operator's file: that is not the Companion's to
    /// remove, and a deployment can be left with a credential and no model,
    /// which is a perfectly ordinary state on the way to naming another one.
    pub fn clear_model(&self) -> Result<bool> {
        let store = self.lock();
        let removed = store
            .execute("DELETE FROM model_configuration WHERE id = 1", [])
            .context("failed to clear the model configuration")?;
        Ok(removed > 0)
    }

    pub fn language(&self) -> Result<Option<Language>> {
        let store = self.lock();
        let stored: Option<String> = store
            .query_row(
                "SELECT language FROM language_preference WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .context("failed to read the language preference")?;
        Ok(stored.as_deref().and_then(Language::parse))
    }

    /// Sets, or with `None` forgets, the user's native language.
    ///
    /// Forgetting is a real state and not the same as English: a persona
    /// whose fallback is unset writes in the language of the message it is
    /// answering and nothing else, which is honest. Defaulting silently to
    /// English is what #164 found in production.
    pub fn set_language(&self, language: Option<Language>) -> Result<()> {
        let store = self.lock();
        match language {
            Some(language) => {
                store
                    .execute(
                        "INSERT INTO language_preference (id, language, updated_at)
                         VALUES (1, ?1, ?2)
                         ON CONFLICT (id) DO UPDATE SET
                           language = excluded.language,
                           updated_at = excluded.updated_at",
                        rusqlite::params![
                            language.as_str(),
                            crate::consent::rfc3339_millis((self.now)())
                        ],
                    )
                    .context("failed to write the language preference")?;
            }
            None => {
                store
                    .execute("DELETE FROM language_preference WHERE id = 1", [])
                    .context("failed to clear the language preference")?;
            }
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.store
            .lock()
            .expect("the settings store mutex is never poisoned")
    }
}

/// Reads the operator's credential file.
///
/// Trailing newlines are trimmed, because every way of writing a file adds
/// one and none of them is part of the key — the same trimming the runtime
/// does, so the two sides of the hand-off cannot disagree about the bytes.
fn read_credential_file(file: &Path) -> Result<FileCredential> {
    let contents = std::fs::read_to_string(file).with_context(|| {
        format!(
            "failed to read GATEWAY_LLM_API_KEY_FILE at {}: it names the credential the \
             personas call the endpoint with, and a Gateway that could not read it would \
             hand over the browser's value instead — which is the precedence rule of \
             ADR 0015 failing silently, in the direction it exists to prevent",
            file.display()
        )
    })?;
    let value = contents.trim_end_matches(['\n', '\r']).to_owned();
    anyhow::ensure!(
        !value.is_empty(),
        "GATEWAY_LLM_API_KEY_FILE at {} is empty: it names the credential the personas call \
         the endpoint with, and an empty file is not one",
        file.display()
    );
    Ok(FileCredential {
        path: file.display().to_string(),
        value,
    })
}

/// The schema, applied on open and versioned through SQLite's own
/// `user_version`, as the session store's is.
///
/// Two single-row tables, each with `CHECK (id = 1)`: one deployment serves
/// one owner (ADR 0011), so "there is one model configuration" is a
/// constraint of the schema rather than a habit of the code that writes it.
fn migrate(store: &Connection) -> Result<()> {
    store.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    let version: u32 = store.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 1 {
        // `credential` is the one column in any of the Gateway's stores that
        // holds an outbound secret in cleartext. Nothing encrypts this file
        // at rest (#14), which is why it is in the security model's
        // credential table and its residual risks rather than only here, and
        // why the file is narrowed to its owner on open.
        store.execute_batch(
            "BEGIN;
             CREATE TABLE model_configuration (
               id          INTEGER PRIMARY KEY CHECK (id = 1),
               base_url    TEXT NOT NULL,
               model       TEXT NOT NULL,
               params      TEXT,
               credential  TEXT,
               updated_at  TEXT NOT NULL
             );
             CREATE TABLE language_preference (
               id          INTEGER PRIMARY KEY CHECK (id = 1),
               language    TEXT NOT NULL CHECK (language IN ('en', 'fr', 'it', 'es', 'de')),
               updated_at  TEXT NOT NULL
             );
             PRAGMA user_version = 1;
             COMMIT;",
        )?;
    }
    Ok(())
}

/// Narrows a path to its owner. Best effort and deliberately quiet on
/// failure: an exotic filesystem that refuses the mode is not a reason to
/// refuse to start, and the operator's volume permissions are the real
/// control (`docs/architecture/security-model.md`).
fn restrict_to_owner(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at() -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_789_000_000_123)
    }

    fn settings(name: &str, credential_file: Option<&Path>) -> Settings {
        let directory = std::env::temp_dir().join(format!(
            "twalk-gateway-settings-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        Settings::open(&directory, credential_file, at).expect("the settings store opens")
    }

    fn update() -> ModelUpdate {
        parse_model_update(&json!({
            "base_url": "http://127.0.0.1:4000/v1",
            "model": "qwen",
            "credential": "sk-set-from-the-browser",
        }))
        .expect("the reference deployment's own configuration parses")
    }

    #[test]
    fn the_reference_deployments_configuration_is_the_one_that_parses() {
        let parsed = update();
        assert_eq!(parsed.base_url, "http://127.0.0.1:4000/v1");
        assert_eq!(parsed.model, "qwen");
        assert_eq!(
            parsed.params, None,
            "an empty passthrough is the healthy shape with a proxy in front (ADR 0015)"
        );
        assert_eq!(
            parsed.credential,
            CredentialUpdate::Set("sk-set-from-the-browser".to_owned())
        );
    }

    #[test]
    fn a_write_says_which_part_of_the_configuration_is_wrong() {
        for (body, code) in [
            (json!({ "model": "qwen" }), "malformed_request"),
            (json!({ "base_url": "http://x/v1" }), "malformed_request"),
            (
                json!({ "base_url": "http://x/v1", "model": "q", "endpoint": "x" }),
                "malformed_request",
            ),
            (
                json!({ "base_url": "127.0.0.1:4000/v1", "model": "qwen" }),
                "invalid_base_url",
            ),
            (
                json!({ "base_url": "http://x/v1", "model": "   " }),
                "invalid_model",
            ),
            (
                json!({ "base_url": "http://x/v1", "model": "q", "params": [1, 2] }),
                "invalid_params",
            ),
            (
                json!({ "base_url": "http://x/v1", "model": "q", "credential": "" }),
                "invalid_credential",
            ),
        ] {
            let refusal = parse_model_update(&body)
                .expect_err("a malformed model configuration must not parse");
            assert_eq!(refusal.code(), code, "for {body}: {}", refusal.message());
        }
    }

    /// The acceptance criterion the reference deployment settles: the model
    /// name comes from the browser and the key from a file, and the file
    /// wins. Not a theoretical precedence — the path that has to work first.
    #[test]
    fn a_credential_in_a_file_beats_one_set_from_the_browser() {
        let file = std::env::temp_dir().join(format!(
            "twalk-gateway-litellm-master-{}.key",
            std::process::id()
        ));
        std::fs::write(&file, "sk-the-operators-key\n").expect("the test can write a file");
        let settings = settings("file-wins", Some(&file));
        settings.set_model(&update()).expect("the model is written");

        let credential = settings
            .credential_in_force()
            .expect("the credential resolves")
            .expect("a credential is configured");
        assert_eq!(credential.source, CredentialSource::File);
        assert_eq!(
            credential.value, "sk-the-operators-key",
            "the trailing newline every editor adds is not part of the key"
        );
        assert_eq!(credential.hint().as_deref(), Some("-key"));
        assert!(
            settings
                .companion_credential_stored()
                .expect("the stored credential is readable"),
            "the browser's value is still stored, and the API has to be able to say so: \
             'the file wins' is useless to a human who cannot see that both exist"
        );
        assert_eq!(
            settings.credential_file_path(),
            Some(file.display().to_string().as_str())
        );
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn without_a_file_the_browsers_credential_is_the_one_in_force() {
        let settings = settings("browser-wins", None);
        assert_eq!(
            settings.credential_in_force().expect("no credential yet"),
            None,
            "an endpoint may need no credential, and none configured is not an error"
        );
        settings.set_model(&update()).expect("the model is written");
        let credential = settings
            .credential_in_force()
            .expect("the credential resolves")
            .expect("the browser set one");
        assert_eq!(credential.source, CredentialSource::Companion);
        assert_eq!(credential.value, "sk-set-from-the-browser");
    }

    #[test]
    fn a_named_but_empty_credential_file_is_an_error_not_an_absence() {
        let file = std::env::temp_dir().join(format!(
            "twalk-gateway-empty-credential-{}.key",
            std::process::id()
        ));
        std::fs::write(&file, "\n").expect("the test can write a file");
        let error = read_credential_file(&file)
            .expect_err("an operator who named a file meant to supply a credential")
            .to_string();
        assert!(error.contains("empty"), "got {error:?}");
        let _ = std::fs::remove_file(&file);
    }

    /// A settings screen that renames the model has never been shown the
    /// credential and has nothing to send back, so an absent member must not
    /// mean "forget it".
    #[test]
    fn a_write_that_says_nothing_about_the_credential_keeps_it() {
        let settings = settings("credential-kept", None);
        settings.set_model(&update()).expect("the model is written");

        let renamed = parse_model_update(&json!({
            "base_url": "http://127.0.0.1:4000/v1",
            "model": "qwen-2",
        }))
        .expect("a model rename parses");
        assert_eq!(renamed.credential, CredentialUpdate::Keep);
        settings.set_model(&renamed).expect("the rename is written");
        assert_eq!(
            settings
                .credential_in_force()
                .expect("the credential resolves")
                .map(|credential| credential.value),
            Some("sk-set-from-the-browser".to_owned()),
            "renaming a model must not lose the key the screen was never shown"
        );

        let cleared = parse_model_update(&json!({
            "base_url": "http://127.0.0.1:4000/v1",
            "model": "qwen-2",
            "credential": null,
        }))
        .expect("an explicit null parses");
        assert_eq!(cleared.credential, CredentialUpdate::Clear);
        settings.set_model(&cleared).expect("the clear is written");
        assert_eq!(
            settings
                .credential_in_force()
                .expect("the credential resolves"),
            None,
            "an explicit null forgets it"
        );
    }

    #[test]
    fn the_stored_model_is_read_back_as_it_was_written() {
        let settings = settings("round-trip", None);
        let written = parse_model_update(&json!({
            "base_url": "https://oai.endpoints.kepler.ai.cloud.ovh.net/v1/",
            "model": "Qwen3.8-27B",
            "params": { "drop_params": true, "temperature": null },
        }))
        .expect("the escape hatch parses too");
        let stored = settings.set_model(&written).expect("the model is written");
        assert_eq!(
            settings.model().expect("the model reads back"),
            Some(stored.clone())
        );
        assert_eq!(
            stored.base_url, "https://oai.endpoints.kepler.ai.cloud.ovh.net/v1",
            "the trailing slash is normalised away, so the runtime and the probe build the \
             same chat-completions URL"
        );
        assert_eq!(
            stored.params,
            Some(json!({ "drop_params": true, "temperature": null })),
            "the passthrough reaches the store untouched, nulls included (ADR 0015)"
        );
        assert!(settings.clear_model().expect("the model is cleared"));
        assert_eq!(
            settings.model().expect("the model reads back"),
            None,
            "no endpoint configured at all is a state, and it stays one"
        );
        assert!(
            !settings
                .clear_model()
                .expect("clearing twice is not an error"),
            "there was nothing left to forget"
        );
    }

    #[test]
    fn the_language_preference_is_one_of_the_five_or_none() {
        let settings = settings("language", None);
        assert_eq!(settings.language().expect("no preference yet"), None);
        settings
            .set_language(Some(Language::Fr))
            .expect("the preference is written");
        assert_eq!(
            settings.language().expect("the preference reads back"),
            Some(Language::Fr)
        );
        settings
            .set_language(None)
            .expect("the preference is cleared");
        assert_eq!(
            settings.language().expect("the preference reads back"),
            None,
            "no preference is a state of its own: a persona then follows the message it is \
             answering and nothing else (ADR 0016)"
        );

        assert_eq!(
            parse_language_update(&json!({ "language": "de" })),
            Ok(Some(Language::De))
        );
        assert_eq!(
            parse_language_update(&json!({ "language": null })),
            Ok(None)
        );
        for refused in ["fr-FR", "FR", "pt", ""] {
            let refusal = parse_language_update(&json!({ "language": refused }))
                .expect_err("only the five the Companion ships are accepted");
            assert_eq!(refusal.code(), "unsupported_language");
            assert!(
                refusal.message().contains("en, fr, it, es, de"),
                "the refusal names the five: {}",
                refusal.message()
            );
        }
    }

    /// The probe sends what a persona sends, including the operator's
    /// passthrough — so what it proves is what a persona will do, and not a
    /// near-miss of it.
    #[test]
    fn the_probe_sends_the_request_a_persona_would() {
        assert_eq!(
            chat_completions_url("http://127.0.0.1:4000/v1"),
            "http://127.0.0.1:4000/v1/chat/completions"
        );
        let plain = probe_payload("qwen", None);
        assert_eq!(plain["model"], json!("qwen"));
        assert_eq!(plain["max_tokens"], json!(1));
        assert_eq!(plain["messages"][0]["role"], json!("user"));

        let merged = probe_payload(
            "qwen",
            Some(&json!({ "max_tokens": 2000, "stream": null, "reasoning_effort": "medium" })),
        );
        assert_eq!(
            merged["max_tokens"],
            json!(2000),
            "the passthrough is merged last, so it overrides what the probe asked for"
        );
        assert_eq!(
            merged.get("stream"),
            None,
            "a member set to null removes a field the request would otherwise carry"
        );
        assert_eq!(merged["reasoning_effort"], json!("medium"));
    }

    /// Three causes, three answers — and a fourth for the endpoint that is
    /// neither absent nor unwilling but simply not speaking this protocol.
    /// The failure this project has produced nine times in two days is one
    /// signal for several causes.
    #[test]
    fn an_endpoint_that_refuses_is_not_an_endpoint_that_is_not_there() {
        let refused = ProbeOutcome::from_answer(401, r#"{"error":{"message":"bad key"}}"#);
        assert_eq!(refused.code(), "endpoint_refused");
        assert!(
            matches!(
                refused,
                ProbeOutcome::Refused {
                    endpoint_status: 401,
                    ..
                }
            ),
            "the endpoint's own status is carried through unedited: {refused:?}"
        );

        let completion = ProbeOutcome::from_answer(
            200,
            r#"{"model":"qwen","choices":[{"message":{"content":"pong"}}]}"#,
        );
        assert_eq!(completion.code(), "ok");
        assert!(matches!(
            completion,
            ProbeOutcome::Ok { model: Some(ref model), .. } if model == "qwen"
        ));

        let index_page = ProbeOutcome::from_answer(200, "<!doctype html>\n<title>a proxy</title>");
        assert_eq!(
            index_page.code(),
            "endpoint_not_compatible",
            "a web server's index page answers 200 very convincingly"
        );

        let unreachable = ProbeOutcome::Unreachable {
            detail: "connection refused".to_owned(),
        };
        assert_eq!(unreachable.code(), "endpoint_unreachable");
        assert_ne!(
            unreachable.code(),
            refused.code(),
            "an endpoint that is not there and one that says no are different problems with \
             different fixes (#116, #141)"
        );
    }
}
