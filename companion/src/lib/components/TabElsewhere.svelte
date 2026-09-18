<!--
	What a second tab of the Companion shows instead of the app.

	Not a nicety: matrix-js-sdk's own warning is that two `MatrixClient`
	instances on one IndexedDB "will cause data corruption and decryption
	failures", and ADR 0014 answers it with a Web Lock. The tab holding the
	lock runs the app; this is every other tab.

	The take-over button is what keeps it from being a dead end — a user with a
	forgotten tab on another desktop must be able to carry on here. See
	`$lib/tabs/lock.ts` for how the hand-over works.

	# The dead end it was

	It was a dead end whenever the holder could not answer (#135): the button
	posted its request, set `asking`, and spun for ever. Found live, on an SSO
	round trip, with no other Twalk tab visible anywhere.

	The measurements say what is really happening, and it is not "nobody is
	there": a Web Lock dies with its tab, so a lock still held is a tab still
	alive. It is a tab the browser has **frozen** — Chrome's Memory Saver keeps
	a background tab's document, and its lock, while running none of its
	JavaScript. It cannot hear the request.

	So this screen has a third state, and what it owes the user there is the
	remedy rather than the diagnosis: find that tab and close it — switching to
	it wakes it, which is why asking again then works — or restart the browser.
	A spinner offers neither.
-->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import { TAKE_OVER_TIMEOUT_MS, type TakeOverOutcome } from '$lib/tabs/lock';

	interface Props {
		/** Asks the holder to let go. Always answers — see `$lib/tabs/lock.ts`. */
		onTakeOver: () => Promise<TakeOverOutcome>;
	}

	let { onTakeOver }: Props = $props();

	/** `asking` while the request is out; `unanswered` once it has not come back. */
	let state = $state<'idle' | 'asking' | 'unanswered'>('idle');

	const seconds = Math.round(TAKE_OVER_TIMEOUT_MS / 1000);

	async function ask() {
		state = 'asking';
		const outcome = await onTakeOver();
		// Becoming active unmounts this screen, so the only outcome that has
		// anything to render here is the one that used to render nothing.
		state = outcome === 'active' ? 'idle' : 'unanswered';
	}
</script>

<section class="screen" data-testid="screen-tab-elsewhere" data-state={state}>
	<header class="stack">
		<h1>
			<Icon name="tab-lock" />
			{$t('tabs.title')}
		</h1>
		<p class="subtitle">{$t('tabs.body')}</p>
	</header>

	{#if state === 'unanswered'}
		<!-- The terminal state. `role="alert"` because it replaces a spinner
		     the user is watching, and the remedy is on the screen rather than
		     implied by a disabled button. -->
		<div class="card card--warning" role="alert" data-testid="take-over-unanswered">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('tabs.unanswered.title')}
			</p>
			<p class="small">{$t('tabs.unanswered.body', { seconds })}</p>
			<p class="small" data-testid="take-over-remedy">{$t('tabs.unanswered.remedy')}</p>
		</div>
	{/if}

	<button
		class="button button--primary"
		data-testid="take-over"
		disabled={state === 'asking'}
		onclick={ask}
	>
		{#if state === 'asking'}
			<span class="spinner" aria-hidden="true"></span>
			{$t('tabs.waiting')}
		{:else if state === 'unanswered'}
			<Icon name="reload" size="dense" />
			{$t('tabs.unanswered.retry')}
		{:else}
			{$t('tabs.takeOver')}
		{/if}
	</button>
</section>

<style>
	h1 {
		display: flex;
		align-items: center;
		gap: var(--space-2);
	}
</style>
