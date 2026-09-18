<!--
	Managing the WhatsApp link (ticket #108): which account is linked, since
	when, how it is doing — and the two actions that exist, Disconnect and
	Re-link.

	The screen itself is `$lib/components/NetworkManage.svelte`; this route is
	the network the Gateway's bridge list is matched on.
-->
<script lang="ts">
	import NetworkManage from '$lib/components/NetworkManage.svelte';
</script>

<NetworkManage network="whatsapp" />
