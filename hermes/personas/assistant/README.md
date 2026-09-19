# assistant

The reference persona: it reads the user's incoming messages and drafts a reply for them to approve. It never sends — a suggestion becomes a reply only when a human approval references it.

It is also the proof that the SDK path works from day one (ADR 0008): `assistant.py` is one handler and a prompt, and everything that must not be got wrong belongs to `sdk/python/twalk_sdk` — the consent gate, the contract's envelopes and their deterministic ids, the durable subscription, the headers, the trace.

What is the persona's own judgement, and therefore lives here:

- the system prompt, which is mostly about restraint: what the model returns is shown as a ready-to-send reply, so anything but the reply itself is noise the user has to delete before approving;
- the language of the reply: the prompt asks for the language of the message being answered, not the user's, because a suggestion exists to be sent to someone else (ADR 0016). The second half of that decision — falling back to the user's own language when the model cannot tell — exists since [#164](https://github.com/linagora/twalk/issues/164): the Gateway stores the preference, the runtime injects it as `TWALK_USER_LANGUAGE`, and this persona appends one sentence naming that language *after* the instruction to follow the message's own. The order is the decision: an unambiguous message still follows the message. With no preference set the sentence is absent and nothing is invented — the persona starts, every readable message is answered in its own language, and the SDK says in its log what will happen to the ambiguous ones. The prompt stays in English throughout, and in code rather than in configuration;
- when to stay quiet: v0.1 answers text, so a message with no text is skipped without a suggestion (the `thinking` event still says the persona started, so the activity is visible);
- the sampling: a low temperature, because the same message should get the same draft on a retry;
- the completion budget, which is a ceiling and not a target: 2000 tokens, because a model that reasons before it answers charges its thinking to the same budget and the 300 this asked for first bought nothing but `reasoning_content` ([#162](https://github.com/linagora/twalk/issues/162)). What keeps a suggestion short is the prompt. An operator who wants another number sets `TWALK_LLM_PARAMS={"max_tokens":4000}`, which is merged last and wins.

What is *not* the persona's own judgement, and therefore lives in the SDK: how long a suggestion stays approvable. That window is the operator's (`TWALK_SUGGESTION_TTL_SECONDS`, an hour by default) and it runs from when the suggestion was produced, not from the message it answers — a persona reads the messages that arrived before it was activated (ADR 0013), so a window keyed off the trigger would hand a new user a screen of expired drafts. The attempt counter is the SDK's for the same reason: a trigger the bus redelivers is the same suggestion, keyed the same way, and never a second draft of one message.

## Running it

It ships as a container image — the SDK and its dependencies install inside, since the host has no pip (PEP 668) and a persona is a separate process anyway:

```bash
# from the repository root: the image carries the SDK from sdk/python/
docker build -f hermes/personas/assistant/Dockerfile -t twalk-assistant .
```

It is configured entirely through the environment; `sdk/python/README.md` documents every variable. The minimum is the persona's identity, the deployment's domain, and the endpoint and model the operator chose — there is no default for either, and the persona refuses to start without them (ADR 0015):

```bash
docker run --rm \
  -e TWALK_PERSONA_ID=assistant \
  -e TWALK_HERMES_DOMAIN=twalk.example.com \
  -e TWALK_NATS_URL=nats://nats:4222 \
  -e TWALK_LLM_BASE_URL=http://llm:8080/v1 \
  -e TWALK_LLM_MODEL=qwen2.5-32b-instruct \
  twalk-assistant
```

In a deployment that configuration comes from the Companion Gateway, injected by the runtime (#23, #98); the persona only ever reads its environment.

The Hermes runtime starts and supervises it as a process ([#23](https://github.com/linagora/twalk/issues/23)); until then, running it by hand or through compose is how it runs.

## Tests

`hermes/tests/assistant.rs`, at the persona's process boundary: the real container, driven by publishing contract fixtures to a real bus, with a stub LLM answering a canned completion. Nothing reaches inside the process.
