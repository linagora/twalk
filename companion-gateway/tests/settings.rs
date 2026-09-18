//! Ticket #98 at the Gateway's process boundary: the model configuration
//! and the user's native language, stored, served, and with the operator's
//! credential file winning over the browser's value.
//!
//! The seam is the real binary, configured through its environment and
//! driven over HTTP — no reach inside the process, and the store is read
//! back through its own bytes when the assertion is about what is *not*
//! there.
//!
//! Four properties this suite exists for, in the order the ticket argues
//! them.
//!
//! 1. **The reference deployment's shape works first.** An
//!    OpenAI-compatible proxy, a model called `qwen`, and a credential in a
//!    file that beats whatever the Companion last wrote (ADR 0015). A model
//!    set from the browser with the key in a file is the normal case, not an
//!    edge one, so it is the case driven here.
//! 2. **The credential is write-only.** It is searched for in every answer
//!    the origin gives and in every log line the process wrote, which is the
//!    only way an absence is ever real (`CONTRIBUTING.md`).
//! 3. **Four causes, four answers.** An endpoint that cannot be reached, one
//!    whose request was refused, one answering something that is not a
//!    completion, and no endpoint configured at all are four different
//!    answers. This project has produced nine incidents in two days from
//!    failures sharing one signal.
//! 4. **The runtime's read is the runtime's.** It takes the service token,
//!    it is the only answer carrying the credential, and a device token does
//!    not open it — which is what keeps a persona from holding the token
//!    that also opens the list of every contact (ADR 0015).

mod harness;

use anyhow::{Context, Result};
use harness::{
    companion_build, ensure_stack, gateway_env_with, gateway_state_dir, poll_until,
    signed_in_device_token, GatewayProc, StubEndpoint, StubLlm, SERVICE_TOKEN,
};
use serde_json::{json, Value};

/// The credential an operator would put in a file on the host — the shape of
/// the reference deployment's `~/deploy/twaky-org-secrets/litellm-master.key`.
const FILE_CREDENTIAL: &str = "sk-the-operators-litellm-master-key";
/// What a developer pastes into the Companion. Deliberately distinctive, so
/// that searching every byte of every answer for it means something.
const BROWSER_CREDENTIAL: &str = "sk-pasted-into-the-browser-h98";

struct Deployment {
    gateway: GatewayProc,
    base: String,
    device: String,
    http: reqwest::Client,
}

impl Deployment {
    /// A Gateway with a signed-in device, optionally holding an operator's
    /// credential file.
    async fn start(test_name: &str, credential_file: Option<&std::path::Path>) -> Result<Self> {
        ensure_stack().await?;
        let static_dir = companion_build(test_name)?;
        let overrides: Vec<(&str, String)> = match credential_file {
            Some(file) => vec![("GATEWAY_LLM_API_KEY_FILE", file.display().to_string())],
            None => Vec::new(),
        };
        let overrides: Vec<(&str, &str)> = overrides
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let gateway = GatewayProc::start(&gateway_env_with(&static_dir, &overrides))?;
        let base = gateway.base_url().await?;
        wait_until_answering(&base).await?;
        let device = signed_in_device_token(&base).await?;
        Ok(Self {
            gateway,
            base,
            device,
            http: reqwest::Client::new(),
        })
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, Value)> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header(
                reqwest::header::COOKIE,
                format!("twalk_device={}", self.device),
            );
        if let Some(body) = body {
            request = request.json(&body);
        }
        answer(request).await
    }

    /// The Hermes runtime's read, with the service token it is configured
    /// with.
    async fn runtime(&self) -> Result<(u16, Value)> {
        answer(
            self.http
                .get(format!("{}/api/settings/runtime", self.base))
                .bearer_auth(SERVICE_TOKEN),
        )
        .await
    }

    async fn get(&self, path: &str) -> Result<(u16, Value)> {
        self.call(reqwest::Method::GET, path, None).await
    }

    async fn put(&self, path: &str, body: Value) -> Result<(u16, Value)> {
        self.call(reqwest::Method::PUT, path, Some(body)).await
    }

    async fn post(&self, path: &str) -> Result<(u16, Value)> {
        self.call(reqwest::Method::POST, path, None).await
    }
}

async fn answer(request: reqwest::RequestBuilder) -> Result<(u16, Value)> {
    let response = request.send().await.context("the gateway did not answer")?;
    let status = response.status().as_u16();
    let text = response.text().await?;
    let body = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text)
            .with_context(|| format!("the answer is not JSON ({status}): {text}"))?
    };
    Ok((status, body))
}

async fn wait_until_answering(base: &str) -> Result<()> {
    poll_until(
        || async {
            reqwest::get(format!("{base}/health"))
                .await
                .ok()?
                .error_for_status()
                .ok()
        },
        "the gateway health endpoint",
    )
    .await?;
    Ok(())
}

fn credential_file(test_name: &str, contents: &str) -> Result<std::path::PathBuf> {
    let path =
        std::env::temp_dir().join(format!("twalk-g98-{test_name}-{}.key", std::process::id()));
    std::fs::write(&path, contents)?;
    Ok(path)
}

/// The configuration the reference deployment actually runs: an
/// OpenAI-compatible proxy in front of the model, a model called `qwen`, and
/// an empty provider passthrough.
fn reference_configuration(base_url: &str) -> Value {
    json!({ "base_url": base_url, "model": "qwen" })
}

// ---------------------------------------------------------------------------
// 1. The reference deployment: the model from the browser, the key from a file
// ---------------------------------------------------------------------------

/// ADR 0015's precedence is not a theoretical one. On the reference
/// deployment the operator's LiteLLM master key is a file on the host and
/// the model name comes from the Companion, so that combination is the path
/// that has to work — and the API has to be able to say which credential is
/// in force, because otherwise "I pasted my key and it is not being used" is
/// unanswerable from the interface.
#[tokio::test]
async fn a_credential_file_wins_over_one_set_from_the_browser_and_the_api_says_so() -> Result<()> {
    let file = credential_file("file-wins", &format!("{FILE_CREDENTIAL}\n"))?;
    let deployment = Deployment::start("settings-file-wins", Some(&file)).await?;

    // Nothing configured yet: a state, not a refusal.
    let (status, unconfigured) = deployment.get("/api/settings/model").await?;
    assert_eq!(status, 200);
    assert_eq!(unconfigured["configured"], json!(false));
    assert_eq!(unconfigured["base_url"], Value::Null);
    assert_eq!(
        unconfigured["credential"]["source"],
        json!("file"),
        "the operator's file is in force before any model is named: a deployment can hold a \
         key and no model, which is where an operator starts"
    );

    // The browser names the model and pastes a key of its own.
    let mut configuration = reference_configuration("http://127.0.0.1:4000/v1");
    configuration["credential"] = json!(BROWSER_CREDENTIAL);
    let (status, written) = deployment.put("/api/settings/model", configuration).await?;
    assert_eq!(status, 200, "{written}");
    assert_eq!(written["configured"], json!(true));
    assert_eq!(written["model"], json!("qwen"));
    assert_eq!(
        written["params"],
        Value::Null,
        "an empty provider passthrough is the expected shape with a proxy in front, not a \
         configuration somebody forgot to finish"
    );
    assert_eq!(
        written["personas"],
        json!({}),
        "the per-persona override ships empty in v0.1, so adding the first one is not a \
         change of shape"
    );

    // The file won, and the browser is told so plainly.
    assert_eq!(written["credential"]["source"], json!("file"));
    assert_eq!(written["credential"]["configured"], json!(true));
    assert_eq!(
        written["credential"]["companion_credential_stored"],
        json!(true),
        "both exist; which one is used is the fact the interface has to be able to explain"
    );
    assert_eq!(
        written["credential"]["file"].as_str(),
        Some(file.display().to_string().as_str()),
        "and it names the file, because 'the file wins' is useless to a human who cannot see \
         which file won"
    );
    assert_eq!(
        written["credential"]["hint"],
        json!("-key"),
        "the last four characters of the credential IN FORCE, so the hint is a hint about the \
         right key"
    );

    // And the runtime is handed the file's credential, not the browser's.
    let (status, runtime) = deployment.runtime().await?;
    assert_eq!(status, 200, "{runtime}");
    assert_eq!(runtime["llm"]["api_key"], json!(FILE_CREDENTIAL));
    assert_eq!(runtime["llm"]["credential_source"], json!("file"));
    assert_eq!(
        runtime["llm"]["base_url"],
        json!("http://127.0.0.1:4000/v1")
    );
    assert_eq!(runtime["llm"]["model"], json!("qwen"));

    // Removing the browser's value changes nothing about what is in force:
    // the file is the operator's and is not the Companion's to remove.
    let (status, cleared) = deployment
        .put(
            "/api/settings/model",
            json!({
                "base_url": "http://127.0.0.1:4000/v1",
                "model": "qwen",
                "credential": null,
            }),
        )
        .await?;
    assert_eq!(status, 200, "{cleared}");
    assert_eq!(cleared["credential"]["source"], json!("file"));
    assert_eq!(
        cleared["credential"]["companion_credential_stored"],
        json!(false)
    );
    let (_, runtime) = deployment.runtime().await?;
    assert_eq!(runtime["llm"]["api_key"], json!(FILE_CREDENTIAL));

    deployment.gateway.stop().await;
    let _ = std::fs::remove_file(&file);
    Ok(())
}

/// Without a file, the browser's value is the one in force — the developer's
/// deployment, which is the other half of the same rule.
#[tokio::test]
async fn without_a_file_the_browsers_credential_is_the_one_in_force() -> Result<()> {
    let deployment = Deployment::start("settings-browser-wins", None).await?;
    let mut configuration = reference_configuration("http://127.0.0.1:4000/v1");
    configuration["credential"] = json!(BROWSER_CREDENTIAL);
    let (status, written) = deployment.put("/api/settings/model", configuration).await?;
    assert_eq!(status, 200, "{written}");
    assert_eq!(written["credential"]["source"], json!("companion"));
    assert_eq!(written["credential"]["file"], Value::Null);

    let (_, runtime) = deployment.runtime().await?;
    assert_eq!(runtime["llm"]["api_key"], json!(BROWSER_CREDENTIAL));
    assert_eq!(runtime["llm"]["credential_source"], json!("companion"));

    // A rename must not lose the key the screen was never shown.
    let (status, renamed) = deployment
        .put(
            "/api/settings/model",
            json!({ "base_url": "http://127.0.0.1:4000/v1", "model": "qwen-2" }),
        )
        .await?;
    assert_eq!(status, 200, "{renamed}");
    assert_eq!(renamed["model"], json!("qwen-2"));
    let (_, runtime) = deployment.runtime().await?;
    assert_eq!(
        runtime["llm"]["api_key"],
        json!(BROWSER_CREDENTIAL),
        "the credential is write-only, so a client editing the model has nothing to send \
         back and an absent member must not mean 'forget it'"
    );

    deployment.gateway.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// 2. The credential is write-only
// ---------------------------------------------------------------------------

/// Asserted the way this project asserts an absence: by looking for the
/// thing in every answer, every log line and every stored byte the Gateway
/// produced. The one place it is allowed to appear is the runtime's read,
/// whose caller is the process that injects it.
#[tokio::test]
async fn no_read_but_the_runtimes_ever_returns_the_credential() -> Result<()> {
    let file = credential_file("write-only", &format!("{FILE_CREDENTIAL}\n"))?;
    let deployment = Deployment::start("settings-write-only", Some(&file)).await?;
    let mut configuration = reference_configuration("http://127.0.0.1:4000/v1");
    configuration["credential"] = json!(BROWSER_CREDENTIAL);
    deployment.put("/api/settings/model", configuration).await?;
    deployment
        .put("/api/settings/language", json!({ "language": "fr" }))
        .await?;

    for path in [
        "/api/settings/model",
        "/api/settings/language",
        "/api/session",
        "/api/devices",
        "/api/deployment",
    ] {
        let (status, body) = deployment.get(path).await?;
        let rendered = body.to_string();
        assert!(
            !rendered.contains(BROWSER_CREDENTIAL) && !rendered.contains(FILE_CREDENTIAL),
            "{path} answered {status} carrying an endpoint credential: {rendered}"
        );
    }

    // The logs: the easiest place in a service to leak a secret into.
    for line in deployment.gateway.logs().await {
        assert!(
            !line.contains(BROWSER_CREDENTIAL) && !line.contains(FILE_CREDENTIAL),
            "a log line carries an endpoint credential: {line}"
        );
    }

    // The runtime's read is the one that carries it, and it is behind the
    // service token.
    let (status, runtime) = deployment.runtime().await?;
    assert_eq!(status, 200);
    assert_eq!(runtime["llm"]["api_key"], json!(FILE_CREDENTIAL));

    deployment.gateway.stop().await;
    let _ = std::fs::remove_file(&file);
    Ok(())
}

/// The two credentials are disjoint, in both directions: a device token
/// opens no runtime read, and the service token opens no device route. A
/// persona is handed neither, and that is what ADR 0015's injection is for.
#[tokio::test]
async fn the_runtimes_read_takes_the_service_token_and_nothing_else() -> Result<()> {
    let deployment = Deployment::start("settings-service-token", None).await?;
    let http = reqwest::Client::new();

    // The owner's own device cookie: refused here.
    let (status, body) = answer(
        http.get(format!("{}/api/settings/runtime", deployment.base))
            .header(
                reqwest::header::COOKIE,
                format!("twalk_device={}", deployment.device),
            ),
    )
    .await?;
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["error"], json!("unauthenticated"));

    // No credential, and a wrong one: the same answer, so a probe learns
    // nothing from the difference.
    for bearer in [None, Some("not-this-gateways-service-token-0123456789")] {
        let mut request = http.get(format!("{}/api/settings/runtime", deployment.base));
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        let (status, body) = answer(request).await?;
        assert_eq!(status, 401, "{body}");
        assert_eq!(body["error"], json!("unauthenticated"));
    }

    // And the other direction: the service token is not a device token.
    let (status, body) = answer(
        http.get(format!("{}/api/settings/model", deployment.base))
            .bearer_auth(SERVICE_TOKEN),
    )
    .await?;
    assert_eq!(
        status, 401,
        "the service token opens the runtime's read and nothing else: {body}"
    );

    deployment.gateway.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. Four causes, four answers
// ---------------------------------------------------------------------------

/// The acceptance criterion the ticket's comment argues for. A persona that
/// cannot reach the endpoint, one whose request the endpoint refused, one
/// pointed at something that is not an LLM at all, and one with no endpoint
/// configured are four distinct answers here — so an operator reads which of
/// the four they have instead of guessing.
#[tokio::test]
async fn an_unreachable_endpoint_a_refused_request_and_no_endpoint_are_different_answers(
) -> Result<()> {
    let deployment = Deployment::start("settings-probe", None).await?;

    // Nothing configured at all.
    let (status, body) = deployment.post("/api/settings/model/probe").await?;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"], json!("model_not_configured"));

    // An endpoint that works: a real OpenAI-compatible answer.
    let llm = StubLlm::start().await?;
    deployment
        .put(
            "/api/settings/model",
            reference_configuration(&llm.base_url()),
        )
        .await?;
    let (status, body) = deployment.post("/api/settings/model/probe").await?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["outcome"], json!("ok"));
    assert_eq!(body["endpoint_status"], json!(200));
    assert_eq!(
        body["endpoint_model"],
        json!("qwen"),
        "the model the endpoint echoed back, which is how an operator confirms what answered"
    );
    let sent = llm.requests();
    assert_eq!(sent.len(), 1, "one completion, and only when a human asked");
    assert_eq!(
        sent[0].body["model"],
        json!("qwen"),
        "the probe sends the model name a persona will send"
    );
    assert_eq!(
        sent[0].body["max_tokens"],
        json!(1),
        "one token: this endpoint is the only one on the origin that spends money"
    );

    // Nothing listening: the loopback trap of the reference deployment, and
    // every other way an address does not answer.
    let dead = harness::unreachable_http_url()?;
    deployment
        .put(
            "/api/settings/model",
            reference_configuration(&format!("{dead}/v1")),
        )
        .await?;
    let (status, body) = deployment.post("/api/settings/model/probe").await?;
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"], json!("endpoint_unreachable"));
    assert_eq!(
        body["endpoint_status"],
        Value::Null,
        "nothing answered, so there is no status to report — which is precisely what tells \
         this apart from a refusal"
    );

    // An endpoint that answered, and said no.
    let refusing = StubEndpoint::answering(
        401,
        "Unauthorized",
        json!({ "error": { "message": "Invalid API key", "type": "authentication_error" } }),
    )
    .await?;
    deployment
        .put(
            "/api/settings/model",
            reference_configuration(&refusing.base_url()),
        )
        .await?;
    let (status, body) = deployment.post("/api/settings/model/probe").await?;
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"], json!("endpoint_refused"));
    assert_eq!(
        body["endpoint_status"],
        json!(401),
        "the endpoint's own status, unedited: it is the operator's most useful clue"
    );
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("Invalid API key"),
        "and its own message, which is never message content: the probe sends `ping`"
    );

    // An endpoint that answered a perfectly good 200 for something that is
    // not a completion: the wrong port.
    let not_an_llm = StubEndpoint::answering(
        200,
        "OK",
        json!({ "status": "ok", "service": "a proxy dashboard" }),
    )
    .await?;
    deployment
        .put(
            "/api/settings/model",
            reference_configuration(&not_an_llm.base_url()),
        )
        .await?;
    let (status, body) = deployment.post("/api/settings/model/probe").await?;
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"], json!("endpoint_not_compatible"));
    assert_eq!(body["endpoint_status"], json!(200));

    // Four codes, four facts.
    deployment.gateway.stop().await;
    Ok(())
}

/// The provider passthrough is the escape hatch and it still has to work:
/// merged last, and a member set to null removing a field the request would
/// otherwise carry (ADR 0015).
#[tokio::test]
async fn the_provider_passthrough_reaches_the_endpoint_untouched() -> Result<()> {
    let deployment = Deployment::start("settings-passthrough", None).await?;
    let llm = StubLlm::start().await?;
    let (status, written) = deployment
        .put(
            "/api/settings/model",
            json!({
                "base_url": llm.base_url(),
                "model": "qwen",
                "params": { "max_tokens": 2000, "stream": null, "reasoning_effort": "medium" },
                "credential": BROWSER_CREDENTIAL,
            }),
        )
        .await?;
    assert_eq!(status, 200, "{written}");
    assert_eq!(
        written["params"],
        json!({ "max_tokens": 2000, "stream": null, "reasoning_effort": "medium" }),
        "the passthrough is stored and served verbatim, nulls included"
    );

    let (status, body) = deployment.post("/api/settings/model/probe").await?;
    assert_eq!(status, 200, "{body}");
    let sent = llm.requests();
    assert_eq!(
        sent[0].body["max_tokens"],
        json!(2000),
        "merged last, so it overrides what the probe itself asked for"
    );
    assert_eq!(
        sent[0].body.get("stream"),
        None,
        "a member set to null removes a field the request would otherwise carry — which is \
         how a provider that rejects one is made to work"
    );
    assert_eq!(sent[0].body["reasoning_effort"], json!("medium"));
    assert_eq!(
        sent[0].authorization.as_deref(),
        Some(format!("Bearer {BROWSER_CREDENTIAL}").as_str()),
        "the credential in force reaches the endpoint, and nowhere else"
    );

    // The runtime is handed the same object, so the persona merges what the
    // probe merged.
    let (_, runtime) = deployment.runtime().await?;
    assert_eq!(
        runtime["llm"]["params"],
        json!({ "max_tokens": 2000, "stream": null, "reasoning_effort": "medium" })
    );

    deployment.gateway.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. What the runtime reads, and what it is told when there is nothing
// ---------------------------------------------------------------------------

/// ADR 0016's fallback finally has a value to fall back to (#164) — and the
/// Gateway holding it is the half of #164 that is the Gateway's.
#[tokio::test]
async fn the_native_language_is_stored_served_and_injected() -> Result<()> {
    let deployment = Deployment::start("settings-language", None).await?;

    let (status, unset) = deployment.get("/api/settings/language").await?;
    assert_eq!(status, 200);
    assert_eq!(
        unset["language"],
        Value::Null,
        "no preference is a state of its own, and it is not English: a persona with no \
         fallback follows the incoming message and nothing else"
    );
    assert_eq!(
        unset["available"],
        json!(["en", "fr", "it", "es", "de"]),
        "the five the Companion ships, in the order it offers them"
    );

    let (status, set) = deployment
        .put("/api/settings/language", json!({ "language": "fr" }))
        .await?;
    assert_eq!(status, 200, "{set}");
    assert_eq!(set["language"], json!("fr"));
    let (_, runtime) = deployment.runtime().await?;
    assert_eq!(runtime["language"], json!("fr"));

    // A sixth language is refused here rather than discovered later by a
    // persona that has no catalogue for it — and the refusal names the five.
    for refused in [json!("fr-FR"), json!("FR"), json!("pt"), json!(7)] {
        let (status, body) = deployment
            .put("/api/settings/language", json!({ "language": refused }))
            .await?;
        assert_eq!(status, 400, "{body}");
        assert_eq!(body["error"], json!("unsupported_language"));
        assert!(
            body["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("en, fr, it, es, de"),
            "the refusal names the choices: {body}"
        );
    }
    // And the refused writes changed nothing.
    let (_, still) = deployment.get("/api/settings/language").await?;
    assert_eq!(still["language"], json!("fr"));

    let (status, forgotten) = deployment
        .put("/api/settings/language", json!({ "language": null }))
        .await?;
    assert_eq!(status, 200, "{forgotten}");
    assert_eq!(forgotten["language"], Value::Null);
    let (_, runtime) = deployment.runtime().await?;
    assert_eq!(runtime["language"], Value::Null);

    deployment.gateway.stop().await;
    Ok(())
}

/// The third of the three signals, one level up from the persona's: a
/// runtime told `llm: null` knows the operator named no model, and that is a
/// different fact from a Gateway it could not reach and from one that
/// refused its token.
#[tokio::test]
async fn a_runtime_with_no_model_configured_is_told_so_rather_than_left_guessing() -> Result<()> {
    let deployment = Deployment::start("settings-no-model", None).await?;

    let (status, runtime) = deployment.runtime().await?;
    assert_eq!(status, 200, "{runtime}");
    assert_eq!(runtime["llm"], Value::Null);
    assert_eq!(runtime["language"], Value::Null);
    assert_eq!(runtime["personas"], json!({}));

    deployment
        .put(
            "/api/settings/model",
            reference_configuration("http://127.0.0.1:4000/v1"),
        )
        .await?;
    let (_, runtime) = deployment.runtime().await?;
    assert_eq!(runtime["llm"]["model"], json!("qwen"));
    assert_eq!(
        runtime["llm"]["api_key"],
        Value::Null,
        "an endpoint may need no credential, and that is not the same as one nobody set"
    );

    // And deleting it puts the deployment back where it started, which is a
    // reachable state and not a wedge.
    let response = deployment
        .http
        .delete(format!("{}/api/settings/model", deployment.base))
        .header(
            reqwest::header::COOKIE,
            format!("twalk_device={}", deployment.device),
        )
        .send()
        .await?;
    assert_eq!(response.status().as_u16(), 204);
    let (_, runtime) = deployment.runtime().await?;
    assert_eq!(runtime["llm"], Value::Null);

    deployment.gateway.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// The store, and what a restart keeps
// ---------------------------------------------------------------------------

/// A configuration set from the browser survives the process: it is in
/// `settings.sqlite3` in the state directory, beside the session store and
/// the consent journal, and a restarted Gateway serves it again. The same
/// test reads the store's own bytes, because "the credential is not in the
/// answer" and "the credential is not stored" are different claims and only
/// the first one is true — the file is not encrypted at rest (#14).
#[tokio::test]
async fn the_configuration_survives_a_restart_and_lands_in_the_state_directory() -> Result<()> {
    ensure_stack().await?;
    let static_dir = companion_build("settings-restart")?;
    let environment = gateway_env_with(&static_dir, &[]);

    let gateway = GatewayProc::start(&environment)?;
    let base = gateway.base_url().await?;
    wait_until_answering(&base).await?;
    let device = signed_in_device_token(&base).await?;
    let http = reqwest::Client::new();
    let cookie = format!("twalk_device={device}");
    answer(
        http.put(format!("{base}/api/settings/model"))
            .header(reqwest::header::COOKIE, &cookie)
            .json(&json!({
                "base_url": "http://127.0.0.1:4000/v1",
                "model": "qwen",
                "credential": BROWSER_CREDENTIAL,
            })),
    )
    .await?;
    answer(
        http.put(format!("{base}/api/settings/language"))
            .header(reqwest::header::COOKIE, &cookie)
            .json(&json!({ "language": "de" })),
    )
    .await?;
    gateway.stop().await;

    let store = gateway_state_dir(&static_dir).join("settings.sqlite3");
    assert!(
        store.is_file(),
        "the settings live in the Gateway's state directory: {}",
        store.display()
    );
    // The store is opened in WAL mode, so a write that has not been
    // checkpointed yet is in `settings.sqlite3-wal` rather than in the
    // database file — both are the state directory, and both are read here.
    let mut bytes = std::fs::read(&store)?;
    if let Ok(wal) = std::fs::read(store.with_extension("sqlite3-wal")) {
        bytes.extend_from_slice(&wal);
    }
    assert!(
        String::from_utf8_lossy(&bytes).contains(BROWSER_CREDENTIAL),
        "the credential is in this file in cleartext, and nothing encrypts it at rest (#14). \
         That is the documented trade-off, not an oversight — the assertion is here so that \
         `docs/architecture/security-model.md` and the code cannot drift apart silently"
    );

    let restarted = GatewayProc::start(&environment)?;
    let base = restarted.base_url().await?;
    wait_until_answering(&base).await?;
    let (status, runtime) = answer(
        http.get(format!("{base}/api/settings/runtime"))
            .bearer_auth(SERVICE_TOKEN),
    )
    .await?;
    assert_eq!(status, 200, "{runtime}");
    assert_eq!(runtime["llm"]["model"], json!("qwen"));
    assert_eq!(runtime["llm"]["api_key"], json!(BROWSER_CREDENTIAL));
    assert_eq!(runtime["language"], json!("de"));

    restarted.stop().await;
    Ok(())
}

/// An operator who named a credential file meant to supply a credential. An
/// empty one is a startup error naming the variable, not a silent fallback
/// to the browser's value — which would be ADR 0015's precedence failing in
/// the direction it exists to prevent.
#[tokio::test]
async fn an_empty_credential_file_stops_the_gateway_rather_than_falling_back() -> Result<()> {
    let static_dir = companion_build("settings-empty-credential-file")?;
    let empty = credential_file("empty", "\n")?;
    let mut gateway = GatewayProc::start(&gateway_env_with(
        &static_dir,
        &[("GATEWAY_LLM_API_KEY_FILE", &empty.display().to_string())],
    ))?;
    let status = gateway.wait_for_exit().await?;
    assert!(
        !status.success(),
        "a Gateway that cannot resolve the credential it was told to \
         use must fail loudly"
    );
    let logs = gateway.logs().await.join("\n");
    assert!(
        logs.contains("GATEWAY_LLM_API_KEY_FILE"),
        "the refusal names the variable at fault: {logs}"
    );
    assert!(
        logs.contains("empty"),
        "and says what is wrong with it: {logs}"
    );
    let _ = std::fs::remove_file(&empty);

    // A path that does not exist is the other half of the same mistake.
    let mut missing = GatewayProc::start(&gateway_env_with(
        &static_dir,
        &[(
            "GATEWAY_LLM_API_KEY_FILE",
            "/nonexistent/twalk-g98/litellm-master.key",
        )],
    ))?;
    let status = missing.wait_for_exit().await?;
    assert!(!status.success());
    assert!(
        missing
            .logs()
            .await
            .join("\n")
            .contains("GATEWAY_LLM_API_KEY_FILE"),
        "and it names the same variable"
    );
    Ok(())
}

/// The refusals a client branches on, each with its own code — so a settings
/// screen can say which field is wrong instead of "something went wrong".
#[tokio::test]
async fn a_malformed_configuration_says_which_part_is_wrong() -> Result<()> {
    let deployment = Deployment::start("settings-refusals", None).await?;
    for (body, code) in [
        (json!({ "model": "qwen" }), "malformed_request"),
        (
            json!({ "base_url": "http://127.0.0.1:4000/v1" }),
            "malformed_request",
        ),
        (
            // The misspelling that would otherwise look like a key that was
            // set: refused, not ignored.
            json!({
                "base_url": "http://127.0.0.1:4000/v1",
                "model": "qwen",
                "credentials": BROWSER_CREDENTIAL,
            }),
            "malformed_request",
        ),
        (
            json!({ "base_url": "127.0.0.1:4000/v1", "model": "qwen" }),
            "invalid_base_url",
        ),
        (
            json!({ "base_url": "http://127.0.0.1:4000/v1", "model": "  " }),
            "invalid_model",
        ),
        (
            json!({ "base_url": "http://127.0.0.1:4000/v1", "model": "qwen", "params": [1] }),
            "invalid_params",
        ),
        (
            json!({ "base_url": "http://127.0.0.1:4000/v1", "model": "qwen", "credential": "" }),
            "invalid_credential",
        ),
    ] {
        let (status, answer) = deployment.put("/api/settings/model", body.clone()).await?;
        assert_eq!(status, 400, "for {body}: {answer}");
        assert_eq!(answer["error"], json!(code), "for {body}: {answer}");
    }
    // Nothing was written by any of them.
    let (_, model) = deployment.get("/api/settings/model").await?;
    assert_eq!(model["configured"], json!(false));

    deployment.gateway.stop().await;
    Ok(())
}
