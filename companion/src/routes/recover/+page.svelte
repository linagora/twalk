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
	import {
		domain,
		restoreDomain,
		rememberDomain,
		homeserverBaseUrl,
		isValidDomain,
		normaliseDomain
	} from '$lib/onboarding/domain';
	import { homeserver, restoreHomeserver, matrixSession } from '$lib/onboarding/progress';
	import { localpartOf } from '$lib/onboarding/account';
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
	/**
	 * The deployment, when this browser does not already know it (ticket #115).
	 *
	 * A recovery screen is reached by definition from a browser that has lost
	 * something, and often from one that never had anything: a new laptop, a
	 * reinstalled phone. Such a browser has no remembered domain — the store is
	 * per origin, so even the same laptop on a different tunnelled port is a
	 * stranger here — and no Gateway session to be told it by. It used to have
	 * nowhere to say *which* deployment either, so it resolved an empty
	 * homeserver URL and failed with a raw `Failed to fetch`.
	 *
	 * The Gateway cannot answer this for us. It knows its homeserver's **server
	 * name**, but not the address this browser reaches it at — through a tunnel
	 * those differ, and the port is exactly what the user must be able to say.
	 */
	let typedDomain = $state('');

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

	/** Asked for only when nothing else can say it. */
	const askDomain = $derived($domain === '');
	const effectiveDomain = $derived(askDomain ? normaliseDomain(typedDomain) : $domain);
	const domainValid = $derived(isValidDomain(effectiveDomain));

	// The homeserver screen 1 resolved, but only if it belongs to the domain in
	// play: a user correcting the domain here must not be sent to the old one.
	const baseUrl = $derived(
		$homeserver !== '' && effectiveDomain === $domain
			? $homeserver
			: homeserverBaseUrl(effectiveDomain)
	);
	const decoded = $derived(decodeRecoveryKey(typedKey));
	const keyProblem = $derived<RecoveryKeyProblem | null>(decoded.ok ? null : decoded.problem);
	const ready = $derived(
		domainValid && username.trim().length > 0 && password.length > 0 && decoded.ok
	);

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
				// The localpart the user typed, not an id built from it. Matrix's
				// `m.id.user` takes either, and the homeserver qualifies a bare
				// localpart with **its own** server name — which is the only
				// party that knows it. Building `@michel:<typed domain>` was
				// right only when the address and the server name happen to be
				// the same string; through a tunnel, or on loopback, they are
				// not, and the homeserver answered 403 on a user it had never
				// heard of while the screen reported a refused password
				// (#96, #115). `matrixIdFor` stays for the display it was
				// written for.
				userId: owner ?? username.trim(),
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

			// This deployment is the one we actually reached, so stop asking for
			// it on this browser.
			rememberDomain(effectiveDomain);

			stage = 'done';
		} catch (cause) {
			stage = 'form';
			const problem = (cause as { problem?: string }).problem;
			failureKind = problem ?? classify(cause);
			failure = cause instanceof Error ? cause.message : String(cause);
		}
	}

	/**
	 * A cause that is not a `RestoreError` at all. The classification of a
	 * failure the homeserver never saw is `$lib/crypto/bootstrap`'s job now —
	 * it is the only place that can tell a rejected `fetch` from a refusal —
	 * and this is the last resort for anything else that reaches here.
	 */
	function classify(cause: unknown): string {
		const message = cause instanceof Error ? cause.message : String(cause);
		return /failed to fetch|networkerror|load failed|fetch failed/iu.test(message)
			? 'unreachable'
			: 'failed';
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
			{#if askDomain}
				<!--
					Asked before anything secret, because it decides where the
					secret would be sent.
				-->
				<div class="field">
					<label class="label" for="recover-domain">{$t('recover.domain.label')}</label>
					<input
						id="recover-domain"
						class="input"
						type="text"
						inputmode="url"
						autocomplete="off"
						spellcheck="false"
						autocapitalize="none"
						placeholder={$t('screen1.domain.placeholder')}
						data-testid="recover-domain-input"
						bind:value={typedDomain}
						disabled={stage === 'working'}
					/>
					{#if touched && typedDomain.trim() !== '' && !domainValid}
						<p class="small error" data-testid="recover-domain-invalid">
							{$t('screen1.domain.invalid', { example: 'example.com' })}
						</p>
					{:else}
						<p class="small muted">{$t('recover.domain.hint')}</p>
					{/if}
				</div>
			{/if}

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
					<!--
						Only when there is a domain to name: an empty one
						produced "The username you created on ." live.
					-->
					{#if effectiveDomain !== ''}
						<p class="small muted">
							{$t('recover.userId.hint', { domain: effectiveDomain })}
						</p>
					{:else}
						<p class="small muted">{$t('recover.userId.hintNoDomain')}</p>
					{/if}
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
					{:else if failureKind === 'unreachable'}
						{$t('recover.error.unreachable', { url: baseUrl })}
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
