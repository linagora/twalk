// The one place the Companion decides whether a network is connected. Pure,
// and worth its own file: this is the judgement #108 was about.

import { describe, expect, it } from 'vitest';

import {
	connectionOf,
	isConnected,
	needsAttention,
	UNKNOWN_CONNECTION,
	type BridgeConnection,
	type ConfiguredBridge
} from './connection';

function row(connection: BridgeConnection, login: ConfiguredBridge['login'] = null): ConfiguredBridge {
	return { bridge_id: 'mautrix-whatsapp', network: 'whatsapp', connection, login };
}

const CONNECTED: BridgeConnection = {
	reachable: true,
	state: 'connected',
	reported: 'CONNECTED',
	reason: null,
	logins: [
		{
			login_id: '33660469852',
			name: '+33660469852',
			profile: { phone: '+33660469852' },
			state: 'connected',
			reported: 'CONNECTED',
			reason: null,
			since: '2026-09-18T07:27:59.000Z'
		}
	],
	unreachable_because: null
};

describe('reading a bridge row as a connection', () => {
	it('names the account the bridge says is linked', () => {
		const connection = connectionOf(row(CONNECTED));
		expect(connection.state).toBe('connected');
		expect(connection.linked).toBe(true);
		expect(connection.account?.login_id).toBe('33660469852');
		expect(connection.account?.since).toBe('2026-09-18T07:27:59.000Z');
		expect(isConnected(connection)).toBe(true);
	});

	it('is unknown for a network this deployment has no bridge for', () => {
		// Matrix, or a bridge the operator never enabled. Different from a
		// bridge that answered "no login", and the screens say different
		// things about the two.
		expect(connectionOf(null)).toEqual(UNKNOWN_CONNECTION);
		expect(connectionOf(undefined)).toEqual(UNKNOWN_CONNECTION);
	});

	it('is unknown, never disconnected, for a bridge that did not answer', () => {
		const connection = connectionOf(
			row({
				reachable: false,
				state: null,
				reported: null,
				reason: null,
				logins: [],
				unreachable_because: 'bridge_unreachable'
			})
		);
		expect(connection.state).toBe('unknown');
		expect(connection.linked).toBe(false);
		expect(isConnected(connection)).toBe(false);
	});

	it('counts an expired session as a link, because it is one', () => {
		// The state a session revoked from the user's own phone reports. The
		// user has a link; what they need is the management screen, not a
		// fresh QR flow — so `linked` is true while `isConnected` is false.
		const connection = connectionOf(
			row({
				...CONNECTED,
				state: 'session_expired',
				reported: 'BAD_CREDENTIALS',
				logins: [
					{
						...CONNECTED.logins[0]!,
						state: 'session_expired',
						reported: 'BAD_CREDENTIALS',
						reason: 'You were logged out from another device'
					}
				]
			})
		);
		expect(connection.linked).toBe(true);
		expect(isConnected(connection)).toBe(false);
		expect(needsAttention(connection)).toBe(true);
		expect(connection.reason).toBe('You were logged out from another device');
	});

	it('does not ask the user to act on a bridge that is fixing itself', () => {
		for (const state of ['starting', 'degraded'] as const) {
			const connection = connectionOf(row({ ...CONNECTED, state }));
			expect(needsAttention(connection)).toBe(false);
			expect(isConnected(connection)).toBe(false);
		}
	});

	it('ignores the login process entirely', () => {
		// Every value `login` can take, against a bridge that is connected.
		// This is the defect: the old reading answered "connected" for exactly
		// one of these and "not connected" for the rest.
		for (const state of ['awaiting_input', 'awaiting_remote', 'complete', 'failed', 'cancelled'] as const) {
			const connection = connectionOf(
				row(CONNECTED, {
					bridge_id: 'mautrix-whatsapp',
					network: 'whatsapp',
					process_id: 'p1',
					flow_id: 'qr',
					login_id: null,
					state,
					started_at: '2026-09-18T08:00:00.000Z',
					started_by: { device_id: 'd1', device_name: 'a phone' },
					expires_at: '2026-09-18T08:30:00.000Z',
					generation: 1,
					step: null,
					login: null,
					error: null
				})
			);
			expect(isConnected(connection), `login process ${state}`).toBe(true);
		}
	});
});
