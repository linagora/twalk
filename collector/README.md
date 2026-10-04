# collector

The collector (*le collecteur*, [ADRs 0033 and 0038](../docs/architecture/adr/0033-twalk-perceives-what-involves-other-people.md), lot 2 of [#251](https://github.com/linagora/twalk/issues/251)): the Twalk component that reads the owner's **own** accounts — a mailbox over JMAP, calendars over CalDAV — rather than the networks they talk on. It is the only process that holds a durable access path to the owner's correspondence, so it is the only one that can index the owner's archive (lot 3a below).

It lives behind the `collector` compose profile, so a deployment that does not want it sets no `COLLECTOR_*` variable and runs no container. Its behaviour is in `src/`; its deployment and its four production proofs are in [`deploy/README.md`](../deploy/README.md), and every variable is documented in [`deploy/docker-compose/.env.example`](../deploy/docker-compose/.env.example), which is the file to read first. This page is the orientation to the code and the map of what each variable decides.

## What is where

| Module | What it does |
| --- | --- |
| `config.rs` | Reads every `COLLECTOR_*` variable and refuses a configuration it cannot honour — naming the variable rather than failing silently. |
| `oidc.rs`, `main.rs` (bin: `twalk-collector`) | The OIDC grant: the operator's sign-in (`twalk-collector authorize`), PKCE, the refresh token written at 0600 and rotated on every renewal, the SSO discovery read lazily. |
| `jmap.rs`, `mails.rs`, `push.rs` | The mailbox: the JMAP session, the frontier that decides what is a message from a person, the poll, and the RFC 8887 push socket that wakes it. |
| `caldav.rs`, `calendars.rs`, `zones.rs`, `windows_zones.rs` | The calendars: the OpenPaaS side service, the CTag/ETag diff, an RFC 5545 reader, TZID handling (IANA and Windows families). |
| `side.rs` | What the two services the grant opens have in common on the wire: the shared bearer, the host for `source`, and the two ways a request fails (#276, #280). |
| `outbound.rs`, `replies.rs` | An approved reply leaves from the owner's own mailbox, `reach=contact`, at most once. |
| `freebusy.rs`, `http.rs` | The internal HTTP endpoint the Companion Gateway relays to: `GET /freebusy` (#281) and, since lot 3a, `GET /search` and `GET /index/status`. |
| `search_index.rs`, `backfill.rs`, `source.rs`, `triage.rs` | The search archive (lot 3a): the Tantivy index, the resumable backfill, the `Source` abstraction, and the owner's triage rules (#417). |
| `consent.rs`, `owner.rs`, `status.rs`, `metrics.rs`, `fs.rs` | The shared consent cache, the owner's identities, the connection states published on the bus, the Prometheus exposition, and the private-file writer. |

## Variables

Every one is documented at length in `.env.example`; this table is the summary, and the **default** is what an empty `.env` gives.

| Variable | Default | What it decides |
| --- | --- | --- |
| `COLLECTOR_CREDENTIAL` | `oidc` | Which credential this process carries: `oidc` (an SSO grant) or `basic` (a username and a password file). One process holds one. |
| `COLLECTOR_STATE_DIR` | — | The directory the grant, the cursors and the search index live in. `compose.yaml` sets it to `/data`, the `collector-data` volume. Required. |
| `COLLECTOR_OIDC_ISSUER` | — | The SSO's issuer, where `/.well-known/openid-configuration` is. With `COLLECTOR_CREDENTIAL=oidc`. |
| `COLLECTOR_OIDC_CLIENT_ID` | — | The confidential client at that SSO. |
| `COLLECTOR_OIDC_REDIRECT_URI` | — | The registered redirect URI; nothing listens there (the address is pasted back at the terminal). |
| `COLLECTOR_OIDC_SCOPES` | `openid profile email offline_access` | The scopes asked for. Must include `offline_access` or the SSO issues no refresh token. |
| `COLLECTOR_OIDC_CLIENT_SECRET_FILE` | — | A **file** on the host holding the client's secret, mode 0600. Never a variable. Copied by the entrypoint for the unprivileged account. |
| `COLLECTOR_BASIC_USER`, `COLLECTOR_BASIC_PASSWORD_FILE` | — | With `COLLECTOR_CREDENTIAL=basic`: the service account and its password in a file at 0600. |
| `COLLECTOR_JMAP_SESSION_URL` | — | The mailbox's JMAP session document. Required when a `kind email` connection is held, and never asked for otherwise. |
| `COLLECTOR_CALDAV_URL` | — | The OpenPaaS calendar side service's root. Required when a `kind calendar` connection is held. |
| `COLLECTOR_OWNER_EMAIL` | — | You, as the services spell your account. A grant for anybody else publishes nothing. |
| `COLLECTOR_OWNER_ALIASES` | empty | Every **other** address that is yours, comma-separated. An address not declared here is a contact the collector will publish. |
| `COLLECTOR_MAIL_CONNECTION`, `COLLECTOR_CALENDAR_CONNECTION` | — | The connections held, by the ids `GATEWAY_CONNECTIONS` declares. At least one, or the collector refuses to start. |
| `COLLECTOR_NATS_URL` | — | The bus. Required. |
| `COLLECTOR_HOST` | `collector` | This collector's name in `source`. Never a secret. |
| `COLLECTOR_GATEWAY_URL`, `COLLECTOR_GATEWAY_SERVICE_TOKEN` | — | The Companion Gateway's registry and snapshot, read with the service token. Set together or neither. |
| `COLLECTOR_HEALTH_INTERVAL_SECONDS` | `60` | How often the grant and the services are checked. |
| `COLLECTOR_CALENDAR_POLL_SECONDS` | `60` | How often the calendars are polled. |
| `COLLECTOR_CALENDAR_WINDOW_BACK_DAYS`, `_AHEAD_DAYS` | `7`, `120` | The window of a calendar the collector watches. |
| `COLLECTOR_MAIL_POLL_SECONDS` | `60` | How often the mailbox is polled — the **fallback**; push is the rule when the server offers it (#277). |
| `COLLECTOR_SEND_RETRY_BASE_MS`, `_MAX_ATTEMPTS` | `1000`, `5` | The retry policy for an approved reply the mailbox will not take. |
| `COLLECTOR_METRICS_LISTEN` | — | Where `/metrics` is served. Empty: not served. |
| `COLLECTOR_HTTP_LISTEN` | — | Where the internal relay endpoint is served (`/freebusy`, `/search`, `/index/status`), answering the service token alone. Empty: `0.0.0.0:8090` when `GATEWAY_SERVICE_TOKEN` is set. |
| `COLLECTOR_INDEX_KEY_FILE` | — | **The key of the search index (lot 3a): a file on the host, mode 0600, never a variable.** **Empty — no key — the index is OFF**: no index is opened, nothing is written, `/search` answers `503 index_not_configured`, and the collector starts anyway. A missing key is a capability not asked for, never a failure of the mail. The index lives under `COLLECTOR_STATE_DIR/index` and **must** sit on an encrypted store — see [`deploy/README.md`, "The search archive"](../deploy/README.md). Sans énumération de l'archive, activer l'index ne rend cherchable **que le courrier que le collecteur lit à partir de ce moment** ; l'énumération de l'archive (JMAP `Email/query` paginé, reprenable) arrive au **lot 3b**. |
| `COLLECTOR_LOG_LEVEL` | `info` | The log level. |

A second collector (`collector-calendar`, profile `collector-calendar`) reads a calendar on another credential; its variables are the `CALENDAR_COLLECTOR_*` block in `.env.example`, mapped onto these.

## The search archive (lot 3a)

The collector holds a **full-text index of the owner's archive** — their mail, then their messaging — in Tantivy, under the state directory it already writes its cursors into. The Companion Gateway relays the owner's own read (`GET /api/search`, behind the session guard) to the internal `GET /search`, which applies the `source`/`from`/`to` filters and the **consent filter**: a correspondent the owner revoked has their hits **withdrawn** and the withdrawal **counted** (`twalk_collector_search_hits_withheld_total{reason}`), so a revoked contact is a removal the owner can read, not an absence ([ADR 0012](../docs/architecture/adr/0012-revoked-consent-reduces-publication.md)). The response carries a bounded **snippet**, never a body.

Three things it never does, each with an ADR: it **acts on nothing** — searching is not deciding ([ADR 0042](../docs/architecture/adr/0042-mail-triage-is-the-owners-rules-and-not-the-agents-judgement.md)); it is **never readable by a persona** — the internal route requires the service token no persona holds ([ADR 0032](../docs/architecture/adr/0032-twalk-governs-what-an-agent-outside-it-may-see-and-do.md)); and the **backfill writes nothing on the bus** — a twenty-five-year history must not overflow the stream that keeps ninety days ([ADR 0037](../docs/architecture/adr/0037-the-bus-keeps-ninety-days-and-two-gigabytes-and-no-more.md)). It is the longest holder of other people's words in the deployment, which is why it is off by default and encrypted at rest ([ADR 0043](../docs/architecture/adr/0043-the-search-archive-is-the-only-long-term-holder-of-third-parties-words.md)).

Metrics: `twalk_collector_index_documents` (a gauge — how far the archive has come), `twalk_collector_search_reads_total{outcome}` (served, or a refusal's code), `twalk_collector_search_hits_withheld_total{reason}`.
