// Screen 4 — activating the `assistant`, and the event that activation *is*.
//
// The assertion this file exists for is the last one: the decision reaches the
// bus. Activation is a `consent.state.changed.v1` event and nothing else (ADR
// 0013) — no control API, no persona registry — so a journey that stopped at
// the green card on screen 4 would be asserting the one half that cannot be
// wrong. Hermes learns that a persona is active by reading that event off
// NATS, and nowhere else.
//
// It also asserts the two absences the design review decided (#74): no
// auto-send toggle, and no active-hours control. An absence is worth a test
// here because it is the kind of thing a later ticket "adds back" without
// noticing what it undoes.

import { expect, test } from '@playwright/test';

import {
	bridgeStack,
	clearLogin,
	connectWhatsApp,
	consentState,
	NO_STACK,
	signIn,
	StubBridge,
	WHATSAPP_BRIDGE
} from './harness';
import { aboutPersona, CONSENT_SUBJECT, watchBus } from './bus';

const stack = bridgeStack();

test.skip(stack === null, NO_STACK);

test.describe.configure({ mode: 'serial' });

let deviceToken = '';

test.beforeEach(async ({ context, request }) => {
	deviceToken = await signIn(context, request, 'the c69 device');
	await clearLogin(request, deviceToken, WHATSAPP_BRIDGE);
	await new StubBridge(request, WHATSAPP_BRIDGE).reset();
});

test('the assistant is activated on exactly the networks that are connected, and the decision reaches the bus', async ({
	page,
	request
}) => {
	await connectWhatsApp(page, request);

	// Subscribed *before* the click: a subscription opened afterwards would
	// have to hope the event was still in flight.
	const bus = await watchBus(stack!.natsPort, CONSENT_SUBJECT);

	try {
		await page.goto('/personas');
		await expect(page.getByTestId('persona-card')).toBeVisible();

		// The wireframe's three toggles, as the design review left them: two,
		// both on, both locked, and no third.
		await expect(page.getByTestId('ability-read')).toHaveAttribute('data-on', 'yes');
		await expect(page.getByTestId('ability-read')).toHaveAttribute('data-locked', 'yes');
		await expect(page.getByTestId('ability-suggest')).toHaveAttribute('data-on', 'yes');
		await expect(page.getByTestId('ability-suggest')).toHaveAttribute('data-locked', 'yes');

		// The two removals, asserted as *controls* rather than as words: the
		// screen names both and says why they are gone, which is the point, so
		// searching the copy would fail on the explanation. What must not exist
		// is a third ability row, and any control that sets a time window.
		await expect(page.getByTestId('persona-card').getByRole('checkbox')).toHaveCount(2);
		await expect(page.locator('input[type="time"], input[type="range"], select')).toHaveCount(0);
		await expect(page.getByTestId('removed-settings')).toContainText(
			/Auto-send|Envoi automatique/i
		);
		await expect(page.getByTestId('removed-settings')).toContainText(
			/Active hours|Heures d’activité/i
		);

		// The consent default: `pending` for everyone, with no address book.
		await expect(page.getByTestId('consent-default')).toContainText(/pending|en attente/i);
		await expect(page.getByTestId('consent-default')).toContainText(
			/no address book|aucun carnet d’adresses/i
		);

		// The perimeter: WhatsApp is connected and ticked; Matrix is offered
		// and is not, because nothing on the Gateway records that the Sensor
		// was ever invited into a room.
		await expect(page.getByTestId('scope-whatsapp').getByRole('checkbox')).toBeChecked();
		await expect(page.getByTestId('scope-matrix').getByRole('checkbox')).not.toBeChecked();

		// And the screen says what is true of *this* deployment about the
		// runtime, read from it rather than assumed (#177, #189): this stack
		// runs a bus and no runtime, so no persona consumer has ever been live
		// on it — `never`, or `gone` if another run left a consumer behind.
		// Either is "nothing runs on your decision", and neither is a claim
		// the screen made up.
		await expect(page.getByTestId('runtime-state')).toHaveAttribute(
			'data-presence',
			/^(never|gone)$/
		);

		await page.getByTestId('activate').click();
		await expect(page.getByTestId('activated')).toBeVisible();
		await expect(page.getByTestId('activated-networks')).toHaveAttribute(
			'data-networks',
			'whatsapp'
		);

		// The event. This is the assertion the ticket asks for.
		const message = await bus.waitFor(aboutPersona('assistant'));
		expect(message.event.type).toBe('fr.linagora.twalk.consent.state.changed.v1');
		expect(message.event.data.new_state).toBe('granted');
		// Scoped to what was connected, and to nothing else: `signal` and
		// `sms` are configured on this deployment and were never logged in,
		// and `matrix` was offered and left unticked.
		expect(message.event.data.scope.networks).toEqual(['whatsapp']);
		expect(message.event.data.actor).toBe(stack!.ownerId);
	} finally {
		bus.close();
	}
});

test('pausing the assistant records a revocation, and says it is starved rather than stopped', async ({
	page,
	request
}) => {
	await page.goto('/dashboard');
	const row = page.getByTestId('persona-assistant');
	await expect(row).toHaveAttribute('data-active', 'yes');
	await expect(page.getByTestId('persona-state-assistant')).toHaveText(/Active|Actif/);

	const bus = await watchBus(stack!.natsPort, CONSENT_SUBJECT);
	try {
		await page.getByTestId('toggle-assistant').click();

		await expect(row).toHaveAttribute('data-active', 'no');
		await expect(page.getByTestId('persona-state-assistant')).toHaveText(/Paused|En pause/);

		// The copy the ticket insists on: a paused persona receives nothing
		// and keeps running. A user who reads "paused" as "gone" will be
		// surprised later.
		await expect(page.getByTestId('pause-meaning')).toContainText(
			/keeps running|continue de tourner/i
		);

		const message = await bus.waitFor(
			(candidate) =>
				aboutPersona('assistant')(candidate) && candidate.event.data.new_state === 'revoked'
		);
		expect(message.event.data.old_state).toBe('granted');
		expect(message.event.data.scope.networks).toEqual(['whatsapp']);
	} finally {
		bus.close();
	}

	// And the Gateway's own record agrees, which is what a cold Hermes would
	// eventually replay.
	const entries = await consentState(request, deviceToken);
	const persona = entries.filter((entry) => entry.subject.type === 'persona');
	expect(persona).toContainEqual(
		expect.objectContaining({
			subject: { type: 'persona', id: 'assistant' },
			network: 'whatsapp',
			state: 'revoked'
		})
	);
});

test('a live link with no login process behind it is still a network to activate on', async ({
	page,
	request
}) => {
	// Found live (#142): WhatsApp and Signal both connected and working, and
	// the persona screen said "no network is connected yet" — so there was
	// nothing to tick and no persona could be activated on anything. The
	// feature was unreachable on a working deployment.
	//
	// This is that deployment: the bridge holds a live session, and the
	// Gateway holds no login process at all — because the login that made it
	// finished, or because the Gateway has restarted since. A login process
	// lives in memory for at most thirty minutes; a link lives in the bridge.
	const bridge = new StubBridge(request, WHATSAPP_BRIDGE);
	await bridge.addExistingLogin('a-live-session', '+33660469852', 'CONNECTED');
	await clearLogin(request, deviceToken, WHATSAPP_BRIDGE);

	await page.goto('/personas');
	await expect(page.getByTestId('persona-card')).toBeVisible();

	// The network is offered, ticked, from the bridge's own answer.
	await expect(page.getByTestId('scope-whatsapp').getByRole('checkbox')).toBeChecked();
	// And the sentence that was the symptom is not on the screen, because it
	// is not true here. It stays for the case where it is.
	await expect(page.getByTestId('no-network')).toHaveCount(0);

	// Reachable: a perimeter with something in it is what enables the button.
	// The activation itself, and the event it is, is the first test in this
	// file — this one must not move the consent journal it asserts on.
	await expect(page.getByTestId('activate')).toBeEnabled();

	await bridge.reset();
});

test('a running runtime is said as such, and the empty approval queue says why it is empty', async ({
	page
}) => {
	// The other direction (#177): a screen that is right only on the machine it
	// was written on is what produced the defect, so the runtime's presence is
	// staged here rather than waited for — the Gateway's own three-state read
	// against a real consumer is `companion-gateway/tests/runtime.rs`'s. The
	// answer staged is the Gateway's exact shape for a runtime hosting one
	// active persona.
	const present = {
		presence: 'present',
		personas: [
			{
				persona_id: 'assistant',
				consumer: 'persona-assistant',
				liveness: 'live',
				activation: 'active',
				waiting_pulls: 1,
				ack_pending: 0
			}
		]
	};
	await page.route('**/api/runtime', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify(present)
		})
	);

	await page.goto('/dashboard');
	const state = page.getByTestId('runtime-state');
	await expect(state).toHaveAttribute('data-presence', 'present');
	// Neither screen may claim that nothing is deployed while one is.
	await expect(page.getByTestId('screen-dashboard')).not.toContainText(
		/No agent runtime|Aucun moteur d’agents/
	);

	await page.goto('/personas');
	await expect(page.getByTestId('runtime-state')).toHaveAttribute('data-presence', 'present');
	await expect(page.getByTestId('runtime-state')).not.toContainText(
		/not deployed|n’est pas encore déployé/
	);

	// The approval queue is empty — arranged rather than assumed: the
	// listing is a projection of the shared bus, and a suggestion another run
	// published stays listed for as long as the stream keeps it, so a test
	// that waited for the real list to be empty was green on a fresh stack and
	// red on every stack after it (#148, the same shape as #211). What is
	// under test is the sentence, and the sentence needs an empty list.
	await page.route('**/api/suggestions', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({
				suggestions: [],
				window: { from_sequence: 1, to_sequence: 1, sequences: 1, reached_start_of_stream: true },
				truncated: false,
				unreadable: 0
			})
		})
	);
	// The previous journeys left the assistant *paused*: with a runtime here,
	// that is "nothing activated" — one of three sentences, and not the one
	// that says no runtime exists.
	await page.goto('/approvals');
	const empty = page.getByTestId('approvals-empty');
	await expect(empty).toBeVisible();
	await expect(empty).toHaveAttribute('data-why', 'notActivated');
	await expect(empty).not.toContainText(/No agent runtime|Aucun moteur d’agents/);
});

test('without a runtime, the empty approval queue says that and not "nothing activated"', async ({
	page
}) => {
	// The real runtime read, on a stack that runs no runtime; the list arranged
	// empty for the reason given above.
	await page.route('**/api/suggestions', (route) =>
		route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({
				suggestions: [],
				window: { from_sequence: 1, to_sequence: 1, sequences: 1, reached_start_of_stream: true },
				truncated: false,
				unreadable: 0
			})
		})
	);
	await page.goto('/approvals');
	const empty = page.getByTestId('approvals-empty');
	await expect(empty).toBeVisible();
	await expect(empty).toHaveAttribute('data-why', 'noRuntime');
});

