// The settings screen (#101), against the real Gateway: the model is named
// here and served back without its credential, a file-locked credential draws
// no field, the probe's answers are told apart, and the language changes this
// interface.
//
// On the bridge origin, whose Gateway has a settings store of its own for the
// run (`tests/real-stack.mjs`), so what a journey sets is what the next one
// reads — and nothing another run set.

import { expect, test } from '@playwright/test';

import { bridgeStack, NO_STACK, signIn } from './harness';

const stack = bridgeStack();

test.skip(stack === null, NO_STACK);

test.describe.configure({ mode: 'serial' });

const SECRET = 'sk-test-secret-that-must-not-come-back-1234';

test.beforeEach(async ({ context, request }) => {
	await signIn(context, request, 'the settings device');
});

test('the dashboard leads here, and a fresh deployment says no model is configured', async ({
	page
}) => {
	await page.goto('/dashboard');
	await page.getByTestId('to-settings').click();
	await expect(page.getByTestId('screen-settings')).toBeVisible();
	// `configured: false` is a state, not an error (#98): the form is empty and
	// says what an unnamed model costs.
	await expect(page.getByTestId('model-unset')).toBeVisible();
	await expect(page.getByTestId('model-credential')).toHaveAttribute('data-source', 'none');
	// The probe has nothing to spend money on yet.
	await expect(page.getByTestId('model-probe')).toBeDisabled();
	// And the tracing card says where it stands rather than drawing a control
	// this deployment cannot store (#99).
	await expect(page.getByTestId('settings-tracing')).toBeVisible();
	await expect(page.getByTestId('settings-tracing').locator('input')).toHaveCount(0);
});

test('a model set here is served back described, and the credential never comes back', async ({
	page
}) => {
	await page.goto('/settings');
	await page.getByTestId('model-base-url').fill('http://127.0.0.1:1/v1');
	await page.getByTestId('model-name').fill('qwen');
	await page.getByTestId('model-credential-input').fill(SECRET);
	await page.getByTestId('model-save').click();

	await expect(page.getByTestId('model-outcome')).toHaveAttribute('data-kind', 'saved');
	// Write-only: the field is empty again, the description says a credential
	// is set and ends how it ends, and the Gateway's own answer carries none
	// of it — asserted on the bytes over the wire, not on the screen.
	await expect(page.getByTestId('model-credential-input')).toHaveValue('');
	await expect(page.getByTestId('model-credential')).toHaveAttribute('data-source', 'companion');
	await expect(page.getByTestId('model-credential-set')).toContainText('1234');
	const served = await page.request.get('/api/settings/model');
	expect(served.ok()).toBe(true);
	const text = await served.text();
	expect(text).not.toContain(SECRET);
	expect(text).toContain('"model":"qwen"');

	// Renaming the model with the credential field left empty keeps the
	// credential: an empty field is "keep what is stored", never "remove".
	await page.getByTestId('model-name').fill('qwen-2');
	await page.getByTestId('model-save').click();
	await expect(page.getByTestId('model-outcome')).toHaveAttribute('data-kind', 'saved');
	await expect(page.getByTestId('model-credential-set')).toContainText('1234');
	expect(await (await page.request.get('/api/settings/model')).text()).toContain('"model":"qwen-2"');
});

test('the probe tells an endpoint that did not answer apart from everything else', async ({
	page
}) => {
	await page.goto('/settings');
	await expect(page.getByTestId('model-probe')).toBeEnabled();
	// Port 1 answers nothing: the one of the four probe answers a test stack
	// can stage without an endpoint of its own. The other three are the
	// Gateway's own `tests/settings.rs`; what is asserted here is that the
	// screen renders the code it was given as its own sentence.
	await page.getByTestId('model-probe').click();
	const outcome = page.getByTestId('model-outcome');
	await expect(outcome).toHaveAttribute('data-kind', 'refused', { timeout: 20_000 });
	await expect(outcome).toHaveAttribute('data-code', 'endpoint_unreachable');
	await expect(outcome).toContainText(/Nothing answered|Rien n’a répondu/);
});

test('a credential the operator supplied as a file is said to be in force, and offers no field', async ({
	page
}) => {
	// #98's precedence, staged: this stack's Gateway runs with no key file, so
	// the answer is the Gateway's exact shape for one that does.
	await page.route('**/api/settings/model', async (route) => {
		if (route.request().method() !== 'GET') {
			await route.continue();
			return;
		}
		const answer = await route.fetch();
		const body = (await answer.json()) as { credential: Record<string, unknown> };
		body.credential = {
			configured: true,
			source: 'file',
			hint: null,
			file: '/etc/twalk/llm.key',
			companion_credential_stored: true
		};
		await route.fulfill({ response: answer, json: body });
	});
	await page.goto('/settings');
	await expect(page.getByTestId('model-credential')).toHaveAttribute('data-source', 'file');
	await expect(page.getByTestId('model-credential-file')).toContainText('/etc/twalk/llm.key');
	await expect(page.getByTestId('model-credential-input')).toHaveCount(0);
});

test('the language is saved, changes this interface, and says what it does not decide', async ({
	page
}) => {
	await page.goto('/settings');
	await expect(page.getByTestId('settings-language')).toContainText(
		/does not decide the language|ne décide pas de la langue/
	);
	await page.getByTestId('language-fr').check();
	await expect(page.getByTestId('language-outcome')).toHaveAttribute('data-kind', 'saved');
	await expect(page.getByTestId('language-choice')).toHaveAttribute('data-language', 'fr');
	// The first effect, at once: this page speaks it.
	await expect(page.getByRole('heading', { level: 1 })).toHaveText('Réglages');
	// And the Gateway holds it for the runtime's read.
	const served = (await (await page.request.get('/api/settings/language')).json()) as {
		language: string | null;
	};
	expect(served.language).toBe('fr');

	// No preference is a state of its own, and not English — and it is what
	// this deployment started with, so the journeys that follow find it so.
	await page.getByTestId('language-none').check();
	await expect(page.getByTestId('language-choice')).toHaveAttribute('data-language', 'none');
	expect(
		((await (await page.request.get('/api/settings/language')).json()) as { language: string | null })
			.language
	).toBeNull();
});

test('forgetting the model is a named act, and takes the credential with it', async ({
	page
}) => {
	await page.goto('/settings');
	await page.getByTestId('model-forget').click();
	await expect(page.getByTestId('model-outcome')).toHaveAttribute('data-kind', 'forgotten');
	await expect(page.getByTestId('model-unset')).toBeVisible();
	await expect(page.getByTestId('model-credential')).toHaveAttribute('data-source', 'none');
	const served = (await (await page.request.get('/api/settings/model')).json()) as {
		configured: boolean;
		credential: { configured: boolean };
	};
	expect(served.configured).toBe(false);
	expect(served.credential.configured).toBe(false);
});

test('the disclosure is on until switched off on the record, and the record says who and when', async ({
	page
}) => {
	const it = stack!;
	await page.goto('/settings');
	const card = page.getByTestId('settings-disclosure');
	await expect(card).toBeVisible();

	// What the contact reads, as an example in this interface's language —
	// the contract's own sentence, not a paraphrase (#121, ADR 0031).
	await expect(card.getByTestId('disclosure-example')).toHaveText(
		/^(Drafted with my AI assistant\.|Rédigé avec mon assistant IA\.)$/
	);

	// The switch is global to this Gateway, and this Gateway is the one every
	// `approvals` journey runs against afterwards: whatever fails between the
	// two presses, the `finally` puts it back on, or one red here would be
	// ten reds there with the cause hidden.
	try {
		// On by default, and the record says nobody ever decided rather than
		// dating a decision nobody took (ADR 0019).
		const toggle = card.getByTestId('disclosure-switch');
		await expect(toggle).toHaveRole('switch');
		await expect(toggle).toHaveAttribute('aria-checked', 'true');
		await expect(card.getByTestId('disclosure-record')).toHaveAttribute('data-enabled', 'yes');
		await expect(card.getByTestId('disclosure-record')).not.toContainText(it.ownerId);

		// Off: one press, one journal row — dated, attributed to the owner, with
		// the note kept and read back.
		await card.getByTestId('disclosure-reason').fill('a test of the record');
		await toggle.click();
		await expect(toggle).toHaveAttribute('aria-checked', 'false');
		await expect(card.getByTestId('disclosure-outcome')).toHaveAttribute('data-kind', 'saved');
		const record = card.getByTestId('disclosure-record');
		await expect(record).toHaveAttribute('data-enabled', 'no');
		await expect(record).toContainText(/Off since|Désactivée depuis/);
		await expect(record).toContainText(it.ownerId);
		await expect(card.getByTestId('disclosure-reason-given')).toContainText('a test of the record');
		// The note field is for the next decision, not a display of the last.
		await expect(card.getByTestId('disclosure-reason')).toHaveValue('');
		const off = (await (await page.request.get('/api/settings/disclosure')).json()) as {
			enabled: boolean;
			since: string | null;
			actor: string | null;
			reason: string | null;
		};
		expect(off.enabled).toBe(false);
		expect(off.actor).toBe(it.ownerId);
		expect(off.reason).toBe('a test of the record');
		expect(off.since).not.toBeNull();
		// The date on screen is the journal's instant, in this interface's
		// locale: the year is the one thing every locale spells the same.
		await expect(record).toContainText(String(new Date(off.since!).getUTCFullYear()));

		// Back on — a second row, not an erased first one: the record now says
		// since when it is on again, which is a different sentence from "never
		// turned off". And it stays on for every journey after this one, which
		// share this Gateway.
		await toggle.click();
		await expect(toggle).toHaveAttribute('aria-checked', 'true');
		await expect(record).toHaveAttribute('data-enabled', 'yes');
		await expect(record).toContainText(/On since|Activée depuis/);
		await expect(record).toContainText(it.ownerId);
		const on = (await (await page.request.get('/api/settings/disclosure')).json()) as {
			enabled: boolean;
			since: string | null;
		};
		expect(on.enabled).toBe(true);
		expect(on.since).not.toBe(off.since);
	} finally {
		const restored = await page.request.put('/api/settings/disclosure', {
			data: { enabled: true }
		});
		expect(restored.ok(), await restored.text()).toBeTruthy();
	}
});
