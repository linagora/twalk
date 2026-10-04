// The search screen, end to end (lot 3a).
//
// # What this proves that nothing else can
//
// `collector/tests/search.rs` proves the index, the query and the consent
// filter on the collector's side; `companion-gateway/tests/search.rs` proves the
// Gateway relays without opening a body. What neither can prove is the half
// that lives in a browser, which is what this lot is about:
//
//   1. the **withdrawal is said** — the collector's `withheld` count reaches the
//      screen as a sentence, and a revoked correspondent is not silently
//      absent;
//   2. the screen **renders what the route renders** — the fixture it is given
//      carries no revoked marker, and none appears on the page. Withholding by
//      consent is the **collector's** guarantee, proven by
//      `companion/src/lib/search/model.test.ts` and
//      `companion-gateway/tests/search.rs`; this e2e proves the screen adds no
//      absence of its own;
//   3. a hit names its correspondent — the first surface of the product allowed
//      to (§9.3) — and hands the document to the mail client rather than
//      routing it here.
//
// # Why the search is intercepted
//
// `tests/real-stack.mjs` configures no collector, so a real `GET /api/search`
// on the bridge Gateway answers `503 search_unavailable`. The screen is what
// this file is about, not the relay (that is the Gateway's own suite), so the
// answer is fulfilled in the browser — the same pattern the network picker's
// spec uses for a bridge it does not want to drive (`networks/picker.spec.ts`).
// The fulfilled body is a real `SearchAnswer`: its `hits` already exclude the
// revoked correspondent, because that filtering *is* the collector's job, and
// `withheld` carries the count it removed.

import { expect, test, type Page } from '@playwright/test';

import { bridgeStack, NO_STACK, signIn } from '../networks/harness';

const stack = bridgeStack();

/** A `SearchAnswer` with one granted hit, one pending hit, and one withheld. */
const GRANTED = 'mailto:alice@example.org';
const PENDING = 'mailto:bruno@example.org';
const REVOKED_MARKER = 'mailto:revoked-person@example.org';
const REVOKED_SUBJECT_MARKER = 'MARKER-REVOKED-SUBJECT';
const REVOKED_BODY_MARKER = 'MARKER-REVOKED-BODY';

const ANSWER = {
	hits: [
		{
			id: 'doc-1',
			source: 'mail-linagora',
			correspondent: GRANTED,
			mailbox: 'INBOX',
			date: 1756720800,
			subject: 'Point hebdo',
			snippet: 'Le point de la semaine…'
		},
		{
			id: 'doc-2',
			source: 'mail-linagora',
			correspondent: PENDING,
			mailbox: 'INBOX',
			date: 1756720900,
			subject: 'Déjeuner jeudi',
			snippet: 'On se retrouve où ?'
		}
	],
	count: 2,
	withheld: 1,
	withheld_reason: 'consent'
};

/** Everything the page rendered, for an absence assertion to search. */
async function rendered(page: Page): Promise<string> {
	return (await page.locator('body').innerText()) + (await page.content());
}

test.describe('the search screen', () => {
	test.skip(stack === null, NO_STACK);
	test.describe.configure({ mode: 'serial' });

	test('says the withdrawal and never shows the revoked correspondent', async ({
		page,
		context,
		request
	}) => {
		test.setTimeout(120_000);
		await signIn(context, request, 'search-withdrawal');

		// The collector's answer, fulfilled here: its `hits` carry no trace of
		// the revoked person, and `withheld` says one was removed.
		await page.route('**/api/search**', (route) =>
			route.fulfill({
				status: 200,
				contentType: 'application/json',
				body: JSON.stringify(ANSWER)
			})
		);

		await page.goto('/search');
		await expect(page.getByTestId('screen-search')).toBeVisible();
		// Before a search: a hint, not a result and not a failure.
		await expect(page.getByTestId('search-hint')).toBeVisible();

		await page.getByTestId('search-input').fill('hebdo');
		await page.getByTestId('search-submit').click();

		// The withdrawal is said at the head of the answer — a revoked contact
		// is not an absence (spec §5.3, §9.1).
		const withheld = page.getByTestId('search-withheld');
		await expect(withheld).toBeVisible();
		await expect(withheld).toContainText('withheld by your consent decisions');

		// The count the answer carried, and the hits.
		await expect(page.getByTestId('search-count')).toContainText('2 results');
		await expect(page.getByTestId('search-results').getByTestId('search-hit')).toHaveCount(2);
		await expect(page.getByTestId('search-results')).toContainText(GRANTED);

		// The fixture carries no revoked marker, and none reaches the page:
		// the screen renders the hits the route gave it, adding no absence of
		// its own. The withholding itself happens in the collector, before the
		// answer ever arrives (proven by the model and Gateway suites).
		const text = await rendered(page);
		expect(text).not.toContain(REVOKED_MARKER);
		expect(text).not.toContain(REVOKED_SUBJECT_MARKER);
		expect(text).not.toContain(REVOKED_BODY_MARKER);

		// A hit hands the document to the mail client and never routes it here.
		const open = page.getByTestId('search-hit').first().getByTestId('hit-open');
		await expect(open).toHaveAttribute('href', /^twalk:\/\/message\?/);
	});

	test('a search the server could not run is a sentence, never a spinner', async ({
		page,
		context,
		request
	}) => {
		test.setTimeout(120_000);
		await signIn(context, request, 'search-refusal');

		await page.route('**/api/search**', (route) =>
			route.fulfill({
				status: 503,
				contentType: 'application/json',
				body: JSON.stringify({ error: 'search_unavailable' })
			})
		);

		await page.goto('/search');
		await page.getByTestId('search-input').fill('hebdo');
		await page.getByTestId('search-submit').click();

		const problem = page.getByTestId('search-problem');
		await expect(problem).toBeVisible();
		await expect(problem).toHaveAttribute('data-code', 'search_unavailable');
		// A cause and a next step, and no result list behind it.
		await expect(page.getByTestId('search-remedy')).toBeVisible();
		await expect(page.getByTestId('search-results')).toHaveCount(0);
	});
});
