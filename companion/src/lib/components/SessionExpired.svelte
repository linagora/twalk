<!--
	The session-expired state (#111): what the Companion shows when both its
	credentials are gone and no refresh can bring them back.

	# Why an overlay and not a screen of its own

	The ticket's fourth acceptance criterion is the one that shapes this file:
	*after signing in again, the user lands back where they were, not at the
	first screen*. The failure it exists to prevent is real and expensive — the
	owner, sent back to the beginning by an expired session, was about to
	re-pair a WhatsApp link that had never broken.

	So this is a dialog over whatever screen the user was on. Nothing navigates
	on its own, nothing is unmounted, and the way out is a link to the sign-in
	screen carrying `next` — the path the user is on right now — so that screen
	brings them back here rather than to the dashboard.

	Signing in is **not** done here. `/signin` (ticket #112) already speaks both
	the homeserver's password flow and its SSO redirect, and then mints the
	OpenID token for `POST /api/session` (ADR 0011). A second sign-in
	implementation inside a dialog would be a second thing to keep correct, and
	the one in a dialog could never offer SSO, which is a redirect by nature.

	# What it says, and what it does not say

	It does not say the server could not be reached. The server answered — that
	is precisely what a `401` is — and telling the user otherwise sends them to
	look at their network for a problem that is not there. See
	`$lib/api/trouble.ts`: that conflation caused three separate incidents in a
	day.
-->
<script lang="ts">
	import { page } from '$app/state';

	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import type { SessionStatus } from '$lib/session/state';

	interface Props {
		/** The expired session, which remembers who was signed in. */
		status: Extract<SessionStatus, { kind: 'expired' }>;
	}

	let { status }: Props = $props();

	/**
	 * Where to come back to: this origin's current path, nothing else. Passed
	 * as a parameter rather than remembered, so the destination is visible in
	 * the link the user is about to follow.
	 */
	const next = $derived(`${page.url.pathname}${page.url.search}`);
	const signInHref = $derived(`/signin?next=${encodeURIComponent(next)}`);
</script>

<div class="scrim" data-testid="session-expired">
	<div class="dialog card" role="alertdialog" aria-modal="true" aria-labelledby="session-expired-title">
		<p class="card__title" id="session-expired-title">
			<Icon name="warning" size="dense" />
			{$t('session.expired.title')}
		</p>
		<p>
			{#if status.owner === null}
				{$t('session.expired.body')}
			{:else}
				{$t('session.expired.bodyFor', { owner: status.owner })}
			{/if}
		</p>
		<p class="small muted">{$t('session.expired.stay')}</p>

		<p>
			<a class="button button--primary" href={signInHref} data-testid="session-expired-signin">
				{$t('session.expired.signIn')}
				<Icon name="continue" size="dense" />
			</a>
		</p>
	</div>
</div>

<style>
	.scrim {
		position: fixed;
		inset: 0;
		z-index: 10;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: var(--layout-gutter);
		background: color-mix(in srgb, var(--color-text) 45%, transparent);
	}

	.dialog {
		width: 100%;
		max-width: 26rem;
		max-height: 100%;
		overflow-y: auto;
		background: var(--color-surface);
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}

	.dialog :global(.button) {
		text-decoration: none;
	}
</style>
