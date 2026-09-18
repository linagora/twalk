# hermes

The agent platform: the consumer runtime plus the reference personas (`assistant` first, then `watch`, `archive`, `writing`, and a triage persona). Personas consume events from the bus, reason with a local or remote LLM, and publish suggestions or replies back on the bus.

The consumer runtime is written in Rust. Personas are always separate processes talking to the bus — never in-process — so first-party and third-party personas take the same path. The reference persona `assistant` is written in Python against the SDK (see `docs/architecture/adr/0008-hermes-rust-runtime-personas-as-processes.md`).

## Status

- `src/` — the runtime ([#23](https://github.com/linagora/twalk/issues/23)): it reads its persona list from the environment, starts one process per persona, restarts a crashed one with backoff, follows the consent stream to learn which personas the user activated, and stops cleanly on SIGTERM. The decisions live in `config`, `environment`, `activation` and `supervisor`; `main.rs` wires them to NATS JetStream and to the process table.
- `personas/assistant/` — the reference persona ([#21](https://github.com/linagora/twalk/issues/21)): it consumes `inbound.message.received`, asks an LLM for a draft reply, and publishes `persona.thinking.emitted` then `persona.suggest.produced`. It ships as a container image and is built on `sdk/python/twalk_sdk`, which holds everything that must not be got wrong — starting with the consent gate, so that a persona author cannot breach consent by forgetting.
- `tests/assistant.rs` — the spec's seam 1: the real persona container, driven by publishing contract fixtures to the bus, with the stub LLM answering a canned completion. It asserts the two events are schema-valid, carry the contract's deterministic ids (`sha256(persona_id:trigger_event_id)` and `sha256(persona_id:trigger_event_id:attempt)`) with `Nats-Msg-Id` set, duplicate `network`/`consent`/`traceparent` as NATS headers and continue the trigger's trace — and it asserts the consent gate by absence: a `pending` message and a revoked sender's reduced message produce no event and no LLM call.
- `tests/suggestion.rs` — the suggestion policy ([#22](https://github.com/linagora/twalk/issues/22)) at the same seam: every suggestion carries an `expires_at`, one window after it was *produced* rather than after the trigger (a persona activated today replays messages from before it existed, so the trigger can be arbitrarily old); the window is the operator's (`TWALK_SUGGESTION_TTL_SECONDS`, an hour by default); and a trigger the bus redelivers is one suggestion at attempt 1, not a second draft — the attempt counts suggestions that exist, not deliveries that were tried.
- `tests/runtime_lifecycle.rs` — the spec's seam 2: the real `twalk-hermes` binary, starting the real persona image. It asserts the three acceptance criteria (a configured persona is started and consumes, killing it makes the runtime restart it, SIGTERM is a clean stop) and three absences — a paused persona receives nothing while still running, a persona is handed none of the runtime's own credentials, and a persona whose image will not run is announced as failed. It also asserts the loopback warning, because the stub LLM this suite runs is on exactly the address a containerised persona would dial as itself.
- `tests/full_loop.rs` — the whole promise, end to end ([#25](https://github.com/linagora/twalk/issues/25)): the reference deployment (`deploy/docker-compose/`) with a real Sensor, a real Companion Gateway and a real homeserver, and the runtime **inside that deployment** — since [#158](https://github.com/linagora/twalk/issues/158) `deploy/docker-compose/compose.yaml` has a `hermes` service, so the test starts no process on the host at all and asserts that the runtime it drove was the deployment's own container. A contact writes in a room, the persona drafts a reply, the user approves it through the Gateway, and the reply is read back **out of the room from the homeserver**. Three of its four assertions are absences: a message whose consent is not `granted` produces no suggestion and no completion request, a suggestion nobody approved never reaches the room, and an approval refused because consent was revoked in the meantime sends nothing.
- `tests/smoke.rs` — the shared test harness ([#20](https://github.com/linagora/twalk/issues/20)) against the real stack.
- `tests/harness/` — the Hermes-specific half of the harness: `PersonaRun` (the persona alone, no runtime), `RuntimeRun` (the runtime, starting the persona itself) and `Deployment` (the reference deployment, Hermes included — it runs `docker compose up -d --wait hermes` and reads the runtime's log with `docker compose logs`).

The approval API is **not** here: ADR 0022 moved it to the Companion Gateway (`companion-gateway/src/approval.rs`, [#24](https://github.com/linagora/twalk/issues/24)), overriding spec [#19](https://github.com/linagora/twalk/issues/19), because an approval's defining clause — refused if the sender's consent is no longer `granted` at that moment — is a read of consent state, and the Gateway is its single writer. The runtime keeps no approval endpoint; it starts and supervises personas and does nothing else with a human decision.

## The runtime

### A persona's environment is constructed, never inherited

The runtime holds credentials a persona must not. The one the ADRs name is the Companion Gateway's service token: it reads the LLM configuration the operator set from the Companion, and it also opens the **consent snapshot** — the list of every contact the deployment knows (ADR 0010). That is exactly why the runtime injects the model configuration rather than letting each persona fetch it (ADR 0015), and the injection would be worth nothing if the child then inherited the token through `environ`.

So it does not. The child's environment is built from `environment::persona_environment` — a closed list of the variables the SDK reads — plus whatever the operator named in `HERMES_PERSONA_ENV_PASSTHROUGH` (default: `PATH`, which is what a child needs to `exec` at all). Everything in the runtime's own `HERMES_` namespace stays in the runtime. A third-party persona takes the same path as `assistant` (ADR 0008), so "a first-party persona would never read it" is not a control; the control is that the variable is not there, and `tests/runtime_lifecycle.rs` reads the container's environment back from Docker to say so.

That closed list is also the **channel** for everything else the deployment holds on a persona's behalf: the model and its parameters (ADR 0015), and the suggestion window `TWALK_SUGGESTION_TTL_SECONDS` (#22). ADR 0016's language preference travels the same way and is not injected yet, because the SDK names no variable for it — one line in `environment.rs` the day it does, and not before: a variable nothing reads is configuration that only looks like it works.

### Activation moves a consumer, not a process

Activating a persona is a consent decision on that persona and nothing else (ADR 0013). The runtime follows `consent.state.changed` from the beginning of the stream — no snapshot, because persona decisions are a handful over a deployment's life — and keeps the last decision per (persona, network). A persona nobody decided about is paused: activation never spreads on its own.

A paused persona **still runs and receives nothing**, so the thing activation moves cannot be the process. It is the persona's durable consumer, which the runtime creates and hands the name of: an active persona's consumer is filtered to `inbound.message.received`, a paused one's to `<prefix>.hermes.paused.<persona id>`, a subject inside the stream that no producer in Twalk ever publishes on. The persona process stays up, stays connected, keeps pulling, and is handed nothing. `supervisor` cannot see the activation state at all, which is how that stays true.

Two consequences, stated rather than discovered later:

- **Messages that arrive while a persona is paused are not replayed to it when it is activated again.** The user paused it; the messages of the pause are not its business afterwards either.
- **Per-network scoping is not enforced at the bus.** A bus subject carries no network, so a consumer cannot be filtered to one: the runtime's enforcement is binary, and a persona granted on `whatsapp` alone is handed every network's inbound messages. Narrowing that is consumer-side work, like the SDK's consent gate, and it is not done here.

### A runtime that cannot start something says so

A run shorter than `HERMES_PERSONA_HEALTHY_AFTER_MS` never really started. After `HERMES_PERSONA_START_FAILURES` of those in a row, the runtime logs `persona failed to start` at error level — once per failure episode, not once per attempt — and keeps retrying at the capped backoff, so an operator who fixes the image does not also have to restart Hermes. A persona that ran, worked and then crashed is a different thing: its backoff resets and it comes straight back.

### Three causes, three messages

A persona that cannot reason fails for one of three reasons, and they need three different answers from an operator. They are told apart, and they are told apart in three different places:

| Cause | Who says it | What it says |
| --- | --- | --- |
| No model configured | the runtime, at startup | `missing required environment variable HERMES_LLM_BASE_URL` — it refuses to start, because there is no default (ADR 0015) |
| The endpoint cannot be reached | the SDK's client, in the persona's log | `the chat-completions endpoint at … is unreachable: …`, and the event is retried |
| The endpoint refused the request | the SDK's client, in the persona's log | `the chat-completions endpoint answered HTTP 401: …`, with the endpoint's own message |

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
| `HERMES_LLM_BASE_URL` | yes | An OpenAI-compatible chat-completions base URL. No default: Twalk ships no LLM (ADR 0015). |
| `HERMES_LLM_MODEL` | yes | The model the operator named. No default, same reason. |
| `HERMES_LLM_API_KEY` | | The endpoint's credential. |
| `HERMES_LLM_API_KEY_FILE` | | The same credential from a file, and it **wins** over `HERMES_LLM_API_KEY`, so a production stack can lock it down (ADR 0015). |
| `HERMES_LLM_PARAMS` | | A JSON object of provider parameters, passed to the persona untouched. A parameter set to `null` removes a field the request would otherwise carry, which is how a provider that rejects one is made to work. |
| `HERMES_LLM_TIMEOUT_SECONDS` | | Passed through to the persona. |
| `HERMES_SUGGESTION_TTL_SECONDS` | | How long a suggestion stays approvable (#22). Operator configuration like the model, so it travels the same way; unset leaves the SDK's own default of an hour. |
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

The runtime spawns a **process**, and a persona ships as a **container image**, so the argv the runtime is configured with in tests is `tests/run-persona-image.sh` — a wrapper that runs the image with whatever environment the runtime gave it, forwarding every variable so that an assertion about what a persona does *not* hold is worth making. It also translates SIGTERM into `docker stop`, because `docker run` does not reliably pass a signal on to the container it started.

The deployment ships its own copy of that wrapper — `deploy/docker-compose/run-persona-image.sh`, inside the Hermes image as `run-persona`, and named in `HERMES_PERSONAS`. It does the same two things (forward every variable, translate SIGTERM into `docker stop`) and adds the network the deployment chose. The two files are kept apart on purpose: one is a test fixture, the other ships in an image an operator runs, and a shared file would make a change for one of them a change for both.
