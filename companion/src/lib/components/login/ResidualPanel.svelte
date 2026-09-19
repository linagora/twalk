<!--
	The residual panel: a step type this build has no panel for.

	The dispatch table makes a kind nobody draws a compile error, and that
	guarantee stops at the network boundary — a Gateway newer than this app
	answers a step type the enum did not have when this app was built. No type
	can prevent that, so the honest thing is a panel that says which half of the
	deployment is behind, names the step, and offers the two actions that can
	work (ADR 0030).

	The bridge's own instructions are shown when it gave any: they may be all
	the user needs to finish in the network's app.
-->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import type { PanelProps, ViewOf } from '$lib/networks/login-panels';

	let { view, flow }: PanelProps<ViewOf<'unknown_step'>> = $props();
</script>

<div
	class="card card--warning stack"
	data-testid="unknown-step"
	data-step-type={view.stepType}
	role="alert"
>
	<p class="card__title">
		<Icon name="warning" size="dense" />
		{$t('networks.unknownStep.title')}
	</p>
	<p>{$t('networks.unknownStep.body', { type: view.stepType })}</p>
	{#if view.instructions !== null && view.instructions !== ''}
		<div class="reported">
			<p class="small muted">{$t('networks.unknownStep.bridgeSays')}</p>
			<p class="small">{view.instructions}</p>
		</div>
	{/if}
	<p class="small muted mono">{view.stepId}</p>
	<p>
		<button class="button button--secondary" type="button" onclick={flow.cancel} data-testid="cancel-login">
			{$t('networks.cancel')}
		</button>
	</p>
</div>

<style>
	.reported {
		border-inline-start: 2px solid var(--color-border);
		padding-inline-start: var(--space-3);
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}
</style>
