import { describe, it, expect } from 'vitest';
import { atBound, documentLink, explain, rows, snippet, when, withheldCopy, SEARCH_LIMIT } from './model';

describe('search results', () => {
	it('says the withdrawal when there is one, and stays silent when there is not', () => {
		// Le retrait est dit, pas caché (§9) : un contact révoqué n'est pas
		// une absence.
		const t = (key: string, values?: Record<string, unknown>) =>
			`${key}:${JSON.stringify(values)}`;
		expect(withheldCopy({ withheld: 3 }, t)).toBe('search.withheld:{"count":3}');
		expect(withheldCopy({ withheld: 0 }, t)).toBeNull();
	});

	it('renders a row per hit and never invents a body', () => {
		const rows_ = rows({
			hits: [
				{
					id: 'doc-1',
					source: 'mail-linagora',
					correspondent: 'mailto:alice@example.org',
					mailbox: 'inbox',
					date: 1756720800,
					subject: 'Point hebdo',
					snippet: 'Le point de la semaine…'
				}
			]
		});
		expect(rows_).toHaveLength(1);
		expect(rows_[0].correspondent).toBe('mailto:alice@example.org');
		expect(rows_[0]).not.toHaveProperty('body');
	});

	it('builds a closed row, dropping any member the collector did not ask for', () => {
		// A `body` the index might one day expose must not travel through an
		// identity mapping: `rows` names each member itself.
		const rows_ = rows({
			hits: [
				{
					id: 'doc-2',
					correspondent: 'mailto:bob@example.org',
					// Not a member of `Row`; must not reach the screen.
					body: 'le texte entier du message',
					snippet: 'un extrait'
				}
			]
		});
		expect(rows_[0]).not.toHaveProperty('body');
		expect(rows_[0].snippet).toBe('un extrait');
		// A missing subject and mailbox become empty values, not `undefined`.
		expect(rows_[0].subject).toBe('');
		expect(rows_[0].mailbox).toBeNull();
	});

	it('bounds an excerpt in characters and only adds an ellipsis when it cut', () => {
		expect(snippet('abcdef', 3)).toBe('abc…');
		expect(snippet('abc', 3)).toBe('abc');
		// Counted by characters, so an accented body is not split mid-glyph.
		expect(snippet('éàüö', 2)).toBe('éà…');
	});

	it('dates a hit, and invents no date for one that carries none', () => {
		expect(when(0, 'en')).toBeNull();
		expect(when(1756720800, 'en')).toContain('2025');
	});

	it('hands a document to the mail client and never routes it here', () => {
		const [row] = rows({
			hits: [{ id: 'doc-3', source: 'mail-linagora', correspondent: 'mailto:z@example.org' }]
		});
		expect(documentLink(row)).toBe('twalk://message?id=doc-3&connection=mail-linagora');
		const [noId] = rows({ hits: [{}] });
		expect(documentLink(noId)).toBeNull();
	});

	it('names a cause and a next step for every refusal, known or not', () => {
		expect(explain('unreachable', null).remedy).toBe('retry');
		expect(explain('session-refused', null).remedy).toBe('sign-in');
		expect(explain('refused', 'invalid_query').cause).toBe('search.refusal.invalid_query');
		expect(explain('refused', 'search_unavailable').cause).toBe('search.refusal.search_unavailable');
		// A code this build has never met still names the code and a next step.
		const unknown = explain('refused', 'invented_tomorrow');
		expect(unknown.code).toBe('invented_tomorrow');
		expect(unknown.remedyText).toBe('search.remedy.diagnostics');
	});

	it('says the list is at its bound only when it is', () => {
		expect(atBound({ count: SEARCH_LIMIT })).toBe(true);
		expect(atBound({ count: SEARCH_LIMIT + 1 })).toBe(true);
		expect(atBound({ count: SEARCH_LIMIT - 1 })).toBe(false);
	});
});
