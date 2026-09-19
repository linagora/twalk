<!--
	What sits beside a step the user cannot answer yet — a code to scan, an
	emoji to match, a network deciding: the screen's own numbered steps, its
	calm note, and the explicit abandon.

	Leaving the page does not cancel a login (ADR 0030); this button does, which
	is why it says so rather than saying "back".
-->
<script lang="ts">
	import { t } from '$lib/i18n';
	import type { LoginScreenCopy } from '$lib/networks/copy';

	interface Props {
		copy: LoginScreenCopy;
		cancel: () => Promise<void>;
	}

	let { copy, cancel }: Props = $props();
</script>

{#if copy.steps.length > 0}
	<ol class="steps">
		{#each copy.steps as step (step)}
			<li>{$t(step)}</li>
		{/each}
	</ol>
{/if}

{#if copy.note !== null}
	<p class="card card--info small">{$t(copy.note)}</p>
{/if}

<p>
	<button class="button button--secondary" type="button" onclick={cancel} data-testid="cancel-login">
		{$t('networks.cancel')}
	</button>
</p>

<style>
	.steps {
		margin: 0;
		padding-inline-start: var(--space-4);
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		color: var(--color-text-muted);
	}
</style>
