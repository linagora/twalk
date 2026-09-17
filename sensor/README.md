# sensor

The Sensor service (Rust): a Matrix client that decrypts portal-room events end-to-end, enriches them with contact and channel context, and publishes them as typed CloudEvents on the bus.

## Consent, and what reaches the bus

The Sensor never writes consent state — the Companion Gateway does, and the decisions reach the Sensor on the bus — but it decides what a published event contains, from the sender's current state ([ADR 0012](../docs/architecture/adr/0012-revoked-consent-reduces-publication.md)):

| Sender's consent | What the Sensor publishes |
| --- | --- |
| `granted` | The full event, contact `network_identifier` included. |
| `pending` | The same full event, labelled `pending`, minus the network identifier. The body **is** on the bus: consumers must refuse to process it. Unchanged by ADR 0012. |
| `revoked` | A **reduced** event: same deterministic id, network, consent label, both timestamps, room and sender references, reply and thread relations, attachments reduced to their shape — and no content. |

For a revoked sender, "no content" means: no `data.body`, no `reply_to.excerpt` and no reaction `target.excerpt` (an excerpt quotes a message, so it is content too — and the Sensor does not even fetch the quoted message), and inside each attachment no `mxc://` reference, no decryption material and no caption. What an attachment keeps is `kind`, `mime_type`, `size_bytes`, `dimensions` and `duration_ms`: they describe the message without carrying it. A filename is content — people name files `contrat-signé.pdf` — and the contract's attachment shape has none: for a media message a filename would only ever travel as the body or the caption, both dropped.

So an operator can answer the two questions a revocation raises: the user keeps the evidence that a message arrived (a silent contact still looks different from a broken bridge), and nothing the contact sends is collected any more. Revocation applies to the future only — events already on the bus stay until retention expires, and erasing history is a separate, explicit action.

## Tests

The integration tests (ticket 01) live in `tests/`. They boot a real Synapse and a real NATS JetStream via docker compose and verify behaviour at the process boundary; bots play the role of bridges over the Matrix client-server API.

What every component's suite shares — the test stack's lifecycle, the `Bus`, contract validation, `poll_until` — lives in the `twalk-test-harness` crate (`../tests/harness/`, ticket #20); `tests/harness/` keeps the Sensor's own half (the Matrix `Bot`, the portal helpers, `SensorProc`) and re-exports the crate, so test files see one flat `harness::` namespace.

```bash
cargo test   # boots the stack itself; ~15s cold, ~3s warm
```

Requires Docker and a Rust toolchain. The implementation lands from ticket 02 onwards (see `.scratch/sensor/issues/`).
