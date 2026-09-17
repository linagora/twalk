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
