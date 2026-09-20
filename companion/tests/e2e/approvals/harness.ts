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

/**
 * The contract's French sentence (`contracts/disclosure/v1/sentences.json`),
 * which is what a persona writing in French selects (#121, ADR 0031). The
 * bodies these journeys publish are French, so this is the sentence they
 * carry unless a spec says otherwise.
 */
export const FRENCH_DISCLOSURE = 'Rédigé avec mon assistant IA.';

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
	/** The sentence the reply discloses itself with, or `null` when it carries none. */
	disclosure: string | null;
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
	/**
	 * The disclosure the suggestion carries as `data.disclosure` (#121).
	 * Default [`FRENCH_DISCLOSURE`]; `null` publishes one without the member,
	 * as a persona from before it existed would.
	 */
	disclosure?: string | null;
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
	const disclosure = options.disclosure === undefined ? FRENCH_DISCLOSURE : options.disclosure;
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
			expires_at: options.expiresAt ?? new Date(now.getTime() + 3_600_000).toISOString(),
			// A field of its own, never inside the body (ADR 0031); the schema
			// has no `null` for it, so an absent member is an absent member.
			...(disclosure === null ? {} : { disclosure })
		}
	});

	return {
		suggestionId,
		triggerId,
		contact,
		roomId,
		body: options.body,
		disclosure,
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
 * Turns the disclosure off, or back on, as the owner — one row appended to
 * the Gateway's journal (#121). The switch is global to the Gateway, and the
 * bridge origin is shared by every project after `networks`, so a spec that
 * turns it off turns it back on in a `finally`.
 */
export async function switchDisclosure(
	request: APIRequestContext,
	token: string,
	enabled: boolean,
	reason?: string
): Promise<{ enabled: boolean; since: string | null; actor: string | null }> {
	const answer = await request.put('/api/settings/disclosure', {
		headers: { cookie: `twalk_device=${token}` },
		data: reason === undefined ? { enabled } : { enabled, reason }
	});
	expect(answer.ok(), await answer.text()).toBeTruthy();
	return (await answer.json()) as { enabled: boolean; since: string | null; actor: string | null };
}

/**
 * How long a suggestion may take to become visible to the Gateway's read.
 *
 * Exported because it is half of [`journeyBudgetMs`]: a wait deliberately placed
 * inside a test has to be smaller than the test's own budget, and until #186 this
 * one was not — see below.
 */
export const SUGGESTION_WAIT_MS = 30_000;

/**
 * How long one of these journeys may take, stated rather than inherited.
 *
 * This project had no budget of its own, so every one of its specs ran under
 * Playwright's default **thirty seconds** — and every one of them begins by
 * waiting up to [`SUGGESTION_WAIT_MS`], which is also thirty. A poll can never
 * use a window as long as the budget containing it: the test dies first, and it
 * dies as a timeout on a spec about approving a reply, which says nothing about
 * what went slowly. That is the `approvals` intermittent reported in #186, and it
 * is the same defect as that ticket's `session` one — a number nobody chose
 * deciding whether the suite is believed.
 *
 * The budget is the longest wait inside it plus a minute for the sign-in, the
 * publish, the screen and the read off NATS. A ceiling, not a delay.
 */
export const journeyBudgetMs = SUGGESTION_WAIT_MS + 60_000;

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
			{ timeout: SUGGESTION_WAIT_MS }
		)
		.toBe(200);
}
