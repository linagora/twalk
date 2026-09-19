//! The portal register's two routes (ticket #105, ADR 0024):
//!
//! - `GET /api/portals` — every conversation this deployment's bridges have
//!   built, and where the Sensor stands in each. The read #143's chooser is
//!   drawn from, and the answer to the question a deployment could not
//!   previously ask itself: *how many of my conversations is the Sensor
//!   outside?*
//! - `POST /api/portals/observation` — the user's decision, one or many
//!   conversations at a time, in either direction.
//!
//! Both need a device token, like every `/api` route that is not explicitly
//! excepted (see [`crate::session_http::requirement`]).
//!
//! Error codes: `portals_not_configured` (503), `invalid_request` (400).
//! Everything that can go wrong with *one* room is an outcome in a `200`,
//! never a status code: a request naming twelve conversations where one is
//! refused has eleven decisions the user made and is entitled to keep.
//!
//! # What is served, and what the Gateway keeps
//!
//! A conversation's name, its member count and the network's own identifier
//! for it are served, because a chooser the user cannot read is not a
//! chooser: *Échecs en Yvelines, 246 members* is the sentence that makes the
//! decision a real one, and the identifier is what keeps the *other* `Échecs
//! en Yvelines` — the same community's announcement group, 11 members — from
//! being an indistinguishable second row (#143). None of it is stored.
//! Every field is read from the homeserver for this request and forgotten
//! with the response, which is the difference between this and #54's
//! pending-contact store, where a display name would have been *kept* and is
//! therefore refused entry.
//!
//! No message content is read at any point: the register reads room state,
//! never a timeline.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::http::Gateway;
use crate::portals::{Observation, PortalRefusal, Register};

pub fn routes() -> Router<Gateway> {
    Router::new()
        .route("/api/portals", get(portals))
        .route("/api/portals/observation", post(observation))
}

/// `GET /api/portals` — the register.
async fn portals(State(gateway): State<Gateway>) -> Response {
    let Some(portals) = gateway.portals() else {
        return not_configured();
    };
    let register = portals.read().await;
    (
        StatusCode::OK,
        Json(register_json(&register, portals.crowd_threshold())),
    )
        .into_response()
}

#[derive(Deserialize)]
struct ObservationRequest {
    #[serde(default)]
    rooms: Vec<String>,
    observed: bool,
}

/// `POST /api/portals/observation` — the decision.
async fn observation(
    State(gateway): State<Gateway>,
    body: Option<Json<ObservationRequest>>,
) -> Response {
    let Some(portals) = gateway.portals() else {
        return not_configured();
    };
    let Some(Json(request)) = body else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "the body is a JSON document with `rooms` (a list of room ids) and `observed` \
             (a boolean)",
        );
    };
    match portals
        .set_observation(&request.rooms, request.observed)
        .await
    {
        Ok(outcomes) => {
            let body = json!({
                "observed": request.observed,
                "outcomes": outcomes
                    .iter()
                    .map(|outcome| {
                        let mut entry = json!({
                            "room_id": outcome.room_id,
                            "status": outcome.status.label(),
                        });
                        if let crate::portals::OutcomeStatus::Failed { reason } = &outcome.status {
                            entry["reason"] = json!(reason);
                        }
                        entry
                    })
                    .collect::<Vec<_>>(),
            });
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(PortalRefusal::InvalidRequest { detail }) => {
            api_error(StatusCode::BAD_REQUEST, "invalid_request", &detail)
        }
    }
}

/// The register as the Companion reads it. The summary is not derived by the
/// client: "the Sensor is outside 17 of your 18 conversations" is a sentence
/// the deployment states, so the numbers are in the answer.
fn register_json(register: &Register, crowd_threshold: u64) -> serde_json::Value {
    let summary = register.summary();
    json!({
        // Served, not assumed: the chooser draws its crowds section from this
        // number and holds none of its own (#252).
        "crowd_threshold": crowd_threshold,
        "portals": register
            .portals
            .iter()
            .map(|portal| json!({
                "room_id": portal.room_id,
                "bridge_id": portal.bridge_id,
                "network": portal.network,
                "name": portal.name,
                // The bridge's own id for the conversation, verbatim. The
                // chooser reads its suffix to tell a person from a group
                // (#143); nothing on this side of the wire reads it at all.
                "network_conversation_id": portal.network_conversation_id,
                "members": portal.members,
                "observation": portal.observation.label(),
                "moved_from": portal.moved_from,
                "unreadable": portal.unreadable,
            }))
            .collect::<Vec<_>>(),
        "summary": {
            "total": register.portals.len(),
            "observing": summary.get(&Observation::Observing).copied().unwrap_or_default(),
            "invited": summary.get(&Observation::Invited).copied().unwrap_or_default(),
            "absent": summary.get(&Observation::Absent).copied().unwrap_or_default(),
            "moved": summary.get(&Observation::Moved).copied().unwrap_or_default(),
        },
        // Every configured bridge, readable or not, so a total is never
        // mistaken for a count of the user's conversations.
        "bridges": register
            .bridges
            .iter()
            .map(|bridge| json!({
                "bridge_id": bridge.bridge_id,
                "network": bridge.network,
                "readable": bridge.unreadable.is_none(),
                "detail": bridge.unreadable,
                // Which account did the asking, and how many rooms it is in
                // at all. #171: `absent: 0` with every bridge readable said
                // both "you have no conversations yet" and "the register
                // asked an account that is in no rooms", and a deployment
                // with 32 portal rooms could not tell which it had been told.
                "asked_as": bridge.asked_as,
                "joined_rooms": bridge.joined_rooms,
            }))
            .collect::<Vec<_>>(),
    })
}

/// This deployment holds no register: a `503` naming what would open it, not
/// an empty list. An empty list would say "your bridges have built no
/// conversations", which is a different and much worse claim than "this
/// Gateway cannot see them".
fn not_configured() -> Response {
    api_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "portals_not_configured",
        "this Gateway holds no portal register: set GATEWAY_SENSOR_USER_ID, GATEWAY_BRIDGES \
         (with each bridge's GATEWAY_BRIDGE_<ID>_AS_TOKEN) and a homeserver to read them from",
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
    use crate::portals::{BridgeReading, Portal};

    fn portal(room_id: &str, name: &str, members: u64, observation: Observation) -> Portal {
        Portal {
            room_id: room_id.to_owned(),
            bridge_id: "mautrix-whatsapp".to_owned(),
            network: "whatsapp".to_owned(),
            name: Some(name.to_owned()),
            network_conversation_id: None,
            members,
            observation,
            moved_from: None,
            unreadable: None,
            replaced_by: None,
        }
    }

    #[test]
    fn the_answer_states_the_number_the_sensor_is_outside() {
        let register = Register {
            portals: vec![
                portal("!a:x", "maria (WA)", 1, Observation::Observing),
                portal("!b:x", "Échecs en Yvelines", 246, Observation::Absent),
                portal("!c:x", "XVDSI", 309, Observation::Absent),
            ],
            bridges: vec![BridgeReading {
                bridge_id: "mautrix-whatsapp".to_owned(),
                network: "whatsapp".to_owned(),
                unreadable: None,
                asked_as: Some("@whatsappbot:x".to_owned()),
                joined_rooms: Some(3),
            }],
        };
        let body = register_json(&register, 20);
        assert_eq!(body["summary"]["total"], 3);
        assert_eq!(
            body["crowd_threshold"], 20,
            "the threshold is served, not assumed by the screen (#252)"
        );
        assert_eq!(body["summary"]["observing"], 1);
        assert_eq!(body["summary"]["absent"], 2);
        assert_eq!(body["summary"]["invited"], 0);
        assert_eq!(body["portals"][1]["members"], 246);
        assert_eq!(body["portals"][1]["observation"], "absent");
        assert_eq!(body["bridges"][0]["readable"], true);
    }

    #[test]
    fn an_unreadable_bridge_is_in_the_answer_with_its_reason() {
        let register = Register {
            portals: Vec::new(),
            bridges: vec![BridgeReading {
                bridge_id: "mautrix-signal".to_owned(),
                network: "signal".to_owned(),
                unreadable: Some("no appservice token configured".to_owned()),
                asked_as: None,
                joined_rooms: None,
            }],
        };
        let body = register_json(&register, 20);
        assert_eq!(body["bridges"][0]["readable"], false);
        assert_eq!(
            body["bridges"][0]["detail"],
            "no appservice token configured"
        );
        // And the totals do not pretend to cover it.
        assert_eq!(body["summary"]["total"], 0);
    }

    /// The network's own id crosses the wire exactly as the bridge wrote it,
    /// and a bridge that wrote none says `null` rather than an empty string.
    ///
    /// The two rooms here are the pair #105's own measurements found at 06:46
    /// — a WhatsApp community and its announcement group, identical names,
    /// 109 members and 6 — and the ids are the only thing in the answer that
    /// tells them apart.
    #[test]
    fn the_networks_own_conversation_id_is_passed_through_untouched() {
        let mut community = portal("!a:x", "Communauté CKCP", 109, Observation::Absent);
        community.network_conversation_id = Some("120363201980306353@g.us".to_owned());
        let mut announcements = portal("!b:x", "Communauté CKCP", 6, Observation::Absent);
        announcements.network_conversation_id = Some("120363333311112222@g.us".to_owned());
        let nameless = portal("!c:x", "maria (WA)", 1, Observation::Observing);

        let register = Register {
            portals: vec![community, announcements, nameless],
            bridges: Vec::new(),
        };
        let body = register_json(&register, 20);
        assert_eq!(
            body["portals"][0]["network_conversation_id"],
            "120363201980306353@g.us"
        );
        assert_eq!(
            body["portals"][1]["network_conversation_id"],
            "120363333311112222@g.us"
        );
        assert!(body["portals"][2]["network_conversation_id"].is_null());
    }
}
