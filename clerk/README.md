# clerk

The clerk (*le greffier*, [ticket #265](https://github.com/linagora/twalk/issues/265)): the Twalk component that writes onto the owner's own Buzz relay what the bus says. Buzz is a **surface and never an authority** ([ADR 0032](../docs/architecture/adr/0032-twalk-governs-what-an-agent-outside-it-may-see-and-do.md)): the clerk holds no consent snapshot, no Companion Gateway credential of any kind — not the service token that opens the consent snapshot, not a device token — and no key of Hermes's. Its only secret is a Nostr key of its own.

Three durable consumers read the bus and turn what they read into a post in one of three channels an operator's `provision-buzz-channels.sh` created for it. A fourth loop, the sweep, deletes a post once its suggestion expires. Nothing here decides anything: approving or refusing a suggestion, and the seam that would let a decision made on Buzz reach the Companion Gateway, is [#284](https://github.com/linagora/twalk/issues/284)'s.

## The three channels: what each carries, and what it never does

| Channel | Fed by | Carries | Never carries |
| --- | --- | --- | --- |
| `approbations` (`CLERK_CHANNEL_APPROVALS`) | `persona.suggest.produced` | One forum post per suggestion: the suggestion's body verbatim, the clerk's own sentences (network, expiry), and a reference line last (`twalk:suggestion:<id> expires <rfc3339>`) | A display name, an excerpt, a `network_identifier`, a room id — the approval screen cannot name the contact ([#160](https://github.com/linagora/twalk/issues/160)) and this surface cannot either ([ADR 0012](../docs/architecture/adr/0012-revoked-consent-reduces-publication.md)) |
| `journal` (`CLERK_CHANNEL_JOURNAL`) | `persona.reply.approved.v1.posted` | One stream line per posted reply: which network, which Matrix ID it was posted as, its `reach` (`contact`, visibly not `nobody`), the approval's id, and the time | Any text of the reply — so "nothing Twalk writes onto Buzz quotes a contact" holds without an exception to defend |
| `activite` (`CLERK_CHANNEL_ACTIVITY`) | `bridge.status.changed`, `consent.state.changed`, and the clerk's own posts and sweep | Only what calls for a look: a bridge down and back, a consent decision (by the *kind* of subject and the new state and networks), a suggestion produced, a suggestion that expired undecided and was deleted | `persona.thinking.emitted`, a message count, or a sender identity — on Buzz, a channel that chatters is a channel that gets muted |

A consent line names the kind of subject (contact, network, persona) and never the subject's Matrix ID — the same rule the Companion's dashboard activity feed follows (`companion/src/lib/dashboard/model.ts`).

The clerk's own sentences exist in French and English (`CLERK_USER_LANGUAGE`); a suggestion's body is posted exactly as the event carried it, whatever language that is.

## Three decisions worth arguing with

**Its own Nostr key.** Not Hermes's — the reader must know who speaks, and what Twalk decided about consent and delivery is not Hermes's to say — and not the Companion Gateway's, because ADR 0032 refuses a Nostr private key in the Gateway. `provision-nostr-key.sh` generates it; the owner tells the relay to accept it and adds it to each channel with `provision-buzz-channels.sh --bot <hex>`.

**No state of its own** ([ADR 0035](../docs/architecture/adr/0035-the-clerk-is-a-projection-of-the-relay-and-holds-no-state.md)). What it already wrote, it finds on the relay — never in a store — and the mechanism is per channel. A **forum post** in `approbations` is recognised by its reference line, `twalk:suggestion:<id> …`, read off the clerk's own posts. A **stream message** in `journal` or `activite` is recognised by a tag every one of them carries, `["r", "twalk:event:<the bus event's id>"]`, which the clerk queries (`#r`) before writing a journal line or a consent or bridge line (the two `activite` lines that follow a forum post rest on that post's own key); the simpler mechanism — dating the line from the bus event's `time` so a redelivery hashed to the same Nostr id and the relay's own duplicate answer did the work — was tried first and does not survive contact with Buzz, which refuses any `created_at` more than fifteen minutes from its clock, and a redelivery after a relay outage is exactly that old. Either way a redelivery of a trigger already handled (a SIGTERM between the relay's `2xx` and the ack, a lost ack, an `ACK_WAIT` overrun) and a cold restart are answered by the same query with the same result, counted as `twalk_clerk_skipped_total{why="duplicate"}`. A SQLite of suggestion id → post id was weighed and rejected: it would make the relay a copy of something Twalk holds, and the moment the two disagreed — a post deleted from Buzz Desktop, a relay restored from an older backup — nothing would say which was true.

**The sweep** (`CLERK_SWEEP_SECONDS`, a minute by default). Every tick it rereads its own posts in `approbations`, deletes each one whose reference line names an `expires_at` that has passed, and says so once in `activite` ([#219](https://github.com/linagora/twalk/issues/219)) — so the relay never holds a quoted word longer than the suggestion that carried it lived, well inside [ADR 0028](../docs/architecture/adr/0028-a-contacts-words-live-apart-from-the-event-that-identifies-them.md)'s seven days. A post it **cannot date** — the contract makes `expires_at` optional and a third-party persona need not set it; an expiry that does not parse; a reference line it does not recognise — is not the one exception that stays for ever: it goes once the relay's own `created_at` on it is seven days old, counted apart as `twalk_clerk_deleted_total{why="undatable"}`, and every sweep that read one warns how many it could not date, so a channel quietly accumulating them is visible before the week is out. Deletion on a *decision* — the owner refusing a suggestion, or editing and sending it — is [#284](https://github.com/linagora/twalk/issues/284)'s, because deciding is; this deletion only ever follows a clock.

Other rulings, briefer:

- `CLERK_RELAY_URL` is the URL the relay **announces**, not merely one that reaches it: NIP-98 binds every signed request to an exact URL, and a Buzz relay is multi-tenant by `Host`.
- An unreachable relay costs a warning, a counter (`twalk_clerk_relay_failures_total`) and a retry with backoff (transient failures are `Nak`ed onto the bus; a refusal of the request itself is logged once and acked, because a post the relay refuses once will refuse the same way again). Neither ever stops the process. The retry is sized to the hour a suggestion lives by default: sixty-four deliveries, two, four, eight, sixteen and thirty-two seconds apart and then a minute, so the last one still finds the suggestion alive and a relay outage shorter than its life loses nothing — and a poisoned message costs at most that hour, since an expired one is skipped on the delivery that finds it so.
- A suggestion that expired before the clerk got to it — including one redelivered after an outage — is skipped and counted (`twalk_clerk_skipped_total{why="expired"}`), never posted stale.
- No log line and no error carries a body the relay sent: a refusal keeps the relay's own `error`/`message` field cut to 120 characters, and a `2xx` that is not the answer a route gives is described by its parse failure and its length — for `/query` that body is the clerk's own posts, which quote suggestions.
- `CLERK_USER_LANGUAGE` is one of the Companion's five (`en`, `fr`, `it`, `es`, `de`) and anything else refuses to start. Unset is `en` **with a startup warning**: on the reference deployment the personas' language comes from the Companion's settings (#184), which the clerk cannot read, so a French deployment gets an English clerk until the operator sets it in `deploy/docker-compose/.env`. `it`, `es` and `de` are accepted but have no clerk sentences of their own yet, so they fall back to English with a startup warning.
- **Two clerks on one key are unsupported.** Both may write the same suggestion or line in the window between one's query and its post; nothing corrupts and the sweep deletes both, but the owner sees two. One clerk per relay key.

## Configuration

Environment variables, like every other Twalk component. `CLERK_RELAY_URL`, `CLERK_NOSTR_KEY_FILE` and the three `CLERK_CHANNEL_*` are the ones with no default: empty on any of them and the clerk (in the reference deployment) starts, says so, and hosts nothing — `deploy/docker-compose/clerk-entrypoint.sh` is what makes that true of the compose service specifically; the bare binary refuses to start on a missing one.

| Variable | Required | Meaning |
| --- | --- | --- |
| `CLERK_RELAY_URL` | yes | The URL the owner's Buzz relay **announces** (its own `RELAY_URL`): `http://` or `https://`, a host and optionally a port, nothing after — no trailing slash, no path. |
| `CLERK_NOSTR_KEY_FILE` | yes | Path to the clerk's own signing key: one line, 64 hex characters or `nsec1…`, or an env-style `BUZZ_PRIVATE_KEY=<key>` line (what `provision-nostr-key.sh` writes). Refused if the file is readable by group or others — it signs everything the clerk posts. |
| `CLERK_CHANNEL_APPROVALS` | yes | The `approbations` channel's UUID, as `provision-buzz-channels.sh --bot <hex>` prints it. |
| `CLERK_CHANNEL_ACTIVITY` | yes | The `activite` channel's UUID. |
| `CLERK_CHANNEL_JOURNAL` | yes | The `journal` channel's UUID. |
| `CLERK_USER_LANGUAGE` | | The language the clerk writes its own sentences in — never a contact's, which it never quotes. One of `en`, `fr`, `it`, `es`, `de`. Unset runs as `en` and warns at startup (`CLERK_USER_LANGUAGE is not set; the clerk writes in English. Set it in deploy/docker-compose/.env`), because the clerk cannot read the language set in the Companion. Refused at startup otherwise, with the five named. |
| `CLERK_NATS_URL` | | Default `nats://localhost:4222`. |
| `CLERK_STREAM` | | The JetStream stream name. Default `twalk`. |
| `CLERK_SUBJECT_PREFIX` | | This deployment's bus namespace: `fr.linagora.twalk.X` becomes `<prefix>.X`. Default `twalk`. |
| `CLERK_LISTEN` | | Where `/health` and `/metrics` are served. Default `127.0.0.1:8084`. |
| `CLERK_SWEEP_SECONDS` | | How often the sweep looks for an expired suggestion's post to delete. Default 60; refused below 1. |
| `CLERK_LOG_LEVEL` | | Default `info`. |

An environment variable that is absent or empty is unset either way: an empty value in a compose `.env` file is how an operator leaves an option out.

### `/health` and `/metrics`

`/health` answers as soon as the origin is bound, before the bus is reached — a late bus does not make the process look down. `/metrics` renders, at zero when nothing has happened yet:

- `twalk_clerk_posts_total{channel}` — posts written to the relay, by channel (`approbations`, `activite`, `journal`).
- `twalk_clerk_deleted_total{why}` — posts the sweep deleted: `expired` (the reference line's `expires_at` had passed) or `undatable` (no expiry the sweep could read, and the relay's own `created_at` seven days old — ADR 0028's ceiling).
- `twalk_clerk_skipped_total{why}` — events read off the bus that did not become a post or a line (`expired`, `unreadable`, `duplicate` — the last on any of the three channels).
- `twalk_clerk_relay_failures_total` — failed writes to the relay (a post, a delete, a query).
- `twalk_clerk_sweeps_total` — completed sweep passes.
- `twalk_clerk_up`, `twalk_clerk_started_at_seconds`.

## Operator steps

Three, in this order, before anything above is set — `deploy/docker-compose/.env.example`'s Clerk section has the same list with the deployment's own paths:

1. `./provision-nostr-key.sh /etc/twalk/clerk.env` — generates the clerk's key, prints its public half, and names the relay-side step: `./run.sh add-member <hex>` in Buzz's own compose bundle, because a relay with restricted writes refuses a key it was not told to accept.
2. `./provision-buzz-channels.sh --bot <that hex pubkey>` — creates the owner's channels (as the owner, from a key the script never prints), adds the clerk as a `bot` member of each, and prints the three `CLERK_CHANNEL_*` lines ready to paste.
3. Paste what was printed into `deploy/docker-compose/.env`, then `docker compose up -d clerk`.

The clerk runs in the **host's** network namespace (`network_mode: host`), because the relay is reached by the URL it announces, the same shape `hermes` is in for the same reason.

## What is deliberately not here

Two things, both [#284](https://github.com/linagora/twalk/issues/284)'s:

- **The delivery line.** An `approbations` post says, in so many words, that its delivery line is not yet known. That line is #284's `delivery` — what `GET /api/suggestions` answers as `can_reach` or `cannot_reach`, the owner's own account's standing in the trigger's room, read by the Companion Gateway as the bridge's bot — and it is #284's because reading it needs a Gateway credential, which this component holds none of by design. (The `journal` line's `reach` is a different fact: the Sensor's own report, after the post, of whether the reply went to the contact or to nobody.)
- **The approval itself.** The clerk only reads the bus; there is no seam yet from a gesture the owner makes on Buzz (approving, refusing, editing a suggestion) back to the Companion Gateway, which remains the single writer of that decision ([ADR 0022](../docs/architecture/adr/0022-the-approval-api-lives-on-the-companion-gateway.md)). Nothing here decides on the owner's behalf.

## Tests

```bash
cd clerk
cargo test   # boots the shared test stack (Synapse, NATS) and a real Buzz relay of its own (Docker required)
```

Integration tests in `clerk/tests/` run the clerk binary at its process boundary (`ClerkProc`, modelled on the Hermes suite's `RuntimeRun`) against the shared harness's Synapse/NATS stack and a **real Buzz relay** the suite brings up itself, seeded by signed events the way an operator seeds the owner's own relay — `smoke.rs`, `approbations.rs` (one post per suggestion across a redelivery and a restart, and a trigger whose body, excerpt, display name and `network_identifier` are distinctive markers searched for in every event of every channel), `journal.rs` (both languages, and one line per report across a redelivery of a report dated past the relay's fifteen-minute window), `activite.rs` (one line per transition across a redelivery) and `sweep.rs`.

The ticket's container-environment criterion — the clerk's container holds neither the Gateway's service token nor Hermes's key — is asserted on a container Docker really started by #284's deployment suite. What guards it here is a unit test on the compose file itself (`config.rs`, `the_compose_service_holds_no_gateway_or_hermes_credential`): it reads the `clerk` service's `environment:` block out of `deploy/docker-compose/compose.yaml` and fails on any variable that is not `CLERK_*`, or whose value names a `GATEWAY_*` or `HERMES_*` variable other than the `HERMES_USER_LANGUAGE` fallback, `BUZZ_PRIVATE_KEY`, or anything ending in `_TOKEN` or `_SECRET`.

The relay stack (`clerk/tests/compose.relay.yaml`: the relay, Postgres, Redis, nothing else) is its own compose project, under its own variables:

| Variable | Default | Meaning |
| --- | --- | --- |
| `TWALK_CLERK_TEST_STACK` | `twalk-clerk-test` | The compose project name. |
| `TWALK_CLERK_TEST_RELAY_PORT` | `17800` | The relay's published port on `127.0.0.1`; also part of the URL every signed request in the suite is bound to, since the relay is multi-tenant by host. |

Like the shared test stack, the relay stack **persists across runs** — every channel a test creates is a fresh UUID, every clerk key is fresh, and every run has a bus stream and subject prefix of its own, so nothing about a previous run's state has to be cleaned up for the next one to be isolated. It goes down with:

```bash
docker compose -p twalk-clerk-test -f clerk/tests/compose.relay.yaml down -v
```

(substituting `TWALK_CLERK_TEST_STACK`'s value for `twalk-clerk-test` if it was set to something else).
