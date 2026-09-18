<!--
	Screen 3d of `docs/wireframes/companion-v0.1.md`: connect an existing Matrix
	account.

	The sovereign path (ADR 0009). No bridge is involved: the user already has
	an account, and Twalk's job is to be invited into the rooms they choose.

	# Where each secret goes

	The Matrix session is made in this page, against the user's own homeserver.
	Its access token stays here and crosses exactly once, as the parameter of
	`POST /api/bootstrap/rooms` — the Gateway reads the Sensor's membership in
	the rooms named, invites it where it is absent, and forgets the token when
	the call returns (ADR 0011). Nothing writes it to storage, so a reload loses
	it and the user signs in again rather than finding it lying about.

	The room **list** is read here too, and never sent anywhere. The Gateway
	learns the ids the user ticked and no others, which is the acceptance
	criterion this screen exists for.

	# Names that cannot be read

	A room's name is unencrypted state, so `m.room.name` is readable without the
	crypto stack. What is not is the name of a room that has none — a direct
	message, which every Matrix client computes from its members' profiles, and
	those need member state this listing deliberately does not fetch. Such a
	room is shown by what is actually known about it and labelled as such,
	rather than rendered blank (`$lib/matrix/rooms.ts`).

	# The field starts empty, and on purpose

	It used to arrive filled with the Twalk deployment's own homeserver — from
	the homeserver onboarding had resolved, or from discovery on the Twalk
	domain — which is the one account this screen is not for (#124). The
	consequence was not cosmetic: the screen reads the login flows of whatever
	is in the field, so the local deployment's password form was offered to an
	owner whose real account signs in through their organisation's identity
	provider, and nothing on the screen suggested their homeserver was even
	supported.

	So nothing guesses. The field is empty on a first visit, it accepts **a
	server name or an address** — `.well-known` delegation is what Matrix has
	for exactly this, and `linagora.com` is what a person can recite — and the
	only value ever offered is the homeserver of an account this browser has
	already connected here.

	# Two refusals, two places

	This screen has two actions with a page between them, so it has two message
	surfaces and they are not interchangeable (#139). `problem` answers signing
	in and listing, and renders by the sign-in controls at the top.
	`inviteProblem` answers the invitation, and renders **against the button** — which on
	an account with a hundred rooms is three thousand pixels further down. The
	owner who reported "nothing happens" had been told, at the top of a page
	they were at the bottom of.
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import ActionProblem from '$lib/components/ActionProblem.svelte';
	import { gateway } from '$lib/api/client';
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import type { MatrixSession } from '$lib/crypto/bootstrap';
	import { discoverHomeserver } from '$lib/matrix/discovery';
	import { takeLoginToken } from '$lib/matrix/login-token';
	import {
		loginFlows,
		loginWithPassword,
		loginWithToken,
		MatrixLoginError,
		ssoRedirectUrl,
		type LoginFlows
	} from '$lib/matrix/login';
	import {
		listRooms,
		matchesQuery,
		resolveDisplayNames,
		roomLabel,
		type RoomSummary
	} from '$lib/matrix/rooms';
	import { get } from 'svelte/store';
	import { homeserver, matrixSession, restoreHomeserver } from '$lib/onboarding/progress';

	type Stage = 'signing-in' | 'rooms' | 'inviting' | 'invited';

	let stage = $state<Stage>('signing-in');
	/** What the user typed: a server name, or a homeserver's address. */
	let typed = $state('');
	/** The homeserver discovery resolved it to, or `''` before it has. */
	let baseUrl = $state('');
	/** Whether `.well-known` sent us somewhere other than what was typed. */
	let delegated = $state(false);
	let resolving = $state(false);
	let flows = $state<LoginFlows | null>(null);
	let username = $state('');
	let password = $state('');
	let busy = $state(false);
	/** Signing in and listing: answered by the controls at the top. */
	let problem = $state<string | null>(null);
	/** The invitation: answered at the invite button, wherever that is (#139). */
	let inviteProblem = $state<string | null>(null);
	let session = $state<MatrixSession | null>(null);
	let rooms = $state<RoomSummary[]>([]);
	let selected = $state<Set<string>>(new Set());
	let outcomes = $state<{ room_id: string; status: string; reason?: string }[]>([]);
	let sensor = $state<string | null>(null);

	/** Display names the homeserver gave for the heroes of nameless rooms. */
	let directory = $state<Map<string, string>>(new Map());
	let query = $state('');

	const anySelected = $derived(selected.size > 0);
	const shown = $derived(rooms.filter((room) => matchesQuery(room, query, directory)));
	/**
	 * Whether every room the filter is showing is already chosen.
	 *
	 * The bulk control acts on **what is on screen**, never on the whole
	 * account. On a work account this list is colleagues' private
	 * conversations, and a control that selects all of them in one click is
	 * how sixty people end up observed without anyone deciding to (#122).
	 */
	const allShownChosen = $derived(shown.length > 0 && shown.every((room) => selected.has(room.roomId)));

	onMount(async () => {
		// Which homeserver this deployment drives, so the screen can refuse a
		// foreign one before asking for a credential rather than after.
		restoreHomeserver();

		// The account the user has just created in the bootstrap journey is
		// already a Matrix account they own: offer it rather than asking them
		// to type a password they set two screens ago. This is a session the
		// user made minutes ago in this browser, not a guess about which
		// homeserver they meant.
		const live = $matrixSession;
		if (live !== null) {
			await useSession(live);
			return;
		}

		// Coming back from the homeserver's SSO page. The token was taken out
		// of the address bar by the root layout, before any screen decided
		// what to render: stripping it here meant not stripping it at all on a
		// load where this route never mounted (#135, and #125 before it).
		const token = takeLoginToken();
		if (token !== null) {
			// The token is exchanged against the homeserver that issued it, or
			// not at all. Falling back to whatever this screen happens to know
			// is how a linagora.com token was presented to twalk.localhost.
			let issuer: string | null = null;
			try {
				issuer = window.localStorage.getItem(SSO_HOMESERVER_KEY);
				window.localStorage.removeItem(SSO_HOMESERVER_KEY);
			} catch {
				issuer = null;
			}
			if (issuer === null || issuer === '') {
				problem = $t('matrix.error.ssoLost');
				return;
			}
			baseUrl = issuer;
			busy = true;
			try {
				await useSession(await loginWithToken(issuer, token));
				return;
			} catch (error) {
				problem = messageFor(error);
			} finally {
				busy = false;
			}
		}

		// The one value this screen ever offers: the homeserver of an account
		// it has already connected in this browser. Nothing is inferred from
		// the Twalk deployment, whose homeserver is the one account this
		// screen is not for (#124).
		const known = knownHomeserver();
		if (known !== null) {
			typed = known;
			await resolveHomeserver();
		}
	});

	/** The last value discovery was run for, so a blur does not re-run it. */
	let resolvedFrom = '';

	/**
	 * Turns what the user typed into a homeserver, then reads its login flows.
	 *
	 * Discovery rather than a base URL binding: `.well-known` delegation is
	 * how a person gets to type `linagora.com` — their own server name —
	 * instead of `https://matrix.linagora.com`, and it is also what tells us,
	 * before any credential is asked for, that there is a homeserver there at
	 * all. An address is still accepted, unchanged.
	 */
	/**
	 * The deployment's own homeserver, when this browser knows it.
	 *
	 * v0.1 observes only an account on it (ADR 0020). The screen asks the
	 * deployment rather than assuming, and it asks **before** any credential:
	 * the owner drove this whole journey against their corporate homeserver —
	 * `.well-known` delegation, SSO through their identity provider, the room
	 * listing — and it failed at the last step, after they had signed in and
	 * chosen a room (#138). A screen that cannot do a thing must say so before
	 * it takes something from you, not after.
	 */
	let foreignHomeserver = $state<string | null>(null);

	async function resolveHomeserver() {
		const value = typed.trim();
		if (resolving || value === '') {
			return;
		}
		if (value === resolvedFrom) {
			// A blur that changed nothing asks the homeserver nothing.
			return;
		}
		resolving = true;
		problem = null;
		flows = null;
		baseUrl = '';
		try {
			const found = await discoverHomeserver(value);
			if (!found.ok) {
				// Which of the two failures it was is `found.kind`; both read
				// the same to a user who mistyped their server name, and the
				// message names the two spellings that work.
				problem = $t('matrix.error.notFound', { domain: value });
				return;
			}
			resolvedFrom = value;

			// Asked before anything is offered. A homeserver this deployment
			// does not drive cannot be observed at all in this version: the
			// Sensor would have to federate into rooms it does not host
			// (ADR 0020), and no amount of signing in changes that.
			//
			// Compared as **base URLs**, and only where the delegation landed.
			//
			// Three things were tried here and two were wrong, so the reasoning
			// is written down. Comparing the typed name against the
			// deployment's server name refuses `delegated.test` even when it
			// delegates to this very deployment — what matters is where the
			// delegation *lands*, not what was typed. And comparing a typed
			// address against a server name is the mistake #130 cost an
			// evening: `127.0.0.1:8009` is where a homeserver answers,
			// `test.twalk` is who it is.
			//
			// A client cannot learn a homeserver's server name before signing
			// in — Matrix offers no such endpoint. So the comparison is
			// between two browser-reachable base URLs, and when this browser
			// does not know its own deployment's, there is no question to ask
			// and nothing is refused: the failure then arrives at the
			// invitation, as it did before.
			const ours = get(homeserver);
			if (ours !== '' && found.homeserver.baseUrl !== ours) {
				foreignHomeserver = value;
				flows = null;
				baseUrl = '';
				return;
			}
			foreignHomeserver = null;

			baseUrl = found.homeserver.baseUrl;
			delegated = found.homeserver.delegated;
			await readFlows();
		} finally {
			resolving = false;
		}
	}

	async function readFlows() {
		problem = null;
		try {
			flows = await loginFlows(baseUrl);
		} catch (error) {
			flows = null;
			problem = messageFor(error);
		}
	}

	async function useSession(next: MatrixSession) {
		session = next;
		matrixSession.set(next);
		baseUrl = next.baseUrl;
		typed = next.baseUrl;
		resolvedFrom = next.baseUrl;
		rememberHomeserver(next.baseUrl);
		stage = 'rooms';
		busy = true;
		try {
			rooms = await listRooms(next.baseUrl, next.accessToken);
			// Names for the rooms that have none, asked of the homeserver
			// afterwards: the list is usable without them and never waits.
			const heroes = rooms.filter((room) => room.name === null && room.alias === null).flatMap((room) => room.heroes);
			if (heroes.length > 0) {
				void resolveDisplayNames(next.baseUrl, next.accessToken, heroes).then((found) => {
					directory = found;
				});
			}
		} catch (error) {
			problem = messageFor(error);
		} finally {
			busy = false;
		}
	}

	async function signInWithPassword(event: SubmitEvent) {
		event.preventDefault();
		if (busy) {
			return;
		}
		busy = true;
		problem = null;
		try {
			await useSession(await loginWithPassword(baseUrl, username, password));
		} catch (error) {
			problem = messageFor(error);
		} finally {
			// The password is not kept a moment longer than the request needs.
			password = '';
			busy = false;
		}
	}

	/**
	 * Which homeserver an SSO round trip was started against.
	 *
	 * A redirect is a full page load, so component state does not come back.
	 * This screen used to rebuild the homeserver from what onboarding had
	 * remembered — the *deployment's* server — and hand a login token issued
	 * by one homeserver to a different one (#125, found against a real
	 * corporate homeserver). A login token is a single-use credential issued
	 * by one server for that server.
	 *
	 * In `localStorage` rather than `sessionStorage` because an identity
	 * provider may answer in a new tab, and the token is worthless without the
	 * server that issued it.
	 */
	const SSO_HOMESERVER_KEY = 'twalk:networks:sso-homeserver';

	/**
	 * The homeserver of the Matrix account this browser has already connected
	 * here — the only value this screen ever offers in its field.
	 *
	 * Not onboarding's homeserver, which is the Twalk deployment's own and the
	 * one account this screen is not for (#124). A public address and nothing
	 * else: the session it belonged to is in memory and is lost on a reload,
	 * by design (ADR 0011).
	 */
	const KNOWN_HOMESERVER_KEY = 'twalk:networks:matrix-account-homeserver';

	function knownHomeserver(): string | null {
		try {
			const stored = window.localStorage.getItem(KNOWN_HOMESERVER_KEY);
			return stored !== null && stored !== '' ? stored : null;
		} catch {
			return null;
		}
	}

	function rememberHomeserver(value: string): void {
		try {
			window.localStorage.setItem(KNOWN_HOMESERVER_KEY, value);
		} catch {
			// Storage is off: the user types it again next time, which is the
			// behaviour of a first visit and is never wrong.
		}
	}

	/** Where the homeserver is asked to send the browser back. */
	const returnUrl = $derived(
		typeof window === 'undefined' ? '' : `${window.location.origin}/networks/matrix`
	);

	function startSso(idpId?: string) {
		try {
			window.localStorage.setItem(SSO_HOMESERVER_KEY, baseUrl);
		} catch {
			// Storage is off: say so rather than starting a round trip whose
			// return this screen would have to refuse.
			problem = $t('matrix.error.ssoNeedsStorage');
			return;
		}
		window.location.href = ssoRedirectUrl(baseUrl, returnUrl, idpId);
	}

	/** Chooses, or unchooses, every room the filter is currently showing. */
	function toggleShown() {
		const next = new Set(selected);
		if (allShownChosen) {
			for (const room of shown) {
				next.delete(room.roomId);
			}
		} else {
			for (const room of shown) {
				next.add(room.roomId);
			}
		}
		selected = next;
	}

	function toggle(roomId: string) {
		const next = new Set(selected);
		if (next.has(roomId)) {
			next.delete(roomId);
		} else {
			next.add(roomId);
		}
		selected = next;
	}

	async function inviteSensor() {
		if (session === null || !anySelected) {
			return;
		}
		stage = 'inviting';
		inviteProblem = null;
		const answer = await gateway.POST('/api/bootstrap/rooms', {
			body: {
				matrix_access_token: session.accessToken,
				rooms: [...selected]
			}
		});
		if (answer.error !== undefined) {
			stage = 'rooms';
			// The homeserver refused the Matrix token. Two causes, and the
			// user can act on both: the Matrix session expired, or — the one
			// found live — this account is on a homeserver that is not this
			// deployment's, which it cannot invite the Sensor into at all
			// (#138). Saying "the invitation failed" for either was true and
			// useless.
			const code = (answer.error as { error?: string } | undefined)?.error;
			inviteProblem =
				code === 'matrix_token_rejected'
					? $t('matrix.error.tokenRejected')
					: $t('matrix.error.invite');
			return;
		}
		outcomes = answer.data.rooms;
		sensor = answer.data.sensor;
		stage = 'invited';
	}

	function messageFor(error: unknown): string {
		if (error instanceof MatrixLoginError) {
			switch (error.problem) {
				case 'unreachable':
					return $t('matrix.error.unreachable');
				case 'rejected':
					return $t('matrix.error.rejected');
				case 'rate-limited':
					return $t('matrix.error.rateLimited');
				default:
					return $t('matrix.error.refused');
			}
		}
		return $t('matrix.error.refused');
	}
</script>

<section class="screen" data-testid="screen-matrix" data-stage={stage}>
	<header class="stack">
		<p class="small">
			<a href="/networks">
				<Icon name="back" size="dense" />
				{$t('networks.back')}
			</a>
		</p>
		<h1>{$t('matrix.title')}</h1>
		<p class="subtitle">{$t('matrix.caption')}</p>
	</header>

	<!-- The sign-in surface. It sits at the top because the controls it answers
	     do: the homeserver field, the identity providers and the password form
	     are all within a screen of here. The invitation's refusal is not here —
	     it is at the invite button, a hundred rooms down (#139). -->
	<ActionProblem message={problem} testId="matrix-problem" />

	{#if stage === 'signing-in'}
		<!-- One field, empty, with the two spellings it accepts in its own
		     placeholder and hint. It asks the question instead of answering it
		     wrongly (#124), and `.well-known` is what makes the answer a user
		     can recite — their server name — sufficient. -->
		<form
			class="field"
			onsubmit={(event) => {
				event.preventDefault();
				void resolveHomeserver();
			}}
		>
			<label class="label" for="homeserver">{$t('matrix.homeserver')}</label>
			<input
				id="homeserver"
				class="input"
				type="text"
				inputmode="url"
				spellcheck="false"
				autocapitalize="none"
				placeholder={$t('matrix.homeserverPlaceholder')}
				aria-describedby="homeserver-hint"
				bind:value={typed}
				onblur={resolveHomeserver}
				disabled={busy || resolving}
				data-testid="matrix-homeserver"
			/>
			<p class="small muted" id="homeserver-hint">{$t('matrix.homeserverHint')}</p>
			<button
				class="button button--secondary"
				type="submit"
				disabled={busy || resolving || typed.trim() === ''}
				data-testid="matrix-homeserver-continue"
			>
				{#if resolving}
					<span class="spinner" aria-hidden="true"></span>
					{$t('matrix.resolving')}
				{:else}
					{$t('networks.continue')}
				{/if}
			</button>
		</form>

		{#if foreignHomeserver !== null}
			<!--
				Refused before a credential is asked for, which is the whole
				point: the owner completed SSO against their corporate
				homeserver and only then discovered the Sensor could not be
				invited into its rooms (#138, ADR 0020).
			-->
			<div class="card card--warning" role="alert" data-testid="matrix-foreign-homeserver">
				<p class="card__title">
					<Icon name="warning" size="dense" />
					{$t('matrix.foreign.title')}
				</p>
				<p>
					{$t('matrix.foreign.body', {
						wanted: foreignHomeserver,
						deployment: get(homeserver)
					})}
				</p>
				<p class="small muted">{$t('matrix.foreign.why')}</p>
			</div>
		{/if}

		{#if baseUrl !== '' && delegated}
			<!-- Where the delegation led, because the user typed one thing and
			     their credentials are about to go to another. -->
			<p class="small muted" data-testid="matrix-resolved" data-homeserver={baseUrl}>
				{$t('matrix.resolved', { url: baseUrl })}
			</p>
		{/if}

		{#if flows !== null && flows.sso}
			<!-- One button per advertised identity provider, labelled with the
			     provider's own name: a user recognises "Connect with Twake",
			     not a generic "single sign-on". A homeserver that advertises
			     none gets one button and its own chooser page. -->
			<div class="actions">
				{#each flows.identityProviders as provider (provider.id)}
					<button
						class="button button--primary"
						type="button"
						onclick={() => startSso(provider.id)}
						data-testid={`matrix-sso-${provider.id}`}
					>
						<Icon name="account" size="dense" />
						{provider.name}
					</button>
				{/each}
				{#if flows.identityProviders.length === 0}
					<button class="button button--primary" type="button" onclick={() => startSso()} data-testid="matrix-sso">
						<Icon name="account" size="dense" />
						{$t('matrix.sso')}
					</button>
				{/if}
			</div>
			<!-- The constraint #124 asks to be named rather than hidden: the
			     round trip comes back to whatever address this Companion is
			     reached at, and a homeserver with an allow-list of return
			     addresses will refuse one it has never been told about. A user
			     meeting that deserves the sentence, not a generic failure. -->
			<p class="small muted" data-testid="matrix-sso-return">
				{$t('matrix.ssoReturn', { origin: returnUrl })}
			</p>
		{/if}

		{#if flows !== null && flows.password}
			<form class="field" onsubmit={signInWithPassword}>
				<label class="label" for="matrix-user">{$t('matrix.username')}</label>
				<input
					id="matrix-user"
					class="input"
					type="text"
					autocomplete="username"
					spellcheck="false"
					autocapitalize="none"
					bind:value={username}
					disabled={busy}
					data-testid="matrix-username"
				/>
				<label class="label" for="matrix-password">{$t('matrix.password')}</label>
				<input
					id="matrix-password"
					class="input"
					type="password"
					autocomplete="current-password"
					bind:value={password}
					disabled={busy}
					data-testid="matrix-password"
				/>
				<button
					class="button button--primary"
					type="submit"
					disabled={busy || username === '' || password === ''}
					data-testid="matrix-signin"
				>
					{#if busy}
						<span class="spinner" aria-hidden="true"></span>
						<span class="visually-hidden">{$t('matrix.signingIn')}</span>
					{:else}
						{$t('matrix.signIn')}
					{/if}
				</button>
			</form>
		{/if}

		{#if flows !== null && !flows.password && !flows.sso}
			<p class="card card--warning" data-testid="matrix-no-flow">{$t('matrix.noFlow')}</p>
		{/if}

		<!-- The wireframe's third method. It carries the user's secrets across
		     from another client and needs the crypto stack to do it, so it is
		     named rather than shown as a control that would do nothing. -->
		<p class="small muted">{$t('matrix.qrLater')}</p>
	{:else if stage === 'rooms' || stage === 'inviting'}
		<p class="card card--info">
			<Icon name="info" size="dense" />
			{$t('matrix.rooms.intro', { user: session?.userId ?? '' })}
		</p>

		{#if busy}
			<p class="muted" data-testid="matrix-loading">
				<span class="spinner" aria-hidden="true"></span>
				{$t('matrix.rooms.loading')}
			</p>
		{:else if rooms.length === 0}
			<p class="card card--warning" data-testid="matrix-no-rooms">{$t('matrix.rooms.none')}</p>
		{:else}
			<div class="field">
				<label class="label" for="room-search">{$t('matrix.rooms.searchLabel')}</label>
				<input
					id="room-search"
					class="input"
					type="search"
					autocomplete="off"
					spellcheck="false"
					placeholder={$t('matrix.rooms.searchPlaceholder')}
					data-testid="matrix-room-search"
					bind:value={query}
					disabled={stage === 'inviting'}
				/>
			</div>

			<p class="small muted" data-testid="matrix-rooms-count">
				{$t('matrix.rooms.showing', { shown: shown.length, total: rooms.length })}
			</p>

			{#if shown.length > 0}
				<!--
					Named by what it will do, and counted, so "select" can never
					be read as "select everything I have".
				-->
				<p>
					<button
						class="button"
						type="button"
						data-testid="matrix-rooms-toggle-shown"
						disabled={stage === 'inviting'}
						onclick={toggleShown}
					>
						{allShownChosen
							? $t('matrix.rooms.deselectShown', { count: shown.length })
							: $t('matrix.rooms.selectShown', { count: shown.length })}
					</button>
				</p>
			{/if}

			<ul class="rooms" data-testid="matrix-rooms">
				{#each shown as room (room.roomId)}
					{@const label = roomLabel(room, directory)}
					<li>
						<label class="room">
							<input
								type="checkbox"
								checked={selected.has(room.roomId)}
								onchange={() => toggle(room.roomId)}
								disabled={stage === 'inviting'}
								data-testid={`room-${room.roomId}`}
							/>
							<span class="room__text">
								<span class="room__name" class:room__name--derived={label.source !== 'name'}>
									{label.text}
								</span>
								{#if label.source !== 'name'}
									<!-- Honest rather than blank: say what this label is. -->
									<span class="small muted" data-testid="room-unnamed">
										{label.source === 'alias'
											? $t('matrix.rooms.byAlias')
											: label.source === 'heroes'
												? $t('matrix.rooms.byMembers')
												: $t('matrix.rooms.byId')}
									</span>
								{/if}
							</span>
							{#if room.encrypted}
								<span class="badge">
									<Icon name="tab-lock" size="dense" />
									{$t('matrix.rooms.encrypted')}
								</span>
							{/if}
						</label>
					</li>
				{/each}
			</ul>

			<!-- The invitation's answer, immediately above the control that asks
			     for it, so that a user who pressed the button at the bottom of a
			     hundred rooms reads the refusal without hunting for it (#139). -->
			<ActionProblem message={inviteProblem} testId="matrix-invite-problem" />

			<button
				class="button button--primary"
				type="button"
				onclick={inviteSensor}
				disabled={!anySelected || stage === 'inviting'}
				data-testid="invite-sensor"
			>
				{#if stage === 'inviting'}
					<span class="spinner" aria-hidden="true"></span>
					<span class="visually-hidden">{$t('matrix.rooms.inviting')}</span>
				{:else}
					{$t('matrix.rooms.invite', { count: selected.size })}
				{/if}
			</button>
			<p class="small muted">{$t('matrix.rooms.onlySelected')}</p>
		{/if}
	{:else}
		<div class="card card--info" data-testid="matrix-invited">
			<p class="card__title">
				<Icon name="ok" size="dense" />
				{$t('matrix.done.title', { sensor: sensor ?? '' })}
			</p>
			<ul class="outcomes">
				{#each outcomes as outcome (outcome.room_id)}
					<li data-testid={`outcome-${outcome.room_id}`} data-status={outcome.status}>
						<Icon name={outcome.status === 'failed' ? 'error' : 'check'} size="dense" />
						{roomLabel(rooms.find((room) => room.roomId === outcome.room_id) ?? {
							roomId: outcome.room_id,
							name: null,
							alias: null,
							encrypted: false,
							heroes: [],
							joinedMembers: null
						}).text}
						<span class="small muted">
							{outcome.status === 'invited'
								? $t('matrix.done.invited')
								: outcome.status === 'already_present'
									? $t('matrix.done.alreadyPresent')
									: $t('matrix.done.failed')}
						</span>
					</li>
				{/each}
			</ul>
			<p>{$t('matrix.done.body')}</p>
			<p>
				<a class="button button--primary" href="/networks">{$t('networks.continue')}</a>
			</p>
		</div>
	{/if}
</section>

<style>
	.rooms {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.room {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		background: var(--color-surface);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-md);
		padding: var(--space-2) var(--space-3);
		min-height: 48px;
		cursor: pointer;
	}

	.room__text {
		display: flex;
		flex-direction: column;
		min-width: 0;
		flex: 1 1 auto;
	}

	.room__name {
		overflow-wrap: anywhere;
	}

	/* A label the room did not give itself reads as what it is. */
	.room__name--derived {
		color: var(--color-text-muted);
		font-style: italic;
	}

	.badge {
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
		font-size: var(--text-xs);
		color: var(--color-text-subtle);
		white-space: nowrap;
	}

	.outcomes {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.outcomes li {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		flex-wrap: wrap;
	}
</style>
