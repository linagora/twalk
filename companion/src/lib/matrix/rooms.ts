// The user's rooms, listed **from the browser**.
//
// Screen 3d's shape is fixed by ADR 0011: the Gateway learns only the room ids
// the user actually selected. So the listing is a call from this page to the
// user's own homeserver with the user's own token, and the selection is what
// crosses to `POST /api/bootstrap/rooms`. Twalk never sees the rooms the user
// did not choose, and never sees the token beyond that one call.
//
// # One `/sync`, not one request per room
//
// `GET /_matrix/client/v3/joined_rooms` gives ids and nothing else, so a name
// would be a request per room — a hundred requests on a real account. One
// filtered `/sync` with `timeout=0` brings back every joined room's name,
// canonical alias, encryption state and hero summary in a single answer, which
// is what this module parses — out of *both* the `state` block and the
// timeline, because which of the two a room's name lands in is the
// homeserver's business and not ours.
//
// # Why a room can have no readable name
//
// A room's name is an unencrypted state event, so `m.room.name` is readable
// here. What is *not* is the name of a room that has none — a direct message,
// whose name every Matrix client computes from the other members' display
// names. Under `lazy_load_members` the homeserver gives the heroes' user ids
// and not their profiles, and resolving those needs the member state (and, for
// the rooms this app cares about, the crypto stack the networks screens
// deliberately do not load).
//
// So a room with no name is not rendered blank and not hidden: it is rendered
// as what is actually known about it — its heroes, or its id — and labelled as
// such. `roomLabel` is pure and says which of the two it did.
//
// # Names, without a client
//
// On a real account most rooms are direct messages, so most of the list was
// Matrix IDs: `@aelazhar:linagora.com`, `@agadji:linagora.com`, page after
// page. A user looking for a colleague is shown a localpart (#137).
//
// The paragraph above conflated two things. Resolving a hero's display name
// needs **member state or the crypto stack** only if you want it from a
// `MatrixClient`; the homeserver also answers
// `GET /_matrix/client/v3/profile/{userId}/displayname`, which is an ordinary
// HTTP call with the user's own token and no client at all. What these screens
// refuse to load is a `MatrixClient`, and that refusal is intact.
//
// So `resolveDisplayNames` asks for the heroes' names, with bounded
// concurrency and a deadline, and `roomLabel` takes what it found. A
// homeserver that will not answer, or answers too slowly, degrades to exactly
// the behaviour above: ids, labelled as ids. The list never waits on it.
//
// # A room that is not a conversation is not listed
//
// Since #226 the user's account is joined to a room that is **not** a
// conversation: the handover room it shares with the Sensor
// (`$lib/matrix/handover.ts`). Offering it here would be the worst defect this
// screen can have — consent in Twalk is a membership fact (ADR 0024), so a
// room the Sensor is joined to that something counts as a conversation is
// consent nobody granted.
//
// It is left out by [`isConversation`], which reads `m.room.create`'s `type`:
// a room with any type at all is not an ordinary conversation, which is
// Matrix's own rule — it is how every client keeps a space out of its room
// list. So the exclusion is not a list of Twalk's own room ids that a later
// room could be forgotten from; it is the same one sentence for a space, for
// the handover room, and for whatever carries a type next.
//
// And it is applied **inside** [`roomsFromSync`], which is the only way a room
// reaches this app from a sync, rather than left for each caller to remember.

/** One joined room, reduced to what the selection screen needs. */
export interface RoomSummary {
	readonly roomId: string;
	/** `m.room.name`, when the room has one. */
	readonly name: string | null;
	/** `m.room.canonical_alias`, when it has one. */
	readonly alias: string | null;
	/** Whether `m.room.encryption` is set. */
	readonly encrypted: boolean;
	/** `m.heroes`: the other members a client would name a nameless room after. */
	readonly heroes: readonly string[];
	/** Joined members, when the summary carries the count. */
	readonly joinedMembers: number | null;
	/**
	 * `m.room.create`'s `type`, and `null` for an ordinary room.
	 *
	 * Immutable: `m.room.create` is the one state event Matrix forbids
	 * replacing or redacting, so a room that says it is not a conversation
	 * cannot later stop saying it.
	 */
	readonly type: string | null;
}

/** How a room's label was arrived at, so the screen can say so. */
export type LabelSource = 'name' | 'alias' | 'heroes' | 'room-id';

export interface RoomLabel {
	readonly text: string;
	readonly source: LabelSource;
}

/** Display names by Matrix ID, as far as the homeserver would say. */
export type Directory = ReadonlyMap<string, string>;

/**
 * The best label this page can honestly produce for a room.
 *
 * Never empty, and never a guess presented as a name: `source` says whether the
 * room told us its name, or whether this is the id standing in for one.
 *
 * `directory` is what `resolveDisplayNames` found, and is optional throughout:
 * every caller works without it, and the label is only ever better with it.
 */
export function roomLabel(room: RoomSummary, directory?: Directory): RoomLabel {
	if (room.name !== null && room.name.trim() !== '') {
		return { text: room.name.trim(), source: 'name' };
	}
	if (room.alias !== null && room.alias.trim() !== '') {
		return { text: room.alias.trim(), source: 'alias' };
	}
	if (room.heroes.length > 0) {
		// What a Matrix client would compute. A hero the directory could not
		// name keeps its id rather than being dropped: a room labelled with
		// half its members named and half not is still true, and hiding the
		// unnamed ones would misdescribe who is in it.
		const named = room.heroes
			.slice(0, 3)
			.map((hero) => directory?.get(hero) ?? hero);
		return { text: named.join(', '), source: 'heroes' };
	}
	return { text: room.roomId, source: 'room-id' };
}

/**
 * Whether a room answers a search.
 *
 * Matches the label a user is reading, and also what they might type from
 * memory instead: an alias, a member's display name, a Matrix ID, the room id.
 * Accent- and case-insensitive, because a name is typed the way it sounds.
 */
export function matchesQuery(room: RoomSummary, query: string, directory?: Directory): boolean {
	const needle = fold(query);
	if (needle === '') {
		return true;
	}
	const haystack = [
		room.name ?? '',
		room.alias ?? '',
		room.roomId,
		...room.heroes,
		...room.heroes.map((hero) => directory?.get(hero) ?? '')
	];
	return haystack.some((straw) => fold(straw).includes(needle));
}

/**
 * Lower-cased and stripped of diacritics, so "lorre" finds "Lorré".
 *
 * Exported because the portal chooser (#143) needs the *same* fold to decide
 * which conversation names belong to one community, and a second spelling of
 * this rule is how #110 happened. It is the only text-normalising rule in the
 * Companion, and there is one of it.
 */
export function fold(value: string): string {
	return value
		.normalize('NFD')
		.replace(/\p{Diacritic}/gu, '')
		.toLowerCase()
		.trim();
}

/**
 * What a bulk control may do, given what the filter is showing.
 *
 * The rule #137 established, and the reason it is a function rather than four
 * lines in a component: this screen hands a set of conversations to an
 * observing agent, and the people in them do not know Twalk exists (#122). A
 * control that reaches past the filter is how sixty of them get observed
 * without anyone deciding to — so the scope is *what is on screen*, the count
 * is in the label, and "select all" is never offered.
 *
 * Two choosers obey it now: the Matrix room chooser (#137) and the portal
 * chooser (#143), whose lists contain a 246-member association. One rule, one
 * implementation, tested once.
 *
 * `shown` is the ids the filter left in, in any order. Deselecting is
 * unrestricted — removing is always safe — which is why there is no threshold
 * anywhere in here: the asymmetry is in the copy the caller chooses from
 * `selectsRatherThanDeselects`, not in what this will do.
 */
export function scopedBulkControl(
	shown: readonly string[],
	selected: ReadonlySet<string>
): {
	/** How many the control will touch. Never "all", always a number. */
	readonly count: number;
	/**
	 * Whether pressing it *adds*. False when every shown id is already
	 * chosen, which is when the same control becomes "deselect the 4 shown".
	 */
	readonly selectsRatherThanDeselects: boolean;
	/** The selection after pressing it. A new set; the input is untouched. */
	apply(): Set<string>;
} {
	const adds = shown.length > 0 && !shown.every((id) => selected.has(id));
	return {
		count: shown.length,
		selectsRatherThanDeselects: adds,
		apply(): Set<string> {
			const next = new Set(selected);
			for (const id of shown) {
				if (adds) {
					next.add(id);
				} else {
					next.delete(id);
				}
			}
			return next;
		}
	};
}

/**
 * The display names of the users named, asked of the homeserver directly.
 *
 * Bounded: at most `concurrency` requests in flight, and the whole thing gives
 * up at `deadlineMs` with whatever it has. A name is a nicety — the list is
 * usable without it, and must never wait on it. A user the homeserver will not
 * describe, or describes with an empty name, is simply absent from the result.
 */
export async function resolveDisplayNames(
	baseUrl: string,
	accessToken: string,
	userIds: readonly string[],
	options: { fetchImpl?: typeof fetch; concurrency?: number; deadlineMs?: number } = {}
): Promise<Map<string, string>> {
	const doFetch = options.fetchImpl ?? globalThis.fetch.bind(globalThis);
	const concurrency = options.concurrency ?? 6;
	const found = new Map<string, string>();
	const queue = [...new Set(userIds)];
	const deadline = Date.now() + (options.deadlineMs ?? 8000);

	const worker = async (): Promise<void> => {
		for (;;) {
			const userId = queue.shift();
			if (userId === undefined || Date.now() > deadline) {
				return;
			}
			try {
				const response = await doFetch(
					`${baseUrl}/_matrix/client/v3/profile/${encodeURIComponent(userId)}/displayname`,
					{ headers: { authorization: `Bearer ${accessToken}` } }
				);
				if (!response.ok) {
					continue;
				}
				const body: unknown = await response.json();
				const name = (body as { displayname?: unknown })?.displayname;
				if (typeof name === 'string' && name.trim() !== '') {
					found.set(userId, name.trim());
				}
			} catch {
				// A profile that cannot be read leaves the id in place, which
				// is what the screen showed before this existed.
			}
		}
	};

	await Promise.all(Array.from({ length: Math.min(concurrency, queue.length) }, worker));
	return found;
}

/** The filter that makes one `/sync` answer this screen's whole question. */
export const ROOM_LIST_FILTER = {
	presence: { types: [] as string[] },
	room: {
		timeline: { limit: 1 },
		ephemeral: { types: [] as string[] },
		state: {
			lazy_load_members: true,
			// `m.room.create` is asked for so that a room which says it is not
			// a conversation can be left out of the list — see the module docs
			// and [`isConversation`].
			types: [
				'm.room.name',
				'm.room.canonical_alias',
				'm.room.encryption',
				'm.room.create'
			]
		}
	}
};

/**
 * Whether a room is a conversation at all.
 *
 * One rule: a room whose `m.room.create` carries a `type` is something else —
 * a space (`m.space`), Twalk's own handover room
 * (`$lib/matrix/handover.ts`), whatever comes next. Matrix's own distinction,
 * so nothing Twalk-specific decides what the user is offered.
 */
export function isConversation(room: RoomSummary): boolean {
	return room.type === null;
}

/**
 * Reads the **conversations** out of a `/sync` answer. Pure, so it is unit
 * tested.
 *
 * Rooms that are not conversations never come out of here, which is what makes
 * that a property of the listing rather than a filter each screen has to
 * remember (see the module docs).
 */
export function roomsFromSync(sync: unknown): RoomSummary[] {
	const joined = pathOf(sync, ['rooms', 'join']);
	if (joined === null || typeof joined !== 'object') {
		return [];
	}
	return Object.entries(joined as Record<string, unknown>)
		.map(([roomId, room]) => summarise(roomId, room))
		.filter(isConversation)
		.sort((left, right) => roomLabel(left).text.localeCompare(roomLabel(right).text));
}

function summarise(roomId: string, room: unknown): RoomSummary {
	// `state` and the timeline both carry state events, and which one a given
	// room's name lands in is the homeserver's business: Synapse puts it in
	// `state` when the timeline is truncated, and in the timeline itself when
	// the room is young enough that its whole history fits. Reading both, in
	// that order, is what makes this work for a room created a second ago and
	// for one with ten years of history.
	const before = pathOf(room, ['state', 'events']);
	const recent = pathOf(room, ['timeline', 'events']);
	const state = [...(Array.isArray(before) ? before : []), ...(Array.isArray(recent) ? recent : [])];
	const heroes = pathOf(room, ['summary', 'm.heroes']);
	const joined = pathOf(room, ['summary', 'm.joined_member_count']);
	return {
		roomId,
		name: contentString(state, 'm.room.name', 'name'),
		alias: contentString(state, 'm.room.canonical_alias', 'alias'),
		encrypted: state.some(
			(event) => typeof event === 'object' && event !== null && (event as Record<string, unknown>)['type'] === 'm.room.encryption'
		),
		heroes: Array.isArray(heroes) ? heroes.filter((hero): hero is string => typeof hero === 'string') : [],
		joinedMembers: typeof joined === 'number' ? joined : null,
		type: contentString(state, 'm.room.create', 'type')
	};
}

/** The last value of a state event's field, so a later revision wins. */
function contentString(state: unknown[], type: string, key: string): string | null {
	let found: string | null = null;
	for (const event of state) {
		if (event === null || typeof event !== 'object') {
			continue;
		}
		const record = event as Record<string, unknown>;
		if (record['type'] !== type) {
			continue;
		}
		const value = pathOf(record['content'], [key]);
		if (typeof value === 'string' && value.length > 0) {
			found = value;
		}
	}
	return found;
}

function pathOf(value: unknown, path: readonly string[]): unknown {
	let current = value;
	for (const step of path) {
		if (current === null || typeof current !== 'object') {
			return null;
		}
		current = (current as Record<string, unknown>)[step];
	}
	return current ?? null;
}

/**
 * Lists the **conversations** this session has joined.
 *
 * `timeout=0` so the homeserver answers with what it has rather than holding
 * the request open: this is a listing, not a live sync.
 *
 * A joined room that is not a conversation — a space, the handover room of
 * `$lib/matrix/handover.ts` — is not in the answer, because it is not in
 * [`roomsFromSync`]'s.
 */
export async function listRooms(
	baseUrl: string,
	accessToken: string,
	fetchImpl?: typeof fetch
): Promise<RoomSummary[]> {
	const doFetch = fetchImpl ?? globalThis.fetch.bind(globalThis);
	const filter = encodeURIComponent(JSON.stringify(ROOM_LIST_FILTER));
	const response = await doFetch(`${baseUrl}/_matrix/client/v3/sync?timeout=0&filter=${filter}`, {
		headers: { authorization: `Bearer ${accessToken}` }
	});
	if (!response.ok) {
		throw new Error(`the homeserver refused to list the rooms: ${response.status}`);
	}
	return roomsFromSync(await response.json());
}
