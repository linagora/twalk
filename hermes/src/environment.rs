//! A persona's environment: **constructed, never inherited**.
//!
//! This module is one function, and it is the reason the module exists as
//! its own file rather than three lines in the supervisor.
//!
//! The runtime holds things a persona has no business holding. The one the
//! ADRs name is the Companion Gateway's service token: it is the credential
//! that reads the LLM configuration the operator set from the Companion,
//! and it is *also* the credential that opens the consent snapshot — the
//! list of every contact the deployment knows (ADR 0010). That is precisely
//! why the runtime injects the model configuration instead of letting each
//! persona fetch it (ADR 0015), and the injection is worth nothing if the
//! child then inherits the token anyway through `environ`.
//!
//! A persona is a separate process (ADR 0008) and a third-party persona
//! takes exactly the same path as `assistant`, so "a first-party persona
//! would never read it" is not a control. The control is that the variable
//! is not there: the child's environment is built from
//! [`persona_environment`] and the operator's named passthrough list, and
//! from nothing else.
//!
//! Since ticket #184 the runtime really does read the Gateway with that token
//! ([`crate::settings`]), which is what makes the property above load-bearing
//! rather than hypothetical: what crosses into the list below is the
//! **answer** — a model name, a language tag, an endpoint credential — and
//! never the credential that fetched it. A persona's environment gains no new
//! credential from that read, and it cannot, because this list is closed.
//!
//! The passthrough list exists because a child still has to be able to
//! `exec`: it defaults to `PATH` alone. An operator who adds to it is
//! making a deliberate, auditable decision — one name at a time, in a
//! variable whose value shows up in `docker inspect`.

use crate::config::{Config, PersonaSpec};
use crate::settings::PersonaSettings;

/// The environment variables the SDK reads
/// (`sdk/python/twalk_sdk/config.py`, `sdk/python/README.md`). Listed here
/// so that "what a persona needs" is one readable list rather than a series
/// of pushes.
pub const PERSONA_ID: &str = "TWALK_PERSONA_ID";
pub const HERMES_DOMAIN: &str = "TWALK_HERMES_DOMAIN";
pub const NATS_URL: &str = "TWALK_NATS_URL";
pub const BUS_STREAM: &str = "TWALK_BUS_STREAM";
pub const BUS_SUBJECT_PREFIX: &str = "TWALK_BUS_SUBJECT_PREFIX";
pub const PERSONA_CONSUMER: &str = "TWALK_PERSONA_CONSUMER";
pub const LLM_BASE_URL: &str = "TWALK_LLM_BASE_URL";
pub const LLM_MODEL: &str = "TWALK_LLM_MODEL";
pub const LLM_API_KEY: &str = "TWALK_LLM_API_KEY";
pub const LLM_PARAMS: &str = "TWALK_LLM_PARAMS";
pub const LLM_TIMEOUT_SECONDS: &str = "TWALK_LLM_TIMEOUT_SECONDS";
pub const SUGGESTION_TTL_SECONDS: &str = "TWALK_SUGGESTION_TTL_SECONDS";
/// The user's own language, for the one thing it governs in a persona: the
/// fallback when the language of the message being answered cannot be told
/// (ADR 0016, ticket #164). It travels this channel and not the persona's own
/// HTTP, for the reason the whole module exists — the token that would read
/// it from the Gateway also opens the consent snapshot.
pub const USER_LANGUAGE: &str = "TWALK_USER_LANGUAGE";
pub const LOG_LEVEL: &str = "TWALK_LOG_LEVEL";

/// Everything one persona process is started with, in a stable order.
///
/// The durable consumer's name is handed over rather than derived twice:
/// the runtime creates the consumer (that is how a paused persona receives
/// nothing — see [`crate::activation`]), so the persona must bind to the
/// one the runtime made.
///
/// `settings` is what the two voices resolved to
/// ([`crate::settings::resolve`]): the model configuration and the user's
/// language, whichever of `.env`, an operator's file or the Companion each
/// field came from. A separate argument rather than a member of `config`
/// because the two are answerable by different people — and because the
/// list below must be handed a **complete** configuration, which is a
/// property `PersonaSettings` has and a half-filled `.env` does not.
pub fn persona_environment(
    config: &Config,
    settings: &PersonaSettings,
    persona: &PersonaSpec,
) -> Vec<(String, String)> {
    let mut environment = vec![
        (PERSONA_ID.to_owned(), persona.id.clone()),
        (HERMES_DOMAIN.to_owned(), config.hermes_domain.clone()),
        (NATS_URL.to_owned(), config.nats_url.clone()),
        (BUS_STREAM.to_owned(), config.stream.clone()),
        (BUS_SUBJECT_PREFIX.to_owned(), config.subject_prefix.clone()),
        (PERSONA_CONSUMER.to_owned(), persona.consumer_name()),
        (LLM_BASE_URL.to_owned(), settings.llm.base_url.clone()),
        (LLM_MODEL.to_owned(), settings.llm.model.clone()),
        (LOG_LEVEL.to_owned(), config.persona_log_level.clone()),
    ];
    // The three optional ones are left out entirely when unset, rather than
    // set to an empty string: the SDK treats an empty value as unset, and
    // an endpoint that needs no credential should show none in the
    // process's environment.
    if let Some(api_key) = &settings.llm.api_key {
        environment.push((LLM_API_KEY.to_owned(), api_key.clone()));
    }
    if let Some(params) = &settings.llm.params {
        environment.push((LLM_PARAMS.to_owned(), params.clone()));
    }
    if let Some(timeout) = &settings.llm.timeout_seconds {
        environment.push((LLM_TIMEOUT_SECONDS.to_owned(), timeout.clone()));
    }
    // How long a suggestion stays approvable (ticket #22). Operator
    // configuration like the model, so it travels the same way: set on the
    // runtime, injected into the persona, never fetched by it.
    if let Some(ttl) = &config.suggestion_ttl_seconds {
        environment.push((SUGGESTION_TTL_SECONDS.to_owned(), ttl.clone()));
    }
    // The user's own language (ADR 0016). Unset is left out entirely rather
    // than passed as an empty string, like the optional ones above: a persona
    // handed no preference writes in each message's own language and says so,
    // which is a different state from one handed a preference it cannot read.
    if let Some(language) = &settings.user_language {
        environment.push((USER_LANGUAGE.to_owned(), language.clone()));
    }
    environment
}

/// The variables of the runtime's own environment the operator named as
/// inheritable, with the values they have here. A name that is unset in the
/// runtime is simply not passed on.
pub fn passthrough(config: &Config) -> Vec<(String, String)> {
    config
        .env_passthrough
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| (name.clone(), value)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LlmConfig, OperatorLlm, PersonaSpec, RestartPolicy};
    use std::time::Duration;

    fn config() -> Config {
        Config {
            personas: vec![persona()],
            hermes_domain: "twalk.example.org".to_owned(),
            nats_url: "nats://bus:4222".to_owned(),
            stream: "twalk".to_owned(),
            subject_prefix: "twalk".to_owned(),
            llm: OperatorLlm::default(),
            gateway_url: Some("http://companion-gateway:8080".to_owned()),
            gateway_service_token: Some("a-service-token-no-persona-may-hold".to_owned()),
            log_level: "info".to_owned(),
            persona_log_level: "debug".to_owned(),
            suggestion_ttl_seconds: Some("900".to_owned()),
            user_language: None,
            restart: RestartPolicy::default(),
            shutdown_grace: Duration::from_secs(10),
            env_passthrough: vec!["PATH".to_owned()],
        }
    }

    /// What the two voices resolved to — here, everything from the Companion,
    /// which is the reference deployment's shape and the one this module had
    /// no way of being handed before ticket #184.
    fn settings() -> PersonaSettings {
        PersonaSettings {
            llm: LlmConfig {
                base_url: "https://endpoint.example.org/v1".to_owned(),
                model: "a-model-the-operator-named".to_owned(),
                api_key: Some("the-operators-endpoint-credential".to_owned()),
                params: Some(r#"{"top_p": 0.9, "temperature": null}"#.to_owned()),
                timeout_seconds: Some("45".to_owned()),
            },
            user_language: Some("fr".to_owned()),
        }
    }

    fn persona() -> PersonaSpec {
        PersonaSpec {
            id: "assistant".to_owned(),
            command: vec!["/bin/true".to_owned()],
        }
    }

    fn value<'a>(environment: &'a [(String, String)], name: &str) -> Option<&'a str> {
        environment
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn a_persona_is_handed_the_model_the_operator_named() {
        let environment = persona_environment(&config(), &settings(), &persona());
        assert_eq!(
            value(&environment, LLM_BASE_URL),
            Some("https://endpoint.example.org/v1")
        );
        assert_eq!(
            value(&environment, LLM_MODEL),
            Some("a-model-the-operator-named")
        );
        assert_eq!(
            value(&environment, LLM_API_KEY),
            Some("the-operators-endpoint-credential")
        );
        assert_eq!(
            value(&environment, LLM_PARAMS),
            Some(r#"{"top_p": 0.9, "temperature": null}"#),
            "the provider parameters reach the persona untouched (ADR 0015)"
        );
        assert_eq!(value(&environment, LLM_TIMEOUT_SECONDS), Some("45"));
    }

    #[test]
    fn a_persona_is_handed_the_consumer_the_runtime_created() {
        let environment = persona_environment(&config(), &settings(), &persona());
        assert_eq!(
            value(&environment, PERSONA_CONSUMER),
            Some("persona-assistant"),
            "the persona binds to the consumer the runtime made, which is what \
             activation moves"
        );
        assert_eq!(value(&environment, BUS_STREAM), Some("twalk"));
        assert_eq!(value(&environment, BUS_SUBJECT_PREFIX), Some("twalk"));
        assert_eq!(value(&environment, NATS_URL), Some("nats://bus:4222"));
        assert_eq!(value(&environment, PERSONA_ID), Some("assistant"));
        assert_eq!(
            value(&environment, HERMES_DOMAIN),
            Some("twalk.example.org"),
            "the authority of the persona's source URI is the runtime's"
        );
        assert_eq!(
            value(&environment, LOG_LEVEL),
            Some("debug"),
            "the persona's log level is its own, not the supervisor's"
        );
    }

    /// ADR 0016's fallback needs a value to fall back to, and this channel is
    /// where it arrives: the preference is the user's, held by the Gateway,
    /// injected here beside the model configuration and the suggestion window
    /// (ticket #164). A persona cannot read a browser's language and must not
    /// hold the token that would let it ask the Gateway.
    #[test]
    fn a_persona_is_handed_the_language_to_fall_back_to() {
        let environment = persona_environment(&config(), &settings(), &persona());
        assert_eq!(
            value(&environment, USER_LANGUAGE),
            Some("fr"),
            "the user's own language reaches the persona, which is the half of \
             ADR 0016 that did not exist"
        );
    }

    /// The assertion ADR 0015 exists for, made the only way it can be made:
    /// the environment is a closed list, so anything the runtime holds and
    /// a persona does not need is absent by construction.
    #[test]
    fn a_persona_is_handed_nothing_beyond_what_the_sdk_reads() {
        let known = [
            PERSONA_ID,
            HERMES_DOMAIN,
            NATS_URL,
            BUS_STREAM,
            BUS_SUBJECT_PREFIX,
            PERSONA_CONSUMER,
            LLM_BASE_URL,
            LLM_MODEL,
            LLM_API_KEY,
            LLM_PARAMS,
            LLM_TIMEOUT_SECONDS,
            SUGGESTION_TTL_SECONDS,
            USER_LANGUAGE,
            LOG_LEVEL,
        ];
        for (name, _) in persona_environment(&config(), &settings(), &persona()) {
            assert!(
                known.contains(&name.as_str()),
                "{name} is not a variable the SDK reads: a persona must be handed nothing it \
                 does not need (ADR 0008)"
            );
            assert!(
                name.starts_with("TWALK_"),
                "{name} is outside the TWALK_ namespace; the runtime's own HERMES_ variables — \
                 the Companion Gateway's service token among them — must not cross (ADR 0015)"
            );
        }
    }

    #[test]
    fn an_endpoint_that_needs_no_credential_shows_none() {
        let mut config = config();
        config.suggestion_ttl_seconds = None;
        let mut settings = settings();
        settings.llm.api_key = None;
        settings.llm.params = None;
        settings.llm.timeout_seconds = None;
        settings.user_language = None;
        let environment = persona_environment(&config, &settings, &persona());
        for absent in [
            LLM_API_KEY,
            LLM_PARAMS,
            LLM_TIMEOUT_SECONDS,
            SUGGESTION_TTL_SECONDS,
            USER_LANGUAGE,
        ] {
            assert_eq!(
                value(&environment, absent),
                None,
                "{absent} must be absent, not empty"
            );
        }
    }

    #[test]
    fn the_passthrough_list_carries_only_the_names_the_operator_gave() {
        let mut config = config();
        config.env_passthrough = vec!["PATH".to_owned(), "A_VARIABLE_NO_ONE_SET_H23".to_owned()];
        let inherited = passthrough(&config);
        assert!(
            inherited.iter().all(|(name, _)| name == "PATH"),
            "only named variables are inherited, and an unset name passes nothing: {inherited:?}"
        );
    }
}
