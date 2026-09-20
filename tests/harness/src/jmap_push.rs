//! The fake JMAP server's push endpoint (issue #277, RFC 8887): a WebSocket
//! on a listener of its own beside the fake SSO's, opened with the ticket
//! TMail hands out (`com:linagora:params:jmap:ws:ticket`) rather than with
//! the bearer — the shape a browser needs and the one the collector uses
//! when the session offers it — and pushing one `StateChange` per delivery
//! to every client that enabled push for `Email`.
//!
//! A test can cut the socket ([`crate::FakeSso::cut_push`]): every client is
//! closed and the listener refuses new connections until
//! [`crate::FakeSso::restore_push`], which is what "with the socket cut, the
//! next poll catches it" is proved against.

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

/// What the fake's WebSocket half shares with the HTTP half.
pub(crate) struct PushState {
    /// Tickets handed out at the ticket endpoint, each good for one
    /// connection.
    pub(crate) tickets: HashSet<String>,
    /// Whether the socket accepts connections and keeps them open.
    pub(crate) up: bool,
    /// How many `StateChange`s were pushed, for a test's assertion.
    pub(crate) pushed: u64,
}

impl Default for PushState {
    fn default() -> Self {
        Self {
            tickets: HashSet::new(),
            up: true,
            pushed: 0,
        }
    }
}

/// The push listener: its address, the channel deliveries are announced on,
/// and its state.
pub(crate) struct Push {
    pub(crate) addr: SocketAddr,
    pub(crate) changes: broadcast::Sender<String>,
    pub(crate) state: Arc<Mutex<PushState>>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl Drop for Push {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

impl Push {
    pub(crate) async fn start() -> Result<Self> {
        let listener = TcpListener::bind(format!("{}:0", Ipv4Addr::LOCALHOST))
            .await
            .context("failed to bind the fake JMAP push listener")?;
        let addr = listener.local_addr()?;
        let (changes, _) = broadcast::channel(64);
        let state = Arc::new(Mutex::new(PushState::default()));
        let accept_task = tokio::spawn(accept_loop(listener, state.clone(), changes.clone()));
        Ok(Self {
            addr,
            changes,
            state,
            accept_task,
        })
    }

    /// The URL the session names under the websocket capability.
    pub(crate) fn url(&self) -> String {
        format!("ws://{}/jmap/ws", self.addr)
    }
}

/// A ticket for one connection, as the ticket endpoint hands it out.
pub(crate) fn mint_ticket(state: &Arc<Mutex<PushState>>) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let ticket = format!("ticket-{nanos:x}");
    state
        .lock()
        .expect("the push state is not poisoned")
        .tickets
        .insert(ticket.clone());
    ticket
}

async fn accept_loop(
    listener: TcpListener,
    state: Arc<Mutex<PushState>>,
    changes: broadcast::Sender<String>,
) {
    while let Ok((stream, _)) = listener.accept().await {
        let state = state.clone();
        let changes = changes.subscribe();
        tokio::spawn(async move {
            let _ = serve(stream, state, changes).await;
        });
    }
}

/// One client: the handshake checked for a live ticket and the `jmap`
/// subprotocol, then `StateChange`s until the client leaves or the socket
/// is cut.
async fn serve(
    stream: TcpStream,
    state: Arc<Mutex<PushState>>,
    mut changes: broadcast::Receiver<String>,
) -> Result<()> {
    let checked_state = state.clone();
    let callback =
        move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
              mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            let ticket = request
                .uri()
                .query()
                .and_then(|query| {
                    query
                        .split('&')
                        .find_map(|pair| pair.strip_prefix("ticket="))
                })
                .map(str::to_owned);
            let mut guard = checked_state
                .lock()
                .expect("the push state is not poisoned");
            let admitted = guard.up
                && ticket
                    .as_deref()
                    .is_some_and(|ticket| guard.tickets.remove(ticket));
            drop(guard);
            if !admitted {
                let mut refusal =
                    tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(Some(
                        "no live ticket".to_owned(),
                    ));
                *refusal.status_mut() =
                    tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED;
                return Err(refusal);
            }
            // RFC 8887 §3: the `jmap` subprotocol, echoed.
            response.headers_mut().insert(
                "sec-websocket-protocol",
                tokio_tungstenite::tungstenite::http::HeaderValue::from_static("jmap"),
            );
            Ok(response)
        };
    let mut socket = tokio_tungstenite::accept_hdr_async(stream, callback)
        .await
        .context("the WebSocket handshake failed")?;
    let mut enabled = false;
    loop {
        tokio::select! {
            message = socket.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let request: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                        if request["@type"] == "WebSocketPushEnable" {
                            enabled = request["dataTypes"]
                                .as_array()
                                .is_none_or(|types| types.iter().any(|t| t == "Email"));
                        } else if request["@type"] == "WebSocketPushDisable" {
                            enabled = false;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = socket.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return Ok(()),
                    _ => {}
                }
            }
            changed = changes.recv() => {
                let Ok(new_state) = changed else { return Ok(()) };
                let up = state.lock().expect("the push state is not poisoned").up;
                if !up {
                    let _ = socket.close(None).await;
                    return Ok(());
                }
                if !enabled {
                    continue;
                }
                let push = json!({
                    "@type": "StateChange",
                    "changed": { crate::jmap_fake::ACCOUNT_ID: { "Email": new_state } }
                });
                if socket.send(Message::Text(push.to_string().into())).await.is_err() {
                    return Ok(());
                }
                state.lock().expect("the push state is not poisoned").pushed += 1;
            }
        }
    }
}
