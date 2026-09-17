<!--
	Screen 1 of `docs/wireframes/companion-v0.1.md`: welcome, what Twalk is,
	and the user's Twalk domain.

	The wireframe's states are all here — empty (CTA disabled), valid (CTA
	enabled), invalid (red micro-copy under the field, announced through an
	`aria-live` region), and loading ("Checking your Twalk deployment…", input
	disabled, CTA replaced by a spinner).

	What the loading state does *not* do yet is reach the homeserver: probing
	the deployment and registering an account is the bootstrap journey's work
	(ticket #67). Screen 1 validates the domain, remembers it, and hands over.
-->
<script lang="ts">
	import { goto } from '$app/navigation';

	import Icon from '$lib/icons/Icon.svelte';
	import Logo from '$lib/components/Logo.svelte';
	import { t } from '$lib/i18n';
	import { domain, isValidDomain, normaliseDomain, rememberDomain } from '$lib/onboarding/domain';

	let typed = $state('');
	/** Errors appear after the field has been left, not while typing into it. */
	let touched = $state(false);
	let submitting = $state(false);

	const normalised = $derived(normaliseDomain(typed));
	const valid = $derived(isValidDomain(typed));
	const showError = $derived(touched && typed.trim().length > 0 && !valid);

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		touched = true;
		if (!valid || submitting) {
			return;
		}
		submitting = true;
		domain.set(normalised);
		rememberDomain(normalised);
		await goto('/onboarding');
		submitting = false;
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
				aria-invalid={showError}
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
				{$t('screen1.continue')}
				<Icon name="continue" size="dense" />
			{/if}
		</button>
	</form>

	<!--
		The wireframe's second slot here is "I already have a Twalk account,
		pair with my other device", which belongs to the device-pairing
		journey and has nothing to navigate to yet. Until it does, the slot
		holds the link a user actually needs from a first screen that will
		not go further: what this browser and this deployment can do.
	-->
	<p class="small">
		<a href="/diagnostics">{$t('diagnostics.title')}</a>
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
</style>
