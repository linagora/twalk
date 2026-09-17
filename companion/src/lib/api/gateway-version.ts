// Generated from companion-gateway/openapi.yaml by scripts/generate-api-client.mjs.
// Do not edit: run `npm run api:generate`. `npm run api:check` fails when
// this file no longer matches the description.

/**
 * The Gateway version this build of the Companion was generated against.
 *
 * `GET /health` reports the Gateway's own version, and the Gateway's
 * conformance test asserts it equals the description's `info.version` — the
 * value below. So a difference between the two at runtime means exactly one
 * thing: this shell outlived a Gateway upgrade, which is what an installed PWA
 * with a service worker makes possible. See `src/lib/version/handshake.ts`.
 */
export const EXPECTED_GATEWAY_VERSION = "0.1.0";
