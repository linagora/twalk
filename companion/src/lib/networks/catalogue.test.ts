// Screen 3's grid is a join of three facts — the catalogue, the deployment's
// bridges, the platform — and every combination of them is a state a user can
// land in. Pure, so they are covered here rather than in a browser.

import { describe, expect, it } from 'vitest';

import { gridFor, looksLikeIos, NETWORK_CARDS, type BridgeRow } from './catalogue';

const configured: BridgeRow[] = [
	{ bridge_id: 'mautrix-whatsapp', network: 'whatsapp', login: null },
	{ bridge_id: 'mautrix-signal', network: 'signal', login: null },
	{ bridge_id: 'mautrix-gmessages', network: 'sms', login: null }
];

function card(network: string, options: Parameters<typeof gridFor>[0]) {
	const found = gridFor(options).find((state) => state.card.network === network);
	expect(found, `no card for ${network}`).toBeDefined();
	return found!;
}

describe('the network grid', () => {
	it('offers the four v0.1 networks and shows the two v0.2 ones', () => {
		const active = NETWORK_CARDS.filter((entry) => entry.milestone === 'v0.1').map(
			(entry) => entry.network
		);
		expect(active).toEqual(['whatsapp', 'signal', 'sms', 'matrix']);

		const grid = gridFor({ bridges: configured, bridgesKnown: true, ios: false });
		expect(grid.map((state) => state.card.network)).toContain('telegram');
		expect(card('telegram', { bridges: configured, bridgesKnown: true, ios: false }).blockedBy).toBe(
			'coming-soon'
		);
		expect(card('discord', { bridges: configured, bridgesKnown: true, ios: false }).blockedBy).toBe(
			'coming-soon'
		);
	});

	it('names the bridge instance serving each network', () => {
		expect(card('whatsapp', { bridges: configured, bridgesKnown: true, ios: false }).bridgeId).toBe(
			'mautrix-whatsapp'
		);
		// The network is what the user experiences; the bridge is the
		// implementation, and `mautrix-gmessages` is the `sms` network.
		expect(card('sms', { bridges: configured, bridgesKnown: true, ios: false }).bridgeId).toBe(
			'mautrix-gmessages'
		);
	});

	it('greys the SMS preview on an iOS user agent, and only that card', () => {
		const state = { bridges: configured, bridgesKnown: true, ios: true };
		expect(card('sms', state).blockedBy).toBe('ios');
		expect(card('whatsapp', state).blockedBy).toBeNull();
		expect(card('signal', state).blockedBy).toBeNull();
	});

	it('greys a network this deployment configured no bridge for', () => {
		const state = { bridges: [configured[0]!], bridgesKnown: true, ios: false };
		expect(card('signal', state).blockedBy).toBe('no-bridge');
		expect(card('whatsapp', state).blockedBy).toBeNull();
		// Matrix needs no bridge: the bring-your-own-account path is always open.
		expect(card('matrix', state).blockedBy).toBeNull();
	});

	it('leaves every card tappable when the Gateway could not be asked', () => {
		// The honest state: "unknown", not "nothing is configured". The screen
		// says so, and each network's own screen handles a missing bridge.
		const state = { bridges: [], bridgesKnown: false, ios: false };
		expect(card('whatsapp', state).blockedBy).toBeNull();
		expect(card('signal', state).blockedBy).toBeNull();
	});

	it('marks a network whose bridge holds a completed login', () => {
		const connected: BridgeRow[] = [
			{ bridge_id: 'mautrix-whatsapp', network: 'whatsapp', login: { state: 'complete' } },
			{ bridge_id: 'mautrix-signal', network: 'signal', login: { state: 'cancelled' } }
		];
		const state = { bridges: connected, bridgesKnown: true, ios: false };
		expect(card('whatsapp', state).connected).toBe(true);
		// A login that was cancelled is still reported; it is not a connection.
		expect(card('signal', state).connected).toBe(false);
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
