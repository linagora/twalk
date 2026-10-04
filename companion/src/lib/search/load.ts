// The one call the search screen makes: `GET /api/search`.
//
// Kept out of the component so the screen holds no Gateway vocabulary and the
// result is one discriminated thing rather than an exception it might forget to
// catch. Returns `ok` or a [`Refusal`] that names a cause and a next step —
// never a throw, and never a value meaning "still working" (#111, #135, #139).
//
// # The 401 is not handled here
//
// `$lib/api/client.ts` refreshes a dead device token once, centrally, and
// replays the request (#111). A screen that implemented that itself would be
// the second implementation of a rule the whole app shares. What reaches this
// module is either the answer or a genuinely terminal refusal.
//
// # The Gateway never reads a body, and neither does this
//
// The answer is relayed from the collector, which holds the index and applies
// the consent filter on its side (spec §5.3): a revoked correspondent's hits are
// already gone by the time they arrive, and `withheld` says how many. This
// module does not re-filter, re-count or open anything — it hands the answer to
// `model.ts`, which keeps the row's shape closed.

import { gateway } from '$lib/api/client';
import { troubleOf } from '$lib/api/trouble';
import type { paths } from '$lib/api/schema';
import { explain, rows, type Answer, type Refusal, type Row } from './model';

export interface Query {
	/** The text to search. Required, and the Gateway refuses an empty one. */
	q: string;
	/** One connection id, as `GET /api/connections` names it. */
	source?: string;
	/** RFC 3339 bounds, relayed untouched to the collector. */
	from?: string;
	to?: string;
	limit?: number;
}

export type SearchResult =
	| { ok: true; rows: Row[]; count: number; withheld: number }
	| { ok: false; refusal: Refusal };

/** The Gateway's stable code, or `null` when the body carried none. */
function codeOf(error: unknown): string | null {
	const code = (error as { error?: unknown } | undefined)?.error;
	return typeof code === 'string' ? code : null;
}

/**
 * Runs one search.
 *
 * Filtering to the members the query actually names: `openapi-fetch` serialises
 * an `undefined` parameter as absent, which is what "no bound" means to the
 * collector, so the caller may leave any of `source`/`from`/`to` out.
 */
export async function search(query: Query): Promise<SearchResult> {
	type SearchQuery = NonNullable<
		paths['/api/search']['get']['parameters']['query']
	>;
	const params: SearchQuery = { q: query.q };
	if (query.source !== undefined && query.source !== '') {
		params.source = query.source;
	}
	if (query.from !== undefined) {
		params.from = query.from;
	}
	if (query.to !== undefined) {
		params.to = query.to;
	}
	if (query.limit !== undefined) {
		params.limit = query.limit;
	}

	const answer = await gateway.GET('/api/search', { params: { query: params } }).catch(() => null);
	if (answer === null) {
		return { ok: false, refusal: explain('unreachable', null) };
	}
	if (answer.data !== undefined) {
		const data: Answer = answer.data;
		return {
			ok: true,
			rows: rows(data),
			count: data.count,
			withheld: data.withheld
		};
	}
	return { ok: false, refusal: explain(troubleOf(answer), codeOf(answer.error)) };
}
