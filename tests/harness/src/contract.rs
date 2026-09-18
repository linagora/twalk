//! Contract validation: the schemas in `contracts/cloudevents/v1/` are the
//! source of truth, so every event-producing behaviour asserts through here.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

/// The contract lives at the repository root, two levels above this crate
/// (`tests/harness/`).
fn contract_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("contracts")
        .join("cloudevents")
        .join("v1")
}

/// Validates an event against one of the contract schemas by type name,
/// e.g. `validate_against_contract(&event, "inbound.message.received")`.
pub fn validate_against_contract(event: &Value, type_name: &str) -> Result<()> {
    let schema_path = contract_dir().join(format!("{type_name}.schema.json"));
    let schema: Value = serde_json::from_slice(
        &std::fs::read(&schema_path)
            .with_context(|| format!("failed to read schema {}", schema_path.display()))?,
    )?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|e| anyhow!("invalid schema {type_name}: {e}"))?;
    let errors = validator
        .iter_errors(event)
        .map(|e| format!("  - {}: {}", e.instance_path(), e))
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        bail!(
            "event failed contract validation for {type_name}:\n{}",
            errors.join("\n")
        );
    }
    Ok(())
}

/// Loads one of the contract fixtures by type name.
pub fn contract_fixture(type_name: &str) -> Result<Value> {
    let path = contract_dir()
        .join("fixtures")
        .join(format!("{type_name}.json"));
    let fixture = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?,
    )?;
    Ok(fixture)
}

/// Lists every contract type that has a fixture, straight from the fixtures
/// directory: a new fixture is automatically covered, never silently skipped.
/// The `variants/` subdirectory is not a type — it holds the conditional
/// shapes listed by [`contract_variant_fixtures`].
pub fn contract_fixture_types() -> Result<Vec<String>> {
    let mut types = Vec::new();
    for entry in std::fs::read_dir(contract_dir().join("fixtures"))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            types.push(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    types.sort();
    Ok(types)
}

/// The `subject` pattern every schema uses when the subject is a Matrix user
/// ID — a person. It is the machine-readable answer to "can the owner be the
/// subject of this type?", and the reason [`contract_types_about_a_person`]
/// can be read off the contract instead of maintained by hand.
pub const MATRIX_USER_ID_SUBJECT_PATTERN: &str = "^@[a-zA-Z0-9._=/+-]+:[^/]+$";

/// Lists every contract type that has a schema, from the schema files.
///
/// Distinct from [`contract_fixture_types`] on purpose: a type is defined by
/// its schema, and a test that asks "which types exist?" must not be able to
/// miss one because somebody added a schema and forgot its fixture.
pub fn contract_schema_types() -> Result<Vec<String>> {
    let mut types = Vec::new();
    for entry in std::fs::read_dir(contract_dir())? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(type_name) = name.strip_suffix(".schema.json") {
            types.push(type_name.to_owned());
        }
    }
    types.sort();
    Ok(types)
}

/// Loads one contract schema by type name.
pub fn contract_schema(type_name: &str) -> Result<Value> {
    let path = contract_dir().join(format!("{type_name}.schema.json"));
    let schema = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?,
    )?;
    Ok(schema)
}

/// Every contract type whose `subject` is a Matrix user ID — that is, every
/// type whose subject is a *person*, and therefore every type the deployment's
/// owner can be the subject of (issue #147).
///
/// Read from the schemas rather than listed, so that a tenth or eleventh type
/// cannot be added without a test asking what it does about the owner. The
/// discriminator is the schema's own `properties.subject.pattern`: a type
/// whose subject is a bridge id, a persona id or a CloudEvents id names no
/// person and cannot be about the owner.
///
/// The match is on the pattern *anchoring at `@`*, not on the exact string
/// in [`MATRIX_USER_ID_SUBJECT_PATTERN`]. A future type that spells the same
/// constraint slightly differently must still be caught: the failure mode
/// that matters here is the false negative — a type quietly left out of the
/// enumeration is a type where "the owner is never a contact" quietly stops
/// being checked.
pub fn contract_types_about_a_person() -> Result<Vec<String>> {
    let mut types = Vec::new();
    for type_name in contract_schema_types()? {
        let schema = contract_schema(&type_name)?;
        let names_a_matrix_user = schema
            .pointer("/properties/subject/pattern")
            .and_then(Value::as_str)
            .is_some_and(|pattern| pattern.starts_with("^@"));
        if names_a_matrix_user {
            types.push(type_name);
        }
    }
    Ok(types)
}

/// Whether a type's schema permits a `consent` extension on the envelope.
///
/// `false` is how a type says "there is no contact in this event" in a way a
/// producer cannot ignore: the schema declares no `consent` property and
/// `additionalProperties: false`, so a third-party producer that sets one
/// fails validation (ADR 0018, ADR 0021).
pub fn contract_type_allows_consent(type_name: &str) -> Result<bool> {
    let schema = contract_schema(type_name)?;
    let declared = schema.pointer("/properties/consent").is_some();
    let open = schema
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    Ok(declared || open)
}

/// Lists the contract's variant fixtures — `fixtures/variants/<type
/// name>/<variant>.json` — as (type name, variant) pairs. A variant
/// demonstrates one conditional shape of a type whose canonical fixture
/// stays in `fixtures/`, so that a shape the schema only allows under a
/// condition (a revoked sender's reduced message, ADR 0012) has a worked
/// example a producer can copy. Every variant validates against its type's
/// schema, like the canonical fixture.
pub fn contract_variant_fixtures() -> Result<Vec<(String, String)>> {
    let variants_dir = contract_dir().join("fixtures").join("variants");
    let mut variants = Vec::new();
    if !variants_dir.exists() {
        return Ok(variants);
    }
    for type_entry in std::fs::read_dir(&variants_dir)? {
        let type_dir = type_entry?.path();
        if !type_dir.is_dir() {
            continue;
        }
        let type_name = type_dir.file_name().unwrap().to_string_lossy().into_owned();
        for entry in std::fs::read_dir(&type_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                let variant = path.file_stem().unwrap().to_string_lossy().into_owned();
                variants.push((type_name.clone(), variant));
            }
        }
    }
    variants.sort();
    Ok(variants)
}

/// Loads one variant fixture by type name and variant, e.g.
/// `contract_variant_fixture("inbound.message.received", "revoked-sender")`.
pub fn contract_variant_fixture(type_name: &str, variant: &str) -> Result<Value> {
    let path = contract_dir()
        .join("fixtures")
        .join("variants")
        .join(type_name)
        .join(format!("{variant}.json"));
    let fixture = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?,
    )?;
    Ok(fixture)
}
