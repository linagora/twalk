// Screen 1's probe, in the states that need no homeserver to reach.
//
// The test server answers `/api/session` with `401 unauthenticated`, which is
// what a Gateway with an owner and no session answers — so the Gateway half of
// the probe succeeds here and the homeserver half is the one under test.

import { expect, test } from '@playwright/test';

test.describe('screen 1 checks the deployment', () => {
	test('says which domain it could not reach, and offers a retry', async ({ page }) => {
		await page.goto('/');
		await page.getByLabel('Your Twalk domain').fill('nothing-here.invalid');
		await page.getByTestId('continue').click();

		// The wireframe's *homeserver unreachable* state: a banner naming the
		// domain, the field marked invalid, and the CTA offering another go
		// rather than pretending nothing happened.
		const banner = page.getByTestId('deployment-error');
		await expect(banner).toBeVisible({ timeout: 30_000 });
		await expect(banner).toContainText('nothing-here.invalid');
		await expect(page.getByLabel('Your Twalk domain')).toHaveAttribute('aria-invalid', 'true');
		await expect(page.getByTestId('continue')).toContainText(/try again/i);

		// And it stayed on screen 1: nothing was created anywhere.
		await expect(page).toHaveURL(/\/$/);
	});

	test('shows a spinner and disables the field while it checks', async ({ page }) => {
		// A homeserver that takes its time, so the state the wireframe
		// describes is actually observable.
		await page.route('https://slow.example/**', async (route) => {
			await new Promise((resolve) => setTimeout(resolve, 3000));
			await route.fulfill({ status: 404 });
		});

		await page.goto('/');
		await page.getByLabel('Your Twalk domain').fill('slow.example');
		await page.getByTestId('continue').click();

		// The wireframe's *loading* state: the subtitle changes, the input is
		// disabled, the CTA is a spinner.
		// Twice on purpose: the subtitle says it, and the CTA's spinner carries
		// the same sentence for a screen reader.
		await expect(page.getByText('Checking your Twalk deployment')).toHaveCount(2);
		await expect(page.getByLabel('Your Twalk domain')).toBeDisabled();
	});
});
