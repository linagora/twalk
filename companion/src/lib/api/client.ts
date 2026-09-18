// The Gateway client. Not a client implementation: `openapi-fetch` is a
// generic runtime, and every path, parameter and response shape it checks
// comes from `schema.d.ts`, which `scripts/generate-api-client.mjs` generates
// from `companion-gateway/openapi.yaml`. Nothing about the Gateway's HTTP
// surface is written down here — a route, a request body or a response member
// that this file appeared to know about would be exactly the drift the ticket
// forbids.
//
// Two configuration facts:
//
//   - `baseUrl: ''`. The Gateway serves the Companion's own files, so its API
//     is same-origin and every request is relative. That is also what makes
//     the device token an `HttpOnly` cookie (ADR 0011): there is no CORS to
//     negotiate and no `Authorization` header to build.
//
//   - `credentials: 'same-origin'`. The browser attaches `twalk_device` (and,
//     on `/api/session`, `twalk_refresh`) itself. `HttpOnly` hides both from
//     JavaScript, so this code must never try to read them.
//
// # The 401, handled once and here (#111)
//
// The device token lives fifteen minutes. Before this middleware, a token that
// died while the user was reading a screen turned every subsequent call into a
// `401` that each screen interpreted on its own — which mostly meant not at
// all: the Signal screen spun on "Asking for a code…" for ever, and the owner
// reported a broken bridge that had never been contacted.
//
// So the rule the ticket asks for lives here and nowhere else. Any `401` from
// any call is answered once, centrally:
//
//   1. refresh the session (`$lib/session/refresh.ts`, one refresh at a time
//      however many calls failed together);
//   2. replay the original request — the browser now holds the rotated cookie,
//      so the replay is the same request with a live credential;
//   3. and only if the refresh was refused, let the `401` through with the
//      session marked *expired*, which is what puts the sign-in-again state on
//      screen.
//
// A screen therefore never implements this, and must not: what reaches a
// caller is either the successful answer or a failure that is genuinely
// terminal.
//
// Three refusals are deliberately left alone. `POST /api/session` is the
// sign-in itself, where a `401` means the homeserver rejected the OpenID token
// — refreshing a session that does not exist would turn a wrong answer into a
// confusing one. `GET /api/session` on a browser that is *known* to be signed
// out is the ordinary first question of screen 1 (`$lib/onboarding/deployment.ts`
// reads that `401` as "this deployment accepts a new account"), so once the
// keeper has established there is no session, a `401` is simply the truth. And
// the refresh route itself never travels through this client at all.
//
// One rule for the screens that use this client, written here because there is
// nowhere better: branch on a refusal's `error` code, which the description
// guarantees is stable, and never on its `detail`, which is an operator's
// sentence and is not for display.

import createClient, { type Middleware } from 'openapi-fetch';

import { refreshSession } from '$lib/session/refresh';
import { sessionStatus } from '$lib/session/state';
import type { paths } from './schema';

/** Sign-in is not a session that can be refreshed; its `401` is its own answer. */
const NOT_REFRESHABLE = new Set(['/api/session/refresh']);

/**
 * The request as it was first sent, kept until its answer comes back so the
 * replay is byte-for-byte the same call. A `Request` body can be read once, so
 * what is kept is a clone taken before the original was consumed.
 */
const originals = new Map<string, Request>();

/**
 * Whether a `401` is about this origin's own session.
 *
 * Read from a clone, so the caller still gets an unread body. A `401` with no
 * readable error code is treated as an expiry, which is the conservative
 * reading: refreshing a session that did not need it costs one request.
 */
async function looksUnauthenticated(response: Response): Promise<boolean> {
	try {
		const body: unknown = await response.clone().json();
		const code = (body as { error?: unknown })?.error;
		return typeof code !== 'string' || code === 'unauthenticated';
	} catch {
		return true;
	}
}

const repairUnauthenticated: Middleware = {
	onRequest({ request, id }) {
		originals.set(id, request.clone());
	},

	async onResponse({ request, response, id, schemaPath, options }) {
		const original = originals.get(id);
		originals.delete(id);

		if (response.status !== 401 || NOT_REFRESHABLE.has(schemaPath)) {
			return;
		}
		// The sign-in's own refusal, and every refusal met by a browser already
		// known to hold no session: neither is an expiry, and neither is
		// repaired by a refresh.
		if (schemaPath === '/api/session' && request.method === 'POST') {
			return;
		}
		if (sessionStatus().kind === 'signed-out') {
			return;
		}
		// A `401` whose error code says something other than "your session is
		// not good" is not this middleware's business. The Gateway used to
		// answer `401 matrix_token_rejected` when a *homeserver* refused a
		// Matrix token carried in a request body — a credential for another
		// server entirely — and this handler dutifully refreshed a healthy
		// session, replayed, failed again, and reported an expiry that had not
		// happened (#141). That status is a `400` now, and this guard is here
		// so the next endpoint to make the same mistake costs a confusing
		// message rather than a loop.
		if (!(await looksUnauthenticated(response))) {
			return;
		}
		if (original === undefined || !(await refreshSession())) {
			return;
		}

		// The Gateway rotated both cookies on the way through; the browser
		// attaches the new device token to the replay by itself.
		const send = options.fetch ?? globalThis.fetch.bind(globalThis);
		try {
			return await send(original);
		} catch {
			// The replay did not reach the Gateway. The original refusal is
			// still the honest answer, and the caller handles it.
			return;
		}
	},

	onError({ id }) {
		originals.delete(id);
	}
};

export const gateway = createClient<paths>({
	baseUrl: '',
	credentials: 'same-origin'
});

gateway.use(repairUnauthenticated);
