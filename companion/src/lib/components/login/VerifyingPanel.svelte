<!--
	A blocking step with nothing to draw: the code was scanned, or the paste was
	sent, and the network is deciding. The wireframes' "Verifying your session…".
-->
<script lang="ts">
	import { t } from '$lib/i18n';
	import type { PanelProps, ViewOf } from '$lib/networks/login-panels';
	import StepAside from './StepAside.svelte';

	let { view, copy, flow }: PanelProps<ViewOf<'verifying'>> = $props();
</script>

<div class="slot">
	<p class="waiting" data-testid="verifying">
		<span class="spinner" aria-hidden="true"></span>
		{$t('networks.verifying')}
	</p>
	{#if view.instructions !== null && view.instructions !== ''}
		<p class="small muted">{view.instructions}</p>
	{/if}
</div>

<StepAside {copy} cancel={flow.cancel} />

<style>
	.slot {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		align-items: center;
		justify-content: center;
		min-height: 17rem;
	}

	.waiting {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--color-text-muted);
	}
</style>
