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
use harness::{
    companion_build, ensure_stack, fresh_owner_user_id, gateway_env, gateway_env_with,
    gateway_env_without_sign_in, missing_static_dir, owner_user_id, poll_until, GatewayProc,
    MatrixUser, FALLBACK_HTML, INDEX_HTML, OTHER_LOCALPART, OWNER_LOCALPART, SERVER_NAME,
};
use reqwest::Method;
use serde_json::{json, Value};
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
        let mut request = self
            .client
            .request(method.clone(), format!("{base}{target}"));
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
            Some(other) => panic!("{other} is not one of the Gateway's credentials"),
        };

        if !path.starts_with("/api") {
            assert_eq!(
                described,
                Requirement::Open,
                "{} {path} is not under /api, where the guard does not run: it cannot \
                 require a credential",
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
