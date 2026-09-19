<!--
	Screen 2 of `docs/wireframes/companion-v0.1.md`: create the account, then —
	"within the same screen", as the wireframe puts it — save the recovery key.

	Four states, in one route because the wireframe designs them as one screen:

	  `account`  the form (username, password, confirmation, strength meter,
	             and the card explaining what a recovery key is);
	  `working`  the Gateway's registration relay, then the in-browser
	             cryptographic bootstrap, each step named as it happens;
	  `key`      the key, once, with copy, PDF and the checkbox gate;
	  `done`     the account exists, this device is signed in to the Gateway.

	The ordering inside `working` is the whole point of ADR 0014 and lives in
	`$lib/crypto/bootstrap.ts`, not here: cross-signing, then the recovery key,
	then secret storage with the key backup. This page shows the steps; it does
	not decide them.

	What this page never does is touch the recovery key beyond handing it to
	the component that displays it. It is generated in the browser, it goes to
	the screen, the clipboard and a PDF built here, and to nothing else.
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';

	import Icon from '$lib/icons/Icon.svelte';
	import RecoveryKeyCard from '$lib/components/RecoveryKeyCard.svelte';
	import { t } from '$lib/i18n';
	import { domain, restoreDomain, homeserverBaseUrl } from '$lib/onboarding/domain';
	import { homeserver, restoreHomeserver, matrixSession } from '$lib/onboarding/progress';
	import {
		checkPassword,
		checkUsername,
		matrixIdFor,
		passwordStrength,
		type PasswordProblem,
		type UsernameProblem
	} from '$lib/onboarding/account';
	import { createAccount, RegistrationError } from '$lib/onboarding/register';
	import type { BootstrapStep } from '$lib/crypto/bootstrap';
	import type { HandoverOutcome } from '$lib/matrix/handover';

	type Stage = 'account' | 'working' | 'key' | 'done';

	let stage = $state<Stage>('account');
	let step = $state<BootstrapStep | 'creating' | 'signing-in' | 'handover'>('creating');

	/**
	 * How the handover room ended (#226): the one encrypted room this account
	 * shares with the Sensor, without which the browser cannot later hand the
	 * Sensor a credential at all (ADR 0034). `null` until the step has run.
	 *
	 * It is reported and never assumed. A send to a user the crypto machine does
	 * not track resolves successfully having sent nothing, so a Companion that
	 * treated "the room was created" as "the handover will work" would be the
	 * defect this step exists to close.
	 */
	let handover = $state<HandoverOutcome | null>(null);

	let username = $state('');
	let password = $state('');
	let confirmation = $state('');
	let touched = $state({ username: false, password: false, confirmation: false });

	/** A refusal that belongs to a field, from the homeserver or the Gateway. */
	let usernameRefusal = $state<'taken' | 'not-the-owner' | null>(null);
	let passwordRefusal = $state<string | null>(null);
	/** A refusal that replaces the form. */
	let blocked = $state<'closed' | 'exists' | 'recovery-key-refused' | null>(null);
	let genericError = $state<string | null>(null);

	let recoveryKey = $state('');
	let accountId = $state('');
	let keyBackup = $state(false);

	onMount(() => {
		restoreDomain();
		restoreHomeserver();
	});

	const usernameProblem = $derived<UsernameProblem | null>(checkUsername(username));
	const passwordProblem = $derived<PasswordProblem | null>(checkPassword(password));
	const mismatch = $derived(confirmation.length > 0 && confirmation !== password);
	const strength = $derived(passwordStrength(password));
	const ready = $derived(
		usernameProblem === null && passwordProblem === null && confirmation === password
	);

	const matrixId = $derived(
		username.length === 0 || $domain === '' ? '' : matrixIdFor(username, $domain)
	);

	/**
	 * The homeserver screen 1 resolved. Falling back to the domain's own base
	 * URL rather than sending the user back a screen: a reload of this page in
	 * a browser with no `localStorage` would otherwise dead-end.
	 */
	const baseUrl = $derived($homeserver !== '' ? $homeserver : homeserverBaseUrl($domain));

	async function submit(event: SubmitEvent) {
		event.preventDefault();
		touched = { username: true, password: true, confirmation: true };
		if (!ready || stage === 'working') {
			return;
		}
		usernameRefusal = null;
		passwordRefusal = null;
		genericError = null;
		stage = 'working';
		step = 'creating';

		try {
			const session = await createAccount({ username, password, baseUrl });
			accountId = session.userId;
			matrixSession.set(session);

			// The crypto stack, and the SDK itself, arrive here and nowhere
			// earlier: this dynamic import is what keeps screen 1 light.
			const { bootstrapIdentity } = await import('$lib/crypto/bootstrap');
			const result = await bootstrapIdentity(session, password, (next) => {
				step = next;
			});
			recoveryKey = result.recoveryKey;
			keyBackup = result.keyBackup;

			// Sign this browser in to the Gateway with a Matrix OpenID token,
			// so a reload finds a session (ADR 0011). Done before the key is
			// shown, so the user never sees the key on a device that then
			// fails to sign in.
			step = 'signing-in';
			const { signInToGateway } = await import('$lib/session/signin');
			const signedIn = await signInToGateway({
				baseUrl,
				userId: session.userId,
				accessToken: session.accessToken
			});

			// The one encrypted room this account shares with the Sensor (#226).
			// Last, because it needs the Gateway session to learn which Sensor
			// this deployment runs, and it needs the crypto stack the bootstrap
			// above brought up. It never costs the user their account: every way
			// it can end is an outcome stated on the next screen.
			step = 'handover';
			const { ensureHandoverRoom } = await import('$lib/matrix/handover');
			const { handoverCrypto } = await import('$lib/crypto/bootstrap');
			const crypto = handoverCrypto();
			handover =
				crypto === null
					? { kind: 'failed', detail: 'the crypto stack is not running in this tab' }
					: await ensureHandoverRoom({
							baseUrl,
							accessToken: session.accessToken,
							userId: session.userId,
							sensorUserId: signedIn.sensor,
							roomName: $t('handover.roomName'),
							crypto
						});

			stage = 'key';
		} catch (cause) {
			stage = 'account';
			if (cause instanceof RegistrationError) {
				switch (cause.problem) {
					case 'username-taken':
						usernameRefusal = 'taken';
						return;
					case 'username-not-the-owner':
						usernameRefusal = 'not-the-owner';
						return;
					case 'password-refused':
						passwordRefusal = cause.matrixErrcode ?? 'M_UNKNOWN';
						return;
					case 'registration-closed':
						blocked = 'closed';
						return;
					case 'account-already-exists':
						blocked = 'exists';
						return;
					case 'recovery-key-refused':
						blocked = 'recovery-key-refused';
						return;
					default:
						genericError = cause.detail;
						return;
				}
			}
			genericError = cause instanceof Error ? cause.message : String(cause);
		}
	}
</script>

<section class="screen" data-testid="screen-onboarding">
	{#if blocked === 'closed'}
		<div class="card card--warning" role="alert" data-testid="registration-closed">
			<p class="card__title">
				<Icon name="error" size="dense" />
				{$t('screen2.error.closed.title')}
			</p>
			<p>{$t('screen2.error.closed.body')}</p>
		</div>
	{:else if blocked === 'exists'}
		<div class="card card--info" role="alert" data-testid="account-exists">
			<p class="card__title">
				<Icon name="info" size="dense" />
				{$t('screen2.error.exists.title')}
			</p>
			<p>{$t('screen2.error.exists.body')}</p>
			<p>
				<a class="button button--primary" href="/recover" data-testid="go-to-recover">
					<Icon name="recovery-key" size="dense" />
					{$t('screen2.error.exists.cta')}
				</a>
			</p>
		</div>
	{:else if blocked === 'recovery-key-refused'}
		<div class="card card--warning" role="alert" data-testid="recovery-key-refused">
			<p class="card__title">
				<Icon name="error" size="dense" />
				{$t('screen2.error.recoveryKeyRefused')}
			</p>
		</div>
	{:else if stage === 'key'}
		<RecoveryKeyCard
			{recoveryKey}
			userId={accountId}
			domain={$domain}
			{keyBackup}
			onContinue={() => (stage = 'done')}
		/>
	{:else if stage === 'done'}
		<header class="stack">
			<h1>{$t('done.title')}</h1>
			<p class="subtitle" data-testid="account-id">{$t('done.body', { id: accountId })}</p>
		</header>
		<!--
			What happened to the room this account shares with the Sensor (#226).
			Stated on the way out rather than left to the logs: without it Twalk
			cannot later hand the Sensor the credential it needs to reply on the
			user's behalf (ADR 0034), and the symptom of not saying so is a reply
			that never arrives for a reason nobody can trace.
		-->
		{#if handover !== null}
			<p
				class="small muted"
				data-testid="handover"
				data-kind={handover.kind}
				data-sensor-devices={handover.kind === 'ready' ? handover.sensorDevices : 0}
				hidden={handover.kind !== 'ready'}
			>
				{$t('handover.ready')}
			</p>
			{#if handover.kind !== 'ready'}
				<div class="card card--warning" role="status" data-testid="handover-problem">
					<p class="card__title">
						<Icon name="error" size="dense" />
						{$t('handover.problem.title')}
					</p>
					<p>
						{#if handover.kind === 'no-sensor'}
							{$t('handover.problem.noSensor')}
						{:else if handover.kind === 'sensor-did-not-join'}
							{$t('handover.problem.didNotJoin')}
						{:else if handover.kind === 'sensor-untracked'}
							{$t('handover.problem.untracked')}
						{:else}
							{$t('handover.problem.failed', { detail: handover.detail })}
						{/if}
					</p>
					<p class="small muted">{$t('handover.problem.after')}</p>
				</div>
			{/if}
		{/if}
		<!--
			The way on, and it is a real one. This card said the network screens
			"are not built yet" — true when #67 wrote it, false since #68 merged
			them, and it left the user of a finished account with nowhere to go.
			The journey is: account, then a network (screen 3), then the
			assistant (screen 4), then the dashboard (screen 5).
		-->
		<div class="card card--info" data-testid="onboarding-done">
			<p class="card__title">
				<Icon name="info" size="dense" />
				{$t('done.next.title')}
			</p>
			<p>{$t('done.next.body')}</p>
			<p>
				<a class="button button--primary" href="/networks" data-testid="to-networks">
					{$t('done.next.cta')}
					<Icon name="continue" size="dense" />
				</a>
			</p>
			<p class="small muted">{$t('done.next.after')}</p>
		</div>
		<p class="small">
			<a href="/dashboard" data-testid="to-dashboard">{$t('done.dashboard')}</a>
			·
			<a href="/diagnostics">{$t('done.diagnostics')}</a>
		</p>
	{:else}
		<header class="stack">
			<p class="small">
				<a href="/">
					<Icon name="back" size="dense" />
					{$t('screen2.back')}
				</a>
			</p>
			<h1>{$t('screen2.title')}</h1>
			{#if $domain !== ''}
				<p class="subtitle" data-testid="chosen-domain">
					{$t('screen2.subtitle', { domain: $domain })}
				</p>
			{/if}
		</header>

		<form class="stack" onsubmit={submit} novalidate>
			<div class="field">
				<label class="label" for="username">{$t('screen2.username.label')}</label>
				<input
					id="username"
					class="input"
					name="username"
					type="text"
					autocomplete="username"
					spellcheck="false"
					autocapitalize="none"
					bind:value={username}
					onblur={() => (touched = { ...touched, username: true })}
					oninput={() => (usernameRefusal = null)}
					disabled={stage === 'working'}
					aria-invalid={(touched.username && usernameProblem !== null) ||
						usernameRefusal !== null}
					aria-describedby="username-error"
				/>
				<p class="small muted">{$t('screen2.username.hint')}</p>
				<p class="error-text" id="username-error" role="alert" aria-live="polite">
					{#if usernameRefusal === 'taken'}
						{$t('screen2.username.taken', { domain: $domain })}
					{:else if usernameRefusal === 'not-the-owner'}
						{$t('screen2.username.not-the-owner')}
					{:else if touched.username && usernameProblem !== null}
						{$t(`screen2.username.${usernameProblem}` as 'screen2.username.empty')}
					{/if}
				</p>
			</div>

			<div class="field">
				<label class="label" for="password">{$t('screen2.password.label')}</label>
				<input
					id="password"
					class="input"
					name="password"
					type="password"
					autocomplete="new-password"
					bind:value={password}
					onblur={() => (touched = { ...touched, password: true })}
					oninput={() => (passwordRefusal = null)}
					disabled={stage === 'working'}
					aria-invalid={touched.password && passwordProblem !== null}
					aria-describedby="password-error"
				/>
				<p class="small muted">{$t('screen2.password.hint')}</p>
				<!-- The meter is feedback, not a gate: the gate is the error
				     text below, which is the wireframe's own floor. -->
				<p class="meter" data-testid="password-strength" data-score={strength}>
					<span class="visually-hidden">{$t('screen2.strength.label')}</span>
					{#each [1, 2, 3, 4] as notch (notch)}
						<span class="meter__notch" class:meter__notch--on={strength >= notch}></span>
					{/each}
					<span class="small muted"
						>{$t(`screen2.strength.${strength}` as 'screen2.strength.0')}</span
					>
				</p>
				<p class="error-text" id="password-error" role="alert" aria-live="polite">
					{#if passwordRefusal !== null}
						{$t('screen2.password.refused', { errcode: passwordRefusal })}
					{:else if touched.password && passwordProblem !== null}
						{$t(`screen2.password.${passwordProblem}` as 'screen2.password.empty')}
					{/if}
				</p>
			</div>

			<div class="field">
				<label class="label" for="confirmation">{$t('screen2.password.confirm')}</label>
				<input
					id="confirmation"
					class="input"
					name="confirmation"
					type="password"
					autocomplete="new-password"
					bind:value={confirmation}
					onblur={() => (touched = { ...touched, confirmation: true })}
					disabled={stage === 'working'}
					aria-invalid={mismatch}
					aria-describedby="confirmation-error"
				/>
				<p class="error-text" id="confirmation-error" role="alert" aria-live="polite">
					{#if mismatch}
						{$t('screen2.password.mismatch')}
					{/if}
				</p>
			</div>

			{#if matrixId !== ''}
				<p class="small muted" data-testid="matrix-id">
					{$t('screen2.matrixId', { id: matrixId })}
				</p>
			{/if}

			<div class="card card--info">
				<p class="card__title">
					<Icon name="recovery-key" size="dense" />
					{$t('screen2.recoveryCard.title')}
				</p>
				<p>{$t('screen2.recoveryCard.body')}</p>
			</div>

			{#if genericError !== null}
				<p class="error-text" role="alert" data-testid="registration-error">
					{$t('screen2.error.generic', { detail: genericError })}
				</p>
			{/if}

			<button
				class="button button--primary"
				type="submit"
				disabled={!ready || stage === 'working'}
				data-testid="create-account"
			>
				{#if stage === 'working'}
					<span class="spinner" aria-hidden="true"></span>
					<span data-testid="bootstrap-step" data-step={step}>
						{$t(`screen2.step.${step}` as 'screen2.step.creating')}
					</span>
				{:else}
					{$t('screen2.create')}
					<Icon name="continue" size="dense" />
				{/if}
			</button>
		</form>
	{/if}
</section>

<style>
	.meter {
		display: flex;
		align-items: center;
		gap: var(--space-1);
	}

	.meter__notch {
		height: 4px;
		width: 28px;
		border-radius: var(--radius-pill);
		background: var(--color-border);
	}

	.meter__notch--on {
		background: var(--color-primary);
	}

	.meter .small {
		margin-inline-start: var(--space-2);
	}

	a.button {
		text-decoration: none;
	}
</style>
