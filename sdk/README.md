# sdk

Client libraries for persona authors, first-party and third-party alike. A persona talks to the bus; it never touches Matrix directly.

- `python/` — `twalk_sdk`, the Python SDK ([#21](https://github.com/linagora/twalk/issues/21)). The consent gate, the contract's `persona.*` envelopes and their deterministic ids, the durable bus subscription and the process loop, plus an OpenAI-compatible chat-completions client. The reference persona `assistant` (`hermes/personas/assistant/`) is built on it. See `python/README.md`.
- TypeScript and Rust SDKs: v0.2.
