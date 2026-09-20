// What the consent screen draws, decided here rather than in the markup.
//
// The screen has one job with a decision in it — *what is actually in force for
// this contact, and what decided it* — and that is the product's central
// promise, so it lives in a module a test can reach.
//
// # Three states, and `pending` is not `revoked`
//
// The Gateway's vocabulary is `granted`, `pending`, `revoked`, and `unset` is
// deliberately not a fourth: it is the **absence** of a decision, and only
// `old_state` carries it. ADR 0010 says it in the snapshot's own terms — *"an
// absent subject means no decision was ever recorded, never a revoked one"*.
//
// So a row carries two facts, not one, and this module's whole reason to exist
// is that they are separate:
//
//   - `state` — what applies: `granted`, `pending` or `revoked`;
//   - `decidedBy` — *what* made it apply: this contact's own decision, the
//     network's default, or **nothing at all**.
//
// `decidedBy: 'nothing'` with `state: 'pending'` is the default of the whole
// model and will be most of this list on a real deployment: eighteen
// conversations appeared on the reference deployment in one day. A screen that
// rendered it as "pending" beside a contact the user deliberately left pending
// would show two states where the model has three, and teach the user a wrong
// picture of their own deployment.
//
// That distinction is also the Gateway's: `GET /api/contacts/pending` lists
// exactly the rows whose `decided_by` is `null`, and a contact the owner
// deliberately set to `pending` is *not* in it — *"they answered, and the
// answer was 'not yet'"*. So [`awaiting`] is not a synonym for
// `state === 'pending'`, and `model.test.ts` asserts that it is not.
//
// # Only `revoked` withholds content
//
// `granted` lets a persona read. `pending` **publishes the message** with a
// label consumers honour by convention — the Sensor labels and consumers
// refuse (ADR 0012). Only `revoked` reduces what is published at all: no body,
// no excerpt, no attachment reference. Copy that implied undecided meant unseen
// would be a comforting untrue thing, which is the class of defect this
// project has spent two days undoing, so [`withholds`] is a function here
// rather than an adjective in a catalogue.
//
// # None of it is retroactive
//
// The label is stamped by the Sensor **at publication**, so a decision taken
// today does not reach yesterday's events. Nothing in this module can express
// a retroactive change, and the screen says so out loud: a user who grants a
// contact and sees nothing happen must not conclude the product is broken.
//
// # The owner is not a contact, and is not filtered out here
//
// ADR 0018 and ADR 0021: the owner has no consent state, on any event. If they
// appear in one of these lists, that is #149's Gateway half showing through —
// a row recorded before #109 landed — and the honest rendering is to **say
// so**, not to hide it. [`isOwner`] flags such a row so the screen can label
// it and refuse to offer a decision on it, and the flag is only ever as good
// as what this browser knows: the owner's canonical Matrix ID, from the
// session. Their *network ghosts* (`@whatsapp_lid-…`, `@signal_<uuid>`) are
// the identities #149 is actually about, and no browser can recognise one —
// the Gateway does not know them either, which is that ticket's fifth
// acceptance criterion.

import type { components } from '$lib/api/schema';
import { labelFor, ofKind, type Connection } from '$lib/connections/registry';

export type Network = components['schemas']['Network'];
export type State = components['schemas']['ConsentState_State'];
export type Entry = components['schemas']['ConsentStateEntry'];
export type PendingContact = components['schemas']['PendingContact'];
export type DisplayName = components['schemas']['ContactDisplayName'];

/**
 * What made a row's state apply.
 *
 * `'nothing'` is the one that matters: it is the model's default, it is what
 * `GET /api/contacts/pending` lists, and it is a different fact from a
 * decision whose answer was `pending`.
 */
export type DecidedBy = 'contact' | 'network' | 'nothing';

/** How a row's label was arrived at, so the screen can say which (as #137 does for a room). */
export type LabelSource = 'display-name' | 'matrix-id';

export interface Row {
	/** The contact's Matrix user ID: what a decision about them names as its subject. */
	contact: string;
	/** The connection this row is about (ADR 0033, #272): the perimeter a decision is scoped to. */
	connection: string;
	/** Which account, when the connection's kind has more than one; `null` when it is the only one. */
	connectionLabel: string | null;
	/** The connection's kind. */
	network: Network;
	/** What applies, after the precedence. */
	state: State;
	/** What made it apply. */
	decidedBy: DecidedBy;
	/** What to call them, and whether that is a name or an id. */
	label: string;
	labelSource: LabelSource;
	/** From `GET /api/contacts/pending`, and `null` for a row that is not in it. */
	firstSeen: string | null;
	lastSeen: string | null;
	/**
	 * Whether this contact's own decision differs from their connection's
	 * default.
	 *
	 * The precedence, made visible: a user who granted a whole connection and
	 * sees one contact still producing nothing needs to be told that the
	 * contact's own decision won, not left to guess (#170).
	 */
	overridesNetwork: boolean;
	/** The connection's default, when there is one, so the screen can name it. */
	networkDefault: State | null;
	/** This deployment's owner, wrongly present as a contact. See ADR 0021, #149. */
	isOwner: boolean;
}

/** Whether a row is waiting for a first decision — the Gateway's `decided_by: null`. */
export function awaiting(row: Row): boolean {
	return row.decidedBy === 'nothing';
}

/**
 * What a state stops from being published, in the only three answers there are.
 *
 * `'content'` is `revoked` and nothing else: the Sensor publishes the event
 * without the body, the excerpts and the attachment (ADR 0012). `'processing'`
 * is `pending` — the message is published in full and consumers refuse it by
 * convention. `'nothing'` is `granted`.
 */
export function withholds(state: State): 'content' | 'processing' | 'nothing' {
	switch (state) {
		case 'revoked':
			return 'content';
		case 'pending':
			return 'processing';
		case 'granted':
			return 'nothing';
	}
}

/**
 * The recorded default of each connection, from the `network` subjects of
 * the state — a network default is held per connection since #270, and the
 * entry's `connection` is the one being defaulted.
 */
export function connectionDefaults(entries: readonly Entry[]): Map<string, State> {
	const defaults = new Map<string, State>();
	for (const entry of entries) {
		if (entry.subject.type === 'network') {
			defaults.set(entry.connection, entry.state);
		}
	}
	return defaults;
}

export interface Inputs {
	/** `GET /api/contacts/pending`'s `contacts`, or `[]` when it was not read. */
	pending: readonly PendingContact[];
	/** `GET /api/consent/state`'s `entries`, or `[]`. */
	entries: readonly Entry[];
	/** `GET /api/contacts/display-names`, as far as the bus still carries them. */
	names: readonly DisplayName[];
	/** This deployment's owner, from the session, or `null` when unknown. */
	owner: string | null;
	/** `GET /api/connections`, or `[]` when it was not read: what names an account when a kind has two. */
	connections: readonly Connection[];
}

/**
 * Every contact this screen can decide about, with what is in force for each.
 *
 * The union of two lists, because neither is the whole truth: a contact who has
 * written and never been decided about is only in `GET /api/contacts/pending`,
 * and a contact who was decided about and has not written since the Gateway's
 * store was created is only in `GET /api/consent/state`. A screen showing one
 * of the two would either lose every decision the user has already taken, or
 * lose everybody waiting for one.
 *
 * `persona` subjects are dropped: activating a persona is a consent decision
 * (ADR 0013) and it belongs to `/personas`, where it has a screen of its own.
 * `network` subjects become each row's `networkDefault` rather than rows —
 * a network is not somebody.
 */
export function toRows(inputs: Inputs): Row[] {
	const defaults = connectionDefaults(inputs.entries);
	const named = new Map<string, string>();
	for (const entry of inputs.names) {
		if (entry.display_name !== null && entry.display_name.trim() !== '') {
			named.set(entry.contact, entry.display_name.trim());
		}
	}

	/** The contact's own decision, by `(contact, connection)`. */
	const decided = new Map<string, { state: State; network: Network }>();
	for (const entry of inputs.entries) {
		if (entry.subject.type === 'contact') {
			decided.set(key(entry.subject.id, entry.connection), {
				state: entry.state,
				network: entry.network
			});
		}
	}

	const sighted = new Map<string, PendingContact>();
	for (const contact of inputs.pending) {
		sighted.set(key(contact.contact, contact.connection), contact);
	}

	const seen = new Set<string>([...decided.keys(), ...sighted.keys()]);
	const rows: Row[] = [];
	for (const at of seen) {
		const { contact, connection } = unkey(at);
		const own = decided.get(at);
		const sighting = sighted.get(at) ?? null;
		// The kind, as the record that put the row here spelled it.
		const network = own?.network ?? sighting?.network;
		if (network === undefined) {
			continue;
		}
		const networkDefault = defaults.get(connection) ?? null;
		const state = own?.state ?? networkDefault ?? 'pending';
		const decidedBy: DecidedBy =
			own !== undefined ? 'contact' : networkDefault !== null ? 'network' : 'nothing';
		const name = named.get(contact);
		const siblings = ofKind(inputs.connections, network);
		const registered = siblings.find((candidate) => candidate.id === connection);
		rows.push({
			contact,
			connection,
			connectionLabel: registered === undefined ? null : labelFor(registered, siblings),
			network,
			state,
			decidedBy,
			label: name ?? contact,
			labelSource: name === undefined ? 'matrix-id' : 'display-name',
			firstSeen: sighting?.first_seen ?? null,
			lastSeen: sighting?.last_seen ?? null,
			overridesNetwork:
				own !== undefined && networkDefault !== null && own.state !== networkDefault,
			networkDefault,
			isOwner: inputs.owner !== null && contact === inputs.owner
		});
	}
	return rows.sort(order);
}

/**
 * Waiting first, then by label.
 *
 * The rows that need a decision are the reason to open this screen, and a list
 * ordered by name buries them among hundreds that are already decided.
 */
function order(left: Row, right: Row): number {
	if (awaiting(left) !== awaiting(right)) {
		return awaiting(left) ? -1 : 1;
	}
	const byLabel = left.label.localeCompare(right.label);
	return byLabel !== 0 ? byLabel : left.connection.localeCompare(right.connection);
}

// A NUL, which no Matrix ID and no connection id contains.
const SEPARATOR = '\u0000';

function key(contact: string, connection: string): string {
	return `${contact}${SEPARATOR}${connection}`;
}

function unkey(at: string): { contact: string; connection: string } {
	const cut = at.lastIndexOf(SEPARATOR);
	return { contact: at.slice(0, cut), connection: at.slice(cut + 1) };
}

/**
 * Whether a row answers a search.
 *
 * Matches what the user is reading and what they might type from memory
 * instead: the display name, the Matrix ID, its localpart — which is where a
 * bridged ghost keeps the phone number the user knows the person by. Accent-
 * and case-insensitive, exactly as `$lib/matrix/rooms.ts` folds a room's name
 * (#137).
 */
export function matchesQuery(row: Row, query: string): boolean {
	const needle = fold(query);
	if (needle === '') {
		return true;
	}
	return [
		row.label,
		row.contact,
		localpart(row.contact),
		row.network,
		row.connectionLabel ?? ''
	].some((straw) => fold(straw).includes(needle));
}

function localpart(matrixId: string): string {
	const at = matrixId.startsWith('@') ? matrixId.slice(1) : matrixId;
	const colon = at.indexOf(':');
	return colon === -1 ? at : at.slice(0, colon);
}

/** Lower-cased and stripped of diacritics, so "aicha" finds "Aïcha". */
function fold(value: string): string {
	return value
		.normalize('NFD')
		.replace(/\p{Diacritic}/gu, '')
		.toLowerCase()
		.trim();
}

export interface Counts {
	granted: number;
	pending: number;
	revoked: number;
	/** How many have never been decided about — a subset of `pending`. */
	awaiting: number;
}

/**
 * The four numbers the screen states.
 *
 * `awaiting` is counted separately from `pending` and is not subtracted from
 * it: both are true of the same row, and a screen that reported "4 pending, 3
 * waiting" as disjoint groups would be inventing a fifth state.
 */
export function counts(rows: readonly Row[]): Counts {
	const tally: Counts = { granted: 0, pending: 0, revoked: 0, awaiting: 0 };
	for (const row of rows) {
		tally[row.state] += 1;
		if (awaiting(row)) {
			tally.awaiting += 1;
		}
	}
	return tally;
}

/**
 * The decisions a bulk control would actually write, over **what the filter is
 * showing** and nothing else.
 *
 * #137's rule, carried to a screen where the stakes are higher: *"'Grant all'
 * over a list containing a 246-member association is the affordance this screen
 * exists to avoid."* So there is no function here that takes the whole list —
 * the caller passes the rows on screen, and the count it displays is this
 * array's length rather than a number computed somewhere else.
 *
 * Two rows are left out, and both are refusals rather than oversights:
 *
 *   - a row already in that state by **its own** decision, because rewriting
 *     it would put a second identical decision in an append-only journal for
 *     no change (the Gateway answers `200 replayed` for it, which is correct
 *     and still a request nobody needed);
 *   - the owner, if #149's row is showing. The screen refuses to offer a
 *     decision about the user, one at a time or in a hundred.
 *
 * A row that holds this state only by its **network's** default is kept: the
 * user asking for it per-contact is asking for a decision that survives the
 * network default changing, and that is a different fact from inheriting it.
 */
export function bulkDecisions(
	shown: readonly Row[],
	state: State
): { contact: string; connection: string }[] {
	return shown
		.filter((row) => !row.isOwner)
		.filter((row) => !(row.decidedBy === 'contact' && row.state === state))
		.map((row) => ({ contact: row.contact, connection: row.connection }));
}

/**
 * The owner rows this screen found, which should not exist (ADR 0018, ADR 0021).
 *
 * Reported rather than removed, and **kept now that #149 has landed**: a
 * Gateway of that version or later serves no such row — not from the consent
 * state, not from the snapshot, not from the pending list, including a row it
 * inherited from before #109 — so on a current deployment this function
 * returns nothing. It stays because a partially upgraded deployment is exactly
 * the situation #149 exists for: an older Gateway behind a newer Companion
 * still serves the row, and silently filtering it here would hide the one
 * symptom of that a user can actually see. Its own unit tests are what keep
 * the defence exercised, since no Gateway response can produce it any more.
 */
export function ownerRows(rows: readonly Row[]): Row[] {
	return rows.filter((row) => row.isOwner);
}
