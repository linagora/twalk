<!--
	The answer to an action the user just took, rendered where they took it.

	# The defect this component is the one decision about

	Found live (#139): the owner picked a room out of about a hundred, scrolled
	to the bottom, pressed "Invite the Sensor into 1 room", and reported that
	nothing happened. Something did — the Gateway refused, the screen set its
	message, and the message rendered at the top of the page, roughly three
	thousand pixels above the button. The code was right, the string was right,
	and the user was told nothing. A correct message nobody reads is not better
	than no message.

	Four hundred automated tests cover these screens and none of them could see
	it, because a test clicks by selector and a person clicks with a finger.

	# The rule, decided once

	A refusal of a submitted action is rendered **beside the control that
	caused it**, and this component is what every screen renders there. Placing
	it is the caller's job — the component cannot know where the button is —
	and the rule is: immediately before or after the action, inside the same
	block.

	Moving the page is the fallback, not the design. `scrollIntoView` with
	`block: 'nearest'` does nothing when the message is already on screen, so a
	message that was placed correctly never makes the page jump; it only rescues
	the case where the action itself has been scrolled out of view since (a
	long list re-rendering, a keyboard opening, a message that grew).

	`role="alert"` is on the element itself, so assistive technology is told
	wherever the message is rendered — the acceptance criterion this must keep
	satisfying, and the reason the alert is created when the message appears
	rather than living in the page empty.
-->
<script lang="ts">
	interface Props {
		/** The refusal, or `null` when the action has not been refused. */
		message: string | null;
		/** So a spec can address this screen's refusal by name. */
		testId: string;
		/**
		 * `card` is the boxed warning of a whole-screen refusal, `text` the
		 * inline error line beneath a control. Both are existing tokens; this
		 * component adds no styling of its own.
		 */
		variant?: 'card' | 'card-small' | 'text';
		/** Extra data attribute some screens carry, e.g. which trouble it was. */
		kind?: string | null;
	}

	let { message, testId, variant = 'card', kind = null }: Props = $props();

	let element = $state<HTMLElement | null>(null);

	const className = $derived(
		variant === 'text'
			? 'error-text'
			: variant === 'card-small'
				? 'card card--warning small'
				: 'card card--warning'
	);

	$effect(() => {
		// Read both, so the effect re-runs when a new message replaces an old
		// one and not only when one first appears.
		const node = element;
		const text = message;
		if (node === null || text === null) {
			return;
		}
		// `nearest` is the whole point: no movement when the message is
		// already visible, the shortest movement when it is not.
		node.scrollIntoView({ block: 'nearest', inline: 'nearest' });
	});
</script>

{#if message !== null}
	<p
		bind:this={element}
		class={className}
		role="alert"
		data-testid={testId}
		data-kind={kind ?? undefined}
	>
		{message}
	</p>
{/if}
