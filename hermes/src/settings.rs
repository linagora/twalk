//! The runtime's read of the Companion Gateway's settings, and the one place
//! that decides what a persona is actually handed (ticket #184).
//!
//! Before this module existed, both halves of a bridge were built and there
//! was no span between them. `#98` made the Gateway serve
//! `GET /api/settings/runtime` — the model the user chose and their native
//! language, with the endpoint credential, on the deployment's service token.
//! `#164` made `environment.rs` inject `TWALK_USER_LANGUAGE` and the SDK read
//! it. Nothing called the Gateway: `twalk-hermes` had no HTTP client at all.
//! So a preference set in the Companion reached a persona only if an operator
//! *also* typed the same value into `.env` by hand, and a user who changed
//! their language saw the Companion confirm it and nothing happen, with
//! nothing anywhere saying why.
//!
//! Four decisions live here rather than in a comment to be discovered later.
//!
//! **1. It reads at startup, and only until it has a model.** A persona's
//! environment is built once, when the runtime spawns the process
//! ([`crate::environment`]), so applying a preference changed afterwards
//! would mean restarting every persona — dropping the event in flight and
//! taking a deployment's suggestions down for a language tag. So the read
//! happens once, before any persona starts, and the runtime **says at
//! startup** which value is in force, where it came from, and that a change
//! made in the Companion afterwards takes effect when the runtime is
//! restarted. Saying it is the point: a restart-to-apply nobody is told about
//! is a silent trap, which is the defect this ticket is about.
//!
//! The one continuation of that read is the background retry in `main.rs`, and it is
//! deliberately not
//! a re-read: **the retry exists to end an outage, never to apply a
//! preference.** A runtime that has no model has nothing to host; it keeps
//! asking until it has one and then starts the personas. A runtime that *is*
//! hosting personas never asks again.
//!
//! **2. A Gateway that does not answer does not stop the runtime.** The
//! Sensor already answered this exact question and its answer is copied
//! rather than reinvented (`sensor/src/main.rs`): it starts anyway, labels
//! every sender `pending`, says so at `ERROR` naming the URL, and retries in
//! the background. A runtime that refused to start because a settings
//! endpoint was slow would take a whole deployment down for a preference. So:
//! an unreachable Gateway is an `ERROR` naming the URL and what the runtime
//! fell back to, and the personas start on the operator's own configuration
//! if there is one — or, if there is not, the runtime hosts nothing, says
//! that, and retries. "Allowed to exist, handed nothing" is the shape the
//! rest of the platform already uses for this (ADR 0013's paused persona).
//!
//! **3. Precedence: the host wins, field by field, and the Gateway fills in
//! the blanks.** ADR 0015 already decided the hard half — a credential the
//! operator supplied *as a file* **wins** over one set from the browser — and
//! this is that decision generalised rather than a second, different one:
//! `.env` is the operator's other host-side voice, so it sits with the file,
//! above the browser. The same order for the model and for the language, with
//! no field exempt:
//!
//! | | the model | the language |
//! | --- | --- | --- |
//! | 1 | `HERMES_LLM_API_KEY_FILE` (the credential only) | — |
//! | 2 | `HERMES_LLM_BASE_URL`, `HERMES_LLM_MODEL`, `HERMES_LLM_API_KEY`, `HERMES_LLM_PARAMS` | `HERMES_USER_LANGUAGE` |
//! | 3 | `GET /api/settings/runtime` | `GET /api/settings/runtime` |
//!
//! That order does not make the browser's value inert, which is the objection
//! to answer: the reference deployment's `.env.example` ships every one of
//! those variables **empty**, and an empty variable is unset here
//! (`config::optional`), so on the reference deployment the value in force is
//! the Companion's. An operator who typed one into `.env` pinned it on
//! purpose, and the startup log says which fields they pinned.
//!
//! **4. A persona's environment gains no new credential.** The service token
//! that reads these settings is the token that opens the consent snapshot —
//! the list of every contact (ADR 0010, ADR 0015). It is held here, in the
//! runtime, and nothing in this module reaches [`crate::environment`]'s closed
//! list: what crosses is the *answer*, never the credential that fetched it.
//! The token is never logged either, and no error raised here carries it.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::config::{Config, LlmConfig, USER_LANGUAGES};

/// The Gateway's runtime read, the second and last row of #52's service-token
/// table (`companion-gateway/src/settings_http.rs`).
pub const RUNTIME_SETTINGS_PATH: &str = "/api/settings/runtime";

/// How long one read is given. Short: a slow Gateway must not hold a
/// deployment's startup, and the answer is a few hundred bytes from a service
/// on the same host.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The retry backoff, the Sensor's own (`SNAPSHOT_RETRY_BASE`,
/// `SNAPSHOT_RETRY_MAX`): one second, doubling to a minute, for as long as the
/// runtime has nothing to host.
pub const RETRY_BASE: Duration = Duration::from_secs(1);
pub const RETRY_MAX: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// What a persona is handed
// ---------------------------------------------------------------------------

/// Everything a persona is started with that could have come from either of
/// the two sources — resolved, complete, and the only thing
/// [`crate::environment`] is allowed to read them from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaSettings {
    pub llm: LlmConfig,
    /// The user's own language (ADR 0016). `None` is a supported state: the
    /// personas then answer in each message's own language and say so.
    pub user_language: Option<String>,
}

/// Which of the two voices a field in force came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `.env`, or a file on the host: the operator's, and it wins.
    Operator,
    /// The Companion, through the Gateway: the user's own choice, in force
    /// wherever the operator left the field empty.
    Gateway,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Gateway => "gateway",
        }
    }
}

/// The outcome of merging the two sources.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
    /// `None` when no model was named by either voice: there is nothing to
    /// host a persona with, and there is no default (ADR 0015).
    pub settings: Option<PersonaSettings>,
    /// Where each field in force came from, in a stable order, for the one
    /// startup line that closes the "I changed it in the Companion and
    /// nothing happened" trap.
    pub origins: BTreeMap<&'static str, Origin>,
    /// Values the Gateway served that a persona would have refused, dropped
    /// with a reason each. A Gateway serving something impossible must not
    /// take the deployment down — the operator cannot fix a stored value from
    /// a crash loop — but it must not be silent either.
    pub dropped: Vec<String>,
}

impl Resolution {
    /// The origins as one log-friendly string: `base_url=gateway
    /// model=gateway language=operator`.
    pub fn origins_line(&self) -> String {
        self.origins
            .iter()
            .map(|(field, origin)| format!("{field}={}", origin.as_str()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Merges the operator's host-side configuration with what the Gateway
/// served, field by field, the operator winning (see this module's decision
/// 3). Pure: the same inputs give the same answer, which is what lets the
/// precedence be tested in both directions without a Gateway or a `.env`.
pub fn resolve(config: &Config, gateway: Option<&GatewayRuntimeSettings>) -> Resolution {
    let mut origins = BTreeMap::new();
    let mut dropped = Vec::new();

    let served = gateway.and_then(|settings| settings.llm.as_ref());

    let base_url = pick(
        "base_url",
        config.llm.base_url.as_ref(),
        served.map(|llm| &llm.base_url),
        &mut origins,
    );
    let model = pick(
        "model",
        config.llm.model.as_ref(),
        served.map(|llm| &llm.model),
        &mut origins,
    );
    let api_key = pick(
        "api_key",
        config.llm.api_key.as_ref(),
        served.and_then(|llm| llm.api_key.as_ref()),
        &mut origins,
    );
    let params = pick(
        "params",
        config.llm.params.as_ref(),
        served.and_then(|llm| llm.params.as_ref()),
        &mut origins,
    );

    // The language, on exactly the same terms as the model — which is the
    // half of the precedence the acceptance criteria ask to be tested in both
    // directions. A tag outside the Companion's five is refused at startup
    // when it came from `.env` (`config::Config::validate`, the operator's
    // typo, refusable once); one served by the Gateway is dropped here with a
    // reason, because injected it would be refused by every persona, on its
    // first line, at every spawn — and a Gateway row an operator cannot edit
    // from a crash loop must not be able to stop the deployment.
    let served_language = match gateway.and_then(|settings| settings.language.as_ref()) {
        Some(language) if USER_LANGUAGES.contains(&language.as_str()) => Some(language),
        Some(refused) => {
            dropped.push(format!(
                "the Companion Gateway served the language {refused:?}, which is not one of {}: \
                 the personas fall back to nothing and answer in each message's own language",
                USER_LANGUAGES.join(", ")
            ));
            None
        }
        None => None,
    };
    let user_language = pick(
        "language",
        config.user_language.as_ref(),
        served_language,
        &mut origins,
    );

    let settings = match (base_url, model) {
        (Some(base_url), Some(model)) => Some(PersonaSettings {
            llm: LlmConfig {
                base_url,
                model,
                api_key,
                params,
                // Host-side only: the Gateway keeps no timeout.
                timeout_seconds: config.llm.timeout_seconds.clone(),
            },
            user_language,
        }),
        _ => {
            // A half-named model is not a model: nothing is hosted, and the
            // origins of the fields that *were* named stay in the report so
            // the log can say which half is missing.
            None
        }
    };

    Resolution {
        settings,
        origins,
        dropped,
    }
}

/// One field's precedence, and the record of which voice it came from. The
/// operator's value wins; the Gateway's fills the blank; a field neither
/// named appears in no report at all, because "unset" has no source.
fn pick(
    field: &'static str,
    operator: Option<&String>,
    served: Option<&String>,
    origins: &mut BTreeMap<&'static str, Origin>,
) -> Option<String> {
    match (operator, served) {
        (Some(value), _) => {
            origins.insert(field, Origin::Operator);
            Some(value.clone())
        }
        (None, Some(value)) => {
            origins.insert(field, Origin::Gateway);
            Some(value.clone())
        }
        (None, None) => None,
    }
}

// ---------------------------------------------------------------------------
// The Gateway's answer
// ---------------------------------------------------------------------------

/// The model configuration as the Gateway serves it. Every field is already a
/// string ready for a persona's environment: `params` is an object there and a
/// JSON string here, because that is what the SDK reads
/// (`TWALK_LLM_PARAMS`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayLlm {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub params: Option<String>,
    /// `file` or `companion`, as the Gateway resolved it on its own side
    /// (ADR 0015). Logged, never injected: it says which credential an
    /// operator is looking at without saying what it is.
    pub credential_source: Option<String>,
}

/// `GET /api/settings/runtime`, parsed.
///
/// `llm: null` is a **fact**, not an absence to paper over: it says the
/// operator has named no model, which the Gateway's own documentation
/// distinguishes from a Gateway that could not be reached and from one that
/// refused the token. Three facts, three signals, and this type keeps the
/// first of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GatewayRuntimeSettings {
    pub llm: Option<GatewayLlm>,
    pub language: Option<String>,
}

impl GatewayRuntimeSettings {
    pub fn parse(document: &Value) -> Result<Self> {
        anyhow::ensure!(
            document.is_object(),
            "the Companion Gateway's runtime settings are not a JSON object"
        );
        let language = document
            .get("language")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let llm = match document.get("llm") {
            None | Some(Value::Null) => None,
            Some(llm) => {
                let field = |name: &str| llm.get(name).and_then(Value::as_str).map(str::to_owned);
                // A served model missing its endpoint or its name is not a
                // model configuration; treated as none, so the runtime says
                // "no model configured" rather than injecting half of one.
                match (field("base_url"), field("model")) {
                    (Some(base_url), Some(model)) => Some(GatewayLlm {
                        base_url,
                        model,
                        api_key: field("api_key"),
                        // The Gateway serves the passthrough as the object an
                        // operator typed; the SDK reads a JSON string. A
                        // non-object is dropped rather than forwarded: the
                        // persona would refuse it on its first line.
                        params: match llm.get("params") {
                            Some(Value::Object(params)) if !params.is_empty() => {
                                Some(Value::Object(params.clone()).to_string())
                            }
                            _ => None,
                        },
                        credential_source: field("credential_source"),
                    }),
                    _ => None,
                }
            }
        };
        Ok(Self { llm, language })
    }
}

/// The real source: `GET /api/settings/runtime` on the Companion Gateway.
///
/// Authenticated by a **service token** from the runtime's own environment,
/// as an `Authorization: Bearer` credential — the same secret and the same
/// shape as the Sensor's read of the consent snapshot
/// (`sensor/src/consent.rs`), because it is the same token. The Hermes
/// runtime is a service and not one of the owner's browsers: it has no Matrix
/// OpenID token to sign in with and must never be given a device token
/// (ADR 0011). That token grants a read of the endpoint credential *and* of
/// every contact the user ever decided about, which is exactly why the runtime
/// holds it and a persona does not — so it is never logged, and no error this
/// module raises carries it.
pub struct GatewaySettings {
    client: reqwest::Client,
    url: String,
    service_token: String,
}

impl GatewaySettings {
    /// `base_url` is the Gateway's origin (e.g. `http://companion-gateway:8080`);
    /// the runtime-settings route is appended to it.
    pub fn new(base_url: &str, service_token: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .build()
            .context("failed to build the Companion Gateway HTTP client")?;
        Ok(Self {
            client,
            url: format!("{}{RUNTIME_SETTINGS_PATH}", base_url.trim_end_matches('/')),
            service_token: service_token.to_owned(),
        })
    }

    /// The source named in this runtime's configuration, or `None` when the
    /// deployment configured no Gateway at all.
    pub fn from_config(config: &Config) -> Result<Option<Self>> {
        match (&config.gateway_url, &config.gateway_service_token) {
            (Some(url), Some(token)) => Ok(Some(Self::new(url, token)?)),
            // `Config::validate` refuses the mixture, so this is the
            // no-Gateway deployment.
            _ => Ok(None),
        }
    }

    /// Where the settings are read from — safe to log, unlike the token.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn fetch(&self) -> Result<GatewayRuntimeSettings> {
        let response = self
            .client
            .get(&self.url)
            .bearer_auth(&self.service_token)
            .send()
            .await
            .with_context(|| format!("the Companion Gateway at {} is unreachable", self.url))?;
        let status = response.status();
        if !status.is_success() {
            // The body carries the Gateway's own error code
            // (`unauthenticated`, `settings_not_configured`,
            // `service_token_not_configured`, …), which is what an operator
            // needs to see; it never carries the token.
            let detail = response.text().await.unwrap_or_default();
            let detail: String = detail.chars().take(500).collect();
            anyhow::bail!(
                "the Companion Gateway at {} answered {status} to the runtime settings: {detail}",
                self.url
            );
        }
        let document: Value = response
            .json()
            .await
            .context("the Companion Gateway's runtime settings are not JSON")?;
        GatewayRuntimeSettings::parse(&document)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{OperatorLlm, PersonaSpec, RestartPolicy};
    use serde_json::json;

    fn config() -> Config {
        Config {
            personas: vec![PersonaSpec {
                id: "assistant".to_owned(),
                command: vec!["/bin/true".to_owned()],
            }],
            hermes_domain: "twalk.example.org".to_owned(),
            nats_url: "nats://bus:4222".to_owned(),
            stream: "twalk".to_owned(),
            subject_prefix: "twalk".to_owned(),
            llm: OperatorLlm::default(),
            gateway_url: Some("http://companion-gateway:8080".to_owned()),
            gateway_service_token: Some("a-service-token-no-persona-may-hold".to_owned()),
            log_level: "info".to_owned(),
            persona_log_level: "info".to_owned(),
            suggestion_ttl_seconds: None,
            user_language: None,
            restart: RestartPolicy::default(),
            shutdown_grace: Duration::from_secs(10),
            env_passthrough: vec!["PATH".to_owned()],
        }
    }

    /// The reference deployment's own document: the model from the browser,
    /// the credential from a file on the host, the language the user chose.
    fn served() -> GatewayRuntimeSettings {
        GatewayRuntimeSettings::parse(&json!({
            "llm": {
                "base_url": "http://127.0.0.1:4000/v1",
                "model": "qwen",
                "api_key": "sk-the-operators-key",
                "credential_source": "file",
                "params": { "max_tokens": 4000 },
            },
            "language": "fr",
            "personas": {},
        }))
        .expect("the Gateway's own document parses")
    }

    #[test]
    fn the_gateways_document_is_read_as_the_gateway_writes_it() {
        let settings = served();
        let llm = settings.llm.as_ref().expect("a model was configured");
        assert_eq!(llm.base_url, "http://127.0.0.1:4000/v1");
        assert_eq!(llm.model, "qwen");
        assert_eq!(llm.api_key.as_deref(), Some("sk-the-operators-key"));
        assert_eq!(llm.credential_source.as_deref(), Some("file"));
        assert_eq!(
            llm.params.as_deref(),
            Some(r#"{"max_tokens":4000}"#),
            "the passthrough reaches the persona as the JSON string the SDK reads (ADR 0015)"
        );
        assert_eq!(settings.language.as_deref(), Some("fr"));
    }

    /// `llm: null` is the Gateway saying the operator named no model, which is
    /// a different fact from a Gateway that could not be reached. It must not
    /// arrive here as an empty configuration that could be mistaken for one.
    #[test]
    fn no_model_configured_is_a_fact_and_not_an_absence() {
        let settings = GatewayRuntimeSettings::parse(&json!({
            "llm": Value::Null,
            "language": Value::Null,
            "personas": {},
        }))
        .expect("the document parses");
        assert_eq!(settings.llm, None);
        assert_eq!(settings.language, None);

        // Half a model is not a model either: injecting an endpoint with no
        // model name would be refused by every persona on its first line.
        let half = GatewayRuntimeSettings::parse(&json!({
            "llm": { "base_url": "http://127.0.0.1:4000/v1" },
            "language": "fr",
        }))
        .expect("the document parses");
        assert_eq!(half.llm, None);
        assert_eq!(half.language.as_deref(), Some("fr"));
    }

    /// The decisive direction, and the ticket's first acceptance criterion:
    /// the operator typed nothing on the host, so what the user set in the
    /// Companion is what a persona runs with.
    #[test]
    fn what_the_user_set_in_the_companion_is_what_a_persona_runs_with() {
        let resolution = resolve(&config(), Some(&served()));
        let settings = resolution
            .settings
            .as_ref()
            .expect("the Gateway named a whole model configuration");
        assert_eq!(settings.llm.base_url, "http://127.0.0.1:4000/v1");
        assert_eq!(settings.llm.model, "qwen");
        assert_eq!(
            settings.llm.api_key.as_deref(),
            Some("sk-the-operators-key")
        );
        assert_eq!(
            settings.llm.params.as_deref(),
            Some(r#"{"max_tokens":4000}"#)
        );
        assert_eq!(
            settings.user_language.as_deref(),
            Some("fr"),
            "the language is the half of ADR 0016 that had no value to fall back to"
        );
        for field in ["base_url", "model", "api_key", "params", "language"] {
            assert_eq!(
                resolution.origins.get(field),
                Some(&Origin::Gateway),
                "{field} came from the Companion, and the startup log must say so"
            );
        }
        assert!(resolution.dropped.is_empty());
    }

    /// The other direction, and the same order for the model and for the
    /// language: ADR 0015 puts what the operator supplied on the host above
    /// what was set from the browser, and `.env` is the operator's other
    /// host-side voice.
    #[test]
    fn the_operators_own_configuration_wins_over_the_browsers() {
        let mut config = config();
        config.llm.base_url = Some("https://pinned.example.org/v1".to_owned());
        config.llm.model = Some("a-model-the-operator-pinned".to_owned());
        config.llm.api_key = Some("sk-from-the-operators-file".to_owned());
        config.llm.params = Some(r#"{"top_p": 0.9}"#.to_owned());
        config.user_language = Some("es".to_owned());

        let resolution = resolve(&config, Some(&served()));
        let settings = resolution.settings.as_ref().expect("a whole configuration");
        assert_eq!(settings.llm.base_url, "https://pinned.example.org/v1");
        assert_eq!(settings.llm.model, "a-model-the-operator-pinned");
        assert_eq!(
            settings.llm.api_key.as_deref(),
            Some("sk-from-the-operators-file"),
            "a credential the operator supplied wins over one set from the browser (ADR 0015)"
        );
        assert_eq!(settings.llm.params.as_deref(), Some(r#"{"top_p": 0.9}"#));
        assert_eq!(
            settings.user_language.as_deref(),
            Some("es"),
            "the language sits in the same order as the model, not a different one"
        );
        for field in ["base_url", "model", "api_key", "params", "language"] {
            assert_eq!(resolution.origins.get(field), Some(&Origin::Operator));
        }
    }

    /// Field by field, not document by document: the reference deployment's
    /// ordinary combination is an endpoint credential from a file on the host
    /// and a model name from the browser (ADR 0015), so a resolution that
    /// took one source whole would get the deployment that actually exists
    /// wrong.
    #[test]
    fn the_two_sources_are_merged_field_by_field() {
        let mut config = config();
        config.llm.api_key = Some("sk-from-the-operators-file".to_owned());
        config.llm.timeout_seconds = Some("45".to_owned());

        let resolution = resolve(&config, Some(&served()));
        let settings = resolution.settings.as_ref().expect("a whole configuration");
        assert_eq!(
            settings.llm.model, "qwen",
            "the model name is the browser's"
        );
        assert_eq!(
            settings.llm.api_key.as_deref(),
            Some("sk-from-the-operators-file"),
            "and the credential is the operator's file"
        );
        assert_eq!(
            settings.llm.timeout_seconds.as_deref(),
            Some("45"),
            "the timeout has one voice only: the Gateway keeps none"
        );
        assert_eq!(resolution.origins.get("model"), Some(&Origin::Gateway));
        assert_eq!(resolution.origins.get("api_key"), Some(&Origin::Operator));
        assert_eq!(
            resolution.origins_line(),
            "api_key=operator base_url=gateway language=gateway model=gateway params=gateway",
            "the startup line names every field's source, so nobody has to guess which \
             voice is in force"
        );
    }

    /// Neither voice named a model: there is nothing to host a persona with
    /// and no default (ADR 0015). The runtime does not die of it — it hosts
    /// nothing, says so and retries — but there is no configuration to hand
    /// over, and this is the type that says it.
    #[test]
    fn a_model_nobody_named_resolves_to_nothing() {
        let no_model = GatewayRuntimeSettings {
            llm: None,
            language: Some("fr".to_owned()),
        };
        let resolution = resolve(&config(), Some(&no_model));
        assert_eq!(resolution.settings, None);

        // …and the same with no Gateway answer at all.
        assert_eq!(resolve(&config(), None).settings, None);

        // The operator's own half alone is enough, though: an unreachable
        // Gateway with a model in `.env` starts the personas.
        let mut pinned = config();
        pinned.llm.base_url = Some("https://pinned.example.org/v1".to_owned());
        pinned.llm.model = Some("a-model-the-operator-pinned".to_owned());
        let resolution = resolve(&pinned, None);
        let settings = resolution.settings.expect("the operator named a model");
        assert_eq!(settings.llm.model, "a-model-the-operator-pinned");
        assert_eq!(
            settings.user_language, None,
            "and the preference is simply not in force, which is what the ERROR says"
        );
    }

    /// A tag outside the Companion's five cannot be typed into the Companion,
    /// so one here is a row an operator cannot edit from a crash loop. It is
    /// dropped with a reason rather than refused: a persona handed it would
    /// refuse on its first line, at every spawn, and a stored value must not
    /// be able to stop a deployment.
    #[test]
    fn a_language_the_gateway_should_not_have_served_is_dropped_and_said() {
        let served = GatewayRuntimeSettings {
            llm: None,
            language: Some("pt-BR".to_owned()),
        };
        let resolution = resolve(&config(), Some(&served));
        assert_eq!(resolution.origins.get("language"), None);
        assert_eq!(resolution.dropped.len(), 1, "{:?}", resolution.dropped);
        assert!(
            resolution.dropped[0].contains("pt-BR")
                && resolution.dropped[0].contains("en, fr, it, es, de"),
            "the line must name the value and the five: {:?}",
            resolution.dropped
        );
    }

    /// The URL is built once and is safe to log; the token never appears in
    /// it, because it travels as an `Authorization` header.
    #[test]
    fn the_settings_url_carries_no_credential() {
        let source = GatewaySettings::new(
            "http://companion-gateway:8080/",
            "a-service-token-no-persona-may-hold",
        )
        .expect("the client builds");
        assert_eq!(
            source.url(),
            "http://companion-gateway:8080/api/settings/runtime",
            "the trailing slash is normalised away"
        );
        assert!(
            !source.url().contains("a-service-token"),
            "the token is a header, never a query parameter: it opens the consent snapshot too"
        );
    }
}
