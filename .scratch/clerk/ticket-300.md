## Parent

Part of [ADR 0032](../blob/main/docs/architecture/adr/0032-twalk-governs-what-an-agent-outside-it-may-see-and-do.md); a follow-up of #284, which gave the clerk a device of the owner on the Companion Gateway and used it only to **write** (`POST /api/approvals`). ADR 0035's last sentence already points here: a decision the Gateway records is "where the clerk can read it from rather than remember it".

## What to build

The device also **reads**, and two rough edges left by #265/#284 want exactly that read — `GET /api/suggestions/{id}`, one call per post at posting time, never per tick, so the "no state" shape holds:

1. **The delivery line on a post.** Every post in `approbations` still carries a placeholder ("Livraison : non lue par le greffier — l'écran Approbations la connaît"). #216's `delivery` (`can_reach` / `cannot_reach` / `unknown`, the owner's account's standing in the trigger's room) is the one certainty an owner about to ✅ should see — a ✅ on a `cannot_reach` post is a reply that reaches nobody. The line is written at posting time from the Gateway's answer; when the Gateway does not answer, the line says so rather than guessing, and the post still goes up (the read half must not depend on the write half's credential being alive).
2. **A redelivered suggestion already approved.** The bus can redeliver a `persona.suggest.produced` after the clerk approved and deleted its post (final review of #284, T5-M4): today the clerk re-posts it and the next ✅ gets `already_approved` and deletes it again — self-healing, but a post the owner sees twice. `standing: approved` on `GET /api/suggestions/{id}` says not to post at all.

Both are **write-half only**: without `CLERK_GATEWAY_SESSION_FILE` the clerk has no device and posts as #265 did.

## Acceptance criteria

- [ ] A post for a trigger whose room the owner's account cannot reach carries the Companion's own `cannot_reach` sentence; `can_reach` and `unknown` each their own; a Gateway that does not answer within the request timeout yields "non lue" and the post still goes up within the same tick.
- [ ] A suggestion redelivered after its approval produces no second post, counted (`twalk_clerk_skipped_total{why="already_approved"}`).
- [ ] Neither read makes the clerk hold anything beyond the tick: proven by a restart between the read and the next tick.
- [ ] `clerk/tests/decisions.rs` (stub Gateway, `GET /api/suggestions/{id}` scripted) covers all three delivery answers and the redelivered case; the deployment suite asserts one real `cannot_reach` post.

## Blocked by

#284. Related: #216, #265, ADR 0035.
