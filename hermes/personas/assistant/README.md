# assistant

The reference persona: it reads the user's incoming messages and drafts a reply for them to approve. It never sends — a suggestion becomes a reply only when a human approval references it.

It is also the proof that the SDK path works from day one (ADR 0008): `assistant.py` is one handler and a prompt, and everything that must not be got wrong belongs to `sdk/python/twalk_sdk` — the consent gate, the contract's envelopes and their deterministic ids, the durable subscription, the headers, the trace.

What is the persona's own judgement, and therefore lives here:

- the system prompt, which is mostly about restraint: what the model returns is shown as a ready-to-send reply, so anything but the reply itself is noise the user has to delete before approving;
- when to stay quiet: v0.1 answers text, so a message with no text is skipped without a suggestion (the `thinking` event still says the persona started, so the activity is visible);
- the sampling: a low temperature, because the same message should get the same draft on a retry.

## Running it

It ships as a container image — the SDK and its dependencies install inside, since the host has no pip (PEP 668) and a persona is a separate process anyway:

```bash
# from the repository root: the image carries the SDK from sdk/python/
docker build -f hermes/personas/assistant/Dockerfile -t twalk-assistant .
```

It is configured entirely through the environment; `sdk/python/README.md` documents every variable. The minimum is the persona's identity, the deployment's domain and an OpenAI-compatible endpoint:

```bash
docker run --rm \
  -e TWALK_PERSONA_ID=assistant \
  -e TWALK_HERMES_DOMAIN=twalk.example.com \
  -e TWALK_NATS_URL=nats://nats:4222 \
  -e TWALK_LLM_BASE_URL=http://llm:8080/v1 \
  twalk-assistant
```

The Hermes runtime starts and supervises it as a process ([#23](https://github.com/linagora/twalk/issues/23)); until then, running it by hand or through compose is how it runs.

## Tests

`hermes/tests/assistant.rs`, at the persona's process boundary: the real container, driven by publishing contract fixtures to a real bus, with a stub LLM answering a canned completion. Nothing reaches inside the process.
