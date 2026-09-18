<!--
	Screen 4 of `docs/wireframes/companion-v0.1.md`: activating the `assistant`.

	The screen as the design review left it (#74), and the differences from the
	wireframe are on the screen itself rather than only in a commit message —
	a user who was shown the old design deserves to know what went and why:

	  - **no auto-send toggle.** The one feature that would let an agent send
	    without human approval, two lines under this card's own promise that it
	    never does.
	  - **no active hours.** A time window is a consent rule enforced and
	    audited at runtime (v1.0's consent policies); a cosmetic control here
	    would suggest a protection that does not exist.
	  - **consent default: `pending` for everyone**, with no address-book rule.
	    v0.1 imports no address book, and adding one is a personal-data path of
	    its own.

	What the button does is one thing: `POST /api/consent/decisions` with a
	`persona` subject, scoped to the networks ticked here (ADR 0013). There is
	no persona control API, and this screen deliberately offers nothing that
	could not be written as that one decision — see `$lib/personas/catalogue.ts`
	for why the two remaining rows are locked.

	It also says plainly that no agent runtime is deployed yet. Activating the
	assistant today records and publishes the decision and produces no
	suggestions, because Hermes is not implemented (#21–#25). A screen that
	animated a waking assistant over an empty deployment would be the kind of
	hopeful story this product cannot afford to tell.
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import { gateway } from '$lib/api/client';
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import { cardFor } from '$lib/networks/catalogue';
	import { PERSONA_CARDS, ASSISTANT, type PersonaCard } from '$lib/personas/catalogue';
	import { activatePersona, type ActivationFailure } from '$lib/personas/activation';
	import {
		defaultSelection,
		scopeOptions,
		MATRIX_NETWORK,
		type BridgeRow,
		type ScopeOption
	} from '$lib/personas/scope';

	type Stage = 'choosing' | 'working' | 'done';

	const card = PERSONA_CARDS.find((entry) => entry.id === ASSISTANT) as PersonaCard;

	let options = $state<ScopeOption[]>([]);
	let selected = $state<string[]>([]);
	let loaded = $state(false);
	let stage = $state<Stage>('choosing');
	let failure = $state<ActivationFailure | null>(null);
	let activatedOn = $state<string[]>([]);

	onMount(async () => {
		const listed = await gateway.GET('/api/bridges');
		const bridges = (listed.data?.bridges ?? []) as BridgeRow[];
		options = scopeOptions(bridges);
		selected = defaultSelection(options);
		loaded = true;
	});

	const anyProven = $derived(options.some((option) => option.proven));

	function toggle(network: string) {
		selected = selected.includes(network)
			? selected.filter((entry) => entry !== network)
			: [...selected, network];
	}

	function label(network: string): string {
		const key = cardFor(network)?.titleKey;
		return key === undefined ? network : $t(key);
	}

	async function activate() {
		if (stage === 'working') {
			return;
		}
		failure = null;
		stage = 'working';
		const result = await activatePersona(card.id, selected);
		if (result.ok) {
			activatedOn = result.decision.scope.networks;
			stage = 'done';
			return;
		}
		failure = result.failure;
		stage = 'choosing';
	}

	const failureText = $derived(textFor(failure));

	function textFor(reason: ActivationFailure | null): string | null {
		switch (reason?.kind) {
			case 'no-networks':
				return $t('persona.error.noNetworks');
			case 'unknown-network':
				return $t('persona.error.refused', { error: reason.network });
			case 'refused':
				return $t('persona.error.refused', { error: reason.error });
			case 'unreachable':
				return $t('persona.error.unreachable');
			default:
				return null;
		}
	}
</script>

<section class="screen" data-testid="screen-personas">
	<header class="stack">
		<p class="small">
			<a href="/networks">
				<Icon name="back" size="dense" />
				{$t('persona.back')}
			</a>
		</p>
		<h1>{$t('persona.title')}</h1>
		<p class="subtitle">{$t('persona.step')}</p>
	</header>

	{#if stage === 'done'}
		<div class="card card--info" data-testid="activated">
			<p class="card__title">
				<Icon name="ok" size="dense" />
				{$t('persona.activated.title', {
					networks: activatedOn.map(label).join(', ')
				})}
			</p>
			<p>{$t('persona.activated.body')}</p>
			<p class="small muted" data-testid="activated-networks" data-networks={activatedOn.join(',')}>
				{$t('persona.runtime.body')}
			</p>
			<p>
				<a class="button button--primary" href="/dashboard" data-testid="to-dashboard">
					{$t('persona.activated.continue')}
					<Icon name="continue" size="dense" />
				</a>
			</p>
		</div>
	{:else}
		<div class="card" data-testid="persona-card">
			<p class="card__title">
				<Icon name={card.icon} />
				{$t(card.nameKey)}
			</p>
			<p>{$t(card.descriptionKey)}</p>

			<ul class="abilities">
				{#each card.abilities as ability (ability.id)}
					<li class="ability" data-testid={`ability-${ability.id}`} data-on="yes" data-locked="yes">
						<span class="switch switch--on" aria-hidden="true"></span>
						<span class="ability__text">
							<span class="ability__label">{$t(ability.labelKey)}</span>
							<span class="small muted">{$t(ability.detailKey)}</span>
							<span class="small locked">
								<Icon name="locked" size="dense" />
								{$t(ability.lockedKey)}
							</span>
						</span>
						<!-- A real control, so a screen reader announces the state, and
						     disabled, because v0.1 records no decision that would turn
						     it off. -->
						<input
							type="checkbox"
							class="visually-hidden"
							checked
							disabled
							aria-label={$t(ability.labelKey)}
						/>
					</li>
				{/each}
			</ul>
		</div>

		<section class="card" data-testid="scope">
			<p class="card__title">
				<Icon name="bridge" size="dense" />
				{$t('persona.scope.title')}
			</p>
			<p class="small muted">{$t('persona.scope.intro')}</p>

			{#if loaded && !anyProven}
				<p class="small" data-testid="no-network">{$t('persona.scope.none')}</p>
				<p>
					<a class="button button--secondary" href="/networks">
						{$t('persona.scope.connectFirst')}
					</a>
				</p>
			{/if}

			<ul class="scope" aria-busy={!loaded}>
				{#each options as option (option.network)}
					<li>
						<label class="scope__row" data-testid={`scope-${option.network}`}>
							<input
								type="checkbox"
								checked={selected.includes(option.network)}
								onchange={() => toggle(option.network)}
							/>
							<span class="scope__text">
								<span>{label(option.network)}</span>
								{#if option.proven}
									<span class="badge badge--ok">
										<Icon name="check" size="dense" />
										{$t('persona.scope.proven')}
									</span>
								{/if}
								{#if option.network === MATRIX_NETWORK && !option.proven}
									<span class="small muted">{$t('persona.scope.matrixHint')}</span>
								{/if}
							</span>
						</label>
					</li>
				{/each}
			</ul>
		</section>

		<div class="card card--info" data-testid="consent-default">
			<p class="card__title">
				<Icon name="consent" size="dense" />
				{$t('persona.consent.title')}
			</p>
			<p class="small">{$t('persona.consent.body')}</p>
		</div>

		<div class="card" data-testid="removed-settings">
			<p class="card__title">
				<Icon name="info" size="dense" />
				{$t('persona.removed.title')}
			</p>
			<ul class="reasons">
				<li class="small">{$t('persona.removed.autoSend')}</li>
				<li class="small">{$t('persona.removed.hours')}</li>
			</ul>
		</div>

		<div class="card card--warning" data-testid="no-runtime">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('persona.runtime.title')}
			</p>
			<p class="small">{$t('persona.runtime.body')}</p>
			<p class="small">{$t('persona.pause.meaning')}</p>
		</div>

		{#if failureText !== null}
			<p class="error-text" role="alert" data-testid="activation-error">{failureText}</p>
		{/if}

		<button
			class="button button--primary"
			type="button"
			onclick={activate}
			disabled={stage === 'working' || selected.length === 0}
			data-testid="activate"
		>
			{#if stage === 'working'}
				<span class="spinner" aria-hidden="true"></span>
				{$t('persona.activating')}
			{:else}
				{$t('persona.activate')}
				<Icon name="continue" size="dense" />
			{/if}
		</button>

		<p class="small">
			<a class="muted" href="/dashboard" data-testid="skip-persona">{$t('persona.skip')}</a>
		</p>
	{/if}
</section>

<style>
	.abilities,
	.scope,
	.reasons {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}

	.reasons {
		gap: var(--space-2);
	}

	.ability {
		display: flex;
		align-items: flex-start;
		gap: var(--space-3);
	}

	.ability__text,
	.scope__text {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 0;
	}

	.ability__label {
		font-weight: var(--font-weight-label);
	}

	.locked {
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
		color: var(--color-text-subtle);
	}

	/* The wireframe's toggle, drawn rather than interactive: these two rows
	   cannot be switched off, and a control that looked switchable would be
	   the cosmetic control the design review objected to. */
	.switch {
		flex: 0 0 auto;
		width: 36px;
		height: 20px;
		border-radius: var(--radius-pill);
		background: var(--color-border);
		position: relative;
		margin-top: 2px;
	}

	.switch::after {
		content: '';
		position: absolute;
		top: 2px;
		left: 2px;
		width: 16px;
		height: 16px;
		border-radius: 50%;
		background: var(--color-surface);
	}

	.switch--on {
		background: var(--color-primary);
	}

	.switch--on::after {
		left: 18px;
	}

	.scope__row {
		display: flex;
		align-items: flex-start;
		gap: var(--space-3);
		cursor: pointer;
	}

	.badge--ok {
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
		font-size: var(--text-xs);
		padding: 2px var(--space-2);
		border-radius: var(--radius-pill);
		background: var(--color-success-surface);
		border: 1px solid var(--color-success);
		align-self: flex-start;
	}

	a.button {
		text-decoration: none;
	}
</style>
