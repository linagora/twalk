<!--
	An emoji to match on the phone: the same blocking step a QR code arrives in,
	with a different payload type. There is nothing to submit — only to confirm
	on the device — so this panel has no form and no button but the abandon.

	It draws for any network whose bridge asks this way, which is the point of
	the renderer: before ADR 0030 this panel existed only inside the SMS screen,
	and an emoji step reaching a QR screen drew an empty box.
-->
<script lang="ts">
	import { t } from '$lib/i18n';
	import type { PanelProps, ViewOf } from '$lib/networks/login-panels';
	import StepAside from './StepAside.svelte';

	let { view, copy, flow }: PanelProps<ViewOf<'emoji'>> = $props();
</script>

<div class="slot" data-testid="sms-emoji">
	<p class="emoji" aria-hidden="true">{view.emoji}</p>
	<p class="visually-hidden">{$t('networks.emoji.alt', { emoji: view.emoji })}</p>
	<p>{view.instructions ?? $t('networks.emoji.body')}</p>
	<p class="muted">
		<span class="spinner" aria-hidden="true"></span>
		{$t('networks.emoji.waiting')}
	</p>
</div>

<StepAside {copy} cancel={flow.cancel} />

<style>
	.slot {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		align-items: center;
		text-align: center;
		min-height: 17rem;
		justify-content: center;
	}

	.emoji {
		font-size: 4rem;
		line-height: 1;
	}

	.muted {
		color: var(--color-text-muted);
	}
</style>
