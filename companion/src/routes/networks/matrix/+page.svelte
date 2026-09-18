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
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import { gateway } from '$lib/api/client';
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import type { MatrixSession } from '$lib/crypto/bootstrap';
	import { discoverHomeserver } from '$lib/matrix/discovery';
	import {
		loginFlows,
		loginTokenFrom,
		loginWithPassword,
		loginWithToken,
		MatrixLoginError,
		ssoRedirectUrl,
		type LoginFlows
	} from '$lib/matrix/login';
	import { listRooms, roomLabel, type RoomSummary } from '$lib/matrix/rooms';
	import { domain, restoreDomain } from '$lib/onboarding/domain';
	import { homeserver, matrixSession, restoreHomeserver } from '$lib/onboarding/progress';

	type Stage = 'signing-in' | 'rooms' | 'inviting' | 'invited';

	let stage = $state<Stage>('signing-in');
	let baseUrl = $state('');
	let flows = $state<LoginFlows | null>(null);
	let username = $state('');
	let password = $state('');
	let busy = $state(false);
	let problem = $state<string | null>(null);
	let session = $state<MatrixSession | null>(null);
	let rooms = $state<RoomSummary[]>([]);
	let selected = $state<Set<string>>(new Set());
	let outcomes = $state<{ room_id: string; status: string; reason?: string }[]>([]);
	let sensor = $state<string | null>(null);

	const anySelected = $derived(selected.size > 0);

	onMount(async () => {
		restoreDomain();
		const remembered = restoreHomeserver();
		baseUrl = remembered !== '' ? remembered : '';

		// The account the user has just created in the bootstrap journey is
		// already a Matrix account they own: offer it rather than asking them
		// to type a password they set two screens ago.
		const live = $matrixSession;
		if (live !== null) {
			await useSession(live);
			return;
		}

		if (baseUrl === '' && $domain !== '') {
			const found = await discoverHomeserver($domain);
			baseUrl = found.ok ? found.homeserver.baseUrl : `https://${$domain}`;
		}

		// Coming back from the homeserver's SSO page.
		const token = loginTokenFrom(new URL(window.location.href));
		if (token !== null) {
			// Out of the address bar before anything else, and whatever
			// happens next: a login token is a credential, and a copied URL
			// must not carry one. Previously this ran only on the path that
			// went on to use the token, so a round trip that could not be
			// completed left the credential in the address bar (#125).
			const clean = new URL(window.location.href);
			clean.searchParams.delete('loginToken');
			history.replaceState(null, '', clean.toString());

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

		if (baseUrl !== '') {
			await readFlows();
		}
	});

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
		stage = 'rooms';
		busy = true;
		try {
			rooms = await listRooms(next.baseUrl, next.accessToken);
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

	function startSso(idpId?: string) {
		try {
			window.localStorage.setItem(SSO_HOMESERVER_KEY, baseUrl);
		} catch {
			// Storage is off: say so rather than starting a round trip whose
			// return this screen would have to refuse.
			problem = $t('matrix.error.ssoNeedsStorage');
			return;
		}
		window.location.href = ssoRedirectUrl(
			baseUrl,
			`${window.location.origin}/networks/matrix`,
			idpId
		);
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
		problem = null;
		const answer = await gateway.POST('/api/bootstrap/rooms', {
			body: {
				matrix_access_token: session.accessToken,
				rooms: [...selected]
			}
		});
		if (answer.error !== undefined) {
			stage = 'rooms';
			problem = $t('matrix.error.invite');
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

	{#if problem !== null}
		<p class="card card--warning" role="alert" data-testid="matrix-problem">{problem}</p>
	{/if}

	{#if stage === 'signing-in'}
		<div class="field">
			<label class="label" for="homeserver">{$t('matrix.homeserver')}</label>
			<input
				id="homeserver"
				class="input"
				type="text"
				inputmode="url"
				spellcheck="false"
				autocapitalize="none"
				bind:value={baseUrl}
				onblur={readFlows}
				disabled={busy}
				data-testid="matrix-homeserver"
			/>
		</div>

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
			<ul class="rooms" data-testid="matrix-rooms">
				{#each rooms as room (room.roomId)}
					{@const label = roomLabel(room)}
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
