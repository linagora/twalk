<!--
	The consent screen (#170): which contacts may be processed, decided one at a
	time.

	`CONTEXT.md` defines the Companion as the PWA that guides a non-technical
	user "from account bootstrap to bridge login, persona activation, and
	**consent management**". The Gateway's half of that last one has been
	complete for weeks — five endpoints — and nothing called the one that
	writes. On the reference deployment the owner's first consent decision was
	made with a hand-forged HTTP request, because there was no other way. This
	is the screen that was missing.

	# What the words on this screen have to be true about

	`CONTEXT.md` is explicit that **consent here is a term of art and is not the
	contact's own consent**: the contact is neither asked nor told, and `granted`
	records that *the user decided*, not that anybody agreed. So nothing on this
	screen says "they agreed", and `consent.intro.notTheirs` says the opposite
	out loud — that paragraph exists in the glossary because writing "the
	contact's agreement" for months is how the gap went unnoticed.

	Three more things, each of which a screen can get wrong while looking right.

	**`pending` is not `revoked`, and most of this list will be `pending`.**
	Never-decided is the default of the whole model (ADR 0010: an absent subject
	means no decision was ever recorded, *never* a revoked one), and it is its
	own state. `$lib/consent/model.ts` carries `state` and `decidedBy` as two
	facts, and a row that nobody has decided about says so in its own words
	rather than borrowing the sentence of a contact the user deliberately left
	undecided. A screen showing two states where the model has three teaches the
	user a wrong picture of their own deployment.

	**Only `revoked` withholds content.** `granted` lets a persona read;
	`pending` **publishes the message** with a label consumers honour by
	convention; only `revoked` reduces what is published at all (ADR 0012). The
	legend says that for each of the three, because copy implying that undecided
	means unseen would be a comforting untrue thing.

	**None of it is retroactive.** The label is stamped by the Sensor at
	publication, so a decision taken now does not reach what is already on the
	bus. A user who grants a contact and sees nothing happen must not conclude
	the product is broken, so the screen says it before they can — twice: once in
	the legend and once against the decision they just took.

	# Search, and a bulk control scoped to what the filter shows

	#137's rule, on a screen where the stakes are higher. A real account produced
	eighteen conversations and roughly 1,300 memberships in one day, so the list
	is long and the search is not a nicety. The bulk control acts on **what the
	filter is showing**, is named by what it will do and counted — and it asks
	twice, because "grant all" over a list containing a 246-member association is
	the affordance this screen exists to avoid. There is no control anywhere here
	that acts on the whole list.

	# Network defaults, and the precedence made visible

	A network-level decision is that network's default and a per-contact decision
	always overrides it. A user who granted a whole network and sees one contact
	still producing nothing has to be able to read why, so a row whose own
	decision disagrees with its network says so (`consent.row.overrides`).

	This screen **reads** the defaults and does not write them: a network
	decision decides for everybody on it at once, including people who have not
	written yet, and offering it beside a per-contact list would make the
	dangerous control the convenient one. The dashboard already says what it
	does, and #122 is the open question about how a whole-network grant should be
	offered.

	# Nothing here spins for ever

	Every failure arrives as an `Explained` from `$lib/consent/refusal.ts` — a
	cause and a next step for each of the Gateway's nine codes — and that type has
	no value meaning "still working" (#111, #135, #139).

	Two of the three reads are rendered *beside* the list rather than instead of
	it, because a read that fails should take away only what it answers
	(`$lib/consent/api.ts`): the display names come from the tail of the bus and
	can be unreadable while the list is perfectly true, and the people who have
	never been decided about come from a projection this deployment may not run at
	all — going blank for that would also hide every decision the user has already
	taken.

	# The owner, who is not a contact

	ADR 0018 and ADR 0021: the owner has no consent state, on any event. Since
	#149 the Gateway serves no row about them — not here, not in the snapshot,
	not in the pending list, and not one recorded before #109 stopped the Sensor
	producing it — so on a current deployment this screen finds none. The label
	stays anyway, and deliberately: an older Gateway behind this Companion still
	serves such a row, which is the situation #149's own premise describes, and
	hiding it would hide the only symptom of that a user can see while offering
	a decision on it would let them revoke their own traffic. What this browser
	can recognise is the owner's canonical Matrix ID; their network ghosts
	(`@whatsapp_lid-…`) are what #149 is about, and nothing here can know one —
	those the Gateway now holds as configuration and keeps to itself.
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import Icon from '$lib/icons/Icon.svelte';
	import { locale, t } from '$lib/i18n';
	import { session } from '$lib/session/state';
	import { networkNameKey, relativeTime } from '$lib/dashboard/format';
	import { decide, load, type Waiting } from '$lib/consent/api';
	import type { Explained } from '$lib/consent/refusal';
	import {
		awaiting,
		bulkDecisions,
		counts,
		matchesQuery,
		ownerRows,
		toRows,
		type Row,
		type State
	} from '$lib/consent/model';

	/** What became of one row's own decision, rendered against that row. */
	type Outcome =
		| { kind: 'recorded'; state: State; replayed: boolean }
		| { kind: 'refused'; problem: Explained };

	let rows = $state<Row[]>([]);
	let waiting = $state<Waiting | null>(null);
	let problem = $state<Explained | null>(null);
	let waitingProblem = $state<Explained | null>(null);
	let namesProblem = $state<Explained | null>(null);
	let loaded = $state(false);
	let refreshing = $state(false);
	let query = $state('');
	let busy = $state<string | null>(null);
	let outcomes = $state<Record<string, Outcome>>({});
	let now = $state(Date.now());

	/**
	 * The bulk control, armed and not yet applied.
	 *
	 * Two presses, because one press that writes ninety decisions is the control
	 * this screen exists to avoid. The armed value carries the count it was
	 * armed for, so a filter changed between the presses disarms it rather than
	 * silently deciding about a different set of people.
	 */
	let armed = $state<{ state: State; count: number } | null>(null);
	let bulkOutcome = $state<
		{ kind: 'done'; written: number; state: State } | { kind: 'refused'; problem: Explained } | null
	>(null);

	/** The owner's Matrix ID, the one owner identity a browser can recognise. */
	const owner = $derived($session.kind === 'live' ? $session.owner : null);

	onMount(() => {
		void refresh();
	});

	async function refresh() {
		refreshing = true;
		const answer = await load();
		if (answer.ok) {
			rows = toRows({
				pending: answer.loaded.pending,
				entries: answer.loaded.entries,
				names: answer.loaded.names,
				owner
			});
			waiting = answer.loaded.waiting;
			waitingProblem = answer.loaded.waitingProblem;
			namesProblem = answer.loaded.namesProblem;
			problem = null;
		} else {
			rows = [];
			waiting = null;
			waitingProblem = null;
			namesProblem = null;
			problem = answer.problem;
		}
		armed = null;
		now = Date.now();
		loaded = true;
		refreshing = false;
	}

	const shown = $derived(rows.filter((row) => matchesQuery(row, query)));
	const tally = $derived(counts(rows));
	const owners = $derived(ownerRows(rows));
	/** What a bulk press would actually write, over what is on screen. */
	const grantable = $derived(bulkDecisions(shown, 'granted'));
	const revocable = $derived(bulkDecisions(shown, 'revoked'));

	function networkLabel(network: string): string {
		const key = networkNameKey(network);
		return key === null ? network : $t(key);
	}

	/** A row's identity: a decision is about `(contact, network)`, never a contact alone. */
	function idOf(row: Row): string {
		return `${row.contact}|${row.network}`;
	}

	function when(instant: string | null): string | null {
		return relativeTime(instant, now, $locale);
	}

	/** Records one decision about one contact on one network. */
	async function set(row: Row, state: State) {
		if (busy !== null || row.isOwner) {
			return;
		}
		const id = idOf(row);
		busy = id;
		const answer = await decide(row.contact, row.network, state);
		busy = null;
		if (answer.ok) {
			outcomes = {
				...outcomes,
				[id]: { kind: 'recorded', state, replayed: answer.recorded.replayed }
			};
			// Re-read, so what the row says is in force comes from the Gateway
			// rather than from what this screen believes it just did.
			await refresh();
			return;
		}
		outcomes = { ...outcomes, [id]: { kind: 'refused', problem: answer.problem } };
	}

	function arm(state: State) {
		const count = state === 'granted' ? grantable.length : revocable.length;
		bulkOutcome = null;
		armed = count === 0 ? null : { state, count };
	}

	/**
	 * Applies the armed bulk decision: one request per contact, because there is
	 * no batch endpoint and a bulk control that hid the number of decisions it
	 * takes would be hiding the thing it has to be honest about.
	 *
	 * It stops at the first refusal and reports how many were written. A partial
	 * write is the truth — the journal already holds them — and saying "it
	 * failed" would leave the user believing nothing happened.
	 */
	async function applyBulk() {
		const request = armed;
		if (request === null || busy !== null) {
			return;
		}
		const decisions = bulkDecisions(shown, request.state);
		if (decisions.length !== request.count) {
			// The filter moved between the two presses. Disarm rather than
			// decide about a different set of people.
			armed = null;
			return;
		}
		busy = 'bulk';
		let written = 0;
		for (const decision of decisions) {
			const answer = await decide(decision.contact, decision.network, request.state);
			if (!answer.ok) {
				busy = null;
				armed = null;
				bulkOutcome = { kind: 'refused', problem: answer.problem };
				await refresh();
				return;
			}
			written += 1;
		}
		busy = null;
		armed = null;
		bulkOutcome = { kind: 'done', written, state: request.state };
		await refresh();
	}

	/** The one word for a state, in the user's own terms. */
	function stateName(state: State): string {
		return state === 'granted'
			? $t('consent.state.granted')
			: state === 'revoked'
				? $t('consent.state.revoked')
				: $t('consent.state.pending');
	}
</script>

<section class="screen" data-testid="screen-consent">
	<header class="stack">
		<p class="small">
			<a href="/dashboard">
				<Icon name="back" size="dense" />
				{$t('consent.back')}
			</a>
		</p>
		<h1>{$t('consent.title')}</h1>
		<p class="subtitle">{$t('consent.step')}</p>
		<button
			class="button button--secondary"
			type="button"
			onclick={refresh}
			disabled={refreshing}
			data-testid="refresh-consent"
		>
			<Icon name="reload" size="dense" />
			{refreshing ? $t('consent.refreshing') : $t('consent.refresh')}
		</button>
	</header>

	<!-- What this screen is, and the sentence the glossary insists on: this is
	     the user's decision, not the contact's agreement. -->
	<div class="card card--info" data-testid="consent-intro">
		<p class="card__title">
			<Icon name="consent" size="dense" />
			{$t('consent.intro.title')}
		</p>
		<p class="small">{$t('consent.intro.body')}</p>
		<p class="small" data-testid="not-theirs">{$t('consent.intro.notTheirs')}</p>
	</div>

	<!-- The three states, each with what it actually withholds. Always visible:
	     a user choosing between three words deserves to know that only one of
	     them stops content being published (ADR 0012). -->
	<div class="card" data-testid="consent-legend">
		<p class="card__title">{$t('consent.legend.title')}</p>
		<dl class="legend">
			<dt><span class="dot" data-tone="ok"></span>{$t('consent.state.granted')}</dt>
			<dd class="small">{$t('consent.legend.granted')}</dd>
			<dt><span class="dot" data-tone="idle"></span>{$t('consent.state.pending')}</dt>
			<dd class="small">{$t('consent.legend.pending')}</dd>
			<dt><span class="dot" data-tone="warn"></span>{$t('consent.state.revoked')}</dt>
			<dd class="small">{$t('consent.legend.revoked')}</dd>
		</dl>
		<p class="small" data-testid="never-decided-explained">
			{$t('consent.legend.neverDecided')}
		</p>
		<!-- Said here and again against a decision just taken. It is the thing a
		     user is most likely to read as a broken product. -->
		<p class="small warn" data-testid="not-retroactive">{$t('consent.legend.notRetroactive')}</p>
	</div>

	{#if problem !== null}
		<div
			class="card card--warning"
			role="alert"
			data-testid="listing-problem"
			data-code={problem.code}
		>
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t(problem.cause)}
			</p>
			<p class="small" data-testid="listing-remedy" data-remedy={problem.remedy}>
				{$t(problem.remedyText)}
			</p>
			{#if problem.remedy === 'retry' || problem.remedy === 'reload'}
				<p>
					<button
						class="button button--secondary"
						type="button"
						onclick={refresh}
						data-testid="retry-listing"
					>
						<Icon name="reload" size="dense" />
						{$t('consent.refresh')}
					</button>
				</p>
			{:else if problem.remedy === 'diagnostics'}
				<p><a class="small" href="/diagnostics">{$t('gate.diagnostics')}</a></p>
			{:else if problem.remedy === 'sign-in'}
				<p><a class="small" href="/signin?next=/consent">{$t('consent.remedy.signInLink')}</a></p>
			{/if}
		</div>
	{/if}

	{#if waitingProblem !== null}
		<!-- The people who have never been decided about are missing, and the
		     decisions already taken are not. A screen that went blank here would
		     hide what it could still show. -->
		<div
			class="card card--warning"
			role="alert"
			data-testid="waiting-problem"
			data-code={waitingProblem.code}
			data-remedy={waitingProblem.remedy}
		>
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t(waitingProblem.cause)}
			</p>
			<p class="small">{$t(waitingProblem.remedyText)}</p>
			<p class="small muted">{$t('consent.waiting.missing')}</p>
		</div>
	{/if}

	{#if namesProblem !== null}
		<!-- The names failed and the list did not. Rendered as a caveat on the
		     labels, never as a failure of the screen. -->
		<p class="card card--info small" data-testid="names-problem" data-code={namesProblem.code}>
			<Icon name="info" size="dense" />
			{$t('consent.names.unreadable')}
		</p>
	{/if}

	{#each owners as row (idOf(row))}
		<!-- ADR 0018, ADR 0021, #149. Said rather than hidden. -->
		<div class="card card--warning" role="alert" data-testid="owner-row">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('consent.owner.title')}
			</p>
			<p class="small">{$t('consent.owner.body', { id: row.contact })}</p>
			<p class="small muted">{$t('consent.owner.remedy')}</p>
		</div>
	{/each}

	{#if loaded && problem === null}
		<p class="counts" data-testid="consent-counts">
			<span class="chip" data-tone="idle" data-count={tally.awaiting}>
				{$t('consent.count.awaiting', { count: tally.awaiting })}
			</span>
			<span class="chip" data-tone="ok" data-count={tally.granted}>
				{$t('consent.count.granted', { count: tally.granted })}
			</span>
			<span class="chip" data-tone="warn" data-count={tally.revoked}>
				{$t('consent.count.revoked', { count: tally.revoked })}
			</span>
		</p>
		{#if waiting !== null && waiting.total !== tally.awaiting}
			<!-- The Gateway's own count of who is waiting, when it disagrees with
			     what this screen drew: a contact on a network this build does not
			     know is counted there and not here, and saying so beats
			     pretending the numbers match. -->
			<p class="small muted" data-testid="counts-differ">
				{$t('consent.count.gateway', { count: waiting.total })}
			</p>
		{/if}
	{/if}

	{#if loaded && problem === null && rows.length === 0}
		<div class="card" data-testid="consent-empty">
			<p class="card__title">
				<Icon name="consent" size="dense" />
				{$t('consent.empty.title')}
			</p>
			<p class="small">{$t('consent.empty.body')}</p>
		</div>
	{/if}

	{#if rows.length > 0}
		<div class="field">
			<label class="label" for="consent-search">{$t('consent.search.label')}</label>
			<input
				id="consent-search"
				class="input"
				type="search"
				autocomplete="off"
				spellcheck="false"
				placeholder={$t('consent.search.placeholder')}
				data-testid="consent-search"
				bind:value={query}
			/>
		</div>

		<p
			class="small muted"
			data-testid="consent-showing"
			data-shown={shown.length}
			data-total={rows.length}
		>
			{$t('consent.search.showing', { shown: shown.length, total: rows.length })}
		</p>

		<!--
			The bulk control. Named by what it will do, counted, and scoped to
			what the filter is showing — never to the whole list (#137). It asks
			twice, because the mistake it prevents is not recoverable by a
			second click.
		-->
		{#if shown.length > 0}
			<div class="card bulk" data-testid="consent-bulk">
				<p class="card__title">{$t('consent.bulk.title')}</p>
				<p class="small muted">{$t('consent.bulk.scope', { shown: shown.length })}</p>
				{#if armed === null}
					<div class="actions">
						<button
							class="button"
							type="button"
							disabled={grantable.length === 0 || busy !== null}
							onclick={() => arm('granted')}
							data-testid="bulk-grant"
							data-count={grantable.length}
						>
							<Icon name="check" size="dense" />
							{$t('consent.bulk.grant', { count: grantable.length })}
						</button>
						<button
							class="button"
							type="button"
							disabled={revocable.length === 0 || busy !== null}
							onclick={() => arm('revoked')}
							data-testid="bulk-revoke"
							data-count={revocable.length}
						>
							<Icon name="revoke" size="dense" />
							{$t('consent.bulk.revoke', { count: revocable.length })}
						</button>
					</div>
				{:else}
					<div
						class="card card--warning small"
						data-testid="bulk-confirm"
						data-count={armed.count}
						data-state={armed.state}
					>
						<p>
							{armed.state === 'granted'
								? $t('consent.bulk.confirmGrant', { count: armed.count })
								: $t('consent.bulk.confirmRevoke', { count: armed.count })}
						</p>
						<div class="actions">
							<button
								class="button button--primary"
								type="button"
								disabled={busy !== null}
								onclick={applyBulk}
								data-testid="bulk-apply"
							>
								{#if busy === 'bulk'}
									<span class="spinner" aria-hidden="true"></span>
									{$t('consent.bulk.applying')}
								{:else}
									{$t('consent.bulk.confirm')}
								{/if}
							</button>
							<button
								class="button button--secondary"
								type="button"
								disabled={busy !== null}
								onclick={() => (armed = null)}
								data-testid="bulk-cancel"
							>
								{$t('consent.bulk.cancel')}
							</button>
						</div>
					</div>
				{/if}
				{#if bulkOutcome !== null && bulkOutcome.kind === 'done'}
					<p class="card card--info small" role="status" data-testid="bulk-done">
						<Icon name="ok" size="dense" />
						{$t('consent.bulk.done', {
							count: bulkOutcome.written,
							state: stateName(bulkOutcome.state)
						})}
						{$t('consent.decided.notRetroactive')}
					</p>
				{:else if bulkOutcome !== null && bulkOutcome.kind === 'refused'}
					<div
						class="card card--warning small"
						role="alert"
						data-testid="bulk-problem"
						data-code={bulkOutcome.problem.code}
						data-remedy={bulkOutcome.problem.remedy}
					>
						<p class="card__title">
							<Icon name="warning" size="dense" />
							{$t(bulkOutcome.problem.cause)}
						</p>
						<p>{$t(bulkOutcome.problem.remedyText)}</p>
						<p>{$t('consent.bulk.partial')}</p>
					</div>
				{/if}
			</div>
		{/if}

		<ul class="contacts" data-testid="consent-rows">
			{#each shown as row (idOf(row))}
				{@const id = idOf(row)}
				{@const outcome = outcomes[id]}
				<li
					class="card contact"
					data-testid={`consent-row-${row.contact}-${row.network}`}
					data-state={row.state}
					data-decided-by={row.decidedBy}
					data-network={row.network}
					data-owner={row.isOwner ? 'yes' : 'no'}
				>
					<p class="card__title">
						<Icon name={row.network} size="dense" />
						<span class:derived={row.labelSource !== 'display-name'}>{row.label}</span>
					</p>
					{#if row.labelSource !== 'display-name'}
						<!-- Honest rather than blank: say what this label is. -->
						<p class="small muted" data-testid="by-id">{$t('consent.row.byId')}</p>
					{:else}
						<p class="small muted mono" data-testid="contact-id">{row.contact}</p>
					{/if}

					<p class="standing" data-testid="row-state">
						<span
							class="dot"
							data-tone={row.state === 'granted' ? 'ok' : row.state === 'revoked' ? 'warn' : 'idle'}
						></span>
						{awaiting(row)
							? $t('consent.row.neverDecided')
							: $t('consent.row.inForce', { state: stateName(row.state) })}
					</p>

					{#if row.decidedBy === 'network'}
						<p class="small muted" data-testid="from-network">
							{$t('consent.row.fromNetwork', {
								network: networkLabel(row.network),
								state: stateName(row.state)
							})}
						</p>
					{:else if row.overridesNetwork && row.networkDefault !== null}
						<!-- The precedence, where it matters: the user granted the
						     network and this contact is still not granted. -->
						<p class="small warn" data-testid="overrides-network">
							{$t('consent.row.overrides', {
								network: networkLabel(row.network),
								state: stateName(row.networkDefault)
							})}
						</p>
					{/if}

					{#if row.lastSeen !== null}
						<p class="small muted" data-testid="row-seen">
							{$t('consent.row.lastWrote', { when: when(row.lastSeen) ?? row.lastSeen })}
						</p>
					{/if}

					{#if row.isOwner}
						<p class="small warn" data-testid="row-is-owner">{$t('consent.row.isOwner')}</p>
					{:else}
						<div class="actions" data-testid="row-actions">
							{#each ['granted', 'pending', 'revoked'] as const as option (option)}
								{@const inForce = row.decidedBy === 'contact' && row.state === option}
								<!--
									The answer already recorded *for this contact* is shown as
									pressed and cannot be pressed again: the journal is
									append-only, so a second identical decision would add a row
									and publish an event that changed nothing. A row holding this
									state only by its **network's** default is still pressable,
									because asking for it per contact is asking for a decision
									that survives the default changing — the same rule
									`bulkDecisions` applies.
								-->
								<button
									class="button"
									class:button--primary={inForce}
									type="button"
									disabled={busy !== null || inForce}
									aria-pressed={inForce}
									onclick={() => set(row, option)}
									data-testid={`set-${option}`}
								>
									{#if busy === id}
										<span class="spinner" aria-hidden="true"></span>
									{/if}
									{option === 'granted'
										? $t('consent.action.grant')
										: option === 'revoked'
											? $t('consent.action.revoke')
											: $t('consent.action.undecide')}
								</button>
							{/each}
						</div>
						<p class="small muted" data-testid="undecide-meaning">
							{$t('consent.action.undecideMeaning')}
						</p>
					{/if}

					{#if outcome !== undefined && outcome.kind === 'recorded'}
						<p class="card card--info small" role="status" data-testid="row-recorded">
							<Icon name="ok" size="dense" />
							{outcome.replayed
								? $t('consent.decided.replayed', { state: stateName(outcome.state) })
								: $t('consent.decided.recorded', { state: stateName(outcome.state) })}
							{$t('consent.decided.notRetroactive')}
						</p>
					{:else if outcome !== undefined && outcome.kind === 'refused'}
						<div
							class="card card--warning small"
							role="alert"
							data-testid="row-problem"
							data-code={outcome.problem.code}
							data-remedy={outcome.problem.remedy}
						>
							<p class="card__title">
								<Icon name="warning" size="dense" />
								{$t(outcome.problem.cause)}
							</p>
							<p data-testid="row-remedy">{$t(outcome.problem.remedyText)}</p>
						</div>
					{/if}
				</li>
			{/each}
		</ul>

		{#if shown.length === 0}
			<p class="card small" data-testid="consent-no-match">{$t('consent.search.none')}</p>
		{/if}

		<!-- Where a whole-network decision lives, and where it does not. -->
		<p class="small muted" data-testid="network-defaults-note">
			{$t('consent.networks.note')}
		</p>
	{/if}
</section>

<style>
	.legend {
		margin: 0;
		display: grid;
		gap: var(--space-1);
	}

	.legend dt {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		font-weight: var(--font-weight-label);
	}

	.legend dd {
		margin: 0 0 var(--space-2) 0;
	}

	.counts {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
		margin: 0;
	}

	.chip {
		display: inline-flex;
		align-items: center;
		gap: var(--space-2);
		padding: var(--space-1) var(--space-3);
		border-radius: var(--radius-pill);
		border: 1px solid var(--color-border-strong);
		font-size: var(--font-size-sm);
	}

	.chip[data-tone='ok'] {
		border-color: var(--color-success);
	}

	.chip[data-tone='warn'] {
		border-color: var(--color-warning);
	}

	.contacts {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}

	.contact {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	/* A label that is an id rather than a name, set apart so the user can see
	   which it is without reading the caption. */
	.derived {
		font-family: var(--font-mono);
		overflow-wrap: anywhere;
	}

	.standing {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		margin: 0;
		font-weight: var(--font-weight-label);
	}

	.dot {
		width: 10px;
		height: 10px;
		border-radius: 50%;
		background: var(--color-border-strong);
		flex: 0 0 auto;
	}

	.dot[data-tone='ok'] {
		background: var(--color-success);
	}

	.dot[data-tone='warn'] {
		background: var(--color-warning);
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.bulk {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	/* The sentences a user is most likely to read as a broken product — "none of
	   this reaches the past", and a contact whose own answer beat their
	   network's. Set apart by a rule rather than by a colour, because amber text
	   on this ground is the contrast this app does not have a token for. */
	.warn {
		border-left: 3px solid var(--color-warning);
		padding-left: var(--space-3);
		font-weight: var(--font-weight-label);
	}
</style>
