# 10: Observability

**GitHub:** [linagora/twalk#11](https://github.com/linagora/twalk/issues/11) — canonical on the tracker; this file is the local mirror.

**What to build:** an operator can tell at a glance whether the Sensor is healthy, across the whole pipeline it now covers. Structured logs with a configurable level; metrics for events published, decryption failures, sync lag and outbound send failures; tracing aligned with the contract — inbound events originate a `traceparent`, and outbound sends continue the trace of the approved reply they carry. The process shuts down cleanly on SIGTERM without losing in-flight events.

**Blocked by:** 04 — Megolm decryption; 09 — Outbound approved replies.

**Status:** done

- [x] Structured logs with configurable level
- [x] Metrics cover: events published, decryption failures, sync lag, outbound send failures
- [x] Inbound events originate a valid `traceparent`; outbound sends continue the approved reply's trace
- [x] SIGTERM drains in-flight work and exits cleanly
