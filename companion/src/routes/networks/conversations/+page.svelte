<!--
	Which conversations a network is observed on (ticket #143).

	The third unit of decision. Twalk decides in two units today — consent, per
	contact, and persona activation, per network — and neither is the unit a
	user thinks in here. For `maria` a contact and a conversation are the same
	thing; for `Échecs en Yvelines` at 246 members, deciding per contact is
	unusable, because nobody adjudicates 246 people one by one. So this screen
	asks one question, once per conversation: *do I watch this room.* What is
	published about each person inside an observed room is still their consent's
	business (ADR 0012), and the two compose.

	# What this screen is careful about, in order

	**The consequence, before the tick.** Every row carries its member count,
	every community carries the total across its conversations, and the control
	that applies the decision names how many people it covers. Past the point
	where a user could have named those people one by one
	(`$lib/portals/selection.ts`), the decision has to be acknowledged before it
	is sent. A number is not a warning, and a warning nobody reads is not a
	decision — those people do not know Twalk exists (#122).

	**The bulk control is scoped to the filter, counted and named.** "Select the
	4 shown", never "select all". Over a list containing a 246-member
	association, "select all" is the affordance this ticket exists to avoid. The
	rule is `scopedBulkControl` in `$lib/matrix/rooms.ts` — the same one the
	Matrix room chooser obeys (#137), one implementation, because a second
	spelling of a rule like this is how #110 happened. The search is that
	module's `matchesQuery`, for the same reason.

	**The structure, not a flat list of near-identical names.** A WhatsApp
	community arrives as a parent, an announcement group and subgroups whose
	names repeat: two rooms called `Communauté CKCP` created in the same minute
	with 109 members and 6, three called `XVDSI`. Shown flat, a user cannot tell
	which one they are ticking. So they are grouped, and the screen states
	plainly what it does and does not know about them: the network says all of
	them are groups and never says which groups form a community, so the
	grouping is by name and no row is labelled "the parent" or "the
	announcement group". Each keeps its member count and the network's own
	address, which is what tells two same-named rows apart.

	**A zero that says which zero it is.** `GET /api/portals` lists every
	configured bridge — readable or not, which account it was read as, and how
	many rooms that account is in. An empty list is rendered as what it is:
	conversations not yet built, a bridge with no token, or a register that
	asked the appservice's `sender_localpart` instead of the bridge's bot
	(#171). It is never rendered as "you have no conversations".
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import ActionProblem from '$lib/components/ActionProblem.svelte';
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import { scopedBulkControl } from '$lib/matrix/rooms';
	import {
		matching,
		sections,
		type ConversationFamily,
		type ConversationRow
	} from '$lib/portals/conversations';
	import {
		applySelection,
		askedNothing,
		readRegister,
		unreadable,
		type BridgeReading,
		type Outcome,
		type ReadFailure
	} from '$lib/portals/register';
	import { consequence, observedNow } from '$lib/portals/selection';

	/** Every conversation the register offered, of every network. */
	let all = $state<ConversationRow[]>([]);
	let bridges = $state<BridgeReading[]>([]);
	/**
	 * The Gateway's crowd threshold, served with the register (#252). `null`
	 * until a register has been read: before that, nothing is costed at all
	 * — never "nothing is a crowd", which would be the wrong side to fail on.
	 */
	let crowdThreshold = $state<number | null>(null);
	let loaded = $state(false);
	let failure = $state<ReadFailure | null>(null);
	let query = $state('');
	let selected = $state<Set<string>>(new Set());
	/** The selection as the register answered, so a reload is not a decision. */
	let applying = $state(false);
	let applyProblem = $state<string | null>(null);
	let outcomes = $state<Outcome[]>([]);
	/** The explicit act that a crowd's worth of people requires. */
	let acknowledged = $state(false);

	/**
	 * Which network this screen is about, from `?network=`.
	 *
	 * Optional, and absent is not an error: the register answers for every
	 * bridge at once, so with no parameter this is the whole deployment's
	 * conversations, and with one it is a network's. Read once, from the
	 * address the page was opened at — nothing here navigates.
	 */
	let network = $state<string | null>(null);

	onMount(async () => {
		if (typeof window !== 'undefined') {
			const asked = new URLSearchParams(window.location.search).get('network');
			network = asked !== null && asked !== '' ? asked : null;
		}
		await read();
	});

	async function read(): Promise<void> {
		loaded = false;
		const answer = await readRegister();
		if (!answer.ok) {
			failure = answer.failure;
			loaded = true;
			return;
		}
		failure = null;
		bridges = [...answer.register.bridges];
		crowdThreshold = answer.register.crowdThreshold;
		all = answer.register.rows.filter(
			(row) => network === null || row.network === network
		);
		// The selection starts from what is already true, so opening this
		// screen and pressing the button changes nothing.
		selected = observedNow(all);
		acknowledged = false;
		outcomes = [];
		loaded = true;
	}

	const shown = $derived(matching(all, query));
	const grouped = $derived(sections(shown));
	/** Scoped to the filter, counted, named. Never "all". */
	const bulk = $derived(scopedBulkControl(shown.map((row) => row.roomId), selected));
	/** Costed against every conversation, not only the ones on screen. */
	const cost = $derived(consequence(all, selected, crowdThreshold ?? 0));
	const observing = $derived(all.filter((row) => row.observation === 'observing').length);
	const blocked = $derived(cost.empty || (cost.acknowledgementNeeded && !acknowledged));

	function toggle(roomId: string): void {
		const next = new Set(selected);
		if (next.has(roomId)) {
			next.delete(roomId);
		} else {
			next.add(roomId);
		}
		selected = next;
		// A changed decision is a decision to acknowledge again: the count the
		// user agreed to is not the count they now have.
		acknowledged = false;
	}

	/** One gesture over a whole community, with its people count in the label. */
	function toggleFamily(family: ConversationFamily): void {
		selected = scopedBulkControl(
			family.rows.map((row) => row.roomId),
			selected
		).apply();
		acknowledged = false;
	}

	function familyControl(family: ConversationFamily) {
		return scopedBulkControl(
			family.rows.map((row) => row.roomId),
			selected
		);
	}

	function toggleShown(): void {
		selected = bulk.apply();
		acknowledged = false;
	}

	async function apply(): Promise<void> {
		if (applying || blocked) {
			return;
		}
		applying = true;
		applyProblem = null;
		const answer = await applySelection(all, selected);
		outcomes = [...answer.outcomes];
		if (!answer.ok) {
			applyProblem =
				answer.trouble === 'session-refused'
					? $t('api.trouble.sessionRefused')
					: answer.trouble === 'refused'
						? $t('api.trouble.refused')
						: $t('api.trouble.unreachable');
		}
		applying = false;
		// Whatever happened, the truth is the register's and not this screen's:
		// re-read, so a partial application is shown as what it actually did.
		const keep = outcomes;
		await read();
		outcomes = keep;
	}

	function outcomeCopy(outcome: Outcome): string {
		switch (outcome.status) {
			case 'invited':
				return $t('conversations.outcome.invited');
			case 'already_observed':
				return $t('conversations.outcome.alreadyObserved');
			case 'removed':
				return $t('conversations.outcome.removed');
			case 'not_observed':
				return $t('conversations.outcome.notObserved');
			case 'unknown_portal':
				return $t('conversations.outcome.unknownPortal');
			default:
				return $t('conversations.outcome.failed', { reason: outcome.reason ?? '' });
		}
	}

	function labelOf(roomId: string): string {
		return all.find((row) => row.roomId === roomId)?.label ?? roomId;
	}

	/** The name a bridge reading is best described by, for the diagnoses. */
	function bridgeName(bridge: BridgeReading): string {
		return bridge.bridge_id;
	}

	/**
	 * The bridges this screen is answerable for.
	 *
	 * Scoped to `?network=` when there is one: a screen about WhatsApp that
	 * reported Signal's missing credential would be telling the user about a
	 * network they did not open, and a screen about everything that left it out
	 * would be the silence this whole answer exists against.
	 */
	const answerable = $derived(
		bridges.filter((bridge) => network === null || bridge.network === network)
	);
	const unreadableBridges = $derived(unreadable(answerable));
	const silentBridges = $derived(askedNothing(answerable));
</script>

<section
	class="screen"
	data-testid="screen-conversations"
	data-network={network ?? 'all'}
	data-loaded={loaded ? 'yes' : 'no'}
	data-crowd-threshold={crowdThreshold ?? undefined}
>
	<header class="stack">
		<p class="small">
			<a href="/networks">
				<Icon name="back" size="dense" />
				{$t('networks.back')}
			</a>
		</p>
		<h1>{$t('conversations.title')}</h1>
		<p class="subtitle">{$t('conversations.caption')}</p>
		<!-- The sentence the deployment could not previously say at all. -->
		{#if loaded && failure === null}
			<p class="muted" data-testid="conversations-summary">
				{$t('conversations.summary', { observing, total: all.length })}
			</p>
		{/if}
		<p class="small muted">{$t('conversations.strangers')}</p>
	</header>

	{#if !loaded}
		<p class="muted" data-testid="conversations-loading">
			<span class="spinner" aria-hidden="true"></span>
			{$t('conversations.loading')}
		</p>
	{:else if failure !== null}
		{#if failure.kind === 'not-configured'}
			<!-- Not a failure of the network or the session: the operator has
			     not given the Gateway a bridge's appservice token, and the
			     Gateway says which variable in its own words. -->
			<div class="card card--warning" role="alert" data-testid="conversations-not-configured">
				<p class="card__title">
					<Icon name="warning" size="dense" />
					{$t('conversations.notConfigured.title')}
				</p>
				<p>{$t('conversations.notConfigured.body')}</p>
				{#if failure.detail !== ''}
					<p class="small muted mono">{failure.detail}</p>
				{/if}
			</div>
		{:else}
			<p class="card card--warning" role="alert" data-testid="conversations-trouble" data-trouble={failure.trouble}>
				{#if failure.trouble === 'session-refused'}
					{$t('api.trouble.sessionRefused')}
				{:else if failure.trouble === 'refused'}
					{$t('api.trouble.refused')}
				{:else}
					{$t('api.trouble.unreachable')}
				{/if}
			</p>
		{/if}
	{:else}
		<!-- Every bridge whose conversations are missing from the list, so a
		     total is never mistaken for a count of what the user has. -->
		{#each unreadableBridges as bridge (bridge.bridge_id)}
			<div class="card card--warning small" role="status" data-testid={`bridge-unreadable-${bridge.bridge_id}`}>
				<p>{$t('conversations.bridgeUnreadable', { bridge: bridgeName(bridge) })}</p>
				{#if bridge.detail !== null}
					<p class="mono">{bridge.detail}</p>
				{/if}
			</div>
		{/each}

		<!-- #171: a bridge that answered, as an account that is in no rooms.
		     Either nothing has become active yet, or the register asked the
		     appservice's sender instead of the bridge's bot. The screen cannot
		     tell, so it says both and names the account. -->
		{#each silentBridges as bridge (bridge.bridge_id)}
			<div class="card card--info small" role="status" data-testid={`bridge-silent-${bridge.bridge_id}`}>
				<p>
					{$t('conversations.bridgeAskedNothing', {
						bridge: bridgeName(bridge),
						account: bridge.asked_as ?? ''
					})}
				</p>
			</div>
		{/each}

		{#if all.length === 0}
			<p class="card card--info" data-testid="conversations-none">
				{$t('conversations.none')}
			</p>
		{:else}
			<div class="field">
				<label class="label" for="conversation-search">
					{$t('conversations.search.label')}
				</label>
				<input
					id="conversation-search"
					class="input"
					type="search"
					autocomplete="off"
					spellcheck="false"
					placeholder={$t('conversations.search.placeholder')}
					data-testid="conversation-search"
					bind:value={query}
					disabled={applying}
				/>
			</div>

			<p class="small muted" data-testid="conversations-count">
				{$t('conversations.showing', { shown: shown.length, total: all.length })}
			</p>

			{#if bulk.count > 0}
				<!-- Counted and named, so "select" can never be read as
				     "everything I have". -->
				<p>
					<button
						class="button"
						type="button"
						data-testid="conversations-toggle-shown"
						disabled={applying}
						onclick={toggleShown}
					>
						{bulk.selectsRatherThanDeselects
							? $t('conversations.selectShown', { count: bulk.count })
							: $t('conversations.deselectShown', { count: bulk.count })}
					</button>
				</p>
			{/if}

			{#snippet conversation(row: ConversationRow)}
				<li>
					<label class="row">
						<input
							type="checkbox"
							checked={selected.has(row.roomId)}
							onchange={() => toggle(row.roomId)}
							disabled={applying}
							data-testid={`conversation-${row.roomId}`}
						/>
						<span class="row__text">
							<span class="row__name" class:row__name--derived={row.labelSource !== 'name'}>
								{row.label}
							</span>
							<span class="small muted">
								<!-- The number that makes this a decision. -->
								<span data-testid={`members-${row.roomId}`}>
									{$t('conversations.members', { count: row.members })}
								</span>
								{#if row.networkConversationId !== null && row.labelSource === 'name'}
									<!-- What tells two rows with one name apart. -->
									· <span class="mono">{row.networkConversationId}</span>
								{/if}
								{#if row.movedFrom !== null}
									<!-- The conversation's room was replaced (ADR 0029). Said on
									     the row, whatever else is true of it: a deployment that
									     changed rooms under the user must be able to say so. The
									     old room is not listed — the register folded it. -->
									· <span data-testid={`moved-${row.roomId}`}>{$t('conversations.moved')}</span>
								{/if}
							</span>
						</span>
						{#if row.observation === 'observing'}
							<span class="badge badge--ok">
								<Icon name="observing" size="dense" />
								{$t('conversations.observing')}
							</span>
						{:else if row.observation === 'invited'}
							<span class="badge badge--attention" title={$t('conversations.invitedWhy')}>
								<Icon name="warning" size="dense" />
								{$t('conversations.invited')}
							</span>
						{:else if row.observation === 'moved'}
							<!-- A decision on record that stopped holding: the user observed
							     this conversation, it moved to a room whose audience crosses
							     the threshold, and the register returned it here (#255).
							     Ticking it again is the decision. -->
							<span
								class="badge badge--attention"
								title={$t('conversations.movedWhy')}
								data-testid={`moved-badge-${row.roomId}`}
							>
								<Icon name="warning" size="dense" />
								{$t('conversations.movedBadge')}
							</span>
						{/if}
					</label>
				</li>
			{/snippet}

			{#snippet plainSection(
				kind: string,
				icon: 'one-to-one' | 'group' | 'broadcast' | 'kind-unstated',
				heading: string,
				why: string,
				list: readonly ConversationRow[]
			)}
				{#if list.length > 0}
					<section class="group" data-testid={`section-${kind}`}>
						<h2 class="group__head">
							<Icon name={icon} size="dense" />
							{heading}
							<span class="badge badge--neutral">{list.length}</span>
						</h2>
						<p class="small muted">{why}</p>
						<ul class="rows">
							{#each list as row (row.roomId)}
								{@render conversation(row)}
							{/each}
						</ul>
					</section>
				{/if}
			{/snippet}

			{@render plainSection(
				'one-to-one',
				'one-to-one',
				$t('conversations.kind.oneToOne'),
				$t('conversations.kind.oneToOne.why'),
				grouped.oneToOne
			)}

			{@render plainSection(
				'group',
				'group',
				$t('conversations.kind.group'),
				$t('conversations.kind.group.why'),
				grouped.groups
			)}

			{#if grouped.communities.length > 0}
				<section class="group" data-testid="section-community">
					<h2 class="group__head">
						<Icon name="community" size="dense" />
						{$t('conversations.kind.community')}
						<span class="badge badge--neutral">{grouped.communities.length}</span>
					</h2>
					<p class="small muted">{$t('conversations.kind.community.why')}</p>
					<!-- Said plainly rather than implied: the network never tells
					     Twalk which groups form a community, so this grouping is
					     by name and no row here is called the parent. -->
					<p class="small muted" data-testid="community-caveat">
						{$t('conversations.kind.community.caveat')}
					</p>

					{#each grouped.communities as family (family.key)}
						{@const control = familyControl(family)}
						<div class="family" data-testid={`family-${family.key}`}>
							<p class="family__head">
								<span class="family__name">{family.name}</span>
								<span class="small muted" data-testid={`family-people-${family.key}`}>
									{$t('conversations.family.head', {
										conversations: family.rows.length,
										people: family.people
									})}
								</span>
							</p>
							<!-- One gesture over the whole community, with the
							     number of people it covers in its own label —
							     before the tick, which is the criterion. -->
							<p>
								<button
									class="button button--compact"
									type="button"
									disabled={applying}
									onclick={() => toggleFamily(family)}
									data-testid={`family-toggle-${family.key}`}
								>
									{control.selectsRatherThanDeselects
										? $t('conversations.family.select', {
												count: family.rows.length,
												people: family.people
											})
										: $t('conversations.family.deselect', { count: family.rows.length })}
								</button>
							</p>
							<ul class="rows">
								{#each family.rows as row (row.roomId)}
									{@render conversation(row)}
								{/each}
							</ul>
						</div>
					{/each}
				</section>
			{/if}

			{@render plainSection(
				'broadcast',
				'broadcast',
				$t('conversations.kind.broadcast'),
				$t('conversations.kind.broadcast.why'),
				grouped.broadcasts
			)}

			{@render plainSection(
				'unstated',
				'kind-unstated',
				$t('conversations.kind.unstated'),
				$t('conversations.kind.unstated.why'),
				grouped.unstated
			)}

			<!-- The consequence of the pending decision, stated while it is
			     still pending, and updated as it changes. -->
			<div class="consequence" data-testid="conversations-consequence">
				{#if cost.empty}
					<p class="muted">{$t('conversations.consequence.none')}</p>
				{:else}
					{#if cost.starting > 0}
						<p data-testid="consequence-adding">
							{$t('conversations.consequence.adding', {
								conversations: cost.starting,
								people: cost.people
							})}
						</p>
						{#if cost.largest !== null}
							<p class="small">
								{$t('conversations.consequence.largest', {
									name: cost.largest.label,
									count: cost.largest.members
								})}
							</p>
						{/if}
					{/if}
					{#if cost.stopping > 0}
						<p data-testid="consequence-stopping">
							{$t('conversations.consequence.stopping', { count: cost.stopping })}
						</p>
					{/if}
				{/if}
			</div>

			{#if cost.acknowledgementNeeded}
				<!-- A crowd is not a number to skim past. The conversations that
				     put the decision over the line are listed, and the decision
				     does not go anywhere until this is ticked. -->
				<div class="card card--warning" data-testid="conversations-acknowledge">
					<p class="card__title">
						<Icon name="warning" size="dense" />
						{$t('conversations.acknowledge.title', { count: cost.people })}
					</p>
					<ul class="crowds">
						{#each cost.crowds as crowd (crowd.label)}
							<li>
								{crowd.label} — {$t('conversations.members', { count: crowd.members })}
								{#if crowd.moved}
									· {$t('conversations.crowdMoved')}
								{/if}
							</li>
						{/each}
					</ul>
					<p>{$t('conversations.acknowledge.body')}</p>
					<label class="row">
						<input
							type="checkbox"
							bind:checked={acknowledged}
							disabled={applying}
							data-testid="conversations-acknowledged"
						/>
						<span>{$t('conversations.acknowledge.label', { count: cost.people })}</span>
					</label>
				</div>
			{/if}

			<!-- The refusal belongs against the control that caused it, not at
			     the top of a list that can be a screen and a half long (#139). -->
			<ActionProblem message={applyProblem} testId="conversations-apply-problem" />

			<button
				class="button button--primary"
				type="button"
				onclick={apply}
				disabled={blocked || applying}
				data-testid="conversations-apply"
			>
				{#if applying}
					<span class="spinner" aria-hidden="true"></span>
					<span class="visually-hidden">{$t('conversations.applying')}</span>
				{:else}
					<!-- What the button will do, spelled out. A control labelled
					     "Apply" over a decision about 246 people is a control
					     that hides its own consequence. -->
					{cost.starting > 0 && cost.stopping > 0
						? $t('conversations.apply.both', {
								starting: cost.starting,
								stopping: cost.stopping
							})
						: cost.starting > 0
							? $t('conversations.apply.start', { count: cost.starting })
							: cost.stopping > 0
								? $t('conversations.apply.stop', { count: cost.stopping })
								: $t('conversations.apply.nothing')}
				{/if}
			</button>

			{#if outcomes.length > 0}
				<ul class="outcomes" data-testid="conversations-outcomes">
					{#each outcomes as outcome (outcome.room_id)}
						<li data-testid={`outcome-${outcome.room_id}`} data-status={outcome.status}>
							<Icon name={outcome.status === 'failed' ? 'error' : 'check'} size="dense" />
							{labelOf(outcome.room_id)}
							<span class="small muted">{outcomeCopy(outcome)}</span>
						</li>
					{/each}
				</ul>
			{/if}
		{/if}
	{/if}
</section>

<style>
	.rows,
	.outcomes,
	.crowds {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.group {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		margin-block: var(--space-4) 0;
	}

	.group__head {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		flex-wrap: wrap;
		font-size: var(--text-lg);
		margin: 0;
	}

	.family {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		border-left: 2px solid var(--color-border);
		padding-left: var(--space-3);
	}

	.family__head {
		display: flex;
		flex-direction: column;
		margin: 0;
	}

	.family__name {
		font-weight: 600;
		overflow-wrap: anywhere;
	}

	.row {
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

	.row__text {
		display: flex;
		flex-direction: column;
		min-width: 0;
		flex: 1 1 auto;
	}

	.row__name {
		overflow-wrap: anywhere;
	}

	/* A label the conversation did not give itself reads as what it is. */
	.row__name--derived {
		color: var(--color-text-muted);
		font-style: italic;
	}

	.consequence {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		margin-block: var(--space-4) var(--space-2);
	}

	.mono {
		font-family: var(--font-mono);
		overflow-wrap: anywhere;
	}

	/* A control inside a community, which sits beside its rows rather than
	   above the whole screen: full width would read as the page's action. */
	.button--compact {
		align-self: flex-start;
		font-size: var(--text-sm);
		padding: var(--space-1) var(--space-3);
	}

	.badge {
		font-size: var(--text-xs);
		padding: 2px var(--space-2);
		border-radius: var(--radius-pill);
		background: var(--color-warning-surface);
		border: 1px solid var(--color-warning);
		color: var(--color-text);
		white-space: nowrap;
	}

	.badge--ok {
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
		background: var(--color-success-surface);
		border-color: var(--color-success);
	}

	/* `invited` is the state that names its own cause — the Sensor refused the
	   inviter — so it keeps the amber that asks to be looked at (ADR 0024). */
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

	.outcomes li {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		flex-wrap: wrap;
	}
</style>
