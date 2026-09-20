//! The events a test publishes: the contract's own fixtures, given a fresh
//! id, a time of now and whatever the test decides about them.
//!
//! Every builder starts from `contract_fixture` and changes only what a
//! run must own — the id (`sha256_hex(run + n)`, so two events of one run
//! never share one and two runs never collide on the bus's deduplication),
//! the time, and for a suggestion its `expires_at`, which every test sets
//! explicitly because it is what the clerk decides on. What the fixture
//! carries otherwise (the network, the persona, the contact's own words in
//! an inbound message) stays, and `Run::publish` validates the result
//! before it goes anywhere.

use anyhow::Result;
use serde_json::{json, Value};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use twalk_test_harness::{contract_fixture, sha256_hex};

/// A fresh deterministic-looking id for the `n`th event of `run`.
pub fn fresh_id(run: &str, n: u32) -> String {
    sha256_hex(&format!("{run}:{n}"))
}

/// Now, RFC 3339 in UTC with second precision — the shape every fixture's
/// `time` has.
pub fn now_rfc3339() -> String {
    in_seconds(0)
}

/// Now plus `seconds` (negative for the past), RFC 3339 in UTC.
pub fn in_seconds(seconds: i64) -> String {
    let at = OffsetDateTime::now_utc() + Duration::seconds(seconds);
    at.replace_nanosecond(0)
        .expect("zero is a nanosecond")
        .format(&Rfc3339)
        .expect("UTC formats as RFC 3339")
}

/// One `persona.suggest.produced`: the fixture's suggestion, as event `n`
/// of `run`, expiring `expires_in_seconds` from now. The trigger it names
/// is the fixture's, which is on no stream of this run.
pub fn suggestion(run: &str, n: u32, expires_in_seconds: i64) -> Result<Value> {
    let mut event = contract_fixture("persona.suggest.produced")?;
    event["id"] = json!(fresh_id(run, n));
    event["time"] = json!(now_rfc3339());
    event["data"]["expires_at"] = json!(in_seconds(expires_in_seconds));
    Ok(event)
}

/// One `inbound.message.received`, as event `n` of `run`: a contact's
/// message, with everything the fixture says about the contact.
pub fn inbound_message(run: &str, n: u32) -> Result<Value> {
    let mut event = contract_fixture("inbound.message.received")?;
    event["id"] = json!(fresh_id(run, n));
    event["time"] = json!(now_rfc3339());
    Ok(event)
}

/// One `persona.reply.approved`, as event `n` of `run`.
pub fn approval(run: &str, n: u32) -> Result<Value> {
    let mut event = contract_fixture("persona.reply.approved")?;
    event["id"] = json!(fresh_id(run, n));
    event["time"] = json!(now_rfc3339());
    Ok(event)
}

/// One `bridge.status.changed`, as event `n` of `run`: the fixture's
/// bridge, `bridge-gmessages-1`, going from `starting` to `connected`.
pub fn bridge_status(run: &str, n: u32) -> Result<Value> {
    let mut event = contract_fixture("bridge.status.changed")?;
    event["id"] = json!(fresh_id(run, n));
    event["time"] = json!(now_rfc3339());
    event["data"]["occurred_at"] = json!(now_rfc3339());
    Ok(event)
}

/// One `consent.state.changed` about a **contact**, as event `n` of `run`:
/// the fixture is about a persona, so the subject is rewritten to the
/// inbound fixture's sender, `@whatsapp_33612345678:example.com` — a
/// Matrix user ID, which is what `activite` must not carry.
pub fn consent_change_about_a_contact(run: &str, n: u32) -> Result<Value> {
    let mut event = contract_fixture("consent.state.changed")?;
    event["id"] = json!(fresh_id(run, n));
    event["time"] = json!(now_rfc3339());
    event["subject"] = json!(CONTACT_MATRIX_ID);
    event["data"]["subject"] = json!({ "type": "contact", "id": CONTACT_MATRIX_ID });
    event["data"]["occurred_at"] = json!(now_rfc3339());
    Ok(event)
}

/// One `persona.thinking.emitted`, as event `n` of `run`.
pub fn thinking(run: &str, n: u32) -> Result<Value> {
    let mut event = contract_fixture("persona.thinking.emitted")?;
    event["id"] = json!(fresh_id(run, n));
    event["time"] = json!(now_rfc3339());
    Ok(event)
}

/// The inbound fixture's sender: the Matrix user ID a contact has on the
/// bus, and the one identifier no channel may carry.
pub const CONTACT_MATRIX_ID: &str = "@whatsapp_33612345678:example.com";
