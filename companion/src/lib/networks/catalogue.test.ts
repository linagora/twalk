// Screen 3's grid is a join of three facts — the catalogue, the deployment's
// bridges, the platform — and every combination of them is a state a user can
// land in. Pure, so they are covered here rather than in a browser.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import { gridFor, looksLikeIos, manageRouteFor, NETWORK_CARDS, cardFor, type BridgeRow } from './catalogue';
import type { BridgeConnection } from './connection';
import type { Connection } from '$lib/connections/registry';

/** A bridge that answered, holding no login: the not-connected state. */
function noLogins(): BridgeConnection {
	return {
		reachable: true,
		state: 'disconnected',
		reported: null,
		reason: 'the bridge holds no login',
		logins: [],
		unreachable_because: null
	};
}

/** A bridge holding one login in a given state. */
function holding(state: BridgeConnection['state'], reported: string): BridgeConnection {
	return {
		reachable: true,
		state,
		reported,
		reason: null,
		logins: [
			{
				login_id: '33660469852',
				name: '+33660469852',
				profile: { phone: '+33660469852' },
				state: state ?? 'disconnected',
				reported,
				reason: null,
				since: '2026-09-18T07:27:59.000Z'
			}
		],
		unreachable_because: null
	};
}

/** A bridge the Gateway could not ask. */
function unreachable(): BridgeConnection {
	return {
		reachable: false,
		state: null,
		reported: null,
		reason: null,
		logins: [],
		unreachable_because: 'bridge_unreachable'
	};
}

function bridge(
	bridgeId: string,
	network: string,
	connection: BridgeConnection = noLogins()
): BridgeRow {
	return { bridge_id: bridgeId, network, connection, login: null };
}

const configured: BridgeRow[] = [
	bridge('mautrix-whatsapp', 'whatsapp'),
	bridge('mautrix-signal', 'signal'),
	bridge('mautrix-gmessages', 'sms')
];

/**
 * The registry a Gateway derives from those bridges (#269): one connection
 * per bridge, named after its network, plus the native Matrix one.
 */
function registryFor(bridges: readonly BridgeRow[]): Connection[] {
	return [
		...bridges.map((row) => ({
			id: row.network,
			kind: row.network as Connection['kind'],
			label: row.bridge_id,
			bridge_id: row.bridge_id
		})),
		{ id: 'matrix', kind: 'matrix' as const, label: 'example.com' }
	];
}

/** `gridFor`'s options for a deployment whose registry is derived from its bridges. */
function deployment(
	bridges: readonly BridgeRow[],
	overrides: Partial<Parameters<typeof gridFor>[0]> = {}
): Parameters<typeof gridFor>[0] {
	return {
		connections: registryFor(bridges),
		connectionsKnown: true,
		bridges,
		bridgesKnown: true,
		ios: false,
		...overrides
	};
}

function card(network: string, options: Parameters<typeof gridFor>[0]) {
	const found = gridFor(options).find((state) => state.card.network === network);
	expect(found, `no card for ${network}`).toBeDefined();
	return found!;
}

describe('the network grid', () => {
	it('offers the five reachable networks and shows the one that is not', () => {
		const active = NETWORK_CARDS.filter((entry) => entry.milestone === 'v0.1').map(
			(entry) => entry.network
		);
		expect(active).toEqual(['whatsapp', 'signal', 'sms', 'matrix', 'telegram']);

		const grid = gridFor(deployment(configured));
		expect(grid.map((state) => state.card.network)).toContain('telegram');
		expect(card('discord', deployment(configured)).blockedBy).toBe(
			'coming-soon'
		);
	});

	it('blocks Telegram on a missing bridge, not on "coming soon"', () => {
		// The distinction is the whole point of moving this card. "Coming soon"
		// is a statement about Twalk and cannot be acted on; "not configured" is
		// a statement about *this deployment* and an operator can fix it. The
		// card said the first while the bridge was up and serving four login
		// flows.
		const withTelegram = [...configured, bridge('mautrix-telegram', 'telegram')];
		expect(card('telegram', deployment(withTelegram)).blockedBy).toBeNull();
		expect(card('telegram', deployment(configured)).blockedBy).toBe(
			'no-bridge'
		);
	});

	it('names the bridge instance serving each network', () => {
		expect(card('whatsapp', deployment(configured)).bridgeId).toBe(
			'mautrix-whatsapp'
		);
		// The network is what the user experiences; the bridge is the
		// implementation, and `mautrix-gmessages` is the `sms` network.
		expect(card('sms', deployment(configured)).bridgeId).toBe(
			'mautrix-gmessages'
		);
	});

	it('greys the SMS preview on an iOS user agent, and only that card', () => {
		const state = deployment(configured, { ios: true });
		expect(card('sms', state).blockedBy).toBe('ios');
		expect(card('whatsapp', state).blockedBy).toBeNull();
		expect(card('signal', state).blockedBy).toBeNull();
	});

	it('greys a network this deployment configured no bridge for', () => {
		const state = deployment([configured[0]!]);
		expect(card('signal', state).blockedBy).toBe('no-bridge');
		expect(card('whatsapp', state).blockedBy).toBeNull();
		// Matrix needs no bridge: the bring-your-own-account path is always open.
		expect(card('matrix', state).blockedBy).toBeNull();
	});

	it('leaves every card tappable when the Gateway could not be asked', () => {
		// The honest state: "unknown", not "nothing is configured". The screen
		// says so, and each network's own screen handles a missing bridge.
		const state = {
			connections: [],
			connectionsKnown: false,
			bridges: [],
			bridgesKnown: false,
			ios: false
		};
		expect(card('whatsapp', state).blockedBy).toBeNull();
		expect(card('signal', state).blockedBy).toBeNull();
	});

	it('draws one card per connection, and names each only when its kind has two', () => {
		// The shape ADR 0033 exists for: two WhatsApp accounts, two bridges.
		// Two cards, each its own connection and its own bridge — found by the
		// id the connection names, never by network — and the label says
		// which account. Signal, with one, reads as it always did.
		const bridges = [
			bridge('mautrix-whatsapp', 'whatsapp'),
			bridge('mautrix-whatsapp-work', 'whatsapp', holding('connected', '2026-09-18T07:27:59.000Z')),
			bridge('mautrix-signal', 'signal')
		];
		const connections: Connection[] = [
			{ id: 'wa-home', kind: 'whatsapp', label: 'Home', bridge_id: 'mautrix-whatsapp' },
			{ id: 'wa-work', kind: 'whatsapp', label: 'Work', bridge_id: 'mautrix-whatsapp-work' },
			{ id: 'signal', kind: 'signal', label: 'mautrix-signal', bridge_id: 'mautrix-signal' },
			{ id: 'matrix', kind: 'matrix', label: 'example.com' }
		];
		const grid = gridFor({ connections, connectionsKnown: true, bridges, bridgesKnown: true, ios: false });
		const whatsapp = grid.filter((state) => state.card.network === 'whatsapp');
		expect(whatsapp.map((state) => state.connection?.id)).toEqual(['wa-home', 'wa-work']);
		expect(whatsapp.map((state) => state.bridgeId)).toEqual(['mautrix-whatsapp', 'mautrix-whatsapp-work']);
		expect(whatsapp.map((state) => state.label)).toEqual(['Home', 'Work']);
		expect(whatsapp.map((state) => state.connected)).toEqual([false, true]);
		expect(whatsapp.map((state) => state.href)).toEqual([
			'/networks/whatsapp?connection=wa-home',
			'/networks/whatsapp?connection=wa-work'
		]);
		const signal = card('signal', { connections, connectionsKnown: true, bridges, bridgesKnown: true, ios: false });
		expect(signal.label).toBeNull();
		expect(signal.href).toBe('/networks/signal');
		// A kind with no connection keeps its one card, blocked as before.
		expect(card('telegram', { connections, connectionsKnown: true, bridges, bridgesKnown: true, ios: false }).blockedBy).toBe('no-bridge');
	});

	it('keys every card so two of one kind are two rows and not one', () => {
		const grid = gridFor(deployment(configured));
		const keys = grid.map((state) => state.key);
		expect(new Set(keys).size).toBe(keys.length);
		expect(card('whatsapp', deployment(configured)).key).toBe('whatsapp');
	});
});

// The defect of #108, at the one place it was computed. Every case below used
// to be decided by `bridge.login.state === 'complete'` — the state of a login
// *process* in the Gateway's memory — and so got the answer wrong whenever the
// process and the link disagreed, which is most of the time.
describe('the connected state of a network', () => {
	it('comes from the bridge, never from the login process', () => {
		const state = deployment([bridge('mautrix-whatsapp', 'whatsapp', holding('connected', 'CONNECTED'))]);
		expect(card('whatsapp', state).connected).toBe(true);
		expect(card('whatsapp', state).link.account?.name).toBe('+33660469852');
	});

	it('survives a login being started on a connected bridge', () => {
		// The live incident, in one assertion: a QR scan is in flight and the
		// account is still linked. The old code read `login.state` here, found
		// `awaiting_remote`, and dropped the badge.
		const row: BridgeRow = {
			...bridge('mautrix-whatsapp', 'whatsapp', holding('connected', 'CONNECTED')),
			login: {
				bridge_id: 'mautrix-whatsapp',
				network: 'whatsapp',
				process_id: 'p1',
				flow_id: 'qr',
				login_id: null,
				state: 'awaiting_remote',
				started_at: '2026-09-18T08:00:00.000Z',
				started_by: { device_id: 'd1', device_name: 'a phone' },
				expires_at: '2026-09-18T08:30:00.000Z',
				generation: 2,
				step: null,
				login: null,
				error: null
			}
		};
		const state = deployment([row]);
		expect(card('whatsapp', state).connected).toBe(true);
	});

	it('survives a cancelled login, and a Gateway that has forgotten every process', () => {
		for (const login of [
			{ state: 'cancelled' as const },
			null
		]) {
			const row = {
				...bridge('mautrix-whatsapp', 'whatsapp', holding('connected', 'CONNECTED')),
				login: login === null ? null : ({ ...login } as unknown as BridgeRow['login'])
			};
			const state = deployment([row]);
			expect(card('whatsapp', state).connected).toBe(true);
		}
	});

	it('is not claimed for a bridge that answered "no login"', () => {
		const state = deployment(configured);
		expect(card('whatsapp', state).connected).toBe(false);
		expect(card('whatsapp', state).linked).toBe(false);
	});

	it('is unknown, not disconnected, when the bridge could not be asked', () => {
		// Guessing `disconnected` here is the same lie in a new place: it tells
		// a user with a working link that it is broken.
		const state = deployment([bridge('mautrix-whatsapp', 'whatsapp', unreachable())]);
		expect(card('whatsapp', state).link.state).toBe('unknown');
		expect(card('whatsapp', state).connected).toBe(false);
		expect(card('whatsapp', state).linked).toBe(false);
	});

	it('keeps the other mautrix states apart instead of rounding them to a tick', () => {
		for (const [reported, expected] of [
			['CONNECTING', 'starting'],
			['BACKFILLING', 'starting'],
			['TRANSIENT_DISCONNECT', 'degraded'],
			// A session revoked from the user's phone. `BAD_CREDENTIALS` is what
			// says so; no mautrix bridge emits `LOGGED_OUT`.
			['BAD_CREDENTIALS', 'session_expired']
		] as const) {
			const state = deployment([bridge('mautrix-whatsapp', 'whatsapp', holding(expected, reported))]);
			expect(card('whatsapp', state).link.state).toBe(expected);
			expect(card('whatsapp', state).connected).toBe(false);
			// …but the link exists, so the card offers Manage and not Connect.
			expect(card('whatsapp', state).linked).toBe(true);
		}
	});
});

describe('where Manage leads', () => {
	it('is the management screen of that network', () => {
		expect(manageRouteFor(card('whatsapp', deployment(configured)))).toBe('/networks/whatsapp/manage');
		expect(manageRouteFor(card('signal', deployment(configured)))).toBe('/networks/signal/manage');
		expect(manageRouteFor(card('sms', deployment(configured)))).toBe('/networks/sms/manage');
	});

	it('names the connection when its kind has two, as the card does', () => {
		const state = {
			card: cardFor('whatsapp')!,
			href: '/networks/whatsapp?connection=wa-work'
		};
		expect(manageRouteFor(state)).toBe('/networks/whatsapp/manage?connection=wa-work');
	});

	it('is nowhere for Matrix, which has no login to manage', () => {
		// The bring-your-own-account path reaches Twalk by the Sensor being
		// invited into the user's rooms (ADR 0009): no bridge, no login, no
		// disconnect.
		expect(manageRouteFor(card('matrix', deployment(configured)))).toBeNull();
	});
});

describe('the iOS hint', () => {
	it('recognises an iPhone', () => {
		expect(
			looksLikeIos(
				'Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15',
				5,
				'iPhone'
			)
		).toBe(true);
	});

	it('recognises an iPad, which claims to be a Mac', () => {
		expect(
			looksLikeIos('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15', 5, 'MacIntel')
		).toBe(true);
	});

	it('leaves a desktop Mac alone', () => {
		expect(
			looksLikeIos('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36', 0, 'MacIntel')
		).toBe(false);
	});

	it('leaves Android and Windows alone', () => {
		expect(looksLikeIos('Mozilla/5.0 (Linux; Android 14; Pixel 8)', 5, 'Linux armv8l')).toBe(false);
		expect(looksLikeIos('Mozilla/5.0 (Windows NT 10.0; Win64; x64)', 0, 'Win32')).toBe(false);
	});
});

describe('every route the catalogue can produce is served', () => {
	// The Telegram login screen shipped without its manage screen, and nothing
	// caught it: `manageRouteFor` derives `<route>/manage` for every card that
	// needs a bridge, so *Manage* appeared the moment the account linked and
	// led to a route that did not exist. A card is not allowed to offer a
	// destination the app cannot serve — that is the defect this file exists to
	// keep out, one directory up from the data it already checks.
	const pages = import.meta.glob('/src/routes/**/+page.svelte');
	const served = new Set(
		Object.keys(pages).map((path) =>
			path.replace('/src/routes', '').replace('/+page.svelte', '')
		)
	);

	it.each(NETWORK_CARDS.filter((card) => card.route !== null))(
		'serves $network',
		(card) => {
			expect(served, `no page for ${card.route}`).toContain(card.route!);
			const manage = manageRouteFor({ card, href: card.route });
			if (manage !== null) {
				expect(served, `no page for ${manage}`).toContain(manage);
			}
		}
	);
});

/**
 * The contract's own definition of the networks — the one authority (ADR
 * 0033, #268). The catalogue is a copy: every card names a network the
 * contract knows, and a network the contract adds is not silently a network
 * this screen cannot name.
 */
const NETWORK_DEFINITION = join(
	dirname(fileURLToPath(import.meta.url)),
	'..',
	'..',
	'..',
	'..',
	'contracts',
	'cloudevents',
	'v1',
	'definitions',
	'network.schema.json'
);

describe('the contract is the authority for the networks', () => {
	const authority = (JSON.parse(readFileSync(NETWORK_DEFINITION, 'utf8')) as { enum: string[] })
		.enum;

	it('names on every card a network the contract knows', () => {
		for (const card of NETWORK_CARDS) {
			expect(authority, `${card.network} is not a network the contract knows`).toContain(
				card.network
			);
		}
	});

	it('has a card for every messaging network, and knows which it has not drawn yet', () => {
		// `email` is a network (ADR 0033) with no card yet: its screen is a
		// connection's, not a bridge's, and lands with the mail collector
		// (#272, #276). Naming it here is what turns "forgot" into "not yet".
		const notDrawnYet = ['email'];
		const drawn = new Set(NETWORK_CARDS.map((card) => card.network));
		for (const network of authority) {
			if (notDrawnYet.includes(network)) {
				expect(drawn.has(network), `${network} has a card now: drop it from notDrawnYet`).toBe(
					false
				);
			} else {
				expect(drawn.has(network), `the contract names ${network} and this screen has no card`).toBe(
					true
				);
			}
		}
	});
});
