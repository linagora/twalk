// What a tick costs, stated before it takes effect (ticket #143).
//
// # Why this is a module and not a line in a component
//
// The acceptance criterion this file exists for is the one that is not
// ergonomics: *ticking a community must make plain how many people it covers,
// before the tick takes effect. A number is not a warning, and a warning
// nobody reads is not a decision.*
//
// Those people do not know Twalk exists (#122). The eighteen portal rooms one
// personal WhatsApp account produced held roughly 1,300 memberships, and
// observing a conversation publishes what the people in it wrote. So the
// count is computed from the pending selection, rendered while the selection
// is still pending, and — past the point where a user could have named the
// people one by one — has to be acknowledged before the request is sent.
//
// It is here rather than in the screen because it is arithmetic with a rule in
// it, and a rule that lives in a template is a rule nobody tests.
//
// # What the two directions are worth
//
// Starting to observe and stopping are not symmetrical, and nothing in here
// pretends they are. Adding a conversation puts other people's messages on the
// bus; removing one stops that. So [`consequence`] counts the additions,
// names the largest of them, and asks for an acknowledgement only about them.
// Removals are counted and never gated — exactly as the bulk control in
// `$lib/matrix/rooms.ts` deselects without restriction.

import type { ConversationRow } from './conversations';

/** One conversation, reduced to what a consequence needs to name it. */
export interface Named {
	readonly label: string;
	readonly members: number;
}

/** What applying the current selection would do, and to how many people. */
export interface Consequence {
	/** Conversations that would start being observed. */
	readonly starting: number;
	/** People those conversations cover. The number the criterion is about. */
	readonly people: number;
	/**
	 * The largest conversation being added, named.
	 *
	 * A total hides a 246 inside it: "402 people" reads as a big number, and
	 * "402 people, the largest being Échecs en Yvelines with 246" reads as a
	 * decision about an association.
	 */
	readonly largest: Named | null;
	/** Conversations that would stop being observed. Never gated. */
	readonly stopping: number;
	/**
	 * The additions at or over the served crowd threshold, largest first, so the card that asks
	 * for the acknowledgement can list them instead of summing them away.
	 */
	readonly crowds: readonly Named[];
	/** Whether the user must acknowledge before this can be sent. */
	readonly acknowledgementNeeded: boolean;
	/** Whether there is anything to send at all. */
	readonly empty: boolean;
}

/** Whether the Sensor is in this conversation, or on its way in. */
export function isObserved(row: ConversationRow): boolean {
	// `invited` counts as observed on this screen, and deliberately: the user
	// has decided, the invitation is out, and offering them the tick again
	// would be offering to make a decision they already made. That it has not
	// completed is a diagnosis the screen shows on the row itself — a portal
	// stuck at `invited` means `SENSOR_ALLOWED_INVITERS` does not name the
	// bridge's bot (ADR 0024) — and not a reason to un-tick it.
	return row.observation === 'observing' || row.observation === 'invited';
}

/** The selection a freshly read register starts from: what is already true. */
export function observedNow(rows: readonly ConversationRow[]): Set<string> {
	return new Set(rows.filter(isObserved).map((row) => row.roomId));
}

/**
 * What applying `selected` would change, against the register as read.
 *
 * `rows` is **every** conversation, not the filtered list: a selection made
 * under one search and applied under another must be costed in full, or the
 * screen would state a number for the part of the decision that happens to be
 * on screen.
 *
 * `crowdThreshold` is the register's own (`crowd_threshold`), never a number
 * of this screen's: the Gateway owns where a list becomes a crowd (#252), so
 * that what the screen asks the user to acknowledge and what the register
 * applies when a room is replaced (ADR 0029) are one number. It gates an
 * acknowledgement, never the action: nothing here can stop a user observing a
 * conversation they decided to observe.
 */
export function consequence(
	rows: readonly ConversationRow[],
	selected: ReadonlySet<string>,
	crowdThreshold: number
): Consequence {
	const adding = rows.filter((row) => selected.has(row.roomId) && !isObserved(row));
	const removing = rows.filter((row) => !selected.has(row.roomId) && isObserved(row));
	const people = adding.reduce((sum, row) => sum + row.members, 0);
	const named = adding
		.map((row) => ({ label: row.label, members: row.members }))
		.sort((left, right) => right.members - left.members);
	const crowds = named.filter((row) => row.members >= crowdThreshold);
	return {
		starting: adding.length,
		people,
		largest: named[0] ?? null,
		stopping: removing.length,
		crowds,
		acknowledgementNeeded: crowds.length > 0,
		empty: adding.length === 0 && removing.length === 0
	};
}

/**
 * The two requests applying a selection takes, in the order to send them.
 *
 * Additions first. `POST /api/portals/observation` decides one direction per
 * call, and a user who has both added and removed has made two decisions; if
 * the second call fails, the one that landed should be the one that starts
 * observation the user asked for rather than the one that quietly did not stop
 * it.
 *
 * Returns the room ids, never a body: the caller owns the Gateway client, and
 * a pure function that returns two arrays is one a test can read.
 */
export function requests(
	rows: readonly ConversationRow[],
	selected: ReadonlySet<string>
): ReadonlyArray<{ readonly observed: boolean; readonly rooms: readonly string[] }> {
	const adding = rows.filter((row) => selected.has(row.roomId) && !isObserved(row));
	const removing = rows.filter((row) => !selected.has(row.roomId) && isObserved(row));
	const calls: { observed: boolean; rooms: string[] }[] = [];
	if (adding.length > 0) {
		calls.push({ observed: true, rooms: adding.map((row) => row.roomId) });
	}
	if (removing.length > 0) {
		calls.push({ observed: false, rooms: removing.map((row) => row.roomId) });
	}
	return calls;
}

/**
 * The Gateway refuses more than this many rooms in one request
 * (`PortalObservationRequest.rooms`, `maxItems: 256`). An old account with
 * hundreds of conversations is exactly who would meet it, so the screen splits
 * rather than discovering it as a `400`.
 */
export const MAX_ROOMS_PER_REQUEST = 256;

/** `rooms`, cut into requests the Gateway will accept. */
export function batched(rooms: readonly string[]): string[][] {
	const batches: string[][] = [];
	for (let at = 0; at < rooms.length; at += MAX_ROOMS_PER_REQUEST) {
		batches.push([...rooms.slice(at, at + MAX_ROOMS_PER_REQUEST)]);
	}
	return batches;
}
