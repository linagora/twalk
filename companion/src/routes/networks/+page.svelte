<!--
	Screen 3 of `docs/wireframes/companion-v0.1.md`: the network picker.

	The grid is the catalogue (`$lib/networks/catalogue.ts`) joined with what
	`GET /api/bridges` says this deployment configured. Two separate facts, and
	the screen keeps them separate:

	  - a card the wireframes list but this deployment has no bridge for is
	    shown, greyed, and says so — hiding it would leave the user wondering
	    where WhatsApp went;
	  - a Gateway that could not answer at all does *not* grey everything. It
	    says the list is unknown and leaves the cards tappable, because the
	    screen behind them handles a missing bridge on its own.

	`GET /api/bridges` reads configuration and the Gateway's own memory: it
	contacts no bridge, so this screen draws while every bridge is down.
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import { gateway } from '$lib/api/client';
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import { gridFor, looksLikeIos, type BridgeRow, type CardState } from '$lib/networks/catalogue';

	let bridges = $state<BridgeRow[]>([]);
	let bridgesKnown = $state(false);
	let loaded = $state(false);
	let ios = $state(false);

	onMount(async () => {
		ios = looksLikeIos(navigator.userAgent, navigator.maxTouchPoints, navigator.platform);
		const listed = await gateway.GET('/api/bridges');
		bridgesKnown = listed.error === undefined;
		bridges = listed.data?.bridges ?? [];
		loaded = true;
	});

	const grid = $derived<CardState[]>(gridFor({ bridges, bridgesKnown, ios }));

	/** The tooltip and the helper line under a card that cannot be tapped. */
	function blockedCopy(state: CardState): string | null {
		switch (state.blockedBy) {
			case 'ios':
				return $t('networks.card.iosBlocked');
			case 'no-bridge':
				return $t('networks.card.notConfigured');
			case 'coming-soon':
				return $t('network.comingSoon');
			default:
				return null;
		}
	}
</script>

<section class="screen" data-testid="screen-networks">
	<header class="stack">
		<p class="small">
			<a href="/onboarding">
				<Icon name="back" size="dense" />
				{$t('onboarding.back')}
			</a>
		</p>
		<h1>{$t('networks.title')}</h1>
		<p class="subtitle">{$t('networks.step')}</p>
		<p class="muted">{$t('networks.intro')}</p>
	</header>

	{#if loaded && !bridgesKnown}
		<p class="card card--warning small" role="status" data-testid="bridges-unknown">
			{$t('networks.unreachable')}
		</p>
	{/if}

	<ul class="grid" data-testid="network-grid" aria-busy={!loaded}>
		{#each grid as state (state.card.network)}
			{@const blocked = blockedCopy(state)}
			<li>
				<!-- svelte-ignore a11y_no_redundant_roles -->
				<div
					class="tile"
					class:tile--muted={state.blockedBy !== null}
					data-testid={`card-${state.card.network}`}
					data-blocked={state.blockedBy ?? ''}
					data-connected={state.connected ? 'yes' : 'no'}
					title={blocked ?? undefined}
				>
					<p class="tile__head">
						<Icon name={state.card.icon} />
						<span class="tile__name">{$t(state.card.titleKey)}</span>
						{#if state.card.preview}
							<span class="badge">{$t('networks.preview')}</span>
						{/if}
						{#if state.connected}
							<span class="badge badge--ok">
								<Icon name="check" size="dense" />
								{$t('networks.connected')}
							</span>
						{/if}
					</p>
					<p class="small muted">{$t(state.card.subtitleKey)}</p>

					{#if state.card.route !== null && state.blockedBy === null}
						<p class="tile__action">
							<a class="button button--secondary" href={state.card.route}>
								{#if state.connected}
									<Icon name="manage" size="dense" />
									{$t('networks.manage')}
								{:else}
									{$t('networks.continue')}
									<Icon name="continue" size="dense" />
								{/if}
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

	<p>
		<a class="skip-link" href="/onboarding" data-testid="skip-networks">{$t('networks.skip')}</a>
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

	.blocked {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--color-text-muted);
	}

	.skip-link {
		color: var(--color-text-muted);
	}
</style>
