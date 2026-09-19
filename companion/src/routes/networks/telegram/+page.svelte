<!--
	Telegram login by QR code, as one more session of the user's account.

	The same lifecycle and the same component as screens 3a and 3b, without the
	disclosure card: Telegram supports several sessions on one account as a
	documented feature, so there is nothing to warn about.

	The bridge offers four flows — `qr`, `phone`, `bot` and a `manual` one its
	own description calls "advanced, do not use". This screen asks for `qr`,
	which is the only one that needs nothing typed; `phone` would ask for a
	number and then a code, and the code is worth avoiding because a refused one
	destroys the login process rather than being correctable (ADR 0030).
-->
<script lang="ts">
	import BridgeLogin from '$lib/components/login/BridgeLogin.svelte';
	import { TELEGRAM } from '$lib/networks/copy';
</script>

<BridgeLogin copy={TELEGRAM} />
