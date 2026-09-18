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
         *     yet". The owner's own Matrix ID is never in it either: their messages
         *     travel through the same rooms, and nobody is their own correspondent.
         *
         *     **The numbers.** `total` and `networks` always count the whole list,
         *     whatever `?network=` narrows `contacts` to, so a badge and the list
         *     beside it can never disagree.
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
         *     `bootstrapped` is whether this deployment has its one account. It is
         *     the same fact the registration relay refuses on, so this publishes
         *     nothing a registration attempt would not reveal. `homeserver` is the
         *     server name, which is in the deployment's own DNS.
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
        /** @description One decision the owner asks the Gateway to record. */
        ConsentDecisionRequest: {
            new_state: components["schemas"]["ConsentState_State"];
            /**
             * @description Optional free text kept in the journal and published with the
             *     event, for the audit trail. The user's own words about their own
             *     decision — never message content.
             */
            reason?: string;
            scope: components["schemas"]["ConsentScope"];
            subject: components["schemas"]["ConsentSubject"];
        };
        /** @description The perimeter of a decision. */
        ConsentScope: {
            /**
             * @description The networks the decision applies to. Order does not matter on
             *     the way in: the Gateway sorts it, because the sorted scope is
             *     part of the event's deterministic id.
             */
            networks: components["schemas"]["Network"][];
        };
        /**
         * @description The whole consent state, and the bus position it reflects. The two
         *     are read together, so no decision can be committed and published
         *     between them without appearing in one of the two.
         */
        ConsentSnapshot: {
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
             *     subjects are excluded.
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
        /** @description One (subject, network) of the current state. */
        ConsentStateEntry: {
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
        /** @description The state that applies to one contact on one network. */
        EffectiveConsent: {
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
         * @description Every refusal the Gateway produces. `error` is the stable code a
         *     client branches on; the operations above enumerate which codes each
         *     status carries.
         */
        Error: {
            /**
             * @description A human-readable explanation, for an operator reading logs. Not
             *     for display to the user, and never matched on.
             */
            detail?: string;
            /** @description The machine-readable code. Stable; never a sentence. */
            error: string;
            /**
             * @description The path that matched no endpoint. Set on the `/api` catch-all's
             *     `404` alone.
             */
            path?: string;
        };
        Health: {
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
         * @description A messaging network as the user experiences it, and `matrix` for
         *     native rooms (ADR 0005, ADR 0009). The contract's own `network`
         *     enum, exactly: a bridge id (`gmessages`) is never a network.
         * @enum {string}
         */
        Network: "whatsapp" | "telegram" | "signal" | "discord" | "sms" | "matrix";
        /**
         * @description One contact waiting for a decision, on one network. Four members, and
         *     deliberately no fifth: a body, a display name or a network identifier
         *     would each turn this list into something else.
         */
        PendingContact: {
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
             * @description The contacts themselves, oldest first sighting first - the order
             *     the user met them in, and stable between polls. Narrowed by
             *     `?network=` when one was given.
             */
            contacts: components["schemas"]["PendingContact"][];
            /**
             * @description The same total, broken down per network, in the contract's own
             *     order of network values. A network with nothing waiting is absent
             *     rather than present at zero.
             */
            networks: components["schemas"]["PendingContactCount"][];
            /**
             * @description How many contacts are waiting for a decision in all - the
             *     dashboard's number. Counts the whole list, never only what a
             *     `?network=` filter left in `contacts`.
             */
            total: number;
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
             *     `subject.type:subject.id:new_state:<sorted networks>:occurred_at`.
             *     A consumer can match a bus event to this answer by it.
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
        /** @description One outcome per room asked about, in the order asked. */
        SensorInvitation: {
            rooms: components["schemas"]["RoomInvitation"][];
            /**
             * @description The Matrix ID the Gateway invited (`GATEWAY_SENSOR_USER_ID`), so
             *     the Companion can name it on screen rather than guess it.
             */
            sensor: string | null;
        };
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
    };
    responses: {
        /**
         * @description The bridge itself is the problem, not the request. The three codes
         *     are three different investigations, and telling them apart without
         *     reading `detail` is the point of having three.
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
                    error?: "bridge_unreachable" | "bridge_refused" | "bridge_answer_unusable";
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
            500: components["responses"]["ConsentStoreUnavailable"];
            503: components["responses"]["ConsentNotConfigured"];
        };
    };
    getEffectiveConsent: {
        parameters: {
            query: {
                /**
                 * @description The contact's Matrix user ID.
                 * @example @whatsapp_33612345678:example.com
                 */
                contact: string;
                /** @description The network the question is about. */
                network: components["schemas"]["Network"];
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
                /** @description Narrows `contacts` to one network. The counts are unaffected. */
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
            /** @description `unknown_value` - `network` is not one of the contract's. */
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
             * @description `sign_in_not_configured` — this deployment has no owner at all — or
             *     `store_unreadable`, when the Gateway's store could not be read. A
             *     store that cannot answer is never reported as "no account yet":
             *     that would send a returning user back to the account form, which is
             *     the journey this operation exists to end.
             */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "sign_in_not_configured" | "store_unreadable";
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
