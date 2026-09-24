---
name: twalk-calendar
description: Read the owner's free/busy through their Twalk deployment before proposing meeting times, and ask what one of their events carries — a join link, whether there is an agenda — without reading its words. Use when a contact asks when the owner is available, proposes a meeting, or asks to move one, and when a meeting the owner is about to attend may have a link or a document worth mentioning.
---

# twalk-calendar

Twalk lets you ask **two** things about the owner's calendar, and nothing else.

**When they are busy**, inside a window of at most fourteen days. Not what they are doing, not with whom. The answer is a list of busy intervals; every gap between them is free.

**What one event carries**, named by the uid the event that woke you carried: whether it has a join link and which, whether there is a description and how long, how many attachments. **Never the description's text** — it is a free-text box written by whoever created the meeting, which on an invitation is somebody who decided nothing about this deployment. No route answers it, and asking differently will not help.

Both are governed pulls under ADR 0032, and every read you make — served or refused — is recorded in the owner's deployment for them to see. Read when a message needs it, not on every turn.

## When to use it

**The free/busy read:**

- A contact asks *"are you free Thursday?"*, *"when can we meet?"*, *"can we move it to next week?"*.
- You are drafting a reply that proposes or confirms a time.

**The event read:**

- A meeting is about to start, or a message is about it, and a **join link** would help: *"your 14:00 has a video link, shall I send it?"*.
- You want to tell the owner there is something to read before a meeting: *"there is an agenda on it"* — you can say that there is one and roughly how long, never what it says.
- A contact asks where a meeting's document is: say there are attachments and how many, and let the owner open them.

Do not use either to summarise the owner's day, to find out what a meeting is **about**, or to check on somebody else. The first cannot, the second will not, and the refusal is recorded.

## Never name a time you have not read

A reply that proposes a slot, accepts one, or agrees to move a meeting is a **commitment made in the owner's name**. Read the free/busy first, every time, even when the contact has proposed the slots themselves and all you have to do is pick one — *especially* then, because picking one looks like agreeing and is in fact scheduling.

This is not a precaution against a hypothetical. On 2026-09-24 a draft on a live deployment answered *"Le mardi 13 octobre à 14h me convient très bien"* to a contact who had offered three slots. Nothing had read the calendar; the Gateway's record of reads for that day was empty. The owner would have sent it (#363).

If the read is refused, or you cannot make it, **say so in the reply and choose nothing**: *"je vérifie mon agenda et je te réponds"* is a true sentence a person can send. A guessed time is not, and the owner approving the draft has no way to tell the two apart.

## How to call it

Run the script beside this file. It signs the request with the same secret your outbound hook signs answers with; the one thing it needs beyond that is where the owner's Companion Gateway is.

```sh
./freebusy.sh <from> <to>                 # the deployment's own calendar
./freebusy.sh <connection> <from> <to>    # naming it, when there are two
```

(`freebusy.sh` in this skill's directory, wherever it was installed; the examples below write it as `skills/twalk-calendar/freebusy.sh`, its path in the Twalk repository.)

- `connection` — the owner's calendar connection, as the deployment names it. **Leave it out.** You have no way to know an id: the message that woke you carries the *kind* of connection it arrived on and never an id (ADR 0033), and the calendar's is a different connection from the mail's. With two arguments the script takes it from `TWALK_CALENDAR_CONNECTION`, which the operator set when they installed this skill. Name one only if the operator told you which of two calendars to read; a guess is refused as `connection_unknown` and the refusal is recorded.
- `from`, `to` — RFC 3339 instants (`2026-09-24T08:00:00Z`), `to` after `from`, at most fourteen days apart. Ask for the days the conversation is about, not for the whole fortnight.

The script needs three environment variables in your `.env`:

- `TWALK_ANSWER_SECRET` — the secret your outbound hook signs with; already there.
- `TWALK_GATEWAY_URL` — the owner's Companion Gateway, the same origin your outbound hook posts answers to (`https://twalk.example.org`); the operator adds it when they install this skill.
- `TWALK_CALENDAR_CONNECTION` — the calendar connection this deployment reads, so that you never have to name one (`calendar-linagora`, `calendar`). Added with the other two.

It prints the Gateway's answer as JSON on stdout and exits non-zero on a refusal, with the refusal on stderr.

### Example

```sh
$ skills/twalk-calendar/freebusy.sh calendar 2026-09-24T08:00:00Z 2026-09-26T18:00:00Z
{"connection":"calendar","from":"2026-09-24T08:00:00Z","to":"2026-09-26T18:00:00Z","busy":[{"start":"2026-09-24T09:00:00Z","end":"2026-09-24T10:30:00Z"},{"start":"2026-09-25T14:00:00Z","end":"2026-09-25T15:00:00Z"}]}
```

The owner is busy Thursday 9:00–10:30 and Friday 14:00–15:00 (UTC); every other moment in the window is free. The intervals are in UTC and the end of each is exclusive.

### Asking what an event carries

```sh
./event-facts.sh <uid>                 # the deployment's own calendar
./event-facts.sh <connection> <uid>    # naming it, when there are two
```

- `connection` — as above: leave it out, and it comes from `TWALK_CALENDAR_CONNECTION`.
- `uid` — the event's iCalendar UID, exactly as `calendar.event.created.v1`, `…changed.v1` or `…removed.v1` carried it in `data.uid`. This route does not search by title or by time: if you do not have the uid, you cannot ask.

```sh
$ skills/twalk-calendar/event-facts.sh calendar 8f3a2b1c-4d5e-6f70-8192-a3b4c5d6e7f8
{"connection":"calendar","uid":"8f3a2b1c-4d5e-6f70-8192-a3b4c5d6e7f8","found":true,"conference":"https://meet.example/abc-def","description_characters":340,"attachments":1}
```

That meeting has a join link you may offer, an agenda of about 340 characters you may **mention** but not read, and one attachment.

`"found": false` means this deployment holds no event with that uid — it never published it, or it falls outside the window the collector polls. It does **not** mean the meeting carries nothing. Say you cannot tell; do not say there is no link.

`"conference": null` means the event declares no join link in a property meant for one. Many calendars put the link in the description instead, and Twalk does not go looking for it there — choosing which URL in a text is "the meeting" means reading the text. If there is a description, say so and let the owner open it.

## What to do with the answer

1. Convert to the owner's time zone before you speak (the operator told you which; if not, say the zone you are using).
2. Propose **up to three** slots inside working hours that fall entirely in gaps, leaving a margin around busy intervals. Never propose a slot that overlaps one.
3. Say nothing about *why* the owner is busy — you do not know, and the answer does not carry it. "Thursday morning is taken" is right; "Thursday morning she has a meeting" is a guess.
4. If the window you need is wider than fourteen days, ask about the nearest fortnight first.

## Refusals you may see

The Gateway answers a JSON error with a code; the script prints it on stderr.

- `window_too_wide` — narrow the window to fourteen days.
- `invalid_uid` — the uid is missing, empty or over 512 characters; take it from the event's `data.uid`.
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

The event read is the same wire with its own path: `GET {TWALK_GATEWAY_URL}/_twalk/hermes/event-facts?connection=…&uid=…`, signed over `GET`, newline, `/_twalk/hermes/event-facts`, newline, the query as sent, newline, the timestamp. **The path is inside the signed line**, so a signature made for one read is refused on the other.

The query string is signed **exactly as sent**, so encode it the way the script does: `:` as `%3A` and `+` as `%2B` in the instants (a `+` left bare reaches the Gateway as a space and the read is refused as `invalid_window`), and `%`, `&`, `=` likewise.

The script hands the secret to `openssl` as an argument, where it is visible to other users of the host for the milliseconds the process runs. Hermes's host is the owner's own and single-user by ADR 0032's allowlist; on a host that is not, sign with a tool that reads the key from a file instead.
