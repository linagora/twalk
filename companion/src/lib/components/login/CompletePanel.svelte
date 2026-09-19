<!-- The network is connected. -->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import type { PanelProps, ViewOf } from '$lib/networks/login-panels';

	let { copy }: PanelProps<ViewOf<'complete'>> = $props();
</script>

<div class="stack done" data-testid="login-complete">
	<p class="success">
		<Icon name="ok" />
		{$t(copy.success)}
	</p>
	{#if copy.successNote !== null}
		<p>{$t(copy.successNote)}</p>
	{/if}
	<p>
		<a class="button button--primary" href="/networks">{$t('networks.continue')}</a>
	</p>
</div>

<style>
	.done {
		align-items: flex-start;
	}

	.success {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--color-success);
		font-weight: var(--font-weight-label);
	}
</style>
