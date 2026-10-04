// What the search screen knows about one result, and what it is allowed to
// say about the whole answer.
//
// # A row has no member a message body could sit in
//
// The collector indexes a body and serves a **bounded excerpt** — `snippet`,
// never the document (spec §5.1, §9.3). This module does not pass the
// collector's object through: [`rows`] builds an explicit `Row` from the
// members it names, so a `body` the collector might one day grow could not
// reach this screen by accident. That is the same shape `$lib/approvals/rows.ts`
// keeps for a contact's words — a type a screen cannot leak through.
//
// # What may be named here, and what may not
//
// This is the **first** surface of the product where a contact's name is
// legitimate (spec §9.3): the owner is searching their own archive, in their
// own session, and showing who wrote is the function of the screen. It is the
// opposite of the approval screen, which has only an id and a type and cannot
// name anybody (#97, ADR 0012). The excerpt stays bounded all the same.
//
// # The withdrawal is said, never hidden
//
// A correspondent the user revoked is removed from the answer by the collector
// and **counted** (`withheld`) — because a silence is the one failure this
// product has shipped several times without noticing (spec §5.3). So
// [`withheldCopy`] renders that count, and returns `null` — not an empty
// sentence — when there is nothing to say: an empty sentence would read as a
// withdrawal of zero.
//
// # Nothing here is "still working"
//
// [`explain`] is total: every refusal code the Gateway enumerates for
// `GET /api/search` has a cause and a next step, and an unknown code gets the
// code named and a diagnostics pointer. There is no value here a screen could
// sit in and spin (#111, #135, #139).

import type { ApiTrouble } from '$lib/api/trouble';
import type { MessageKey, MessageValues } from '$lib/i18n';
import type { paths } from '$lib/api/schema';

/** What `GET /api/search` answers, exactly as the contract describes it. */
export type Answer =
	paths['/api/search']['get']['responses'][200]['content']['application/json'];

/** One hit as the collector shapes it — a `snippet` and never a `body`. */
export type Hit = Answer['hits'][number];

/**
 * One result, as the screen draws it.
 *
 * Every member is required and none is a message body: the collector may answer
 * a hit missing a subject or a mailbox, and the screen still has a row to draw
 * rather than an `undefined` to render. `mailbox` is `null` when the source has
 * no folders, which is a fact and not a gap.
 */
export interface Row {
	id: string;
	source: string;
	correspondent: string;
	mailbox: string | null;
	/** Seconds since the Unix epoch, `0` when the collector reported none. */
	date: number;
	subject: string;
	snippet: string;
}

type Translate = (key: MessageKey, values?: MessageValues) => string;

/**
 * The rows to draw, built member by member.
 *
 * Deliberately not `answer.hits`. A spread or an identity here would let a
 * member this module does not name — a full text, one day — travel from the
 * collector's answer straight into the markup. Building the literal is what
 * keeps `Row` a closed shape.
 */
export function rows(answer: { hits: readonly Hit[] }): Row[] {
	return answer.hits.map((hit) => ({
		id: hit.id ?? '',
		source: hit.source ?? '',
		correspondent: hit.correspondent ?? '',
		mailbox: hit.mailbox ?? null,
		date: hit.date ?? 0,
		subject: hit.subject ?? '',
		snippet: hit.snippet ?? ''
	}));
}

/**
 * The sentence for a withdrawal, or `null` when there is none.
 *
 * `null` rather than `''`: a screen that rendered an empty withdrawal sentence
 * would be saying "0 results withheld" in the same breath as "there were 40",
 * which is worse than saying nothing. The screen renders the sentence only when
 * this returns a string.
 */
export function withheldCopy(answer: { withheld: number }, t: Translate): string | null {
	if (answer.withheld <= 0) {
		return null;
	}
	return t('search.withheld', { count: answer.withheld });
}

/**
 * A bounded excerpt of a text — the same rule the collector applies to a body
 * before it ever reaches here, so a screen that had to shorten a snippet for
 * display would do it by the one rule rather than a second one.
 *
 * Counted in characters, not bytes: the product's own bodies are French and
 * Arabic as often as English, and cutting a UTF-8 continuation byte in half
 * renders a replacement glyph. The ellipsis is added only when something was
 * cut, so a complete excerpt never claims to be truncated.
 */
export function snippet(text: string, limit: number): string {
	const cut: string = [...text].slice(0, Math.max(0, limit)).join('');
	return [...text].length > limit ? `${cut}…` : cut;
}

/**
 * A hit's date, in the locale in force — `null` when the collector reported
 * none.
 *
 * `null` in, `null` out: `0` is "1 January 1970", which is a date a screen must
 * not invent for a document whose date the index did not carry.
 */
export function when(date: number, locale: string): string | null {
	if (date <= 0) {
		return null;
	}
	return new Intl.DateTimeFormat(locale, { dateStyle: 'medium' }).format(new Date(date * 1000));
}

/**
 * The link that hands one document to the user's mail client (spec §9.4).
 *
 * Twalk is not a mail client: it renders the trace and never the document, so
 * the click is a hand-over and not a route on this origin. The scheme is the
 * design's; `null` when the hit carries no id to hand over, so the screen draws
 * a plain row rather than a link to nowhere.
 */
export function documentLink(row: Row): string | null {
	if (row.id === '') {
		return null;
	}
	const query = new URLSearchParams({ id: row.id });
	if (row.source !== '') {
		query.set('connection', row.source);
	}
	return `twalk://message?${query.toString()}`;
}

/** What the user can do about a refusal. Five actions, not five words for "try again". */
export type Remedy = 'retry' | 'reload' | 'sign-in' | 'diagnostics' | 'none';

export interface Refusal {
	/** The Gateway's or the collector's stable code, carried for a data attribute. */
	code: string;
	/** The sentence that names the cause. */
	cause: MessageKey;
	/** The sentence that says what to do. Never absent. */
	remedyText: MessageKey;
	remedy: Remedy;
}

interface Rule {
	cause: MessageKey;
	remedy: Remedy;
}

/**
 * Every code `GET /api/search` enumerates in `companion-gateway/openapi.yaml`,
 * plus the transport failures that carry no code at all.
 *
 * The four collector codes keep their own sentences: "this deployment has no
 * index", "the index would not open" and "this Gateway has no collector" are
 * three different hands to turn to, and collapsing them into one is the
 * conflation `$lib/api/trouble.ts` exists to prevent, one level up.
 */
const TABLE: Record<string, Rule> = {
	// The query itself (400). The user typed it, so the user can fix it.
	invalid_query: { cause: 'search.refusal.invalid_query', remedy: 'none' },
	invalid_window: { cause: 'search.refusal.invalid_window', remedy: 'none' },

	// The index behind the Gateway (503). An operator's, named as such.
	index_not_configured: { cause: 'search.refusal.index_not_configured', remedy: 'none' },
	index_unavailable: { cause: 'search.refusal.index_unavailable', remedy: 'retry' },
	search_unavailable: { cause: 'search.refusal.search_unavailable', remedy: 'none' },

	// The collector behind the Gateway (502).
	collector_unreachable: { cause: 'search.refusal.collector_unreachable', remedy: 'retry' },
	collector_refused: { cause: 'search.refusal.collector_refused', remedy: 'diagnostics' }
};

const REMEDY_TEXT: Record<Remedy, MessageKey> = {
	retry: 'search.remedy.retry',
	reload: 'search.remedy.reload',
	'sign-in': 'search.remedy.signIn',
	diagnostics: 'search.remedy.diagnostics',
	none: 'search.remedy.none'
};

/**
 * A code this build has never met: still a cause and a next step, and still the
 * code, because an operator reading a screenshot needs the word the server used.
 */
const UNKNOWN: Rule = { cause: 'search.refusal.unknown', remedy: 'diagnostics' };

/** Every code this table answers for. */
export function knownCodes(): string[] {
	return Object.keys(TABLE);
}

/**
 * Reads one failed search.
 *
 * `trouble` comes from `$lib/api/trouble.ts` and settles the question that
 * costs this project incidents: did anything answer? A transport failure has no
 * code and is not a deployment that refused.
 */
export function explain(trouble: ApiTrouble, code: string | null): Refusal {
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
			cause: 'api.trouble.sessionRefused',
			remedyText: REMEDY_TEXT['sign-in'],
			remedy: 'sign-in'
		};
	}
	const rule = (code === null ? undefined : TABLE[code]) ?? UNKNOWN;
	return {
		code: code ?? 'unknown',
		cause: rule.cause,
		remedyText: REMEDY_TEXT[rule.remedy],
		remedy: rule.remedy
	};
}

/**
 * How many hits one search asks for.
 *
 * Named here so the screen and its bound sentence cannot disagree: the
 * collector clamps the number to its own bounds, and this is the number the
 * screen asked with. When the answer comes back full, the screen says the list
 * is at its bound — the bound is in the answer (`count`) and not only in the
 * configuration (the `window.reached_start_of_stream` pattern, spec §5.4).
 */
export const SEARCH_LIMIT = 20;

/** Whether an answer came back at the bound the screen asked for. */
export function atBound(answer: { count: number }): boolean {
	return answer.count >= SEARCH_LIMIT;
}
