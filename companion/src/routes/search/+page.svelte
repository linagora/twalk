<!--
	The search screen (lot 3a): the owner's own archive, searched through the
	Gateway and never read by it.

	# The first surface where a contact may be named

	Spec §9.3 makes this deliberate. The approval screen cannot name the person
	it is answering (#97, ADR 0012) because all it holds is an id and a type.
	Here the owner is searching *their own* messages, in *their own* session,
	and telling them who wrote is the function of the screen, not a leak. The
	text of the excerpts stays bounded all the same: a row carries a `snippet`,
	never a body — `$lib/search/model.ts` builds the row member by member so no
	text could reach the markup through a spread.

	# Four guards, each one requirement of the design (§9)

	  1. **The withdrawal is said, not hidden.** A correspondent the user revoked
	     is removed by the collector, which counts what it removed; the screen
	     renders that count at the head of the answer. A revoked contact is not
	     an absence.
	  2. **The bound is said.** A search returns at most `SEARCH_LIMIT` hits, and
	     when the answer is full the screen says so rather than letting the user
	     believe they have seen everything.
	  3. **A name here is legitimate** — the paragraph above.
	  4. **A click opens the mail in the mail client, not in Twalk.** Twalk
	     renders the trace and never the document; the hand-over is
	     `search.open`, a `twalk://` link a mail client can claim.

	# Nothing here spins for ever

	Every failure arrives as a `Refusal` from `$lib/search/model.ts` — a cause
	and a next step for every code the Gateway enumerates, and a named answer
	for one it does not — and that type has no value meaning "still working"
	(#111, #135, #139). The screen runs no search on mount: an empty query is a
	refusal at the Gateway, and an empty box is not a failure.
-->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import { locale, t } from '$lib/i18n';
	import { search } from '$lib/search/load';
	import {
		atBound,
		documentLink,
		when,
		withheldCopy,
		SEARCH_LIMIT,
		type Refusal,
		type Row
	} from '$lib/search/model';

	let query = $state('');
	let asked = $state('');
	let results = $state<Row[]>([]);
	let count = $state(0);
	let withheld = $state(0);
	let refusal = $state<Refusal | null>(null);
	/** Whether a search has been run at all — the empty box is not a result. */
	let searched = $state(false);
	let busy = $state(false);

	/**
	 * Whether a search has anything to run. `$derived` rather than a plain
	 * value so the button follows the box and a search in flight blocks a
	 * second: two searches in flight could answer out of order, and the screen
	 * would show the older one's hits under the newer one's word.
	 */
	const canSubmit = $derived(query.trim().length > 0 && !busy);

	async function run() {
		if (!canSubmit) {
			return;
		}
		busy = true;
		const text = query.trim();
		const answer = await search({ q: text, limit: SEARCH_LIMIT });
		asked = text;
		searched = true;
		if (answer.ok) {
			results = answer.rows;
			count = answer.count;
			withheld = answer.withheld;
			refusal = null;
		} else {
			results = [];
			count = 0;
			withheld = 0;
			refusal = answer.refusal;
		}
		busy = false;
	}

	function submit(event: SubmitEvent) {
		event.preventDefault();
		void run();
	}

	const withheld_ = $derived(withheldCopy({ withheld }, $t));

	/** A hit's date, in the interface's language, or nothing when it carries none. */
	function dated(row: Row): string | null {
		return when(row.date, $locale);
	}
</script>

<section class="screen" data-testid="screen-search">
	<header class="stack">
		<p class="small">
			<a href="/dashboard">
				<Icon name="back" size="dense" />
				{$t('search.back')}
			</a>
		</p>
		<h1>{$t('search.title')}</h1>
		<p class="subtitle">{$t('search.step')}</p>

		<form class="field" onsubmit={submit} novalidate>
			<label class="label" for="archive-search">{$t('search.label')}</label>
			<div class="input-row">
				<input
					id="archive-search"
					class="input"
					name="q"
					type="search"
					autocomplete="off"
					spellcheck="false"
					placeholder={$t('search.placeholder')}
					data-testid="search-input"
					bind:value={query}
					disabled={busy}
				/>
			</div>
			<p>
				<button
					class="button button--primary"
					type="submit"
					disabled={!canSubmit}
					data-testid="search-submit"
				>
					{busy ? $t('search.searching') : $t('search.submit')}
				</button>
			</p>
		</form>
	</header>

	{#if refusal !== null}
		<!-- A refusal is a sentence and a next step, never a spinner. The
		     code is a data attribute so a journey can assert which one it was. -->
		<div
			class="card card--warning"
			role="alert"
			data-testid="search-problem"
			data-code={refusal.code}
			data-remedy={refusal.remedy}
		>
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t(refusal.cause)}
			</p>
			<p class="small" data-testid="search-remedy">{$t(refusal.remedyText)}</p>
			{#if refusal.remedy === 'retry' || refusal.remedy === 'reload'}
				<p>
					<button
						class="button button--secondary"
						type="button"
						onclick={run}
						disabled={!canSubmit}
						data-testid="retry-search"
					>
						<Icon name="reload" size="dense" />
						{$t('search.submit')}
					</button>
				</p>
			{:else if refusal.remedy === 'diagnostics'}
				<p><a class="small" href="/diagnostics">{$t('gate.diagnostics')}</a></p>
			{:else if refusal.remedy === 'sign-in'}
				<p><a class="small" href="/signin?next=/search">{$t('search.remedy.signIn')}</a></p>
			{/if}
		</div>
	{:else if searched}
		<!-- The result head: how many, the withdrawal, and the bound. The
		     withdrawal is a sentence of its own and deliberately not folded into
		     the count — a count that hid it would be the silence §5.3 forbids. -->
		<div class="head" data-testid="search-headline">
			<p data-testid="search-count">{$t('search.count', { count })}</p>
			{#if withheld_ !== null}
				<p class="card card--info small" data-testid="search-withheld" role="status">
					<Icon name="info" size="dense" />
					{withheld_}
				</p>
			{/if}
			{#if atBound({ count })}
				<p class="small muted" data-testid="search-window">
					{$t('search.window', { count: SEARCH_LIMIT })}
				</p>
			{/if}
		</div>

		{#if results.length === 0}
			<p class="card card--info" data-testid="search-empty">
				{$t('search.empty')}
			</p>
		{:else}
			<ul class="hits" data-testid="search-results">
				{#each results as row (row.id === '' ? `${row.source}:${row.date}:${row.subject}` : row.id)}
					{@const href = documentLink(row)}
					{@const date = dated(row)}
					<li class="hit card" data-testid="search-hit" data-id={row.id}>
						<p class="hit__head">
							<span class="hit__subject" data-testid="hit-subject">
								{row.subject === '' ? row.snippet : row.subject}
							</span>
							{#if date !== null}
								<span class="small muted" data-testid="hit-date">{date}</span>
							{/if}
						</p>
						<!-- Who wrote: legitimate here and only here (§9.3). -->
						<p class="small" data-testid="hit-correspondent">{row.correspondent}</p>
						{#if row.subject !== ''}
							<p class="small muted" data-testid="hit-snippet">{row.snippet}</p>
						{/if}
						<p class="hit__meta small muted">
							<span data-testid="hit-source">{$t('search.rowSource')}: {row.source}</span>
							{#if row.mailbox !== null && row.mailbox !== ''}
								<span data-testid="hit-mailbox">{$t('search.rowMailbox')}: {row.mailbox}</span>
							{/if}
						</p>
						{#if href !== null}
							<!-- The trace, handed to the mail client — Twalk is not one. -->
							<a class="small" href={href} data-testid="hit-open">
								<Icon name="external-link" size="dense" />
								{$t('search.open')}
							</a>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
	{:else}
		<p class="muted" data-testid="search-hint">{$t('search.emptyQuery')}</p>
	{/if}
</section>

<style>
	.head {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		margin-bottom: var(--space-3);
	}

	.hits {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}

	.hit {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.hit__head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--space-2);
	}

	.hit__subject {
		font-weight: var(--font-weight-label);
		overflow-wrap: anywhere;
	}

	.hit__meta {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-3);
	}

	/* A search field belongs with its button, and neither should stretch to
	   the whole viewport on a desktop. */
	.input-row {
		display: flex;
	}
</style>
