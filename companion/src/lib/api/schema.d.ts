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
