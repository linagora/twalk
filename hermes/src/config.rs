//! The runtime's configuration, read from the environment alone, so Hermes
//! joins the compose deployment the way the Sensor and the Companion
//! Gateway do.
//!
//! Two decisions are encoded here rather than in the binary.
//!
//! **The persona list is the operator's, and it is a list of commands.** A
//! persona is a separate process (ADR 0008); what that process *is* — a
//! container run, a binary, a third party's image — is not the runtime's
//! business, so `HERMES_PERSONAS` names an id and an argv and nothing else.
//! An argv, not a shell string: there is no quoting to get wrong and no
//! shell between the runtime and the persona.
//!
//! **There is no default model.** A model has to be named, and the runtime
//! refuses to start when nobody has named one and nobody can — the same
//! refusal the SDK makes, one level up, because the runtime is what hands it
//! over (ADR 0015). A runtime that started with no model configured would
//! spawn personas that all die on their first line, which is a worse way to
//! say the same thing.
//!
//! What this module holds is only the **operator's** half of that: what was
//! typed on the host, in `.env` or in a file. The other half is the user's,
//! held by the Companion Gateway and read by [`crate::settings`], which is
//! why `HERMES_LLM_BASE_URL` and `HERMES_LLM_MODEL` are no longer required
//! *here*: a deployment that names `HERMES_GATEWAY_URL` has somewhere to
//! read them from. The refusal moved rather than went away — with neither a
//! model nor a Gateway, this module still refuses to start, and says both
//! ways out.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// The bus namespace a Twalk deployment publishes under, and the stream
/// that captures it (`sensor/src/normalize.rs`).
pub const DEFAULT_STREAM: &str = "twalk";
pub const DEFAULT_SUBJECT_PREFIX: &str = "twalk";
pub const DEFAULT_NATS_URL: &str = "nats://localhost:4222";

/// The interface languages the Companion ships, which are the five the
/// Companion Gateway stores a preference among
/// (`companion-gateway/src/settings.rs`, `companion/src/lib/i18n/`) and the
/// five the SDK knows the English name of (`sdk/python/twalk_sdk/language.py`).
///
/// The runtime validates against them for the same reason it requires a
/// model: a value only a persona would refuse would refuse it once per
/// spawn, on its first line, for ever (ADR 0016).
pub const USER_LANGUAGES: [&str; 5] = ["en", "fr", "it", "es", "de"];

/// `data.persona_id` and the last segment of `source` in every `persona.*`
/// schema: the runtime validates a persona id against the contract's own
/// pattern, so a misconfigured id fails at startup instead of producing
/// events no consumer accepts.
fn is_contract_persona_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// One persona the operator asked this deployment to host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaSpec {
    /// The persona id, e.g. `assistant`: the subject of its activation
    /// decisions, half of every deterministic id it produces, and the name
    /// of its durable consumer.
    pub id: String,
    /// The argv the runtime spawns. In the reference deployment this runs
    /// the persona's container image; in the tests it runs the same image.
    pub command: Vec<String>,
}

impl PersonaSpec {
    /// The durable consumer this persona reads through — the name the SDK
    /// derives from the persona id when it is given none
    /// (`sdk/python/twalk_sdk/config.py`). The runtime creates the consumer
    /// and hands the name over, so the two cannot disagree.
    pub fn consumer_name(&self) -> String {
        format!("persona-{}", self.id)
    }
}

/// Where the personas this runtime hosts reason, with which model, and with
/// what credentials — **complete**, whichever of the two sources each field
/// came from ([`crate::settings::resolve`]).
///
/// Held by the runtime, injected into each persona's environment — never
/// fetched by a persona, because the credential that would open the
/// Gateway's configuration also opens the consent snapshot (ADR 0015).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    /// The operator's provider parameters, as the JSON object the SDK
    /// passes through to the endpoint untouched.
    ///
    /// Routinely `None`, and that is the healthy shape: the object exists
    /// because providers differ in what they reject (ADR 0015), and an
    /// operator running a proxy in front of their model keeps those quirks
    /// in the proxy's own configuration, where they belong. Nothing here
    /// may depend on it being set.
    pub params: Option<String>,
    pub timeout_seconds: Option<String>,
}

/// The model configuration **as the operator typed it on the host** — every
/// field optional, because on the reference deployment every one of them is
/// empty and the answer comes from the Companion Gateway instead.
///
/// It is not an [`LlmConfig`] that happens to be half-filled: it is the
/// higher-precedence half of one (`crate::settings`). Keeping the two types
/// apart is what makes "a persona is handed a complete configuration" a fact
/// the compiler enforces rather than a habit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperatorLlm {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub params: Option<String>,
    /// Host-side only: the Gateway keeps no timeout, so nothing overlays this.
    pub timeout_seconds: Option<String>,
}

impl LlmConfig {
    /// The host of the configured endpoint, lowercased: `127.0.0.1` from
    /// `http://127.0.0.1:4000/v1`.
    pub fn endpoint_host(&self) -> Option<String> {
        let rest = self
            .base_url
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(&self.base_url);
        let authority = rest.split(['/', '?', '#']).next()?;
        let authority = authority
            .rsplit_once('@')
            .map(|(_, host)| host)
            .unwrap_or(authority);
        // An IPv6 literal is bracketed, and its colons are not a port.
        if let Some(bracketed) = authority.strip_prefix('[') {
            return bracketed
                .split_once(']')
                .map(|(host, _)| host.to_ascii_lowercase());
        }
        let host = authority.split(':').next()?;
        (!host.is_empty()).then(|| host.to_ascii_lowercase())
    }

    /// Whether the endpoint is on **this host's** loopback.
    ///
    /// It is a perfectly good address — an operator's proxy published on
    /// `127.0.0.1:4000` is the reference deployment's shape — and it is a
    /// trap for a persona that runs in a container of its own network
    /// namespace, which would dial itself. The runtime cannot fix that (how
    /// a persona's container joins a network is the command's business and
    /// the deployment's), but it is the one component that holds the URL,
    /// so it is the one that can say so before a message ever arrives.
    pub fn endpoint_is_loopback(&self) -> bool {
        let Some(host) = self.endpoint_host() else {
            return false;
        };
        host == "localhost"
            || host.ends_with(".localhost")
            || host == "::1"
            || host == "0:0:0:0:0:0:0:1"
            || host
                .parse::<std::net::Ipv4Addr>()
                .is_ok_and(|address| address.is_loopback())
    }
}

/// Resolves the endpoint credential from the two places it can come from.
///
/// **The file wins** (ADR 0015): it is the operator's, on the host, and the
/// other one is whatever the Companion last wrote through the Gateway — so
/// a production stack can lock the credential down. On the reference
/// deployment that is the ordinary combination, not the exotic one: the
/// model name comes from the browser and the key from a file.
///
/// The file's content is trimmed of trailing newlines, because every way of
/// writing one adds one, and an empty file is an error rather than "no
/// credential": an operator who named a file meant to supply one.
pub fn resolve_api_key(file: Option<(&str, &str)>, inline: Option<&str>) -> Result<Option<String>> {
    match file {
        Some((path, contents)) => {
            let key = contents.trim_end_matches(['\n', '\r']);
            if key.is_empty() {
                bail!(
                    "HERMES_LLM_API_KEY_FILE at {path} is empty: it names the credential the \
                     personas call the endpoint with, and an empty file is not one"
                );
            }
            Ok(Some(key.to_owned()))
        }
        None => Ok(inline.map(str::to_owned)),
    }
}

/// How a crashed — or unstartable — persona is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy {
    /// First delay after a failed run; doubles with each consecutive
    /// failure, capped at `max_backoff`.
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    /// How long a persona must stay up before the run counts as a success
    /// and the backoff resets. A process that dies faster than this never
    /// really started.
    pub healthy_after: Duration,
    /// How many consecutive runs may fail before the runtime announces the
    /// persona as failed rather than as restarting.
    pub start_failures: u32,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            base_backoff: Duration::from_millis(1_000),
            max_backoff: Duration::from_millis(60_000),
            healthy_after: Duration::from_millis(10_000),
            start_failures: 3,
        }
    }
}

/// The whole runtime configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub personas: Vec<PersonaSpec>,
    /// The authority of every hosted persona's `source` URI
    /// (`hermes://<domain>/personas/<persona id>`).
    pub hermes_domain: String,
    pub nats_url: String,
    pub stream: String,
    pub subject_prefix: String,
    /// What the **operator** named, on this host. The user's own choices come
    /// from the Companion Gateway and are overlaid underneath this by
    /// [`crate::settings::resolve`].
    pub llm: OperatorLlm,
    /// The Companion Gateway's origin, e.g. `http://companion-gateway:8080`,
    /// and the service token that reads it. Both or neither: a URL without a
    /// token is a read that would only ever answer `401`, and it is refused
    /// at startup rather than reported once a minute.
    ///
    /// The token is the same secret that opens the **consent snapshot** — the
    /// list of every contact (ADR 0010, ADR 0015) — which is the whole reason
    /// the runtime reads the settings and injects them rather than letting
    /// each persona fetch them. It is never logged and never injected; see
    /// [`crate::environment`].
    pub gateway_url: Option<String>,
    pub gateway_service_token: Option<String>,
    pub log_level: String,
    /// The log level the personas are started with. Separate from the
    /// runtime's: an operator debugging their own persona should not have
    /// to make the supervisor noisy too.
    pub persona_log_level: String,
    /// How long a suggestion stays approvable, in whole seconds (ticket
    /// #22). Operator configuration like the model, so it is held here and
    /// injected; unset leaves the SDK's own default.
    pub suggestion_ttl_seconds: Option<String>,
    /// The **user's own** language, one of [`USER_LANGUAGES`]: what a persona
    /// falls back to when it cannot tell what language the message it is
    /// answering was written in, and nothing else (ADR 0016, ticket #164).
    ///
    /// A user preference rather than an operator's setting, which is why it
    /// is held by the Gateway and travels the injection channel the model
    /// configuration travels: a persona runs in a container and has no
    /// browser to read it from. This field is the **operator's override** of
    /// it — `HERMES_USER_LANGUAGE`, empty in the reference deployment's
    /// `.env`; the Gateway's stored preference is what fills it in
    /// ([`crate::settings::resolve`]). `None` on both sides is a supported
    /// state — the personas then answer in each message's own language and
    /// say in their logs what happens to a message whose language they cannot
    /// tell. The runtime does not refuse to start over it: the fallback is
    /// the ambiguous case, and a deployment that suggests replies correctly
    /// for every readable message is not an outage.
    pub user_language: Option<String>,
    pub restart: RestartPolicy,
    /// How long a persona is given to exit after SIGTERM before it is
    /// killed.
    pub shutdown_grace: Duration,
    /// Variables of the runtime's **own** environment that a persona is
    /// allowed to inherit, by name. Defaults to `PATH` alone, which is what
    /// a child needs to exec at all. Anything else an operator adds here is
    /// a deliberate, auditable act — see [`crate::environment`] for why the
    /// list exists rather than the inheritance.
    pub env_passthrough: Vec<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let config = Self {
            personas: parse_personas(&required("HERMES_PERSONAS")?)?,
            hermes_domain: required("HERMES_DOMAIN")?,
            nats_url: optional("HERMES_NATS_URL").unwrap_or_else(|| DEFAULT_NATS_URL.to_owned()),
            stream: optional("HERMES_BUS_STREAM").unwrap_or_else(|| DEFAULT_STREAM.to_owned()),
            subject_prefix: optional("HERMES_BUS_SUBJECT_PREFIX")
                .unwrap_or_else(|| DEFAULT_SUBJECT_PREFIX.to_owned()),
            llm: OperatorLlm {
                base_url: optional("HERMES_LLM_BASE_URL"),
                model: optional("HERMES_LLM_MODEL"),
                api_key: llm_api_key()?,
                params: optional("HERMES_LLM_PARAMS"),
                timeout_seconds: optional("HERMES_LLM_TIMEOUT_SECONDS"),
            },
            gateway_url: optional("HERMES_GATEWAY_URL"),
            gateway_service_token: optional("HERMES_GATEWAY_SERVICE_TOKEN"),
            log_level: optional("HERMES_LOG_LEVEL").unwrap_or_else(|| "info".to_owned()),
            persona_log_level: optional("HERMES_PERSONA_LOG_LEVEL")
                .unwrap_or_else(|| "info".to_owned()),
            suggestion_ttl_seconds: optional("HERMES_SUGGESTION_TTL_SECONDS"),
            user_language: optional("HERMES_USER_LANGUAGE"),
            restart: RestartPolicy {
                base_backoff: millis("HERMES_RESTART_BACKOFF_BASE_MS", 1_000)?,
                max_backoff: millis("HERMES_RESTART_BACKOFF_MAX_MS", 60_000)?,
                healthy_after: millis("HERMES_PERSONA_HEALTHY_AFTER_MS", 10_000)?,
                start_failures: number("HERMES_PERSONA_START_FAILURES", 3)?,
            },
            shutdown_grace: millis("HERMES_SHUTDOWN_GRACE_MS", 10_000)?,
            env_passthrough: list("HERMES_PERSONA_ENV_PASSTHROUGH")
                .unwrap_or_else(|| vec!["PATH".to_owned()]),
        };
        config.validate()?;
        Ok(config)
    }

    /// The bus subject of a contract event type, in this deployment's
    /// namespace: `fr.linagora.twalk.<domain>.<action>.<version>` becomes
    /// `<prefix>.<domain>.<action>.<version>`, exactly as the Sensor maps it.
    pub fn subject(&self, event_type: &str) -> String {
        let rest = event_type
            .strip_prefix("fr.linagora.twalk.")
            .expect("contract event types always carry the fr.linagora.twalk prefix");
        format!("{}.{rest}", self.subject_prefix)
    }

    /// The subjects the stream this runtime reads and writes must capture.
    pub fn stream_subjects(&self) -> Vec<String> {
        vec![format!("{}.>", self.subject_prefix)]
    }

    fn validate(&self) -> Result<()> {
        if self.personas.is_empty() {
            bail!("HERMES_PERSONAS names no persona: there would be nothing to host");
        }
        let mut seen = HashSet::new();
        for persona in &self.personas {
            if !is_contract_persona_id(&persona.id) {
                bail!(
                    "HERMES_PERSONAS has a persona id the contract would refuse: {:?} does not \
                     match ^[a-z0-9_-]+$, so its events could not be schema-valid",
                    persona.id
                );
            }
            if persona.command.is_empty() {
                bail!(
                    "the persona {:?} has an empty command: the runtime spawns a process, so it \
                     needs an argv to spawn",
                    persona.id
                );
            }
            if !seen.insert(persona.id.as_str()) {
                bail!(
                    "HERMES_PERSONAS names {:?} twice: two processes would share one durable \
                     consumer and one deterministic id space",
                    persona.id
                );
            }
        }
        if self.hermes_domain.is_empty() || self.hermes_domain.contains('/') {
            bail!(
                "HERMES_DOMAIN must be a domain without a slash (it is the authority of every \
                 hosted persona's source URI), got {:?}",
                self.hermes_domain
            );
        }
        if let Some(language) = &self.user_language {
            if !USER_LANGUAGES.contains(&language.as_str()) {
                bail!(
                    "HERMES_USER_LANGUAGE is the user's own language — the one a persona falls \
                     back to when it cannot tell what language it is answering (ADR 0016) — and \
                     must be one of {}, spelled as the Companion writes it; got {:?}",
                    USER_LANGUAGES.join(", "),
                    language
                );
            }
        }
        if let Some(params) = &self.llm.params {
            let parsed: Value = serde_json::from_str(params)
                .context("HERMES_LLM_PARAMS must be a JSON object of provider parameters")?;
            if !parsed.is_object() {
                bail!("HERMES_LLM_PARAMS must be a JSON object, got {parsed}");
            }
        }
        // A URL with no token is refused: that read could only ever answer
        // `401`, once a minute, for the life of the deployment. The mirror
        // case — a token with no URL — is *not* an error, for the same reason
        // it is not one in the Sensor: the URL is what says "read the
        // Gateway", and a credential naming no endpoint simply opens nothing.
        // The runtime says so at startup instead ([`crate::settings`]), which
        // is what keeps it from being a silent trap.
        if self.gateway_url.is_some() && self.gateway_service_token.is_none() {
            bail!(
                "HERMES_GATEWAY_URL names {} but HERMES_GATEWAY_SERVICE_TOKEN is unset: the \
                 runtime settings take this deployment's service token as an Authorization: \
                 Bearer credential, so that read could only ever answer 401",
                self.gateway_url.as_deref().unwrap_or_default()
            );
        }
        // The ADR 0015 refusal, in the one place it still belongs: nobody has
        // named a model and there is nowhere to read one from. A Gateway is
        // enough — what it answers is its business, and a Gateway that is down
        // or that holds no model yet is `settings`'s to report, loudly, rather
        // than a reason to take the deployment down (ticket #184).
        if !self.gateway_configured() && !self.llm.is_complete() {
            bail!(
                "no model is configured and no Companion Gateway to read one from: Twalk ships \
                 no LLM and there is no default endpoint (ADR 0015). Either set \
                 HERMES_LLM_BASE_URL and HERMES_LLM_MODEL, or point HERMES_GATEWAY_URL and \
                 HERMES_GATEWAY_SERVICE_TOKEN at this deployment's Companion Gateway, which \
                 holds the model the user chose in the Companion"
            );
        }
        Ok(())
    }

    /// Whether this runtime has a Companion Gateway to read the user's
    /// settings from. A URL without a token is refused when the configuration
    /// is validated; a token without a URL simply configures no Gateway.
    pub fn gateway_configured(&self) -> bool {
        self.gateway_url.is_some() && self.gateway_service_token.is_some()
    }
}

impl OperatorLlm {
    /// Whether the operator named a whole model configuration on the host, so
    /// nothing has to be read from the Gateway for the personas to start.
    pub fn is_complete(&self) -> bool {
        self.base_url.is_some() && self.model.is_some()
    }
}

/// Parses `HERMES_PERSONAS`: a JSON array of `{"id": ..., "command": [...]}`.
///
/// JSON rather than a delimited string because a command is an argv and an
/// argv in a delimited string is a quoting bug waiting to happen — the same
/// reason `TWALK_LLM_PARAMS` is JSON.
pub fn parse_personas(raw: &str) -> Result<Vec<PersonaSpec>> {
    let parsed: Value = serde_json::from_str(raw).context(
        "HERMES_PERSONAS must be a JSON array of {\"id\": \"...\", \"command\": [\"...\"]}",
    )?;
    let entries = parsed
        .as_array()
        .context("HERMES_PERSONAS must be a JSON array")?;
    let mut personas = Vec::with_capacity(entries.len());
    for entry in entries {
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .with_context(|| format!("a persona in HERMES_PERSONAS has no string id: {entry}"))?;
        let command = entry
            .get("command")
            .and_then(Value::as_array)
            .with_context(|| format!("the persona {id:?} has no command array"))?
            .iter()
            .map(|argument| {
                argument
                    .as_str()
                    .map(str::to_owned)
                    .with_context(|| format!("the persona {id:?} has a non-string argv entry"))
            })
            .collect::<Result<Vec<_>>>()?;
        personas.push(PersonaSpec {
            id: id.to_owned(),
            command,
        });
    }
    Ok(personas)
}

/// Reads the two credential sources from the environment and lets
/// [`resolve_api_key`] choose between them.
fn llm_api_key() -> Result<Option<String>> {
    let path = optional("HERMES_LLM_API_KEY_FILE");
    let contents = match &path {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("failed to read HERMES_LLM_API_KEY_FILE at {path}"))?,
        ),
        None => None,
    };
    let file = match (&path, &contents) {
        (Some(path), Some(contents)) => Some((path.as_str(), contents.as_str())),
        _ => None,
    };
    resolve_api_key(file, optional("HERMES_LLM_API_KEY").as_deref())
}

/// An environment variable that is absent or empty is unset: an empty value
/// in a compose `.env` file is how an operator leaves an option out.
fn optional(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn list(name: &str) -> Option<Vec<String>> {
    optional(name).map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

fn required(name: &str) -> Result<String> {
    optional(name).with_context(|| format!("missing required environment variable {name}"))
}

fn number<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match optional(name) {
        Some(raw) => raw
            .parse()
            .with_context(|| format!("environment variable {name} has an invalid value")),
        None => Ok(default),
    }
}

fn millis(name: &str, default: u64) -> Result<Duration> {
    Ok(Duration::from_millis(number(name, default)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_persona_list_is_an_id_and_an_argv() {
        let personas = parse_personas(
            r#"[{"id": "assistant", "command": ["docker", "run", "--rm", "twalk/assistant"]}]"#,
        )
        .expect("a well-formed persona list parses");
        assert_eq!(
            personas,
            vec![PersonaSpec {
                id: "assistant".to_owned(),
                command: vec![
                    "docker".to_owned(),
                    "run".to_owned(),
                    "--rm".to_owned(),
                    "twalk/assistant".to_owned()
                ],
            }]
        );
        assert_eq!(personas[0].consumer_name(), "persona-assistant");
    }

    #[test]
    fn a_persona_list_that_is_not_one_says_which_part_is_wrong() {
        for (raw, expected) in [
            ("not json", "JSON array"),
            (r#"{"id": "assistant"}"#, "JSON array"),
            (r#"[{"command": ["x"]}]"#, "no string id"),
            (r#"[{"id": "assistant"}]"#, "no command array"),
            (
                r#"[{"id": "assistant", "command": [7]}]"#,
                "non-string argv",
            ),
        ] {
            let error = parse_personas(raw)
                .expect_err("a malformed persona list must not parse")
                .to_string();
            assert!(
                error.contains(expected),
                "the error for {raw:?} must mention {expected:?}, got {error:?}"
            );
        }
    }

    fn config_with(personas: Vec<PersonaSpec>) -> Config {
        Config {
            personas,
            hermes_domain: "twalk.example.org".to_owned(),
            nats_url: DEFAULT_NATS_URL.to_owned(),
            stream: DEFAULT_STREAM.to_owned(),
            subject_prefix: DEFAULT_SUBJECT_PREFIX.to_owned(),
            llm: OperatorLlm {
                base_url: Some("http://localhost:8000/v1".to_owned()),
                model: Some("a-model-the-operator-named".to_owned()),
                ..Default::default()
            },
            gateway_url: None,
            gateway_service_token: None,
            log_level: "info".to_owned(),
            persona_log_level: "info".to_owned(),
            suggestion_ttl_seconds: None,
            user_language: None,
            restart: RestartPolicy::default(),
            shutdown_grace: Duration::from_secs(10),
            env_passthrough: vec!["PATH".to_owned()],
        }
    }

    fn spec(id: &str) -> PersonaSpec {
        PersonaSpec {
            id: id.to_owned(),
            command: vec!["/bin/true".to_owned()],
        }
    }

    #[test]
    fn a_persona_id_the_contract_would_refuse_is_refused_at_startup() {
        let error = config_with(vec![spec("Assistant")])
            .validate()
            .expect_err("an id outside the contract's pattern must not start")
            .to_string();
        assert!(error.contains("^[a-z0-9_-]+$"), "got {error:?}");
    }

    #[test]
    fn the_same_persona_twice_is_refused_at_startup() {
        let error = config_with(vec![spec("assistant"), spec("assistant")])
            .validate()
            .expect_err("two processes must not share one durable consumer")
            .to_string();
        assert!(error.contains("twice"), "got {error:?}");
    }

    #[test]
    fn a_persona_with_no_command_is_refused_at_startup() {
        let mut empty = spec("assistant");
        empty.command.clear();
        let error = config_with(vec![empty])
            .validate()
            .expect_err("there is no process to spawn")
            .to_string();
        assert!(error.contains("empty command"), "got {error:?}");
    }

    /// The Gateway refuses a tag outside the five on the way in, so one here
    /// was typed into `.env` by hand. Refusing it at startup is the only
    /// place it can be refused once: injected, it would be refused by every
    /// persona, on its first line, at every spawn (ADR 0016, ticket #164).
    #[test]
    fn a_language_that_is_not_one_of_the_companions_five_is_refused_at_startup() {
        let mut config = config_with(vec![spec("assistant")]);
        for refused in ["fr-FR", "FR", "pt", "français"] {
            config.user_language = Some(refused.to_owned());
            let error = config
                .validate()
                .expect_err("a tag outside the five must not start")
                .to_string();
            assert!(error.contains("en, fr, it, es, de"), "got {error:?}");
            assert!(error.contains(refused), "got {error:?}");
        }
        for accepted in USER_LANGUAGES {
            config.user_language = Some(accepted.to_owned());
            config
                .validate()
                .expect("each of the Companion's five is a preference a persona can be handed");
        }
        config.user_language = None;
        config
            .validate()
            .expect("no preference is a supported state: the fallback is the ambiguous case alone");
    }

    #[test]
    fn provider_parameters_that_are_not_a_json_object_are_refused_at_startup() {
        let mut config = config_with(vec![spec("assistant")]);
        config.llm.params = Some("[1, 2]".to_owned());
        let error = config
            .validate()
            .expect_err("the SDK would refuse it one process later")
            .to_string();
        assert!(error.contains("JSON object"), "got {error:?}");
    }

    /// The reference deployment's shape: the model name comes from the
    /// Companion, the credential from a file on the host. The file wins, so
    /// that combination is the one that has to work (ADR 0015).
    #[test]
    fn a_credential_in_a_file_beats_one_set_from_the_browser() {
        assert_eq!(
            resolve_api_key(
                Some(("/run/secrets/litellm-master.key", "sk-the-operators-key\n")),
                Some("whatever-the-companion-last-wrote"),
            )
            .expect("a file credential resolves"),
            Some("sk-the-operators-key".to_owned()),
            "the file is the operator's and it wins"
        );
        assert_eq!(
            resolve_api_key(Some(("/k", "sk-crlf\r\n")), None).expect("trailing CRLF is trimmed"),
            Some("sk-crlf".to_owned()),
            "every way of writing a file adds a newline; none of them is part of the key"
        );
        assert_eq!(
            resolve_api_key(None, Some("from-the-companion")).expect("an inline credential"),
            Some("from-the-companion".to_owned())
        );
        assert_eq!(
            resolve_api_key(None, None).expect("an endpoint may need no credential"),
            None
        );
    }

    #[test]
    fn a_named_but_empty_credential_file_is_an_error_not_an_absence() {
        let error = resolve_api_key(Some(("/run/secrets/llm.key", "\n")), Some("fallback"))
            .expect_err("an operator who named a file meant to supply a credential")
            .to_string();
        assert!(error.contains("/run/secrets/llm.key"), "got {error:?}");
        assert!(error.contains("empty"), "got {error:?}");
    }

    /// The reference deployment publishes its LiteLLM proxy on the host's
    /// loopback, which a persona in a container of its own network namespace
    /// would dial as itself. The runtime holds the URL, so it is the one
    /// component that can say so before a message ever arrives.
    fn llm(base_url: &str) -> LlmConfig {
        LlmConfig {
            base_url: base_url.to_owned(),
            model: "a-model-the-operator-named".to_owned(),
            api_key: None,
            params: None,
            timeout_seconds: None,
        }
    }

    #[test]
    fn a_loopback_endpoint_is_recognised_as_one() {
        for loopback in [
            "http://127.0.0.1:4000/v1",
            "http://localhost:4000/v1",
            "http://127.1.2.3/v1",
            "http://[::1]:4000/v1",
            "https://LOCALHOST/v1",
        ] {
            assert!(
                llm(loopback).endpoint_is_loopback(),
                "{loopback} is on this host's loopback"
            );
        }
        for reachable in [
            "http://host.docker.internal:4000/v1",
            "http://172.17.0.1:4000/v1",
            "https://api.example.org/v1",
            "http://user:pass@endpoint.example.org:4000/v1",
        ] {
            assert!(
                !llm(reachable).endpoint_is_loopback(),
                "{reachable} is reachable from another network namespace"
            );
        }
    }

    #[test]
    fn the_endpoints_host_is_read_out_of_the_url() {
        for (url, expected) in [
            ("http://127.0.0.1:4000/v1", "127.0.0.1"),
            ("https://api.example.org/v1/", "api.example.org"),
            (
                "http://user:pass@Endpoint.Example.org:4000/v1",
                "endpoint.example.org",
            ),
            ("http://[2001:db8::1]:4000/v1", "2001:db8::1"),
        ] {
            assert_eq!(llm(url).endpoint_host().as_deref(), Some(expected));
        }
    }

    /// ADR 0015's refusal, in the one place it still belongs. The runtime used
    /// to require the endpoint and the model outright; since ticket #184 a
    /// Gateway to read them from is the other way to satisfy it, and the
    /// refusal names both.
    #[test]
    fn a_runtime_with_neither_a_model_nor_a_gateway_refuses_to_start() {
        let mut config = config_with(vec![spec("assistant")]);
        config.llm = OperatorLlm::default();
        let error = config
            .validate()
            .expect_err("nobody named a model and there is nowhere to read one from")
            .to_string();
        assert!(error.contains("HERMES_LLM_BASE_URL"), "got {error:?}");
        assert!(error.contains("HERMES_GATEWAY_URL"), "got {error:?}");

        // Either way out is enough on its own.
        config.gateway_url = Some("http://companion-gateway:8080".to_owned());
        config.gateway_service_token = Some("a-service-token-no-persona-may-hold".to_owned());
        config
            .validate()
            .expect("a Gateway to read the model from is the other half of ADR 0015");
        config.gateway_url = None;
        config.gateway_service_token = None;
        config.llm.base_url = Some("https://endpoint.example.org/v1".to_owned());
        config.llm.model = Some("a-model-the-operator-named".to_owned());
        config
            .validate()
            .expect("an operator who named a model needs no Gateway");
    }

    /// A Gateway URL with no token is a read that could only ever answer
    /// `401`, once a minute, for the life of the deployment: refused once,
    /// here. The mirror case is deliberately **not** an error, on the same
    /// terms as the Sensor's: the URL is what says "read the Gateway", a
    /// credential naming no endpoint opens nothing, and the runtime holding a
    /// secret it does not use is exactly the state
    /// `hermes/tests/harness/runtime.rs` puts it in so that the absence
    /// assertion ADR 0015 asks for has something to be about.
    #[test]
    fn a_gateway_url_with_no_token_is_refused_and_a_token_with_no_url_is_not() {
        let mut config = config_with(vec![spec("assistant")]);
        config.gateway_url = Some("http://companion-gateway:8080".to_owned());
        let error = config
            .validate()
            .expect_err("a URL without a token must not start")
            .to_string();
        assert!(
            error.contains("HERMES_GATEWAY_SERVICE_TOKEN"),
            "got {error:?}"
        );
        config.gateway_url = None;
        config.gateway_service_token = Some("a-service-token-no-persona-may-hold".to_owned());
        config
            .validate()
            .expect("a credential that names no endpoint is not a reason to refuse to start");
        assert!(
            !config.gateway_configured(),
            "and it configures no Gateway: the settings are this host's own"
        );
    }

    #[test]
    fn a_contract_type_maps_to_this_deployments_subject() {
        let mut config = config_with(vec![spec("assistant")]);
        assert_eq!(
            config.subject("fr.linagora.twalk.inbound.message.received.v1"),
            "twalk.inbound.message.received.v1"
        );
        config.subject_prefix = "h23-run".to_owned();
        assert_eq!(
            config.subject("fr.linagora.twalk.consent.state.changed.v1"),
            "h23-run.consent.state.changed.v1"
        );
        assert_eq!(config.stream_subjects(), vec!["h23-run.>".to_owned()]);
    }
}
