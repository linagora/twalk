// Reading a `/sync` answer into the list screen 3d shows, and labelling a room
// that will not name itself. Pure, and the part of screen 3d most likely to
// meet a shape nobody anticipated — a homeserver's answer is not this app's to
// design.

import { describe, expect, it } from 'vitest';

import { matchesQuery, resolveDisplayNames, roomLabel, roomsFromSync, type RoomSummary } from './rooms';

function sync(rooms: Record<string, unknown>) {
	return { next_batch: 's1', rooms: { join: rooms } };
}

function stateEvent(type: string, content: Record<string, unknown>) {
	return { type, state_key: '', content, event_id: `$${type}`, sender: '@you:example.com' };
}

function room(overrides: Partial<RoomSummary> = {}): RoomSummary {
	return {
		roomId: '!room:example.com',
		name: null,
		alias: null,
		encrypted: false,
		heroes: [],
		joinedMembers: null,
		...overrides
	};
}

describe('reading the rooms out of a sync', () => {
	it('takes the name, the alias and the encryption state', () => {
		const rooms = roomsFromSync(
			sync({
				'!a:example.com': {
					state: {
						events: [
							stateEvent('m.room.name', { name: 'Family' }),
							stateEvent('m.room.canonical_alias', { alias: '#family:example.com' }),
							stateEvent('m.room.encryption', { algorithm: 'm.megolm.v1.aes-sha2' })
						]
					},
					summary: { 'm.joined_member_count': 4 }
				}
			})
		);
		expect(rooms).toEqual([
			{
				roomId: '!a:example.com',
				name: 'Family',
				alias: '#family:example.com',
				encrypted: true,
				heroes: [],
				joinedMembers: 4
			}
		]);
	});

	it('finds the name when the homeserver put it in the timeline', () => {
		// Observed against Synapse: a room young enough that its whole history
		// fits in the timeline has an *empty* `state`, and its `m.room.name`
		// among the timeline events. A reader that only looked at `state`
		// would list every freshly created room as nameless.
		const rooms = roomsFromSync(
			sync({
				'!new:example.com': {
					state: { events: [] },
					timeline: { events: [stateEvent('m.room.name', { name: 'Just created' })] }
				}
			})
		);
		expect(rooms[0]?.name).toBe('Just created');
	});

	it('prefers the later of two values for the same state event', () => {
		const rooms = roomsFromSync(
			sync({
				'!renamed:example.com': {
					state: { events: [stateEvent('m.room.name', { name: 'Old' })] },
					timeline: { events: [stateEvent('m.room.name', { name: 'New' })] }
				}
			})
		);
		expect(rooms[0]?.name).toBe('New');
	});

	it('keeps a room that carries nothing at all', () => {
		// A homeserver may answer a joined room with no state we asked for.
		// Dropping it would hide a room the user can invite the Sensor into.
		const rooms = roomsFromSync(sync({ '!bare:example.com': {} }));
		expect(rooms).toHaveLength(1);
		expect(rooms[0]?.roomId).toBe('!bare:example.com');
	});

	it('reads the heroes a nameless room would be named after', () => {
		const rooms = roomsFromSync(
			sync({
				'!dm:example.com': {
					summary: { 'm.heroes': ['@alice:example.com', 42, '@bob:example.com'] }
				}
			})
		);
		expect(rooms[0]?.heroes).toEqual(['@alice:example.com', '@bob:example.com']);
	});

	it('answers an empty list rather than throwing on a shape it did not expect', () => {
		expect(roomsFromSync(null)).toEqual([]);
		expect(roomsFromSync({})).toEqual([]);
		expect(roomsFromSync({ rooms: { join: null } })).toEqual([]);
		expect(roomsFromSync('not a sync')).toEqual([]);
	});

	it('sorts by what the user will read', () => {
		const rooms = roomsFromSync(
			sync({
				'!b:example.com': { state: { events: [stateEvent('m.room.name', { name: 'Work' })] } },
				'!a:example.com': { state: { events: [stateEvent('m.room.name', { name: 'Family' })] } }
			})
		);
		expect(rooms.map((entry) => entry.name)).toEqual(['Family', 'Work']);
	});
});

describe('labelling a room', () => {
	it('uses the room’s own name when it has one', () => {
		expect(roomLabel(room({ name: 'Family' }))).toEqual({ text: 'Family', source: 'name' });
	});

	it('falls back to the alias, and says that is what it did', () => {
		expect(roomLabel(room({ alias: '#family:example.com' }))).toEqual({
			text: '#family:example.com',
			source: 'alias'
		});
	});

	it('falls back to the members of a room with no name', () => {
		// A direct message. Every Matrix client computes this from display
		// names; without the member state this listing does not fetch, the
		// user ids are what is honestly known.
		const label = roomLabel(room({ heroes: ['@alice:example.com', '@bob:example.com'] }));
		expect(label.source).toBe('heroes');
		expect(label.text).toContain('@alice:example.com');
	});

	it('never renders blank: the id stands in when nothing else does', () => {
		expect(roomLabel(room())).toEqual({ text: '!room:example.com', source: 'room-id' });
		// Whitespace is not a name either.
		expect(roomLabel(room({ name: '   ' })).source).toBe('room-id');
	});
});

describe('naming a room without a name', () => {
	// On a real work account most rooms are direct messages, so most of the
	// list was Matrix IDs and a user looking for a colleague was shown a
	// localpart (#137).
	const dm = (heroes: string[]): RoomSummary => ({
		roomId: '!abc:linagora.com',
		name: null,
		alias: null,
		encrypted: true,
		heroes,
		joinedMembers: heroes.length + 1
	});

	it('uses the display names the homeserver gave', () => {
		const directory = new Map([['@jplorre:linagora.com', 'Jean-Pierre LORRÉ']]);
		expect(roomLabel(dm(['@jplorre:linagora.com']), directory)).toEqual({
			text: 'Jean-Pierre LORRÉ',
			source: 'heroes'
		});
	});

	it('keeps the id of a hero it could not name, beside the ones it could', () => {
		// Half-named is still true. Dropping the unnamed would misdescribe who
		// is in the room, which is worse than an unfriendly label.
		const directory = new Map([['@jplorre:linagora.com', 'Jean-Pierre LORRÉ']]);
		expect(roomLabel(dm(['@jplorre:linagora.com', '@julie:linagora.com']), directory).text).toBe(
			'Jean-Pierre LORRÉ, @julie:linagora.com'
		);
	});

	it('degrades to exactly what it did before, with no directory at all', () => {
		expect(roomLabel(dm(['@jplorre:linagora.com'])).text).toBe('@jplorre:linagora.com');
	});
});

describe('matchesQuery', () => {
	const room: RoomSummary = {
		roomId: '!zYPBIKiQbCfLavhgUA:linagora.com',
		name: null,
		alias: null,
		encrypted: true,
		heroes: ['@jplorre:linagora.com'],
		joinedMembers: 2
	};
	const directory = new Map([['@jplorre:linagora.com', 'Jean-Pierre LORRÉ']]);

	it('matches an empty query, so a blank field hides nothing', () => {
		expect(matchesQuery(room, '', directory)).toBe(true);
		expect(matchesQuery(room, '   ', directory)).toBe(true);
	});

	it('finds a person by the name the user is reading', () => {
		expect(matchesQuery(room, 'Jean-Pierre', directory)).toBe(true);
	});

	it('ignores case and accents, because a name is typed the way it sounds', () => {
		expect(matchesQuery(room, 'lorre', directory)).toBe(true);
		expect(matchesQuery(room, 'LORRÉ', directory)).toBe(true);
	});

	it('also finds what a user might type from memory instead of the label', () => {
		expect(matchesQuery(room, 'jplorre', directory)).toBe(true);
		expect(matchesQuery(room, 'zYPBIKi', directory)).toBe(true);
	});

	it('does not match something absent', () => {
		expect(matchesQuery(room, 'julie', directory)).toBe(false);
	});
});

describe('resolveDisplayNames', () => {
	const answering = (names: Record<string, unknown>): typeof fetch =>
		(async (url: string) => {
			const id = decodeURIComponent(String(url).split('/profile/')[1].split('/')[0]);
			if (!(id in names)) {
				return new Response('{}', { status: 404 });
			}
			return new Response(JSON.stringify({ displayname: names[id] }), { status: 200 });
		}) as unknown as typeof fetch;

	it('asks once per distinct user and returns what it was told', async () => {
		let calls = 0;
		const counting = (async (url: string) => {
			calls += 1;
			return answering({ '@a:x': 'Ada' })(url as never);
		}) as unknown as typeof fetch;
		const found = await resolveDisplayNames('https://x', 'token', ['@a:x', '@a:x'], {
			fetchImpl: counting
		});
		expect(found.get('@a:x')).toBe('Ada');
		expect(calls, 'the same user is not asked about twice').toBe(1);
	});

	it('leaves out a user the homeserver will not name', async () => {
		const found = await resolveDisplayNames('https://x', 'token', ['@a:x', '@b:x'], {
			fetchImpl: answering({ '@a:x': 'Ada', '@b:x': '   ' })
		});
		// An empty name is not a name: the id stands in, as it always did.
		expect([...found.keys()]).toEqual(['@a:x']);
	});

	it('survives a homeserver that refuses outright', async () => {
		const throwing = (async () => {
			throw new TypeError('Failed to fetch');
		}) as unknown as typeof fetch;
		await expect(
			resolveDisplayNames('https://x', 'token', ['@a:x'], { fetchImpl: throwing })
		).resolves.toEqual(new Map());
	});
});
