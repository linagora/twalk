// The perimeter of an activation, which is the whole of ADR 0013 in one
// function: what the assistant may read is what the user connected and ticked,
// and nothing that arrives afterwards.
//
// The rows here carry a `connection` — the bridge's own answer about the link
// it holds — and a `login`, the login *process* in the Gateway's memory. Every
// case below sets the two against each other on purpose, because reading the
// second is what made this screen offer nothing at all on a deployment with
// two networks connected (#142).

import { describe, expect, it } from 'vitest';

import { defaultSelection, scopeFor, scopeOptions, type BridgeRow } from './scope';
import type { Connection } from '$lib/connections/registry';
import type { BridgeConnection } from '$lib/networks/connection';

function connection(state: BridgeConnection['state'], linked = true): BridgeConnection {
	return {
		reachable: true,
		state,
		reported: state === 'connected' ? 'CONNECTED' : 'UNKNOWN',
		reason: null,
		logins: linked
			? [
					{
						login_id: 'a-login',
						name: '+33660469852',
						profile: { phone: '+33660469852' },
						state,
						reported: state === 'connected' ? 'CONNECTED' : 'UNKNOWN',
						reason: null,
						since: '2026-09-18T07:27:59.000Z'
					}
				]
			: [],
		unreachable_because: null
	} as BridgeConnection;
}

/**
 * A row as the Gateway answers it. `login` is deliberately `null` in most
 * cases: a bridge holding a live session has **no login process** — the one
 * that made it finished, or the Gateway has restarted since — and that is the
 * everyday state this screen used to read as "nothing is connected".
 */
function bridge(
	network: string,
	state: BridgeConnection['state'],
	login: BridgeRow['login'] = null
): BridgeRow {
	return {
		bridge_id: `mautrix-${network}`,
		network,
		connection: connection(state, state !== null),
		login
	} as BridgeRow;
}

/**
 * The registry a Gateway derives from those bridges (#269): one connection
 * per bridge named after its network, and the native Matrix one.
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

/** `scopeOptions` on a deployment whose registry is derived from its bridges. */
function optionsFor(bridges: readonly BridgeRow[]) {
	return scopeOptions(registryFor(bridges), bridges);
}

const MATRIX_OPTION = {
	connection: 'matrix',
	network: 'matrix',
	label: null,
	proven: false,
	preselected: false
};

describe('scopeOptions', () => {
	it('offers the connected connections ticked, and Matrix unticked', () => {
		const options = optionsFor([
			bridge('whatsapp', 'connected'),
			bridge('signal', 'disconnected'),
			bridge('sms', 'disconnected')
		]);
		expect(options).toEqual([
			{ connection: 'whatsapp', network: 'whatsapp', label: null, proven: true, preselected: true },
			// Nothing the Gateway records says the Sensor was invited into any
			// room, so Matrix is offered and never assumed.
			MATRIX_OPTION
		]);
	});

	it('offers a connected connection that has no login process at all', () => {
		// The case found live (#142): two bridges holding live sessions, no
		// login in flight, and the screen offered nothing — so no persona
		// could be activated on any network. A login process is not a link.
		const options = optionsFor([bridge('whatsapp', 'connected'), bridge('signal', 'connected')]);
		expect(defaultSelection(options)).toEqual(['whatsapp', 'signal']);
	});

	it('does not offer a connection whose only evidence is a login in flight', () => {
		// The mirror image, and the reason this is not "read both": a QR code
		// somebody is in the middle of scanning is not a network to scope a
		// consent decision onto.
		const options = optionsFor([
			bridge('whatsapp', 'disconnected', {
				state: 'complete'
			} as BridgeRow['login'])
		]);
		expect(options.some((option) => option.network === 'whatsapp')).toBe(false);
	});

	it('does not offer a bridge that is starting or reconnecting', () => {
		// `starting` and `degraded` are honest states of a bridge on its way
		// somewhere, and `isConnected` rounds neither up — the same judgement
		// the picker's badge makes, from the same function.
		const options = optionsFor([bridge('whatsapp', 'starting'), bridge('signal', 'degraded')]);
		expect(options).toEqual([MATRIX_OPTION]);
	});

	it('offers Matrix even on a deployment with no bridge at all', () => {
		expect(optionsFor([])).toEqual([MATRIX_OPTION]);
	});

	it('offers two connections of one kind as two rows, each named', () => {
		// The shape ADR 0033 exists for (#272): a persona activated on the
		// work WhatsApp is not activated on the home one, so the perimeter is
		// offered per connection and the label says which.
		const bridges = [
			bridge('whatsapp', 'connected'),
			{ ...bridge('whatsapp', 'connected'), bridge_id: 'mautrix-whatsapp-work' }
		];
		const registry: Connection[] = [
			{ id: 'wa-home', kind: 'whatsapp', label: 'Home', bridge_id: 'mautrix-whatsapp' },
			{ id: 'wa-work', kind: 'whatsapp', label: 'Work', bridge_id: 'mautrix-whatsapp-work' },
			{ id: 'matrix', kind: 'matrix', label: 'example.com' }
		];
		const options = scopeOptions(registry, bridges);
		expect(options.map((option) => [option.connection, option.label, option.preselected])).toEqual([
			['wa-home', 'Home', true],
			['wa-work', 'Work', true],
			['matrix', null, false]
		]);
		expect(defaultSelection(options)).toEqual(['wa-home', 'wa-work']);
	});
});

describe('scopeFor', () => {
	it('sorts and deduplicates, as the event id recipe does', () => {
		expect(scopeFor(['whatsapp', 'signal', 'whatsapp'])).toEqual(['signal', 'whatsapp']);
	});

	it('is empty for an empty selection, which is not a decision', () => {
		expect(scopeFor([])).toEqual([]);
	});
});
