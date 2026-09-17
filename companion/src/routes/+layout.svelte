<!--
	The app shell: the boot sequence, the capability gate, and the frame every
	screen sits in.

	The order here is the order spec #65 asks for. Nothing of onboarding is
	rendered until the capability probe has answered, and a browser that cannot
	do the job gets the gate *instead of* the app rather than a screen that
	fails halfway through. `/diagnostics` is the one exception: it is where a
	user is sent to find out what is wrong, so it must render even when nothing
	else can.
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';

	import '@fontsource-variable/inter';
	import '@fontsource-variable/jetbrains-mono';
	import '$lib/styles/tokens.css';
	import '$lib/styles/base.css';

	import CapabilityGate from '$lib/components/CapabilityGate.svelte';
	import Icon from '$lib/icons/Icon.svelte';
	import TabElsewhere from '$lib/components/TabElsewhere.svelte';
	import VersionBanner from '$lib/components/VersionBanner.svelte';
	import { boot, startBoot } from '$lib/boot';
	import { locale, t } from '$lib/i18n';
	import type { TabRole } from '$lib/tabs/lock';

	let { children } = $props();

	/**
	 * Which tab this is (ADR 0014). `electing` until the Web Lock answers,
	 * which is one task, so no screen flashes on the way through.
	 */
	let tabRole = $state<TabRole>('electing');
	let takeOver = $state<() => void>(() => {});

	onMount(() => {
		void startBoot();

		// Browser-only, and dynamic for the same reason as the capability
		// probe: `navigator.locks` does not exist in the Node process that
		// prerenders this app.
		let stop: (() => void) | null = null;
		void (async () => {
			const [{ electTab }, { releaseClient }] = await Promise.all([
				import('$lib/tabs/lock'),
				import('$lib/crypto/bootstrap')
			]);
			const election = electTab({
				onRole: (role) => (tabRole = role),
				// Giving up the lock without giving up the client would leave
				// the store open in a tab that no longer owns it.
				onYield: () => releaseClient()
			});
			takeOver = election.takeOver;
			stop = election.stop;
		})();

		return () => stop?.();
	});

	const bootState = $derived($boot);

	// The document's language follows the negotiated locale. Set imperatively
	// because there is no server render to put it in the markup, and only in
	// an effect, which runs in the browser alone.
	$effect(() => {
		document.documentElement.lang = $locale;
	});

	/** Diagnostics is reachable whatever the browser can do. */
	const alwaysAllowed = $derived(page.url.pathname.startsWith('/diagnostics'));

	const blocked = $derived(bootState.capabilities !== null && !bootState.capabilities.ok && !alwaysAllowed);
	const degraded = $derived(
		bootState.capabilities !== null && bootState.capabilities.ok && bootState.capabilities.degraded.length > 0
	);
</script>

<svelte:head>
	<title>{$t('app.name')}</title>
	<meta name="description" content={$t('app.tagline')} />
	<!-- The manifest, the icons and the viewport are in `src/app.html`: they
	     belong to the document, not to a component, and a browser deciding
	     whether the app is installable looks for them in the shell. -->
</svelte:head>

<a class="skip" href="#main">{$t('app.skipToContent')}</a>

<VersionBanner handshake={bootState.handshake} reloadRefused={bootState.reloadRefused} />

<main id="main" data-testid="app" data-locale={$locale} data-tab-role={tabRole}>
	{#if bootState.capabilities === null}
		<p class="booting" data-testid="booting">
			<span class="spinner" aria-hidden="true"></span>
			{$t('gate.checking')}
		</p>
	{:else if blocked && bootState.capabilities !== null}
		<CapabilityGate report={bootState.capabilities} />
	{:else if tabRole === 'elsewhere' && !alwaysAllowed}
		<TabElsewhere onTakeOver={takeOver} />
	{:else}
		{#if degraded}
			<p class="degraded" data-testid="capability-degraded" role="status">
				<Icon name="warning" size="dense" />
				<span class="small">{$t('gate.degraded.intro')}</span>
				<a class="small" href="/diagnostics">{$t('gate.diagnostics')}</a>
			</p>
		{/if}
		{@render children()}
	{/if}
</main>

<style>
	.skip {
		position: absolute;
		left: var(--space-2);
		top: calc(-1 * var(--space-7));
		z-index: 1;
		background: var(--color-surface);
		color: var(--color-text);
		padding: var(--space-2) var(--space-3);
		border-radius: var(--radius-md);
		transition: top var(--duration-state) var(--easing);
	}

	.skip:focus {
		top: var(--space-2);
	}

	.booting {
		display: flex;
		align-items: center;
		gap: var(--space-3);
		justify-content: center;
		color: var(--color-text-muted);
		padding: var(--space-7) var(--layout-gutter);
	}

	.degraded {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		flex-wrap: wrap;
		width: 100%;
		max-width: var(--layout-width);
		margin: var(--space-3) auto 0;
		padding: var(--space-2) var(--space-3);
		background: var(--color-warning-surface);
		border: 1px solid var(--color-warning);
		border-radius: var(--radius-md);
	}
</style>
