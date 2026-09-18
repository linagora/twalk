// What the Companion believes about its own Gateway session, in one place.
//
// The device token is an `HttpOnly` cookie (ADR 0011), so this app cannot read
// it, cannot see when it expires, and cannot tell a browser that never signed
// in from one whose credential died an hour ago. Everything it knows, it knows
// because the Gateway said so — a sign-in, a refresh, or a `GET /api/session`
// — and that is what this store holds.
//
// The distinction the whole ticket turns on is between **signed out** and
// **expired**:
//
//   - *signed out* is a browser with no session and no memory of one. A `401`
//     is then the plain truth, screen 1 is the right screen, and shouting
//     "your session expired" at a first-time visitor would be a lie.
//   - *expired* is a browser that held a live session in this page's lifetime
//     and no longer does. That is the state #111 exists for: it must be
//     visible, it must offer the way back, and it must never look like
//     loading.
//
// The transition between them is `afterUnauthenticated`, a pure function so
// the rule can be tested rather than inferred from a component.

import { writable, type Readable } from 'svelte/store';

import type { components } from '$lib/api/schema';

/** What the Gateway answers with when it issues or rotates a pair of tokens. */
export type IssuedSession = components['schemas']['IssuedSession'];
/** The same document without a lifetime: `GET /api/session` reports no expiry. */
export type Session = components['schemas']['Session'];

export type SessionStatus =
	/** Nothing has been asked of the Gateway yet. */
	| { kind: 'unknown' }
	/** The Gateway says nobody is signed in on this browser. */
	| { kind: 'signed-out' }
	/**
	 * A session the Gateway confirmed. `expiresAt` is epoch milliseconds when
	 * the answer carried `expires_in`, and `null` when it did not —
	 * `GET /api/session` describes a session without dating it, so a browser
	 * that only ever asked that question knows it is signed in and not for how
	 * much longer.
	 */
	| { kind: 'live'; owner: string; homeserver: string; deviceId: string; expiresAt: number | null }
	/**
	 * Both credentials are gone: the device token was refused and the refresh
	 * token could not replace it. The owner is remembered so the way back can
	 * be offered with the username already filled in.
	 */
	| { kind: 'expired'; owner: string | null; homeserver: string | null };

const state = writable<SessionStatus>({ kind: 'unknown' });

/** The session, for any screen that wants to render it. Read-only by design. */
export const session: Readable<SessionStatus> = { subscribe: state.subscribe };

let current: SessionStatus = { kind: 'unknown' };
state.subscribe((next) => (current = next));

/** The status without subscribing — for the client wrapper, which is not a component. */
export function sessionStatus(): SessionStatus {
	return current;
}

/**
 * The Gateway confirmed a session. `expiresIn` is seconds, when the answer
 * carried it.
 */
export function noteLive(
	document: Session | IssuedSession,
	at: number = Date.now()
): void {
	const expiresIn = 'expires_in' in document ? document.expires_in : null;
	state.set({
		kind: 'live',
		owner: document.owner,
		homeserver: document.homeserver,
		deviceId: document.device.id,
		expiresAt: expiresIn === null ? null : at + expiresIn * 1000
	});
}

/**
 * A `401` that survived a refresh attempt. Whether that is an expiry or simply
 * a browser with no session is decided by what we knew a moment ago.
 */
export function noteUnauthenticated(): void {
	state.update(afterUnauthenticated);
}

/** The Companion gave the session up itself (signing out). */
export function noteSignedOut(): void {
	state.set({ kind: 'signed-out' });
}

/**
 * The rule, as a pure function.
 *
 * A live session that is refused has expired. A browser that had no session,
 * or was already told its session expired, learns nothing new from another
 * refusal — and in particular a first-time visitor on screen 1, whose
 * `GET /api/session` answers `401` by design, is *signed out*, not expired.
 */
export function afterUnauthenticated(current: SessionStatus): SessionStatus {
	switch (current.kind) {
		case 'live':
			return { kind: 'expired', owner: current.owner, homeserver: current.homeserver };
		case 'expired':
			return current;
		default:
			return { kind: 'signed-out' };
	}
}

/** Test seam: puts the store back where a fresh page load would find it. */
export function resetSession(): void {
	state.set({ kind: 'unknown' });
}
