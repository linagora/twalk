# 11: Reference compose deployment

**GitHub:** [linagora/twalk#12](https://github.com/linagora/twalk/issues/12) — canonical on the tracker; this file is the local mirror.

**What to build:** the Sensor joins the reference deployment. A fresh `docker compose up` starts Synapse, NATS JetStream and the Sensor wired together with named volumes, configured through a documented environment file — no manual steps. Within minutes of a bridge inviting the Sensor into a room, events flow on the bus of the demo stack.

**Blocked by:** 10 — Observability.

**Status:** done

- [x] `docker compose up` starts Synapse, NATS and Sensor together with named volumes
- [x] The Sensor is configured through a documented `.env.example`, environment only
- [x] On a fresh deployment, an invited room produces events on the bus without manual intervention
