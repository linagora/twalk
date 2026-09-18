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

describe('scopeOptions', () => {
	it('offers the connected networks ticked, and Matrix unticked', () => {
		const options = scopeOptions([
			bridge('whatsapp', 'connected'),
			bridge('signal', 'disconnected'),
			bridge('sms', 'disconnected')
		]);
		expect(options).toEqual([
			{ network: 'whatsapp', proven: true, preselected: true },
			// Nothing the Gateway records says the Sensor was invited into any
			// room, so Matrix is offered and never assumed.
			{ network: 'matrix', proven: false, preselected: false }
		]);
	});

	it('offers a connected network that has no login process at all', () => {
		// The case found live (#142): two bridges holding live sessions, no
		// login in flight, and the screen offered nothing — so no persona
		// could be activated on any network. A login process is not a link.
		const options = scopeOptions([bridge('whatsapp', 'connected'), bridge('signal', 'connected')]);
		expect(defaultSelection(options)).toEqual(['whatsapp', 'signal']);
	});

	it('does not offer a network whose only evidence is a login in flight', () => {
		// The mirror image, and the reason this is not "read both": a QR code
		// somebody is in the middle of scanning is not a network to scope a
		// consent decision onto.
		const options = scopeOptions([
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
		const options = scopeOptions([bridge('whatsapp', 'starting'), bridge('signal', 'degraded')]);
		expect(options).toEqual([{ network: 'matrix', proven: false, preselected: false }]);
	});

	it('offers Matrix even on a deployment with no bridge at all', () => {
		expect(scopeOptions([])).toEqual([{ network: 'matrix', proven: false, preselected: false }]);
	});

	it('does not list one network twice when two bridges serve it', () => {
		const options = scopeOptions([bridge('sms', 'connected'), bridge('sms', 'connected')]);
		expect(options.filter((option) => option.network === 'sms')).toHaveLength(1);
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
