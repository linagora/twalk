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
import type { components } from '$lib/api/schema';
import { explain, type Explained } from './refusal';
import type { Listing } from './rows';

export type Approval = components['schemas']['Approval'];

export type Listed = { ok: true; listing: Listing } | { ok: false; problem: Explained };

export type Approved = { ok: true; approval: Approval } | { ok: false; problem: Explained };

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
 * `final` absent means "send what the persona wrote", and the published
 * event's `edited` flag says which happened — so the audit trail answers "how
 * often do I correct my assistant?" without keeping a word of what was said.
 */
export async function approve(id: string, edited?: string): Promise<Approved> {
	const answer = await gateway
		.POST('/api/approvals', {
			body: {
				suggestion_event_id: id,
				...(edited === undefined ? {} : { final: { body: edited, format: 'text/plain' as const } })
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
