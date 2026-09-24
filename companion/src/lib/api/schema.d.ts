// Generated from companion-gateway/openapi.yaml by scripts/generate-api-client.mjs.
// Do not edit: run `npm run api:generate`. `npm run api:check` fails when
// this file no longer matches the description.

export interface paths {
    "/_twalk/bridges/{bridge_id}/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * A bridge pushes its connection state.
         * @description Mautrix's **only** push channel, and the producer behind
         *     `bridge.status.changed.v1` on the bus. Each bridge's
         *     `homeserver.status_endpoint` names this URL; the bridge POSTs its
         *     `BridgeState` here whenever its connection state changes, retried
         *     with backoff and deduplicated on its own side within a TTL.
         *
         *     This is not an endpoint the Companion calls, and not one a generated
         *     client has any use for. It is described because every answer this
         *     origin gives is described.
         *
         *     **Authentication.** The calling bridge's own `as_token`, as
         *     `Authorization: Bearer` — the same value as that bridge's
         *     `appservice.as_token` and as `GATEWAY_BRIDGE_<ID>_AS_TOKEN` in the
         *     Gateway's configuration. Trusting the caller's position on the
         *     compose network instead was explicitly refused: every container on
         *     that network can reach this port, and a forged push could tell the
         *     user a dead session was healthy. A device token is not accepted here,
         *     and an `as_token` opens nothing else.
         *
         *     **What the Gateway does with it.** `state_event` is translated into
         *     the contract's vocabulary — `BAD_CREDENTIALS` to `session_expired`
         *     (this, not `LOGGED_OUT`, is what a remotely revoked session reports),
         *     `TRANSIENT_DISCONNECT` to `degraded`, `CONNECTING` and `BACKFILLING`
         *     to `starting`, `CONNECTED` to `connected`, anything else to
         *     `disconnected` — compared with the state this bridge was last known
         *     to be in, and published as a transition only if the two differ. A
         *     repeated identical state is accepted and publishes nothing.
         */
        post: operations["pushBridgeStatus"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/_twalk/hermes/answers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Hermes pushes back an answer, which becomes a suggestion.
         * @description The return half of ADR 0032's seam (ticket #206). A persona wakes
         *     Hermes — Nous Research's agent runtime, outside this deployment — by
         *     posting a signed webhook to it, and Hermes's answer comes back
         *     **here**. Not onto the bus: the reference bus has no authentication,
         *     so "Hermes may publish" would mean "anything that can reach the bus
         *     may publish a suggestion", which stops being theoretical the moment
         *     the two are different machines.
         *
         *     What the Gateway does with it is what it already does with a
         *     suggestion. The answer becomes a `persona.suggest.produced.v1` on the
         *     bus, with the contract's deterministic id
         *     (`sha256(persona_id:trigger_event_id:attempt)`), the trigger's
         *     `network`, `consent` and trace copied from the message being
         *     answered, and a `source` naming the persona — so
         *     `GET /api/suggestions`, the approval screen and `POST /api/approvals`
         *     see one kind of suggestion and the contract gains no type for "a
         *     suggestion that came from outside".
         *
         *     **Authentication.** An HMAC-SHA256 signature over the raw request
         *     body, as `X-Hermes-Signature-256: sha256=<hex>` — the wire format
         *     Hermes's own outbound hook sends. The secret is Twalk's configuration
         *     (`GATEWAY_HERMES_ANSWER_SECRET`) and not Hermes's, and it opens
         *     nothing else on this origin. A device token is not accepted here, the
         *     Sensor's service token is not accepted here, and this signature opens
         *     no other route.
         *
         *     The signature authenticates the **sender and not the content**, which
         *     Nous Research's own documentation says of the inbound direction and
         *     which is equally true of this one. So nothing in the body is trusted:
         *     the reference the answer carries is checked for shape, the message it
         *     names is looked up on the bus, and the sender's consent is read from
         *     this Gateway's own journal **at that moment** — a contact the user
         *     revoked while Hermes was reasoning gets no draft on the approval
         *     screen. Replay is answered twice over: the push's own `timestamp` is
         *     inside the signed body and must be within five minutes of this clock,
         *     and the suggestion's id is deterministic, so a repeated answer
         *     republishes one event the bus absorbs rather than a second draft.
         *
         *     **The answer's shape.** `extra.response_text` is a JSON object with
         *     three required members — `reference` (the `TWALK-REF:` token the
         *     persona sent and Hermes copies back), `reply`, and `language`. The
         *     language is required and an answer without one is **refused rather
         *     than defaulted**: a reply's disclosure is written in the language of
         *     the reply and not the user's (ADR 0031), so a silent default is how a
         *     French disclosure ends up under an English reply with nothing anywhere
         *     to say so.
         *
         *     **The language becomes the disclosure here** (ticket #121). Hermes is
         *     outside this deployment and holds no copy of the contract, so its
         *     answer names a language and the Gateway selects the sentence: the
         *     tag's primary subtag (`fr-CA` is `fr`) is looked up in
         *     `contracts/disclosure/v1/sentences.json` and the suggestion is
         *     published with the sentence as `data.disclosure`, exactly as a
         *     persona's own SDK would have. A language the contract holds no
         *     sentence for — anything but `en`, `fr`, `it`, `es`, `de` today — is
         *     `422 hermes_answer_language_unsupported`, counted, and **no
         *     suggestion at all**: a reply that cannot be disclosed is one that
         *     should not exist (ADR 0031), and the refusal names the five so it
         *     reads as the one-line contribution it is asking for.
         *
         *     **What is ignored rather than refused.** The Hermes hook fires for
         *     every turn of its profile, so a turn the owner typed themselves
         *     reaches this endpoint too. That answers `200` with
         *     `status: "ignored"`, because a run that was never a Twalk wake is not
         *     a failure and an endpoint that answered `4xx` to it would teach an
         *     operator to ignore its errors.
         */
        post: operations["receiveHermesAnswer"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/_twalk/hermes/freebusy": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Hermes reads the owner's free/busy — the one governed pull.
         * @description ADR 0032's founding example (ticket #281, lot 4 of #251): a message
         *     that needs a calendar looked at before three times are proposed.
         *     Everything else Hermes knows was pushed to it through a persona's
         *     wake, after the consent gate; this is the one thing it **asks** for,
         *     and the route is shaped by the reasons it may.
         *
         *     **Authentication.** The same secret as the answers
         *     (`GATEWAY_HERMES_ANSWER_SECRET`), so the operator holds one fact
         *     about the seam. A `GET` has no body to sign, so the signature is
         *     HMAC-SHA256 over the canonical line
         *     `GET\n/_twalk/hermes/freebusy\n<query string as sent>\n<timestamp>`,
         *     carried as `X-Hermes-Signature-256: sha256=<hex>` beside
         *     `X-Hermes-Timestamp` (RFC 3339, within five minutes of this clock —
         *     the answers' window, in the other direction). The query string is
         *     signed **exactly as sent**, not re-encoded. An optional
         *     `X-Hermes-Delivery` names the attempt, for the record.
         *
         *     **The window.** `from` and `to`, RFC 3339, at most **fourteen days**
         *     apart (`window_too_wide` otherwise): a persona proposing times needs
         *     the next few days, and a reader that could ask for a year would be
         *     reading the owner's life one call at a time.
         *
         *     **The connection.** A calendar connection this Gateway's registry
         *     names. One that is not `connected` — or that no collector has spoken
         *     for yet — is refused with its state, the same `409
         *     connection_not_connected` an approval towards it gets.
         *
         *     **The answer.** Busy intervals and nothing else: `[{start, end}]`,
         *     clipped to the window and merged. No title, no participant, no
         *     location: the collector runs a CalDAV `free-busy-query`, which
         *     carries none, and this Gateway relays its answer without adding to
         *     it. The relay is over internal HTTP to the collector
         *     (`GATEWAY_COLLECTOR_URL`, this Gateway's `GATEWAY_SERVICE_TOKEN` as
         *     the bearer — the snapshot seam, in the other direction), so the one
         *     process that holds the owner's grant is the one that reads their
         *     agenda.
         *
         *     **The record.** Every read is recorded, served or refused: the
         *     connection, the window, when, which delivery, the outcome
         *     (`hermes_read` in the Gateway's store,
         *     `twalk_companion_gateway_hermes_reads_total{outcome}` on
         *     `/metrics`). A pull that left no trace would be the one thing here
         *     the owner could not audit.
         *
         *     How Hermes calls this — the canonical line, the headers, what to do
         *     with the answer — is the skill Twalk ships,
         *     `skills/twalk-calendar/SKILL.md`: a document that teaches Twalk to
         *     Hermes, not a coupling.
         */
        get: operations["readHermesFreeBusy"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/{companionPath}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The Companion's own build.
         * @description Every path the Gateway's own routes do not claim is the Companion's.
         *     One templated segment stands here for the whole remaining path,
         *     however many segments it has: OpenAPI path templating cannot express
         *     a wildcard, and this operation is documentation for a human and a
         *     conformance test, not something a generated client calls — the
         *     browser fetches these paths by navigating.
         *
         *     The resolution order is SvelteKit's static-adapter preview server's:
         *     the exact file; else the path plus `index.html` (trailing slash) or
         *     plus `.html`; else a `307` to whichever trailing-slash spelling the
         *     build does have; else the SPA fallback (`200.html`) with `200`, so a
         *     deep link reloaded cold loads the app instead of a `404`.
         *
         *     The `/api/` prefix is the one exception to that fallback: under it a
         *     `404` stays a `404`, as JSON, because a client parsing an API
         *     response must never be handed an HTML page instead.
         */
        get: operations["getCompanionFile"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/approvals": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Approve one suggestion, and send the reply.
         * @description The human act that turns a suggestion into an outbound reply
         *     (`CONTEXT.md`). It publishes a `persona.reply.approved.v1` on the
         *     bus, which the Sensor consumes and posts into the portal room.
         *
         *     **One suggestion.** There is deliberately no endpoint that approves a
         *     list: an approval is a deliberate act and never a batch, so a body
         *     carrying an array is refused with `approval_is_not_a_batch` rather
         *     than helpfully interpreted. A client that wants to approve three
         *     replies sends three requests, and the user clicks three times.
         *
         *     **Who approved it** is this deployment's owner, from configuration.
         *     `approved_by` may be stated and must then be that same Matrix ID; a
         *     request naming anybody else is `403`, because silently rewriting the
         *     one identity field of an audit trail is worse than refusing.
         *
         *     **The consent check is made now, not when the suggestion was
         *     produced.** A suggestion the persona wrote while the contact was
         *     granted is refused if the user has revoked that contact since — the
         *     definition's "at that moment", and the reason this endpoint is on
         *     the Gateway rather than on the Hermes runtime, which would have had
         *     to ask over HTTP for state the Gateway itself writes (ADR 0022).
         *
         *     **There is no queue.** The reply is published inside this request or
         *     it is not published at all: a `201` means the bus acknowledged it and
         *     names the position it landed at, and a bus that does not answer is a
         *     `502` the user can retry. An approval is never "accepted, we will try
         *     later", because a send held for later is a send whose consent check
         *     has gone stale.
         *
         *     **The disclosure is appended here** (ticket #121, ADR 0019, ADR
         *     0031). When the suggestion carries `disclosure` — the sentence the
         *     persona selected in the language it wrote in, *"Rédigé avec mon
         *     assistant IA."* — and the switch (`GET /api/settings/disclosure`) is
         *     on, the published event's `final.body` is the approved body, a
         *     newline and that sentence, and the event carries the sentence again
         *     as `data.disclosure`. The body the user approves or edits is the
         *     reply alone: the sentence is not in `final.body` of the request, is
         *     not counted against its limit, and cannot be edited out. `edited`
         *     compares the body alone. With the switch off, neither the line nor
         *     the member is on the event; the Gateway composes no sentence of its
         *     own, so a suggestion that carries none goes out undisclosed and says
         *     so in the log.
         *
         *     The suggestion and the message it answers are read from the bus,
         *     which has no index from an event id to a stream position, so the read
         *     is bounded (`GATEWAY_APPROVAL_LOOKUP_WINDOW`). The bound is visible
         *     in the answer: a suggestion the search did not reach is `410
         *     suggestion_out_of_reach` and never `404 suggestion_not_found`.
         */
        post: operations["approveSuggestion"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/approvals/{suggestion_event_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * What became of one approval.
         * @description The answer to "did that reply actually go out?", read from the
         *     Gateway's own record.
         *
         *     Every answer is terminal. `publication` is `published` with the
         *     stream position the reply landed at, or `unpublished` - which means
         *     this Gateway wrote the row and the publication did not land, a crash
         *     between the two, repaired by approving the same suggestion again
         *     (the bus deduplicates on the contract's id, so nothing is sent
         *     twice). There is no "in flight": an approval publishes inside its own
         *     request or it is refused, so there is no state here a screen should
         *     render as a spinner.
         *
         *     No answer of this endpoint carries the text that was sent. The reply
         *     is on the bus, where the retention is declared; the Gateway keeps the
         *     suggestion's id, who approved it, whether they edited it and where it
         *     landed.
         */
        get: operations["getApproval"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/bootstrap/account": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Create this deployment's one account, once.
         * @description Wireframe screen 2. The Gateway creates the account on the homeserver
         *     with Synapse's admin registration endpoint, which is authenticated by
         *     a MAC keyed with the registration shared secret rather than by an
         *     admin token — and which works while the homeserver's own
         *     self-service registration stays off. That is the point: a personal
         *     server does not become a public one to give its owner an account.
         *
         *     **Exactly one account, ever.** `username` must be the localpart of
         *     this deployment's `GATEWAY_OWNER`; anything else is `403`. Once that
         *     account exists, every further attempt is `409`, enforced both by the
         *     Gateway's own store and by the homeserver's `M_USER_IN_USE`, so
         *     losing the Gateway's volume does not re-open the window.
         *
         *     **No credential is required, and none exists yet.** This runs before
         *     the account anyone could sign in with, so it is the one `/api`
         *     endpoint outside the guard. What stands in for authentication is the
         *     rule above, plus the operator's decision to set
         *     `GATEWAY_REGISTRATION_SHARED_SECRET` at all: unset, this endpoint
         *     answers `503` and the window never opens. The residual risk is
         *     stated in `docs/architecture/security-model.md`.
         *
         *     **The recovery key is neither accepted nor returned.** It is
         *     generated in the browser and used there (ADR 0014), so screen 2's
         *     "Twalk never sees it" is a property of where the code runs. The
         *     request object is closed, and a body carrying any
         *     recovery-key-shaped member is refused with `recovery_key_refused`
         *     rather than quietly ignored.
         *
         *     **The access token in the answer passes through and is not kept.**
         *     It is the Matrix session the browser continues in — what
         *     matrix-js-sdk bootstraps the cross-signing identity and the recovery
         *     key with. The Gateway writes it nowhere and logs it nowhere
         *     (ADR 0011).
         */
        post: operations["createOwnerAccount"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/bootstrap/rooms": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Invite the Sensor into the rooms the user selected.
         * @description Wireframe screen 3d. The Sensor observes the rooms it was invited to
         *     and nothing else, and an invitation has to come from an account that
         *     is in the room — the user's. So the Companion sends the user's own
         *     Matrix access token with the room list: the Gateway reads the
         *     Sensor's membership in each room, invites it where it is absent, and
         *     **forgets the token when the call returns**. It is a parameter of
         *     this one operation, never stored and never logged (ADR 0011), which
         *     `tests/bootstrap.rs` asserts against the store's bytes and the
         *     captured log output.
         *
         *     Using the user's token rather than an admin credential is also what
         *     works on a homeserver with password login disabled: an invitation
         *     needs no more than the inviter's own session.
         *
         *     One room failing does not fail the others — the user ticked several
         *     and wants to know which took — so `200` carries one outcome per
         *     room, in the order they were asked about. A token the homeserver
         *     rejects is `401` instead, because then nothing was attempted
         *     anywhere. Asking about a room the Sensor is already in is
         *     `already_present`, not an error.
         *
         *     The Sensor's own account is provisioned by the deployment, not by
         *     this endpoint, and its startup depends on nothing here.
         */
        post: operations["inviteSensorIntoRooms"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/bridges": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The bridges this deployment can connect a network through.
         * @description Each entry carries two different things, and the difference is the
         *     whole point of this endpoint:
         *
         *     - **`connection`** — what the bridge itself says, read live from its
         *       `whoami`: the logins it holds, the account each one is linked to,
         *       and the state of the link. This is what "is this network
         *       connected?" is a question about. It is persistent, it lives in the
         *       bridge, and it survives a restart of this Gateway.
         *     - **`login`** — the login *process* this Gateway has in flight: a QR
         *       scan somebody is in the middle of. It lives in memory for at most
         *       thirty minutes and it is **never** an answer to whether the network
         *       is connected.
         *
         *     Reading the second where the first was meant is what made a live
         *     WhatsApp link read as no link at all whenever a login was started,
         *     cancelled, or the Gateway restarted.
         *
         *     Because `connection` is the bridge's answer, this endpoint does
         *     contact each configured bridge — all of them at once, on a short
         *     leash. A bridge that cannot be reached answers
         *     `connection.reachable: false` with a `null` state: the Gateway does
         *     not know, and says so rather than reporting `disconnected`. The list
         *     itself never fails, so the networks screen still draws while every
         *     bridge is down.
         *
         *     A deployment with no bridge configured answers an empty list. That is
         *     the honest answer to "what can I connect?", not an error — bridges
         *     are opt-in in the reference deployment, behind a compose profile.
         */
        get: operations["getBridges"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/bridges/{bridge_id}/login": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The state of the login in flight — the document the Companion polls.
         * @description **Always immediate.** The blocking step of a mautrix login lives
         *     inside the Gateway, not in the browser: the Gateway sits in
         *     `POST /_matrix/provision/v3/login/step/…/display_and_wait` — which
         *     does not answer until the phone does — and this endpoint reports
         *     whatever the last answer was. A phone that sleeps mid-scan therefore
         *     loses a poll and not the login.
         *
         *     `generation` is the number to watch: it changes every time the bridge
         *     hands back a new step, **refreshes included**. A refreshed QR is a
         *     bumped generation with a new `step.payload.data` — that is the signal
         *     to redraw, before the code on screen expires. `step.expires_at` and
         *     `step.valid_for_seconds` say how long the one on screen is worth
         *     acting on; for a QR that is the Gateway's own estimate of the
         *     network's refresh interval, because the bridge states no expiry.
         *
         *     A login that finished, failed or was cancelled is still reported, so
         *     the Companion can show how it ended; `404 no_login_in_flight` means
         *     nothing was ever started on this bridge.
         */
        get: operations["getBridgeLogin"];
        put?: never;
        /**
         * Start a login on this bridge, or repair an existing one.
         * @description Answers at once, with the flow's **first step already in it**: a QR
         *     flow returns the first code here, and the Companion polls
         *     `GET` on this same path from then on.
         *
         *     Pass `login_id` to **reconnect**: the flow then re-logs in to the
         *     login the bridge already holds (mautrix's `?login_id=`), which is how
         *     a broken session is repaired. It never restarts a container, and it
         *     never creates a second login.
         *
         *     One login at a time per bridge instance in v0.1 — the contract has no
         *     login dimension yet (spec #47) — so starting a second while one is in
         *     flight is `409 login_in_flight`, whose `detail` names the device and
         *     the instant that started the first. A login that has completed,
         *     failed or been cancelled does not stand in the way.
         *
         *     The acting Matrix user is this deployment's owner, from
         *     configuration: mautrix's shared-secret auth takes the acting user on
         *     trust, so it is never something a request can choose (ADR 0011).
         */
        post: operations["startBridgeLogin"];
        /**
         * Cancel the login in flight.
         * @description Cancels the current step — which is what releases the request the
         *     Gateway is holding — and then the process, so the bridge forgets it
         *     rather than keeping it until its 30-minute cap. The login's state
         *     then reads `cancelled` until something else is started, and the
         *     bridge is free for the next attempt at once.
         *
         *     A bridge that has already forgotten the process is not an error here:
         *     the login is over either way, which is what was asked for.
         */
        delete: operations["cancelBridgeLogin"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/bridges/{bridge_id}/login/flows": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The login flows this bridge offers.
         * @description Asked of the bridge itself (`GET /_matrix/provision/v3/login/flows`),
         *     so the list is the bridge's and not a table Twalk keeps: a QR flow, a
         *     phone number, a cookie paste, whatever that bridge version has. The
         *     `id` of one of them is what `POST .../login` takes as `flow_id`.
         */
        get: operations["getBridgeLoginFlows"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/bridges/{bridge_id}/login/submit": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Answer the step the login is waiting on.
         * @description For a step whose `type` is `user_input` or `cookies` — the ones where
         *     the login waits for the user rather than for the network. `data` is
         *     passed to the bridge **untouched**: the Gateway does not know what a
         *     given network's input fields are called and does not pretend to.
         *
         *     What travels here is a network credential — a phone number, the seven
         *     Google cookies of the SMS preview path — and it is relayed and
         *     forgotten (ADR 0011). Nothing stores it, and the Gateway's log line
         *     records the *shape* of what went through and never a value.
         *
         *     The answer is the login's new state: the next step, or `complete`.
         *     Submit against the `step_id` the polled state reports; anything else
         *     is `400`, because a stale client answering an old step would
         *     otherwise silently do nothing.
         */
        post: operations["submitBridgeLoginStep"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/bridges/{bridge_id}/logins": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The logins this bridge already holds.
         * @description Asked of the bridge (`GET /_matrix/provision/v3/logins`). This is
         *     what the Companion reads to offer "reconnect": the `login_id` to pass
         *     back to `POST .../login`, and the name the network gives the account.
         *
         *     One login per bridge instance is the v0.1 constraint, so this list
         *     normally holds none or one; it is a list because the bridge's API is,
         *     and because a second account on one network is what a later contract
         *     version adds.
         */
        get: operations["getBridgeLogins"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/bridges/{bridge_id}/logins/{login_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Log this login out of its network.
         * @description The bridge drops the session and the network credentials it kept for
         *     it; the Gateway never held either, so there is nothing on its side to
         *     forget. The network's own "linked devices" list is where the user
         *     sees the other half of this.
         *
         *     A login the bridge does not have is `404 not_found_on_bridge` — the
         *     distinction from `unknown_bridge` matters, because one is a
         *     configuration problem and the other is a stale screen.
         */
        delete: operations["logoutBridgeLogin"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/connections": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The registry of connections, the perimeters consent is scoped to.
         * @description Every configured account this deployment observes or acts through
         *     (ADR 0033, #269): a bridge, a mailbox, a calendar. Each has an opaque
         *     stable id, a kind — one of the contract's
         *     `definitions/kind.schema.json` — and a label; the ones a bridge
         *     carries name the bridge and, when the operator named one, its bot.
         *
         *     Configuration, read-only: `GATEWAY_CONNECTIONS` (`id=kind[=label]`;
         *     the bridge a connection rides is the `GATEWAY_BRIDGES` entry of the
         *     same id, else the only bridge of its kind), or, unset, one connection
         *     per bridge **whose id is the network's name** — the id every existing
         *     consent decision was migrated onto (#270). A second bridge of one
         *     network is declared, never derived. The native `matrix` connection —
         *     the user's own account on the homeserver — is in every registry.
         *
         *     The Companion draws a card per connection and decides per connection
         *     (#272); `network` on a card is its kind.
         *
         *     Since #275 an entry carries `status` when the connection's own
         *     collector has said what state it is in (`connection.status.changed.v1`,
         *     read off the bus): one of the contract's four states, when it was
         *     observed, the service it is about and the operator's hint — the
         *     Companion's card shows the state as one of four sentences, never one.
         *     A connection nobody has spoken for (a bridge's, whose state is
         *     `GET /api/bridges`'s; a collector that has not run) has no `status`.
         *     `transitions` lists the most recent state changes, newest first, for
         *     the dashboard's activity feed.
         */
        get: operations["listConnections"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/consent/decisions": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Record one consent decision.
         * @description The body mirrors the contract's own `data` object
         *     (`contracts/cloudevents/v1/consent.state.changed.schema.json`) minus
         *     what the Gateway stamps itself: `old_state` comes from the journal,
         *     `occurred_at` from the Gateway's clock, `actor` from the
         *     deployment's owner.
         *
         *     The answer returns as soon as the decision is **committed**.
         *     Publication follows through a transactional outbox, so a bus that is
         *     down delays the event and never refuses the decision — and a crash
         *     between the commit and the publication republishes rather than
         *     loses, deduplicated on the bus by the event's own deterministic id.
         *
         *     A decision about the **owner** is refused with `409
         *     subject_is_the_owner`: the owner is never a contact and never has a
         *     consent state (ADR 0018, ADR 0021), so there is nothing to record and
         *     saying so beats dropping it silently.
         *
         *     `subject.type` is `contact`, `network` or `persona`. A persona is on
         *     this path and not on a control API of its own, because activating or
         *     pausing one *is* a consent decision (ADR 0013): `scope.networks` on a
         *     persona subject names the networks that persona may read, so
         *     activation never spreads — a network connected afterwards is in no
         *     scope the user decided on, and the persona stays inactive on it until
         *     they say otherwise. Pausing a persona is `new_state: revoked` on it:
         *     the persona still runs and is still supervised, and receives nothing.
         */
        post: operations["recordConsentDecision"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/consent/effective": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The consent state that applies to one contact on one network.
         * @description The precedence, resolved: the contact's own decision if it has one,
         *     the network's default otherwise, and `pending` when neither exists —
         *     with `decided_by` naming the decision that answered, or `null` when
         *     none did. That `null` is how a caller tells "never decided" from
         *     "decided pending"; an absent decision is never a revocation.
         *
         *     Asked about an **owner identity**, this endpoint refuses rather than
         *     answers (`409 subject_is_the_owner`). `pending` with `decided_by:
         *     null` means "no decision was ever recorded", which is a contact's
         *     state; about the owner it would read as an invitation to go and take
         *     the decision, and there is none to take (ticket #149, ADR 0018,
         *     ADR 0021).
         */
        get: operations["getEffectiveConsent"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/consent/snapshot": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The whole consent state, with the stream position it reflects.
         * @description What a consumer whose cache is cold reads before it follows the bus
         *     (ADR 0010). A durable consumer resumes at its ack floor, so decisions
         *     it has already applied are never redelivered; this endpoint answers
         *     the question directly, and names the JetStream sequence its content
         *     reflects so the consumer can carry on from there.
         *
         *     **The hand-off.** Apply `entries`, then create the stream consumer at
         *     `next_stream_sequence` — which is `stream_sequence + 1`, spelled out
         *     because that off-by-one is the one mistake that would skip a
         *     decision. Every decision ever taken is then in exactly one of the
         *     two: in this snapshot, or on the stream after `stream_sequence`.
         *     Never both, never neither.
         *
         *     That holds because the snapshot reflects the journal's **published
         *     prefix**: a decision still waiting in the Gateway's outbox has no
         *     position on the bus yet, so it is left out of the content and
         *     arrives, in order, after the position named here. A bus outage
         *     therefore delays what the snapshot knows rather than corrupting the
         *     hand-off. With nothing published yet, `stream_sequence` is `0` and
         *     the consumer starts at `1`: the whole stream.
         *
         *     **What is in `entries`.** One entry per (subject, network):
         *     `network` entries are that network's default and `contact` entries
         *     override them, exactly as `GET /api/consent/state` reports them and
         *     with the precedence resolved by `GET /api/consent/effective`.
         *     Revocations are as explicit as grants — an absent subject means
         *     "never decided", never "revoked", which is the distinction the shape
         *     exists to keep. `persona` subjects are excluded: activating a persona
         *     is a consent decision (ADR 0013), but it is not state a consumer
         *     labels senders by.
         *
         *     **The owner is not in `entries`, and is named in
         *     `owner_identities`.** The owner is never a contact and never has a
         *     consent state (ADR 0018, ADR 0021), so this document serves none —
         *     including a row recorded before the Gateway knew whose identity it
         *     was, since the exclusion is at read time and nothing is deleted from
         *     an append-only journal (ticket #149). That matters most here: this is
         *     the document a consumer builds its whole cold cache from, and a
         *     consumer without a filter of its own would apply a `revoked` on an
         *     owner ghost and silence the user's own traffic with nothing to say
         *     why.
         *
         *     **Not paginated.** A cursor would be a second ordering to get wrong,
         *     and a half-applied snapshot is worse than none. Instead there is a
         *     cap (`GATEWAY_CONSENT_SNAPSHOT_MAX_ENTRIES`, 100000 by default) and a
         *     state over it is refused with `snapshot_too_large` rather than
         *     truncated.
         *
         *     **Authentication.** A service token from the Gateway's own
         *     configuration (`GATEWAY_SERVICE_TOKEN`), as `Authorization: Bearer`.
         *     Its caller is the Sensor: a service, not one of the owner's browsers,
         *     with no Matrix OpenID token to sign in with and no cookie to send.
         *     The two credentials are disjoint — a device token is not accepted
         *     here, and this token opens no other endpoint. There is no timestamp
         *     and no version in the answer: the stream sequence is the only
         *     ordering this design trusts.
         */
        get: operations["getConsentSnapshot"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/consent/state": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The current consent state, as recorded.
         * @description One entry per (subject, network): the state the most recent decision
         *     covering them left behind. It is a projection of the decision
         *     journal, not a second record of truth.
         *
         *     `network` entries are that network's default and `contact` entries
         *     override them — this endpoint reports what was *recorded*, and
         *     `/api/consent/effective` applies the precedence. Revocations are as
         *     explicit as grants, and an absent subject means "never decided",
         *     never "revoked".
         *
         *     The **owner** is not in it: not their Matrix ID and not one of their
         *     network ghosts (`GATEWAY_OWNER_IDENTITIES`), including one a decision
         *     was recorded about before this Gateway knew whose identity it was
         *     (ticket #149, ADR 0018, ADR 0021). Such a row is not deleted — the
         *     journal is append-only — it is served to nobody, and counted on
         *     `/metrics` as `twalk_companion_gateway_owner_consent_rows`.
         *
         *     This endpoint is the owner's own read, behind a device token, and it
         *     names no stream position. It **includes** `persona` subjects, which is
         *     how the dashboard knows which personas the user activated and on which
         *     networks. The snapshot a cold consumer reads is
         *     `GET /api/consent/snapshot`: the same entries plus the JetStream
         *     sequence they reflect, authenticated by the Sensor's service token,
         *     and with `persona` subjects excluded — a persona is not consent state
         *     a Sensor labels senders by. Neither is paginated.
         */
        get: operations["getConsentState"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/contacts/display-names": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * What these contacts are called, read from the bus.
         * @description A separate call from the list, because the Gateway stores no display
         *     name: a record of who writes to the user *and what they are called*
         *     is a directory, and the pending-contact store exists precisely to not
         *     be one. A name is looked up when a screen needs it and kept nowhere
         *     afterwards - not in the store, not in a cache, not in the logs.
         *
         *     The Gateway reads the tail of the inbound stream, takes the most
         *     recent name it finds for each contact asked about, and drops
         *     everything else it walked past. The read is bounded, so a contact
         *     whose last message has fallen outside that window comes back with
         *     `display_name: null`. That is a real answer and not a failure: the
         *     Companion then shows the Matrix ID, which is what the decision will
         *     name anyway.
         *
         *     `contact` repeats, once per contact
         *     (`?contact=@a:example.com&contact=@b:example.com`), and the answer
         *     holds one entry per distinct contact asked about, in the order they
         *     were asked. Asking about more than 200 at once is refused rather than
         *     truncated, so a short answer never passes for a complete one.
         */
        get: operations["getContactDisplayNames"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/contacts/pending": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The contacts waiting for a consent decision.
         * @description Screen 5's "3 consent decisions waiting", and the list behind it.
         *
         *     **What a contact is here.** Four values, and there will never be a
         *     fifth: the sender's Matrix user ID as the bridge materialised it, the
         *     network it wrote on, and the first and last instants it did - taken
         *     from the events' own `time`, never from the Gateway's clock, so a
         *     Gateway installed after weeks of Sensor traffic reports when those
         *     contacts actually wrote. There is no message body here, no display
         *     name and no `network_identifier`: the Gateway's store holds none of
         *     them, so no read of it can hand them out. Display names are a
         *     separate call, answered from the bus and written nowhere - see
         *     `GET /api/contacts/display-names`.
         *
         *     Note what storing the Matrix ID does and does not hide: a bridged
         *     ghost user's ID conventionally embeds the network identifier
         *     (`@whatsapp_33612345678:example.com`), so this list keeps phone
         *     numbers although it has no field for one. It is stored because a
         *     consent decision has to name its subject and that ID is the subject.
         *
         *     **What "waiting" means.** The same question
         *     `GET /api/consent/effective` answers with `decided_by: null`: neither
         *     this contact's own decision nor its network's default exists. So
         *     granting or revoking a whole network empties this list of every
         *     contact on it at once, and a contact the owner deliberately left
         *     `pending` is *not* in it - they answered, and the answer was "not
         *     yet". The **owner** is never in it either, under any of their
         *     identities - their Matrix ID and the network ghosts of
         *     `GATEWAY_OWNER_IDENTITIES`: their own messages travel through the same
         *     rooms, and nobody is their own correspondent. A sighting of one of them
         *     is not recorded at all; one an older build recorded - before #109 the
         *     user's own messages were published as a contact's - is excluded when
         *     this list is read, so an upgraded deployment stops offering the user a
         *     decision about their own ghost (ticket #149, ADR 0018, ADR 0021).
         *
         *     **The numbers.** `total`, `connections` and `networks` always count
         *     the whole list, whatever `?connection=` or `?network=` narrows
         *     `contacts` to, so a badge and the list beside it can never disagree.
         *     A screen that decides per connection (#272) reads `connections`;
         *     `networks` adds the connections of one kind up.
         *
         *     Not paginated and not capped, like `GET /api/consent/state`.
         */
        get: operations["getPendingContacts"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/deployment": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * What this deployment is, before anyone can sign in.
         * @description Unauthenticated, and deliberately narrow. It exists so a screen can
         *     **ask** what it needs to know instead of attempting something and
         *     reading the failure — the pattern behind four defects found in one day
         *     of live testing (ticket #112): a first screen that offered to create an
         *     account and discovered on submit that one existed, a network screen
         *     that rendered an expired session as "your server could not be reached".
         *
         *     `bootstrapped` is whether this deployment has its one account — on the
         *     homeserver, whoever created it. The Gateway's own memory of the
         *     registration relay succeeding settles it when that row exists; when it
         *     does not (an account provisioned by hand, a state directory recreated
         *     or restored from before onboarding), the homeserver is asked, because
         *     a working deployment whose owner is told it has no account and shown
         *     no way in is the failure this operation exists to end (ticket #133).
         *     It is the same fact the registration relay refuses on, so this
         *     publishes nothing a registration attempt would not reveal.
         *     `homeserver` is the server name, which is in the deployment's own DNS.
         *
         *     Absent by design: the owner's Matrix ID. Naming the human who owns a
         *     deployment to anyone who can reach it is a different disclosure, and no
         *     screen needs it before sign-in.
         */
        get: operations["describeDeployment"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/devices": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The devices that have signed in.
         * @description Every device, revoked ones included and dated, so that "I revoked
         *     that phone" stays visible. The device the request came from is
         *     flagged `current`.
         */
        get: operations["listDevices"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/devices/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Revoke one device.
         * @description The device's token stops working on its very next request, including
         *     when that device is the caller's own — nothing is cached. The row
         *     stays in the list with its revocation date; its token digests are
         *     dropped.
         */
        delete: operations["revokeDevice"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/portals": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Every conversation the bridges have built, and where the Sensor stands in each.
         * @description The read a per-conversation chooser is drawn from, and the answer to
         *     a question this deployment could not previously ask itself: *how
         *     many of my conversations is the Sensor outside?*
         *
         *     Read live, as each bridge's own bot, through the appservice token
         *     that bridge's configuration already carries
         *     (`GATEWAY_BRIDGE_<ID>_AS_TOKEN`). Nothing here is stored: a
         *     conversation's name and its member count are read for this request
         *     and forgotten with the response, and `observation` is the Sensor's
         *     own `m.room.member` event rather than a row the Gateway wrote.
         *
         *     `summary` is in the answer rather than left to the client, because
         *     "the Sensor is outside 17 of your 18 conversations" is a sentence
         *     the deployment states. `/metrics` states the same numbers as
         *     `twalk_companion_gateway_portal_rooms`.
         *
         *     `bridges` lists **every** configured bridge, readable or not. A
         *     bridge the Gateway has no appservice token for contributes no
         *     portals, and a total that quietly covered fewer bridges than the
         *     user has connected would reproduce the very defect this endpoint
         *     exists for.
         *
         *     `members` excludes the bridge bot and the Sensor, so deciding to
         *     observe a conversation never changes how large it looks.
         *
         *     No message content is read at any point: the register reads room
         *     state, never a timeline.
         */
        get: operations["listPortals"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/portals/moves": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The conversations whose room was replaced while the Sensor was in them, and what the register decided.
         * @description The register's journal of **moves** (ADR 0029, #255). A conversation's
         *     room can be replaced — a Telegram group promoted to a supergroup, a
         *     Matrix room upgraded — while the user's decision to observe it is on
         *     record. The register follows `m.room.tombstone` to the successor and
         *     decides what that decision now means: **followed** when the successor's
         *     audience is under the crowd threshold (the Sensor is invited there),
         *     **returned to the chooser** when it is not (the conversation stays
         *     `moved` and asks to be acknowledged again as a crowd).
         *
         *     Either way the move is said here, once per successor, with the numbers
         *     it was decided on. A deployment that changed rooms under the user
         *     without being able to say so is one whose history they cannot check;
         *     the dashboard's activity feed draws from this. Read from the Gateway's
         *     store alone — no homeserver call.
         */
        get: operations["listPortalMoves"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/portals/observation": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Decide which conversations the Sensor observes.
         * @description The user's decision, in either direction, over one conversation or
         *     over a selection of them.
         *
         *     `observed: true` invites the Sensor into each named portal room, as
         *     that bridge's own bot — the account that created the room and is in
         *     it. The Sensor joins on its own when it next syncs, **provided
         *     `SENSOR_ALLOWED_INVITERS` names that bot**; a portal that stays at
         *     `invited` is what a deployment missing that setting looks like.
         *
         *     `observed: false` removes the Sensor from the room, again as the
         *     bot. Stopping observation is the exact inverse of starting it,
         *     through one credential and one mechanism, rather than a second
         *     control plane for a membership Matrix already models.
         *
         *     A room this Gateway does not hold as a portal is `unknown_portal`
         *     and **no call is made with the appservice credential**: its reach is
         *     its own bot's rooms, and a room id in a request is never a reason to
         *     widen it. Inviting the Sensor into a room of the user's own is a
         *     different operation with a different credential
         *     (`POST /api/bootstrap/rooms`).
         *
         *     One room failing does not fail the others: `200` carries one outcome
         *     per room, in the order they were asked about. A request naming
         *     twelve conversations where one is refused still holds eleven
         *     decisions the user made.
         */
        post: operations["setPortalObservation"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/runtime": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Whether a persona runtime is hosting personas here.
         * @description A projection of the bus's consumer list, stored nowhere. The runtime
         *     creates one durable consumer per persona it hosts (`persona-<id>`)
         *     and the persona pulls from it in a loop; that consumer is the one
         *     thing a runtime leaves behind, and it outlives the process that made
         *     it — so existence alone would say "a runtime was once here", and this
         *     read says more.
         *
         *     `presence` is one of three words, distinguishable by the caller and
         *     not only in the Gateway's log:
         *
         *     - `never` — no persona consumer on the stream: no runtime configured
         *       with a persona has ever run against this bus;
         *     - `gone` — consumers exist and none is live: a runtime was here and
         *       is not now;
         *     - `present` — at least one consumer has a pull outstanding or a
         *       message delivered and awaiting its ack: a runtime is here, hosting
         *       personas.
         *
         *     A consumer is **live** when a process is on the other end right now:
         *     a pull request waiting (`waiting_pulls`), or a message taken and not
         *     yet acknowledged (`ack_pending`, which is what a persona in a model
         *     call looks like between two fetches). The persona's loop leaves a gap
         *     of milliseconds between one pull expiring and the next, so the read
         *     samples the list more than once before calling a consumer idle. The
         *     one residual is stated: a persona that crashed mid-message reads as
         *     live until the bus redelivers, which is the consumer's `ack_wait`.
         *
         *     Not read: whether a `hermes` container or process is up. The
         *     reference deployment deliberately keeps an unconfigured runtime
         *     running and hosting nothing (ADR 0015, ADR 0023), so presence of the
         *     process would be the wrong fact.
         *
         *     Each persona is listed with the counters its verdict was read from,
         *     and with `activation` — the filter subject the runtime set on the
         *     consumer (ADR 0013): `active`, or `paused`, which is a runtime that
         *     is here and a persona that receives nothing.
         */
        get: operations["readRuntimePresence"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/session": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Who is signed in, on which device.
         * @description What the Companion asks on boot to know whether it must show the
         *     sign-in screen: a `200` means the cookie it already holds is live.
         */
        get: operations["getSession"];
        put?: never;
        /**
         * Sign this device in with a Matrix OpenID token.
         * @description The Companion asks its homeserver for an OpenID token
         *     (`POST /_matrix/client/v3/user/{userId}/openid/request_token`) and
         *     forwards that document here, unchanged. The Gateway verifies it at
         *     the homeserver's federation `openid/userinfo` endpoint, checks the
         *     resulting Matrix ID against the deployment's single owner, refuses a
         *     token it has already accepted, and issues its own pair of tokens.
         *
         *     The Gateway never receives, stores or logs the Matrix access token
         *     that minted the OpenID token (ADR 0011).
         *
         *     On success, two `Set-Cookie` headers carry the credentials: the
         *     device token (`twalk_device`, `Path=/`) and the refresh token
         *     (`twalk_refresh`, `Path=/api/session`). The response body names
         *     `expires_in` so the client refreshes on time instead of discovering
         *     the expiry as a `401`.
         */
        post: operations["signIn"];
        /**
         * Sign this device out, revoking it.
         * @description Signing out revokes the device: a device the user signed out of is a
         *     device that should stop working, and signing in again is a silent
         *     round trip for the Companion. Both cookies are cleared.
         */
        delete: operations["signOut"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/session/refresh": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Exchange the refresh token for a new pair.
         * @description Authenticated by the refresh cookie rather than the device cookie —
         *     the device token is short-lived by design. Both tokens are rotated,
         *     so a device token that leaked dies at the next refresh instead of
         *     living out its lifetime; the previous refresh token stops working
         *     immediately.
         *
         *     The refresh cookie is scoped to `/api/session`, so it is not sent
         *     with every static file and every API call.
         */
        post: operations["refreshSession"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/settings/disclosure": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Whether approved replies carry the disclosure, and who last decided.
         * @description The switch ADR 0019 requires and ADR 0031 placed (ticket #121). Every
         *     reply a persona drafted reaches the contact with one sentence after
         *     it, on a line of its own, in the language the reply was written in —
         *     *"Rédigé avec mon assistant IA."* — and this is the one control over
         *     that: **global**, never per message, because a sentence that can be
         *     argued down conversation by conversation is one that ends up nowhere.
         *
         *     It is on by default and the answer says so with `enabled: true` and
         *     nothing else: `since`, `actor` and `reason` are `null` while nobody
         *     has decided. After a decision they name when it was taken and by whom
         *     (this deployment's owner — the only person who can), so the approval
         *     screen can say "the disclosure is not added: turned off on … by …" at
         *     the one moment the user is thinking about a particular message going
         *     to a particular person.
         *
         *     The record is an append-only journal in the consent store — a
         *     sibling of the consent journal, sharing its shape and its triggers,
         *     and deliberately not the settings table, which would forget who
         *     decided and what was before. So on a deployment with no bus there is
         *     no journal and no approval path for it to govern, and the answer is
         *     `503 consent_not_configured`.
         */
        get: operations["getDisclosureState"];
        /**
         * Turn the disclosure off, or back on, as a recorded decision.
         * @description Appends one decision to the disclosure journal and answers the state
         *     it leaves behind. `actor` is stamped by the Gateway — this
         *     deployment's owner, from configuration, exactly as a consent
         *     decision's is — and `reason` is the user's own note, optional and at
         *     most 1 024 characters, kept with the decision.
         *
         *     Every call appends, including one that restates the current state:
         *     the journal is the record of what was decided and when, not a
         *     projection to keep tidy. Nothing is retroactive: a reply approved
         *     while the switch was off went out without the sentence, and turning
         *     it on does not send one after it.
         */
        put: operations["putDisclosureState"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/settings/language": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The user's native language, and the five the Companion ships.
         * @description A stored preference rather than a browser fact, because a persona
         *     runs in a container and cannot read `navigator.language`
         *     ([ADR 0016](../docs/architecture/adr/0016-a-reply-follows-the-conversation-not-the-user.md)).
         *     It governs the interface, the explanations, and the language a
         *     persona falls back to when it cannot tell what language the message
         *     it is answering was written in. **It never governs the text sent to
         *     a contact**: a suggestion follows the conversation, because a French
         *     user answering an English-speaking contact in French has been handed
         *     something they cannot send.
         *
         *     `language: null` is "no preference", which is not the same as
         *     English. A persona with no fallback follows the incoming message and
         *     nothing else; defaulting silently to English is exactly what
         *     [#164](https://github.com/linagora/twalk/issues/164) found in
         *     production, against a French speaker who wrote `test`.
         */
        get: operations["getLanguagePreference"];
        /**
         * Set, or with null forget, the user's native language.
         * @description One of the five the Companion ships, spelled exactly as the
         *     catalogues are (`fr`, never `fr-FR` and never `FR`), or `null` for
         *     no preference. A closed list, so a tag no catalogue exists for is
         *     refused here rather than discovered later by a persona.
         */
        put: operations["putLanguagePreference"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/settings/model": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The endpoint and model the operator named, without the credential.
         * @description Twalk ships no LLM and has no default: nothing is ever sent to a
         *     model the operator did not name, and a persona refuses to start
         *     without one ([ADR 0015](../docs/architecture/adr/0015-no-default-llm-configured-through-the-companion.md)).
         *     This is what they named, and what the Hermes runtime will inject.
         *
         *     **`configured: false` is a state, not a refusal.** A deployment
         *     whose operator has not chosen a model yet answers `200` with every
         *     member `null`, so a settings screen draws it without an error
         *     branch. "No endpoint configured at all" stays distinguishable from
         *     every way an endpoint can fail, which is what
         *     `POST /api/settings/model/probe` reports.
         *
         *     **The credential never comes back.** `credential` says whether one
         *     is configured, which of the two sources is in force, and its last
         *     four characters — enough for a human to recognise the key they
         *     pasted, not enough for anyone to use it. The only answer on this
         *     origin that carries the credential itself is
         *     `GET /api/settings/runtime`, whose caller is the Hermes runtime.
         *
         *     **A credential the operator supplied as a file wins** over one set
         *     here (ADR 0015). That is how the reference deployment runs — the
         *     model name from the browser, the key from a file — so
         *     `credential.source` is `file` while `companion_credential_stored`
         *     stays `true`, and the interface can answer "why is the key I pasted
         *     not being used?".
         */
        get: operations["getModelConfiguration"];
        /**
         * Name the endpoint, the model, and optionally the credential.
         * @description Replaces the configuration whole.
         *
         *     **The recommended shape is an OpenAI-compatible proxy in front of
         *     the model** — the reference deployment runs LiteLLM in front of Qwen
         *     at OVH, and Twalk therefore sees `http://127.0.0.1:4000/v1` serving
         *     a model called `qwen`. Every provider peculiarity (`drop_params`,
         *     `additional_drop_params`, `reasoning_effort`, the provider's real
         *     model id) then lives in the proxy's own configuration, which is what
         *     a proxy is for.
         *
         *     `params` is the **escape hatch**, not the norm: an operator with no
         *     proxy needs it, because providers differ in what they reject — OVH's
         *     AI Endpoints, the first endpoint tried in practice, rejects fields
         *     OpenAI clients send by default. It is merged into every completion
         *     request untouched and **last**, so it also overrides what a persona
         *     asked for, and a member set to `null` **removes** a field the
         *     request would otherwise carry. Leaving it empty is the healthy shape.
         *
         *     **The credential has three cases, because it is write-only and a
         *     client cannot round-trip it.** The member absent keeps whatever is
         *     stored — which is what a screen that only renamed the model sends,
         *     having never been shown the key. `null` forgets the one set from
         *     this browser; it does not touch the operator's file, which is not
         *     the Companion's to remove. A string replaces it.
         *
         *     A member the shape does not have is **refused**, not ignored: a
         *     client that sent `credentials` for `credential` would otherwise
         *     believe it had set a key it had not.
         */
        put: operations["putModelConfiguration"];
        post?: never;
        /**
         * Forget the endpoint, the model and the browser's credential.
         * @description `204` whether or not there was anything to forget: the caller asked
         *     for a deployment with no model configured, and that is what it has.
         *
         *     The operator's credential file is untouched — it is not the
         *     Companion's to remove — so a deployment can be left holding a key
         *     and no model, which is an ordinary state on the way to naming
         *     another one.
         */
        delete: operations["deleteModelConfiguration"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/settings/model/probe": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Ask the configured endpoint whether it is there, and willing.
         * @description Sends **one chat completion of one token** to the configured
         *     endpoint, with the operator's provider parameters merged in exactly
         *     as a persona merges them — so what this proves is what a persona
         *     will do, and not a near-miss of it. It is the only endpoint on this
         *     origin that spends the operator's money, and it does so only when a
         *     human asks. The prompt is the word `ping`: no message content
         *     reaches the endpoint.
         *
         *     **Four causes, four answers.** That is the whole point of this
         *     operation: "it does not work" has four different fixes here, and a
         *     single signal for several causes is the failure pattern behind nine
         *     incidents in two days on this project
         *     ([#116](https://github.com/linagora/twalk/issues/116),
         *     [#141](https://github.com/linagora/twalk/issues/141)).
         *
         *     | answer | what happened | what to do |
         *     | --- | --- | --- |
         *     | `200 outcome: ok` | the endpoint answered a chat completion | nothing |
         *     | `409 model_not_configured` | nothing is configured to probe | name one |
         *     | `502 endpoint_unreachable` | nothing answered: DNS, connection, TLS, or this Gateway's ten-second deadline | check the address and the network. A proxy on `127.0.0.1` is a good address the Gateway can reach and a container of its own network namespace cannot |
         *     | `502 endpoint_refused` | it answered and said no, and `endpoint_status` is its own status | check the credential, then the model name |
         *     | `502 endpoint_not_compatible` | it answered a success that is not a chat completion | wrong port or wrong path: a web server's index page answers `200` very convincingly |
         *
         *     A success does **not** mean the model produced text: with a budget
         *     of one token a reasoning model spends it thinking and answers with
         *     no content ([#162](https://github.com/linagora/twalk/issues/162)),
         *     and that is still a reachable, willing endpoint that knows this
         *     model's name.
         */
        post: operations["probeModelConfiguration"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/settings/runtime": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Everything the Hermes runtime injects into a persona, in one read.
         * @description The one read on this origin that carries the endpoint credential,
         *     because its caller is the process that puts it into a persona's
         *     environment.
         *
         *     **Authentication.** The same service token the consent snapshot
         *     takes (`GATEWAY_SERVICE_TOKEN`), as `Authorization: Bearer`. Its
         *     caller is the Hermes runtime: a service, not one of the owner's
         *     browsers, with no Matrix OpenID token to sign in with. A device
         *     token is not accepted here.
         *
         *     **Why the runtime and not each persona.** That same token opens
         *     `GET /api/consent/snapshot` — the list of every contact the
         *     deployment knows — and a persona has no business holding it to learn
         *     which model to call. A third-party persona takes the same path as a
         *     first-party one, so "a first-party persona would never read it" is
         *     not a control; the control is that the runtime injects the answer
         *     and the variable is not in the persona's environment
         *     (ADR 0008, ADR 0015).
         *
         *     **`llm: null` is the third of three signals.** A runtime that cannot
         *     reach this Gateway sees a transport failure; one whose token is
         *     wrong sees `401`; one told `llm: null` knows the operator has named
         *     no model, and refuses to start saying exactly that. None of the
         *     three is an empty document that could be mistaken for another.
         */
        get: operations["getRuntimeSettings"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/suggestions": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The suggestions personas have proposed, newest first.
         * @description What #100's approval screen draws: what each persona proposed, for
         *     which message, when, whether it has expired and whether it was
         *     already approved.
         *
         *     **This is a projection of the bus, not a second store.** The
         *     suggestion lives in the stream; the Gateway keeps no copy of it, so
         *     there is nothing here that a replay could disagree with. The cost is
         *     that the read is bounded — the same window an approval's lookup uses
         *     (`GATEWAY_APPROVAL_LOOKUP_WINDOW`) — and the bound is in the answer
         *     rather than in the release notes: `window.reached_start_of_stream`
         *     is `false` when older suggestions may exist and were not read.
         *
         *     **It carries nothing of the message being answered.** A suggestion
         *     quotes a contact's message, and an excerpt belongs to the author of
         *     the quoted message rather than to whoever sent the event carrying it
         *     (ticket #110, ADR 0012) — so a listing that resolved the trigger and
         *     showed its text would re-publish a revoked contact's words through a
         *     new door. The trigger appears here as its CloudEvents id and type,
         *     which is identity and not content. The persona's own proposed text
         *     *is* carried: it is the thing being approved.
         *
         *     Whether a suggestion can actually be approved also depends on the
         *     sender's consent **at that moment**, which is not a property of the
         *     suggestion and is not reported here. `POST /api/approvals` is the
         *     only thing that can answer it, and it answers it as a refusal that
         *     names the contact.
         */
        get: operations["getSuggestions"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/suggestions/{suggestion_event_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * One suggestion, by the id of the event that proposed it.
         * @description For a screen that was handed an id — a deep link, a reload, the
         *     answer to an approval that was refused.
         *
         *     This is where three situations are three answers, which is the whole
         *     reason the route exists beside the listing:
         *
         *     - `200` with `standing: "expired"` - it is there, and it is no
         *       longer approvable (ticket #22's policy).
         *     - `200` with `standing: "approved"` - it was approved, and the
         *       `approval` member says where the reply landed.
         *     - `404 suggestion_not_found` - the whole retained stream was read and
         *       no suggestion has this id.
         *     - `410 suggestion_out_of_reach` - the bounded read gave up first, so
         *       it may exist further back than this Gateway looks.
         *
         *     Collapsing any two of those into one signal is the defect this
         *     project has spent eight incidents on. They are the same codes and the
         *     same statuses `POST /api/approvals` answers with, because they are
         *     the same facts about the same bounded read.
         */
        get: operations["getSuggestion"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/suggestions/{suggestion_event_id}/message": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The message this suggestion answers, read on demand.
         * @description The third of #160's options, kept for the one surface that may have
         *     it: the Companion's own approval card, served to the owner through
         *     their identity provider. A relay this deployment does not run — Buzz
         *     — gets the persona's summary and nothing else (#334, #335).
         *
         *     It is a **read on demand**. No listing carries a message; this route
         *     is reached only when the owner asks for one suggestion's trigger, and
         *     it is served by the same module that reads triggers for an approval,
         *     so the rules exist once and not twice. That is the whole design: a
         *     second implementation of the reduction is how #110 happened.
         *
         *     **Consent is read now.** A contact revoked since their message
         *     arrived is a refusal with its code, exactly where `POST
         *     /api/approvals` would refuse to send — a screen that could show a
         *     revoked contact's words because they were granted yesterday would be
         *     a way around the decision.
         *
         *     The body is what the bus holds, which is what the Sensor published
         *     under the reduction that applied when it arrived (ADR 0012, ADR
         *     0028). Attachments are counted, never named.
         */
        get: operations["getAnsweredMessage"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/health": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Liveness, and the version handshake's server half.
         * @description Always `200` while the process answers. Liveness, not readiness.
         *
         *     `version` is the stable half of the Companion's version handshake:
         *     the PWA is installed, so a service worker may hold an app shell built
         *     against an older Gateway. The client compares this value with the
         *     version baked into its own build and reloads when they differ.
         *     `revision` is provenance for an operator, not an input to the
         *     handshake.
         */
        get: operations["getHealth"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/metrics": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The Prometheus text exposition.
         * @description The `twalk_companion_gateway_*` metrics, in the Sensor's conventions,
         *     as Prometheus text exposition format 0.0.4 — served with
         *     `Content-Type: text/plain; version=0.0.4; charset=utf-8`. Not for the
         *     Companion: for the operator's scrape.
         */
        get: operations["getMetrics"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/openapi.yaml": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * This description, as the running binary carries it.
         * @description The bytes of `companion-gateway/openapi.yaml`, embedded in the binary
         *     at build time. What a client generator points at to be sure it is
         *     describing the Gateway it is talking to, rather than a file from
         *     another revision of the repository.
         */
        get: operations["getOpenApiDescription"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        /**
         * @description One approval as this Gateway recorded it. The same shape from
         *     `POST /api/approvals` and from `GET /api/approvals/{id}`, so a client
         *     renders one thing.
         *
         *     There is no member for the reply's text, and there will not be one:
         *     the text is on the bus, where the retention is declared, and the
         *     Gateway keeps the suggestion's id, who approved it, whether they
         *     edited it and where the publication landed. The published event's
         *     `final.body` is that text followed, on a line of its own, by the
         *     disclosure the suggestion carried — when the switch is on (ticket
         *     #121, ADR 0031) — and `edited` is about the text alone.
         */
        Approval: {
            /**
             * Format: date-time
             * @description When the Gateway recorded the approval.
             */
            approved_at: string;
            /**
             * @description The Matrix ID of the human who approved it: this deployment's
             *     owner. The audit trail of who sent what goes through this field.
             */
            approved_by: string;
            /**
             * @description The contact the reply goes to — and whose consent was checked, at
             *     the moment the approval was given.
             */
            contact: string;
            /**
             * @description Whether the text sent differed from the persona's suggestion. A
             *     boolean, not the text: "how often do I correct my assistant?" is
             *     answerable without keeping a word of what was said.
             */
            edited: boolean;
            /**
             * @description The CloudEvents id of the `persona.reply.approved.v1` that was
             *     published: `sha256(suggestion_event_id:approved_by)`. No clock
             *     in it, so approving the same suggestion again lands on the
             *     message the bus already holds instead of sending a second reply.
             */
            event_id: string;
            /**
             * @description The sender gave up on this reply (#311), or `null` while it has
             *     not. Present on `GET /api/approvals/{id}` for the reason
             *     `posted` is, and absent from the `POST` answer for the same
             *     reason: at that moment nothing has been attempted.
             *
             *     The third of three — recorded (`publication`), delivered
             *     (`posted`), given up on (this). A record whose `publication` is
             *     `published` and whose `given_up` is set is a reply that
             *     reached the bus and never reached the contact; rendering the
             *     first without reading the third is what told an owner "sent"
             *     for a message that never left.
             */
            given_up?: null | components["schemas"]["GivenUpReport"];
            /** @description The network the reply goes out on. */
            network: components["schemas"]["Network"];
            /** @description The persona that proposed the reply. */
            persona_id: string;
            /**
             * @description The Sensor's own report of what the reply reached once it posted
             *     it, or `null` while there is none (issue #216). Present on
             *     `GET /api/approvals/{id}` and on a suggestion's `approval`; never
             *     on the `POST` answer, because at that moment the reply has been
             *     published and not yet posted — which is the whole distinction:
             *     `publication` says the reply reached the bus, `posted` says what
             *     it reached from there. A client must never render one as the
             *     other.
             */
            posted?: null | components["schemas"]["PostedReport"];
            /**
             * @description Whether the reply reached the bus. Both values are terminal and
             *     neither is "in flight": an approval is published inside its own
             *     request or it is refused, so `unpublished` means a crash between
             *     the Gateway's write and the bus's acknowledgement, repaired by
             *     approving the same suggestion again.
             * @enum {string}
             */
            publication: "published" | "unpublished";
            /** @description When the bus acknowledged it. `null` on the same terms. */
            published_at?: string | null;
            /**
             * @description Where on the bus the reply landed. `null` while `publication` is
             *     `unpublished`.
             */
            stream_sequence?: number | null;
            /** @description The suggestion that was approved. */
            suggestion_event_id: string;
            /**
             * @description Whose words went out (#327): the persona's draft, corrections
             *     included, or the reply the owner wrote in its place. What the
             *     disclosure followed. Rows recorded before this member existed
             *     read `persona`, which is what they were.
             * @enum {string}
             */
            written_by: "persona" | "owner";
        };
        /**
         * @description One approval. A closed object with one required member, and that
         *     shape is the rule rather than a convenience: an approval names
         *     exactly one suggestion, so there is no `suggestions` array here and
         *     there is not going to be one (`CONTEXT.md`: "never a batch").
         */
        ApprovalRequest: {
            /**
             * @description Optional, and checked rather than trusted: it must be this
             *     deployment's owner, and a request naming anybody else is `403`.
             *     Omit it and the Gateway stamps the owner, which is what an
             *     audit trail of "who sent what" needs.
             */
            approved_by?: string;
            /**
             * @description The edited content, when the user changed the suggestion before
             *     approving. Absent means "send what the persona wrote"; the
             *     published event's `edited` flag says which happened, and its
             *     `written_by` says whose words they were (#327).
             */
            final?: {
                /**
                 * @description The exact text to send, **without** the disclosure: the
                 *     Gateway appends that after the body at publication (ticket
                 *     #121). Empty is refused: approving an empty reply sends an
                 *     empty message, which is never what was meant. The limit is
                 *     the contract's 65 536 less a newline and the 200 characters a
                 *     disclosure may run to, so the sum never exceeds the schema —
                 *     reserved whether the switch is on or off.
                 */
                body: string;
                /**
                 * @description The contract's three content types.
                 * @default text/plain
                 * @enum {string}
                 */
                format: "text/plain" | "text/markdown" | "text/html";
                /**
                 * @description Who wrote this text (#327). `persona` — the default, and
                 *     what a client that says nothing means — is the draft, the
                 *     user's corrections to it included. `owner` is the reply the
                 *     user wrote in its place, having thrown the draft away.
                 *
                 *     It is **what the disclosure follows**. ADR 0019 says that a
                 *     message the user wrote themselves carries nothing, so an
                 *     `owner` reply goes out with no sentence appended, whatever
                 *     the switch says; a `persona` one carries it while the switch
                 *     is on. The distinction is declared by the gesture and never
                 *     measured on the text: a body that differs from the draft
                 *     says that something changed, not whether a word was
                 *     corrected or the draft was replaced.
                 *
                 *     A client that claims `owner` for a body identical to the
                 *     draft is answered as `persona`: a claim of authorship is not
                 *     a way to send the persona's own words undisclosed.
                 * @default persona
                 * @enum {string}
                 */
                written_by: "persona" | "owner";
            };
            /**
             * @description The CloudEvents id of the `persona.suggest.produced` event being
             *     approved.
             */
            suggestion_event_id: string;
        };
        /**
         * @description What one bridge says about the logins it holds — the persistent
         *     links, with the network's credentials behind them. Read from the
         *     bridge's `whoami`, which is the only provisioning call that describes
         *     a login: `GET /v3/logins` answers bare id strings and nothing else.
         */
        BridgeConnection: {
            /**
             * @description One entry per login the bridge holds. v0.1 is one login per
             *     bridge instance, so this is empty or has one member; it is a list
             *     because the bridge's answer is one, and truncating it here would
             *     hide a deployment that has somehow grown a second.
             */
            logins: components["schemas"]["BridgeLinkedLogin"][];
            /**
             * @description Whether the Gateway got an answer it could read. `false` means it
             *     does not know the state of this link — **not** that the network
             *     is disconnected, and not necessarily that nothing answered:
             *     `unreachable_because` says which of the three it was, and only
             *     `bridge_unreachable` means the bridge itself did not reply.
             */
            reachable: boolean;
            /** @description The bridge's own human-readable message for that state. */
            reason: string | null;
            /**
             * @description Mautrix's own `state_event`, verbatim, when the bridge reported
             *     one and this build recognises it. For an operator's eyes: branch
             *     on `state`.
             */
            reported: string | null;
            /**
             * @description The bridge's overall state, in the contract's vocabulary — the
             *     same five values `bridge.status.changed.v1` carries, translated
             *     from mautrix's by the one mapping table the Gateway owns. Notably
             *     `BAD_CREDENTIALS` is `session_expired`: that, and not
             *     `LOGGED_OUT`, is what a session revoked from the user's own phone
             *     reports, and no mautrix bridge emits `LOGGED_OUT` at all.
             *
             *     A bridge holding no login is `disconnected`. A bridge holding one
             *     it has not reported on yet is `starting`, not `disconnected`: its
             *     state lives in its memory and it has just come back up.
             *
             *     `null` exactly when `reachable` is `false`.
             */
            state: ("starting" | "connected" | "degraded" | "disconnected" | "session_expired") | null;
            /**
             * @description Why this bridge's logins could not be read, as one of the same
             *     stable codes the error answers use. `null` when they were.
             *
             *     `bridge_unreachable` is the only one that means nothing
             *     answered. `bridge_refused` is the bridge's own error, and
             *     `bridge_answer_unusable` is a bridge that answered a `whoami`
             *     this build could not read (#116) — the bridge is running, and a
             *     screen that says otherwise is repeating the defect this member's
             *     name predates.
             */
            unreachable_because: string | null;
        };
        /**
         * @description One link the bridge holds: which account, since when, and how it is
         *     doing. This is what the management screen shows and what
         *     `DELETE .../logins/{login_id}` drops.
         */
        BridgeLinkedLogin: {
            /**
             * @description What `POST .../login` takes as `login_id` to re-link, and what
             *     `DELETE .../logins/{login_id}` drops.
             */
            login_id: string;
            /**
             * @description The name the network gives the account — a phone number on both
             *     reference bridges. `null` when the bridge names none.
             */
            name: string | null;
            /**
             * @description The bridge's own profile object for the login, passed through
             *     unread: its shape is the connector's and the network's.
             */
            profile: unknown;
            /** @description The bridge's human-readable message for this login. */
            reason: string | null;
            /** @description Mautrix's own `state_event` for this login, verbatim. */
            reported: string | null;
            /**
             * Format: date-time
             * @description When this login's state last changed, from the bridge's own
             *     `state.timestamp`. For a `connected` login that is when it
             *     connected — the honest answer to "linked since when?", and the
             *     only one a bridge offers. `null` when the bridge has reported no
             *     timestamp, which is what a bridge that has just restarted looks
             *     like: its state is in memory.
             */
            since: string | null;
            /**
             * @description This login's state, in the same vocabulary as above.
             * @enum {string}
             */
            state: "starting" | "connected" | "degraded" | "disconnected" | "session_expired";
        };
        BridgeList: {
            bridges: components["schemas"]["ConfiguredBridge"][];
        };
        /**
         * @description The whole state of one login, and the document the Companion polls.
         *     Watch `state` and `generation`; everything else is what to draw.
         */
        BridgeLogin: {
            bridge_id: string;
            /** @description Why the login failed; `null` unless `state` is `failed`. */
            error: {
                /**
                 * @description - `login_lost` — the bridge stopped answering, or no
                 *       longer knows the process. **A login in flight does not
                 *       survive a restart of the bridge or of the Gateway**:
                 *       nothing about it is persisted on either side. Start
                 *       again.
                 *     - `login_expired` — the bridge ended it: cancelled, timed
                 *       out, or already finished.
                 *     - `webauthn_required` — the flow asked for a passkey,
                 *       which the Companion cannot drive. The login fails here
                 *       rather than hanging on a step nobody will answer; a
                 *       bridge with `provisioning.fail_on_webauthn` refuses it
                 *       one step earlier.
                 *     - `unsupported_step` — a step type this facade does not
                 *       drive (`client_http`).
                 *     - `bridge_refused` — the bridge answered an error of its
                 *       own, with its own errcode.
                 *     - `bridge_unreachable` — nothing answered at all.
                 *     - `bridge_answer_unusable` — the bridge answered
                 *       successfully and this build could not use the answer.
                 *       **The bridge is running**: this is a defect in Twalk,
                 *       not a broken deployment, and the screen must not say
                 *       the bridge could not be reached (#116). `detail` names
                 *       the provisioning call and the field that was looked
                 *       for, and never repeats the answer itself.
                 * @enum {string}
                 */
                code: "login_lost" | "login_expired" | "webauthn_required" | "unsupported_step" | "bridge_refused" | "bridge_unreachable" | "bridge_answer_unusable";
                /**
                 * @description For an operator's logs. Not for display, and never
                 *     matched on.
                 */
                detail: string | null;
            } | null;
            /**
             * Format: date-time
             * @description The bridge's own deadline for the process: bridgev2 caps a login
             *     at 30 minutes. Past it the login is gone whatever this document
             *     last said.
             */
            expires_at: string;
            flow_id: string;
            /**
             * @description Increments on every step the bridge hands back, **refreshes
             *     included**. A changed generation with the same `step.step_id` is
             *     a refreshed QR code: redraw it. This, not a timer, is the signal
             *     that a fresh code has arrived.
             */
            generation: number;
            /** @description The login the network accepted; `null` until `complete`. */
            login: {
                /**
                 * @description What `DELETE .../logins/{login_id}` drops, and what a
                 *     later reconnect names.
                 */
                login_id: string;
                /** @description The bridge's user-login id, when it names one. */
                user_id: string | null;
            } | null;
            /**
             * @description The existing login this flow is repairing, when it is a
             *     reconnect; `null` for a first login.
             */
            login_id: string | null;
            network: string;
            /**
             * @description The bridge's own id for this login process. It lives in the
             *     bridge's memory and does not survive its restart.
             */
            process_id: string;
            /**
             * Format: date-time
             * @description When this login was started, to the millisecond.
             */
            started_at: string;
            /**
             * @description The device that started it. One owner per deployment, so this
             *     says *which of my devices*, never *who*.
             */
            started_by: {
                device_id: string;
                device_name: string;
            };
            /**
             * @description - `awaiting_input` — the step needs something from the user;
             *       submit it.
             *     - `awaiting_remote` — the Gateway is holding the blocking step
             *       and the user is scanning. Keep polling; there is nothing to
             *       send.
             *     - `complete` — the network accepted the login, and `login` names
             *       it.
             *     - `failed` — `error` says why.
             *     - `cancelled` — this device, or the remote side, stopped it.
             * @enum {string}
             */
            state: "awaiting_input" | "awaiting_remote" | "complete" | "failed" | "cancelled";
            /** @description The step to render, or `null` once the login is over. */
            step: components["schemas"]["BridgeLoginStep"] | null;
        };
        BridgeLoginFlows: {
            flows: {
                /** @description The bridge's longer explanation, when it gives one. */
                description: string | null;
                /** @description What `POST .../login` takes as `flow_id`. */
                id: string;
                /** @description The bridge's own label for the flow. */
                name: string;
            }[];
        };
        BridgeLogins: {
            logins: {
                /**
                 * @description What `POST .../login` takes as `login_id` to reconnect, and
                 *     what `DELETE .../logins/{login_id}` drops.
                 */
                login_id: string;
                /** @description The name the network gives the account. */
                name: string | null;
                /**
                 * @description The bridge's own profile object for the login, passed
                 *     through unread — its shape is the bridge's and the network's.
                 */
                profile: unknown;
            }[];
        };
        BridgeLoginStep: {
            /**
             * Format: date-time
             * @description `received_at` plus `valid_for_seconds`.
             */
            expires_at: string;
            /**
             * @description The bridge's own words for the user, when it gives any. Show them
             *     rather than inventing copy: they are the network's wording for
             *     where to find the "link a device" screen.
             */
            instructions: string | null;
            /**
             * @description The step's payload, **passed through from the bridge**. For a QR
             *     step it is `{"type": "qr", "data": "…"}`, and `data` is the raw
             *     payload the browser draws: the bridge renders no image. For
             *     `user_input` it is the field list to render; for `cookies`, the
             *     URL and the cookies to collect.
             *
             *     Its shape is the bridge's, not the Gateway's, which is why it is
             *     not constrained here. It is also a network credential in flight:
             *     it is held nowhere else and is gone as soon as the next step
             *     replaces it.
             */
            payload: {
                [key: string]: unknown;
            } | null;
            /**
             * Format: date-time
             * @description When the bridge handed this step over.
             */
            received_at: string;
            /**
             * @description The bridge's id for this step. `POST .../login/submit` is
             *     answered against exactly this value.
             */
            step_id: string;
            /**
             * @description bridgev2's own step types. `display_and_wait` is the blocking one
             *     — the Gateway is holding it, and the browser only polls;
             *     `user_input` and `cookies` are the ones to submit. `client_http`
             *     and `webauthn` never appear here: they fail the login instead
             *     (see `error.code`).
             * @enum {string}
             */
            type: "user_input" | "cookies" | "display_and_wait" | "client_http" | "webauthn" | "complete";
            /**
             * @description How long this step is worth acting on, from `received_at`. For a
             *     QR code it is the Gateway's own estimate of the network's refresh
             *     interval — the bridge states no expiry, and WhatsApp's whole QR
             *     budget is about 2m40 across refreshes — so treat it as "redraw
             *     soon" and `generation` as the fact. Never past `expires_at` of
             *     the login itself.
             */
            valid_for_seconds: number;
        };
        BridgeStatePush: {
            /**
             * @description The bridge's error code. Used as `reason` when there is no
             *     `message`: a code is a worse sentence than a message and a much
             *     better one than nothing.
             */
            error?: string;
            /**
             * @description Mautrix's free-form map. `last_message_at` (or
             *     `last_message_ts`) is read out of it into the event's
             *     `last_message_at` when a bridge puts one there; none does today.
             */
            info?: {
                [key: string]: unknown;
            } | null;
            /**
             * @description The bridge's own human-readable cause, which becomes the event's
             *     `reason` (truncated to the contract's 1024 characters).
             */
            message?: string;
            /**
             * @description The state the bridge is reporting. Mautrix declares seven;
             *     anything else is read as `disconnected`, because telling a
             *     dashboard that an unrecognised state is fine is the one wrong
             *     direction.
             * @enum {string}
             */
            state_event: "CONNECTING" | "BACKFILLING" | "CONNECTED" | "TRANSIENT_DISCONNECT" | "BAD_CREDENTIALS" | "UNKNOWN_ERROR" | "LOGGED_OUT";
            /**
             * @description When the state changed, in whole seconds since the epoch — the
             *     instant of the last *change*, not of this push. It becomes the
             *     event's `occurred_at`, and therefore part of its deterministic
             *     id. Absent: the Gateway's own clock is used.
             */
            timestamp?: number;
            /**
             * @description What the bridge thinks the user has to do. Logged by the Gateway
             *     and not published: `bridge.status.changed.v1` has no field for
             *     it, and a producer inventing one would be a contract change.
             * @enum {string}
             */
            user_action?: "OPEN_NATIVE" | "RELOGIN" | "RESTART";
        } & {
            [key: string]: unknown;
        };
        ConfiguredBridge: {
            /**
             * @description The bridge instance's id, as this deployment's configuration
             *     declares it (`mautrix-whatsapp`). It identifies the
             *     implementation, never the network (CONTEXT.md), and it is what
             *     every other path below takes.
             */
            bridge_id: string;
            connection: components["schemas"]["BridgeConnection"];
            /**
             * @description The login **process** this Gateway has in flight, or `null` when
             *     nothing has been started on it. A login that completed, failed or
             *     was cancelled is still reported here until the next one replaces
             *     it.
             *
             *     This is a QR scan in somebody's browser, not a connection. It is
             *     never what a "connected" badge is read from: use `connection`.
             */
            login: components["schemas"]["BridgeLogin"] | null;
            /**
             * @description The network the user experiences — `whatsapp`, `signal`, `sms` —
             *     which is what the Companion labels the screen with. Configured
             *     per instance, because a bridge's name and its network are not the
             *     same thing: `mautrix-gmessages` is the `sms` network.
             */
            network: string;
        };
        /** @description One connection of the registry (ADR 0033, */
        Connection: {
            /**
             * @description The bridge's bot (`GATEWAY_BRIDGE_<ID>_BOT_USER_ID`), the account
             *     the Sensor recognises a portal of this connection by. Absent when
             *     the operator named none; the Sensor then resolves the connection
             *     by kind when the kind has exactly one.
             */
            bridge_bot?: string;
            /** @description The bridge instance carrying it (`GATEWAY_BRIDGES`), for a connection a bridge carries. */
            bridge_id?: string;
            /**
             * @description Opaque and stable. On a deployment with one connection per
             *     network it is the network's name; nothing parses an id.
             */
            id: components["schemas"]["ConnectionId"];
            kind: components["schemas"]["Kind"];
            /** @description For a person to read; a bridge's instance id by default. */
            label: string;
            status?: components["schemas"]["ConnectionStatus"];
        };
        /**
         * @description The id of a connection (ADR 0033): the one shape, copied from the
         *     contract's `definitions/connection.schema.json` and tested against
         *     it. Every member that names a connection references this schema.
         */
        ConnectionId: string;
        /**
         * @description The contract's four states of a connection
         *     (`connection.status.changed.schema.json`, `data.to_state`): `connected`;
         *     `unreachable`, a service did not answer and the collector retries;
         *     `reconnect_required`, the SSO refused the grant and the operator has
         *     to authorize again; `pending_operator`, a service refused a fresh
         *     token and the operator has to change the client. Copied from the
         *     contract and tested against it; four sentences in the Companion, never one.
         * @enum {string}
         */
        ConnectionState: "connected" | "unreachable" | "reconnect_required" | "pending_operator";
        /**
         * @description What the connection's collector last said about it (#275,
         *     `connection.status.changed.v1`). Present only on a connection that
         *     spoke for itself.
         */
        ConnectionStatus: {
            /** @description The operator's next step, in one sentence, as the collector said it. The Companion shows it beside the state; it never names a token. */
            hint?: string | null;
            /**
             * Format: date-time
             * @description When the collector observed the transition into this state.
             */
            occurred_at: string;
            /**
             * @description Which service the state is about, when one is; absent or null on `connected`.
             * @enum {string|null}
             */
            service?: "sso" | "jmap" | "caldav" | null;
            state: components["schemas"]["ConnectionState"];
        };
        /** @description One recorded state change of a connection, for the dashboard's feed. */
        ConnectionTransition: {
            connection: components["schemas"]["ConnectionId"];
            /**
             * @description The state before; `unknown` on the first publication of a collector's run.
             * @enum {string}
             */
            from_state: "unknown" | "connected" | "unreachable" | "reconnect_required" | "pending_operator";
            hint?: string | null;
            kind: components["schemas"]["Kind"];
            /** Format: date-time */
            occurred_at: string;
            /** @enum {string|null} */
            service?: "sso" | "jmap" | "caldav" | null;
            to_state: components["schemas"]["ConnectionState"];
        };
        /** @description One decision the owner asks the Gateway to record. */
        ConsentDecisionRequest: {
            new_state: components["schemas"]["ConsentState_State"];
            /**
             * @description Optional free text kept in the journal and published with the
             *     event, for the audit trail. The user's own words about their own
             *     decision — never message content.
             */
            reason?: string;
            scope: components["schemas"]["ConsentScopeRequest"];
            subject: components["schemas"]["ConsentSubject"];
        };
        /**
         * @description The perimeter of a decision as recorded: the connections it covers —
         *     the key the state is held under — and their kinds, derived and kept
         *     for a client not yet migrated (#270).
         */
        ConsentScope: {
            /** @description The connections, sorted ascending. */
            connections: components["schemas"]["ConnectionId"][];
            /** @description The kinds of `connections`, deduplicated, sorted. */
            networks: components["schemas"]["Network"][];
        };
        /**
         * @description The perimeter of a decision on the way in (ADR 0033, #270). Either
         *     member is enough: `connections` names the perimeter; `networks`
         *     alone is read as each network's single connection on this
         *     deployment, and refused (`malformed_request`, naming the candidates)
         *     when a network has several. Both together must agree.
         */
        ConsentScopeRequest: {
            /**
             * @description The connections the decision applies to, by the ids of
             *     `GET /api/connections`. Order does not matter: the Gateway sorts
             *     it, because the sorted scope is part of the event's deterministic
             *     id.
             */
            connections?: components["schemas"]["ConnectionId"][];
            /**
             * @description Deprecated as an input since #270 — send `connections`. Read as
             *     each network's single connection.
             */
            networks?: components["schemas"]["Network"][];
        };
        /**
         * @description The whole consent state, and the bus position it reflects. The two
         *     are read together, so no decision can be committed and published
         *     between them without appearing in one of the two.
         */
        ConsentSnapshot: {
            /**
             * @description The registry of connections (ADR 0033, #269): every configured
             *     account this deployment observes or acts through, with the bridge
             *     and the bot for the ones a bridge carries. Here on the same
             *     argument as `owner_identities`: the registry has one owner and
             *     the Sensor reads it rather than deriving it, stamping every event
             *     with the connection of the bridge that built the room. A bot no
             *     connection names is a bridge no connection covers, and the Sensor
             *     publishes nothing from its portals rather than guessing. Empty
             *     on a deployment with no bridge and no declaration; never absent.
             */
            connections: components["schemas"]["Connection"][];
            /**
             * @description The position in the Gateway's own decision journal this snapshot
             *     reflects — the same counter as a recorded decision's `sequence`.
             *     Not what a consumer starts from (that is the stream sequence
             *     above): this is for an operator comparing the two.
             */
            decision_sequence: number;
            /**
             * @description One entry per (subject, network), ordered by subject type,
             *     subject and network. Revocations are explicit; `persona`
             *     subjects are excluded, and so is every subject named in
             *     `owner_identities` — including one a decision was recorded about
             *     before this Gateway knew whose identity it was.
             */
            entries: components["schemas"]["ConsentStateEntry"][];
            /**
             * @description `stream_sequence + 1`: where a consumer starts its stream
             *     consumer after applying `entries`. Spelled out rather than left
             *     to the client to compute, because that off-by-one would silently
             *     skip or re-apply one decision.
             */
            next_stream_sequence: number;
            /**
             * @description Every Matrix ID this deployment has confirmed as the owner's own
             *     — their account and their network ghosts — and therefore the
             *     subjects `entries` will never contain (ticket #149, ADR 0018,
             *     ADR 0021). Sorted, and never empty: `GATEWAY_OWNER` is itself one
             *     of them.
             *
             *     It is here because the set **cannot be derived**. A bridge
             *     materialises a ghost of the user's own account that is
             *     indistinguishable in shape from a contact's, there is more than
             *     one per network (`@whatsapp_<phone>` *and*
             *     `@whatsapp_lid-<lid>`, and the messages arrived under the LID
             *     one), a bridge's `whoami` carries a login id and no ghost Matrix
             *     ID at all, and the set grows when a network starts using a new
             *     addressing scheme. So the deployment confirms it
             *     (`GATEWAY_OWNER_IDENTITIES`) and the Gateway — the single writer
             *     of consent state — serves it, so that a consumer which must apply
             *     the same rule reads this list instead of maintaining a second
             *     copy of it. An identity that is not in it is a contact, because
             *     unknown is not the owner.
             * @example [
             *       "@you:matrix.example.com",
             *       "@whatsapp_lid-115332874281144:matrix.example.com"
             *     ]
             */
            owner_identities: string[];
            /**
             * @description The JetStream stream `stream_sequence` is a sequence of — the
             *     deployment's one stream, `twalk`. Named here so a consumer does
             *     not have to agree with the Gateway about it out of band.
             * @example twalk
             */
            stream: string;
            /**
             * @description The sequence of the last decision this snapshot reflects, as the
             *     bus stored it. `0` when no decision has reached the bus yet.
             */
            stream_sequence: number;
            /**
             * @description The subject the decisions in this state were published on, which
             *     is what a consumer filters its stream consumer by.
             * @example twalk.consent.state.changed.v1
             */
            subject: string;
        };
        ConsentState: {
            /**
             * @description Every recorded entry, ordered by subject type, subject and
             *     network. Not paginated, and uncapped: the cap belongs to the
             *     snapshot, where the entries are a consumer's whole starting
             *     state.
             */
            entries: components["schemas"]["ConsentStateEntry"][];
        };
        /**
         * @description The data-processing agreement state of a subject. `unset` is not one
         *     of them — it is the absence of a decision, and only `old_state`
         *     carries it.
         * @enum {string}
         */
        ConsentState_State: "granted" | "pending" | "revoked";
        /**
         * @description One (subject, connection) of the current state (#270). `network` is
         *     the connection's kind, kept beside it for a client not yet migrated.
         */
        ConsentStateEntry: {
            /** @description The perimeter, by its id in `GET /api/connections`. */
            connection: components["schemas"]["ConnectionId"];
            /**
             * Format: date-time
             * @description When the decision this entry comes from was taken.
             */
            decided_at: string;
            /** @description That decision's position in the journal. */
            decision_sequence: number;
            network: components["schemas"]["Network"];
            state: components["schemas"]["ConsentState_State"];
            subject: components["schemas"]["ConsentSubject"];
        };
        /** @description Who or what a consent decision applies to. */
        ConsentSubject: {
            /**
             * @description A Matrix user ID for a contact; for a network subject, that
             *     network's own value — in which case the scope holds exactly that
             *     one network; for a persona, the persona's name (`assistant`).
             */
            id: string;
            /**
             * @description `contact` for one contact, `network` for a whole network's
             *     default, `persona` for a persona's own activation (ADR 0013).
             * @enum {string}
             */
            type: "contact" | "network" | "persona";
        };
        ContactDisplayName: {
            /** @description The contact asked about. */
            contact: string;
            /**
             * @description What the bus's most recent event says this contact is called, or
             *     `null` when the read found none. Never stored by the Gateway.
             */
            display_name: string | null;
        };
        ContactDisplayNames: {
            /** @description One entry per distinct contact asked about, in order. */
            contacts: components["schemas"]["ContactDisplayName"][];
        };
        /**
         * @description A username and a password, and deliberately nothing else: a closed
         *     object is what makes "no recovery key can arrive here" a property of
         *     the API rather than a convention (ADR 0014).
         */
        CreateAccountRequest: {
            /**
             * @description The password the user chose. Passed to the homeserver, which
             *     applies its own policy, and never stored or logged by the
             *     Gateway.
             */
            password: string;
            /**
             * @description The localpart of the account to create. Must be the localpart of
             *     this deployment's `GATEWAY_OWNER`; any other value is `403`.
             */
            username: string;
        };
        /**
         * @description The Matrix session the homeserver answered with, forwarded whole:
         *     what the Companion continues in the browser to generate the
         *     cross-signing keys and the recovery key itself (ADR 0014). The
         *     Gateway keeps none of it.
         */
        CreatedAccount: {
            /**
             * @description The account's Matrix access token. Returned because the browser
             *     needs a session, and held nowhere: not in the Gateway's store,
             *     not in a log line (ADR 0011).
             */
            access_token: string;
            /**
             * @description The device the session belongs to. The browser's crypto store is
             *     bound to it.
             */
            device_id: string;
            /** @description The homeserver name, as the homeserver reports it. */
            home_server: string;
            /** @description The Matrix ID of the account that was created. */
            user_id: string;
        };
        /**
         * @description Whether an approved reply could reach the contact, as far as this
         *     Gateway can tell **before** it is sent (issue #216). Read from the
         *     room the trigger arrived in — its *envelope*, never its body — and
         *     from where the owner's own account stands in that room, asked of the
         *     homeserver as the bridge's bot.
         *
         *     Three answers, and the third is honest rather than optimistic.
         *     `cannot_reach` is a certainty: the room is a portal of a configured
         *     bridge and the owner's account is not joined to it, so nothing posted
         *     there is relayed, whatever else is true — the mechanism that changes
         *     it is #123 (a device of the owner's account, joined on the bridge
         *     bot's invitation). `can_reach` is the register's best reading: the
         *     owner is joined, which is what a bridge relays from. `unknown` is a
         *     room no bridge bot of this deployment can read — native Matrix
         *     (ADR 0009), which reaches its reader with no bridge in the way, or a
         *     portal of a bridge with no token — and the Sensor's own report
         *     (`posted`) is the answer there, after the fact.
         */
        Delivery: {
            /**
             * @description The one word behind the answer. `owner_invited` is the common
             *     `cannot_reach`: the bridge invited the owner into the
             *     conversation and no device of theirs has accepted — every portal,
             *     on a deployment with no owner device configured; that room, on one
             *     whose device could not join it (#237). `trigger_out_of_reach` is
             *     the message this suggestion answers lying beyond the read window,
             *     so its room could not be read; `lookup_failed` is the bus not
             *     answering that second read.
             * @enum {string}
             */
            detail: "owner_joined" | "owner_invited" | "owner_absent" | "not_a_known_portal" | "portal_unreadable" | "no_portal_register" | "no_owner_configured" | "trigger_out_of_reach" | "lookup_failed";
            /** @enum {string} */
            reach: "can_reach" | "cannot_reach" | "unknown";
        };
        /**
         * @description One device in the list. Dates are seconds since the epoch: the
         *     Gateway carries no date library, and a client renders local time
         *     from a number as readily as from a string.
         */
        Device: {
            /** @description When this device first signed in. */
            created_unix_seconds: number;
            /** @description Whether this is the device the request came from. */
            current: boolean;
            /** @description The device's id — what `DELETE /api/devices/{id}` takes. */
            id: string;
            /** @description When a request last authenticated with this device's token. */
            last_seen_unix_seconds: number;
            /**
             * @description The name the device was signed in under, or the one the Gateway
             *     gave it.
             */
            name: string;
            /**
             * @description When the device was revoked, or `null` while it is live. Present
             *     either way: an absent member would be indistinguishable from a
             *     client that failed to read it.
             */
            revoked_unix_seconds: number | null;
        };
        DeviceList: {
            /**
             * @description Every device, revoked ones included, so that a revocation stays
             *     visible to the user who made it.
             */
            devices: components["schemas"]["Device"][];
        };
        /**
         * @description The disclosure switch as the journal answers it (ticket #121): on or
         *     off, and — when somebody decided — since when, by whom and why. All
         *     three are `null` in the default state, which is on: "nobody
         *     decided" is the record, not a gap in it.
         */
        DisclosureState: {
            /**
             * @description The Matrix ID of who decided: this deployment's owner, stamped
             *     by the Gateway. `null` when nobody has.
             */
            actor: string | null;
            /**
             * @description `true`: every approved reply goes out with the disclosure after
             *     it, in the language it was written in. `false`: none does, until
             *     it is turned on again.
             */
            enabled: boolean;
            /** @description The note given with the decision, if one was. */
            reason: string | null;
            /**
             * Format: date-time
             * @description When the current state was decided; `null` when never.
             */
            since: string | null;
        };
        /** @description One decision about the disclosure. A closed object. */
        DisclosureUpdate: {
            /**
             * @description `false` turns the disclosure off for every reply approved from
             *     now on; `true` turns it back on. Never per message (ADR 0019).
             */
            enabled: boolean;
            /**
             * @description Why, in the user's own words. Optional, kept with the decision,
             *     and shown back on the settings screen.
             */
            reason?: string | null;
        };
        /** @description The state that applies to one contact on one connection. */
        EffectiveConsent: {
            /** @description The connection the answer is about (#270). */
            connection: components["schemas"]["ConnectionId"];
            /** @description The contact the question was about. */
            contact: string;
            /**
             * @description The subject whose decision answered — the contact itself, or the
             *     network whose default applied — and `null` when no decision
             *     exists, in which case `state` is `pending`.
             */
            decided_by: components["schemas"]["ConsentSubject"] | null;
            network: components["schemas"]["Network"];
            state: components["schemas"]["ConsentState_State"];
        };
        /**
         * @description What a browser is told about the endpoint's credential. Never the
         *     credential.
         */
        EndpointCredential: {
            /**
             * @description Whether a credential was set from the Companion — `true` even
             *     when the file is the one in force, which is exactly the
             *     combination a settings screen has to explain.
             */
            companion_credential_stored: boolean;
            /** @description Whether any credential is in force. Some endpoints need none. */
            configured: boolean;
            /**
             * @description The path `GATEWAY_LLM_API_KEY_FILE` names, when it names one.
             *     Shown because "the file wins" is useless to a human who cannot
             *     see which file won.
             */
            file: string | null;
            /**
             * @description The last four characters of the credential in force: enough for
             *     a human to recognise the key they pasted, not enough for anyone
             *     to use it. `null` for a credential under eight characters, where
             *     four would be most of it.
             */
            hint: string | null;
            /**
             * @description Which of the two won. `file` is a credential the operator
             *     supplied through `GATEWAY_LLM_API_KEY_FILE`, and it **wins** —
             *     so a production stack can lock the credential down while the
             *     model name still comes from the browser (ADR 0015).
             * @enum {string|null}
             */
            source: "file" | "companion" | null;
        };
        /**
         * @description Every refusal the Gateway produces. `error` is the stable code a
         *     client branches on; the operations above enumerate which codes each
         *     status carries.
         */
        Error: {
            approval?: components["schemas"]["Approval"];
            /**
             * @description A human-readable explanation, for an operator reading logs. Not
             *     for display to the user, and never matched on.
             */
            detail?: string;
            /**
             * @description The HTTP status the configured LLM endpoint answered the probe
             *     with. Set on `POST /api/settings/model/probe`'s `502` alone, and
             *     `null` there when nothing answered at all — which is the whole
             *     distinction between `endpoint_refused` and
             *     `endpoint_unreachable`.
             */
            endpoint_status?: number | null;
            /** @description The machine-readable code. Stable; never a sentence. */
            error: string;
            /**
             * @description The path that matched no endpoint. Set on the `/api` catch-all's
             *     `404` alone.
             */
            path?: string;
            /**
             * @description The connection's state, on `GET /_twalk/hermes/freebusy`'s
             *     `409 connection_not_connected` alone (ticket #281): what a
             *     reader outside the deployment is told instead of a sentence to
             *     parse, so it can say "the calendar is not reachable right now"
             *     and not guess.
             * @enum {string}
             */
            state?: "unknown" | "unreachable" | "reconnect_required" | "pending_operator";
        };
        /**
         * @description What the component that had to send an approved reply said when it
         *     gave up on it (#311): the approval republished unchanged on
         *     `twalk.persona.reply.approved.v1.dead` by the Sensor or the
         *     collector, the reason in a header, and this is what the Gateway
         *     reads off it.
         *
         *     Why a subject of the senders' own rather than a field: the same
         *     reason `.posted` is one (see `PostedReport`) — the answer is a
         *     property of the *send*, not of the approval, and every v1 schema is
         *     `additionalProperties: false`.
         */
        GivenUpReport: {
            /**
             * @description Why the sender gave up, in its own words — the `reason` header of
             *     the dead letter, capped by the sender (512 characters). Neither
             *     the Sensor nor the collector quotes the reply's body in it.
             *
             *     `null` when the sender set none: a Sensor older than #311
             *     published dead letters with no such header. The Gateway answers
             *     the absence rather than a sentence of its own — a client renders
             *     it in the user's language, and English prose from the Gateway
             *     would end up inside a French one. The reply still did not leave,
             *     which is what the member's presence says.
             */
            reason: null | string;
            /**
             * @description Where the dead letter landed on the bus. Always after the
             *     publication's own position: the sender reads the reply before it
             *     gives up on it.
             */
            stream_sequence: number;
        };
        Health: {
            /**
             * @description The build id of the Companion this origin serves, read from the
             *     export's own `_app/version.json` — the value the running app
             *     carries as its own build. The Companion compares the two (#222):
             *     a browser running a build the Gateway no longer ships is holding
             *     a stale shell, and reloads once, then says so — the version-skew
             *     that used to be invisible while the server reported itself
             *     current. `null` when the export carries no id.
             */
            companion_build: string | null;
            /**
             * @description The revision the binary was built from, or `unknown`.
             *     Provenance, not part of the handshake.
             */
            revision: string;
            /**
             * @description `ok` while the process answers. Liveness, not readiness.
             * @enum {string}
             */
            status: "ok";
            /**
             * @description The Gateway's version — the same value as this description's
             *     `info.version`. The Companion compares it with the version baked
             *     into its own build and reloads on a mismatch.
             */
            version: string;
        };
        /**
         * @description What became of one push: a suggestion, or an ignored run. Two shapes
         *     rather than one with optional members, because "published" and
         *     "ignored" are different facts and a client should not have to
         *     discover which by looking for a null.
         */
        HermesAnswerAccepted: {
            /**
             * @description The primary subtag of the language the answer declared —
             *     `fr` for `fr-CA` — which is the language the suggestion's
             *     `disclosure` was selected by.
             * @enum {string}
             */
            language: "en" | "fr" | "it" | "es" | "de";
            /** @enum {string} */
            status: "published";
            /** @description Where on the bus the suggestion landed. */
            stream_sequence: number;
            /**
             * @description The suggestion's CloudEvents id: the contract's
             *     `sha256(persona_id:trigger_event_id:attempt)`, which is also
             *     the idempotency key the persona gave Hermes for the wake.
             */
            suggestion_event_id: string;
        } | {
            /**
             * @description Why this push was not a Twalk wake. Counted on `/metrics`
             *     under the same word.
             * @enum {string}
             */
            reason: "not_our_hook" | "not_a_webhook_run";
            /** @enum {string} */
            status: "ignored";
        };
        /**
         * @description One of Hermes's outbound-hook deliveries, as much of it as this
         *     Gateway reads. Additional members are allowed and dropped: the hook's
         *     wire format is Hermes's and grows with it, and pinning it would break
         *     on a Tuesday (ADR 0032 pins the route's shape and nothing deeper).
         */
        HermesAnswerPush: {
            /** @description Hermes's own id for this push. Logged, never trusted. */
            delivery_id?: string;
            /** @description The hook's own arguments. */
            extra: {
                /** @description The model Hermes reasoned with. Logged. */
                model?: string;
                /**
                 * @description The Hermes platform the turn ran under. `webhook` is a Twalk
                 *     wake; anything else is a turn somebody had with Hermes
                 *     directly and is ignored.
                 * @example webhook
                 */
                platform?: string;
                /**
                 * @description What the model wrote: a JSON object with `reference`, `reply`
                 *     and `language` — a BCP 47 tag whose primary subtag must be one
                 *     the contract holds a disclosure sentence for. A fenced code
                 *     block around it is unwrapped.
                 */
                response_text: string;
                /** @description Hermes's own session key. Logged, never the correlation. */
                session_id?: string;
            };
            /**
             * @description The Hermes hook that fired. `transform_llm_output` is the one this
             *     endpoint acts on; any other is ignored with a `200`.
             * @example transform_llm_output
             */
            hook_event_name: string;
            /**
             * @description When Hermes sent the push, RFC 3339. Inside the signed body, and
             *     checked against this clock: it is this wire format's only replay
             *     protection.
             */
            timestamp?: string;
        };
        /**
         * @description The owner's busy intervals in the window asked for (ticket #281):
         *     clipped to it, merged where they touch, and nothing else — no
         *     title, no participant, no location, because the report they come
         *     from carries none.
         */
        HermesFreeBusy: {
            busy: {
                /**
                 * Format: date-time
                 * @description RFC 3339, UTC; exclusive.
                 */
                end: string;
                /**
                 * Format: date-time
                 * @description RFC 3339, UTC.
                 */
                start: string;
            }[];
            connection: string;
            /** Format: date-time */
            from: string;
            /** Format: date-time */
            to: string;
        };
        /**
         * @description The homeserver would not do what the Gateway relayed, or could not
         *     be reached. Carries the homeserver's own error code where there was
         *     one, so a client can turn a password policy into a message instead
         *     of a shrug.
         */
        HomeserverRefusal: {
            /**
             * @description A human-readable explanation, for an operator reading logs. Not
             *     for display to the user, and never matched on.
             */
            detail?: string;
            /** @enum {string} */
            error: "homeserver_refused" | "homeserver_unreachable";
            /**
             * @description The homeserver's `errcode` (`M_USER_IN_USE`,
             *     `M_PASSWORD_TOO_SHORT`, …), on `homeserver_refused`. The
             *     homeserver's human-readable message is deliberately not
             *     forwarded: it goes to the Gateway's log, where an operator reads
             *     it.
             */
            matrix_errcode?: string;
        };
        /**
         * @description The user's Matrix access token and the rooms they selected. Closed,
         *     for the same reason as the registration request.
         */
        InviteSensorRequest: {
            /**
             * @description The user's own Matrix access token — the session that is in the
             *     rooms and can therefore invite. Used for these calls and
             *     forgotten: never stored, never logged.
             */
            matrix_access_token: string;
            /**
             * @description The rooms the user selected, as Matrix room ids
             *     (`!opaque:server`). An empty list is allowed and does nothing.
             */
            rooms: string[];
        };
        /**
         * @description A session whose tokens were just issued or rotated: the session
         *     document plus the device token's lifetime, so the client refreshes
         *     on time rather than discovering the expiry as a `401`.
         *
         *     Spelled out rather than an `allOf` over [`Session`]: a closed object
         *     (`additionalProperties: false`) is what makes the conformance test
         *     catch a response member nobody described, and that keyword cannot
         *     see through an `allOf`.
         */
        IssuedSession: {
            device: components["schemas"]["Device"];
            /**
             * @description Seconds the device token stays valid from now. The refresh
             *     token lives longer; its lifetime is not reported, because
             *     the client's only move when it expires is to sign in again.
             */
            expires_in: number;
            /** @description The homeserver name that identity belongs to. */
            homeserver: string;
            /**
             * @description The Matrix ID of the single human this deployment serves
             *     (`GATEWAY_OWNER`).
             */
            owner: string;
            sensor: components["schemas"]["SensorUserId"];
        };
        /**
         * @description The kind of a connection: the contract's own definition
         *     (`contracts/cloudevents/v1/definitions/kind.schema.json`, the one
         *     authority): every network, plus `calendar` (ADR 0033).
         * @enum {string}
         */
        Kind: "whatsapp" | "telegram" | "signal" | "discord" | "sms" | "matrix" | "email" | "calendar";
        /** @description The user's native language, and the choices. */
        LanguagePreference: {
            /** @description The interface languages the Companion ships, in the order it offers them. */
            available: ("en" | "fr" | "it" | "es" | "de")[];
            /**
             * @description `null` is "no preference", which is not English: a persona with
             *     no fallback follows the incoming message and nothing else.
             * @enum {string|null}
             */
            language: "en" | "fr" | "it" | "es" | "de" | null;
        };
        LanguagePreferenceRequest: {
            /** @enum {string|null} */
            language: "en" | "fr" | "it" | "es" | "de" | null;
        };
        /**
         * @description The homeserver's OpenID token document, forwarded unchanged. The
         *     Gateway reads `access_token` and `matrix_server_name`; the rest of
         *     the homeserver's answer (`token_type`, `expires_in`) is accepted and
         *     ignored, so a client can forward the document as it received it.
         */
        MatrixOpenIdToken: {
            /**
             * @description The OpenID token itself. Not a Matrix access token: it proves an
             *     identity to a third party and nothing more.
             */
            access_token: string;
            /**
             * @description The homeserver that minted the token. Checked against this
             *     deployment's homeserver before any outbound call; absent, the
             *     Gateway asks its own homeserver, the only one it would ask.
             */
            matrix_server_name?: string;
        };
        /**
         * @description The endpoint and model the operator named, as a browser is told
         *     them. The credential is described and never carried.
         */
        ModelConfiguration: {
            /**
             * @description The OpenAI-compatible chat-completions **base**, without a
             *     trailing slash. Normalised on write, so the runtime and the
             *     probe build the same `…/chat/completions`.
             * @example http://127.0.0.1:4000/v1
             */
            base_url: string | null;
            /**
             * @description Whether a model has been named at all. `false` is a state and
             *     not a failure: every other member is then `null`, and a settings
             *     screen draws it without an error branch.
             */
            configured: boolean;
            credential: components["schemas"]["EndpointCredential"];
            /**
             * @description The model name **the configured endpoint** knows, not a family
             *     name: `qwen` on the reference deployment, because that is what
             *     its LiteLLM serves it as.
             * @example qwen
             */
            model: string | null;
            /**
             * @description The provider passthrough, merged into every completion request
             *     untouched. `null` when there is none, which is the healthy shape
             *     with a proxy in front.
             */
            params: Record<string, unknown> | null;
            /**
             * @description Per-persona overrides. **Always `{}` in v0.1.** The member
             *     exists so that adding the first override later is not a change
             *     of shape; shipping a working override with one persona would
             *     ship an unexercised path.
             */
            personas: Record<string, unknown>;
            /**
             * Format: date-time
             * @description When the configuration was last written. RFC 3339, to the millisecond.
             */
            updated_at: string | null;
        };
        /**
         * @description A whole model configuration. `credential` absent keeps the stored
         *     one, `null` forgets the one set from this browser, and a string
         *     replaces it; the three cases exist because the credential is
         *     write-only, so a client cannot round-trip it.
         */
        ModelConfigurationRequest: {
            /**
             * @description An absolute `http` or `https` chat-completions **base** URL with
             *     a host.
             * @example http://127.0.0.1:4000/v1
             */
            base_url: string;
            /**
             * @description The endpoint's bearer token. Write-only: no read of this API
             *     returns it. An empty string is refused — `null` is how it is
             *     forgotten.
             */
            credential?: string | null;
            /** @example qwen */
            model: string;
            /**
             * @description Provider parameters merged into every completion request
             *     untouched and last, a member set to `null` removing a field the
             *     request would otherwise carry. The escape hatch for an operator
             *     with no proxy in front; leave it out otherwise.
             * @example {
             *       "max_tokens": 2000
             *     }
             */
            params?: Record<string, unknown> | null;
        };
        /** @description The configured endpoint answered a chat completion. */
        ModelProbe: {
            /** @description The endpoint that was probed. */
            base_url: string;
            /**
             * @description The model the endpoint echoed back, when it did. A proxy that
             *     maps a friendly name onto a provider's real one reports the
             *     provider's here, which is how an operator confirms what actually
             *     answered.
             */
            endpoint_model: string | null;
            /** @description The status the endpoint answered with. */
            endpoint_status: number;
            /** @description The model name that was sent. */
            model: string;
            /**
             * @description Always `ok` here. The other outcomes are refusals with their own
             *     statuses and their own `error` codes, so a client never has to
             *     read a success to discover a failure.
             * @enum {string}
             */
            outcome: "ok";
        };
        /**
         * @description A messaging network as the user experiences it, and `matrix` for
         *     native rooms (ADR 0005, ADR 0009). The contract's own `network`
         *     definition, exactly (`contracts/cloudevents/v1/definitions/network.schema.json`,
         *     the one authority — `tests/openapi.rs` compares): a bridge id
         *     (`gmessages`) is never a network; `email` is one (ADR 0033).
         * @enum {string}
         */
        Network: "whatsapp" | "telegram" | "signal" | "discord" | "sms" | "matrix" | "email";
        PendingConnectionCount: {
            connection: components["schemas"]["ConnectionId"];
            /** @description How many contacts are waiting on that connection. */
            count: number;
            /** @description The connection's kind. */
            network: components["schemas"]["Network"];
        };
        /**
         * @description One contact waiting for a decision, on one connection. An id, a
         *     perimeter and two instants — and deliberately nothing else: a body, a
         *     display name or a network identifier would each turn this list into
         *     something else. `network` is the connection's kind (#270).
         */
        PendingContact: {
            /**
             * @description The connection they wrote on: the perimeter a decision about
             *     them is scoped to.
             */
            connection: components["schemas"]["ConnectionId"];
            /**
             * @description The contact's Matrix user ID, as the bridge materialised it, and
             *     what a decision about them will name as its subject.
             * @example @whatsapp_33612345678:example.com
             */
            contact: string;
            /**
             * Format: date-time
             * @description When this contact first wrote, from the event's own `time` - not
             *     when this Gateway happened to read it.
             */
            first_seen: string;
            /**
             * Format: date-time
             * @description When this contact last wrote, on the same terms.
             */
            last_seen: string;
            network: components["schemas"]["Network"];
        };
        PendingContactCount: {
            /** @description How many contacts are waiting on that network. */
            count: number;
            network: components["schemas"]["Network"];
        };
        PendingContacts: {
            /**
             * @description The same total, broken down per connection (#272): the numbers a
             *     screen that decides per connection reads. In the contract's
             *     order of network values, then by connection id. A connection
             *     with nothing waiting is absent rather than present at zero.
             */
            connections: components["schemas"]["PendingConnectionCount"][];
            /**
             * @description The contacts themselves, oldest first sighting first - the order
             *     the user met them in, and stable between polls. Narrowed by
             *     `?network=` when one was given.
             */
            contacts: components["schemas"]["PendingContact"][];
            /**
             * @description The same total, broken down per network, in the contract's own
             *     order of network values — kept for a screen that has not learned
             *     connections yet. A network with nothing waiting is absent rather
             *     than present at zero.
             */
            networks: components["schemas"]["PendingContactCount"][];
            /**
             * @description How many contacts are waiting for a decision in all - the
             *     dashboard's number. Counts the whole list, never only what a
             *     `?connection=` or `?network=` filter left in `contacts`.
             */
            total: number;
        };
        /** @description One persona the runtime created a consumer for. */
        PersonaPresence: {
            /** @description Messages delivered and not yet acknowledged at the last sample. */
            ack_pending: number;
            /**
             * @description What the runtime set the consumer's filter subject to: the
             *     inbound subject (`active`) or the paused subject nothing
             *     publishes on (`paused`, ADR 0013).
             * @enum {string}
             */
            activation: "active" | "paused";
            /**
             * @description The consumer's durable name on the stream.
             * @example persona-assistant
             */
            consumer: string;
            /**
             * @description `live` - a pull is outstanding or a delivered message awaits its
             *     ack; `idle` - neither, across every sample.
             * @enum {string}
             */
            liveness: "live" | "idle";
            /**
             * @description The persona's id, as its consumer's name carries it.
             * @example assistant
             */
            persona_id: string;
            /** @description Pull requests outstanding at the last sample. */
            waiting_pulls: number;
        };
        /** @description One conversation, as the homeserver answers about it now. */
        Portal: {
            /**
             * @description The bridge instance whose bot is in this room, as
             *     `GATEWAY_BRIDGES` names it. Never a network value.
             */
            bridge_id: string;
            /**
             * @description How many people are in the conversation: joined members,
             *     excluding the bridge bot and the Sensor. Excluding them is what
             *     keeps the number from changing when the user decides to observe
             *     the room.
             */
            members: number;
            /**
             * @description The room this conversation lived in before it was replaced —
             *     the immediate predecessor, out of the homeserver's own tombstone
             *     chain — or `null` for a room that was never replaced. Present
             *     whatever the observation: a conversation nobody chose moves too,
             *     and stays `absent`.
             */
            moved_from: string | null;
            /**
             * @description The room's name — for a one-to-one portal the contact's, for a
             *     group the group's — or `null` where the bridge set none. Read
             *     through and stored nowhere.
             */
            name: string | null;
            network: components["schemas"]["Network"];
            /**
             * @description **The network's own identifier for this conversation**, exactly as
             *     the bridge wrote it into the room's `m.bridge` marker
             *     (`channel.id`), or `null` where it wrote none. Read through and
             *     stored nowhere, like `name`.
             *
             *     Passed through untouched — not parsed, not normalised, not
             *     invented by this Gateway. It is the network's vocabulary, and its
             *     **suffix** is what says what kind of conversation this is without
             *     guessing: `231546065817642@lid` and
             *     `33612345678@s.whatsapp.net` are how WhatsApp addresses one
             *     person, `120363201980306353@g.us` how it addresses a group.
             *     Reading that suffix is the Companion's job (#143); nothing here
             *     interprets it.
             *
             *     It is served because a chooser has to be able to say *which*
             *     conversation. A WhatsApp community arrives as a parent, an
             *     announcement group and subgroups carrying near-identical names —
             *     two rooms called `Communauté CKCP`, created in the same minute,
             *     with 109 members and 6 — and a name and a headcount cannot tell
             *     those apart.
             */
            network_conversation_id: string | null;
            /**
             * @description `observing` — the Sensor has joined, and this conversation
             *     reaches the bus.
             *     `invited` — the Sensor was invited and has not joined; normally
             *     a moment, and when it lasts, `SENSOR_ALLOWED_INVITERS` does not
             *     name this bridge's bot.
             *     `absent` — the Sensor is not in the room and has not been asked
             *     to be. The default for every portal a bridge builds.
             *     `moved` — the conversation's room was replaced while the Sensor
             *     was in it, and the Sensor is not in the successor (ADR 0029). A
             *     decision is on record about this conversation and has silently
             *     stopped meaning anything; this state is what makes it a sentence
             *     instead of a zero. `moved_from` names the dead room.
             * @enum {string}
             */
            observation: "observing" | "invited" | "absent" | "moved";
            /**
             * @description Where the **owner's own account** stands in this room — the fact
             *     that decides whether an approved reply into this conversation can
             *     be delivered at all (issue #216, ADR 0025). A bridge relays only
             *     what the logged-in user's own account sends, so `join` is what a
             *     reply needs; `invite` is the bridge having asked and no device of
             *     the owner's having accepted — every portal on a deployment with
             *     no owner device (#123), or a room that device could not join
             *     (#237); `absent` is not in the room and not asked. `null` when
             *     this register was built with no owner to ask about, or for a
             *     successor it could not read.
             * @enum {string|null}
             */
            owner_membership: "join" | "invite" | "absent" | null;
            /**
             * @description The portal room on this deployment's homeserver — the room that
             *     is **alive**. A room replaced by another (`m.room.tombstone`) is
             *     never a row: its conversation appears once, at the successor,
             *     with `moved_from` saying where it came from (ADR 0029).
             */
            room_id: string;
            /**
             * @description `null` normally. A reason when this row stands for a successor
             *     the register **could not read** — the bridge bot is not in it —
             *     and was built from the tombstone that names it: the name, network
             *     and count are the dead room's, the room id is the successor's. A
             *     tombstone that points into the dark is a fact, not a reason to
             *     fold the conversation into nothing.
             */
            unreadable: string | null;
        };
        /**
         * @description Whether one configured bridge could be read, **which account did the
         *     asking**, and how many rooms that account is in.
         *
         *     The last two are there because `absent: 0` with every bridge readable
         *     used to say two different things — "your bridges have built no
         *     conversations yet", which is the truth on a fresh deployment, and "the
         *     register asked an account that is in no rooms", which is a defect — and
         *     a deployment with 32 portal rooms could not tell which it had been told
         *     (#171).
         */
        PortalBridgeReading: {
            /**
             * @description The Matrix ID the register actually spoke as: the bridge bot the
             *     operator configured (`GATEWAY_BRIDGE_<ID>_BOT_USER_ID`), or —
             *     where none is configured — whatever the homeserver's own
             *     `whoami` says that credential is. `null` only where the bridge
             *     could not be read at all.
             *
             *     An appservice token used with no `?user_id=` acts as the
             *     registration's `sender_localpart`, which for a generated
             *     registration is a random localpart joined to nothing. That is what
             *     this field is for: it names the account so a zero can be
             *     attributed.
             */
            asked_as: string | null;
            bridge_id: string;
            /**
             * @description On `readable: false`, what stopped the read, in the operator's
             *     words — the variable that would open it, or the homeserver's own
             *     refusal. `null` otherwise.
             */
            detail: string | null;
            /**
             * @description How many rooms that account is joined to — **every** room, not
             *     only the ones carrying an `m.bridge` marker. `0` says the asker is
             *     in nothing at all; a positive number with no portals says it is in
             *     rooms and none of them is a conversation. `null` where the bridge
             *     could not be read.
             */
            joined_rooms: number | null;
            network: components["schemas"]["Network"];
            /**
             * @description `false` means none of this bridge's conversations are in
             *     `portals` or in `summary`.
             */
            readable: boolean;
        };
        /** @description One conversation's move, as the register decided it (#255). */
        PortalMove: {
            bridge_id: string;
            /** @description The threshold applied, kept so the entry stays readable after the operator changes it. */
            crowd_threshold: number;
            /** Format: date-time */
            decided_at: string;
            /**
             * @description `true` — the Sensor was invited into the successor: the decision
             *     followed the conversation. `false` — the successor's audience is
             *     at or above the threshold, so the conversation went back to the
             *     chooser as a crowd, `moved`, for the user to acknowledge again.
             */
            followed: boolean;
            /**
             * @description People in the successor when the register decided, bridge bot
             *     and Sensor excluded — the number the threshold was applied to.
             */
            members: number;
            /** @description The room it left — the one the user's decision named. */
            predecessor: string;
            /** @description The room the conversation lives in now. */
            successor: string;
        };
        /** @description One outcome per room asked about, in the order asked. */
        PortalObservation: {
            /** @description The decision that was applied, echoed back. */
            observed: boolean;
            outcomes: components["schemas"]["PortalObservationOutcome"][];
        };
        /** @description What happened in one room. */
        PortalObservationOutcome: {
            /**
             * @description Present on `failed` alone: the homeserver's own error code
             *     (`M_FORBIDDEN` where the bot may not invite or remove), or the
             *     Gateway's own where there was none.
             */
            reason?: string;
            /** @description The room, exactly as the request spelled it. */
            room_id: string;
            /**
             * @description `invited` — the Sensor was invited and joins on its own.
             *     `already_observed` — it was already in the room, or already
             *     invited to it; asking twice is not an error.
             *     `removed` — it was taken out, so this conversation stops
             *     reaching the bus.
             *     `not_observed` — it was not in the room to begin with.
             *     `unknown_portal` — this room is not a portal of any bridge this
             *     Gateway can read, and nothing was attempted in it.
             *     `failed` — this room alone did not work; `reason` says why.
             * @enum {string}
             */
            status: "invited" | "already_observed" | "removed" | "not_observed" | "unknown_portal" | "failed";
        };
        /** @description The user's decision about one or more conversations. */
        PortalObservationRequest: {
            /**
             * @description `true` puts the Sensor into each conversation, `false` takes it
             *     out.
             */
            observed: boolean;
            /**
             * @description The portal rooms to decide about, as `GET /api/portals` spelled
             *     them.
             */
            rooms: string[];
        };
        /**
         * @description Every portal room every readable bridge holds, what it is called,
         *     how many people are in it, and where the Sensor stands in it.
         */
        PortalRegister: {
            /** @description Every configured bridge, readable or not. */
            bridges: components["schemas"]["PortalBridgeReading"][];
            /**
             * @description The member count at or above which a conversation is a **crowd**
             *     the user must acknowledge the size of before observing it (#143):
             *     `GATEWAY_CROWD_THRESHOLD`, default 20. Served because it has one
             *     owner and it is this Gateway (#252, ADR 0029): the chooser draws
             *     its crowds section from this number and holds none of its own,
             *     and the register applies the same number when a conversation's
             *     room is replaced — followed under it, returned to the chooser
             *     above it.
             */
            crowd_threshold: number;
            portals: components["schemas"]["Portal"][];
            summary: components["schemas"]["PortalSummary"];
        };
        /**
         * @description The counts the deployment states rather than leaving a client to
         *     derive: the same numbers `/metrics` carries.
         */
        PortalSummary: {
            absent: number;
            invited: number;
            /**
             * @description Conversations that moved while the Sensor was in them and whose
             *     successor it is not in — decisions on record that no longer
             *     hold (ADR 0029, #253).
             */
            moved: number;
            observing: number;
            /**
             * @description Conversations across every **readable** bridge, each counted
             *     once whatever its history of rooms. Read it together with
             *     `bridges`.
             */
            total: number;
        };
        /**
         * @description What the Sensor said one approved reply reached, once it posted it
         *     (issue #216, ADR 0025). The Sensor republishes the approval unchanged
         *     on `twalk.persona.reply.approved.v1.posted` with two headers, and this
         *     is those headers. Published on the good case as well as the bad one:
         *     a report that existed only when something failed would make success a
         *     silence.
         */
        PostedReport: {
            /** @description The Matrix ID the reply was posted by. */
            posted_as: string;
            /**
             * @description `contact` — the reply was posted by a device of the owner's own
             *     account into a room the owner is a joined member of, which is what
             *     a bridge relays; or the room is native Matrix (ADR 0009) with no
             *     bridge in the way.
             *     `nobody` — the reply was posted as `@sensor:` into a portal room.
             *     Synapse accepted it and the bridge ignored it, because a bridge
             *     relays only the logged-in user's own account (#123): the message
             *     exists in a room the contact cannot see.
             * @enum {string}
             */
            reach: "contact" | "nobody";
            /** @description Where on the bus the report landed. */
            stream_sequence: number;
        };
        /** @description A decision as the journal holds it. */
        RecordedConsentDecision: {
            /**
             * @description The Matrix user ID of the owner the decision is attributed to.
             *     One owner per deployment (ADR 0011), so this is the human, not
             *     the device it arrived from.
             */
            actor: string;
            /**
             * @description The id of the `consent.state.changed.v1` event this decision is
             *     published as: the contract's deterministic key, the lowercase-hex
             *     sha256 of
             *     `subject.type:subject.id:new_state:<sorted connections>:occurred_at`.
             *     A consumer can match a bus event to this answer by it. (Before
             *     #270 the recipe joined the networks; a connection named after
             *     its network makes the same string, so no id changed.)
             */
            event_id: string;
            new_state: components["schemas"]["ConsentState_State"];
            /**
             * Format: date-time
             * @description When the decision was taken, stamped by the Gateway to the
             *     millisecond. Part of the event's id, and stored verbatim so the
             *     id stays reproducible.
             */
            occurred_at: string;
            /**
             * @description What the subject held on this perimeter before, read inside the
             *     recording transaction from the most recent decision covering any
             *     of the scoped networks. `unset` means none ever did.
             * @enum {string}
             */
            old_state: "unset" | "granted" | "pending" | "revoked";
            /**
             * @description `true` when the journal already held this exact decision and
             *     nothing was recorded (the `200` answer).
             */
            replayed: boolean;
            scope: components["schemas"]["ConsentScope"];
            /**
             * @description The decision's position in the journal, which is the only
             *     ordering the Gateway trusts. Not the bus's sequence — that is
             *     `decision_sequence` against `stream_sequence` in
             *     `GET /api/consent/snapshot`.
             */
            sequence: number;
            subject: components["schemas"]["ConsentSubject"];
        };
        /** @description What happened in one room the user selected. */
        RoomInvitation: {
            /**
             * @description Present on `failed` alone: the homeserver's own error code
             *     (`M_FORBIDDEN` where the user may not invite in that room), or
             *     the Gateway's (`invalid_room_id` for a string that is not a
             *     room id at all).
             */
            reason?: string;
            /** @description The room, exactly as the request spelled it. */
            room_id: string;
            /**
             * @description `invited` — the Sensor was invited and will join on its own.
             *     `already_present` — it is already in the room, or already
             *     invited to it; asking twice is not an error.
             *     `failed` — this room alone did not work; `reason` says why.
             * @enum {string}
             */
            status: "invited" | "already_present" | "failed";
        };
        RuntimeLlm: {
            /**
             * @description The credential **in force**, resolved: the operator's file if
             *     there is one, otherwise whatever the Companion set, otherwise
             *     `null` for an endpoint that needs none.
             */
            api_key: string | null;
            base_url: string;
            /** @enum {string|null} */
            credential_source: "file" | "companion" | null;
            model: string;
            params: Record<string, unknown> | null;
        };
        /**
         * @description Whether a persona runtime is present, read off the bus's consumer
         *     list (ticket #189), and the evidence per persona.
         */
        RuntimePresence: {
            /**
             * @description One row per consumer the runtime created, live or not, in
             *     consumer-name order. Empty exactly when `presence` is `never`.
             */
            personas: components["schemas"]["PersonaPresence"][];
            /**
             * @description `never` - no persona consumer exists; `gone` - consumers exist
             *     and none is live; `present` - at least one is live.
             * @enum {string}
             */
            presence: "never" | "gone" | "present";
        };
        /**
         * @description Everything the Hermes runtime injects into a persona's environment.
         *     The one document on this origin that carries the endpoint
         *     credential.
         */
        RuntimeSettings: {
            /**
             * @description The user's native language, for the fallback ADR 0016 defines.
             *     `null` when they have set none.
             * @enum {string|null}
             */
            language: "en" | "fr" | "it" | "es" | "de" | null;
            /**
             * @description `null` when the operator has named no model — which is a
             *     different fact from a Gateway the runtime could not reach and
             *     from one that refused its token, and the runtime refuses to
             *     start saying exactly that (ADR 0015).
             */
            llm: components["schemas"]["RuntimeLlm"] | null;
            /** @description Per-persona overrides. Always `{}` in v0.1. */
            personas: Record<string, unknown>;
        };
        /** @description One outcome per room asked about, in the order asked. */
        SensorInvitation: {
            rooms: components["schemas"]["RoomInvitation"][];
            /**
             * @description The Matrix ID the Gateway invited (`GATEWAY_SENSOR_USER_ID`), so
             *     the Companion can name it on screen rather than guess it.
             */
            sensor: string | null;
        };
        /**
         * @description The Matrix ID of the Sensor this deployment runs
         *     (`GATEWAY_SENSOR_USER_ID`), and `null` when the operator configured
         *     none.
         *
         *     Here because the Companion has to **create a room with it** during
         *     onboarding (ticket #226, ADR 0034): the browser hands the Sensor a
         *     device credential as an Olm-encrypted to-device message, and that
         *     send is a silent no-op unless the two accounts already share an
         *     encrypted room. A browser that has to name the Sensor cannot derive
         *     it — `@sensor:<server>` is a deployment's convention and not a fact,
         *     which is the refusal ADR 0018 made about the owner's network ghosts
         *     and ADR 0024 about a bridge bot's localpart.
         *
         *     Said in the session document rather than in a new operation, and only
         *     to a signed-in device: it is a fact about the deployment the owner
         *     already knows, and `GET /api/deployment` deliberately names no
         *     identity to an unauthenticated caller. The alternative on offer was
         *     `POST /api/bootstrap/rooms` with an empty room list, which answers
         *     `sensor` today — and which would mean handing the Gateway the user's
         *     own Matrix access token to read a configured string, for no other
         *     reason.
         */
        SensorUserId: string | null;
        /** @description Who is signed in, on which device. */
        Session: {
            device: components["schemas"]["Device"];
            /** @description The homeserver name that identity belongs to. */
            homeserver: string;
            /**
             * @description The Matrix ID of the single human this deployment serves
             *     (`GATEWAY_OWNER`).
             */
            owner: string;
            sensor: components["schemas"]["SensorUserId"];
        };
        SignInRequest: {
            /**
             * @description How this device should appear in the device list — the user's
             *     own words ("Pixel 8", "the laptop"). Optional; the Gateway names
             *     an unnamed device itself.
             */
            device_name?: string;
            matrix_openid_token: components["schemas"]["MatrixOpenIdToken"];
        };
        /**
         * @description A flow to run, and optionally the login to repair. Closed: the acting
         *     user is the deployment's owner, from configuration, and no request
         *     can name somebody else.
         */
        StartBridgeLoginRequest: {
            /** @description One of the ids `GET .../login/flows` lists. */
            flow_id: string;
            /**
             * @description Reconnect: re-log in to this existing login instead of creating a
             *     second one. This is what repairs a broken session.
             */
            login_id?: string;
        };
        SubmitBridgeLoginStepRequest: {
            /**
             * @description What the step's own payload asked for, passed to the bridge
             *     untouched: `{"phone_number": "+33…"}` for a `user_input` step,
             *     `{"cookies": {…}}` for a `cookies` one. A network credential — it
             *     is relayed and forgotten, never stored and never logged
             *     (ADR 0011).
             */
            data: {
                [key: string]: unknown;
            };
            /**
             * @description The step being answered — the `step.step_id` the polled state
             *     reports. A different one is refused rather than guessed at.
             */
            step_id: string;
        };
        /**
         * @description One suggestion as it stands on the bus right now. The same shape from
         *     `GET /api/suggestions` and from `GET /api/suggestions/{id}`, so a
         *     client renders one thing.
         */
        Suggestion: {
            /**
             * @description The approval this Gateway recorded, or `null` when there is none.
             *     Its own `publication` member says whether the reply reached the
             *     bus, which is the answer to "did it actually go out?" — the same
             *     document `GET /api/approvals/{id}` answers with.
             */
            approval: null | components["schemas"]["Approval"];
            /**
             * @description Which suggestion this is for that trigger, starting at 1. Several
             *     may exist for one message; each attempt has its own id.
             */
            attempt: number | null;
            /**
             * @description The consent label the trigger carried **when the Sensor observed
             *     it**. An audit fact about the past, and never the current state:
             *     consent can have been revoked since, which is exactly why
             *     `POST /api/approvals` re-reads it rather than trusting this.
             * @enum {string}
             */
            consent: "granted" | "pending" | "revoked";
            /**
             * @description Who this reply answers and what they asked (#334, resolving
             *     #160), written by the persona at publication time: the one
             *     component allowed to read the message, saying what it says while
             *     it reads it, so that nothing downstream has to reopen an inbound
             *     event to draw an approval screen (#110, ADR 0012).
             *
             *     `null` when the suggestion carries none — a persona that predates
             *     this, or one that chose not to — and `null` when the label the
             *     trigger carried was not `granted`: a context is a contact's
             *     message in somebody else's words, and it lives under the consent
             *     of the words it derives from. Whether consent still stands *now*
             *     is not folded in here, for the reason the rest of this listing
             *     does not fold it in either: that question is answered at `POST
             *     /api/approvals`, the moment it matters.
             */
            context: null | {
                /**
                 * @description The contact's display name as the trigger carried it, or
                 *     `null` when the network gave none.
                 */
                contact: null | string;
                /**
                 * @description What the contact asked, in the persona's own words —
                 *     never a quotation of their message.
                 */
                summary: string;
            };
            delivery: components["schemas"]["Delivery"];
            /**
             * @description The sentence the reply will disclose itself with, after the body
             *     and on a line of its own, in the language the persona wrote in —
             *     *"Rédigé avec mon assistant IA."*, *"Drafted with my AI
             *     assistant."* (ticket #121, ADR 0019, ADR 0031). Selected by the
             *     persona, or by the Gateway from the language Hermes declared;
             *     never composed here. A screen shows it **fixed** beside the
             *     editable body, because it is not in the field the user edits and
             *     cannot be removed from one message. Whether it is appended at
             *     approval is the switch's business (`GET /api/settings/disclosure`),
             *     not this read's. `null` when the suggestion carries none — one
             *     published before the member existed, or by a persona that set
             *     none — and the reply then goes out undisclosed. Always one of
             *     the contract's five sentences verbatim: a value on the bus that
             *     is none of them is listed as `null` rather than drawn as fixed,
             *     and `POST /api/approvals` refuses that suggestion as
             *     `suggestion_unreadable`.
             */
            disclosure: string | null;
            /**
             * @description The CloudEvents id of the `persona.suggest.produced.v1`. This is
             *     what `POST /api/approvals` takes as `suggestion_event_id`.
             */
            event_id: string;
            /**
             * @description When it goes stale (ticket #22). `null` when the persona set
             *     none, which the contract allows and the first-party SDK never
             *     does — a suggestion with no expiry can be approved at any later
             *     date.
             */
            expires_at: string | null;
            /**
             * @description The reply was given up on (#311): the Sensor or the collector
             *     exhausted its retries, or was refused for good, and
             *     dead-lettered the approval on
             *     `twalk.persona.reply.approved.v1.dead`.
             *
             *     A fact of its own, beside `approval.publication` and `posted`,
             *     and never a variant of either: those two say where a reply that
             *     went out got to, and this one says it did not go out. Until this
             *     member existed, an approval whose reply could never be sent read
             *     as `published` for ever — the silence #216 was written against,
             *     and the one that told an owner "sent" for a message that never
             *     left.
             *
             *     A screen that shows `approval.publication: published` without
             *     reading this one is telling the user something that may have
             *     stopped being true.
             *
             *     Deliberately **not** called `undelivered`: by this project's
             *     glossary a reply whose `reach` is `nobody` is undelivered too,
             *     and that one was posted. `given_up` names what happened — the
             *     sender abandoned the send.
             */
            given_up: null | components["schemas"]["GivenUpReport"];
            /** @description The network the message it answers arrived on. */
            network: components["schemas"]["Network"];
            /** @description The persona that proposed it. */
            persona_id: string;
            /**
             * @description The Sensor's report of what the approved reply reached, once it
             *     posted it, or `null` while there is none — before the approval,
             *     while the Sensor has not posted it yet, or when the report lies
             *     beyond the read window. Never the same fact as
             *     `approval.publication` (#216).
             */
            posted: null | components["schemas"]["PostedReport"];
            /**
             * Format: date-time
             * @description When the persona produced the suggestion.
             */
            produced_at: string;
            /**
             * @description `hermes://<domain>/personas/<persona id>` — the persona that
             *     proposed it, and the `source` the approved reply will carry
             *     (ADR 0022).
             */
            source: string;
            /**
             * @description Where this suggestion stands, as far as approving it goes.
             *
             *     - `approvable` - nothing about the suggestion itself stops it.
             *     - `expired` - its `expires_at` has passed. Still readable, no
             *       longer approvable.
             *     - `approved` - this Gateway recorded an approval of it, and
             *       `approval` says where the reply landed.
             *
             *     A suggestion that does not exist is not a fourth value here: it
             *     is `404` from `GET /api/suggestions/{id}` and an absence from the
             *     listing, so a screen cannot confuse "gone stale" with "never
             *     was". Nor is the sender's consent folded in: that is a different
             *     fact about a different subject, read at the moment of approval.
             * @enum {string}
             */
            standing: "approvable" | "expired" | "approved";
            /** @description Where on the bus this suggestion was found. */
            stream_sequence: number;
            /**
             * @description What the persona proposed — the text the screen draws and the
             *     user approves, edits or refuses. It is the persona's own words,
             *     which is why it is here when the quoted message's are not.
             */
            suggestion: {
                /**
                 * @description The suggested reply in its canonical text form — the
                 *     persona's words alone, without the disclosure.
                 */
                body: string;
                /**
                 * @description Content type of the body.
                 * @enum {string}
                 */
                format: "text/plain" | "text/markdown" | "text/html";
            };
            trigger: components["schemas"]["SuggestionTrigger"];
        };
        /**
         * @description The suggestions in the read window, newest first, and what the read
         *     covered.
         */
        SuggestionListing: {
            /** @description Newest first. */
            suggestions: components["schemas"]["Suggestion"][];
            /**
             * @description `true` when `limit` cut the list: more suggestions were found
             *     inside the window than were answered with. Distinct from
             *     `window.reached_start_of_stream`, which is about the bus rather
             *     than about the request.
             */
            truncated: boolean;
            /**
             * @description Suggestions found in the window that this build could not read —
             *     an unknown network, an unknown consent state, a content type the
             *     contract does not name. Counted rather than dropped in silence,
             *     so a screen missing a row has somewhere to look. A single read of
             *     one of these answers `409 suggestion_unreadable` instead.
             */
            unreadable: number;
            window: components["schemas"]["SuggestionReadWindow"];
        };
        /**
         * @description The stretch of the stream an answer was computed from.
         *
         *     The bus has no index from a CloudEvents id to a stream position, so
         *     finding suggestions means reading the stream, so the read is bounded.
         *     A bound nobody can see is a bound that lies, which is why it is here
         *     and not only in the configuration.
         */
        SuggestionReadWindow: {
            /** @description The first stream position read. `0` when the stream is empty. */
            from_sequence: number;
            /**
             * @description `true` when the read began at the stream's first retained
             *     message, so nothing older exists to have been missed. `false`
             *     means there may be older suggestions this Gateway did not read —
             *     a fact about this answer, not about the bus.
             */
            reached_start_of_stream: boolean;
            /**
             * @description The configured width of the read
             *     (`GATEWAY_APPROVAL_LOOKUP_WINDOW`), shared with the approval
             *     lookup because it is the same bus and the same question.
             */
            sequences: number;
            /**
             * @description The last, which is the stream's head at the moment of the read.
             *     `0` when the stream is empty.
             */
            to_sequence: number;
        };
        /**
         * @description The message a suggestion answers, **by identity alone**.
         *
         *     There is no member here for its text, its sender, its room or an
         *     excerpt of what it was itself quoting, and there will not be one. An
         *     excerpt belongs to the author of the quoted message rather than to
         *     whoever sent the event carrying it, and a listing that re-published a
         *     revoked contact's words would be ticket #110's defect one layer up
         *     (ADR 0012). The enforcement is not this schema: the Gateway's
         *     projection never opens an inbound event at all.
         */
        SuggestionTrigger: {
            /** @description The CloudEvents id of the event the suggestion replies to. */
            event_id: string;
            /**
             * @description Its CloudEvents type, e.g.
             *     `fr.linagora.twalk.inbound.message.received.v1`.
             */
            event_type: string;
        };
    };
    responses: {
        /**
         * @description This deployment approves nothing: `GATEWAY_NATS_URL` is unset, so
         *     there is no bus to read the suggestion from and none to publish the
         *     reply on. Answered before the suggestion is looked at, so it is
         *     never mistaken for a statement about that suggestion; `detail` names
         *     the variables.
         */
        ApprovalsNotConfigured: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "approvals_not_configured";
                };
            };
        };
        /**
         * @description The bridge itself, or this build's conversation with it, is the
         *     problem — never the user's values. The four codes are four different
         *     investigations, and telling them apart without reading `detail` is
         *     the point of having four.
         *
         *     - `bridge_unreachable` — **nothing answered**: connection refused,
         *       timeout, no route. This is the one an operator checks containers,
         *       ports and addresses for.
         *     - `bridge_refused` — it answered an **error of its own**, with its
         *       own mautrix errcode; `detail` names the code and what it usually
         *       means (a wrong provisioning secret, a user without login
         *       permissions, a stale transaction id).
         *     - `bridge_answer_unusable` — it answered **successfully**, and this
         *       build could not use the answer: it looked for a field and did not
         *       find it. The bridge is running and replied, so this is a defect in
         *       Twalk's reading of that bridge's provisioning API rather than a
         *       broken deployment, and nothing about the deployment will explain
         *       it. `detail` names the provisioning call and what was looked for.
         *     - `bridge_request_unusable` — the mirror: it answered `400
         *       M_NOT_JSON`, meaning it could not read what **this build sent**.
         *       bridgev2 refuses the body before any connector runs, so the
         *       network saw nothing, refused nothing, and the login is still
         *       waiting on the same step. A defect in Twalk's writing of the
         *       provisioning API; `detail` says so and names the code. It exists
         *       because a cookie jar sent as a nested map arrived here as
         *       `invalid_request` — *"the network refused what was submitted,
         *       start the login again"* — and sent a user round the same paste
         *       three times (#221).
         *
         *       It exists because it used to be `bridge_unreachable` (#116): the
         *       first live WhatsApp login failed here, was reported as a bridge
         *       that could not be reached, and sent a debugging session to
         *       networking and ports — the one place the fault was not.
         *
         *       `detail` never repeats the bridge's answer, not even a fragment of
         *       it: a provisioning answer can carry identifiers from a network
         *       account — a phone number as a login id, an account name, a QR
         *       payload. It names what was missing, not what was received.
         */
        BridgeUnavailable: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "bridge_unreachable" | "bridge_refused" | "bridge_answer_unusable" | "bridge_request_unusable";
                };
            };
        };
        /**
         * @description This deployment writes no consent: `GATEWAY_NATS_URL` is unset, so
         *     the Gateway has no bus to publish a decision on and refuses to
         *     record one it could not announce. The session and the origin are
         *     unaffected; `detail` names the variable.
         */
        ConsentNotConfigured: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "consent_not_configured";
                };
            };
        };
        /**
         * @description The consent journal (SQLite, the record of truth for consent)
         *     failed. On a write, nothing was recorded and nothing was published —
         *     the answer is a failure precisely so that a client is never told a
         *     decision is stored when it is not.
         */
        ConsentStoreUnavailable: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "store_unavailable";
                };
            };
        };
        /**
         * @description This deployment projects no pending contacts: `GATEWAY_NATS_URL` is
         *     unset, so the Gateway consumes no inbound stream and has no list to
         *     answer with. An empty list would claim that nobody has written to the
         *     user, which is a very different statement from "this Gateway is not
         *     watching"; `detail` names the variables.
         */
        ContactsNotConfigured: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "contacts_not_configured";
                };
            };
        };
        /**
         * @description - `sign_in_not_configured` — this deployment has no owner
         *       (`GATEWAY_OWNER` is unset), so its whole API is closed. This is
         *       what the guard answers.
         *     - `consent_not_configured` — `GATEWAY_NATS_URL` is unset, so there
         *       is no consent store, no disclosure journal in it, and no approval
         *       path for the switch to govern. The consent routes' own code, so a
         *       client learns one fact under one word.
         */
        DisclosureNotConfigured: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "sign_in_not_configured" | "consent_not_configured";
                };
            };
        };
        /**
         * @description `store_unavailable` — the disclosure journal could not be read, or
         *     the decision could not be recorded; nothing was changed.
         */
        DisclosureStoreUnavailable: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "store_unavailable";
                };
            };
        };
        /**
         * @description `no_login_in_flight` — nothing has been started on this bridge, so
         *     there is nothing to poll, answer or cancel. `unknown_bridge` — no
         *     such bridge is configured.
         */
        NoBridgeLogin: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "no_login_in_flight" | "unknown_bridge";
                };
            };
        };
        /**
         * @description This Gateway holds no portal register: there is no Sensor to invite
         *     (`GATEWAY_SENSOR_USER_ID`), no bridge configured (`GATEWAY_BRIDGES`),
         *     or no homeserver to read them from — or it has no owner at all and
         *     the whole API is closed. A refusal rather than an empty list: "your
         *     bridges have built no conversations" and "this Gateway cannot see
         *     them" are very different claims, and only one of them is about the
         *     user's messages. `detail` names what would open it.
         */
        PortalsNotConfigured: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "portals_not_configured" | "sign_in_not_configured";
                };
            };
        };
        /**
         * @description - `sign_in_not_configured` — this deployment has no owner
         *       (`GATEWAY_OWNER` is unset), so it keeps no settings and its whole
         *       API is closed. This is what the guard answers, and in practice it
         *       is the one a caller sees.
         *     - `settings_not_configured` — there is no settings store. The two
         *       are configured together, so this is the handler saying the same
         *       fact rather than leaving a corner of the surface silent.
         */
        SettingsNotConfigured: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "sign_in_not_configured" | "settings_not_configured";
                };
            };
        };
        /**
         * @description `store_unavailable` — the Gateway's settings store could not be read
         *     or written. Nothing was changed.
         */
        SettingsStoreUnavailable: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "store_unavailable";
                };
            };
        };
        /**
         * @description This deployment has no owner (`GATEWAY_OWNER` is unset), so nobody
         *     can sign in and the whole API is closed. The origin still serves the
         *     Companion, `/health` and `/metrics`, so the app can tell the
         *     operator what is missing — `detail` names the variables.
         */
        SignInNotConfigured: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "sign_in_not_configured";
                };
            };
        };
        /**
         * @description The Gateway's own store (SQLite, its record of truth) failed. Not
         *     the client's fault and not retryable by changing the request.
         */
        StoreFailed: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "store_failed";
                };
            };
        };
        /**
         * @description This deployment reads no suggestions: `GATEWAY_NATS_URL` is unset, so
         *     there is no bus to project. An empty list would claim that no persona
         *     has proposed anything, which is a very different statement from "this
         *     Gateway is not watching"; `detail` names the variable.
         */
        SuggestionsNotConfigured: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "suggestions_not_configured";
                };
            };
        };
        /**
         * @description No device token, or one that is unknown, expired or revoked — one
         *     answer for all three. Also what an unknown `/api` path answers to a
         *     caller with no device token.
         */
        Unauthenticated: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "unauthenticated";
                };
            };
        };
        /**
         * @description `unknown_bridge` — this deployment has no bridge with that id.
         *     `GET /api/bridges` lists the ones it has; a deployment whose
         *     operator has not enabled the bridges compose profile has none.
         */
        UnknownBridge: {
            headers: {
                [name: string]: unknown;
            };
            content: {
                "application/json": components["schemas"]["Error"] & {
                    /** @enum {unknown} */
                    error?: "unknown_bridge";
                };
            };
        };
    };
    parameters: {
        /**
         * @description The bridge instance, as `GET /api/bridges` names it — the
         *     `bridge_id` this deployment declared in its configuration.
         * @example mautrix-whatsapp
         */
        bridgeId: string;
        /**
         * @description The bridge as the **event contract** names it: `^bridge-[a-z0-9-]+$`,
         *     which is what `bridge.status.changed.v1` carries as `data.bridge_id`.
         *     Distinct from the instance id above on purpose — that one names the
         *     software an operator configured, this one is the identity third
         *     parties code against, and it is stable across restarts.
         * @example bridge-whatsapp
         */
        statusBridgeId: string;
        /**
         * @description The CloudEvents id of a `persona.suggest.produced` event: 64
         *     lowercase hex characters, the contract's own id shape. The same
         *     parameter names the suggestion being read (`GET /api/suggestions/{id}`)
         *     and the one whose approval is being asked about.
         * @example 319be8ff15d5dee005c8aa27119b983da8223959987e5dbc639d81e370b5ef9b
         */
        suggestionEventId: string;
    };
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
    pushBridgeStatus: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description The bridge as the **event contract** names it: `^bridge-[a-z0-9-]+$`,
                 *     which is what `bridge.status.changed.v1` carries as `data.bridge_id`.
                 *     Distinct from the instance id above on purpose — that one names the
                 *     software an operator configured, this one is the identity third
                 *     parties code against, and it is stable across restarts.
                 * @example bridge-whatsapp
                 */
                bridge_id: components["parameters"]["statusBridgeId"];
            };
            cookie?: never;
        };
        /**
         * @description A mautrix `BridgeState`. Only `state_event` is required; the rest
         *     is read when present and ignored when not. A `GlobalBridgeState`
         *     wrapper (`remoteState`) is unwrapped.
         */
        requestBody: {
            content: {
                "application/json": components["schemas"]["BridgeStatePush"];
            };
        };
        responses: {
            /**
             * @description The push was verified and applied. No body, and deliberately the
             *     same answer whether or not it changed anything: a bridge has no
             *     use for the difference, and a distinct status would only invite
             *     it to retry on one of them.
             */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /**
             * @description `invalid_request` — the body is not JSON, or not a mautrix
             *     `BridgeState`: it carries no `state_event`, which is the one
             *     thing this endpoint exists to receive.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "invalid_request";
                    };
                };
            };
            /**
             * @description `unauthenticated` — no `Authorization: Bearer` header, one the
             *     Gateway cannot read, or a token that is not this bridge's
             *     `as_token`. One answer for all of them. The bridge retries with
             *     backoff, so an operator whose two halves disagree sees this
             *     again rather than once.
             */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "unauthenticated";
                    };
                };
            };
            /**
             * @description `unknown_bridge` — no configured bridge reports status under this
             *     id. It is `GATEWAY_BRIDGE_<ID>_STATUS_ID`, which defaults to the
             *     instance id with `mautrix-` stripped under a `bridge-` prefix.
             */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "unknown_bridge";
                    };
                };
            };
            /**
             * @description `store_unavailable` — the transition could not be recorded. The
             *     push is not applied, and the bridge's retry is the recovery.
             */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "store_unavailable";
                    };
                };
            };
            /**
             * @description - `sign_in_not_configured` — this deployment has no owner, so it
             *       has no store and no bus either.
             *     - `bridge_status_not_configured` — `GATEWAY_NATS_URL` is unset,
             *       so there is nowhere to record a transition and nowhere to
             *       publish it. Saying so beats accepting a push and throwing it
             *       away.
             *     - `as_token_not_configured` — the bridge is configured, but this
             *       Gateway holds no `as_token` for it, so the push cannot be
             *       verified. An unverified push is refused, never trusted.
             */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "sign_in_not_configured" | "bridge_status_not_configured" | "as_token_not_configured";
                    };
                };
            };
        };
    };
    receiveHermesAnswer: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * @description One of Hermes's outbound-hook deliveries. Only `hook_event_name`,
         *     `timestamp` and `extra` are read; everything else is dropped as the
         *     body is parsed. The hook this endpoint expects is
         *     `transform_llm_output`, which carries the turn's answer and nothing
         *     else — deliberately not `post_llm_call`, which carries the whole
         *     conversation history and would ship Hermes's accumulated memory of
         *     the user back into this process on every message.
         */
        requestBody: {
            content: {
                "application/json": components["schemas"]["HermesAnswerPush"];
            };
        };
        responses: {
            /**
             * @description Either the answer became a suggestion (`status: "published"`, with
             *     the suggestion's CloudEvents id, the language its disclosure was
             *     selected by and the stream position it landed at) or the push was
             *     not a Twalk wake and was ignored (`status: "ignored"`, with the
             *     reason, also counted on `/metrics`).
             */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["HermesAnswerAccepted"];
                };
            };
            /**
             * @description - `invalid_request` — the body is not one of Hermes's
             *       outbound-hook deliveries.
             *     - `hermes_answer_stale` — the push's own timestamp is more than
             *       five minutes from this clock. That timestamp is inside the
             *       signed body and is this wire format's only replay protection,
             *       so it is checked rather than read.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "invalid_request" | "hermes_answer_stale";
                    };
                };
            };
            /**
             * @description `unauthenticated` — no `X-Hermes-Signature-256`, one that cannot
             *     be read, or one this Gateway's secret does not produce over these
             *     bytes. One answer for all of them.
             */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "unauthenticated";
                    };
                };
            };
            /**
             * @description `trigger_not_found` — the message the answer's reference names is
             *     not on the bus, and the read reached the start of the stream. The
             *     same code and the same status `POST /api/approvals` gives for the
             *     same fact: two doors onto one fact must not teach a client two
             *     vocabularies.
             */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "trigger_not_found";
                    };
                };
            };
            /**
             * @description - `consent_revoked` — the sender's consent is `revoked` now,
             *       whatever it was when the message arrived. The answer is
             *       discarded and no suggestion is published.
             *     - `consent_pending` — no decision has ever been recorded about
             *       this sender, so there is nothing to draft a reply to.
             *     - `suggestion_unreadable` — the trigger names a network or a
             *       consent state this build does not know.
             */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "consent_revoked" | "consent_pending" | "suggestion_unreadable";
                    };
                };
            };
            /**
             * @description `trigger_out_of_reach` — the read did not go back far enough to
             *     find the message, which is not the same fact as its not being
             *     there. The bound is `GATEWAY_APPROVAL_LOOKUP_WINDOW`, shared with
             *     the approval path.
             */
            410: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "trigger_out_of_reach";
                    };
                };
            };
            /**
             * @description `push_too_large` — the push is over this endpoint's limit. An
             *     answer is one reply and three short fields, so a body this size
             *     means the Hermes hook is pointed at a fatter event than
             *     `transform_llm_output`.
             */
            413: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "push_too_large";
                    };
                };
            };
            /**
             * @description The push was well formed and correctly signed, and the answer
             *     inside it cannot become a suggestion. Its own status family, so
             *     that an operator can tell "Hermes sent nonsense" from "the model
             *     answered something unusable".
             *
             *     - `hermes_answer_unreadable` — the answer is not the JSON object
             *       the route asks Hermes to write.
             *     - `hermes_answer_has_no_reference` — no `TWALK-REF:` token
             *       anywhere in it, so there is no telling which message it answers,
             *       and guessing would mean drafting into somebody else's
             *       conversation.
             *     - `hermes_answer_has_no_language` — the answer names no language.
             *       Refused, deliberately not defaulted (ADR 0031).
             *     - `hermes_answer_language_unreadable` — it names something that is
             *       not a language tag.
             *     - `hermes_answer_language_unsupported` — it names a language tag
             *       the contract holds no disclosure sentence for (ticket #121). No
             *       suggestion is published; `detail` names the languages that have
             *       one.
             *     - `hermes_answer_is_empty` — the reply is empty.
             *     - `hermes_answer_too_long` — the reply is over 65 335 characters:
             *       the contract's 65 536 less the line the disclosure is appended
             *       on at approval.
             */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "hermes_answer_unreadable" | "hermes_answer_has_no_reference" | "hermes_answer_has_no_language" | "hermes_answer_language_unreadable" | "hermes_answer_language_unsupported" | "hermes_answer_is_empty" | "hermes_answer_too_long";
                    };
                };
            };
            /**
             * @description `store_unavailable` — the sender's consent state could not be
             *     read. Failing closed: an answer whose consent cannot be read
             *     publishes nothing.
             */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "store_unavailable";
                    };
                };
            };
            /**
             * @description `bus_unreachable` — the bus did not answer, so the message could
             *     not be looked up or the suggestion could not be published. A
             *     `502` and not a `503` because the dependency that failed is
             *     behind this Gateway and not this Gateway itself.
             */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "bus_unreachable";
                    };
                };
            };
            /**
             * @description - `sign_in_not_configured` — this deployment has no owner, so it
             *       has no store and no bus either.
             *     - `hermes_answers_not_configured` —
             *       `GATEWAY_HERMES_ANSWER_SECRET` or `GATEWAY_HERMES_DOMAIN` is
             *       unset, or `GATEWAY_NATS_URL` is, so there is no seam to answer
             *       through. Saying so beats accepting an answer and throwing it
             *       away, and it is how "Hermes does not answer" and "Hermes was
             *       never configured" stay two sentences (ADR 0024's rule, applied
             *       to the one component that is not ours).
             */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "sign_in_not_configured" | "hermes_answers_not_configured";
                    };
                };
            };
        };
    };
    readHermesFreeBusy: {
        parameters: {
            query: {
                /** @description The calendar connection to read, as the registry names it. */
                connection: string;
                /** @description The window's start, RFC 3339. */
                from: string;
                /** @description The window's end, RFC 3339, at most fourteen days after `from`. */
                to: string;
            };
            header: {
                /** @description One attempt's name, for the record; a retry keeps it. */
                "X-Hermes-Delivery"?: string;
                /** @description When the read was signed, RFC 3339; inside the signed line. */
                "X-Hermes-Timestamp": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The owner's busy intervals in the window, and nothing else. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["HermesFreeBusy"];
                };
            };
            /**
             * @description - `invalid_request` — no `connection`.
             *     - `invalid_window` — `from` or `to` is missing, is not an RFC
             *       3339 instant, or `to` is not after `from`.
             *     - `window_too_wide` — wider than fourteen days.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "invalid_request" | "invalid_window" | "window_too_wide";
                    };
                };
            };
            /**
             * @description - `unsigned` — no `X-Hermes-Signature-256` or no
             *       `X-Hermes-Timestamp`.
             *     - `bad_signature` — the signature is not this Gateway's secret
             *       over the canonical line, with the query string as sent.
             *     - `stale_timestamp` — the timestamp is more than five minutes
             *       from this clock, or is not an instant.
             */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "unsigned" | "bad_signature" | "stale_timestamp";
                    };
                };
            };
            /**
             * @description `connection_unknown` — `connection` names no calendar connection
             *     of this deployment (`GATEWAY_CONNECTIONS`, kind `calendar`). An
             *     agenda is read on a calendar connection and on nothing else.
             */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "connection_unknown";
                    };
                };
            };
            /**
             * @description `connection_not_connected` — the connection is not `connected`,
             *     or no collector has reported it yet; `state` says which, and
             *     `detail` carries the collector's hint when it gave one. The same
             *     refusal an approval towards that connection gets.
             */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "connection_not_connected";
                        /** @enum {string} */
                        state: "unknown" | "unreachable" | "reconnect_required" | "pending_operator";
                    };
                };
            };
            /**
             * @description - `collector_unreachable` — the collector did not answer, or
             *       answered something that is not a free/busy answer.
             *     - `collector_refused` — the collector refused the read with a
             *       code of its own; `detail` carries it.
             */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "collector_unreachable" | "collector_refused";
                    };
                };
            };
            /**
             * @description - `sign_in_not_configured` — this deployment has no owner, so it
             *       has no store either.
             *     - `hermes_answers_not_configured` —
             *       `GATEWAY_HERMES_ANSWER_SECRET` is unset: there is no seam to
             *       Hermes, so nothing to verify a read with.
             *     - `collector_not_configured` — `GATEWAY_COLLECTOR_URL` or
             *       `GATEWAY_SERVICE_TOKEN` is unset: there is no collector to read
             *       the agenda from.
             *     - `store_unavailable` — the connection's state could not be read.
             */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "sign_in_not_configured" | "hermes_answers_not_configured" | "collector_not_configured" | "store_unavailable";
                    };
                };
            };
        };
    };
    getCompanionFile: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description The request path, standing for any number of segments. */
                companionPath: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /**
             * @description The file the build has for this path, or — for a route that
             *     exists only in the client-side router — the SPA fallback. The
             *     content type is the one the path's extension calls for, and
             *     `text/html` when it has none; `.wasm` is exactly
             *     `application/wasm`, with no parameters, or
             *     `WebAssembly.instantiateStreaming` refuses it. A pre-compressed
             *     sibling (`.br`, `.gz`) is served with the matching
             *     `Content-Encoding` to a client that accepts it.
             */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "*/*": string;
                    "application/wasm": string;
                    "text/html": string;
                };
            };
            /**
             * @description The build has this page under its other trailing-slash spelling;
             *     `Location` names it, with the query string preserved.
             */
            307: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /**
             * @description There is no build to serve at all — not even a fallback file — so
             *     `GATEWAY_STATIC_DIR` points at nothing the Gateway can serve.
             *     A plain-text message naming the directory, for the operator who
             *     will read it in a browser. The origin stays up: `/health` and
             *     `/metrics` keep answering.
             */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "text/plain": string;
                };
            };
        };
    };
    approveSuggestion: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ApprovalRequest"];
            };
        };
        responses: {
            /**
             * @description The reply is on the bus, at the position `stream_sequence`
             *     names. `publication` is `published`.
             */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Approval"];
                };
            };
            /**
             * @description The request is not one approval.
             *
             *     - `malformed_request` - the body is not JSON, a required member
             *       is missing or of the wrong type, `suggestion_event_id` is not
             *       a contract event id, `final.body` is empty (approving an
             *       empty reply sends an empty message, which is never what was
             *       meant), or `final.body` is over 65 335 characters — the
             *       contract's 65 536 less the line the disclosure is appended on,
             *       which `detail` names.
             *     - `approval_is_not_a_batch` - the body is a list, or
             *       `suggestion_event_id` is. An approval names exactly one
             *       suggestion.
             *     - `unknown_value` - `final.format` is not one of the contract's
             *       three content types.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "malformed_request" | "approval_is_not_a_batch" | "unknown_value";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description `approved_by_is_not_the_owner` - the request names an approver
             *     who is not this deployment's owner. Omit the member and the
             *     Gateway stamps it.
             */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "approved_by_is_not_the_owner";
                    };
                };
            };
            /**
             * @description Nothing to approve, and the bus was read to its first retained
             *     message - so this is "it is not there", not "it was not looked
             *     for far enough", which is `410`.
             *
             *     - `suggestion_not_found` - no `persona.suggest.produced` on the
             *       bus has this id.
             *     - `trigger_not_found` - the suggestion exists and the message it
             *       answers does not, so there is no room to reply in.
             */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "suggestion_not_found" | "trigger_not_found";
                    };
                };
            };
            /**
             * @description The suggestion exists and cannot be approved. Seven situations,
             *     seven codes: each one is a different sentence for the user, and
             *     collapsing them would be the defect that cost this project seven
             *     incidents in two days. Six are about the suggestion or the
             *     sender's consent; the seventh, `trigger_has_no_room`, is about
             *     the message it answers.
             *
             *     - `suggestion_expired` - its `expires_at` has passed. A stale
             *       suggestion cannot be approved late (the policy that sets it is
             *       `sdk/python/twalk_sdk/policy.py`). The suggestion stays
             *       visible; ask the persona for a new one.
             *     - `consent_revoked` - the sender's consent **is revoked now**,
             *       whatever it was when the persona wrote the reply. Nothing was
             *       sent.
             *     - `consent_pending` - the sender's consent is pending now:
             *       never decided, or decided "not yet". A different sentence from
             *       a revocation, so a different code.
             *     - `suggestion_was_never_consented` - the message the suggestion
             *       answers was observed with a consent label that was not
             *       `granted`, so the suggestion should never have been produced.
             *       This is the audit fact at observation time, and it is checked
             *       separately from the two above, which are about now.
             *     - `already_approved` - this suggestion was approved before. The
             *       body carries that first approval under `approval`, so a client
             *       whose first answer was lost learns where its reply went
             *       instead of being told to try again. Nothing was sent twice:
             *       the contract's id is deterministic and the bus deduplicates.
             *     - `trigger_has_no_room` - the message the suggestion answers
             *       does not name a portal room, so there is nowhere to send the
             *       reply.
             *     - `connection_not_connected` - the connection the reply would
             *       leave by last said it cannot send (#275): its collector
             *       published `reconnect_required`, `pending_operator` or
             *       `unreachable`, so the reply would sit on the bus for a sender
             *       that will not take it. The detail carries the state and the
             *       operator's own hint. A connection that never said anything
             *       is not refused on this ground.
             *     - `suggestion_unreadable` - the suggestion is on the bus and
             *       this build cannot read it (an unknown network, an unknown
             *       consent state, a content type the contract does not name — or,
             *       sent unedited, a body so long that the disclosure appended
             *       after it would exceed the contract's 65 536, which only a
             *       persona that ignored the SDK's cap can publish; editing the
             *       reply shorter is the way out — or a `disclosure` that is not
             *       one of the contract's own sentences, verbatim, from
             *       `contracts/disclosure/v1/sentences.json`: the schema allows
             *       any string of 1..200, the bus has no authentication, and what
             *       a contact is told is the contract's sentence and never a
             *       persona's wording (ADR 0031), so the reply is neither sent
             *       with that text after it nor sent without a line; `detail`
             *       names the file and the length, not the text). Found and not
             *       understood, which is not "not found".
             */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "suggestion_expired" | "consent_revoked" | "consent_pending" | "suggestion_was_never_consented" | "already_approved" | "trigger_has_no_room" | "connection_not_connected" | "suggestion_unreadable";
                    };
                };
            };
            /**
             * @description The search reached its bound before the stream's first retained
             *     message, so the suggestion may exist further back than this
             *     Gateway reads. Not a `404`: "gone" and "never was" lead a user
             *     to different actions, and a suggestion this old has expired in
             *     any case. `GATEWAY_APPROVAL_LOOKUP_WINDOW` widens the search.
             *
             *     - `suggestion_out_of_reach` - the suggestion itself.
             *     - `trigger_out_of_reach` - the message it answers, searched
             *       backwards from the suggestion's own position.
             */
            410: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "suggestion_out_of_reach" | "trigger_out_of_reach";
                    };
                };
            };
            /**
             * @description - `store_unavailable` - the Gateway's own store could not be
             *       read or written. Nothing was sent.
             *     - `approval_published_but_not_recorded` - the reply **was**
             *       published and the Gateway could not write that down. The reply
             *       has gone out; it will not appear under `GET /api/approvals/{id}`
             *       until the record is repaired by approving again, which
             *       republishes under the same deterministic id and is
             *       deduplicated by the bus. Its own code because the answer to
             *       "did my reply go out?" is yes, and every other `500` here
             *       means no.
             */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "store_unavailable" | "approval_published_but_not_recorded";
                    };
                };
            };
            /**
             * @description `bus_unreachable` - the bus did not answer, so the suggestion
             *     could not be read or the reply could not be published. Nothing
             *     was sent, and this approval can be given again once the bus is
             *     back.
             *
             *     A `502` and not a `503`: this Gateway is configured and
             *     answering, and what failed is the thing behind it. A `503` here
             *     would tell a client that the deployment does not do approvals,
             *     which is a different problem with a different fix.
             */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "bus_unreachable";
                    };
                };
            };
            503: components["responses"]["ApprovalsNotConfigured"];
        };
    };
    getApproval: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description The CloudEvents id of a `persona.suggest.produced` event: 64
                 *     lowercase hex characters, the contract's own id shape. The same
                 *     parameter names the suggestion being read (`GET /api/suggestions/{id}`)
                 *     and the one whose approval is being asked about.
                 * @example 319be8ff15d5dee005c8aa27119b983da8223959987e5dbc639d81e370b5ef9b
                 */
                suggestion_event_id: components["parameters"]["suggestionEventId"];
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The approval this Gateway recorded. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Approval"];
                };
            };
            /**
             * @description `malformed_request` - the path names something that is not a
             *     contract event id (64 lowercase hex characters).
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "malformed_request";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description `approval_not_found` - this Gateway has no record of approving
             *     that suggestion. It was never approved here, and no reply went
             *     out because of it.
             */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "approval_not_found";
                    };
                };
            };
            /**
             * @description `store_unavailable` - the Gateway's own store could not be read,
             *     so whether the reply went out is unknown rather than no.
             */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "store_unavailable";
                    };
                };
            };
            503: components["responses"]["ApprovalsNotConfigured"];
        };
    };
    createOwnerAccount: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateAccountRequest"];
            };
        };
        responses: {
            /**
             * @description The account exists. The body is the Matrix session the Companion
             *     continues in the browser, and nothing else.
             */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CreatedAccount"];
                };
            };
            /**
             * @description The body is not a registration document — a missing or empty
             *     field, or any member the request object does not declare
             *     (`invalid_request`) — or it carried something recovery-key-shaped
             *     (`recovery_key_refused`), which the Gateway refuses to see.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "invalid_request" | "recovery_key_refused";
                    };
                };
            };
            /**
             * @description The username is not this deployment's owner. Refused before the
             *     registration secret is used at all: the relay creates the
             *     owner's account or none.
             */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "not_the_owner";
                    };
                };
            };
            /**
             * @description This deployment's account has already been created. The client's
             *     move is to sign in, not to retry.
             */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "account_already_exists";
                    };
                };
            };
            /**
             * @description The homeserver refused the creation, carrying its own error code
             *     in `matrix_errcode` — a password policy, a username it will not
             *     accept, a registration shared secret it does not share
             *     (`homeserver_refused`) — or could not be reached at all
             *     (`homeserver_unreachable`). An operator's problem in both cases.
             */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["HomeserverRefusal"];
                };
            };
            /**
             * @description Either this deployment relays no registration
             *     (`GATEWAY_REGISTRATION_SHARED_SECRET` unset — its account was
             *     provisioned by its operator: `registration_not_configured`), or
             *     it has no owner at all and the whole API is closed
             *     (`sign_in_not_configured`). `detail` names the variable that
             *     would open it.
             */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "registration_not_configured" | "sign_in_not_configured";
                    };
                };
            };
        };
    };
    inviteSensorIntoRooms: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["InviteSensorRequest"];
            };
        };
        responses: {
            /**
             * @description Every selected room was asked about. Read each outcome: the
             *     request as a whole succeeded even where a room did not.
             */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SensorInvitation"];
                };
            };
            /**
             * @description The body is not an invitation document — a missing or empty
             *     field, or any member the request object does not declare
             *     (`invalid_request`) — or it named more rooms than one request
             *     may carry (`too_many_rooms`), or it carried something
             *     recovery-key-shaped (`recovery_key_refused`), or the homeserver
             *     rejected the **Matrix** access token it carried, in which case
             *     nothing was invited anywhere (`matrix_token_rejected`).
             *
             *     That last one is a `400` and not a `401` deliberately. A `401`
             *     from this origin means the caller's credentials *to this Gateway*
             *     are not good, and a client is entitled to read it as an expired
             *     session and repair it by refreshing — which the Companion's
             *     central handler does. A rejected Matrix token therefore sent it
             *     refreshing a perfectly healthy session, twice per click, and
             *     reporting a session expiry that had not happened (#141). What was
             *     refused is a credential the caller put in the request body, for a
             *     different server: refreshing repairs nothing, and the user's move
             *     is to sign in to Matrix again, not here.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "invalid_request" | "too_many_rooms" | "recovery_key_refused" | "matrix_token_rejected";
                    };
                };
            };
            /** @description No device token, or one that is unknown, expired or revoked. */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "unauthenticated";
                    };
                };
            };
            /**
             * @description The homeserver could not be reached at all, so no room was
             *     asked about.
             */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "homeserver_unreachable";
                    };
                };
            };
            /**
             * @description Either this deployment does not know which Sensor to invite
             *     (`GATEWAY_SENSOR_USER_ID` unset: `sensor_not_configured`), or it
             *     has no owner at all and the whole API is closed
             *     (`sign_in_not_configured`). `detail` names the variable that
             *     would open it.
             */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "sensor_not_configured" | "sign_in_not_configured";
                    };
                };
            };
        };
    };
    getBridges: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Every configured bridge, in configuration order. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BridgeList"];
                };
            };
            401: components["responses"]["Unauthenticated"];
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    getBridgeLogin: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description The bridge instance, as `GET /api/bridges` names it — the
                 *     `bridge_id` this deployment declared in its configuration.
                 * @example mautrix-whatsapp
                 */
                bridge_id: components["parameters"]["bridgeId"];
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The login's current state. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BridgeLogin"];
                };
            };
            401: components["responses"]["Unauthenticated"];
            404: components["responses"]["NoBridgeLogin"];
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    startBridgeLogin: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description The bridge instance, as `GET /api/bridges` names it — the
                 *     `bridge_id` this deployment declared in its configuration.
                 * @example mautrix-whatsapp
                 */
                bridge_id: components["parameters"]["bridgeId"];
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["StartBridgeLoginRequest"];
            };
        };
        responses: {
            /**
             * @description The login is started, and its first step is in the body. Poll
             *     `GET /api/bridges/{bridge_id}/login` from here.
             */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BridgeLogin"];
                };
            };
            /** @description `invalid_request` — the body is not JSON, or names no `flow_id`. */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "invalid_request";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description `too_many_logins` — the network itself will not accept another
             *     login on this account (`FI.MAU.BRIDGE.TOO_MANY_LOGINS`). The user
             *     removes a linked device on the network, or logs one of this
             *     bridge's logins out.
             */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "too_many_logins";
                    };
                };
            };
            /**
             * @description `unknown_bridge` — no such bridge is configured.
             *     `not_found_on_bridge` — the bridge does not know the `login_id`
             *     this request asked to repair.
             */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "unknown_bridge" | "not_found_on_bridge";
                    };
                };
            };
            /**
             * @description `login_in_flight` — a login is already running on this bridge.
             *     `detail` names the device that started it and when, so the user
             *     can tell "my other phone is mid-scan" from "the server is stuck".
             */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "login_in_flight";
                    };
                };
            };
            502: components["responses"]["BridgeUnavailable"];
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    cancelBridgeLogin: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description The bridge instance, as `GET /api/bridges` names it — the
                 *     `bridge_id` this deployment declared in its configuration.
                 * @example mautrix-whatsapp
                 */
                bridge_id: components["parameters"]["bridgeId"];
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The login is cancelled. No body. */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            401: components["responses"]["Unauthenticated"];
            404: components["responses"]["NoBridgeLogin"];
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    getBridgeLoginFlows: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description The bridge instance, as `GET /api/bridges` names it — the
                 *     `bridge_id` this deployment declared in its configuration.
                 * @example mautrix-whatsapp
                 */
                bridge_id: components["parameters"]["bridgeId"];
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The flows, as the bridge describes them. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BridgeLoginFlows"];
                };
            };
            401: components["responses"]["Unauthenticated"];
            404: components["responses"]["UnknownBridge"];
            502: components["responses"]["BridgeUnavailable"];
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    submitBridgeLoginStep: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description The bridge instance, as `GET /api/bridges` names it — the
                 *     `bridge_id` this deployment declared in its configuration.
                 * @example mautrix-whatsapp
                 */
                bridge_id: components["parameters"]["bridgeId"];
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SubmitBridgeLoginStepRequest"];
            };
        };
        responses: {
            /** @description The step was accepted; the body is the login's new state. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BridgeLogin"];
                };
            };
            /**
             * @description `invalid_request` — the body is not JSON, names no `step_id`,
             *     carries no `data` object, names a step this login is not on, or
             *     arrives while the Gateway is holding a blocking step (there is
             *     nothing to submit then).
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "invalid_request";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            404: components["responses"]["NoBridgeLogin"];
            /**
             * @description `step_cancelled` — the step was cancelled on the bridge's side
             *     before this answer arrived (`FI.MAU.LOGIN_STEP_CANCELLED`). Poll
             *     the login and act on the step it reports.
             */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "step_cancelled";
                    };
                };
            };
            /**
             * @description `login_expired` — the login is over: cancelled, timed out, or
             *     already finished (mautrix's three 410s). A login in flight lives
             *     in the bridge's memory, for at most 30 minutes and only while the
             *     network's own code is valid. Start a new one.
             */
            410: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "login_expired";
                    };
                };
            };
            502: components["responses"]["BridgeUnavailable"];
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    getBridgeLogins: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description The bridge instance, as `GET /api/bridges` names it — the
                 *     `bridge_id` this deployment declared in its configuration.
                 * @example mautrix-whatsapp
                 */
                bridge_id: components["parameters"]["bridgeId"];
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The logins the bridge holds. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BridgeLogins"];
                };
            };
            401: components["responses"]["Unauthenticated"];
            404: components["responses"]["UnknownBridge"];
            502: components["responses"]["BridgeUnavailable"];
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    logoutBridgeLogin: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description The bridge instance, as `GET /api/bridges` names it — the
                 *     `bridge_id` this deployment declared in its configuration.
                 * @example mautrix-whatsapp
                 */
                bridge_id: components["parameters"]["bridgeId"];
                /** @description The login to drop, as `GET .../logins` names it. */
                login_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The login is gone. No body. */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description `unknown_bridge` — no such bridge is configured.
             *     `not_found_on_bridge` — the bridge holds no such login.
             */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "unknown_bridge" | "not_found_on_bridge";
                    };
                };
            };
            502: components["responses"]["BridgeUnavailable"];
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    listConnections: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The registry, in the order declared. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": {
                        connections: components["schemas"]["Connection"][];
                        /** @description The most recent connection state changes, newest first; empty when none was ever recorded. */
                        transitions: components["schemas"]["ConnectionTransition"][];
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            /** @description The Gateway's store could not be read. */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "store_unavailable";
                    };
                };
            };
        };
    };
    recordConsentDecision: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ConsentDecisionRequest"];
            };
        };
        responses: {
            /**
             * @description The identical decision — same subject, same state, same
             *     perimeter, same instant — was already in the journal, so
             *     nothing was recorded and `replayed` is `true`. The body is the
             *     decision as it was first recorded, with its original
             *     `event_id`: the contract's ids are deterministic, so a retried
             *     request never becomes a second decision.
             */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecordedConsentDecision"];
                };
            };
            /**
             * @description The decision is recorded, and the event named by `event_id` is
             *     on its way to the bus.
             */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecordedConsentDecision"];
                };
            };
            /**
             * @description The decision is not one the contract allows.
             *
             *     - `malformed_request` — a required member is missing or of the
             *       wrong type, or the body is not JSON.
             *     - `unknown_value` — a value outside the contract's enums: a
             *       `new_state`, a network, a `subject.type`. `unset` is not a
             *       state to move to; it is the absence of a decision.
             *     - `unsupported_subject_type` — a subject type the contract has
             *       that this endpoint does not accept. Nothing is in that state
             *       today; the code stays enumerated so a client that branches on
             *       it keeps working.
             *     - `scope_contradicts_subject` — a `network` subject whose scope
             *       is not exactly its own network, which the contract forbids.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "malformed_request" | "unknown_value" | "unsupported_subject_type" | "scope_contradicts_subject";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description `subject_is_the_owner` — the subject is one of this deployment's
             *     own owner identities, and the owner is never a contact and never
             *     has a consent state, on any event (ADR 0018, ADR 0021). There is
             *     nothing to decide, so nothing is recorded and no
             *     `consent.state.changed` is published.
             *
             *     The identities are the owner's Matrix ID (`GATEWAY_OWNER`) and the
             *     network ghosts the deployment confirmed as theirs
             *     (`GATEWAY_OWNER_IDENTITIES`); `GET /api/consent/snapshot` serves
             *     the list as `owner_identities`.
             *
             *     A status and a code of its own, deliberately. It is not a `400`:
             *     the request is well formed and its subject is a perfectly good
             *     Matrix ID. And it is emphatically not a `404`: "there is no such
             *     subject" and "that subject is you" are two different facts, and a
             *     client that read them as one signal would offer the user the
             *     decision again.
             */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "subject_is_the_owner";
                    };
                };
            };
            500: components["responses"]["ConsentStoreUnavailable"];
            503: components["responses"]["ConsentNotConfigured"];
        };
    };
    getEffectiveConsent: {
        parameters: {
            query: {
                /**
                 * @description The connection the question is about (#270), by its id in
                 *     `GET /api/connections`. One of `connection` and `network` is
                 *     required; `connection` wins when both are sent.
                 */
                connection?: components["schemas"]["ConnectionId"];
                /**
                 * @description The contact's Matrix user ID.
                 * @example @whatsapp_33612345678:example.com
                 */
                contact: string;
                /**
                 * @description The network the question is about, read as its single connection
                 *     on this deployment — refused (`malformed_request`) when the
                 *     network has none or several; send `connection` then.
                 */
                network?: components["schemas"]["Network"];
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The state that applies, and what decided it. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["EffectiveConsent"];
                };
            };
            /**
             * @description `malformed_request` when `contact` or `network` is missing or
             *     empty; `unknown_value` when `network` is not one of the
             *     contract's.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "malformed_request" | "unknown_value";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description `subject_is_the_owner` — `contact` names one of this deployment's
             *     owner identities, which have no consent state to resolve. Never a
             *     `404`: the identity is known, and it is the user's own.
             */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "subject_is_the_owner";
                    };
                };
            };
            500: components["responses"]["ConsentStoreUnavailable"];
            503: components["responses"]["ConsentNotConfigured"];
        };
    };
    getConsentSnapshot: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The whole consent state, and the position it reflects. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConsentSnapshot"];
                };
            };
            /**
             * @description `unauthenticated` — no `Authorization: Bearer` header, one the
             *     Gateway cannot read, or a token that is not this Gateway's
             *     service token. One answer for all of them, so a probe learns
             *     nothing from the difference; a device token is one of the things
             *     refused here.
             */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "unauthenticated";
                    };
                };
            };
            /**
             * @description - `snapshot_too_large` — the consent state holds more entries
             *       than this Gateway serves in one snapshot. The snapshot is
             *       refused whole and `detail` names the cap: a silently truncated
             *       snapshot would tell a consumer that contacts the user granted
             *       were never decided about.
             *     - `store_unavailable` — the consent journal could not be read.
             */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "snapshot_too_large" | "store_unavailable";
                    };
                };
            };
            /**
             * @description - `service_token_not_configured` — `GATEWAY_SERVICE_TOKEN` is
             *       unset, so this Gateway serves no snapshot to anyone. Answered
             *       before authentication, so an unauthenticated caller learns
             *       nothing about the deployment's consent configuration beyond
             *       this.
             *     - `consent_not_configured` — the token is configured and
             *       correct, but `GATEWAY_NATS_URL` is not set, so there is no
             *       consent journal to snapshot.
             */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "service_token_not_configured" | "consent_not_configured";
                    };
                };
            };
        };
    };
    getConsentState: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Every recorded (subject, network) entry. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConsentState"];
                };
            };
            401: components["responses"]["Unauthenticated"];
            500: components["responses"]["ConsentStoreUnavailable"];
            503: components["responses"]["ConsentNotConfigured"];
        };
    };
    getContactDisplayNames: {
        parameters: {
            query: {
                /**
                 * @description A contact's Matrix user ID. Repeat the parameter to ask about
                 *     several, at most 200 per call.
                 * @example [
                 *       "@whatsapp_33612345678:example.com"
                 *     ]
                 */
                contact: string[];
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /**
             * @description One entry per contact asked about, its `display_name` `null` when
             *     the bus no longer carries one for it.
             */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ContactDisplayNames"];
                };
            };
            /**
             * @description `malformed_request` - no `contact` parameter at all, or more than
             *     200 of them.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "malformed_request";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description `bus_unreachable` - the display names could not be read from the
             *     bus. The pending list itself is unaffected: it comes from the
             *     Gateway's own store, and the Companion can show the Matrix IDs.
             */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "bus_unreachable";
                    };
                };
            };
            503: components["responses"]["ContactsNotConfigured"];
        };
    };
    getPendingContacts: {
        parameters: {
            query?: {
                /**
                 * @description Narrows `contacts` to one connection, by its id in
                 *     `GET /api/connections`. The counts are unaffected. Wins over
                 *     `network` when both are sent.
                 */
                connection?: components["schemas"]["ConnectionId"];
                /**
                 * @description Narrows `contacts` to one network — every connection of that
                 *     kind. The counts are unaffected.
                 */
                network?: components["schemas"]["Network"];
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The contacts waiting for a decision, and their counts. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PendingContacts"];
                };
            };
            /**
             * @description `unknown_value` - `network` is not one of the contract's, or
             *     `connection` is not one of the registry's.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "unknown_value";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            /** @description `store_unavailable` - the Gateway's store could not be read. */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "store_unavailable";
                    };
                };
            };
            503: components["responses"]["ContactsNotConfigured"];
        };
    };
    describeDeployment: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The deployment describes itself. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": {
                        /** @description Whether this deployment's one account exists. */
                        bootstrapped: boolean;
                        /**
                         * @description The server name this deployment's owner is on.
                         * @example example.com
                         */
                        homeserver: string;
                    };
                };
            };
            /**
             * @description `sign_in_not_configured` — this deployment has no owner at all;
             *     `store_unreadable`, when the Gateway's store could not be read and
             *     the homeserver could not answer either; or
             *     `homeserver_unreachable`, when the store has no row and the
             *     homeserver could not be asked. A source that cannot answer is never
             *     reported as "no account yet": that would send a returning user back
             *     to the account form, which is the journey this operation exists to
             *     end.
             */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "sign_in_not_configured" | "store_unreadable" | "homeserver_unreachable";
                    };
                };
            };
        };
    };
    listDevices: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The device list, newest activity first is not implied. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DeviceList"];
                };
            };
            401: components["responses"]["Unauthenticated"];
            500: components["responses"]["StoreFailed"];
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    revokeDevice: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description The device's id, as the device list reports it. */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The device is revoked. */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description No device of this deployment has that id, or it was already
             *     revoked. One answer for both: the caller asked for that device
             *     to stop working, and it does not work.
             */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "not_found";
                    };
                };
            };
            500: components["responses"]["StoreFailed"];
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    listPortals: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The register as the homeserver answers about it right now. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PortalRegister"];
                };
            };
            401: components["responses"]["Unauthenticated"];
            503: components["responses"]["PortalsNotConfigured"];
        };
    };
    listPortalMoves: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The most recent moves, newest first. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": {
                        moves: components["schemas"]["PortalMove"][];
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description `portals_not_configured` — as for `GET /api/portals`. Or
             *     `moves_not_journaled` — this Gateway has a register but no store
             *     (no consent configured), so moves are decided and logged but not
             *     kept; `detail` names what to configure.
             */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "portals_not_configured" | "moves_not_journaled";
                    };
                };
            };
        };
    };
    setPortalObservation: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PortalObservationRequest"];
            };
        };
        responses: {
            /**
             * @description Every named room was asked about. Read each outcome: the request
             *     as a whole succeeded even where a room did not.
             */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PortalObservation"];
                };
            };
            /**
             * @description The body is not an observation document, or it named no room at
             *     all, or it named more rooms than one request may carry
             *     (`invalid_request`; `detail` says which).
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "invalid_request";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            503: components["responses"]["PortalsNotConfigured"];
        };
    };
    readRuntimePresence: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The read, as the bus answers about it right now. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RuntimePresence"];
                };
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description `bus_unreachable` - the bus is configured and did not answer, so
             *     whether a runtime is present is unknown. Never `never`: a bus that
             *     cannot be asked and a bus with no runtime on it are two
             *     situations, and the screen must be able to tell them apart.
             */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "bus_unreachable";
                    };
                };
            };
            /**
             * @description `runtime_not_configured` - this Gateway has no bus
             *     (`GATEWAY_NATS_URL`), so there is nothing to read a runtime's
             *     presence from. A refusal rather than `never`: "no runtime has
             *     ever been here" and "this Gateway is not watching the bus" are
             *     different claims, and only the first is about the runtime.
             *     `sign_in_not_configured` when the whole API is closed.
             */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "runtime_not_configured" | "sign_in_not_configured";
                    };
                };
            };
        };
    };
    getSession: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The session this device's token belongs to. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Session"];
                };
            };
            401: components["responses"]["Unauthenticated"];
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    signIn: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SignInRequest"];
            };
        };
        responses: {
            /**
             * @description Signed in. The device token and the refresh token are set as
             *     cookies; the body describes the session.
             */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IssuedSession"];
                };
            };
            /**
             * @description The request body is not a sign-in document
             *     (`invalid_request`), or the OpenID token says it was minted by
             *     another homeserver than this deployment's — refused before any
             *     outbound call (`foreign_homeserver`).
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "invalid_request" | "foreign_homeserver";
                    };
                };
            };
            /**
             * @description The homeserver does not know this token — never minted, expired
             *     or simply wrong (`openid_token_rejected`) — or the Gateway has
             *     already accepted it once (`openid_token_replayed`). Verification
             *     does not consume a token at the homeserver, so the Gateway keeps
             *     its own ledger and a second sign-in with the same token is
             *     refused.
             */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "openid_token_rejected" | "openid_token_replayed";
                    };
                };
            };
            /**
             * @description The token resolved to a Matrix ID that is not this deployment's
             *     owner — including when the homeserver answered with an ID it
             *     cannot speak for. One deployment serves one owner (ADR 0011),
             *     and the homeserver has other accounts, the Sensor's among them.
             */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "not_the_owner";
                    };
                };
            };
            500: components["responses"]["StoreFailed"];
            /**
             * @description The homeserver could not be reached, or answered something that
             *     is not a userinfo document. An operator's problem, not the
             *     user's: the client should offer to retry rather than send the
             *     user back to a sign-in screen.
             */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "homeserver_unverifiable";
                    };
                };
            };
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    signOut: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /**
             * @description Signed out. The device is revoked, and both session cookies are
             *     cleared.
             */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            401: components["responses"]["Unauthenticated"];
            500: components["responses"]["StoreFailed"];
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    refreshSession: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /**
             * @description A new device token and a new refresh token, set as cookies, with
             *     the session document.
             */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IssuedSession"];
                };
            };
            /**
             * @description No refresh cookie, or one that is unknown, expired, already
             *     rotated, or belongs to a revoked device. The client's answer is
             *     to sign in again.
             */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "unauthenticated";
                    };
                };
            };
            503: components["responses"]["SignInNotConfigured"];
        };
    };
    getDisclosureState: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The switch, and the last decision about it if any. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DisclosureState"];
                };
            };
            401: components["responses"]["Unauthenticated"];
            500: components["responses"]["DisclosureStoreUnavailable"];
            503: components["responses"]["DisclosureNotConfigured"];
        };
    };
    putDisclosureState: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["DisclosureUpdate"];
            };
        };
        responses: {
            /** @description The switch as it now stands, with this decision as its record. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DisclosureState"];
                };
            };
            /**
             * @description `malformed_request` — the body is not JSON, is not an object,
             *     is missing `enabled`, has an `enabled` that is not a boolean, a
             *     `reason` that is not a string or is over 1 024 characters, or
             *     carries another member. `detail` names which.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "malformed_request";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            500: components["responses"]["DisclosureStoreUnavailable"];
            503: components["responses"]["DisclosureNotConfigured"];
        };
    };
    getLanguagePreference: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The preference, and the choices. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["LanguagePreference"];
                };
            };
            401: components["responses"]["Unauthenticated"];
            500: components["responses"]["SettingsStoreUnavailable"];
            503: components["responses"]["SettingsNotConfigured"];
        };
    };
    putLanguagePreference: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["LanguagePreferenceRequest"];
            };
        };
        responses: {
            /** @description The preference as it now stands. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["LanguagePreference"];
                };
            };
            /**
             * @description - `malformed_request` — the body is not JSON, is not an object,
             *       is missing `language`, or carries another member.
             *     - `unsupported_language` — not one of the five, and not `null`.
             *       `detail` names the five.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "malformed_request" | "unsupported_language";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            500: components["responses"]["SettingsStoreUnavailable"];
            503: components["responses"]["SettingsNotConfigured"];
        };
    };
    getModelConfiguration: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The model configuration, credential described and not carried. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ModelConfiguration"];
                };
            };
            401: components["responses"]["Unauthenticated"];
            500: components["responses"]["SettingsStoreUnavailable"];
            503: components["responses"]["SettingsNotConfigured"];
        };
    };
    putModelConfiguration: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ModelConfigurationRequest"];
            };
        };
        responses: {
            /**
             * @description The configuration as it now stands — the same document `GET`
             *     answers, the credential still absent from it.
             */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ModelConfiguration"];
                };
            };
            /**
             * @description - `malformed_request` — the body is not JSON, is not an object,
             *       is missing `base_url` or `model`, or carries a member this
             *       endpoint does not have.
             *     - `invalid_base_url` — not an absolute `http`/`https` URL with a
             *       host. It is the chat-completions **base**
             *       (`http://127.0.0.1:4000/v1`), not the completions path.
             *     - `invalid_model` — empty. There is no default model.
             *     - `invalid_params` — not a JSON object.
             *     - `invalid_credential` — not a string and not `null`; an empty
             *       string is refused, because `null` is how a credential is
             *       forgotten.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "malformed_request" | "invalid_base_url" | "invalid_model" | "invalid_params" | "invalid_credential";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            500: components["responses"]["SettingsStoreUnavailable"];
            503: components["responses"]["SettingsNotConfigured"];
        };
    };
    deleteModelConfiguration: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description There is now no model configured. */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            401: components["responses"]["Unauthenticated"];
            500: components["responses"]["SettingsStoreUnavailable"];
            503: components["responses"]["SettingsNotConfigured"];
        };
    };
    probeModelConfiguration: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The endpoint answered a chat completion. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ModelProbe"];
                };
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description `model_not_configured` — there is no endpoint to probe. Not a
             *     `404`: the route exists and the deployment is fine, and this is
             *     the answer that keeps "no endpoint configured at all" distinct
             *     from every way a configured endpoint can fail.
             */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "model_not_configured";
                    };
                };
            };
            500: components["responses"]["SettingsStoreUnavailable"];
            /**
             * @description - `endpoint_unreachable` — nothing answered. `endpoint_status`
             *       is `null`, because there was no answer to have a status.
             *     - `endpoint_refused` — the endpoint answered and refused;
             *       `endpoint_status` carries its status unedited.
             *     - `endpoint_not_compatible` — the endpoint answered a success
             *       that is not an OpenAI chat completion.
             */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "endpoint_unreachable" | "endpoint_refused" | "endpoint_not_compatible";
                    };
                };
            };
            503: components["responses"]["SettingsNotConfigured"];
        };
    };
    getRuntimeSettings: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /**
             * @description The model configuration with the credential in force, and the
             *     language preference.
             */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RuntimeSettings"];
                };
            };
            /**
             * @description `unauthenticated` — no `Authorization: Bearer` header, one the
             *     Gateway cannot read, or a token that is not this Gateway's
             *     service token. One answer for all of them; a device token is one
             *     of the things refused here.
             */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "unauthenticated";
                    };
                };
            };
            500: components["responses"]["SettingsStoreUnavailable"];
            /**
             * @description - `sign_in_not_configured` — this deployment has no owner, so it
             *       keeps no settings and its whole API is closed.
             *     - `service_token_not_configured` — `GATEWAY_SERVICE_TOKEN` is
             *       unset, so this Gateway serves no runtime settings to anyone.
             *       Answered before authentication, as the snapshot's is.
             *     - `settings_not_configured` — there is no settings store.
             */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "sign_in_not_configured" | "service_token_not_configured" | "settings_not_configured";
                    };
                };
            };
        };
    };
    getSuggestions: {
        parameters: {
            query?: {
                /**
                 * @description How many suggestions to answer with, newest first. A screen draws
                 *     a page; reading further back is the deployment's window, not this
                 *     parameter.
                 */
                limit?: number;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The suggestions in the read window, and what the read covered. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SuggestionListing"];
                };
            };
            /**
             * @description `malformed_request` - `limit` is not a whole number between 1 and
             *     200.
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "malformed_request";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description `store_unavailable` - the Gateway's own store could not be read,
             *     so whether a suggestion was already approved is unknown rather
             *     than no. Nothing is answered half-known.
             */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "store_unavailable";
                    };
                };
            };
            /**
             * @description `bus_unreachable` - the suggestions are on the bus and the bus
             *     did not answer. A `502` and not a `503`: this deployment does
             *     read suggestions, and what failed is the thing behind it.
             */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "bus_unreachable";
                    };
                };
            };
            503: components["responses"]["SuggestionsNotConfigured"];
        };
    };
    getSuggestion: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description The CloudEvents id of a `persona.suggest.produced` event: 64
                 *     lowercase hex characters, the contract's own id shape. The same
                 *     parameter names the suggestion being read (`GET /api/suggestions/{id}`)
                 *     and the one whose approval is being asked about.
                 * @example 319be8ff15d5dee005c8aa27119b983da8223959987e5dbc639d81e370b5ef9b
                 */
                suggestion_event_id: components["parameters"]["suggestionEventId"];
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The suggestion, and where it stands. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Suggestion"];
                };
            };
            /**
             * @description `malformed_request` - the path names something that is not a
             *     contract event id (64 lowercase hex characters).
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "malformed_request";
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description `suggestion_not_found` - the whole retained stream was read and
             *     no suggestion has this id. Not the same answer as the one below.
             */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "suggestion_not_found";
                    };
                };
            };
            /**
             * @description `suggestion_unreadable` - the suggestion is on the bus and this
             *     build cannot read it: a network, a consent state or a content
             *     type it does not know. "I found it and do not understand it" is
             *     not "it is not there", so it is neither a `404` nor a silence. In
             *     a listing the same suggestion is counted under `unreadable`
             *     instead, because one bad message must not blank a screen.
             */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "suggestion_unreadable";
                    };
                };
            };
            /**
             * @description `suggestion_out_of_reach` - the read is bounded
             *     (`GATEWAY_APPROVAL_LOOKUP_WINDOW`) and the bound was reached
             *     before the stream's first retained message. The suggestion may
             *     exist, further back than the Gateway looks; `detail` names the
             *     variable that widens the search.
             */
            410: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "suggestion_out_of_reach";
                    };
                };
            };
            /**
             * @description `store_unavailable` - the Gateway's own store could not be read,
             *     so whether this suggestion was already approved is unknown.
             */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "store_unavailable";
                    };
                };
            };
            /**
             * @description `bus_unreachable` - the suggestion is on the bus and the bus did
             *     not answer.
             */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "bus_unreachable";
                    };
                };
            };
            503: components["responses"]["SuggestionsNotConfigured"];
        };
    };
    getAnsweredMessage: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description The CloudEvents id of a `persona.suggest.produced` event: 64
                 *     lowercase hex characters, the contract's own id shape. The same
                 *     parameter names the suggestion being read (`GET /api/suggestions/{id}`)
                 *     and the one whose approval is being asked about.
                 * @example 319be8ff15d5dee005c8aa27119b983da8223959987e5dbc639d81e370b5ef9b
                 */
                suggestion_event_id: components["parameters"]["suggestionEventId"];
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The message, as the contact wrote it. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": {
                        /** @description How many files came with it, never what they are called. */
                        attachments: number;
                        body: string;
                        /**
                         * @description The contact's display name as the event carried it, or
                         *     `null`. Their identifier is not here: the screen names a
                         *     person, it does not address one.
                         */
                        contact: null | string;
                        format: string;
                        /** @description When the network says they wrote it. */
                        received_at: null | string;
                    };
                };
            };
            401: components["responses"]["Unauthenticated"];
            /**
             * @description `suggestion_not_found`, `trigger_not_found` - the whole retained
             *     stream was read and the suggestion, or the message it answers, is
             *     not on it.
             */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "suggestion_not_found" | "trigger_not_found";
                    };
                };
            };
            /**
             * @description `consent_revoked`, `consent_pending` - the contact's consent does
             *     not stand at this moment, so their message is not shown. The same
             *     codes, the same status and the same reason as the approval that
             *     would be refused: this read is closed exactly where the send is.
             *
             *     `suggestion_was_never_consented` - the message arrived under a
             *     label that was not `granted`, so it was never the persona's to
             *     answer and is not the screen's to show.
             */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "consent_revoked" | "consent_pending" | "suggestion_was_never_consented";
                    };
                };
            };
            /**
             * @description `suggestion_out_of_reach`, `trigger_out_of_reach` - the bounded
             *     read gave up first, so it may exist further back than this
             *     Gateway looks. `detail` names the variable that widens it.
             */
            410: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "suggestion_out_of_reach" | "trigger_out_of_reach";
                    };
                };
            };
            /**
             * @description `store_unavailable` - the Gateway's own store could not be read,
             *     so whether this contact's consent stands could not be settled.
             *     Nothing is shown on a store that cannot answer.
             */
            500: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "store_unavailable";
                    };
                };
            };
            /** @description `suggestions_not_configured` - this Gateway watches no bus. */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "suggestions_not_configured";
                    };
                };
            };
        };
    };
    getHealth: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The service is answering. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Health"];
                };
            };
        };
    };
    getMetrics: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The exposition, as a text document. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "text/plain": string;
                };
            };
        };
    };
    getOpenApiDescription: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The OpenAPI 3.1 description of this Gateway. */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/yaml": string;
                };
            };
        };
    };
}
