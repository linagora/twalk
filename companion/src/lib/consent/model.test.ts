// The consent screen's model, which is where every decision in this screen
// lives.
//
// What is worth asserting here is not that a grant renders as "granted". It is
// the three things the ticket argues, each of which a screen can get wrong
// while looking right:
//
//   - never-decided is its own state, and is not "pending";
//   - only `revoked` withholds content;
//   - a bulk control acts on what the filter shows and on nothing else.

import { describe, expect, it } from 'vitest';

import {
	awaiting,
	bulkDecisions,
	counts,
	matchesQuery,
	networkDefaults,
	ownerRows,
	toRows,
	withholds,
	type Entry,
	type PendingContact,
	type Row
} from './model';

const OWNER = '@owner:test.twalk';

function sighting(
	contact: string,
	network: PendingContact['network'] = 'whatsapp'
): PendingContact {
	return {
		contact,
		network,
		first_seen: '2026-09-18T07:00:00.000Z',
		last_seen: '2026-09-18T09:00:00.000Z'
	};
}

function entry(
	type: Entry['subject']['type'],
	id: string,
	network: Entry['network'],
	state: Entry['state']
): Entry {
	return {
		subject: { type, id },
		network,
		state,
		decided_at: '2026-09-18T08:00:00.000Z',
		decision_sequence: 1
	};
}

function rows(
	pending: PendingContact[],
	entries: Entry[],
	names: { contact: string; display_name: string | null }[] = []
): Row[] {
	return toRows({ pending, entries, names, owner: OWNER });
}

describe('the three states', () => {
	it('keeps never-decided apart from a decision whose answer was pending', () => {
		// The distinction the whole screen turns on (ADR 0010). Two contacts,
		// both `pending`, and only one of them is waiting for the user.
		const list = rows(
			[sighting('@whatsapp_1:test.twalk')],
			[entry('contact', '@whatsapp_2:test.twalk', 'whatsapp', 'pending')]
		);
		const never = list.find((row) => row.contact === '@whatsapp_1:test.twalk');
		const answered = list.find((row) => row.contact === '@whatsapp_2:test.twalk');

		expect(never?.state).toBe('pending');
		expect(never?.decidedBy).toBe('nothing');
		expect(awaiting(never as Row)).toBe(true);

		expect(answered?.state).toBe('pending');
		expect(answered?.decidedBy).toBe('contact');
		expect(awaiting(answered as Row)).toBe(false);
	});

	it('never reports a never-decided contact as revoked', () => {
		// ADR 0010, in its own words: "an absent subject means no decision was
		// ever recorded, never a revoked one."
		const list = rows([sighting('@whatsapp_1:test.twalk')], []);
		expect(list[0].state).not.toBe('revoked');
		expect(list[0].state).toBe('pending');
	});

	it('counts waiting as a subset of pending rather than a fourth state', () => {
		const list = rows(
			[sighting('@a:test.twalk')],
			[
				entry('contact', '@b:test.twalk', 'whatsapp', 'pending'),
				entry('contact', '@c:test.twalk', 'whatsapp', 'granted'),
				entry('contact', '@d:test.twalk', 'whatsapp', 'revoked')
			]
		);
		expect(counts(list)).toEqual({ granted: 1, pending: 2, revoked: 1, awaiting: 1 });
	});
});

describe('what each state withholds', () => {
	it('says only revoked withholds content', () => {
		// ADR 0012: `pending` publishes the message and consumers refuse it;
		// `revoked` is the only one that reduces what is published at all. Copy
		// implying that undecided means unseen would be a lie.
		expect(withholds('revoked')).toBe('content');
		expect(withholds('pending')).toBe('processing');
		expect(withholds('granted')).toBe('nothing');
	});
});

describe('the precedence', () => {
	it('lets a network default answer a contact nobody decided about', () => {
		const list = rows(
			[sighting('@whatsapp_1:test.twalk')],
			[entry('network', 'whatsapp', 'whatsapp', 'granted')]
		);
		expect(list[0].state).toBe('granted');
		expect(list[0].decidedBy).toBe('network');
		expect(list[0].networkDefault).toBe('granted');
		expect(awaiting(list[0])).toBe(false);
	});

	it("lets the contact's own decision win, and says that it did", () => {
		// The case that makes a user think the product is broken: they granted
		// the whole network, and this one contact still produces nothing.
		const list = rows(
			[],
			[
				entry('network', 'whatsapp', 'whatsapp', 'granted'),
				entry('contact', '@whatsapp_1:test.twalk', 'whatsapp', 'revoked')
			]
		);
		expect(list[0].state).toBe('revoked');
		expect(list[0].decidedBy).toBe('contact');
		expect(list[0].overridesNetwork).toBe(true);
		expect(list[0].networkDefault).toBe('granted');
	});

	it('does not call an agreeing decision an override', () => {
		const list = rows(
			[],
			[
				entry('network', 'whatsapp', 'whatsapp', 'granted'),
				entry('contact', '@whatsapp_1:test.twalk', 'whatsapp', 'granted')
			]
		);
		expect(list[0].overridesNetwork).toBe(false);
	});

	it('keeps a decision per network rather than per contact', () => {
		// One person on two networks is two decisions: consent is looked up by
		// `(subject, network)` and a grant on WhatsApp says nothing about SMS.
		const list = rows(
			[sighting('@person:test.twalk', 'sms')],
			[entry('contact', '@person:test.twalk', 'whatsapp', 'granted')]
		);
		expect(list).toHaveLength(2);
		expect(list.find((row) => row.network === 'whatsapp')?.state).toBe('granted');
		expect(list.find((row) => row.network === 'sms')?.decidedBy).toBe('nothing');
	});

	it('reads the defaults off the network subjects alone', () => {
		const defaults = networkDefaults([
			entry('network', 'whatsapp', 'whatsapp', 'granted'),
			entry('contact', '@a:test.twalk', 'signal', 'revoked'),
			entry('persona', 'assistant', 'whatsapp', 'granted')
		]);
		expect([...defaults.entries()]).toEqual([['whatsapp', 'granted']]);
	});
});

describe('the list', () => {
	it('is the union of who is waiting and who was decided about', () => {
		// Either list alone loses half the screen: the pending projection knows
		// nothing about a contact already decided, and the consent state knows
		// nothing about one who has only just written.
		const list = rows(
			[sighting('@waiting:test.twalk')],
			[entry('contact', '@decided:test.twalk', 'whatsapp', 'granted')]
		);
		expect(list.map((row) => row.contact)).toEqual([
			'@waiting:test.twalk',
			'@decided:test.twalk'
		]);
	});

	it('puts the rows that need a decision first', () => {
		const list = rows(
			[sighting('@zz:test.twalk')],
			[entry('contact', '@aa:test.twalk', 'whatsapp', 'granted')]
		);
		expect(list[0].contact).toBe('@zz:test.twalk');
	});

	it('leaves personas out: they have a screen of their own', () => {
		// Activating a persona is a consent decision (ADR 0013) and it is not a
		// person. A persona in this list would be somebody to decide about.
		const list = rows([], [entry('persona', 'assistant', 'whatsapp', 'granted')]);
		expect(list).toEqual([]);
	});

	it('reads a network subject as a default rather than as a row', () => {
		const list = rows(
			[sighting('@a:test.twalk')],
			[entry('network', 'whatsapp', 'whatsapp', 'revoked')]
		);
		expect(list).toHaveLength(1);
		expect(list[0].contact).toBe('@a:test.twalk');
		expect(list[0].networkDefault).toBe('revoked');
	});

	it('shows a name when the bus still had one, and says when it did not', () => {
		const list = rows(
			[sighting('@whatsapp_33612345678:test.twalk'), sighting('@whatsapp_999:test.twalk')],
			[],
			[
				{ contact: '@whatsapp_33612345678:test.twalk', display_name: 'Aïcha' },
				{ contact: '@whatsapp_999:test.twalk', display_name: null }
			]
		);
		const named = list.find((row) => row.label === 'Aïcha');
		expect(named?.labelSource).toBe('display-name');
		const unnamed = list.find((row) => row.contact === '@whatsapp_999:test.twalk');
		expect(unnamed?.label).toBe('@whatsapp_999:test.twalk');
		expect(unnamed?.labelSource).toBe('matrix-id');
	});
});

describe('the search', () => {
	const list = rows(
		[sighting('@whatsapp_33612345678:test.twalk'), sighting('@signal_abc:test.twalk', 'signal')],
		[],
		[{ contact: '@whatsapp_33612345678:test.twalk', display_name: 'Aïcha Lorré' }]
	);

	it('matches a name typed the way it sounds', () => {
		const hit = list.filter((row) => matchesQuery(row, 'lorre'));
		expect(hit.map((row) => row.contact)).toEqual(['@whatsapp_33612345678:test.twalk']);
	});

	it('matches the phone number a bridged ghost carries in its localpart', () => {
		// On a phone-based network this is how a user actually looks somebody up.
		const hit = list.filter((row) => matchesQuery(row, '33612345678'));
		expect(hit).toHaveLength(1);
	});

	it('shows everything for an empty query', () => {
		expect(list.filter((row) => matchesQuery(row, '   '))).toHaveLength(2);
	});
});

describe('the bulk control', () => {
	const list = rows(
		[
			sighting('@whatsapp_1:test.twalk'),
			sighting('@whatsapp_2:test.twalk'),
			sighting('@signal_3:test.twalk', 'signal')
		],
		[]
	);

	it('acts on what the filter shows and on nothing else', () => {
		// #137's rule, on a screen where "grant all" over a 246-member
		// association is the affordance it exists to avoid.
		const shown = list.filter((row) => matchesQuery(row, 'whatsapp'));
		expect(shown).toHaveLength(2);
		const decisions = bulkDecisions(shown, 'granted');
		expect(decisions).toHaveLength(2);
		expect(decisions.map((decision) => decision.contact)).not.toContain('@signal_3:test.twalk');
	});

	it('counts exactly what it will write', () => {
		// The number on the button is this array's length, so a control that
		// said "grant 4" and wrote 3 would be a failing test rather than a
		// screen nobody checked.
		const decisions = bulkDecisions(list, 'granted');
		expect(decisions).toHaveLength(list.length);
	});

	it('skips a contact already in that state by its own decision', () => {
		const decided = rows(
			[sighting('@whatsapp_1:test.twalk')],
			[entry('contact', '@whatsapp_1:test.twalk', 'whatsapp', 'granted')]
		);
		expect(bulkDecisions(decided, 'granted')).toEqual([]);
		expect(bulkDecisions(decided, 'revoked')).toHaveLength(1);
	});

	it('still writes a contact that holds the state only by its network default', () => {
		// Asking for it per contact is asking for a decision that survives the
		// network default changing, which is a different fact from inheriting it.
		const inherited = rows(
			[sighting('@whatsapp_1:test.twalk')],
			[entry('network', 'whatsapp', 'whatsapp', 'granted')]
		);
		expect(bulkDecisions(inherited, 'granted')).toHaveLength(1);
	});

	it('never writes a decision about the owner', () => {
		const withOwner = rows([sighting(OWNER)], []);
		expect(bulkDecisions(withOwner, 'granted')).toEqual([]);
	});
});

describe('the owner, who is not a contact', () => {
	it('is flagged rather than hidden', () => {
		// ADR 0018 and ADR 0021: the owner has no consent state. Since #149 a
		// current Gateway serves no such row, so this synthetic one is the only
		// thing that can still exercise the label — and the label stays,
		// because an older Gateway behind this Companion does serve one and
		// filtering it in the screen would hide the only symptom a user can
		// see.
		const list = rows([sighting(OWNER), sighting('@whatsapp_1:test.twalk')], []);
		expect(list).toHaveLength(2);
		expect(ownerRows(list).map((row) => row.contact)).toEqual([OWNER]);
	});

	it('flags nothing when this browser does not know who the owner is', () => {
		const list = toRows({ pending: [sighting(OWNER)], entries: [], names: [], owner: null });
		expect(ownerRows(list)).toEqual([]);
	});
});
