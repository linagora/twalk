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
//
// # A contact is never a Matrix ID on its own here
//
// Consent is keyed on `(subject, network)` — the perimeter is half of the fact —
// and this file used to ask the Gateway and the bus about a contact by Matrix ID
// alone. That is #200, and it cost two days of believing the consent screen was
// broken. The same account writes on more than one network in this project's own
// test stack: `@bot_beta:test.twalk` is the contact of this journey on `matrix`
// and a WhatsApp ghost's stand-in in half of `sensor/tests/`, on one shared
// JetStream that every suite publishes to and that the Gateway's pending-contact
// projection replays from the beginning on each run. So:
//
//   - the pending list is read **per network** ([`waitingNetworks`], which asks
//     the Gateway the same question `companion-gateway/tests/pending.rs` asks
//     it), because "is this contact waiting?" has no answer and "is this contact
//     waiting on matrix?" has one. Read the other way, *"returning a contact to
//     undecided is a decision"* failed on both attempts against a screen and a
//     Gateway that were both doing exactly the right thing, and eight specs
//     behind it never ran;
//   - a bus event is matched on its subject **and** its network
//     ([`about`]), because another suite's event about the same account, carrying
//     the contract fixture's own `consent: granted`, would otherwise satisfy an
//     assertion that this journey's message is labelled `pending` — which is the
//     intermittent failure of the first spec, the same cause wearing the other
//     face;
//   - and this journey **arranges** the second perimeter rather than inheriting
//     it: `beforeAll` publishes a WhatsApp sighting of the same contact, so the
//     distinction is a fixture of this file and passing no longer depends on
//     whether the Sensor's suite has ever run against this stack. It is also the
//     stronger assertion: the `matrix` decision answers for `matrix` and leaves
//     `whatsapp` waiting, which is what a perimeter *is*.

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

/**
 * The network this journey decides about, and the network it deliberately does
 * not.
 *
 * The contact writes to the user in a native Matrix room, so every decision this
 * file takes is scoped to `matrix`. `OTHER_NETWORK` is the second perimeter the
 * same person is seen on — arranged in `beforeAll` — and the whole point of it is
 * that no decision here ever covers it.
 */
const NETWORK = 'matrix';
const OTHER_NETWORK = 'whatsapp';

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
		// The describe-level `setTimeout` above applies to the **tests**, not to
		// this hook, which had the default thirty seconds — and this hook builds
		// the Sensor if it is cold, logs it into a real homeserver, waits for it
		// to accept an invitation and publishes to the bus. Thirty seconds is
		// enough on a warm machine and nowhere near enough otherwise, and when it
		// runs out Playwright reports the first spec as failed and the rest as
		// never run, which is the same misleading shape #200 describes. So the
		// hook says its own budget, here, where it is the hook's.
		test.setTimeout(1_800_000);
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

		// The **same contact, on another network** (#200). The fixture is already
		// a WhatsApp event, so only the subject moves.
		//
		// This is the perimeter made into a fixture. Consent is `(subject,
		// network)` and a decision taken here covers `matrix` alone, so this
		// person is two rows on the screen and two entries in the Gateway's
		// pending list — and every assertion below about "waiting for a decision"
		// has to name which of the two it means. The reason to publish it rather
		// than to let the stack supply it: on this project's shared test stack
		// `@bot_beta:test.twalk` *is* a WhatsApp sender in half of
		// `sensor/tests/`, on the one JetStream the Gateway's projection replays
		// from the beginning — so a run after the Sensor's suite saw this and a
		// run on a stack created that morning did not, and the two runs disagreed
		// about whether this file passes. Arranged here, they agree.
		await publish(stack.natsPort, INBOUND_SUBJECT, {
			...INBOUND_FIXTURE,
			id: `${Date.now().toString(16)}${'1'.repeat(48)}`.slice(0, 64),
			subject: contact.userId,
			network: OTHER_NETWORK,
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
			//
			// Matched on the network as well as the subject: this same account is
			// a WhatsApp sender elsewhere on this bus, and the contract's own
			// fixture carries `consent: granted`, so an event about them from
			// another suite would answer this assertion with the wrong label
			// about the wrong conversation.
			const first = await inbound.waitFor(about(contact.userId, NETWORK), 60_000);
			expect(labelOf(first)).toBe('pending');
		} finally {
			inbound.close();
		}

		// The Gateway's projection is a durable consumer, so the list catches up
		// rather than being instantaneous. Read per network, because that is the
		// unit the list is keyed on: this contact is waiting on `matrix` — and on
		// `whatsapp` too, which is the perimeter `beforeAll` arranged and which
		// the decisions below will leave exactly where it is.
		await expect
			.poll(async () => await waitingNetworks(request, token, contact.userId), {
				timeout: 60_000
			})
			.toEqual([OTHER_NETWORK, NETWORK].sort());

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
								about(contact.userId, NETWORK)(message) &&
								!seenBefore.has(message.event.id)
						);
						return later.some((message) => labelOf(message) === 'granted');
					},
					{ timeout: 120_000, intervals: [2_000] }
				)
				.toBe(true);

			// The label is on the envelope, and the envelope is about the contact
			// on the network the decision was scoped to — which `about` is now the
			// keeper of, so a `granted` label about somebody else, or about this
			// person's WhatsApp perimeter, cannot satisfy the poll above either.
			const granted = inbound.seen.find(
				(message) =>
					about(contact.userId, NETWORK)(message) &&
					!seenBefore.has(message.event.id) &&
					labelOf(message) === 'granted'
			);
			expect(granted).toBeDefined();
			expect(networkOf(granted as BusMessage)).toBe(NETWORK);
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
		// the never-decided ones, no longer holds this contact **on this
		// network**.
		await expect(row).toHaveAttribute('data-decided-by', 'contact');
		// And what remains is the assertion that says why the network belongs in
		// the question. The decision was scoped to `matrix`; this same person is
		// still waiting on `whatsapp`, because nobody has answered for that
		// conversation and an absent decision is never a revoked one (ADR 0010).
		// Asserted as the whole list rather than as an absence, so a decision that
		// answered for too much would fail here too.
		await expect
			.poll(async () => await waitingNetworks(request, token, contact.userId), {
				timeout: 60_000
			})
			.toEqual([OTHER_NETWORK]);
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
			await inbound.waitFor(about(stack!.ownerId, NETWORK), 60_000);
		} finally {
			inbound.close();
		}

		// Given a moment, and then asked. A poll that waited for the owner to
		// appear would be the wrong shape: this asserts they never do.
		//
		// Asked about **every** network rather than one: the owner is not a
		// contact anywhere, so the empty list is the whole statement and naming a
		// perimeter here would weaken it.
		await new Promise((resolve) => setTimeout(resolve, 3_000));
		expect(await waitingNetworks(request, token, stack!.ownerId)).toEqual([]);

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
			.poll(async () => await waitingNetworks(request, token, SECOND_CONTACT), {
				timeout: 60_000
			})
			.toEqual([OTHER_NETWORK]);

		await signIn(page.context(), page.context().request, 'consent-bulk');
		await page.goto('/consent');
		await expect(page.getByTestId('consent-rows')).toBeVisible();

		// Narrow to one person, and the control says what it is about to write.
		// #137's rule: it acts on what is on screen, never on the whole list.
		//
		// Two rows, not one, and that is the perimeter again rather than a looser
		// assertion: one person seen on two networks is **two decisions**, because
		// a decision covers a conversation's network and not a human being. The
		// count on the button is the length of the array `bulkDecisions` returns
		// for the rows on screen, so this is the number of requests the press
		// would make.
		await page.getByTestId('consent-search').fill(CONTACT_LOCALPART);
		const showing = page.getByTestId('consent-showing');
		await expect(showing).toHaveAttribute('data-shown', '2');
		// The point of the assertion: the list is longer than what is shown, and
		// the control below counts the shown ones.
		expect(Number(await showing.getAttribute('data-total'))).toBeGreaterThan(2);
		const grant = page.getByTestId('bulk-grant');
		await expect(grant).toHaveAttribute('data-count', '2');

		// And it asks twice. One press arms, and the confirmation names the
		// number again.
		await grant.click();
		await expect(page.getByTestId('bulk-confirm')).toHaveAttribute('data-count', '2');
		await page.getByTestId('bulk-cancel').click();
		await expect(page.getByTestId('bulk-confirm')).toHaveCount(0);
	});
});

/**
 * Whether a bus message is an event about this subject **on this network**.
 *
 * Both halves, because consent is keyed on both and this bus is shared. The same
 * Matrix ID is this journey's contact on `matrix` and a WhatsApp sender in
 * `sensor/tests/`, and the contract's own inbound fixture carries `consent:
 * granted` — so a predicate on the subject alone would let another suite's event
 * answer an assertion about this conversation's label, which is #200's other half
 * and why the first spec of this file was intermittent.
 */
function about(subject: string, network: string) {
	return (message: BusMessage) =>
		message.event.subject === subject && networkOf(message) === network;
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

/**
 * Which networks the Gateway says this contact is waiting for a first decision on
 * — sorted, and empty when none.
 *
 * The same question `companion-gateway/tests/pending.rs::waiting_networks` asks
 * the same endpoint, deliberately the same shape: the pending list is keyed on
 * `(contact, network)` and that suite already asserts the consequence — *"a
 * contact that writes on a second network is waiting again there: consent has a
 * perimeter, and so does the list."* The version of this helper that returned
 * only the Matrix IDs threw the perimeter away, and a `true` it could not
 * distinguish from the one it wanted is #200.
 */
async function waitingNetworks(
	request: APIRequestContext,
	token: string,
	contact: string
): Promise<string[]> {
	const answer = await request.get('/api/contacts/pending', {
		headers: { cookie: `twalk_device=${token}` }
	});
	expect(answer.ok(), await answer.text()).toBeTruthy();
	const body = (await answer.json()) as { contacts: { contact: string; network: string }[] };
	return body.contacts
		.filter((entry) => entry.contact === contact)
		.map((entry) => entry.network)
		.sort();
}
