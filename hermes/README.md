# hermes

The agent platform: the consumer runtime plus the reference personas (`assistant` first, then `watch`, `archive`, `writing`, and a triage persona). Personas consume events from the bus, reason with a local or remote LLM, and publish suggestions or replies back on the bus.

The consumer runtime is written in Rust. Personas are always separate processes talking to the bus — never in-process — so first-party and third-party personas take the same path. The reference persona `assistant` is written in Python against the SDK (see `docs/architecture/adr/0008-hermes-rust-runtime-personas-as-processes.md`).

## Status

- `src/` — the runtime ([#23](https://github.com/linagora/twalk/issues/23)): it reads its persona list from the environment, starts one process per persona, restarts a crashed one with backoff, follows the consent stream to learn which personas the user activated, and stops cleanly on SIGTERM. The decisions live in `config`, `environment`, `activation` and `supervisor`; `main.rs` wires them to NATS JetStream and to the process table.
- `personas/assistant/` — the reference persona ([#21](https://github.com/linagora/twalk/issues/21)): it consumes `inbound.message.received`, asks an LLM for a draft reply, and publishes `persona.thinking.emitted` then `persona.suggest.produced`. It ships as a container image and is built on `sdk/python/twalk_sdk`, which holds everything that must not be got wrong — starting with the consent gate, so that a persona author cannot breach consent by forgetting.
- `tests/assistant.rs` — the spec's seam 1: the real persona container, driven by publishing contract fixtures to the bus, with the stub LLM answering a canned completion. It asserts the two events are schema-valid, carry the contract's deterministic ids (`sha256(persona_id:trigger_event_id)` and `sha256(persona_id:trigger_event_id:attempt)`) with `Nats-Msg-Id` set, duplicate `network`/`consent`/`traceparent` as NATS headers and continue the trigger's trace — and it asserts the consent gate by absence: a `pending` message and a revoked sender's reduced message produce no event and no LLM call.
- `tests/suggestion.rs` — the suggestion policy ([#22](https://github.com/linagora/twalk/issues/22)) at the same seam: every suggestion carries an `expires_at`, one window after it was *produced* rather than after the trigger (a persona activated today replays messages from before it existed, so the trigger can be arbitrarily old); the window is the operator's (`TWALK_SUGGESTION_TTL_SECONDS`, an hour by default); and a trigger the bus redelivers is one suggestion at attempt 1, not a second draft — the attempt counts suggestions that exist, not deliveries that were tried.
- `tests/runtime_lifecycle.rs` — the spec's seam 2: the real `twalk-hermes` binary, starting the real persona image. It asserts the three acceptance criteria (a configured persona is started and consumes, killing it makes the runtime restart it, SIGTERM is a clean stop) and three absences — a paused persona receives nothing while still running, a persona is handed none of the runtime's own credentials, and a persona whose image will not run is announced as failed. It also asserts the loopback warning, because the stub LLM this suite runs is on exactly the address a containerised persona would dial as itself.
- `tests/runtime_settings.rs` — [#184](https://github.com/linagora/twalk/issues/184) at the same seam: the runtime reading the Companion Gateway's settings, against a stub Gateway that serves `GET /api/settings/runtime` and refuses a read without the deployment's service token. Its decisive test removes `HERMES_LLM_BASE_URL`, `HERMES_LLM_MODEL`, `HERMES_LLM_API_KEY`, `HERMES_LLM_PARAMS` and `HERMES_USER_LANGUAGE` from the runtime's own environment — "nothing written into `.env`" — and asserts the language the Companion holds in the prompt the stub LLM was really sent, the model in the same request, the credential in its `Authorization` header, and the Gateway's service token nowhere in the persona container's environment. Three more cover the decisions the ticket took: the precedence in the other direction, a Gateway that does not answer, and a deployment nobody has named a model for
- `tests/full_loop.rs` — the whole promise, end to end ([#25](https://github.com/linagora/twalk/issues/25)): the reference deployment (`deploy/docker-compose/`) with a real Sensor, a real Companion Gateway and a real homeserver, and the runtime **inside that deployment** — since [#158](https://github.com/linagora/twalk/issues/158) `deploy/docker-compose/compose.yaml` has a `hermes` service, so the test starts no process on the host at all and asserts that the runtime it drove was the deployment's own container. A contact writes in a room, the persona drafts a reply, the user approves it through the Gateway, and the reply is read back **out of the room from the homeserver**. Three of its four assertions are absences: a message whose consent is not `granted` produces no suggestion and no completion request, a suggestion nobody approved never reaches the room, and an approval refused because consent was revoked in the meantime sends nothing.
- `tests/smoke.rs` — the shared test harness ([#20](https://github.com/linagora/twalk/issues/20)) against the real stack.
- `tests/harness/` — the Hermes-specific half of the harness: `PersonaRun` (the persona alone, no runtime), `RuntimeRun` (the runtime, starting the persona itself) and `Deployment` (the reference deployment, Hermes included — it runs `docker compose up -d --wait hermes` and reads the runtime's log with `docker compose logs`).

The approval API is **not** here: ADR 0022 moved it to the Companion Gateway (`companion-gateway/src/approval.rs`, [#24](https://github.com/linagora/twalk/issues/24)), overriding spec [#19](https://github.com/linagora/twalk/issues/19), because an approval's defining clause — refused if the sender's consent is no longer `granted` at that moment — is a read of consent state, and the Gateway is its single writer. The runtime keeps no approval endpoint; it starts and supervises personas and does nothing else with a human decision.

## The runtime

### A persona's environment is constructed, never inherited

The runtime holds credentials a persona must not. The one the ADRs name is the Companion Gateway's service token: it reads the LLM configuration the operator set from the Companion, and it also opens the **consent snapshot** — the list of every contact the deployment knows (ADR 0010). That is exactly why the runtime injects the model configuration rather than letting each persona fetch it (ADR 0015), and the injection would be worth nothing if the child then inherited the token through `environ`.

So it does not. The child's environment is built from `environment::persona_environment` — a closed list of the variables the SDK reads — plus whatever the operator named in `HERMES_PERSONA_ENV_PASSTHROUGH` (default: `PATH`, which is what a child needs to `exec` at all). Everything in the runtime's own `HERMES_` namespace stays in the runtime. A third-party persona takes the same path as `assistant` (ADR 0008), so "a first-party persona would never read it" is not a control; the control is that the variable is not there, and `tests/runtime_lifecycle.rs` reads the container's environment back from Docker to say so.

That closed list is also the **channel** for everything else the deployment holds on a persona's behalf: the model and its parameters (ADR 0015), the suggestion window `TWALK_SUGGESTION_TTL_SECONDS` (#22), and — since [#164](https://github.com/linagora/twalk/issues/164) — ADR 0016's language preference as `TWALK_USER_LANGUAGE`.

Since [#184](https://github.com/linagora/twalk/issues/184) the runtime really does read the Gateway with that token, which is what makes the property above load-bearing rather than hypothetical. What crosses into the closed list is the **answer** — a model name, a language tag, an endpoint credential — and never the credential that fetched it. A persona's environment gains no new credential from that read, and `tests/runtime_settings.rs` asserts it on the container Docker actually started, in the run where the token was used.

### What a persona runs with is the user's choice, read once, and attributed

The model and the language are not the operator's settings but the **user's**, held by the Companion Gateway and set from the Companion (ADR 0015, ADR 0016). Until [#184](https://github.com/linagora/twalk/issues/184) nothing read them: `#98` built the Gateway's `GET /api/settings/runtime` and `#164` built the injection, and `twalk-hermes` had no HTTP client at all — so a preference reached a persona only if an operator *also* typed the same value into `.env`, and somebody who changed their language saw the Companion confirm it and nothing happen. `src/settings.rs` is that span, and it takes four decisions.

**It reads at startup, and only until it has a model.** A persona's environment is built once, when the runtime spawns the process, so applying a preference changed afterwards would mean restarting every persona — dropping the event in flight and taking a deployment's suggestions down for a language tag. So the read happens once, before any persona starts, and the runtime **says at startup** which value is in force, where it came from, and that a change made from now on takes effect when it is restarted (`docker compose restart hermes`). Saying it is the point: a restart-to-apply nobody is told about is a silent trap, which is the defect this ticket was about.

The one continuation of that read is the background retry, and it is deliberately not a re-read: **the retry exists to end an outage, never to apply a preference.** A runtime with no model has nothing to host, so it keeps asking until it has one and then starts the personas — which means naming a model in the Companion brings a deployment up with no restart at all. A runtime that *is* hosting personas never asks again.

**A Gateway that does not answer does not stop the runtime.** The Sensor already answered this exact question and its answer is copied rather than reinvented (`sensor/src/main.rs`): start anyway, say so at `ERROR` naming the URL, retry in the background. A runtime that refused to start because a settings endpoint was slow would take a whole deployment down for a preference. So an unreachable Gateway is an `ERROR` naming the URL *and what it fell back to*, and the personas start on whatever the host named. With nothing named anywhere, the runtime runs and hosts nothing — "allowed to exist, handed nothing", which is the shape ADR 0013's paused persona already uses.

**Precedence: the host wins, field by field, and the Gateway fills in the blanks.** ADR 0015 already decided the hard half — a credential the operator supplied *as a file* **wins** over one set from the browser — and this generalises that decision rather than inventing a second one: `.env` is the operator's other host-side voice, so it sits with the file, above the browser. The same order for the model and for the language, with no field exempt:

| | the model | the language |
| --- | --- | --- |
| 1 | `HERMES_LLM_API_KEY_FILE` (the credential only) | — |
| 2 | `HERMES_LLM_BASE_URL`, `HERMES_LLM_MODEL`, `HERMES_LLM_API_KEY`, `HERMES_LLM_PARAMS` | `HERMES_USER_LANGUAGE` |
| 3 | `GET /api/settings/runtime` | `GET /api/settings/runtime` |

That order does not make the browser's value inert, which is the objection to answer: `deploy/docker-compose/.env.example` ships every one of those variables **empty**, and an empty variable is unset here, so on the reference deployment the value in force is the Companion's. An operator who typed one into `.env` pinned it on purpose, and the startup line names which fields they pinned (`sources=api_key=operator base_url=gateway model=gateway language=gateway`). The merge is field by field and not document by document, because the reference deployment's ordinary combination is a credential from a file on the host and a model name from the browser.

**A persona's environment gains no new credential.** The token that reads these settings is the token that opens the consent snapshot — the list of every contact (ADR 0010, ADR 0015). It is held by the runtime and nothing in `settings.rs` reaches `environment.rs`'s closed list: what crosses is the answer, never the credential that fetched it. Two values the Gateway could serve that a persona would refuse are dropped here with a reason rather than injected — a language outside the Companion's five, and a model configuration missing its endpoint or its name — because a stored row an operator cannot edit from a crash loop must not be able to stop a deployment. A tag typed into `.env` is a different case and is still a startup refusal: that one an operator can fix before starting.

### Activation moves a consumer, not a process

Activating a persona is a consent decision on that persona and nothing else (ADR 0013). The runtime follows `consent.state.changed` from the beginning of the stream — no snapshot, because persona decisions are a handful over a deployment's life — and keeps the last decision per (persona, network). A persona nobody decided about is paused: activation never spreads on its own.

A paused persona **still runs and receives nothing**, so the thing activation moves cannot be the process. It is the persona's durable consumer, which the runtime creates and hands the name of: an active persona's consumer is filtered to `inbound.message.received`, a paused one's to `<prefix>.hermes.paused.<persona id>`, a subject inside the stream that no producer in Twalk ever publishes on. The persona process stays up, stays connected, keeps pulling, and is handed nothing. `supervisor` cannot see the activation state at all, which is how that stays true.

Two consequences, stated rather than discovered later:

- **Messages that arrive while a persona is paused are not replayed to it when it is activated again.** The user paused it; the messages of the pause are not its business afterwards either.
- **Per-network scoping is not enforced at the bus.** A bus subject carries no network, so a consumer cannot be filtered to one: the runtime's enforcement is binary, and a persona granted on `whatsapp` alone is handed every network's inbound messages. Narrowing that is consumer-side work, like the SDK's consent gate, and it is not done here.

### A runtime that cannot start something says so

A run shorter than `HERMES_PERSONA_HEALTHY_AFTER_MS` never really started. After `HERMES_PERSONA_START_FAILURES` of those in a row, the runtime logs `persona failed to start` at error level — once per failure episode, not once per attempt — and keeps retrying at the capped backoff, so an operator who fixes the image does not also have to restart Hermes. A persona that ran, worked and then crashed is a different thing: its backoff resets and it comes straight back.

### Five causes, five messages

A persona that cannot reason fails for one of five reasons, and they need five different answers from an operator. They are told apart, and they are told apart in different places:

| Cause | Who says it | What it says |
| --- | --- | --- |
| No model configured | the runtime, at startup | With no Companion Gateway to read one from, it refuses to start naming both ways out, because there is no default (ADR 0015). With a Gateway whose user has named no model, it starts, hosts nothing, says `no model is configured … this runtime is up and hosts nothing`, and starts the personas the moment one is named |
| The endpoint cannot be reached | the SDK's client, in the persona's log | `the chat-completions endpoint at … is unreachable: …`, and the event is retried |
| The endpoint refused the request | the SDK's client, in the persona's log | `the chat-completions endpoint answered HTTP 401: …`, with the endpoint's own message |
| The model answered nothing | the SDK, in the persona's log | `the model answered nothing: it stopped on its own …` — the endpoint and the budget are not the problem, and the event is retried |
| The model spent its budget thinking | the SDK, in the persona's log | `the model spent its whole 2000-token budget on reasoning and never answered …` — `HTTP 200`, `finish_reason: "length"`, no content, and the event is **not** retried, because the same request gets the same non-answer and each one is billed. The message names the remedy: a larger budget in `HERMES_LLM_PARAMS` ([#162](https://github.com/linagora/twalk/issues/162)) |

One case sits between them and the runtime is the only component that can see it: an endpoint on **this host's loopback**. `http://127.0.0.1:4000/v1` is a perfectly good address — an operator's LiteLLM proxy published on loopback is the reference deployment's shape — and a persona in a container of its own network namespace dials itself. The runtime holds the URL, so it warns at startup and names the two fixes (host networking for the persona's command, or an address its container can resolve) rather than letting the first message arrive and time out.

Making that address reachable is **not** the runtime's job: how a persona's container joins a network is the command's business and the deployment's. `deploy/docker-compose/` answers it since [#158](https://github.com/linagora/twalk/issues/158) — the `hermes` service and its persona containers share the host's network namespace, so a loopback endpoint is reachable and the bus is reached through its published port ([ADR 0023](../docs/architecture/adr/0023-the-deployment-starts-personas-through-the-hosts-docker-socket.md)). The warning above still fires there, because the runtime cannot see how the command it was given joins a network; on that deployment it is noise, and silencing it would mean the runtime knowing something only the argv knows.

### Observability

Lifecycle transitions are structured logs and nothing else. The runtime emits no telemetry of its own: observability is OTLP and opt-in (ADR 0017), and there is no endpoint configured here to opt into yet. No log line carries message content — the runtime never reads a message.

### Configuration

Environment variables, like every other Twalk component.

| Variable | Required | Meaning |
| --- | --- | --- |
| `HERMES_PERSONAS` | yes | The persona list, as a JSON array of `{"id": "assistant", "command": ["docker", "run", …]}`. An argv, not a shell string: there is no quoting to get wrong and no shell between the runtime and the persona. |
| `HERMES_DOMAIN` | yes | The authority of every hosted persona's `source` URI (`hermes://<domain>/personas/<persona id>`). |
| `HERMES_GATEWAY_URL` | | The Companion Gateway's origin, e.g. `http://127.0.0.1:8080`. Set, the runtime reads `GET /api/settings/runtime` once at startup and runs the personas on the model and the language the user chose in the Companion (#184). Unset, the two below are the only voice, and the runtime says at startup that nothing set in the Companion will reach a persona. A URL with no token is refused at startup — that read could only ever answer `401`. |
| `HERMES_GATEWAY_SERVICE_TOKEN` | | The deployment's service token, sent as `Authorization: Bearer`. The **same** secret that opens the consent snapshot (ADR 0010), which is why the runtime holds it and a persona never does (ADR 0015). Never logged, never injected. A token with no URL is not an error — it simply opens nothing, and the runtime says so. |
| `HERMES_LLM_BASE_URL` | unless a Gateway is configured | An OpenAI-compatible chat-completions base URL. No default: Twalk ships no LLM (ADR 0015). Set here, it **wins** over the one the user chose in the Companion, which is ADR 0015's file-beats-browser order generalised; on the reference deployment it is empty and the Companion's value is in force. |
| `HERMES_LLM_MODEL` | unless a Gateway is configured | The model the operator pinned. No default, same reason, same precedence. With neither these two nor a Gateway, the runtime refuses to start and names both ways out. |
| `HERMES_LLM_API_KEY` | | The endpoint's credential. |
| `HERMES_LLM_API_KEY_FILE` | | The same credential from a file, and it **wins** over `HERMES_LLM_API_KEY`, so a production stack can lock it down (ADR 0015). |
| `HERMES_LLM_PARAMS` | | A JSON object of provider parameters, passed to the persona untouched. A parameter set to `null` removes a field the request would otherwise carry, which is how a provider that rejects one is made to work. Wins over the Gateway's own passthrough. |
| `HERMES_LLM_TIMEOUT_SECONDS` | | Passed through to the persona. |
| `HERMES_SUGGESTION_TTL_SECONDS` | | How long a suggestion stays approvable (#22). Operator configuration like the model, so it travels the same way; unset leaves the SDK's own default of an hour. |
| `HERMES_USER_LANGUAGE` | | The **user's own** language, one of `en`, `fr`, `it`, `es`, `de`, injected into each persona as `TWALK_USER_LANGUAGE` (#164). It is what a persona falls back to when it cannot tell what language the message it is answering was written in, and nothing else (ADR 0016). A user preference rather than an operator's setting, which is why the Gateway holds it — this variable is the **operator's override** of it, and it wins, on exactly the same terms as the model (#184). Unset here and with a Gateway configured, the stored preference is the one in force; unset on both sides is supported — the personas then answer in each message's own language and say in their logs what happens to a message whose language they cannot tell. A tag outside the five is refused **here**, at startup: injected, it would be refused by every persona, on its first line, at every spawn. |
| `HERMES_NATS_URL` | | Default `nats://localhost:4222`. |
| `HERMES_BUS_STREAM` | | Default `twalk`. |
| `HERMES_BUS_SUBJECT_PREFIX` | | Default `twalk`. |
| `HERMES_LOG_LEVEL` | | The runtime's own, default `info`. |
| `HERMES_PERSONA_LOG_LEVEL` | | The personas', default `info`: debugging a persona should not make the supervisor noisy too. |
| `HERMES_RESTART_BACKOFF_BASE_MS` | | Default 1000. |
| `HERMES_RESTART_BACKOFF_MAX_MS` | | Default 60000. |
| `HERMES_PERSONA_HEALTHY_AFTER_MS` | | Default 10000: how long a run must last to count as one. |
| `HERMES_PERSONA_START_FAILURES` | | Default 3: consecutive failed starts before the verdict. |
| `HERMES_SHUTDOWN_GRACE_MS` | | Default 10000: how long a persona is given after SIGTERM before it is killed. |
| `HERMES_PERSONA_ENV_PASSTHROUGH` | | Names of the runtime's own variables a persona may inherit, comma-separated. Default `PATH`. |

## Tests

```bash
cd hermes
cargo test   # boots the shared test stack and builds the persona image itself (Docker required)
```

The stack is the same one the Sensor suite uses, so the two suites share one Synapse and one NATS JetStream; `TWALK_TEST_STACK`, `TWALK_TEST_SYNAPSE_PORT` and `TWALK_TEST_NATS_PORT` move it aside for parallel worktrees. Each persona test runs the persona under its own compose project and on its own JetStream stream, and removes both afterwards; each runtime test runs its own `twalk-hermes` process, its own persona containers (named `h23-…`) and its own stream, and removes all of them.

`tests/full_loop.rs` is the exception to all of that, and it is the expensive one: it brings up the **reference deployment** — `deploy/docker-compose/compose.yaml`, configured only through a generated environment file — under its own compose project and its own host ports, so that the Sensor which posts the approved reply and the Gateway which serves the approval are the ones an operator runs. `TWALK_LOOP_TEST_STACK` (default `twalk-h25-loop`), `TWALK_LOOP_TEST_SYNAPSE_PORT` (19508), `TWALK_LOOP_TEST_GATEWAY_PORT` (19518) and `TWALK_LOOP_TEST_NATS_PORT` (19522) move it aside. Unlike the other deployment suites it **tears its stack down at the end of every run, passing or failing**, and removes the four images it tagged for itself (`twalk/companion-gateway:<stack>`, `twalk/sensor:<stack>`, `twalk/hermes:<stack>`, `twalk/persona-assistant:<stack>`) and the persona container the runtime started outside compose: a stack that outlives its run is a defect ([#128](https://github.com/linagora/twalk/issues/128)), and what a failure needs in order to be diagnosed — the runtime's logs and the deployment's — is attached to the failure itself. `TWALK_LOOP_TEST_KEEP=1` keeps it up for an operator who would rather look by hand.

`tests/runtime_settings.rs` adds one thing to that list and nothing else: a stub Companion Gateway (`tests/harness/gateway.rs`) bound to a port in **17500–17699**, serving `GET /api/settings/runtime` with the Gateway's own document shape and the Gateway's own `401 unauthenticated` refusal. It is a stub of one read rather than a real Gateway because a real one needs a homeserver, a state directory, an owner and a signed-in device — and `companion-gateway/tests/settings.rs` already drives the real one. `17699` is reserved as the address of a Gateway that is not there, so "unreachable" never quietly means "answered a parallel test's document".

The runtime spawns a **process**, and a persona ships as a **container image**, so the argv the runtime is configured with in tests is `tests/run-persona-image.sh` — a wrapper that runs the image with whatever environment the runtime gave it, forwarding every variable so that an assertion about what a persona does *not* hold is worth making. It also translates SIGTERM into `docker stop`, because `docker run` does not reliably pass a signal on to the container it started.

The deployment ships its own copy of that wrapper — `deploy/docker-compose/run-persona-image.sh`, inside the Hermes image as `run-persona`, and named in `HERMES_PERSONAS`. It does the same two things (forward every variable, translate SIGTERM into `docker stop`) and adds the network the deployment chose. The two files are kept apart on purpose: one is a test fixture, the other ships in an image an operator runs, and a shared file would make a change for one of them a change for both.
