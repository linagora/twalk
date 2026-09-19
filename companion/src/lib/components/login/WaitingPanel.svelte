<!--
	Before the bridge has handed anything over: the login is being started.

	`idle` and `starting` are one panel because they are one thing to the user —
	nothing to act on yet — and telling them apart on screen would mean a state
	that says nothing at all for the first instant.
-->
<script lang="ts">
	import { t } from '$lib/i18n';
	import type { PanelProps, ViewOf } from '$lib/networks/login-panels';
	import StepAside from './StepAside.svelte';

	let { copy, flow }: PanelProps<ViewOf<'idle' | 'starting'>> = $props();
</script>

<div class="slot">
	<p class="waiting">
		<span class="spinner" aria-hidden="true"></span>
		{$t('networks.starting')}
	</p>
</div>

<StepAside {copy} cancel={flow.cancel} />

<style>
	.slot {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		align-items: center;
		justify-content: center;
		/* The code's own height: nothing jumps when it appears. */
		min-height: 17rem;
	}

	.waiting {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--color-text-muted);
		text-align: center;
	}
</style>
