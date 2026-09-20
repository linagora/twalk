//! Push (issue #277, RFC 8887): the mail server tells the collector an
//! Email state changed, and the collector reads the changes then — a mail
//! reaches the bus in seconds rather than at the next poll. The poll stays,
//! as the fallback: a socket that is down, or a server offering no push,
//! is what `COLLECTOR_MAIL_POLL_SECONDS` was for all along.
//!
//! The socket is opened the way a browser opens TMail's — with a ticket from
//! `com:linagora:params:jmap:ws:ticket`, one per socket — when the session
//! offers the endpoint, and with the bearer on the handshake otherwise
//! (RFC 8887 §3). The client asks for `Email` state changes
//! (`WebSocketPushEnable`) and reads `StateChange`s; every one for the
//! account wakes the run loop's mail poll, which does what it always does:
//! `Email/changes` from the persisted state. The socket carries no mail and
//! no request — it is a doorbell.
//!
//! A socket that closes is said and reopened with a growing delay, and the
//! poll is rung once it is back, for what arrived meanwhile; a session that
//! offers no push is said once and asked again later.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::jmap::{PushEndpoint, Session};
use crate::metrics::Metrics;
use crate::oidc::AccessToken;
use crate::replies::SharedAccess;

/// The longest wait between two attempts to open the socket.
const MAX_RECONNECT: Duration = Duration::from_secs(60);
/// How long to wait before asking a server that offered no push again.
const NO_PUSH_RECHECK: Duration = Duration::from_secs(300);

/// Never returns: keeps a socket open to the server's push endpoint for as
/// long as the process runs, ringing `wake` on every `StateChange` for the
/// account.
pub async fn listen(
    session_url: String,
    access: SharedAccess,
    wake: Arc<Notify>,
    metrics: Arc<Metrics>,
) {
    let http = match crate::side::client() {
        Ok(http) => http,
        Err(error) => {
            warn!(%error, "no HTTP client for push; polling only");
            return;
        }
    };
    let mut failures: u32 = 0;
    let mut said_no_push = false;
    let mut opened_before = false;
    loop {
        let Some(token) = access.read().await.clone() else {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        let session = match session(&http, &session_url, &token).await {
            Ok(session) => session,
            Err(error) => {
                failures += 1;
                warn!(%error, "the JMAP session could not be read for push; polling meanwhile");
                tokio::time::sleep(backoff(failures)).await;
                continue;
            }
        };
        let Some(endpoint) = &session.push else {
            if !said_no_push {
                info!("the JMAP server offers no push: the mailbox is polled on COLLECTOR_MAIL_POLL_SECONDS");
                said_no_push = true;
            }
            metrics.set_push_connected(false);
            tokio::time::sleep(NO_PUSH_RECHECK).await;
            continue;
        };
        match subscribe(
            &http,
            endpoint,
            &token,
            &session.account_id,
            &wake,
            &metrics,
            opened_before,
        )
        .await
        {
            Ok(()) => {
                failures = 0;
                opened_before = true;
                warn!("the push socket closed; polling until it is back");
            }
            Err(error) => {
                failures += 1;
                warn!(%error, attempt = failures, "the push socket could not be opened; polling until it is");
            }
        }
        metrics.set_push_connected(false);
        tokio::time::sleep(backoff(failures)).await;
    }
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs(2u64.saturating_pow(failures.min(6))).min(MAX_RECONNECT)
}

async fn session(http: &reqwest::Client, url: &str, token: &AccessToken) -> Result<Session> {
    let document: Value = crate::side::send(
        http.get(url).header("accept", "application/json"),
        &token.token,
        "jmap",
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}"))?
    .json()
    .await
    .context("the session is not JSON")?;
    Session::parse(&document)
}

/// One socket: opened, push enabled, read until it closes. `Ok(())` is a
/// socket that closed after opening; `Err` one that never opened. A socket
/// reopened after one closed rings the poll at once: no `pushState` is
/// asked for on the handshake, so what the server would have said while
/// the socket was down is read by the poll it rings instead.
async fn subscribe(
    http: &reqwest::Client,
    endpoint: &PushEndpoint,
    token: &AccessToken,
    account: &str,
    wake: &Notify,
    metrics: &Metrics,
    reopened: bool,
) -> Result<()> {
    let mut request =
        tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
            endpoint.websocket_url.as_str(),
        )
        .context("the push URL is not a WebSocket URL")?;
    request.headers_mut().insert(
        "sec-websocket-protocol",
        tokio_tungstenite::tungstenite::http::HeaderValue::from_static("jmap"),
    );
    match &endpoint.ticket_url {
        Some(ticket_url) => {
            // TMail's ticket: one POST with the bearer, one ticket, one socket.
            let ticket: Value = crate::side::send(http.post(ticket_url), &token.token, "jmap")
                .await
                .map_err(|error| anyhow::anyhow!("the ticket endpoint refused: {error}"))?
                .json()
                .await
                .context("the ticket is not JSON")?;
            let value = ticket
                .get("value")
                .and_then(Value::as_str)
                .context("the ticket endpoint answered no ticket")?;
            let separator = if endpoint.websocket_url.contains('?') {
                '&'
            } else {
                '?'
            };
            let url = format!("{}{separator}ticket={value}", endpoint.websocket_url);
            *request.uri_mut() = url.parse().context("the ticketed URL is not a URI")?;
        }
        None => {
            request.headers_mut().insert(
                "authorization",
                tokio_tungstenite::tungstenite::http::HeaderValue::from_str(&format!(
                    "Bearer {}",
                    token.token
                ))
                .context("the token is not a header value")?,
            );
        }
    }
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .context("the WebSocket handshake failed")?;
    socket
        .send(Message::Text(
            json!({ "@type": "WebSocketPushEnable", "dataTypes": ["Email"] })
                .to_string()
                .into(),
        ))
        .await
        .context("push could not be enabled")?;
    metrics.set_push_connected(true);
    info!(url = %endpoint.websocket_url, "push is on: a delivery wakes the mail poll");
    if reopened {
        debug!("the push socket is back; reading what arrived while it was down");
        wake.notify_one();
    }
    while let Some(message) = socket.next().await {
        match message {
            Ok(Message::Text(text)) => {
                let change: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                if change["@type"] == "StateChange"
                    && change["changed"][account].get("Email").is_some()
                {
                    debug!("the server says the Email state changed; waking the mail poll");
                    metrics.record_push_wake();
                    wake.notify_one();
                }
            }
            Ok(Message::Ping(payload)) => {
                if socket.send(Message::Pong(payload)).await.is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(error) => {
                warn!(%error, "the push socket errored");
                break;
            }
        }
    }
    Ok(())
}
