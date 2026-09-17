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
