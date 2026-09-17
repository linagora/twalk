# tests

End-to-end and CloudEvents conformance tests, plus the harness every
component's suite is built on.

- `harness/` — the `twalk-test-harness` crate: the test stack's lifecycle
  (`compose.test.yaml`, a real Synapse and a real NATS JetStream), the
  `Bus`, contract validation against `contracts/cloudevents/v1/`,
  `poll_until`, and a stub OpenAI-compatible LLM for persona tests. Consumed
  as a dev dependency by `sensor/` and `hermes/`; component-specific helpers
  (the Matrix `Bot`, `SensorProc`) stay with their component. Its own logic
  (`poll_until`, the stub LLM) is unit-tested in place: `cd harness && cargo
  test` needs no Docker.

Every event fixture in `contracts/` must validate against its schema here.
