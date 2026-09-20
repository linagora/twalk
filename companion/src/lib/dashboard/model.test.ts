// Screen 5's judgements, tested without a browser: which dot, which banner,
// which rows — and the one invariant this screen exists to keep.

import { describe, expect, it } from 'vitest';

import type { Connection } from '$lib/connections/registry';

import {
	activityFeed,
	bridgeRows,
	expiredBridges,
	FEED_LENGTH,
	messageCount,
	overallHealth,
	pendingByConnection,
	pendingDecisions,
	personaRows,
	type ConfiguredBridge,
	type ConsentEntry,
	type Device
} from './model';

/**
 * What the bridge says about the link it holds — the thing a row's state is
 * read from since #108.
 *
 * `state` is the contract's, and `linked` says whether the bridge holds an
 * account at all: the two together are what separate "no account here" from
 * "an account whose link is down".
 */
function connection(
	state: ConfiguredBridge['connection']['state'],
	linked = state !== 'disconnected'
): ConfiguredBridge['connection'] {
	return {
		reachable: state !== null,
		state,
		reported: null,
		reason: null,
		logins: linked
			? [
					{
						login_id: '33660469852',
						name: '+33660469852',
						profile: { phone: '+33660469852' },
						state: state ?? 'disconnected',
						reported: null,
						reason: null,
						since: '2026-09-18T07:27:59.000Z'
					}
				]
			: [],
		unreachable_because: state === null ? 'bridge_unreachable' : null
	};
}

function bridge(
	network: string,
	connectionState: ConfiguredBridge['connection'],
	login: Partial<NonNullable<ConfiguredBridge['login']>> | null = null
): ConfiguredBridge {
	return {
		bridge_id: `mautrix-${network}`,
		network,
		connection: connectionState,
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
		connection: network,
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

/**
 * The registry a Gateway derives from its bridges (#269): one connection per
 * bridge, named after its network.
 */
function derived(bridges: readonly ConfiguredBridge[]): Connection[] {
	return bridges.map((row) => ({
		id: row.network,
		kind: row.network as Connection['kind'],
		label: row.bridge_id,
		bridge_id: row.bridge_id
	}));
}

/** `bridgeRows` on a deployment whose registry is derived from its bridges. */
function rowsFor(bridges: readonly ConfiguredBridge[]) {
	return bridgeRows(derived(bridges), bridges);
}

describe('bridgeRows', () => {
	it('is one row per connection a bridge carries, found by the bridge id it names', () => {
		// Two WhatsApp accounts, two bridges (#272): two rows, each its own
		// connection, each labelled, each leading to its own manage screen.
		// A connection no bridge carries — Matrix — is not a bridge row.
		const bridges = [
			bridge('whatsapp', connection('connected')),
			{ ...bridge('whatsapp', connection('disconnected', true)), bridge_id: 'mautrix-whatsapp-work' },
			bridge('signal', connection('connected'))
		];
		const registry: Connection[] = [
			{ id: 'wa-home', kind: 'whatsapp', label: 'Home', bridge_id: 'mautrix-whatsapp' },
			{ id: 'wa-work', kind: 'whatsapp', label: 'Work', bridge_id: 'mautrix-whatsapp-work' },
			{ id: 'signal', kind: 'signal', label: 'mautrix-signal', bridge_id: 'mautrix-signal' },
			{ id: 'matrix', kind: 'matrix', label: 'example.com' }
		];
		const rows = bridgeRows(registry, bridges);
		expect(rows.map((row) => row.connectionId)).toEqual(['wa-home', 'wa-work', 'signal']);
		expect(rows.map((row) => row.bridgeId)).toEqual([
			'mautrix-whatsapp',
			'mautrix-whatsapp-work',
			'mautrix-signal'
		]);
		expect(rows.map((row) => row.label)).toEqual(['Home', 'Work', null]);
		expect(rows.map((row) => row.state)).toEqual(['connected', 'failed', 'connected']);
		expect(rows[0]!.route).toBe('/networks/whatsapp/manage?connection=wa-home');
		expect(rows[2]!.route).toBe('/networks/signal/manage');
	});

	it('reads the contract state the bridge reports, not a login process', () => {
		const rows = rowsFor([
			bridge('whatsapp', connection('connected')),
			bridge('signal', connection('session_expired')),
			bridge('sms', connection('disconnected', true)),
			bridge('telegram', connection('disconnected'))
		]);
		expect(rows.map((row) => row.state)).toEqual(['connected', 'expired', 'failed', 'never']);
		expect(rows.map((row) => row.tone)).toEqual(['ok', 'warn', 'bad', 'idle']);
	});

	it('is still connected while a login is in flight, and after a Gateway restart', () => {
		// The defect of #108 as this screen showed it: the dot was read off
		// `login`, so a scan in progress and a forgotten process each turned a
		// live WhatsApp link into "not connected".
		const scanning = rowsFor([
			bridge('whatsapp', connection('connected'), { state: 'awaiting_remote' })
		]);
		expect(scanning[0]!.state).toBe('connected');

		const restarted = rowsFor([bridge('whatsapp', connection('connected'), null)]);
		expect(restarted[0]!.state).toBe('connected');
	});

	it('says the state is unknown, not "never", when the bridge could not be asked', () => {
		const [row] = rowsFor([bridge('whatsapp', connection(null, false))]);
		expect(row.state).toBe('unknown');
		// Idle, not red: nothing is known to be wrong either.
		expect(row.tone).toBe('idle');
	});

	it('treats a bridge that is coming back up as connecting, not broken', () => {
		const rows = rowsFor([
			bridge('whatsapp', connection('starting')),
			bridge('signal', connection('degraded'))
		]);
		expect(rows.map((row) => row.state)).toEqual(['connecting', 'connecting']);
	});

	it('sends Manage to the management screen once an account is linked', () => {
		const [linked] = rowsFor([bridge('whatsapp', connection('connected'))]);
		expect(linked.route).toBe('/networks/whatsapp/manage');
		// Nothing linked: the row leads to the login screen, which is the only
		// useful thing there.
		const [empty] = rowsFor([bridge('whatsapp', connection('disconnected'))]);
		expect(empty.route).toBe('/networks/whatsapp');
	});

	it('dates the row from when the link last changed state', () => {
		const [row] = rowsFor([bridge('whatsapp', connection('connected'))]);
		expect(row.since).toBe('2026-09-18T07:27:59.000Z');
	});

	it('reports no last-message time, because nothing reports one yet', () => {
		// The honest answer: a zero or a boot time here would be a number the
		// user could act on and that means nothing.
		const [row] = rowsFor([bridge('whatsapp', connection('connected'))]);
		expect(row.lastMessageAt).toBeNull();
	});

	it('names only the expired bridges for the amber banner', () => {
		// `session_expired` — which is `BAD_CREDENTIALS`, what a session
		// revoked from the user's own phone reports.
		const rows = rowsFor([
			bridge('whatsapp', connection('session_expired')),
			bridge('signal', connection('connected'))
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
});

describe('pendingByConnection', () => {
	it('names each connection with people waiting, labelled when its kind has two', () => {
		// The dashboard counts per connection (#272): the same number, said
		// per account when the deployment has two of one kind, so the user
		// reads which inbox is waiting — and unlabelled on the reference shape.
		const registry: Connection[] = [
			{ id: 'wa-home', kind: 'whatsapp', label: 'Home', bridge_id: 'mautrix-whatsapp' },
			{ id: 'wa-work', kind: 'whatsapp', label: 'Work', bridge_id: 'mautrix-whatsapp-work' },
			{ id: 'signal', kind: 'signal', label: 'mautrix-signal', bridge_id: 'mautrix-signal' }
		];
		const rows = pendingByConnection(
			[
				{ connection: 'wa-home', network: 'whatsapp', count: 2 },
				{ connection: 'signal', network: 'signal', count: 1 }
			],
			registry
		);
		expect(rows).toEqual([
			{ connection: 'wa-home', network: 'whatsapp', label: 'Home', count: 2 },
			{ connection: 'signal', network: 'signal', label: null, count: 1 }
		]);
		// A connection the registry does not name is still counted, by its id.
		expect(
			pendingByConnection([{ connection: 'gone', network: 'sms', count: 1 }], registry)
		).toEqual([{ connection: 'gone', network: 'sms', label: 'gone', count: 1 }]);
	});

	it('draws nothing for an empty inbox, which the screen treats as no chip', () => {
		expect(pendingDecisions({ total: 0 })).toBe(0);
	});
});

describe('overallHealth', () => {
	const connected = rowsFor([bridge('whatsapp', connection('connected'))]);
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
		const stale = rowsFor([
			bridge('whatsapp', connection('session_expired'))
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

	it('says a conversation moved, followed or returned, with numbers and no room', () => {
		// #255, ADR 0029: a deployment that changed rooms under the user
		// without being able to say so is one whose history they cannot check.
		// The room ids and the room's name never reach the feed — a one-to-one
		// portal is named after the contact.
		const feed = activityFeed({
			bridges: [],
			consent: [],
			devices: [],
			moves: [
				{
					successor: '!new:example.com',
					predecessor: '!old:example.com',
					bridge_id: 'mautrix-whatsapp',
					members: 3,
					crowd_threshold: 20,
					followed: true,
					decided_at: '2026-09-19T10:00:00.000Z'
				},
				{
					successor: '!bigger:example.com',
					predecessor: '!small:example.com',
					bridge_id: 'mautrix-whatsapp',
					members: 24,
					crowd_threshold: 20,
					followed: false,
					decided_at: '2026-09-19T11:00:00.000Z'
				}
			]
		});
		expect(feed.map((row) => row.messageKey)).toEqual([
			'dashboard.feed.move.returned',
			'dashboard.feed.move.followed'
		]);
		expect(feed[0].values).toEqual({ members: '24', threshold: '20' });
		expect(feed[0].tone).toBe('idle');
		expect(feed[1].tone).toBe('ok');
		expect(JSON.stringify(feed.map((row) => row.values))).not.toContain('example.com');
	});

	it('carries bridge state, persona activity and device events, newest first', () => {
		const feed = activityFeed({
			bridges: [
				bridge('whatsapp', connection('connected'), {
					state: 'complete',
					started_at: '2026-09-18T08:00:00.000Z'
				})
			],
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
