# Real mautrix provisioning answers, captured verbatim

Every file here is an answer a **real** mautrix bridge gave, recorded by
calling its provisioning API from inside the reference deployment's docker
network. They exist because three bugs in a row (#106) were bugs of exactly
one kind: the stub bridge answered a shape no mautrix bridge produces, ten
Gateway tests went green, and a real WhatsApp login failed on a field nobody
had ever seen the bridge send.

So the rule these fixtures impose is: **the stub serves these bodies.** If the
Gateway can drive the stub, it can drive the bridge, because the bytes are the
same bytes. `harness/fixtures.rs` loads them and `harness/stub_bridge.rs`
answers out of them; nothing in the stub hand-writes a provisioning document
any more.

## Where they came from

| Bridge | Image | Captured |
| --- | --- | --- |
| mautrix-whatsapp | `dock.mau.dev/mautrix/whatsapp:v26.09` | 2026-09-18, reference deployment (`deploy/docker-compose`), appservice listener `:29318` |
| mautrix-signal | `dock.mau.dev/mautrix/signal:v26.09` | 2026-09-18, reference deployment, appservice listener `:29328` |

Both were reached the way a Gateway reaches them — `Authorization: Bearer
<provisioning secret>` against `/_matrix/provision/v3/*` on the bridge's own
appservice listener, on the deployment's docker network.

## What is in a file

One file per endpoint per bridge. Each holds every answer that endpoint was
observed to give, keyed by a name the stub and the tests refer to:

```json
{
  "endpoint": "POST /_matrix/provision/v3/login/start/{flow}",
  "bridge": "mautrix-whatsapp v26.09",
  "answers": {
    "qr": { "status": 200, "body": { … }, "note": "…", "substituted": [ … ] }
  }
}
```

`substituted` names every value that is **not** verbatim, and there are only
two reasons for one:

- **a credential.** A QR payload is a network credential (ADR 0011) and does
  not belong in a git repository, so `display_and_wait.data` carries a
  structurally identical placeholder: the real scheme and prefix, a fake
  secret. The prefix is the part the Companion branches on, and it is real.
- **the owner's personal data.** The reference deployment's WhatsApp account
  is a real phone number. It is replaced by the ITU documentation-reserved
  number `+33612345678` wherever it appeared, as an id, a name or a profile
  field.

Process ids, step ids, transaction ids, error codes, error strings, statuses,
field specs and every other member are exactly what the bridge sent.

## Asymmetries worth knowing before you assume

These are the reasons a fixture set exists per bridge rather than one "mautrix"
fixture set.

- **`GET /logins` answers ids, not logins.** `{"login_ids":["…"]}` — bare
  strings. The name, the profile and the connection state of a login are in
  `GET /whoami`'s `logins` array and **nowhere else**. The Gateway read
  `/logins` as a list of objects and therefore reported the bridge
  unreachable; see `login-ids-not-logins` in the PR for #106.
- **An unknown flow id is a hard error on Signal and invisible on WhatsApp.**
  `POST /login/start/definitely-not-a-flow` gives Signal's `404 M_NOT_FOUND
  "Invalid login flow ID"` and WhatsApp's `200` with… the QR flow. A typo in a
  flow id fails loudly on one bridge and silently succeeds on the other.
- **Signal offers one flow (`qr`); WhatsApp offers two (`qr`, `phone`).** The
  step ids are namespaced per bridge — `fi.mau.signal.login.qr` versus
  `fi.mau.whatsapp.login.qr` — so nothing may hard-code one.
- **`txn_id` is re-issued with every step answer**, not once per process: the
  QR refresh comes back with a new one, and the next call must echo *that*.
  A wrong one is `500 M_BAD_STATE "Transaction ID does not match"`; omitting
  it skips validation entirely and the call just blocks.
- **The step cancel is not usable for a QR flow.** `POST
  /login/step/{process}/{step}/cancel` answers `500 M_BAD_STATE "Login
  process does not support cancelling steps"` on both bridges. What releases
  a held `display_and_wait` is the *process* cancel, and the held request
  then comes back `410 FI.MAU.BRIDGE.LOGIN_CANCELLED`.
- **A refused `user_input` destroys the process.** WhatsApp answered a bad
  phone number with `400 FI.MAU.WHATSAPP.PHONE_NUMBER_TOO_SHORT`, and every
  later call against that process was `404 M_NOT_FOUND`. There is no retrying
  a step; the user starts again.
- **Nothing at the bridge enforces one login at a time.** Two concurrent
  `login/start` calls for the same user are both accepted, with two process
  ids. One-login-per-bridge is the Gateway's own rule (#55), deliberately, so
  that a second device cannot silently take over a scan in progress.

## What is *not* here, and cannot be without a phone

- **`complete`.** Completing a login needs a human scanning a code with the
  account's phone, so no `complete` answer was captured. What the stub serves
  for it is bridgev2's declared shape (`complete.login_id`,
  `complete.user_login_id`) and it is **unverified against a real bridge** —
  the one remaining place in this file where the old failure mode could still
  be hiding.
- **A successful `POST /logout/{login}`.** The only login on the deployment is
  the owner's, and logging it out was out of the question. The 404 for an
  unknown login id is captured; the success body is not.
- **`FI.MAU.BRIDGE.TOO_MANY_LOGINS`.** bridgev2 defines it (a `max_logins`
  cap), the reference deployment sets no cap, so it was never provoked. The
  stub can still be told to answer it, because the Gateway maps it.

## The `matrix` network has no fixtures, and never will

Matrix is not a bridge. There is no mautrix bridge, no provisioning API and no
login flow for the `matrix` network: the user's own homeserver account reaches
Twalk by the Sensor being invited into their rooms (#18, #53). There is
nothing to probe, so there is nothing here.
