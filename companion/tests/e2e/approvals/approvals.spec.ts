// The approval journey (#100), against a real Gateway and a real bus.
//
// One suggestion appears, is approved, and the screen says what happened — and
// the reply is read back off NATS, because "the UI said so" is not evidence
// that anything left the deployment. Then the refusals, each asserted to reach
// a **terminal** state: a named cause, a next step, and no spinner.
//
// The absence assertions are the ones worth reading. The contact's own words,
// their display name and their network identifier are published on the trigger
// on purpose, and every screen this journey visits is searched for them.

import { expect, test, type Page } from '@playwright/test';

import { watchBus, type BusWatch } from '../dashboard/bus';
import {
	bridgeStack,
	decideAbout,
	FRENCH_DISCLOSURE,
	journeyBudgetMs,
	NO_STACK,
	publishSuggestion,
	signIn,
	switchDisclosure,
	waitForSuggestion
} from './harness';

const REPLY_APPROVED_SUBJECT = 'twalk.persona.reply.approved.v1';

const stack = bridgeStack();

test.skip(stack === null, NO_STACK);
test.describe.configure({ mode: 'serial' });

// A budget of this file's own, because every spec below opens by waiting up to
// thirty seconds for the Gateway to see the suggestion it just published — and
// the default budget is also thirty, so that wait could never use its window
// (#186). `./harness.ts` says what the number is made of.
test.beforeEach(() => {
	test.setTimeout(journeyBudgetMs);
});

/** Everything the page rendered, for an absence assertion to search. */
async function rendered(page: Page): Promise<string> {
	return (await page.locator('body').innerText()) + (await page.content());
}

test('a suggestion appears, is approved, and the screen says what happened', async ({
	page,
	context,
	request
}) => {
	const it = stack!;
	const token = await signIn(context, request, 'the approval journey');
	const published = await publishSuggestion({
		serverName: it.serverName,
		natsPort: it.natsPort,
		body: 'MARKER-REPLY-approve Parfait, on dit 20h alors !'
	});
	await decideAbout(request, token, published.contact, 'granted');
	await waitForSuggestion(request, token, published.suggestionId);

	const bus = await watchBus(it.natsPort, REPLY_APPROVED_SUBJECT);
	try {
		await page.goto('/approvals');
		const row = page.getByTestId(`suggestion-${published.suggestionId}`);
		await expect(row).toBeVisible();
		await expect(row.getByTestId('proposed')).toContainText('MARKER-REPLY-approve');
		await expect(row).toHaveAttribute('data-standing', 'approvable');

		// #121, ADR 0031: the outgoing message, whole. The sentence the reply
		// discloses itself with stands beside the body, fixed — in the
		// suggestion's own language, which is the persona's choice and not this
		// interface's — and it is not in the blockquote, which is the persona's
		// words alone.
		const disclosure = row.getByTestId('disclosure');
		await expect(disclosure).toBeVisible();
		await expect(disclosure).toHaveAttribute('data-switch', 'on');
		await expect(disclosure.getByTestId('disclosure-sentence')).toHaveText(FRENCH_DISCLOSURE);
		await expect(row.getByTestId('proposed')).not.toContainText(FRENCH_DISCLOSURE);
		await expect(row.getByTestId('disclosure-off')).toHaveCount(0);

		// #160, on the screen: the row says which network it answers and never
		// who wrote. Nothing of the contact's message is anywhere on this page
		// — not their words, not their name, not their number (#110, ADR 0012).
		await expect(row.getByTestId('trigger')).toContainText('WhatsApp');
		const page1 = await rendered(page);
		expect(page1).not.toContain(published.inboundBody);
		expect(page1).not.toContain(published.displayName);
		expect(page1).not.toContain(published.networkIdentifier);

		// #216, before the button: whether this reply can reach the contact at
		// all is said *here*, from the Gateway's read of the homeserver, and
		// not discovered after the fact. This stack's bridges are stubs the
		// register cannot ask, so the honest answer is `unknown` — the point
		// is that the sentence exists and names its reason, and that it is
		// never `can_reach` on a guess.
		const delivery = row.getByTestId('delivery');
		await expect(delivery).toBeVisible();
		await expect(delivery).toHaveAttribute('data-reach', 'unknown');
		await expect(delivery).toHaveAttribute('data-detail', /.+/);

		// The one deliberate act.
		await row.getByTestId('approve').click();
		await expect(row.getByTestId('sent')).toBeVisible();
		// It says where the reply landed, and under whose name — and calls it
		// published, never sent: delivery is the next sentence, and the
		// Sensor's to write.
		await expect(row.getByTestId('sent')).toContainText(it.ownerId);
		// Sent as written: the persona's words stay the text on screen, and
		// nothing claims the user wrote something else (#217).
		await expect(row.getByTestId('proposed')).toContainText('MARKER-REPLY-approve');
		await expect(row.getByTestId('approved-text')).toHaveCount(0);
		await expect(row.getByTestId('delivered')).toBeVisible();
		await expect(row.getByTestId('delivered')).toHaveAttribute('data-reach', 'pending');
		// #121: once the reply is sealed, the screen stops speaking about the
		// switch in the present tense — what went out is the Gateway's record,
		// not the switch as it stands now.
		await expect(row.getByTestId('disclosure')).toHaveCount(0);
		await expect(row.getByTestId('disclosure-off')).toHaveCount(0);

		// And it actually left the deployment.
		const event = await bus.waitFor(
			(message) => message.event.subject === published.suggestionId
		);
		const envelope = event.event as unknown as {
			source: string;
			data: {
				approved_by: string;
				target: { room_id: string };
				final: { body: string };
				disclosure?: string;
			};
		};
		// ADR 0022: the Gateway publishes it, and the `source` still names the
		// persona whose suggestion it was.
		expect(envelope.source).toContain('/personas/assistant');
		expect(envelope.data.approved_by).toBe(it.ownerId);
		expect(envelope.data.target.room_id).toBe(published.roomId);
		// #121: what actually left is the body **and** the sentence after it, on
		// a line of its own — appended by the Gateway, exactly as the screen
		// showed it, and carried as a member of its own besides.
		expect(envelope.data.final.body).toBe(`${published.body}\n${FRENCH_DISCLOSURE}`);
		expect(envelope.data.disclosure).toBe(FRENCH_DISCLOSURE);
	} finally {
		bus.close();
	}

	// Re-read: the standing comes from the Gateway, not from what the screen
	// believes it just did.
	await page.reload();
	await expect(page.getByTestId(`suggestion-${published.suggestionId}`)).toHaveAttribute(
		'data-standing',
		'approved'
	);
	await expect(
		page.getByTestId(`suggestion-${published.suggestionId}`).getByTestId('already-sent')
	).toBeVisible();
	// Published and delivered are two sentences after a re-read too, and no
	// Sensor runs on this stack, so the second one is still "not yet".
	await expect(
		page.getByTestId(`suggestion-${published.suggestionId}`).getByTestId('delivered')
	).toHaveAttribute('data-reach', 'pending');
	// An approved row offers nothing to press again.
	await expect(
		page.getByTestId(`suggestion-${published.suggestionId}`).getByTestId('approve')
	).toHaveCount(0);
});

test('nothing is approved by a keystroke, and nothing approves a list', async ({
	page,
	context,
	request
}) => {
	const it = stack!;
	const token = await signIn(context, request, 'the deliberate-act journey');
	const published = await publishSuggestion({
		serverName: it.serverName,
		natsPort: it.natsPort,
		body: 'MARKER-REPLY-keystroke Je te confirme demain.'
	});
	await decideAbout(request, token, published.contact, 'granted');
	await waitForSuggestion(request, token, published.suggestionId);

	const approvals: string[] = [];
	page.on('request', (sent) => {
		if (sent.method() === 'POST' && new URL(sent.url()).pathname === '/api/approvals') {
			approvals.push(sent.url());
		}
	});

	await page.goto('/approvals');
	const row = page.getByTestId(`suggestion-${published.suggestionId}`);
	await expect(row).toBeVisible();

	// `CONTEXT.md`: "never a default, never a batch". There is no control that
	// approves everything, and nothing is pre-selected.
	await expect(page.getByTestId('approve-all')).toHaveCount(0);
	await expect(page.locator('input[type="checkbox"]')).toHaveCount(0);

	// Editing, then typing, then pressing Enter, then Escape: none of it sends.
	await row.getByTestId('edit').click();
	const editor = row.getByTestId('editor');
	await expect(editor).toBeVisible();
	// #121: the editor opens on the persona's words alone, and the sentence
	// stands under it — the one line of the outgoing message the user does not
	// write, and cannot remove from this reply.
	await expect(editor).not.toHaveValue(new RegExp(FRENCH_DISCLOSURE));
	await expect(row.getByTestId('disclosure')).toHaveCount(1);
	await expect(row.getByTestId('disclosure-sentence')).toHaveText(FRENCH_DISCLOSURE);
	await editor.fill('Une autre formulation');
	await editor.press('Enter');
	await editor.press('Escape');
	await page.keyboard.press('Enter');
	expect(approvals, 'a keystroke approved something').toEqual([]);

	// It is the button that sends, and the edit is carried.
	await row.getByTestId('approve').click();
	await expect(row.getByTestId('sent')).toBeVisible();
	expect(approvals.length).toBe(1);
	// #217: what the screen now shows is what went out — the user's words —
	// with the persona's original still reachable, not the other way round.
	// The Gateway holds no text (ADR 0022), so this is the screen's own memory.
	await expect(row.getByTestId('approved-text')).toContainText('Une autre formulation');
	await expect(row.getByTestId('proposed')).toContainText('MARKER-REPLY-keystroke');
	await expect(row.getByTestId('proposed')).not.toContainText('Une autre formulation');
	const answer = await request.get(`/api/suggestions/${published.suggestionId}`, {
		headers: { cookie: `twalk_device=${token}` }
	});
	const body = (await answer.json()) as { approval: { edited: boolean } };
	expect(body.approval.edited, 'the edit was recorded as an edit').toBe(true);
});

test('a consent revoked since the suggestion was written is a named refusal, not a spinner', async ({
	page,
	context,
	request
}) => {
	const it = stack!;
	const token = await signIn(context, request, 'the revoked-consent journey');
	const published = await publishSuggestion({
		serverName: it.serverName,
		natsPort: it.natsPort,
		body: 'MARKER-REPLY-revoked Avec plaisir, à bientôt.'
	});
	// Granted when the persona wrote it; revoked before the user approves —
	// which is the whole point of checking consent "at that moment".
	await decideAbout(request, token, published.contact, 'granted');
	await waitForSuggestion(request, token, published.suggestionId);
	await decideAbout(request, token, published.contact, 'revoked');

	await page.goto('/approvals');
	const row = page.getByTestId(`suggestion-${published.suggestionId}`);
	await expect(row).toBeVisible();
	await row.getByTestId('approve').click();

	const problem = row.getByTestId('row-problem');
	await expect(problem).toBeVisible();
	// The cause, by the Gateway's own stable code — and never collapsed into
	// `consent_pending`, which is a different sentence about a different fact.
	await expect(problem).toHaveAttribute('data-code', 'consent_revoked');
	await expect(problem).toHaveAttribute('data-sent', 'no');
	// And what to do about it. A cause with no next step is a dead end.
	await expect(row.getByTestId('row-remedy')).not.toBeEmpty();
	// Terminal: the button is back to offering the action, not spinning.
	await expect(row.getByTestId('approve')).toBeEnabled();
	await expect(row.locator('.spinner')).toHaveCount(0);
});

test('a suggestion never consented is its own refusal', async ({ page, context, request }) => {
	const it = stack!;
	const token = await signIn(context, request, 'the never-consented journey');
	// Observed as `pending` at the Sensor: the audit fact at observation time,
	// which the Gateway checks separately from the state now.
	const published = await publishSuggestion({
		serverName: it.serverName,
		natsPort: it.natsPort,
		body: 'MARKER-REPLY-unconsented Bonjour !',
		observedConsent: 'pending'
	});
	await decideAbout(request, token, published.contact, 'granted');
	await waitForSuggestion(request, token, published.suggestionId);

	await page.goto('/approvals');
	const row = page.getByTestId(`suggestion-${published.suggestionId}`);
	await row.getByTestId('approve').click();
	const problem = row.getByTestId('row-problem');
	await expect(problem).toBeVisible();
	await expect(problem).toHaveAttribute('data-code', 'suggestion_was_never_consented');
	await expect(row.getByTestId('row-remedy')).not.toBeEmpty();
});

test('an expired suggestion is shown as no longer approvable, with the reason', async ({
	page,
	context,
	request
}) => {
	const it = stack!;
	const token = await signIn(context, request, 'the expired journey');
	const published = await publishSuggestion({
		serverName: it.serverName,
		natsPort: it.natsPort,
		body: 'MARKER-REPLY-expired Trop tard, celle-ci.',
		expiresAt: new Date(Date.now() - 60_000).toISOString()
	});
	await decideAbout(request, token, published.contact, 'granted');
	await waitForSuggestion(request, token, published.suggestionId);

	await page.goto('/approvals');
	const row = page.getByTestId(`suggestion-${published.suggestionId}`);
	await expect(row).toBeVisible();
	await expect(row).toHaveAttribute('data-standing', 'expired');
	// Shown rather than vanished, with the reason — and with nothing to press.
	await expect(row.getByTestId('proposed')).toContainText('MARKER-REPLY-expired');
	await expect(row.getByTestId('expired-reason')).toBeVisible();
	await expect(row.getByTestId('approve')).toHaveCount(0);
});

test('a server that does not answer is a retry, and never a spinner', async ({
	page,
	context,
	request
}) => {
	const it = stack!;
	const token = await signIn(context, request, 'the unreachable journey');
	const published = await publishSuggestion({
		serverName: it.serverName,
		natsPort: it.natsPort,
		body: 'MARKER-REPLY-unreachable Je te rappelle.'
	});
	await decideAbout(request, token, published.contact, 'granted');
	await waitForSuggestion(request, token, published.suggestionId);

	await page.goto('/approvals');
	const row = page.getByTestId(`suggestion-${published.suggestionId}`);
	await expect(row).toBeVisible();

	// Nothing answers the approval at all — the failure `troubleOf` exists to
	// keep apart from a refusal (#116, #141).
	await page.route('**/api/approvals', (route) => route.abort('failed'));
	await row.getByTestId('approve').click();

	const problem = row.getByTestId('row-problem');
	await expect(problem).toBeVisible();
	await expect(problem).toHaveAttribute('data-code', 'unreachable');
	await expect(problem).toHaveAttribute('data-remedy', 'retry');
	await expect(row.getByTestId('approve')).toBeEnabled();
	await expect(row.locator('.spinner')).toHaveCount(0);

	// And it recovers: the same press, once the server answers again.
	await page.unroute('**/api/approvals');
	await row.getByTestId('approve').click();
	await expect(row.getByTestId('sent')).toBeVisible();
});

test('refusing hides the row here and changes nothing at the server', async ({
	page,
	context,
	request
}) => {
	const it = stack!;
	const token = await signIn(context, request, 'the refusal journey');
	const published = await publishSuggestion({
		serverName: it.serverName,
		natsPort: it.natsPort,
		body: 'MARKER-REPLY-refused Non, pas celle-là.'
	});
	await decideAbout(request, token, published.contact, 'granted');
	await waitForSuggestion(request, token, published.suggestionId);

	await page.goto('/approvals');
	const row = page.getByTestId(`suggestion-${published.suggestionId}`);
	await expect(row).toBeVisible();
	// The screen says exactly what refusing does before it is pressed.
	await expect(row.getByTestId('refuse-meaning')).toBeVisible();
	await row.getByTestId('refuse').click();
	await expect(page.getByTestId(`suggestion-${published.suggestionId}`)).toHaveCount(0);
	await expect(page.getByTestId('notice-dismissed')).toBeVisible();

	// Nothing was recorded: the suggestion is exactly where it was, and it is
	// still approvable. A local hiding is all this can honestly be.
	const answer = await request.get(`/api/suggestions/${published.suggestionId}`, {
		headers: { cookie: `twalk_device=${token}` }
	});
	const still = (await answer.json()) as { standing: string };
	expect(still.standing).toBe('approvable');

	// And it can be taken back, because a hiding that could not be undone
	// would be worse than one that can.
	await page.getByTestId('restore-dismissed').click();
	await expect(page.getByTestId(`suggestion-${published.suggestionId}`)).toBeVisible();
});

test('the dashboard carries a count and a link, and not one word of the text', async ({
	page,
	context,
	request
}) => {
	const it = stack!;
	const token = await signIn(context, request, 'the dashboard-count journey');
	const published = await publishSuggestion({
		serverName: it.serverName,
		natsPort: it.natsPort,
		body: 'MARKER-REPLY-dashboard Ça marche pour jeudi.'
	});
	await decideAbout(request, token, published.contact, 'granted');
	await waitForSuggestion(request, token, published.suggestionId);

	await page.goto('/dashboard');
	const chip = page.getByTestId('approvals-chip');
	await expect(chip).toBeVisible();
	expect(Number(await chip.getAttribute('data-count'))).toBeGreaterThan(0);

	// The rule the dashboard must keep (#100, #74): the home screen is what
	// gets unlocked on a train.
	const drawn = await rendered(page);
	expect(drawn, 'the dashboard rendered a suggestion').not.toContain('MARKER-REPLY-dashboard');
	expect(drawn).not.toContain(published.inboundBody);
	expect(drawn).not.toContain(published.displayName);

	// A count, and where to act on it.
	await page.getByTestId('to-approvals').click();
	await expect(page.getByTestId('screen-approvals')).toBeVisible();
	await expect(
		page.getByTestId(`suggestion-${published.suggestionId}`).getByTestId('proposed')
	).toContainText('MARKER-REPLY-dashboard');
});

test('when the disclosure is off, the screen says so, and the reply goes out bare', async ({
	page,
	context,
	request
}) => {
	const it = stack!;
	const token = await signIn(context, request, 'the disclosure-off journey');
	const published = await publishSuggestion({
		serverName: it.serverName,
		natsPort: it.natsPort,
		body: 'MARKER-REPLY-undisclosed Oui, ça me va.'
	});
	await decideAbout(request, token, published.contact, 'granted');
	await waitForSuggestion(request, token, published.suggestionId);

	// ADR 0019: turning it off is a deliberate act with a timestamp, and it is
	// global — so this journey is the one that must put it back, whatever
	// happens in between, or every journey after it would run undisclosed.
	// The flip is inside the `try` so that the `finally` covers it too.
	let bus: BusWatch | null = null;
	try {
		const decided = await switchDisclosure(request, token, false, 'the disclosure-off journey');
		expect(decided.enabled).toBe(false);
		expect(decided.actor).toBe(it.ownerId);
		bus = await watchBus(it.natsPort, REPLY_APPROVED_SUBJECT);
		await page.goto('/approvals');
		const row = page.getByTestId(`suggestion-${published.suggestionId}`);
		await expect(row).toBeVisible();

		// ADR 0031: "when the switch is off, the screen says so" — with the
		// date, at the one moment the user is thinking about this message going
		// to this person. The sentence is not drawn as though it would go out.
		const off = row.getByTestId('disclosure-off');
		await expect(off).toBeVisible();
		await expect(off).toContainText(/turned off since|désactivée depuis/);
		await expect(off).toContainText(String(new Date(decided.since!).getUTCFullYear()));
		await expect(row.getByTestId('disclosure')).toHaveCount(0);

		await row.getByTestId('approve').click();
		await expect(row.getByTestId('sent')).toBeVisible();

		// And what left is the body alone: no line after it, no member.
		const event = await bus.waitFor(
			(message) => message.event.subject === published.suggestionId
		);
		const data = (event.event as unknown as { data: { final: { body: string } } }).data;
		expect(data.final.body).toBe(published.body);
		expect('disclosure' in data).toBe(false);
	} finally {
		bus?.close();
		const restored = await switchDisclosure(request, token, true);
		expect(restored.enabled).toBe(true);
	}

	// Back on: the off line is gone from the screen on the next read.
	await page.reload();
	await expect(
		page.getByTestId(`suggestion-${published.suggestionId}`).getByTestId('disclosure-off')
	).toHaveCount(0);
});

test('a suggestion that carries no sentence is said to go out without one', async ({
	page,
	context,
	request
}) => {
	const it = stack!;
	const token = await signIn(context, request, 'the no-disclosure journey');
	// A persona from before the member existed, or one that set none: the
	// Gateway appends nothing, and the screen must not invent a sentence the
	// contact would not receive.
	const published = await publishSuggestion({
		serverName: it.serverName,
		natsPort: it.natsPort,
		body: 'MARKER-REPLY-nosentence Bien reçu.',
		disclosure: null
	});
	await decideAbout(request, token, published.contact, 'granted');
	await waitForSuggestion(request, token, published.suggestionId);

	await page.goto('/approvals');
	const row = page.getByTestId(`suggestion-${published.suggestionId}`);
	await expect(row).toBeVisible();
	await expect(row.getByTestId('disclosure-none')).toBeVisible();
	await expect(row.getByTestId('disclosure')).toHaveCount(0);
	await expect(row.getByTestId('disclosure-off')).toHaveCount(0);
	expect(await row.innerText()).not.toContain(FRENCH_DISCLOSURE);
});
