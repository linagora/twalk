// A login of several questions, drawn by the field type — the thing no
// component did before ADR 0030, and the reason this file exists.
//
// # Why the journey starts at a QR code
//
// Because that is where the defect was. `QrLogin.svelte` branched on six of the
// view's kinds with no `{:else}`, so a step asking a question drew an empty
// seventeen-rem box that the session polled once a second for ever — and a
// Telegram QR login by an account with two-factor authentication reaches exactly
// that, because the network interjects a `password` step mid-scan. So the first
// journey below scans, is asked for a password, is asked for a code, and
// finishes: one login, three steps, two of them questions.
//
// # Why this could not be written before
//
// Neither stub could serve two `user_input` steps in a row: every submit that
// was not blocking or cookies completed the login. `queue-next-step` and
// `release-step` are the controls that made a sequence expressible at all, and
// they went in first — this project has already paid three bugs in one ticket
// for trusting a hand-written shape (#106).
//
// # What is asserted, beyond "it draws"
//
// That what was typed reached the **bridge**, unchanged (the stub's own record
// of the bodies the Gateway relayed), that it is in no browser store and not on
// screen afterwards, and that the two hazards behave: a field type this build
// cannot draw is refused **and named** rather than drawn as text, and a refusal
// is its own outcome that never blames the user's own server.

import { expect, test } from '@playwright/test';

import {
	bridgeStack,
	clearLogin,
	NO_STACK,
	signIn,
	StubBridge,
	WHATSAPP_BRIDGE
} from './harness';

test.skip(bridgeStack() === null, NO_STACK);

test.describe.configure({ mode: 'serial' });

/** The two-factor password step a network interjects into a QR login. */
const PASSWORD_STEP = {
	step_id: 'fi.mau.stub.login.password',
	instructions: 'Enter your two-step verification password',
	fields: [
		{
			type: 'password',
			id: 'password',
			name: 'Two-step verification password',
			description: 'The password you set on the account itself'
		}
	]
};

/** The code that follows it, which only exists because the password was right. */
const CODE_STEP = {
	step_id: 'fi.mau.stub.login.code',
	instructions: 'Enter the code we sent you',
	fields: [{ type: '2fa_code', id: 'code', name: 'Code' }]
};

test.beforeEach(async ({ context, request }) => {
	const token = await signIn(context, request, 'the two-step device');
	await clearLogin(request, token, WHATSAPP_BRIDGE);
	await new StubBridge(request, WHATSAPP_BRIDGE).reset();
});

test('a login of three steps: a code scanned, a password, a one-time code', async ({
	page,
	request
}) => {
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	await page.goto('/networks/whatsapp');
	await page.getByTestId('accept-disclosure').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();

	// The network interjects a question mid-scan. Before this work, this is
	// where the screen went blank and stayed blank.
	await bridge.releaseStep(PASSWORD_STEP);

	const step = page.getByTestId('login-step');
	await expect(step).toBeVisible();
	await expect(step).toHaveAttribute('data-step-id', PASSWORD_STEP.step_id);
	// The step is **named, never counted**: a bridge does not know how many
	// steps remain until it knows the account, so no "step 2 of 3" is possible.
	await expect(page.getByTestId('screen-whatsapp')).not.toContainText(/of 3|sur 3/);
	// The bridge's own words for its own step.
	await expect(step).toContainText('two-step verification');

	// The field type decided the control: a password is never drawn as text.
	const password = page.getByTestId('field-password');
	await expect(password).toHaveAttribute('type', 'password');
	await expect(password).toHaveAttribute('data-field-type', 'password');

	// Nothing is sent while the control is empty.
	await expect(page.getByTestId('submit-step')).toBeDisabled();

	await bridge.queueNextStep(CODE_STEP);
	await password.fill('the-account-password');
	await page.getByTestId('submit-step').click();

	// The second question, on the same login.
	await expect(step).toHaveAttribute('data-step-id', CODE_STEP.step_id);
	const code = page.getByTestId('field-code');
	// A phone can offer the code out of the message that just arrived.
	await expect(code).toHaveAttribute('autocomplete', 'one-time-code');
	// And nothing typed for the previous question survived it.
	await expect(code).toHaveValue('');
	await expect(page.getByTestId('field-password')).toHaveCount(0);

	await code.fill('123456');
	await page.getByTestId('submit-step').click();

	await expect(page.getByTestId('login-complete')).toBeVisible();

	// Both answers reached the bridge, under the ids its own fields named, and
	// unchanged — the proof they passed *through* rather than being kept.
	const stats = await bridge.stats();
	const answered = stats.submits.filter((submit) => submit.step_type === 'user_input');
	expect(answered.map((submit) => submit.step_id)).toEqual([
		PASSWORD_STEP.step_id,
		CODE_STEP.step_id
	]);
	expect(answered[0]?.body).toMatchObject({ password: 'the-account-password' });
	expect(answered[1]?.body).toMatchObject({ code: '123456' });
	// One login throughout: a question is a step, not a new process.
	expect(stats.starts).toHaveLength(1);

	// And neither answer is in this browser, nor left on screen.
	const stored = await page.evaluate(() =>
		JSON.stringify({ ...localStorage, ...sessionStorage })
	);
	expect(stored).not.toContain('the-account-password');
	expect(stored).not.toContain('123456');
	await expect(page.getByTestId('login-step')).toHaveCount(0);
});

test('a field type this version cannot draw is refused and named', async ({ page, request }) => {
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	await page.goto('/networks/whatsapp');
	await page.getByTestId('accept-disclosure').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();

	// Two hazards in one step: a type this build does not know, and a field
	// whose type the bridge did not declare at all. Drawing either as a text
	// input is how a password ends up on screen in plain text.
	await bridge.releaseStep({
		step_id: 'fi.mau.stub.login.exotic',
		instructions: 'Present your passkey',
		fields: [
			{ type: 'fi.mau.future.passkey', id: 'passkey', name: 'Passkey' },
			{ id: 'mystery', name: 'Mystery value' }
		]
	});

	const refused = page.getByTestId('field-refused');
	await expect(refused).toBeVisible();
	// Named, both of them, and the unknown type quoted so a report can say it.
	await expect(refused).toContainText('Passkey');
	await expect(refused).toContainText('fi.mau.future.passkey');
	await expect(refused).toContainText('Mystery value');
	// No control, so nothing can be typed into it and nothing can be sent.
	await expect(page.getByTestId('field-passkey')).toHaveCount(0);
	await expect(page.getByTestId('field-mystery')).toHaveCount(0);
	await expect(page.getByTestId('submit-step')).toHaveCount(0);
	// The way out is on screen: this login is going nowhere.
	await expect(page.getByTestId('cancel-login')).toBeVisible();

	// Nothing was submitted.
	expect((await bridge.stats()).submits.some((s) => s.step_type === 'user_input')).toBe(false);
});

test('a jar of request headers is one paste, and is not called a jar of cookies', async ({
	page,
	request
}) => {
	// The shape a first-party bridgev2 connector really asks for (LinkedIn): three
	// fields of type `request_header` — a whole `Cookie` header and two `X-LI-*`
	// values — carried, as bridgev2 carries them, in each field's `sources` rather
	// than on the field itself.
	//
	// It is here because the rule as first written would have refused this login:
	// the grouped-type set came from the types this repository had met, and this
	// one was not among them. The set is bridgev2's own enumeration now, and the
	// one thing the browser must get right is that the answer is keyed by the
	// bridge's **id** while the paste is keyed by the browser's **name**.
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	await page.goto('/networks/whatsapp');
	await page.getByTestId('accept-disclosure').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();

	await bridge.releaseStep({
		step_id: 'fi.mau.stub.login.headers',
		type: 'cookies',
		instructions: 'Sign in, then bring back these request headers',
		url: 'https://www.example.invalid/login',
		fields: [
			{ id: 'cookie', required: true, sources: [{ type: 'request_header', name: 'Cookie' }] },
			{ id: 'csrf', required: true, sources: [{ type: 'request_header', name: 'Csrf-Token' }] },
			{ id: 'track', required: false, sources: [{ type: 'request_header', name: 'X-Li-Track' }] }
		]
	});

	const step = page.getByTestId('login-step');
	await expect(step).toBeVisible();
	// One control for the group, named as the browser names each value.
	const names = page.getByTestId('cookie-names');
	await expect(names).toContainText('Cookie');
	await expect(names).toContainText('Csrf-Token');
	// And not called cookies, because two of these are not.
	await expect(step).not.toContainText(/cookies your bridge asked for|cookies demandés/);
	await expect(step).toContainText(/values your bridge asked for|valeurs demandées/);
	// A credential handover this project has no words for says so, rather than
	// handing one over explained only by its bridge.
	await expect(page.getByTestId('step-copy-missing')).toBeVisible();

	await page
		.getByTestId('cookie-paste')
		.fill('{"Cookie": "li_at=a-session; lidc=b", "Csrf-Token": "ajax:42"}');
	await page.getByTestId('submit-step').click();

	// The stub answers a cookies step the way mautrix-gmessages does: with the
	// emoji pairing step. Drawn here on a **QR** screen, which is the migration
	// working — before ADR 0030 this panel lived inside the SMS screen and an
	// emoji step anywhere else drew an empty seventeen-rem box.
	await expect(page.getByTestId('sms-emoji')).toBeVisible();
	await bridge.releaseCompletion('headers-login');
	await expect(page.getByTestId('login-complete')).toBeVisible();

	// The answer went out as one map, keyed by the bridge's own field ids — not
	// by the header names the user pasted — and the optional value it did not
	// have is simply absent rather than empty.
	const stats = await bridge.stats();
	const relayed = stats.submits.find((submit) => submit.step_type === 'cookies');
	expect(relayed?.body?.cookies).toEqual({
		cookie: 'li_at=a-session; lidc=b',
		csrf: 'ajax:42'
	});
});

test('a refused answer is its own outcome, and never blames the user’s server', async ({
	page,
	request
}) => {
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	await page.goto('/networks/whatsapp');
	await page.getByTestId('accept-disclosure').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();

	await bridge.releaseStep(PASSWORD_STEP);
	await expect(page.getByTestId('login-step')).toBeVisible();

	// The network declines the value — and the bridge drops the login process
	// with it, which is why there is nothing to correct.
	await bridge.refuseNextSubmit('FI.MAU.STUB.PASSWORD_WRONG');
	await page.getByTestId('field-password').fill('not-the-password');
	await page.getByTestId('submit-step').click();

	const refusal = page.getByTestId('login-refused');
	await expect(refusal).toBeVisible();
	// The Gateway's own sentence, which names the network's code and says the
	// login must be started again. Before this work it was never shown.
	await expect(refusal).toContainText('FI.MAU.STUB.PASSWORD_WRONG');
	await expect(refusal).toContainText(/start the login again/);
	// And the sentence that used to be shown instead — a false accusation
	// against the user's own deployment (#175's first defect).
	await expect(page.getByTestId('screen-whatsapp')).not.toContainText(
		/went wrong on your Twalk server|mal passé sur votre serveur Twalk/
	);
	// The step is gone rather than left inviting a resubmit that would 404.
	await expect(page.getByTestId('login-step')).toHaveCount(0);
	await expect(page.getByTestId('field-password')).toHaveCount(0);

	// The only way on is a fresh login, and it is on screen.
	await page.getByTestId('restart').click();
	await expect(page.getByTestId('qr-code')).toBeVisible();
	expect((await bridge.stats()).starts.length).toBeGreaterThanOrEqual(2);
});
