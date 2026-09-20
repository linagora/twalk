<!--
	Settings (ticket #101): the model, and the user's language.

	This is where a non-technical user meets the sovereignty decisions in a
	row, so the copy carries the screen: each control says what leaves the
	machine when it is used, and each answer the Gateway can give has a
	sentence and a next step. The rules are in `$lib/settings/model.ts`, where
	they are tested; this file renders them.

	Three things the model card must not get wrong, from #98's hand-over:

	  - the credential is **write-only**: the Gateway describes it and never
	    returns it, so the field is empty on every load and "leave it empty"
	    means "keep what is stored". Forgetting it is a separate, named act;
	  - a credential the operator supplied as a **file** wins over anything set
	    here (ADR 0015), so when the Gateway says a file is in force the field is
	    not drawn — a control that looked like it worked and did nothing would
	    be the defect this project keeps closing;
	  - the **probe** spends the operator's money: one real one-token completion
	    to the configured endpoint, the only call in Twalk that bills without a
	    message arriving first. The button says so before it is pressed.

	The language card has two effects and says both: it changes this interface,
	and it is the language an assistant falls back to when it cannot tell what
	language a message was written in (ADR 0016). It does not choose the language
	a suggestion is written in, and the screen says that too — a French user
	answering an English contact in French has been handed something they
	cannot send.

	Tracing is not here. #101 asks for the OTLP endpoint and the content switch,
	and the Gateway has no settings surface for either until #99 lands; a
	control this screen could not store would be a promise, so a card says
	where it stands instead.
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import Icon from '$lib/icons/Icon.svelte';
	import { setLocale, t, LOCALES, type Locale } from '$lib/i18n';
	import {
		forgetModel,
		loadLanguage,
		loadModel,
		probeModel,
		saveLanguage,
		saveModel,
		type Refused
	} from '$lib/settings/api';
	import {
		credentialState,
		formOf,
		LANGUAGE_NAMES,
		probeFailureCopy,
		refusalCopy,
		requestOf,
		type CredentialState,
		type FormProblem,
		type Language,
		type ModelConfiguration,
		type ModelForm,
		type Probe
	} from '$lib/settings/model';

	/** What became of the last act on the model card. */
	type ModelOutcome =
		| { kind: 'saved' }
		| { kind: 'forgotten' }
		| { kind: 'probed'; probe: Probe }
		| { kind: 'refused'; refused: Refused };

	let loaded = $state(false);
	let modelProblem = $state<Refused | null>(null);
	let languageProblem = $state<Refused | null>(null);
	let configuration = $state<ModelConfiguration | null>(null);
	let form = $state<ModelForm>({ baseUrl: '', model: '', credential: '', params: '' });
	let advanced = $state(false);
	let formProblems = $state<FormProblem[]>([]);
	let busy = $state<'save' | 'forget' | 'probe' | 'language' | null>(null);
	let outcome = $state<ModelOutcome | null>(null);

	let language = $state<Language | null>(null);
	let available = $state<Language[]>([]);
	let languageOutcome = $state<'saved' | { refused: Refused } | null>(null);

	const credential = $derived<CredentialState | null>(
		configuration === null ? null : credentialState(configuration)
	);
	const problemOf = $derived(
		(field: FormProblem['field']) => formProblems.find((problem) => problem.field === field) ?? null
	);

	onMount(() => {
		void load();
	});

	async function load() {
		const [model, preference] = await Promise.all([loadModel(), loadLanguage()]);
		if (model.ok) {
			configuration = model.configuration;
			form = formOf(model.configuration);
			advanced = form.params !== '';
			modelProblem = null;
		} else {
			modelProblem = model;
		}
		if (preference.ok) {
			language = preference.preference.language;
			available = preference.preference.available;
			languageProblem = null;
		} else {
			languageProblem = preference;
		}
		loaded = true;
	}

	async function save(event: SubmitEvent) {
		event.preventDefault();
		outcome = null;
		const built = requestOf(form);
		if (!built.ok) {
			formProblems = built.problems;
			return;
		}
		formProblems = [];
		busy = 'save';
		const answer = await saveModel(built.request);
		busy = null;
		if (answer.ok) {
			configuration = answer.configuration;
			// The credential just typed is stored and never echoed back: the
			// field is cleared here, and the description below says it is set.
			form = formOf(answer.configuration);
			outcome = { kind: 'saved' };
		} else {
			outcome = { kind: 'refused', refused: answer };
		}
	}

	/** Forgets the model and the credential set here — a named act, not an empty field. */
	async function forget() {
		outcome = null;
		busy = 'forget';
		const answer = await forgetModel();
		busy = null;
		if (answer.ok) {
			const reloaded = await loadModel();
			if (reloaded.ok) {
				configuration = reloaded.configuration;
				form = formOf(reloaded.configuration);
			}
			outcome = { kind: 'forgotten' };
		} else {
			outcome = { kind: 'refused', refused: answer };
		}
	}

	async function probe() {
		outcome = null;
		busy = 'probe';
		const answer = await probeModel();
		busy = null;
		outcome = answer.ok ? { kind: 'probed', probe: answer.probe } : { kind: 'refused', refused: answer };
	}

	async function chooseLanguage(next: Language | null) {
		languageOutcome = null;
		busy = 'language';
		const answer = await saveLanguage(next);
		busy = null;
		if (answer.ok) {
			language = answer.preference.language;
			languageOutcome = 'saved';
			// The first of the two effects, applied at once: this interface
			// speaks the language just chosen, when it ships in it.
			if (next !== null && (LOCALES as readonly string[]).includes(next)) {
				setLocale(next as Locale);
			}
		} else {
			languageOutcome = { refused: answer };
		}
	}

	/** The sentence for a refusal: the table's, or the Gateway's own words. */
	function refusalText(refused: Refused, probe = false): string {
		if (refused.trouble === 'unreachable') {
			return $t('api.trouble.unreachable');
		}
		if (refused.trouble === 'session-refused') {
			return $t('api.trouble.sessionRefused');
		}
		const key = (probe ? probeFailureCopy(refused.code) : null) ?? refusalCopy(refused.code);
		if (key !== null) {
			return $t(key, { status: refused.endpointStatus ?? '', detail: refused.detail ?? '' });
		}
		return refused.detail ?? $t('api.trouble.refused');
	}
</script>

<section class="screen" data-testid="screen-settings">
	<header class="stack">
		<p class="small">
			<a href="/dashboard">
				<Icon name="back" size="dense" />
				{$t('settings.back')}
			</a>
		</p>
		<h1>{$t('settings.title')}</h1>
		<p class="subtitle">{$t('settings.subtitle')}</p>
	</header>

	<!-- The model -->
	<section class="card stack" data-testid="settings-model" aria-busy={!loaded}>
		<h2 class="card__title">
			<Icon name="persona" size="dense" />
			{$t('settings.model.title')}
		</h2>
		<p class="small">{$t('settings.model.intro')}</p>

		{#if modelProblem !== null}
			<p class="card card--warning small" role="alert" data-testid="model-problem" data-code={modelProblem.code}>
				{refusalText(modelProblem)}
			</p>
		{:else if loaded && configuration !== null}
			{#if !configuration.configured}
				<p class="card card--warning small" data-testid="model-unset">
					{$t('settings.model.unset')}
				</p>
			{/if}

			<form class="stack" onsubmit={save} data-testid="model-form">
				<label class="field">
					<span class="label">{$t('settings.model.baseUrl.label')}</span>
					<input
						class="input"
						type="url"
						inputmode="url"
						autocomplete="off"
						spellcheck="false"
						placeholder="http://127.0.0.1:4000/v1"
						bind:value={form.baseUrl}
						data-testid="model-base-url"
					/>
					<span class="small muted">{$t('settings.model.baseUrl.help')}</span>
					{#if problemOf('baseUrl') !== null}
						<span class="error-text" role="alert" data-testid="model-base-url-problem">
							{problemOf('baseUrl')?.because === 'empty'
								? $t('settings.model.baseUrl.empty')
								: $t('settings.model.baseUrl.notAUrl')}
						</span>
					{/if}
				</label>

				<label class="field">
					<span class="label">{$t('settings.model.name.label')}</span>
					<input
						class="input"
						type="text"
						autocomplete="off"
						spellcheck="false"
						placeholder="qwen"
						bind:value={form.model}
						data-testid="model-name"
					/>
					<span class="small muted">{$t('settings.model.name.help')}</span>
					{#if problemOf('model') !== null}
						<span class="error-text" role="alert" data-testid="model-name-problem">
							{$t('settings.model.name.empty')}
						</span>
					{/if}
				</label>

				<!-- The credential: three states, one field at most (#98). -->
				<div class="field" data-testid="model-credential" data-source={credential?.kind ?? 'none'}>
					<span class="label">{$t('settings.model.credential.label')}</span>
					{#if credential?.kind === 'file'}
						<p class="card small" data-testid="model-credential-file">
							<Icon name="ok" size="dense" />
							{$t('settings.model.credential.file', { path: credential.path })}
							{#if credential.companionStored}
								{$t('settings.model.credential.fileOverrides')}
							{/if}
						</p>
					{:else}
						{#if credential?.kind === 'companion'}
							<p class="small" data-testid="model-credential-set">
								{$t('settings.model.credential.set', { hint: credential.hint ?? '…' })}
							</p>
						{/if}
						<input
							class="input"
							type="password"
							autocomplete="off"
							spellcheck="false"
							bind:value={form.credential}
							data-testid="model-credential-input"
						/>
						<span class="small muted">
							{credential?.kind === 'companion'
								? $t('settings.model.credential.keep')
								: $t('settings.model.credential.help')}
						</span>
					{/if}
				</div>

				<details class="stack" bind:open={advanced} data-testid="model-advanced">
					<summary class="small">{$t('settings.model.params.summary')}</summary>
					<label class="field">
						<span class="label">{$t('settings.model.params.label')}</span>
						<textarea
							class="input mono"
							rows="4"
							spellcheck="false"
							placeholder={'{"temperature": 0.2}'}
							bind:value={form.params}
							data-testid="model-params"
						></textarea>
						<!-- What the provider rejects shows up at the first call, not
						     here: this field is not a validation the screen performs. -->
						<span class="small muted">{$t('settings.model.params.help')}</span>
						{#if problemOf('params') !== null}
							<span class="error-text" role="alert" data-testid="model-params-problem">
								{$t('settings.model.params.notAnObject')}
							</span>
						{/if}
					</label>
				</details>

				<p class="actions">
					<button class="button button--primary" type="submit" disabled={busy !== null} data-testid="model-save">
						{busy === 'save' ? $t('settings.saving') : $t('settings.model.save')}
					</button>
					<button
						class="button button--secondary"
						type="button"
						onclick={probe}
						disabled={busy !== null || !configuration.configured}
						data-testid="model-probe"
					>
						{busy === 'probe' ? $t('settings.model.probing') : $t('settings.model.probe')}
					</button>
					{#if configuration.configured}
						<button
							class="button button--secondary"
							type="button"
							onclick={forget}
							disabled={busy !== null}
							data-testid="model-forget"
						>
							{$t('settings.model.forget')}
						</button>
					{/if}
				</p>
				<p class="small muted">{$t('settings.model.probe.cost')}</p>
			</form>

			{#if outcome !== null}
				{#if outcome.kind === 'saved'}
					<p class="card card--info small" role="status" data-testid="model-outcome" data-kind="saved">
						{$t('settings.model.saved')}
					</p>
				{:else if outcome.kind === 'forgotten'}
					<p class="card card--info small" role="status" data-testid="model-outcome" data-kind="forgotten">
						{$t('settings.model.forgotten')}
					</p>
				{:else if outcome.kind === 'probed'}
					<p
						class="card card--info small"
						role="status"
						data-testid="model-outcome"
						data-kind="probed"
						data-endpoint-model={outcome.probe.endpoint_model ?? ''}
					>
						{$t('settings.probe.ok', {
							status: outcome.probe.endpoint_status,
							model: outcome.probe.endpoint_model ?? outcome.probe.model
						})}
					</p>
				{:else}
					<p
						class="card card--warning small"
						role="alert"
						data-testid="model-outcome"
						data-kind="refused"
						data-code={outcome.refused.code ?? ''}
					>
						{refusalText(outcome.refused, true)}
					</p>
				{/if}
			{/if}
		{/if}
	</section>

	<!-- The language -->
	<section class="card stack" data-testid="settings-language" aria-busy={!loaded}>
		<h2 class="card__title">
			<Icon name="info" size="dense" />
			{$t('settings.language.title')}
		</h2>
		<p class="small">{$t('settings.language.effect')}</p>
		<p class="small muted">{$t('settings.language.notTheReply')}</p>

		{#if languageProblem !== null}
			<p class="card card--warning small" role="alert" data-testid="language-problem" data-code={languageProblem.code}>
				{refusalText(languageProblem)}
			</p>
		{:else if loaded}
			<fieldset class="stack" data-testid="language-choice" data-language={language ?? 'none'}>
				<legend class="label">{$t('settings.language.label')}</legend>
				{#each available as option (option)}
					<label class="choice">
						<input
							type="radio"
							name="language"
							value={option}
							checked={language === option}
							disabled={busy !== null}
							onchange={() => chooseLanguage(option)}
							data-testid={`language-${option}`}
						/>
						<span lang={option}>{LANGUAGE_NAMES[option]}</span>
					</label>
				{/each}
				<label class="choice">
					<input
						type="radio"
						name="language"
						value=""
						checked={language === null}
						disabled={busy !== null}
						onchange={() => chooseLanguage(null)}
						data-testid="language-none"
					/>
					<span>{$t('settings.language.none')}</span>
				</label>
			</fieldset>
			{#if languageOutcome === 'saved'}
				<p class="card card--info small" role="status" data-testid="language-outcome" data-kind="saved">
					{$t('settings.language.saved')}
				</p>
			{:else if languageOutcome !== null}
				<p class="card card--warning small" role="alert" data-testid="language-outcome" data-kind="refused">
					{refusalText(languageOutcome.refused)}
				</p>
			{/if}
		{/if}
	</section>

	<!-- Tracing: decided (ADR 0017), and not yet storable (#99). -->
	<section class="card stack" data-testid="settings-tracing">
		<h2 class="card__title">
			<Icon name="info" size="dense" />
			{$t('settings.tracing.title')}
		</h2>
		<p class="small">{$t('settings.tracing.pending')}</p>
		<p class="small muted">{$t('settings.tracing.contentWarning')}</p>
	</section>
</section>

<style>
	h2 {
		font-size: var(--text-lg);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.choice {
		display: flex;
		align-items: center;
		gap: var(--space-2);
	}

	.mono {
		font-family: var(--font-mono);
	}
</style>
