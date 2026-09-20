# contracts

Source of truth for every Twalk component: CloudEvents v1 schemas, fixtures, and the persona specification. Nothing here is implementation — if a component and a schema disagree, the schema wins.

This directory is the **bus** contract: what every component publishes and consumes, shared by all of them. A single component's own HTTP surface is described with that component instead, and owned by it — the Companion Gateway's origin is `companion-gateway/openapi.yaml` (OpenAPI 3.1, served by the Gateway at `/openapi.yaml`, checked against the running binary by `companion-gateway/tests/openapi.rs`).

`cloudevents/v1/definitions/` holds what several schemas share, and it is the **one authority** for those lists (ADR 0033): `network.schema.json` names the networks — `email` is one — and `kind.schema.json` names the kinds of connection, the networks plus `calendar`. A schema that needs a network `$ref`s the definition rather than repeating it, and every component that holds a copy — the Sensor's and the Gateway's Rust enums, the Gateway's store constraints and OpenAPI, the Companion's catalogue — has a conformance test that reads the definition and fails on divergence. To add a value, add it here first; the tests then name every copy to update.

Schemas are released under CC0 1.0 to encourage third-party interoperability; the rest of the repo is AGPLv3.

`disclosure/` is the other authority in this directory, alongside the event schemas: `disclosure/v1/sentences.json` names the sentence a persona's reply discloses itself with, one per language (ADR 0019, ADR 0031), read by the Python SDK and the Companion Gateway and carried on two v1 events as the optional `data.disclosure` member.
