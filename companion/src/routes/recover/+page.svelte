<!--
	The store-loss journey (ticket #67, ADR 0014): a returning user whose
	encryption keys are no longer in this browser.

	This is **not an error page**, and the copy is written as though the user
	did nothing wrong, because they did not: iOS deletes a Safari tab's
	IndexedDB after seven days without interaction unless the Companion is
	installed to the home screen, and "clear browsing data" does the same
	everywhere. ADR 0014 accepted that in exchange for a store no passphrase
	guards, and this screen is the other half of that bargain.

	The user gets here three ways: the app recognised the situation on screen 1
	(a live Gateway session with no crypto store), screen 2 met an account that
	already exists, or they followed the link themselves.

	What happens is a *new device joining an existing identity*: log in again,
	then open the account's secret storage with the recovery key and pull the
	cross-signing secrets and the key-backup key back out. Nothing is created
	and nothing is reset — resetting would break every other device and lose
	the message history, which is exactly what screen 2 promised would happen
	if the key were lost.
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import { gateway } from '$lib/api/client';
	import { domain, restoreDomain, homeserverBaseUrl } from '$lib/onboarding/domain';
	import { homeserver, restoreHomeserver, matrixSession } from '$lib/onboarding/progress';
	import { localpartOf, matrixIdFor } from '$lib/onboarding/account';
	import { decodeRecoveryKey, groupRecoveryKey, type RecoveryKeyProblem } from '$lib/recovery/key';

	type Stage = 'form' | 'working' | 'done';

	let stage = $state<Stage>('form');
	let step = $state<'signing-in' | 'loading-crypto' | 'unlocking'>('signing-in');

	/** Filled from the Gateway session when there is one: one less thing to type. */
	let owner = $state<string | null>(null);
	let username = $state('');
	let password = $state('');
	let typedKey = $state('');
	let touched = $state(false);

	let failure = $state<string | null>(null);
	let failureKind = $state<string | null>(null);
	let verified = $state(false);
	let keyBackup = $state(false);
	/** The two halves of "verified", reported so a failure names itself. */
	let detail = $state({ crossSigningReady: false, deviceSigned: false });

	onMount(() => {
		restoreDomain();
		restoreHomeserver();
		void (async () => {
			try {
				const result = await gateway.GET('/api/session');
				if (result.data !== undefined) {
					owner = result.data.owner;
					username = localpartOf(result.data.owner);
					if ($domain === '') {
						domain.set(result.data.homeserver);
					}
				}
			} catch {
				// No session: the user types their username like anyone else.
			}
		})();
	});

	const baseUrl = $derived($homeserver !== '' ? $homeserver : homeserverBaseUrl($domain));
	const decoded = $derived(decodeRecoveryKey(typedKey));
	const keyProblem = $derived<RecoveryKeyProblem | null>(decoded.ok ? null : decoded.problem);
	const ready = $derived(username.trim().length > 0 && password.length > 0 && decoded.ok);

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		touched = true;
		if (!ready || stage === 'working' || !decoded.ok) {
			return;
		}
		failure = null;
		failureKind = null;
		stage = 'working';
		step = 'signing-in';

		try {
			const { restoreFromRecoveryKey } = await import('$lib/crypto/bootstrap');
			const result = await restoreFromRecoveryKey({
				baseUrl,
				userId: owner ?? matrixIdFor(username, $domain),
				password,
				privateKey: decoded.bytes,
				onStep: (next) => {
					if (next !== 'done') {
						step = next;
					}
				}
			});
			verified = result.verified;
			keyBackup = result.keyBackup;
			detail = {
				crossSigningReady: result.crossSigningReady,
				deviceSigned: result.deviceSigned
			};
			matrixSession.set({
				baseUrl,
				userId: result.userId,
				deviceId: result.deviceId,
				accessToken: result.accessToken
			});

			// A device that has just come back needs a Gateway session too:
			// the cookie may have been evicted with the store.
			const { signInToGateway } = await import('$lib/session/signin');
			await signInToGateway({
				baseUrl,
				userId: result.userId,
				accessToken: result.accessToken
			}).catch(() => {
				// The keys are back either way; a Gateway sign-in that fails
				// is a separate problem, and screen 1 will say so.
			});

			stage = 'done';
		} catch (cause) {
			stage = 'form';
			const problem = (cause as { problem?: string }).problem;
			failureKind = problem ?? 'failed';
			failure = cause instanceof Error ? cause.message : String(cause);
		}
	}
</script>

<section class="screen" data-testid="screen-recover">
	{#if stage === 'done'}
		<header class="stack">
			<h1>{$t('recover.done.title')}</h1>
			<p
				class="subtitle"
				data-testid="recover-done"
				data-cross-signing-ready={detail.crossSigningReady}
				data-device-signed={detail.deviceSigned}
			>
				{verified ? $t('recover.done.body') : $t('recover.done.unverified')}
			</p>
		</header>
		{#if keyBackup}
			<p class="small muted" data-testid="recover-backup">
				<Icon name="ok" size="dense" />
				{$t('recover.done.backup')}
			</p>
		{/if}
		<p><a class="button button--primary" href="/">{$t('key.continue')}</a></p>
	{:else}
		<header class="stack">
			<h1>{$t('recover.title')}</h1>
			<p class="subtitle">{$t('recover.intro')}</p>
			<p>{$t('recover.fix')}</p>
			{#if owner !== null}
				<p class="small muted" data-testid="recover-account">
					{$t('recover.account', { id: owner })}
				</p>
			{/if}
		</header>

		<form class="stack" onsubmit={submit} novalidate>
			{#if owner === null}
				<div class="field">
					<label class="label" for="username">{$t('recover.userId.label')}</label>
					<input
						id="username"
						class="input"
						type="text"
						autocomplete="username"
						spellcheck="false"
						autocapitalize="none"
						bind:value={username}
						disabled={stage === 'working'}
					/>
					<p class="small muted">{$t('recover.userId.hint', { domain: $domain })}</p>
				</div>
			{/if}

			<div class="field">
				<label class="label" for="password">{$t('recover.password.label')}</label>
				<input
					id="password"
					class="input"
					type="password"
					autocomplete="current-password"
					bind:value={password}
					disabled={stage === 'working'}
				/>
			</div>

			<div class="field">
				<label class="label" for="recovery-key">{$t('recover.key.label')}</label>
				<input
					id="recovery-key"
					class="input mono"
					type="text"
					spellcheck="false"
					autocapitalize="none"
					autocomplete="off"
					bind:value={typedKey}
					onblur={() => {
						touched = true;
						// Regroup what was pasted, so the field reads like the
						// sheet the user is copying from.
						if (typedKey.trim() !== '') {
							typedKey = groupRecoveryKey(typedKey);
						}
					}}
					disabled={stage === 'working'}
					aria-invalid={touched && keyProblem !== null}
					aria-describedby="recovery-key-error"
					data-testid="recovery-key-input"
				/>
				<p class="small muted">{$t('recover.key.hint')}</p>
				<p class="error-text" id="recovery-key-error" role="alert" aria-live="polite">
					{#if touched && keyProblem !== null && keyProblem !== 'empty'}
						{$t(`recover.key.${keyProblem}` as 'recover.key.empty')}
					{/if}
				</p>
			</div>

			{#if failure !== null}
				<p class="error-text" role="alert" data-testid="recover-error" data-kind={failureKind}>
					{#if failureKind === 'wrong-password'}
						{$t('recover.error.wrong-password')}
					{:else if failureKind === 'wrong-recovery-key'}
						{$t('recover.error.wrong-recovery-key')}
					{:else if failureKind === 'no-secret-storage'}
						{$t('recover.error.no-secret-storage')}
					{:else}
						{$t('recover.error.failed', { detail: failure })}
					{/if}
				</p>
			{/if}

			<button
				class="button button--primary"
				type="submit"
				disabled={!ready || stage === 'working'}
				data-testid="recover-submit"
			>
				{#if stage === 'working'}
					<span class="spinner" aria-hidden="true"></span>
					<span data-testid="recover-step" data-step={step}>
						{$t(`recover.step.${step}` as 'recover.step.signing-in')}
					</span>
				{:else}
					{$t('recover.submit')}
					<Icon name="continue" size="dense" />
				{/if}
			</button>
		</form>

		<p class="small muted">
			<Icon name="phone" size="dense" />
			{$t('recover.install')}
		</p>
	{/if}
</section>

<style>
	a.button {
		text-decoration: none;
	}
</style>
