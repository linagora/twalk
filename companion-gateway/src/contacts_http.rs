//! The pending contacts' HTTP surface: the list waiting for a decision, and
//! the display names a screen asks for (ticket #54).
//!
//! # Who is calling
//!
//! Nothing here authenticates: #52's guard already does, for everything under
//! `/api/` that its table does not except ([`crate::session_http`]). Both of
//! these routes take a device token, because both of them are the owner's own
//! read of their own correspondents — and neither was added to the guard's
//! exception table, which is the direction that table exists for.
//!
//! # What the answers deliberately do not carry
//!
//! A pending contact is four values: a Matrix ID, a network and two instants.
//! No body, no display name, no `network_identifier` — not because the
//! serialiser leaves them out, but because [`crate::store`] never held them
//! and [`crate::contacts`] never parsed them. What the API can hand out is
//! bounded by what the store can hold, which is the point of building it that
//! way round.
//!
//! The display names are the one exception, and they are an exception in
//! shape as well as in content: a separate call, answered from the bus,
//! written nowhere. A contact the bus no longer remembers a name for comes
//! back as `null`, and the Companion shows the Matrix ID.
//!
//! # What the answers look like
//!
//! One shape for every refusal, the Gateway's own `Error` document
//! (`openapi.yaml`). The codes are `malformed_request`, `unknown_value`,
//! `contacts_not_configured`, `bus_unreachable` and `store_unavailable`;
//! `unauthenticated` comes from the guard.

use std::collections::HashMap;

use axum::extract::{Query, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use tracing::{debug, error, info};

use crate::consent::Network;
use crate::contacts::{pending_json, MAX_DISPLAY_NAME_LOOKUPS};
use crate::http::Gateway;
use crate::store::SeenContact;

/// The pending-contact routes. Merged into the Gateway's router, so
/// registering them is additive to whatever else the API grows.
pub fn routes() -> Router<Gateway> {
    Router::new()
        .route("/api/contacts/pending", get(pending_contacts))
        .route("/api/contacts/display-names", get(display_names))
}

/// `GET /api/contacts/pending` — the contacts waiting for a decision, and how
/// many of them there are per network.
///
/// ```json
/// {
///   "total": 3,
///   "networks": [{ "network": "whatsapp", "count": 2 },
///                { "network": "signal", "count": 1 }],
///   "contacts": [{ "contact": "@whatsapp_33612345678:example.com",
///                  "network": "whatsapp",
///                  "first_seen": "2026-09-17T10:00:00.000Z",
///                  "last_seen":  "2026-09-17T18:30:00.000Z" }]
/// }
/// ```
///
/// `total` and `networks` are the dashboard's numbers — screen 5's "3 consent
/// decisions waiting" — and they always count the whole list, whatever
/// `?network=` narrows `contacts` to. A client that filters must not have to
/// choose between the list it shows and the badge it shows beside it.
///
/// "Waiting" is the same question `GET /api/consent/effective` answers with
/// `decided_by: null`: neither this contact's own decision nor its network's
/// default exists. So granting a whole network empties this list of every
/// contact on it at once, and a contact the user deliberately left `pending`
/// is not in it — the user answered, and the answer was "not yet".
///
/// Not paginated and not capped, like `GET /api/consent/state`: it is one
/// short row per contact the user has not answered about, and a list that
/// grows without bound is a deployment whose owner has stopped deciding, not
/// a shape problem.
async fn pending_contacts(
    State(gateway): State<Gateway>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(contacts) = gateway.contacts() else {
        return not_configured();
    };
    // The filter (#272): a connection, or a network — the latter narrows to
    // every connection of that kind, which is what a screen that has not
    // yet learned connections asked for before.
    let filter = match (
        query.get("connection").filter(|value| !value.is_empty()),
        query.get("network").filter(|value| !value.is_empty()),
    ) {
        (Some(connection), _) => match gateway.connections().get(connection) {
            Some(_) => Some(Filter::Connection(connection.clone())),
            None => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "unknown_value",
                    &format!("connection has the unknown value {connection:?}"),
                )
            }
        },
        (None, Some(network)) => match Network::parse(network) {
            Some(network) => Some(Filter::Network(network)),
            None => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    "unknown_value",
                    &format!("network has the unknown value {network:?}"),
                )
            }
        },
        (None, None) => None,
    };
    match contacts.pending() {
        Ok(pending) => Json(pending_document(&pending, filter.as_ref())).into_response(),
        Err(error) => {
            error!(%error, "failed to read the pending contacts");
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_unavailable",
                "the pending contacts could not be read",
            )
        }
    }
}

/// `GET /api/contacts/display-names?contact=<id>&contact=<id>` — what these
/// contacts are called, read from the bus.
///
/// ```json
/// { "contacts": [{ "contact": "@whatsapp_33612345678:example.com",
///                  "display_name": "Aïcha Benali" },
///                { "contact": "@signal_x:example.com",
///                  "display_name": null }] }
/// ```
///
/// A separate call from the list on purpose. The Gateway stores no display
/// name — a store of who writes to the user *and what they are called* is a
/// directory, and this ticket exists to not build one — so a name is looked
/// up when a screen needs it and kept nowhere afterwards. The Gateway reads
/// the tail of the inbound stream, takes the most recent name it finds for
/// each contact asked about, and drops everything else it walked past.
///
/// `null` is a real answer, not a failure: the contact's last message has
/// fallen out of the window the Gateway reads, and the Companion shows the
/// Matrix ID — which is what the decision will name anyway.
///
/// The `contact` parameter repeats, once per contact, and at most
/// [`MAX_DISPLAY_NAME_LOOKUPS`] times. Over that the request is refused
/// rather than truncated, so a caller never mistakes a short answer for a
/// complete one.
async fn display_names(State(gateway): State<Gateway>, RawQuery(query): RawQuery) -> Response {
    let Some(contacts) = gateway.contacts() else {
        return not_configured();
    };
    let asked = contact_parameters(query.as_deref().unwrap_or_default());
    if asked.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "malformed_request",
            "at least one contact query parameter is required: a contact's Matrix user ID, \
             repeated once per contact",
        );
    }
    if asked.len() > MAX_DISPLAY_NAME_LOOKUPS {
        return api_error(
            StatusCode::BAD_REQUEST,
            "malformed_request",
            &format!(
                "a display-name read asks about at most {MAX_DISPLAY_NAME_LOOKUPS} contacts, \
                 and is refused rather than truncated: ask in several calls"
            ),
        );
    }
    match contacts.display_names(&asked).await {
        Ok(found) => {
            info!(
                asked = asked.len(),
                resolved = found.iter().filter(|(_, name)| name.is_some()).count(),
                "read display names from the bus; none was stored"
            );
            Json(json!({
                "contacts": found
                    .iter()
                    .map(|(contact, display_name)| json!({
                        "contact": contact,
                        "display_name": display_name,
                    }))
                    .collect::<Vec<_>>(),
            }))
            .into_response()
        }
        Err(error) => {
            // The bus, not the store: the list itself is still served, so
            // the Companion can render the contacts without their names.
            debug!(%error, "failed to read display names from the bus");
            api_error(
                StatusCode::BAD_GATEWAY,
                "bus_unreachable",
                "the display names could not be read from the bus; the pending list itself \
                 is unaffected and the Companion can show the Matrix IDs",
            )
        }
    }
}

/// The `contact` query parameters, in the order they were given, deduplicated
/// — asking twice about one contact is one lookup.
///
/// Parsed by hand because axum's `Query` extractor maps a query string to a
/// map, and a map keeps one value per key: a repeated parameter is exactly
/// what this endpoint takes.
fn contact_parameters(query: &str) -> Vec<String> {
    let mut asked: Vec<String> = Vec::new();
    for (key, value) in form_urlencoded_pairs(query) {
        if key != "contact" || value.is_empty() {
            continue;
        }
        if !asked.iter().any(|existing| existing == &value) {
            asked.push(value);
        }
    }
    asked
}

/// The `application/x-www-form-urlencoded` pairs of a query string, decoded.
/// Small enough to not be worth a dependency: `+` is a space, `%XX` is a
/// byte, and everything else is itself.
fn form_urlencoded_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    fn nibble(byte: u8) -> Option<u8> {
        char::from(byte)
            .to_digit(16)
            .and_then(|digit| u8::try_from(digit).ok())
    }
    // Byte by byte, never by slicing the `&str`: a `%` followed by the
    // middle of a multi-byte character would panic on a char boundary.
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                match (nibble(bytes[index + 1]), nibble(bytes[index + 2])) {
                    (Some(high), Some(low)) => {
                        out.push((high << 4) | low);
                        index += 3;
                    }
                    _ => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What `?connection=` or `?network=` narrows the list to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Filter {
    Connection(String),
    Network(Network),
}

impl Filter {
    fn admits(&self, seen: &SeenContact) -> bool {
        match self {
            Filter::Connection(id) => &seen.connection == id,
            Filter::Network(network) => *network == seen.network,
        }
    }
}

/// The pending list with its counts. The counts are of the whole list and
/// `contacts` is what the filter left, so a badge and the list beside it can
/// disagree without either being wrong. Two breakdowns: per connection —
/// the one a screen that decides per connection reads (#272) — and per
/// network, kept for a screen that has not learned connections yet.
fn pending_document(pending: &[SeenContact], filter: Option<&Filter>) -> Value {
    let mut by_connection: Vec<(&str, Network, u64)> = Vec::new();
    let mut by_network: Vec<(Network, u64)> = Vec::new();
    for seen in pending {
        match by_connection
            .iter_mut()
            .find(|(connection, _, _)| *connection == seen.connection)
        {
            Some((_, _, count)) => *count += 1,
            None => by_connection.push((seen.connection.as_str(), seen.network, 1)),
        }
        match by_network
            .iter_mut()
            .find(|(network, _)| *network == seen.network)
        {
            Some((_, count)) => *count += 1,
            None => by_network.push((seen.network, 1)),
        }
    }
    // A stable order whatever order the rows arrived in: the contract's own
    // ordering of network values, then the connection's id.
    by_connection.sort_by_key(|(connection, network, _)| (network.as_str(), *connection));
    by_network.sort_by_key(|(network, _)| network.as_str());
    json!({
        "total": pending.len(),
        "connections": by_connection
            .iter()
            .map(|(connection, network, count)| {
                json!({ "connection": connection, "network": network.as_str(), "count": count })
            })
            .collect::<Vec<_>>(),
        "networks": by_network
            .iter()
            .map(|(network, count)| json!({ "network": network.as_str(), "count": count }))
            .collect::<Vec<_>>(),
        "contacts": pending
            .iter()
            .filter(|seen| filter.is_none_or(|filter| filter.admits(seen)))
            .map(pending_json)
            .collect::<Vec<_>>(),
    })
}

/// This deployment projects no inbound stream: a `503` that names the
/// variables, not an empty list. An empty list would say "nobody has written
/// to you", which is a different and much worse claim than "this Gateway is
/// not watching".
fn not_configured() -> Response {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "contacts_not_configured",
        "this Gateway projects no pending contacts: set GATEWAY_NATS_URL (and GATEWAY_OWNER, \
         which the projection takes its owner and state directory from)",
    )
}

/// One error answer shape for the whole surface: the `Error` schema of
/// `openapi.yaml`.
fn api_error(status: StatusCode, code: &str, detail: &str) -> Response {
    (status, Json(json!({ "error": code, "detail": detail }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(contact: &str, network: Network, at: &str) -> SeenContact {
        seen_on(contact, network.as_str(), network, at)
    }

    fn seen_on(contact: &str, connection: &str, network: Network, at: &str) -> SeenContact {
        SeenContact {
            contact: contact.to_owned(),
            connection: connection.to_owned(),
            network,
            first_seen: at.to_owned(),
            last_seen: at.to_owned(),
        }
    }

    #[test]
    fn the_counts_are_of_the_whole_list_whatever_the_filter_shows() {
        let pending = vec![
            seen(
                "@a:example.com",
                Network::Whatsapp,
                "2026-09-17T10:00:00.000Z",
            ),
            seen(
                "@b:example.com",
                Network::Whatsapp,
                "2026-09-17T10:01:00.000Z",
            ),
            seen(
                "@c:example.com",
                Network::Signal,
                "2026-09-17T10:02:00.000Z",
            ),
        ];
        let whole = pending_document(&pending, None);
        assert_eq!(whole["total"], json!(3));
        assert_eq!(
            whole["networks"],
            json!([
                { "network": "signal", "count": 1 },
                { "network": "whatsapp", "count": 2 }
            ]),
            "the counts are in the contract's own order of network values"
        );
        assert_eq!(
            whole["connections"],
            json!([
                { "connection": "signal", "network": "signal", "count": 1 },
                { "connection": "whatsapp", "network": "whatsapp", "count": 2 }
            ]),
            "and per connection, the same numbers on the reference shape"
        );
        assert_eq!(whole["contacts"].as_array().map(Vec::len), Some(3));

        let filtered = pending_document(&pending, Some(&Filter::Network(Network::Signal)));
        assert_eq!(
            filtered["total"],
            json!(3),
            "the dashboard's number counts everything waiting, not what this call showed"
        );
        assert_eq!(filtered["networks"], whole["networks"]);
        assert_eq!(filtered["contacts"].as_array().map(Vec::len), Some(1));
        assert_eq!(filtered["contacts"][0]["contact"], json!("@c:example.com"));
    }

    #[test]
    fn two_connections_of_one_network_are_counted_and_filtered_apart() {
        // The shape the perimeter exists for (#272): a contact waiting on
        // the work account is not waiting on the home one, and the
        // dashboard's per-connection numbers say so while the per-network
        // number still adds them up.
        let pending = vec![
            seen_on(
                "@a:example.com",
                "wa-home",
                Network::Whatsapp,
                "2026-09-17T10:00:00.000Z",
            ),
            seen_on(
                "@a:example.com",
                "wa-work",
                Network::Whatsapp,
                "2026-09-17T10:01:00.000Z",
            ),
            seen_on(
                "@b:example.com",
                "wa-work",
                Network::Whatsapp,
                "2026-09-17T10:02:00.000Z",
            ),
        ];
        let whole = pending_document(&pending, None);
        assert_eq!(
            whole["connections"],
            json!([
                { "connection": "wa-home", "network": "whatsapp", "count": 1 },
                { "connection": "wa-work", "network": "whatsapp", "count": 2 }
            ])
        );
        assert_eq!(
            whole["networks"],
            json!([{ "network": "whatsapp", "count": 3 }])
        );
        let work = pending_document(&pending, Some(&Filter::Connection("wa-work".to_owned())));
        assert_eq!(work["contacts"].as_array().map(Vec::len), Some(2));
        assert_eq!(work["total"], json!(3), "the total is the whole list");
        let whatsapp = pending_document(&pending, Some(&Filter::Network(Network::Whatsapp)));
        assert_eq!(whatsapp["contacts"].as_array().map(Vec::len), Some(3));
    }

    #[test]
    fn an_empty_list_is_a_document_and_not_an_absence() {
        assert_eq!(
            pending_document(&[], None),
            json!({ "total": 0, "connections": [], "networks": [], "contacts": [] })
        );
    }

    #[test]
    fn a_pending_contact_carries_nothing_but_its_four_values() {
        let document = pending_document(
            &[seen(
                "@whatsapp_33612345678:example.com",
                Network::Whatsapp,
                "2026-09-17T10:00:00.000Z",
            )],
            None,
        );
        let entry = &document["contacts"][0];
        let members: Vec<&String> = entry.as_object().expect("an object").keys().collect();
        assert_eq!(
            members,
            vec![
                "connection",
                "contact",
                "first_seen",
                "last_seen",
                "network"
            ],
            "a body, a display name or a network identifier must never be one of these"
        );
    }

    #[test]
    fn the_contact_parameter_repeats_and_is_percent_decoded() {
        assert_eq!(
            contact_parameters("contact=%40whatsapp_33612345678%3Aexample.com&contact=%40b%3Ax"),
            vec![
                "@whatsapp_33612345678:example.com".to_owned(),
                "@b:x".to_owned()
            ]
        );
        // Asking twice about one contact is one lookup.
        assert_eq!(
            contact_parameters("contact=%40a%3Ax&contact=%40a%3Ax"),
            vec!["@a:x".to_owned()]
        );
        // Other parameters, empty values and an empty query are all "nothing
        // asked about".
        assert_eq!(contact_parameters("network=whatsapp"), Vec::<String>::new());
        assert_eq!(contact_parameters("contact="), Vec::<String>::new());
        assert_eq!(contact_parameters(""), Vec::<String>::new());
    }
}
