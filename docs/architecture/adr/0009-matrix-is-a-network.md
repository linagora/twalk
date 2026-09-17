# Native Matrix traffic is a first-class network

The contract's `network` enum gains `matrix`. A room with no `m.bridge` state event and no ghost-prefixed sender is native Matrix traffic (the bring-your-own-account channel); the Sensor publishes it with `network=matrix` instead of dropping it.

Rationale: v0.1's fourth channel promises to observe the user's existing Matrix rooms. Dropping them silently would violate "no message is ever silently lost" on precisely the path advanced users prefer. Decided pre-freeze, while the change is cheap: one enum value in the schemas, no consumer impact beyond a new possible value.
