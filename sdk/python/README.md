# twalk_sdk

Write a Twalk persona without touching NATS or CloudEvents.

A persona is a process that consumes inbound events from the bus and publishes thinking and suggestion events back onto it, under human oversight. This SDK is the part of that which is the same for every persona — and, above all, the part that must not be left to an author to remember.

```python
from twalk_sdk import Persona, Suggestion
from twalk_sdk.llm import system, user

persona = Persona.from_env()


@persona.on_inbound_message
async def draft(message, context):
    if not message.body:
        return None          # nothing to suggest is a valid answer
    reply = await context.llm.complete(
        [system("Draft a reply."), user(message.body)]
    )
    return Suggestion(body=reply)


if __name__ == "__main__":
    persona.run()
```

That is a whole persona. Everything below happens without it being asked for.

## The consent gate

Twalk's promise is that no message is processed without the sender's consent. JetStream cannot filter by message header, so a consumer receives every inbound event and has to decide — and a persona author who forgot to decide would breach the promise silently. So the SDK decides, in `consent.py`, before the handler is called and before the LLM client is touched. A `pending` or `revoked` event produces no event and no completion request at all.

Two properties, both tested:

- The decision reads the envelope's own `consent` extension and nothing else. It never looks inside `data`, so it cannot come to depend on a field that may not be there: a revoked sender's message arrives with no body, no excerpt and no attachment reference (ADR 0012, [#58](https://github.com/linagora/twalk/issues/58)), and the gate drops it before that shape could matter.
- Anything that is not exactly `granted` is refused — a missing extension, a misspelled one, a value a future contract adds — because the safe default when consent cannot be read is to not process the message.

## The trigger-type gate

Beside the consent gate, in `trigger.py`, and there for the same reason: an author cannot forget it. A persona is woken by `inbound.message.received` and by nothing else — an allowlist, so that a type the contract adds later does not start waking personas because nobody thought to exclude it.

It exists because of one event the consent gate structurally cannot stop. A bridge mirrors a conversation in both directions, so the messages the *user* sends from their own phone reach the bus too; on a live WhatsApp account they arrive under a ghost of the user's own network identity (`@whatsapp_lid-…`), not their Matrix ID. [#109](https://github.com/linagora/twalk/issues/109) and [ADR 0018](../../docs/architecture/adr/0018-the-users-own-messages-are-their-own-event-type.md) settled the shape: they are their own type, `outbound.message.sent`, with the operator's Matrix ID as the subject and **no `consent` extension at all** — the extension is a contact's decision, and there is no contact in that event. So a gate that reads consent has nothing to read; and under the alternative the ADR rejected, where the user's traffic stayed inbound, the sender would have been the user, the most consenting subject in the system. Either way consent says yes to answering the operator. The type is the only attribute that tells the two apart, which is why this gate runs first, and why the log line it writes names the type rather than blaming a consent state that does not exist.

The event is still published, and a persona may still subscribe to it deliberately: a persona that cannot see the user has already replied would suggest answers to closed conversations. What it may not do is be *woken* by one.

[#147](https://github.com/linagora/twalk/issues/147) and [ADR 0021](../../docs/architecture/adr/0021-the-owner-is-never-a-contact-on-any-event.md) made `outbound.*` a family: the user's own reaction is `outbound.reaction.added`, on the same terms. The gate needed no edit to exclude it, which is the point — an allowlist excludes a new type by default. It is also the reason not to replace the allowlist with a rule that excludes `outbound.*`: a rule that names what is refused is a denylist, and a denylist admits whatever nobody remembered to name, which is precisely the failure this gate exists to prevent.

## What else the SDK does for you

- **Deterministic ids**, from the contract's natural keys: `thinking` is `sha256(persona_id:trigger_event_id)`, `suggest` is `sha256(persona_id:trigger_event_id:attempt)`. A retry recomputes the same id, so a replay deduplicates on the bus instead of showing the user the same suggestion twice.
- **Contract-valid envelopes** by construction (`envelope.py`): the trigger reference, the `network` and `consent` extensions copied from the trigger, the `dataschema`, the timestamp, and the schemas' own length caps applied on the way out, so a chatty model cannot make a persona publish an event the contract refuses.
- **`Nats-Msg-Id`** set to the event id on every publish, with `network`, `consent` and `traceparent` duplicated as NATS headers, because JetStream filters subjects and headers, not payloads.
- **The trace continued** from the trigger, so one message's trace links sensor → persona → approval → outbound.
- **A durable pull consumer**, so a restart resumes where the persona stopped instead of losing events; explicit acks, and a bounded retry (NAK with a delay, `max_deliver` 3) for the failure that is usually an endpoint briefly away.
- **`thinking` on start**, published the moment processing begins, so oversight can show activity in real time.

What it deliberately does **not** do: send anything. A persona produces suggestions; a suggestion becomes a reply only when a human approval references it.

## Configuration

Everything is an environment variable, so a persona joins the compose deployment the way the Sensor does. Four are required; the rest have deployment defaults.

Twalk ships no LLM and names no model of its own: a persona **refuses to start** without an endpoint and a model the operator chose (ADR 0015). In a deployment that configuration is held by the Companion Gateway, set from the Companion and injected into the persona's environment by the Hermes runtime when it spawns it (#23, #98) — the persona never fetches it, because the Gateway's service token also opens the consent snapshot. From the SDK's side that is invisible: it reads its environment.

| Variable | Default | What it is |
| --- | --- | --- |
| `TWALK_PERSONA_ID` | — | The persona's id (`assistant`), and the first half of every deterministic id it produces. Must match the contract's `^[a-z0-9_-]+$`. |
| `TWALK_HERMES_DOMAIN` | — | The authority of the events' `source`: `hermes://<domain>/personas/<persona_id>`. |
| `TWALK_LLM_BASE_URL` | — | Any OpenAI-compatible chat-completions endpoint, e.g. `http://llm:8080/v1`. The operator's choice of endpoint is the only reason message content ever leaves their infrastructure. |
| `TWALK_LLM_MODEL` | — | Sent with every request, and reported in `thinking` so oversight can say what reasoned. |
| `TWALK_LLM_API_KEY` | none | Sent as a bearer token to that endpoint and nowhere else. |
| `TWALK_LLM_PARAMS` | `{}` | A JSON object of provider parameters, merged into every request untouched — because providers differ in what they reject. A parameter set to `null` *removes* a field the request would otherwise carry (`{"temperature": null}`), which is how an endpoint that refuses one of them is made to work. |
| `TWALK_LLM_TIMEOUT_SECONDS` | `60` | |
| `TWALK_NATS_URL` | `nats://localhost:4222` | |
| `TWALK_BUS_STREAM` | `twalk` | The JetStream stream the events live in. |
| `TWALK_BUS_SUBJECT_PREFIX` | `twalk` | The bus namespace: a contract type `fr.linagora.twalk.<rest>` travels on `<prefix>.<rest>`. `twalk` in every deployment; configurable because a test drives a persona on a namespace of its own, rather than replaying the whole shared history. |
| `TWALK_PERSONA_CONSUMER` | `persona-<persona_id>` | The durable consumer's name. |
| `TWALK_LOG_LEVEL` | `info` | |

The persona is a consumer, not the bus's operator: it waits for its stream to exist rather than creating one, and it retries a bus that is not up yet.

## Packaging

Personas ship as **container images**: the SDK and its two dependencies (`nats-py`, `httpx`) install inside, because the development host has no pip (PEP 668) and no venv module — and because a persona is a separate process talking to the bus (ADR 0008), so an image is what the deployment wants anyway. `hermes/personas/assistant/Dockerfile` is the reference: it copies `sdk/python/twalk_sdk` next to the persona's own module. There is no host installation step and no PYTHONPATH to set.

## Tests

The package's pure half — the gate, the envelopes, the configuration — imports nothing outside the standard library, so its tests run wherever Python does, with no dependency and no container:

```bash
cd sdk/python
python3 -m unittest discover -s tests
```

They assert against the contract's own fixtures (`contracts/cloudevents/v1/`), not against hand-written copies: the ids and envelopes this SDK builds reproduce `persona.thinking.emitted.json` and `persona.suggest.produced.json` exactly.

The halves that need a dependency (`Persona`, `Llm`) are imported on first use, which is what keeps that possible. They are tested where they belong — at the persona's process boundary, against a real bus: `hermes/tests/assistant.rs` drives the real `assistant` container and asserts what appears on the bus.

## Scope

v0.1 is a single completion per message: no tool calling, no multi-turn planning, no streaming. The suggestion policy — a second attempt for the same trigger, and the expiry that goes with it — is [#22](https://github.com/linagora/twalk/issues/22); this SDK builds the envelope that extends (`FIRST_ATTEMPT`, and no `expires_at`).

Not here yet, and deliberately: **OpenTelemetry spans** for the model call, following the GenAI conventions, off until an OTLP endpoint is configured and with prompt content behind a second switch (ADR 0017, [#99](https://github.com/linagora/twalk/issues/99)). The `traceparent` the SDK already carries from the trigger is what those spans will hang from. Until then the SDK logs event ids, subjects and model names — never a message body, a prompt or a completion.

One consequence of the gate is worth naming here, since ADR 0017 states it as a property of the system: an unconsented message never reaches the model, so it never reaches a trace either.
