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

## The disclosure

Every reply a persona drafted reaches the contact with one sentence after it, on a line of its own, in the language the reply was written in — *"Rédigé avec mon assistant IA."*, *"Drafted with my AI assistant."* ([ADR 0019](../../docs/architecture/adr/0019-a-persona-discloses-itself-to-the-contact.md), [ADR 0031](../../docs/architecture/adr/0031-the-disclosure-is-written-by-neither-the-model-nor-the-gateway.md), [#121](https://github.com/linagora/twalk/issues/121)). The author writes none of it, and the example above needed no change to get it.

**The sentences are the contract's**: `contracts/disclosure/v1/sentences.json`, five of them, one per language the Companion ships. `disclosure.py` carries a copy that `tests/test_disclosure.py` pins to the file value for value, and the Companion Gateway reads the same file, so the two components cannot tell a contact two different things. The model never writes the sentence — a model can be argued out of anything — and the sentence never goes inside the body, which the user may edit: it travels as `data.disclosure` on `persona.suggest.produced`, and the Gateway appends it to `final.body` at approval, on by default and switched off only globally, as a recorded decision.

**The language is the model's answer, never a library's guess.** After the handler returns a `Suggestion`, the SDK asks the same model one more question — the reply as the user message, `Answer with exactly one language tag among en, fr, it, es, de, or other.` as the system prompt, five tokens of budget, no temperature — and reads one token back (`Llm.language_of`). That is ADR 0016 applied a second time: detection is the model's job. The cost is stated rather than hidden: **one more, small, completion per suggestion**, billed like the reply's own. An author who *knows* the language — a persona that only ever writes French, or one that asked its model for a structured answer naming it — says so, `Suggestion(body=reply, language="fr")`, and the ask is skipped; `fr-CA` maps to the French sentence, on this path as on the Gateway's.

**A language with no sentence is no suggestion at all.** The model answers `ja`, or `other`: the delivery is terminated with one `ERROR` line — `disclosure has no sentence for the language the model answered: ja` — and never retried, because the same reply gets the same answer and every attempt is billed, exactly as a model that spent its whole budget reasoning is handled. Nothing is published. Falling back to English, or to the user's own language, was rejected in ADR 0031: a disclosure the contact cannot read is a sentence nobody reads, and a named refusal is a contribution request an operator can act on with one line in the contract. The body's cap is `65 536 − 1 − 200` rather than the schema's 65 536 for the same arithmetic the Gateway applies to an edited body: the appended line must never make an approved reply one the contract refuses.

**The ask has a budget, and a reasoning model needs it raised.** The ask carries five tokens (`LANGUAGE_ASK_MAX_TOKENS`) unless `TWALK_LLM_PARAMS` names `max_tokens` — the parameters are merged last, so a budget set there governs the ask exactly as it governs the reply. A model that thinks before it answers — the reference deployment's Qwen behind LiteLLM, the shape [#162](https://github.com/linagora/twalk/issues/162) was found on — spends five tokens thinking and writes no tag, and `completion.py` reports what it reports about the reply: budget spent reasoning, not retried. That would happen on **every** message, after each reply was drafted and billed, so the loop wraps it (`LanguageAskFailed`): the `ERROR` line says it was the *language ask* that exhausted the budget, that the reply was drafted fine, and where the budget is set. The consequence is worth stating plainly, because the #162 advice was "leave `HERMES_LLM_PARAMS` empty unless your model needs more room": on a reasoning model, it is not optional, and `{"max_tokens": 2000}` makes the ask a reasoning pass per suggestion rather than "one more, small, completion" — the cost of asking a thinking model a one-word question. A persona that knows its language (`Suggestion(language="fr")`) pays neither.

## What else the SDK does for you

- **Deterministic ids**, from the contract's natural keys: `thinking` is `sha256(persona_id:trigger_event_id)`, `suggest` is `sha256(persona_id:trigger_event_id:attempt)`. A retry recomputes the same id, so a replay deduplicates on the bus instead of showing the user the same suggestion twice.
- **Contract-valid envelopes** by construction (`envelope.py`): the trigger reference, the `network` and `consent` extensions copied from the trigger, the `dataschema`, the timestamp, and the schemas' own length caps applied on the way out, so a chatty model cannot make a persona publish an event the contract refuses.
- **`Nats-Msg-Id`** set to the event id on every publish, with `network`, `consent` and `traceparent` duplicated as NATS headers, because JetStream filters subjects and headers, not payloads.
- **The trace continued** from the trigger, so one message's trace links sensor → persona → approval → outbound.
- **A durable pull consumer**, so a restart resumes where the persona stopped instead of losing events; explicit acks, and a bounded retry (NAK with a delay, `max_deliver` 3) for the failure that is usually an endpoint briefly away — but **only for a failure a retry could change**, see below.
- **Four named outcomes for one model call** (`completion.py`, [#162](https://github.com/linagora/twalk/issues/162)): an endpoint that could not be reached, one that refused the request, a model that answered nothing, and a model that spent its whole budget reasoning. The last one is why the module exists: a reasoning model asked for a reply-sized budget answers `HTTP 200`, `finish_reason: "length"`, `content: null`, with the budget in `reasoning_content` — the endpoint is healthy, there is no text, and **asking again produces the same answer and another invoice**. So that one is not retried: the delivery is terminated on the first try and the refusal names the remedy (`TWALK_LLM_PARAMS`, `HERMES_LLM_PARAMS` on the runtime that injects it). Reading an answer is pure and tested without a network; `except LlmError` still catches all four.
- **A trigger that produced no suggestion is accounted for**, either way: the non-retryable outcome is terminated with one line naming the trigger, and a retried one that reaches the consumer's redelivery limit says that this was the last attempt instead of leaving the event to disappear. "The persona gave up" and "the bus will hand it over again" are different facts, and only the second one costs money.
- **`thinking` on start**, published the moment processing begins, so oversight can show activity in real time.
- **The disclosure selected, never written** (`disclosure.py`, above): the contract's sentence for the language the reply is in, asked of the model, carried as a field of its own — and a language with no sentence refused by name with no retry.
- **An expiry on every suggestion** (`policy.py`): a draft the user never got to goes stale rather than staying approvable for ever. The window is the operator's (`TWALK_SUGGESTION_TTL_SECONDS`, an hour by default) and is measured from when the suggestion was *produced*, not from the trigger's own time — a persona is activated by a consent decision (ADR 0013) and its consumer starts at the beginning of the stream, so the message it answers can be arbitrarily old.
- **An attempt that counts suggestions, not deliveries**: the bus is at-least-once, so the same trigger reaches a persona again after a crash or a NAK. It is re-keyed to the same attempt, so it recomputes the same id and collapses on the bus instead of offering the user two drafts of one message — one of them from a run that failed halfway. The persona says so in its logs when the bus reports the publish as a duplicate, so an absorbed replay reads differently from a suggestion that never came.

What it deliberately does **not** do: send anything. A persona produces suggestions; a suggestion becomes a reply only when a human approval references it.

## Configuration

Everything is an environment variable, so a persona joins the compose deployment the way the Sensor does. Four are required; the rest have deployment defaults.

Twalk ships no LLM and names no model of its own: a persona **refuses to start** without an endpoint and a model the operator chose (ADR 0015). In a deployment that configuration is held by the Companion Gateway, set from the Companion, read from it by the Hermes runtime and injected into the persona's environment when it spawns it ([#23](https://github.com/linagora/twalk/issues/23), [#98](https://github.com/linagora/twalk/issues/98), [#184](https://github.com/linagora/twalk/issues/184)) — the persona never fetches it, because the Gateway's service token also opens the consent snapshot. From the SDK's side that is invisible: it reads its environment, and it needed no change for the Gateway's half to arrive, which is what "the runtime injects it" was supposed to mean all along.

| Variable | Default | What it is |
| --- | --- | --- |
| `TWALK_PERSONA_ID` | — | The persona's id (`assistant`), and the first half of every deterministic id it produces. Must match the contract's `^[a-z0-9_-]+$`. |
| `TWALK_HERMES_DOMAIN` | — | The authority of the events' `source`: `hermes://<domain>/personas/<persona_id>`. |
| `TWALK_LLM_BASE_URL` | — | Any OpenAI-compatible chat-completions endpoint, e.g. `http://llm:8080/v1`. The operator's choice of endpoint is the only reason message content ever leaves their infrastructure. |
| `TWALK_LLM_MODEL` | — | Sent with every request, and reported in `thinking` so oversight can say what reasoned. |
| `TWALK_LLM_API_KEY` | none | Sent as a bearer token to that endpoint and nowhere else. |
| `TWALK_LLM_PARAMS` | `{}` | A JSON object of provider parameters, merged into every request untouched — because providers differ in what they reject. A parameter set to `null` *removes* a field the request would otherwise carry (`{"temperature": null}`), which is how an endpoint that refuses one of them is made to work. **Every** request, including the language ask (above): merged last, `{"max_tokens": N}` replaces the ask's own five-token budget as well as the reply's. **A reasoning model needs it set** — one that thinks before it answers spends the ask's five tokens thinking, every time, and the persona then refuses every suggestion after drafting each reply, naming the ask and this variable. |
| `TWALK_LLM_TIMEOUT_SECONDS` | `60` | |
| `TWALK_NATS_URL` | `nats://localhost:4222` | |
| `TWALK_BUS_STREAM` | `twalk` | The JetStream stream the events live in. |
| `TWALK_BUS_SUBJECT_PREFIX` | `twalk` | The bus namespace: a contract type `fr.linagora.twalk.<rest>` travels on `<prefix>.<rest>`. `twalk` in every deployment; configurable because a test drives a persona on a namespace of its own, rather than replaying the whole shared history. |
| `TWALK_PERSONA_CONSUMER` | `persona-<persona_id>` | The durable consumer's name. |
| `TWALK_USER_LANGUAGE` | none | The **user's own** language, one of `en`, `fr`, `it`, `es`, `de` — the five the Companion ships and the Gateway stores. It governs one thing: what a persona writes in when it cannot tell what language the message it is answering was written in (ADR 0016). It never governs a suggestion whose language the persona *can* tell. A tag outside the five fails at startup with the five named. **Unset is a supported state and not a default**: the persona starts, answers every readable message in that message's own language, and warns at startup that an ambiguous one — a greeting, a single word, an emoji, a link — will be answered in whatever language the model picks. Refusing to start was the alternative and was rejected; choosing silently was not on the table ([#164](https://github.com/linagora/twalk/issues/164)). |
| `TWALK_SUGGESTION_TTL_SECONDS` | `3600` | How long a suggestion stays approvable, from when it was produced. A whole number of seconds, at least one: there is no "never expires", because an approval that can be given at any later date is what the expiry exists to prevent. |
| `TWALK_LOG_LEVEL` | `info` | |
| `TWALK_HERMES_WEBHOOK_URL` | none | One of **Hermes's** webhook routes (`https://<host>:8644/webhooks/<route>`), and what turns ADR 0032's seam on. Unset — every deployment before that ADR — the persona reasons with the model endpoint above and speaks to nothing outside the deployment. Set, the persona posts a signed wake there and proposes nothing itself: the answer comes back through the Companion Gateway. An `http://` URL is refused at startup unless `TWALK_HERMES_ALLOW_INSECURE_URL` says otherwise, and a URL that is not a route (the origin, `/health`) is refused with the reason. |
| `TWALK_HERMES_WEBHOOK_SECRET` | none | The secret the wake is signed with, and the same value the route is configured with on the Hermes side. Required when the URL is set: Hermes refuses a route that has none, and its `INSECURE_NO_AUTH` escape hatch is for its own tests. It is a credential the persona holds — its own, for its own outbound call, opening nothing on this deployment. |
| `TWALK_HERMES_TIMEOUT_SECONDS` | `10` | How long a wake waits for Hermes's front door. Short on purpose: the adapter answers `202` before the agent runs, so this measures reachability and never reasoning. |
| `TWALK_HERMES_ALLOW_INSECURE_URL` | `false` | Records that an operator accepted a plaintext hop. The HMAC authenticates the sender and not the content, so plaintext gives the user's messages no confidentiality at all; the persona warns on every start naming the URL. |

The persona is a consumer, not the bus's operator: it waits for its stream to exist rather than creating one, and it retries a bus that is not up yet.

## The seam to Hermes

Hermes is Nous Research's agent runtime, outside this project and outside the deployment (ADR 0032). A persona reaches it by posting a **signed webhook**, and its answer returns through the Companion Gateway rather than through that response — the adapter answers `202 Accepted` before the agent runs, so the seam is asynchronous by construction.

Two modules, split the way `completion.py` and `llm.py` are: `webhook.py` decides what crosses and how it is signed and is pure; `hermes.py` owns the socket.

**What crosses is a narrow template of named fields** — ten of them, listed in `webhook.py`, and an eleventh for a mail: its Subject line, which is the sender's own words and travels with the body (#362) — and never the raw event. Hermes keeps memories and nothing expires what it is shown, so a body copied into `MEMORY.md` outlives ADR 0028's seven days on a machine Twalk may not own. The template names the message's words — its body and, for a mail, its Subject line — and the shape of the conversation around them, and withholds everybody's identity: the sender's Matrix ID, their display name, the network's own identifier for them, the portal room, the quoted excerpt that belongs to whoever wrote it, and an attachment's name or decryption material. A persona author cannot widen it, for the same reason they cannot forget the consent gate. `tests/test_webhook.py` asserts the whole key set and then searches the serialised body for each withheld value.

**The signature is Hermes's generic V2**: `X-Webhook-Signature-V2` is the hex HMAC-SHA256 of `<timestamp>.<body>`, with `X-Webhook-Timestamp` in unix seconds and within five minutes of Hermes's clock. V2 rather than V1 because V1 signs the body alone, so a captured request replays for ever.

**The idempotency key is the suggestion's own deterministic id** — `sha256(persona_id:trigger_event_id:attempt)`, which is also `Nats-Msg-Id` on the bus. Hermes caches a delivery id for an hour and the bus deduplicates on that value, so a redelivered trigger cannot become a second agent run or a second draft of one message.

**An unreachable Hermes does not stop the deployment.** The persona probes at startup, logs at `ERROR` naming the URL and what happens instead, starts anyway, and re-probes in the background — the shape the Sensor already uses for the Gateway's consent snapshot. A wake that fails is redelivered by the bus; a wake refused for a reason a retry cannot change (the secret, the route, the body) terminates the delivery with the remedy in the message, exactly as a model that spent its budget reasoning does. A route over its rate limit is its own case, with a delay of its own: see `RATE_LIMIT_DELAY_SECONDS`.

## Packaging

Personas ship as **container images**: the SDK and its two dependencies (`nats-py`, `httpx`) install inside, because the development host has no pip (PEP 668) and no venv module — and because a persona is a separate process talking to the bus (ADR 0008), so an image is what the deployment wants anyway. `hermes/personas/assistant/Dockerfile` is the reference: it copies `sdk/python/twalk_sdk` next to the persona's own module. There is no host installation step and no PYTHONPATH to set.

## Tests

The package's pure half — the gate, the envelopes, the configuration, the disclosure's sentences and the reading of the model's language answer — imports nothing outside the standard library, so its tests run wherever Python does, with no dependency and no container:

```bash
cd sdk/python
python3 -m unittest discover -s tests
```

They assert against the contract's own fixtures (`contracts/cloudevents/v1/`), not against hand-written copies: the ids and envelopes this SDK builds reproduce `persona.thinking.emitted.json` and `persona.suggest.produced.json` exactly, disclosure included, and the sentences equal `contracts/disclosure/v1/sentences.json`.

The halves that need a dependency (`Persona`, `Llm`) are imported on first use, which is what keeps that possible. They are tested where they belong — at the persona's process boundary, against a real bus: `hermes/tests/assistant.rs` drives the real `assistant` container and asserts what appears on the bus, including the language ask the stub model was sent and the refusal when it answers a language the contract has no sentence for.

## Scope

v0.1 is a single completion per message: no tool calling, no multi-turn planning, no streaming, and one suggestion per trigger. The suggestion policy ([#22](https://github.com/linagora/twalk/issues/22)) settles both numbers the contract's `suggest` envelope carries: the expiry, which every suggestion now has, and the attempt, which stays at `FIRST_ATTEMPT` because a redelivery is not a new suggestion. A *deliberate* second attempt — a human asking for a redraft — increments it explicitly, and needs the surface that would ask for one: the runtime's ([#23](https://github.com/linagora/twalk/issues/23), [#24](https://github.com/linagora/twalk/issues/24)). `suggest_event` takes the attempt rather than counting anything, which is the seam that lands on.

Not here yet, and deliberately: **OpenTelemetry spans** for the model call, following the GenAI conventions, off until an OTLP endpoint is configured and with prompt content behind a second switch (ADR 0017, [#99](https://github.com/linagora/twalk/issues/99)). The `traceparent` the SDK already carries from the trigger is what those spans will hang from. Until then the SDK logs event ids, subjects and model names — never a message body, a prompt or a completion.

One consequence of the gate is worth naming here, since ADR 0017 states it as a property of the system: an unconsented message never reaches the model, so it never reaches a trace either.
