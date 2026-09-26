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

	The disclosure card (#121, ADR 0019, ADR 0031) is the one control over the
	sentence every reply a persona drafted goes out with — "Rédigé avec mon
	assistant IA." — and it is global and recorded, which the card says before
	the switch is pressed: off is for every reply and never for one, and turning
	it off writes a dated, attributed row in the Gateway's own journal, read back
	here as "off since <date> by <actor>". The sentence the card shows is an
	example in the interface's language; the contact reads it in the language
	of the reply, and the note under it says so, because a French user who saw
	the French sentence and assumed an English contact reads French would be
	misled by the screen.

	Tracing is not here. #101 asks for the OTLP endpoint and the content switch,
	and the Gateway has no settings surface for either until #99 lands; a
	control this screen could not store would be a promise, so a card says
	where it stands instead.
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import Icon from '$lib/icons/Icon.svelte';
	import { locale, setLocale, t, LOCALES, type Locale } from '$lib/i18n';
	import {
		forgetModel,
		loadCalendarLocation,
		loadDisclosure,
		loadLanguage,
		loadModel,
		loadWorkingDay,
		probeModel,
		saveCalendarLocation,
		saveDisclosure,
		saveLanguage,
		saveWorkingDay,
		saveModel,
		type Refused
	} from '$lib/settings/api';
	import {
		calendarLocationRecord,
		daysOnTheDefault,
		exceptionDays,
		workingDayBody,
		workingDayForm,
		workingDayRecord,
		workingDayTrouble,
		credentialState,
		disclosureRecord,
		formOf,
		LANGUAGE_NAMES,
		probeFailureCopy,
		refusalCopy,
		requestOf,
		type CalendarLocationState,
		type WorkingDayState,
		type CredentialState,
		type DisclosureState,
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
	let busy = $state<
		| 'save'
		| 'forget'
		| 'probe'
		| 'language'
		| 'disclosure'
		| 'calendar-location'
		| 'working-day'
		| null
	>(null);
	let outcome = $state<ModelOutcome | null>(null);

	let language = $state<Language | null>(null);
	let available = $state<Language[]>([]);
	let languageOutcome = $state<'saved' | { refused: Refused } | null>(null);

	let disclosure = $state<DisclosureState | null>(null);
	let disclosureProblem = $state<Refused | null>(null);
	/** The note the user may leave with a decision; sent with the next press and cleared. */
	let disclosureReason = $state('');
	let disclosureOutcome = $state<'saved' | { refused: Refused } | null>(null);

	// Where a meeting is (#354): the same three pieces of state as the
	// disclosure's, because it is the same kind of decision — one switch,
	// one note, one outcome — with the opposite default.
	let calendarLocation = $state<CalendarLocationState | null>(null);
	let calendarLocationProblem = $state<Refused | null>(null);
	let calendarLocationReason = $state('');
	let calendarLocationOutcome = $state<'saved' | { refused: Refused } | null>(null);

	// The week, as pairs rather than a template: `$t` checks every key against
	// the catalogue's own type, and a key built by interpolation is a key
	// nothing checks (#381).
	const WEEK = [
		[1, 'settings.workingDay.day.1'],
		[2, 'settings.workingDay.day.2'],
		[3, 'settings.workingDay.day.3'],
		[4, 'settings.workingDay.day.4'],
		[5, 'settings.workingDay.day.5'],
		[6, 'settings.workingDay.day.6'],
		[7, 'settings.workingDay.day.7']
	] as const;

	// The owner's working day (#381): what makes a free gap an offer.
	let workingDay = $state<WorkingDayState | null>(null);
	let workingDayProblem = $state<Refused | null>(null);
	let workingDayReason = $state('');
	let workingDayOutcome = $state<'saved' | 'cleared' | { refused: Refused } | null>(null);
	let workingDayDraft = $state(workingDayForm(null));
	/** The day the "hours of its own" control is about to add, or `null`. */
	let dayToAdd = $state<number | null>(null);

	const credential = $derived<CredentialState | null>(
		configuration === null ? null : credentialState(configuration)
	);
	const record = $derived(disclosure === null ? null : disclosureRecord(disclosure, $locale));
	const locationRecord = $derived(
		calendarLocation === null ? null : calendarLocationRecord(calendarLocation, $locale)
	);
	const dayRecord = $derived(workingDay === null ? null : workingDayRecord(workingDay, $locale));
	/** Why the button is disabled, said rather than left to a `422` (#381). */
	const dayTrouble = $derived(workingDayTrouble(workingDayDraft));
	/** The days that run other hours, and the ticked days that do not (#386). */
	const ownHours = $derived(exceptionDays(workingDayDraft));
	const onTheDefault = $derived(daysOnTheDefault(workingDayDraft));
	/** One weekday's name, through the same typed keys the tick boxes use. */
	const dayName = $derived((day: number) => {
		const named = WEEK.find(([weekday]) => weekday === day);
		return named === undefined ? '' : $t(named[1]);
	});
	const problemOf = $derived(
		(field: FormProblem['field']) => formProblems.find((problem) => problem.field === field) ?? null
	);

	onMount(() => {
		void load();
	});

	async function load() {
		const [model, preference, switched, located, worked] = await Promise.all([
			loadModel(),
			loadLanguage(),
			loadDisclosure(),
			loadCalendarLocation(),
			loadWorkingDay()
		]);
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
		if (switched.ok) {
			disclosure = switched.state;
			disclosureProblem = null;
		} else {
			disclosureProblem = switched;
		}
		if (located.ok) {
			calendarLocation = located.state;
			calendarLocationProblem = null;
		} else {
			calendarLocationProblem = located;
		}
		if (worked.ok) {
			workingDay = worked.state;
			workingDayDraft = workingDayForm(worked.state);
			workingDayProblem = null;
		} else {
			workingDayProblem = worked;
		}
		loaded = true;
	}

	/**
	 * One decision about the working day (#381): these days, this wide, from
	 * the next free/busy read on. Never retroactive and never a busy
	 * interval: what the user is doing is a fact, and when they would rather
	 * not be asked is a preference.
	 */
	async function commitWorkingDay() {
		if (dayTrouble !== null) {
			return;
		}
		workingDayOutcome = null;
		busy = 'working-day';
		const answer = await saveWorkingDay(workingDayBody(workingDayDraft), workingDayReason);
		if (answer.ok) {
			workingDay = answer.state;
			workingDayDraft = workingDayForm(answer.state);
			workingDayReason = '';
			workingDayOutcome = 'saved';
		} else {
			workingDayOutcome = { refused: answer };
		}
		busy = null;
	}

	/** Say nothing again: every free gap is offered, as the deployment ships. */
	async function clearWorkingDay() {
		workingDayOutcome = null;
		busy = 'working-day';
		const answer = await saveWorkingDay(null, workingDayReason);
		if (answer.ok) {
			workingDay = answer.state;
			workingDayReason = '';
			workingDayOutcome = 'cleared';
		} else {
			workingDayOutcome = { refused: answer };
		}
		busy = null;
	}

	/** Tick or untick one day of the week. */
	function toggleDay(day: number) {
		const ticked = workingDayDraft.days.includes(day);
		const exceptions = { ...workingDayDraft.exceptions };
		if (ticked) {
			// Its own hours go with it. An exception for a day the user does not
			// accept meetings on is two statements that contradict each other,
			// and the Gateway refuses it by name (#386) — so the form does not
			// hold one either.
			delete exceptions[day];
		}
		workingDayDraft = {
			...workingDayDraft,
			days: ticked
				? workingDayDraft.days.filter((other) => other !== day)
				: [...workingDayDraft.days, day],
			exceptions
		};
	}

	/** Give one day hours of its own, starting from the default (#386). */
	function giveOwnHours(day: number | null) {
		if (day === null || workingDayDraft.exceptions[day] !== undefined) {
			return;
		}
		workingDayDraft = {
			...workingDayDraft,
			exceptions: {
				...workingDayDraft.exceptions,
				// From the default rather than from nothing: the user narrows a
				// day they can already see, and an empty pair of time inputs
				// would be a form that cannot be submitted until both are typed.
				[day]: { startsAt: workingDayDraft.startsAt, endsAt: workingDayDraft.endsAt }
			}
		};
		dayToAdd = null;
	}

	/** And back to the default: "as usual again", not a day removed. */
	function backToDefault(day: number) {
		const exceptions = { ...workingDayDraft.exceptions };
		delete exceptions[day];
		workingDayDraft = { ...workingDayDraft, exceptions };
	}

	/** One exception's own hours, as its two time inputs give them. */
	function setOwnHours(day: number, part: 'startsAt' | 'endsAt', value: string) {
		const hours = workingDayDraft.exceptions[day];
		if (hours === undefined) {
			return;
		}
		workingDayDraft = {
			...workingDayDraft,
			exceptions: { ...workingDayDraft.exceptions, [day]: { ...hours, [part]: value } }
		};
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
			// speaks the language just chosen — the five it offers are the five
			// it ships (#102).
			if (next !== null && (LOCALES as readonly string[]).includes(next)) {
				setLocale(next as Locale);
			}
		} else {
			languageOutcome = { refused: answer };
		}
	}

	/**
	 * The one press that turns the disclosure off, or back on: a decision
	 * appended to the Gateway's journal, never a preference overwritten. The
	 * state drawn afterwards is the Gateway's answer, not the press.
	 */
	async function switchDisclosure() {
		if (disclosure === null) {
			return;
		}
		disclosureOutcome = null;
		busy = 'disclosure';
		const answer = await saveDisclosure(!disclosure.enabled, disclosureReason);
		busy = null;
		if (answer.ok) {
			disclosure = answer.state;
			disclosureReason = '';
			disclosureOutcome = 'saved';
		} else {
			disclosureOutcome = { refused: answer };
		}
	}

	/**
	 * The one press that lets calendar events carry where a meeting is, or
	 * stops them (#354). The disclosure's shape exactly: a decision appended
	 * to a journal, and the state drawn afterwards is the Gateway's answer.
	 */
	async function switchCalendarLocation() {
		if (calendarLocation === null) {
			return;
		}
		calendarLocationOutcome = null;
		busy = 'calendar-location';
		const answer = await saveCalendarLocation(!calendarLocation.enabled, calendarLocationReason);
		busy = null;
		if (answer.ok) {
			calendarLocation = answer.state;
			calendarLocationReason = '';
			calendarLocationOutcome = 'saved';
		} else {
			calendarLocationOutcome = { refused: answer };
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

	<!-- The disclosure (#121): global, on by default, and off only on the record. -->
	<section class="card stack" data-testid="settings-disclosure" aria-busy={!loaded}>
		<h2 class="card__title">
			<Icon name="persona" size="dense" />
			{$t('settings.disclosure.title')}
		</h2>
		<p class="small">{$t('settings.disclosure.intro')}</p>

		<!-- What the contact reads: the contract's sentence, in the interface's
		     language as an example. The contact reads it in the language of the
		     reply, and the note says so. -->
		<p class="small muted">{$t('settings.disclosure.exampleLabel')}</p>
		<blockquote class="sentence" lang={$locale} data-testid="disclosure-example">
			{$t('settings.disclosure.sentence')}
		</blockquote>
		<p class="small muted">{$t('settings.disclosure.exampleNote')}</p>

		{#if disclosureProblem !== null}
			<p class="card card--warning small" role="alert" data-testid="disclosure-problem" data-code={disclosureProblem.code}>
				{refusalText(disclosureProblem)}
			</p>
		{:else if loaded && disclosure !== null && record !== null}
			<div class="switch-row">
				<button
					id="disclosure-switch"
					class="switch"
					type="button"
					role="switch"
					aria-checked={disclosure.enabled}
					aria-labelledby="disclosure-switch-label"
					disabled={busy !== null}
					onclick={switchDisclosure}
					data-testid="disclosure-switch"
				>
					<span class="switch__knob" aria-hidden="true"></span>
				</button>
				<label class="label" id="disclosure-switch-label" for="disclosure-switch">
					{$t('settings.disclosure.switch')}
					<span class="small muted">
						— {disclosure.enabled
							? $t('settings.disclosure.switchOn')
							: $t('settings.disclosure.switchOff')}
					</span>
				</label>
			</div>

			<!-- The record: who decided, and when. The default state is "nobody
			     ever did", said as such rather than dated. -->
			<p
				class="small {record.kind === 'off' ? 'card card--warning' : 'muted'}"
				data-testid="disclosure-record"
				data-enabled={disclosure.enabled ? 'yes' : 'no'}
			>
				{#if record.kind === 'off'}<Icon name="warning" size="dense" />{/if}
				{$t(record.key, record.values)}
			</p>
			{#if disclosure.reason !== null && disclosure.reason !== ''}
				<p class="small muted" data-testid="disclosure-reason-given">
					{$t('settings.disclosure.record.reason', { reason: disclosure.reason })}
				</p>
			{/if}

			<label class="field">
				<span class="small">{$t('settings.disclosure.reason.label')}</span>
				<input
					class="input"
					type="text"
					autocomplete="off"
					maxlength="1024"
					bind:value={disclosureReason}
					disabled={busy !== null}
					data-testid="disclosure-reason"
				/>
				<span class="small muted">{$t('settings.disclosure.reason.help')}</span>
			</label>

			<p class="small muted">{$t('settings.disclosure.notRetroactive')}</p>

			{#if disclosureOutcome === 'saved'}
				<p class="card card--info small" role="status" data-testid="disclosure-outcome" data-kind="saved">
					{$t('settings.disclosure.saved')}
				</p>
			{:else if disclosureOutcome !== null}
				<p class="card card--warning small" role="alert" data-testid="disclosure-outcome" data-kind="refused">
					{refusalText(disclosureOutcome.refused)}
				</p>
			{/if}
		{/if}
	</section>

	<!-- Where a meeting is (#354, #351): off as this ships, and on only on
	     the record. Placed after the disclosure because it is the same kind
	     of decision, and its emphasis is the mirror image — here the state
	     worth noticing is the one that sends something. -->
	<section class="card stack" data-testid="settings-calendar-location" aria-busy={!loaded}>
		<h2 class="card__title">
			<Icon name="calendar" size="dense" />
			{$t('settings.calendarLocation.title')}
		</h2>
		<p class="small">{$t('settings.calendarLocation.intro')}</p>
		<p class="small muted">{$t('settings.calendarLocation.whatLeaves')}</p>
		<p class="small muted">{$t('settings.calendarLocation.whatNeverLeaves')}</p>

		{#if calendarLocationProblem !== null}
			<p
				class="card card--warning small"
				role="alert"
				data-testid="calendar-location-problem"
				data-code={calendarLocationProblem.code}
			>
				{refusalText(calendarLocationProblem)}
			</p>
		{:else if loaded && calendarLocation !== null && locationRecord !== null}
			<div class="switch-row">
				<button
					id="calendar-location-switch"
					class="switch"
					type="button"
					role="switch"
					aria-checked={calendarLocation.enabled}
					aria-labelledby="calendar-location-switch-label"
					disabled={busy !== null}
					onclick={switchCalendarLocation}
					data-testid="calendar-location-switch"
				>
					<span class="switch__knob" aria-hidden="true"></span>
				</button>
				<label class="label" id="calendar-location-switch-label" for="calendar-location-switch">
					{$t('settings.calendarLocation.switch')}
					<span class="small muted">
						— {calendarLocation.enabled
							? $t('settings.calendarLocation.switchOn')
							: $t('settings.calendarLocation.switchOff')}
					</span>
				</label>
			</div>

			<p
				class="small {locationRecord.kind === 'sending' ? 'card card--warning' : 'muted'}"
				data-testid="calendar-location-record"
				data-enabled={calendarLocation.enabled ? 'yes' : 'no'}
			>
				{#if locationRecord.kind === 'sending'}<Icon name="warning" size="dense" />{/if}
				{$t(locationRecord.key, locationRecord.values)}
			</p>
			{#if calendarLocation.reason !== null && calendarLocation.reason !== ''}
				<p class="small muted" data-testid="calendar-location-reason-given">
					{$t('settings.calendarLocation.record.reason', { reason: calendarLocation.reason })}
				</p>
			{/if}

			<label class="field">
				<span class="small">{$t('settings.calendarLocation.reason.label')}</span>
				<input
					class="input"
					type="text"
					autocomplete="off"
					maxlength="1024"
					bind:value={calendarLocationReason}
					disabled={busy !== null}
					data-testid="calendar-location-reason"
				/>
				<span class="small muted">{$t('settings.calendarLocation.reason.help')}</span>
			</label>

			<p class="small muted">{$t('settings.calendarLocation.notRetroactive')}</p>

			{#if calendarLocationOutcome === 'saved'}
				<p
					class="card card--info small"
					role="status"
					data-testid="calendar-location-outcome"
					data-kind="saved"
				>
					{$t('settings.calendarLocation.saved')}
				</p>
			{:else if calendarLocationOutcome !== null}
				<p
					class="card card--warning small"
					role="alert"
					data-testid="calendar-location-outcome"
					data-kind="refused"
				>
					{refusalText(calendarLocationOutcome.refused)}
				</p>
			{/if}
		{/if}
	</section>

	<!-- The owner's working day (#381): what turns a free gap into an offer.
	     Beside the calendar-location card, because both are decisions about
	     the same agenda — that one about what leaves the machine, this one
	     about what the assistant may offer of the user's time. -->
	<section class="card stack" data-testid="settings-working-day" aria-busy={!loaded}>
		<h2 class="card__title">
			<Icon name="calendar" size="dense" />
			{$t('settings.workingDay.title')}
		</h2>
		<p class="small">{$t('settings.workingDay.intro')}</p>
		<p class="small muted">{$t('settings.workingDay.why')}</p>

		{#if workingDayProblem !== null}
			<p
				class="card card--warning small"
				role="alert"
				data-testid="working-day-problem"
				data-code={workingDayProblem.code}
			>
				{refusalText(workingDayProblem)}
			</p>
		{:else if loaded && workingDay !== null && dayRecord !== null}
			<fieldset class="days" data-testid="working-day-days">
				<legend class="small">{$t('settings.workingDay.days')}</legend>
				{#each WEEK as [day, key] (day)}
					<label class="day">
						<input
							type="checkbox"
							checked={workingDayDraft.days.includes(day)}
							disabled={busy !== null}
							onchange={() => toggleDay(day)}
							data-testid="working-day-{day}"
						/>
						<span class="small">{$t(key)}</span>
					</label>
				{/each}
			</fieldset>

			<div class="amplitude">
				<label class="field">
					<span class="small">{$t('settings.workingDay.startsAt')}</span>
					<input
						class="input"
						type="time"
						bind:value={workingDayDraft.startsAt}
						disabled={busy !== null}
						data-testid="working-day-starts-at"
					/>
				</label>
				<label class="field">
					<span class="small">{$t('settings.workingDay.endsAt')}</span>
					<input
						class="input"
						type="time"
						bind:value={workingDayDraft.endsAt}
						disabled={busy !== null}
						data-testid="working-day-ends-at"
					/>
				</label>
			</div>

			<!-- The days that run other hours (#386). Only those are listed: a
			     row per ticked day would be five rows to say one thing, and the
			     line under them says what the rest follow. A day is added by
			     naming it, and "as usual again" is a decision rather than a row
			     deleted — the same reading `days` has, where absence means the
			     default and never "no meetings". -->
			<div class="own-hours" data-testid="working-day-exceptions">
				{#each ownHours as day (day)}
					<div class="own-day" data-testid="working-day-exception-{day}">
						<span class="small day-name">{dayName(day)}</span>
						<label class="field">
							<span class="small">{$t('settings.workingDay.startsAt')}</span>
							<input
								class="input"
								type="time"
								value={workingDayDraft.exceptions[day].startsAt}
								disabled={busy !== null}
								oninput={(event) =>
									setOwnHours(day, 'startsAt', (event.currentTarget as HTMLInputElement).value)}
								data-testid="working-day-exception-{day}-starts-at"
							/>
						</label>
						<label class="field">
							<span class="small">{$t('settings.workingDay.endsAt')}</span>
							<input
								class="input"
								type="time"
								value={workingDayDraft.exceptions[day].endsAt}
								disabled={busy !== null}
								oninput={(event) =>
									setOwnHours(day, 'endsAt', (event.currentTarget as HTMLInputElement).value)}
								data-testid="working-day-exception-{day}-ends-at"
							/>
						</label>
						<button
							class="button button--quiet small"
							type="button"
							disabled={busy !== null}
							onclick={() => backToDefault(day)}
							data-testid="working-day-exception-{day}-clear"
						>
							{$t('settings.workingDay.exceptions.asUsual')}
						</button>
					</div>
				{/each}

				{#if ownHours.length > 0}
					<p class="small muted" data-testid="working-day-exceptions-rest">
						{$t('settings.workingDay.exceptions.theRest', {
							startsAt: workingDayDraft.startsAt,
							endsAt: workingDayDraft.endsAt
						})}
					</p>
				{/if}

				{#if onTheDefault.length > 0}
					<div class="own-add">
						<label class="field">
							<span class="small">{$t('settings.workingDay.exceptions.add')}</span>
							<select
								class="input"
								bind:value={dayToAdd}
								disabled={busy !== null}
								data-testid="working-day-exception-add"
							>
								<option value={null}>{$t('settings.workingDay.exceptions.choose')}</option>
								{#each onTheDefault as day (day)}
									<option value={day}>{dayName(day)}</option>
								{/each}
							</select>
						</label>
						<button
							class="button button--quiet small"
							type="button"
							disabled={busy !== null || dayToAdd === null}
							onclick={() => giveOwnHours(dayToAdd)}
							data-testid="working-day-exception-add-confirm"
						>
							{$t('settings.workingDay.exceptions.give')}
						</button>
					</div>
				{/if}
			</div>

			<!-- Said, rather than left to the Gateway's 422: a button that is
			     disabled without saying why teaches nothing. -->
			{#if dayTrouble !== null}
				<p class="small muted" role="status" data-testid="working-day-trouble" data-kind={dayTrouble}>
					{$t(`settings.workingDay.trouble.${dayTrouble}`)}
				</p>
			{/if}

			<label class="field">
				<span class="small">{$t('settings.workingDay.reason.label')}</span>
				<input
					class="input"
					type="text"
					autocomplete="off"
					maxlength="1024"
					bind:value={workingDayReason}
					disabled={busy !== null}
					data-testid="working-day-reason"
				/>
			</label>

			<div class="row">
				<button
					class="button"
					type="button"
					disabled={busy !== null || dayTrouble !== null}
					onclick={commitWorkingDay}
					data-testid="working-day-save"
				>
					{$t('settings.workingDay.save')}
				</button>
				{#if workingDay.day !== null}
					<button
						class="button button--quiet"
						type="button"
						disabled={busy !== null}
						onclick={clearWorkingDay}
						data-testid="working-day-clear"
					>
						{$t('settings.workingDay.clear')}
					</button>
				{/if}
			</div>

			<p
				class="small muted"
				data-testid="working-day-record"
				data-kind={dayRecord.kind}
			>
				{$t(dayRecord.key, dayRecord.values)}
			</p>
			{#if workingDay.reason !== null && workingDay.reason !== ''}
				<p class="small muted" data-testid="working-day-reason-given">
					{$t('settings.workingDay.record.reason', { reason: workingDay.reason })}
				</p>
			{/if}
			<p class="small muted">{$t('settings.workingDay.notRetroactive')}</p>

			{#if workingDayOutcome === 'saved' || workingDayOutcome === 'cleared'}
				<p
					class="card card--info small"
					role="status"
					data-testid="working-day-outcome"
					data-kind={workingDayOutcome}
				>
					{$t(
						workingDayOutcome === 'saved'
							? 'settings.workingDay.saved'
							: 'settings.workingDay.clearedNotice'
					)}
				</p>
			{:else if workingDayOutcome !== null}
				<p
					class="card card--warning small"
					role="alert"
					data-testid="working-day-outcome"
					data-kind="refused"
				>
					{refusalText(workingDayOutcome.refused)}
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
	/* The week as a row of ticks, wrapping on a phone: seven checkboxes are a
	   week, and a dropdown per day would be four taps for what is one glance
	   (#381). */
	.days {
		border: 0;
		margin: 0;
		padding: 0;
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2) var(--space-3);
	}

	.day {
		display: flex;
		align-items: center;
		gap: var(--space-1);
	}

	/* The days that run other hours (#386): rows under the amplitude, each one
	   a day, its two times and the way back to the default. Quiet, because they
	   are exceptions to the sentence above them and not a second form. */
	.own-hours {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.own-day,
	.own-add {
		display: flex;
		flex-wrap: wrap;
		align-items: flex-end;
		gap: var(--space-3);
	}

	.own-day .day-name {
		min-width: 6rem;
	}

	/* Two times side by side, because they are one amplitude, and stacked
	   when the screen is narrow. */
	.amplitude {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-3);
	}

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

	/* The contract's sentence, shown as the contact will read it. */
	.sentence {
		margin: 0;
		padding: var(--space-2) var(--space-3);
		border-left: 3px solid var(--color-primary);
		background: var(--color-primary-surface);
		border-radius: var(--radius-md);
	}

	.switch-row {
		display: flex;
		align-items: center;
		gap: var(--space-3);
	}

	/* A switch, drawn as one: a track and a knob that sits left for off and
	   right for on, with the colour carrying the same fact. */
	.switch {
		position: relative;
		flex: 0 0 auto;
		width: 48px;
		height: 28px;
		padding: 0;
		border: 1px solid var(--color-border-strong);
		border-radius: var(--radius-pill);
		background: var(--color-gray-400);
		cursor: pointer;
		transition: background 120ms ease;
	}

	.switch[aria-checked='true'] {
		background: var(--color-primary);
		border-color: var(--color-primary-strong);
	}

	.switch:disabled {
		cursor: default;
		opacity: 0.6;
	}

	.switch:focus-visible {
		outline: 2px solid var(--color-focus-ring);
		outline-offset: 2px;
	}

	.switch__knob {
		position: absolute;
		top: 3px;
		left: 3px;
		width: 20px;
		height: 20px;
		border-radius: 50%;
		background: var(--color-surface);
		transition: transform 120ms ease;
	}

	.switch[aria-checked='true'] .switch__knob {
		transform: translateX(20px);
	}
</style>
