//! A stub OpenAI-compatible chat-completions server for persona tests.
//!
//! A persona reasons with an LLM, so its tests need one — but a real model
//! answers differently every run, and the harness's assertions must not
//! depend on a network service. The stub answers `POST /v1/chat/completions`
//! with a canned completion: the same request always gets byte-identical
//! bytes back, and every request is recorded so a test can assert the
//! persona called the model — or, for the consent gate, that it did not.
//!
//! It runs in the test process, bound on an ephemeral loopback port, so
//! parallel suites never collide and no container is needed. HTTP/1.1 is
//! hand-rolled over tokio, like the Sensor's metrics endpoint: the stub
//! serves one route and a hand-rolled response is smaller than any
//! dependency.
//!
//! A canned completion is not the only thing a real endpoint answers, and
//! the ones that are not completions are where personas went wrong: a model
//! that spends its whole budget reasoning answers `200` with no content at
//! all (issue #162). So the stub scripts [`StubAnswer`]s rather than strings,
//! and a test can put that shape — the one a Qwen behind LiteLLM really
//! answered — in front of a persona.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::sha256_hex;

/// What the stub answers when no reply has been scripted.
pub const DEFAULT_REPLY: &str = "This is a canned suggestion from the stub LLM.";

/// The `model` the stub reports when the request names none.
const DEFAULT_MODEL: &str = "stub-model";

/// A fixed `created` timestamp: two identical requests must get two
/// identical responses, so nothing in the answer may come from the clock.
const CREATED: u64 = 1_700_000_000;

/// One chat-completions request as the stub received it.
#[derive(Clone, Debug)]
pub struct StubRequest {
    /// The request path, e.g. `/v1/chat/completions`.
    pub path: String,
    /// The `Authorization` header, if the caller sent one: the operator
    /// configures the endpoint's credentials, and a test may assert they
    /// reached it.
    pub authorization: Option<String>,
    /// The parsed request body.
    pub body: Value,
}

impl StubRequest {
    /// The `content` of the last message of the conversation — what the
    /// persona actually asked the model.
    pub fn last_message_content(&self) -> Option<&str> {
        self.body
            .get("messages")?
            .as_array()?
            .last()?
            .get("content")?
            .as_str()
    }
}

/// What the stub answers one request with: a completion, or one of the two
/// ways an OpenAI-compatible endpoint answers `200` with no usable text.
///
/// The distinction is the point. A persona must not treat them alike — one
/// may pass on a retry, the other is the model behaving exactly as
/// configured and will not (issue #162) — so a harness that could only serve
/// text could not put that difference under test.
#[derive(Clone, Debug)]
pub enum StubAnswer {
    /// A completion: this text, `finish_reason: "stop"`.
    Completion(String),
    /// A reasoning model that spent the whole budget thinking: the exact
    /// shape the reference deployment saw — `finish_reason: "length"`,
    /// `content: null`, and the budget in `reasoning_content`.
    BudgetSpentReasoning { reasoning: String },
    /// A model that stopped on its own having said nothing: an empty
    /// completion, `finish_reason: "stop"`, no reasoning.
    NoContent,
}

impl StubAnswer {
    /// A reasoning answer of a plausible size, which is all a test needs of
    /// the text: nothing asserts what a model was thinking.
    pub fn budget_spent_reasoning() -> Self {
        Self::BudgetSpentReasoning {
            reasoning: "The user wrote a short message. I should consider what \
                 they mean before answering. "
                .repeat(20),
        }
    }

    /// The `message` object and `finish_reason` of this answer.
    fn body(&self) -> (Value, &'static str) {
        match self {
            Self::Completion(text) => (json!({ "role": "assistant", "content": text }), "stop"),
            Self::BudgetSpentReasoning { reasoning } => (
                json!({
                    "role": "assistant",
                    "content": Value::Null,
                    "reasoning_content": reasoning,
                }),
                // Not "stop": the model ran out of budget, which is the one
                // fact that tells a persona the budget was the problem.
                "length",
            ),
            Self::NoContent => (json!({ "role": "assistant", "content": "" }), "stop"),
        }
    }
}

struct State {
    answer: StubAnswer,
    /// Answers scripted one at a time, consumed in order before `answer`.
    scripted: VecDeque<StubAnswer>,
    requests: Vec<StubRequest>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            answer: StubAnswer::Completion(DEFAULT_REPLY.to_owned()),
            scripted: VecDeque::new(),
            requests: Vec::new(),
        }
    }
}

/// A running stub LLM. Dropping it stops the server.
pub struct StubLlm {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl StubLlm {
    /// Starts the stub on an ephemeral loopback port, answering
    /// [`DEFAULT_REPLY`].
    pub async fn start() -> Result<Self> {
        Self::start_with_reply(DEFAULT_REPLY).await
    }

    /// [`start`](Self::start) with an explicit canned reply.
    pub async fn start_with_reply(reply: &str) -> Result<Self> {
        Self::start_on(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), reply).await
    }

    /// [`start_with_reply`](Self::start_with_reply) on an explicit address:
    /// a persona running in a container reaches the host through the docker
    /// gateway, not through loopback, and must be given a routable bind.
    pub async fn start_on(addr: SocketAddr, reply: &str) -> Result<Self> {
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("failed to bind the stub LLM on {addr}"))?;
        let addr = listener
            .local_addr()
            .context("the stub LLM listener has no local address")?;
        let state = Arc::new(Mutex::new(State {
            answer: StubAnswer::Completion(reply.to_owned()),
            ..Default::default()
        }));
        let accept_task = tokio::spawn(accept_loop(listener, state.clone()));
        Ok(Self {
            addr,
            state,
            accept_task,
        })
    }

    /// The OpenAI-compatible base URL, as the `OPENAI_BASE_URL` style
    /// configuration of a persona expects it.
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    /// The full chat-completions endpoint.
    pub fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url())
    }

    /// Replaces the canned reply used once the scripted ones run out.
    pub fn set_reply(&self, reply: &str) {
        self.set_answer(StubAnswer::Completion(reply.to_owned()));
    }

    /// Queues one reply, served (in order) before the canned one: a
    /// multi-turn test scripts the answers it needs.
    pub fn push_reply(&self, reply: &str) {
        self.push_answer(StubAnswer::Completion(reply.to_owned()));
    }

    /// [`set_reply`](Self::set_reply) for an answer that is not a completion:
    /// what the endpoint serves from now on, once the scripted answers run
    /// out.
    pub fn set_answer(&self, answer: StubAnswer) {
        self.lock().answer = answer;
    }

    /// Queues one answer, served (in order) before the standing one.
    pub fn push_answer(&self, answer: StubAnswer) {
        self.lock().scripted.push_back(answer);
    }

    /// Every request received so far, oldest first.
    pub fn requests(&self) -> Vec<StubRequest> {
        self.lock().requests.clone()
    }

    pub fn request_count(&self) -> usize {
        self.lock().requests.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("the stub LLM mutex is never poisoned")
    }
}

impl Drop for StubLlm {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

async fn accept_loop(listener: TcpListener, state: Arc<Mutex<State>>) {
    while let Ok((stream, _)) = listener.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            // A failed connection is the test's problem to observe through
            // its own client, not something the stub can report.
            let _ = serve_connection(stream, state).await;
        });
    }
}

/// Serves exactly one request, then closes the connection (the response
/// says `Connection: close`, so clients do not reuse it).
async fn serve_connection(mut stream: TcpStream, state: Arc<Mutex<State>>) -> Result<()> {
    let request = match read_request(&mut stream).await? {
        Some(request) => request,
        None => return Ok(()),
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

struct RawRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

/// Reads one HTTP/1.1 request: the head up to the blank line, then
/// `content-length` bytes of body.
async fn read_request(stream: &mut TcpStream) -> Result<Option<RawRequest>> {
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(position) = find_head_end(&buffer) {
            break position;
        }
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            // The peer closed before sending a complete head.
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_owned();
    let path = request_line.next().unwrap_or_default().to_owned();

    let mut content_length = 0_usize;
    let mut authorization = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().unwrap_or(0);
        } else if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value.to_owned());
        }
    }

    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);

    Ok(Some(RawRequest {
        method,
        path,
        authorization,
        body,
    }))
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Routes one request and builds its response, recording it when it is a
/// well-formed chat-completions call.
fn respond(request: &RawRequest, state: &Arc<Mutex<State>>) -> (&'static str, Value) {
    let path = request.path.split('?').next().unwrap_or_default();
    if !path.ends_with("/chat/completions") {
        return (
            "404 Not Found",
            error_body(
                format!("the stub LLM serves /v1/chat/completions only, not {path}"),
                "invalid_request_error",
            ),
        );
    }
    if request.method != "POST" {
        return (
            "405 Method Not Allowed",
            error_body(
                format!(
                    "chat completions is a POST endpoint, not {}",
                    request.method
                ),
                "invalid_request_error",
            ),
        );
    }
    let Ok(body) = serde_json::from_slice::<Value>(&request.body) else {
        return (
            "400 Bad Request",
            error_body("the request body is not JSON", "invalid_request_error"),
        );
    };
    let has_messages = body
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| !messages.is_empty());
    if !has_messages {
        return (
            "400 Bad Request",
            error_body(
                "a chat-completions request needs a non-empty messages array",
                "invalid_request_error",
            ),
        );
    }

    let answer = {
        let mut state = state.lock().expect("the stub LLM mutex is never poisoned");
        state.requests.push(StubRequest {
            path: path.to_owned(),
            authorization: request.authorization.clone(),
            body: body.clone(),
        });
        state
            .scripted
            .pop_front()
            .unwrap_or_else(|| state.answer.clone())
    };
    let (message, finish_reason) = answer.body();

    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_MODEL);
    // The id is a function of the request alone: identical requests get
    // identical answers, replay included.
    let id = format!(
        "chatcmpl-stub-{}",
        &sha256_hex(&String::from_utf8_lossy(&request.body))[..24]
    );
    (
        "200 OK",
        json!({
            "id": id,
            "object": "chat.completion",
            "created": CREATED,
            "model": model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": finish_reason,
            }],
            "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 },
        }),
    )
}

fn error_body(message: impl Into<String>, kind: &str) -> Value {
    json!({ "error": { "message": message.into(), "type": kind } })
}

#[cfg(test)]
mod tests {
    use super::{StubAnswer, StubLlm, DEFAULT_REPLY};
    use anyhow::Result;
    use serde_json::{json, Value};

    fn chat_request() -> Value {
        json!({
            "model": "local-model",
            "messages": [
                { "role": "system", "content": "You draft replies." },
                { "role": "user", "content": "on décale à 20h ?" },
            ],
        })
    }

    async fn post(url: &str, body: &Value) -> Result<(reqwest::StatusCode, Value)> {
        let response = reqwest::Client::new()
            .post(url)
            .header("authorization", "Bearer test-only-llm-key")
            .json(body)
            .send()
            .await?;
        let status = response.status();
        Ok((status, response.json().await?))
    }

    #[tokio::test]
    async fn answers_the_same_completion_for_the_same_request() -> Result<()> {
        let stub = StubLlm::start().await?;

        let (status, first) = post(&stub.chat_completions_url(), &chat_request()).await?;
        assert_eq!(status, 200);
        assert_eq!(
            first
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str),
            Some(DEFAULT_REPLY),
            "the stub must serve its canned reply"
        );
        assert_eq!(
            first.get("model").and_then(Value::as_str),
            Some("local-model"),
            "the stub echoes the requested model"
        );

        let (_, second) = post(&stub.chat_completions_url(), &chat_request()).await?;
        assert_eq!(
            first, second,
            "the same request must get a byte-identical answer, id included"
        );
        Ok(())
    }

    #[tokio::test]
    async fn records_every_request_it_answered() -> Result<()> {
        let stub = StubLlm::start().await?;

        for _ in 0..3 {
            post(&stub.chat_completions_url(), &chat_request()).await?;
        }

        let requests = stub.requests();
        assert_eq!(requests.len(), 3, "every request must be recorded");
        assert_eq!(requests[0].path, "/v1/chat/completions");
        assert_eq!(
            requests[0].authorization.as_deref(),
            Some("Bearer test-only-llm-key"),
            "the operator's credentials must reach the endpoint"
        );
        assert_eq!(
            requests[0].last_message_content(),
            Some("on décale à 20h ?"),
            "the persona's prompt must be readable by the test"
        );
        Ok(())
    }

    #[tokio::test]
    async fn scripted_replies_are_served_in_order_before_the_canned_one() -> Result<()> {
        let stub = StubLlm::start_with_reply("canned").await?;
        stub.push_reply("first");
        stub.push_reply("second");

        let url = stub.chat_completions_url();
        let mut served = Vec::new();
        for _ in 0..3 {
            let (_, body) = post(&url, &chat_request()).await?;
            served.push(
                body.pointer("/choices/0/message/content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            );
        }

        assert_eq!(
            served,
            vec!["first", "second", "canned"],
            "scripted replies come first, in order, then the canned one"
        );
        Ok(())
    }

    /// The shape that broke the reference deployment (issue #162): `200`,
    /// `finish_reason: "length"`, no content, the budget in
    /// `reasoning_content`. Asserted here so that a persona test asserting
    /// what a persona *does* with it is not also asserting what the stub
    /// serves.
    #[tokio::test]
    async fn serves_the_answer_a_reasoning_model_gives_when_its_budget_is_gone() -> Result<()> {
        let stub = StubLlm::start().await?;
        stub.push_answer(StubAnswer::budget_spent_reasoning());
        stub.push_answer(StubAnswer::NoContent);

        let url = stub.chat_completions_url();
        let (status, spent) = post(&url, &chat_request()).await?;
        assert_eq!(status, 200, "the endpoint is healthy; that is the problem");
        assert_eq!(
            spent.pointer("/choices/0/finish_reason"),
            Some(&json!("length"))
        );
        assert_eq!(
            spent.pointer("/choices/0/message/content"),
            Some(&Value::Null),
            "the whole budget went to reasoning, so there is no content at all"
        );
        assert!(spent
            .pointer("/choices/0/message/reasoning_content")
            .and_then(Value::as_str)
            .is_some_and(|reasoning| !reasoning.is_empty()));

        let (_, nothing) = post(&url, &chat_request()).await?;
        assert_eq!(
            nothing.pointer("/choices/0/message/content"),
            Some(&json!("")),
            "a model that stopped on its own is a different answer"
        );
        assert_eq!(
            nothing.pointer("/choices/0/finish_reason"),
            Some(&json!("stop"))
        );

        let (_, canned) = post(&url, &chat_request()).await?;
        assert_eq!(
            canned
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str),
            Some(DEFAULT_REPLY),
            "the scripted answers run out and the canned completion resumes"
        );
        Ok(())
    }

    #[tokio::test]
    async fn rejects_a_malformed_request_and_an_unknown_route() -> Result<()> {
        let stub = StubLlm::start().await?;

        let (status, body) = post(&stub.chat_completions_url(), &json!({ "model": "m" })).await?;
        assert_eq!(
            status, 400,
            "a request without messages is not a completion"
        );
        assert!(body.pointer("/error/message").is_some());

        let response = reqwest::get(format!("{}/models", stub.base_url())).await?;
        assert_eq!(response.status(), 404, "the stub serves one route only");

        assert_eq!(
            stub.request_count(),
            0,
            "only well-formed completions count as calls to the model"
        );
        Ok(())
    }

    #[tokio::test]
    async fn stops_serving_once_dropped() -> Result<()> {
        let stub = StubLlm::start().await?;
        let url = stub.chat_completions_url();
        post(&url, &chat_request()).await?;
        drop(stub);

        // The listener closes with the accept task, so a new connection is
        // refused instead of hanging. Cancellation is not instantaneous, so
        // the check is a short poll rather than a single attempt.
        for attempt in 0..20 {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                reqwest::Client::new()
                    .post(&url)
                    .json(&chat_request())
                    .send(),
            )
            .await?;
            match result {
                Err(_) => return Ok(()),
                Ok(_) if attempt == 19 => {
                    panic!("a dropped stub must stop answering on {url}")
                }
                Ok(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
        unreachable!("the loop either returns or panics")
    }
}
