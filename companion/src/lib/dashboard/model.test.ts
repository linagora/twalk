// Screen 5's judgements, tested without a browser: which dot, which banner,
// which rows — and the one invariant this screen exists to keep.

import { describe, expect, it } from 'vitest';

import {
	activityFeed,
	bridgeRows,
	expiredBridges,
	FEED_LENGTH,
	messageCount,
	overallHealth,
	pendingDecisions,
	personaRows,
	type ConfiguredBridge,
	type ConsentEntry,
	type Device
} from './model';

function bridge(
	network: string,
	login: Partial<NonNullable<ConfiguredBridge['login']>> | null
): ConfiguredBridge {
	return {
		bridge_id: `mautrix-${network}`,
		network,
		login:
			login === null
				? null
				: ({
						bridge_id: `mautrix-${network}`,
						network,
						process_id: 'p1',
						flow_id: 'qr',
						login_id: null,
						state: 'complete',
						started_at: '2026-09-18T08:00:00.000Z',
						started_by: { device_id: 'd1', device_name: 'the laptop' },
						expires_at: '2026-09-18T08:30:00.000Z',
						generation: 1,
						step: null,
						login: null,
						error: null,
						...login
					} as NonNullable<ConfiguredBridge['login']>)
	};
}

function entry(
	type: 'contact' | 'network' | 'persona',
	id: string,
	network: ConsentEntry['network'],
	state: ConsentEntry['state'],
	sequence = 1,
	decided = '2026-09-18T09:00:00.000Z'
): ConsentEntry {
	return {
		subject: { type, id },
		network,
		state,
		decided_at: decided,
		decision_sequence: sequence
	};
}

function device(overrides: Partial<Device> = {}): Device {
	return {
		id: 'device-1',
		name: 'the laptop',
		created_unix_seconds: 1_789_000_000,
		last_seen_unix_seconds: 1_789_000_100,
		revoked_unix_seconds: null,
		current: true,
		...overrides
	};
}

describe('bridgeRows', () => {
	it('reads a completed login as connected and a lost one as expired', () => {
		const rows = bridgeRows([
			bridge('whatsapp', { state: 'complete' }),
			bridge('signal', { state: 'failed', error: { code: 'login_expired', detail: null } }),
			bridge('sms', { state: 'failed', error: { code: 'bridge_refused', detail: null } }),
			bridge('telegram', null)
		]);
		expect(rows.map((row) => row.state)).toEqual(['connected', 'expired', 'failed', 'never']);
		expect(rows.map((row) => row.tone)).toEqual(['ok', 'warn', 'bad', 'idle']);
	});

	it('reports no last-message time, because nothing reports one yet', () => {
		// The honest answer while #56 is not deployed: a zero or a boot time
		// here would be a number the user could act on and that means nothing.
		const [row] = bridgeRows([bridge('whatsapp', { state: 'complete' })]);
		expect(row.lastMessageAt).toBeNull();
	});

	it('names only the expired bridges for the amber banner', () => {
		const rows = bridgeRows([
			bridge('whatsapp', { state: 'failed', error: { code: 'login_lost', detail: null } }),
			bridge('signal', { state: 'complete' })
		]);
		expect(expiredBridges(rows).map((row) => row.network)).toEqual(['whatsapp']);
	});
});

describe('personaRows', () => {
	it('is active on the granted networks and paused when every one is revoked', () => {
		const [row] = personaRows([
			entry('persona', 'assistant', 'whatsapp', 'granted', 1),
			entry('persona', 'assistant', 'signal', 'granted', 1)
		]);
		expect(row.active).toBe(true);
		expect(row.networks).toEqual(['signal', 'whatsapp']);

		const [paused] = personaRows([
			entry('persona', 'assistant', 'whatsapp', 'revoked', 2),
			entry('persona', 'assistant', 'signal', 'revoked', 2)
		]);
		expect(paused.active).toBe(false);
		// The perimeter is remembered, because re-activating has to know what
		// the user had decided on rather than guess a new one.
		expect(paused.decidedNetworks).toEqual(['signal', 'whatsapp']);
	});

	it('gives a never-decided persona a row of its own', () => {
		const [row] = personaRows([], ['assistant']);
		expect(row.decided).toBe(false);
		expect(row.active).toBe(false);
		expect(row.networks).toEqual([]);
	});

	it('ignores contact and network subjects', () => {
		expect(personaRows([entry('contact', '@a:example.com', 'whatsapp', 'granted')])).toEqual([]);
	});
});

describe('pendingDecisions', () => {
	it('is the projection’s own total', () => {
		// The projection (#54) counts contacts who wrote and about whom
		// nothing was ever decided, which a read of the decision journal
		// cannot know: a contact with no decision has no entry in it.
		expect(pendingDecisions({ total: 3 })).toBe(3);
	});

	it('is unknown rather than zero when the read did not answer', () => {
		// The chip is hidden on `null`. A zero would tell the user there is
		// nothing waiting, which is not what an unanswered read means — and a
		// deployment that projects no inbound stream answers nothing at all.
		expect(pendingDecisions(null)).toBeNull();
	});

	it('draws nothing for an empty inbox, which the screen treats as no chip', () => {
		expect(pendingDecisions({ total: 0 })).toBe(0);
	});
});

describe('overallHealth', () => {
	const connected = bridgeRows([bridge('whatsapp', { state: 'complete' })]);
	const active = personaRows([entry('persona', 'assistant', 'whatsapp', 'granted')]);

	it('is green when a network is connected and an agent is active', () => {
		expect(overallHealth({ bridges: connected, personas: active, gatewayReachable: true })).toBe(
			'ok'
		);
	});

	it('calls an unfinished journey setup, not degradation', () => {
		expect(overallHealth({ bridges: [], personas: [], gatewayReachable: true })).toBe('setup');
	});

	it('wants attention for an expired bridge or an unreachable Gateway', () => {
		const stale = bridgeRows([
			bridge('whatsapp', { state: 'failed', error: { code: 'login_expired', detail: null } })
		]);
		expect(overallHealth({ bridges: stale, personas: active, gatewayReachable: true })).toBe(
			'attention'
		);
		expect(overallHealth({ bridges: connected, personas: active, gatewayReachable: false })).toBe(
			'attention'
		);
	});
});

describe('activityFeed', () => {
	it('never names a correspondent, even though the decision does', () => {
		// The invariant of screen 5 (#74). The consent state carries the
		// contact's Matrix ID — this is the one place the home screen could
		// have become the list of who writes to the user.
		const contact = '@whatsapp_33612345678:example.com';
		const feed = activityFeed({
			bridges: [],
			consent: [entry('contact', contact, 'whatsapp', 'granted')],
			devices: []
		});
		expect(feed).toHaveLength(1);
		const rendered = JSON.stringify(feed[0].values);
		expect(rendered).not.toContain(contact);
		expect(rendered).not.toContain('33612345678');
		expect(rendered).not.toContain('@');
		expect(feed[0].messageKey).toBe('dashboard.feed.consent.contact');
	});

	it('carries bridge state, persona activity and device events, newest first', () => {
		const feed = activityFeed({
			bridges: [bridge('whatsapp', { state: 'complete', started_at: '2026-09-18T08:00:00.000Z' })],
			consent: [entry('persona', 'assistant', 'whatsapp', 'granted', 1, '2026-09-18T09:00:00.000Z')],
			devices: [device({ created_unix_seconds: Date.parse('2026-09-18T07:00:00.000Z') / 1000 })]
		});
		expect(feed.map((row) => row.messageKey)).toEqual([
			'dashboard.feed.persona.activated',
			'dashboard.feed.bridge.connected',
			'dashboard.feed.device.signedIn'
		]);
	});

	it('shows a revoked device as its own event', () => {
		const feed = activityFeed({
			bridges: [],
			consent: [],
			devices: [device({ revoked_unix_seconds: Date.parse('2026-09-18T10:00:00.000Z') / 1000 })]
		});
		expect(feed[0].messageKey).toBe('dashboard.feed.device.revoked');
	});

	it('keeps the wireframe’s ten rows', () => {
		const consent = Array.from({ length: 20 }, (_, index) =>
			entry(
				'network',
				'whatsapp',
				'whatsapp',
				'granted',
				index + 1,
				`2026-09-18T09:${String(index).padStart(2, '0')}:00.000Z`
			)
		);
		expect(activityFeed({ bridges: [], consent, devices: [] })).toHaveLength(FEED_LENGTH);
	});
});

describe('messageCount', () => {
	it('is unknown, because nothing between the bus and the Companion counts', () => {
		expect(messageCount()).toBeNull();
	});
});
