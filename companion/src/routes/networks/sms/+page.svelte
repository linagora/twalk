<!--
	Screen 3c of `docs/wireframes/companion-v0.1.md`: SMS through Google
	Messages — the v0.1 **preview** path.

	The framing the milestone decided is not decoration and is not softened
	here: SMS transits Google Messages Web, a Google account is required, an
	iPhone alone cannot feed it, and v0.2 replaces the whole path with the
	first-party Twake SMS Companion (ADR 0004). The disclosure card says all of
	that before anything else is shown.

	# The cookies, honestly

	Google deprecated the QR login for third-party Google Messages clients in
	2024, so mautrix-gmessages signs in with the user's Google session cookies
	(docs.mau.fi). The wireframe imagined an in-app browser doing the
	extraction; a PWA has none, and no page may read another origin's cookies —
	that is the same-origin policy, not a gap to work around. So the user copies
	them, and this screen's job is to say plainly what they are handing over and
	what the two browser settings are that otherwise make the copy useless:

	  - a **private window**, because signing out of Google — or letting the
	    ordinary session rotate — invalidates the cookies the bridge is holding;
	  - **Device Bound Session Credentials off** in Chrome, because that feature
	    binds the session to this device's hardware key, and a copied cookie
	    then works nowhere else. It is exactly what it says on the tin, and it
	    is on by default in recent Chrome.

	# Where they go, and where they do not

	Into `POST /api/bridges/{id}/login/submit`, relayed to the bridge and
	forgotten by the Gateway (ADR 0011). **This screen writes them nowhere**:
	not to `localStorage`, not to IndexedDB, not to a store that outlives the
	page. The textarea is cleared the moment the submission is made.
-->
<script lang="ts">
	import { onDestroy, onMount } from 'svelte';

	import { gateway } from '$lib/api/client';
	import { troubleOf, type ApiTrouble } from '$lib/api/trouble';
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import { looksLikeIos } from '$lib/networks/catalogue';
	import { missing, parseCookies } from '$lib/networks/cookies';
	import { LoginSession } from '$lib/networks/login-session';
	import { type LoginState } from '$lib/networks/login-view';

	const DISCLOSURE_KEY = 'twalk.disclosure.sms';

	let ios = $state(false);
	let bridgeId = $state<string | null>(null);
	let bridgeKnown = $state(false);
	/**
	 * Why the bridge list could not be read, or `null` when it could (#111).
	 * Three outcomes, not two: `$lib/api/trouble.ts`.
	 */
	let bridgesTrouble = $state<ApiTrouble | null>(null);
	let accepted = $state(false);
	let session = $state<LoginSession | null>(null);
	let loginState = $state<LoginState>({ view: { kind: 'idle' }, conflict: null, trouble: null });
	let pasted = $state('');
	let cookieProblem = $state<string | null>(null);
	let unsubscribe: (() => void) | null = null;

	const view = $derived(loginState.view);
	/** The cookie names the bridge's own step asked for. */
	const wanted = $derived(view.kind === 'cookies' ? view.request.fields : []);
	const signInUrl = $derived(
		view.kind === 'cookies' && view.request.url !== null
			? view.request.url
			: 'https://messages.google.com/web/authentication'
	);

	onMount(async () => {
		ios = looksLikeIos(navigator.userAgent, navigator.maxTouchPoints, navigator.platform);
		try {
			accepted = localStorage.getItem(DISCLOSURE_KEY) === 'accepted';
		} catch {
			accepted = false;
		}
		await findBridge();
	});

	/**
	 * Which bridge serves SMS, and the login on it. Retryable, because a
	 * Gateway that could not answer is a dead end this screen must be able to
	 * leave (#111) rather than a spinner.
	 */
	async function findBridge() {
		bridgeKnown = false;
		bridgesTrouble = null;
		unsubscribe?.();
		unsubscribe = null;
		session?.stop();
		session = null;
		const listed = await gateway.GET('/api/bridges').catch(() => null);
		if (listed === null || listed.error !== undefined) {
			bridgesTrouble = troubleOf(listed);
			return;
		}
		bridgeKnown = true;
		bridgeId = listed.data.bridges.find((bridge) => bridge.network === 'sms')?.bridge_id ?? null;
		if (bridgeId !== null) {
			session = new LoginSession(bridgeId);
			unsubscribe = session.state.subscribe((next: LoginState) => (loginState = next));
			if (accepted && !ios) {
				await session.start({ prefer: 'cookies' });
			}
		}
	}

	onDestroy(() => {
		unsubscribe?.();
		session?.stop();
	});

	async function accept() {
		accepted = true;
		try {
			localStorage.setItem(DISCLOSURE_KEY, 'accepted');
		} catch {
			// The card comes back next time, which is safe.
		}
		await session?.start({ prefer: 'cookies' });
	}

	async function submitCookies(event: SubmitEvent) {
		event.preventDefault();
		if (view.kind !== 'cookies') {
			return;
		}
		cookieProblem = null;
		const parsed = parseCookies(pasted);
		if (!parsed.ok) {
			cookieProblem = $t(parsed.reason === 'empty' ? 'sms.cookies.empty' : 'sms.cookies.unreadable');
			return;
		}
		const absent = missing(wanted, parsed.cookies);
		if (absent.length > 0) {
			cookieProblem = $t('sms.cookies.missing', { names: absent.join(', ') });
			return;
		}
		const stepId = view.stepId;
		// Cleared before the request, not after: nothing keeps them on screen
		// while the network is slow, and the parsed jar dies with this call.
		pasted = '';
		await session?.submit(stepId, { cookies: parsed.cookies });
	}

	async function restart() {
		await session?.start({ prefer: 'cookies', takeOver: true });
	}
</script>

<section class="screen" data-testid="screen-sms" data-login-state={view.kind}>
	<header class="stack">
		<p class="small">
			<a href="/networks">
				<Icon name="back" size="dense" />
				{$t('networks.back')}
			</a>
		</p>
		<h1>
			{$t('sms.title')}
			<span class="badge">{$t('networks.preview')}</span>
		</h1>
	</header>

	{#if ios}
		<!-- The wireframe's *iOS user detected* state: the screen is replaced,
		     not decorated, and the way out is a button. -->
		<div class="card card--info" data-testid="sms-ios">
			<p class="card__title">
				<Icon name="phone" size="dense" />
				{$t('sms.ios.title')}
			</p>
			<p>{$t('sms.ios.body')}</p>
			<p>
				<a class="button button--primary" href="/networks">{$t('sms.ios.skip')}</a>
			</p>
		</div>
	{:else if bridgesTrouble !== null}
		<!-- #111: a Gateway that would not answer is not a login in progress —
		     and a Gateway that answered is not one that could not be reached. -->
		<div
			class="card card--warning"
			data-testid="bridges-unreadable"
			data-trouble={bridgesTrouble}
			role="alert"
		>
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('networks.bridgesUnreadable.title')}
			</p>
			<p>
				{#if bridgesTrouble === 'unreachable'}
					{$t('api.trouble.unreachable')}
				{:else if bridgesTrouble === 'session-refused'}
					{$t('api.trouble.sessionRefused')}
				{:else}
					{$t('api.trouble.refused')}
				{/if}
			</p>
			{#if bridgesTrouble !== 'session-refused'}
				<p>
					<button class="button button--primary" type="button" onclick={findBridge} data-testid="retry-bridges">
						<Icon name="reload" size="dense" />
						{$t('networks.retry')}
					</button>
				</p>
			{/if}
		</div>
	{:else if bridgeKnown && bridgeId === null}
		<div class="card card--warning" data-testid="no-bridge">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('networks.noBridge.title')}
			</p>
			<p>{$t('networks.noBridge.body', { network: $t('network.sms.name') })}</p>
		</div>
	{:else if !accepted}
		<div class="card card--warning" data-testid="disclosure">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('sms.disclosure.title')}
			</p>
			<p>{$t('sms.disclosure.body')}</p>
			<div class="actions">
				<button class="button button--primary" type="button" onclick={accept} data-testid="accept-disclosure">
					{$t('qr.disclosure.confirm')}
				</button>
				<a
					class="button button--secondary"
					href="https://github.com/linagora/twalk/blob/main/docs/architecture/adr/0004-twake-sms-companion-first-party-app.md"
					target="_blank"
					rel="noreferrer noopener"
				>
					{$t('qr.disclosure.learnMore')}
					<Icon name="external-link" size="dense" />
				</a>
			</div>
		</div>
	{:else if loginState.conflict !== null}
		<div class="card card--warning" data-testid="login-in-flight">
			<p class="card__title">
				<Icon name="device" size="dense" />
				{$t('networks.conflict.title')}
			</p>
			<p>
				{$t('networks.conflict.body', {
					device: loginState.conflict.deviceName,
					started: loginState.conflict.startedAt
				})}
			</p>
			<div class="actions">
				<button class="button button--primary" type="button" onclick={restart} data-testid="take-over">
					{$t('networks.conflict.takeOver')}
				</button>
				<a class="button button--secondary" href="/networks">{$t('networks.conflict.leaveIt')}</a>
			</div>
		</div>
	{:else}
		<p class="card card--info">
			<Icon name="phone" size="dense" />
			{$t('sms.requirement')}
		</p>

		{#if loginState.trouble !== null}
			<p class="card card--warning" role="alert" data-testid="login-trouble">
				{loginState.trouble === 'unauthenticated'
					? $t('networks.trouble.unauthenticated')
					: loginState.trouble === 'bridge_unreachable' || loginState.trouble === 'bridge_refused'
						? $t('networks.trouble.bridge')
						: $t('networks.trouble.unexpected')}
			</p>
		{/if}

		{#if view.kind === 'cookies' || view.kind === 'starting' || view.kind === 'idle'}
			<section class="stack">
				<h2>{$t('sms.step1.title')}</h2>
				<ol class="steps">
					<li>{$t('sms.step1.a')}</li>
					<li>{$t('sms.step1.b')}</li>
					<li>{$t('sms.step1.c')}</li>
				</ol>
			</section>

			<section class="stack">
				<h2>{$t('sms.step2.title')}</h2>
				<p>{$t('sms.step2.why')}</p>

				<div class="card card--warning">
					<p class="card__title">
						<Icon name="cookie" size="dense" />
						{$t('sms.step2.requirements')}
					</p>
					<ul class="bullets">
						<li>{$t('sms.step2.privateWindow')}</li>
						<li>{$t('sms.step2.dbsc')}</li>
					</ul>
				</div>

				<ol class="steps">
					<li>
						{$t('sms.step2.a')}
						<a href={signInUrl} target="_blank" rel="noreferrer noopener">
							{signInUrl}
							<Icon name="external-link" size="dense" />
						</a>
					</li>
					<li>{$t('sms.step2.b')}</li>
					<li>{$t('sms.step2.c')}</li>
				</ol>

				{#if wanted.length > 0}
					<p class="small">
						{$t('sms.cookies.wanted', { count: wanted.length })}
					</p>
					<ul class="names" data-testid="cookie-names">
						{#each wanted as name (name)}
							<li class="mono">{name}</li>
						{/each}
					</ul>
				{/if}

				<p class="card card--info small">{$t('sms.cookies.whatTheyAre')}</p>

				<form class="field" onsubmit={submitCookies}>
					<label class="label" for="cookies">{$t('sms.cookies.label')}</label>
					<textarea
						id="cookies"
						class="input paste"
						rows="5"
						spellcheck="false"
						autocapitalize="none"
						autocomplete="off"
						placeholder={'SID=…; HSID=…; SSID=…'}
						bind:value={pasted}
						disabled={view.kind !== 'cookies'}
						data-testid="cookie-paste"
					></textarea>
					<p class="error-text" role="alert" aria-live="polite">
						{#if cookieProblem !== null}{cookieProblem}{/if}
					</p>
					<button
						class="button button--primary"
						type="submit"
						disabled={view.kind !== 'cookies' || pasted.trim() === ''}
						data-testid="submit-cookies"
					>
						{$t('sms.cookies.submit')}
					</button>
					<p class="small muted">{$t('sms.cookies.neverStored')}</p>
				</form>
			</section>
		{:else if view.kind === 'emoji'}
			<!-- The pairing step: the same blocking step the QR screens poll,
			     with an emoji instead of a code. Nothing to submit. -->
			<div class="stack pairing" data-testid="sms-emoji" aria-live="polite">
				<p class="emoji" aria-hidden="true">{view.emoji}</p>
				<p class="visually-hidden">{$t('sms.emoji.alt', { emoji: view.emoji })}</p>
				<p>{$t('sms.emoji.body')}</p>
				<p class="muted">
					<span class="spinner" aria-hidden="true"></span>
					{$t('sms.emoji.waiting')}
				</p>
			</div>
		{:else if view.kind === 'verifying'}
			<p class="muted" data-testid="verifying">
				<span class="spinner" aria-hidden="true"></span>
				{$t('networks.verifying')}
			</p>
		{:else if view.kind === 'complete'}
			<div class="card card--info" data-testid="login-complete">
				<p class="card__title">
					<Icon name="ok" size="dense" />
					{$t('sms.success')}
				</p>
				<p>{$t('sms.migration')}</p>
				<p>
					<a class="button button--primary" href="/networks">{$t('networks.continue')}</a>
				</p>
			</div>
		{:else if view.kind === 'failed' || view.kind === 'cancelled'}
			<div class="card card--warning" data-testid="login-failed" data-error-code={view.kind === 'failed' ? view.code : 'cancelled'} role="alert">
				<p>
					{#if view.kind === 'cancelled'}
						{$t('networks.cancelled')}
					{:else if view.code === 'login_lost'}
						{$t('networks.failed.loginLost')}
					{:else if view.code === 'login_expired'}
						{$t('networks.failed.expired')}
					{:else}
						{$t('sms.failed.google')}
					{/if}
				</p>
				<button class="button button--primary" type="button" onclick={restart} data-testid="retry">
					<Icon name="reload" size="dense" />
					{$t('networks.retry')}
				</button>
			</div>
		{/if}
	{/if}

	<details class="small" data-testid="troubleshooting">
		<summary>{$t('networks.troubleshooting')}</summary>
		<ul>
			<li>{$t('sms.trouble1')}</li>
			<li>{$t('sms.trouble2')}</li>
			<li>{$t('sms.trouble3')}</li>
		</ul>
	</details>
</section>

<style>
	h1 {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		flex-wrap: wrap;
	}

	h2 {
		font-size: var(--text-lg);
	}

	.badge {
		font-size: var(--text-xs);
		padding: 2px var(--space-2);
		border-radius: var(--radius-pill);
		background: var(--color-warning-surface);
		border: 1px solid var(--color-warning);
		color: var(--color-text);
		font-weight: var(--font-weight-body);
	}

	.steps,
	.bullets {
		margin: 0;
		padding-inline-start: var(--space-4);
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		color: var(--color-text-muted);
	}

	.names {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.names li {
		background: var(--color-surface);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-sm);
		padding: 2px var(--space-2);
		font-size: var(--text-sm);
	}

	.paste {
		font-family: var(--font-mono);
		font-size: var(--text-sm);
		resize: vertical;
		min-height: 6rem;
	}

	.pairing {
		align-items: center;
		text-align: center;
	}

	.emoji {
		font-size: 4rem;
		line-height: 1;
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	details ul {
		margin: var(--space-2) 0 0;
		padding-inline-start: var(--space-4);
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		color: var(--color-text-muted);
	}
</style>
