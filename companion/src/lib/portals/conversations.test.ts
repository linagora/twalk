// The classification and the grouping, against the account that motivated the
// ticket (#143).
//
// The fixture is not invented: it is the eighteen portal rooms one personal
// WhatsApp account produced on the reference deployment, with the names, the
// member counts and the creation order #105's own measurements recorded. The
// network identifiers are spelled the way WhatsApp spells them, which is the
// only part the measurement did not print per room.
//
// It is worth keeping as the fixture because every awkward case in the ticket
// is in it: two rooms called `Communauté CKCP` created in the same minute with
// 109 members and 6, three called `XVDSI` (309, 7, and `XVDSI - General` at
// 221), two called `Échecs en Yvelines` (11 and 246), a status broadcast, and
// a 246-member association sitting in the same list as a two-person
// conversation with a family member.

import { describe, expect, it } from 'vitest';

import {
	conversationKind,
	families,
	matching,
	rows,
	sections,
	stem,
	type Portal
} from './conversations';

/** One portal, spelled the way `GET /api/portals` spells one. */
function portal(
	name: string | null,
	networkConversationId: string | null,
	members: number,
	observation: Portal['observation'] = 'absent'
): Portal {
	return {
		room_id: `!${name ?? 'nameless'}-${members}:twalk.localhost`,
		bridge_id: 'mautrix-whatsapp',
		network: 'whatsapp',
		name,
		network_conversation_id: networkConversationId,
		members,
		observation
	};
}

/**
 * The eighteen rooms, in the order they were created between 04:54 and 13:24.
 * That order is the ticket's other point — the set grows all day — and it is
 * also a fair test of a sort that must not depend on it.
 */
const ACCOUNT: Portal[] = [
	portal('maria (WA)', '33612345678@s.whatsapp.net', 2, 'observing'),
	portal('Alexandre Zapolsky (WA)', '231546065817642@lid', 2),
	portal('WhatsApp Status Broadcast', 'status@broadcast', 0),
	portal('Victoire (WA)', '33698765432@s.whatsapp.net', 2),
	portal('Communauté CKCP', '120363201980306353@g.us', 109),
	portal('Communauté CKCP', '120363111122223333@g.us', 6),
	portal('XVDSI - General', '120363444455556666@g.us', 221),
	portal('XVDSI', '120363777788889999@g.us', 7),
	portal('XVDSI', '120363000011112222@g.us', 309),
	portal('Linagora : Team Clean', '120363123412341234@g.us', 7),
	portal('+33620856621 (WA)', '33620856621@s.whatsapp.net', 2),
	portal('Échanges Association Vivre à Chapet', '120363555566667777@g.us', 189),
	portal('Association « Vivre à Chapet »', '120363888899990000@g.us', 3),
	portal("Amies & amis de l'Échiquier", '120363246824682468@g.us', 136),
	portal('Échecs en Yvelines', '120363135713571357@g.us', 11),
	portal('Recrut Gouvernante', '120363864286428642@g.us', 7),
	portal('Échecs en Yvelines', '120363975319753197@g.us', 246),
	portal('RAG I Infos', '120363159715971597@g.us', 73)
];

describe('conversationKind', () => {
	it('reads the network’s own suffix, for each spelling WhatsApp uses', () => {
		expect(conversationKind('33612345678@s.whatsapp.net')).toBe('one-to-one');
		expect(conversationKind('231546065817642@lid')).toBe('one-to-one');
		expect(conversationKind('33612345678@c.us')).toBe('one-to-one');
		expect(conversationKind('120363201980306353@g.us')).toBe('group');
		expect(conversationKind('status@broadcast')).toBe('broadcast');
		expect(conversationKind('120363201980306353@newsletter')).toBe('broadcast');
	});

	it('reads Signal’s two shapes: an ACI is a person, a master key is a group', () => {
		expect(conversationKind('8f3a1c2e-4b5d-6e7f-8091-a2b3c4d5e6f7')).toBe('one-to-one');
		expect(conversationKind('a'.repeat(64))).toBe('group');
	});

	it('says nothing rather than guessing when the network said nothing', () => {
		// The honest answers. A member count is evidence about a
		// conversation's size, never about its type: inferring `group` from
		// "lots of people" is the guessing this ticket exists to avoid.
		expect(conversationKind(null)).toBe('unstated');
		expect(conversationKind('')).toBe('unstated');
		expect(conversationKind('   ')).toBe('unstated');
		expect(conversationKind('some-opaque-gmessages-conversation-id')).toBe('unstated');
	});

	it('is decided by the suffix and never by the size', () => {
		// A group of one is a group somebody left; a one-to-one with a large
		// count would be a bug somewhere, and would still be a one-to-one.
		expect(conversationKind('120363201980306353@g.us')).toBe('group');
		expect(conversationKind('33612345678@s.whatsapp.net')).toBe('one-to-one');
	});

	it('is not fooled by a suffix that ends like a shorter one', () => {
		// `…@s.whatsapp.net` must never be matched by a `…@lid`-shaped rule or
		// by anything else that happens to end the same way.
		expect(conversationKind('X@LID')).toBe('one-to-one');
		expect(conversationKind('X@S.WhatsApp.net')).toBe('one-to-one');
	});
});

describe('the rows', () => {
	const all = rows(ACCOUNT);

	it('labels a conversation by its name, and says when it could not', () => {
		const maria = all.find((row) => row.label === 'maria (WA)');
		expect(maria?.labelSource).toBe('name');

		// A portal the bridge named nothing falls back to the network's own
		// address for it, which is at least something a user can recognise —
		// and says that is what it did.
		const [nameless] = rows([portal(null, '33699998888@s.whatsapp.net', 2)]);
		expect(nameless?.label).toBe('33699998888@s.whatsapp.net');
		expect(nameless?.labelSource).toBe('network-id');

		const [anonymous] = rows([portal(null, null, 2)]);
		expect(anonymous?.labelSource).toBe('room-id');
	});

	it('keeps the member count the register gave, untouched', () => {
		expect(all.find((row) => row.members === 246)?.label).toBe('Échecs en Yvelines');
	});

	it('orders two same-named conversations stably, smallest first', () => {
		const yvelines = all.filter((row) => row.label === 'Échecs en Yvelines');
		expect(yvelines.map((row) => row.members)).toEqual([11, 246]);
	});
});

describe('the search', () => {
	const all = rows(ACCOUNT);

	it('finds a conversation by its name, however it is accented or cased', () => {
		expect(matching(all, 'echecs').map((row) => row.members)).toEqual([11, 246]);
		expect(matching(all, 'ÉCHECS').map((row) => row.members)).toEqual([11, 246]);
	});

	it('finds a one-to-one by the participant’s name, which is its name', () => {
		// The register carries no member identities at all, by design. For a
		// one-to-one portal mautrix names the room after the contact, which is
		// what makes searching by a participant work here.
		expect(matching(all, 'zapolsky').map((row) => row.label)).toEqual([
			'Alexandre Zapolsky (WA)'
		]);
	});

	it('finds a conversation by the network’s own address for it', () => {
		// Which is the only way to reach one of two identically named rows
		// directly, and the reason that id is on the screen at all.
		expect(matching(all, '120363975319753197').map((row) => row.members)).toEqual([246]);
	});

	it('shows everything for an empty search, and nothing for a miss', () => {
		expect(matching(all, '')).toHaveLength(18);
		expect(matching(all, '   ')).toHaveLength(18);
		expect(matching(all, 'nobody writes this')).toHaveLength(0);
	});
});

describe('stem', () => {
	it('strips the punctuation two names of one community disagree about', () => {
		expect(stem('Association « Vivre à Chapet »')).toBe('association vivre a chapet');
		expect(stem('XVDSI - General')).toBe('xvdsi general');
		expect(stem("Amies & amis de l'Échiquier")).toBe('amies amis de l echiquier');
	});
});

describe('families', () => {
	const grouped = families(rows(ACCOUNT).filter((row) => row.kind === 'group'));
	const named = (name: string) => grouped.find((family) => family.name === name);

	it('puts the community and its announcement group together', () => {
		// The pair created in the same minute, 109 members and 6. Presenting
		// them as two rows with the same name is the defect the ticket names.
		const ckcp = named('Communauté CKCP');
		expect(ckcp?.rows.map((row) => row.members)).toEqual([6, 109]);
		expect(ckcp?.people).toBe(115);
	});

	it('pulls in a subgroup whose name extends the community’s', () => {
		const xvdsi = named('XVDSI');
		expect(xvdsi?.rows.map((row) => `${row.label}:${row.members}`)).toEqual([
			'XVDSI:7',
			'XVDSI:309',
			'XVDSI - General:221'
		]);
		expect(xvdsi?.people).toBe(537);
	});

	it('relates two names by containment, not by a shared prefix', () => {
		// `Association « Vivre à Chapet »` sits *inside*
		// `Échanges Association Vivre à Chapet` rather than at its start, and a
		// prefix rule would have missed it.
		const chapet = named('Association « Vivre à Chapet »');
		expect(chapet?.rows.map((row) => row.members)).toEqual([3, 189]);
	});

	it('leaves a group nobody’s name claims on its own', () => {
		expect(named('RAG I Infos')?.rows).toHaveLength(1);
		expect(named("Amies & amis de l'Échiquier")?.rows).toHaveLength(1);
		expect(named('Recrut Gouvernante')?.rows).toHaveLength(1);
		expect(named('Linagora : Team Clean')?.rows).toHaveLength(1);
	});

	it('accounts for every group exactly once', () => {
		const groups = rows(ACCOUNT).filter((row) => row.kind === 'group');
		const seen = grouped.flatMap((family) => family.rows.map((row) => row.roomId));
		expect(seen).toHaveLength(groups.length);
		expect(new Set(seen).size).toBe(groups.length);
	});

	it('never joins two bridges’ conversations', () => {
		const signalGroup = portal('Famille', 'a1b2c3d4'.repeat(8), 5);
		const shared = [
			portal('Famille', '120363111111111111@g.us', 5),
			{ ...signalGroup, bridge_id: 'mautrix-signal', network: 'signal' as const }
		];
		expect(families(rows(shared).filter((row) => row.kind === 'group'))).toHaveLength(2);
	});

	it('does not let a two-letter name pull other conversations in', () => {
		const noisy = [
			portal('AB', '120363111111111111@g.us', 4),
			portal('AB CD', '120363222222222222@g.us', 40),
			portal('XY AB', '120363333333333333@g.us', 400)
		];
		expect(families(rows(noisy).filter((row) => row.kind === 'group'))).toHaveLength(3);
	});
});

describe('sections', () => {
	const grouped = sections(rows(ACCOUNT));

	it('groups the account as the ticket asks: one-to-one, group, community', () => {
		expect(grouped.oneToOne.map((row) => row.label).sort()).toEqual([
			'+33620856621 (WA)',
			'Alexandre Zapolsky (WA)',
			'Victoire (WA)',
			'maria (WA)'
		]);
		// Four communities: the three the owner identified by name, plus the
		// Vivre à Chapet pair.
		expect(grouped.communities.map((family) => family.name).sort()).toEqual([
			'Association « Vivre à Chapet »',
			'Communauté CKCP',
			'XVDSI',
			'Échecs en Yvelines'
		]);
		expect(grouped.groups.map((row) => row.label).sort()).toEqual([
			"Amies & amis de l'Échiquier",
			'Linagora : Team Clean',
			'RAG I Infos',
			'Recrut Gouvernante'
		]);
		expect(grouped.broadcasts.map((row) => row.label)).toEqual(['WhatsApp Status Broadcast']);
		expect(grouped.unstated).toHaveLength(0);
	});

	it('puts a conversation in exactly one section', () => {
		const everywhere = [
			...grouped.oneToOne,
			...grouped.groups,
			...grouped.communities.flatMap((family) => family.rows),
			...grouped.broadcasts,
			...grouped.unstated
		].map((row) => row.roomId);
		expect(everywhere).toHaveLength(18);
		expect(new Set(everywhere).size).toBe(18);
	});

	it('sections the filtered list, so what is grouped is what is shown', () => {
		// The bulk control's scope and the sections have to agree: a family
		// header that counted rows the search hid would name people the user
		// cannot see.
		const shown = sections(matching(rows(ACCOUNT), 'xvdsi'));
		expect(shown.communities).toHaveLength(1);
		expect(shown.communities[0]?.rows).toHaveLength(3);
		expect(shown.oneToOne).toHaveLength(0);
	});

	it('gives a conversation the network did not classify a section of its own', () => {
		const grouped = sections(rows([portal('Somebody', null, 4)]));
		expect(grouped.unstated.map((row) => row.label)).toEqual(['Somebody']);
		expect(grouped.groups).toHaveLength(0);
		expect(grouped.oneToOne).toHaveLength(0);
	});
});
