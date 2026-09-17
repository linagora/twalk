// Generated from companion-gateway/openapi.yaml by scripts/generate-api-client.mjs.
// Do not edit: run `npm run api:generate`. `npm run api:check` fails when
// this file no longer matches the description.

export interface paths {
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
         *     `subject.type` is `contact` or `network` here. A `persona` subject is
         *     refused with `unsupported_subject_type`: activating a persona is a
         *     consent decision on the same write path (ADR 0013), and ticket #60
         *     is what opens it.
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
         *     Ticket #50 adds the snapshot a cold consumer reads: the same state
         *     plus the JetStream sequence it reflects, authenticated by the
         *     Sensor's service token. This endpoint names no stream position and
         *     is not paginated.
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
        ConsentState: {
            /**
             * @description Every recorded entry, ordered by subject type, subject and
             *     network. Not paginated: the snapshot's documented cap is #50's.
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
             *     one network.
             */
            id: string;
            /**
             * @description `contact` for one contact, `network` for a whole network's
             *     default. `persona` exists in the contract and is not writable
             *     here yet (#60).
             * @enum {string}
             */
            type: "contact" | "network";
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
             *     the snapshot's business (#50).
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
    };
    responses: {
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
    };
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
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
             *     recovery-key-shaped (`recovery_key_refused`).
             */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "invalid_request" | "too_many_rooms" | "recovery_key_refused";
                    };
                };
            };
            /**
             * @description No device token, or one that is unknown, expired or revoked
             *     (`unauthenticated`) — or the homeserver rejected the *Matrix*
             *     access token in the body, in which case nothing was invited
             *     anywhere (`matrix_token_rejected`). The second is the user's
             *     Matrix session having expired, not their Gateway session: the
             *     client's move is to log in to Matrix again, not to sign in here.
             */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Error"] & {
                        /** @enum {unknown} */
                        error?: "unauthenticated" | "matrix_token_rejected";
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
             *     - `unsupported_subject_type` — `persona`, which this endpoint
             *       does not accept yet (#60).
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
