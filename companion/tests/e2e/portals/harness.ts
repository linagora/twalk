// What the conversation chooser's journey needs: real portal rooms, built the
// way a bridge builds one, and a way to ask the homeserver where the Sensor
// stands in them (ticket #143).
//
// # Why the rooms are made through an appservice
//
// A portal room is created by the bridge's **bot** and the bot is an appservice
// user. That is not decoration here: the Gateway reads the register by naming
// that bot in `?user_id=`, and Synapse honours that parameter only for a token
// belonging to an appservice — for any other token it is silently ignored and
// the call acts as the token's own user. So a fixture built on an ordinary
// account could not exercise the path the screen depends on at all (#171).
//
// `tests/real-stack.mjs` registers the bot and the ghosts through
// `tests/harness/synapse/appservice-portals.yaml` before the Gateway starts;
// everything below acts as one of them.
//
// # Why the member counts are real
//
// The criterion this screen exists for is that ticking a conversation states
// how many people it covers **before** the tick takes effect, and past a crowd
// requires that number to be acknowledged. A fixture that faked the count would
// assert the screen against itself. So the crowded group really does hold
// twenty-two accounts, each registered and each joined, and the number the
// browser renders is the homeserver's own answer.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

/** What `tests/real-stack.mjs` wrote about this run's portal fixture. */
export interface PortalStack {
	synapseUrl: string;
	serverName: string;
	natsPort: number;
	portals: {
		/** The stack's appservice token: what the Gateway holds as an `as_token`. */
		appserviceToken: string;
		/** The bridge bot, in every portal room and the account the register acts as. */
		bot: string;
		/** One network ghost, standing for the person on the other side. */
		ghost: string;
		/** Enough accounts to make a group a crowd. */
		crowd: string[];
		/** `GATEWAY_CROWD_THRESHOLD` the stack's Gateway was started with (#252). */
		crowdThreshold: number;
		/** The account the register watches for, and the one the Sensor logs in as. */
		sensorId: string;
	};
}

const STACK_FILE = join(
	dirname(fileURLToPath(import.meta.url)),
	'..',
	'..',
	'.bridge-stack.json'
);

export function portalStack(): PortalStack | null {
	if (process.env.TWALK_TEST_REAL_STACK !== '1') {
		return null;
	}
	try {
		const stack = JSON.parse(readFileSync(STACK_FILE, 'utf8')) as Partial<PortalStack>;
		return stack.portals === undefined ? null : (stack as PortalStack);
	} catch {
		return null;
	}
}

export const NO_STACK =
	'needs a real Gateway, a real Synapse, the stub bridge and a real Sensor: run `npm run test:e2e:stack` (Docker and cargo required)';

// The Sensor is `tests/e2e/consent/sensor.ts`'s: its durable consent consumer
// has a constant name, so two of them on one bus would split the consent stream
// between them, and one helper starting one process is the only shape that
// stays true. `playwright.config.ts` orders this project after `consent` for
// the same reason.

/** One client-server call as an appservice user. */
async function asUser(
	stack: PortalStack,
	userId: string,
	method: string,
	path: string,
	body?: unknown
): Promise<unknown> {
	const url = `${stack.synapseUrl}${path}${path.includes('?') ? '&' : '?'}user_id=${encodeURIComponent(userId)}`;
	const answer = await fetch(url, {
		method,
		headers: {
			'content-type': 'application/json',
			authorization: `Bearer ${stack.portals.appserviceToken}`
		},
		body: body === undefined ? undefined : JSON.stringify(body)
	});
	if (!answer.ok) {
		throw new Error(`${method} ${path} as ${userId}: ${answer.status} ${await answer.text()}`);
	}
	return answer.json();
}

export interface BuiltPortal {
	readonly roomId: string;
	readonly name: string;
	readonly conversationId: string;
	/** How many people the register will count: the members, less the bot. */
	readonly members: number;
}

/**
 * A portal room, as a bridge builds one when a conversation becomes active.
 *
 * The bot creates it — so it is the room's admin, which is what lets it invite
 * the Sensor later — marks it with `m.bridge`, and pulls each member in.
 * `conversationId` is what the marker's `channel.id` carries: the network's own
 * address for the conversation, whose suffix is what the chooser reads to know
 * whether this is one person or a group. It is spelled the way WhatsApp spells
 * one, because a placeholder would let the whole path work for a value no
 * bridge produces.
 */
export async function buildPortal(
	stack: PortalStack,
	name: string,
	conversationId: string,
	members: readonly string[]
): Promise<BuiltPortal> {
	const bot = stack.portals.bot;
	const created = (await asUser(stack, bot, 'POST', '/_matrix/client/v3/createRoom', {
		name,
		preset: 'private_chat'
	})) as { room_id: string };
	const roomId = created.room_id;
	const stateKey = encodeURIComponent(`test.twalk/whatsapp`);
	await asUser(
		stack,
		bot,
		'PUT',
		`/_matrix/client/v3/rooms/${encodeURIComponent(roomId)}/state/m.bridge/${stateKey}`,
		{
			bridgebot: bot,
			protocol: { id: 'whatsapp', displayname: 'WhatsApp' },
			channel: { id: conversationId, displayname: name }
		}
	);
	// In parallel, because the crowded group holds twenty-two accounts and
	// forty-four sequential round trips is most of a hook's budget. Each
	// member's own invite-then-join stays ordered; only the members race, and
	// the test stack's Synapse has its rate limits off.
	const room = encodeURIComponent(roomId);
	await Promise.all(
		members.map(async (member) => {
			await asUser(stack, bot, 'POST', `/_matrix/client/v3/rooms/${room}/invite`, {
				user_id: member
			});
			await asUser(stack, member, 'POST', `/_matrix/client/v3/rooms/${room}/join`, {});
		})
	);
	return { roomId, name, conversationId, members: members.length };
}

/** Where the Sensor stands in a room, as the homeserver answers — not the Gateway. */
export async function sensorMembership(
	stack: PortalStack,
	roomId: string
): Promise<string | null> {
	const url =
		`${stack.synapseUrl}/_matrix/client/v3/rooms/${encodeURIComponent(roomId)}` +
		`/state/m.room.member/${encodeURIComponent(stack.portals.sensorId)}` +
		`?user_id=${encodeURIComponent(stack.portals.bot)}`;
	const answer = await fetch(url, {
		headers: { authorization: `Bearer ${stack.portals.appserviceToken}` }
	});
	if (!answer.ok) {
		return null;
	}
	const body = (await answer.json()) as { membership?: string };
	return body.membership ?? null;
}

/** A message from the person on the other side of a conversation. */
export async function ghostSays(
	stack: PortalStack,
	roomId: string,
	ghost: string,
	body: string
): Promise<string> {
	const txn = `twalk-c143-${Date.now()}-${Math.floor(Math.random() * 1000)}`;
	const sent = (await asUser(
		stack,
		ghost,
		'PUT',
		`/_matrix/client/v3/rooms/${encodeURIComponent(roomId)}/send/m.room.message/${txn}`,
		{ msgtype: 'm.text', body }
	)) as { event_id: string };
	return sent.event_id;
}

/**
 * A WhatsApp group id, spelled the way WhatsApp spells one. `at` keeps two
 * groups of one fixture distinguishable, which is the whole point of the id
 * being on the screen.
 */
export function groupId(at: number): string {
	return `1203632019803${String(at).padStart(5, '0')}@g.us`;
}

/** A phone-addressed WhatsApp conversation: one person, said by the network. */
export function personId(phone: string): string {
	return `${phone}@s.whatsapp.net`;
}
