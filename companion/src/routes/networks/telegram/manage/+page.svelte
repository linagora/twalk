<!--
	Managing the Telegram link. The same screen as WhatsApp's and Signal's: the
	Gateway reports one shape for every bridge, so there is one component and
	this route only names the network.

	It was missing when the Telegram login screen shipped, which is not a
	cosmetic gap: `manageRouteFor` derives `<route>/manage` for every card that
	needs a bridge, so *Manage* appeared on the card the moment the account
	linked and led nowhere. A route the catalogue can produce and the app does
	not serve is the same defect in miniature as everything else found today —
	something that reports itself as available while being absent.
-->
<script lang="ts">
	import NetworkManage from '$lib/components/NetworkManage.svelte';
</script>

<NetworkManage network="telegram" />
