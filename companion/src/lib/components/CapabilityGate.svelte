<!--
	The screen that stands in front of onboarding when the browser cannot do
	the job (spec #65, ADR 0014).

	It leads with the *cause* — "this looks like iOS Lockdown Mode, here is the
	setting" — and only then lists what is present and what is not. The list
	alone is what makes a user give up: "IndexedDB is missing" is true in
	Lockdown Mode and tells them nothing they can act on.

	Only the blocking case gets a screen. Missing installability or the
	two-tab lock costs a convenience, not the journey, and is reported as a
	notice in the shell and in full on `/diagnostics`.
-->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import { t, type MessageKey } from '$lib/i18n';
	import CapabilityList from './CapabilityList.svelte';
	import { recheckCapabilities } from '$lib/boot';
	import type { CapabilityReport } from '$lib/capabilities/report';

	interface Props {
		report: CapabilityReport;
	}

	let { report }: Props = $props();

	let rechecking = $state(false);

	async function recheck() {
		rechecking = true;
		try {
			await recheckCapabilities();
		} finally {
			rechecking = false;
		}
	}

	// The advice sentence takes the placeholders its language needs; passing
	// all of them keeps each catalogue free to use whichever it wants.
	const causeValues = $derived({
		https: 'https://',
		localhost: 'http://localhost',
		origin: typeof window === 'undefined' ? '' : window.location.origin
	});

	const causeKey = $derived(`gate.cause.${report.cause}` as MessageKey);
</script>

<section class="screen" data-testid="capability-gate" aria-live="polite">
	<header class="stack">
		<h1>{$t('gate.title')}</h1>
		<p class="subtitle">{$t('gate.intro', { count: report.missing.length })}</p>
	</header>

	{#if report.cause !== 'none'}
		<div class="card card--warning" data-testid="capability-cause" data-cause={report.cause}>
			<p class="cause">
				<Icon name="warning" />
				<span>{$t(causeKey, causeValues)}</span>
			</p>
		</div>
	{/if}

	<CapabilityList rows={report.rows} />

	<div class="actions">
		<button class="button button--primary" onclick={recheck} disabled={rechecking}>
			{#if rechecking}
				<span class="spinner" aria-hidden="true"></span>
			{:else}
				<Icon name="reload" size="dense" />
			{/if}
			{$t('gate.recheck')}
		</button>
		<a class="button button--secondary" href="/diagnostics">
			<Icon name="diagnostics" size="dense" />
			{$t('gate.diagnostics')}
		</a>
	</div>
</section>

<style>
	.cause {
		display: flex;
		align-items: flex-start;
		gap: var(--space-2);
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.actions > * {
		flex: 1 1 12rem;
		text-decoration: none;
	}
</style>
