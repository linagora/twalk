<!--
	Screen 3 of `docs/wireframes/companion-v0.1.md`: the network picker.

	The grid is the catalogue (`$lib/networks/catalogue.ts`) joined with the
	deployment's registry of connections (`GET /api/connections`, ADR 0033)
	and with what `GET /api/bridges` says about each connection's transport.
	**A card is a connection** (#272): a deployment with a personal WhatsApp
	and a work one has two WhatsApp cards, each named, each with its own
	login and its own decisions; with one connection per kind — the reference
	shape — the grid reads exactly as it always did. Separate facts, and the
	screen keeps them separate:

	  - a kind the wireframes list but this deployment has no connection for
	    is shown, greyed, and says so — hiding it would leave the user
	    wondering where WhatsApp went;
	  - a Gateway that could not answer at all does *not* grey everything. It
	    says the list is unknown and leaves the cards tappable, because the
	    screen behind them handles a missing bridge on its own.

	And it says *which* kind of "could not answer" it was (#111). This screen
	is where the conflation was caught live: `GET /api/bridges` answered `401`
	in zero milliseconds — the session had expired — and the card told the
	owner their Twalk server could not be reached. It was reached. It refused.
	The two are fixed in entirely different places, so `$lib/api/trouble.ts`
	tells them apart and the copy follows.

	Whether a network is *connected* is the bridge's answer and nobody else's:
	`GET /api/bridges` carries a `connection` read from the bridge's own
	`whoami`, and this screen reads that. It must never read `login`, which is
	a QR scan in flight — doing so is what made a live WhatsApp link show no
	badge at all once the Gateway had been restarted (#108).

	A bridge the Gateway could not reach is *unknown*, not disconnected: the
	card says nothing rather than claiming a working link is broken. The list
	itself still answers, so one bridge being down costs the other nothing.

	# Two journeys, and the screen says which one this is

	Observed live with WhatsApp connected for two hours and Signal for two
	minutes: *"Connecter votre premier réseau — Étape 1 sur 3 — Choisissez-en
	un pour commencer."* Every word of it was wrong for that user (#120). The
	onboarding wizard's copy was shown unconditionally to anyone who reached
	the picker, including a returning user adding a network to a working
	deployment.

	So the heading is derived from the same answer the cards are: how many
	networks `GET /api/bridges` says are connected, read through
	`connection.ts` like everything else. Never from a flag the Companion wrote
	into browser storage — spec #65 is explicit that onboarding progress is
	read from what exists on the Gateway.

	Three modes and not two, because "I could not ask" is not "nothing is
	connected". A Gateway that did not answer gets the neutral heading: it
	claims nothing, where *first network* and *step 1 of 3* are claims that can
	be false. That is the same rule as the cards below, one level up.
-->
<script lang="ts">
	import { onDestroy, onMount } from 'svelte';

	import { gateway } from '$lib/api/client';
	import { troubleOf, type ApiTrouble } from '$lib/api/trouble';
	import { loadRegistry, NOT_READ_YET, type Registry } from '$lib/connections/registry';
	import { rereadWhileUnsettled, type Rereader } from '$lib/networks/reread';
	import Icon from '$lib/icons/Icon.svelte';
	import { t, type MessageKey } from '$lib/i18n';
	import {
		gridFor,
		looksLikeIos,
		manageRouteFor,
		type BridgeRow,
		type CardState,
		type CollectorState
	} from '$lib/networks/catalogue';
	import { connectionOf } from '$lib/networks/connection';

	let registry = $state<Registry>(NOT_READ_YET);
	let registryRead = $state(false);
	let bridges = $state<BridgeRow[]>([]);
	let bridgesKnown = $state(false);
	/** Why the list is unknown, when it is. */
	let bridgesTrouble = $state<ApiTrouble | null>(null);
	let loaded = $state(false);
	let ios = $state(false);

	let rereader: Rereader | null = null;

	onMount(() => {
		ios = looksLikeIos(navigator.userAgent, navigator.maxTouchPoints, navigator.platform);
		// Read now, and again while the answer is transient (#148): a Gateway
		// that did not answer, or one that answered and could not ask a bridge
		// at that instant, is a card that would otherwise read *unknown* for
		// as long as this page is open. A refusal is not re-asked — a `401` is
		// repaired centrally (#111), and a `4xx` is an answer.
		rereader = rereadWhileUnsettled(readBridges, () => loaded && transient);
	});

	onDestroy(() => {
		rereader?.stop();
	});

	/**
	 * The two reads together (#272): the registry says which connections
	 * there are, the bridge list what each one's transport reports.
	 */
	async function readBridges() {
		const [connections, listed] = await Promise.all([
			loadRegistry(),
			gateway.GET('/api/bridges').catch(() => null)
		]);
		registry = connections;
		registryRead = connections.trouble === null;
		bridgesKnown = listed !== null && listed.error === undefined;
		bridgesTrouble = bridgesKnown ? null : troubleOf(listed);
		bridges = listed?.data?.bridges ?? [];
		loaded = true;
	}

	/**
	 * Whether the last read is worth asking again: the list could not be read
	 * for a reason that is not a refusal, or it could and some bridge could not
	 * be asked (`connection.state === 'unknown'`, the `whoami` that did not
	 * answer). Both are the Gateway's word for "not now", and neither is the
	 * Gateway's word for "no".
	 */
	const transient = $derived(
		(!bridgesKnown && bridgesTrouble === 'unreachable') ||
			(bridgesKnown && bridges.some((bridge) => connectionOf(bridge).state === 'unknown'))
	);

	const grid = $derived<CardState[]>(
		gridFor({
			connections: registry.connections,
			connectionsKnown: registryRead,
			bridges,
			bridgesKnown,
			ios
		})
	);

	/**
	 * Which journey this is, from the deployment's own state.
	 *
	 * `first` is onboarding: no network connected, and the step counter means
	 * something. `add` is a returning user. `unknown` is a Gateway that did
	 * not answer, and it reads like `add`: a heading that claims nothing is
	 * the honest one when nothing is known.
	 */
	const mode = $derived<'first' | 'add' | 'unknown'>(
		!loaded || !bridgesKnown
			? 'unknown'
			: grid.some((state) => state.connected)
				? 'add'
				: 'first'
	);

	/** The tooltip and the helper line under a card that cannot be tapped. */
	function blockedCopy(state: CardState): string | null {
		switch (state.blockedBy) {
			case 'ios':
				return $t('networks.card.iosBlocked');
			case 'no-bridge':
				return $t('networks.card.notConfigured');
			case 'no-collector':
				return $t('networks.card.noCollector');
			case 'coming-soon':
				return $t('network.comingSoon');
			default:
				return null;
		}
	}

	/**
	 * The badge a card wears, in the contract's own five states.
	 *
	 * Deliberately five badges and not two. `starting` is a bridge coming back
	 * up and `degraded` is one reconnecting by itself: rounding either to
	 * "connected" would claim more than the bridge said, and rounding it to
	 * nothing would repeat the defect. `session_expired` is the one the user
	 * has to act on — it is what a session revoked from their own phone
	 * reports — so it is the one that looks like a warning.
	 */
	function badgeFor(state: CardState): { key: MessageKey; tone: string; icon: 'check' | 'warning' | 'reload' } | null {
		if (state.collector !== null) {
			return collectorBadgeFor(state.collector.state);
		}
		switch (state.link.state) {
			case 'connected':
				return { key: 'networks.connected', tone: 'badge--ok', icon: 'check' };
			case 'session_expired':
				return { key: 'networks.state.sessionExpired', tone: 'badge--attention', icon: 'warning' };
			case 'starting':
				return { key: 'networks.state.starting', tone: 'badge--neutral', icon: 'reload' };
			case 'degraded':
				return { key: 'networks.state.degraded', tone: 'badge--neutral', icon: 'reload' };
			default:
				// `disconnected` and `unknown` wear nothing. There is no link to
				// describe, or no way to know — and an "unknown" badge on every
				// card while a bridge restarts would be noise.
				return null;
		}
	}

	/**
	 * A collector connection's badge (#275): the contract's four states as
	 * four sentences, never one. `connected` is the tick; `unreachable` is a
	 * service not answering, which the collector retries by itself;
	 * `reconnect_required` and `pending_operator` are the operator's — the
	 * grant to give again, the client to change — and look like warnings, with
	 * the collector's own hint under the card.
	 */
	function collectorBadgeFor(
		state: CollectorState
	): { key: MessageKey; tone: string; icon: 'check' | 'warning' | 'reload' } | null {
		switch (state) {
			case 'connected':
				return { key: 'networks.connected', tone: 'badge--ok', icon: 'check' };
			case 'unreachable':
				return { key: 'networks.collector.unreachable', tone: 'badge--neutral', icon: 'reload' };
			case 'reconnect_required':
				return { key: 'networks.collector.reconnectRequired', tone: 'badge--attention', icon: 'warning' };
			case 'pending_operator':
				return { key: 'networks.collector.pendingOperator', tone: 'badge--attention', icon: 'warning' };
			default:
				return null;
		}
	}
</script>

<section class="screen" data-testid="screen-networks" data-mode={mode}>
	<header class="stack">
		<p class="small">
			<!-- Back to where this user came from: onboarding has a previous
			     step, a working deployment has a dashboard. -->
			{#if mode === 'first'}
				<a href="/onboarding">
					<Icon name="back" size="dense" />
					{$t('onboarding.back')}
				</a>
			{:else}
				<a href="/dashboard" data-testid="back-to-dashboard">
					<Icon name="back" size="dense" />
					{$t('networks.backToDashboard')}
				</a>
			{/if}
		</p>
		{#if mode === 'first'}
			<h1>{$t('networks.title')}</h1>
			<!-- The step counter belongs to the journey that has steps. -->
			<p class="subtitle" data-testid="networks-step">{$t('networks.step')}</p>
			<p class="muted">{$t('networks.intro')}</p>
		{:else}
			<h1>{$t('networks.add.title')}</h1>
			<p class="muted">{$t('networks.add.intro')}</p>
		{/if}
		{#if !loaded}
			<p class="muted" data-testid="networks-loading">
				<span class="spinner" aria-hidden="true"></span>
				{$t('networks.loading')}
			</p>
		{/if}
	</header>

	{#if loaded && !bridgesKnown}
		<p
			class="card card--warning small"
			role="status"
			data-testid="bridges-unknown"
			data-trouble={bridgesTrouble}
		>
			{#if bridgesTrouble === 'session-refused'}
				{$t('api.trouble.sessionRefused')}
			{:else if bridgesTrouble === 'refused'}
				{$t('api.trouble.refused')}
			{:else}
				{$t('api.trouble.unreachable')}
			{/if}
			{$t('networks.unknownList')}
		</p>
	{/if}

	<ul class="grid" data-testid="network-grid" aria-busy={!loaded}>
		{#each grid as state (state.key)}
			{@const blocked = blockedCopy(state)}
			{@const badge = badgeFor(state)}
			{@const manage = state.linked ? manageRouteFor(state) : null}
			<li>
				<!-- svelte-ignore a11y_no_redundant_roles -->
				<div
					class="tile"
					class:tile--muted={state.blockedBy !== null}
					data-testid={`card-${state.key}`}
					data-network={state.card.network}
					data-connection-id={state.connection?.id ?? ''}
					data-blocked={state.blockedBy ?? ''}
					data-connected={state.connected ? 'yes' : 'no'}
					data-connection={state.collector === null ? state.link.state : state.collector.state}
					data-linked={state.linked ? 'yes' : 'no'}
					title={blocked ?? undefined}
				>
					<p class="tile__head">
						<Icon name={state.card.icon} />
						<span class="tile__name">
							{$t(state.card.titleKey)}
							{#if state.label !== null}
								<!-- Which account, when the kind has more than one
								     (#272): the label the operator gave the
								     connection, never a guess between two. -->
								<span class="tile__account" data-testid={`label-${state.key}`}
									>— {state.label}</span
								>
							{/if}
						</span>
						{#if state.card.preview}
							<span class="badge">{$t('networks.preview')}</span>
						{/if}
						{#if badge !== null}
							<span class={`badge ${badge.tone}`} data-testid={`state-${state.key}`}>
								<Icon name={badge.icon} size="dense" />
								{$t(badge.key)}
							</span>
						{/if}
					</p>
					<p class="small muted">{$t(state.card.subtitleKey)}</p>
					{#if state.collector !== null && state.collector.hint !== null && state.collector.state !== 'connected'}
						<!-- The collector's own next step for the operator, as it
						     said it on the bus (#275): the words the log has, on the
						     card, so nobody reads a log to learn them. -->
						<p class="small muted" data-testid={`hint-${state.key}`}>{state.collector.hint}</p>
					{/if}
					{#if state.linked && state.link.account?.name}
						<!-- Which account, on the card itself: the user's own question
						     is "is *my* number linked", and the answer is a fact the
						     bridge already gave us. -->
						<p class="small muted account" data-testid={`account-${state.key}`}>
							{state.link.account.name}
						</p>
					{/if}

					{#if state.linked && state.blockedBy === null}
						<!-- The standing decision, not a setup step (#143). A
						     bridge builds a portal room every time a
						     conversation becomes active, so this list grows all
						     day and the user comes back to it — which is why it
						     is on the card rather than inside the login journey
						     that ran once. -->
						<p class="tile__action">
							<a
								class="button button--secondary"
								href={state.connection === null
									? `/networks/conversations?network=${state.card.network}`
									: `/networks/conversations?connection=${encodeURIComponent(state.connection.id)}`}
								data-testid={`conversations-${state.key}`}
							>
								<Icon name="observing" size="dense" />
								{$t('networks.conversations')}
							</a>
						</p>
					{/if}

					{#if manage !== null && state.blockedBy === null}
						<!-- A link exists, so *Manage* opens the screen that manages it.
						     It used to lead to the login screen, which started a fresh
						     QR flow against a working connection and was refused with a
						     409 — the Gateway being right and the Companion asking the
						     wrong question (#108). -->
						<p class="tile__action">
							<a class="button button--secondary" href={manage} data-testid={`manage-${state.key}`}>
								<Icon name="manage" size="dense" />
								{$t('networks.manage')}
							</a>
						</p>
					{:else if state.href !== null && state.blockedBy === null}
						<p class="tile__action">
							<a class="button button--secondary" href={state.href}>
								{$t('networks.continue')}
								<Icon name="continue" size="dense" />
							</a>
						</p>
					{:else if blocked !== null}
						<!-- The greyed card's reason, in the page rather than only in a
						     `title` attribute: a tooltip is invisible to a touch
						     screen and to a screen reader. -->
						<p class="small blocked">
							<Icon name="unavailable" size="dense" />
							{blocked}
						</p>
					{/if}
				</div>
			</li>
		{/each}
	</ul>

	<!-- Screen 4 is next, whether or not a network was connected: its own
	     screen says plainly that an assistant with no network to read has
	     nothing to do, and offers the way back here. -->
	<p class="onwards">
		<a class="button button--primary" href="/personas" data-testid="to-personas">
			{$t('networks.toAssistant')}
			<Icon name="continue" size="dense" />
		</a>
		{#if mode === 'first'}
			<!-- "Skip for now, I'll add networks later" is an offer to leave a
			     journey. A user who came back to add a network is not in one. -->
			<a class="skip-link" href="/personas" data-testid="skip-networks">{$t('networks.skip')}</a>
		{:else}
			<a class="skip-link" href="/dashboard" data-testid="done-adding">{$t('networks.add.done')}</a>
		{/if}
	</p>

	<p class="card card--info small">
		<Icon name="info" size="dense" />
		{$t('networks.footer')}
	</p>
</section>

<style>
	.grid {
		list-style: none;
		margin: 0;
		padding: 0;
		display: grid;
		gap: var(--space-3);
		grid-template-columns: 1fr;
	}

	/* The wireframes are mobile-first; two columns once there is room. */
	@media (min-width: 30rem) {
		.grid {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}

	.tile {
		height: 100%;
		background: var(--color-surface);
		border: 1px solid var(--color-border);
		border-radius: var(--radius-lg);
		padding: var(--space-3);
		box-shadow: var(--shadow-sm);
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.tile--muted {
		background: var(--color-disabled-surface);
		box-shadow: none;
	}

	.tile--muted .tile__name {
		color: var(--color-text-muted);
	}

	.tile__head {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		flex-wrap: wrap;
	}

	.tile__name {
		font-weight: var(--font-weight-label);
	}

	.tile__action {
		margin-top: auto;
	}

	.badge {
		font-size: var(--text-xs);
		padding: 2px var(--space-2);
		border-radius: var(--radius-pill);
		background: var(--color-warning-surface);
		border: 1px solid var(--color-warning);
		color: var(--color-text);
	}

	.badge--ok {
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
		background: var(--color-success-surface);
		border-color: var(--color-success);
	}

	/* The one state the user has to act on keeps the amber of the warning
	   card; the two the bridge will resolve by itself are calm on purpose. */
	.badge--attention {
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
	}

	.badge--neutral {
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
		background: var(--color-surface);
		border-color: var(--color-border);
		color: var(--color-text-muted);
	}

	.account {
		font-variant-numeric: tabular-nums;
	}

	.blocked {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--color-text-muted);
	}

	.skip-link {
		color: var(--color-text-muted);
	}

	.onwards {
		display: flex;
		align-items: center;
		gap: var(--space-3);
		flex-wrap: wrap;
	}

	.onwards .button {
		text-decoration: none;
	}
</style>
