# Consent state is owned by the Companion Gateway

The Companion Gateway is the single writer of consent state: it persists it and is the sole producer of `consent.state.changed` events. Messagr and Buzz render consent state and relay user decisions, but never write it. This corrects the v1 schema, which described consent as "tracked by Messagr".

Rationale: a single writer keeps the consent audit trail replayable and unambiguous, and lets Messagr remain a pure interface. Personas consume consent as a read-only gate: they must refuse to process events whose consent is not `granted`.
