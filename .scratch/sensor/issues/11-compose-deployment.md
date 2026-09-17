# 11: Reference compose deployment

**What to build:** the Sensor joins the reference deployment. A fresh `docker compose up` starts Synapse, NATS JetStream and the Sensor wired together with named volumes, configured through a documented environment file — no manual steps. Within minutes of a bridge inviting the Sensor into a room, events flow on the bus of the demo stack.

**Blocked by:** 10 — Observability.

**Status:** ready-for-agent

- [ ] `docker compose up` starts Synapse, NATS and Sensor together with named volumes
- [ ] The Sensor is configured through a documented `.env.example`, environment only
- [ ] On a fresh deployment, an invited room produces events on the bus without manual intervention
