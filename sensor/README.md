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

The table keys on the *sender*, which is right for every field an event carries except two. A reply's `reply_to.excerpt` and a reaction's `target.excerpt` quote a message **somebody else** wrote, and in a group that is routinely a different contact — so an excerpt published on the sender's label alone travels under a label that is not its author's, past the consumer gates that read that one label ([#110](https://github.com/linagora/twalk/issues/110)). Both decisions therefore have to be in the right state: an excerpt is published only when the sender's own state does not reduce publication **and** the author of the quoted message is `granted`. Anything short of a grant withholds it — an author the Sensor cannot resolve to a contact, a message it cannot fetch or decrypt, and `pending`, which in its consent cache is the absence of a decision rather than a weaker yes. The owner is the exception: the user's own messages, and the replies the Sensor sends for them, are quoted as they always were — there is no decision about the user to consult. Today that is the Sensor's own Matrix ID alone: the user's network ghosts still resolve as contacts, so a message they sent from their phone is quoted like anybody else's until [ADR 0018](../docs/architecture/adr/0018-the-users-own-messages-are-their-own-event-type.md) lands ([#109](https://github.com/linagora/twalk/issues/109)).

What is withheld is the quotation and never the event: the reaction keeps its emoji, the reply keeps the sender's own body, and both keep the reference to the message they point at. A reaction's `target.excerpt` is simply absent, which the contract allows unconditionally; a reply's `reply_to.excerpt` is present and **empty**, because the contract requires the field from any sender not `revoked` — the same empty excerpt the Sensor has always published for a parent it could not read.

So an operator can answer the two questions a revocation raises: the user keeps the evidence that a message arrived (a silent contact still looks different from a broken bridge), and nothing the contact sends is collected any more. Revocation applies to the future only — events already on the bus stay until retention expires, and erasing history is a separate, explicit action.

## Two identities: one observes, one acts

The Sensor holds **two** Matrix clients, and confusing them would undo most of what the rest of this document promises.

`@sensor:` is the one that **observes**. It syncs portal rooms, decrypts their events and publishes them, on the terms above. Its membership of a room is what the portal register reads as `observing` ([ADR 0024](../docs/architecture/adr/0024-a-portal-room-is-observed-by-invitation-per-conversation.md)), and nothing on this page changes for it.

The second is a device of the **owner's own account** (`SENSOR_OWNER_DEVICE_ACCESS_TOKEN` and `SENSOR_OWNER_DEVICE_ID`, [ADR 0025](../docs/architecture/adr/0025-twalk-acts-as-the-user-through-a-device-of-their-account.md), [#123](https://github.com/linagora/twalk/issues/123)), and it is the one that **acts**. It exists because a mautrix bridge relays to its network only what the logged-in user's own Matrix account sends: a reply from `@sensor:` is ignored, without a log line, so the outbound half of this product used to return an event id and deliver nothing. It is write-only — it joins portal rooms and posts approved replies, it registers no event handler, it publishes nothing on the bus, and it reads no history, which is why it needs neither cross-signing nor a recovery key. Its stores live in their own subdirectory of `SENSOR_STATE_DIR`, because a crypto store belongs to one device.

It joins **only** a room a bridge bot named in `SENSOR_BRIDGE_BOTS` invited it to. The inviter is the one authenticated fact in an invitation — the room id, the room's name and its `m.bridge` marker are all chosen by whoever sent it — and this device posts messages, so nothing else is enough.

Both halves are unset-by-default, and the degradation is stated rather than implied. With no owner device the Sensor says so once at startup, naming #123, and then says per reply what the reply reached:

| Posted by | Into | What the Sensor reports |
| --- | --- | --- |
| the owner's device | a portal room it has joined | `reach: contact` — the bridge relays it, because it really is the user's |
| `@sensor:` | a portal room | `reach: nobody` — the bridge ignores it and the contact receives nothing ([#216](https://github.com/linagora/twalk/issues/216)) |
| `@sensor:` | a room no bridge marked | `reach: contact` — native Matrix traffic ([ADR 0009](../docs/architecture/adr/0009-matrix-is-a-network.md)) has no bridge to ignore it |

The report is the approval event, unchanged, republished on `twalk.persona.reply.approved.v1.posted` with `reach` and `posted-as` headers — a sibling of the dead-letter subject, so no contract schema had to grow a field for it. It is published on every successful post and not only on the failures, because a signal that exists only in the bad case makes the good case a silence. The same answer is counted as `twalk_sensor_outbound_replies_total{reach}`.

When an owner device is configured and a reply targets a **portal** room it has not joined, nothing is posted: the send fails transiently, is retried, and ends on the dead-letter subject if the device never joins. Posting as `@sensor:` there would produce an event id and total silence, which is the outcome all of this exists to remove.

## Tests

The integration tests (ticket 01) live in `tests/`. They boot a real Synapse and a real NATS JetStream via docker compose and verify behaviour at the process boundary; bots play the role of bridges over the Matrix client-server API.

What every component's suite shares — the test stack's lifecycle, the `Bus`, contract validation, `poll_until` — lives in the `twalk-test-harness` crate (`../tests/harness/`, ticket #20); `tests/harness/` keeps the Sensor's own half (the Matrix `Bot`, the portal helpers, `SensorProc`) and re-exports the crate, so test files see one flat `harness::` namespace.

```bash
cargo test   # boots the stack itself; ~15s cold, ~3s warm
```

Requires Docker and a Rust toolchain. The implementation lands from ticket 02 onwards (see `.scratch/sensor/issues/`).
