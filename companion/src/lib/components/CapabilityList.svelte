<!--
	One row per capability, with its state and a sentence saying what the
	Companion needs it for. Shared by the gate screen and `/diagnostics`, so
	the two never disagree about what is missing.
-->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import { t, type MessageKey } from '$lib/i18n';
	import type { IconName } from '$lib/icons';
	import type { CapabilityId, CapabilityRow } from '$lib/capabilities/report';

	interface Props {
		rows: CapabilityRow[];
	}

	let { rows }: Props = $props();

	const iconFor: Record<CapabilityId, IconName> = {
		'secure-context': 'secure-context',
		webassembly: 'webassembly',
		'crypto-subtle': 'recovery-key',
		storage: 'storage',
		'service-worker': 'service-worker',
		'tab-lock': 'tab-lock'
	};

	function stateKey(row: CapabilityRow): MessageKey {
		return `gate.state.${row.state === 'available' ? 'present' : row.state}` as MessageKey;
	}
</script>

<ul class="capabilities" data-testid="capability-list">
	{#each rows as row (row.id)}
		<li class="capability" data-capability={row.id} data-state={row.state}>
			<Icon name={iconFor[row.id]} />
			<span class="capability__text">
				<span class="label">{$t(`gate.capability.${row.id}` as MessageKey)}</span>
				<span class="small muted">{$t(`gate.capability.${row.id}.why` as MessageKey)}</span>
			</span>
			<span class="capability__state small">
				<Icon
					name={row.state === 'available' ? 'ok' : row.required ? 'error' : 'warning'}
					size="dense"
				/>
				{$t(stateKey(row))}
			</span>
		</li>
	{/each}
</ul>

<style>
	.capabilities {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.capability {
		display: flex;
		align-items: flex-start;
		gap: var(--space-3);
		padding: var(--space-3);
		background: var(--color-surface);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
	}

	.capability[data-state='missing'],
	.capability[data-state='blocked'] {
		border-color: var(--color-border-strong);
	}

	.capability__text {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		flex: 1 1 auto;
		min-width: 0;
	}

	.capability__state {
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
		flex: none;
		color: var(--color-text-muted);
	}

	.capability[data-state='available'] .capability__state {
		color: var(--color-success);
	}

	.capability[data-state='missing'] .capability__state,
	.capability[data-state='blocked'] .capability__state {
		color: var(--color-danger);
	}
</style>
