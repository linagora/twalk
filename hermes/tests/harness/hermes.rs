//! A stub of **Hermes's webhook route** — and of nothing deeper (ticket
//! #206).
//!
//! ADR 0032 pins Hermes to a release tag and treats the webhook route's shape
//! as the entire contract, because the runtime ships over a thousand commits
//! between three-day releases and anything deeper breaks on a Tuesday. This
//! file is that contract, transcribed from the adapter's own source and its
//! own documentation
//! (`~/.hermes/hermes-agent/website/docs/user-guide/messaging/webhooks.md`,
//! `gateway/platforms/webhook.py`), and it is the whole of what this suite
//! asks a persona to speak:
//!
//! - `GET /health` answers `{"status": "ok", "platform": "webhook"}`, which is
//!   how a persona can say "Hermes is not there" at startup rather than
//!   discovering it on the first message;
//! - `POST /webhooks/<route>` validates the **generic V2** signature —
//!   `X-Webhook-Signature-V2` is the hex HMAC-SHA256 of `<timestamp>.<body>`
//!   and `X-Webhook-Timestamp` must be within ±300 seconds — and answers
//!   `202 {"status": "accepted", …}`. The adapter is **asynchronous by
//!   construction**: it returns before the agent runs, so nothing a persona
//!   does can wait for an answer, which is exactly why the answer comes back
//!   through the Companion Gateway;
//! - a repeated `X-Request-ID` inside the idempotency window answers
//!   `200 {"status": "duplicate"}` and runs nothing;
//! - a missing or wrong signature is a `401`.
//!
//! What it deliberately does **not** do is reason, remember, deliver, or speak
//! ACP — none of which crosses this seam.
//!
//! It records every accepted wake, so a test can assert the body **field by
//! field, including what is absent**, against the bytes that really went over
//! a socket.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The route this stub serves, and the one the persona is pointed at.
pub const ROUTE: &str = "twalk-messages";

/// The secret the route is configured with, on both sides. Hermes refuses a
/// route that has none, and its `INSECURE_NO_AUTH` escape hatch is for its own
/// tests and never for Twalk's configuration — so this suite uses a real one.
pub const WEBHOOK_SECRET: &str = "a-throwaway-webhook-secret-for-the-test-stack-only";

/// The ports a stub Hermes may bind, claimed one at a time so that several
/// tests — and several worktrees — can run at once. This ticket's own range.
const PORT_RANGE: std::ops::Range<u16> = 18500..18698;

/// The last port of the range, bound by no stub: the address of a Hermes that
/// is not there. Reserved rather than taken from a stub that has been dropped,
/// because a port a parallel test could claim between the drop and the read
/// would make "unreachable" mean "answered somebody else's route".
pub const UNREACHABLE_HERMES_URL: &str = "http://127.0.0.1:18699/webhooks/twalk-messages";

/// The adapter's replay window, from its own source: `abs(now - ts) > 300`.
const CLOCK_SKEW_SECONDS: i64 = 300;

/// One wake, as it arrived.
#[derive(Debug, Clone)]
pub struct Wake {
    /// The body exactly as the bytes on the wire, before any parsing. What an
    /// absence assertion has to search.
    pub raw: String,
    pub body: Value,
    /// `X-Request-ID`: Hermes's idempotency key, which the persona sets to the
    /// deterministic id of the suggestion this wake will produce.
    pub delivery_id: String,
    /// Whether this delivery was absorbed as a duplicate rather than run.
    pub duplicate: bool,
}

#[derive(Debug, Default)]
struct State {
    wakes: Vec<Wake>,
    refusals: usize,
    health_checks: usize,
    seen: HashMap<String, ()>,
}

/// A running stub Hermes. Dropping it stops the server.
pub struct StubHermes {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl StubHermes {
    pub async fn start() -> Result<Self> {
        let listener = bind_in_range().await?;
        let addr = listener
            .local_addr()
            .context("the stub Hermes listener has no local address")?;
        let state = Arc::new(Mutex::new(State::default()));
        let accept_task = tokio::spawn(accept_loop(listener, state.clone()));
        Ok(Self {
            addr,
            state,
            accept_task,
        })
    }

    /// The URL of the route, as a persona is configured with it.
    pub fn route_url(&self) -> String {
        format!("http://{}/webhooks/{ROUTE}", self.addr)
    }

    /// Every wake this route accepted, oldest first.
    pub fn wakes(&self) -> Vec<Wake> {
        self.lock().wakes.clone()
    }

    /// How many requests were refused — a wrong signature, a stale timestamp,
    /// an unknown route. A test that asserts a persona sent *nothing* asserts
    /// this is zero as well as [`Self::wakes`] being empty, so that a wake
    /// refused at the door is not mistaken for a wake never sent.
    pub fn refusals(&self) -> usize {
        self.lock().refusals
    }

    /// How many times the persona asked whether Hermes is there.
    pub fn health_checks(&self) -> usize {
        self.lock().health_checks
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("the stub Hermes mutex is never poisoned")
    }
}

impl Drop for StubHermes {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

async fn bind_in_range() -> Result<TcpListener> {
    for port in PORT_RANGE {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        if let Ok(listener) = TcpListener::bind(addr).await {
            return Ok(listener);
        }
    }
    anyhow::bail!(
        "no free port in {}..{} for a stub Hermes",
        PORT_RANGE.start,
        PORT_RANGE.end
    )
}

async fn accept_loop(listener: TcpListener, state: Arc<Mutex<State>>) {
    while let Ok((stream, _)) = listener.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            let _ = serve_connection(stream, state).await;
        });
    }
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

async fn serve_connection(mut stream: TcpStream, state: Arc<Mutex<State>>) -> Result<()> {
    let Some(request) = read_request(&mut stream).await? else {
        return Ok(());
    };
    let (status, body) = respond(&request, &state);
    let body = serde_json::to_vec(&body)?;
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_request(stream: &mut TcpStream) -> Result<Option<Request>> {
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();

    let length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Ok(Some(Request {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }))
}

fn respond(request: &Request, state: &Arc<Mutex<State>>) -> (&'static str, Value) {
    let path = request.path.split('?').next().unwrap_or_default();
    if path == "/health" {
        state
            .lock()
            .expect("the stub Hermes mutex is never poisoned")
            .health_checks += 1;
        return ("200 OK", json!({ "status": "ok", "platform": "webhook" }));
    }
    if request.method != "POST" || path != format!("/webhooks/{ROUTE}") {
        let mut state = state
            .lock()
            .expect("the stub Hermes mutex is never poisoned");
        state.refusals += 1;
        return (
            "404 Not Found",
            json!({ "error": format!("no route at {path}") }),
        );
    }
    if !authenticates(request) {
        let mut state = state
            .lock()
            .expect("the stub Hermes mutex is never poisoned");
        state.refusals += 1;
        return ("401 Unauthorized", json!({ "error": "invalid signature" }));
    }
    let delivery_id = request
        .headers
        .get("x-request-id")
        .or_else(|| request.headers.get("x-github-delivery"))
        .cloned()
        .unwrap_or_default();
    let body: Value = serde_json::from_str(&request.body).unwrap_or(Value::Null);
    let event = body
        .get("event_type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let mut state = state
        .lock()
        .expect("the stub Hermes mutex is never poisoned");
    let duplicate = !delivery_id.is_empty() && state.seen.contains_key(&delivery_id);
    if !duplicate && !delivery_id.is_empty() {
        state.seen.insert(delivery_id.clone(), ());
    }
    state.wakes.push(Wake {
        raw: request.body.clone(),
        body,
        delivery_id: delivery_id.clone(),
        duplicate,
    });
    if duplicate {
        // The adapter's own answer for a retry inside the idempotency TTL: a
        // `200`, no agent run, and the caller told which it was.
        return (
            "200 OK",
            json!({ "status": "duplicate", "route": ROUTE, "delivery_id": delivery_id }),
        );
    }
    // `202 Accepted`, before the agent has run. The seam is asynchronous by
    // construction, which is the whole reason the answer comes back through
    // the Companion Gateway and not through this response.
    (
        "202 Accepted",
        json!({
            "status": "accepted",
            "route": ROUTE,
            "event": event,
            "delivery_id": delivery_id,
        }),
    )
}

/// Hermes's generic V2 check, transcribed: the hex HMAC-SHA256 of
/// `<timestamp>.<body>`, with the timestamp within ±300 seconds.
fn authenticates(request: &Request) -> bool {
    let Some(signature) = request.headers.get("x-webhook-signature-v2") else {
        return false;
    };
    let Some(timestamp) = request.headers.get("x-webhook-timestamp") else {
        return false;
    };
    let Ok(sent) = timestamp.parse::<i64>() else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default();
    if (now - sent).abs() > CLOCK_SKEW_SECONDS {
        return false;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(WEBHOOK_SECRET.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(request.body.as_bytes());
    let expected = format!("{:x}", mac.finalize().into_bytes());
    // Constant time is not the point in a stub; being exactly the adapter's
    // arithmetic is.
    signature.eq_ignore_ascii_case(&expected)
}
