---
name: twalk-calendar
description: Read the owner's free/busy through their Twalk deployment before proposing meeting times. Use when a contact asks when the owner is available, proposes a meeting, or asks to move one — never guess an agenda, and never answer with more than free or busy.
---

# twalk-calendar

Twalk lets you ask **one** thing about the owner's calendar: when they are busy, inside a window of at most fourteen days. Nothing else — not what they are doing, not with whom, not where. The answer is a list of busy intervals; every gap between them is free. This is the one governed pull of ADR 0032, and every read you make — served or refused — is recorded in the owner's deployment, for them to see, so read when a message needs it and not on every turn.

## When to use it

- A contact asks *"are you free Thursday?"*, *"when can we meet?"*, *"can we move it to next week?"*.
- You are drafting a reply that proposes or confirms a time.

Do not use it to summarise the owner's day, to find out what a meeting is about, or to check on somebody else: it cannot, and the refusal is recorded.

## How to call it

Run the script beside this file. It signs the request with the same secret your outbound hook signs answers with; the one thing it needs beyond that is where the owner's Companion Gateway is.

```sh
./freebusy.sh <connection> <from> <to>
```

(`freebusy.sh` in this skill's directory, wherever it was installed; the examples below write it as `skills/twalk-calendar/freebusy.sh`, its path in the Twalk repository.)

- `connection` — the owner's calendar connection, as the deployment names it (the operator told you; usually `calendar`).
- `from`, `to` — RFC 3339 instants (`2026-09-24T08:00:00Z`), `to` after `from`, at most fourteen days apart. Ask for the days the conversation is about, not for the whole fortnight.

The script needs two environment variables in your `.env`:

- `TWALK_ANSWER_SECRET` — the secret your outbound hook signs with; already there.
- `TWALK_GATEWAY_URL` — the owner's Companion Gateway, the same origin your outbound hook posts answers to (`https://twalk.example.org`); the operator adds it when they install this skill.

It prints the Gateway's answer as JSON on stdout and exits non-zero on a refusal, with the refusal on stderr.

### Example

```sh
$ skills/twalk-calendar/freebusy.sh calendar 2026-09-24T08:00:00Z 2026-09-26T18:00:00Z
{"connection":"calendar","from":"2026-09-24T08:00:00Z","to":"2026-09-26T18:00:00Z","busy":[{"start":"2026-09-24T09:00:00Z","end":"2026-09-24T10:30:00Z"},{"start":"2026-09-25T14:00:00Z","end":"2026-09-25T15:00:00Z"}]}
```

The owner is busy Thursday 9:00–10:30 and Friday 14:00–15:00 (UTC); every other moment in the window is free. The intervals are in UTC and the end of each is exclusive.

## What to do with the answer

1. Convert to the owner's time zone before you speak (the operator told you which; if not, say the zone you are using).
2. Propose **up to three** slots inside working hours that fall entirely in gaps, leaving a margin around busy intervals. Never propose a slot that overlaps one.
3. Say nothing about *why* the owner is busy — you do not know, and the answer does not carry it. "Thursday morning is taken" is right; "Thursday morning she has a meeting" is a guess.
4. If the window you need is wider than fourteen days, ask about the nearest fortnight first.

## Refusals you may see

The Gateway answers a JSON error with a code; the script prints it on stderr.

- `window_too_wide` — narrow the window to fourteen days.
- `invalid_window` — `from`/`to` are not RFC 3339 or not in order.
- `connection_unknown` — the connection name is wrong; ask the operator.
- `connection_not_connected` — the owner's calendar is not reachable right now (the answer says which state it is in). Say you cannot check the calendar at the moment; do not guess.
- `unsigned`, `bad_signature`, `stale_timestamp` — your `TWALK_ANSWER_SECRET` or your clock; tell the operator.
- `collector_not_configured`, `collector_unreachable`, `collector_refused` — the deployment's side; tell the operator.

## The wire, for a client that is not this script

`GET {TWALK_GATEWAY_URL}/_twalk/hermes/freebusy?connection=…&from=…&to=…` with two headers:

- `X-Hermes-Timestamp: <now, RFC 3339>`
- `X-Hermes-Signature-256: sha256=<hex HMAC-SHA256>` over the line `GET`, newline, `/_twalk/hermes/freebusy`, newline, the query string exactly as sent, newline, the timestamp — keyed with `TWALK_ANSWER_SECRET`.

An optional `X-Hermes-Delivery` names the attempt: every call is a line in the owner's record, and a retry that carries the same delivery is recorded as the same attempt tried again. The script takes it from `TWALK_DELIVERY_ID` and invents one per run otherwise.

The query string is signed **exactly as sent**, so encode it the way the script does: `:` as `%3A` and `+` as `%2B` in the instants (a `+` left bare reaches the Gateway as a space and the read is refused as `invalid_window`), and `%`, `&`, `=` likewise.

The script hands the secret to `openssl` as an argument, where it is visible to other users of the host for the milliseconds the process runs. Hermes's host is the owner's own and single-user by ADR 0032's allowlist; on a host that is not, sign with a tool that reads the key from a file instead.
