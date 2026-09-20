// Which conversations a network is observed on, and how to present them so
// the user can tell one decision from another (ticket #143).
//
// # The decision this screen introduces
//
// Twalk decides in two units today: consent, per contact, and persona
// activation, per network. Observation is a third — **per conversation** —
// and the reason it has to exist is in the register's own numbers. On the
// reference deployment, one personal WhatsApp account produced eighteen
// portal rooms holding roughly 1,300 memberships:
//
//     maria (WA)                              2 members
//     Alexandre Zapolsky (WA)                 2
//     Linagora : Team Clean                   7
//     Échecs en Yvelines                     11
//     Communauté CKCP                       109
//     XVDSI                                 309
//     Échecs en Yvelines                    246
//
// For `maria`, a contact and a conversation are the same thing and consent
// per contact works. For `Échecs en Yvelines` at 246 members, deciding per
// contact is unusable — nobody adjudicates 246 people one by one — and
// ticking the row is a decision **about 246 people in one gesture**. Those
// two rows are not the same decision, and everything in this module exists to
// stop them looking like it.
//
// # Classification, and the one place it stops
//
// The kind of a conversation is not guessed. Every portal carries the
// network's own identifier for it (`network_conversation_id`, the `channel.id`
// the bridge wrote into `m.bridge`, passed through untouched by the Gateway),
// and its **suffix** is the network's own statement of what it addresses:
// `@lid` and `@s.whatsapp.net` are one person, `@g.us` is a group,
// `@newsletter` is a broadcast. [`conversationKind`] reads that table and
// nothing else. A portal whose bridge wrote no id, or wrote one this version
// does not recognise, is `unstated` — said plainly, never inferred from the
// member count, because a headcount is evidence about a conversation's size
// and not about its type.
//
// Where it stops is **communities**, and this is the honest part. A WhatsApp
// community is, at the network level, a set of ordinary groups: its parent,
// its announcement group and its subgroups all carry `@g.us`, and no field in
// `m.bridge` says "this group belongs to community X". So the suffix cannot
// tell them apart, and neither can the Gateway.
//
// What the register *can* see is the thing the owner saw when they worked out
// what those eighteen rooms were (#105): a community arrives as several
// groups whose names are the same or contain one another.
//
//     06:46  Communauté CKCP     109      06:46  Communauté CKCP       6
//     08:19  XVDSI               309      08:18  XVDSI - General     221
//     13:05  Échecs en Yvelines  246      10:35  Échecs en Yvelines   11
//
// [`families`] clusters on exactly that — whole-word containment of one
// folded name in another — and a cluster of two or more groups is presented
// as a community. It is inference **from the names**, which is what the
// evidence consists of, and it is stated as such on the screen: Twalk does
// not claim which row is the parent and which the announcement group, because
// nothing in the data says. Each row keeps its own member count and its own
// network identifier, which is what lets the user tell two rows called
// `XVDSI` apart at all.
//
// Its limit is stated too, rather than hidden: a community's subgroup with an
// unrelated name (`Échanges Association Vivre à Chapet`, 189) is
// indistinguishable from an ordinary group and appears as one.
//
// # Reuse, not a second implementation
//
// The search and the bulk control are `$lib/matrix/rooms.ts`'s, reached
// through [`asRoom`]: one `matchesQuery`, one `fold`, one
// `scopedBulkControl`, one `roomLabel`. A second implementation of a matching
// rule is how #110 happened, and this list is longer and more dangerous than
// the one that rule was written for.

import { fold, matchesQuery, roomLabel, type RoomSummary } from '$lib/matrix/rooms';
import type { components } from '$lib/api/schema';

export type Portal = components['schemas']['Portal'];
export type Observation = Portal['observation'];

/**
 * What the network says it is addressing, read off the suffix of its own
 * identifier for the conversation.
 *
 * `unstated` is a real answer and not a bucket for failures: a bridge that
 * wrote no identifier, or one this version has no rule for, has told us
 * nothing about the kind, and saying so is the only truthful option. The
 * screen renders it as its own section rather than folding it into `group`,
 * because guessing `group` for a conversation that might be one person is the
 * error that matters in the direction that matters.
 */
export type ConversationKind = 'one-to-one' | 'group' | 'broadcast' | 'unstated';

/**
 * The suffixes, per network, exactly as the networks spell them.
 *
 * WhatsApp's are whatsmeow's JID servers, which is what mautrix-whatsapp
 * writes: `s.whatsapp.net` for a phone-addressed person, `lid` for the
 * linked-identity addressing WhatsApp moved to, `c.us` for the legacy
 * spelling still seen on older portals, `g.us` for a group, `broadcast` for a
 * status broadcast or a broadcast list, `newsletter` for a WhatsApp Channel.
 *
 * Ordered longest-suffix-first so `s.whatsapp.net` is never matched by a
 * shorter rule that happens to end the same way.
 */
const SUFFIX_KINDS: ReadonlyArray<readonly [string, ConversationKind]> = [
	['@s.whatsapp.net', 'one-to-one'],
	['@newsletter', 'broadcast'],
	['@broadcast', 'broadcast'],
	['@c.us', 'one-to-one'],
	['@lid', 'one-to-one'],
	['@g.us', 'group']
];

/** A Signal ACI: one person, and the only thing a UUID is ever used for. */
const SIGNAL_ACI = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/iu;
/** A Signal group identifier: the 32-byte master key, hex or base64. */
const SIGNAL_GROUP = /^(?:[0-9a-f]{64}|[A-Za-z0-9+/]{43}=)$/u;

/**
 * The kind of conversation the network's own identifier names.
 *
 * Pure, total, and it never consults the member count: a two-person group and
 * a 300-person group are the same kind of decision differently sized, and a
 * 1-member group is a group somebody left.
 */
export function conversationKind(networkConversationId: string | null): ConversationKind {
	if (networkConversationId === null) {
		return 'unstated';
	}
	const id = networkConversationId.trim();
	if (id === '') {
		return 'unstated';
	}
	const lowered = id.toLowerCase();
	for (const [suffix, kind] of SUFFIX_KINDS) {
		if (lowered.endsWith(suffix)) {
			return kind;
		}
	}
	if (SIGNAL_ACI.test(id)) {
		return 'one-to-one';
	}
	if (SIGNAL_GROUP.test(id)) {
		return 'group';
	}
	return 'unstated';
}

/** One conversation, as this screen needs to render and decide about it. */
export interface ConversationRow {
	readonly roomId: string;
	readonly bridgeId: string;
	readonly network: string;
	/** What to call it: its name, its network address, or its room id. */
	readonly label: string;
	/** Which of those three the label is, so the screen can say so. */
	readonly labelSource: 'name' | 'network-id' | 'room-id';
	readonly kind: ConversationKind;
	/** The network's own id, kept so two same-named rows are distinguishable. */
	readonly networkConversationId: string | null;
	/** People in the conversation, excluding the bridge bot and the Sensor. */
	readonly members: number;
	readonly observation: Observation;
	/**
	 * The room this conversation lived in before it was replaced, or `null`
	 * for one that never moved (ADR 0029, #256). The screen says so on the
	 * row — a deployment that changed rooms under the user must be able to
	 * say it — and never lists the dead room: the register already folded it.
	 */
	readonly movedFrom: string | null;
	/**
	 * The very summary [`asRoom`] built, carried rather than rebuilt.
	 *
	 * So that labelling and searching are two calls against **one** adapted
	 * value: a row whose search shape differed from its label shape would be a
	 * row a user can see and cannot find.
	 */
	readonly room: RoomSummary;
}

/**
 * A portal in the shape `$lib/matrix/rooms.ts` already knows how to search
 * and label. The mapping is deliberate, field by field:
 *
 * - `name` is the room's name, as there.
 * - `alias` is the **network's own identifier**. An alias is Matrix's "another
 *   address for this room", and a portal's other address is the network's:
 *   `33612345678@s.whatsapp.net` is how WhatsApp addresses that conversation.
 *   So a search finds a conversation by the id shown beside it, and a portal
 *   the bridge named nothing falls back to that id rather than to a room id
 *   nobody can read.
 * - `heroes` is empty, and not because it was awkward to fill. The register
 *   reads room state and forgets it; it carries no member identities at all,
 *   by design. For a one-to-one portal the participant's display name **is**
 *   the room name (mautrix names the portal after the contact), which is what
 *   makes searching "on a participant's display name" work at all here.
 * - `encrypted` is true: a portal room is end-to-end encrypted by
 *   construction (`CONTEXT.md`), so this is a fact and not a default.
 * - `type` is `null`: a portal is an ordinary room a bridge marked with
 *   `m.bridge`, not a room Matrix types as something other than a
 *   conversation. Every portal the register hands over *is* a conversation —
 *   that is what the register's `m.bridge` check already decided — so this is
 *   a fact about portals and not a default either.
 */
export function asRoom(portal: Portal): RoomSummary {
	return {
		roomId: portal.room_id,
		name: portal.name,
		alias: portal.network_conversation_id,
		encrypted: true,
		heroes: [],
		joinedMembers: portal.members,
		type: null
	};
}

/** Every portal as a row, in a stable order: by kind, then by name. */
export function rows(portals: readonly Portal[]): ConversationRow[] {
	return portals
		.map((portal) => {
			const room = asRoom(portal);
			const label = roomLabel(room);
			return {
				room,
				roomId: portal.room_id,
				bridgeId: portal.bridge_id,
				network: portal.network,
				label: label.text,
				labelSource:
					label.source === 'name'
						? ('name' as const)
						: label.source === 'alias'
							? ('network-id' as const)
							: ('room-id' as const),
				kind: conversationKind(portal.network_conversation_id),
				networkConversationId: portal.network_conversation_id,
				members: portal.members,
				observation: portal.observation,
				movedFrom: portal.moved_from
			};
		})
		.sort(byLabel);
}

function byLabel(left: ConversationRow, right: ConversationRow): number {
	// Then by member count, so the two rows called `XVDSI` come out in a
	// stable order rather than the homeserver's.
	return left.label.localeCompare(right.label) || left.members - right.members;
}

/** Only the rows a search leaves in. `matchesQuery`'s rule, unchanged. */
export function matching(all: readonly ConversationRow[], query: string): ConversationRow[] {
	return all.filter((row) => matchesQuery(row.room, query));
}

/**
 * A community: two or more groups whose names say they belong together, or a
 * lone group, which is the same structure with one row in it.
 */
export interface ConversationFamily {
	/** Stable across renders and searches: the folded stem they share. */
	readonly key: string;
	/** The shortest name in the family, as its bridge wrote it. */
	readonly name: string;
	readonly rows: readonly ConversationRow[];
	/** People across the whole family. What ticking all of it covers. */
	readonly people: number;
}

/** Below this, a shared name is a coincidence rather than a community. */
const MIN_STEM = 3;

/**
 * Clusters group conversations into families by name.
 *
 * Whole-word containment of one folded, punctuation-stripped name in another,
 * within one bridge. `Communauté CKCP` and `Communauté CKCP` are one family;
 * so are `XVDSI` and `XVDSI - General`, and `Association « Vivre à Chapet »`
 * and `Échanges Association Vivre à Chapet`. A name shorter than
 * [`MIN_STEM`] letters never pulls another in, because at two characters
 * containment stops meaning anything.
 *
 * It is inference from names and nothing else, which is what the evidence is:
 * the network tells Twalk that all of these are groups and does not say which
 * groups form a community. The screen says that, and never labels a row
 * parent or announcement group.
 *
 * Quadratic in the number of groups, deliberately: it is called on every
 * keystroke, and the account the ticket is about has eighteen conversations
 * while "hundreds" is the worst case it names. Three hundred groups is ninety
 * thousand `indexOf` calls on short strings — under a millisecond — and an
 * index keyed on something would have to guess in advance which substring two
 * names might share, which is the thing there is no rule for.
 */
export function families(groups: readonly ConversationRow[]): ConversationFamily[] {
	const stems = groups.map((row) => stem(row.label));
	// Union-find, because containment is not transitive on its own and a
	// family of three has to survive being discovered in any order.
	const parent = groups.map((_, index) => index);
	const find = (index: number): number => {
		let at = index;
		while (parent[at] !== at) {
			at = parent[at] as number;
		}
		return at;
	};
	const union = (left: number, right: number): void => {
		const a = find(left);
		const b = find(right);
		if (a !== b) {
			parent[b] = a;
		}
	};
	for (let i = 0; i < groups.length; i += 1) {
		for (let j = i + 1; j < groups.length; j += 1) {
			if (groups[i]?.bridgeId !== groups[j]?.bridgeId) {
				continue;
			}
			if (related(stems[i] as string, stems[j] as string)) {
				union(i, j);
			}
		}
	}

	const grouped = new Map<number, ConversationRow[]>();
	groups.forEach((row, index) => {
		const root = find(index);
		const bucket = grouped.get(root);
		if (bucket === undefined) {
			grouped.set(root, [row]);
		} else {
			bucket.push(row);
		}
	});

	return [...grouped.entries()]
		.map(([root, members]) => {
			const shortest = members.reduce((best, row) =>
				stem(row.label).length <= stem(best.label).length ? row : best
			);
			return {
				key: `${members[0]?.bridgeId ?? ''}:${stem(shortest.label)}:${stems[root] ?? root}`,
				name: shortest.label,
				rows: [...members].sort(byLabel),
				people: members.reduce((sum, row) => sum + row.members, 0)
			};
		})
		.sort((left, right) => left.name.localeCompare(right.name));
}

/**
 * A name reduced to what two conversations could plausibly share: folded by
 * `$lib/matrix/rooms.ts`'s one folding rule, then with every run of
 * punctuation turned into a single space.
 *
 * The punctuation step is what makes `Association « Vivre à Chapet »` and
 * `Échanges Association Vivre à Chapet` comparable, and `XVDSI - General`
 * contain `XVDSI`. It is a normaliser for name affinity and deliberately not
 * the one the search uses — a search must not ignore punctuation the user
 * typed — which is why it builds on `fold` rather than restating it.
 */
export function stem(name: string): string {
	return fold(name)
		.replace(/[^\p{L}\p{N}]+/gu, ' ')
		.trim();
}

/** Whether two stems say their conversations belong to one community. */
function related(left: string, right: string): boolean {
	if (left === '' || right === '') {
		return false;
	}
	if (left === right) {
		return left.length >= MIN_STEM;
	}
	const [inner, outer] = left.length <= right.length ? [left, right] : [right, left];
	if (inner.length < MIN_STEM) {
		return false;
	}
	const at = outer.indexOf(inner);
	if (at < 0) {
		return false;
	}
	const before = at === 0 || outer[at - 1] === ' ';
	const after = at + inner.length === outer.length || outer[at + inner.length] === ' ';
	return before && after;
}

/**
 * The screen's sections, in the order the ticket names them: one-to-one,
 * group, community, then broadcast and whatever the network did not state.
 *
 * Communities come out of the group rows: a family of two or more is a
 * community, a family of one is an ordinary group. So the same clustering
 * decides both, and a group cannot be in neither section or in both.
 */
export interface ConversationSections {
	readonly oneToOne: readonly ConversationRow[];
	/** Groups no other group's name claims. */
	readonly groups: readonly ConversationRow[];
	readonly communities: readonly ConversationFamily[];
	readonly broadcasts: readonly ConversationRow[];
	readonly unstated: readonly ConversationRow[];
}

export function sections(all: readonly ConversationRow[]): ConversationSections {
	const of = (kind: ConversationKind): ConversationRow[] => all.filter((row) => row.kind === kind);
	const clustered = families(of('group'));
	return {
		oneToOne: of('one-to-one'),
		groups: clustered.filter((family) => family.rows.length === 1).flatMap((family) => family.rows),
		communities: clustered.filter((family) => family.rows.length > 1),
		broadcasts: of('broadcast'),
		unstated: of('unstated')
	};
}
