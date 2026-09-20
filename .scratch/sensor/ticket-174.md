## Problem

The bus keeps every event for ever, and nobody decided that. `sensor/src/main.rs:241` creates the stream with `..Default::default()`, so the retention policy is NATS JetStream's defaults rather than a choice this project made.

Read off the live reference deployment's own store (`/var/lib/docker/volumes/twalk_nats-data/_data/jetstream/$G/streams/twalk/meta.inf`) on 2026-09-19:

```
max_age            0        nothing ever expires
max_bytes          -1       no size ceiling
max_msgs           -1       no count ceiling
storage            file
compression        none     CloudEvents JSON stored raw
num_replicas       1
duplicate_window   120s     Nats-Msg-Id dedup covers two minutes
```

Measured growth: the stream was created 2026-09-18T01:06:46Z and held **2.2 MB across ~1,530 events** 27 hours later — about **1,400 events/day at ~1.4 KB each, so ~2 MB/day**. Roughly 95% of that volume is the defect in #152 (two bridge bots produced 1,150 of 1,216 presence events); a real account's traffic after that fix is nearer 1 MB/day.

Two MB a day is not the problem. A self-hosted appliance whose store has no ceiling is: this host reached 100% disk three times on 2026-09-18, and a deployment that fills a user's disk because its event log has no policy is a defect of design, not of capacity.

## Decision taken

The operator decided on 2026-09-19, with the measurements above in hand:

- **`max_age` = 90 days**, **`max_bytes` = 2 GB**, `discard = old`. Ninety days of real traffic is ~90 MB, so the ceiling is not the working limit — it exists so that the user's disk is never the thing that gives way. Long enough that a replay means something, short enough that the bus does not silently become an archive of other people's messages that nobody chose to keep.
- **`compression` = `s2`.** CloudEvents JSON compresses several-fold, no consumer is affected, and it applies to new blocks.
- **`duplicate_window` = 24 hours**, which is a correctness fix and not a storage one — see below.

## What to build

- The stream's configuration becomes explicit and reviewable, with every field named rather than inherited from `Default`. A reader of that call must be able to see the retention policy without knowing NATS's defaults.
- The values are the operator's to set, with the decided ones as defaults, and documented in `.env.example` wherever the deployment's other knobs are.
- **An ADR.** This is hard to reverse (expired events are gone), surprising without context (an event hub whose events expire), and a real trade-off against replay and against the reduced-publication rules in ADR 0012. It must say plainly that the bus holds contacts' messages and that retention is therefore a personal-data decision, not just a disk one.
- Changing the configuration of a stream that already exists is its own problem: `get_or_create_stream` does not reconcile an existing stream's config. Say what happens on an already-running deployment — whether the configuration is updated in place, and what the operator sees if it cannot be.

## The duplicate window is a correctness defect, not a tuning knob

The Sensor publishes with a `Nats-Msg-Id` derived from the event's content, so a re-publication of the same Matrix event is meant to be dropped by the bus. JetStream only remembers those ids for `duplicate_window`, which is **two minutes**.

A Sensor restart re-syncs from Matrix, and the live logs show it re-processing sync responses. Any event it republishes more than two minutes after the first publication is **not** deduplicated: it lands twice, with two stream sequences, and every consumer sees it twice — a persona triggered twice on one message, a contact counted twice. Two minutes protects against a retry in flight and nothing else, which is not what the dedup was for.

Acceptance criterion worth stating separately: a test that publishes the same event id, waits past the old two-minute window, publishes again, and proves the bus holds one copy.

## Notes

The projections are not the problem and should not be changed here. `companion-gateway/src/suggestions.rs` reads a bounded window at the end of the stream, and `contacts.rs` uses `DeliverPolicy::All` once on a cold start and then follows its own durable consumer. What retention changes for them is what a cold start can still see, which is exactly what the ADR has to say out loud: after 90 days, a Gateway installed fresh cannot rebuild a contact list older than that.

