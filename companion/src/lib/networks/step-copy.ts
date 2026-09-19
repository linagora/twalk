// A step's words, where the bridge's are not enough.
//
// # Why this module exists at all
//
// A purely generic renderer — whatever the bridge says, drawn by type, with no
// Companion copy — would have given every new network its screen for free. It
// was rejected (ADR 0030) for one reason: mautrix's instructions for the Google
// cookie step are eleven words, and what the user needs to be told is why a
// private window is required, that signing out of a normal window kills the
// session the server is holding, that Chrome's Device Bound Session Credentials
// tie a session to the hardware key and must be off, and what the seven cookies
// together let their holder do. No bridge will ever say any of that — a bridge
// explains its own mechanism, not the user's exposure — and #57 made all four
// acceptance criteria.
//
// # The three rules that keep it honest
//
//   - **Keyed on the step id**, which is the bridge's own namespaced name for
//     the question, and **replacing** its instructions rather than appending to
//     them. Two explanations of one thing, one of them eleven words long, is
//     worse than either.
//   - **Failure copy too.** "Google refused the session" needs the three likely
//     causes, and the generic sentence has nowhere to put them.
//   - **Loud when the shape drifts.** An override is written against a step
//     whose type and field types are known; when the bridge changes either, the
//     explanation has quietly become wrong, and what this module does is say so
//     on screen and fall back to the bridge's own words. That is a mitigation
//     and not a cure — a bridge that renames a step takes its explanation with
//     it, which is the accepted cost of keying on the id.

import type { MessageKey } from '$lib/i18n';

import type { LoginView } from './login-view';

/** A block of this project's prose: a heading, a paragraph, a list. */
export interface StepSection {
	readonly title: MessageKey | null;
	readonly body: MessageKey | null;
	/** Numbered, for something the user does in order. */
	readonly steps: readonly MessageKey[];
	/** Bulleted, for conditions that all hold at once. */
	readonly bullets: readonly MessageKey[];
	/** Drawn as a warning card: what makes the difference between working and not. */
	readonly warn: boolean;
	/**
	 * Shows the step's own URL as a link at the end of the numbered steps.
	 *
	 * The URL is the **bridge's** — the page it says to sign in on — so this
	 * says "put it here", never what it is.
	 */
	readonly link: boolean;
}

/**
 * The shape an override was written against.
 *
 * Not decoration: it is what makes drift detectable. The SMS explanation is
 * about cookies, so a day when that step asks for a password instead must not
 * be a day when the user reads three paragraphs about private windows.
 */
export interface StepShape {
	/** The view kind the step reads as. */
	readonly kind: Extract<LoginView, { fields: unknown }>['kind'];
	/** The field types it asks for, as a set — order and count are the bridge's. */
	readonly fieldTypes: readonly string[];
}

/** This project's words for one step of one bridge's login. */
export interface StepOverride {
	/**
	 * The step ids this explanation is for.
	 *
	 * A list because a step id is per connector and the same explanation serves
	 * the bridge and the stub that stands in for it in the suite. An id that
	 * matches nothing means the bridge renamed its step: the panel then shows
	 * the bridge's own words and says an explanation is missing, rather than
	 * silently dropping four acceptance criteria.
	 */
	readonly stepIds: readonly string[];
	readonly expects: StepShape;
	/** Replaces the bridge's `instructions`. */
	readonly instructions: MessageKey | null;
	/** Prose above the controls. */
	readonly before: readonly StepSection[];
	/** The submit button's own words. */
	readonly submit: MessageKey;
	/** Prose below the controls — what happens to what was typed. */
	readonly after: readonly MessageKey[];
	/** Replaces the generic sentence when *this step's* answer is refused. */
	readonly refused: MessageKey | null;
}

/** The override for a step, or `null` when this project has nothing to add. */
export function overrideFor(
	overrides: readonly StepOverride[],
	stepId: string
): StepOverride | null {
	return overrides.find((override) => override.stepIds.includes(stepId)) ?? null;
}

/**
 * How an override no longer fits the step it was written for.
 *
 * `expected` and `found` are for the screen and the report: a drift the user
 * sees must name what changed, because the person who can fix it is reading
 * over their shoulder.
 */
export interface Drift {
	readonly because: 'kind' | 'field_types';
	readonly expected: string;
	readonly found: string;
}

/** Whether a step still has the shape its explanation was written against. */
export function driftOf(override: StepOverride, view: LoginView): Drift | null {
	if (!('fields' in view) || view.kind !== override.expects.kind) {
		return { because: 'kind', expected: override.expects.kind, found: view.kind };
	}
	const found = types(view.fields.map((field) => field.type));
	const expected = types(override.expects.fieldTypes);
	if (found.join(' ') !== expected.join(' ')) {
		return { because: 'field_types', expected: expected.join(', '), found: found.join(', ') };
	}
	return null;
}

/** The distinct field types of a step, sorted, so a count is not a difference. */
function types(raw: readonly (string | null)[]): string[] {
	return [...new Set(raw.map((type) => type ?? '(none)'))].sort();
}

/** A section, with everything the caller did not say left out. */
export function section(parts: Partial<StepSection>): StepSection {
	return {
		title: parts.title ?? null,
		body: parts.body ?? null,
		steps: parts.steps ?? [],
		bullets: parts.bullets ?? [],
		warn: parts.warn ?? false,
		link: parts.link ?? false
	};
}
