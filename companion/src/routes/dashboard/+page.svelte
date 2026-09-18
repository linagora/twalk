<!--
	Screen 5 of `docs/wireframes/companion-v0.1.md`: the home screen the user
	returns to.

	The wireframe's three states are all here — the calm all-green layout, the
	amber "your WhatsApp session expired" banner with its Reconnect action, and
	the pending-decisions chip — and so is the one correction that matters most
	(#74, #69): **the activity feed is operational only**. Bridge state, consent
	decisions, persona activity, plus a message count. Never who wrote. The feed
	as originally designed is the list of the user's correspondents, rendered on
	a home screen that is unlocked in public, in a product whose argument is
	sovereignty. The rule is enforced in `$lib/dashboard/model.ts`, where it can
	be tested, not in this markup.

	Three things this screen refuses to imply, because they are not true of the
	deployment as it stands:

	  - an **active agent produces nothing** today. Hermes is not implemented
	    (#21–#25) — only its test harness landed — so the persona rows carry
	    that, and pausing one is explained as *starved, not stopped* (ADR 0013).
	  - the bridge dots read the **last login** each bridge holds, not a
	    heartbeat, and nothing reports the time of the last message. #56 has
	    landed the producer — bridges push their state and the Gateway
	    publishes `bridge.status.changed.v1` — but no read on this origin
	    serves it to a browser, so both facts are said on the screen rather
	    than drawn as a confident green.
	The pending-decisions chip is a real number now: #54's projection over the
	inbound stream answers `GET /api/contacts/pending`, and the chip reads its
	`total`. Its `contacts` array — the list of who has written and not been
	decided about — is dropped in `$lib/dashboard/load.ts` and never reaches
	this file. What it has no destination for yet is the tap: the consent inbox
	is v0.2, so the chip says where the decisions are not, instead of pretending
	to lead somewhere.

	The approval chip (#100) is the same idea one more time: `GET /api/suggestions`
	answers with every proposed reply in full, and this screen is allowed a
	number and a link. The reduction happens in `$lib/dashboard/load.ts`, so no
	component here could render a persona's words even by accident; the content
	lives at `/approvals`, opened deliberately.

	The version handshake is not repeated here: `$lib/boot.ts` runs it before
	any screen renders and `VersionBanner` (in the root layout) is what tells
	the user the Gateway moved under an installed shell. The footer names the
	version this deployment answers, which is the same handshake's other half.
-->
<script lang="ts">
	import { onMount, onDestroy } from 'svelte';

	import ActionProblem from '$lib/components/ActionProblem.svelte';
	import Icon from '$lib/icons/Icon.svelte';
	import { boot } from '$lib/boot';
	import { locale, t } from '$lib/i18n';
	import { cardFor } from '$lib/networks/catalogue';
	import { PERSONA_CARDS } from '$lib/personas/catalogue';
	import { decideOnPersona } from '$lib/personas/activation';
	import { loadDashboard, revokeDevice, EMPTY, type Snapshot } from '$lib/dashboard/load';
	import { relativeTime } from '$lib/dashboard/format';
	import {
		activityFeed,
		bridgeRows,
		expiredBridges,
		messageCount,
		overallHealth,
		pendingDecisions,
		personaRows,
		type BridgeRow,
		type PersonaRow
	} from '$lib/dashboard/model';

	/** How often the screen re-reads. There is no event stream to subscribe to. */
	const POLL_MS = 15_000;

	let snapshot = $state<Snapshot>(EMPTY);
	let loaded = $state(false);
	let refreshing = $state(false);
	/** Re-read on a tick, for the relative times. */
	let now = $state(Date.now());
	let busyPersona = $state<string | null>(null);
	let personaError = $state<string | null>(null);
	let busyDevice = $state<string | null>(null);
	let deviceError = $state<string | null>(null);

	let timer: ReturnType<typeof setInterval> | null = null;

	onMount(() => {
		void refresh();
		timer = setInterval(() => {
			now = Date.now();
			void refresh();
		}, POLL_MS);
	});

	onDestroy(() => {
		if (timer !== null) {
			clearInterval(timer);
		}
	});

	async function refresh() {
		refreshing = true;
		snapshot = await loadDashboard();
		now = Date.now();
		loaded = true;
		refreshing = false;
	}

	const bridges = $derived<BridgeRow[]>(bridgeRows(snapshot.bridges ?? []));
	const personas = $derived<PersonaRow[]>(
		personaRows(
			snapshot.consent ?? [],
			PERSONA_CARDS.map((card) => card.id)
		)
	);
	const expired = $derived(expiredBridges(bridges));
	const pending = $derived(pendingDecisions(snapshot.pending));
	const messages = $derived(messageCount());
	const feed = $derived(
		activityFeed({
			bridges: snapshot.bridges ?? [],
			consent: snapshot.consent ?? [],
			devices: snapshot.devices ?? []
		})
	);
	const health = $derived(
		overallHealth({ bridges, personas, gatewayReachable: snapshot.reachable })
	);
	const gatewayVersion = $derived($boot.health?.version ?? null);

	function networkLabel(network: string): string {
		const key = cardFor(network)?.titleKey;
		return key === undefined ? network : $t(key);
	}

	function ago(iso: string | null): string | null {
		return relativeTime(iso, now, $locale);
	}

	/**
	 * A feed row's interpolations, with the network turned into the name the
	 * user reads on screen 3. The model keeps the contract's own value
	 * (`whatsapp`), because that is what it reasons about; the name is a
	 * rendering decision and belongs here.
	 */
	function feedValues(entry: { values: Record<string, string> }): Record<string, string> {
		const values = { ...entry.values };
		if (values.network !== undefined) {
			values.network = networkLabel(values.network);
		}
		return values;
	}

	/**
	 * The active/paused toggle. Activating re-uses the networks the user
	 * already decided on for this persona; a persona that was never decided on
	 * has no perimeter to re-use, so the row sends the user to screen 4 rather
	 * than choosing one for them (ADR 0013: activation never spreads).
	 */
	async function togglePersona(row: PersonaRow) {
		if (busyPersona !== null) {
			return;
		}
		personaError = null;
		busyPersona = row.persona;
		const result = await decideOnPersona({
			persona: row.persona,
			state: row.active ? 'revoked' : 'granted',
			networks: row.active ? row.networks : row.decidedNetworks
		});
		busyPersona = null;
		if (!result.ok) {
			personaError =
				result.failure.kind === 'refused' ? result.failure.error : result.failure.kind;
		}
		await refresh();
	}

	async function revoke(id: string, current: boolean) {
		if (busyDevice !== null) {
			return;
		}
		if (current && !window.confirm($t('dashboard.devices.confirmCurrent'))) {
			return;
		}
		deviceError = null;
		busyDevice = id;
		const error = await revokeDevice(id);
		busyDevice = null;
		if (error !== null) {
			deviceError = error;
		}
		await refresh();
	}
</script>

<section class="screen" data-testid="screen-dashboard" data-health={health}>
	<header class="head">
		<h1>{$t('dashboard.title')}</h1>
		<button
			class="button button--secondary"
			type="button"
			onclick={refresh}
			disabled={refreshing}
			data-testid="refresh"
		>
			<Icon name="reload" size="dense" />
			{refreshing ? $t('dashboard.refreshing') : $t('dashboard.refresh')}
		</button>
	</header>

	{#if loaded && !snapshot.reachable}
		<p class="card card--warning small" role="alert" data-testid="dashboard-unreachable">
			{$t('dashboard.unreachable')}
		</p>
	{/if}

	{#each expired as row (row.bridgeId)}
		<div class="card card--warning" role="alert" data-testid={`expired-${row.network}`}>
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('dashboard.banner.expired.title', { network: networkLabel(row.network) })}
			</p>
			<p class="small">{$t('dashboard.banner.expired.body')}</p>
			<p>
				<a class="button button--primary" href={row.route} data-testid="reconnect">
					{$t('dashboard.banner.expired.cta')}
					<Icon name="continue" size="dense" />
				</a>
			</p>
		</div>
	{/each}

	<!-- #100's count and link, and deliberately nothing else: what the
	     assistant proposed is on `/approvals`, opened on purpose. The text
	     never reaches this file — `$lib/dashboard/load.ts` drops it at the
	     seam, the same way it drops the pending contacts' identities. -->
	{#if snapshot.waiting !== null && snapshot.waiting.count > 0}
		<div class="waiting" data-testid="approvals-waiting">
			<p class="chip" data-testid="approvals-chip" data-count={snapshot.waiting.count}>
				<Icon name="persona" size="dense" />
				{$t('dashboard.chip.suggestions', { count: snapshot.waiting.count })}
			</p>
			{#if snapshot.waiting.atLeast}
				<p class="small muted" data-testid="approvals-at-least">
					{$t('dashboard.chip.suggestionsAtLeast')}
				</p>
			{/if}
			<p class="small muted" data-testid="approvals-private">
				{$t('dashboard.chip.suggestionsPrivate')}
			</p>
			<p>
				<a class="button button--primary" href="/approvals" data-testid="to-approvals">
					{$t('dashboard.chip.suggestionsLink')}
					<Icon name="continue" size="dense" />
				</a>
			</p>
		</div>
	{/if}

	{#if pending !== null && pending > 0}
		<div class="waiting" data-testid="pending-waiting">
			<p class="chip" data-testid="pending-chip" data-count={pending}>
				<Icon name="consent" size="dense" />
				{$t('dashboard.chip.pending', { count: pending })}
			</p>
			<!-- A count, and where to act on it: nowhere yet. The searchable
			     consent inbox is v0.2, and a chip that led to a screen that does
			     not exist would be worse than one that says so. -->
			<p class="small muted" data-testid="pending-no-inbox">{$t('dashboard.chip.noInbox')}</p>
		</div>
	{/if}

	<div class="card" data-testid="system-health">
		<p class="card__title">
			<span class="dot" data-tone={health === 'ok' ? 'ok' : health === 'setup' ? 'idle' : 'warn'}
			></span>
			<span data-testid="health-label">
				{health === 'ok'
					? $t('dashboard.health.ok')
					: health === 'setup'
						? $t('dashboard.health.setup')
						: $t('dashboard.health.attention')}
			</span>
		</p>
		{#if health === 'setup'}
			<p class="small muted">{$t('dashboard.health.setupDetail')}</p>
		{/if}
	</div>

	<section class="card" data-testid="bridges">
		<p class="card__title">
			<Icon name="bridge" size="dense" />
			{$t('dashboard.bridges.title')}
		</p>
		{#if bridges.length === 0}
			<p class="small muted">{$t('dashboard.bridges.none')}</p>
		{:else}
			<ul class="rows">
				{#each bridges as row (row.bridgeId)}
					<li class="row" data-testid={`bridge-${row.network}`} data-state={row.state}>
						<span class="dot" data-tone={row.tone}></span>
						<span class="row__text">
							<span class="row__name">{networkLabel(row.network)}</span>
							<span class="small muted" data-testid={`bridge-state-${row.network}`}>
								{$t(`dashboard.bridge.state.${row.state}` as 'dashboard.bridge.state.connected')}
							</span>
							<span class="small muted">{$t('dashboard.bridge.lastMessage.unknown')}</span>
						</span>
						<a
							class="row__action"
							href={row.route}
							aria-label={$t('dashboard.bridge.manage', { network: networkLabel(row.network) })}
						>
							<Icon name="continue" size="dense" />
						</a>
					</li>
				{/each}
			</ul>
			<p class="small muted">{$t('dashboard.bridges.note')}</p>
		{/if}
	</section>

	<section class="card" data-testid="personas">
		<p class="card__title">
			<Icon name="persona" size="dense" />
			{$t('dashboard.personas.title')}
		</p>
		<ul class="rows">
			{#each personas as row (row.persona)}
				<li class="row" data-testid={`persona-${row.persona}`} data-active={row.active ? 'yes' : 'no'}>
					<span class="dot" data-tone={row.active ? 'ok' : 'idle'}></span>
					<span class="row__text">
						<span class="row__name">{row.persona}</span>
						<span class="small muted" data-testid={`persona-state-${row.persona}`}>
							{row.active
								? $t('dashboard.persona.active')
								: row.decided
									? $t('dashboard.persona.paused')
									: $t('dashboard.persona.never')}
						</span>
						<span class="small muted">
							{row.networks.length > 0
								? $t('dashboard.persona.on', {
										networks: row.networks.map(networkLabel).join(', ')
									})
								: $t('dashboard.persona.nowhere')}
						</span>
						{#if ago(row.lastDecisionAt) !== null}
							<span class="small muted">{ago(row.lastDecisionAt)}</span>
						{/if}
					</span>
					{#if row.active || row.decidedNetworks.length > 0}
						<button
							class="button button--secondary"
							type="button"
							onclick={() => togglePersona(row)}
							disabled={busyPersona !== null}
							data-testid={`toggle-${row.persona}`}
						>
							{#if busyPersona === row.persona}
								<span class="spinner" aria-hidden="true"></span>
								{$t('dashboard.persona.working')}
							{:else}
								<Icon name={row.active ? 'pause' : 'activate'} size="dense" />
								{row.active ? $t('dashboard.persona.pause') : $t('dashboard.persona.activate')}
							{/if}
						</button>
					{:else}
						<a class="button button--secondary" href="/personas">
							{$t('dashboard.persona.activate')}
						</a>
					{/if}
				</li>
			{/each}
		</ul>
		<!-- The toggle that was refused may be any row of the list above, so the
		     message moves to the reader when the reader is elsewhere (#139). -->
		<ActionProblem
			message={personaError === null ? null : $t('dashboard.persona.failed', { error: personaError })}
			testId="persona-error"
			variant="text"
		/>
		<p class="small muted" data-testid="pause-meaning">{$t('dashboard.persona.pausedMeaning')}</p>
		<p class="small muted" data-testid="no-runtime">{$t('dashboard.persona.noRuntime')}</p>
	</section>

	<section class="card" data-testid="activity">
		<p class="card__title">
			<Icon name="info" size="dense" />
			{$t('dashboard.feed.title')}
		</p>

		<p class="small" data-testid="messages-carried">
			<strong>{$t('dashboard.messages.title')}:</strong>
			{messages === null
				? $t('dashboard.messages.unknown')
				: $t('dashboard.messages.count', { count: messages })}
		</p>

		{#if feed.length === 0}
			<p class="small muted">{$t('dashboard.feed.empty')}</p>
		{:else}
			<ul class="feed">
				{#each feed as entry (entry.id)}
					<li class="feed__row" data-testid="feed-row">
						<span class="dot" data-tone={entry.tone}></span>
						<Icon name={entry.icon} size="dense" />
						<span class="small">{$t(entry.messageKey, feedValues(entry))}</span>
						<span class="small muted feed__when">{ago(entry.at)}</span>
					</li>
				{/each}
			</ul>
		{/if}
		<p class="small muted" data-testid="feed-privacy">{$t('dashboard.feed.privacy')}</p>
	</section>

	<section class="card" data-testid="devices">
		<p class="card__title">
			<Icon name="device" size="dense" />
			{$t('dashboard.devices.title')}
		</p>
		{#if (snapshot.devices ?? []).length === 0}
			<p class="small muted">{$t('dashboard.devices.none')}</p>
		{:else}
			<ul class="rows">
				{#each snapshot.devices ?? [] as device (device.id)}
					{@const revoked =
						device.revoked_unix_seconds !== null && device.revoked_unix_seconds !== undefined}
					<li
						class="row"
						data-testid={`device-${device.id}`}
						data-revoked={revoked ? 'yes' : 'no'}
						data-current={device.current ? 'yes' : 'no'}
					>
						<span class="dot" data-tone={revoked ? 'warn' : 'ok'}></span>
						<span class="row__text">
							<span class="row__name">{device.name}</span>
							{#if device.current}
								<span class="small muted">{$t('dashboard.devices.current')}</span>
							{/if}
							<span class="small muted">
								{revoked
									? $t('dashboard.devices.revokedOn', {
											when: ago(new Date(device.revoked_unix_seconds! * 1000).toISOString()) ?? ''
										})
									: $t('dashboard.devices.lastSeen', {
											when: ago(new Date(device.last_seen_unix_seconds * 1000).toISOString()) ?? ''
										})}
							</span>
						</span>
						{#if !revoked}
							<button
								class="button button--secondary"
								type="button"
								onclick={() => revoke(device.id, device.current)}
								disabled={busyDevice !== null}
								data-testid={`revoke-${device.id}`}
							>
								{#if busyDevice === device.id}
									<span class="spinner" aria-hidden="true"></span>
									{$t('dashboard.devices.revoking')}
								{:else}
									<Icon name="revoke" size="dense" />
									{$t('dashboard.devices.revoke')}
								{/if}
							</button>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
		<!-- A deployment the owner has used for a while has a long device list,
		     and the revoke button that was refused can be at the top of it. -->
		<ActionProblem
			message={deviceError === null ? null : $t('dashboard.devices.failed', { error: deviceError })}
			testId="device-error"
			variant="text"
		/>
	</section>

	<section class="card" data-testid="messagr">
		<p class="card__title">
			<Icon name="unavailable" size="dense" />
			{$t('dashboard.messagr.title')}
		</p>
		<p class="small muted">{$t('dashboard.messagr.body')}</p>
	</section>

	<nav class="actions">
		<a class="button button--secondary" href="/networks">{$t('dashboard.addNetwork')}</a>
		<a class="button button--secondary" href="/personas">{$t('dashboard.activateAgent')}</a>
	</nav>

	<p class="small muted" data-testid="deployment-version">
		{gatewayVersion === null ? '' : $t('dashboard.version', { version: gatewayVersion })}
		·
		<a href="/diagnostics">{$t('diagnostics.title')}</a>
	</p>
</section>

<style>
	.head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
		flex-wrap: wrap;
	}

	.rows,
	.feed {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}

	.feed {
		gap: var(--space-2);
	}

	.row {
		display: flex;
		align-items: flex-start;
		gap: var(--space-2);
	}

	.row__text {
		display: flex;
		flex-direction: column;
		gap: 2px;
		flex: 1 1 auto;
		min-width: 0;
	}

	.row__name {
		font-weight: var(--font-weight-label);
	}

	.row__action {
		align-self: center;
	}

	.feed__row {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		flex-wrap: wrap;
	}

	.feed__when {
		margin-inline-start: auto;
		white-space: nowrap;
	}

	.dot {
		flex: 0 0 auto;
		width: 10px;
		height: 10px;
		border-radius: 50%;
		background: var(--color-border);
		margin-top: 6px;
	}

	.dot[data-tone='ok'] {
		background: var(--color-success);
	}

	.dot[data-tone='warn'] {
		background: var(--color-warning);
	}

	.dot[data-tone='bad'] {
		background: var(--color-danger);
	}

	.waiting {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}

	.chip {
		display: inline-flex;
		align-items: center;
		gap: var(--space-2);
		align-self: flex-start;
		padding: var(--space-1) var(--space-3);
		border-radius: var(--radius-pill);
		background: var(--color-primary-surface);
		border: 1px solid var(--color-primary);
	}

	.actions {
		display: flex;
		gap: var(--space-2);
		flex-wrap: wrap;
	}

	a.button {
		text-decoration: none;
	}
</style>
