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
