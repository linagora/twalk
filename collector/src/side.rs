//! What the two services the grant opens have in common on the wire (#276,
//! #280): a bearer, a host for `source`, and the two ways a request fails
//! — the token refused, which is the connection's state to say, and
//! everything else, which is retried next round. `calendars.rs` and
//! `mails.rs` each keep the requests that are theirs.

use anyhow::Result;

/// Why a request to a service did not answer with what was asked.
#[derive(Debug)]
pub enum SideError {
    /// `401`/`403`: the token itself, or what the client lacks. The
    /// `WWW-Authenticate` the service offered comes with it, because what
    /// the operator has to do depends on it and on nothing else the
    /// collector can see (#320): a service that asks for a bearer wants
    /// more of the token, one that asks for something else — or for
    /// nothing at all — is not reading the SSO's tokens here.
    Refused {
        status: u16,
        challenge: Option<String>,
    },
    /// No answer, another status, or an answer that is not what the service
    /// says.
    Unreachable { detail: String },
}

impl std::fmt::Display for SideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { status, .. } => {
                write!(f, "the service refused the token with {status}")
            }
            Self::Unreachable { detail } => f.write_str(detail),
        }
    }
}

impl std::error::Error for SideError {}

/// How a request to a service proves who it is (#342).
///
/// One OIDC grant for the whole collector (ADR 0038) was true while a
/// deployment's mailbox and calendar belonged to one organisation's SSO.
/// It is not true of the reference deployment, whose calendar service
/// answers `WWW-Authenticate: Basic realm="ESN"` and sits behind a
/// different SSO from its mailbox's — so the credential belongs to the
/// connection, and a process holds one connection's worth of it.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    /// An access token from this connection's OIDC grant.
    Bearer(String),
    /// A username and a password the operator holds, for a service that
    /// challenges `Basic`. No SSO, so nothing to renew and no grant to
    /// reconnect: such a connection is `connected` or it is not.
    Basic { user: String, password: String },
}

impl Credential {
    /// The `Authorization` header this credential makes, for a place that
    /// builds its own request — the WebSocket handshake of #277's push,
    /// which is not a `reqwest` builder.
    pub fn header_value(&self) -> String {
        match self {
            Self::Bearer(token) => format!("Bearer {token}"),
            Self::Basic { user, password } => {
                format!("Basic {}", base64(format!("{user}:{password}").as_bytes()))
            }
        }
    }

    pub(crate) fn apply(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self {
            Self::Bearer(token) => request.bearer_auth(token),
            Self::Basic { user, password } => request.basic_auth(user, Some(password)),
        }
    }

    /// What a log may say about it: the scheme, and for `Basic` the user —
    /// which is not a secret and is what an operator needs to recognise
    /// the account. Never the token, never the password.
    pub fn described(&self) -> String {
        match self {
            Self::Bearer(_) => "a bearer from the connection's grant".to_owned(),
            Self::Basic { user, .. } => format!("basic as {user}"),
        }
    }
}

/// Redacted, deliberately: a credential that printed itself would end up
/// in a log the day something else went wrong.
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.described())
    }
}

/// A request that proves itself, for a caller building its own.
pub fn authorize(
    request: reqwest::RequestBuilder,
    credential: &Credential,
) -> reqwest::RequestBuilder {
    credential.apply(request)
}

/// Standard base64 (RFC 4648 §4), for the one header that is written by
/// hand rather than by `reqwest`. The collector already carries a
/// base64**url** encoder for PKCE (`oidc.rs`), which is a different
/// alphabet and no padding — near enough to be worth saying why they are
/// two functions.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut buffer = [0u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let triple = u32::from(buffer[0]) << 16 | u32::from(buffer[1]) << 8 | u32::from(buffer[2]);
        for position in 0..4 {
            if position <= chunk.len() {
                let index = (triple >> (18 - 6 * position)) & 0x3f;
                out.push(ALPHABET[index as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The HTTP client both services are asked through: one timeout, rustls.
pub fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

/// The owner as the events spell people: `mailto:`, lower-cased — whether
/// the caller passed the address or the URI. One identity for the mail and
/// the calendar side alike (ADR 0033).
pub fn owner_mailto(owner: &str) -> String {
    let address = owner.trim();
    let address = address
        .get(..7)
        .filter(|prefix| prefix.eq_ignore_ascii_case("mailto:"))
        .map(|_| &address[7..])
        .unwrap_or(address);
    format!("mailto:{}", address.trim().to_ascii_lowercase())
}

/// The host of a service URL, for `source` (`caldav://<host>/…`,
/// `jmap://<host>/…`).
pub fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Sends with the bearer and reads the status the way every service is read:
/// `401`/`403` is a refusal of the token, any other failure is unreachable.
pub async fn send(
    request: reqwest::RequestBuilder,
    credential: &Credential,
    service: &str,
) -> Result<reqwest::Response, SideError> {
    let response =
        credential
            .apply(request)
            .send()
            .await
            .map_err(|error| SideError::Unreachable {
                detail: format!("{service} did not answer: {error}"),
            })?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(SideError::Refused {
            status: status.as_u16(),
            challenge: response
                .headers()
                .get(reqwest::header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok())
                .map(|value| value.trim().to_owned()),
        });
    }
    if !status.is_success() {
        return Err(SideError::Unreachable {
            detail: format!("{service} answered {status}"),
        });
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_is_what_a_source_names() {
        assert_eq!(
            host_of("https://mail.example.com/jmap/session"),
            "mail.example.com"
        );
        assert_eq!(host_of("http://127.0.0.1:4321/"), "127.0.0.1:4321");
        assert_eq!(host_of("calendar.example.com"), "calendar.example.com");
        assert_eq!(
            owner_mailto("MAILTO:Michel@Example.com "),
            "mailto:michel@example.com"
        );
        assert_eq!(
            owner_mailto("michel@example.com"),
            "mailto:michel@example.com"
        );
    }
}
