# Network, channel, and bridge are three distinct terms

The CloudEvents `network` extension names the external messaging service as the user experiences it (`whatsapp`, `signal`, `telegram`, `discord`, `sms`) — never a transport. `gmessages` was removed from the v1 enum: SMS carries `network=sms` whether it transits through mautrix-gmessages (v0.1 proof of concept) or the SMS Companion (v0.2), and the transport is identified by `bridge_id` in `bridge.status.changed` events. "Channel" is tolerated in user-facing copy only.

Rationale: a network outlives its transports. When a user's SMS path migrates from mautrix-gmessages to the SMS Companion, consumers of the bus must observe no change at the contract level. (ADR numbers 0001–0004 are reserved for the decisions already referenced from the README and wireframes; this is the first ADR written in this repo.)
