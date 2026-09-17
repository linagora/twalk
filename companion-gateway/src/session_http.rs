//! The session's HTTP surface: sign-in, refresh, sign-out, the device list,
//! revocation — and the guard that decides what every other endpoint of the
//! Gateway's API requires (ticket #52).
//!
//! # The guard is the default, not an opt-in
//!
//! Anything under `/api/` requires a valid device token unless
//! [`requirement`] says otherwise. That direction matters more than it looks:
//! a route added by a later ticket is protected before its author has written
//! a line of authentication code, and forgetting the guard is not one of the
//! mistakes available. The exceptions are a short, readable table:
//!
//! - sign-in itself, which is where a device gets its first token;
//! - refresh, which authenticates the refresh token instead and rotates it;
//! - the registration relay (ticket #53), which runs before any account
//!   exists and therefore before anyone can sign in. What stands in for
//!   authentication there is the one-account rule and the operator's decision
//!   to set `GATEWAY_REGISTRATION_SHARED_SECRET` at all — see
//!   [`crate::bootstrap`];
//! - the consent snapshot (ticket #50), which takes a service token — the
//!   Sensor is not a device and has no OpenID token to sign in with.
//!
//! Note what is *not* in that table: the Sensor's invitation
//! (`POST /api/bootstrap/rooms`, ticket #53) takes a device token like
//! everything else, because it added no row. That is the direction working as
//! intended.
//!
//! The snapshot's row (ticket #50) is the one line that arrived that way:
//! `(&Method::GET, "/api/consent/snapshot") => Requirement::ServiceToken`.
//! The guard asks for no device cookie on that path and injects no device
//! identity into the request, leaving [`crate::consent_snapshot`] to compare
//! the caller's bearer token with the service token from its own
//! configuration. Nothing else about the guard changed, and no other route
//! became reachable without a device token — a service token opens that one
//! path and nothing else, exactly as a device token opens everything else and
//! not that path.
//!
//! # The cookie
//!
//! Same origin (the Gateway serves the Companion's own files), so the device
//! token travels as a cookie the browser's JavaScript cannot read:
//! `HttpOnly`, `SameSite=Lax`, `Secure` unless the origin is localhost —
//! where `Secure` would simply stop the cookie from working for a developer
//! on plain HTTP. `SameSite=Lax` rather than `Strict` so that following a
//! link into the Companion lands the user signed in; every state-changing
//! endpoint is a `POST` or a `DELETE`, which `Lax` does not send
//! cross-site.
//!
//! The refresh cookie is scoped to `/api/session`, so it is not sent with
//! every static file and every API call — the only requests that need it are
//! the refresh and the sign-out.

use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Router};
use serde::Deserialize;
use tracing::{info, warn};

use crate::http::Gateway;
use crate::matrix_openid::{OpenIdToken, Refusal};
use crate::session::{Device, Issued, Sessions, SignInRefusal};

/// The cookie carrying the device token.
pub const DEVICE_COOKIE: &str = "twalk_device";

/// The cookie carrying the refresh token, scoped to the refresh and
/// sign-out endpoints.
pub const REFRESH_COOKIE: &str = "twalk_refresh";

/// The path the refresh cookie is scoped to.
const REFRESH_COOKIE_PATH: &str = "/api/session";

/// What a request under `/api/` must carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    /// Nothing: this is where a device gets its first token.
    Open,
    /// A live device's token, in the [`DEVICE_COOKIE`]. The default for
    /// everything under `/api/`.
    DeviceToken,
    /// The refresh token, which the handler reads and rotates itself.
    RefreshToken,
    /// A service token the route's own handler verifies — the consent
    /// snapshot (#50), whose caller is the Sensor and not a device. The
    /// guard authenticates nothing here and injects no device identity, so a
    /// device cookie grants no access to such a route.
    ServiceToken,
}

/// What the request at this method and path must carry. The whole
/// authentication policy of the Gateway's API is this function.
pub fn requirement(method: &Method, path: &str) -> Requirement {
    match (method, path) {
        (&Method::POST, "/api/session") => Requirement::Open,
        (&Method::POST, "/api/session/refresh") => Requirement::RefreshToken,
        // The registration relay: there is no account yet, so there is no
        // device token to have (ticket #53).
        (&Method::POST, "/api/bootstrap/account") => Requirement::Open,
        // The consent snapshot (ticket #50): its caller is the Sensor, which
        // is not a device and has no OpenID token to sign in with, so it
        // presents a service token the handler checks itself.
        (&Method::GET, "/api/consent/snapshot") => Requirement::ServiceToken,
        _ => Requirement::DeviceToken,
    }
}

/// The session routes. Merged into the Gateway's router, so registering them
/// is additive to whatever else the API grows.
pub fn routes() -> Router<Gateway> {
    Router::new()
        .route("/api/session", post(sign_in).get(current).delete(sign_out))
        .route("/api/session/refresh", post(refresh))
        .route("/api/devices", get(devices))
        .route("/api/devices/{id}", delete(revoke))
}

/// The guard. Runs inside the observation layer, so a refusal is counted and
/// logged like any other answer.
pub async fn guard(State(gateway): State<Gateway>, mut request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if !path.starts_with("/api/") && path != "/api" {
        // The Companion's own files, the health endpoint, the metrics
        // endpoint: the origin's public half. The app shell has to load
        // before anyone can sign in.
        return next.run(request).await;
    }
    let Some(sessions) = gateway.sessions() else {
        // Sign-in is not configured, so no request can be authenticated and
        // none is served. The origin stays up — the Companion still loads
        // and can say what is wrong — but its API is closed.
        return not_configured();
    };
    match requirement(request.method(), path) {
        Requirement::Open | Requirement::RefreshToken | Requirement::ServiceToken => {
            next.run(request).await
        }
        Requirement::DeviceToken => {
            let Some(token) = cookie(request.headers(), DEVICE_COOKIE) else {
                return refused(StatusCode::UNAUTHORIZED, "unauthenticated");
            };
            let Some(device) = sessions.authenticate(token) else {
                // Unknown, expired or revoked — the same answer for all
                // three, so a probe learns nothing from the difference.
                return refused(StatusCode::UNAUTHORIZED, "unauthenticated");
            };
            request.extensions_mut().insert(device);
            next.run(request).await
        }
    }
}

/// `POST /api/session` — sign in with a Matrix OpenID token.
async fn sign_in(
    State(gateway): State<Gateway>,
    headers: HeaderMap,
    body: Result<Json<SignInRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Some(sessions) = gateway.sessions() else {
        return not_configured();
    };
    let Ok(Json(request)) = body else {
        return refused(StatusCode::BAD_REQUEST, "invalid_request");
    };
    match sessions
        .sign_in(&request.matrix_openid_token, request.device_name.as_deref())
        .await
    {
        Ok(issued) => {
            gateway.metrics().record_sign_in("accepted");
            info!(
                device = %issued.device.id,
                device_name = %issued.device.name,
                "a device signed in"
            );
            session_response(StatusCode::OK, &sessions, &issued, &headers)
        }
        Err(refusal) => {
            gateway.metrics().record_sign_in(refusal.label());
            let (status, error) = match &refusal {
                // The identity is not the owner's: the one refusal the user
                // can act on, and the one an operator wants to see.
                SignInRefusal::NotTheOwner { subject } => {
                    warn!(
                        subject = %subject,
                        owner = %sessions.owner(),
                        "refused a sign-in: the Matrix ID is not this deployment's owner"
                    );
                    (StatusCode::FORBIDDEN, "not_the_owner")
                }
                SignInRefusal::Replayed => {
                    warn!("refused a sign-in: this OpenID token has already been used");
                    (StatusCode::UNAUTHORIZED, "openid_token_replayed")
                }
                SignInRefusal::Unresolved(Refusal::ForeignHomeserver { claimed }) => {
                    warn!(
                        claimed = %claimed,
                        homeserver = %sessions.homeserver_name(),
                        "refused a sign-in: the OpenID token names another homeserver"
                    );
                    (StatusCode::BAD_REQUEST, "foreign_homeserver")
                }
                SignInRefusal::Unresolved(Refusal::ForeignSubject { subject })
                | SignInRefusal::Unresolved(Refusal::MalformedSubject { subject }) => {
                    warn!(
                        subject = %subject,
                        "refused a sign-in: the homeserver answered with a Matrix ID it cannot speak for"
                    );
                    (StatusCode::FORBIDDEN, "not_the_owner")
                }
                SignInRefusal::Unresolved(Refusal::TokenRejected) => {
                    (StatusCode::UNAUTHORIZED, "openid_token_rejected")
                }
                SignInRefusal::Unresolved(Refusal::Unverifiable { detail }) => {
                    warn!(%detail, "could not verify an OpenID token at the homeserver");
                    (StatusCode::BAD_GATEWAY, "homeserver_unverifiable")
                }
                SignInRefusal::Store(error) => {
                    warn!(%error, "the session store failed during a sign-in");
                    (StatusCode::INTERNAL_SERVER_ERROR, "store_failed")
                }
            };
            refused(status, error)
        }
    }
}

/// `POST /api/session/refresh` — exchange the refresh token for a new pair.
async fn refresh(State(gateway): State<Gateway>, headers: HeaderMap) -> Response {
    let Some(sessions) = gateway.sessions() else {
        return not_configured();
    };
    let Some(token) = cookie(&headers, REFRESH_COOKIE) else {
        return refused(StatusCode::UNAUTHORIZED, "unauthenticated");
    };
    match sessions.refresh(token) {
        Some(issued) => session_response(StatusCode::OK, &sessions, &issued, &headers),
        None => refused(StatusCode::UNAUTHORIZED, "unauthenticated"),
    }
}

/// `GET /api/session` — who is signed in, on which device. What the Companion
/// asks on boot to know whether it must show the sign-in screen.
async fn current(State(gateway): State<Gateway>, Extension(device): Extension<Device>) -> Response {
    let Some(sessions) = gateway.sessions() else {
        return not_configured();
    };
    Json(serde_json::json!({
        "owner": sessions.owner(),
        "homeserver": sessions.homeserver_name(),
        "device": device_document(&device, Some(&device)),
    }))
    .into_response()
}

/// `DELETE /api/session` — sign this device out, which revokes it: a device
/// the user signed out of is a device that should stop working, and signing
/// in again is a silent round trip for the Companion.
async fn sign_out(
    State(gateway): State<Gateway>,
    headers: HeaderMap,
    Extension(device): Extension<Device>,
) -> Response {
    let Some(sessions) = gateway.sessions() else {
        return not_configured();
    };
    if let Err(error) = sessions.revoke(&device.id) {
        warn!(%error, "failed to revoke a device on sign-out");
        return refused(StatusCode::INTERNAL_SERVER_ERROR, "store_failed");
    }
    info!(device = %device.id, "a device signed out");
    let secure = secure_origin(&headers);
    (
        StatusCode::NO_CONTENT,
        cookies([
            cleared_cookie(DEVICE_COOKIE, "/", secure),
            cleared_cookie(REFRESH_COOKIE, REFRESH_COOKIE_PATH, secure),
        ]),
    )
        .into_response()
}

/// `GET /api/devices` — the device list: each device named and dated, with
/// the revoked ones kept so that "I revoked that phone" stays visible.
async fn devices(
    State(gateway): State<Gateway>,
    Extension(current): Extension<Device>,
) -> Response {
    let Some(sessions) = gateway.sessions() else {
        return not_configured();
    };
    match sessions.devices() {
        Ok(devices) => Json(serde_json::json!({
            "devices": devices
                .iter()
                .map(|device| device_document(device, Some(&current)))
                .collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(error) => {
            warn!(%error, "failed to read the device list");
            refused(StatusCode::INTERNAL_SERVER_ERROR, "store_failed")
        }
    }
}

/// `DELETE /api/devices/{id}` — revoke one device. Its token stops working
/// on its very next request, including when that device is this one.
async fn revoke(
    State(gateway): State<Gateway>,
    Extension(current): Extension<Device>,
    Path(id): Path<String>,
) -> Response {
    let Some(sessions) = gateway.sessions() else {
        return not_configured();
    };
    match sessions.revoke(&id) {
        Ok(true) => {
            info!(device = %id, revoked_by = %current.id, "revoked a device");
            StatusCode::NO_CONTENT.into_response()
        }
        // An unknown id and an already revoked device are one answer: the
        // caller asked for that device to stop working, and it does not.
        Ok(false) => refused(StatusCode::NOT_FOUND, "not_found"),
        Err(error) => {
            warn!(%error, "failed to revoke a device");
            refused(StatusCode::INTERNAL_SERVER_ERROR, "store_failed")
        }
    }
}

/// The body of a sign-in: the OpenID token the Companion got from the
/// homeserver, and the name for this device in the list.
#[derive(Debug, Deserialize)]
struct SignInRequest {
    matrix_openid_token: OpenIdToken,
    #[serde(default)]
    device_name: Option<String>,
}

/// The session document, with the two `Set-Cookie` headers that are the
/// actual credential.
fn session_response(
    status: StatusCode,
    sessions: &Sessions,
    issued: &Issued,
    headers: &HeaderMap,
) -> Response {
    let secure = secure_origin(headers);
    (
        status,
        cookies([
            set_cookie(
                DEVICE_COOKIE,
                &issued.device_token,
                issued.device_token_ttl_seconds,
                "/",
                secure,
            ),
            set_cookie(
                REFRESH_COOKIE,
                &issued.refresh_token,
                issued.refresh_token_ttl_seconds,
                REFRESH_COOKIE_PATH,
                secure,
            ),
        ]),
        Json(serde_json::json!({
            "owner": sessions.owner(),
            "homeserver": sessions.homeserver_name(),
            "device": device_document(&issued.device, Some(&issued.device)),
            // So the Companion knows when to refresh rather than discovering
            // it from a 401.
            "expires_in": issued.device_token_ttl_seconds,
        })),
    )
        .into_response()
}

/// One device as the API renders it. Dates are seconds since the epoch: the
/// Gateway carries no date library, and the Companion renders local time
/// from a number as readily as from a string.
fn device_document(device: &Device, current: Option<&Device>) -> serde_json::Value {
    serde_json::json!({
        "id": device.id,
        "name": device.name,
        "created_unix_seconds": device.created_unix_seconds,
        "last_seen_unix_seconds": device.last_seen_unix_seconds,
        "revoked_unix_seconds": device.revoked_unix_seconds,
        "current": current.is_some_and(|current| current.id == device.id),
    })
}

/// A refusal the Companion's fetch can parse. Never a reason it could use to
/// tell "wrong token" from "revoked device" apart.
fn refused(status: StatusCode, error: &str) -> Response {
    (status, Json(serde_json::json!({ "error": error }))).into_response()
}

/// Sign-in is not configured: the API is closed, and the answer says which
/// variable would open it. Shared with [`crate::bootstrap_http`], whose
/// endpoints are equally meaningless without an owner.
pub(crate) fn not_configured() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": "sign_in_not_configured",
            "detail": "the Gateway has no owner: set GATEWAY_OWNER (and GATEWAY_HOMESERVER_FEDERATION_URL, GATEWAY_STATE_DIR) to enable sign-in",
        })),
    )
        .into_response()
}

/// Both session cookies as a header map. Two `Set-Cookie` headers, appended
/// rather than inserted: a response carries one header per cookie, and an
/// array of header pairs would have the second replace the first.
fn cookies<const N: usize>(values: [HeaderValue; N]) -> HeaderMap {
    let mut headers = HeaderMap::with_capacity(N);
    for value in values {
        headers.append(header::SET_COOKIE, value);
    }
    headers
}

/// A `Set-Cookie` header for one of the session cookies.
fn set_cookie(
    name: &str,
    value: &str,
    max_age_seconds: u64,
    path: &str,
    secure: bool,
) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    let value = format!(
        "{name}={value}; Max-Age={max_age_seconds}; Path={path}; HttpOnly; SameSite=Lax{secure}"
    );
    HeaderValue::from_str(&value).expect("a cookie of hex and ASCII is a header value")
}

/// The `Set-Cookie` that removes one.
fn cleared_cookie(name: &str, path: &str, secure: bool) -> HeaderValue {
    set_cookie(name, "", 0, path, secure)
}

/// One cookie's value out of the request's `Cookie` header(s).
fn cookie<'headers>(headers: &'headers HeaderMap, name: &str) -> Option<&'headers str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
        .filter(|value| !value.is_empty())
}

/// Whether the session cookies carry `Secure`. Yes, except on localhost —
/// where the developer's origin is plain HTTP and a `Secure` cookie would be
/// dropped by the browser, leaving sign-in silently broken. In a deployment
/// the origin is the operator's own name behind their TLS proxy, and the
/// flag is set.
fn secure_origin(headers: &HeaderMap) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    // Strip the port; an IPv6 literal keeps its brackets.
    let host = match host.rsplit_once(':') {
        Some((before, _)) if !before.is_empty() && !before.ends_with(':') => before,
        _ => host,
    };
    !matches!(
        host.trim_end_matches('.'),
        "localhost" | "127.0.0.1" | "[::1]" | "::1"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_requires_a_device_token_unless_the_table_says_otherwise() {
        assert_eq!(
            requirement(&Method::POST, "/api/session"),
            Requirement::Open,
            "sign-in is where a device gets its first token"
        );
        assert_eq!(
            requirement(&Method::POST, "/api/session/refresh"),
            Requirement::RefreshToken
        );
        assert_eq!(
            requirement(&Method::POST, "/api/bootstrap/account"),
            Requirement::Open,
            "registration runs before any account exists, so before any sign-in"
        );
        assert_eq!(
            requirement(&Method::GET, "/api/consent/snapshot"),
            Requirement::ServiceToken,
            "the snapshot's caller is the Sensor, which is not a device"
        );
        // And only under that method: a write to the snapshot's path is not
        // a route, and the service token must not be the way to reach one.
        assert_eq!(
            requirement(&Method::POST, "/api/consent/snapshot"),
            Requirement::DeviceToken
        );
        // Everything else, including routes that do not exist yet: a device
        // token. This is the property that makes forgetting the guard
        // impossible for a later ticket.
        for (method, path) in [
            (Method::GET, "/api/session"),
            (Method::DELETE, "/api/session"),
            (Method::GET, "/api/devices"),
            (Method::DELETE, "/api/devices/abc"),
            (Method::POST, "/api/consent/decisions"),
            (Method::GET, "/api/anything/at/all"),
            // Not the sign-in route under another method.
            (Method::GET, "/api/session/refresh"),
            // Inviting the Sensor added no row, so it is behind the guard
            // without asking (ticket #53).
            (Method::POST, "/api/bootstrap/rooms"),
            // And neither the registration route under another method, nor a
            // path that merely starts like it.
            (Method::GET, "/api/bootstrap/account"),
            (Method::POST, "/api/bootstrap/accounts"),
        ] {
            assert_eq!(
                requirement(&method, path),
                Requirement::DeviceToken,
                "{method} {path}"
            );
        }
    }

    #[test]
    fn a_cookie_is_read_out_of_the_header_it_shares_with_others() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("other=1; twalk_device=abc123; another=2"),
        );
        assert_eq!(cookie(&headers, DEVICE_COOKIE), Some("abc123"));
        assert_eq!(cookie(&headers, REFRESH_COOKIE), None);

        // Two Cookie headers, as a proxy may leave them.
        let mut split = HeaderMap::new();
        split.append(header::COOKIE, HeaderValue::from_static("other=1"));
        split.append(
            header::COOKIE,
            HeaderValue::from_static("twalk_refresh=def456"),
        );
        assert_eq!(cookie(&split, REFRESH_COOKIE), Some("def456"));

        // A cleared cookie is not a credential.
        let mut cleared = HeaderMap::new();
        cleared.insert(header::COOKIE, HeaderValue::from_static("twalk_device="));
        assert_eq!(cookie(&cleared, DEVICE_COOKIE), None);
    }

    #[test]
    fn the_cookie_carries_the_attributes_that_make_it_a_session_credential() {
        let value = set_cookie(DEVICE_COOKIE, "abc", 900, "/", true);
        let value = value.to_str().expect("ascii");
        assert!(value.starts_with("twalk_device=abc;"), "{value}");
        for attribute in [
            "Max-Age=900",
            "Path=/",
            "HttpOnly",
            "SameSite=Lax",
            "Secure",
        ] {
            assert!(
                value.contains(attribute),
                "{attribute} missing from {value}"
            );
        }
        // Without TLS there is no Secure flag, or the cookie would not work
        // at all on a developer's origin.
        let insecure = set_cookie(DEVICE_COOKIE, "abc", 900, "/", false);
        assert!(!insecure.to_str().expect("ascii").contains("Secure"));

        // Two cookies make two headers, not one that replaced the other.
        let both = cookies([
            set_cookie(DEVICE_COOKIE, "abc", 900, "/", false),
            set_cookie(REFRESH_COOKIE, "def", 9_000, REFRESH_COOKIE_PATH, false),
        ]);
        assert_eq!(both.get_all(header::SET_COOKIE).iter().count(), 2);
    }

    #[test]
    fn only_a_localhost_origin_drops_the_secure_flag() {
        let host = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, HeaderValue::from_str(value).expect("a host"));
            secure_origin(&headers)
        };
        assert!(!host("localhost"));
        assert!(!host("localhost:8080"));
        assert!(!host("127.0.0.1:18368"));
        assert!(!host("[::1]:8080"));
        assert!(host("twalk.example.com"));
        assert!(host("twalk.example.com:8443"));
        // No Host header at all: treat the origin as public.
        assert!(secure_origin(&HeaderMap::new()));
    }
}
