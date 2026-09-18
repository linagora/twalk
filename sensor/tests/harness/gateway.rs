//! A stub Companion Gateway serving `GET /api/consent/snapshot`.
//!
//! The Sensor's seam with the Gateway is one HTTP read (ADR 0010, ticket
//! #51): the whole consent state, and the stream sequence it reflects. What
//! the Sensor's tests need from the other side of that seam is control over
//! both halves — a state the test decided, at a position the test knows,
//! because the position is what the hand-off is made of. The real Gateway
//! reaches that state through sign-in, device tokens and its own journal;
//! putting all of that in the Sensor's suite would test the Gateway, and it
//! is tested there (`companion-gateway/tests/consent_snapshot.rs`).
//!
//! So this stub answers the documented document, checks the documented
//! credential, and lets a test set both. It runs in the test process on an
//! ephemeral loopback port — no container, and parallel suites never collide
//! — with HTTP/1.1 hand-rolled over tokio, like the shared harness's stub LLM
//! and the Sensor's own metrics endpoint.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The bus the stub's sequences belong to, as the real Gateway names them.
const STREAM: &str = "twalk";
const CONSENT_SUBJECT: &str = "twalk.consent.state.changed.v1";

#[derive(Default)]
struct State {
    entries: Vec<Value>,
    /// The stream sequence the served state reflects; the document's
    /// `next_stream_sequence` is this plus one.
    stream_sequence: u64,
    /// Every request's `Authorization` header, in order — a test asserts the
    /// Sensor presented the service token, and counts the retries.
    requests: Vec<Option<String>>,
}

/// A running stub Gateway. Dropping it stops the server.
pub struct StubGateway {
    addr: SocketAddr,
    service_token: String,
    state: Arc<Mutex<State>>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl StubGateway {
    /// Starts the stub on an ephemeral loopback port, accepting
    /// `service_token` as the bearer credential, with an empty consent state
    /// at the beginning of the stream.
    pub async fn start(service_token: &str) -> Result<Self> {
        Self::start_on(&format!("{}:0", Ipv4Addr::LOCALHOST), service_token).await
    }

    /// [`start`](Self::start) at an address the caller chose: a test that
    /// points the Sensor at a Gateway which is *not there yet*, and then
    /// brings one up at that same address, needs to name it in advance.
    pub async fn start_on(addr: &str, service_token: &str) -> Result<Self> {
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("failed to bind the stub Companion Gateway on {addr}"))?;
        let addr = listener
            .local_addr()
            .context("the stub Companion Gateway listener has no local address")?;
        let state = Arc::new(Mutex::new(State::default()));
        let accept_task = tokio::spawn(accept_loop(
            listener,
            state.clone(),
            service_token.to_owned(),
        ));
        Ok(Self {
            addr,
            service_token: service_token.to_owned(),
            state,
            accept_task,
        })
    }

    /// The origin, as `SENSOR_GATEWAY_URL` takes it.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn service_token(&self) -> &str {
        &self.service_token
    }

    /// Serves this state from now on: the entries, and the stream sequence
    /// they reflect. The Sensor will follow the stream from `stream_sequence`
    /// **plus one**, so pass the sequence of the last decision the entries
    /// account for.
    pub fn serve(&self, entries: Vec<Value>, stream_sequence: u64) {
        let mut state = self.lock();
        state.entries = entries;
        state.stream_sequence = stream_sequence;
    }

    /// The `Authorization` header of every snapshot request, oldest first.
    pub fn requests(&self) -> Vec<Option<String>> {
        self.lock().requests.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("the stub Gateway mutex is never poisoned")
    }
}

impl Drop for StubGateway {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

/// One `ConsentStateEntry` about a contact, as `companion-gateway/
/// openapi.yaml` shapes it.
pub fn contact_entry(subject_id: &str, network: &str, state: &str) -> Value {
    entry(
        json!({ "type": "contact", "id": subject_id }),
        network,
        state,
    )
}

/// One `ConsentStateEntry` holding a network's default.
pub fn network_entry(network: &str, state: &str) -> Value {
    entry(json!({ "type": "network", "id": network }), network, state)
}

fn entry(subject: Value, network: &str, state: &str) -> Value {
    json!({
        "subject": subject,
        "network": network,
        "state": state,
        "decided_at": "2026-09-17T10:00:00.000Z",
        "decision_sequence": 1,
    })
}

async fn accept_loop(listener: TcpListener, state: Arc<Mutex<State>>, service_token: String) {
    while let Ok((stream, _)) = listener.accept().await {
        let state = state.clone();
        let service_token = service_token.clone();
        tokio::spawn(async move {
            // A failed connection is the test's problem to observe through
            // the Sensor's own behaviour, not something the stub can report.
            let _ = serve_connection(stream, state, service_token).await;
        });
    }
}

/// Serves exactly one request, then closes the connection.
async fn serve_connection(
    mut stream: TcpStream,
    state: Arc<Mutex<State>>,
    service_token: String,
) -> Result<()> {
    let Some(request) = read_request(&mut stream).await? else {
        return Ok(());
    };
    let (status, body) = respond(&request, &state, &service_token);
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

struct RawRequest {
    method: String,
    path: String,
    authorization: Option<String>,
}

/// Reads one HTTP/1.1 request head; the snapshot route has no body.
async fn read_request(stream: &mut TcpStream) -> Result<Option<RawRequest>> {
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
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
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_owned();
    let path = request_line.next().unwrap_or_default().to_owned();
    let mut authorization = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_owned());
            }
        }
    }
    Ok(Some(RawRequest {
        method,
        path,
        authorization,
    }))
}

/// Routes one request. The credential check is the real one's: a service
/// token as `Authorization: Bearer`, and nothing else — the Sensor holds no
/// device token and must never be able to read this with one.
fn respond(
    request: &RawRequest,
    state: &Arc<Mutex<State>>,
    service_token: &str,
) -> (&'static str, Value) {
    let path = request.path.split('?').next().unwrap_or_default();
    if path != "/api/consent/snapshot" {
        return (
            "404 Not Found",
            error_body(
                "not_found",
                "the stub Companion Gateway serves /api/consent/snapshot only",
            ),
        );
    }
    if request.method != "GET" {
        return (
            "405 Method Not Allowed",
            error_body("not_found", "the consent snapshot is a GET"),
        );
    }
    let presented = request
        .authorization
        .as_deref()
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, credential)| credential.trim());
    let mut state = state
        .lock()
        .expect("the stub Gateway mutex is never poisoned");
    state.requests.push(request.authorization.clone());
    if presented != Some(service_token) {
        return (
            "401 Unauthorized",
            error_body(
                "unauthenticated",
                "the consent snapshot takes this Gateway's service token as an \
                 Authorization: Bearer credential",
            ),
        );
    }
    (
        "200 OK",
        json!({
            "stream": STREAM,
            "subject": CONSENT_SUBJECT,
            "stream_sequence": state.stream_sequence,
            "next_stream_sequence": state.stream_sequence + 1,
            "decision_sequence": state.entries.len(),
            "entries": state.entries,
        }),
    )
}

/// The Gateway's one error document (`openapi.yaml`'s `Error`).
fn error_body(code: &str, detail: &str) -> Value {
    json!({ "error": code, "detail": detail })
}
