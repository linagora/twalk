// Ticket #111 — the session, from a browser, against a real Gateway.
//
// The defect these journeys exist for: the device token lives fifteen minutes
// by design, the sign-in answer carries `expires_in` precisely so a client can
// refresh before it dies, the Gateway exposes `POST /api/session/refresh` — and
// nothing in `companion/src/` ever called it. A quarter of an hour into any
// session, `GET /api/bridges` answered `401` and the Signal screen span on
// "Asking for a code…" for ever. The owner reported "I cannot get a QR code";
// the Signal bridge had never been contacted.
//
// The TTL is not the bug and is not raised here to hide it. This origin
// configures a **five-second** device token (`tests/real-stack.mjs`), so the
// expiry the ticket describes arrives inside a test rather than inside an
// afternoon. Everything else is the deployment: a real Gateway, a real
// Synapse, the real cookies with the paths the Gateway scopes them to.
//
// Four journeys, and each proves a different half of the mechanism:
//
//   1. **the braces** — a working screen, left alone across three whole token
//      lifetimes, never sees a refusal at all;
//   2. **the belt** — a tab whose scheduled refresh did not happen meets one
//      `401`, which is repaired once, centrally, and the screen works;
//   3. **waking up** — a tab whose timers were frozen while the token died
//      refreshes late, on waking, and keeps working;
//   4. **the end of the road** — both credentials dead, the session-expired
//      state over the screen the user was on, `/signin` (#112) reached with
//      that screen as `next`, and a connected network still reading as
//      connected once the browser is signed in again.

import { expect, test } from '@playwright/test';

import {
	connectWhatsApp,
	NO_STACK,
	rememberDeployment,
	revokeDevice,
	sessionStack,
	signInDevice,
	Traffic,
	useDevice,
	waitForTokenToExpire
} from './harness';

const stack = sessionStack();

test.skip(stack === null, NO_STACK);

// One Gateway, one device list, one login per bridge: these journeys revoke
// devices and complete logins, and each would otherwise inherit the last one's
// deployment.
test.describe.configure({ mode: 'serial' });

test('a session refreshed on time never interrupts the screen the user is on', async ({
	page,
	context,
	request
}) => {
	const deployment = stack!;
	await rememberDeployment(context, deployment);
	await useDevice(context, await signInDevice(request, 'the working device'));

	const traffic = new Traffic(page);
	await page.goto('/networks');
	await expect(page.getByTestId('network-grid')).toBeVisible();

	// Counted from here, with the screen up and the session adopted. A browser
	// handed a five-second token that then spends longer than that fetching the
	// app is this fixture racing itself on a loaded machine, and says nothing
	// about the fifteen-minute credential a deployment issues.
	const settled = traffic.refusals.length;

	// Three whole token lifetimes of the user reading the screen and touching
	// nothing. Before this ticket, the session was dead after the first one.
	await page.waitForTimeout(3 * (deployment.deviceTokenTtlSeconds + 1) * 1000);

	// And the next thing they do just works.
	await page.getByTestId('to-personas').click();
	await expect(page.getByTestId('scope')).toBeVisible();
	await expect(page.getByTestId('scope-unknown')).toHaveCount(0);
	await expect(page.getByTestId('session-expired')).toHaveCount(0);

	// The assertion that matters: over three lifetimes of a working screen, not
	// one request was refused. The user was never interrupted, and nothing had
	// to be retried on their behalf.
	await expect
		.poll(() => traffic.successfulRefreshes.length, {
			message: 'the session was renewed once per lifetime, ahead of each expiry'
		})
		.toBeGreaterThanOrEqual(3);
	expect(traffic.refusals.length).toBe(settled);
});

test('a token that died while nothing was refreshing is repaired centrally, once', async ({
	page,
	context,
	request
}) => {
	const deployment = stack!;
	await rememberDeployment(context, deployment);
	await useDevice(context, await signInDevice(request, 'the interrupted device'));

	const traffic = new Traffic(page);
	await page.goto('/networks');
	await expect(page.getByTestId('network-grid')).toBeVisible();

	// A tab whose scheduled refresh cannot get out: offline, throttled, asleep.
	// The refresh this page had planned fails, the client backs off, and the
	// token dies with nobody watching — which is precisely the case a timer
	// cannot cover and the `401` must.
	await page.route('**/api/session/refresh', (route) => route.abort());
	await waitForTokenToExpire(page, deployment);
	traffic.clear();
	await page.unroute('**/api/session/refresh');

	// The user clicks. One request is refused, and they never find out.
	await page.getByTestId('to-personas').click();
	await expect(page.getByTestId('scope')).toBeVisible();
	await expect(page.getByTestId('scope-unknown')).toHaveCount(0);
	await expect(page.getByTestId('session-expired')).toHaveCount(0);

	// Once, centrally: one refusal, one refresh, and the call replayed. Not one
	// refresh per screen, and not one per failed call — the refresh rotates
	// both tokens, so a second one racing the first would destroy it.
	//
	// Polled rather than read once: a response the page has already acted on
	// can still be on its way to this process, and a screen that rendered is
	// not proof that every event about it has arrived.
	await expect
		.poll(() => traffic.successfulRefreshes.length, {
			message: 'exactly one refresh repaired the refusal'
		})
		.toBe(1);
	expect(traffic.refusals.map((entry) => entry.url)).toEqual(['/api/bridges']);
});

test('a tab that slept through its refresh catches up when it wakes', async ({
	page,
	context,
	request
}) => {
	const deployment = stack!;
	await rememberDeployment(context, deployment);
	await useDevice(context, await signInDevice(request, 'the sleeping device'));

	// A frozen page clock is a backgrounded tab: the timers this page sets do
	// not fire, however long the wall clock runs. The Gateway's clock is its
	// own and keeps running, which is the whole asymmetry — and the reason a
	// scheduled refresh alone was never going to be enough.
	await page.clock.install();

	const traffic = new Traffic(page);
	await page.goto('/networks');
	await expect(page.getByTestId('network-grid')).toBeVisible();

	// The tab has adopted its session and knows when to renew it.
	await expect
		.poll(() => traffic.successfulRefreshes.length, {
			message: 'the page adopted the session it was given'
		})
		.toBeGreaterThanOrEqual(1);

	// Asleep, from here. Counted rather than cleared: a response the page has
	// already had can still be on its way to this process.
	await page.clock.pauseAt(new Date(Date.now() + 500));
	const beforeSleeping = traffic.refreshes.length;
	const refusedBeforeSleeping = traffic.refusals.length;

	await waitForTokenToExpire(page, deployment);
	expect(traffic.refreshes.length, 'a sleeping tab refreshes nothing').toBe(beforeSleeping);

	// Waking up: the page's timers run again, late, and the refresh this tab
	// slept through happens now.
	await page.clock.runFor(10_000);
	await expect
		.poll(() => traffic.successfulRefreshes.length, {
			message: 'the woken tab refreshes the session it slept through'
		})
		.toBeGreaterThan(beforeSleeping);

	// Fully awake: the page's clock runs by itself again, as a foregrounded
	// tab's does.
	await page.clock.resume();

	// And the screen works, without the woken tab having met a single refusal:
	// the catch-up refresh got there before the user's next click did.
	await page.getByTestId('to-personas').click();
	await expect(page.getByTestId('scope')).toBeVisible();
	await expect(page.getByTestId('session-expired')).toHaveCount(0);
	expect(traffic.refusals.length).toBe(refusedBeforeSleeping);
});

test('with both credentials dead the user is told, and comes back to the same screen', async ({
	page,
	context,
	request
}) => {
	const deployment = stack!;
	await rememberDeployment(context, deployment);
	const device = await signInDevice(request, 'the device that expires');
	const elsewhere = await signInDevice(request, 'another device of the same owner');
	await useDevice(context, device);

	// A network this user connected, which is the thing they must not be made
	// to re-pair. The owner nearly did exactly that, twice, because an expired
	// session looked like a broken bridge.
	await connectWhatsApp(page, request);
	await page.goto('/networks');
	await expect(page.getByTestId('card-whatsapp')).toHaveAttribute('data-connected', 'yes');

	// And the user walks on, to a screen well inside the journey. This is
	// where they are when it happens.
	await page.getByTestId('to-personas').click();
	await expect(page.getByTestId('scope')).toBeVisible();

	// Both credentials dead at the Gateway: the device token by its own
	// lifetime, the refresh token because this device was revoked — the
	// dashboard's own button, or the same thing done from another device.
	await revokeDevice(request, device, elsewhere);
	await waitForTokenToExpire(page, deployment);

	// Nobody clicked anything. The scheduled refresh came round, could not be
	// honoured, and the user is told then and there rather than at their next
	// failure — not a spinner, not a blank screen, and not the first screen.
	await expect(page.getByTestId('session-expired')).toBeVisible();
	await expect(page).toHaveURL(/\/personas$/);

	// The screen underneath is still mounted and still theirs: the dialog is
	// over it, not instead of it.
	await expect(page.getByTestId('screen-personas')).toBeVisible();

	// And the way out carries where to come back to — screen 4, not the
	// dashboard and not screen 1. `/signin` is ticket #112's screen; what this
	// ticket owns is that it is handed the right destination.
	const wayOut = page.getByTestId('session-expired-signin');
	await expect(wayOut).toHaveAttribute('href', `/signin?next=${encodeURIComponent('/personas')}`);

	// Followed, and it arrives on the sign-in screen carrying screen 4 as its
	// destination.
	await wayOut.click();
	await expect(page.getByTestId('screen-signin')).toBeVisible();
	await expect(page).toHaveURL(new RegExp(`/signin\\?next=${encodeURIComponent('/personas')}$`));

	// The sign-in itself is not completed here, and cannot be: `/signin` offers
	// its form only where the Gateway's registration relay created the owner's
	// account, and this origin's owner is a bot provisioned on the shared test
	// stack years of test runs ago. That journey is #112's own, in
	// `tests/e2e/bootstrap.spec.ts`, on the Gateway that did create its account.
	//
	// So the session is restored the way the rest of this file does it, and
	// what this ticket owns is asserted: what the user finds when they get
	// back.
	await useDevice(context, await signInDevice(request, 'the device signed in again'));
	await page.goto('/personas');
	await expect(page.getByTestId('scope')).toBeVisible();
	await expect(page.getByTestId('session-expired')).toHaveCount(0);
	await expect(page.getByTestId('scope-unknown')).toHaveCount(0);

	// And the network reads as connected, not as broken. Nothing about the
	// bridge ever changed; only the browser's credential did.
	await page.goto('/networks');
	await expect(page.getByTestId('card-whatsapp')).toHaveAttribute('data-connected', 'yes');
	await expect(page.getByTestId('bridges-unknown')).toHaveCount(0);
});
