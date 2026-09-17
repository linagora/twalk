<!--
	Screen 1 of `docs/wireframes/companion-v0.1.md`: welcome, what Twalk is,
	and the user's Twalk domain.

	The wireframe's states are all here — empty (CTA disabled), valid (CTA
	enabled), invalid (red micro-copy under the field, announced through an
	`aria-live` region), loading ("Checking your Twalk deployment…", input
	disabled, CTA replaced by a spinner) and *homeserver unreachable* (the
	banner, the red field, a retry).

	The loading state is a real probe now (ticket #67): the Gateway at this
	origin is asked whether it accepts anybody, and the domain is resolved to a
	homeserver through Matrix's own discovery. See
	`$lib/onboarding/deployment.ts` for why those two questions, in that order.

	This screen is also where a *returning* user is recognised, which is what
	makes store loss a journey rather than an error: a browser holding a
	Gateway session whose crypto store is gone is sent to `/recover` before it
	is ever asked to type a domain.
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';

	import Icon from '$lib/icons/Icon.svelte';
	import Logo from '$lib/components/Logo.svelte';
	import { t } from '$lib/i18n';
	import { domain, isValidDomain, normaliseDomain, rememberDomain } from '$lib/onboarding/domain';
	import { probeDeployment, type DeploymentOutcome } from '$lib/onboarding/deployment';
	import { rememberHomeserver } from '$lib/onboarding/progress';
	import { gateway } from '$lib/api/client';

	let typed = $state('');
	/** Errors appear after the field has been left, not while typing into it. */
	let touched = $state(false);
	let submitting = $state(false);
	let failure = $state<DeploymentOutcome | null>(null);
	let signedInAs = $state<string | null>(null);

	const normalised = $derived(normaliseDomain(typed));
	const valid = $derived(isValidDomain(typed));
	const showError = $derived(touched && typed.trim().length > 0 && !valid);

	onMount(() => {
		void resume();
	});

	/**
	 * What the Gateway already knows about this browser, asked before the
	 * user types anything. Progress is derived, never remembered (spec #65).
	 */
	async function resume(): Promise<void> {
		let owner: string | null = null;
		try {
			const result = await gateway.GET('/api/session');
			owner = result.data?.owner ?? null;
		} catch {
			// The Gateway is unreachable; the form still works and the probe
			// on "Continue" will say so properly.
			return;
		}
		if (owner === null) {
			return;
		}
		const { hasCryptoStore } = await import('$lib/crypto/store');
		if (!(await hasCryptoStore())) {
			// The designed path of ADR 0014: signed in, keys evicted.
			await goto('/recover');
			return;
		}
		signedInAs = owner;
	}

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		touched = true;
		if (!valid || submitting) {
			return;
		}
		submitting = true;
		failure = null;
		const outcome = await probeDeployment(normalised);
		submitting = false;

		if (outcome.kind === 'ready' || outcome.kind === 'signed-in') {
			domain.set(normalised);
			rememberDomain(normalised);
			rememberHomeserver(outcome.homeserver.baseUrl);
			await goto('/onboarding');
			return;
		}
		failure = outcome;
	}

	const banner = $derived(bannerFor(failure));

	function bannerFor(outcome: DeploymentOutcome | null): string | null {
		switch (outcome?.kind) {
			case 'gateway-unreachable':
				return $t('screen1.error.gateway');
			case 'not-configured':
				return $t('screen1.error.notConfigured');
			case 'homeserver-unreachable':
				return $t('screen1.error.homeserver', { domain: outcome.domain });
			case 'not-a-homeserver':
				return $t('screen1.error.notMatrix', { domain: outcome.domain });
			default:
				return null;
		}
	}
</script>

<section class="screen" data-testid="screen-welcome">
	<header class="stack">
		<h1 class="title">
			<!-- Decorative: the heading's own text names the app, and a
			     screen reader announcing "Twalk Companion" twice is worse
			     than announcing it once. -->
			<Logo size={32} />
			<span>{$t('app.name')}</span>
		</h1>
		<p class="subtitle" aria-live="polite">
			{submitting ? $t('screen1.checking') : $t('app.tagline')}
		</p>
	</header>

	{#if banner !== null}
		<div class="card card--warning" role="alert" data-testid="deployment-error">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('screen1.error.title')}
			</p>
			<p>{banner}</p>
		</div>
	{/if}

	{#if signedInAs !== null}
		<div class="card card--info" data-testid="already-signed-in">
			<p class="card__title">
				<Icon name="ok" size="dense" />
				{$t('screen1.signedIn.title')}
			</p>
			<p>{$t('screen1.signedIn.body', { owner: signedInAs })}</p>
			<p>
				<a class="button button--primary" href="/onboarding">
					{$t('screen1.signedIn.continue')}
					<Icon name="continue" size="dense" />
				</a>
			</p>
		</div>
	{:else}
		<p class="muted">{$t('screen1.description')}</p>

		<form class="field" onsubmit={submit} novalidate>
			<label class="label" for="domain">{$t('screen1.domain.label')}</label>
			<div class="input-row">
				<Icon name="domain" />
				<input
					id="domain"
					class="input"
					name="domain"
					type="text"
					inputmode="url"
					autocomplete="url"
					spellcheck="false"
					autocapitalize="none"
					placeholder={$t('screen1.domain.placeholder')}
					bind:value={typed}
					onblur={() => (touched = true)}
					disabled={submitting}
					aria-invalid={showError || failure !== null}
					aria-describedby="domain-error"
				/>
			</div>
			<p class="error-text" id="domain-error" role="alert" aria-live="polite">
				{#if showError}
					{$t('screen1.domain.invalid', { example: $t('screen1.domain.placeholder') })}
				{/if}
			</p>

			<button
				class="button button--primary"
				type="submit"
				disabled={!valid || submitting}
				data-testid="continue"
			>
				{#if submitting}
					<span class="spinner" aria-hidden="true"></span>
					<span class="visually-hidden">{$t('screen1.checking')}</span>
				{:else}
					{failure === null ? $t('screen1.continue') : $t('screen1.retry')}
					<Icon name="continue" size="dense" />
				{/if}
			</button>
		</form>
	{/if}

	<!--
		The wireframe's second slot here is "I already have a Twalk account,
		pair with my other device", which belongs to the device-pairing
		journey and has nothing to navigate to yet. Until it does, the slot
		holds the two links a user actually needs from this screen: what this
		browser and this deployment can do, and the way back in when the keys
		are gone from this device.
	-->
	<p class="small">
		<a href="/diagnostics">{$t('diagnostics.title')}</a>
		·
		<a href="/recover" data-testid="recover-link">{$t('screen2.error.exists.cta')}</a>
	</p>
</section>

<style>
	.title {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		/* One line at 390 px, which is the middle of the wireframes' 375–428
		   mobile range. */
		font-size: var(--text-xl);
	}

	.input-row {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--color-text-subtle);
	}

	.input-row .input {
		flex: 1 1 auto;
		min-width: 0;
	}

	a.button {
		text-decoration: none;
	}
</style>
