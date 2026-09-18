//! Shared integration-test harness for the Twalk components (ticket #20).
//!
//! Every component's suite tests at the same seam — its process boundary,
//! against a real stack — and therefore needs the same four things: the test
//! stack's lifecycle ([`stack`]), the bus ([`bus`]), contract validation
//! ([`contract`]) and a poller ([`poll_until`]). They started life inside the
//! Sensor suite (ticket 01) and live here so `sensor/` and `hermes/` share
//! one implementation. Persona tests additionally need an LLM that answers
//! the same way every run: [`stub_llm`].
//!
//! Component-specific helpers stay with their component: the Matrix `Bot`
//! and `SensorProc` in `sensor/tests/harness/`, which re-exports this crate
//! so its own test files see one flat `harness::` namespace.
//!
//! Nothing here reaches inside a component's process.

pub mod bus;
pub mod contract;
pub mod stack;
pub mod stub_llm;
mod wait;

pub use bus::{Bus, StoredMessage};
pub use contract::{
    contract_fixture, contract_fixture_types, contract_schema, contract_schema_types,
    contract_type_allows_consent, contract_types_about_a_person, contract_variant_fixture,
    contract_variant_fixtures, validate_against_contract, MATRIX_USER_ID_SUBJECT_PATTERN,
};
pub use stack::{ensure_stack, nats_url, synapse_url, SERVER_NAME};
pub use stub_llm::{StubLlm, StubRequest};
pub use wait::poll_until;

use sha2::{Digest, Sha256};

/// SHA-256 of the input as lowercase hex: recomputes the contract's
/// deterministic event ids independently of the component under test.
pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn sha256_hex_matches_the_known_digest_of_the_empty_string() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
