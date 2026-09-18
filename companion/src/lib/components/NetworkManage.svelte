<!--
	The management screen of a connected network (ticket #108).

	# Why this screen exists

	*Manage* used to lead to the login screen. On a network that was already
	connected, that meant `GET .../login/flows` then `POST .../login` — a fresh
	QR flow against a working link. The Gateway refused it with
	`409 login_in_flight`, which was the Gateway being right: one login at a
	time per bridge is the rule (#55), and nothing here weakens it. What was
	wrong is that the Companion asked for a login at all, when what a user
	clicking *Manage* on a connected account wants is to see it — and, if they
	mean to, to end it.

	So this screen asks for nothing. It reads `GET /api/bridges`, shows the
	link the bridge holds — which account, since when, in what state — and
	offers the two actions that exist:

	  - **Disconnect**, which is `DELETE .../logins/{login_id}`, behind a
	    confirmation that says plainly what stops and what does not;
	  - **Re-link**, which is the existing reconnect path: the login screen
	    with the login id, so the flow repairs the session the bridge already
	    holds instead of creating a second one.

	# What it never reads

	`ConfiguredBridge.login` — the login *process*. It is on the same row, it
	has a confusable name, and reading it is the bug this ticket fixes.
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import { gateway } from '$lib/api/client';
	import { troubleOf, type ApiTrouble } from '$lib/api/trouble';
	import ActionProblem from '$lib/components/ActionProblem.svelte';
	import Icon from '$lib/icons/Icon.svelte';
	import { t, type MessageKey } from '$lib/i18n';
	import { cardFor } from '$lib/networks/catalogue';
	import {
		connectionOf,
		UNKNOWN_CONNECTION,
		type NetworkConnection
	} from '$lib/networks/connection';

	interface Props {
		/** The network this screen manages, as the Gateway spells it. */
		network: string;
	}

	let { network }: Props = $props();

	let loaded = $state(false);
	let bridgeId = $state<string | null>(null);
	/** `false` when `GET /api/bridges` itself could not be read. */
	let listKnown = $state(false);
	/** And which kind of "could not be read" it was (#111). */
	let listTrouble = $state<ApiTrouble | null>(null);
	let connection = $state<NetworkConnection>(UNKNOWN_CONNECTION);
	let confirming = $state(false);
	let working = $state(false);
	let trouble = $state<string | null>(null);
	let disconnected = $state(false);

	const card = $derived(cardFor(network));
	const account = $derived(connection.account);
	const loginRoute = $derived(card?.route ?? '/networks');

	onMount(load);

	async function load() {
		const listed = await gateway.GET('/api/bridges').catch(() => null);
		listKnown = listed !== null && listed.error === undefined;
		listTrouble = listKnown ? null : troubleOf(listed);
		const row = listed?.data?.bridges.find((bridge) => bridge.network === network) ?? null;
		bridgeId = row?.bridge_id ?? null;
		connection = connectionOf(row);
		loaded = true;
	}

	/**
	 * Ends the link. The bridge drops the session and the network credentials
	 * it kept for it; the Gateway never held either.
	 */
	async function disconnect() {
		if (bridgeId === null || account === null) {
			return;
		}
		working = true;
		trouble = null;
		const answer = await gateway
			.DELETE('/api/bridges/{bridge_id}/logins/{login_id}', {
				params: { path: { bridge_id: bridgeId, login_id: account.login_id } }
			})
			.catch(() => null);
		working = false;
		if (answer === null || answer.error !== undefined) {
			trouble = $t('manage.disconnect.failed');
			return;
		}
		confirming = false;
		disconnected = true;
		// Read the bridge again rather than assuming: the screen's whole point
		// is that it reports what the bridge says.
		await load();
	}

	/** One sentence per state, in the vocabulary the Gateway reports. */
	function stateCopy(state: NetworkConnection['state']): MessageKey {
		switch (state) {
			case 'connected':
				return 'manage.state.connected';
			case 'starting':
				return 'manage.state.starting';
			case 'degraded':
				return 'manage.state.degraded';
			case 'session_expired':
				return 'manage.state.sessionExpired';
			case 'disconnected':
				return 'manage.state.disconnected';
			default:
				return 'manage.state.unknown';
		}
	}

	function stateTone(state: NetworkConnection['state']): string {
		return state === 'session_expired' ? 'card--warning' : 'card--info';
	}

	/**
	 * The short label for a state — the same words the card on screen 3 uses,
	 * so the two screens never disagree about what is happening.
	 *
	 * Written as a switch over literals rather than built from the state's
	 * name: every key the app draws is a literal the catalogue's types check,
	 * which is what makes a missing translation a compile error instead of a
	 * key printed on screen.
	 */
	function stateLabel(state: NetworkConnection['state']): MessageKey {
		switch (state) {
			case 'connected':
				return 'networks.connected';
			case 'starting':
				return 'networks.state.starting';
			case 'degraded':
				return 'networks.state.degraded';
			case 'session_expired':
				return 'networks.state.sessionExpired';
			case 'disconnected':
				return 'networks.state.disconnected';
			default:
				return 'networks.state.unknown';
		}
	}
</script>

<section class="screen" data-testid={`screen-manage-${network}`} data-connection={connection.state}>
	<header class="stack">
		<p class="small">
			<a href="/networks">
				<Icon name="back" size="dense" />
				{$t('networks.back')}
			</a>
		</p>
		<h1>{$t('manage.title', { network: card === undefined ? network : $t(card.titleKey) })}</h1>
		<p class="subtitle">{$t('manage.caption')}</p>
	</header>

	{#if !loaded}
		<p class="waiting" data-testid="manage-loading">
			<span class="spinner" aria-hidden="true"></span>
			{$t('networks.loading')}
		</p>
	{:else if !listKnown}
		<!-- A server that refused is not a server that could not be reached, and
		     a screen that conflates them sends the user to look at their
		     firewall for an expired session (#111). -->
		<p
			class="card card--warning"
			role="status"
			data-testid="manage-list-unknown"
			data-trouble={listTrouble}
		>
			{#if listTrouble === 'session-refused'}
				{$t('api.trouble.sessionRefused')}
			{:else if listTrouble === 'refused'}
				{$t('api.trouble.refused')}
			{:else}
				{$t('api.trouble.unreachable')}
			{/if}
		</p>
	{:else if bridgeId === null}
		<div class="card card--warning" data-testid="no-bridge">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('networks.noBridge.title')}
			</p>
			<p>{$t('networks.noBridge.body', { network: card === undefined ? network : $t(card.titleKey) })}</p>
		</div>
	{:else if connection.state === 'unknown'}
		<!-- The Gateway could not ask the bridge. It says so rather than
		     reporting "disconnected", because telling a user their working link
		     is broken is the defect this ticket is about. -->
		<div class="card card--warning" data-testid="manage-unknown">
			<p class="card__title">
				<Icon name="warning" size="dense" />
				{$t('manage.unknown.title')}
			</p>
			<p>{$t('manage.unknown.body')}</p>
			<p>
				<button class="button button--secondary" type="button" onclick={load} data-testid="manage-retry">
					<Icon name="reload" size="dense" />
					{$t('networks.retry')}
				</button>
			</p>
		</div>
	{:else if account === null}
		<!-- Nothing is linked. Either it never was, or Disconnect just ended
		     it — and the difference is worth saying, because one of them is
		     something the user did a moment ago. -->
		<div class="card card--info" data-testid="manage-no-link">
			<p class="card__title">
				<Icon name="info" size="dense" />
				{$t(disconnected ? 'manage.disconnected.title' : 'manage.noLink.title')}
			</p>
			<p>{$t(disconnected ? 'manage.disconnected.body' : 'manage.noLink.body')}</p>
			<p>
				<a class="button button--primary" href={loginRoute} data-testid="manage-connect">
					{$t('manage.connect')}
					<Icon name="continue" size="dense" />
				</a>
			</p>
		</div>
	{:else}
		<dl class="facts card" data-testid="manage-link">
			<dt>{$t('manage.account.label')}</dt>
			<dd data-testid="manage-account">
				<Icon name="account" size="dense" />
				{account.name ?? account.login_id}
			</dd>

			<dt>{$t('manage.since.label')}</dt>
			<dd data-testid="manage-since">
				{#if account.since === null}
					<span class="muted">{$t('manage.since.unknown')}</span>
				{:else}
					{new Date(account.since).toLocaleString()}
				{/if}
			</dd>

			<dt>{$t('manage.state.label')}</dt>
			<dd data-testid="manage-state">
				<span class="state">{$t(stateLabel(connection.state))}</span>
			</dd>
		</dl>

		<p class={`card small ${stateTone(connection.state)}`} data-testid="manage-state-detail">
			{$t(stateCopy(connection.state))}
		</p>

		<!-- Above the disconnect and re-link controls, which are what it
		     answers, and it follows them when the confirmation opens (#139). -->
		<ActionProblem message={trouble} testId="manage-trouble" variant="card-small" />

		{#if confirming}
			<!-- The confirmation says what stops, and — just as important — what
			     does not. A user who thinks disconnecting deletes their
			     conversations will not disconnect a session they should. -->
			<div class="card card--warning" data-testid="disconnect-confirm">
				<p class="card__title">
					<Icon name="warning" size="dense" />
					{$t('manage.disconnect.confirm.title', {
						network: card === undefined ? network : $t(card.titleKey)
					})}
				</p>
				<ul class="consequences">
					<li data-testid="disconnect-stops">{$t('manage.disconnect.confirm.stops')}</li>
					<li data-testid="disconnect-keeps">{$t('manage.disconnect.confirm.keeps')}</li>
					<li>{$t('manage.disconnect.confirm.reversible')}</li>
				</ul>
				<div class="actions">
					<button
						class="button button--secondary danger"
						type="button"
						disabled={working}
						onclick={disconnect}
						data-testid="disconnect-yes"
					>
						<Icon name="revoke" size="dense" />
						{$t('manage.disconnect.confirm.yes')}
					</button>
					<button
						class="button button--secondary"
						type="button"
						disabled={working}
						onclick={() => (confirming = false)}
						data-testid="disconnect-no"
					>
						{$t('manage.disconnect.confirm.no')}
					</button>
				</div>
			</div>
		{:else}
			<div class="actions">
				<!-- Re-link is the Gateway's reconnect: the same flow against the
				     login the bridge already holds, never a second one. -->
				<a
					class="button button--primary"
					href={`${loginRoute}?relink=${encodeURIComponent(account.login_id)}`}
					data-testid="relink"
				>
					<Icon name="reload" size="dense" />
					{$t('manage.relink')}
				</a>
				<button
					class="button button--secondary"
					type="button"
					onclick={() => (confirming = true)}
					data-testid="disconnect"
				>
					<Icon name="revoke" size="dense" />
					{$t('manage.disconnect')}
				</button>
			</div>
			<p class="small muted">{$t('manage.relink.hint')}</p>
		{/if}
	{/if}
</section>

<style>
	.facts {
		display: grid;
		grid-template-columns: auto 1fr;
		gap: var(--space-2) var(--space-3);
		margin: 0;
		align-items: baseline;
	}

	.facts dt {
		color: var(--color-text-muted);
		font-size: var(--text-sm);
	}

	.facts dd {
		margin: 0;
		display: flex;
		align-items: center;
		gap: var(--space-2);
		font-weight: var(--font-weight-label);
	}

	.state {
		font-weight: var(--font-weight-label);
	}

	.consequences {
		margin: var(--space-2) 0 0;
		padding-inline-start: var(--space-4);
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	/* The one destructive button on this screen, marked as such without a
	   whole new button variant: it ends a link the user has. */
	.danger {
		color: var(--color-danger);
		border-color: var(--color-danger);
	}

	.waiting {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--color-text-muted);
	}
</style>
