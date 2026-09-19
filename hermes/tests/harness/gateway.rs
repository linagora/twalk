//! A stub Companion Gateway serving `GET /api/settings/runtime` (ticket
//! #184).
//!
//! The Hermes suite's seam is the runtime's process boundary: the real
//! `twalk-hermes` binary, the real persona image, the real bus. The Gateway is
//! the one thing it cannot bring up cheaply — a real one needs a homeserver,
//! a state directory, an owner and a signed-in device — and what the runtime
//! needs from it is one read of one document. So this is a stub of exactly
//! that read, on the same terms as `sensor/tests/harness/gateway.rs` is a stub
//! of the consent snapshot.
//!
//! Two things it does **not** stub away, because they are what the test is
//! about:
//!
//! - **the token.** The read is refused with the Gateway's own `401
//!   unauthenticated` body unless the request carries
//!   `Authorization: Bearer <the service token>`. A runtime that reached the
//!   settings without the credential would pass a test that asserted only the
//!   answer.
//! - **the document.** The shape is
//!   `companion-gateway/src/settings_http.rs`'s `runtime_json`, field for
//!   field, `llm: null` included — copied here rather than imported, because a
//!   fixture that took the Gateway's own constructor would agree with it
//!   whatever it became.
//!
//! The stub records every request, so a test can assert that the runtime read
//! the settings **once** — the decision the ticket took — rather than polling
//! a Gateway for the life of the deployment.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The service token the stub expects, and the one the runtime is configured
/// with. It is the credential that opens the consent snapshot as well as these
/// settings (ADR 0010, ADR 0015), which is why a persona must never hold it —
/// `runtime_lifecycle.rs` looks for this exact string in the persona
/// container's environment.
pub use super::runtime::{GATEWAY_SERVICE_TOKEN, GATEWAY_SERVICE_TOKEN_VAR};

/// The ports a stub Gateway may bind, claimed one at a time so that several
/// tests — and several worktrees — can run at once. The range is this
/// suite's own (17500–17699).
const PORT_RANGE: std::ops::Range<u16> = 17500..17699;

/// The last port of the range, deliberately outside [`PORT_RANGE`] and
/// therefore bound by no stub: the address of a Companion Gateway that is not
/// there. Reserved rather than taken from a stub that has been dropped,
/// because a port a parallel test could claim between the drop and the read
/// would make "unreachable" mean "answered somebody else's document".
pub const UNREACHABLE_GATEWAY_URL: &str = "http://127.0.0.1:17699";

/// What the stub serves, and what it was asked.
#[derive(Debug, Default)]
struct State {
    /// The document served to an authenticated read. `None` makes every read
    /// answer `503 settings_not_configured`, which is a Gateway that is up and
    /// has no settings store.
    document: Option<Value>,
    reads: usize,
    unauthenticated_reads: usize,
}

/// A running stub Companion Gateway. Dropping it stops the server.
pub struct StubGateway {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl StubGateway {
    /// Starts a Gateway serving the runtime document `document`.
    pub async fn start(document: Value) -> Result<Self> {
        let listener = bind_in_range().await?;
        let addr = listener
            .local_addr()
            .context("the stub Gateway listener has no local address")?;
        let state = Arc::new(Mutex::new(State {
            document: Some(document),
            ..Default::default()
        }));
        let accept_task = tokio::spawn(accept_loop(listener, state.clone()));
        Ok(Self {
            addr,
            state,
            accept_task,
        })
    }

    /// The reference deployment's own document, with the model pointed at
    /// `base_url`: the model name and the language from the browser, the
    /// credential resolved from a file on the host (ADR 0015).
    pub fn runtime_document(base_url: &str, model: &str, language: Option<&str>) -> Value {
        json!({
            "llm": {
                "base_url": base_url,
                "model": model,
                "api_key": super::LLM_API_KEY,
                "credential_source": "file",
                "params": serde_json::from_str::<Value>(super::LLM_PARAMS)
                    .expect("the suite's provider parameters are a JSON object"),
            },
            "language": language,
            "personas": {},
        })
    }

    /// A Gateway whose user has named no model: `llm: null`, which the
    /// Gateway's own documentation insists is a different fact from a Gateway
    /// that could not be reached.
    pub fn no_model_document(language: Option<&str>) -> Value {
        json!({ "llm": Value::Null, "language": language, "personas": {} })
    }

    /// The origin the runtime is pointed at (`HERMES_GATEWAY_URL`).
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Replaces the document served from now on.
    pub fn set_document(&self, document: Value) {
        self.lock().document = Some(document);
    }

    /// How many authenticated reads of `/api/settings/runtime` this Gateway
    /// has answered. The runtime reads **once**, at startup, so this is how a
    /// test asserts that a running deployment is not polling.
    pub fn reads(&self) -> usize {
        self.lock().reads
    }

    /// Reads that arrived without the service token — none, if the runtime
    /// authenticates the way the Gateway requires.
    pub fn unauthenticated_reads(&self) -> usize {
        self.lock().unauthenticated_reads
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

/// Claims the first free port in this suite's range. Asking the kernel for an
/// ephemeral one would be simpler and would collide with whatever else on this
/// host happens to hold a high port; the range is the suite's own.
async fn bind_in_range() -> Result<TcpListener> {
    for port in PORT_RANGE {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        if let Ok(listener) = TcpListener::bind(addr).await {
            return Ok(listener);
        }
    }
    anyhow::bail!(
        "no free port in {}..{} for a stub Companion Gateway",
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

async fn serve_connection(mut stream: TcpStream, state: Arc<Mutex<State>>) -> Result<()> {
    let Some((path, authorization)) = read_request(&mut stream).await? else {
        return Ok(());
    };
    let (status, body) = respond(&path, authorization.as_deref(), &state);
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

/// Reads one HTTP/1.1 request head — the path and the `Authorization` header
/// are all this endpoint takes; it has no body.
async fn read_request(stream: &mut TcpStream) -> Result<Option<(String, Option<String>)>> {
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
    let path = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let authorization = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.trim().to_owned());
    Ok(Some((path, authorization)))
}

/// The Gateway's own refusals, by their own codes
/// (`companion-gateway/src/settings_http.rs`): a test that drove a stub with a
/// vocabulary of its own would prove nothing about the real one.
fn respond(
    path: &str,
    authorization: Option<&str>,
    state: &Arc<Mutex<State>>,
) -> (&'static str, Value) {
    let path = path.split('?').next().unwrap_or_default();
    if path != "/api/settings/runtime" {
        return (
            "404 Not Found",
            json!({ "error": "not_found", "detail": format!("this stub Gateway serves /api/settings/runtime only, not {path}") }),
        );
    }
    let expected = format!("Bearer {GATEWAY_SERVICE_TOKEN}");
    if authorization != Some(expected.as_str()) {
        state
            .lock()
            .expect("the stub Gateway mutex is never poisoned")
            .unauthenticated_reads += 1;
        return (
            "401 Unauthorized",
            json!({
                "error": "unauthenticated",
                "detail": "the runtime settings take this Gateway's service token as an \
                           Authorization: Bearer credential; a device token is not accepted here",
            }),
        );
    }
    let mut state = state
        .lock()
        .expect("the stub Gateway mutex is never poisoned");
    match state.document.clone() {
        Some(document) => {
            state.reads += 1;
            ("200 OK", document)
        }
        None => (
            "503 Service Unavailable",
            json!({
                "error": "settings_not_configured",
                "detail": "this Gateway keeps no settings: set GATEWAY_OWNER and GATEWAY_STATE_DIR",
            }),
        ),
    }
}
