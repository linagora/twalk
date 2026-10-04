//! The collector's internal HTTP endpoint (issue #281): the routes the
//! Companion Gateway calls on this process — `GET /freebusy` and
//! `GET /event-facts` (the snapshot seam the other way round), and, since
//! lot 3a, `GET /search` and `GET /index/status` over the owner's archive.
//! The Gateway reads the owner's free/busy here on Hermes's behalf and never
//! from the side service itself, so the one process that holds the owner's
//! grant is the one that reads their agenda, and what leaves it is the
//! intervals `freebusy.rs` allows — nothing else. A search answers hits —
//! id, source, correspondent, date, subject, a bounded snippet — and never a
//! body.
//!
//! The caller is the Gateway and only the Gateway: the bearer is the same
//! service token this collector presents to read the registry
//! (`COLLECTOR_GATEWAY_SERVICE_TOKEN`), which is why `COLLECTOR_HTTP_LISTEN`
//! cannot be set without it. The endpoint is not the Gateway's API: it
//! carries no device token, no session, and answers on the internal network
//! the compose file gives it. Every read is counted by outcome; the record
//! of who asked, for what window, and when, is the Gateway's (`hermes_read`),
//! because the Gateway is where Hermes's signature was checked.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::calendars::Calendars;
use crate::freebusy::Window;
use crate::metrics::Metrics;
use crate::side::SideError;

/// Every outcome a read is counted under, `served` first.
pub const READ_OUTCOMES: [&str; 8] = [
    "served",
    "unauthenticated",
    "connection_unknown",
    "invalid_window",
    "window_too_wide",
    "connection_not_connected",
    "caldav_refused",
    "caldav_unreachable",
];

/// Which of the endpoint's two reads a refusal belongs to (#355). They
/// share their shape and their refusal codes and are counted apart, because
/// "how often was my agenda pulled" and "how often was an event asked
/// about" are two questions an owner asks separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    FreeBusy,
    EventFacts,
}

/// Every outcome a read of one event's facts is counted under (#355).
/// `served` covers an event this collector holds and one it does not: what
/// differs there is the answer's `found`, not whether the read worked.
pub const EVENT_FACT_OUTCOMES: [&str; 7] = [
    "served",
    "unauthenticated",
    "connection_unknown",
    "invalid_uid",
    "connection_not_connected",
    "caldav_refused",
    "caldav_unreachable",
];

/// What the run loop knows about the calendar connection, for a read to
/// check before it asks the side service: the owner's id there, and the
/// connection's state as last observed.
#[derive(Debug, Clone, Default)]
pub struct CalendarAccess {
    pub owner_id: Option<String>,
    /// The state's name, as published; `None` before the first observation.
    pub state: Option<&'static str>,
}

pub type SharedCalendarAccess = Arc<RwLock<CalendarAccess>>;

#[derive(Clone)]
pub struct Endpoint {
    pub service_token: String,
    pub calendars: Option<Arc<Calendars>>,
    pub calendar_access: SharedCalendarAccess,
    pub access: crate::replies::SharedCredential,
    pub metrics: Arc<Metrics>,
    /// L'index de recherche (lot 3a), `None` quand aucune clé n'est
    /// configurée (`COLLECTOR_INDEX_KEY_FILE`) : un index absent est un `503
    /// index_not_configured`, jamais un refus de démarrage (§4.2).
    ///
    /// C27 : c'est **le même** `Arc<Mutex<Stored>>` que la boucle de poll
    /// tient dans `main.rs` — le poll l'écrit, la route le lit, un seul
    /// `Stored` pour un seul index. Un second `Stored` sur le même
    /// répertoire divergerait.
    pub search: Option<Arc<std::sync::Mutex<crate::search_index::Stored>>>,
    /// Le cache de consentement, pour retirer les révoqués des résultats
    /// (§5.3) : le collecteur le détient déjà, la route ne l'ouvre pas.
    pub consent: twalk_consent_cache::ConsentCache,
}

pub fn router(endpoint: Endpoint) -> Router {
    Router::new()
        .route("/freebusy", get(free_busy))
        .route("/event-facts", get(event_facts))
        .route("/search", get(search))
        .route("/index/status", get(index_status))
        .with_state(endpoint)
}

/// Whether a request carries this collector's own service token — the
/// Gateway's, and nobody else's. Compared as digests, in constant time: the
/// Companion Gateway's habit with this token (`consent_snapshot.rs`), kept
/// on this side, and one function so the two routes cannot drift into two
/// standards.
fn authenticated(endpoint: &Endpoint, headers: &HeaderMap) -> bool {
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    let presented = bearer.map(|token| Sha256::digest(token.as_bytes()));
    let expected = Sha256::digest(endpoint.service_token.as_bytes());
    presented.is_some_and(|presented| presented == expected)
}

#[derive(Debug, Deserialize)]
struct FreeBusyQuery {
    connection: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

async fn free_busy(
    State(endpoint): State<Endpoint>,
    headers: HeaderMap,
    Query(query): Query<FreeBusyQuery>,
) -> Response {
    if !authenticated(&endpoint, &headers) {
        return refuse(
            &endpoint,
            Read::FreeBusy,
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "this endpoint answers the Companion Gateway's service token and nothing else",
            None,
        );
    }
    let connection = query.connection.unwrap_or_default();
    let Some(calendars) = endpoint
        .calendars
        .as_ref()
        .filter(|calendars| calendars.connection == connection)
    else {
        return refuse(
            &endpoint,
            Read::FreeBusy,
            StatusCode::NOT_FOUND,
            "connection_unknown",
            &format!("this collector holds no calendar connection named {connection:?}"),
            None,
        );
    };
    let window = match Window::parse(
        query.from.as_deref().unwrap_or_default(),
        query.to.as_deref().unwrap_or_default(),
    ) {
        Ok(window) => window,
        Err(error) => {
            return refuse(
                &endpoint,
                Read::FreeBusy,
                StatusCode::BAD_REQUEST,
                error.code(),
                &error.message(),
                None,
            );
        }
    };
    // The connection's state as the run loop last observed it, and the two
    // things a read needs that the loop holds — the owner's id on the side
    // service and the credential. A state of `connected` with either
    // missing is a round that has not completed yet: `unknown`, not
    // `connected`, since "connected but unreadable" is not a state.
    let calendar = endpoint.calendar_access.read().await.clone();
    let token = endpoint.access.read().await.clone();
    let (owner_id, token) = match (calendar.state, calendar.owner_id, token) {
        (Some("connected"), Some(owner_id), Some(token)) => (owner_id, token),
        (state, _, _) => {
            let state = state
                .filter(|state| *state != "connected")
                .unwrap_or("unknown");
            return refuse(
                &endpoint,
                Read::FreeBusy,
                StatusCode::CONFLICT,
                "connection_not_connected",
                &format!(
                    "the calendar connection {connection:?} is {state}; the agenda cannot be \
                     read until it is connected"
                ),
                Some(state),
            );
        }
    };
    match calendars.free_busy(&owner_id, &token, &window).await {
        Ok((busy, zone)) => {
            endpoint.metrics.record_freebusy_read("served");
            // The zone and the hour it is there travel with the intervals
            // (#369). A zone the calendar declares but that is not an IANA
            // name is worth saying out loud and worth nothing to a reader, so
            // it is dropped rather than passed on: a model handed "Romance
            // Standard Time" will use it in a sentence.
            let local = zone.as_ref().and_then(|zone| match crate::zones::read(&zone.name) {
                Some(tz) => Some((zone, chrono::Utc::now().with_timezone(&tz))),
                None => {
                    warn!(
                        connection,
                        timezone = zone.name,
                        "the calendar declares a zone this build does not know; the read says \
                         it has none rather than name one nobody can convert"
                    );
                    None
                }
            });
            info!(
                connection,
                from = %window.from,
                to = %window.to,
                intervals = busy.len(),
                timezone = local.as_ref().map(|(zone, _)| zone.name.as_str()).unwrap_or("(none declared)"),
                "a free/busy read was served"
            );
            // The gaps, spelled in the owner's own time when it is known
            // (#379): a drafting agent that copies them cannot write an hour
            // in the wrong zone, and one that computes them already has.
            // The owner's working day, when they have said what it is (#381):
            // read here rather than in the gap computation, so that the pure
            // function stays pure and the decision is fetched once.
            let working_day = calendars
                .working_day
                .lock()
                .ok()
                .and_then(|day| day.clone());
            let free = crate::freebusy::free_between(
                &busy,
                &window,
                local.as_ref().map(|(zone, _)| zone.name.as_str()),
                working_day.as_ref(),
            );
            let mut answer = json!({
                "connection": connection,
                "from": window.from.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                "to": window.to.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                "busy": busy,
                "free": free,
            });
            // The amplitude that was applied, so that an agent can say the
            // gaps it was given are the ones inside it — and write "I am
            // taken on my usual hours that week", which is true, rather than
            // "I have no slot at all", which is not (#381).
            if let Some(day) = &working_day {
                answer["working_day"] = json!({
                    "days": day.days,
                    "starts_at": day.starts_at,
                    "ends_at": day.ends_at,
                });
                // And the days that run other hours (#386), when there are
                // any: the gaps of a short Wednesday were clipped by 12:30,
                // and an agent that was told the default alone would read
                // *"taken from 12:30 to 18:00"* out of a week the owner does
                // not work. Absent when every day runs the default, so a
                // deployment with one amplitude answers what it answered
                // before — the same rule as the zone members below.
                if !day.exceptions.is_empty() {
                    answer["working_day"]["exceptions"] = serde_json::Value::Object(
                        day.exceptions
                            .iter()
                            .map(|(weekday, span)| {
                                (
                                    weekday.to_string(),
                                    json!({
                                        "starts_at": span.starts_at,
                                        "ends_at": span.ends_at,
                                    }),
                                )
                            })
                            .collect(),
                    );
                }
            }
            // Absent, not null, when there is no zone to name: a member that
            // is there and empty says "I looked and the answer is nothing",
            // which is a different sentence from "there is no such fact" —
            // #359's rule, at a third door. The three go together.
            if let Some((zone, at)) = local {
                answer["timezone"] = json!(zone.name);
                answer["timezone_source"] = json!(zone.source);
                answer["now"] = json!(at.format("%Y-%m-%dT%H:%M:%S%:z").to_string());
            }
            (StatusCode::OK, Json(answer)).into_response()
        }
        Err(SideError::Refused { status, .. }) => refuse(
            &endpoint,
            Read::FreeBusy,
            StatusCode::BAD_GATEWAY,
            "caldav_refused",
            &format!("the calendar service refused the free-busy report with HTTP {status}"),
            None,
        ),
        Err(SideError::Unreachable { detail }) => refuse(
            &endpoint,
            Read::FreeBusy,
            StatusCode::BAD_GATEWAY,
            "caldav_unreachable",
            &format!("the calendar service did not answer the free-busy report: {detail}"),
            None,
        ),
    }
}

#[derive(Debug, Deserialize)]
struct FactsQuery {
    connection: Option<String>,
    uid: Option<String>,
}

/// `GET /event-facts?connection=&uid=` — what one of the owner's events
/// carries, without its words (#355).
///
/// The same terms as `/freebusy` above, and for the same reasons: the
/// Gateway's service token and nothing else, the internal network, and the
/// record of who asked and for what is the Gateway's, because that is where
/// Hermes's signature was checked.
///
/// What it answers is counts, one flag's worth of knowledge and at most one
/// URL — never the description, never an attachment's name. An event this
/// collector never published is one it does not answer about: `null`, said
/// as `found: false`, so a persona learns "I hold nothing about that" and
/// not "that meeting carries nothing".
async fn event_facts(
    State(endpoint): State<Endpoint>,
    headers: HeaderMap,
    Query(query): Query<FactsQuery>,
) -> Response {
    if !authenticated(&endpoint, &headers) {
        return refuse(
            &endpoint,
            Read::EventFacts,
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "this endpoint answers the Companion Gateway's service token and nothing else",
            None,
        );
    }
    let connection = query.connection.unwrap_or_default();
    let Some(calendars) = endpoint
        .calendars
        .as_ref()
        .filter(|calendars| calendars.connection == connection)
    else {
        return refuse(
            &endpoint,
            Read::EventFacts,
            StatusCode::NOT_FOUND,
            "connection_unknown",
            &format!("this collector holds no calendar connection named {connection:?}"),
            None,
        );
    };
    let uid = query.uid.unwrap_or_default();
    if uid.is_empty() || uid.chars().count() > 512 {
        return refuse(
            &endpoint,
            Read::EventFacts,
            StatusCode::BAD_REQUEST,
            "invalid_uid",
            "uid is the event's iCalendar UID, between 1 and 512 characters",
            None,
        );
    }
    let calendar = endpoint.calendar_access.read().await.clone();
    let token = endpoint.access.read().await.clone();
    let (owner_id, token) = match (calendar.state, calendar.owner_id, token) {
        (Some("connected"), Some(owner_id), Some(token)) => (owner_id, token),
        (state, _, _) => {
            let state = state
                .filter(|state| *state != "connected")
                .unwrap_or("unknown");
            return refuse(
                &endpoint,
                Read::EventFacts,
                StatusCode::CONFLICT,
                "connection_not_connected",
                &format!(
                    "the calendar connection {connection:?} is {state}; an event cannot be read \
                     until it is connected"
                ),
                Some(state),
            );
        }
    };
    match calendars.facts_about(&uid, &owner_id, &token).await {
        Ok(facts) => {
            endpoint.metrics.record_event_fact_read("served");
            info!(
                connection,
                found = facts.is_some(),
                "the facts about an event were served"
            );
            let body = match facts {
                Some(facts) => json!({
                    "connection": connection,
                    "uid": uid,
                    "found": true,
                    "conference": facts.conference,
                    "description_characters": facts.description_characters,
                    "attachments": facts.attachments,
                }),
                None => json!({
                    "connection": connection,
                    "uid": uid,
                    "found": false,
                    "conference": Value::Null,
                    "description_characters": Value::Null,
                    "attachments": 0,
                }),
            };
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(SideError::Refused { status, .. }) => refuse(
            &endpoint,
            Read::EventFacts,
            StatusCode::BAD_GATEWAY,
            "caldav_refused",
            &format!("the calendar service refused the read with HTTP {status}"),
            None,
        ),
        Err(SideError::Unreachable { detail }) => refuse(
            &endpoint,
            Read::EventFacts,
            StatusCode::BAD_GATEWAY,
            "caldav_unreachable",
            &format!("the calendar service did not answer the read: {detail}"),
            None,
        ),
    }
}

/// Every outcome a search is counted under (lot 3a), `served` first: the
/// search series holds the reads of the owner's archive, apart from the
/// agenda's, because a search is the read that touches the most words.
pub const SEARCH_OUTCOMES: [&str; 6] = [
    "served",
    "unauthenticated",
    "index_not_configured",
    "index_unavailable",
    "invalid_query",
    "invalid_window",
];

/// La raison d'un retrait par consentement, telle qu'elle part dans la
/// métrique — une seule chaîne, lue par la production et par son test.
pub const SEARCH_WITHHELD_CONSENT: &str = "consent";

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: Option<String>,
    source: Option<String>,
    from: Option<String>,
    to: Option<String>,
    limit: Option<String>,
}

/// Parse une borne de fenêtre (RFC 3339) en secondes Unix. `None` = absente
/// ou vide ; une chaîne illisible est une `Err`, pour un `400 invalid_window`
/// plutôt qu'un filtre silencieusement ignoré.
fn parse_bound(value: Option<&str>) -> Result<Option<i64>, ()> {
    match value {
        None => Ok(None),
        Some(text) if text.is_empty() => Ok(None),
        Some(text) => chrono::DateTime::parse_from_rfc3339(text)
            .map(|at| at.timestamp())
            .map(Some)
            .map_err(|_| ()),
    }
}

/// `GET /search?q=&source=&from=&to=&limit=` — l'archive de l'owner
/// interrogée sur l'endpoint interne, le chemin que le Companion Gateway
/// relaira au nom de l'owner dans sa session.
///
/// Ce que la route rend est des hits — id, source, correspondant, date,
/// sujet, extrait — **jamais un corps**. Le filtre de consentement est
/// appliqué ici, côté collecteur qui détient déjà le cache (§5.3) ; les hits
/// d'un révoqué sont retirés et le nombre est rendu, parce que le produit
/// compte ses silences.
async fn search(
    State(endpoint): State<Endpoint>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Response {
    if !authenticated(&endpoint, &headers) {
        return refuse_search(&endpoint, StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    let Some(stored) = &endpoint.search else {
        return refuse_search(&endpoint, StatusCode::SERVICE_UNAVAILABLE, "index_not_configured");
    };
    let text = query.q.unwrap_or_default();
    if text.trim().is_empty() || text.chars().count() > 512 {
        return refuse_search(&endpoint, StatusCode::BAD_REQUEST, "invalid_query");
    }
    // La fenêtre est parsée AVANT la recherche : une borne illisible ou
    // inversée est un refus, pas un filtre que l'on ignore.
    let (Ok(from), Ok(to)) = (
        parse_bound(query.from.as_deref()),
        parse_bound(query.to.as_deref()),
    ) else {
        return refuse_search(&endpoint, StatusCode::BAD_REQUEST, "invalid_window");
    };
    if let (Some(from), Some(to)) = (from, to) {
        if from > to {
            return refuse_search(&endpoint, StatusCode::BAD_REQUEST, "invalid_window");
        }
    }
    // Un `limit` hors bornes est un refus, jamais une borne silencieuse : un
    // client qui demande 1000 résultats et en reçoit 100 croit que l'archive
    // n'en contient pas plus (Review Focus classe 2, spec §5.1). Le code de
    // refus est `invalid_query`, déjà déclaré pour cette route.
    let limit = match query.limit.as_deref() {
        None => 20usize,
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) if (1..=100).contains(&n) => n,
            _ => return refuse_search(&endpoint, StatusCode::BAD_REQUEST, "invalid_query"),
        },
    };
    // C27 : le `Stored` est partagé avec l'indexation temps réel — on le
    // verrouille le temps de la lecture, et l'on ne fait aucune I/O sous le
    // verrou (`Stored::search` lit l'index en mémoire).
    let stored = stored.lock().expect("the index lock is not poisoned");
    match stored.search(&text, limit) {
        Ok(hits) => {
            // Les filtres source/from/to s'appliquent APRÈS la recherche : le
            // moteur Tantivy ne les connaît pas, `source` et `date` sont des
            // champs `stored`. Coût accepté : moins que `limit` résultats
            // (spec §5.1, §5.4).
            let hits: Vec<_> = hits
                .into_iter()
                .filter(|hit| {
                    query
                        .source
                        .as_deref()
                        .is_none_or(|source| hit.source == source)
                })
                .filter(|hit| from.is_none_or(|from| hit.date >= from))
                .filter(|hit| to.is_none_or(|to| hit.date <= to))
                .collect();
            // La connexion du filtre vient du `source` de chaque hit, jamais
            // de la requête (C18/C19) : il n'y a pas de paramètre à passer.
            let (kept, withheld) =
                crate::search_index::filter_by_consent(hits, &endpoint.consent);
            if withheld > 0 {
                endpoint.metrics.record_search_hit_withheld(SEARCH_WITHHELD_CONSENT);
            }
            endpoint.metrics.record_search_read("served");
            info!(
                query_length = text.chars().count(),
                hits = kept.len(),
                withheld,
                "a search was served"
            );
            let mut body = json!({
                "hits": kept,
                "count": kept.len(),
                "withheld": withheld,
            });
            if withheld > 0 {
                body["withheld_reason"] = json!("consent");
            }
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(error) => {
            // Le détail est loggé, jamais rendu : c'est un fait de l'index,
            // et le corps d'un refus a la forme `{ "error": <code> }` que le
            // Companion Gateway et `openapi.yaml` décrivent.
            warn!(
                error = %format!("{error:#}"),
                "the search index could not be read"
            );
            refuse_search(&endpoint, StatusCode::SERVICE_UNAVAILABLE, "index_unavailable")
        }
    }
}

/// `GET /index/status` — l'état d'indexation, pour la barre de progression.
async fn index_status(State(endpoint): State<Endpoint>, headers: HeaderMap) -> Response {
    if !authenticated(&endpoint, &headers) {
        return refuse_search(&endpoint, StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    let Some(stored) = &endpoint.search else {
        return refuse_search(&endpoint, StatusCode::SERVICE_UNAVAILABLE, "index_not_configured");
    };
    // `document_count` existe depuis T3 (C17 : ne pas le redéfinir). C27 :
    // lu sous le même verrou que la recherche.
    let documents = stored
        .lock()
        .expect("the index lock is not poisoned")
        .document_count();
    (StatusCode::OK, Json(json!({ "documents": documents }))).into_response()
}

/// Un refus de recherche : compté sous son code, dit à `warn`, dans la forme
/// `Error` du Companion Gateway (`{ "error": <code> }`) — le détail reste au
/// log.
fn refuse_search(endpoint: &Endpoint, status: StatusCode, code: &'static str) -> Response {
    endpoint.metrics.record_search_read(code);
    warn!(%code, status = status.as_u16(), "a search was refused");
    (status, Json(json!({ "error": code }))).into_response()
}

/// One refusal: counted under its code, said at `warn`, answered in the
/// Companion Gateway's `Error` shape — with the connection's `state` beside
/// it when that is what was refused.
fn refuse(
    endpoint: &Endpoint,
    read: Read,
    status: StatusCode,
    code: &'static str,
    detail: &str,
    state: Option<&str>,
) -> Response {
    match read {
        Read::FreeBusy => endpoint.metrics.record_freebusy_read(code),
        Read::EventFacts => endpoint.metrics.record_event_fact_read(code),
    }
    warn!(%code, status = status.as_u16(), state, detail, "a free/busy read was refused");
    let mut body = json!({ "error": code, "detail": detail });
    if let Some(state) = state {
        body["state"] = json!(state);
    }
    (status, Json(body)).into_response()
}

/// Serves the endpoint until the process ends.
pub async fn serve(listener: tokio::net::TcpListener, endpoint: Endpoint) {
    if let Err(error) = axum::serve(listener, router(endpoint)).await {
        warn!(%error, "the internal HTTP endpoint stopped");
    }
}
