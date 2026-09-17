<!--
	The Twalk mark: a 32 px rounded square in the primary teal with a speech
	tail, as screen 1 asks for a 32 px logo in its header.

	Inline SVG rather than a file, so it takes its colour from the design
	tokens and follows the app into dark mode without a second asset.
-->
<script lang="ts">
	interface Props {
		size?: number;
		/** An accessible name, when the mark stands alone. */
		label?: string;
	}

	let { size = 32, label }: Props = $props();
</script>

<svg
	width={size}
	height={size}
	viewBox="0 0 32 32"
	fill="none"
	role={label === undefined ? undefined : 'img'}
	aria-hidden={label === undefined ? 'true' : undefined}
	aria-label={label}
	focusable="false"
>
	<rect width="32" height="32" rx="9" fill="var(--color-primary)" />
	<path
		d="M9 11.5h14M16 11.5v11"
		stroke="var(--color-primary-contrast)"
		stroke-width="2.75"
		stroke-linecap="round"
	/>
	<path d="M21.5 20.5v4.5l4-4.5z" fill="var(--color-primary-contrast)" />
</svg>
