<!--
	Managing the SMS preview's link (ticket #108).

	The login path for SMS is its own screen — cookies pasted, then an emoji to
	match — but the *link* it leaves behind is the same kind of thing as any
	other bridge's, so managing it is the same screen.
-->
<script lang="ts">
	import NetworkManage from '$lib/components/NetworkManage.svelte';
</script>

<NetworkManage network="sms" />
