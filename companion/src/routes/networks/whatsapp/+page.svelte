<!--
	Screen 3a — WhatsApp login by QR code. The anchor experience of v0.1.

	The screen itself is `$lib/components/QrLogin.svelte`; this route is the
	words (`$lib/networks/copy.ts`) and the network the Gateway's bridge list is
	matched on.
-->
<script lang="ts">
	import QrLogin from '$lib/components/QrLogin.svelte';
	import { WHATSAPP } from '$lib/networks/copy';
</script>

<QrLogin copy={WHATSAPP} />
