<!--
	What the version handshake has to say, when it has anything to say at all.

	A matching handshake draws nothing: the common case is silence. A mismatch
	normally reloads the page without asking (see `$lib/version/reload.ts`), so
	this banner only appears for the two cases a reload cannot fix — the
	reload was already tried for this Gateway version, or `/health` never
	answered.
-->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import type { Handshake } from '$lib/version/handshake';

	interface Props {
		handshake: Handshake;
		/** A reload has already been tried for this Gateway version. */
		reloadRefused: boolean;
	}

	let { handshake, reloadRefused }: Props = $props();

	const show = $derived(
		((handshake.kind === 'mismatch' || handshake.kind === 'stale-shell') && reloadRefused) ||
			handshake.kind === 'unreachable'
	);
</script>

{#if show}
	<div class="banner" role="status" data-testid="version-banner" data-kind={handshake.kind}>
		<Icon name="attention" size="dense" />
		<div class="banner__text">
			{#if handshake.kind === 'mismatch'}
				<span class="label">{$t('version.mismatch.title')}</span>
				<span class="small"
					>{$t('version.mismatch.body', {
						expected: handshake.expected,
						actual: handshake.actual
					})}</span
				>
			{:else if handshake.kind === 'stale-shell'}
				<span class="label">{$t('version.staleShell.title')}</span>
				<span class="small" data-testid="stale-shell-builds" data-running={handshake.running} data-shipped={handshake.shipped}
					>{$t('version.staleShell.body', {
						running: handshake.running,
						shipped: handshake.shipped
					})}</span
				>
			{:else if handshake.kind === 'unreachable'}
				<span class="small">{$t('version.unreachable')}</span>
			{/if}
		</div>
		{#if handshake.kind === 'mismatch' || handshake.kind === 'stale-shell'}
			<button class="button button--secondary" onclick={() => window.location.reload()}>
				<Icon name="reload" size="dense" />
				{$t('version.reload')}
			</button>
		{/if}
	</div>
{/if}

<style>
	.banner {
		display: flex;
		align-items: center;
		gap: var(--space-3);
		flex-wrap: wrap;
		padding: var(--space-2) var(--layout-gutter);
		background: var(--color-warning-surface);
		border-bottom: 1px solid var(--color-warning);
		color: var(--color-text);
	}

	.banner__text {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		flex: 1 1 16rem;
		min-width: 0;
	}
</style>
