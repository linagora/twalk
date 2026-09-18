// The personas the Companion knows, and what activating one means.
//
// v0.1 ships exactly one, `assistant` (spec #65: "multi-persona management
// screens" are v0.2), so this table has one row — but it is a table, because
// the dashboard draws a row per persona and a screen written around a single
// hard-coded name would have to be rewritten rather than extended.
//
// # What a persona card promises, and what it does not
//
// ADR 0013: activating a persona is a consent decision on that persona,
// scoped to the networks it may read. There is no control API, no "start" and
// no "stop" — so anything this screen offered that is not expressible as a
// consent decision would be a control the system cannot honour. That is
// exactly why the design review (#74) struck two rows off the wireframe's
// screen 4:
//
//   - **auto-send** — the only feature in the product that would let an agent
//     send without human approval, two lines under the same card's promise
//     that nothing is sent without approval. Removed; if it is ever wanted it
//     needs its own ADR.
//   - **active hours** — a time window is a consent rule enforced at runtime
//     and audited, which v1.0's consent policies plan. A cosmetic control in a
//     privacy screen suggests a protection that does not exist.
//
// The same reasoning is why the two rows that remain are both locked. Reading
// is the minimum for the persona to be useful; suggesting is the only thing it
// can do with what it reads. Neither has a separate representation in the
// journal — a decision carries a subject, a state and a scope, and nothing
// else — so a switch for either would be a switch that records nothing.
// Activating the assistant is one decision, and pausing it is its revocation.

import type { IconName } from '$lib/icons';
import type { MessageKey } from '$lib/i18n';

/** One line of "what this persona will do", as screen 4 lists them. */
export interface PersonaAbility {
	readonly id: 'read' | 'suggest';
	readonly labelKey: MessageKey;
	readonly detailKey: MessageKey;
	/** Always on in v0.1. */
	readonly on: true;
	/**
	 * Why the row cannot be switched off — shown to the user, because a
	 * disabled control with no explanation is the thing the design review
	 * objected to.
	 */
	readonly lockedKey: MessageKey;
}

export interface PersonaCard {
	/** The persona's name, and the `subject.id` of every decision about it. */
	readonly id: string;
	readonly icon: IconName;
	readonly nameKey: MessageKey;
	readonly descriptionKey: MessageKey;
	readonly abilities: readonly PersonaAbility[];
}

export const ASSISTANT = 'assistant';

export const PERSONA_CARDS: readonly PersonaCard[] = [
	{
		id: ASSISTANT,
		icon: 'persona',
		nameKey: 'persona.assistant.name',
		descriptionKey: 'persona.assistant.description',
		abilities: [
			{
				id: 'read',
				labelKey: 'persona.ability.read.label',
				detailKey: 'persona.ability.read.detail',
				on: true,
				lockedKey: 'persona.ability.read.locked'
			},
			{
				id: 'suggest',
				labelKey: 'persona.ability.suggest.label',
				detailKey: 'persona.ability.suggest.detail',
				on: true,
				lockedKey: 'persona.ability.suggest.locked'
			}
		]
	}
];

export function personaCard(id: string): PersonaCard | undefined {
	return PERSONA_CARDS.find((card) => card.id === id);
}
