<!--
	The way back in (ticket #112).

	Before this screen existed, `signInToGateway` was called from exactly one
	place — the onboarding screen, immediately after creating the account — so
	a returning user whose device token had expired had two options, both
	wrong: an onboarding screen offering to create an account that already
	exists, or `/recover`, which asks for the recovery key. That key is stored
	offline for the day a browser loses its crypto store; producing it because
	a fifteen-minute timer elapsed is not the same event, and a user who did
	not write it down was locked out of a working deployment.

	This screen asks for what it needs and nothing more: a Matrix sign-in,
	password where the homeserver allows it and SSO where it advertises it,
	then an OpenID token, then `POST /api/session`. No recovery key. The crypto
	store is untouched — a device that still has its keys keeps them, and one
	that does not is sent to `/recover`, which is what that screen is for.

	Two things here are deliberate and were each a defect somewhere else first.

	**The homeserver survives the SSO round trip.** An SSO redirect is a full
	page load: component state does not come back. The networks screen rebuilt
	it from what onboarding had remembered, and so presented a login token
	issued by one homeserver to a different one (#125). A login token is a
	single-use credential issued by one server for that server, so this screen
	remembers which homeserver it sent the user to, and **refuses** to exchange
	the token anywhere else rather than falling back to whatever it has.

	**It asks which deployment when it cannot know.** Reached in a new tab, on
	a new device, or through a different tunnelled port — a different origin is
	a different `localStorage` — there is nothing to restore. `/recover` failed
	on exactly this with a raw `Failed to fetch` (#115).
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';

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
	import {
		loginFlows,
		loginTokenFrom,
		loginWithPassword,
		loginWithToken,
		ssoRedirectUrl,
		type LoginFlows
	} from '$lib/matrix/login';
	import { signInToGateway } from '$lib/session/signin';
	import { DEFAULT_DESTINATION, destinationFrom } from '$lib/signin/destination';

	/**
	 * Which homeserver an SSO round trip was started against.
	 *
	 * In `localStorage` rather than `sessionStorage` because an identity
	 * provider may answer in a new tab, and the token is worthless without the
	 * server that issued it.
	 */
	const SSO_HOMESERVER_KEY = 'twalk:signin:sso-homeserver';

	type Stage = 'form' | 'working' | 'done';

	let stage = $state<Stage>('form');
	let typedDomain = $state('');
	let username = $state('');
	let password = $state('');
	let flows = $state<LoginFlows | null>(null);
	let failure = $state<string | null>(null);
	let failureKind = $state<string | null>(null);
	/**
	 * Whether this deployment has its account yet.
	 *
	 * `null` until the Gateway answers. A screen reached before any account
	 * exists must say so and point at onboarding — offering a sign-in form for
	 * an account nobody created is the same defect as the account form that
	 * offered to create one that already existed (#112), pointing the other
	 * way.
	 */
	let bootstrapped = $state<boolean | null>(null);

	/** Asked for only when nothing else can say it. */
	const askDomain = $derived($domain === '');
	const effectiveDomain = $derived(askDomain ? normaliseDomain(typedDomain) : $domain);
	const domainValid = $derived(isValidDomain(effectiveDomain));
	const baseUrl = $derived(
		$homeserver !== '' && effectiveDomain === $domain
			? $homeserver
			: homeserverBaseUrl(effectiveDomain)
	);
	const ready = $derived(domainValid && username.trim().length > 0 && password.length > 0);

	let next = $state(DEFAULT_DESTINATION);

	onMount(() => {
		restoreDomain();
		restoreHomeserver();
		const url = new URL(window.location.href);
		next = destinationFrom(url);

		void (async () => {
			// What this deployment is, asked rather than discovered through a
			// refusal. It also names the homeserver, which is how a browser
			// that has never been here learns where it is.
			try {
				const described = await gateway.GET('/api/deployment');
				if (described.data !== undefined) {
					bootstrapped = described.data.bootstrapped;
					if ($domain === '') {
						domain.set(described.data.homeserver);
					}
				}
			} catch {
				// The screen still works: the user can say where they are.
			}

			const token = loginTokenFrom(url);
			if (token !== null) {
				await completeSso(url, token);
				return;
			}
			if (effectiveDomain !== '') {
				await readFlows();
			}
		})();
	});

	/**
	 * The second half of the SSO round trip.
	 *
	 * The token is exchanged against the homeserver that issued it, or not at
	 * all. Guessing is what #125 did.
	 */
	async function completeSso(url: URL, token: string) {
		// Out of the address bar before anything else: a login token is a
		// credential, and a copied URL must not carry one.
		const clean = new URL(url.toString());
		clean.searchParams.delete('loginToken');
		history.replaceState(null, '', clean.toString());

		let issuer: string | null = null;
		try {
			issuer = window.localStorage.getItem(SSO_HOMESERVER_KEY);
			window.localStorage.removeItem(SSO_HOMESERVER_KEY);
		} catch {
			issuer = null;
		}
		if (issuer === null || issuer === '') {
			failureKind = 'sso-lost';
			failure = 'the homeserver this sign-in started at is not known any more';
			return;
		}

		stage = 'working';
		try {
			await finish(await loginWithToken(issuer, token), issuer);
		} catch (cause) {
			stage = 'form';
			report(cause);
		}
	}

	async function readFlows() {
		try {
			flows = await loginFlows(baseUrl);
			failure = null;
			failureKind = null;
		} catch (cause) {
			flows = null;
			report(cause);
		}
	}

	function startSso(idpId?: string) {
		try {
			window.localStorage.setItem(SSO_HOMESERVER_KEY, baseUrl);
		} catch {
			// Storage is off: say so rather than starting a round trip whose
			// return this screen would have to refuse.
			failureKind = 'sso-needs-storage';
			failure = 'this browser will not let the sign-in remember where it started';
			return;
		}
		rememberDomain(effectiveDomain);
		window.location.href = ssoRedirectUrl(
			baseUrl,
			`${window.location.origin}/signin?next=${encodeURIComponent(next)}`,
			idpId
		);
	}

	async function submitPassword(event: SubmitEvent) {
		event.preventDefault();
		if (!ready || stage === 'working') {
			return;
		}
		stage = 'working';
		failure = null;
		failureKind = null;
		try {
			await finish(await loginWithPassword(baseUrl, username.trim(), password), baseUrl);
		} catch (cause) {
			stage = 'form';
			report(cause);
		} finally {
			// The password is not kept a moment longer than the request needs.
			password = '';
		}
	}

	/** Matrix session in hand: exchange it for a Gateway session and leave. */
	async function finish(session: Awaited<ReturnType<typeof loginWithPassword>>, server: string) {
		matrixSession.set(session);
		await signInToGateway({
			baseUrl: server,
			userId: session.userId,
			accessToken: session.accessToken
		});
		rememberDomain(effectiveDomain);
		stage = 'done';
		await goto(next);
	}

	function report(cause: unknown) {
		const problem = (cause as { problem?: string }).problem;
		const message = cause instanceof Error ? cause.message : String(cause);
		failureKind =
			problem ??
			(/failed to fetch|networkerror|load failed|fetch failed/iu.test(message)
				? 'unreachable'
				: 'failed');
		failure = message;
	}
</script>

<section class="screen" data-testid="screen-signin">
	<header class="stack">
		<h1>{$t('signin.title')}</h1>
		<p class="subtitle">{$t('signin.intro')}</p>
	</header>

	{#if bootstrapped === false}
		<div class="card card--info" data-testid="signin-not-bootstrapped">
			<p>{$t('signin.noAccountYet')}</p>
			<p><a class="button button--primary" href="/">{$t('screen1.continue')}</a></p>
		</div>
	{:else}
	{#if askDomain}
		<div class="field">
			<label class="label" for="signin-domain">{$t('recover.domain.label')}</label>
			<input
				id="signin-domain"
				class="input"
				type="text"
				inputmode="url"
				autocomplete="off"
				spellcheck="false"
				autocapitalize="none"
				placeholder={$t('screen1.domain.placeholder')}
				data-testid="signin-domain-input"
				bind:value={typedDomain}
				onblur={readFlows}
				disabled={stage === 'working'}
			/>
			<p class="small muted">{$t('recover.domain.hint')}</p>
		</div>
	{/if}

	{#if flows !== null && flows.sso}
		<!-- One button per advertised identity provider, labelled with the
		     provider's own name: a user recognises "Connect with Twake", not a
		     generic "single sign-on". -->
		{#if flows.identityProviders.length > 0}
			{#each flows.identityProviders as provider (provider.id)}
				<p>
					<button
						class="button button--primary"
						type="button"
						data-testid={`signin-sso-${provider.id}`}
						disabled={stage === 'working' || !domainValid}
						onclick={() => startSso(provider.id)}
					>
						{$t('signin.ssoWith', { provider: provider.name })}
					</button>
				</p>
			{/each}
		{:else}
			<p>
				<button
					class="button button--primary"
					type="button"
					data-testid="signin-sso"
					disabled={stage === 'working' || !domainValid}
					onclick={() => startSso()}
				>
					{$t('matrix.sso')}
				</button>
			</p>
		{/if}
	{/if}

	{#if flows === null || flows.password}
		<form class="stack" onsubmit={submitPassword} novalidate>
			<div class="field">
				<label class="label" for="signin-username">{$t('recover.userId.label')}</label>
				<input
					id="signin-username"
					class="input"
					type="text"
					autocomplete="username"
					spellcheck="false"
					autocapitalize="none"
					bind:value={username}
					disabled={stage === 'working'}
				/>
			</div>

			<div class="field">
				<label class="label" for="signin-password">{$t('recover.password.label')}</label>
				<input
					id="signin-password"
					class="input"
					type="password"
					autocomplete="current-password"
					bind:value={password}
					disabled={stage === 'working'}
				/>
			</div>

			<button
				class="button button--primary"
				type="submit"
				data-testid="signin-submit"
				disabled={!ready || stage === 'working'}
			>
				{#if stage === 'working'}
					<span class="spinner" aria-hidden="true"></span>
					<span class="visually-hidden">{$t('signin.working')}</span>
				{:else}
					{$t('signin.submit')}
					<Icon name="continue" size="dense" />
				{/if}
			</button>
		</form>
	{/if}

	{#if failure !== null}
		<p class="error-text" role="alert" data-testid="signin-error" data-kind={failureKind}>
			{#if failureKind === 'unreachable'}
				{$t('recover.error.unreachable', { url: baseUrl })}
			{:else if failureKind === 'wrong-password'}
				{$t('recover.error.wrong-password')}
			{:else if failureKind === 'sso-lost'}
				{$t('signin.error.ssoLost')}
			{:else if failureKind === 'sso-needs-storage'}
				{$t('signin.error.ssoNeedsStorage')}
			{:else if failureKind === 'not-the-owner'}
				{$t('signin.error.notTheOwner')}
			{:else}
				{$t('recover.error.failed', { detail: failure })}
			{/if}
		</p>
	{/if}

	<!--
		The keys are a different problem from the session, and saying so here
		is what stops a user reaching for their recovery key because a timer
		elapsed.
	-->
	<p class="small muted">{$t('signin.notRecovery')}</p>
	<p class="small"><a href="/recover" data-testid="signin-recover-link">{$t('signin.lostKeys')}</a></p>
	{/if}
</section>
