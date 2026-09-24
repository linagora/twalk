<!--
	The approval screen (#100): what a persona proposed, and the one deliberate
	act that sends it.

	# A screen of its own, and why

	The dashboard is the home screen, unlocked on a train, and an approval queue
	shows proposed text. So the dashboard carries a count and a link — the count
	comes through `$lib/approvals/summary.ts`, which drops every word of every
	suggestion at the seam — and the content lives here, opened deliberately.
	It is the same reasoning that emptied the activity feed (#74).

	# What this screen cannot say, said on the screen (#160)

	It cannot name the contact. `GET /api/suggestions` carries the message being
	answered as an id and a type and nothing else, because naming the sender
	means opening their message, which is the leak #110 closed (ADR 0012). So a
	row says *"a reply to a message on WhatsApp"* and never *"a reply to
	Aïcha"*.

	This screen does not work around that. It does not fetch the inbound event,
	it does not ask a second endpoint, and it holds no cache that could be
	filled from elsewhere. What it does instead is say so, in
	`approvals.whoIsIt.*`, next to the row it affects: a user asked to be
	deliberate about something they cannot see deserves to be told that it is a
	decision and not a bug, and #160 is the open question about whether the
	trade is right. Building it is the evidence that question was waiting for,
	and the honest report is that the reply's own text carries most of the
	context — a persona writes in the language of the message it answers and
	quotes its subject — while *which conversation this is* is genuinely missing
	whenever two contacts are in flight at once.

	# Deliberate by construction (`CONTEXT.md`)

	Three properties, each visible in the markup and each with a test:

	  - **no batch.** There is no "approve all" control and no list-shaped
	    call. `$lib/approvals/api.ts` has no function that takes more than one
	    id, so writing one would be a change to a module whose comment is about
	    why it has none.
	  - **no default.** Nothing is selected, pre-ticked or focused for
	    submission. A row's approve button is the only thing that approves.
	  - **no keystroke.** The editor is a `textarea` inside no `form`, so
	    Enter inserts a newline and submits nothing; `approvals.editing.hint`
	    says so, and the journey presses Enter and asserts nothing was sent.

	# Nothing here spins for ever

	Every failure path arrives as an `Explained` from `$lib/approvals/refusal.ts`
	— a cause and a next step, for each of the Gateway's twenty-odd codes — and
	that type has no value meaning "still working". The three incidents this
	rule descends from (#111, #135, #139) were each a screen that met a failure
	it had no sentence for.

	The one refusal that is not a failure is `approval_published_but_not_recorded`:
	the reply **went out** and the Gateway could not write that down. It renders
	as a success with a caveat, because telling the user it failed is the lie
	that makes them send twice.

	# Lost replies

	A suggestion whose `standing` is `approved` and whose approval's
	`publication` is `unpublished` is a reply this deployment recorded and never
	sent. It is listed here with its text and a *Send it again* action, which
	republishes under the same deterministic id — the bus deduplicates, so it
	cannot go out twice. Both values of `publication` are terminal and neither
	is "in flight", so there is nothing here to render as a spinner.

	The other kind of lost reply — one that reached the bus and that the Sensor
	could not post into the room — is not visible from this origin at all: no
	event is published for it and no route reports it. This screen does not
	invent one; that gap is named in the pull request rather than papered over
	with a row that would always be empty.

	# The outgoing message, whole (#121, ADR 0019, ADR 0031)

	A reply a persona drafted goes out with one sentence after it, on a line of
	its own, in the language it was written in — "Rédigé avec mon assistant
	IA." — and the Gateway appends it at approval. So what the contact receives
	is not what the blockquote shows, and this screen says so at the one
	moment the user is thinking about a particular message going to a
	particular person: the sentence stands under the body, and under the editor
	while editing, drawn as fixed and labelled as not editable here, because it
	is not in the field the user edits and cannot be removed from one reply.
	When the switch is off (`GET /api/settings/disclosure`, read once per
	load), the line says that instead, with the date — a fact the user may have
	forgotten deciding. A suggestion that carries no sentence at all goes out
	undisclosed and is said to; a switch that could not be read is said to be
	unread rather than guessed on. Nothing here composes the sentence: it is
	the suggestion's own member, and the Gateway appends exactly that.
-->
<script lang="ts">
	import { onDestroy, onMount } from 'svelte';

	import Icon from '$lib/icons/Icon.svelte';
	import { locale, t } from '$lib/i18n';
	import { networkNameKey, relativeTime } from '$lib/dashboard/format';
	import { gateway } from '$lib/api/client';
	import {
		answeredMessage,
		approve,
		loadSuggestions,
		type AnsweredMessage
	} from '$lib/approvals/api';
	import { loadDisclosure } from '$lib/settings/api';
	import { disclosureRecord, type DisclosureState } from '$lib/settings/model';
	import { personaRows } from '$lib/dashboard/model';
	import { emptyApprovalsKey, readRuntime, UNKNOWN, type Runtime } from '$lib/runtime/presence';
	import { dismiss, dismissed, restoreAll } from '$lib/approvals/dismissed';
	import type { Explained } from '$lib/approvals/refusal';
	import {
		deliveryCopy,
		deliveryDetailKey,
		goneStale,
		noticesFor,
		postedCopy,
		toRows,
		triggerKey,
		type Listing,
		type Notice,
		type Row
	} from '$lib/approvals/rows';

	/** What became of one row's own action, rendered against that row. */
	type Outcome =
		| { kind: 'sent'; sequence: number | null; edited: boolean; by: string; text: string }
		| { kind: 'refused'; problem: Explained };

	let listing = $state<Listing | null>(null);
	let problem = $state<Explained | null>(null);
	let loaded = $state(false);
	let refreshing = $state(false);
	let hidden = $state<Set<string>>(new Set());
	/** Which row is open in the editor, and what the user has typed into it. */
	let editing = $state<string | null>(null);
	let draft = $state('');
	let busy = $state<string | null>(null);
	let outcomes = $state<Record<string, Outcome>>({});
	let now = $state(Date.now());

	let ticker: ReturnType<typeof setInterval> | null = null;

	/**
	 * The two facts the empty state needs to say *why* it is empty (#177):
	 * whether an agent runtime is here at all (#189), and whether the user has
	 * activated any assistant — a read of the consent state this screen never
	 * made, although the dashboard makes it. Neither is a value that means
	 * "still working": `unknown` and `null` are rendered as not knowing.
	 */
	let runtime = $state<Runtime>(UNKNOWN);
	let anyPersonaActive = $state<boolean | null>(null);

	/**
	 * The disclosure switch, read once per load and again on refresh (#121).
	 * `null` is "could not be read", which the row says rather than treating as
	 * either state: whether the sentence goes out is the Gateway's decision at
	 * approval, and this screen only reports what it was told.
	 */
	let disclosureSwitch = $state<DisclosureState | null>(null);

	onMount(() => {
		hidden = dismissed();
		void refresh();
		void readRuntime().then((read) => {
			runtime = read;
		});
		void gateway
			.GET('/api/consent/state')
			.then((answer) => {
				anyPersonaActive =
					answer.data === undefined
						? null
						: personaRows(answer.data.entries).some((row) => row.active);
			})
			.catch(() => {
				anyPersonaActive = null;
			});
		// The relative times, and the staleness warning. No polling: this
		// screen re-reads when the user asks, because a list that reordered
		// itself under a finger about to press approve is the last thing an
		// approval queue should do.
		ticker = setInterval(() => {
			now = Date.now();
		}, 30_000);
	});

	onDestroy(() => {
		if (ticker !== null) {
			clearInterval(ticker);
		}
	});

	async function refresh() {
		refreshing = true;
		const [answer, switched] = await Promise.all([loadSuggestions(), loadDisclosure()]);
		disclosureSwitch = switched.ok ? switched.state : null;
		if (answer.ok) {
			listing = answer.listing;
			problem = null;
		} else {
			listing = null;
			problem = answer.problem;
		}
		now = Date.now();
		loaded = true;
		refreshing = false;
	}

	const rows = $derived<Row[]>(listing === null ? [] : toRows(listing, hidden));
	const hiddenHere = $derived(
		listing === null
			? 0
			: listing.suggestions.filter((entry) => hidden.has(entry.event_id)).length
	);
	const notices = $derived<Notice[]>(listing === null ? [] : noticesFor(listing, hiddenHere));

	function networkLabel(network: string): string {
		const key = networkNameKey(network);
		return key === null ? network : $t(key);
	}

	/**
	 * Whose words the open editor holds (#327). Correcting the draft leaves
	 * it the persona's — the sentence still describes how the text was
	 * produced — and writing one's own reply in its place does not, which
	 * is ADR 0019's own rule: *a message the user wrote themselves carries
	 * nothing*. Two gestures, because no comparison of two texts can tell a
	 * correction from a replacement.
	 */
	let authorship = $state<'persona' | 'owner'>('persona');

	function openEditor(row: Row) {
		editing = row.id;
		authorship = 'persona';
		draft = row.body;
	}

	function openOwnReply(row: Row) {
		editing = row.id;
		authorship = 'owner';
		draft = '';
	}

	function closeEditor() {
		editing = null;
		authorship = 'persona';
		draft = '';
	}

	/**
	 * The one call that sends. `edited` is passed only when the text actually
	 * differs from what the persona wrote, so the audit trail's `edited` flag
	 * means what it says.
	 */
	async function send(row: Row) {
		if (busy !== null) {
			return;
		}
		const text = editing === row.id ? draft : row.body;
		if (text.trim().length === 0) {
			outcomes = {
				...outcomes,
				[row.id]: {
					kind: 'refused',
					problem: {
						code: 'empty_reply',
						cause: 'approvals.editing.empty',
						remedyText: 'approvals.remedy.none',
						remedy: 'none',
						sent: false
					}
				}
			};
			return;
		}
		busy = row.id;
		const answer = await approve(
			row.id,
			text === row.body ? undefined : text,
			editing === row.id ? authorship : 'persona'
		);
		busy = null;
		if (answer.ok) {
			outcomes = {
				...outcomes,
				[row.id]: {
					kind: 'sent',
					sequence: answer.approval.stream_sequence ?? null,
					edited: answer.approval.edited,
					by: answer.approval.approved_by,
					// What went out, remembered here and nowhere else: the Gateway
					// holds no text (ADR 0022), so after an edit this screen is the
					// only place the user's own words can be read back (#217).
					text
				}
			};
			closeEditor();
			// Re-read, so the row's standing comes from the Gateway rather than
			// from what this screen believes it just did.
			await refresh();
			return;
		}
		outcomes = { ...outcomes, [row.id]: { kind: 'refused', problem: answer.problem } };
	}

	function refuse(row: Row) {
		hidden = dismiss(row.id);
		if (editing === row.id) {
			closeEditor();
		}
	}

	function restore() {
		hidden = restoreAll();
	}

	/** A relative time, falling back to the instant itself rather than to nothing. */
	function when(instant: string): string {
		return relativeTime(instant, now, $locale) ?? instant;
	}

	/**
	 * The message being answered, per suggestion, read only when the owner
	 * opens it (#336). Nothing here is fetched with the listing: the
	 * Gateway's route is the one door onto a contact's words, and it reads
	 * consent at the moment it is asked — so a refusal is rendered as what
	 * it says, never as an empty quote.
	 */
	let answered = $state<Record<string, AnsweredMessage | Explained | 'reading'>>({});

	async function readAnswered(id: string): Promise<void> {
		if (answered[id] !== undefined && answered[id] !== 'reading') {
			return;
		}
		answered = { ...answered, [id]: 'reading' };
		const result = await answeredMessage(id);
		answered = { ...answered, [id]: result.ok ? result.message : result.problem };
	}

	function isExplained(value: AnsweredMessage | Explained): value is Explained {
		return 'code' in value;
	}
</script>

{#snippet disclosureLine(row: Row)}
	<!-- The rest of the outgoing message (#121): the sentence, fixed, or the
	     reason there is none. One line, drawn once per row — under the editor
	     while editing, under the body otherwise — and only on a row that is
	     still to be decided: the switch read here is the switch *now*, and a
	     reply already approved went out under the switch as it stood at the
	     press, which the Gateway decided and this screen cannot read back
	     yet. Saying "goes out with" or "is not added" about a sealed row
	     would be a sentence about the present put on a message from the past. -->
	{#if row.standing === 'approvable' && (outcomes[row.id]?.kind ?? 'none') !== 'sent'}
		{#if disclosureSwitch !== null && !disclosureSwitch.enabled}
			<p class="small muted disclosure-off" data-testid="disclosure-off">
				<Icon name="info" size="dense" />
				{$t('approvals.disclosure.off', {
					date: disclosureRecord(disclosureSwitch, $locale).values.date
				})}
			</p>
		{:else if row.disclosure === null}
			<p class="small muted" data-testid="disclosure-none">{$t('approvals.disclosure.none')}</p>
		{:else}
			<div class="disclosure" data-testid="disclosure" data-switch={disclosureSwitch === null ? 'unread' : 'on'}>
				<p class="small muted disclosure__label">
					<Icon name="fixed" size="dense" />
					{$t('approvals.disclosure.label')}
				</p>
				<p class="disclosure__sentence" aria-readonly="true" data-testid="disclosure-sentence">
					{row.disclosure}
				</p>
				<p class="small muted">
					{disclosureSwitch === null
						? $t('approvals.disclosure.unknown')
						: $t('approvals.disclosure.fixed')}
				</p>
			</div>
		{/if}
	{/if}
{/snippet}

<section class="screen" data-testid="screen-approvals">
	<header class="stack">
		<p class="small">
			<a href="/dashboard">
				<Icon name="back" size="dense" />
				{$t('approvals.back')}
			</a>
		</p>
		<h1>{$t('approvals.title')}</h1>
		<p class="subtitle">{$t('approvals.step')}</p>
		<button
			class="button button--secondary"
			type="button"
			onclick={refresh}
			disabled={refreshing}
			data-testid="refresh-approvals"
		>
			<Icon name="reload" size="dense" />
			{refreshing ? $t('approvals.refreshing') : $t('approvals.refresh')}
		</button>
	</header>

	{#if problem !== null}
		<div class="card card--warning" role="alert" data-testid="listing-problem" data-code={problem.code}>
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t(problem.cause)}
			</p>
			<p class="small" data-testid="listing-remedy" data-remedy={problem.remedy}>
				{$t(problem.remedyText)}
			</p>
			{#if problem.remedy === 'retry' || problem.remedy === 'reload'}
				<p>
					<button class="button button--secondary" type="button" onclick={refresh} data-testid="retry-listing">
						<Icon name="reload" size="dense" />
						{$t('approvals.refresh')}
					</button>
				</p>
			{:else if problem.remedy === 'diagnostics'}
				<p><a class="small" href="/diagnostics">{$t('gate.diagnostics')}</a></p>
			{/if}
		</div>
	{/if}

	{#each notices as notice (notice.kind)}
		<p class="card card--info small" data-testid={`notice-${notice.kind}`} data-count={notice.count}>
			<Icon name="info" size="dense" />
			{#if notice.kind === 'window'}
				{$t('approvals.notice.window')}
			{:else if notice.kind === 'truncated'}
				{$t('approvals.notice.truncated')}
			{:else if notice.kind === 'unreadable'}
				{$t('approvals.notice.unreadable', { count: notice.count })}
			{:else}
				{$t('approvals.notice.dismissed', { count: notice.count })}
			{/if}
		</p>
	{/each}

	{#if hiddenHere > 0}
		<p class="small">
			<button class="linkish" type="button" onclick={restore} data-testid="restore-dismissed">
				{$t('approvals.dismissed.restore')}
			</button>
		</p>
	{/if}

	{#if loaded && problem === null && rows.length === 0}
		{@const why = emptyApprovalsKey(runtime, anyPersonaActive)}
		<div class="card" data-testid="approvals-empty" data-why={why.slice('approvals.empty.'.length)}>
			<p class="card__title">
				<Icon name="persona" size="dense" />
				{$t('approvals.empty.title')}
			</p>
			<p class="small">{$t('approvals.empty.body')}</p>
			<!-- Three situations, three sentences (#177): no runtime to propose
			     anything; a runtime and nothing activated; an active assistant
			     that proposed nothing — the common case, and the one a single
			     sentence for all three used to bury. -->
			<p class="small muted" data-testid="approvals-empty-why">{$t(why)}</p>
		</div>
	{/if}

	{#each rows as row (row.id)}
		{@const outcome = outcomes[row.id]}
		<article
			class="card suggestion"
			data-testid={`suggestion-${row.id}`}
			data-standing={row.standing}
			data-lost={row.lostReply ? 'yes' : 'no'}
			data-network={row.network}
		>
			<p class="card__title">
				<Icon name="persona" size="dense" />
				{$t('approvals.row.persona', { persona: row.personaId })}
			</p>

			<p class="small muted" data-testid="trigger">
				{#if row.trigger.event_type === 'fr.linagora.twalk.inbound.message.received.v1'}
					{$t(triggerKey(row.trigger.event_type), { network: networkLabel(row.network) })}
				{:else}
					{$t(triggerKey(row.trigger.event_type), {
						network: networkLabel(row.network),
						type: row.trigger.event_type
					})}
				{/if}
			</p>

			{#if row.context !== null}
				<!-- What the persona says it is answering (#334, #335): read
				     before the text it proposes, so the user knows why before
				     they judge what. The persona's words about the message,
				     never the message — that is a click of its own (#336). -->
				<p class="small muted" data-testid="context">
					{#if row.context.contact !== null && row.context.contact !== undefined}
						{$t('approvals.context.named', {
							contact: row.context.contact,
							summary: row.context.summary
						})}
					{:else}
						{row.context.summary}
					{/if}
				</p>
			{/if}

			{#if outcome !== undefined && outcome.kind === 'sent' && outcome.edited}
				<!-- Once an edited reply has gone out, what matters is what went
				     out, and that is the user's text — which this screen has in
				     hand because it just submitted it. The persona's words stay
				     reachable: "how often do I correct my assistant" is a question
				     a user may want answered (#217). -->
				<p class="small muted" data-testid="approved-label">{$t('approvals.approvedText.label')}</p>
				<blockquote class="proposed" data-testid="approved-text">{outcome.text}</blockquote>
				<details class="small" data-testid="original">
					<summary>{$t('approvals.approvedText.original')}</summary>
					<blockquote class="proposed muted" data-testid="proposed">{row.body}</blockquote>
				</details>
			{:else}
				<!-- The thing being approved: the persona's own words, and the only
				     text on this screen. -->
				<blockquote class="proposed" data-testid="proposed">{row.body}</blockquote>
			{/if}
			{#if editing !== row.id}
				{@render disclosureLine(row)}
			{/if}

			<!-- #336: the message being answered, on demand and on this screen
			     alone. A click, not a line: the words of somebody who did not
			     choose to be here are read when the owner needs them to
			     decide, and the Gateway checks consent again to serve them. -->
			<details
				class="small answered"
				data-testid="answered"
				ontoggle={(event) =>
					(event.currentTarget as HTMLDetailsElement).open ? readAnswered(row.id) : undefined}
			>
				<summary>{$t('approvals.answered.open')}</summary>
				{#if answered[row.id] === 'reading' || answered[row.id] === undefined}
					<p class="muted" data-testid="answered-reading">{$t('approvals.answered.loading')}</p>
				{:else if isExplained(answered[row.id] as AnsweredMessage | Explained)}
					{@const problem = answered[row.id] as Explained}
					<p class="muted" data-testid="answered-problem" data-code={problem.code}>
						{$t(problem.cause)} — {$t(problem.remedyText)}
					</p>
				{:else}
					{@const message = answered[row.id] as AnsweredMessage}
					<p class="muted" data-testid="answered-header">
						{message.contact ?? ''}
						{#if message.received_at !== null}
							· {$t('approvals.answered.received', { when: when(message.received_at) })}
						{/if}
						· {$t('approvals.answered.attachments', { count: message.attachments })}
					</p>
					<blockquote class="proposed muted" data-testid="answered-body">{message.body}</blockquote>
				{/if}
			</details>

			<p class="small muted" data-testid="timing">
				{$t('approvals.row.produced', { when: when(row.producedAt) })}
				{#if row.expiresAt !== null}
					· {$t('approvals.row.expires', { when: when(row.expiresAt) })}
				{:else}
					· {$t('approvals.row.expiresNever')}
				{/if}
				{#if row.attempt !== null && row.attempt > 1}
					· {$t('approvals.row.attempt', { attempt: row.attempt })}
				{/if}
			</p>

			<p class="standing" data-testid="standing">
				<span class="dot" data-tone={row.standing === 'approvable' ? 'ok' : row.lostReply ? 'warn' : 'idle'}
				></span>
				{row.standing === 'approvable'
					? $t('approvals.standing.approvable')
					: row.standing === 'expired'
						? $t('approvals.standing.expired')
						: $t('approvals.standing.approved')}
			</p>

			{#if row.standing === 'expired'}
				<p class="small" data-testid="expired-reason">{$t('approvals.expired.reason')}</p>
			{:else if goneStale(row, now)}
				<p class="small" role="status" data-testid="stale">{$t('approvals.stale')}</p>
			{/if}

			{#if row.lostReply}
				<div class="card card--warning small" data-testid="lost-reply">
					<p class="card__title">
						<Icon name="warning" size="dense" />
						{$t('approvals.lost.title')}
					</p>
					<p>{$t('approvals.lost.body')}</p>
				</div>
			{:else if row.standing === 'approved' && row.approval !== null}
				<p class="small" data-testid="already-sent">
					<Icon name="ok" size="dense" />
					{$t('approvals.sent.body', {
						owner: row.approval.approved_by,
						sequence: row.approval.stream_sequence ?? 0
					})}
					{row.approval.edited ? $t('approvals.sent.edited') : $t('approvals.sent.unedited')}
				</p>
				<!-- Published is not delivered (#216): a second sentence, from the
				     Sensor's own report when there is one, and never from the
				     approval's `publication`. -->
				{@const givenUp = row.undelivered !== null && row.undelivered !== undefined}
				<p
					class="small {givenUp || row.posted?.reach === 'nobody' ? 'card card--warning' : ''}"
					data-testid="delivered"
					data-reach={givenUp ? 'undelivered' : (row.posted?.reach ?? 'pending')}
				>
					<Icon
						name={givenUp || row.posted?.reach === 'nobody' ? 'warning' : 'ok'}
						size="dense"
					/>
					{$t(postedCopy(row), {
						postedAs: row.posted?.posted_as ?? '',
						reason: row.undelivered?.reason ?? '',
						detail: $t(deliveryDetailKey(row.delivery))
					})}
				</p>
			{/if}

			<!-- Before the button, not after it: whether this reply can reach the
			     contact at all (#216). `cannot_reach` is a certainty the Gateway
			     read off the homeserver, so it is drawn as a warning; the button
			     stays, because the Gateway is the authority on refusing and a
			     screen that hid it would be a second, disagreeing one. -->
			{#if row.standing === 'approvable'}
				{@const delivery = deliveryCopy(row.delivery)}
				<p
					class="small {delivery.warns ? 'card card--warning' : 'muted'}"
					role={delivery.warns ? 'alert' : undefined}
					data-testid="delivery"
					data-reach={row.delivery.reach}
					data-detail={row.delivery.detail}
				>
					{#if delivery.warns}<Icon name="warning" size="dense" />{/if}
					{$t(delivery.key, { detail: $t(deliveryDetailKey(row.delivery)) })}
				</p>
			{/if}

			{#if editing === row.id}
				<label class="field" for={`editor-${row.id}`}>
					<span class="label">
						{authorship === 'owner'
							? $t('approvals.writeOwn.label')
							: $t('approvals.editing.label')}
					</span>
					<!-- Not inside a form, so no key submits it. -->
					<textarea
						id={`editor-${row.id}`}
						class="input editor"
						bind:value={draft}
						rows="4"
						data-testid="editor"
					></textarea>
				</label>
				<p class="small muted" data-testid="editor-hint">{$t('approvals.editing.hint')}</p>
				{#if authorship === 'owner'}
					<!-- ADR 0019: a message the user wrote themselves carries
					     nothing, so the card says so before they send it rather
					     than leaving them to notice its absence afterwards. -->
					<p class="small muted" data-testid="own-reply-undisclosed">
						{$t('approvals.writeOwn.undisclosed')}
					</p>
				{:else}
					<!-- Under the editor, not inside it: the one line the user does
					     not write. -->
					{@render disclosureLine(row)}
				{/if}
			{/if}

			{#if outcome !== undefined && outcome.kind === 'sent'}
				<p class="card card--info small" role="status" data-testid="sent">
					<Icon name="ok" size="dense" />
					{$t('approvals.sent.title')} ·
					{$t('approvals.sent.body', { owner: outcome.by, sequence: outcome.sequence ?? 0 })}
					{outcome.edited ? $t('approvals.sent.edited') : $t('approvals.sent.unedited')}
				</p>
				{#if row.standing !== 'approved'}
					<!-- The re-read has not landed yet: what is known about delivery
					     is what was known before the button. -->
					<p class="small muted" data-testid="delivered" data-reach="pending">
						{$t(postedCopy(row), {
							postedAs: '',
							reason: row.undelivered?.reason ?? '',
							detail: $t(deliveryDetailKey(row.delivery))
						})}
					</p>
				{/if}
			{:else if outcome !== undefined && outcome.kind === 'refused'}
				<div
					class="card card--warning small"
					role="alert"
					data-testid="row-problem"
					data-code={outcome.problem.code}
					data-remedy={outcome.problem.remedy}
					data-sent={outcome.problem.sent ? 'yes' : 'no'}
				>
					<p class="card__title">
						<Icon name={outcome.problem.sent ? 'ok' : 'warning'} size="dense" />
						{$t(outcome.problem.cause)}
					</p>
					<p data-testid="row-remedy">{$t(outcome.problem.remedyText)}</p>
				</div>
			{/if}

			<div class="actions">
				{#if row.actions.includes('approve')}
					<button
						class="button button--primary"
						type="button"
						onclick={() => send(row)}
						disabled={busy !== null}
						data-testid="approve"
					>
						{#if busy === row.id}
							<span class="spinner" aria-hidden="true"></span>
							{$t('approvals.approving')}
						{:else}
							<Icon name="check" size="dense" />
							{editing === row.id ? $t('approvals.editing.save') : $t('approvals.approve')}
						{/if}
					</button>
				{/if}
				{#if row.actions.includes('retry')}
					<button
						class="button button--primary"
						type="button"
						onclick={() => send(row)}
						disabled={busy !== null}
						data-testid="retry-send"
					>
						{#if busy === row.id}
							<span class="spinner" aria-hidden="true"></span>
							{$t('approvals.approving')}
						{:else}
							<Icon name="reload" size="dense" />
							{$t('approvals.lost.retry')}
						{/if}
					</button>
				{/if}
				{#if row.actions.includes('edit')}
					<button
						class="button button--secondary"
						type="button"
						onclick={() =>
							editing === row.id && authorship === 'persona' ? closeEditor() : openEditor(row)}
						data-testid="edit"
					>
						{editing === row.id && authorship === 'persona'
							? $t('approvals.editing.cancel')
							: $t('approvals.edit')}
					</button>
				{/if}
				{#if row.actions.includes('edit')}
					<button
						class="button button--secondary"
						type="button"
						onclick={() =>
							editing === row.id && authorship === 'owner' ? closeEditor() : openOwnReply(row)}
						data-testid="write-own"
					>
						{editing === row.id && authorship === 'owner'
							? $t('approvals.editing.cancel')
							: $t('approvals.writeOwn')}
					</button>
				{/if}
				{#if row.actions.includes('dismiss')}
					<button
						class="button button--secondary"
						type="button"
						onclick={() => refuse(row)}
						data-testid="refuse"
					>
						<Icon name="unavailable" size="dense" />
						{$t('approvals.dismiss')}
					</button>
				{/if}
			</div>

			{#if row.actions.includes('dismiss')}
				<p class="small muted" data-testid="refuse-meaning">{$t('approvals.dismiss.meaning')}</p>
			{/if}

			<details class="who" data-testid="who-is-it">
				<summary class="small">{$t('approvals.whoIsIt.title')}</summary>
				<p class="small muted">{$t('approvals.whoIsIt.body')}</p>
				<p class="small mono" data-testid="trigger-id">
					{$t('approvals.trigger.id', { id: row.trigger.event_id })}
				</p>
			</details>
		</article>
	{/each}

	{#if rows.length > 0}
		<p class="small muted" data-testid="deliberate">{$t('approvals.deliberate')}</p>
	{/if}
</section>

<style>
	.suggestion {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}

	/* The proposed reply, set apart from everything that describes it: it is
	   the text the user is about to put their name on. */
	.proposed {
		margin: 0;
		padding: var(--space-3);
		border-left: 3px solid var(--color-primary);
		background: var(--color-primary-surface);
		border-radius: var(--radius-md);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}

	.editor {
		min-height: calc(var(--space-7) * 2);
		resize: vertical;
		font: inherit;
	}

	/* The disclosure: the same block as the proposed reply, so it reads as
	   part of the outgoing message, and a dashed edge with a lock so it reads
	   as the part nobody edits here. */
	.disclosure {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}

	.disclosure__label {
		margin: 0;
	}

	.disclosure__sentence {
		margin: 0;
		padding: var(--space-2) var(--space-3);
		border-left: 3px dashed var(--color-border-strong);
		background: var(--color-surface-raised);
		color: var(--color-text-muted);
		border-radius: var(--radius-md);
		font-style: italic;
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}

	.disclosure-off {
		margin: 0;
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

	.who > summary {
		cursor: pointer;
		color: var(--color-text-muted);
	}

	.linkish {
		background: none;
		border: none;
		padding: 0;
		color: var(--color-primary);
		text-decoration: underline;
		cursor: pointer;
		font: inherit;
	}
</style>
