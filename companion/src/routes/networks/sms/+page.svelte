<!--
	Screen 3c of `docs/wireframes/companion-v0.1.md`: SMS through Google
	Messages — the v0.1 **preview** path.

	The framing the milestone decided is not decoration and is not softened here:
	SMS transits Google Messages Web, a Google account is required, an iPhone
	alone cannot feed it, and v0.2 replaces the whole path with the first-party
	Twake SMS Companion (ADR 0004). The disclosure card says all of that before
	anything else is shown.

	# What this route is, since ADR 0030

	Two things and no more: the iOS blocker, which is about *this device* and
	cannot be a step of any login, and `SMS` in `$lib/networks/copy.ts`. The
	screen is `$lib/components/login/BridgeLogin.svelte` — the same component the
	QR screens use — and the cookie step is drawn by the same field-type-driven
	panel every step goes through.

	That migration is the test of whether the design works, and it is the reason
	this file is short rather than the reason anything was lost. The cookie step
	already *is* a bridgev2 step whose field names come from the bridge; what was
	bespoke here was the **prose**, and prose is what `stepCopy` carries. #57's
	four requirements — which cookies, where to get them, why a private window,
	and that Device Bound Session Credentials must be off — are keyed on the
	step id in `copy.ts`, and the panel says so out loud if that step ever stops
	matching the explanation.

	# The cookies, honestly

	Google deprecated the QR login for third-party Google Messages clients in
	2024, so mautrix-gmessages signs in with the user's Google session cookies
	(docs.mau.fi). The wireframe imagined an in-app browser doing the extraction;
	a PWA has none, and no page may read another origin's cookies — that is the
	same-origin policy, not a gap to work around. So the user copies them, and
	the step's words say plainly what they are handing over.

	# Where they go, and where they do not

	Into `POST /api/bridges/{id}/login/submit`, relayed to the bridge and
	forgotten by the Gateway (ADR 0011). **Nothing here writes them anywhere**:
	not to `localStorage`, not to IndexedDB, not to a store that outlives the
	page. The paste is cleared the moment the submission is made.
-->
<script lang="ts">
	import { onMount } from 'svelte';

	import BridgeLogin from '$lib/components/login/BridgeLogin.svelte';
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import { looksLikeIos } from '$lib/networks/catalogue';
	import { SMS } from '$lib/networks/copy';

	let ios = $state(false);
	/** Until the user agent has been read, neither branch is taken. */
	let known = $state(false);

	onMount(() => {
		ios = looksLikeIos(navigator.userAgent, navigator.maxTouchPoints, navigator.platform);
		known = true;
	});
</script>

{#if known && ios}
	<!-- The wireframe's *iOS user detected* state: the screen is replaced, not
	     decorated, and the way out is a button. No login is started behind it. -->
	<BridgeLogin copy={SMS}>
		{#snippet blocked()}
			<div class="card card--info" data-testid="sms-ios">
				<p class="card__title">
					<Icon name="phone" size="dense" />
					{$t('sms.ios.title')}
				</p>
				<p>{$t('sms.ios.body')}</p>
				<p>
					<a class="button button--primary" href="/networks">{$t('sms.ios.skip')}</a>
				</p>
			</div>
		{/snippet}
	</BridgeLogin>
{:else if known}
	<BridgeLogin copy={SMS} />
{/if}
