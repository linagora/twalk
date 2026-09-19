<!--
	The wireframes' *Failure* state, with a specific reason.

	The Gateway's `error.code` is the stable contract; its `detail` is an
	operator's sentence and is never shown here. A screen may replace the
	generic "the bridge refused this login" with its own words, which is how the
	SMS path says the three things that actually go wrong with a Google session.
-->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import type { PanelProps, ViewOf } from '$lib/networks/login-panels';

	let { view, copy, flow }: PanelProps<ViewOf<'failed'>> = $props();

	/** A suspected ban is the one failure the wireframe escalates. */
	const escalated = $derived(view.code === 'webauthn_required');
</script>

<div
	class={escalated ? 'stack escalated' : 'stack'}
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
			{$t(copy.failure ?? 'networks.failed.bridge')}
		{/if}
	</p>
	<p>
		<button class="button button--primary" type="button" onclick={flow.restart} data-testid="retry">
			<Icon name="reload" size="dense" />
			{$t('networks.retry')}
		</button>
	</p>
</div>

<style>
	.failure {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--color-danger);
		font-weight: var(--font-weight-label);
	}

	.escalated {
		border: 1px solid var(--color-danger);
		background: var(--color-danger-surface);
		border-radius: var(--radius-lg);
		padding: var(--space-3);
	}
</style>
