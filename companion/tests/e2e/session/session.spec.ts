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
// configures a **short** device token (`tests/real-stack.mjs`), so the expiry the
// ticket describes arrives inside a test rather than inside an afternoon.
// Everything else is the deployment: a real Gateway, a real Synapse, the real
// cookies with the paths the Gateway scopes them to.
//
// How short, and what that number buys, is #186 and is stated in
// `./harness.ts`: the client renews with a fifth of the lifetime in hand, so the
// lifetime this origin picks *is* the tolerance these four journeys have. It was
// five seconds, which left one, which is less than a browser timer slips on a
// machine that is compiling something — so this suite lost to load it created
// itself and was read as flaky. Nothing below waits for a clock to have probably
// run out any more: where the death of a credential is the precondition, the
// Gateway is asked; where the point is that something did *not* happen, the wait
// is the moment it was due, and the moment is derived rather than guessed.
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
	headroomMs,
	journeyBudgetMs,
	NO_STACK,
	rememberDeployment,
	renewedAfterMs,
	revokeDevice,
	sessionStack,
	signInDevice,
	Traffic,
	useDevice,
	waitPastTheScheduledRenewal,
	waitUntilTheGatewayRefusesThisBrowser
} from './harness';

const stack = sessionStack();

test.skip(stack === null, NO_STACK);

// One Gateway, one device list, one login per bridge: these journeys revoke
// devices and complete logins, and each would otherwise inherit the last one's
// deployment.
test.describe.configure({ mode: 'serial' });

// And a budget of their own, derived from the lifetime rather than left to the
// default thirty seconds — which was quietly carrying an eighteen-second
// deliberate wait plus a real-stack journey, and timing out under load on a spec
// about keeping sessions alive. `./harness.ts` says what the number is made of.
test.beforeEach(() => {
	if (stack !== null) {
		test.setTimeout(journeyBudgetMs(stack));
	}
});

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

	// The session this browser was handed has been adopted, which is the first
	// renewal and the thing that gives the page its schedule. Counted from after
	// it, with the screen up: a browser handed a short-lived token that then
	// spends a chunk of it fetching the app is this fixture racing itself on a
	// loaded machine, and says nothing about the fifteen-minute credential a
	// deployment issues.
	await expect
		.poll(() => traffic.successfulRefreshes.length, {
			message: 'the page adopted the session it was given'
		})
		.toBeGreaterThanOrEqual(1);
	const settled = traffic.refusals.length;
	const renewedBefore = traffic.successfulRefreshes.length;

	// Three whole token lifetimes of the user reading the screen and touching
	// nothing. Before this ticket, the session was dead after the first one.
	//
	// This is the spec the tolerance in `./harness.ts` is for: across these three
	// lifetimes the client has to renew on time every time, each renewal within
	// its own headroom, and if that headroom is smaller than the machine's timer
	// slip then what fails here is the fixture rather than the product (#186).
	const idleMs = 3 * deployment.deviceTokenTtlSeconds * 1000;
	await page.waitForTimeout(idleMs);

	// **Every** renewal that fell inside those three lifetimes happened — not
	// "at least three", which three lifetimes would satisfy while the client
	// quietly missed one and the `401` repair covered for it.
	//
	// `floor` and not `ceil`, and the difference is a whole renewal: the count is
	// of renewals whose moment is *inside* the window, which is a fact already
	// settled when the wait ends. Asking for one more means asking for the
	// renewal due just **after** it, and that is a wait on the future dressed up
	// as an assertion about the past — measured failing on a machine at load 25,
	// which is the exact mistake #186 is about.
	const renewalsDue = Math.floor(idleMs / renewedAfterMs(deployment));
	expect(
		traffic.successfulRefreshes.length - renewedBefore,
		'the session was renewed once per lifetime, ahead of each expiry'
	).toBeGreaterThanOrEqual(renewalsDue);

	// And nothing is mid-rotation as the user clicks. A refresh **rotates both
	// tokens**, and the Gateway invalidates the previous device token the moment
	// it issues the new one (`companion-gateway/src/session.rs`,
	// `Sessions::refresh`) — deliberately, so a leaked token stops working at the
	// next refresh rather than living out its lifetime. A request that crosses a
	// rotation therefore carries a credential that has just stopped being one, is
	// refused, and is repaired centrally, which is the *next* spec's subject and
	// not this one's.
	//
	// Two things keep the click clear of one. The wait above is three lifetimes,
	// which is **3.75 renewal intervals** on this origin (the client renews at
	// four fifths), so it ends in the middle of an interval rather than on a
	// boundary — the earlier `3 × (lifetime + 1)` was exactly four intervals and
	// clicked into a rotation on most runs, measuring an overlap it had arranged
	// itself. And a renewal cannot be in flight here, because every one the page
	// has asked for has answered.
	expect(traffic.refreshesAsked.length, 'no renewal is in flight as the user clicks').toBe(
		traffic.refreshes.length
	);

	// And the next thing they do just works.
	await page.getByTestId('to-personas').click();
	await expect(page.getByTestId('scope')).toBeVisible();
	await expect(page.getByTestId('scope-unknown')).toHaveCount(0);
	await expect(page.getByTestId('session-expired')).toHaveCount(0);

	// The assertion that matters: over three lifetimes of a working screen and
	// the click that followed them, not one request was refused. The user was
	// never interrupted, and nothing had to be retried on their behalf.
	expect(
		traffic.refusals.length,
		`refused: ${JSON.stringify(traffic.refusals)}; refreshes: ${JSON.stringify(traffic.refreshes)}`
	).toBe(settled);
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
	//
	// Waited out by **asking the Gateway**, not by sleeping for the configured
	// lifetime: this page rotates its token on its own schedule, so a lifetime
	// counted from here was never a bound on when the one it holds dies. When the
	// guess was short the click below simply worked, no `401` arrived to repair,
	// and this spec failed as though the repair were broken (#186).
	await page.route('**/api/session/refresh', (route) => route.abort());
	await waitUntilTheGatewayRefusesThisBrowser(page, deployment);
	// Including the refusals that probe just caused: what this spec counts is what
	// the *user's* next click costs.
	traffic.clear();
	await page.unroute('**/api/session/refresh');

	// The user clicks. One request is refused, and they never find out.
	await page.getByTestId('to-personas').click();
	await expect(page.getByTestId('scope')).toBeVisible();
	await expect(page.getByTestId('scope-unknown')).toHaveCount(0);
	await expect(page.getByTestId('session-expired')).toHaveCount(0);

	// Once, centrally: however many calls were refused together, one refresh,
	// and each call replayed. Not one refresh per screen, and not one per
	// failed call — the refresh rotates both tokens, so a second one racing
	// the first would destroy it.
	//
	// Polled rather than read once: a response the page has already acted on
	// can still be on its way to this process, and a screen that rendered is
	// not proof that every event about it has arrived.
	await expect
		.poll(() => traffic.successfulRefreshes.length, {
			message: 'exactly one refresh repaired the refusal'
		})
		.toBe(1);
	// The screen reads two things on mount — its perimeter and whether a
	// runtime is here (#177) — and both were issued with the dead token, so
	// both were refused; the point above is that one refresh repaired both.
	// Every refusal is one of those reads, and there is at least one.
	const refused = traffic.refusals.map((entry) => entry.url);
	expect(refused.length).toBeGreaterThan(0);
	expect(new Set(refused).size).toBe(refused.length);
	for (const url of refused) {
		expect(['/api/bridges', '/api/runtime']).toContain(url);
	}
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
	//
	// Counted as **requests**, which is the fix for this spec's own flake (#186).
	// "A sleeping tab refreshes nothing" is about a timer not firing, and a
	// refresh already in flight when the clock froze has its answer arrive during
	// the sleep — so counting answers made a legitimate in-flight response look
	// like a refresh the sleeping tab had made.
	await page.clock.pauseAt(new Date(Date.now() + 500));
	const askedBeforeSleeping = traffic.refreshesAsked.length;
	const answeredBeforeSleeping = traffic.refreshes.length;
	const refusedBeforeSleeping = traffic.refusals.length;

	// Past the moment this tab had scheduled its renewal for — which, its clock
	// being frozen, is a moment that goes by without the timer firing.
	await waitPastTheScheduledRenewal(page, deployment);
	expect(traffic.refreshesAsked.length, 'a sleeping tab refreshes nothing').toBe(
		askedBeforeSleeping
	);

	// Waking up: the page's timers run again, late, and the refresh this tab
	// slept through happens now. `runFor` past the renewal it missed, with this
	// suite's headroom on top.
	await page.clock.runFor(renewedAfterMs(deployment) + headroomMs(deployment));
	await expect
		.poll(() => traffic.successfulRefreshes.length, {
			message: 'the woken tab refreshes the session it slept through'
		})
		.toBeGreaterThan(answeredBeforeSleeping);

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

	// Both credentials dead at the Gateway, and dead the moment this returns:
	// revoking a device drops **both** of its token digests, so there is nothing
	// left to wait for at the Gateway — the dashboard's own button does this, and
	// so does signing out from another device.
	await revokeDevice(request, device, elsewhere);

	// Nobody clicked anything. The scheduled refresh came round, could not be
	// honoured, and the user is told then and there rather than at their next
	// failure — not a spinner, not a blank screen, and not the first screen.
	//
	// The only wait here is for that renewal to come round, and it is given the
	// interval it is actually due at rather than a sleep that happened to be
	// longer than it (#186). Before, the sleep was the token's lifetime, which is
	// shorter than the renewal interval for any lifetime this origin might sanely
	// be given — so the assertion's own five-second default was doing the waiting,
	// invisibly, and would have started failing the moment the lifetime moved.
	await expect(page.getByTestId('session-expired')).toBeVisible({
		timeout: renewedAfterMs(deployment) + headroomMs(deployment)
	});
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
