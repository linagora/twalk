<!--
	A step with fields to answer: the renderer's central case, and the one that
	had no component at all before ADR 0030.

	# What decides what is drawn

	The **field type**, and nothing else (`step-fields.ts`). Ordinary fields get
	one control each; fields the bridge groups by type — a jar of `cookie` fields
	sharing a domain — are collected through one control and its parser, because
	people arrive at that step holding a whole `Cookie` header or an extension's
	export and not seven values to transcribe. A type this build does not know,
	**or one the bridge did not declare**, refuses that field and names it,
	rather than defaulting to a text input that would put a password on screen.

	# One panel for two kinds

	`input` and `cookies` are one panel because they are one thing: a question
	with a field list. That the field list happens to be all cookies is a fact
	about the types in it, which is the whole reason the renderer is field-driven
	rather than a special case per network.

	# The words

	The bridge's instructions, **replaced** by this project's where honesty needs
	more than a bridge says (#57: which cookies, where to get them, why a private
	window, that Device Bound Session Credentials must be off). An override is
	written against a step's shape, and when that shape drifts the explanation
	has quietly become wrong — so the panel says so and falls back to the
	bridge's own words instead of reading a stale one out.

	# What it keeps

	Nothing. Typed values live in this component's state and nowhere else: no
	`localStorage`, no store, no module scope. The values are cleared before the
	request rather than after, so nothing stays on screen while the network is
	slow, and the component itself is re-created when the step changes — the flow
	keys it on the step id — so no answer can outlive the question.
-->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import type { PanelProps, ViewOf } from '$lib/networks/login-panels';
	import { answerable, answerOf, controlsOf, type Problem } from '$lib/networks/step-fields';
	import { driftOf, overrideFor } from '$lib/networks/step-copy';

	let { view, copy, flow }: PanelProps<ViewOf<'input' | 'cookies'>> = $props();

	const controls = $derived(controlsOf(view.fields));
	const ready = $derived(answerable(controls));
	/** This project's words for this step, or `null` when the bridge's will do. */
	const override = $derived(overrideFor(copy.stepCopy, view.stepId));
	/** How the step stopped matching its explanation, or `null`. */
	const drift = $derived(override === null ? null : driftOf(override, view));
	/** An explanation is used only while it still describes the question. */
	const explained = $derived(drift === null ? override : null);
	const url = $derived(view.kind === 'cookies' ? view.request.url : null);

	let values = $state<Record<string, string>>({});
	let problems = $state<readonly Problem[]>([]);

	const problemOf = $derived(
		(controlId: string): Problem | null =>
			problems.find((problem) => problem.controlId === controlId) ?? null
	);
	const typedSomething = $derived(
		controls.some((control) => (values[control.id] ?? '').trim() !== '')
	);

	async function answer(event: SubmitEvent) {
		event.preventDefault();
		const built = answerOf(controls, values);
		if (!built.ok) {
			problems = built.problems;
			return;
		}
		problems = [];
		const stepId = view.stepId;
		// Cleared before the request, not after: nothing is left on screen while
		// the network is slow, and the built answer dies with this call.
		values = {};
		await flow.submit(stepId, built.data);
	}
</script>

<section class="stack" data-testid="login-step" data-step-id={view.stepId}>
	{#if explained !== null && explained.instructions !== null}
		<p>{$t(explained.instructions)}</p>
	{:else if view.instructions !== null && view.instructions !== ''}
		<!-- The bridge's own words: the network's wording for its own screen. -->
		<p>{view.instructions}</p>
	{/if}

	{#if drift !== null}
		<!-- Loud, because an explanation that no longer matches the question is
		     worse than none: it tells the user confidently about a step that is
		     not the one in front of them. -->
		<div class="card card--warning" data-testid="step-copy-drifted" role="alert">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('networks.step.driftTitle')}
			</p>
			<p>{$t('networks.step.driftBody', { expected: drift.expected, found: drift.found })}</p>
			<p class="small muted mono">{view.stepId}</p>
		</div>
	{:else if override === null && view.kind === 'cookies'}
		<!-- A credential handover with only the bridge's eleven words to go on.
		     Said out loud, because a step id this project guessed wrong would
		     otherwise silently drop four acceptance criteria (#57). -->
		<div class="card card--warning" data-testid="step-copy-missing">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('networks.step.noExplanationTitle')}
			</p>
			<p>{$t('networks.step.noExplanationBody')}</p>
			<p class="small muted mono">{view.stepId}</p>
		</div>
	{/if}

	{#if explained !== null}
		{#each explained.before as block, index (index)}
			<section class={block.warn ? 'card card--warning stack' : 'stack'}>
				{#if block.title !== null}
					{#if block.warn}
						<p class="card__title">
							<Icon name="cookie" size="dense" />
							{$t(block.title)}
						</p>
					{:else}
						<h2>{$t(block.title)}</h2>
					{/if}
				{/if}
				{#if block.body !== null}
					<p>{$t(block.body)}</p>
				{/if}
				{#if block.bullets.length > 0}
					<ul class="bullets">
						{#each block.bullets as bullet (bullet)}
							<li>{$t(bullet)}</li>
						{/each}
					</ul>
				{/if}
				{#if block.steps.length > 0}
					<ol class="steps">
						{#each block.steps as step, at (step)}
							<li>
								{$t(step)}
								{#if block.link && at === 0 && url !== null}
									<a href={url} target="_blank" rel="noreferrer noopener">
										{url}
										<Icon name="external-link" size="dense" />
									</a>
								{/if}
							</li>
						{/each}
					</ol>
				{/if}
			</section>
		{/each}
	{/if}

	{#if !ready}
		<!-- Not submittable, on purpose. Answering part of a step does not get a
		     correction: the network ends the login when it refuses an answer, and
		     the user pays for it with a fresh code. -->
		<div class="card card--warning stack" data-testid="field-refused" role="alert">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('networks.field.blockedTitle')}
			</p>
			<ul class="bullets">
				{#each controls as control (control.id)}
					{#if control.kind === 'refused'}
						<li data-testid="refused-field" data-field-id={control.field.id}>
							{#if control.because === 'no_type'}
								{$t('networks.field.refusedNoType', { field: control.field.name })}
							{:else if control.because === 'no_options'}
								{$t('networks.field.refusedNoOptions', { field: control.field.name })}
							{:else}
								{$t('networks.field.refusedUnknownType', {
									field: control.field.name,
									type: control.field.type ?? ''
								})}
							{/if}
						</li>
					{/if}
				{/each}
			</ul>
			<p>{$t('networks.field.blockedBody')}</p>
		</div>
	{:else}
		<form class="stack" onsubmit={answer}>
			{#each controls as control (control.id)}
				{#if control.kind === 'jar'}
					<!-- One control for the whole group, because a bridge that groups
					     these by type is saying they are fetched from one browser
					     session in one sitting — and because the answer it wants is
					     one map, whatever each field's source was. -->
					<div class="field">
						<p class="small">
							{control.allCookies
								? $t('networks.cookies.wanted', { count: control.names.length })
								: $t('networks.jar.wanted', { count: control.names.length })}
						</p>
						<ul class="names" data-testid="cookie-names">
							{#each control.names as name (name)}
								<li class="mono">{name}</li>
							{/each}
						</ul>
						{#if control.domain !== null}
							<p class="small muted">{$t('networks.cookies.domain', { domain: control.domain })}</p>
						{/if}
						<label class="label" for={control.id}>
							{control.allCookies ? $t('networks.cookies.label') : $t('networks.jar.label')}
						</label>
						<textarea
							id={control.id}
							class="input paste"
							rows="5"
							spellcheck="false"
							autocapitalize="none"
							autocomplete="off"
							placeholder={control.allCookies
								? 'SID=…; HSID=…; SSID=…'
								: '{"Cookie": "…", "X-Example-Token": "…"}'}
							bind:value={values[control.id]}
							data-testid="cookie-paste"
						></textarea>
						{@render problem(control.id)}
					</div>
				{:else if control.kind === 'entry' && control.control === 'select'}
					<div class="field">
						<label class="label" for={control.id}>{control.field.name}</label>
						{#if control.field.description !== null}
							<p class="small muted">{control.field.description}</p>
						{/if}
						<select
							id={control.id}
							class="input"
							bind:value={values[control.id]}
							data-testid={`field-${control.field.id}`}
							data-field-type={control.type}
						>
							<!-- No option selected to begin with: a list that answers
							     itself is a value the user did not choose. -->
							<option value="" disabled selected>{$t('networks.field.choose')}</option>
							{#each control.field.options as option (option)}
								<option value={option}>{option}</option>
							{/each}
						</select>
						{@render problem(control.id)}
					</div>
				{:else if control.kind === 'entry'}
					<div class="field">
						<label class="label" for={control.id}>{control.field.name}</label>
						{#if control.field.description !== null}
							<p class="small muted">{control.field.description}</p>
						{/if}
						<input
							id={control.id}
							class="input"
							type={control.control}
							inputmode={control.control === 'tel' ? 'tel' : undefined}
							autocomplete={control.autocomplete}
							autocapitalize={control.secret ? 'none' : undefined}
							spellcheck="false"
							bind:value={values[control.id]}
							data-testid={`field-${control.field.id}`}
							data-field-type={control.type}
						/>
						{@render problem(control.id)}
					</div>
				{/if}
			{/each}

			<button
				class="button button--primary"
				type="submit"
				disabled={!typedSomething}
				data-testid="submit-step"
			>
				{$t(explained?.submit ?? 'networks.step.submit')}
			</button>
			{#each controls as control (control.id)}
				{#if control.kind === 'refused'}
					<!-- Answerable in spite of it: the bridge called this one
					     optional, so the login can go on without it — said out loud,
					     because a field that vanished silently is a field the user
					     will look for. -->
					<p class="small muted" data-testid="field-left-out" data-field-id={control.field.id}>
						{$t('networks.field.leftOut', { field: control.field.name })}
					</p>
				{/if}
			{/each}
			{#if explained !== null}
				{#each explained.after as line (line)}
					<p class="small muted">{$t(line)}</p>
				{/each}
			{:else}
				<p class="small muted">{$t('networks.step.neverStored')}</p>
			{/if}
		</form>
	{/if}

	<p>
		<button class="button button--secondary" type="button" onclick={flow.cancel} data-testid="cancel-login">
			{$t('networks.cancel')}
		</button>
	</p>
</section>

{#snippet problem(controlId: string)}
	{@const found = problemOf(controlId)}
	<p class="error-text" role="alert" aria-live="polite">
		{#if found !== null}
			{#if found.because === 'empty'}
				{$t('networks.field.empty')}
			{:else if found.because === 'pattern'}
				{$t('networks.field.pattern')}
			{:else if found.because === 'unreadable'}
				{$t('networks.cookies.unreadable')}
			{:else if
				override !== null &&
				override.hostScopedMissing !== null &&
				found.names.some((name) => override.hostScoped.includes(name))
			}
				<!-- A cookie the API host never carries: the paste was complete
				     and the cookie was never in it, so the cause is the request
				     copied, not the copying (#220). -->
				<span data-testid="cookies-missing-scoped">
					{$t(override.hostScopedMissing, {
						names: found.names.join(', '),
						scoped: found.names.filter((name) => override.hostScoped.includes(name)).join(', ')
					})}
				</span>
			{:else}
				{$t('networks.cookies.missing', { names: found.names.join(', ') })}
			{/if}
		{/if}
	</p>
{/snippet}

<style>
	h2 {
		font-size: var(--text-lg);
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
</style>
