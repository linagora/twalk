//! What the two services the grant opens have in common on the wire (#276,
//! #280): a bearer, a host for `source`, and the two ways a request fails
//! — the token refused, which is the connection's state to say, and
//! everything else, which is retried next round. `calendars.rs` and
//! `mails.rs` each keep the requests that are theirs.

use anyhow::Result;

/// Why a request to a service did not answer with what was asked.
#[derive(Debug)]
pub enum SideError {
    /// `401`/`403`: the token itself, or what the client lacks — the words
    /// the status machinery already has (`ServiceRefusal`).
    Refused { status: u16 },
    /// No answer, another status, or an answer that is not what the service
    /// says.
    Unreachable { detail: String },
}

impl std::fmt::Display for SideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { status } => write!(f, "the service refused the token with {status}"),
            Self::Unreachable { detail } => f.write_str(detail),
        }
    }
}

impl std::error::Error for SideError {}

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
    token: &str,
    service: &str,
) -> Result<reqwest::Response, SideError> {
    let response =
        request
            .bearer_auth(token)
            .send()
            .await
            .map_err(|error| SideError::Unreachable {
                detail: format!("{service} did not answer: {error}"),
            })?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(SideError::Refused {
            status: status.as_u16(),
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
