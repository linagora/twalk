<!--
	A QR code drawn from a raw payload, with the text alternative the
	wireframes' accessibility section requires.

	The image is one `<svg role="img">` with an `aria-label`, not a decorative
	graphic: "Scan this code with WhatsApp Settings > Linked Devices" is the
	alternative a screen-reader user gets, and it is the *instruction*, not a
	description of a square — a description of the pixels would be useless, and
	the payload itself is a credential nobody should have read aloud.

	The quiet zone is four modules on every side, which the QR specification
	requires: a code drawn edge to edge is a code many cameras will not lock
	on to.

	`shape-rendering: crispEdges` keeps module boundaries on whole device
	pixels. Anti-aliased module edges are the classic cause of a code that
	scans on a desktop and fails on a phone held at an angle.
-->
<script lang="ts">
	import { encodeQr, qrPath } from '$lib/qr/code';

	interface Props {
		/** The raw payload the bridge handed over. */
		data: string;
		/** The screen-reader alternative: what to do with this code. */
		label: string;
		class?: string;
	}

	let { data, label, class: className = '' }: Props = $props();

	const QUIET_ZONE = 4;

	const grid = $derived.by(() => {
		try {
			return encodeQr(data);
		} catch {
			// A payload no QR version holds. The screen above shows its own
			// failure copy; drawing nothing is better than drawing a lie.
			return null;
		}
	});

	const path = $derived(grid === null ? '' : qrPath(grid));
	const extent = $derived(grid === null ? 0 : grid.size + QUIET_ZONE * 2);
</script>

{#if grid !== null}
	<svg
		class={`qr ${className}`}
		viewBox={`0 0 ${extent} ${extent}`}
		role="img"
		aria-label={label}
		data-testid="qr-code"
		data-modules={grid.size}
	>
		<rect width={extent} height={extent} fill="var(--qr-light, #ffffff)" />
		<g transform={`translate(${QUIET_ZONE} ${QUIET_ZONE})`}>
			<path d={path} fill="var(--qr-dark, #121417)" />
		</g>
	</svg>
{:else}
	<p class="error-text" role="alert" data-testid="qr-undrawable">{label}</p>
{/if}

<style>
	.qr {
		display: block;
		width: 100%;
		max-width: 17rem; /* 272 px: readable on a 375 px phone with gutters */
		height: auto;
		margin-inline: auto;
		border-radius: var(--radius-md);
		background: #ffffff;
		shape-rendering: crispEdges;
	}
</style>
