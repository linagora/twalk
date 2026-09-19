//! The consent snapshot: the full current state, and the bus position it
//! reflects, for a consumer whose cache is cold (ticket #50, ADR 0010).
//!
//! # The problem it solves
//!
//! A durable consumer resumes at its ack floor, so the decisions it has
//! already applied are never redelivered — and a consumer that keeps its
//! consent in memory, as the Sensor does, comes back from a restart knowing
//! nothing and is never told again (issue #16). Replaying the whole stream
//! instead would make a confidentiality guarantee expire with a retention
//! policy. So the Gateway, which is the single writer of consent state
//! (ADR 0006), answers the question directly: here is the whole state, and
//! here is the stream sequence it reflects. A cold consumer applies the
//! snapshot and creates its stream consumer at **that sequence plus one**.
//!
//! # What makes the hand-off exact
//!
//! The snapshot's position must be consistent with its content: no decision
//! may be committed-and-published between the read of the state and the read
//! of the position without appearing in one of the two. That is
//! [`crate::store::Store::snapshot`]'s property, and it is why the snapshot
//! is the state of the journal's *published prefix* rather than of its head —
//! a decision still waiting in the outbox has no position on the bus yet, and
//! including it would hand the consumer a decision it is about to receive
//! again. The response is therefore one of two things for every decision ever
//! taken: in the snapshot, or on the stream after `stream_sequence`. Never
//! both, never neither.
//!
//! # Who may read it
//!
//! Not a device. The Sensor is a service, not one of the owner's browsers:
//! it has no Matrix OpenID token to sign in with and no cookie to send, so
//! this is the one `/api` route that takes a **service token** from the
//! Gateway's own configuration ([`crate::config::Snapshot`]) —
//! `Requirement::ServiceToken` in #52's guard, which then asks for no device
//! cookie here and injects no device identity. The two credentials are
//! deliberately disjoint: a device token opens no snapshot, and the service
//! token opens nothing else. What it does grant is the whole social graph the
//! user ever decided about, which is why it is a generated secret and why
//! this module compares it without leaking its length
//! (`docs/architecture/security-model.md`).
//!
//! # Who is not in it at all
//!
//! The owner (ticket #149, ADR 0018, ADR 0021). Not as a subject, not as a
//! state, not as a row the query reads: the owner is never a contact and never
//! has a consent state, and this is the document every consumer builds its
//! whole cold cache from — a row about the owner here is the defect, because a
//! consumer without the Sensor's own filter (#147) applies it, and a `revoked`
//! on an owner ghost silences the user's own traffic with nothing to say why.
//! The exclusion is in [`crate::store::Store::snapshot`]'s SQL, beside the
//! `persona` one, and it covers a row recorded before this was enforced — a
//! deployment upgraded across #109 can hold one, since the user's own messages
//! used to be published as a contact's. Such a row is withheld and *counted*,
//! never deleted: the journal is append-only, so the honest treatment is to
//! serve nobody and tell the operator (a `warn` here, and
//! `twalk_companion_gateway_owner_consent_rows` on `/metrics`).
//!
//! And because the set of the owner's identities cannot be derived from a
//! bridge, the answer **carries** it: `owner_identities`, so that a consumer
//! applying the same rule reads it from the single writer of consent state.
//!
//! # What the answer deliberately does not carry
//!
//! No timestamp, no version, no entry count. The stream sequence is the only
//! ordering the design trusts: ADR 0010 rejected per-subject version counters
//! and clocks precisely so that there is nothing to arbitrate between. And no
//! pagination — a cursor would be a second ordering to get wrong, and a
//! half-applied snapshot is worse than none. The cap is a bound that fails
//! loudly instead.

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{error, info, warn};

use crate::consent::{self, CONSENT_CHANGED_TYPE, STREAM_NAME};
use crate::http::Gateway;
use crate::store::{Entry, Snapshot, SnapshotRefusal};

/// The largest snapshot a Gateway serves unless the operator says otherwise
/// (GATEWAY_CONSENT_SNAPSHOT_MAX_ENTRIES).
///
/// It is an entry per (subject, network), so a user with a thousand contacts
/// across six networks is nowhere near it; what it bounds is the pathological
/// case — a runaway writer, an import nobody meant — where the honest answer
/// is an error an operator sees rather than a multi-megabyte document a
/// consumer chokes on. A snapshot at this cap is about 20 MB of JSON.
pub const DEFAULT_MAX_ENTRIES: usize = 100_000;

/// What the snapshot endpoint needs beyond the consent store: the service
/// token that authenticates its caller, and the cap.
///
/// The token is kept as its SHA-256 digest, not as itself. Two reasons, in
/// order of how much they matter: the comparison is then over two
/// fixed-length digests, so it leaks neither the token's length nor how far a
/// guess got, and a memory image of the process holds one fewer copy of a
/// secret than it otherwise would.
pub struct Snapshots {
    service_token_sha256: [u8; 32],
    max_entries: usize,
}

impl Snapshots {
    pub fn new(service_token: &str, max_entries: usize) -> Self {
        Self {
            service_token_sha256: digest(service_token),
            max_entries,
        }
    }

    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Whether the request carries this Gateway's service token as
    /// `Authorization: Bearer <token>`.
    ///
    /// Constant-time in the token's content, and length-free: both sides are
    /// hashed to 32 bytes first, so a caller learns nothing from how long a
    /// refusal took. No other credential is accepted here — a device cookie
    /// is not read at all.
    pub fn authenticates(&self, headers: &HeaderMap) -> bool {
        let Some(presented) = bearer(headers) else {
            return false;
        };
        constant_time_eq(&digest(presented), &self.service_token_sha256)
    }
}

/// The snapshot route. Merged into the Gateway's router like every other
/// ticket's, and — unlike every other ticket's — declared in #52's guard as
/// taking a service token.
pub fn routes() -> Router<Gateway> {
    Router::new().route("/api/consent/snapshot", get(consent_snapshot))
}

/// `GET /api/consent/snapshot` — the whole consent state, and the stream
/// sequence it reflects.
///
/// ```json
/// {
///   "stream": "twalk",
///   "subject": "twalk.consent.state.changed.v1",
///   "stream_sequence": 41,
///   "next_stream_sequence": 42,
///   "decision_sequence": 7,
///   "entries": [
///     { "subject": { "type": "network", "id": "whatsapp" }, "network": "whatsapp",
///       "state": "granted", "decided_at": "…", "decision_sequence": 3 }
///   ]
/// }
/// ```
///
/// `entries` holds one entry per (subject, network): network entries are that
/// network's default and contact entries override them (the precedence is
/// `GET /api/consent/effective`'s). Revocations are as explicit as grants,
/// and an absent subject means "never decided" — never "revoked", which is
/// the distinction the whole shape exists to keep.
///
/// `stream` and `subject` name the bus the sequence belongs to, so a consumer
/// does not have to agree with the Gateway about them out of band;
/// `next_stream_sequence` is `stream_sequence + 1`, spelled out because the
/// off-by-one is the one mistake that would make a consumer skip a decision.
/// With nothing yet published both are `0` and `1`: start at the beginning of
/// the stream.
async fn consent_snapshot(State(gateway): State<Gateway>, headers: HeaderMap) -> Response {
    // Authentication first, so that an unauthenticated caller learns nothing
    // about this deployment's consent configuration.
    let Some(snapshots) = gateway.snapshots() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_token_not_configured",
            "this Gateway serves no consent snapshot: set GATEWAY_SERVICE_TOKEN to the same \
             value the Sensor is configured with",
        );
    };
    if !snapshots.authenticates(&headers) {
        // One answer for a missing token, a malformed header and a wrong
        // token, as the device guard does for its three: a probe learns
        // nothing from the difference.
        warn!("refused a consent snapshot: the service token is missing or wrong");
        return api_error(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "the consent snapshot takes this Gateway's service token as an \
             Authorization: Bearer credential; a device token is not accepted here",
        );
    }
    let Some(consent) = gateway.consent() else {
        return api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "consent_not_configured",
            "this Gateway has no consent store: set GATEWAY_NATS_URL (and GATEWAY_OWNER, \
             which consent takes its owner, state directory and domain from)",
        );
    };
    match consent.store().snapshot(snapshots.max_entries()) {
        Ok(snapshot) => {
            // The rows this snapshot withheld, counted from the store rather
            // than from what the read dropped — because it dropped nothing: the
            // exclusion is in the query (ticket #149). Reported here because
            // this is the read that used to hand a consumer the owner as a
            // subject, so it is where an operator should be told that their
            // journal still holds such a row.
            let withheld = match consent.store().owner_entries() {
                Ok(rows) => {
                    gateway.metrics().set_owner_consent_rows(rows.len() as u64);
                    rows.len()
                }
                Err(error) => {
                    warn!(%error, "failed to count the consent rows held about the owner");
                    0
                }
            };
            if withheld > 0 {
                warn!(
                    withheld,
                    identities = %consent.owner().identities().join(","),
                    "this consent journal holds rows about the owner and served none of them: \
                     the owner is never a contact and has no consent state (ADR 0018, ADR 0021), \
                     so a row about them is a row that should not exist — most likely left by a \
                     deployment upgraded across #109"
                );
            }
            info!(
                entries = snapshot.entries.len(),
                decision_sequence = snapshot.decision_sequence,
                stream_sequence = snapshot.stream_sequence,
                owner_identities = consent.owner().identities().len(),
                withheld,
                "served a consent snapshot"
            );
            Json(snapshot_json(&snapshot, consent.owner())).into_response()
        }
        Err(SnapshotRefusal::TooLarge { max_entries }) => {
            // Loudly, and with nothing in the body a consumer could mistake
            // for a state: a truncated snapshot would make granted contacts
            // look never decided.
            error!(
                max_entries,
                "refused a consent snapshot: the consent state is larger than the cap"
            );
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "snapshot_too_large",
                &format!(
                    "the consent state holds more than the {max_entries} entries this Gateway \
                     serves in one snapshot, and a snapshot is never truncated: raise \
                     GATEWAY_CONSENT_SNAPSHOT_MAX_ENTRIES"
                ),
            )
        }
        Err(SnapshotRefusal::Store(error)) => {
            error!(%error, "failed to read the consent snapshot");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_unavailable",
                "the consent state could not be read",
            )
        }
    }
}

/// The snapshot document, plus the one thing about consent state that is not a
/// decision: who has none at all.
///
/// `owner_identities` is ticket #149's, and it is here rather than on a route
/// of its own because this is already the one read a consumer makes of the
/// single writer of consent state, and because the two facts belong together:
/// these are exactly the subjects the entries above will never contain. A
/// consumer that must apply the same rule — the Sensor does, and says so at
/// `warn` (ADR 0021) — reads the list from the Gateway instead of keeping a
/// second copy of a list nobody can derive from a bridge. Two components
/// reading one list is tolerable; two maintaining their own is not.
///
/// It is always present and never empty: `GATEWAY_OWNER` is itself an identity,
/// so an absent member would mean "this Gateway is older than #149" and an
/// empty array would mean "this deployment has no owner", which cannot happen
/// on a Gateway that serves a snapshot at all.
fn snapshot_json(snapshot: &Snapshot, owner: &crate::owner::Owner) -> Value {
    json!({
        "stream": STREAM_NAME,
        "subject": consent::bus_subject(CONSENT_CHANGED_TYPE),
        "stream_sequence": snapshot.stream_sequence,
        "next_stream_sequence": snapshot.stream_sequence + 1,
        "decision_sequence": snapshot.decision_sequence,
        "owner_identities": owner.identities(),
        "entries": snapshot.entries.iter().map(entry_json).collect::<Vec<_>>(),
    })
}

/// One entry, in the same shape `GET /api/consent/state` renders — the same
/// `ConsentStateEntry` of `openapi.yaml`, so a consumer reads one shape
/// whichever of the two reads it came from.
fn entry_json(entry: &Entry) -> Value {
    json!({
        "subject": { "type": entry.subject.kind.as_str(), "id": entry.subject.id },
        "network": entry.network.as_str(),
        "state": entry.state.as_str(),
        "decided_at": entry.decided_at,
        "decision_sequence": entry.decision_sequence,
    })
}

/// The Gateway's one error document (`openapi.yaml`'s `Error`).
fn api_error(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({ "error": code, "detail": detail }))).into_response()
}

/// The credential out of an `Authorization: Bearer …` header. The scheme is
/// matched case-insensitively, as RFC 9110 requires.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, credential) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| credential.trim())
        .filter(|credential| !credential.is_empty())
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

/// Compares two digests without an early return, so the time a refusal takes
/// says nothing about how much of a guess was right.
fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::{Network, State, Subject, SubjectType};
    use axum::http::HeaderValue;

    const TOKEN: &str = "test-only-service-token-0123456789abcdef";

    fn authorization(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(value).expect("an ascii header"),
        );
        headers
    }

    #[test]
    fn only_this_gateways_service_token_as_a_bearer_credential_is_accepted() {
        let snapshots = Snapshots::new(TOKEN, DEFAULT_MAX_ENTRIES);
        assert!(snapshots.authenticates(&authorization(&format!("Bearer {TOKEN}"))));
        // The scheme is case-insensitive; the credential is not.
        assert!(snapshots.authenticates(&authorization(&format!("bearer {TOKEN}"))));
        assert!(!snapshots.authenticates(&authorization(&format!("Bearer {TOKEN}x"))));
        assert!(!snapshots.authenticates(&authorization(&format!("Bearer {}", &TOKEN[..8]))));
        assert!(!snapshots.authenticates(&authorization(&format!("Basic {TOKEN}"))));
        assert!(!snapshots.authenticates(&authorization("Bearer ")));
        assert!(!snapshots.authenticates(&authorization(TOKEN)));
        // No header at all, and — the property the ticket is explicit about —
        // a device cookie is not a credential here. The guard does not even
        // read one on this route; this is the handler saying so too.
        assert!(!snapshots.authenticates(&HeaderMap::new()));
        let mut cookie = HeaderMap::new();
        cookie.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("twalk_device={TOKEN}")).unwrap(),
        );
        assert!(!snapshots.authenticates(&cookie));
    }

    fn owner() -> crate::owner::Owner {
        crate::owner::Owner::new(
            "@michel:example.com",
            ["@whatsapp_lid-115332874281144:example.com".to_owned()],
        )
    }

    #[test]
    fn the_document_names_the_stream_position_a_consumer_starts_after() {
        let snapshot = Snapshot {
            entries: vec![Entry {
                subject: Subject {
                    kind: SubjectType::Network,
                    id: "whatsapp".to_owned(),
                },
                network: Network::Whatsapp,
                state: State::Granted,
                decided_at: "2026-09-17T10:00:00.000Z".to_owned(),
                decision_sequence: 3,
            }],
            decision_sequence: 7,
            stream_sequence: 41,
        };
        assert_eq!(
            snapshot_json(&snapshot, &owner()),
            json!({
                "stream": "twalk",
                "subject": "twalk.consent.state.changed.v1",
                "stream_sequence": 41,
                "next_stream_sequence": 42,
                "decision_sequence": 7,
                "owner_identities": [
                    "@michel:example.com",
                    "@whatsapp_lid-115332874281144:example.com"
                ],
                "entries": [{
                    "subject": { "type": "network", "id": "whatsapp" },
                    "network": "whatsapp",
                    "state": "granted",
                    "decided_at": "2026-09-17T10:00:00.000Z",
                    "decision_sequence": 3
                }]
            })
        );
    }

    #[test]
    fn an_empty_snapshot_starts_a_consumer_at_the_beginning_of_the_stream() {
        let empty = Snapshot {
            entries: Vec::new(),
            decision_sequence: 0,
            stream_sequence: 0,
        };
        let document = snapshot_json(&empty, &owner());
        assert_eq!(document["stream_sequence"], json!(0));
        assert_eq!(
            document["next_stream_sequence"],
            json!(1),
            "nothing decided yet: the consumer reads the whole stream, and \
             an empty state is not the same claim as a revoked one"
        );
        assert_eq!(document["entries"], json!([]));
        // Nothing decided is not nobody exempt: who has no consent state at
        // all is configuration, and it is served whether or not a decision was
        // ever taken (#149).
        assert_eq!(
            document["owner_identities"],
            json!([
                "@michel:example.com",
                "@whatsapp_lid-115332874281144:example.com"
            ])
        );
    }

    #[test]
    fn the_owners_own_matrix_id_is_served_even_with_no_ghost_configured() {
        // A deployment that has confirmed no ghost still says who its owner is,
        // so `owner_identities` is never absent and never empty on a Gateway
        // that serves a snapshot at all.
        let document = snapshot_json(
            &Snapshot {
                entries: Vec::new(),
                decision_sequence: 0,
                stream_sequence: 0,
            },
            &crate::owner::Owner::new("@michel:example.com", []),
        );
        assert_eq!(document["owner_identities"], json!(["@michel:example.com"]));
    }
}
