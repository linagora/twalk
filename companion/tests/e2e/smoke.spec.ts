// The smoke journey ticket #66 asks for: the app loads, shows screen 1, and
// reports its version — against the built export, served the way the Gateway
// serves it.

import { expect, test } from '@playwright/test';

test.describe('the app loads', () => {
	test('shows screen 1 with the wireframe’s states', async ({ page }) => {
		await page.goto('/');

		// The shell rendered and the capability probe answered, so the gate is
		// not in the way.
		await expect(page.getByTestId('screen-welcome')).toBeVisible();
		await expect(page.getByTestId('capability-gate')).toHaveCount(0);

		// Screen 1: title, subtitle, the description, the domain field.
		await expect(page.getByRole('heading', { level: 1 })).toHaveText('Twalk Companion');
		await expect(page.getByText(/WhatsApp, Telegram, Signal, Discord/)).toBeVisible();

		const input = page.getByLabel('Your Twalk domain');
		await expect(input).toBeVisible();
		const cta = page.getByTestId('continue');

		// *Empty*: the CTA is disabled, and there is no error yet.
		await expect(cta).toBeDisabled();

		// *Invalid input*: micro-copy under the field, in an alert region.
		await input.fill('not a domain');
		await input.blur();
		await expect(page.getByRole('alert')).toContainText(/example\.com|exemple\.fr/);
		await expect(cta).toBeDisabled();

		// *Valid input*: the CTA is enabled.
		await input.fill('example.com');
		await expect(cta).toBeEnabled();
	});

	test('carries the domain into the next screen', async ({ page }) => {
		await page.goto('/');
		await page.getByRole('textbox').fill('https://Twalk.Example.COM/');
		await page.getByTestId('continue').click();

		await expect(page.getByTestId('screen-onboarding')).toBeVisible();
		// Normalised on the way: scheme, case and trailing slash removed.
		await expect(page.getByTestId('chosen-domain')).toContainText('twalk.example.com');
		await expect(page).toHaveURL(/\/onboarding$/);
	});

	test('reports its version and the Gateway’s', async ({ page }) => {
		await page.goto('/diagnostics');
		await expect(page.getByTestId('screen-diagnostics')).toBeVisible();

		// The version baked into this build by the client generator, and what
		// /health reports. The test server reports 0.1.0, the version in
		// companion-gateway/openapi.yaml.
		await expect(page.getByTestId('expected-gateway-version')).toHaveText('0.1.0');
		await expect(page.getByTestId('gateway-version')).toHaveText('0.1.0');
		await expect(page.getByTestId('handshake-kind')).toHaveAttribute('data-kind', 'match');
		await expect(page.getByTestId('app-build')).not.toBeEmpty();
	});

	test('speaks the browser’s language, French included', async ({ browser }) => {
		// The default locale comes from navigator.language (the wireframes'
		// localization section). French is the owner's.
		const french = await browser.newContext({ locale: 'fr-FR' });
		const page = await french.newPage();
		await page.goto('/');
		await expect(page.getByTestId('app')).toHaveAttribute('data-locale', 'fr');
		await expect(page.getByText('Votre domaine Twalk')).toBeVisible();
		await french.close();

		const english = await browser.newContext({ locale: 'en-GB' });
		const other = await english.newPage();
		await other.goto('/');
		await expect(other.getByTestId('app')).toHaveAttribute('data-locale', 'en');
		await expect(other.getByText('Your Twalk domain')).toBeVisible();
		await english.close();
	});

	test('sends nothing to a third party', async ({ page, baseURL }) => {
		// No telemetry and no third-party error collector (spec #65). The
		// assertion is the strongest one available: every request the app
		// makes is to the origin it was served from. A web font from a CDN, an
		// analytics beacon or an error collector would all show up here.
		const own = new URL(baseURL ?? 'http://127.0.0.1').origin;
		const foreign: string[] = [];
		page.on('request', (request) => {
			const url = new URL(request.url());
			if (url.origin !== own && url.protocol !== 'data:' && url.protocol !== 'blob:') {
				foreign.push(request.url());
			}
		});

		await page.goto('/');
		await expect(page.getByTestId('screen-welcome')).toBeVisible();
		await page.goto('/diagnostics');
		await expect(page.getByTestId('screen-diagnostics')).toBeVisible();

		expect(foreign).toEqual([]);
	});
});
