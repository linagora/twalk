//! Consent's HTTP surface: recording a decision, reading the current state,
//! and resolving what applies to one contact on one network (ticket #49).
//!
//! # Who is calling
//!
//! Nothing here authenticates: #52's guard already does, for everything
//! under `/api/` that its table does not except ([`crate::session_http`]).
//! A request that reaches these handlers carried a live device token, and
//! the guard put that [`Device`] in the request's extensions — so a handler
//! asks for it as an extractor and a route added without thinking about
//! authentication is protected anyway.
//!
//! The decision's `actor` is the deployment's **owner**, not the device: one
//! owner per Gateway (ADR 0011), so every device that can sign in is the
//! owner's, and what the audit trail records is the human. The device's id
//! goes to the log line instead, where it answers "from which of my devices
//! did I do that?".
//!
//! # What the answers look like
//!
//! One shape for every refusal, the Gateway's own `Error` document
//! (`openapi.yaml`): a stable `error` code a client branches on, and a
//! `detail` for an operator's logs — never a sentence to match on, and never
//! for display. The codes are
//! `malformed_request`, `unknown_value`, `unsupported_subject_type`,
//! `scope_contradicts_subject`, `subject_is_the_owner`,
//! `consent_not_configured` and `store_unavailable`; `unauthenticated` comes
//! from the guard.
//!
//! # The owner is not a subject
//!
//! `subject_is_the_owner` is ticket #149's, and it is a `409` rather than a
//! `400` or a `404`: the request is well formed and its subject is a perfectly
//! good Matrix ID, and what refuses it is a rule about who that Matrix ID is
//! (ADR 0018, ADR 0021). A `404` would say "no such subject", and this project
//! has closed a dozen defects whose whole cause was two situations sharing one
//! signal. The same code answers `GET /api/consent/effective` about an owner
//! identity, for the same reason: `pending` with `decided_by: null` means "no
//! decision was ever recorded", which is a contact's state and not an
//! invitation to take one about yourself.
//!
//! `GET /api/consent/state` does not refuse anything — it lists — so it simply
//! does not serve the owner's rows. That exclusion is the store's
//! ([`crate::store`]), not this module's, so every read of the consent state
//! has it whether or not a handler remembered.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use serde_json::{json, Value};
use tracing::{debug, error};

use crate::consent::{Decision, Effective, Invalid, Network, Subject};
use crate::http::Gateway;
use crate::outbox::Refused;
use crate::session::Device;
use crate::store::Entry;

/// The consent routes. Merged into the Gateway's router, so registering them
/// is additive to whatever else the API grows.
pub fn routes() -> Router<Gateway> {
    Router::new()
        .route("/api/consent/decisions", post(record_decision))
        .route("/api/consent/state", get(consent_state))
        .route("/api/consent/effective", get(effective_consent))
}

/// `POST /api/consent/decisions` — record one consent decision.
///
/// The body mirrors the contract's `data`, minus what the Gateway stamps
/// itself (`old_state` from the journal, `occurred_at` from the clock,
/// `actor` from the owner):
///
/// ```json
/// {
///   "subject": { "type": "contact", "id": "@whatsapp_33612345678:example.com" },
///   "new_state": "granted",
///   "scope": { "networks": ["whatsapp"] },
///   "reason": "optional, kept in the audit trail"
/// }
/// ```
///
/// `201` with the recorded decision and the id of the event the outbox will
/// publish; `200` with the same body when the identical decision was already
/// recorded (same subject, state, perimeter and instant), because the
/// contract's ids are deterministic and a retry must not become a second
/// decision. The answer returns as soon as the decision is **committed** —
/// publication follows through the outbox, so a bus that is down delays the
/// event and never refuses the decision.
async fn record_decision(
    State(gateway): State<Gateway>,
    Extension(device): Extension<Device>,
    body: String,
) -> Response {
    let Some(consent) = gateway.consent() else {
        return not_configured();
    };
    let body: Value = match serde_json::from_str(&body) {
        Ok(body) => body,
        Err(error) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "malformed_request",
                &format!("the request body is not JSON: {error}"),
            )
        }
    };
    let decision = match Decision::parse(&body) {
        Ok(decision) => decision,
        Err(invalid) => {
            debug!(
                code = invalid.code(),
                device = %device.id,
                "refused a consent decision"
            );
            return api_error(StatusCode::BAD_REQUEST, invalid.code(), &invalid.message());
        }
    };
    match consent.record(&decision, &device) {
        // The owner is not a subject (ticket #149, ADR 0018, ADR 0021). The
        // writer refused, and the caller is told which subject and why — never
        // a silent 2xx, which would leave a screen claiming a decision no
        // store holds, and never a `404`, which would say the subject does not
        // exist when in fact it is the user themselves.
        Err(Refused::Invalid(invalid)) => {
            debug!(
                code = invalid.code(),
                device = %device.id,
                "refused a consent decision"
            );
            api_error(
                StatusCode::from_u16(invalid.status()).unwrap_or(StatusCode::BAD_REQUEST),
                invalid.code(),
                &invalid.message(),
            )
        }
        Ok(committed) => {
            let recorded = &committed.recorded;
            let status = if committed.replayed {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            (
                status,
                Json(json!({
                    "event_id": committed.event_id,
                    "sequence": committed.sequence,
                    "subject": subject_json(&recorded.decision.subject),
                    "old_state": recorded.old_state.as_str(),
                    "new_state": recorded.decision.new_state.as_str(),
                    "scope": { "networks": networks_json(&recorded.decision.networks) },
                    "occurred_at": recorded.occurred_at,
                    "actor": recorded.actor,
                    "replayed": committed.replayed,
                })),
            )
                .into_response()
        }
        Err(Refused::Store(error)) => {
            // The decision was not recorded, so the honest answer is a
            // failure: a 2xx here would tell the user their consent is
            // stored when it is not.
            error!(%error, "failed to record a consent decision");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_unavailable",
                "the consent decision could not be recorded; nothing was changed",
            )
        }
    }
}

/// `GET /api/consent/state` — the current state, one entry per (subject,
/// network), as recorded.
///
/// Network entries are that network's default and contact entries override
/// them; the resolution is [`effective_consent`]. Revocations are as
/// explicit as grants, and an absent subject means "never decided" — never
/// "revoked" (ADR 0010). This is the projection #50 serves as a snapshot
/// alongside the stream position it reflects; it names no position, and has
/// no cap, because both are that ticket's.
///
/// The owner's own rows are not in it (ticket #149) — a row about the owner is
/// a row that should not exist, and this read is the one the Companion's
/// consent screen draws from, where until now it appeared as a decision the
/// user was invited to take about themselves.
async fn consent_state(State(gateway): State<Gateway>) -> Response {
    let Some(consent) = gateway.consent() else {
        return not_configured();
    };
    match consent.store().entries() {
        Ok(entries) => Json(json!({
            "entries": entries.iter().map(entry_json).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(error) => {
            error!(%error, "failed to read the consent state");
            store_unavailable()
        }
    }
}

/// `GET /api/consent/effective?contact=<matrix id>&network=<network>` — the
/// consent state that actually applies to one contact on one network, with
/// the precedence resolved.
///
/// The contact's own decision wins; without one, the network's default
/// applies; without either, `pending` — and `decided_by` is then `null`,
/// which is how a caller tells "never decided" from "decided pending".
async fn effective_consent(
    State(gateway): State<Gateway>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(consent) = gateway.consent() else {
        return not_configured();
    };
    let Some(contact) = query.get("contact").filter(|value| !value.is_empty()) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "malformed_request",
            "the contact query parameter is required: a contact's Matrix user ID",
        );
    };
    let Some(network) = query.get("network").filter(|value| !value.is_empty()) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "malformed_request",
            "the network query parameter is required",
        );
    };
    let Some(network) = Network::parse(network) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "unknown_value",
            &format!("network has the unknown value {network:?}"),
        );
    };
    // "What consent applies to the owner?" has no answer, and the honest reply
    // is to say so rather than to hand back `pending` with `decided_by: null`
    // (ticket #149). That document means "no decision was ever recorded",
    // which is a *contact's* state and the one thing ADR 0010 is emphatic
    // about keeping distinct; about the owner it would read as an invitation
    // to go and take the decision.
    if consent.owner().is_owner(contact) {
        let refusal = Invalid::SubjectIsTheOwner {
            id: contact.clone(),
        };
        debug!(code = refusal.code(), "refused an effective-consent read");
        return api_error(
            StatusCode::from_u16(refusal.status()).unwrap_or(StatusCode::CONFLICT),
            refusal.code(),
            &refusal.message(),
        );
    }
    match consent.store().effective(contact, network) {
        Ok(effective) => Json(effective_json(contact, network, &effective)).into_response(),
        Err(error) => {
            error!(%error, "failed to resolve the effective consent state");
            store_unavailable()
        }
    }
}

/// This deployment writes no consent: a `503` that names the variable, not a
/// `404` and not a silent success.
fn not_configured() -> Response {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "consent_not_configured",
        "this Gateway has no consent store: set GATEWAY_NATS_URL (and GATEWAY_OWNER, \
         which consent takes its owner, state directory and domain from)",
    )
}

fn store_unavailable() -> Response {
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store_unavailable",
        "the consent state could not be read",
    )
}

/// One error answer shape for the whole surface: the `Error` schema of
/// `openapi.yaml`, which every other endpoint of the Gateway answers with
/// too.
fn api_error(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({ "error": code, "detail": detail }))).into_response()
}

fn subject_json(subject: &Subject) -> Value {
    json!({ "type": subject.kind.as_str(), "id": subject.id })
}

fn networks_json(networks: &[Network]) -> Vec<Value> {
    networks
        .iter()
        .map(|network| Value::from(network.as_str()))
        .collect()
}

fn entry_json(entry: &Entry) -> Value {
    json!({
        "subject": subject_json(&entry.subject),
        "network": entry.network.as_str(),
        "state": entry.state.as_str(),
        "decided_at": entry.decided_at,
        "decision_sequence": entry.decision_sequence,
    })
}

fn effective_json(contact: &str, network: Network, effective: &Effective) -> Value {
    json!({
        "contact": contact,
        "network": network.as_str(),
        "state": effective.state.as_str(),
        "decided_by": effective.decided_by.as_ref().map(subject_json),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::{State, SubjectType};

    #[test]
    fn an_entry_renders_as_the_contract_names_its_parts() {
        let entry = Entry {
            subject: Subject {
                kind: SubjectType::Network,
                id: "whatsapp".to_owned(),
            },
            network: Network::Whatsapp,
            state: State::Granted,
            decided_at: "2026-09-17T10:00:00.000Z".to_owned(),
            decision_sequence: 7,
        };
        assert_eq!(
            entry_json(&entry),
            json!({
                "subject": { "type": "network", "id": "whatsapp" },
                "network": "whatsapp",
                "state": "granted",
                "decided_at": "2026-09-17T10:00:00.000Z",
                "decision_sequence": 7
            })
        );
    }

    #[test]
    fn an_undecided_contact_names_no_decision() {
        let undecided = Effective::resolve("@a:example.com", Network::Telegram, None, None);
        assert_eq!(
            effective_json("@a:example.com", Network::Telegram, &undecided),
            json!({
                "contact": "@a:example.com",
                "network": "telegram",
                "state": "pending",
                "decided_by": null
            })
        );
    }
}
