<!--
	Screen 3b — Signal login by QR code, as a linked (secondary) device.

	The same lifecycle as screen 3a and the same component, without the
	disclosure card: Signal officially supports linked devices, so there is no
	ban risk to disclose.
-->
<script lang="ts">
	import BridgeLogin from '$lib/components/login/BridgeLogin.svelte';
	import { SIGNAL } from '$lib/networks/copy';
</script>

<BridgeLogin copy={SIGNAL} />
