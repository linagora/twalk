// One tab at a time (ADR 0014).
//
// matrix-js-sdk is explicit: two `MatrixClient` instances on one IndexedDB
// "will cause data corruption and decryption failures". The damage is silent —
// a corrupt crypto store does not announce itself, it fails to decrypt
// something weeks later — so the guard has to be structural, and a Web Lock is
// the only guard a browser releases when the tab holding it dies.
//
// Both tabs here are in **one browser context**, because that is what two tabs
// of one browser is: a Web Lock is scoped to an origin within a context, and
// two contexts would be two browsers and would prove nothing.

import { expect, test } from '@playwright/test';

test.describe('two tabs on one crypto store', () => {
	test('the second says so, and can take over', async ({ browser }) => {
		const context = await browser.newContext({ viewport: { width: 390, height: 844 } });

		const first = await context.newPage();
		await first.goto('/');
		await expect(first.getByTestId('screen-welcome')).toBeVisible();

		// The second tab loses the election and gets the explanation instead
		// of the app.
		const second = await context.newPage();
		await second.goto('/');
		await expect(second.getByTestId('screen-tab-elsewhere')).toBeVisible();
		await expect(second.getByTestId('screen-welcome')).toHaveCount(0);

		// Taking over: the holder is asked over a `BroadcastChannel`, lets go
		// of its client and its lock, and becomes the one that says so.
		await second.getByTestId('take-over').click();
		await expect(second.getByTestId('screen-welcome')).toBeVisible();
		await expect(first.getByTestId('screen-tab-elsewhere')).toBeVisible();

		// Closing the active tab releases the lock, and the other takes it
		// back without anyone clicking anything: this is the property a
		// hand-rolled `localStorage` mutex does not have.
		await second.close();
		await expect(first.getByTestId('screen-welcome')).toBeVisible({ timeout: 15_000 });

		await context.close();
	});

	test('diagnostics stays reachable in the tab that lost', async ({ browser }) => {
		// The page a user is sent to when something is wrong must never be
		// the page a lock hides.
		const context = await browser.newContext();
		const first = await context.newPage();
		await first.goto('/');
		await expect(first.getByTestId('screen-welcome')).toBeVisible();

		const second = await context.newPage();
		await second.goto('/diagnostics');
		await expect(second.getByTestId('screen-diagnostics')).toBeVisible();

		await context.close();
	});
});

test.describe('a holder that cannot answer', () => {
	test('the take-over says so, and names what works', async ({ browser }) => {
		// The case found live (#135), and the one the measurements pinned: the
		// holder is **alive** — a Web Lock dies with its tab, so a lock still
		// held is a tab still there — and running no JavaScript, because the
		// browser froze it. A `BroadcastChannel` that delivers nothing is that
		// tab exactly, without waiting for Memory Saver to decide.
		const context = await browser.newContext({ viewport: { width: 390, height: 844 } });

		const frozen = await context.newPage();
		await frozen.addInitScript(() => {
			class Deaf {
				onmessage: unknown = null;
				postMessage() {}
				addEventListener() {}
				removeEventListener() {}
				close() {}
			}
			Object.defineProperty(window, 'BroadcastChannel', { value: Deaf, writable: true });
		});
		await frozen.goto('/');
		await expect(frozen.getByTestId('screen-welcome')).toBeVisible();

		const second = await context.newPage();
		await second.goto('/');
		await expect(second.getByTestId('screen-tab-elsewhere')).toBeVisible();

		await second.getByTestId('take-over').click();

		// The terminal state the spinner never reached. It says the other tab
		// is there and silent — not that it is gone, which is the diagnosis
		// that fits nothing — and it offers the remedy that actually works on
		// a frozen tab.
		const unanswered = second.getByTestId('take-over-unanswered');
		await expect(unanswered).toBeVisible({ timeout: 20_000 });
		await expect(unanswered).toHaveAttribute('role', 'alert');
		await expect(unanswered).toContainText(/not answering|ne répond pas/i);
		await expect(second.getByTestId('take-over-remedy')).toContainText(
			/close it|fermez-le/i
		);

		// And the button is a button again rather than a spinner: waking the
		// other tab and asking again is the way through.
		await expect(second.getByTestId('take-over')).toBeEnabled();
		await expect(second.getByTestId('screen-tab-elsewhere')).toHaveAttribute(
			'data-state',
			'unanswered'
		);

		await context.close();
	});

	test('a credential in the URL is gone even on a load that renders the lock screen', async ({
		browser
	}) => {
		// The consequence that made the ticket worth writing: `/networks/matrix`
		// strips the `loginToken` in `onMount`, and the route never mounted —
		// the layout showed this screen instead. A precaution that only runs
		// when the page it guards is allowed to run is not a precaution, so it
		// now runs before the election.
		const context = await browser.newContext({ viewport: { width: 390, height: 844 } });

		const holder = await context.newPage();
		await holder.goto('/');
		await expect(holder.getByTestId('screen-welcome')).toBeVisible();

		const returning = await context.newPage();
		await returning.goto('/networks/matrix?loginToken=a-real-credential');
		await expect(returning.getByTestId('screen-tab-elsewhere')).toBeVisible();

		expect(returning.url()).not.toContain('loginToken');
		expect(returning.url()).not.toContain('a-real-credential');

		await context.close();
	});

	test('a tab that has been away and come back gets the app', async ({ browser }) => {
		// The SSO round trip is a full page load away and a full page load
		// back, and the first explanation of this ticket was that the
		// returning instance loses the election to the leaving one. It does
		// not — measured — and this is what keeps that true.
		const context = await browser.newContext({ viewport: { width: 390, height: 844 } });
		const page = await context.newPage();

		await page.goto('/');
		await expect(page.getByTestId('screen-welcome')).toBeVisible();

		// Away to somebody else's page, as the homeserver's SSO screen is…
		await page.goto('about:blank');
		// …and back, the way the homeserver sends the browser back.
		await page.goto('/networks/matrix?loginToken=another-credential');

		await expect(page.getByTestId('screen-matrix')).toBeVisible();
		await expect(page.getByTestId('screen-tab-elsewhere')).toHaveCount(0);
		expect(page.url()).not.toContain('loginToken');

		await context.close();
	});
});
