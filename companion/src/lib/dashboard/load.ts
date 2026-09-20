// What screen 5 reads, in one call, so the component has one `await` and one
// shape to render.
//
// Four reads, all of them the owner's own, all behind the device cookie:
//
//   GET /api/session   who is signed in, and on which device
//   GET /api/bridges   configuration plus the login each bridge holds
//   GET /api/consent/state   every recorded decision, personas included
//   GET /api/devices   every device, revoked ones dated
//   GET /api/contacts/pending   how many contacts are waiting for a decision
//   GET /api/suggestions   how many replies are waiting for approval
//
// **The last one is read for its numbers and for nothing else.** Its answer
// carries a `contacts` array of Matrix user IDs — the list of who has written
// to this user and not yet been decided about, which is the most sensitive
// document this origin serves. Screen 5 is a home screen unlocked in public,
// so it takes `total` and the per-network counts and drops the array here
// (#74), at the seam, rather than carrying it into a component where a later
// row could render it. The list belongs to the consent inbox, which is v0.2.
//
// **And the last one on the same terms** (#100). `GET /api/suggestions`
// answers with every proposed reply *in full*, because the approval screen has
// to draw them. The dashboard may not: the home screen is what gets unlocked
// on a train, and an approval queue shows the proposed text. So the answer is
// reduced to a count here, at the seam, by `$lib/approvals/summary.ts` — the
// same shape as `pending.contacts` above, for the same reason, and the reason
// the ticket's "grep the rendered dashboard for the suggestion's text and find
// nothing" holds by construction rather than by a component's restraint.
//
// It is also the first read on this poll that scans the bus rather than a
// store: the Gateway's suggestion listing is a projection of the stream and
// keeps no copy. It is read on the same fifteen-second tick as the rest
// because a second timer would be a second thing to reason about, and the
// window it scans is the deployment's own.
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
import { dismissed } from '$lib/approvals/dismissed';
import { summarise, type Waiting } from '$lib/approvals/summary';

/**
 * How many contacts are waiting for a decision, and on which networks. The
 * numbers from `GET /api/contacts/pending`, with the identities left behind.
 */
export interface PendingSummary {
	total: number;
	/** Per connection (#272): what a screen that decides per connection reads. */
	connections: components['schemas']['PendingConnectionCount'][];
	networks: components['schemas']['PendingContactCount'][];
}

export type Snapshot = {
	session: components['schemas']['Session'] | null;
	/** The registry of connections (ADR 0033), `null` when the Gateway did not answer. */
	connections: components['schemas']['Connection'][] | null;
	bridges: components['schemas']['ConfiguredBridge'][] | null;
	consent: components['schemas']['ConsentStateEntry'][] | null;
	devices: components['schemas']['Device'][] | null;
	/**
	 * The register's journal of moves (#255): conversations whose room was
	 * replaced while the Sensor was in them, and what the register decided.
	 * `null` when the deployment keeps no journal.
	 */
	moves: components['schemas']['PortalMove'][] | null;
	/** `null` when the deployment does not project the inbound stream. */
	pending: PendingSummary | null;
	/**
	 * How many replies are waiting for approval, and nothing about what they
	 * say. `null` when this deployment reads no suggestions, which is a
	 * different statement from "none are waiting" and is rendered as neither.
	 */
	waiting: Waiting | null;
	/** False when nothing answered at all. */
	reachable: boolean;
};

export const EMPTY: Snapshot = {
	session: null,
	connections: null,
	bridges: null,
	consent: null,
	devices: null,
	moves: null,
	pending: null,
	waiting: null,
	reachable: true
};

export async function loadDashboard(): Promise<Snapshot> {
	const [session, connections, bridges, consent, devices, pending, suggestions, moves] =
		await Promise.all([
		ask(() => gateway.GET('/api/session')),
		ask(() => gateway.GET('/api/connections')),
		ask(() => gateway.GET('/api/bridges')),
		ask(() => gateway.GET('/api/consent/state')),
		ask(() => gateway.GET('/api/devices')),
		ask(() => gateway.GET('/api/contacts/pending')),
		ask(() => gateway.GET('/api/suggestions')),
		ask(() => gateway.GET('/api/portals/moves'))
	]);
	return {
		session: session,
		connections: connections?.connections ?? null,
		bridges: bridges?.bridges ?? null,
		consent: consent?.entries ?? null,
		devices: devices?.devices ?? null,
		moves: moves?.moves ?? null,
		// `pending.contacts` is deliberately not carried past this line. See
		// the module note: the counts are the dashboard's, the list is not.
		pending:
			pending === null
				? null
				: { total: pending.total, connections: pending.connections, networks: pending.networks },
		// Same line, same reason: what a persona wrote does not travel past
		// here. `summarise` returns two numbers and has nowhere to put a text.
		waiting: suggestions === null ? null : summarise(suggestions, dismissed()),
		reachable:
			session !== null ||
			connections !== null ||
			bridges !== null ||
			consent !== null ||
			devices !== null ||
			pending !== null ||
			suggestions !== null
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
