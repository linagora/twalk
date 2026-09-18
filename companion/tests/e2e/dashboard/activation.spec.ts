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

		// And the screen does not pretend an activated assistant will do
		// anything today: Hermes is not implemented.
		await expect(page.getByTestId('no-runtime')).toBeVisible();

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
