## Parent

Part of [ADR 0032](../blob/main/docs/architecture/adr/0032-twalk-governs-what-an-agent-outside-it-may-see-and-do.md); the write half of the clerk, whose read half is #265. Decided with the owner on 2026-09-20.

## What to build

A ✅ on a post in `approbations`, or a reply in its thread, becomes **the** approval — the Gateway's, through `POST /api/approvals` — and Buzz never decides anything.

**The gesture.** `approbations` is a forum: a suggestion is a post with a thread (#265). A **✅ reaction** on the post by the owner approves the suggestion as written; a **reply in the thread** by the owner approves it with that text (the edit #100 allows); a **❌ reaction** is the Companion's *Refuser*, local by nature — the post is deleted, nothing reaches the Gateway. No vote (a ranking, not a decision) and no magic word ("oui" under "on dit 20h ?" is ambiguous).

**Who may.** The clerk accepts the owner's key **alone**, from configuration (`CLERK_OWNER_PUBKEY`) and never read off the relay: a compromised relay must not be able to name a second approver, and an `admin` of the relay is not the owner. A ✅ by any other key does nothing, is answered in the thread ("seul le propriétaire approuve"), and is counted.

**The credential.** The clerk is a **device of the owner** on the Gateway (#52), named `Buzz`: it appears in the dashboard's device list, is revoked there like any other, and the approval row reads `approved_by: <owner>` through that device. It is created once by an operator route, `provision-clerk-device.sh` — the owner's Matrix password read at the terminal (never an argument, never echoed), a Matrix OpenID token, `POST /api/session`, the refresh token written into the clerk's env file at mode 0600 — and kept alive by `POST /api/session/refresh` (a device token lives 15 minutes, its refresh token 30 days). Idempotent: a device `Buzz` already present is not duplicated, for #227's reason (a credential nobody knows exists is one nobody revokes). A clerk asleep for over 30 days needs the route again, and says so. Deliberately **not** a second service token (a second class of credential, `approved_by` fixed by configuration) and **not** NIP-98 on the Gateway (a Nostr verifier on the write path, for one client).

**What the clerk does with the answer.** The refusal vocabulary is the Gateway's (#24, #100, #97 — "two doors onto one fact must not teach a client two vocabularies"): `consent_revoked`, `suggestion_expired`, `suggestion_already_approved`, `410 suggestion_out_of_reach`… are answered in the thread with the Companion's own sentences (`refusal.ts` has one and a next step for each code, fr/en), in the owner's interface language (`CLERK_USER_LANGUAGE`), not the reply's. On success the post is **deleted** (#219: after a decision the text has done its job) and `journal` gets its line when the Sensor's `.posted` report arrives (#265). The owner's own reply in the thread is theirs (ADR 0028: the owner's words, ninety days) and stays, now without a root — `journal` records "approved, edited"; how Buzz Desktop renders an orphaned reply is an acceptance point, not an assumption.

**Recovery.** The reaction is the retry: a ✅ already present when the clerk starts is a decision to carry, and `suggestion_already_approved` is the Gateway's normal answer to a duplicate, treated as success (post deleted). A Gateway that does not answer is retried with backoff for the suggestion's remaining lifetime, then the thread says "non enregistrée — réagis de nouveau".

## Acceptance criteria

- [ ] A ✅ by the owner's key results in exactly one row in the Gateway's `approval` table, `approved_by` the owner and the device `Buzz`, and in nothing else; the post is gone within seconds.
- [ ] A reply in the thread by the owner's key results in an approval whose `edited` is true and whose reply body is the reply's text; a redelivery of that reply event does not send twice.
- [ ] A ✅ by any other key — a relay `admin` included — results in no Gateway call, a thread reply, and a count.
- [ ] Each Gateway refusal is answered in the thread with the Companion's sentence for that code; a suite asserts the vocabulary against `companion-gateway/openapi.yaml` the way `refusal.test.ts` does, in both directions.
- [ ] The device is listed on the dashboard as `Buzz`; revoking it there makes the next ✅ a thread reply naming the revocation, and `provision-clerk-device.sh` run again restores it without a second device.
- [ ] The clerk's container environment holds a refresh token and nothing else of the Gateway's: not the service token, not a device token at rest beyond its 15 minutes.
- [ ] `/metrics` renders `twalk_clerk_approvals_total{outcome}` with the Gateway's outcome vocabulary plus `not_the_owner` and `gateway_unreachable`.
- [ ] `deploy/README.md` names `provision-clerk-device.sh` beside `provision-owner-device.sh` and says which device each creates (a Gateway device, a Matrix device).

## Blocked by

#265 (the clerk, its key, its channels, its projection of the relay). Related: #52, #24, #100, #216, #219, #227.

