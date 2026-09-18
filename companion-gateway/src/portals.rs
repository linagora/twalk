//! The portal register: which portal rooms a bridge has built, and which of
//! them the Sensor is inside (ticket #105, ADR 0024).
//!
//! # The defect this exists for
//!
//! A bridge builds a portal room **lazily**, when a conversation becomes
//! active — not once, at login. Measured on the reference deployment, one
//! WhatsApp account produced eighteen portal rooms between 04:54 and 13:24 as
//! people wrote, and the Sensor was in exactly one of them: the one somebody
//! had invited it into by hand. mautrix invites *the user* into a portal it
//! creates and nothing else; there is no bridge setting that would invite a
//! third account, and `SENSOR_ALLOWED_INVITERS` only settles what the Sensor
//! *accepts* once somebody has asked.
//!
//! So the deployment went deaf on seventeen conversations while every
//! component reported itself healthy, and the deafness **grows on its own**:
//! a fix that invited the Sensor into the rooms that exist when a network is
//! connected would have been correct for an hour and wrong by the evening.
//! Whatever closes this has to react to portal creation, continuously.
//!
//! # What this module is, and what it deliberately is not
//!
//! It is the **mechanism**. The policy — which conversations Twalk may watch
//! — is the user's, and it is expressed one conversation at a time (#143).
//! Those eighteen rooms held roughly 1,300 memberships, a two-person
//! conversation with a family member and a 246-member association under the
//! same mechanism; inviting the Sensor into all of them by default would
//! publish those people's messages, which is the decision #122 is about. So
//! **the default is to observe nothing**: this module discovers portal rooms
//! and offers them, and the Sensor enters one only when the user says so.
//!
//! It is also **not a store**. Every read asks the homeserver, and the answer
//! to "is the Sensor observing this conversation?" is the Sensor's own
//! membership event, not a row the Gateway wrote. Nothing here can drift from
//! the system it describes, a reconnection that rebuilds every portal is
//! simply the next read, and there is no second record for a replay to
//! disagree with.
//!
//! # Whose credential does the asking
//!
//! A portal room's members are the user, the bridge bot and the network
//! ghosts. The Gateway holds no Matrix access token of the user's (ADR 0011)
//! and will not start holding one, so the account it acts as is **the bridge
//! bot**, through the appservice token the operator already configured for
//! that bridge (`GATEWAY_BRIDGE_<ID>_AS_TOKEN`, ticket #56). Until now the
//! Gateway held that token only to *verify* the bridge's status pushes; this
//! module is the first thing that acts with it, which is the change of
//! posture a reviewer should weigh (`docs/architecture/security-model.md`).
//!
//! The bot is the right asker for three reasons: it is in every portal of its
//! own bridge and in nobody else's rooms, it created them and therefore can
//! invite, and the credential exists already — nothing new is stored and no
//! new secret is minted. The token is a Matrix credential and nothing more:
//! the code below speaks the ordinary client-server API with it, which is why
//! a test can hand it an ordinary account's access token instead.
//!
//! A bridge the operator gave no token for is **not** silently skipped. It is
//! reported as unreadable, with the variable that would open it, because "the
//! Sensor is outside 17 of your 18 conversations" must never quietly mean
//! "…of the 18 I could see".
//!
//! # The three states, and why `invited` is one of them
//!
//! A portal is `observing`, `invited` or `absent`. `invited` is not an
//! implementation detail on the way to `observing`: it is what a deployment
//! looks like when `SENSOR_ALLOWED_INVITERS` does not name the bridge bot.
//! The invitation lands, the Sensor ignores it, and without this state the
//! symptom would again be silence that nothing accounts for. A portal stuck
//! at `invited` names its own cause.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::StreamExt;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::metrics::Metrics;

/// How long the Gateway waits for the homeserver on one portal call. Shorter
/// than bootstrap's registration timeout: a register read is many calls and a
/// slow one must not hold the whole list.
const HOMESERVER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How many rooms of one bridge are read at a time. The homeserver is on the
/// deployment's own network and a personal account has tens of portals, so
/// this is about not opening ninety sockets at once rather than about speed.
const ROOM_READ_CONCURRENCY: usize = 6;

/// The most rooms one observation request may name. Higher than bootstrap's
/// cap, because that one is a list a user ticks by hand on screen 3d and this
/// one is what a "watch this whole network" control (#143) sends in one
/// gesture — and still bounded, so one request cannot become an unbounded
/// number of invitations.
pub const MAX_ROOMS_PER_REQUEST: usize = 256;

/// How often the background refresher re-reads the register to keep the
/// gauges honest, when the operator sets no interval. The API's own read is
/// always live; this only decides how stale `/metrics` may be, so it is
/// deliberately unhurried — the register costs one call per portal room.
pub const DEFAULT_REFRESH_SECONDS: u64 = 300;

/// Where the Sensor stands in one portal room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Observation {
    /// The Sensor has joined: this conversation reaches the bus.
    Observing,
    /// The Sensor was invited and has not joined. Normally a moment; when it
    /// lasts, the Sensor is refusing the inviter — see the module docs.
    Invited,
    /// The Sensor is not in the room and has not been asked to be. The
    /// default for every portal a bridge builds.
    Absent,
}

impl Observation {
    /// The stable label, in the API, in the logs and in the metric.
    pub fn label(&self) -> &'static str {
        match self {
            Observation::Observing => "observing",
            Observation::Invited => "invited",
            Observation::Absent => "absent",
        }
    }
}

/// One portal room, as the homeserver answers about it right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Portal {
    pub room_id: String,
    /// The bridge instance whose bot is in this room (`GATEWAY_BRIDGES`).
    pub bridge_id: String,
    /// The network the user experiences, from the room's own `m.bridge`
    /// marker when it names one this version knows, and from the bridge's
    /// configuration otherwise.
    pub network: String,
    /// The room's name — for a one-to-one portal, the contact's; for a group,
    /// the group's. Read through and never written down.
    pub name: Option<String>,
    /// How many people are in the conversation: joined members, **excluding**
    /// the bridge bot and the Sensor. Excluding them is what keeps the number
    /// the user reads from changing when they decide to observe the room.
    pub members: u64,
    pub observation: Observation,
}

/// Whether one configured bridge could be read, and why not when it could
/// not. Always present for every bridge in `GATEWAY_BRIDGES`, so a count of
/// portals is never mistaken for a count of conversations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeReading {
    pub bridge_id: String,
    pub network: String,
    /// `None` when the bridge was read. `Some(detail)` names what stopped it,
    /// in the operator's words.
    pub unreadable: Option<String>,
}

/// The whole register at one instant: every portal room every readable bridge
/// holds, and what happened to the ones that could not be read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Register {
    pub portals: Vec<Portal>,
    pub bridges: Vec<BridgeReading>,
}

impl Register {
    /// How many portals stand in each state. The fact the deployment has to
    /// be able to state: *the Sensor is outside 17 of your 18 conversations*.
    pub fn summary(&self) -> BTreeMap<Observation, u64> {
        let mut summary = BTreeMap::from([
            (Observation::Observing, 0),
            (Observation::Invited, 0),
            (Observation::Absent, 0),
        ]);
        for portal in &self.portals {
            *summary.entry(portal.observation).or_insert(0) += 1;
        }
        summary
    }

    pub fn unreadable_bridges(&self) -> u64 {
        self.bridges
            .iter()
            .filter(|bridge| bridge.unreadable.is_some())
            .count() as u64
    }
}

/// What happened to one room of an observation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationOutcome {
    pub room_id: String,
    pub status: OutcomeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutcomeStatus {
    /// The Sensor was invited. It joins when it next syncs, provided
    /// `SENSOR_ALLOWED_INVITERS` names this bridge's bot.
    Invited,
    /// The Sensor was already in the room, or already invited to it.
    AlreadyObserved,
    /// The Sensor was removed from the room, so this conversation stops
    /// reaching the bus.
    Removed,
    /// The Sensor was not in the room to begin with.
    NotObserved,
    /// This room is not a portal of any bridge this Gateway can read. No call
    /// was made: the appservice credential acts only where its own bot
    /// already is, and a room id from a request is never a reason to use it
    /// somewhere else.
    UnknownPortal,
    /// The homeserver refused this one room. `reason` is its error code, or
    /// the Gateway's own.
    Failed { reason: String },
}

impl OutcomeStatus {
    pub fn label(&self) -> &'static str {
        match self {
            OutcomeStatus::Invited => "invited",
            OutcomeStatus::AlreadyObserved => "already_observed",
            OutcomeStatus::Removed => "removed",
            OutcomeStatus::NotObserved => "not_observed",
            OutcomeStatus::UnknownPortal => "unknown_portal",
            OutcomeStatus::Failed { .. } => "failed",
        }
    }
}

/// Why a whole request was refused, as opposed to one room of it failing.
#[derive(Debug)]
pub enum PortalRefusal {
    /// The request named more rooms than [`MAX_ROOMS_PER_REQUEST`], or named
    /// none.
    InvalidRequest { detail: String },
}

/// The register's configuration: where the homeserver is, who the Sensor is,
/// and one credential per bridge.
pub struct Portals {
    /// Base URL of the homeserver's **client** API, without a trailing slash.
    homeserver_url: String,
    /// The Matrix ID whose membership decides [`Observation`]
    /// (`GATEWAY_SENSOR_USER_ID`).
    sensor_user_id: String,
    /// Every bridge in `GATEWAY_BRIDGES`, in that order, with the appservice
    /// token that reads it or the reason there is none.
    bridges: Vec<PortalBridge>,
    metrics: Arc<Metrics>,
    http: reqwest::Client,
}

/// One bridge, as the register sees it.
pub struct PortalBridge {
    pub bridge_id: String,
    pub network: String,
    /// The bridge's appservice token. `None` when the operator configured
    /// none, in which case that bridge's portals cannot be read at all.
    pub as_token: Option<String>,
}

impl Portals {
    /// `None` when this deployment cannot hold a register: no Sensor to
    /// invite, no homeserver to ask, or no bridge configured at all. The
    /// endpoints then answer `503` naming the variable, rather than an empty
    /// list — "this bridge has built no conversations yet" and "this Gateway
    /// cannot see your conversations" are very different claims.
    pub fn new(
        homeserver_url: Option<&str>,
        sensor_user_id: Option<&str>,
        bridges: Vec<PortalBridge>,
        metrics: Arc<Metrics>,
    ) -> anyhow::Result<Option<Self>> {
        let (Some(homeserver_url), Some(sensor_user_id)) = (homeserver_url, sensor_user_id) else {
            return Ok(None);
        };
        if bridges.is_empty() {
            return Ok(None);
        }
        let http = reqwest::Client::builder()
            .timeout(HOMESERVER_TIMEOUT)
            .build()
            .map_err(|error| anyhow::anyhow!("{}", error.without_url()))?;
        Ok(Some(Self {
            homeserver_url: homeserver_url.trim_end_matches('/').to_owned(),
            sensor_user_id: sensor_user_id.to_owned(),
            bridges,
            metrics,
            http,
        }))
    }

    pub fn sensor_user_id(&self) -> &str {
        &self.sensor_user_id
    }

    /// Reads the whole register from the homeserver.
    ///
    /// One bridge failing never fails the read: it becomes a
    /// [`BridgeReading`] with the reason, and the portals of the bridges that
    /// answered are still returned. That is the point — a partial answer that
    /// says it is partial beats a refusal, and beats a total that silently
    /// counts fewer conversations than the user has.
    ///
    /// Updates the gauges as a side effect, so `/metrics` is as fresh as the
    /// last read whoever made it.
    pub async fn read(&self) -> Register {
        let mut register = Register::default();
        for bridge in &self.bridges {
            let Some(as_token) = &bridge.as_token else {
                register.bridges.push(BridgeReading {
                    bridge_id: bridge.bridge_id.clone(),
                    network: bridge.network.clone(),
                    unreadable: Some(format!(
                        "no appservice token configured: set GATEWAY_BRIDGE_{}_AS_TOKEN to the \
                         same value as this bridge's own appservice.as_token",
                        crate::config::variable_slug(&bridge.bridge_id)
                    )),
                });
                continue;
            };
            match self.read_bridge(bridge, as_token).await {
                Ok(mut portals) => {
                    register.portals.append(&mut portals);
                    register.bridges.push(BridgeReading {
                        bridge_id: bridge.bridge_id.clone(),
                        network: bridge.network.clone(),
                        unreadable: None,
                    });
                }
                Err(detail) => {
                    warn!(
                        bridge = %bridge.bridge_id,
                        %detail,
                        "could not read a bridge's portal rooms, so its conversations are not counted"
                    );
                    self.metrics.record_portal_refresh_failure();
                    register.bridges.push(BridgeReading {
                        bridge_id: bridge.bridge_id.clone(),
                        network: bridge.network.clone(),
                        unreadable: Some(detail),
                    });
                }
            }
        }
        // A stable order, so two reads of an unchanged deployment are the
        // same list: by bridge as configured, then by name, then by room id.
        let order: BTreeMap<&str, usize> = self
            .bridges
            .iter()
            .enumerate()
            .map(|(index, bridge)| (bridge.bridge_id.as_str(), index))
            .collect();
        register.portals.sort_by(|left, right| {
            order
                .get(left.bridge_id.as_str())
                .cmp(&order.get(right.bridge_id.as_str()))
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.room_id.cmp(&right.room_id))
        });
        self.metrics.set_portal_rooms(&register);
        register
    }

    /// One bridge's portal rooms, read as its own bot.
    async fn read_bridge(
        &self,
        bridge: &PortalBridge,
        as_token: &str,
    ) -> Result<Vec<Portal>, String> {
        let rooms = self.joined_rooms(as_token).await?;
        let portals: Vec<Option<Portal>> = futures::stream::iter(rooms.into_iter())
            .map(|room_id| async move {
                match self.portal_of(bridge, as_token, &room_id).await {
                    Ok(portal) => portal,
                    Err(detail) => {
                        // One unreadable room is not a reason to lose the
                        // other seventeen; it is a reason to say so.
                        warn!(
                            bridge = %bridge.bridge_id,
                            room = %room_id,
                            %detail,
                            "could not read a room the bridge bot is in, skipping it"
                        );
                        None
                    }
                }
            })
            .buffer_unordered(ROOM_READ_CONCURRENCY)
            .collect()
            .await;
        Ok(portals.into_iter().flatten().collect())
    }

    /// The rooms this credential's own user is joined to.
    async fn joined_rooms(&self, as_token: &str) -> Result<Vec<String>, String> {
        let url = self.url(&["_matrix", "client", "v3", "joined_rooms"])?;
        let response = self
            .http
            .get(url)
            .bearer_auth(as_token)
            .send()
            .await
            .map_err(|error| error.without_url().to_string())?;
        if !response.status().is_success() {
            return Err(matrix_error(response).await);
        }
        #[derive(Deserialize)]
        struct JoinedRooms {
            #[serde(default)]
            joined_rooms: Vec<String>,
        }
        let joined: JoinedRooms = response
            .json()
            .await
            .map_err(|error| error.without_url().to_string())?;
        Ok(joined.joined_rooms)
    }

    /// One room: a portal of this bridge, or not one at all.
    ///
    /// The marker is the room's `m.bridge` state event — the same one the
    /// Sensor attributes a network with (`sensor/src/network.rs`), so a room
    /// this register offers is a room the Sensor could publish. A room the
    /// bot is in that carries no marker is the bridge's management room or
    /// its space, and is not a conversation.
    async fn portal_of(
        &self,
        bridge: &PortalBridge,
        as_token: &str,
        room_id: &str,
    ) -> Result<Option<Portal>, String> {
        let url = self.url(&["_matrix", "client", "v3", "rooms", room_id, "state"])?;
        let response = self
            .http
            .get(url)
            .bearer_auth(as_token)
            .send()
            .await
            .map_err(|error| error.without_url().to_string())?;
        if !response.status().is_success() {
            return Err(matrix_error(response).await);
        }
        let state: Vec<serde_json::Value> = response
            .json()
            .await
            .map_err(|error| error.without_url().to_string())?;

        let mut bridged = false;
        let mut network = None;
        let mut name = None;
        let mut members = 0u64;
        let mut observation = Observation::Absent;
        let bot = bot_user_id(&state);
        for event in &state {
            let Some(event_type) = event.get("type").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let content = event.get("content");
            match event_type {
                "m.bridge" => {
                    bridged = true;
                    network = network.or_else(|| {
                        content
                            .and_then(|content| content.get("protocol"))
                            .and_then(|protocol| protocol.get("id"))
                            .and_then(serde_json::Value::as_str)
                            .and_then(network_of_protocol)
                            .map(str::to_owned)
                    });
                }
                "m.room.name" => {
                    name = content
                        .and_then(|content| content.get("name"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned);
                }
                "m.room.member" => {
                    let Some(who) = event.get("state_key").and_then(serde_json::Value::as_str)
                    else {
                        continue;
                    };
                    let membership = content
                        .and_then(|content| content.get("membership"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    if who == self.sensor_user_id {
                        observation = match membership {
                            "join" => Observation::Observing,
                            "invite" => Observation::Invited,
                            _ => Observation::Absent,
                        };
                        continue;
                    }
                    if membership != "join" {
                        continue;
                    }
                    // The bridge bot is the account doing the asking, so it
                    // is in every one of these rooms and is nobody's
                    // correspondent. Counting it would make every
                    // conversation look one person larger than it is.
                    if Some(who) == bot.as_deref() {
                        continue;
                    }
                    members += 1;
                }
                _ => {}
            }
        }
        if !bridged {
            debug!(
                bridge = %bridge.bridge_id,
                room = %room_id,
                "a room with no m.bridge marker is not a conversation, skipping it"
            );
            return Ok(None);
        }
        Ok(Some(Portal {
            room_id: room_id.to_owned(),
            bridge_id: bridge.bridge_id.clone(),
            network: network.unwrap_or_else(|| bridge.network.clone()),
            name,
            members,
            observation,
        }))
    }

    /// Puts the Sensor into the named portal rooms, or takes it out of them.
    ///
    /// Every room is resolved against a live register first. A room this
    /// Gateway does not hold as a portal is answered
    /// [`OutcomeStatus::UnknownPortal`] and **no call is made with the
    /// appservice credential** — that credential's reach is the bot's own
    /// rooms and a room id in a request is never a reason to widen it.
    pub async fn set_observation(
        &self,
        rooms: &[String],
        observed: bool,
    ) -> Result<Vec<ObservationOutcome>, PortalRefusal> {
        if rooms.is_empty() {
            return Err(PortalRefusal::InvalidRequest {
                detail: "name at least one room".to_owned(),
            });
        }
        if rooms.len() > MAX_ROOMS_PER_REQUEST {
            return Err(PortalRefusal::InvalidRequest {
                detail: format!(
                    "too many rooms in one request: {} named, {MAX_ROOMS_PER_REQUEST} is the most",
                    rooms.len()
                ),
            });
        }
        let register = self.read().await;
        let known: BTreeMap<&str, &Portal> = register
            .portals
            .iter()
            .map(|portal| (portal.room_id.as_str(), portal))
            .collect();
        let tokens: BTreeMap<&str, &str> = self
            .bridges
            .iter()
            .filter_map(|bridge| Some((bridge.bridge_id.as_str(), bridge.as_token.as_deref()?)))
            .collect();

        let mut outcomes = Vec::with_capacity(rooms.len());
        for room_id in rooms {
            let Some(portal) = known.get(room_id.as_str()) else {
                outcomes.push(ObservationOutcome {
                    room_id: room_id.clone(),
                    status: OutcomeStatus::UnknownPortal,
                });
                continue;
            };
            let Some(as_token) = tokens.get(portal.bridge_id.as_str()) else {
                // Unreachable in practice: a portal is only in the register
                // because its bridge's token read it.
                outcomes.push(ObservationOutcome {
                    room_id: room_id.clone(),
                    status: OutcomeStatus::Failed {
                        reason: "no_appservice_token".to_owned(),
                    },
                });
                continue;
            };
            let status = if observed {
                self.invite(as_token, portal).await
            } else {
                self.remove(as_token, portal).await
            };
            outcomes.push(ObservationOutcome {
                room_id: room_id.clone(),
                status,
            });
        }
        Ok(outcomes)
    }

    /// Invites the Sensor into one portal, as the bridge's own bot.
    async fn invite(&self, as_token: &str, portal: &Portal) -> OutcomeStatus {
        if portal.observation != Observation::Absent {
            return OutcomeStatus::AlreadyObserved;
        }
        let url = match self.url(&[
            "_matrix",
            "client",
            "v3",
            "rooms",
            &portal.room_id,
            "invite",
        ]) {
            Ok(url) => url,
            Err(detail) => return OutcomeStatus::Failed { reason: detail },
        };
        let response = self
            .http
            .post(url)
            .bearer_auth(as_token)
            .json(&serde_json::json!({ "user_id": self.sensor_user_id }))
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => {
                info!(
                    room = %portal.room_id,
                    bridge = %portal.bridge_id,
                    sensor = %self.sensor_user_id,
                    "invited the Sensor into a portal room"
                );
                OutcomeStatus::Invited
            }
            Ok(response) => {
                let detail = matrix_error(response).await;
                if detail.contains("already in the room") {
                    return OutcomeStatus::AlreadyObserved;
                }
                warn!(
                    room = %portal.room_id,
                    %detail,
                    "the homeserver refused to invite the Sensor into a portal room"
                );
                OutcomeStatus::Failed {
                    reason: errcode_of(&detail),
                }
            }
            Err(error) => OutcomeStatus::Failed {
                reason: error.without_url().to_string(),
            },
        }
    }

    /// Takes the Sensor out of one portal, as the bridge's own bot.
    ///
    /// A removal rather than a request to leave: the Sensor has no inbound
    /// API and inventing one so that it could be asked to leave a room would
    /// be a second control plane for a membership Matrix already models. The
    /// bot created the room and can remove a member from it, so stopping
    /// observation is the exact inverse of starting it, through one
    /// credential and one mechanism.
    async fn remove(&self, as_token: &str, portal: &Portal) -> OutcomeStatus {
        if portal.observation == Observation::Absent {
            return OutcomeStatus::NotObserved;
        }
        let url = match self.url(&["_matrix", "client", "v3", "rooms", &portal.room_id, "kick"]) {
            Ok(url) => url,
            Err(detail) => return OutcomeStatus::Failed { reason: detail },
        };
        let response = self
            .http
            .post(url)
            .bearer_auth(as_token)
            .json(&serde_json::json!({
                "user_id": self.sensor_user_id,
                "reason": "the user stopped observing this conversation",
            }))
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => {
                info!(
                    room = %portal.room_id,
                    bridge = %portal.bridge_id,
                    sensor = %self.sensor_user_id,
                    "removed the Sensor from a portal room"
                );
                OutcomeStatus::Removed
            }
            Ok(response) => {
                let detail = matrix_error(response).await;
                warn!(
                    room = %portal.room_id,
                    %detail,
                    "the homeserver refused to remove the Sensor from a portal room"
                );
                OutcomeStatus::Failed {
                    reason: errcode_of(&detail),
                }
            }
            Err(error) => OutcomeStatus::Failed {
                reason: error.without_url().to_string(),
            },
        }
    }

    /// Joins a path onto the homeserver's base URL, percent-encoding each
    /// segment — room ids carry `!` and `:`.
    fn url(&self, segments: &[&str]) -> Result<reqwest::Url, String> {
        let mut url = reqwest::Url::parse(&self.homeserver_url)
            .map_err(|error| format!("GATEWAY_HOMESERVER_URL is not a URL: {error}"))?;
        url.path_segments_mut()
            .map_err(|_| "GATEWAY_HOMESERVER_URL cannot carry a path".to_owned())?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }
}

/// Re-reads the register on an interval, for as long as the process runs, so
/// `/metrics` can state how many conversations the Sensor is outside without
/// anybody opening the Companion.
///
/// This is the loop that makes the mechanism *continuous*. The bridge builds
/// a portal when a conversation becomes active, and there is no push the
/// Gateway could subscribe to — an appservice transaction goes to the
/// appservice, and the appservice is the bridge. So the register is read
/// again, and a portal created at 13:24 is in it by 13:29 at the latest, and
/// in the Companion's own read immediately.
pub async fn refresh_until_shutdown(portals: Arc<Portals>, interval_seconds: u64) {
    if interval_seconds == 0 {
        info!("the portal register's background refresh is off (GATEWAY_PORTAL_REFRESH_SECONDS=0)");
        return;
    }
    let interval = std::time::Duration::from_secs(interval_seconds);
    info!(
        seconds = interval_seconds,
        sensor = %portals.sensor_user_id(),
        "refreshing the portal register"
    );
    loop {
        let register = portals.read().await;
        let summary = register.summary();
        debug!(
            observing = summary
                .get(&Observation::Observing)
                .copied()
                .unwrap_or_default(),
            invited = summary
                .get(&Observation::Invited)
                .copied()
                .unwrap_or_default(),
            absent = summary
                .get(&Observation::Absent)
                .copied()
                .unwrap_or_default(),
            unreadable_bridges = register.unreadable_bridges(),
            "read the portal register"
        );
        tokio::time::sleep(interval).await;
    }
}

/// The bridge bot's own Matrix ID, read out of the room's state rather than
/// asked for: the `m.bridge` marker names it in `content.bridgebot`, which
/// saves a `whoami` per bridge and stays right on a deployment running two.
/// `None` when the marker omits it, in which case the bot is counted as a
/// member — a conversation one person too large is a better failure than a
/// count that silently drops somebody.
fn bot_user_id(state: &[serde_json::Value]) -> Option<String> {
    state
        .iter()
        .filter(|event| event.get("type").and_then(serde_json::Value::as_str) == Some("m.bridge"))
        .find_map(|event| {
            event
                .get("content")?
                .get("bridgebot")?
                .as_str()
                .map(str::to_owned)
        })
}

/// The contract network a bridge's `m.bridge` protocol id names, or `None`
/// when this version knows none — the same folding the Sensor does
/// (`sensor/src/network.rs`), so the two cannot disagree about a room.
fn network_of_protocol(protocol_id: &str) -> Option<&'static str> {
    match protocol_id {
        "whatsapp" => Some("whatsapp"),
        "telegram" => Some("telegram"),
        "signal" => Some("signal"),
        "discord" | "discordgo" => Some("discord"),
        "sms" | "gmessages" | "gmessages-sms" | "gmessages-rcs" => Some("sms"),
        _ => None,
    }
}

/// The homeserver's own words about a refusal: its error code and message,
/// for the operator's log and for the `reason` one room carries.
async fn matrix_error(response: reqwest::Response) -> String {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(document) => {
            let errcode = document
                .get("errcode")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let error = document
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            format!("{status}: {errcode} {error}").trim_end().to_owned()
        }
        Err(_) => format!("{status}"),
    }
}

/// The `M_...` code out of [`matrix_error`]'s sentence, which is the part a
/// client can act on. A closed shape, so a `reason` never carries a
/// homeserver's prose.
fn errcode_of(detail: &str) -> String {
    detail
        .split_whitespace()
        .find(|word| word.starts_with("M_"))
        .unwrap_or("unreachable")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portal(room_id: &str, observation: Observation) -> Portal {
        Portal {
            room_id: room_id.to_owned(),
            bridge_id: "mautrix-whatsapp".to_owned(),
            network: "whatsapp".to_owned(),
            name: None,
            members: 2,
            observation,
        }
    }

    #[test]
    fn the_summary_counts_every_state_even_at_zero() {
        // A deployment that observes nothing must be able to say so, which
        // means the series exists at zero rather than being absent.
        let register = Register::default();
        let summary = register.summary();
        assert_eq!(summary.get(&Observation::Observing), Some(&0));
        assert_eq!(summary.get(&Observation::Invited), Some(&0));
        assert_eq!(summary.get(&Observation::Absent), Some(&0));
    }

    #[test]
    fn the_summary_is_the_sentence_the_deployment_has_to_be_able_to_say() {
        let mut register = Register::default();
        register
            .portals
            .push(portal("!a:x", Observation::Observing));
        for index in 0..17 {
            register
                .portals
                .push(portal(&format!("!b{index}:x"), Observation::Absent));
        }
        let summary = register.summary();
        assert_eq!(summary.get(&Observation::Observing), Some(&1));
        assert_eq!(summary.get(&Observation::Absent), Some(&17));
        assert_eq!(register.portals.len(), 18);
    }

    #[test]
    fn a_bridge_with_no_token_is_reported_not_skipped() {
        let register = Register {
            portals: Vec::new(),
            bridges: vec![
                BridgeReading {
                    bridge_id: "mautrix-whatsapp".to_owned(),
                    network: "whatsapp".to_owned(),
                    unreadable: None,
                },
                BridgeReading {
                    bridge_id: "mautrix-signal".to_owned(),
                    network: "signal".to_owned(),
                    unreadable: Some("no appservice token configured".to_owned()),
                },
            ],
        };
        assert_eq!(register.unreadable_bridges(), 1);
    }

    #[test]
    fn a_protocol_id_folds_to_the_network_the_sensor_would_attribute() {
        assert_eq!(network_of_protocol("whatsapp"), Some("whatsapp"));
        assert_eq!(network_of_protocol("gmessages-rcs"), Some("sms"));
        assert_eq!(network_of_protocol("discordgo"), Some("discord"));
        // Unknown means the Sensor could not attribute it either; the
        // bridge's configured network is the fallback, never a guess here.
        assert_eq!(network_of_protocol("matrix"), None);
        assert_eq!(network_of_protocol("irc"), None);
    }

    #[test]
    fn a_reason_is_an_error_code_and_never_the_homeservers_prose() {
        assert_eq!(
            errcode_of("403 Forbidden: M_FORBIDDEN @sensor:x is not invited to this room"),
            "M_FORBIDDEN"
        );
        assert_eq!(errcode_of("502 Bad Gateway"), "unreachable");
    }
}
