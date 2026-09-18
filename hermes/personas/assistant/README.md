# assistant

The reference persona: it reads the user's incoming messages and drafts a reply for them to approve. It never sends — a suggestion becomes a reply only when a human approval references it.

It is also the proof that the SDK path works from day one (ADR 0008): `assistant.py` is one handler and a prompt, and everything that must not be got wrong belongs to `sdk/python/twalk_sdk` — the consent gate, the contract's envelopes and their deterministic ids, the durable subscription, the headers, the trace.

What is the persona's own judgement, and therefore lives here:

- the system prompt, which is mostly about restraint: what the model returns is shown as a ready-to-send reply, so anything but the reply itself is noise the user has to delete before approving;
- the language of the reply: the prompt asks for the language of the message being answered, not the user's, because a suggestion exists to be sent to someone else (ADR 0016). The second half of that decision — falling back to the user's own language when the model cannot tell — needs the language preference the Gateway stores and the runtime injects, so it lands with those ([#101](https://github.com/linagora/twalk/issues/101), [#23](https://github.com/linagora/twalk/issues/23)); the prompt stays in English either way, and in code rather than in configuration;
- when to stay quiet: v0.1 answers text, so a message with no text is skipped without a suggestion (the `thinking` event still says the persona started, so the activity is visible);
- the sampling: a low temperature, because the same message should get the same draft on a retry.

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
