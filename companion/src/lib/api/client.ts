// The Gateway client. Not a client implementation: `openapi-fetch` is a
// generic runtime, and every path, parameter and response shape it checks
// comes from `schema.d.ts`, which `scripts/generate-api-client.mjs` generates
// from `companion-gateway/openapi.yaml`. Nothing about the Gateway's HTTP
// surface is written down here — a route, a request body or a response member
// that this file appeared to know about would be exactly the drift the ticket
// forbids.
//
// Two configuration facts, and they are the whole file:
//
//   - `baseUrl: ''`. The Gateway serves the Companion's own files, so its API
//     is same-origin and every request is relative. That is also what makes
//     the device token an `HttpOnly` cookie (ADR 0011): there is no CORS to
//     negotiate and no `Authorization` header to build.
//
//   - `credentials: 'same-origin'`. The browser attaches `twalk_device` (and,
//     on `/api/session`, `twalk_refresh`) itself. `HttpOnly` hides both from
//     JavaScript, so this code must never try to read them.

import createClient from 'openapi-fetch';

import type { paths } from './schema';

export const gateway = createClient<paths>({
	baseUrl: '',
	credentials: 'same-origin'
});

/**
 * The stable error code the Gateway puts in every refusal, or `null` when the
 * body is not one of its error documents.
 *
 * The description is explicit that `error` is what a client branches on and
 * `detail` is for an operator's logs — never displayed, never matched on. This
 * is the one place that reads it, so that rule has somewhere to live.
 */
export function errorCode(body: unknown): string | null {
	if (typeof body !== 'object' || body === null) {
		return null;
	}
	const code = (body as { error?: unknown }).error;
	return typeof code === 'string' ? code : null;
}
