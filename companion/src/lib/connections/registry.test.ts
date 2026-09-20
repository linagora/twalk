// A card is a connection and a bridge is its transport (ADR 0033, #272).
// Everything a screen decides about "which account is this?" goes through
// these three pure functions, so the two-connections shape — the one the
// reference deployment does not have and the one the perimeter exists for —
// is covered here rather than in a browser.

import { describe, expect, it } from 'vitest';

import { bridgeOf, labelFor, ofKind, pick, type Connection } from './registry';
import type { ConfiguredBridge } from '$lib/networks/connection';

function connection(
	id: string,
	kind: Connection['kind'],
	label = id,
	bridge: { id: string; bot?: string } | null = { id: `mautrix-${id}` }
): Connection {
	return {
		id,
		kind,
		label,
		...(bridge === null ? {} : { bridge_id: bridge.id }),
		...(bridge?.bot === undefined ? {} : { bridge_bot: bridge.bot })
	};
}

function bridge(bridgeId: string, network: string): ConfiguredBridge {
	return {
		bridge_id: bridgeId,
		network,
		connection: {
			reachable: true,
			state: 'disconnected',
			reported: null,
			reason: 'the bridge holds no login',
			logins: [],
			unreachable_because: null
		},
		login: null
	};
}

/** The reference deployment: one connection per kind, named after it. */
const reference: Connection[] = [
	connection('whatsapp', 'whatsapp', 'mautrix-whatsapp'),
	connection('signal', 'signal', 'mautrix-signal'),
	connection('matrix', 'matrix', 'example.com', null)
];

/** A deployment with two WhatsApp accounts. */
const twoAccounts: Connection[] = [
	connection('wa-home', 'whatsapp', 'Home', { id: 'mautrix-whatsapp' }),
	connection('wa-work', 'whatsapp', 'Work', { id: 'mautrix-whatsapp-work' }),
	connection('signal', 'signal', 'mautrix-signal'),
	connection('matrix', 'matrix', 'example.com', null)
];

describe('picking the connection a screen is about', () => {
	it('is the named one when the URL names it', () => {
		expect(pick(twoAccounts, 'whatsapp', 'wa-work')?.id).toBe('wa-work');
		expect(pick(reference, 'whatsapp', 'whatsapp')?.id).toBe('whatsapp');
	});

	it('is the kind\'s only one when nothing is named — never a guess between two', () => {
		expect(pick(reference, 'whatsapp', null)?.id).toBe('whatsapp');
		expect(pick(twoAccounts, 'signal', null)?.id).toBe('signal');
		// Two of the kind and no name: nothing, and the screen says so. The
		// first match winning is exactly the defect ADR 0033 named.
		expect(pick(twoAccounts, 'whatsapp', null)).toBeNull();
	});

	it('refuses a name that is not a connection of this kind', () => {
		expect(pick(twoAccounts, 'signal', 'wa-work')).toBeNull();
		expect(pick(twoAccounts, 'whatsapp', 'wa-other')).toBeNull();
		expect(pick(twoAccounts, 'telegram', null)).toBeNull();
	});
});

describe('a bridge is a connection\'s transport', () => {
	const bridges = [
		bridge('mautrix-whatsapp', 'whatsapp'),
		bridge('mautrix-whatsapp-work', 'whatsapp'),
		bridge('mautrix-signal', 'signal')
	];

	it('is found by the bridge id the connection names, never by network', () => {
		expect(bridgeOf(twoAccounts[1]!, bridges)?.bridge_id).toBe('mautrix-whatsapp-work');
		expect(bridgeOf(twoAccounts[0]!, bridges)?.bridge_id).toBe('mautrix-whatsapp');
	});

	it('is nothing for a connection no bridge carries, or one the list lacks', () => {
		expect(bridgeOf(twoAccounts[3]!, bridges)).toBeNull();
		expect(bridgeOf(connection('telegram', 'telegram'), bridges)).toBeNull();
	});
});

describe('what a card says about which account it is', () => {
	it('names the account only when the kind has more than one', () => {
		expect(labelFor(twoAccounts[0]!, ofKind(twoAccounts, 'whatsapp'))).toBe('Home');
		expect(labelFor(twoAccounts[1]!, ofKind(twoAccounts, 'whatsapp'))).toBe('Work');
		// One WhatsApp: the card is "WhatsApp", as it always was.
		expect(labelFor(reference[0]!, ofKind(reference, 'whatsapp'))).toBeNull();
	});

	it('lists a kind\'s connections in the registry\'s order', () => {
		expect(ofKind(twoAccounts, 'whatsapp').map((c) => c.id)).toEqual(['wa-home', 'wa-work']);
		expect(ofKind(twoAccounts, 'telegram')).toEqual([]);
	});
});
