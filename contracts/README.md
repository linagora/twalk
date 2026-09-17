# contracts

Source of truth for every Twalk component: CloudEvents v1 schemas, fixtures, and the persona specification. Nothing here is implementation — if a component and a schema disagree, the schema wins.

This directory is the **bus** contract: what every component publishes and consumes, shared by all of them. A single component's own HTTP surface is described with that component instead, and owned by it — the Companion Gateway's origin is `companion-gateway/openapi.yaml` (OpenAPI 3.1, served by the Gateway at `/openapi.yaml`, checked against the running binary by `companion-gateway/tests/openapi.rs`).

Schemas are released under CC0 1.0 to encourage third-party interoperability; the rest of the repo is AGPLv3.
