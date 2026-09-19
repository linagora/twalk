<!--
	Screen 3a — WhatsApp login by QR code. The anchor experience of v0.1.

	The screen itself is `$lib/components/login/BridgeLogin.svelte`, which owns
	the flow and delegates each kind of step to a panel (ADR 0030); this route is
	the words (`$lib/networks/copy.ts`) and the network the Gateway's bridge list
	is matched on.
-->
<script lang="ts">
	import BridgeLogin from '$lib/components/login/BridgeLogin.svelte';
	import { WHATSAPP } from '$lib/networks/copy';
</script>

<BridgeLogin copy={WHATSAPP} />
