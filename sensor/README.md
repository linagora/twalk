# sensor

The Sensor service (Rust): a Matrix client that decrypts portal-room events end-to-end, enriches them with contact and channel context, and publishes them as typed CloudEvents on the bus.

## Tests

The integration tests (ticket 01) live in `tests/`. They boot a real Synapse and a real NATS JetStream via docker compose and verify behaviour at the process boundary; bots play the role of bridges over the Matrix client-server API.

What every component's suite shares — the test stack's lifecycle, the `Bus`, contract validation, `poll_until` — lives in the `twalk-test-harness` crate (`../tests/harness/`, ticket #20); `tests/harness/` keeps the Sensor's own half (the Matrix `Bot`, the portal helpers, `SensorProc`) and re-exports the crate, so test files see one flat `harness::` namespace.

```bash
cargo test   # boots the stack itself; ~15s cold, ~3s warm
```

Requires Docker and a Rust toolchain. The implementation lands from ticket 02 onwards (see `.scratch/sensor/issues/`).
