<!--
	Screens 3a (WhatsApp) and 3b (Signal) of `docs/wireframes/companion-v0.1.md`:
	a QR login, drawn in the browser from the payload the Gateway relays.

	One component for both, because the Gateway reports one shape for every
	bridge (ticket #55) and the two screens differ only in their words — which
	arrive as `$lib/networks/copy.ts`.

	# What this screen does *not* do

	It does not hold a request open. The Gateway holds the bridge's blocking
	step and answers `GET .../login` immediately; this screen polls
	(`LoginSession`) and redraws when `generation` changes. That is why a phone
	that sleeps mid-scan loses a poll rather than the login, and why the
	wireframes' Server-Sent Events are not here.

	It does not trust the countdown. `valid_for_seconds` is the Gateway's own
	estimate of the network's refresh interval, documented as such; WhatsApp's
	whole budget is about 2m40 across refreshes. So the countdown is shown
	because the wireframe asks for it, and the *fact* the redraw keys off is the
	generation.

	It stores nothing. The payload lives in the component's state and in the
	SVG; the only thing that outlives the page is the disclosure dismissal,
	which is a boolean.
-->
<script lang="ts">
	import { onDestroy, onMount } from 'svelte';

	import { gateway } from '$lib/api/client';
	import Icon from '$lib/icons/Icon.svelte';
	import QrCode from '$lib/components/QrCode.svelte';
	import { t } from '$lib/i18n';
	import { LoginSession } from '$lib/networks/login-session';
	import { secondsLeft, type LoginState } from '$lib/networks/login-view';
	import type { QrScreenCopy } from '$lib/networks/copy';

	interface Props {
		copy: QrScreenCopy;
	}

	let { copy }: Props = $props();

	/** `null` until `GET /api/bridges` has answered. */
	let bridgeId = $state<string | null>(null);
	let bridgeKnown = $state(false);
	/** Set in `onMount`: a screen with no disclosure has nothing to accept. */
	let disclosureAccepted = $state(false);
	let session = $state<LoginSession | null>(null);
	let loginState = $state<LoginState>({ view: { kind: 'idle' }, conflict: null, trouble: null });
	let now = $state(Date.now());
	let troubleshootingOpen = $state(false);
	let unsubscribe: (() => void) | null = null;
	let ticker: ReturnType<typeof setInterval> | null = null;

	const view = $derived(loginState.view);
	const remaining = $derived(view.kind === 'qr' ? secondsLeft(view.expiresAt, now) : 0);
	/**
	 * The wireframes' *QR expired* state. Reached only when a refreshed
	 * generation did not arrive in time — the bridge stopped refreshing, or the
	 * Gateway lost the step — so it offers the button rather than a stale code.
	 */
	const expired = $derived(view.kind === 'qr' && remaining === 0);

	onMount(async () => {
		ticker = setInterval(() => (now = Date.now()), 1000);
		const listed = await gateway.GET('/api/bridges');
		bridgeKnown = listed.error === undefined;
		const row = listed.data?.bridges.find((bridge) => bridge.network === copy.network);
		bridgeId = row?.bridge_id ?? null;
		disclosureAccepted =
			copy.disclosure === null || remembered(copy.disclosure.storageKey);
		if (bridgeId !== null) {
			session = new LoginSession(bridgeId);
			unsubscribe = session.state.subscribe((next: LoginState) => (loginState = next));
			if (disclosureAccepted) {
				await session.start({ prefer: 'qr' });
			}
		}
	});

	onDestroy(() => {
		unsubscribe?.();
		session?.stop();
		if (ticker !== null) {
			clearInterval(ticker);
		}
	});

	/**
	 * The disclosure dismissal, per device and without expiry, as the wireframe
	 * says. Wrapped because a private window or Lockdown Mode throws here, and
	 * a browser that cannot remember the dismissal must show the card again —
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
		await session?.start({ prefer: 'qr' });
	}

	/** The *Refresh* button, and the way out of every dead end on this screen. */
	async function restart() {
		await session?.start({ prefer: 'qr', takeOver: true });
	}

	async function cancel() {
		await session?.cancel();
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
		<h1>{$t(copy.title)}</h1>
		<p class="subtitle">{$t(copy.caption)}</p>
	</header>

	{#if bridgeKnown && bridgeId === null}
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
		     which of the owner's devices started it and when, and offers the
		     only useful action. -->
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
				<button class="button button--primary" type="button" onclick={restart} data-testid="take-over">
					{$t('networks.conflict.takeOver')}
				</button>
				<a class="button button--secondary" href="/networks">{$t('networks.conflict.leaveIt')}</a>
			</div>
		</div>
	{:else}
		{#if loginState.trouble !== null}
			<p class="card card--warning" role="alert" data-testid="login-trouble">
				{#if loginState.trouble === 'unauthenticated'}
					{$t('networks.trouble.unauthenticated')}
				{:else if loginState.trouble === 'too_many_logins'}
					{$t('networks.trouble.tooManyLogins')}
				{:else if loginState.trouble === 'bridge_unreachable' || loginState.trouble === 'bridge_refused'}
					{$t('networks.trouble.bridge')}
				{:else if loginState.trouble === 'network'}
					{$t('networks.trouble.network')}
				{:else}
					{$t('networks.trouble.unexpected')}
				{/if}
			</p>
		{/if}

		<div class="qr-slot" aria-live="polite" data-testid="qr-slot">
			{#if view.kind === 'starting' || view.kind === 'idle'}
				<p class="waiting">
					<span class="spinner" aria-hidden="true"></span>
					{$t('networks.starting')}
				</p>
			{:else if view.kind === 'qr' && !expired}
				<!-- `#key` on the generation: a refreshed code is a bumped
				     generation with a new payload, and re-creating the element is
				     what guarantees the drawn code is the current one. -->
				{#key view.generation}
					<QrCode data={view.data} label={$t(copy.qrAlt)} />
				{/key}
				<p class="small muted countdown" data-testid="countdown">
					{$t('networks.expiresIn', { seconds: remaining })}
				</p>
			{:else if view.kind === 'qr'}
				<div class="waiting stack" data-testid="qr-expired">
					<p>{$t('networks.expired')}</p>
					<button class="button button--primary" type="button" onclick={restart} data-testid="refresh-qr">
						<Icon name="reload" size="dense" />
						{$t('networks.refresh')}
					</button>
				</div>
			{:else if view.kind === 'verifying'}
				<p class="waiting" data-testid="verifying">
					<span class="spinner" aria-hidden="true"></span>
					{$t('networks.verifying')}
				</p>
			{:else if view.kind === 'complete'}
				<div class="waiting stack" data-testid="login-complete">
					<p class="success">
						<Icon name="ok" />
						{$t(copy.success)}
					</p>
					<a class="button button--primary" href="/networks">{$t('networks.continue')}</a>
				</div>
			{:else if view.kind === 'cancelled'}
				<div class="waiting stack" data-testid="login-cancelled">
					<p>{$t('networks.cancelled')}</p>
					<button class="button button--primary" type="button" onclick={restart} data-testid="restart">
						<Icon name="reload" size="dense" />
						{$t('networks.refresh')}
					</button>
				</div>
			{:else if view.kind === 'failed'}
				<!-- The wireframes' *Failure* state, with a specific reason. The
				     Gateway's `error.code` is the stable contract; its `detail`
				     is an operator's sentence and is never shown. -->
				<div
					class={view.code === 'webauthn_required' ? 'waiting stack escalated' : 'waiting stack'}
					data-testid="login-failed"
					data-error-code={view.code}
					role="alert"
				>
					<p class="failure">
						<Icon name="error" />
						{#if view.code === 'login_lost'}
							{$t('networks.failed.loginLost')}
						{:else if view.code === 'login_expired'}
							{$t('networks.failed.expired')}
						{:else if view.code === 'webauthn_required'}
							{$t('networks.failed.webauthn')}
						{:else if view.code === 'unsupported_step'}
							{$t('networks.failed.unsupportedStep')}
						{:else}
							{$t('networks.failed.bridge')}
						{/if}
					</p>
					<button class="button button--primary" type="button" onclick={restart} data-testid="retry">
						<Icon name="reload" size="dense" />
						{$t('networks.retry')}
					</button>
				</div>
			{/if}
		</div>

		{#if view.kind === 'qr' || view.kind === 'verifying' || view.kind === 'starting'}
			<ol class="steps">
				{#each copy.steps as step (step)}
					<li>{$t(step)}</li>
				{/each}
			</ol>

			{#if copy.note !== null}
				<p class="card card--info small">{$t(copy.note)}</p>
			{/if}

			<p>
				<button class="button button--secondary" type="button" onclick={cancel} data-testid="cancel-login">
					{$t('networks.cancel')}
				</button>
			</p>
		{/if}
	{/if}

	<details class="small" bind:open={troubleshootingOpen} data-testid="troubleshooting">
		<summary>{$t('networks.troubleshooting')}</summary>
		<ul>
			{#each copy.troubleshooting as item (item)}
				<li>{$t(item)}</li>
			{/each}
		</ul>
	</details>
</section>

<style>
	.qr-slot {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		align-items: center;
		min-height: 17rem; /* the code's own height: no jump when it appears */
		justify-content: center;
	}

	.waiting {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--color-text-muted);
		text-align: center;
	}

	.waiting.stack {
		align-items: center;
	}

	.countdown {
		font-variant-numeric: tabular-nums;
	}

	.success {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--color-success);
		font-weight: var(--font-weight-label);
	}

	.failure {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--color-danger);
		font-weight: var(--font-weight-label);
	}

	/* A suspected ban is the one failure the wireframe escalates. */
	.escalated {
		border: 1px solid var(--color-danger);
		background: var(--color-danger-surface);
		border-radius: var(--radius-lg);
		padding: var(--space-3);
	}

	.steps {
		margin: 0;
		padding-inline-start: var(--space-4);
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		color: var(--color-text-muted);
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
