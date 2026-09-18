//! Ticket #63, the Gateway's OpenAPI description: the Gateway publishes an
//! OpenAPI 3.1 description of its whole HTTP surface, serves it on its own
//! origin, and this suite is what fails when the description and the
//! implementation disagree.
//!
//! # Which way the check runs
//!
//! The description is the source of truth and the implementation is checked
//! against it — not the other way round. Generating a description from the
//! handlers would describe whatever the handlers happen to do, including the
//! mistakes, and the Companion's lot would generate a client from a document
//! nobody reviewed; a description a human wrote and a reviewer diffed is a
//! statement of intent the handlers must live up to. So the loop is: read
//! `openapi.yaml`, drive the real binary through everything it declares, and
//! fail on the first disagreement.
//!
//! Four properties, one per test below, cover both directions of drift:
//!
//! 1. the bytes the origin serves are the committed file, describing this
//!    build (`info.version` against `/health`);
//! 2. every route the router registers is described, and every described
//!    path is a route — a later ticket cannot add an endpoint and forget the
//!    description, which is the obligation spec #46 puts on every ticket
//!    after this one;
//! 3. the authentication each operation declares is the guard's own table
//!    (`session_http::requirement`), so a client cannot guess wrong about a
//!    device token, the refresh token or the Sensor's service token;
//! 4. every declared response is answered as declared: the status, the
//!    media type, and the body against the response's own JSON Schema —
//!    OpenAPI 3.1's schemas *are* JSON Schema 2020-12, which is why the
//!    description is 3.1. `additionalProperties: false` on the response
//!    objects makes that catch an undocumented member too, not only a
//!    missing one. A declared status the suite cannot reach at this seam is
//!    listed, with its reason, in [`UNEXERCISED`] — so a status added to the
//!    description without a test also fails.
//!
//! The seam is the Gateway's process boundary, like the rest of the suite:
//! the real binary, a real Synapse minting the OpenID tokens, HTTP calls
//! from the test. The two static tests (2 and 3) need no process.

mod harness;

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use harness::stub_bridge::{COOKIES_FLOW, COOKIES_STEP, QR_FLOW};
use harness::{
    bridge_status_path, companion_build, ensure_stack, fresh_owner_user_id, gateway_env,
    gateway_env_with, gateway_env_with_bridges_and_consent, gateway_env_with_consent,
    gateway_env_without_sign_in, missing_static_dir, nats_url, owner_user_id, poll_until,
    GatewayProc, MatrixUser, StubBridge, FALLBACK_HTML, INDEX_HTML, OTHER_LOCALPART,
    OWNER_LOCALPART, SERVER_NAME, SERVICE_TOKEN, STUB_AS_TOKEN, STUB_BRIDGE_ID,
    STUB_STATUS_BRIDGE_ID, UNREACHABLE_BRIDGE_ID, UNREACHABLE_STATUS_BRIDGE_ID,
};
use reqwest::Method;
use serde_json::{json, Value};
use twalk_companion_gateway::bridge_status::is_reserved_path;
use twalk_companion_gateway::session_http::{requirement, Requirement};

/// Routes the router registers that the description deliberately does not
/// declare as operations, with the reason. Anything else registered and
/// undescribed fails test 2.
const UNDESCRIBED_ROUTES: &[(&str, &str)] = &[
    // The JSON 404 catch-all. OpenAPI path templating has no wildcard, so
    // there is no honest way to write `/api/{*rest}` as a path item — and it
    // is not an endpoint a client calls on purpose. It is described in prose
    // (`info.description`) and by the `path` member of the `Error` schema,
    // and exercised by `an_undescribed_api_path_answers_the_json_404`.
    ("/api", "the JSON 404 catch-all, described in prose"),
    ("/api/{*rest}", "the JSON 404 catch-all, described in prose"),
];

/// Declared responses this suite cannot produce at the process boundary,
/// with the reason. Everything else declared must be exercised.
const UNEXERCISED: &[(&str, &str, &str, &str)] = &[
    (
        "post",
        "/api/session",
        "500",
        "store_failed needs the Gateway's own SQLite file to fail under a running process: a fault-injection seam this suite does not have",
    ),
    (
        "delete",
        "/api/session",
        "500",
        "store_failed, as above",
    ),
    ("get", "/api/devices", "500", "store_failed, as above"),
    (
        "delete",
        "/api/devices/{id}",
        "500",
        "store_failed, as above",
    ),
    (
        "post",
        "/api/consent/decisions",
        "200",
        "the already-recorded answer needs two identical decisions inside one millisecond — the Gateway stamps occurred_at, which is part of the id, so this seam cannot force the collision; `store::tests::the_identical_decision_arriving_twice_records_once` covers it",
    ),
    (
        "post",
        "/api/consent/decisions",
        "500",
        "store_unavailable needs the consent journal to fail under a running process: the same fault-injection seam this suite does not have",
    ),
    (
        "get",
        "/api/consent/state",
        "500",
        "store_unavailable, as above",
    ),
    (
        "get",
        "/api/consent/effective",
        "500",
        "store_unavailable, as above",
    ),
    (
        "post",
        "/_twalk/bridges/{bridge_id}/status",
        "500",
        "store_unavailable needs the Gateway's own SQLite file to fail under a running process: the same fault-injection seam this suite does not have",
    ),
];

// ---------------------------------------------------------------------------
// The description
// ---------------------------------------------------------------------------

/// The committed description, parsed. YAML is read into `serde_json::Value`
/// so the OpenAPI schemas can be handed to a JSON Schema validator as they
/// are.
struct Description {
    doc: Value,
}

impl Description {
    fn path() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("openapi.yaml")
    }

    fn load() -> Result<Self> {
        let text = std::fs::read_to_string(Self::path())
            .with_context(|| format!("failed to read {}", Self::path().display()))?;
        Self::parse(&text)
    }

    fn parse(text: &str) -> Result<Self> {
        let doc: Value =
            serde_yaml_ng::from_str(text).context("the description is not valid YAML")?;
        let version = doc["openapi"]
            .as_str()
            .context("the description names no OpenAPI version")?;
        anyhow::ensure!(
            version.starts_with("3.1"),
            "the description must be OpenAPI 3.1 (its schemas are then JSON Schema 2020-12): {version}"
        );
        Ok(Self { doc })
    }

    /// Every declared operation, as (lowercase method, path template).
    fn operations(&self) -> Vec<(String, String)> {
        let mut operations = Vec::new();
        for (path, item) in self.paths() {
            let object = item.as_object().expect("a path item is an object");
            for method in object.keys() {
                if is_http_method(method) {
                    operations.push((method.clone(), path.clone()));
                }
            }
        }
        operations.sort();
        operations
    }

    fn paths(&self) -> Vec<(String, &Value)> {
        self.doc["paths"]
            .as_object()
            .expect("the description declares paths")
            .iter()
            .map(|(path, item)| (path.clone(), item))
            .collect()
    }

    fn operation(&self, method: &str, path: &str) -> Result<&Value> {
        self.doc["paths"]
            .get(path)
            .and_then(|item| item.get(method))
            .ok_or_else(|| {
                anyhow!(
                    "the description declares no {} {path}",
                    method.to_uppercase()
                )
            })
    }

    /// One declared response, with `$ref`s to `components/responses`
    /// followed.
    fn response(&self, method: &str, path: &str, status: u16) -> Result<&Value> {
        let responses = self
            .operation(method, path)?
            .get("responses")
            .ok_or_else(|| anyhow!("{method} {path} declares no responses"))?;
        let declared = responses.get(status.to_string()).ok_or_else(|| {
            anyhow!(
                "{} {path} answered {status}, which the description does not declare (it declares {})",
                method.to_uppercase(),
                responses
                    .as_object()
                    .map(|object| object.keys().cloned().collect::<Vec<_>>().join(", "))
                    .unwrap_or_default()
            )
        })?;
        self.resolve(declared)
    }

    /// Follows a local `$ref`, once (the description nests no deeper).
    fn resolve<'a>(&'a self, value: &'a Value) -> Result<&'a Value> {
        match value.get("$ref").and_then(Value::as_str) {
            None => Ok(value),
            Some(reference) => {
                let pointer = reference
                    .strip_prefix('#')
                    .ok_or_else(|| anyhow!("only local refs are supported: {reference}"))?;
                self.doc
                    .pointer(pointer)
                    .ok_or_else(|| anyhow!("dangling ref: {reference}"))
            }
        }
    }

    /// A validator for one schema of the description. The whole
    /// `components` section is carried into the schema document's root, so
    /// the schema's own `$ref`s resolve exactly as they read.
    fn validator(&self, schema: &Value) -> Result<jsonschema::Validator> {
        let mut document = schema.clone();
        let object = document
            .as_object_mut()
            .context("a response schema is an object")?;
        object.insert(
            "$schema".to_owned(),
            json!("https://json-schema.org/draft/2020-12/schema"),
        );
        object.insert("components".to_owned(), self.doc["components"].clone());
        jsonschema::validator_for(&document)
            .map_err(|error| anyhow!("the description holds an invalid schema: {error}"))
    }

    fn validate(&self, schema: &Value, instance: &Value) -> Result<()> {
        let validator = self.validator(schema)?;
        let errors: Vec<String> = validator
            .iter_errors(instance)
            .map(|error| format!("  - {}: {error}", error.instance_path()))
            .collect();
        if !errors.is_empty() {
            bail!(
                "the answer does not match the schema the description declares:\n{}\nthe answer was: {}",
                errors.join("\n"),
                serde_json::to_string(instance).unwrap_or_default()
            );
        }
        Ok(())
    }
}

fn is_http_method(key: &str) -> bool {
    matches!(
        key,
        "get" | "put" | "post" | "delete" | "options" | "head" | "patch" | "trace"
    )
}

// ---------------------------------------------------------------------------
// Driving the Gateway, and checking each answer against the description
// ---------------------------------------------------------------------------

/// An answer that matched the description: its body, for the test to read an
/// id out of, and its `Set-Cookie` values, for the test to carry the
/// credentials it was just issued.
struct Checked {
    body: Value,
    cookies: Vec<String>,
}

/// One HTTP call, checked against the description.
///
/// `template` is the path as the description spells it (`/api/devices/{id}`)
/// and `target` the path actually requested — they differ only where a path
/// has a parameter.
struct Call<'a> {
    client: &'a reqwest::Client,
    description: &'a Description,
    /// Every (method, template, status) this suite has checked, so the
    /// coverage assertion at the end can name what the description declares
    /// and nobody exercised.
    exercised: &'a mut BTreeSet<(String, String, String)>,
}

impl Call<'_> {
    #[allow(clippy::too_many_arguments)]
    async fn check(
        &mut self,
        method: Method,
        base: &str,
        template: &str,
        target: &str,
        cookies: &[(&str, &str)],
        body: Option<Value>,
        expected_status: u16,
        expected_error: Option<&str>,
    ) -> Result<Checked> {
        self.check_with_bearer(
            method,
            base,
            template,
            target,
            cookies,
            None,
            body,
            expected_status,
            expected_error,
        )
        .await
    }

    /// The same check, with a bearer credential: the consent snapshot's
    /// service token (#50), which is the one endpoint of this origin that
    /// takes one. Kept as a separate entry point rather than a tenth
    /// parameter on [`Self::check`], so that the many calls that send a
    /// cookie stay as readable as they were.
    #[allow(clippy::too_many_arguments)]
    async fn check_with_bearer(
        &mut self,
        method: Method,
        base: &str,
        template: &str,
        target: &str,
        cookies: &[(&str, &str)],
        bearer: Option<&str>,
        body: Option<Value>,
        expected_status: u16,
        expected_error: Option<&str>,
    ) -> Result<Checked> {
        let mut request = self
            .client
            .request(method.clone(), format!("{base}{target}"));
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        if !cookies.is_empty() {
            let header = cookies
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; ");
            request = request.header(reqwest::header::COOKIE, header);
        }
        if let Some(body) = &body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("failed to call {method} {target}"))?;

        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(essence)
            .unwrap_or_default();
        let cookies: Vec<String> = response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .map(str::to_owned)
            .collect();
        let text = response.text().await?;
        let method_key = method.as_str().to_lowercase();

        anyhow::ensure!(
            status == expected_status,
            "{method} {target} answered {status}, expected {expected_status}: {text}"
        );
        let declared = self
            .description
            .response(&method_key, template, status)
            .with_context(|| format!("{method} {target} answered {status}"))?;
        self.exercised
            .insert((method_key.clone(), template.to_owned(), status.to_string()));

        let body = match declared.get("content") {
            // A response the description declares without a body — a 204, a
            // 307 — must not carry one.
            None => {
                anyhow::ensure!(
                    text.is_empty(),
                    "the description declares no body for {method} {template} {status}, but the answer carried one: {text}"
                );
                Value::Null
            }
            Some(content) => {
                let media_types = content
                    .as_object()
                    .context("a response's content is a map of media types")?;
                anyhow::ensure!(
                    media_types.contains_key(&content_type) || media_types.contains_key("*/*"),
                    "{method} {template} answered {status} as {content_type}, which the description does not declare (it declares {})",
                    media_types.keys().cloned().collect::<Vec<_>>().join(", ")
                );
                if content_type == "application/json" {
                    let body: Value = serde_json::from_str(&text)
                        .with_context(|| format!("the answer is not JSON: {text}"))?;
                    let schema = media_types
                        .get(&content_type)
                        .and_then(|media| media.get("schema"))
                        .ok_or_else(|| {
                            anyhow!("{method} {template} {status} declares no schema for JSON")
                        })?;
                    self.description
                        .validate(schema, &body)
                        .with_context(|| format!("{method} {template} answered {status}"))?;
                    body
                } else {
                    anyhow::ensure!(
                        !text.is_empty(),
                        "{method} {template} answered {status} with an empty body"
                    );
                    Value::String(text)
                }
            }
        };

        if let Some(expected_error) = expected_error {
            anyhow::ensure!(
                body["error"].as_str() == Some(expected_error),
                "{method} {target} answered {status} with error {:?}, expected {expected_error:?} — \
                 the code a client branches on is part of the description",
                body["error"]
            );
        }
        Ok(Checked { body, cookies })
    }
}

/// The media type without its parameters: `text/plain; charset=utf-8` is
/// `text/plain`. Parameters are real (the metrics endpoint's
/// `version=0.0.4`, the WebAssembly module's deliberate absence of any) and
/// asserted where they matter — by `tests/service.rs`, on the exact header.
fn essence(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// A client that follows no redirect and keeps no cookie: the tests drive
/// the session cookies by hand, as `tests/signin.rs` does.
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

fn cookie(response_cookies: &[String], name: &str) -> Option<String> {
    response_cookies.iter().find_map(|value| {
        let (pair, _) = value.split_once(';').unwrap_or((value.as_str(), ""));
        let (key, cookie_value) = pair.split_once('=')?;
        (key == name && !cookie_value.is_empty()).then(|| cookie_value.to_owned())
    })
}

/// Signs a device in and returns its two cookies, unchecked: the checked
/// sign-in is the one the conformance test itself drives.
async fn sign_in_cookies(
    client: &reqwest::Client,
    base: &str,
    user: &MatrixUser,
    device_name: &str,
) -> Result<(String, String)> {
    let token = user.openid_token().await?;
    let response = client
        .post(format!("{base}/api/session"))
        .json(&json!({ "matrix_openid_token": token, "device_name": device_name }))
        .send()
        .await?;
    anyhow::ensure!(
        response.status().is_success(),
        "a sign-in the test relies on failed: {}",
        response.text().await.unwrap_or_default()
    );
    let cookies: Vec<String> = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::to_owned)
        .collect();
    Ok((
        cookie(&cookies, "twalk_device").context("the sign-in set no device cookie")?,
        cookie(&cookies, "twalk_refresh").context("the sign-in set no refresh cookie")?,
    ))
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

/// A fully configured Gateway with a Companion build of its own.
async fn start(test_name: &str) -> Result<(GatewayProc, String)> {
    let static_dir = companion_build(test_name)?;
    let gateway = GatewayProc::start(&gateway_env(&static_dir))?;
    let base = gateway.base_url().await?;
    wait_until_answering(&base).await?;
    Ok((gateway, base))
}

// ---------------------------------------------------------------------------
// 1. The served description is the committed file, and describes this build
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_origin_serves_the_committed_description_of_this_build() -> Result<()> {
    let (gateway, base) = start("openapi-served").await?;
    let client = client()?;

    let response = client.get(format!("{base}/openapi.yaml")).send().await?;
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/yaml"),
        "the description is served as YAML (RFC 9512), so a generator pointed at \
         the origin gets a document and not a download"
    );
    let served = response.text().await?;
    let committed = std::fs::read_to_string(Description::path())?;
    assert_eq!(
        served, committed,
        "the origin must serve the committed description byte for byte — the binary \
         embeds the file, so a difference here means the bytes were copied somewhere"
    );

    // And it describes *this* build: the version the description names is
    // the version the binary reports.
    let description = Description::parse(&served)?;
    let health: Value = client
        .get(format!("{base}/health"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(
        description.doc["info"]["version"].as_str(),
        health["version"].as_str(),
        "the description's info.version is the Gateway's version, so a released \
         binary always carries a description of itself"
    );

    gateway.stop().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// 2. Every route is described, and every described path is a route
// ---------------------------------------------------------------------------

/// The routes the router registers, read out of the crate's own source: the
/// path literal of every `.route(...)` call, with the methods registered on
/// it.
///
/// Reading the source is the seam available: axum's `Router` does not expose
/// what it holds, and an in-code list of routes to compare the description
/// against would be a third place to forget. A `.route` call whose path is
/// not a literal is an error rather than a silent miss.
fn registered_routes() -> Result<HashMap<String, BTreeSet<String>>> {
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut routes: HashMap<String, BTreeSet<String>> = HashMap::new();
    for entry in std::fs::read_dir(&source_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let source = uncommented(&std::fs::read_to_string(&path)?);
        for call in route_calls(&source) {
            let (route_path, arguments) = call?;
            let methods = routes.entry(route_path).or_default();
            for method in [
                "get", "post", "put", "delete", "patch", "head", "options", "any",
            ] {
                if mentions_call(&arguments, method) {
                    methods.insert(method.to_owned());
                }
            }
        }
    }
    anyhow::ensure!(
        !routes.is_empty(),
        "no routes found in {}: the scanner is broken, not the router",
        source_dir.display()
    );
    Ok(routes)
}

/// Line comments removed, so a `.route(` in a doc comment is not a route.
fn uncommented(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Each `.route(...)` call of a source file: its path literal and the whole
/// text of its arguments.
fn route_calls(source: &str) -> Vec<Result<(String, String)>> {
    let mut calls = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find(".route(") {
        let arguments_start = at + ".route(".len();
        let arguments = match balanced(&rest[arguments_start..]) {
            Some(arguments) => arguments,
            None => {
                calls.push(Err(anyhow!("unbalanced .route( call")));
                break;
            }
        };
        let literal = arguments.trim_start();
        match literal.strip_prefix('"').and_then(|rest| {
            rest.find('"')
                .map(|end| (rest[..end].to_owned(), arguments.to_owned()))
        }) {
            Some(call) => calls.push(Ok(call)),
            None => calls.push(Err(anyhow!(
                "a route is registered with a path that is not a string literal, \
                 so the description cannot be checked against it: .route({literal})"
            ))),
        }
        rest = &rest[arguments_start + arguments.len()..];
    }
    calls
}

/// The text up to the parenthesis matching the one just opened.
fn balanced(after_open: &str) -> Option<&str> {
    let mut depth = 1usize;
    for (at, character) in after_open.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&after_open[..at]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether the arguments call `name(`, as a whole word — so `get(current)`
/// counts and `target(x)` does not.
fn mentions_call(arguments: &str, name: &str) -> bool {
    let needle = format!("{name}(");
    let mut rest = arguments;
    while let Some(at) = rest.find(&needle) {
        let preceding = rest[..at].chars().next_back();
        if !preceding.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            return true;
        }
        rest = &rest[at + needle.len()..];
    }
    false
}

#[test]
fn every_route_the_router_registers_is_described() -> Result<()> {
    let description = Description::load()?;
    let registered = registered_routes()?;
    let declared = description.operations();

    for (route_path, methods) in &registered {
        if let Some((_, reason)) = UNDESCRIBED_ROUTES
            .iter()
            .find(|(exempt, _)| exempt == route_path)
        {
            assert!(
                !description.doc["paths"]
                    .as_object()
                    .expect("paths")
                    .contains_key(route_path),
                "{route_path} is listed as undescribed ({reason}) but the description \
                 declares it: drop it from UNDESCRIBED_ROUTES"
            );
            continue;
        }
        for method in methods {
            // `any(...)` on a described path would mean the description
            // cannot enumerate its methods; no such route exists.
            assert_ne!(
                method, "any",
                "{route_path} is registered for every method, which no description can \
                 enumerate: either narrow the route or list it in UNDESCRIBED_ROUTES"
            );
            assert!(
                declared.contains(&(method.clone(), route_path.clone())),
                "the router answers {} {route_path} and the description does not declare it. \
                 Every ticket of spec #46 that adds an endpoint extends openapi.yaml in the \
                 same commit — that is what this assertion is for.",
                method.to_uppercase()
            );
        }
    }

    // And the other way: nothing described that is not served. The
    // Companion's own surface is the exception — it is the router's
    // `fallback`, not a route, and one templated segment stands for every
    // path the Gateway's routes do not claim.
    for (method, path) in &declared {
        if path == "/{companionPath}" {
            continue;
        }
        let methods = registered.get(path).unwrap_or_else(|| {
            panic!("the description declares {path}, which the router does not register")
        });
        assert!(
            methods.contains(method),
            "the description declares {} {path}, which the router does not register",
            method.to_uppercase()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. The described authentication is the guard's own table
// ---------------------------------------------------------------------------

#[test]
fn the_described_authentication_is_the_guards_own_table() -> Result<()> {
    let description = Description::load()?;
    let schemes = description.doc["components"]["securitySchemes"]
        .as_object()
        .context("the description declares its security schemes")?;

    for (method, path) in description.operations() {
        let operation = description.operation(&method, &path)?;
        let security = operation
            .get("security")
            .unwrap_or_else(|| {
                panic!(
                    "{} {path} declares no `security`: an endpoint that does not say what it \
                     requires is exactly what a client guesses wrong about",
                    method.to_uppercase()
                )
            })
            .as_array()
            .context("`security` is an array")?;
        assert!(
            security.len() <= 1,
            "{} {path} declares several alternative credentials, which the Gateway has \
             no endpoint for",
            method.to_uppercase()
        );
        let declared_scheme = security.first().and_then(|requirement| {
            requirement
                .as_object()
                .and_then(|object| object.keys().next().cloned())
        });
        if let Some(scheme) = &declared_scheme {
            assert!(
                schemes.contains_key(scheme),
                "{} {path} requires the undeclared security scheme {scheme}",
                method.to_uppercase()
            );
        }

        let described = match declared_scheme.as_deref() {
            None => Requirement::Open,
            Some("deviceToken") => Requirement::DeviceToken,
            Some("refreshToken") => Requirement::RefreshToken,
            Some("serviceToken") => Requirement::ServiceToken,
            Some("bridgeAsToken") => Requirement::BridgeToken,
            Some(other) => panic!("{other} is not one of the Gateway's credentials"),
        };

        // The guard runs under `/api` and under the Gateway's own reserved
        // `/_twalk/` prefix, where the bridge status webhook lives (#56).
        // Everywhere else — the Companion's files, `/health`, `/metrics`,
        // this description — it does not run at all, so an operation there
        // cannot require a credential.
        if !path.starts_with("/api") && !is_reserved_path(&path) {
            assert_eq!(
                described,
                Requirement::Open,
                "{} {path} is outside the guard's scope: it cannot require a credential",
                method.to_uppercase()
            );
            continue;
        }
        // The guard reads a request path, so a templated segment gets a
        // value; `requirement` matches on whole paths, so any value does.
        let request_path = path.replace("{id}", "a-device-id");
        let implemented = requirement(
            &Method::from_bytes(method.to_uppercase().as_bytes()).expect("a method"),
            &request_path,
        );
        assert_eq!(
            described,
            implemented,
            "the description says {} {path} takes {described:?} and the guard requires \
             {implemented:?} — one of the two is a bug, and a client that believes the \
             description would be refused",
            method.to_uppercase()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. Every declared response is answered as declared
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_described_response_is_answered_as_described() -> Result<()> {
    ensure_stack().await?;
    let description = Description::load()?;
    let http = client()?;
    let mut exercised = BTreeSet::new();
    let mut call = Call {
        client: &http,
        description: &description,
        exercised: &mut exercised,
    };

    let (gateway, base) = start("openapi-conformance").await?;
    let owner = MatrixUser::login(OWNER_LOCALPART).await?;
    let other = MatrixUser::login(OTHER_LOCALPART).await?;

    // --- the operator's endpoints, and the description itself
    let health = call
        .check(
            Method::GET,
            &base,
            "/health",
            "/health",
            &[],
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(health.body["status"].as_str(), Some("ok"));
    call.check(
        Method::GET,
        &base,
        "/metrics",
        "/metrics",
        &[],
        None,
        200,
        None,
    )
    .await?;
    call.check(
        Method::GET,
        &base,
        "/openapi.yaml",
        "/openapi.yaml",
        &[],
        None,
        200,
        None,
    )
    .await?;

    // --- sign-in: the happy path, and the cookies it sets
    let token = owner.openid_token().await?;
    let signed_in = call
        .check(
            Method::POST,
            &base,
            "/api/session",
            "/api/session",
            &[],
            Some(json!({ "matrix_openid_token": token, "device_name": "the conformance device" })),
            200,
            None,
        )
        .await?;
    let owner_id = owner_user_id();
    assert_eq!(signed_in.body["owner"].as_str(), Some(owner_id.as_str()));
    // The same token again: verification does not consume it at the
    // homeserver, so the Gateway's own ledger is what refuses the replay.
    call.check(
        Method::POST,
        &base,
        "/api/session",
        "/api/session",
        &[],
        Some(json!({ "matrix_openid_token": token })),
        401,
        Some("openid_token_replayed"),
    )
    .await?;

    // A device's cookies, for the authenticated operations. Taken from a
    // fresh sign-in rather than the checked one above, because the checked
    // call reads the body and drops the headers.
    let (device, refresh) = sign_in_cookies(&http, &base, &owner, "the driving device").await?;
    let device_cookie = [("twalk_device", device.as_str())];

    // --- the session and the device list
    call.check(
        Method::GET,
        &base,
        "/api/session",
        "/api/session",
        &device_cookie,
        None,
        200,
        None,
    )
    .await?;
    let (doomed, _) = sign_in_cookies(&http, &base, &owner, "the revoked device").await?;
    let devices = call
        .check(
            Method::GET,
            &base,
            "/api/devices",
            "/api/devices",
            &device_cookie,
            None,
            200,
            None,
        )
        .await?;
    let doomed_id = devices.body["devices"]
        .as_array()
        .context("the device list is an array")?
        .iter()
        .find(|device| device["name"].as_str() == Some("the revoked device"))
        .and_then(|device| device["id"].as_str())
        .context("the device list does not hold the device that just signed in")?
        .to_owned();
    // The cookie of a device that is about to be revoked still works.
    call.check(
        Method::GET,
        &base,
        "/api/session",
        "/api/session",
        &[("twalk_device", doomed.as_str())],
        None,
        200,
        None,
    )
    .await?;
    call.check(
        Method::DELETE,
        &base,
        "/api/devices/{id}",
        &format!("/api/devices/{doomed_id}"),
        &device_cookie,
        None,
        204,
        None,
    )
    .await?;
    // Revocation is immediate: the revoked device's own cookie is now
    // indistinguishable from no cookie at all.
    call.check(
        Method::GET,
        &base,
        "/api/session",
        "/api/session",
        &[("twalk_device", doomed.as_str())],
        None,
        401,
        Some("unauthenticated"),
    )
    .await?;
    call.check(
        Method::DELETE,
        &base,
        "/api/devices/{id}",
        "/api/devices/no-such-device",
        &device_cookie,
        None,
        404,
        Some("not_found"),
    )
    .await?;

    // --- refresh, then sign out with the device token it just issued: the
    // refresh rotates both tokens, so the pair the refresh answers with is
    // the only one that still works.
    let refreshed = call
        .check(
            Method::POST,
            &base,
            "/api/session/refresh",
            "/api/session/refresh",
            &[("twalk_refresh", refresh.as_str())],
            None,
            200,
            None,
        )
        .await?;
    let rotated_device =
        cookie(&refreshed.cookies, "twalk_device").context("the refresh set no device cookie")?;
    call.check(
        Method::DELETE,
        &base,
        "/api/session",
        "/api/session",
        &[("twalk_device", rotated_device.as_str())],
        None,
        204,
        None,
    )
    .await?;

    // --- the refusals every /api endpoint shares: no credential at all
    for (method, template, target) in [
        (Method::GET, "/api/session", "/api/session"),
        (Method::DELETE, "/api/session", "/api/session"),
        (Method::GET, "/api/devices", "/api/devices"),
        (
            Method::DELETE,
            "/api/devices/{id}",
            "/api/devices/a-device-id",
        ),
        (Method::POST, "/api/session/refresh", "/api/session/refresh"),
        (Method::POST, "/api/bootstrap/rooms", "/api/bootstrap/rooms"),
        (
            Method::POST,
            "/api/consent/decisions",
            "/api/consent/decisions",
        ),
        (Method::GET, "/api/consent/state", "/api/consent/state"),
        (
            Method::GET,
            "/api/consent/effective",
            "/api/consent/effective?contact=%40a%3Atest.twalk&network=whatsapp",
        ),
        // The snapshot's refusal has a different reason — no service token
        // rather than no device token — and deliberately the same answer.
        (
            Method::GET,
            "/api/consent/snapshot",
            "/api/consent/snapshot",
        ),
        (Method::GET, "/api/bridges", "/api/bridges"),
        (
            Method::GET,
            "/api/bridges/{bridge_id}/login/flows",
            "/api/bridges/mautrix-whatsapp/login/flows",
        ),
        (
            Method::POST,
            "/api/bridges/{bridge_id}/login",
            "/api/bridges/mautrix-whatsapp/login",
        ),
        (
            Method::GET,
            "/api/bridges/{bridge_id}/login",
            "/api/bridges/mautrix-whatsapp/login",
        ),
        (
            Method::DELETE,
            "/api/bridges/{bridge_id}/login",
            "/api/bridges/mautrix-whatsapp/login",
        ),
        (
            Method::POST,
            "/api/bridges/{bridge_id}/login/submit",
            "/api/bridges/mautrix-whatsapp/login/submit",
        ),
        (
            Method::GET,
            "/api/bridges/{bridge_id}/logins",
            "/api/bridges/mautrix-whatsapp/logins",
        ),
        (
            Method::DELETE,
            "/api/bridges/{bridge_id}/logins/{login_id}",
            "/api/bridges/mautrix-whatsapp/logins/a-login",
        ),
    ] {
        call.check(
            method,
            &base,
            template,
            target,
            &[],
            None,
            401,
            Some("unauthenticated"),
        )
        .await?;
    }

    // --- consent, on a Gateway that has no bus: signed in, and told so
    // rather than told nothing. That Gateway is the one started above —
    // `gateway_env` configures sign-in and no bus, which is a deployment an
    // operator can have. A device of its own, because the one above was
    // rotated and then signed out.
    let (consent_device, _) = sign_in_cookies(&http, &base, &owner, "the consent device").await?;
    let consent_cookie = [("twalk_device", consent_device.as_str())];
    // A live device token, on the one route that does not take one: the two
    // credentials are disjoint, which is a property a client must not have to
    // guess at.
    call.check(
        Method::GET,
        &base,
        "/api/consent/snapshot",
        "/api/consent/snapshot",
        &consent_cookie,
        None,
        401,
        Some("unauthenticated"),
    )
    .await?;
    for (method, template, target, body) in [
        (
            Method::POST,
            "/api/consent/decisions",
            "/api/consent/decisions",
            Some(json!({
                "subject": { "type": "contact", "id": "@whatsapp_33612345678:test.twalk" },
                "new_state": "granted",
                "scope": { "networks": ["whatsapp"] }
            })),
        ),
        (
            Method::GET,
            "/api/consent/state",
            "/api/consent/state",
            None,
        ),
        (
            Method::GET,
            "/api/consent/effective",
            "/api/consent/effective?contact=%40whatsapp_33612345678%3Atest.twalk&network=whatsapp",
            None,
        ),
    ] {
        call.check(
            method,
            &base,
            template,
            target,
            &consent_cookie,
            body,
            503,
            Some("consent_not_configured"),
        )
        .await?;
    }
    // The snapshot answers the same way, to the service token this Gateway
    // does have: authentication first, then the half that is missing.
    call.check_with_bearer(
        Method::GET,
        &base,
        "/api/consent/snapshot",
        "/api/consent/snapshot",
        &[],
        Some(SERVICE_TOKEN),
        None,
        503,
        Some("consent_not_configured"),
    )
    .await?;

    // And so does the bridge status webhook (#56): with no bus there is
    // nowhere to record a transition and nowhere to publish it, and saying
    // so beats accepting a push the Gateway would throw away. Answered
    // before the token is looked at, as the snapshot's own 503 is.
    call.check_with_bearer(
        Method::POST,
        &base,
        "/_twalk/bridges/{bridge_id}/status",
        "/_twalk/bridges/bridge-whatsapp/status",
        &[],
        Some("any-token-at-all"),
        Some(json!({ "state_event": "CONNECTED" })),
        503,
        Some("bridge_status_not_configured"),
    )
    .await?;

    // --- the sign-in's own refusals
    call.check(
        Method::POST,
        &base,
        "/api/session",
        "/api/session",
        &[],
        Some(json!({ "not": "a sign-in document" })),
        400,
        Some("invalid_request"),
    )
    .await?;
    let foreign = json!({
        "access_token": "irrelevant",
        "matrix_server_name": "another.example",
    });
    call.check(
        Method::POST,
        &base,
        "/api/session",
        "/api/session",
        &[],
        Some(json!({ "matrix_openid_token": foreign })),
        400,
        Some("foreign_homeserver"),
    )
    .await?;
    call.check(
        Method::POST,
        &base,
        "/api/session",
        "/api/session",
        &[],
        Some(json!({
            "matrix_openid_token": {
                "access_token": "not-a-token-this-homeserver-minted",
                "matrix_server_name": SERVER_NAME,
            }
        })),
        401,
        Some("openid_token_rejected"),
    )
    .await?;
    call.check(
        Method::POST,
        &base,
        "/api/session",
        "/api/session",
        &[],
        Some(json!({ "matrix_openid_token": other.openid_token().await? })),
        403,
        Some("not_the_owner"),
    )
    .await?;

    // --- bootstrap: the registration relay's refusals, on a Gateway whose
    // owner already has an account (every test bot is provisioned), and the
    // Sensor's invitation.
    call.check(
        Method::POST,
        &base,
        "/api/bootstrap/account",
        "/api/bootstrap/account",
        &[],
        Some(json!({ "username": OWNER_LOCALPART, "password": "test-only-password-g53" })),
        409,
        Some("account_already_exists"),
    )
    .await?;
    call.check(
        Method::POST,
        &base,
        "/api/bootstrap/account",
        "/api/bootstrap/account",
        &[],
        Some(json!({ "username": OTHER_LOCALPART, "password": "test-only-password-g53" })),
        403,
        Some("not_the_owner"),
    )
    .await?;
    call.check(
        Method::POST,
        &base,
        "/api/bootstrap/account",
        "/api/bootstrap/account",
        &[],
        Some(json!({ "not": "a registration document" })),
        400,
        Some("invalid_request"),
    )
    .await?;
    // The promise screen 2 makes to the user, as an API property: there is no
    // member a recovery key can arrive in (ADR 0014).
    call.check(
        Method::POST,
        &base,
        "/api/bootstrap/account",
        "/api/bootstrap/account",
        &[],
        Some(json!({
            "username": OWNER_LOCALPART,
            "password": "test-only-password-g53",
            "recovery_key": "EsTx abcd efgh ijkl mnop qrst uvwx yz23 4567",
        })),
        400,
        Some("recovery_key_refused"),
    )
    .await?;

    // Its own device, because the refresh above rotated the driving one.
    let (bootstrapping, _) = sign_in_cookies(&http, &base, &owner, "the bootstrap device").await?;
    let bootstrap_cookie = [("twalk_device", bootstrapping.as_str())];
    let room = owner.create_room("the conformance room").await?;
    let invited = call
        .check(
            Method::POST,
            &base,
            "/api/bootstrap/rooms",
            "/api/bootstrap/rooms",
            &bootstrap_cookie,
            Some(json!({
                "matrix_access_token": owner.matrix_access_token(),
                "rooms": [room],
            })),
            200,
            None,
        )
        .await?;
    assert_eq!(
        invited.body["rooms"][0]["status"].as_str(),
        Some("invited"),
        "the Sensor is invited into the room the user selected: {}",
        invited.body
    );
    call.check(
        Method::POST,
        &base,
        "/api/bootstrap/rooms",
        "/api/bootstrap/rooms",
        &bootstrap_cookie,
        Some(json!({ "rooms": [] })),
        400,
        Some("invalid_request"),
    )
    .await?;
    // A Matrix token the homeserver does not know: the whole request is
    // refused, because nothing was attempted in any room.
    call.check(
        Method::POST,
        &base,
        "/api/bootstrap/rooms",
        "/api/bootstrap/rooms",
        &bootstrap_cookie,
        Some(json!({
            "matrix_access_token": "syt_not_a_token_this_homeserver_minted",
            "rooms": ["!a-room:test.twalk"],
        })),
        401,
        Some("matrix_token_rejected"),
    )
    .await?;

    // --- the Companion's own surface: a file, the SPA fallback, a redirect
    call.check(
        Method::GET,
        &base,
        "/{companionPath}",
        "/",
        &[],
        None,
        200,
        None,
    )
    .await?;
    let fallback = call
        .check(
            Method::GET,
            &base,
            "/{companionPath}",
            "/onboarding/a-client-side-route",
            &[],
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(
        fallback.body.as_str(),
        Some(FALLBACK_HTML),
        "a path the build has no file for loads the app shell, with 200"
    );
    let index = call
        .check(
            Method::GET,
            &base,
            "/{companionPath}",
            "/index.html",
            &[],
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(index.body.as_str(), Some(INDEX_HTML));
    // The build keeps `onboarding/signal/index.html`, so the slash-less
    // spelling redirects to the one it has.
    call.check(
        Method::GET,
        &base,
        "/{companionPath}",
        "/onboarding/signal",
        &[],
        None,
        307,
        None,
    )
    .await?;
    gateway.stop().await;

    // --- a Gateway an operator has not finished configuring: no owner, and
    // no Companion build either, which is the origin's other 404.
    let unconfigured_static = missing_static_dir("openapi-unconfigured");
    let unconfigured = GatewayProc::start(&gateway_env_without_sign_in(&unconfigured_static))?;
    let unconfigured_base = unconfigured.base_url().await?;
    wait_until_answering(&unconfigured_base).await?;
    for (method, template, target) in [
        (Method::POST, "/api/session", "/api/session"),
        (Method::GET, "/api/session", "/api/session"),
        (Method::DELETE, "/api/session", "/api/session"),
        (Method::POST, "/api/session/refresh", "/api/session/refresh"),
        (Method::GET, "/api/devices", "/api/devices"),
        (
            Method::DELETE,
            "/api/devices/{id}",
            "/api/devices/a-device-id",
        ),
        (
            Method::POST,
            "/api/bootstrap/account",
            "/api/bootstrap/account",
        ),
        (Method::POST, "/api/bootstrap/rooms", "/api/bootstrap/rooms"),
        (Method::GET, "/api/bridges", "/api/bridges"),
        (
            Method::GET,
            "/api/bridges/{bridge_id}/login/flows",
            "/api/bridges/mautrix-whatsapp/login/flows",
        ),
        (
            Method::POST,
            "/api/bridges/{bridge_id}/login",
            "/api/bridges/mautrix-whatsapp/login",
        ),
        (
            Method::GET,
            "/api/bridges/{bridge_id}/login",
            "/api/bridges/mautrix-whatsapp/login",
        ),
        (
            Method::DELETE,
            "/api/bridges/{bridge_id}/login",
            "/api/bridges/mautrix-whatsapp/login",
        ),
        (
            Method::POST,
            "/api/bridges/{bridge_id}/login/submit",
            "/api/bridges/mautrix-whatsapp/login/submit",
        ),
        (
            Method::GET,
            "/api/bridges/{bridge_id}/logins",
            "/api/bridges/mautrix-whatsapp/logins",
        ),
        (
            Method::DELETE,
            "/api/bridges/{bridge_id}/logins/{login_id}",
            "/api/bridges/mautrix-whatsapp/logins/a-login",
        ),
        // The status webhook is outside `/api`, and the guard covers it all
        // the same (#56): a Gateway with no owner has no store and no bus,
        // so it closes this route like every other one.
        (
            Method::POST,
            "/_twalk/bridges/{bridge_id}/status",
            "/_twalk/bridges/bridge-whatsapp/status",
        ),
    ] {
        call.check(
            method,
            &unconfigured_base,
            template,
            target,
            &[],
            None,
            503,
            Some("sign_in_not_configured"),
        )
        .await?;
    }
    call.check(
        Method::GET,
        &unconfigured_base,
        "/{companionPath}",
        "/",
        &[],
        None,
        404,
        None,
    )
    .await?;
    unconfigured.stop().await;

    // --- a homeserver the Gateway cannot reach: the one refusal that is the
    // operator's problem and not the user's.
    let unreachable_static = companion_build("openapi-unreachable")?;
    let unreachable = GatewayProc::start(&gateway_env_with(
        &unreachable_static,
        // Port 1 on the loopback: nothing listens, and the connection is
        // refused rather than hanging.
        &[("GATEWAY_HOMESERVER_FEDERATION_URL", "http://127.0.0.1:1")],
    ))?;
    let unreachable_base = unreachable.base_url().await?;
    wait_until_answering(&unreachable_base).await?;
    call.check(
        Method::POST,
        &unreachable_base,
        "/api/session",
        "/api/session",
        &[],
        Some(json!({
            "matrix_openid_token": {
                "access_token": "irrelevant",
                "matrix_server_name": SERVER_NAME,
            }
        })),
        502,
        Some("homeserver_unverifiable"),
    )
    .await?;
    unreachable.stop().await;

    // --- a Gateway whose owner has no account yet: the one registration a
    // deployment ever does (ticket #53).
    let fresh_static = companion_build("openapi-bootstrap-created")?;
    let fresh_owner = fresh_owner_user_id("openapi");
    let fresh = GatewayProc::start(&gateway_env_with(
        &fresh_static,
        &[("GATEWAY_OWNER", fresh_owner.as_str())],
    ))?;
    let fresh_base = fresh.base_url().await?;
    wait_until_answering(&fresh_base).await?;
    let fresh_localpart = fresh_owner
        .trim_start_matches('@')
        .split_once(':')
        .expect("a Matrix ID")
        .0
        .to_owned();
    let created = call
        .check(
            Method::POST,
            &fresh_base,
            "/api/bootstrap/account",
            "/api/bootstrap/account",
            &[],
            Some(json!({
                "username": fresh_localpart,
                "password": "test-only-password-g53-openapi",
            })),
            201,
            None,
        )
        .await?;
    assert_eq!(
        created.body["user_id"].as_str(),
        Some(fresh_owner.as_str()),
        "the relay creates the owner's account: {}",
        created.body
    );
    fresh.stop().await;

    // --- a Gateway whose homeserver *client* API is out of reach, while its
    // federation API (where sign-in verifies) still answers: the one shape
    // that can exercise both bootstrap calls' 502 while still signing a
    // device in.
    let unreachable_client_static = companion_build("openapi-bootstrap-unreachable")?;
    let unreachable_client = GatewayProc::start(&gateway_env_with(
        &unreachable_client_static,
        &[("GATEWAY_HOMESERVER_URL", "http://127.0.0.1:1")],
    ))?;
    let unreachable_client_base = unreachable_client.base_url().await?;
    wait_until_answering(&unreachable_client_base).await?;
    call.check(
        Method::POST,
        &unreachable_client_base,
        "/api/bootstrap/account",
        "/api/bootstrap/account",
        &[],
        Some(json!({ "username": OWNER_LOCALPART, "password": "test-only-password-g53" })),
        502,
        Some("homeserver_unreachable"),
    )
    .await?;
    let (stranded, _) = sign_in_cookies(
        &http,
        &unreachable_client_base,
        &owner,
        "the stranded device",
    )
    .await?;
    call.check(
        Method::POST,
        &unreachable_client_base,
        "/api/bootstrap/rooms",
        "/api/bootstrap/rooms",
        &[("twalk_device", stranded.as_str())],
        Some(json!({
            "matrix_access_token": owner.matrix_access_token(),
            "rooms": ["!a-room:test.twalk"],
        })),
        502,
        Some("homeserver_unreachable"),
    )
    .await?;
    unreachable_client.stop().await;

    // --- a Gateway that does write consent (#49): its own bus, its own
    // journal, and a device of its own to sign in on. Consent is one of the
    // halves a deployment can have configured or not, so its answers are
    // driven on a Gateway of its own rather than by reconfiguring the first.
    let consent_static = companion_build("openapi-consent")?;
    let consenting = GatewayProc::start(&gateway_env_with_consent(&consent_static, &nats_url()))?;
    let consenting_base = consenting.base_url().await?;
    wait_until_answering(&consenting_base).await?;
    let (deciding_device, _) =
        sign_in_cookies(&http, &consenting_base, &owner, "the deciding device").await?;
    let deciding_cookie = [("twalk_device", deciding_device.as_str())];
    // A contact nobody else's run has decided about: the bus and this
    // Synapse are shared with every other suite.
    let subject = format!(
        "@whatsapp_openapi_{}:{SERVER_NAME}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );

    let recorded = call
        .check(
            Method::POST,
            &consenting_base,
            "/api/consent/decisions",
            "/api/consent/decisions",
            &deciding_cookie,
            Some(json!({
                "subject": { "type": "contact", "id": subject },
                "new_state": "granted",
                "scope": { "networks": ["whatsapp"] },
                "reason": "the description says a reason is kept"
            })),
            201,
            None,
        )
        .await?;
    assert_eq!(
        recorded.body["actor"].as_str(),
        Some(owner_id.as_str()),
        "a decision is attributed to the owner, not to the device it arrived from"
    );
    assert_eq!(recorded.body["old_state"].as_str(), Some("unset"));
    assert_eq!(recorded.body["replayed"].as_bool(), Some(false));

    // The two reads over it, and the precedence the second one resolves.
    let state = call
        .check(
            Method::GET,
            &consenting_base,
            "/api/consent/state",
            "/api/consent/state",
            &deciding_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert!(
        state.body["entries"]
            .as_array()
            .is_some_and(|entries| entries
                .iter()
                .any(|entry| entry["subject"]["id"] == json!(subject))),
        "the decision just taken is in the current state: {}",
        state.body
    );
    let effective = call
        .check(
            Method::GET,
            &consenting_base,
            "/api/consent/effective",
            &format!(
                "/api/consent/effective?contact={}&network=whatsapp",
                subject.replace('@', "%40").replace(':', "%3A")
            ),
            &deciding_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(effective.body["state"].as_str(), Some("granted"));
    assert_eq!(
        effective.body["decided_by"]["type"].as_str(),
        Some("contact")
    );
    // And a contact nobody decided about: pending, decided by nothing — the
    // `null` the description declares.
    let undecided = call
        .check(
            Method::GET,
            &consenting_base,
            "/api/consent/effective",
            "/api/consent/effective?contact=%40nobody-decided%3Atest.twalk&network=telegram",
            &deciding_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(undecided.body["state"].as_str(), Some("pending"));
    assert_eq!(undecided.body["decided_by"], Value::Null);

    // The snapshot (#50): the same entries, with the stream sequence they
    // reflect, to a caller presenting the service token instead of a device
    // cookie.
    let snapshot = call
        .check_with_bearer(
            Method::GET,
            &consenting_base,
            "/api/consent/snapshot",
            "/api/consent/snapshot",
            &[],
            Some(SERVICE_TOKEN),
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(
        snapshot.body["next_stream_sequence"].as_u64(),
        snapshot.body["stream_sequence"]
            .as_u64()
            .map(|sequence| sequence + 1),
        "the start sequence a consumer uses is the position plus one: {}",
        snapshot.body
    );

    // The write path's refusals, one per code the description enumerates.
    for (body, error) in [
        (
            json!({
                "subject": { "type": "persona", "id": "assistant" },
                "new_state": "granted",
                "scope": { "networks": ["whatsapp"] }
            }),
            "unsupported_subject_type",
        ),
        (
            json!({
                "subject": { "type": "network", "id": "whatsapp" },
                "new_state": "granted",
                "scope": { "networks": ["signal"] }
            }),
            "scope_contradicts_subject",
        ),
        (
            json!({
                "subject": { "type": "contact", "id": subject },
                "new_state": "granted",
                "scope": { "networks": ["gmessages"] }
            }),
            "unknown_value",
        ),
        (
            json!({ "new_state": "granted", "scope": { "networks": ["whatsapp"] } }),
            "malformed_request",
        ),
    ] {
        call.check(
            Method::POST,
            &consenting_base,
            "/api/consent/decisions",
            "/api/consent/decisions",
            &deciding_cookie,
            Some(body),
            400,
            Some(error),
        )
        .await?;
    }
    // And the read's, on the network it is asked about.
    call.check(
        Method::GET,
        &consenting_base,
        "/api/consent/effective",
        "/api/consent/effective?contact=%40a%3Atest.twalk",
        &deciding_cookie,
        None,
        400,
        Some("malformed_request"),
    )
    .await?;
    call.check(
        Method::GET,
        &consenting_base,
        "/api/consent/effective",
        "/api/consent/effective?contact=%40a%3Atest.twalk&network=irc",
        &deciding_cookie,
        None,
        400,
        Some("unknown_value"),
    )
    .await?;
    consenting.stop().await;

    // --- a Gateway whose snapshot cap is below its own state: the one
    // answer that is neither the state nor a truncation of it (#50). One
    // entry is the smallest cap there is, so two decisions exceed it.
    let capped_static = companion_build("openapi-snapshot-cap")?;
    let capped = GatewayProc::start(&gateway_env_with(
        &capped_static,
        &[
            ("GATEWAY_NATS_URL", &nats_url()),
            ("GATEWAY_CONSENT_SNAPSHOT_MAX_ENTRIES", "1"),
        ],
    ))?;
    let capped_base = capped.base_url().await?;
    wait_until_answering(&capped_base).await?;
    let (capped_device, _) =
        sign_in_cookies(&http, &capped_base, &owner, "the capped device").await?;
    for index in 0..2 {
        let response = http
            .post(format!("{capped_base}/api/consent/decisions"))
            .header(
                reqwest::header::COOKIE,
                format!("twalk_device={capped_device}"),
            )
            .json(&json!({
                "subject": { "type": "contact", "id": format!("{subject}-capped-{index}") },
                "new_state": "granted",
                "scope": { "networks": ["whatsapp"] }
            }))
            .send()
            .await?;
        anyhow::ensure!(
            response.status().as_u16() == 201,
            "a decision the cap test relies on was not recorded: {}",
            response.text().await.unwrap_or_default()
        );
    }
    // The snapshot reflects the published prefix, so the refusal appears
    // once the outbox has drained both decisions.
    poll_until(
        || async {
            let status = http
                .get(format!("{capped_base}/api/consent/snapshot"))
                .bearer_auth(SERVICE_TOKEN)
                .send()
                .await
                .ok()?
                .status();
            (status.as_u16() != 200).then_some(())
        },
        "the capped Gateway to refuse its oversized snapshot",
    )
    .await?;
    call.check_with_bearer(
        Method::GET,
        &capped_base,
        "/api/consent/snapshot",
        "/api/consent/snapshot",
        &[],
        Some(SERVICE_TOKEN),
        None,
        500,
        Some("snapshot_too_large"),
    )
    .await?;
    capped.stop().await;

    // --- a Gateway that serves no snapshot at all: no service token, so
    // there is no credential that would open it, and the answer says which
    // variable would.
    let tokenless_static = companion_build("openapi-snapshot-tokenless")?;
    let tokenless = GatewayProc::start(&gateway_env_with(
        &tokenless_static,
        // An empty override removes the variable.
        &[("GATEWAY_SERVICE_TOKEN", "")],
    ))?;
    let tokenless_base = tokenless.base_url().await?;
    wait_until_answering(&tokenless_base).await?;
    call.check_with_bearer(
        Method::GET,
        &tokenless_base,
        "/api/consent/snapshot",
        "/api/consent/snapshot",
        &[],
        Some(SERVICE_TOKEN),
        None,
        503,
        Some("service_token_not_configured"),
    )
    .await?;
    tokenless.stop().await;

    // --- the bridge facade (#55): a Gateway of its own, with a stub bridge
    // implementing the provisioning contract behind it and a second bridge
    // configured at a port nothing listens on. A real mautrix bridge needs a
    // live account and a phone, so the stub is the seam — `tests/bridges.rs`
    // is where the behaviour is asserted, and this drives every answer the
    // description declares.
    let stub = StubBridge::start().await?;
    let bridges_static = companion_build("openapi-bridges")?;
    let bridged = GatewayProc::start(&gateway_env_with_bridges_and_consent(
        &bridges_static,
        &stub.base_url(),
        &nats_url(),
    ))?;
    let bridged_base = bridged.base_url().await?;
    wait_until_answering(&bridged_base).await?;
    let (bridge_device, _) =
        sign_in_cookies(&http, &bridged_base, &owner, "the connecting device").await?;
    let bridge_cookie = [("twalk_device", bridge_device.as_str())];
    let stub_login = format!("/api/bridges/{STUB_BRIDGE_ID}/login");
    let dead_login = format!("/api/bridges/{UNREACHABLE_BRIDGE_ID}/login");

    let listed = call
        .check(
            Method::GET,
            &bridged_base,
            "/api/bridges",
            "/api/bridges",
            &bridge_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(
        listed.body["bridges"][0]["bridge_id"].as_str(),
        Some(STUB_BRIDGE_ID),
        "the bridge list is configuration, in configuration order: {}",
        listed.body
    );

    call.check(
        Method::GET,
        &bridged_base,
        "/api/bridges/{bridge_id}/login/flows",
        &format!("/api/bridges/{STUB_BRIDGE_ID}/login/flows"),
        &bridge_cookie,
        None,
        200,
        None,
    )
    .await?;
    // A bridge this deployment does not have, and one that is configured and
    // down: the two refusals an operator has to be able to tell apart.
    call.check(
        Method::GET,
        &bridged_base,
        "/api/bridges/{bridge_id}/login/flows",
        "/api/bridges/mautrix-telegram/login/flows",
        &bridge_cookie,
        None,
        404,
        Some("unknown_bridge"),
    )
    .await?;
    call.check(
        Method::GET,
        &bridged_base,
        "/api/bridges/{bridge_id}/login/flows",
        &format!("/api/bridges/{UNREACHABLE_BRIDGE_ID}/login/flows"),
        &bridge_cookie,
        None,
        502,
        Some("bridge_unreachable"),
    )
    .await?;

    // Nothing started yet.
    call.check(
        Method::GET,
        &bridged_base,
        "/api/bridges/{bridge_id}/login",
        &stub_login,
        &bridge_cookie,
        None,
        404,
        Some("no_login_in_flight"),
    )
    .await?;
    call.check(
        Method::DELETE,
        &bridged_base,
        "/api/bridges/{bridge_id}/login",
        &stub_login,
        &bridge_cookie,
        None,
        404,
        Some("no_login_in_flight"),
    )
    .await?;

    // A QR login: started, polled while the Gateway holds the blocking step,
    // then cancelled.
    let started = call
        .check(
            Method::POST,
            &bridged_base,
            "/api/bridges/{bridge_id}/login",
            &stub_login,
            &bridge_cookie,
            Some(json!({ "flow_id": QR_FLOW })),
            201,
            None,
        )
        .await?;
    assert_eq!(
        started.body["step"]["type"].as_str(),
        Some("display_and_wait"),
        "a QR flow's first step is the blocking one: {}",
        started.body
    );
    call.check(
        Method::GET,
        &bridged_base,
        "/api/bridges/{bridge_id}/login",
        &stub_login,
        &bridge_cookie,
        None,
        200,
        None,
    )
    .await?;
    // A second login while that one is in flight.
    call.check(
        Method::POST,
        &bridged_base,
        "/api/bridges/{bridge_id}/login",
        &stub_login,
        &bridge_cookie,
        Some(json!({ "flow_id": QR_FLOW })),
        409,
        Some("login_in_flight"),
    )
    .await?;
    // There is nothing to submit while the Gateway is holding the step.
    call.check(
        Method::POST,
        &bridged_base,
        "/api/bridges/{bridge_id}/login/submit",
        &format!("/api/bridges/{STUB_BRIDGE_ID}/login/submit"),
        &bridge_cookie,
        Some(json!({ "step_id": "fi.mau.stub.login.qr", "data": {} })),
        400,
        Some("invalid_request"),
    )
    .await?;
    call.check(
        Method::DELETE,
        &bridged_base,
        "/api/bridges/{bridge_id}/login",
        &stub_login,
        &bridge_cookie,
        None,
        204,
        None,
    )
    .await?;

    // The start's own refusals: no flow, a bridge that is not configured, a
    // bridge that is down, and the network refusing another linked device.
    call.check(
        Method::POST,
        &bridged_base,
        "/api/bridges/{bridge_id}/login",
        &stub_login,
        &bridge_cookie,
        Some(json!({ "not": "a start request" })),
        400,
        Some("invalid_request"),
    )
    .await?;
    call.check(
        Method::POST,
        &bridged_base,
        "/api/bridges/{bridge_id}/login",
        "/api/bridges/mautrix-telegram/login",
        &bridge_cookie,
        Some(json!({ "flow_id": QR_FLOW })),
        404,
        Some("unknown_bridge"),
    )
    .await?;
    call.check(
        Method::POST,
        &bridged_base,
        "/api/bridges/{bridge_id}/login",
        &dead_login,
        &bridge_cookie,
        Some(json!({ "flow_id": QR_FLOW })),
        502,
        Some("bridge_unreachable"),
    )
    .await?;
    stub.refuse_next_start(403, "FI.MAU.BRIDGE.TOO_MANY_LOGINS");
    call.check(
        Method::POST,
        &bridged_base,
        "/api/bridges/{bridge_id}/login",
        &stub_login,
        &bridge_cookie,
        Some(json!({ "flow_id": QR_FLOW })),
        403,
        Some("too_many_logins"),
    )
    .await?;
    // And a login id the bridge does not know, which is the reconnect that
    // is pointing at nothing.
    stub.refuse_next_start(404, "M_NOT_FOUND");
    call.check(
        Method::POST,
        &bridged_base,
        "/api/bridges/{bridge_id}/login",
        &stub_login,
        &bridge_cookie,
        Some(json!({ "flow_id": QR_FLOW, "login_id": "never-existed" })),
        404,
        Some("not_found_on_bridge"),
    )
    .await?;

    // A cookies login, which is the path where a credential is submitted:
    // its refusals first, then the one that goes through.
    let submit = format!("/api/bridges/{STUB_BRIDGE_ID}/login/submit");
    // One refusal per login: a step that was refused ends that login, which
    // is the point — the polled state has to say so — so each of these needs
    // a login of its own.
    for (status, errcode, expected_status, expected_error) in [
        (409, "FI.MAU.LOGIN_STEP_CANCELLED", 409, "step_cancelled"),
        (500, "M_UNKNOWN", 502, "bridge_refused"),
        (410, "LOGIN_TIMED_OUT", 410, "login_expired"),
    ] {
        call.check(
            Method::POST,
            &bridged_base,
            "/api/bridges/{bridge_id}/login",
            &stub_login,
            &bridge_cookie,
            Some(json!({ "flow_id": COOKIES_FLOW })),
            201,
            None,
        )
        .await?;
        stub.refuse_next_step(status, errcode);
        call.check(
            Method::POST,
            &bridged_base,
            "/api/bridges/{bridge_id}/login/submit",
            &submit,
            &bridge_cookie,
            Some(json!({ "step_id": COOKIES_STEP, "data": { "cookies": {} } })),
            expected_status,
            Some(expected_error),
        )
        .await?;
    }
    // A submission to a bridge nobody has started a login on.
    call.check(
        Method::POST,
        &bridged_base,
        "/api/bridges/{bridge_id}/login/submit",
        &format!("/api/bridges/{UNREACHABLE_BRIDGE_ID}/login/submit"),
        &bridge_cookie,
        Some(json!({ "step_id": COOKIES_STEP, "data": {} })),
        404,
        Some("no_login_in_flight"),
    )
    .await?;
    // And the one that goes through, which leaves a login on the bridge.
    let cookies_started = call
        .check(
            Method::POST,
            &bridged_base,
            "/api/bridges/{bridge_id}/login",
            &stub_login,
            &bridge_cookie,
            Some(json!({ "flow_id": COOKIES_FLOW })),
            201,
            None,
        )
        .await?;
    let submitted = call
        .check(
            Method::POST,
            &bridged_base,
            "/api/bridges/{bridge_id}/login/submit",
            &submit,
            &bridge_cookie,
            Some(json!({
                "step_id": cookies_started.body["step"]["step_id"].as_str().unwrap_or_default(),
                "data": { "cookies": { "SID": "a-cookie-the-gateway-relays-and-forgets" } }
            })),
            200,
            None,
        )
        .await?;
    assert_eq!(submitted.body["state"].as_str(), Some("complete"));
    let login_id = submitted.body["login"]["login_id"]
        .as_str()
        .context("the completed login names itself")?
        .to_owned();

    // The logins the bridge holds, and logging one out.
    let logins = call
        .check(
            Method::GET,
            &bridged_base,
            "/api/bridges/{bridge_id}/logins",
            &format!("/api/bridges/{STUB_BRIDGE_ID}/logins"),
            &bridge_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert!(
        logins.body["logins"].as_array().is_some_and(|logins| logins
            .iter()
            .any(|login| login["login_id"] == json!(login_id))),
        "the login just completed is one the bridge holds: {}",
        logins.body
    );
    call.check(
        Method::GET,
        &bridged_base,
        "/api/bridges/{bridge_id}/logins",
        "/api/bridges/mautrix-telegram/logins",
        &bridge_cookie,
        None,
        404,
        Some("unknown_bridge"),
    )
    .await?;
    call.check(
        Method::GET,
        &bridged_base,
        "/api/bridges/{bridge_id}/logins",
        &format!("/api/bridges/{UNREACHABLE_BRIDGE_ID}/logins"),
        &bridge_cookie,
        None,
        502,
        Some("bridge_unreachable"),
    )
    .await?;
    call.check(
        Method::DELETE,
        &bridged_base,
        "/api/bridges/{bridge_id}/logins/{login_id}",
        &format!("/api/bridges/{STUB_BRIDGE_ID}/logins/{login_id}"),
        &bridge_cookie,
        None,
        204,
        None,
    )
    .await?;
    call.check(
        Method::DELETE,
        &bridged_base,
        "/api/bridges/{bridge_id}/logins/{login_id}",
        &format!("/api/bridges/{STUB_BRIDGE_ID}/logins/{login_id}"),
        &bridge_cookie,
        None,
        404,
        Some("not_found_on_bridge"),
    )
    .await?;
    call.check(
        Method::DELETE,
        &bridged_base,
        "/api/bridges/{bridge_id}/logins/{login_id}",
        &format!("/api/bridges/{UNREACHABLE_BRIDGE_ID}/logins/{login_id}"),
        &bridge_cookie,
        None,
        502,
        Some("bridge_unreachable"),
    )
    .await?;
    // --- the bridge status webhook (#56): the one route a bridge calls, on
    // the same Gateway. Its credential is the bridge's own as_token, so
    // `check_with_bearer` carries that rather than a cookie.
    let status_path = "/_twalk/bridges/{bridge_id}/status";
    let stub_status = bridge_status_path(STUB_STATUS_BRIDGE_ID);
    call.check_with_bearer(
        Method::POST,
        &bridged_base,
        status_path,
        &stub_status,
        &[],
        Some(STUB_AS_TOKEN),
        Some(json!({ "state_event": "CONNECTED", "timestamp": 1_789_000_000u64 })),
        204,
        None,
    )
    .await?;
    // A body that is not a mautrix BridgeState.
    call.check_with_bearer(
        Method::POST,
        &bridged_base,
        status_path,
        &stub_status,
        &[],
        Some(STUB_AS_TOKEN),
        Some(json!({ "message": "nothing about a state" })),
        400,
        Some("invalid_request"),
    )
    .await?;
    // No credential at all: the refusal that makes the compose network not a
    // credential.
    call.check(
        Method::POST,
        &bridged_base,
        status_path,
        &stub_status,
        &[],
        Some(json!({ "state_event": "CONNECTED" })),
        401,
        Some("unauthenticated"),
    )
    .await?;
    // A bridge nobody configured.
    call.check_with_bearer(
        Method::POST,
        &bridged_base,
        status_path,
        &bridge_status_path("bridge-nobody-configured"),
        &[],
        Some(STUB_AS_TOKEN),
        Some(json!({ "state_event": "CONNECTED" })),
        404,
        Some("unknown_bridge"),
    )
    .await?;
    // A configured bridge this Gateway holds no as_token for: refused, never
    // trusted.
    call.check_with_bearer(
        Method::POST,
        &bridged_base,
        status_path,
        &bridge_status_path(UNREACHABLE_STATUS_BRIDGE_ID),
        &[],
        Some(STUB_AS_TOKEN),
        Some(json!({ "state_event": "CONNECTED" })),
        503,
        Some("as_token_not_configured"),
    )
    .await?;
    bridged.stop().await;
    stub.stop().await;

    // --- and now the coverage assertion: everything the description
    // declares was either exercised above, or is listed with its reason.
    let mut missing = Vec::new();
    for (method, path) in description.operations() {
        let responses = description.operation(&method, &path)?["responses"]
            .as_object()
            .context("an operation declares responses")?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for status in responses {
            if exercised.contains(&(method.clone(), path.clone(), status.clone())) {
                continue;
            }
            if UNEXERCISED.iter().any(
                |(unexercised_method, unexercised_path, unexercised_status, _)| {
                    *unexercised_method == method
                        && *unexercised_path == path
                        && *unexercised_status == status
                },
            ) {
                continue;
            }
            missing.push(format!("{} {path} {status}", method.to_uppercase()));
        }
    }
    assert!(
        missing.is_empty(),
        "the description declares responses this test never saw the Gateway answer: {}. \
         Either drive them here, or list them in UNEXERCISED with the reason they cannot \
         be reached at the process boundary.",
        missing.join(", ")
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The one part of the surface that is described in prose
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_undescribed_api_path_answers_the_json_404() -> Result<()> {
    ensure_stack().await?;
    let description = Description::load()?;
    let http = client()?;
    let (gateway, base) = start("openapi-api-404").await?;

    // Unauthenticated, the catch-all is a 401: the guard answers before
    // routing, so a caller with no device token learns nothing about the
    // API's shape.
    let unauthenticated = http.get(format!("{base}/api/nothing-here")).send().await?;
    assert_eq!(unauthenticated.status().as_u16(), 401);

    let (device, _) = sign_in_cookies(
        &http,
        &base,
        &MatrixUser::login(OWNER_LOCALPART).await?,
        "the 404 device",
    )
    .await?;
    let response = http
        .get(format!("{base}/api/nothing-here"))
        .header(reqwest::header::COOKIE, format!("twalk_device={device}"))
        .send()
        .await?;
    assert_eq!(response.status().as_u16(), 404);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(essence),
        Some("application/json".to_owned()),
        "a client parsing an API response must never be handed an HTML page"
    );
    let body: Value = response.json().await?;
    // Not an operation of the description — OpenAPI has no wildcard path —
    // but its shape is the description's `Error` schema, `path` member
    // included.
    description.validate(&json!({ "$ref": "#/components/schemas/Error" }), &body)?;
    assert_eq!(body["error"].as_str(), Some("not_found"));
    assert_eq!(body["path"].as_str(), Some("/api/nothing-here"));

    gateway.stop().await;
    Ok(())
}
