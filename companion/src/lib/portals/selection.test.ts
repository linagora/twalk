// The consequence of a tick, which is the criterion that is not ergonomics
// (#143).

import { describe, expect, it } from 'vitest';

import { rows, type Portal } from './conversations';
import { batched, consequence, observedNow, requests } from './selection';

function portal(
	name: string,
	id: string,
	members: number,
	observation: Portal['observation'] = 'absent'
): Portal {
	return {
		room_id: `!${name}-${members}:twalk.localhost`,
		bridge_id: 'mautrix-whatsapp',
		network: 'whatsapp',
		name,
		network_conversation_id: id,
		members,
		observation,
		moved_from: null,
		unreadable: null
	};
}

/** The default the Gateway serves, as the reference deployment measured it (#143). */
const CROWD_THRESHOLD = 20;

/** maria, already observed; a team; and the 246-member association. */
const ACCOUNT = rows([
	portal('maria (WA)', '33612345678@s.whatsapp.net', 2, 'observing'),
	portal('Linagora : Team Clean', '120363123412341234@g.us', 7),
	portal('Échecs en Yvelines', '120363975319753197@g.us', 246),
	portal('Communauté CKCP', '120363201980306353@g.us', 109)
]);

const id = (label: string) => ACCOUNT.find((row) => row.label === label)?.roomId ?? '';

describe('observedNow', () => {
	it('starts from what is already true, so nothing is re-decided', () => {
		expect([...observedNow(ACCOUNT)]).toEqual([id('maria (WA)')]);
	});

	it('counts an invited conversation as observed', () => {
		// The user decided, the invitation is out. Offering the tick again
		// would be offering a decision they already made; that it has not
		// completed is a diagnosis about SENSOR_ALLOWED_INVITERS (ADR 0024),
		// not a reason to untick.
		const invited = rows([portal('Victoire (WA)', '33698765432@s.whatsapp.net', 2, 'invited')]);
		expect(observedNow(invited).size).toBe(1);
	});
});

describe('consequence', () => {
	it('is empty when nothing would change', () => {
		const nothing = consequence(ACCOUNT, observedNow(ACCOUNT), CROWD_THRESHOLD);
		expect(nothing.empty).toBe(true);
		expect(nothing.people).toBe(0);
		expect(nothing.acknowledgementNeeded).toBe(false);
	});

	it('states how many people a tick covers, before it is applied', () => {
		const one = consequence(
			ACCOUNT,
			new Set([id('maria (WA)'), id('Échecs en Yvelines')]),
			CROWD_THRESHOLD
		);
		expect(one.starting).toBe(1);
		expect(one.people).toBe(246);
		expect(one.largest).toEqual({ label: 'Échecs en Yvelines', members: 246 });
	});

	it('names the largest rather than letting a total hide it', () => {
		// 355 reads as a big number; "355, the largest being Échecs en
		// Yvelines with 246" reads as a decision about an association.
		const family = consequence(
			ACCOUNT,
			new Set([id('maria (WA)'), id('Échecs en Yvelines'), id('Communauté CKCP')]),
			CROWD_THRESHOLD
		);
		expect(family.people).toBe(355);
		expect(family.largest?.members).toBe(246);
		expect(family.crowds.map((row) => row.members)).toEqual([246, 109]);
	});

	it('asks to be acknowledged once any conversation is a crowd', () => {
		const team = consequence(
			ACCOUNT,
			new Set([id('maria (WA)'), id('Linagora : Team Clean')]),
			CROWD_THRESHOLD
		);
		expect(team.people).toBe(7);
		expect(team.acknowledgementNeeded).toBe(false);

		const association = consequence(
			ACCOUNT,
			new Set([id('Échecs en Yvelines')]),
			CROWD_THRESHOLD
		);
		expect(association.acknowledgementNeeded).toBe(true);
	});

	it('does not gate removals, because removing is always safe', () => {
		// Untick the only observed conversation: two people stop being read,
		// and nothing has to be acknowledged to stop reading them.
		const stopping = consequence(ACCOUNT, new Set(), CROWD_THRESHOLD);
		expect(stopping.stopping).toBe(1);
		expect(stopping.starting).toBe(0);
		expect(stopping.people).toBe(0);
		expect(stopping.acknowledgementNeeded).toBe(false);
		expect(stopping.empty).toBe(false);
	});

	it('costs the whole decision, not the part a search left on screen', () => {
		// The selection is made across searches. Costing only the filtered
		// rows would state a number for some of what is about to happen.
		const shown = ACCOUNT.filter((row) => row.label === 'Linagora : Team Clean');
		const wholeAccount = consequence(
			ACCOUNT,
			new Set([id('Linagora : Team Clean'), id('Échecs en Yvelines')]),
			CROWD_THRESHOLD
		);
		const filteredOnly = consequence(
			shown,
			new Set([id('Linagora : Team Clean'), id('Échecs en Yvelines')]),
			CROWD_THRESHOLD
		);
		expect(wholeAccount.people).toBe(253);
		expect(filteredOnly.people).toBe(7);
	});

	it('draws the crowds from the served threshold, not from a number of its own', () => {
		// The Gateway owns the threshold (#252): the same selection is a crowd
		// under one served value and a list under another, and this screen
		// has nothing to say about which.
		const selection = new Set([id('Échecs en Yvelines')]);
		expect(consequence(ACCOUNT, selection, 20).crowds.map((row) => row.label)).toEqual([
			'Échecs en Yvelines'
		]);
		expect(consequence(ACCOUNT, selection, 20).acknowledgementNeeded).toBe(true);
		expect(consequence(ACCOUNT, selection, 300).crowds).toEqual([]);
		expect(consequence(ACCOUNT, selection, 300).acknowledgementNeeded).toBe(false);
	});
});

describe('requests', () => {
	it('sends nothing when nothing changed', () => {
		expect(requests(ACCOUNT, observedNow(ACCOUNT))).toEqual([]);
	});

	it('sends one call per direction, additions first', () => {
		const calls = requests(ACCOUNT, new Set([id('Échecs en Yvelines')]));
		expect(calls).toEqual([
			{ observed: true, rooms: [id('Échecs en Yvelines')] },
			{ observed: false, rooms: [id('maria (WA)')] }
		]);
	});

	it('never names a conversation that is already where it should be', () => {
		const calls = requests(ACCOUNT, new Set([id('maria (WA)'), id('Communauté CKCP')]));
		expect(calls).toEqual([{ observed: true, rooms: [id('Communauté CKCP')] }]);
	});
});

describe('batched', () => {
	it('keeps a request inside the Gateway’s own cap', () => {
		// An old account with hundreds of conversations is exactly who would
		// meet `maxItems: 256`, so the screen splits rather than discovering it
		// as a 400.
		const many = Array.from({ length: 600 }, (_, at) => `!room${at}:twalk.localhost`);
		const batches = batched(many);
		expect(batches.map((batch) => batch.length)).toEqual([256, 256, 88]);
		expect(batches.flat()).toEqual(many);
	});

	it('is one batch, or none, for the ordinary case', () => {
		expect(batched(['!a:x'])).toEqual([['!a:x']]);
		expect(batched([])).toEqual([]);
	});
});
