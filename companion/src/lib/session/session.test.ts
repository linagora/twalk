// The two decisions in the session keeper that are logic rather than plumbing:
// when to renew a token, and what a `401` means.
//
// Everything else about #111 — the refresh itself, the central retry, the
// session-expired state — is a browser talking to a real Gateway, and lives in
// `tests/e2e/session/session.spec.ts`, as spec #65 requires.

import { describe, expect, it } from 'vitest';

import { refreshAfterSeconds } from './refresh';
import { afterUnauthenticated, type SessionStatus } from './state';

describe('refreshAfterSeconds', () => {
	it('renews the default fifteen-minute token with a minute in hand', () => {
		// `GATEWAY_DEVICE_TOKEN_TTL` defaults to 900 seconds, and the ticket is
		// explicit that this is correct and must not be raised to hide the bug.
		expect(refreshAfterSeconds(900)).toBe(840);
	});

	it('keeps a fifth of a shorter lifetime in hand', () => {
		// The cap only binds once a fifth of the lifetime exceeds a minute.
		expect(refreshAfterSeconds(100)).toBe(80);
		expect(refreshAfterSeconds(10)).toBe(8);
	});

	it('never schedules sooner than a second away', () => {
		// A deployment that configured an absurd lifetime must not turn the
		// browser into a refresh loop.
		expect(refreshAfterSeconds(1)).toBe(1);
		expect(refreshAfterSeconds(0)).toBe(1);
		expect(refreshAfterSeconds(-5)).toBe(1);
		expect(refreshAfterSeconds(Number.NaN)).toBe(1);
	});

	it('always leaves the token alive at the moment it renews', () => {
		for (const lifetime of [2, 5, 30, 120, 900, 3600, 86_400]) {
			expect(refreshAfterSeconds(lifetime)).toBeLessThan(lifetime);
		}
	});
});

describe('what a refused request means', () => {
	const live: SessionStatus = {
		kind: 'live',
		owner: '@you:example.com',
		homeserver: 'example.com',
		deviceId: 'device-1',
		expiresAt: 1_000
	};

	it('turns a live session into an expired one, remembering who it was', () => {
		// The owner is what lets the way back offer a filled-in username
		// instead of sending the user to the first screen.
		expect(afterUnauthenticated(live)).toEqual({
			kind: 'expired',
			owner: '@you:example.com',
			homeserver: 'example.com'
		});
	});

	it('leaves a browser that never had a session merely signed out', () => {
		// Screen 1 asks `GET /api/session` of every first-time visitor and is
		// answered `401` by design. Shouting "your session expired" at someone
		// who has never signed in would be a lie, and would cover the screen
		// they are supposed to be reading.
		expect(afterUnauthenticated({ kind: 'unknown' })).toEqual({ kind: 'signed-out' });
		expect(afterUnauthenticated({ kind: 'signed-out' })).toEqual({ kind: 'signed-out' });
	});

	it('does not downgrade an expiry it has already announced', () => {
		const expired: SessionStatus = {
			kind: 'expired',
			owner: '@you:example.com',
			homeserver: 'example.com'
		};
		// Several calls fail together — the dashboard makes five at once — and
		// the second refusal must not replace the dialog with a blank screen.
		expect(afterUnauthenticated(expired)).toBe(expired);
	});
});
