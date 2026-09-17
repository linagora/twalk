//! Resolving a Matrix OpenID token to the Matrix ID that minted it.
//!
//! The Companion asks the user's homeserver for an OpenID token
//! (`POST /_matrix/client/v3/user/{userId}/openid/request_token`) and posts it
//! here; the Gateway hands it back to that homeserver at
//! `GET /_matrix/federation/v1/openid/userinfo?access_token=…`, which answers
//! `{"sub": "@user:server"}`. That endpoint is the unauthenticated corner of
//! the federation API — no federation identity, no signing keys, just
//! outbound HTTP — which is what makes the whole of ADR 0011 cheap: the
//! Gateway learns who the user is without ever holding a Matrix access token
//! for them.
//!
//! Two obligations come with it, and both are met here:
//!
//! - **The domain of `sub` is checked against the server that was asked.**
//!   The specification requires it, and for a good reason: a homeserver that
//!   answered `{"sub": "@owner:example.com"}` for a token of its own would
//!   otherwise let an unrelated server speak for `example.com`'s users.
//! - **The token is not a nonce.** Synapse's verification is a pure lookup
//!   with no delete, so the same token answers for its whole lifetime (an
//!   hour, by Synapse's default). Replay protection is the Gateway's own job
//!   and lives in [`crate::session`], not here.
//!
//! The federation base URL is pinned in configuration rather than resolved
//! from the server name. A deployment serves exactly one homeserver, which
//! the operator already configured; implementing the full server-name
//! resolution algorithm (`/.well-known/matrix/server`, SRV records, the
//! port-8448 fallback) would be a federation stack in a service that does
//! not federate. The domain check stays regardless: pinning decides *which
//! server is asked*, never *whose answer is believed*.
//!
//! One rule runs through this module: the token never reaches a log line, an
//! error message or a stored row. It travels in a query string, so even
//! `reqwest`'s own error messages — which quote the URL — are stripped with
//! [`reqwest::Error::without_url`] before they are formatted anywhere.

use std::time::Duration;

use serde::Deserialize;

/// How long the Gateway waits for the homeserver's userinfo answer. A local
/// homeserver answers in milliseconds; a sign-in that has not resolved in
/// this long is better refused than left hanging on the user's screen.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(10);

/// The OpenID token as the Companion received it from the homeserver and
/// forwarded to the Gateway. `matrix_server_name` is the homeserver's own
/// statement of which server minted the token; it is checked, not trusted.
#[derive(Debug, Clone, Deserialize)]
pub struct OpenIdToken {
    pub access_token: String,
    /// Present in every Synapse answer, optional here because it is the
    /// client's to forward: when it is absent the Gateway simply asks its own
    /// homeserver, which is the only one it would ever ask anyway.
    #[serde(default)]
    pub matrix_server_name: Option<String>,
}

/// Why a token did not resolve to an identity. No variant carries the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The token says it was minted by another homeserver. Refused before any
    /// outbound call: this Gateway serves one homeserver.
    ForeignHomeserver { claimed: String },
    /// The homeserver does not know this token: never minted, already
    /// expired, or simply wrong.
    TokenRejected,
    /// The homeserver's answer named a user of another domain than the server
    /// that was asked — the check the specification requires.
    ForeignSubject { subject: String },
    /// The answer was not a Matrix ID at all.
    MalformedSubject { subject: String },
    /// The homeserver could not be reached, or answered something that is not
    /// a userinfo document. An operator problem, not the user's.
    Unverifiable { detail: String },
}

impl Refusal {
    /// A short, stable label for logs and the metrics outcome. Never the
    /// token, never a secret.
    pub fn label(&self) -> &'static str {
        match self {
            Refusal::ForeignHomeserver { .. } => "foreign_homeserver",
            Refusal::TokenRejected => "token_rejected",
            Refusal::ForeignSubject { .. } => "foreign_subject",
            Refusal::MalformedSubject { .. } => "malformed_subject",
            Refusal::Unverifiable { .. } => "unverifiable",
        }
    }
}

/// The homeserver this Gateway verifies tokens at.
#[derive(Debug, Clone)]
pub struct Verifier {
    /// Base URL of the homeserver's federation API, e.g.
    /// `http://synapse:8008` — pinned in configuration (see the module
    /// documentation), without a trailing slash.
    federation_base_url: String,
    /// The Matrix server name this deployment serves, e.g. `example.com`.
    /// Both the token's claimed server and the domain of the answered Matrix
    /// ID must equal it.
    server_name: String,
    http: reqwest::Client,
}

impl Verifier {
    pub fn new(federation_base_url: &str, server_name: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(VERIFY_TIMEOUT)
            .build()
            .map_err(|error| anyhow::anyhow!("{}", error.without_url()))?;
        Ok(Self {
            federation_base_url: federation_base_url.trim_end_matches('/').to_owned(),
            server_name: server_name.to_owned(),
            http,
        })
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// The Matrix ID that minted this token, as the homeserver says it and
    /// after the domain check the specification requires.
    pub async fn resolve(&self, token: &OpenIdToken) -> Result<String, Refusal> {
        if let Some(claimed) = &token.matrix_server_name {
            if claimed != &self.server_name {
                return Err(Refusal::ForeignHomeserver {
                    claimed: claimed.clone(),
                });
            }
        }
        let subject = self.userinfo(&token.access_token).await?;
        match domain_of(&subject) {
            None => Err(Refusal::MalformedSubject { subject }),
            Some(domain) if domain != self.server_name => Err(Refusal::ForeignSubject { subject }),
            Some(_) => Ok(subject),
        }
    }

    /// The raw userinfo call. The token travels as a query parameter, so
    /// every error out of here is stripped of its URL.
    async fn userinfo(&self, access_token: &str) -> Result<String, Refusal> {
        let url = format!(
            "{}/_matrix/federation/v1/openid/userinfo",
            self.federation_base_url
        );
        let response = self
            .http
            .get(&url)
            .query(&[("access_token", access_token)])
            .send()
            .await
            .map_err(|error| Refusal::Unverifiable {
                detail: error.without_url().to_string(),
            })?;
        let status = response.status();
        // 401 is the homeserver saying it does not know the token — the
        // user's problem (an expired token, a retried sign-in), not the
        // operator's. Anything else is the operator's.
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Refusal::TokenRejected);
        }
        if !status.is_success() {
            return Err(Refusal::Unverifiable {
                detail: format!("the homeserver answered {status} on the userinfo endpoint"),
            });
        }
        #[derive(Deserialize)]
        struct UserInfo {
            sub: String,
        }
        let userinfo: UserInfo = response
            .json()
            .await
            .map_err(|error| Refusal::Unverifiable {
                detail: format!(
                    "the homeserver's userinfo answer is not a userinfo document: {}",
                    error.without_url()
                ),
            })?;
        Ok(userinfo.sub)
    }
}

/// The server-name part of a Matrix ID, or `None` when the string is not one:
/// a Matrix ID is `@localpart:server_name`, with a non-empty localpart and a
/// non-empty server name. The server name may itself carry a port
/// (`@user:example.com:8448`), so the split is on the *first* colon.
pub fn domain_of(user_id: &str) -> Option<&str> {
    let rest = user_id.strip_prefix('@')?;
    let (localpart, domain) = rest.split_once(':')?;
    if localpart.is_empty() || domain.is_empty() {
        return None;
    }
    Some(domain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_matrix_id_yields_its_server_name() {
        assert_eq!(domain_of("@alice:example.com"), Some("example.com"));
        // A server name may carry a port: the split is on the first colon.
        assert_eq!(
            domain_of("@alice:example.com:8448"),
            Some("example.com:8448")
        );
        // Bridged puppets are ordinary Matrix IDs.
        assert_eq!(
            domain_of("@whatsapp_33612345678:example.com"),
            Some("example.com")
        );
    }

    #[test]
    fn what_is_not_a_matrix_id_has_no_domain() {
        for not_an_id in [
            "",
            "alice:example.com",
            "@alice",
            "@:example.com",
            "@alice:",
            "example.com",
            "@@:",
        ] {
            assert_eq!(domain_of(not_an_id), None, "{not_an_id:?}");
        }
    }

    #[tokio::test]
    async fn a_token_minted_by_another_homeserver_is_refused_without_a_call() {
        // The federation URL is deliberately unroutable: reaching it at all
        // would fail the test with Unverifiable instead.
        let verifier = Verifier::new("http://127.0.0.1:1", "example.com").expect("a verifier");
        let refused = verifier
            .resolve(&OpenIdToken {
                access_token: "irrelevant".to_owned(),
                matrix_server_name: Some("evil.example".to_owned()),
            })
            .await
            .expect_err("a token from another homeserver is refused");
        assert_eq!(
            refused,
            Refusal::ForeignHomeserver {
                claimed: "evil.example".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn an_unreachable_homeserver_is_an_operator_problem_and_leaks_no_token() {
        let verifier = Verifier::new("http://127.0.0.1:1", "example.com").expect("a verifier");
        let refused = verifier
            .resolve(&OpenIdToken {
                access_token: "syt_a_secret_looking_token".to_owned(),
                matrix_server_name: None,
            })
            .await
            .expect_err("an unreachable homeserver refuses the sign-in");
        let Refusal::Unverifiable { detail } = &refused else {
            panic!("expected Unverifiable, got {refused:?}");
        };
        assert!(
            !detail.contains("syt_a_secret_looking_token"),
            "a refusal must not quote the token: {detail}"
        );
    }
}
