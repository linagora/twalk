// The consent journey (#170), against a real stack with a **real Sensor** in it.
//
// The ticket's keystone criterion is one sentence and every word of it is load
// bearing: *a contact writes, appears as awaiting a decision, is granted, and a
// **subsequent** message is labelled `granted` on the bus — asserted at the bus,
// not in the screen's own state.*
//
// So this file is the only Companion journey that starts a Sensor
// (`./sensor.ts` says why). What it proves cannot be proved any other way: that
// the decision this screen writes reaches the process that stamps the label. The
// Sensor here has **no Gateway snapshot configured**, so its consent cache
// starts cold and everything is `pending`; a `granted` label can therefore only
// have come from the decision the browser took, through the Gateway's outbox,
// onto the bus, into the Sensor's cache. Nothing in the Companion's own state is
// asked anything.
//
// The conversation is a native Matrix room (ADR 0009), which is also the one
// shape of this journey no existing suite covers: `sensor/tests/consent.rs`
// proves relabelling on WhatsApp with a decision the test itself published, and
// this proves it on `matrix` with a decision a **user took in a browser**.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test, type APIRequestContext } from '@playwright/test';

import { bridgeStack, NO_STACK, signIn } from '../networks/harness';
import {
	CONSENT_SUBJECT,
	INBOUND_SUBJECT,
	publish,
	watchBus,
	type BusMessage
} from '../dashboard/bus';
import {
	createRoom,
	joinRoom,
	logIn,
	sendMessage,
	waitForMembership,
	type MatrixUser
} from './matrix';
import { startSensor, type RunningSensor } from './sensor';

/** The account `provision-bots.sh` creates that is neither the owner nor the Sensor. */
const CONTACT_LOCALPART = 'bot_beta';

/** The contract's own fixture, so the bus carries a real event and not a shape. */
const INBOUND_FIXTURE = JSON.parse(
	readFileSync(
		join(
			dirname(fileURLToPath(import.meta.url)),
			'..',
			'..',
			'..',
			'..',
			'contracts',
			'cloudevents',
			'v1',
			'fixtures',
			'inbound.message.received.json'
		),
		'utf8'
	)
) as Record<string, unknown>;

/**
 * A second contact, so that "the bulk control acts on what the filter shows"
 * is a statement about this screen rather than about what another spec left in
 * the Gateway's journal.
 *
 * Published rather than sent, because the Sensor is here to prove the *label*
 * and one labelled contact proves it. This one only has to exist.
 */
const SECOND_CONTACT = `@whatsapp_170${Date.now().toString(36)}:test.twalk`;

const stack = bridgeStack();

test.describe.configure({ mode: 'serial' });

test.describe('the consent screen', () => {
	test.skip(stack === null, NO_STACK);
	// A cold `cargo build` of the Sensor is minutes, and matrix-sdk's crypto
	// stack is most of it.
	test.setTimeout(1_800_000);

	let sensor: RunningSensor;
	let owner: MatrixUser;
	let contact: MatrixUser;
	let roomId: string;
	let token: string;

	test.beforeAll(async ({ browser }) => {
		if (stack === null) {
			return;
		}
		// A device token for the API assertions, minted the way the Companion
		// gets one. The browser contexts each test opens sign in for themselves.
		const context = await browser.newContext();
		token = await signIn(context, context.request, 'consent-journey');
		await context.close();

		owner = await logIn(stack.synapseUrl, stack.owner);
		contact = await logIn(stack.synapseUrl, CONTACT_LOCALPART);

		// Nothing is observed by default: the Sensor joins on an invitation from
		// a user `SENSOR_ALLOWED_INVITERS` names, and on nobody else's.
		sensor = await startSensor({
			synapseUrl: stack.synapseUrl,
			serverName: stack.serverName,
			natsPort: stack.natsPort,
			allowedInviters: [stack.ownerId]
		});

		roomId = await createRoom(stack.synapseUrl, owner, `consent-${Date.now()}`, [
			sensor.userId,
			contact.userId
		]);
		await joinRoom(stack.synapseUrl, contact, roomId);
		// Asked of the homeserver rather than of a log line: "the Sensor is in
		// the room" is a fact about room state.
		await waitForMembership(stack.synapseUrl, owner, roomId, sensor.userId, 'join');

		await publish(stack.natsPort, INBOUND_SUBJECT, {
			...INBOUND_FIXTURE,
			// A fresh id: the bus deduplicates on it, and this event must land.
			id: `${Date.now().toString(16)}${'0'.repeat(48)}`.slice(0, 64),
			subject: SECOND_CONTACT,
			time: new Date().toISOString().replace(/\.\d{3}Z$/u, 'Z')
		});
	});

	test.afterAll(async () => {
		// The process is reaped whether this suite passed or not: a Sensor left
		// running holds a durable consumer on the shared bus, and the next run
		// would split the consent stream between two of them.
		await sensor?.stop();
	});

	test('a contact who has written appears as awaiting a decision', async ({ page, request }) => {
		const inbound = await watchBus(stack!.natsPort, INBOUND_SUBJECT);
		try {
			await sendMessage(stack!.synapseUrl, contact, roomId, 'bonjour, on se voit demain ?');
			// The Sensor's own label before any decision exists. `pending` and
			// not `revoked`: nothing has been decided (ADR 0010).
			const first = await inbound.waitFor(about(contact.userId), 60_000);
			expect(labelOf(first)).toBe('pending');
		} finally {
			inbound.close();
		}

		// The Gateway's projection is a durable consumer, so the list catches up
		// rather than being instantaneous.
		await expect
			.poll(async () => (await pendingContacts(request, token)).includes(contact.userId), {
				timeout: 60_000
			})
			.toBe(true);

		await signIn(page.context(), page.context().request, 'consent-screen');
		await page.goto('/consent');
		const row = page.getByTestId(`consent-row-${contact.userId}-matrix`);
		await expect(row).toBeVisible();

		// The three states, and the one that is the default of the whole model.
		await expect(row).toHaveAttribute('data-state', 'pending');
		await expect(row).toHaveAttribute('data-decided-by', 'nothing');
		await expect(row.getByTestId('row-state')).toContainText(/nobody|personne/i);

		// The two things the screen must say and that a user would otherwise
		// discover by being confused.
		await expect(page.getByTestId('not-retroactive')).toBeVisible();
		await expect(page.getByTestId('never-decided-explained')).toBeVisible();
		await expect(page.getByTestId('not-theirs')).toBeVisible();
	});

	test('granting in the browser labels the next message granted on the bus', async ({
		page,
		request
	}) => {
		const decisions = await watchBus(stack!.natsPort, CONSENT_SUBJECT);
		const inbound = await watchBus(stack!.natsPort, INBOUND_SUBJECT);
		try {
			await signIn(page.context(), page.context().request, 'consent-grant');
			await page.goto('/consent');
			const row = page.getByTestId(`consent-row-${contact.userId}-matrix`);
			await expect(row).toBeVisible();
			await row.getByTestId('set-granted').click();

			// One: the decision left the deployment. Not "the screen says so" —
			// the event, on the bus, with the perimeter the screen chose.
			const decision = await decisions.waitFor(
				(message) =>
					message.event.data.subject.type === 'contact' &&
					message.event.data.subject.id === contact.userId &&
					message.event.data.new_state === 'granted',
				60_000
			);
			expect(decision.event.data.scope.networks).toEqual(['matrix']);
			expect(decision.event.data.actor).toBe(stack!.ownerId);
			// Never decided before, and the journal says so in its own word for
			// it: `unset` is the absence of a decision, not a state (ADR 0010).
			expect(decision.event.data.old_state).toBe('unset');

			// And the screen now reports it as in force, from the Gateway's own
			// answer rather than from what it believes it just did.
			await expect(row).toHaveAttribute('data-state', 'granted');
			await expect(row).toHaveAttribute('data-decided-by', 'contact');

			// Two, and this is the criterion: a **subsequent** message carries
			// `granted`. Resent until the label flips, because the Sensor's
			// consent consumer and its sync loop are two tasks and the first
			// message after a decision can still be labelled with the old state
			// — the same race `sensor/tests/consent.rs` resends through.
			const seenBefore = new Set(inbound.seen.map((message) => message.event.id));
			await expect
				.poll(
					async () => {
						await sendMessage(stack!.synapseUrl, contact, roomId, 'et pour 20h ?');
						const later = inbound.seen.filter(
							(message) =>
								about(contact.userId)(message) && !seenBefore.has(message.event.id)
						);
						return later.some((message) => labelOf(message) === 'granted');
					},
					{ timeout: 120_000, intervals: [2_000] }
				)
				.toBe(true);

			// The label is on the envelope, and the envelope is about the contact
			// on the network the decision was scoped to. A `granted` label about
			// somebody else, or on another network, would pass a weaker
			// assertion.
			const granted = inbound.seen.find(
				(message) =>
					about(contact.userId)(message) &&
					!seenBefore.has(message.event.id) &&
					labelOf(message) === 'granted'
			);
			expect(granted).toBeDefined();
			expect(networkOf(granted as BusMessage)).toBe('matrix');
		} finally {
			decisions.close();
			inbound.close();
		}

		// And the Gateway agrees, which is what a cold consumer would read.
		const effective = await request.get(
			`/api/consent/effective?contact=${encodeURIComponent(contact.userId)}&network=matrix`,
			{ headers: { cookie: `twalk_device=${token}` } }
		);
		expect(effective.ok(), await effective.text()).toBeTruthy();
		const body = (await effective.json()) as {
			state: string;
			decided_by: { type: string; id: string } | null;
		};
		expect(body.state).toBe('granted');
		// `decided_by` naming the contact is how "decided" is told apart from
		// "never decided", which is the distinction the whole screen turns on.
		expect(body.decided_by).toEqual({ type: 'contact', id: contact.userId });
	});

	test('returning a contact to undecided is a decision, not an erasure', async ({
		page,
		request
	}) => {
		await signIn(page.context(), page.context().request, 'consent-undecide');
		await page.goto('/consent');
		const row = page.getByTestId(`consent-row-${contact.userId}-matrix`);
		await expect(row).toBeVisible();
		await row.getByTestId('set-pending').click();

		await expect(row).toHaveAttribute('data-state', 'pending');
		// The point: `pending` by decision is *not* never-decided. The journal is
		// append-only and there is nothing to un-record, so the row keeps saying
		// somebody answered — and the Gateway's pending list, which is exactly
		// the never-decided ones, no longer holds this contact.
		await expect(row).toHaveAttribute('data-decided-by', 'contact');
		await expect
			.poll(async () => (await pendingContacts(request, token)).includes(contact.userId), {
				timeout: 60_000
			})
			.toBe(false);
	});

	test('the owner is never somebody to decide about', async ({ page, request }) => {
		// ADR 0018 and ADR 0021: the owner has no consent state, on any event.
		// This Sensor runs with no `SENSOR_OWNER`, so it publishes the owner's
		// own message straight through the inbound door with the owner as its
		// subject — the world before #109, and the sharpest probe available of
		// whether the Gateway keeps them out of the list of people to decide
		// about.
		const inbound = await watchBus(stack!.natsPort, INBOUND_SUBJECT);
		try {
			await sendMessage(stack!.synapseUrl, owner, roomId, "c'est noté");
			// Wait for the event the Gateway's projection had the chance to
			// read, so that the absence asserted below is an absence and not a
			// race.
			await inbound.waitFor(about(stack!.ownerId), 60_000);
		} finally {
			inbound.close();
		}

		// Given a moment, and then asked. A poll that waited for the owner to
		// appear would be the wrong shape: this asserts they never do.
		await new Promise((resolve) => setTimeout(resolve, 3_000));
		expect(await pendingContacts(request, token)).not.toContain(stack!.ownerId);

		await signIn(page.context(), page.context().request, 'consent-owner');
		await page.goto('/consent');
		await expect(page.getByTestId('consent-rows')).toBeVisible();
		// Neither as a row nor as the anomaly card: there is nothing to report.
		await expect(page.getByTestId(`consent-row-${stack!.ownerId}-matrix`)).toHaveCount(0);
		await expect(page.getByTestId('owner-row')).toHaveCount(0);
	});

	test('a refusal names its cause and its remedy, and does not spin', async ({ page }) => {
		await signIn(page.context(), page.context().request, 'consent-refusal');
		// A deployment that records no consent, which is a statement about the
		// deployment rather than about this contact.
		await page.route('**/api/consent/decisions', (route) =>
			route.fulfill({
				status: 503,
				contentType: 'application/json',
				body: JSON.stringify({
					error: 'consent_not_configured',
					detail: 'GATEWAY_NATS_URL is unset'
				})
			})
		);
		await page.goto('/consent');
		const row = page.getByTestId(`consent-row-${contact.userId}-matrix`);
		await expect(row).toBeVisible();
		await row.getByTestId('set-granted').click();

		const refusal = row.getByTestId('row-problem');
		await expect(refusal).toBeVisible();
		await expect(refusal).toHaveAttribute('data-code', 'consent_not_configured');
		// A remedy, always — and no spinner left behind, because `Explained` has
		// no value that means "still working" (#111, #135, #139).
		await expect(refusal).toHaveAttribute('data-remedy', 'none');
		await expect(row.getByTestId('row-remedy')).not.toBeEmpty();
		await expect(row.locator('.spinner')).toHaveCount(0);
	});

	test('the bulk control is scoped to what the filter shows, and counted', async ({
		page,
		request
	}) => {
		// The second contact has to be in the list for "scoped" to mean anything.
		await expect
			.poll(async () => (await pendingContacts(request, token)).includes(SECOND_CONTACT), {
				timeout: 60_000
			})
			.toBe(true);

		await signIn(page.context(), page.context().request, 'consent-bulk');
		await page.goto('/consent');
		await expect(page.getByTestId('consent-rows')).toBeVisible();

		// Narrow to one person, and the control says one. #137's rule: it acts
		// on what is on screen, never on the whole list.
		await page.getByTestId('consent-search').fill(CONTACT_LOCALPART);
		const showing = page.getByTestId('consent-showing');
		await expect(showing).toHaveAttribute('data-shown', '1');
		// The point of the assertion: the list is longer than what is shown, and
		// the control below counts the shown ones.
		expect(Number(await showing.getAttribute('data-total'))).toBeGreaterThan(1);
		const grant = page.getByTestId('bulk-grant');
		await expect(grant).toHaveAttribute('data-count', '1');

		// And it asks twice. One press arms, and the confirmation names the
		// number again.
		await grant.click();
		await expect(page.getByTestId('bulk-confirm')).toHaveAttribute('data-count', '1');
		await page.getByTestId('bulk-cancel').click();
		await expect(page.getByTestId('bulk-confirm')).toHaveCount(0);
	});
});

/** Whether a bus message is an event about this subject. */
function about(subject: string) {
	return (message: BusMessage) => message.event.subject === subject;
}

/**
 * The envelope's `consent` extension.
 *
 * `BusMessage` is typed for the consent events `tests/e2e/dashboard/bus.ts` was
 * written for, and this file reads inbound ones too, so the two extensions it
 * needs are read through one narrowing here rather than by widening a type three
 * other specs depend on.
 */
function labelOf(message: BusMessage): string | undefined {
	return (message.event as unknown as { consent?: string }).consent;
}

function networkOf(message: BusMessage): string | undefined {
	return (message.event as unknown as { network?: string }).network;
}

/** Who the Gateway says is waiting for a first decision. */
async function pendingContacts(request: APIRequestContext, token: string): Promise<string[]> {
	const answer = await request.get('/api/contacts/pending', {
		headers: { cookie: `twalk_device=${token}` }
	});
	expect(answer.ok(), await answer.text()).toBeTruthy();
	const body = (await answer.json()) as { contacts: { contact: string }[] };
	return body.contacts.map((entry) => entry.contact);
}
