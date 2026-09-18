// Every way approving a reply can fail, and what the user is told about each.
//
// # Why this is a table and not a `catch`
//
// `POST /api/approvals` refuses with fifteen distinct codes, plus the three
// the request itself can be wrong in, plus `503` for a deployment that
// approves nothing — and the Gateway's own description says why they are not
// one code: *"each one is a different sentence for the user, and collapsing
// them would be the defect that cost this project seven incidents in two
// days"*. #111, #135 and #139 are three of those, and each one is the same
// shape: a screen met a failure it had no sentence for, so it showed the
// sentence it had — usually none, usually a spinner.
//
// So this module is the sentence for every code, written down once, and a test
// reads `companion-gateway/openapi.yaml` and fails when the Gateway grows a
// code this table has not met. That is the only way the table can be trusted:
// a default branch that says "something went wrong" would pass every test
// anybody would think to write.
//
// # Two invariants, both load-bearing
//
// **Every answer is terminal.** [`explain`] is total — it returns an
// `Explained` for anything, including a code invented this morning — and there
// is no value in `Explained` that means "still working". A screen that reaches
// this module has stopped waiting. There is no state a caller can be left in
// where it renders a spinner, because there is nothing here to render one
// from.
//
// **Every answer names what the user can do.** `remedy` is never optional. A
// cause without a next step is a screen that has told the user they are stuck
// without telling them so.
//
// **One answer is not a failure at all.** `approval_published_but_not_recorded`
// means the reply *went out* and the Gateway could not write that down. It is
// a `500`, and telling the user their message was not sent would be a lie that
// makes them send it twice. `sent` is the member that keeps that apart, and it
// is the reason this type has a member most error types do not.

import type { MessageKey } from '$lib/i18n';
import type { ApiTrouble } from '$lib/api/trouble';

/**
 * What the user can do about a refusal. Five, because there are five different
 * actions — not five different words for "try again".
 */
export type Remedy =
	/** The same request may work: the bus or the store was momentarily out. */
	| 'retry'
	/** The screen's picture of the world is stale; re-reading fixes it. */
	| 'reload'
	/** The session is gone. Nothing is wrong with the deployment. */
	| 'sign-in'
	/** This is a defect in the Companion or a misconfiguration. */
	| 'diagnostics'
	/** Nothing the user can do from here, and saying so is the honest answer. */
	| 'none';

export interface Explained {
	/** The Gateway's stable code, carried through for a test and a data attribute. */
	code: string;
	/** The sentence that names the cause. */
	cause: MessageKey;
	/** The sentence that says what to do. Never absent. */
	remedyText: MessageKey;
	remedy: Remedy;
	/**
	 * Whether the reply went out despite the refusal. True for exactly one
	 * code, and a screen that ignores it tells the user to send twice.
	 */
	sent: boolean;
}

interface Row {
	cause: MessageKey;
	remedy: Remedy;
	sent?: true;
}

/**
 * The table. One row per code the Gateway's OpenAPI description enumerates for
 * `POST /api/approvals`, `GET /api/suggestions` and `GET /api/suggestions/{id}`
 * — the two doors onto one fact answer with the same vocabulary, deliberately
 * (`suggestions_http.rs`), so one table serves both.
 */
const TABLE: Record<string, Row> = {
	// The request was wrong. Nothing the user did: this screen builds the
	// request, so these are defects here.
	malformed_request: { cause: 'approvals.refusal.malformed_request', remedy: 'diagnostics' },
	approval_is_not_a_batch: {
		cause: 'approvals.refusal.approval_is_not_a_batch',
		remedy: 'diagnostics'
	},
	unknown_value: { cause: 'approvals.refusal.unknown_value', remedy: 'diagnostics' },
	approved_by_is_not_the_owner: {
		cause: 'approvals.refusal.approved_by_is_not_the_owner',
		remedy: 'diagnostics'
	},

	// The session. Never described as a deployment problem (#116, #141).
	unauthenticated: { cause: 'approvals.refusal.unauthenticated', remedy: 'sign-in' },

	// It is not there, and the bus was read to its first retained message.
	suggestion_not_found: { cause: 'approvals.refusal.suggestion_not_found', remedy: 'reload' },
	trigger_not_found: { cause: 'approvals.refusal.trigger_not_found', remedy: 'none' },

	// It may be there, further back than this Gateway reads. A different
	// sentence from the two above, because "gone" and "never was" lead a user
	// to different actions.
	suggestion_out_of_reach: { cause: 'approvals.refusal.suggestion_out_of_reach', remedy: 'none' },
	trigger_out_of_reach: { cause: 'approvals.refusal.trigger_out_of_reach', remedy: 'none' },

	// It exists and cannot be approved. Seven facts, seven sentences.
	suggestion_expired: { cause: 'approvals.refusal.suggestion_expired', remedy: 'none' },
	consent_revoked: { cause: 'approvals.refusal.consent_revoked', remedy: 'none' },
	consent_pending: { cause: 'approvals.refusal.consent_pending', remedy: 'none' },
	suggestion_was_never_consented: {
		cause: 'approvals.refusal.suggestion_was_never_consented',
		remedy: 'none'
	},
	already_approved: { cause: 'approvals.refusal.already_approved', remedy: 'reload' },
	trigger_has_no_room: { cause: 'approvals.refusal.trigger_has_no_room', remedy: 'none' },
	suggestion_unreadable: { cause: 'approvals.refusal.suggestion_unreadable', remedy: 'none' },

	// The things behind the Gateway.
	bus_unreachable: { cause: 'approvals.refusal.bus_unreachable', remedy: 'retry' },
	store_unavailable: { cause: 'approvals.refusal.store_unavailable', remedy: 'retry' },

	// The one that is not a failure. `sent` is why this table has that member.
	approval_published_but_not_recorded: {
		cause: 'approvals.refusal.approval_published_but_not_recorded',
		remedy: 'reload',
		sent: true
	},

	// This deployment does not do this at all — a statement about the
	// deployment, never about the suggestion, which is why the Gateway answers
	// it before looking at one.
	approvals_not_configured: { cause: 'approvals.refusal.approvals_not_configured', remedy: 'none' },
	suggestions_not_configured: {
		cause: 'approvals.refusal.suggestions_not_configured',
		remedy: 'none'
	}
};

const REMEDY_TEXT: Record<Remedy, MessageKey> = {
	retry: 'approvals.remedy.retry',
	reload: 'approvals.remedy.reload',
	'sign-in': 'approvals.remedy.signIn',
	diagnostics: 'approvals.remedy.diagnostics',
	none: 'approvals.remedy.none'
};

/**
 * A code this build has never met.
 *
 * It still names a cause and a next step, and it still says the code, because
 * an operator reading a screenshot needs the word the Gateway used. What it
 * must never do is look like success or like waiting.
 */
const UNKNOWN: Row = { cause: 'approvals.refusal.unknown', remedy: 'diagnostics' };

/** Every code this table answers for, for the test that reads the Gateway's description. */
export function knownCodes(): string[] {
	return Object.keys(TABLE);
}

/**
 * Reads one refusal.
 *
 * `trouble` comes from `$lib/api/trouble.ts` and settles the question that
 * costs this project incidents: did anything answer? A transport failure has
 * no code and is not a deployment that refused.
 */
export function explain(trouble: ApiTrouble, code: string | null): Explained {
	if (trouble === 'unreachable') {
		return {
			code: 'unreachable',
			cause: 'api.trouble.unreachable',
			remedyText: REMEDY_TEXT.retry,
			remedy: 'retry',
			sent: false
		};
	}
	if (trouble === 'session-refused') {
		return {
			code: code ?? 'unauthenticated',
			cause: 'approvals.refusal.unauthenticated',
			remedyText: REMEDY_TEXT['sign-in'],
			remedy: 'sign-in',
			sent: false
		};
	}
	const row = (code === null ? undefined : TABLE[code]) ?? UNKNOWN;
	return {
		code: code ?? 'unknown',
		cause: row.cause,
		remedyText: REMEDY_TEXT[row.remedy],
		remedy: row.remedy,
		sent: row.sent === true
	};
}
