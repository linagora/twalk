<!--
	A code to scan, drawn in the browser from the raw payload the bridge handed
	over: a real bridge renders no image.

	It does not trust the countdown. `valid_for_seconds` is the Gateway's own
	estimate of the network's refresh interval, documented as such, and
	WhatsApp's whole budget is about 2m40 across refreshes. So the countdown is
	shown because the wireframe asks for it, and the *fact* the redraw keys off
	is the generation.

	It stores nothing: the payload lives in this component's props and in the SVG.
-->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import QrCode from '$lib/components/QrCode.svelte';
	import { t } from '$lib/i18n';
	import type { PanelProps, ViewOf } from '$lib/networks/login-panels';
	import { secondsLeft } from '$lib/networks/login-view';
	import StepAside from './StepAside.svelte';

	let { view, copy, flow, now }: PanelProps<ViewOf<'qr'>> = $props();

	const remaining = $derived(secondsLeft(view.expiresAt, now));
	/**
	 * The wireframes' *QR expired* state. Reached only when a refreshed
	 * generation did not arrive in time — the bridge stopped refreshing, or the
	 * Gateway lost the step — so it offers the button rather than a stale code.
	 */
	const expired = $derived(remaining === 0);
</script>

<div class="slot">
	{#if expired}
		<div class="waiting stack" data-testid="qr-expired">
			<p>{$t('networks.expired')}</p>
			<button class="button button--primary" type="button" onclick={flow.restart} data-testid="refresh-qr">
				<Icon name="reload" size="dense" />
				{$t('networks.refresh')}
			</button>
		</div>
	{:else}
		<!-- `#key` on the generation: a refreshed code is a bumped generation
		     with a new payload, and re-creating the element is what guarantees
		     the drawn code is the current one. -->
		{#key view.generation}
			<QrCode data={view.data} label={$t(copy.qrAlt ?? 'networks.qrAlt')} />
		{/key}
		<p class="small muted countdown" data-testid="countdown">
			{$t('networks.expiresIn', { seconds: remaining })}
		</p>
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
		text-align: center;
	}

	.countdown {
		font-variant-numeric: tabular-nums;
	}
</style>
