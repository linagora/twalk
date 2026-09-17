<!--
	Draws one of the symbols named in `./index.ts`.

	Decorative by default: an icon beside a label adds nothing for a screen
	reader, so it is `aria-hidden` unless the caller passes a `label`, in which
	case it becomes an `img` with that accessible name. The wireframes ask for a
	descriptive name wherever the icon carries meaning on its own.

	Size and stroke come from the design tokens, applied as CSS (`.icon`,
	`.icon--dense` in `$lib/styles/base.css`) rather than through the icon
	library's `size` prop: that prop writes SVG presentation *attributes*, and
	`width="var(--icon-size)"` is not a length an SVG attribute accepts. CSS
	overrides those attributes, so the tokens win and a caller cannot pick a
	size off the grid.
-->
<script lang="ts">
	import { icons, type IconName, type IconSize } from './index';

	interface Props {
		name: IconName;
		size?: IconSize;
		/** An accessible name. Omit for an icon that sits beside its own label. */
		label?: string;
		class?: string;
	}

	let { name, size = 'default', label, class: className }: Props = $props();

	const Glyph = $derived(icons[name]);
	const classes = $derived(
		['icon', size === 'dense' ? 'icon--dense' : null, className].filter(Boolean).join(' ')
	);
</script>

<Glyph
	class={classes}
	aria-hidden={label === undefined ? 'true' : undefined}
	aria-label={label}
	role={label === undefined ? undefined : 'img'}
	focusable="false"
/>
