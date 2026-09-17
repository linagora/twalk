<!--
	What a second tab of the Companion shows instead of the app.

	Not a nicety: matrix-js-sdk's own warning is that two `MatrixClient`
	instances on one IndexedDB "will cause data corruption and decryption
	failures", and ADR 0014 answers it with a Web Lock. The tab holding the
	lock runs the app; this is every other tab.

	The take-over button is what keeps it from being a dead end — a user with a
	forgotten tab on another desktop must be able to carry on here. See
	`$lib/tabs/lock.ts` for how the hand-over works.
-->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';

	interface Props {
		onTakeOver: () => void;
	}

	let { onTakeOver }: Props = $props();
	let asking = $state(false);
</script>

<section class="screen" data-testid="screen-tab-elsewhere">
	<header class="stack">
		<h1>
			<Icon name="tab-lock" />
			{$t('tabs.title')}
		</h1>
		<p class="subtitle">{$t('tabs.body')}</p>
	</header>
	<button
		class="button button--primary"
		data-testid="take-over"
		disabled={asking}
		onclick={() => {
			asking = true;
			onTakeOver();
		}}
	>
		{#if asking}
			<span class="spinner" aria-hidden="true"></span>
			{$t('tabs.waiting')}
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
