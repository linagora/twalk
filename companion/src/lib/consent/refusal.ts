// Every way a consent decision, or a read of the consent state, can fail — and
// what the user is told about each.
//
// The same shape as `$lib/approvals/refusal.ts`, for the same reason and with
// the same two invariants (#100):
//
// **Every answer is terminal.** [`explain`] is total, and `Explained` has no
// value meaning "still working". A screen that reaches this module has stopped
// waiting, so there is nothing here it could render as a spinner (#111, #135,
// #139).
//
// **Every answer names what the user can do.** `remedy` is never optional. A
// cause without a next step is a screen that has told the user they are stuck
// without telling them so.
//
// Why a table rather than a `catch`: the five operations this screen calls
// enumerate nine distinct codes between them, and they are nine different
// sentences. `consent_not_configured` and `contacts_not_configured` in
// particular are statements about the **deployment** — `GATEWAY_NATS_URL` is
// unset, so the Gateway refuses to record a decision it could not announce, and
// consumes no inbound stream — and rendering either as "no contacts are
// waiting" would be the comforting untrue thing: an empty list claims nobody
// has written, which is a very different statement from "this Gateway is not
// watching". `refusal.test.ts` reads `companion-gateway/openapi.yaml` in both
// directions so this table cannot fall behind the Gateway or outlive it.

import type { MessageKey } from '$lib/i18n';
import type { ApiTrouble } from '$lib/api/trouble';

/** What the user can do about a refusal. The same five actions the approval screen has. */
export type Remedy = 'retry' | 'reload' | 'sign-in' | 'diagnostics' | 'none';

export interface Explained {
	/** The Gateway's stable code, carried through for a test and a data attribute. */
	code: string;
	cause: MessageKey;
	/** Never absent. */
	remedyText: MessageKey;
	remedy: Remedy;
}

interface Row {
	cause: MessageKey;
	remedy: Remedy;
}

/**
 * One row per code the Gateway's description enumerates for
 * `POST /api/consent/decisions`, `GET /api/consent/state`,
 * `GET /api/consent/effective`, `GET /api/contacts/pending` and
 * `GET /api/contacts/display-names`.
 */
const TABLE: Record<string, Row> = {
	// The request was wrong. This screen builds every request, so each of these
	// is a defect here rather than something the user did.
	malformed_request: { cause: 'consent.refusal.malformed_request', remedy: 'diagnostics' },
	unknown_value: { cause: 'consent.refusal.unknown_value', remedy: 'diagnostics' },
	unsupported_subject_type: {
		cause: 'consent.refusal.unsupported_subject_type',
		remedy: 'diagnostics'
	},
	scope_contradicts_subject: {
		cause: 'consent.refusal.scope_contradicts_subject',
		remedy: 'diagnostics'
	},

	// The session. Never described as a deployment problem (#116, #141).
	unauthenticated: { cause: 'consent.refusal.unauthenticated', remedy: 'sign-in' },

	// The journal. On a write this is the one refusal that is unambiguously
	// good news about honesty: nothing was recorded and nothing published, so
	// the user is never told a decision is stored when it is not.
	store_unavailable: { cause: 'consent.refusal.store_unavailable', remedy: 'retry' },

	// The two statements about the deployment rather than about a contact.
	consent_not_configured: { cause: 'consent.refusal.consent_not_configured', remedy: 'none' },
	contacts_not_configured: { cause: 'consent.refusal.contacts_not_configured', remedy: 'none' },

	// The display names, and only those: the list itself comes from the
	// Gateway's own store and is unaffected. This is the one code whose remedy
	// is "nothing, and it does not matter much" — the screen shows Matrix IDs,
	// which is what the decision will name anyway.
	bus_unreachable: { cause: 'consent.refusal.bus_unreachable', remedy: 'none' }
};

const REMEDY_TEXT: Record<Remedy, MessageKey> = {
	retry: 'consent.remedy.retry',
	reload: 'consent.remedy.reload',
	'sign-in': 'consent.remedy.signIn',
	diagnostics: 'consent.remedy.diagnostics',
	none: 'consent.remedy.none'
};

/**
 * A code this build has never met. It still names a cause and a next step, and
 * it still says the code, because an operator reading a screenshot needs the
 * word the Gateway used.
 */
const UNKNOWN: Row = { cause: 'consent.refusal.unknown', remedy: 'diagnostics' };

/** Every code this table answers for, for the test that reads the Gateway's description. */
export function knownCodes(): string[] {
	return Object.keys(TABLE);
}

/**
 * Reads one refusal.
 *
 * `trouble` comes from `$lib/api/trouble.ts` and settles the question that
 * costs this project incidents: did anything answer? A transport failure has no
 * code and is not a deployment that refused.
 */
export function explain(trouble: ApiTrouble, code: string | null): Explained {
	if (trouble === 'unreachable') {
		return {
			code: 'unreachable',
			cause: 'api.trouble.unreachable',
			remedyText: REMEDY_TEXT.retry,
			remedy: 'retry'
		};
	}
	if (trouble === 'session-refused') {
		return {
			code: code ?? 'unauthenticated',
			cause: 'consent.refusal.unauthenticated',
			remedyText: REMEDY_TEXT['sign-in'],
			remedy: 'sign-in'
		};
	}
	const row = (code === null ? undefined : TABLE[code]) ?? UNKNOWN;
	return {
		code: code ?? 'unknown',
		cause: row.cause,
		remedyText: REMEDY_TEXT[row.remedy],
		remedy: row.remedy
	};
}
