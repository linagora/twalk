// Keeping the Gateway session alive, which the Companion never did (#111).
//
// The device token lives fifteen minutes by design — it is the credential that
// travels on every request, and #52 kept it short on purpose. The other half of
// that decision is `POST /api/session/refresh`, and until this module nothing
// in `companion/src/` called it: every session died a quarter of an hour after
// sign-in, on whatever screen the user happened to be using.
//
// # Braces and belt
//
// Two mechanisms, and both are needed:
//
//   - **the scheduled refresh** (here). `expires_in` comes back with every
//     sign-in and every refresh, so the client knows exactly when its
//     credential dies and renews it with a fifth of the lifetime still in
//     hand. A user working continuously is never interrupted, and never sees a
//     failure at all.
//   - **the central `401` retry** (`$lib/api/client.ts`). A timer is a promise
//     a browser does not keep: a backgrounded tab, a laptop closed over lunch,
//     a phone that slept — the page comes back with a dead token and a timer
//     that fired late or not at all. So every `401` is caught once, centrally,
//     and repaired by the same refresh this module owns.
//
// A woken tab is handled from both ends: `visibilitychange`, `pageshow`,
// `focus` and `online` all ask whether the refresh is overdue and run it
// immediately if it is, so the recovery usually happens before the user's
// first click rather than after their first failure.
//
// # One refresh at a time, always
//
// The refresh **rotates both tokens** and invalidates the previous refresh
// token immediately (`companion-gateway/openapi.yaml`). Two refreshes racing
// would therefore destroy each other's credential — and the dashboard fires
// five reads in one `Promise.all`, so five simultaneous `401`s is the ordinary
// case, not the exotic one. Every caller here joins the one in flight.
//
// # Why a second generated client
//
// `direct` is the same generated client as `$lib/api/client.ts`, without the
// `401` middleware: the wrapper's repair runs *through* this module, so a
// refresh that went back through the wrapper would answer its own `401` with
// another refresh, for ever. Nothing about the Gateway's HTTP surface is
// written down here either — the path and the response shape come from
// `schema.d.ts`, generated from `companion-gateway/openapi.yaml`.

import createClient from 'openapi-fetch';

import type { paths } from '$lib/api/schema';
import { noteLive, noteUnauthenticated } from './state';

const direct = createClient<paths>({ baseUrl: '', credentials: 'same-origin' });

/**
 * How long to wait before renewing a token that lives `expiresIn` seconds.
 *
 * A fifth of the lifetime in hand, capped at a minute — so the default
 * fifteen-minute token is renewed after twelve minutes, and a short one used
 * by a test is still renewed before it dies. Never sooner than a second from
 * now, because a pathologically short lifetime must not turn into a spin.
 */
export function refreshAfterSeconds(expiresIn: number): number {
	if (!Number.isFinite(expiresIn) || expiresIn <= 0) {
		return 1;
	}
	const margin = Math.max(1, Math.min(60, expiresIn * 0.2));
	return Math.max(1, expiresIn - margin);
}

/** After a refresh that failed for a reason that is not a refusal. */
const RETRY_AFTER_MS = 30_000;

let inFlight: Promise<boolean> | null = null;
let timer: ReturnType<typeof setTimeout> | null = null;
/** Epoch milliseconds the scheduled refresh is due at, or `null` when none is. */
let dueAt: number | null = null;
let listening = false;

/**
 * Refreshes the session, joining a refresh already in flight.
 *
 * `true` when the Gateway issued a new pair. `false` on a refusal — in which
 * case the session state has already moved to *expired* or *signed out* — and
 * on a network failure, which is not an expiry and leaves the state alone.
 */
export function refreshSession(): Promise<boolean> {
	inFlight ??= run();
	return inFlight;
}

async function run(): Promise<boolean> {
	try {
		const answer = await direct.POST('/api/session/refresh', {});
		if (answer.data !== undefined) {
			noteLive(answer.data);
			schedule(refreshAfterSeconds(answer.data.expires_in) * 1000);
			return true;
		}
		if (answer.response.status === 401) {
			// The refresh token is gone too: this session cannot be repaired
			// without the user. `503 sign_in_not_configured` is deliberately
			// not this — that is an operator's problem, not an expiry.
			clearSchedule();
			noteUnauthenticated();
			return false;
		}
		schedule(RETRY_AFTER_MS);
		return false;
	} catch {
		// The Gateway did not answer at all. A dropped request is not an
		// expired session, so nothing is claimed about the session here.
		schedule(RETRY_AFTER_MS);
		return false;
	} finally {
		inFlight = null;
	}
}

function schedule(afterMs: number): void {
	clearSchedule();
	dueAt = Date.now() + afterMs;
	timer = setTimeout(() => {
		timer = null;
		void refreshSession();
	}, afterMs);
}

function clearSchedule(): void {
	if (timer !== null) {
		clearTimeout(timer);
		timer = null;
	}
	dueAt = null;
}

/** Whether the scheduled refresh should already have happened. */
export function refreshOverdue(now: number = Date.now()): boolean {
	return dueAt !== null && now >= dueAt;
}

/**
 * Runs the refresh if its moment has passed. What a tab does when it wakes:
 * a timer that was throttled or suspended has not fired, and the token may
 * already be dead.
 */
export function refreshIfOverdue(): void {
	if (refreshOverdue()) {
		void refreshSession();
	}
}

/**
 * Adopts whatever session this browser already holds, and keeps it alive.
 *
 * Called once from the boot sequence. The first move is a refresh rather than
 * a `GET /api/session`, because a refresh is the **only** way a browser can
 * learn its token's lifetime: the token is an `HttpOnly` cookie, the session
 * document carries no expiry, and `expires_in` comes back with a rotation and
 * nowhere else. It also means a page that has been sitting in a background tab
 * since yesterday starts its day with a fresh credential.
 */
export async function startSessionKeeper(): Promise<void> {
	listen();
	if (await refreshSession()) {
		return;
	}
	// No refresh cookie, or one the Gateway will not honour. There may still
	// be a live device token — the refresh cookie is scoped to `/api/session`
	// and a browser can lose it on its own — so ask before concluding
	// anything. This is also the call that tells a first-time visitor's
	// screen 1 that nobody is signed in.
	try {
		const answer = await direct.GET('/api/session');
		if (answer.data !== undefined) {
			noteLive(answer.data);
			return;
		}
		if (answer.response.status === 401) {
			noteUnauthenticated();
		}
	} catch {
		// Unreachable Gateway: the boot handshake and each screen say so in
		// their own words, and claiming an expiry here would be a guess.
	}
}

/**
 * The session was just issued or repaired by something other than this module
 * — a sign-in, or the sign-in offered by the session-expired state.
 */
export function sessionIssued(issued: { expires_in: number }): void {
	schedule(refreshAfterSeconds(issued.expires_in) * 1000);
}

/** Browser-only, idempotent: the events that mean "this tab may have slept". */
function listen(): void {
	if (listening || typeof document === 'undefined' || typeof window === 'undefined') {
		return;
	}
	listening = true;
	document.addEventListener('visibilitychange', () => {
		if (document.visibilityState === 'visible') {
			refreshIfOverdue();
		}
	});
	window.addEventListener('pageshow', () => refreshIfOverdue());
	window.addEventListener('focus', () => refreshIfOverdue());
	window.addEventListener('online', () => refreshIfOverdue());
}
