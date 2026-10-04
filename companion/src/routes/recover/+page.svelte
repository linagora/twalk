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
	and nothing is reset, and that is the better outcome — the other devices
	stay verified and whatever is in the key backup stays reachable.

	**And there is a second path, for the browser that has no key** (#439). It
	is beside the first and never instead of it: the key field comes first, the
	reset is offered under it, and choosing it shows what a reset costs **this
	account** — read from the account a moment earlier, not from a warning
	written in advance. This screen used to say that resetting "would break
	every other device and lose the message history", and on the deployment
	where that mattered both halves were false: not one device was
	cross-signed, and the key backup held 0 room keys against 132 encrypted
	rooms. So an owner who had their password was sent to another Matrix client
	against a homeserver published nowhere, which means an SSH tunnel and a
	desktop client.
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import Icon from '$lib/icons/Icon.svelte';
	import RecoveryKeyCard from '$lib/components/RecoveryKeyCard.svelte';
	import { t } from '$lib/i18n';
	import { gateway } from '$lib/api/client';
	import {
		domain,
		restoreDomain,
		rememberDomain,
		matrixBaseUrl,
		isValidDomain,
		normaliseDomain
	} from '$lib/onboarding/domain';
	import { homeserver, restoreHomeserver, matrixSession } from '$lib/onboarding/progress';
	import { localpartOf } from '$lib/onboarding/account';
	import { decodeRecoveryKey, groupRecoveryKey, type RecoveryKeyProblem } from '$lib/recovery/key';
	import type { ResetCost, ResettableCrypto } from '$lib/crypto/reset';

	type Stage = 'form' | 'working' | 'cost' | 'done';

	let stage = $state<Stage>('form');
	let step = $state<
		'signing-in' | 'loading-crypto' | 'unlocking' | 'reading' | 'resetting'
	>('signing-in');

	/**
	 * The reset path's own state (#439).
	 *
	 * `cost` is what this account would lose, measured; `session` is the login
	 * the measurement was made through, kept so the reset itself does not sign
	 * in a second time and leave a second device behind. `resetKey` is the new
	 * key, which exists in this variable and nowhere else.
	 */
	let cost = $state<ResetCost | null>(null);
	let session = $state<{
		crypto: ResettableCrypto;
		userId: string;
		deviceId: string;
		accessToken: string;
	} | null>(null);
	let resetKey = $state<string | null>(null);
	/** What the deployment says about where its homeserver answers (#323). */
	let clientUrl = $state<string | null>(null);
	let resetBackup = $state(false);

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
			try {
				// Where this deployment's homeserver answers a browser (#323).
				// Asked of the deployment rather than derived from its name,
				// which is not always an address the outside can call.
				const described = await gateway.GET('/api/deployment');
				clientUrl = described.data?.client_url ?? null;
			} catch {
				// The screen still works: the address falls back to the name.
			}
		})();
	});

	/** Asked for only when nothing else can say it. */
	const askDomain = $derived($domain === '');
	const effectiveDomain = $derived(askDomain ? normaliseDomain(typedDomain) : $domain);
	const domainValid = $derived(isValidDomain(effectiveDomain));

	/**
	 * Where this browser sends its Matrix requests (#323). The deployment's
	 * own answer wins when there is one, then what screen 1 resolved, then the
	 * domain itself — and the last two only while the domain in play is still
	 * the deployment's, so a user correcting it is not sent to the old one.
	 *
	 * This screen is where it was found: both paths below begin with a
	 * password login, and on a deployment published under another name than
	 * its server name the login went to the browser's own loopback. What the
	 * owner read was `the login response was not a session`.
	 */
	const baseUrl = $derived(
		matrixBaseUrl({
			deploymentClientUrl: clientUrl,
			discoveredHomeserver: $homeserver,
			deploymentDomain: $domain,
			effectiveDomain
		})
	);
	const decoded = $derived(decodeRecoveryKey(typedKey));
	const keyProblem = $derived<RecoveryKeyProblem | null>(decoded.ok ? null : decoded.problem);
	const ready = $derived(
		domainValid && username.trim().length > 0 && password.length > 0 && decoded.ok
	);
	/**
	 * The reset path needs everything the restore path does **except** the key
	 * — which is the whole of why it exists. Its own derivation rather than a
	 * relaxation of `ready`, so the primary button can never become clickable
	 * without a key by accident.
	 */
	const readyWithoutKey = $derived(
		domainValid && username.trim().length > 0 && password.length > 0
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
	 * The reset path's first half: sign in, and ask the account what a reset
	 * would cost **it** (#439).
	 *
	 * Nothing is changed here. The owner is shown the measurement and chooses
	 * — which is the whole shape of this feature: the cost is not the same on
	 * every deployment, and the client can tell the difference before asking.
	 */
	async function askWhatAResetCosts(): Promise<void> {
		touched = true;
		if (!readyWithoutKey || stage === 'working') {
			return;
		}
		failure = null;
		failureKind = null;
		stage = 'working';
		step = 'signing-in';

		try {
			const { signInWithoutRecoveryKey } = await import('$lib/crypto/bootstrap');
			const signedIn = await signInWithoutRecoveryKey({
				baseUrl,
				userId: owner ?? username.trim(),
				password,
				onStep: (next) => {
					step = next;
				}
			});
			step = 'reading';
			const { whatAResetCosts } = await import('$lib/crypto/reset');
			cost = await whatAResetCosts(signedIn.crypto, signedIn.userId);
			session = signedIn;
			stage = 'cost';
		} catch (cause) {
			stage = 'form';
			const problem = (cause as { problem?: string }).problem;
			failureKind = problem ?? classify(cause);
			failure = cause instanceof Error ? cause.message : String(cause);
		}
	}

	/**
	 * The second half, after the owner has read the cost and said yes.
	 *
	 * The new key is shown once, on the card onboarding uses — it exists in
	 * `resetKey` and nowhere else, and travels in no request.
	 */
	async function confirmReset(): Promise<void> {
		if (session === null || stage === 'working') {
			return;
		}
		const signedIn = session;
		failure = null;
		failureKind = null;
		stage = 'working';
		step = 'resetting';

		try {
			const { resetIdentity } = await import('$lib/crypto/reset');
			const result = await resetIdentity({
				crypto: signedIn.crypto,
				userId: signedIn.userId,
				password,
				deviceId: signedIn.deviceId
			});
			resetKey = result.recoveryKey;
			resetBackup = result.keyBackup;
			matrixSession.set({
				baseUrl,
				userId: signedIn.userId,
				deviceId: signedIn.deviceId,
				accessToken: signedIn.accessToken
			});

			// As on the restore path: the Gateway cookie may have gone with the
			// store, and the keys are back either way if this fails.
			const { signInToGateway } = await import('$lib/session/signin');
			await signInToGateway({
				baseUrl,
				userId: signedIn.userId,
				accessToken: signedIn.accessToken
			}).catch(() => {});

			rememberDomain(effectiveDomain);
			verified = true;
			stage = 'done';
		} catch (cause) {
			// Back to the measurement rather than to the form: the owner has
			// already signed in, and sending them to type their password again
			// would read as a refused password.
			stage = 'cost';
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
	{#if stage === 'done' && resetKey !== null}
		<!--
			The reset's own end: the new key, on the card onboarding shows, once.
			A different header from the restore path's on purpose — this device
			is not "back", it is signed by an identity that is one minute old.
		-->
		<header class="stack">
			<h1>{$t('recover.reset.title')}</h1>
			<p class="subtitle" data-testid="recover-reset-done">{$t('recover.reset.body')}</p>
		</header>
		<RecoveryKeyCard
			recoveryKey={resetKey}
			userId={session?.userId ?? owner ?? username}
			domain={effectiveDomain}
			keyBackup={resetBackup}
			onContinue={() => {
				resetKey = null;
				window.location.assign('/');
			}}
		/>
	{:else if stage === 'cost'}
		<!--
			What this account loses, measured a moment ago. The numbers are the
			point: a generic warning is what kept this path shut, and it was
			false on the account it was protecting (#439).
		-->
		<header class="stack">
			<h1>{$t('recover.cost.title')}</h1>
			<p class="subtitle">{$t('recover.cost.intro')}</p>
		</header>
		<!--
			The account's total is on the element and not in the sentence: "one of
			your 1 devices" is reachable and reads as a bug, and the number the
			owner acts on is how many devices lose their signature. The journey
			asserts both attributes.
		-->
		<ul class="stack" data-testid="recover-cost" data-signed={cost?.signedDevices}
			data-devices={cost?.devices} data-room-keys={cost?.roomKeys ?? 'none'}>
			<li>
				{$t('recover.cost.devices', { signed: cost?.signedDevices ?? 0 })}
			</li>
			<li>
				{#if cost?.roomKeys === null || cost?.roomKeys === undefined}
					{$t('recover.cost.noBackup')}
				{:else}
					{$t('recover.cost.history', { count: cost.roomKeys })}
				{/if}
			</li>
		</ul>
		{#if failure !== null}
			<p class="error-text" role="alert" data-testid="recover-reset-error" data-kind={failureKind}>
				{$t('recover.error.failed', { detail: failure })}
			</p>
		{/if}
		<p class="stack">
			<button
				class="button button--primary"
				type="button"
				data-testid="recover-reset-confirm"
				onclick={confirmReset}
			>
				{$t('recover.cost.confirm')}
			</button>
			<button
				class="button"
				type="button"
				data-testid="recover-reset-back"
				onclick={() => {
					stage = 'form';
					cost = null;
				}}
			>
				{$t('recover.cost.back')}
			</button>
		</p>
	{:else if stage === 'done'}
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

			<!--
				The second path, and its placement is the ticket's own
				requirement: **beside** the key field and under the primary
				button, never instead of them. Entering the key keeps every
				other device verified and the key backup reachable, so it stays
				the thing to do; this is for the browser that cannot (#439).
			-->
			<div class="stack" data-testid="recover-no-key">
				<p class="small muted">{$t('recover.noKey.offer')}</p>
				<button
					class="button"
					type="button"
					disabled={!readyWithoutKey || stage === 'working'}
					data-testid="recover-no-key-action"
					onclick={askWhatAResetCosts}
				>
					{$t('recover.noKey.action')}
				</button>
			</div>
		</form>

		<p class="small muted">
			<Icon name="phone" size="dense" />
			{$t('recover.install')}
		</p>

		<!--
			The way out, and it has to be here. Only `/` asks whether the crypto
			store exists; approvals, every settings screen and mail triage do
			not, and they work untouched without the key. A screen that gates
			one route out of six and offers no exit reads as a locked
			application, which is how the owner of the reference deployment
			concluded they had to reset and lose their history (#433). It is
			quiet and it is last: entering the key is still the thing to do.
		-->
		<p class="small muted" data-testid="recover-carry-on">
			{$t('recover.carryOn')}
			<a href="/approvals">{$t('recover.carryOn.link')}</a>
		</p>
	{/if}
</section>

<style>
	a.button {
		text-decoration: none;
	}
</style>
