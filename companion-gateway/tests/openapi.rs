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
use harness::{sha256_hex, unreachable_nats_url, validate_against_contract, Bus};
use harness::{StubEndpoint, StubLlm};
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
    // `GET /api/deployment`'s 503 is reached below on the unconfigured
    // deployment, so the status is exercised; its other error code,
    // `store_unreadable`, needs the Gateway's own SQLite file to fail under a
    // running process — the same missing seam as the entry that follows.
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
        "post",
        "/api/approvals",
        "500",
        "store_unavailable and approval_published_but_not_recorded both need the Gateway's own SQLite file to fail under a running process — the same fault-injection seam this suite does not have. The second one is the interesting half (the reply went out and the record did not), and it is asserted as a shape by `approval_http::tests` and `store::tests::an_approval_is_recorded_unpublished_and_then_marked`",
    ),
    (
        "get",
        "/api/approvals/{suggestion_event_id}",
        "500",
        "store_unavailable, as above",
    ),
    (
        "get",
        "/api/consent/effective",
        "500",
        "store_unavailable, as above",
    ),
    // The settings store's own failures (#98). Same missing seam: the
    // Gateway's SQLite file would have to fail under a running process.
    // `settings::tests` and `settings_http::tests` cover the shapes.
    (
        "get",
        "/api/settings/model",
        "500",
        "store_unavailable needs the Gateway's settings store to fail under a running process: the same fault-injection seam this suite does not have",
    ),
    ("put", "/api/settings/model", "500", "store_unavailable, as above"),
    (
        "delete",
        "/api/settings/model",
        "500",
        "store_unavailable, as above",
    ),
    (
        "post",
        "/api/settings/model/probe",
        "500",
        "store_unavailable, as above",
    ),
    (
        "get",
        "/api/settings/language",
        "500",
        "store_unavailable, as above",
    ),
    (
        "put",
        "/api/settings/language",
        "500",
        "store_unavailable, as above",
    ),
    (
        "get",
        "/api/settings/runtime",
        "500",
        "store_unavailable, as above",
    ),
    // Hermes's answer webhook (#206). What this suite reaches without bus
    // state is exercised above; the four below need a bus in a particular
    // state, and `tests/hermes_answers.rs` puts it there.
    (
        "post",
        "/_twalk/hermes/answers",
        "409",
        "consent_revoked and consent_pending need a trigger on the bus and a decision in the journal; `hermes_answers.rs::a_contact_revoked_while_hermes_was_reasoning_gets_no_suggestion` stages both",
    ),
    (
        "post",
        "/_twalk/hermes/answers",
        "410",
        "trigger_out_of_reach needs a message behind a deliberately narrow lookup window, which is a Gateway of its own; `hermes_answers.rs` stages it",
    ),
    (
        "post",
        "/_twalk/hermes/answers",
        "500",
        "store_unavailable needs the consent journal to fail under a running process: the same fault-injection seam this suite does not have",
    ),
    (
        "post",
        "/_twalk/hermes/answers",
        "502",
        "bus_unreachable needs a Gateway whose bus is configured and does not answer; `hermes_answers.rs::a_bus_that_does_not_answer_is_a_502_and_not_a_503` stages it",
    ),
    (
        "post",
        "/_twalk/bridges/{bridge_id}/status",
        "500",
        "store_unavailable needs the Gateway's own SQLite file to fail under a running process: the same fault-injection seam this suite does not have",
    ),
    (
        "get",
        "/api/contacts/pending",
        "500",
        "store_unavailable needs the pending-contact store to fail under a running process: the same fault-injection seam this suite does not have",
    ),
    (
        "get",
        "/api/suggestions",
        "500",
        "store_unavailable here is the approval rows failing to be read while the bus answers: the same fault-injection seam this suite does not have. `suggestions_http::tests` asserts the shape of the answer",
    ),
    (
        "get",
        "/api/suggestions/{suggestion_event_id}",
        "500",
        "store_unavailable, as above",
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

    /// The same check, with a body sent as the caller composed it and a
    /// header of the caller's choosing.
    ///
    /// Hermes's answer webhook (#206) needs both: its credential is an HMAC
    /// over the **bytes** of the request, so a body that this helper
    /// re-serialised would authenticate something other than what the test
    /// signed.
    #[allow(clippy::too_many_arguments)]
    async fn check_raw(
        &mut self,
        method: Method,
        base: &str,
        template: &str,
        target: &str,
        headers: &[(&str, &str)],
        body: &str,
        expected_status: u16,
        expected_error: Option<&str>,
    ) -> Result<Checked> {
        let mut request = self
            .client
            .request(method.clone(), format!("{base}{target}"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_owned());
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("failed to call {method} {target}"))?;
        self.verify(
            method,
            template,
            target,
            expected_status,
            expected_error,
            response,
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
        self.verify(
            method,
            template,
            target,
            expected_status,
            expected_error,
            response,
        )
        .await
    }

    /// Everything both entry points do to an answer: the status, the media
    /// type, the body against the response's own schema, and the error code.
    ///
    /// One implementation, because the conformance rule is about the answer and
    /// not about how the request was composed.
    async fn verify(
        &mut self,
        method: Method,
        template: &str,
        target: &str,
        expected_status: u16,
        expected_error: Option<&str>,
        response: reqwest::Response,
    ) -> Result<Checked> {
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

/// One `inbound.message.received.v1` as the Sensor publishes it — the full
/// shape, carrying a body, a display name and a `network_identifier`.
///
/// The pending-contact projection (#54) is shown all three and keeps none of
/// them; that property is `tests/pending.rs`'s to assert against the store's
/// own bytes. Here the event exists so the description's pending-list and
/// display-name answers have something to be about.
fn inbound_event(subject: &str) -> Value {
    let at = "2026-09-17T10:00:00Z";
    let room: String = sha256_hex(&format!("openapi-room:{subject}"))
        .chars()
        .take(18)
        .collect();
    let event = json!({
        "specversion": "1.0",
        "id": sha256_hex(&format!("openapi:{subject}:{at}")),
        "source": format!("matrix://{SERVER_NAME}/!{room}:{SERVER_NAME}"),
        "type": "fr.linagora.twalk.inbound.message.received.v1",
        "time": at,
        "subject": subject,
        "datacontenttype": "application/json",
        "network": "whatsapp",
        "connection": "whatsapp",
        "consent": "pending",
        "data": {
            "body": "un message que la Gateway ne garde pas",
            "format": "text/plain",
            "reply_to": null,
            "attachments": [],
            "contact": {
                "display_name": "Aicha Benali",
                "network_identifier": "+33612345678"
            }
        }
    });
    validate_against_contract(&event, "inbound.message.received")
        .expect("the fixture is an event the contract allows");
    event
}

/// The message an approval's suggestion answers (#24): the only place the
/// portal room and the sender are named, which is why the approval path has
/// to find it on the bus.
fn approval_trigger_event(sender: &str, room_id: &str) -> Value {
    let at = "2026-09-17T10:00:00Z";
    let event = json!({
        "specversion": "1.0",
        "id": sha256_hex(&format!("openapi-g24-trigger:{sender}:{room_id}")),
        "source": format!("matrix://{SERVER_NAME}/{room_id}"),
        "type": "fr.linagora.twalk.inbound.message.received.v1",
        "time": at,
        "subject": sender,
        "datacontenttype": "application/json",
        "network": "whatsapp",
        "connection": "whatsapp",
        "consent": "granted",
        "data": {
            "body": "On décale à 20h ?",
            "format": "text/plain",
            "reply_to": null,
            "attachments": [],
            "contact": { "display_name": "Aicha Benali" }
        }
    });
    validate_against_contract(&event, "inbound.message.received")
        .expect("the fixture is an event the contract allows");
    event
}

/// The suggestion a persona produced for it, with the expiry ticket #22's
/// policy always sets — `in_seconds` from now, so the description's `201` is
/// driven against a suggestion that is still approvable however long this
/// suite has been running.
fn approval_suggestion_event(trigger: &Value, expires_in_seconds: i64) -> Value {
    let trigger_id = trigger["id"].as_str().expect("the trigger has an id");
    let expires_at = (time::OffsetDateTime::now_utc()
        + time::Duration::seconds(expires_in_seconds))
    .replace_nanosecond(0)
    .expect("a whole second is a valid instant")
    .format(&time::format_description::well_known::Rfc3339)
    .expect("an instant formats as RFC 3339");
    let event = json!({
        "specversion": "1.0",
        "id": sha256_hex(&format!("openapi-g24-suggest:{trigger_id}")),
        "source": format!("hermes://{SERVER_NAME}/personas/assistant"),
        "type": "fr.linagora.twalk.persona.suggest.produced.v1",
        "time": trigger["time"],
        "subject": trigger_id,
        "datacontenttype": "application/json",
        "network": "whatsapp",
        "connection": "whatsapp",
        "consent": "granted",
        "data": {
            "persona_id": "assistant",
            "trigger": {
                "event_id": trigger_id,
                "event_type": "fr.linagora.twalk.inbound.message.received.v1"
            },
            "suggestion": { "body": "Pas de problème, à 20h !", "format": "text/plain" },
            "attempt": 1,
            "expires_at": expires_at
        }
    });
    validate_against_contract(&event, "persona.suggest.produced")
        .expect("the fixture is an event the contract allows");
    event
}

/// A suggestion from a contract version this build does not have: its
/// network is one nobody has heard of (#97).
///
/// Deliberately not validated against today's contract — the schemas' enums
/// are closed, so there is no way to write this event and have it pass, and
/// that is the point. A Gateway that fell over on one would blank the whole
/// approval screen; this one counts it in a listing and answers `409
/// suggestion_unreadable` when asked about it directly, which is the third
/// thing "it is not there" must not be confused with.
fn unreadable_suggestion_event() -> Value {
    let trigger_id = sha256_hex("openapi-g97-unreadable-trigger");
    json!({
        "specversion": "1.0",
        "id": sha256_hex("openapi-g97-unreadable-suggest"),
        "source": format!("hermes://{SERVER_NAME}/personas/assistant"),
        "type": "fr.linagora.twalk.persona.suggest.produced.v1",
        "time": "2026-09-17T10:00:00Z",
        "subject": trigger_id,
        "datacontenttype": "application/json",
        "network": "carrierpigeon",
        "connection": "carrierpigeon",
        "consent": "granted",
        "data": {
            "persona_id": "assistant",
            "trigger": {
                "event_id": trigger_id,
                "event_type": "fr.linagora.twalk.inbound.message.received.v1"
            },
            "suggestion": { "body": "Par retour de pigeon.", "format": "text/plain" },
            "attempt": 1
        }
    })
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

/// The contract is the one authority for the network values (ADR 0033, #268),
/// and this description's `Network` schema is a copy: tested against what it
/// copies, order included, so a network added to the contract fails here
/// until the description — and the Companion's generated client behind it —
/// knows it.
#[test]
fn the_networks_the_description_names_are_the_contracts() -> Result<()> {
    let description = Description::load()?;
    let authority = twalk_test_harness::contract_definition_values("network")?;
    let copy: Vec<String> = description.doc["components"]["schemas"]["Network"]["enum"]
        .as_array()
        .context("components.schemas.Network.enum")?
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        copy, authority,
        "openapi.yaml's Network schema disagrees with the contract"
    );
    // The kinds too (#269): the networks plus what is not a network.
    let kinds = twalk_test_harness::contract_definition_values("kind")?;
    let kinds_copy: Vec<String> = description.doc["components"]["schemas"]["Kind"]["enum"]
        .as_array()
        .context("components.schemas.Kind.enum")?
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        kinds_copy, kinds,
        "openapi.yaml's Kind schema disagrees with the contract"
    );
    // And no other inline copy of either list anywhere in the description:
    // two schemas, `$ref`'d — a third list is a third authority.
    let text = std::fs::read_to_string(Description::path())?;
    assert_eq!(
        text.matches("enum: [whatsapp").count(),
        2,
        "openapi.yaml repeats the network or kind list instead of referencing Network or Kind"
    );
    // The shape of a connection's id, the same way (#269): the contract's
    // `definitions/connection.schema.json` is the authority, the
    // description's `Connection.id` a copy held to it.
    let id_pattern = twalk_test_harness::contract_definition("connection")?["pattern"]
        .as_str()
        .context("the connection definition has a pattern")?
        .to_owned();
    assert_eq!(
        description.doc["components"]["schemas"]["Connection"]["properties"]["id"]["pattern"]
            .as_str(),
        Some(id_pattern.as_str()),
        "openapi.yaml's Connection.id pattern disagrees with the contract"
    );
    Ok(())
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
            Some("hermesSignature") => Requirement::HermesSignature,
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

    // What this deployment is, asked by a caller with no credential at all —
    // which is the whole point of the operation (#112): a screen that can ask
    // does not have to attempt something and read the failure.
    let described = call
        .check(
            Method::GET,
            &base,
            "/api/deployment",
            "/api/deployment",
            &[],
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(
        described.body["homeserver"].as_str(),
        Some(SERVER_NAME),
        "the deployment names the server its owner is on"
    );
    assert!(
        described.body["bootstrapped"].is_boolean(),
        "bootstrapped is a fact, not an absence"
    );
    assert!(
        described.body.get("owner").is_none(),
        "the owner's Matrix ID is not published to an unauthenticated caller"
    );

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
    let absent_approval = format!("/api/approvals/{}", "a".repeat(64));
    let absent_suggestion = format!("/api/suggestions/{}", "a".repeat(64));
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
        (
            Method::GET,
            "/api/contacts/pending",
            "/api/contacts/pending",
        ),
        (
            Method::GET,
            "/api/contacts/display-names",
            "/api/contacts/display-names?contact=%40a%3Atest.twalk",
        ),
        (Method::GET, "/api/suggestions", "/api/suggestions"),
        (
            Method::GET,
            "/api/suggestions/{suggestion_event_id}",
            absent_suggestion.as_str(),
        ),
        (Method::GET, "/api/portals", "/api/portals"),
        (Method::GET, "/api/portals/moves", "/api/portals/moves"),
        (Method::GET, "/api/connections", "/api/connections"),
        (
            Method::POST,
            "/api/portals/observation",
            "/api/portals/observation",
        ),
        (Method::GET, "/api/runtime", "/api/runtime"),
        (Method::POST, "/api/approvals", "/api/approvals"),
        (
            Method::GET,
            "/api/approvals/{suggestion_event_id}",
            absent_approval.as_str(),
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
        (Method::GET, "/api/settings/model", "/api/settings/model"),
        (Method::PUT, "/api/settings/model", "/api/settings/model"),
        (Method::DELETE, "/api/settings/model", "/api/settings/model"),
        (
            Method::POST,
            "/api/settings/model/probe",
            "/api/settings/model/probe",
        ),
        (
            Method::GET,
            "/api/settings/language",
            "/api/settings/language",
        ),
        (
            Method::PUT,
            "/api/settings/language",
            "/api/settings/language",
        ),
        // As the snapshot above: a different reason — no service token
        // rather than no device token — and deliberately the same answer.
        (
            Method::GET,
            "/api/settings/runtime",
            "/api/settings/runtime",
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
    // The pending contacts are the same half of the same configuration: no
    // bus, so no projection, and the answer says so rather than claiming
    // that nobody has written to the user (#54).
    for (template, target) in [
        ("/api/contacts/pending", "/api/contacts/pending"),
        (
            "/api/contacts/display-names",
            "/api/contacts/display-names?contact=%40a%3Atest.twalk",
        ),
    ] {
        call.check(
            Method::GET,
            &base,
            template,
            target,
            &consent_cookie,
            None,
            503,
            Some("contacts_not_configured"),
        )
        .await?;
    }
    // The portal register is off for a different reason on this deployment:
    // no bridge is configured at all, so there is no conversation for the
    // Sensor to be inside or outside of. A refusal rather than an empty
    // list, because "your bridges have built no conversations" and "this
    // Gateway cannot see them" are very different claims (#105).
    for (method, template, body) in [
        (Method::GET, "/api/portals", None),
        (Method::GET, "/api/portals/moves", None),
        (
            Method::POST,
            "/api/portals/observation",
            Some(json!({ "rooms": ["!a:test.twalk"], "observed": true })),
        ),
    ] {
        call.check(
            method,
            &base,
            template,
            template,
            &consent_cookie,
            body,
            503,
            Some("portals_not_configured"),
        )
        .await?;
    }
    // Approvals are the same half of the same configuration (#24): no bus, so
    // no suggestion to read and nowhere to publish the reply. Answered before
    // the suggestion is looked at, so a client never reads "this deployment
    // does not approve" as a statement about that suggestion.
    call.check(
        Method::POST,
        &base,
        "/api/approvals",
        "/api/approvals",
        &consent_cookie,
        Some(json!({ "suggestion_event_id": "a".repeat(64) })),
        503,
        Some("approvals_not_configured"),
    )
    .await?;
    call.check(
        Method::GET,
        &base,
        "/api/approvals/{suggestion_event_id}",
        &absent_approval,
        &consent_cookie,
        None,
        503,
        Some("approvals_not_configured"),
    )
    .await?;
    // And reading suggestions (#97) is the same half again: they live on the
    // bus, so with no bus there is nothing to project. An empty list would
    // claim that no persona has proposed anything, which is a different
    // statement with a different fix.
    call.check(
        Method::GET,
        &base,
        "/api/suggestions",
        "/api/suggestions",
        &consent_cookie,
        None,
        503,
        Some("suggestions_not_configured"),
    )
    .await?;
    call.check(
        Method::GET,
        &base,
        "/api/suggestions/{suggestion_event_id}",
        &absent_suggestion,
        &consent_cookie,
        None,
        503,
        Some("suggestions_not_configured"),
    )
    .await?;
    // And whether a runtime is present (#189) is read off the same bus:
    // with none, the answer is a refusal naming the variable and never
    // `never`, which would claim that no runtime has ever been here.
    call.check(
        Method::GET,
        &base,
        "/api/runtime",
        "/api/runtime",
        &consent_cookie,
        None,
        503,
        Some("runtime_not_configured"),
    )
    .await?;
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
        400,
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

    // --- the model and the language (#98), on the same Gateway: the
    // configuration an operator names, and the four different answers a
    // probe of it can give. A device of its own, because the driving one
    // above was rotated and then signed out.
    let (settings_device, _) = sign_in_cookies(&http, &base, &owner, "the settings device").await?;
    let settings_cookie = [("twalk_device", settings_device.as_str())];
    // Nothing named yet: `200` with `configured: false`, because "no
    // endpoint configured at all" is a state a settings screen draws and not
    // an error it branches on.
    let unconfigured_model = call
        .check(
            Method::GET,
            &base,
            "/api/settings/model",
            "/api/settings/model",
            &settings_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(
        unconfigured_model.body["configured"],
        json!(false),
        "a deployment whose operator has named no model says so"
    );
    // And the probe's fourth answer: there is nothing to probe.
    call.check(
        Method::POST,
        &base,
        "/api/settings/model/probe",
        "/api/settings/model/probe",
        &settings_cookie,
        None,
        409,
        Some("model_not_configured"),
    )
    .await?;

    // A real OpenAI-compatible endpoint: the reference deployment's shape,
    // a proxy in front and a model called `qwen`.
    let llm = StubLlm::start().await?;
    let named = call
        .check(
            Method::PUT,
            &base,
            "/api/settings/model",
            "/api/settings/model",
            &settings_cookie,
            Some(json!({
                "base_url": llm.base_url(),
                "model": "qwen",
                "credential": "sk-the-conformance-key",
            })),
            200,
            None,
        )
        .await?;
    assert_eq!(named.body["credential"]["source"], json!("companion"));
    assert!(
        !named.body.to_string().contains("sk-the-conformance-key"),
        "the credential is write-only: no read of this API returns it"
    );
    call.check(
        Method::PUT,
        &base,
        "/api/settings/model",
        "/api/settings/model",
        &settings_cookie,
        Some(json!({ "base_url": "not-a-url", "model": "qwen" })),
        400,
        Some("invalid_base_url"),
    )
    .await?;
    let probed = call
        .check(
            Method::POST,
            &base,
            "/api/settings/model/probe",
            "/api/settings/model/probe",
            &settings_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(probed.body["outcome"], json!("ok"));

    // The three ways a configured endpoint fails, each its own code.
    let dead_endpoint = harness::unreachable_http_url()?;
    for (base_url, expected) in [
        (format!("{dead_endpoint}/v1"), "endpoint_unreachable"),
        (
            StubEndpoint::answering(401, "Unauthorized", json!({ "error": "no" }))
                .await?
                .base_url(),
            "endpoint_refused",
        ),
        (
            StubEndpoint::answering(200, "OK", json!({ "service": "not an llm" }))
                .await?
                .base_url(),
            "endpoint_not_compatible",
        ),
    ] {
        call.check(
            Method::PUT,
            &base,
            "/api/settings/model",
            "/api/settings/model",
            &settings_cookie,
            Some(json!({ "base_url": base_url, "model": "qwen" })),
            200,
            None,
        )
        .await?;
        call.check(
            Method::POST,
            &base,
            "/api/settings/model/probe",
            "/api/settings/model/probe",
            &settings_cookie,
            None,
            502,
            Some(expected),
        )
        .await?;
    }
    call.check(
        Method::DELETE,
        &base,
        "/api/settings/model",
        "/api/settings/model",
        &settings_cookie,
        None,
        204,
        None,
    )
    .await?;

    // The language, and the runtime's read of both.
    let language = call
        .check(
            Method::PUT,
            &base,
            "/api/settings/language",
            "/api/settings/language",
            &settings_cookie,
            Some(json!({ "language": "fr" })),
            200,
            None,
        )
        .await?;
    assert_eq!(language.body["language"], json!("fr"));
    call.check(
        Method::PUT,
        &base,
        "/api/settings/language",
        "/api/settings/language",
        &settings_cookie,
        Some(json!({ "language": "fr-FR" })),
        400,
        Some("unsupported_language"),
    )
    .await?;
    call.check(
        Method::GET,
        &base,
        "/api/settings/language",
        "/api/settings/language",
        &settings_cookie,
        None,
        200,
        None,
    )
    .await?;
    let runtime = call
        .check_with_bearer(
            Method::GET,
            &base,
            "/api/settings/runtime",
            "/api/settings/runtime",
            &[],
            Some(SERVICE_TOKEN),
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(
        runtime.body["llm"],
        Value::Null,
        "the model was deleted above, and the runtime is told that rather than left to \
         guess between a Gateway it cannot reach and one that named nothing"
    );
    assert_eq!(runtime.body["language"], json!("fr"));
    drop(llm);

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
        // Open to everyone, and still closed here: a deployment with no owner
        // has nothing to describe, and answering "no account yet" would send
        // a returning user to the account form (#112).
        (Method::GET, "/api/deployment", "/api/deployment"),
        // The settings (#98) are configured with sign-in — same state
        // directory, same owner — so a Gateway with no owner keeps none and
        // the guard closes them with everything else, the runtime's
        // service-token read included.
        (Method::GET, "/api/settings/model", "/api/settings/model"),
        (Method::PUT, "/api/settings/model", "/api/settings/model"),
        (Method::DELETE, "/api/settings/model", "/api/settings/model"),
        (
            Method::POST,
            "/api/settings/model/probe",
            "/api/settings/model/probe",
        ),
        (
            Method::GET,
            "/api/settings/language",
            "/api/settings/language",
        ),
        (
            Method::PUT,
            "/api/settings/language",
            "/api/settings/language",
        ),
        (
            Method::GET,
            "/api/settings/runtime",
            "/api/settings/runtime",
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
    // The approval search window (#24) is widened past the shared stream's
    // whole length, so that a suggestion nobody published is answered `404
    // suggestion_not_found` — the whole stream having been read — rather
    // than the `410` a bounded search that gave up would give. Both answers
    // are exercised; this Gateway is the one that gives the first.
    let consenting = GatewayProc::start(&gateway_env_with(
        &consent_static,
        &[
            ("GATEWAY_NATS_URL", &nats_url()),
            ("GATEWAY_APPROVAL_LOOKUP_WINDOW", "100000000"),
        ],
    ))?;
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

    // And the owner (#149), on both consent operations that take a subject.
    // No ghost has to be configured for this: the owner's own Matrix ID is
    // always one of their identities, which is what makes the refusal
    // reachable on every deployment. The behaviour — including the row an
    // upgrade left behind — is `tests/consent_snapshot.rs`'s; what is driven
    // here is the answer the description declares.
    let refused_decision = call
        .check(
            Method::POST,
            &consenting_base,
            "/api/consent/decisions",
            "/api/consent/decisions",
            &deciding_cookie,
            Some(json!({
                "subject": { "type": "contact", "id": owner_id },
                "new_state": "revoked",
                "scope": { "networks": ["whatsapp"] }
            })),
            409,
            Some("subject_is_the_owner"),
        )
        .await?;
    assert!(
        refused_decision.body["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(owner_id.as_str())),
        "the refusal says which subject, and why: {}",
        refused_decision.body
    );
    call.check(
        Method::GET,
        &consenting_base,
        "/api/consent/effective",
        &format!(
            "/api/consent/effective?contact={}&network=whatsapp",
            owner_id.replace('@', "%40").replace(':', "%3A")
        ),
        &deciding_cookie,
        None,
        409,
        Some("subject_is_the_owner"),
    )
    .await?;

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
    // Who has no consent state at all, served beside the state itself (#149):
    // the set cannot be derived from a bridge, so the single writer of consent
    // state is what hands it to the one consumer that needs it.
    assert!(
        snapshot.body["owner_identities"]
            .as_array()
            .is_some_and(|identities| identities.iter().any(|id| id == &json!(owner_id))),
        "the snapshot names the owner's own identities: {}",
        snapshot.body
    );

    // The pending contacts (#54), on the same Gateway: it consumes the
    // inbound stream the Sensor publishes to, so the contact this test
    // invents appears in the list once the projection has read it. The
    // behaviour is `tests/pending.rs`'s; what is driven here is every answer
    // the description declares.
    let correspondent = format!(
        "@whatsapp_g54_openapi_{}:{SERVER_NAME}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let inbound = inbound_event(&correspondent);
    let bus = Bus::connect().await?;
    bus.ensure_stream("twalk", &["twalk.>"]).await?;
    bus.publish_event("twalk.inbound.message.received.v1", &inbound)
        .await?;
    poll_until(
        || async {
            let listed: Value = http
                .get(format!("{consenting_base}/api/contacts/pending"))
                .header(
                    reqwest::header::COOKIE,
                    format!("twalk_device={deciding_device}"),
                )
                .send()
                .await
                .ok()?
                .json()
                .await
                .ok()?;
            listed["contacts"]
                .as_array()?
                .iter()
                .any(|entry| entry["contact"] == json!(correspondent))
                .then_some(())
        },
        "the projection to put the published contact in the pending list",
    )
    .await?;
    let pending = call
        .check(
            Method::GET,
            &consenting_base,
            "/api/contacts/pending",
            "/api/contacts/pending?network=whatsapp",
            &deciding_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert!(
        pending.body["contacts"]
            .as_array()
            .is_some_and(|contacts| contacts
                .iter()
                .any(|entry| entry["contact"] == json!(correspondent))),
        "the contact that wrote is waiting for a decision: {}",
        pending.body
    );
    call.check(
        Method::GET,
        &consenting_base,
        "/api/contacts/pending",
        "/api/contacts/pending?network=irc",
        &deciding_cookie,
        None,
        400,
        Some("unknown_value"),
    )
    .await?;

    // The display names: read from the bus, and never stored — which is why
    // they are their own call.
    let names = call
        .check(
            Method::GET,
            &consenting_base,
            "/api/contacts/display-names",
            &format!(
                "/api/contacts/display-names?contact={}",
                correspondent.replace('@', "%40").replace(':', "%3A")
            ),
            &deciding_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(
        names.body["contacts"][0]["display_name"].as_str(),
        Some("Aicha Benali"),
        "the name comes from the bus: {}",
        names.body
    );
    call.check(
        Method::GET,
        &consenting_base,
        "/api/contacts/display-names",
        "/api/contacts/display-names",
        &deciding_cookie,
        None,
        400,
        Some("malformed_request"),
    )
    .await?;

    // The write path's refusals, one per code the description enumerates.
    for (body, error) in [
        (
            json!({
                "subject": { "type": "device", "id": "assistant" },
                "new_state": "granted",
                "scope": { "networks": ["whatsapp"] }
            }),
            "unknown_value",
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

    // --- approvals (#24), on the same Gateway: the act that turns a
    // suggestion into an outbound reply. Its behaviour is
    // `tests/approvals.rs`'s; what is driven here is every answer the
    // description declares.
    //
    // A conversation of this run's own: the message, then the suggestion
    // that answers it, both published as a Sensor and a persona would.
    let approvable = format!(
        "@whatsapp_openapi_g24_{}:{SERVER_NAME}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let approvable_room = format!(
        "!openapig24{}:{SERVER_NAME}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    call.check(
        Method::POST,
        &consenting_base,
        "/api/consent/decisions",
        "/api/consent/decisions",
        &deciding_cookie,
        Some(json!({
            "subject": { "type": "contact", "id": approvable },
            "new_state": "granted",
            "scope": { "networks": ["whatsapp"] }
        })),
        201,
        None,
    )
    .await?;
    let trigger = approval_trigger_event(&approvable, &approvable_room);
    bus.publish_event("twalk.inbound.message.received.v1", &trigger)
        .await?;
    let suggestion = approval_suggestion_event(&trigger, 3600);
    let suggestion_id = suggestion["id"].as_str().unwrap().to_owned();
    bus.publish_event("twalk.persona.suggest.produced.v1", &suggestion)
        .await?;

    // Nothing to approve: the whole retained stream was read, which is why
    // this Gateway's search window is wider than the stream.
    call.check(
        Method::POST,
        &consenting_base,
        "/api/approvals",
        "/api/approvals",
        &deciding_cookie,
        Some(json!({ "suggestion_event_id": sha256_hex("openapi-no-such-suggestion") })),
        404,
        Some("suggestion_not_found"),
    )
    .await?;
    // Never a batch, and never under another name.
    call.check(
        Method::POST,
        &consenting_base,
        "/api/approvals",
        "/api/approvals",
        &deciding_cookie,
        Some(json!([{ "suggestion_event_id": suggestion_id }])),
        400,
        Some("approval_is_not_a_batch"),
    )
    .await?;
    call.check(
        Method::POST,
        &consenting_base,
        "/api/approvals",
        "/api/approvals",
        &deciding_cookie,
        Some(json!({
            "suggestion_event_id": suggestion_id,
            "approved_by": "@not-the-owner:test.twalk"
        })),
        403,
        Some("approved_by_is_not_the_owner"),
    )
    .await?;
    // The act itself.
    let approved = call
        .check(
            Method::POST,
            &consenting_base,
            "/api/approvals",
            "/api/approvals",
            &deciding_cookie,
            Some(json!({
                "suggestion_event_id": suggestion_id,
                "final": { "body": "d'accord pour 20h", "format": "text/plain" }
            })),
            201,
            None,
        )
        .await?;
    assert_eq!(approved.body["publication"].as_str(), Some("published"));
    assert_eq!(approved.body["edited"].as_bool(), Some(true));
    // And again: refused, carrying the first approval so a client whose
    // answer was lost learns where its reply went.
    let again = call
        .check(
            Method::POST,
            &consenting_base,
            "/api/approvals",
            "/api/approvals",
            &deciding_cookie,
            Some(json!({ "suggestion_event_id": suggestion_id })),
            409,
            Some("already_approved"),
        )
        .await?;
    assert_eq!(
        again.body["approval"]["event_id"], approved.body["event_id"],
        "the already-approved answer names the approval that stands: {}",
        again.body
    );
    // The read-back: the answer to "did my reply go out?".
    let recorded = call
        .check(
            Method::GET,
            &consenting_base,
            "/api/approvals/{suggestion_event_id}",
            &format!("/api/approvals/{suggestion_id}"),
            &deciding_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(
        recorded.body["stream_sequence"], approved.body["stream_sequence"],
        "the read-back names the same position the approval did: {}",
        recorded.body
    );
    call.check(
        Method::GET,
        &consenting_base,
        "/api/approvals/{suggestion_event_id}",
        &absent_approval,
        &deciding_cookie,
        None,
        404,
        Some("approval_not_found"),
    )
    .await?;
    call.check(
        Method::GET,
        &consenting_base,
        "/api/approvals/{suggestion_event_id}",
        "/api/approvals/not-an-event-id",
        &deciding_cookie,
        None,
        400,
        Some("malformed_request"),
    )
    .await?;

    // --- reading suggestions (#97), on the same Gateway and the same bus:
    // the listing the approval screen draws from, and one suggestion by id.
    // Its behaviour is `tests/suggestions.rs`'s; what is driven here is every
    // answer the description declares.
    //
    // One suggestion this build cannot read, so that the listing's tolerance
    // and the single read's refusal are both exercised against a real
    // message on the real bus.
    bus.publish_event(
        "twalk.persona.suggest.produced.v1",
        &unreadable_suggestion_event(),
    )
    .await?;
    let unreadable_id = unreadable_suggestion_event()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let listing = call
        .check(
            Method::GET,
            &consenting_base,
            "/api/suggestions",
            "/api/suggestions?limit=200",
            &deciding_cookie,
            None,
            200,
            None,
        )
        .await?;
    // Whether a runtime is present (#189), on the same bus. Nothing in this
    // suite runs a runtime or creates a persona consumer, so the honest
    // answer is `never` with an empty list — the three states against real
    // consumers are `tests/runtime.rs`'s.
    let runtime = call
        .check(
            Method::GET,
            &consenting_base,
            "/api/runtime",
            "/api/runtime",
            &deciding_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(
        runtime.body["presence"].as_str(),
        Some("never"),
        "no runtime has ever hosted a persona on this suite's bus: {}",
        runtime.body
    );
    assert!(
        listing.body["suggestions"]
            .as_array()
            .is_some_and(|entries| entries
                .iter()
                .any(|entry| entry["event_id"].as_str() == Some(&suggestion_id))),
        "the suggestion this suite published is not in the listing: {}",
        listing.body
    );
    // It was approved above, so the listing says so rather than offering it
    // again — and the record is the same document `GET /api/approvals/{id}`
    // answered with.
    let single = call
        .check(
            Method::GET,
            &consenting_base,
            "/api/suggestions/{suggestion_event_id}",
            &format!("/api/suggestions/{suggestion_id}"),
            &deciding_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(single.body["standing"].as_str(), Some("approved"));
    assert_eq!(
        single.body["approval"]["stream_sequence"], approved.body["stream_sequence"],
        "the suggestion's approval names the same position the approval did: {}",
        single.body
    );
    // Found and not understood: neither a 404 nor a silence.
    call.check(
        Method::GET,
        &consenting_base,
        "/api/suggestions/{suggestion_event_id}",
        &format!("/api/suggestions/{unreadable_id}"),
        &deciding_cookie,
        None,
        409,
        Some("suggestion_unreadable"),
    )
    .await?;
    call.check(
        Method::GET,
        &consenting_base,
        "/api/suggestions/{suggestion_event_id}",
        &format!(
            "/api/suggestions/{}",
            sha256_hex("openapi-no-such-suggestion")
        ),
        &deciding_cookie,
        None,
        404,
        Some("suggestion_not_found"),
    )
    .await?;
    call.check(
        Method::GET,
        &consenting_base,
        "/api/suggestions/{suggestion_event_id}",
        "/api/suggestions/not-an-event-id",
        &deciding_cookie,
        None,
        400,
        Some("malformed_request"),
    )
    .await?;
    call.check(
        Method::GET,
        &consenting_base,
        "/api/suggestions",
        "/api/suggestions?limit=0",
        &deciding_cookie,
        None,
        400,
        Some("malformed_request"),
    )
    .await?;
    consenting.stop().await;

    // --- a Gateway whose approval search is bounded to one stream position:
    // the suggestion above is far behind the head, so it is `410 gone` and
    // not `404 not found`. Two answers for two different facts, and the
    // description declares both.
    //
    let run = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_nanos();
    // Traffic after the suggestion, so that it is genuinely behind the head:
    // a window of one position only excludes it once something newer exists.
    //
    // Each filler is a contact of this run's own, and that is not tidiness:
    // `approval_trigger_event` keys the CloudEvents id on the sender, the id
    // is the `Nats-Msg-Id`, and JetStream absorbs a duplicate for two minutes
    // — so fillers with a fixed name were *deduplicated* when this suite ran
    // twice inside that window, the head did not move, the suggestion was
    // still inside a one-position window, and this assertion failed with
    // `409 consent_pending` on a Gateway whose consent store is empty. A
    // suite that passes only when it has not run recently is worse than one
    // that fails (#206 found this while adding the endpoint below).
    for index in 0..4 {
        let filler = approval_trigger_event(
            &format!("@whatsapp_openapi_g24_filler_{index}_{run}:{SERVER_NAME}"),
            &format!("!openapig24filler{index}{run}:{SERVER_NAME}"),
        );
        bus.publish_event("twalk.inbound.message.received.v1", &filler)
            .await?;
    }
    let narrow_static = companion_build("openapi-approval-window")?;
    let narrow = GatewayProc::start(&gateway_env_with(
        &narrow_static,
        &[
            ("GATEWAY_NATS_URL", &nats_url()),
            ("GATEWAY_APPROVAL_LOOKUP_WINDOW", "1"),
        ],
    ))?;
    let narrow_base = narrow.base_url().await?;
    wait_until_answering(&narrow_base).await?;
    let (narrow_device, _) =
        sign_in_cookies(&http, &narrow_base, &owner, "the narrow device").await?;
    call.check(
        Method::POST,
        &narrow_base,
        "/api/approvals",
        "/api/approvals",
        &[("twalk_device", narrow_device.as_str())],
        Some(json!({ "suggestion_event_id": suggestion_id })),
        410,
        Some("suggestion_out_of_reach"),
    )
    .await?;
    // The same fact through the read door (#97), with the same code and the
    // same status: a bounded read that gave up is not a suggestion that does
    // not exist, whichever endpoint is asked.
    call.check(
        Method::GET,
        &narrow_base,
        "/api/suggestions/{suggestion_event_id}",
        &format!("/api/suggestions/{suggestion_id}"),
        &[("twalk_device", narrow_device.as_str())],
        None,
        410,
        Some("suggestion_out_of_reach"),
    )
    .await?;
    narrow.stop().await;

    // --- a Gateway whose bus is configured and does not answer: `502`, and
    // not the `503` that would tell a client this deployment does not do
    // approvals. What failed is the thing behind the Gateway, and the fixes
    // are different.
    let busless_static = companion_build("openapi-approval-bus-down")?;
    let busless = GatewayProc::start(&gateway_env_with(
        &busless_static,
        &[("GATEWAY_NATS_URL", &unreachable_nats_url()?)],
    ))?;
    let busless_base = busless.base_url().await?;
    wait_until_answering(&busless_base).await?;
    let (busless_device, _) =
        sign_in_cookies(&http, &busless_base, &owner, "the busless device").await?;
    call.check(
        Method::POST,
        &busless_base,
        "/api/approvals",
        "/api/approvals",
        &[("twalk_device", busless_device.as_str())],
        Some(json!({ "suggestion_event_id": suggestion_id })),
        502,
        Some("bus_unreachable"),
    )
    .await?;
    call.check(
        Method::GET,
        &busless_base,
        "/api/suggestions",
        "/api/suggestions",
        &[("twalk_device", busless_device.as_str())],
        None,
        502,
        Some("bus_unreachable"),
    )
    .await?;
    call.check(
        Method::GET,
        &busless_base,
        "/api/suggestions/{suggestion_event_id}",
        &format!("/api/suggestions/{suggestion_id}"),
        &[("twalk_device", busless_device.as_str())],
        None,
        502,
        Some("bus_unreachable"),
    )
    .await?;
    // The runtime read (#189) on a bus that does not answer is a 502 and not
    // `never`: unknown is not absent.
    call.check(
        Method::GET,
        &busless_base,
        "/api/runtime",
        "/api/runtime",
        &[("twalk_device", busless_device.as_str())],
        None,
        502,
        Some("bus_unreachable"),
    )
    .await?;
    busless.stop().await;

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

    // --- a Gateway configured with a bus nothing listens on: its pending
    // list still answers, because that comes from its own store, and the
    // display names — which do come from the bus — say which half failed
    // (#54).
    let dead_bus_static = companion_build("openapi-dead-bus")?;
    let dead_bus = GatewayProc::start(&gateway_env_with_consent(
        &dead_bus_static,
        &unreachable_nats_url()?,
    ))?;
    let dead_bus_base = dead_bus.base_url().await?;
    wait_until_answering(&dead_bus_base).await?;
    let (dead_bus_device, _) =
        sign_in_cookies(&http, &dead_bus_base, &owner, "the offline device").await?;
    call.check(
        Method::GET,
        &dead_bus_base,
        "/api/contacts/display-names",
        "/api/contacts/display-names?contact=%40a%3Atest.twalk",
        &[("twalk_device", dead_bus_device.as_str())],
        None,
        502,
        Some("bus_unreachable"),
    )
    .await?;
    dead_bus.stop().await;

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
    // And the other half of that 502, which used to share its code (#116):
    // a bridge that answered, in JSON this build cannot use. Declared in
    // `BridgeUnavailable`, so a description that forgot it fails here.
    stub.answer_next_start_with(json!({ "ok": true, "login": { "id": "33612345678" } }));
    call.check(
        Method::POST,
        &bridged_base,
        "/api/bridges/{bridge_id}/login",
        &stub_login,
        &bridge_cookie,
        Some(json!({ "flow_id": QR_FLOW })),
        502,
        Some("bridge_answer_unusable"),
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
    // The portal register (#105). This Gateway has two bridges: one whose
    // appservice token is a test constant the homeserver has never heard of,
    // and one with no token at all. So both are unreadable, for two different
    // reasons, and the answer says so — which is the property that matters
    // more than a populated list here: a count of conversations must never
    // quietly cover fewer bridges than the user has connected.
    // `tests/portals.rs` is where the register is driven against real portal
    // rooms with a credential that works.
    let register = call
        .check(
            Method::GET,
            &bridged_base,
            "/api/portals",
            "/api/portals",
            &bridge_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(
        register.body["bridges"].as_array().map(Vec::len),
        Some(2),
        "every configured bridge is in the answer, readable or not: {}",
        register.body
    );
    assert_eq!(
        register.body["bridges"][0]["readable"], false,
        "a bridge whose appservice token the homeserver rejects is unreadable, not empty: {}",
        register.body
    );
    assert_eq!(
        register.body["summary"]["total"], 0,
        "and no total pretends to cover it: {}",
        register.body
    );
    // The registry of connections (#269): nothing declared, so one per bridge,
    // named after its network — the id every decision is migrated onto.
    let connections = call
        .check(
            Method::GET,
            &bridged_base,
            "/api/connections",
            "/api/connections",
            &bridge_cookie,
            None,
            200,
            None,
        )
        .await?;
    let ids: Vec<&str> = connections.body["connections"]
        .as_array()
        .context("a list of connections")?
        .iter()
        .filter_map(|connection| connection["id"].as_str())
        .collect();
    assert_eq!(
        ids,
        ["whatsapp", "signal", "matrix"],
        "one per bridge in GATEWAY_BRIDGES order, named after its network, and the native \
         Matrix connection every deployment has: {}",
        connections.body
    );
    for connection in connections.body["connections"].as_array().unwrap() {
        assert_eq!(
            connection["id"], connection["kind"],
            "derived from a bridge, a connection is named after its network: {connection}"
        );
    }
    assert_eq!(
        connections.body["connections"][0]["bridge_id"], STUB_BRIDGE_ID,
        "a connection a bridge carries names it: {}",
        connections.body
    );
    assert!(
        connections.body["connections"][2]["bridge_id"].is_null(),
        "the native connection rides no bridge: {}",
        connections.body
    );
    // The journal of moves (#255): this Gateway has a store, so the answer is
    // a list — empty, since nothing it can read has moved. A move against real
    // rooms is `tests/portals.rs`'s.
    let moves = call
        .check(
            Method::GET,
            &bridged_base,
            "/api/portals/moves",
            "/api/portals/moves",
            &bridge_cookie,
            None,
            200,
            None,
        )
        .await?;
    assert_eq!(moves.body["moves"], json!([]), "{}", moves.body);
    // A room id that is not a portal of any readable bridge: an outcome, not
    // a status code, and nothing is attempted with the appservice credential.
    let refused = call
        .check(
            Method::POST,
            &bridged_base,
            "/api/portals/observation",
            "/api/portals/observation",
            &bridge_cookie,
            Some(json!({ "rooms": ["!nobody:test.twalk"], "observed": true })),
            200,
            None,
        )
        .await?;
    assert_eq!(
        refused.body["outcomes"][0]["status"], "unknown_portal",
        "a room this Gateway does not hold as a portal is never invited into: {}",
        refused.body
    );
    // A decision about nothing is the one shape that is refused outright: it
    // is a client bug, and answering 200 to it would hide one.
    call.check(
        Method::POST,
        &bridged_base,
        "/api/portals/observation",
        "/api/portals/observation",
        &bridge_cookie,
        Some(json!({ "rooms": [], "observed": true })),
        400,
        Some("invalid_request"),
    )
    .await?;
    bridged.stop().await;
    stub.stop().await;

    // --- Hermes's answer webhook (#206, ADR 0032): the one route whose
    // caller is outside this deployment. Everything asserted here is reached
    // without any bus state, which is why it belongs in the conformance suite
    // rather than in `tests/hermes_answers.rs`: what a wrong, stale, unsigned
    // or unusable answer is *answered with* is the description's business, and
    // what a right one does to the bus is that suite's.
    let hermes_static = companion_build("openapi-hermes-answers")?;
    let hermes = GatewayProc::start(&harness::gateway_env_with_hermes(
        &hermes_static,
        &nats_url(),
    ))?;
    let hermes_base = hermes.base_url().await?;
    wait_until_answering(&hermes_base).await?;
    let absent_trigger = sha256_hex("a trigger no suite ever published, #206");
    let good_reference = harness::hermes_reference("assistant", &absent_trigger, 1);

    // No signature at all. One answer for every way the credential can be
    // missing or wrong.
    let unsigned = harness::hermes_push(&harness::hermes_answer(&good_reference, "ok", Some("en")));
    call.check_raw(
        Method::POST,
        &hermes_base,
        "/_twalk/hermes/answers",
        "/_twalk/hermes/answers",
        &[],
        &unsigned,
        401,
        Some("unauthenticated"),
    )
    .await?;
    // A signature over other bytes, which is the interesting half: the MAC
    // covers the body, so a push whose body was edited in flight fails here
    // even though the header is a real signature of something.
    call.check_raw(
        Method::POST,
        &hermes_base,
        "/_twalk/hermes/answers",
        "/_twalk/hermes/answers",
        &[(
            "X-Hermes-Signature-256",
            harness::hermes_signature("a different body entirely").as_str(),
        )],
        &unsigned,
        401,
        Some("unauthenticated"),
    )
    .await?;

    // Correctly signed and not one of Hermes's deliveries.
    let nonsense = "{\"not\":\"a push\"}";
    call.check_raw(
        Method::POST,
        &hermes_base,
        "/_twalk/hermes/answers",
        "/_twalk/hermes/answers",
        &[(
            "X-Hermes-Signature-256",
            harness::hermes_signature(nonsense).as_str(),
        )],
        nonsense,
        400,
        Some("invalid_request"),
    )
    .await?;

    // Correctly signed, well formed, and hours old. The timestamp is inside
    // the signed body and is this wire format's only replay protection, so it
    // is checked rather than read.
    let stale = harness::hermes_push_at(
        &harness::hermes_answer(&good_reference, "ok", Some("en")),
        "2026-09-17T10:00:00Z",
    );
    call.check_raw(
        Method::POST,
        &hermes_base,
        "/_twalk/hermes/answers",
        "/_twalk/hermes/answers",
        &[(
            "X-Hermes-Signature-256",
            harness::hermes_signature(&stale).as_str(),
        )],
        &stale,
        400,
        Some("hermes_answer_stale"),
    )
    .await?;

    // A turn somebody had with Hermes directly, on the same profile. Ignored
    // with a `200` and a reason, because a run that was never a Twalk wake is
    // not a failure — and an endpoint that answered `4xx` to it would teach an
    // operator to ignore its errors.
    let other_hook =
        harness::hermes_push(&harness::hermes_answer(&good_reference, "ok", Some("en")))
            .replace("\"transform_llm_output\"", "\"on_session_end\"");
    let ignored = call
        .check_raw(
            Method::POST,
            &hermes_base,
            "/_twalk/hermes/answers",
            "/_twalk/hermes/answers",
            &[(
                "X-Hermes-Signature-256",
                harness::hermes_signature(&other_hook).as_str(),
            )],
            &other_hook,
            200,
            None,
        )
        .await?;
    assert_eq!(
        ignored.body["status"].as_str(),
        Some("ignored"),
        "a push that is not a Twalk wake is ignored and says why: {}",
        ignored.body
    );
    assert_eq!(ignored.body["reason"].as_str(), Some("not_our_hook"));

    // The answer with no language — the acceptance criterion this endpoint
    // exists to satisfy loudly. Refused, and deliberately not defaulted: a
    // reply's disclosure is written in the language of the reply and not the
    // user's (ADR 0031).
    let languageless = harness::hermes_push(&harness::hermes_answer(
        &good_reference,
        "See you at 8",
        None,
    ));
    let refused = call
        .check_raw(
            Method::POST,
            &hermes_base,
            "/_twalk/hermes/answers",
            "/_twalk/hermes/answers",
            &[(
                "X-Hermes-Signature-256",
                harness::hermes_signature(&languageless).as_str(),
            )],
            &languageless,
            422,
            Some("hermes_answer_has_no_language"),
        )
        .await?;
    assert!(
        refused.body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("ADR 0031"),
        "the refusal says which decision it is keeping: {}",
        refused.body
    );

    // A push over the endpoint's limit. Signed, so that what is being
    // asserted is the limit and not the credential.
    let fat = harness::hermes_push(&harness::hermes_answer(
        &good_reference,
        &"x".repeat(300_000),
        Some("en"),
    ));
    call.check_raw(
        Method::POST,
        &hermes_base,
        "/_twalk/hermes/answers",
        "/_twalk/hermes/answers",
        &[(
            "X-Hermes-Signature-256",
            harness::hermes_signature(&fat).as_str(),
        )],
        &fat,
        413,
        Some("push_too_large"),
    )
    .await?;

    // A reference whose shape is right and whose message is not on the bus.
    // The same code and the same status `POST /api/approvals` gives for the
    // same fact.
    let orphan = harness::hermes_push(&harness::hermes_answer(
        &good_reference,
        "an answer to a message that was never published",
        Some("en"),
    ));
    call.check_raw(
        Method::POST,
        &hermes_base,
        "/_twalk/hermes/answers",
        "/_twalk/hermes/answers",
        &[(
            "X-Hermes-Signature-256",
            harness::hermes_signature(&orphan).as_str(),
        )],
        &orphan,
        404,
        Some("trigger_not_found"),
    )
    .await?;
    hermes.stop().await;

    // And the same route on a Gateway that configured no seam: the variable
    // that would open it, not a refusal shaped like a wrong answer.
    let seamless_static = companion_build("openapi-hermes-seamless")?;
    let seamless_gateway = GatewayProc::start(&gateway_env(&seamless_static))?;
    let seamless_base = seamless_gateway.base_url().await?;
    wait_until_answering(&seamless_base).await?;
    let seamless = harness::hermes_push(&harness::hermes_answer(&good_reference, "ok", Some("en")));
    call.check_raw(
        Method::POST,
        &seamless_base,
        "/_twalk/hermes/answers",
        "/_twalk/hermes/answers",
        &[(
            "X-Hermes-Signature-256",
            harness::hermes_signature(&seamless).as_str(),
        )],
        &seamless,
        503,
        Some("hermes_answers_not_configured"),
    )
    .await?;
    seamless_gateway.stop().await;

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
