// What screen 5 reads, in one call, so the component has one `await` and one
// shape to render.
//
// Four reads, all of them the owner's own, all behind the device cookie:
//
//   GET /api/session   who is signed in, and on which device
//   GET /api/bridges   configuration plus the login each bridge holds
//   GET /api/consent/state   every recorded decision, personas included
//   GET /api/devices   every device, revoked ones dated
//
// There is no `GET /api/events/stream`. The wireframe's screen 5 subscribes to
// a merged Server-Sent Events stream and renders "without page refresh"; the
// Gateway describes no such endpoint, so this screen polls and offers an
// explicit refresh. Inventing a stream here would mean asserting an API that
// does not exist. #56 gave that stream something live to carry —
// `bridge.status.changed.v1` is published now — and added no read for it, so
// the stream is still the missing half.
//
// One failed read does not empty the screen. Each document keeps its own
// `null` for "not answered", because a Gateway that answered three of four
// questions has told the user three true things.

import { gateway } from '$lib/api/client';
import type { components } from '$lib/api/schema';

export type Snapshot = {
	session: components['schemas']['Session'] | null;
	bridges: components['schemas']['ConfiguredBridge'][] | null;
	consent: components['schemas']['ConsentStateEntry'][] | null;
	devices: components['schemas']['Device'][] | null;
	/** False when nothing answered at all. */
	reachable: boolean;
};

export const EMPTY: Snapshot = {
	session: null,
	bridges: null,
	consent: null,
	devices: null,
	reachable: true
};

export async function loadDashboard(): Promise<Snapshot> {
	const [session, bridges, consent, devices] = await Promise.all([
		ask(() => gateway.GET('/api/session')),
		ask(() => gateway.GET('/api/bridges')),
		ask(() => gateway.GET('/api/consent/state')),
		ask(() => gateway.GET('/api/devices'))
	]);
	return {
		session: session,
		bridges: bridges?.bridges ?? null,
		consent: consent?.entries ?? null,
		devices: devices?.devices ?? null,
		reachable: session !== null || bridges !== null || consent !== null || devices !== null
	};
}

async function ask<T>(call: () => Promise<{ data?: T }>): Promise<T | null> {
	try {
		return (await call()).data ?? null;
	} catch {
		return null;
	}
}

/** Revokes one device. Returns the Gateway's stable error code, or `null`. */
export async function revokeDevice(id: string): Promise<string | null> {
	try {
		const answer = await gateway.DELETE('/api/devices/{id}', { params: { path: { id } } });
		if (answer.response.ok) {
			return null;
		}
		return (answer.error as { error?: string } | undefined)?.error ?? 'unknown';
	} catch {
		return 'unreachable';
	}
}
