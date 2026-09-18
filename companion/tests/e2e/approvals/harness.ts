// Putting a real suggestion on a real bus, so the approval screen has
// something real to draw.
//
// # Why the events are published by the test
//
// A suggestion is produced by a persona, hosted by Hermes, reasoning with an
// LLM. Bringing all of that up to render one card would make this suite a test
// of Hermes — which has its own, against its own process boundary
// (`hermes/tests/`) — and would make a Companion journey fail for reasons that
// have nothing to do with the Companion. What this screen has to be held to is
// what it does when a suggestion exists, so the suggestion is published
// directly, exactly as the dashboard's journey publishes the inbound events
// that make a contact pending (`tests/e2e/dashboard/bus.ts`).
//
// Everything downstream of that is real: the Gateway reads the suggestion off
// the stream with its own bounded scan, checks its own consent journal, and
// publishes the approved reply itself.
//
// # What a suggestion needs to be approvable
//
// `POST /api/approvals` refuses for fifteen distinct reasons, and four of them
// are about the *setting* rather than the act. So an approvable suggestion
// needs all of:
//
//   1. an `inbound.message.received.v1` on the bus, whose `source` is a portal
//      room (`matrix://<homeserver>/!room:server`) — that is the room the reply
//      is posted into, and its absence is `trigger_has_no_room`;
//   2. that event's `consent` extension reading `granted` — the audit fact at
//      observation time, whose absence is `suggestion_was_never_consented`;
//   3. a consent decision in the Gateway's own journal granting that contact
//      **now** — its absence is `consent_pending`, and its revocation
//      `consent_revoked`;
//   4. a `persona.suggest.produced.v1` whose `subject` is the trigger's id and
//      whose `expires_at` is still in the future.
//
// Each of those is a refusal this suite then goes and provokes on purpose.

import { randomBytes } from 'node:crypto';

import { expect, type APIRequestContext } from '@playwright/test';

import { publish } from '../dashboard/bus';

export { bridgeStack, NO_STACK, signIn } from '../networks/harness';

const INBOUND_TYPE = 'fr.linagora.twalk.inbound.message.received.v1';
const SUGGEST_TYPE = 'fr.linagora.twalk.persona.suggest.produced.v1';
const INBOUND_SUBJECT = 'twalk.inbound.message.received.v1';
const SUGGEST_SUBJECT = 'twalk.persona.suggest.produced.v1';

/** A contract event id: 64 lowercase hex characters, and unique per run. */
export function eventId(): string {
	return randomBytes(32).toString('hex');
}

export interface PublishedSuggestion {
	suggestionId: string;
	triggerId: string;
	contact: string;
	roomId: string;
	/** The persona's proposed reply — the text the screen draws. */
	body: string;
	/** What the *contact* wrote, which must appear nowhere on any screen. */
	inboundBody: string;
	displayName: string;
	networkIdentifier: string;
}

export interface SuggestionOptions {
	serverName: string;
	natsPort: number;
	/** The persona's proposed reply. Distinctive, so a screen can be grepped for it. */
	body: string;
	/** Default: an hour from now. A past instant makes the suggestion expired. */
	expiresAt?: string;
	/** The consent label the Sensor observed. Default `granted`. */
	observedConsent?: 'granted' | 'pending' | 'revoked';
}

/**
 * Publishes a trigger and the suggestion that answers it.
 *
 * The trigger is loaded with the things that must never reach a screen — a
 * body, a display name, a network identifier — for the same reason the
 * Gateway's own suite loads it that way: the assertion that none of them
 * appears is only worth making against an event that actually carried them.
 */
export async function publishSuggestion(
	options: SuggestionOptions
): Promise<PublishedSuggestion> {
	const tag = randomBytes(4).toString('hex');
	const triggerId = eventId();
	const suggestionId = eventId();
	const contact = `@whatsapp_3361${tag}:${options.serverName}`;
	const roomId = `!approvals${tag}:${options.serverName}`;
	const inboundBody = `MARKER-INBOUND-${tag} on décale à 20h ?`;
	const displayName = `MARKER-NAME-${tag}`;
	const networkIdentifier = `+3361${tag}`;
	const observed = options.observedConsent ?? 'granted';
	const now = new Date();

	await publish(options.natsPort, INBOUND_SUBJECT, {
		specversion: '1.0',
		id: triggerId,
		source: `matrix://${options.serverName}/${roomId}`,
		type: INBOUND_TYPE,
		time: now.toISOString(),
		subject: contact,
		datacontenttype: 'application/json',
		dataschema:
			'https://schemas.twalk.dev/cloudevents/v1/inbound.message.received.schema.json',
		network: 'whatsapp',
		consent: observed,
		data: {
			body: inboundBody,
			format: 'text/plain',
			reply_to: null,
			attachments: [],
			contact: { display_name: displayName, network_identifier: networkIdentifier },
			network_timestamp: now.toISOString()
		}
	});

	await publish(options.natsPort, SUGGEST_SUBJECT, {
		specversion: '1.0',
		id: suggestionId,
		source: `hermes://${options.serverName}/personas/assistant`,
		type: SUGGEST_TYPE,
		time: now.toISOString(),
		subject: triggerId,
		datacontenttype: 'application/json',
		dataschema:
			'https://schemas.twalk.dev/cloudevents/v1/persona.suggest.produced.schema.json',
		network: 'whatsapp',
		consent: observed,
		data: {
			persona_id: 'assistant',
			trigger: { event_id: triggerId, event_type: INBOUND_TYPE },
			suggestion: { body: options.body, format: 'text/plain' },
			attempt: 1,
			expires_at: options.expiresAt ?? new Date(now.getTime() + 3_600_000).toISOString()
		}
	});

	return {
		suggestionId,
		triggerId,
		contact,
		roomId,
		body: options.body,
		inboundBody,
		displayName,
		networkIdentifier
	};
}

/** Records one consent decision about a contact, as the owner. */
export async function decideAbout(
	request: APIRequestContext,
	token: string,
	contact: string,
	state: 'granted' | 'revoked' | 'pending'
): Promise<void> {
	const answer = await request.post('/api/consent/decisions', {
		headers: { cookie: `twalk_device=${token}` },
		data: {
			subject: { type: 'contact', id: contact },
			new_state: state,
			scope: { networks: ['whatsapp'] }
		}
	});
	expect(answer.ok(), await answer.text()).toBeTruthy();
}

/**
 * Waits for the Gateway's projection to see a suggestion.
 *
 * The listing is a bounded scan of the stream rather than a store, so there is
 * no consumer to catch up — but NATS acknowledges a publish before every
 * consumer of the stream has it, and a page that loaded in between would be a
 * flake blamed on the screen.
 */
export async function waitForSuggestion(
	request: APIRequestContext,
	token: string,
	suggestionId: string
): Promise<void> {
	await expect
		.poll(
			async () => {
				const answer = await request.get(`/api/suggestions/${suggestionId}`, {
					headers: { cookie: `twalk_device=${token}` }
				});
				return answer.status();
			},
			{ timeout: 30_000 }
		)
		.toBe(200);
}
