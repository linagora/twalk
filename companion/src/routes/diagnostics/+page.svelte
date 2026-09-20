<!--
	What the Companion has instead of telemetry.

	Spec #65: no telemetry, no third-party error collector. So the user gets a
	page they can read and a button that copies it, and nothing is transmitted
	anywhere. The text is built by `$lib/diagnostics.ts`, which is unit-tested
	on what it does and does not include.

	This page renders even when the capability gate is blocking the rest of the
	app (see the root layout): it is where the gate sends the user.
-->
<script lang="ts">
	import { version as appBuild } from '$app/environment';

	import CapabilityList from '$lib/components/CapabilityList.svelte';
	import Icon from '$lib/icons/Icon.svelte';
	import { boot, EXPECTED_GATEWAY_VERSION } from '$lib/boot';
	import { buildDiagnostics, copyDiagnostics } from '$lib/diagnostics';
	import { locale, setLocale, t, LOCALES, type Locale } from '$lib/i18n';

	const bootState = $derived($boot);

	let copied = $state<'idle' | 'done' | 'failed'>('idle');

	const installed = $derived(
		typeof window !== 'undefined' &&
			typeof window.matchMedia === 'function' &&
			window.matchMedia('(display-mode: standalone)').matches
	);

	const text = $derived(
		buildDiagnostics({
			appBuild,
			expectedGatewayVersion: EXPECTED_GATEWAY_VERSION,
			health: bootState.health,
			handshake: bootState.handshake,
			capabilities: bootState.capabilities,
			locale: $locale,
			userAgent: typeof navigator === 'undefined' ? '' : navigator.userAgent,
			installed,
			generatedAt: new Date()
		})
	);

	async function copy() {
		copied = (await copyDiagnostics(text)) ? 'done' : 'failed';
	}
</script>

<section class="screen" data-testid="screen-diagnostics">
	<header class="stack">
		<p class="small"><a href="/"><Icon name="back" size="dense" />{$t('diagnostics.back')}</a></p>
		<h1>{$t('diagnostics.title')}</h1>
		<p class="subtitle">{$t('diagnostics.intro')}</p>
	</header>

	<div class="card">
		<h2 class="card__title">{$t('diagnostics.section.versions')}</h2>
		<dl class="pairs">
			<dt>{$t('diagnostics.app')}</dt>
			<dd class="mono" data-testid="app-build">{appBuild}</dd>
			<dt>{$t('diagnostics.expectedGateway')}</dt>
			<dd class="mono" data-testid="expected-gateway-version">{EXPECTED_GATEWAY_VERSION}</dd>
			<dt>{$t('diagnostics.gateway')}</dt>
			<dd class="mono" data-testid="gateway-version">
				{bootState.health?.version ?? $t('diagnostics.unknown')}
			</dd>
			<dt>{$t('diagnostics.gatewayRevision')}</dt>
			<dd class="mono" data-testid="gateway-revision">
				{bootState.health?.revision ?? $t('diagnostics.unknown')}
			</dd>
			<!-- Beside the build this page runs (#222): the two agreeing is what
			     "current" means, and the two differing is the fact that was
			     invisible while an owner reloaded a fixed screen five times. -->
			<dt>{$t('diagnostics.shippedBuild')}</dt>
			<dd class="mono" data-testid="shipped-build">
				{bootState.health?.companionBuild ?? $t('diagnostics.unknown')}
			</dd>
		</dl>
		<p class="small muted" data-testid="handshake-kind" data-kind={bootState.handshake.kind}>
			{bootState.handshake.kind}
		</p>
	</div>

	<h2>{$t('diagnostics.section.capabilities')}</h2>
	{#if bootState.capabilities === null}
		<p class="muted">{$t('gate.checking')}</p>
	{:else}
		<p class="small muted" data-testid="capability-cause-id" data-cause={bootState.capabilities.cause}>
			{bootState.capabilities.cause}
		</p>
		<CapabilityList rows={bootState.capabilities.rows} />
	{/if}

	<div class="card">
		<h2 class="card__title">{$t('diagnostics.section.environment')}</h2>
		<dl class="pairs">
			<dt>{$t('diagnostics.locale')}</dt>
			<dd class="mono">{$locale}</dd>
			<dt>{$t('diagnostics.installed')}</dt>
			<dd class="mono">{installed ? $t('diagnostics.yes') : $t('diagnostics.no')}</dd>
		</dl>
		<fieldset class="languages">
			<legend class="label">{$t('lang.switch')}</legend>
			{#each LOCALES as option (option)}
				<label class="small">
					<input
						type="radio"
						name="locale"
						value={option}
						checked={$locale === option}
						onchange={() => setLocale(option as Locale)}
					/>
					{$t(`lang.${option}` as 'lang.fr' | 'lang.en')}
				</label>
			{/each}
		</fieldset>
	</div>

	<button class="button button--primary" onclick={copy} data-testid="copy-diagnostics">
		<Icon name={copied === 'done' ? 'copied' : 'copy'} size="dense" />
		{copied === 'done' ? $t('diagnostics.copied') : $t('diagnostics.copy')}
	</button>

	{#if copied === 'failed'}
		<p class="error-text" role="alert">{$t('diagnostics.copyFailed')}</p>
	{/if}

	<pre class="report" data-testid="diagnostics-text">{text}</pre>
</section>

<style>
	.pairs {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
		gap: var(--space-1) var(--space-3);
		margin: 0;
		font-size: var(--text-sm);
	}

	.pairs dt {
		color: var(--color-text-muted);
	}

	.pairs dd {
		margin: 0;
		overflow-wrap: anywhere;
	}

	.languages {
		display: flex;
		gap: var(--space-3);
		align-items: center;
		border: 0;
		margin: 0;
		padding: 0;
	}

	.report {
		margin: 0;
		padding: var(--space-3);
		background: var(--color-surface);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		font-size: var(--text-xs);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		user-select: all;
	}
</style>
