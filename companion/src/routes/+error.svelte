<!--
	What a path the client-side router does not know answers with.

	This is the far end of the fallback contract: the Gateway answers any
	unknown path with `200.html` at HTTP 200 (never a 404, so a deep link
	reloaded cold loads the app), the app boots, its router finds no route, and
	this is what the user sees. A 404 from the origin would have shown them the
	Gateway's plain text instead of the Companion.
-->
<script lang="ts">
	import { page } from '$app/state';

	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
</script>

<section class="screen" data-testid="screen-error">
	<h1>{$t('error.title')}</h1>
	<p class="subtitle">{$t('error.body', { path: page.url.pathname })}</p>
	<p>
		<a class="button button--primary" href="/">
			<Icon name="back" size="dense" />
			{$t('error.home')}
		</a>
	</p>
</section>

<style>
	a.button {
		text-decoration: none;
	}
</style>
