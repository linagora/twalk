<!--
	The network would not accept the answer.

	Its own outcome, not a banner over the step, because the bridge **destroys
	the login process** when it refuses a value: every later call against it
	answers `404`, the cancels included. So there is nothing to correct, the step
	is gone from the screen rather than left inviting a resubmit, and the only
	way on is a fresh login (ADR 0030).

	Three sentences, in this order: what happened, in the user's language; this
	step's own likely causes where the screen has any (#57's three for a Google
	session); and the Gateway's own words, which name the network's code. The
	last is read, never branched on — the codes are the contract and this is the
	one place a `detail` is shown because the network's reason lives nowhere
	else.
-->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import type { PanelProps, ViewOf } from '$lib/networks/login-panels';
	import { overrideFor } from '$lib/networks/step-copy';

	let { view, copy, flow }: PanelProps<ViewOf<'refused'>> = $props();

	/** This step's own failure copy, where the screen wrote any. */
	const explained = $derived(overrideFor(copy.stepCopy, view.stepId)?.refused ?? null);
	const detail = $derived(view.detail === null || view.detail === '' ? null : view.detail);
</script>

<div class="card card--warning stack" data-testid="login-refused" data-step-id={view.stepId} role="alert">
	<p class="card__title">
		<Icon name="warning" size="dense" />
		{$t('networks.refused.title')}
	</p>
	<p>{$t('networks.refused.body')}</p>
	{#if explained !== null}
		<p>{$t(explained)}</p>
	{/if}
	{#if detail !== null}
		<div class="reported">
			<p class="small muted">{$t('networks.refused.reported')}</p>
			<p class="small" data-testid="refused-detail">{detail}</p>
		</div>
	{/if}
	<p>
		<button class="button button--primary" type="button" onclick={flow.restart} data-testid="restart">
			<Icon name="reload" size="dense" />
			{$t('networks.refused.startAgain')}
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
