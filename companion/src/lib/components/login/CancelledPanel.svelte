<!-- The login was abandoned on purpose, here or from another device. -->
<script lang="ts">
	import Icon from '$lib/icons/Icon.svelte';
	import { t } from '$lib/i18n';
	import type { PanelProps, ViewOf } from '$lib/networks/login-panels';

	let { flow }: PanelProps<ViewOf<'cancelled'>> = $props();
</script>

<div class="stack" data-testid="login-cancelled">
	<p>{$t('networks.cancelled')}</p>
	<p>
		<button class="button button--primary" type="button" onclick={flow.restart} data-testid="restart">
			<Icon name="reload" size="dense" />
			{$t('networks.refresh')}
		</button>
	</p>
</div>
