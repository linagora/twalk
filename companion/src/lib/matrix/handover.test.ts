// The handover room's properties, asserted rather than commented (ticket
// #226).
//
// The three that matter are absences, in this project's habit: the room can
// never be posted in, nothing is ever sent into it, and re-running onboarding
// creates no second one. The fourth is the reason it exists at all — the
// crypto machine must be able to enumerate the Sensor's devices — and it is
// asserted as the *only* path to a `ready` outcome.
//
// The homeserver is a recorded fake, so every request the module makes is
// visible to the assertions. That is what lets a test say "no request sent an
// event into this room" over a whole run, which is the shape of claim the
// module's promise needs.

import { describe, expect, it } from 'vitest';

import {
	ensureHandoverRoom,
	handoverAlias,
	handoverRoomCreation,
	HANDOVER_ALIAS_LOCALPART,
	HANDOVER_ROOM_TYPE,
	SEND_LEVEL_NOBODY_HAS,
	serverNameOf,
	type HandoverCrypto
} from './handover';

const OWNER = '@you:example.com';
const SENSOR = '@sensor:example.com';
const ROOM = '!handover:example.com';

/** One request the module made, as the assertions need to read it. */
interface Made {
	method: string;
	path: string;
	body: Record<string, unknown> | null;
}

/**
 * A homeserver that starts with no handover room and behaves as Synapse does:
 * the alias is 404 until the room is created, the Sensor's membership is
 * `invite` until it joins, and a second `createRoom` with a taken alias is
 * `M_ROOM_IN_USE`.
 */
function homeserver(options: { sensorJoins?: boolean; existingRoom?: string } = {}) {
	const sensorJoins = options.sensorJoins ?? true;
	const made: Made[] = [];
	let roomId: string | null = options.existingRoom ?? null;
	let sensorMembership: string | null = options.existingRoom === undefined ? null : 'join';
	let membershipReads = 0;

	const fetchImpl = (async (url: string | URL, init?: RequestInit) => {
		const path = String(url).replace('https://home.example.com', '');
		const method = init?.method ?? 'GET';
		const body =
			typeof init?.body === 'string' ? (JSON.parse(init.body) as Record<string, unknown>) : null;
		made.push({ method, path, body });

		const answer = (status: number, document: unknown): Response =>
			({ status, json: async () => document }) as unknown as Response;

		if (path.startsWith('/_matrix/client/v3/directory/room/')) {
			return roomId === null
				? answer(404, { errcode: 'M_NOT_FOUND', error: 'Room alias not found' })
				: answer(200, { room_id: roomId, servers: ['example.com'] });
		}
		if (path === '/_matrix/client/v3/createRoom') {
			if (roomId !== null) {
				return answer(400, { errcode: 'M_ROOM_IN_USE', error: 'Room alias already taken' });
			}
			roomId = ROOM;
			sensorMembership = 'invite';
			return answer(200, { room_id: roomId });
		}
		if (path.includes('/state/m.room.member/')) {
			membershipReads += 1;
			// The Sensor accepts on its next sync: `invite` first, then `join`.
			if (sensorMembership === 'invite' && sensorJoins && membershipReads > 1) {
				sensorMembership = 'join';
			}
			return sensorMembership === null
				? answer(404, { errcode: 'M_NOT_FOUND', error: 'not found' })
				: answer(200, { membership: sensorMembership });
		}
		if (path.endsWith('/invite')) {
			sensorMembership = 'invite';
			return answer(200, {});
		}
		return answer(404, { errcode: 'M_UNRECOGNIZED', error: 'no such endpoint' });
	}) as unknown as typeof fetch;

	return { fetchImpl, made };
}

/** A crypto machine that tracks the Sensor once a sync has run. */
function tracking(devices: number): HandoverCrypto & { syncs: number; running: boolean } {
	return {
		syncs: 0,
		running: false,
		async startSync() {
			this.syncs += 1;
			this.running = true;
		},
		stopSync() {
			this.running = false;
		},
		async trackedDeviceCount(userId: string) {
			return this.running && userId === SENSOR ? devices : 0;
		}
	};
}

function run(
	overrides: Partial<Parameters<typeof ensureHandoverRoom>[0]> & {
		fetchImpl: typeof fetch;
		crypto: HandoverCrypto;
	}
) {
	return ensureHandoverRoom({
		baseUrl: 'https://home.example.com',
		accessToken: 'syt_owner_token',
		userId: OWNER,
		sensorUserId: SENSOR,
		roomName: 'Twalk',
		joinDeadlineMs: 50,
		trackingDeadlineMs: 50,
		pollIntervalMs: 1,
		...overrides
	});
}

describe('the room Twalk creates', () => {
	it('is typed as something other than a conversation, for the room’s whole life', () => {
		const body = handoverRoomCreation({ sensorUserId: SENSOR, name: 'Twalk' });
		// `m.room.create` cannot be replaced or redacted in Matrix, so this is
		// the marker that cannot be taken off later — and it is the same field
		// a space uses, which is what `$lib/matrix/rooms.ts` reads.
		expect(body['creation_content']).toEqual({ type: HANDOVER_ROOM_TYPE });
		expect(HANDOVER_ROOM_TYPE).toBe('fr.linagora.twalk.handover');
	});

	it('is encrypted at creation, because that is the whole reason it exists', () => {
		const body = handoverRoomCreation({ sensorUserId: SENSOR, name: 'Twalk' });
		expect(body['initial_state']).toEqual([
			{
				type: 'm.room.encryption',
				state_key: '',
				content: { algorithm: 'm.megolm.v1.aes-sha2' }
			}
		]);
		// A user becomes tracked through a shared *encrypted* room; an
		// unencrypted one would leave the later credential send empty.
		expect(body['invite']).toEqual([SENSOR]);
	});

	it('cannot be posted in by anybody, its creator included', () => {
		const levels = handoverRoomCreation({ sensorUserId: SENSOR, name: 'Twalk' })[
			'power_level_content_override'
		] as Record<string, number>;
		// 101 is above the 100 a room's creator gets, so the homeserver refuses
		// every message event from every member. Verified against Synapse:
		// `user_level (100) < send_level (101)`.
		expect(SEND_LEVEL_NOBODY_HAS).toBeGreaterThan(100);
		expect(levels['events_default']).toBe(SEND_LEVEL_NOBODY_HAS);
		// And the Sensor, at power level 0, cannot bring a third account in.
		expect(levels['invite']).toBe(100);
		expect(levels['kick']).toBe(100);
	});

	it('is not a direct message, so no client files it beside the user’s people', () => {
		expect(handoverRoomCreation({ sensorUserId: SENSOR, name: 'Twalk' })['is_direct']).toBe(false);
		expect(handoverRoomCreation({ sensorUserId: SENSOR, name: 'Twalk' })['room_alias_name']).toBe(
			HANDOVER_ALIAS_LOCALPART
		);
	});
});

describe('the alias that identifies it', () => {
	it('is one per homeserver, read off the user’s own Matrix ID', () => {
		expect(handoverAlias('example.com')).toBe('#twalk-handover:example.com');
		expect(serverNameOf(OWNER)).toBe('example.com');
		expect(serverNameOf('@you:example.com:8448')).toBe('example.com:8448');
		expect(serverNameOf('you:example.com')).toBeNull();
		expect(serverNameOf('@you')).toBeNull();
		expect(serverNameOf('@:example.com')).toBeNull();
	});
});

describe('ensureHandoverRoom', () => {
	it('creates the room, waits for the Sensor to join, and proves the devices are enumerable', async () => {
		const { fetchImpl, made } = homeserver();
		const crypto = tracking(2);
		const outcome = await run({ fetchImpl, crypto });

		expect(outcome).toEqual({ kind: 'ready', roomId: ROOM, sensorDevices: 2, created: true });
		// The whole point: the answer came from the crypto machine's own store,
		// and the sync that fills it was started and stopped again.
		expect(crypto.syncs).toBe(1);
		expect(crypto.running).toBe(false);
		expect(made.filter((request) => request.path === '/_matrix/client/v3/createRoom')).toHaveLength(
			1
		);
	});

	it('is ready only when the machine knows a device: a joined Sensor is not enough', async () => {
		// This is the defect the room exists to prevent, so it has its own
		// outcome rather than being folded into success: with no tracked
		// device, `encryptAndSendToDevice` would put an empty batch on the wire
		// and resolve successfully (ADR 0034).
		const { fetchImpl } = homeserver();
		const outcome = await run({ fetchImpl, crypto: tracking(0) });
		expect(outcome).toEqual({ kind: 'sensor-untracked', roomId: ROOM });
	});

	it('never reports a handover the Sensor did not accept', async () => {
		const { fetchImpl, made } = homeserver({ sensorJoins: false });
		const crypto = tracking(2);
		const outcome = await run({ fetchImpl, crypto });
		expect(outcome).toEqual({ kind: 'sensor-did-not-join', roomId: ROOM });
		// And no sync was started for a room the Sensor is not in.
		expect(crypto.syncs).toBe(0);
		// It was asked, though — `createRoom` carries the invitation, so there
		// is no second call to make — and the membership was read from the
		// homeserver rather than inferred from the invitation succeeding.
		const creation = made.find((request) => request.path === '/_matrix/client/v3/createRoom');
		expect(creation?.body?.['invite']).toEqual([SENSOR]);
		expect(
			made.filter((request) => request.path.includes('/state/m.room.member/')).length
		).toBeGreaterThan(1);
	});

	it('creates no second room when one already exists', async () => {
		const { fetchImpl, made } = homeserver({ existingRoom: ROOM });
		const outcome = await run({ fetchImpl, crypto: tracking(1) });
		expect(outcome).toEqual({ kind: 'ready', roomId: ROOM, sensorDevices: 1, created: false });
		expect(made.filter((request) => request.path === '/_matrix/client/v3/createRoom')).toHaveLength(
			0
		);
	});

	it('treats a taken alias as the room it is, not as a reason to make another', async () => {
		// Two tabs, or a retried onboarding: the alias is the room's identity,
		// so `M_ROOM_IN_USE` is an answer and not an error.
		let calls = 0;
		const { fetchImpl: real } = homeserver({ existingRoom: ROOM });
		const fetchImpl = (async (url: string | URL, init?: RequestInit) => {
			// The first alias read answers 404 — as it would for a browser that
			// resolved it a moment before another tab created the room.
			if (calls === 0 && String(url).includes('/directory/room/')) {
				calls += 1;
				return { status: 404, json: async () => ({ errcode: 'M_NOT_FOUND' }) } as Response;
			}
			calls += 1;
			return real(url as string, init);
		}) as unknown as typeof fetch;

		const outcome = await run({ fetchImpl, crypto: tracking(1) });
		expect(outcome).toEqual({ kind: 'ready', roomId: ROOM, sensorDevices: 1, created: false });
	});

	it('nothing is ever sent into it', async () => {
		// The claim is about the whole run, so it is asserted over every request
		// the module made rather than at one call site. Nothing may create an
		// event in this room: no `/send/`, and no state event of ours beyond
		// what `createRoom` itself writes.
		const { fetchImpl, made } = homeserver();
		await run({ fetchImpl, crypto: tracking(1) });
		expect(made.length).toBeGreaterThan(2);
		for (const request of made) {
			expect(request.path, `${request.method} ${request.path}`).not.toContain('/send/');
			if (request.method === 'PUT' || request.method === 'POST') {
				expect(
					request.path === '/_matrix/client/v3/createRoom' || request.path.endsWith('/invite'),
					`${request.method} ${request.path} wrote something into the room`
				).toBe(true);
			}
		}
	});

	it('says so when the deployment names no Sensor, and asks the homeserver nothing', async () => {
		const { fetchImpl, made } = homeserver();
		expect(await run({ fetchImpl, crypto: tracking(1), sensorUserId: null })).toEqual({
			kind: 'no-sensor'
		});
		expect(await run({ fetchImpl, crypto: tracking(1), sensorUserId: '  ' })).toEqual({
			kind: 'no-sensor'
		});
		expect(made).toHaveLength(0);
	});

	it('carries the homeserver’s own refusal rather than a shrug', async () => {
		const fetchImpl = (async (url: string | URL) =>
			String(url).includes('/directory/room/')
				? ({ status: 404, json: async () => ({ errcode: 'M_NOT_FOUND' }) } as Response)
				: ({
						status: 403,
						json: async () => ({ errcode: 'M_FORBIDDEN', error: 'cannot create rooms' })
					} as Response)) as unknown as typeof fetch;
		const outcome = await run({ fetchImpl, crypto: tracking(1) });
		expect(outcome.kind).toBe('failed');
		expect(outcome.kind === 'failed' && outcome.detail).toContain('M_FORBIDDEN');
	});

	it('never leaves a sync running when the machine throws', async () => {
		const { fetchImpl } = homeserver();
		const crypto: HandoverCrypto & { running: boolean } = {
			running: false,
			async startSync() {
				this.running = true;
			},
			stopSync() {
				this.running = false;
			},
			async trackedDeviceCount() {
				throw new Error('the crypto machine is gone');
			}
		};
		const outcome = await run({ fetchImpl, crypto });
		expect(outcome.kind).toBe('failed');
		expect(crypto.running).toBe(false);
	});
});
