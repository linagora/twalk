# sensor

The Sensor service (Rust): a Matrix client that decrypts portal-room events end-to-end, enriches them with contact and channel context, and publishes them as typed CloudEvents on the bus.

## Tests

The integration-test harness (ticket 01) lives in `tests/`. It boots a real Synapse and a real NATS JetStream via docker compose and verifies behaviour at the process boundary; bots play the role of bridges over the Matrix client-server API.

```bash
cargo test   # boots the stack itself; ~15s cold, ~3s warm
```

Requires Docker and a Rust toolchain. The implementation lands from ticket 02 onwards (see `.scratch/sensor/issues/`).
