// The two calls the approval screen makes, and the shape it gets back.
//
// Both return a discriminated result rather than throwing, and neither has a
// value that means "still working": what comes back is the listing, or an
// `Explained` refusal from `./refusal.ts` that names a cause and a next step.
// That is the ticket's rule — *none of them may render as a spinner that never
// ends* (#111, #135, #139) — arranged so that a screen cannot get it wrong:
// there is no third state for it to sit in.
//
// # One approval, one request
//
// [`approve`] takes one suggestion id. There is no `approveMany`, no array
// parameter and no options bag with a list in it, because `POST /api/approvals`
// refuses a body carrying an array with `approval_is_not_a_batch` rather than
// helpfully interpreting it — *"a client that wants to approve three replies
// sends three requests, and the user clicks three times"*. This module keeps
// that true on the Companion's side of the wire.
//
// `approved_by` is deliberately not sent. The Gateway stamps this deployment's
// owner, and a request that names anybody else is a `403`: the one identity
// field of an audit trail is not something a browser should be composing.

import { gateway } from '$lib/api/client';
import { troubleOf } from '$lib/api/trouble';
import type { components, paths } from '$lib/api/schema';
import { explain, type Explained } from './refusal';
import type { Listing } from './rows';

export type Approval = components['schemas']['Approval'];

export type Listed = { ok: true; listing: Listing } | { ok: false; problem: Explained };

export type Approved = { ok: true; approval: Approval } | { ok: false; problem: Explained };

/** The message a suggestion answers, as the contact wrote it (#336). */
export type AnsweredMessage =
	paths['/api/suggestions/{suggestion_event_id}/message']['get']['responses'][200]['content']['application/json'];

export type Answered =
	| { ok: true; message: AnsweredMessage }
	| { ok: false; problem: Explained };

/** The Gateway's stable code, or `null` when the body carried none. */
function codeOf(error: unknown): string | null {
	const code = (error as { error?: unknown } | undefined)?.error;
	return typeof code === 'string' ? code : null;
}

/** The suggestions in the Gateway's read window, newest first. */
export async function loadSuggestions(): Promise<Listed> {
	const answer = await gateway.GET('/api/suggestions').catch(() => null);
	if (answer === null) {
		return { ok: false, problem: explain('unreachable', null) };
	}
	if (answer.data !== undefined) {
		return { ok: true, listing: answer.data };
	}
	return { ok: false, problem: explain(troubleOf(answer), codeOf(answer.error)) };
}

/**
 * Approves one suggestion, optionally with the text the user edited.
 *
 * `final` absent means "send what the persona wrote"; the published event's
 * `edited` flag says which happened and `written_by` says whose words they
 * were (#327) — so the audit trail answers "how
 * often do I correct my assistant?" without keeping a word of what was said.
 */
/**
 * The message one suggestion answers — asked for, never listed.
 *
 * The listing carries nothing of a contact's message on purpose (#110, ADR
 * 0012); this is the one route that reads it, on the owner's own screen,
 * behind their own sign-in, when they ask for this one suggestion (#336).
 * The Gateway reads consent **now**, so a contact revoked since they wrote
 * is a refusal here exactly as they would be at the moment of sending —
 * which is why the caller renders the problem and never a blank quote.
 */
export async function answeredMessage(id: string): Promise<Answered> {
	const answer = await gateway
		.GET('/api/suggestions/{suggestion_event_id}/message', {
			params: { path: { suggestion_event_id: id } }
		})
		.catch(() => null);
	if (answer === null) {
		return { ok: false, problem: explain('unreachable', null) };
	}
	if (answer.data !== undefined) {
		return { ok: true, message: answer.data };
	}
	return { ok: false, problem: explain(troubleOf(answer), codeOf(answer.error)) };
}

export async function approve(
	id: string,
	edited?: string,
	writtenBy: 'persona' | 'owner' = 'persona'
): Promise<Approved> {
	const answer = await gateway
		.POST('/api/approvals', {
			body: {
				suggestion_event_id: id,
				...(edited === undefined
					? {}
					: {
							final: {
								body: edited,
								format: 'text/plain' as const,
								// #327: who wrote this text, declared by the gesture that
								// produced it — correcting the draft leaves it the
								// persona's, writing one's own reply does not. It is what
								// decides whether the disclosure is appended, so the
								// screen states it rather than letting the Gateway guess
								// from a body that merely differs.
								written_by: writtenBy
							}
						})
			}
		})
		.catch(() => null);
	if (answer === null) {
		return { ok: false, problem: explain('unreachable', null) };
	}
	if (answer.data !== undefined) {
		return { ok: true, approval: answer.data };
	}
	return { ok: false, problem: explain(troubleOf(answer), codeOf(answer.error)) };
}
