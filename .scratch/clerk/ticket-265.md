## Parent

Part of [ADR 0032](../blob/main/docs/architecture/adr/0032-twalk-governs-what-an-agent-outside-it-may-see-and-do.md): Buzz is a **surface** for approval, never its authority. This ticket is the surface's read half; #284 is the write half and is blocked by this one. Decided with the owner on 2026-09-20, question by question; the answers are the decisions below.

## Where this comes from

On 2026-09-19 the reference deployment got its Buzz relay (`https://buzz.maudet.cloud`, `restricted_writes`), Hermes as a member under its own key (#240), and the owner's four channels (#263): `approbations` (forum), `activite`, `hermes`, `journal`. Hermes answers the owner in `hermes`. **The other three are empty and nothing feeds them.**

## The clerk

A new Twalk component, **the clerk** (*le greffier*), and its glossary entry, which the PR adds to `CONTEXT.md` under *Ecosystem*, after **Buzz**:

> **Clerk**: The Twalk component that writes onto Buzz what the bus says and carries to the Companion Gateway what the owner decides there. A surface and never an authority (ADR 0032): it holds no consent snapshot, no store, no key of Hermes's, and no credential the Gateway would take for anyone but the owner's own device.
> _Avoid_: "Buzz bot", "the Twalk agent", "herald".

It is the shape of the pending-contact projection (#54) and of the portal register (#105): a consumer of the bus with a narrow view and, here, **no store at all** — see the decision on state below.

## What to build

A Cargo package `clerk/` (Rust, the `nostr` crate for keys, signing and NIP-98 — the CLI stays the operator's tool), shipped as a compose service `clerk` on the host's network namespace like `hermes`, with `/health` and `/metrics`. It consumes the bus with durable consumers and writes into the owner's channels **under a Nostr key of its own**, a third identity beside the owner's and Hermes's, added to the relay (`./run.sh add-member`) and to each channel (`--role bot`) by the owner. `provision-hermes-nostr-key.sh` is renamed `provision-nostr-key.sh` (a symlink keeps the old name) because it takes an env file and is not Hermes's; `provision-buzz-channels.sh` gains `--bot <pubkey>` so the owner adds the clerk the way it adds Hermes.

**`approbations`** — one forum post per `persona.suggest.produced`, whoever produced it (the reference persona or Hermes's answer through the Gateway), and never two for one suggestion:

```
Title:  Réponse proposée · WhatsApp · expire à 23:41
Body:   « Oui, à 20h ! »                                   ← suggestion.body, verbatim
        Peut atteindre le contact : votre compte est dans la conversation.   ← delivery (#216)
        ✅ envoyer tel quel · ❌ refuser · répondre ici pour envoyer un autre texte
        twalk:suggestion:1a5dfe3b…                           ← the reference line
```

The body holds `suggestion.body` and the clerk's own sentences and **nothing else**: no display name, no excerpt, no `network_identifier`, no room id — the approval screen cannot name the contact (#160, ADR 0012) and this surface cannot either. `delivery` is `GET /api/suggestions`' word (#216), rendered with the Companion's sentences. The reference line is visible rather than hidden in a tag: what the clerk reads to find its way, the owner reads too.

**`journal`** — one line per `twalk.persona.reply.approved.v1.posted` (the Sensor's report, #216): what went out, on which network, posted as which account, and `reach` — `contact` visibly not `nobody` — with the approval's id. **No text**: the text stays on the bus, where retention is decided, and the sentence "nothing Twalk writes onto Buzz quotes a contact" stays true without an exception to defend.

**`activite`** — only what calls for a look: `bridge.status.changed` (down and back), `consent.state.changed`, one line per suggestion produced ("l'assistant a proposé une réponse → `approbations`"), and one per suggestion that expired undecided. No `persona.thinking.emitted`, no message counts: on Buzz a channel that chatters is a channel that gets muted. Never a sender identity (`src/lib/dashboard/model.ts` is the rule).

**Deletion (#219)** — a post in `approbations` is deleted the moment its suggestion **expires** (`expires_at`, an hour by default), by a sweep of the channel every minute, with the `activite` line above. So the relay never holds a quoted word longer than a suggestion lives, well inside ADR 0028's seven days. (Deletion on decision is #284's, because deciding is.)

**Language** — `CLERK_USER_LANGUAGE`, one of the Companion's five, refused at startup otherwise; the host-side voice until #101, and never inferred from a channel's name.

## Decisions, and the one that is an ADR

- **The clerk is a projection of the relay and holds no state.** How it knows what it already posted, across a redelivery or a restart, is by reading the channel: the reference line is the key. Zero local state, nothing to back up or reconcile — and a store added later would change what the relay is allowed to be, which is why this one is written as an ADR in the PR (hard to reverse, surprising without context, a real trade-off against a SQLite of ids).
- **Its identity is its own key**, not Hermes's (the reader must know who speaks: what Twalk decided about consent and delivery is not Hermes's to say) and not the Gateway's (ADR 0032 refuses a Nostr private key in the Gateway). A NIP-OA delegation of the owner's key — revocable from Buzz Desktop, no relay authorisation to do — is the named evolution, once that Desktop flow has been measured.
- **Unreachable relay**: the durable consumer keeps the message and the clerk retries with backoff; a suggestion that expired meanwhile is not posted and is counted (`twalk_clerk_skipped_total{why="expired"}`).
- The reference deployment's rule, in the README: a Buzz relay is **multi-tenant by `Host`**, and the clerk must use the URL the relay announces (`RELAY_URL`), like every other client.

## Acceptance criteria

- [ ] A `persona.suggest.produced` on the bus is one post in `approbations` within seconds, with its `delivery` sentence, whoever produced it; the bus redelivering the trigger, and the clerk restarting, produce no second post.
- [ ] A test publishes a trigger whose body, quoted excerpt, display name and `network_identifier` are distinctive markers and searches every event of every channel for them, the way `tests/suggestions.rs` searches the listing: none is on the relay.
- [ ] The Sensor's `.posted` report appears in `journal` with `reach`, and a `nobody` report reads as one; no `journal` line carries text.
- [ ] A suggestion that expires undecided is deleted from `approbations` within the sweep interval, and `activite` says so.
- [ ] `activite` carries the four kinds above and no other; a suite asserts the absence of `persona.thinking.emitted` and of any Matrix user ID.
- [ ] The clerk's container environment holds neither the Gateway's service token nor Hermes's key (asserted on the container Docker really started, as `tests/runtime_settings.rs` does).
- [ ] `/metrics` renders `twalk_clerk_posts_total{channel}`, `twalk_clerk_deleted_total{why}`, `twalk_clerk_skipped_total{why}`; a relay that does not answer costs a warning and a counter, never the process.
- [ ] `.github/ci/suites.json` routes the new package (the routing test fails otherwise, and the harness gains a consumer).
- [ ] `CONTEXT.md` carries the entry above; the ADR is written.

## Blocked by

None (can start immediately). Related: #218, #219, #216, #240, #263. Blocks #284.

