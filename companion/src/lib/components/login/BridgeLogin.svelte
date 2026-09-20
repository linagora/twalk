<!--
	Screens 3a, 3b and 3c of `docs/wireframes/companion-v0.1.md`: one bridge
	login, whatever the bridge asks for.

	# What this component owns, and what it does not

	It owns the **flow**: which bridge serves this network, the disclosure that
	comes before anything is started, the login session and its polling, the
	refusals that are about the deployment rather than the step, and the explicit
	abandon. It does **not** draw a step — each kind of step has a panel of its
	own, reached through the typed table in `panels.ts` (ADR 0030).

	That split is the fix for a real defect and not tidiness. Before it, the QR
	screen owned the sequence and knew six of the view's ten kinds, with no
	`{:else}`: an `input`, `cookies` or `emoji` step drew an empty seventeen-rem
	box that the session polled once a second for ever, which is where a Telegram
	QR login by an account with two-factor authentication ended up. The table
	makes a kind nobody draws a compile error, and the residual panel covers the
	one case no type can: a Gateway newer than this app.

	# What it does not do

	It does not hold a request open. The Gateway holds the bridge's blocking step
	and answers `GET .../login` immediately; this component polls
	(`LoginSession`) and the QR panel redraws when `generation` changes. That is
	why a phone that sleeps mid-scan loses a poll rather than the login, and why
	the wireframes' Server-Sent Events are not here.

	It does not present a failure as loading (#111). `GET /api/bridges` is the
	first thing it asks, and when that does not answer there is no bridge id, so
	no login is started — which used to leave a spinner and "Asking for a code…"
	on screen for ever. That is the incident this screen caused: the owner waited
	here, reported "I cannot get a QR code", and the Signal bridge had never been
	contacted.

	It stores nothing but the disclosure dismissal, which is a boolean. An answer
	to a step is a network credential in flight (ADR 0011): it lives in the
	panel's own state, is cleared before the request that relays it, and the
	panel is re-created when the step changes — the `{#key}` below — so nothing
	typed can outlive the question it answered.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { onDestroy, onMount } from 'svelte';

	import { page } from '$app/state';

	import { gateway } from '$lib/api/client';
	import { troubleOf, type ApiTrouble } from '$lib/api/trouble';
	import {
		bridgeOf,
		connectionNamedBy,
		isAmbiguous,
		loadRegistryAndBridges,
		pick
	} from '$lib/connections/registry';
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import type { LoginScreenCopy } from '$lib/networks/copy';
	import { LoginSession } from '$lib/networks/login-session';
	import type { FlowActions, StepPanel } from '$lib/networks/login-panels';
	import type { LoginState } from '$lib/networks/login-view';
	import { PANELS } from './panels';

	interface Props {
		copy: LoginScreenCopy;
		/**
		 * Something about this device that makes the login impossible, drawn in
		 * place of the whole login.
		 *
		 * The SMS preview's one: it reads the SMS off an Android phone, and an
		 * iPhone alone cannot feed it. A screen, not a warning above the form —
		 * and no login is started behind it.
		 */
		blocked?: Snippet;
	}

	let { copy, blocked }: Props = $props();

	/**
	 * The login this flow should **repair** rather than replace, from
	 * `?relink=`, which is where the management screen's *Re-link* button sends
	 * the user (ticket #108).
	 *
	 * It becomes the Gateway's `login_id` on the start call — mautrix's
	 * `?login_id=` — so the bridge re-logs in to the session it already holds
	 * instead of creating a second one. Without it a network that caps linked
	 * devices refuses the login outright, and the user's working session is
	 * still the one that is broken.
	 */
	const relink = $derived(page.url.searchParams.get('relink') ?? undefined);

	/** `null` until `GET /api/bridges` has answered. */
	let bridgeId = $state<string | null>(null);
	let bridgeKnown = $state(false);
	/**
	 * This kind has several connections and the URL named none: the screen
	 * will not pick one, and sends the user back to the cards (#272).
	 */
	let ambiguous = $state(false);
	/**
	 * Why the bridge list could not be read, or `null` when it could.
	 *
	 * Three outcomes rather than two, because "could not be reached" and
	 * "answered, and refused this session" send the user to look in completely
	 * different places — see `$lib/api/trouble.ts`.
	 */
	let bridgesTrouble = $state<ApiTrouble | null>(null);
	/** Set in `onMount`: a screen with no disclosure has nothing to accept. */
	let disclosureAccepted = $state(false);
	let session = $state<LoginSession | null>(null);
	let loginState = $state<LoginState>({ view: { kind: 'idle' }, conflict: null, trouble: null });
	let now = $state(Date.now());
	let unsubscribe: (() => void) | null = null;
	let ticker: ReturnType<typeof setInterval> | null = null;

	const view = $derived(loginState.view);
	/**
	 * What the panel is keyed on: the step, not the poll.
	 *
	 * A new step means a new panel, which is how a typed value cannot survive
	 * the question it was typed for. A kind with no step id — a code, a
	 * completion — keys on the kind, so the QR panel is not re-created every
	 * second.
	 */
	const stepKey = $derived('stepId' in view ? `${view.kind}:${view.stepId}` : view.kind);

	const flow: FlowActions = {
		restart: async () => {
			await session?.start({ prefer: copy.prefer, loginId: relink, takeOver: true });
		},
		cancel: async () => {
			await session?.cancel();
		},
		submit: async (stepId, data) => {
			await session?.submit(stepId, data);
		}
	};

	onMount(async () => {
		ticker = setInterval(() => (now = Date.now()), 1000);
		disclosureAccepted = copy.disclosure === null || remembered(copy.disclosure.storageKey);
		if (blocked === undefined) {
			await findBridge();
		}
	});

	/**
	 * Which connection this screen is about, which bridge carries it, and the
	 * login on it (#272).
	 *
	 * The connection is the URL's (`?connection=`) or the kind's only one —
	 * never the first of two — and the bridge is found by the id the
	 * connection names, never by matching networks. Separate from `onMount`
	 * so the terminal state below can offer it again: the Gateway being
	 * momentarily unreadable is a thing that passes, and the user's only
	 * useful action is to ask once more.
	 */
	async function findBridge() {
		bridgeKnown = false;
		bridgesTrouble = null;
		// A retry must not leave the previous attempt's subscription behind.
		unsubscribe?.();
		unsubscribe = null;
		session?.stop();
		session = null;
		const deployment = await loadRegistryAndBridges();
		if (deployment.trouble !== null) {
			bridgesTrouble = deployment.trouble;
			return;
		}
		bridgeKnown = true;
		const named = connectionNamedBy(page.url);
		const connection = pick(deployment.registry, copy.network, named);
		ambiguous = isAmbiguous(deployment.registry, copy.network, named);
		const row = connection === null ? null : bridgeOf(connection, deployment.bridges);
		bridgeId = row?.bridge_id ?? null;
		if (bridgeId !== null) {
			session = new LoginSession(bridgeId);
			unsubscribe = session.state.subscribe((next: LoginState) => (loginState = next));
			if (disclosureAccepted) {
				await session.start({ prefer: copy.prefer, loginId: relink });
			}
		}
	}

	onDestroy(() => {
		unsubscribe?.();
		session?.stop();
		if (ticker !== null) {
			clearInterval(ticker);
		}
	});

	/**
	 * The disclosure dismissal, per device and without expiry, as the wireframe
	 * says. Wrapped because a private window or Lockdown Mode throws here, and a
	 * browser that cannot remember the dismissal must show the card again —
	 * never fail the screen.
	 */
	function remembered(key: string): boolean {
		try {
			return localStorage.getItem(key) === 'accepted';
		} catch {
			return false;
		}
	}

	async function acceptDisclosure() {
		disclosureAccepted = true;
		if (copy.disclosure !== null) {
			try {
				localStorage.setItem(copy.disclosure.storageKey, 'accepted');
			} catch {
				// Nothing to do: the card comes back next time, which is safe.
			}
		}
		await session?.start({ prefer: copy.prefer, loginId: relink });
	}
</script>

<section class="screen" data-testid={`screen-${copy.network}`} data-login-state={view.kind}>
	<header class="stack">
		<p class="small">
			<a href="/networks">
				<Icon name="back" size="dense" />
				{$t('networks.back')}
			</a>
		</p>
		<h1>
			{$t(copy.title)}
			{#if copy.badge !== null}
				<span class="badge">{$t(copy.badge)}</span>
			{/if}
		</h1>
		{#if copy.caption !== null}
			<p class="subtitle">{$t(copy.caption)}</p>
		{/if}
	</header>

	{#if blocked !== undefined}
		{@render blocked()}
	{:else if bridgesTrouble !== null}
		<!-- #111: never a spinner. With no bridge list there is no login to
		     start, so this screen says so — and says which of the two things
		     happened, because they are fixed in different places. -->
		<div
			class="card card--warning"
			data-testid="bridges-unreadable"
			data-trouble={bridgesTrouble}
			role="alert"
		>
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('networks.bridgesUnreadable.title')}
			</p>
			<p>
				{#if bridgesTrouble === 'unreachable'}
					{$t('api.trouble.unreachable')}
				{:else if bridgesTrouble === 'session-refused'}
					{$t('api.trouble.sessionRefused')}
				{:else}
					{$t('api.trouble.refused')}
				{/if}
			</p>
			{#if bridgesTrouble !== 'session-refused'}
				<p>
					<button class="button button--primary" type="button" onclick={findBridge} data-testid="retry-bridges">
						<Icon name="reload" size="dense" />
						{$t('networks.retry')}
					</button>
				</p>
			{/if}
		</div>
	{:else if bridgeKnown && ambiguous}
		<!-- Two accounts of this kind and no word which: the cards say which is
		     which, and this screen will not guess (ADR 0033). -->
		<div class="card card--warning" data-testid="which-connection">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('networks.whichConnection.title', { network: $t(copy.title) })}
			</p>
			<p>{$t('networks.whichConnection.body', { network: $t(copy.title) })}</p>
			<p class="actions">
				<a class="button button--secondary" href="/networks">{$t('networks.whichConnection.back')}</a>
			</p>
		</div>
	{:else if bridgeKnown && bridgeId === null}
		<!-- The deployment has no bridge for this network. Configuration, not a
		     failure: say which variable turns it on rather than "error". -->
		<div class="card card--warning" data-testid="no-bridge">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('networks.noBridge.title')}
			</p>
			<p>{$t('networks.noBridge.body', { network: $t(copy.title) })}</p>
		</div>
	{:else if copy.disclosure !== null && !disclosureAccepted}
		<div class="card card--warning" data-testid="disclosure">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t(copy.disclosure.title)}
			</p>
			<p>{$t(copy.disclosure.body)}</p>
			<div class="actions">
				<button class="button button--primary" type="button" onclick={acceptDisclosure} data-testid="accept-disclosure">
					{$t(copy.disclosure.confirm)}
				</button>
				<a
					class="button button--secondary"
					href={copy.disclosure.learnMoreHref}
					target="_blank"
					rel="noreferrer noopener"
				>
					{$t(copy.disclosure.learnMore)}
					<Icon name="external-link" size="dense" />
				</a>
			</div>
		</div>
	{:else if loginState.conflict !== null}
		<!-- One login at a time per bridge. A screen, not a raw error: it names
		     which of the owner's devices started it and when, and offers the only
		     useful action. -->
		<div class="card card--warning" data-testid="login-in-flight">
			<p class="card__title">
				<Icon name="device" size="dense" />
				{$t('networks.conflict.title')}
			</p>
			<p>
				{$t('networks.conflict.body', {
					device: loginState.conflict.deviceName,
					started: loginState.conflict.startedAt
				})}
			</p>
			<div class="actions">
				<button class="button button--primary" type="button" onclick={flow.restart} data-testid="take-over">
					{$t('networks.conflict.takeOver')}
				</button>
				<a class="button button--secondary" href="/networks">{$t('networks.conflict.leaveIt')}</a>
			</div>
		</div>
	{:else}
		{#if copy.intro !== null}
			<p class="card card--info">
				<Icon name="phone" size="dense" />
				{$t(copy.intro)}
			</p>
		{/if}

		{#if loginState.trouble !== null}
			<p class="card card--warning" role="alert" data-testid="login-trouble" data-trouble={loginState.trouble}>
				{#if loginState.trouble === 'unauthenticated'}
					{$t('networks.trouble.unauthenticated')}
				{:else if loginState.trouble === 'too_many_logins'}
					{$t('networks.trouble.tooManyLogins')}
				{:else if loginState.trouble === 'bridge_unreachable' || loginState.trouble === 'bridge_refused'}
					{$t('networks.trouble.bridge')}
				{:else if loginState.trouble === 'network'}
					{$t('networks.trouble.network')}
				{:else if loginState.trouble === 'refused'}
					<!-- Never "something went wrong on your Twalk server": the
					     Gateway answered, and would not take the request. -->
					{$t('networks.trouble.refused')}
				{:else}
					{$t('networks.trouble.unexpected')}
				{/if}
			</p>
		{/if}

		<div class="panel" aria-live="polite">
			{#key stepKey}
				<!-- The one widening in the dispatch, and the reason it is safe:
				     the table's *type* is what forces a panel to exist for every
				     kind, and a panel that accepts a narrower view than the one
				     it is registered for cannot be put in the table at all. -->
				{@const Panel = PANELS[view.kind] as StepPanel}
				<Panel {view} {copy} {flow} {now} />
			{/key}
		</div>
	{/if}

	{#if copy.troubleshooting.length > 0}
		<details class="small" data-testid="troubleshooting">
			<summary>{$t('networks.troubleshooting')}</summary>
			<ul>
				{#each copy.troubleshooting as item (item)}
					<li>{$t(item)}</li>
				{/each}
			</ul>
		</details>
	{/if}
</section>

<style>
	h1 {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		flex-wrap: wrap;
	}

	.badge {
		font-size: var(--text-xs);
		padding: 2px var(--space-2);
		border-radius: var(--radius-pill);
		background: var(--color-warning-surface);
		border: 1px solid var(--color-warning);
		color: var(--color-text);
		font-weight: var(--font-weight-body);
	}

	.panel {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	details ul {
		margin: var(--space-2) 0 0;
		padding-inline-start: var(--space-4);
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		color: var(--color-text-muted);
	}
</style>
