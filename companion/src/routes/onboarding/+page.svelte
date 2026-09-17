<!--
	Where screen 1 hands over. Screen 2 — account creation, cross-signing and
	the recovery key — is the bootstrap journey's (ticket #67), and this page
	is the seam it will replace: it confirms the domain the user chose and says
	plainly what is not built yet, rather than dead-ending on a 404.

	It is also a second *prerendered* route, which is not incidental: it is how
	the Playwright harness checks the Gateway's `.html` resolution (step 2 of
	`companion-gateway/src/static_files.rs`) against a real build.
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import { domain, restoreDomain } from '$lib/onboarding/domain';

	onMount(restoreDomain);
</script>

<section class="screen" data-testid="screen-onboarding">
	<header class="stack">
		<p class="small">
			<a href="/">
				<Icon name="back" size="dense" />
				{$t('onboarding.back')}
			</a>
		</p>
		<h1>{$t('onboarding.title')}</h1>
		{#if $domain !== ''}
			<p class="subtitle" data-testid="chosen-domain">
				{$t('onboarding.domain', { domain: $domain })}
			</p>
		{/if}
	</header>

	<div class="card card--info">
		<p class="card__title">
			<Icon name="info" size="dense" />
			{$t('onboarding.pending.title')}
		</p>
		<p>{$t('onboarding.pending.body')}</p>
	</div>

	<p class="small">
		<a href="/diagnostics">{$t('onboarding.diagnostics')}</a>
	</p>
</section>
