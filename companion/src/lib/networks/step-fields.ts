// What a login step's fields become on screen, and what the answer to them
// looks like on the wire.
//
// # The field type is what decides, and nothing else
//
// A step says what it wants as a list of fields, each with a **type** the
// bridge set (ADR 0030). That type is the whole instruction: `password` is a
// control whose value is never shown, `phone_number` is one a phone keyboard
// opens for, and `cookie` is not a control at all — several `cookie` fields
// sharing a domain are one jar, collected through a single paste and read by
// one parser, because people arrive at that step holding a whole `Cookie`
// header or an extension's export and not seven values to transcribe.
//
// Two rules follow from that, and they are the reason this module exists
// instead of a `{#each fields}` in a component:
//
//   - **A type this build does not know is refused and named.** Not drawn as
//     text, not guessed at. The alternative is a password field the bridge
//     typed as something new being drawn as a plain input with the value on
//     screen, which is the worst outcome available to a type-driven renderer.
//   - **An absent type is the same case.** It used to become `username`
//     (#175's third defect), which is exactly that outcome for a bridge that
//     simply omitted the member.
//
// # Why refusing one field blocks the step
//
// Because a bridge drops the login process when it refuses a value: submitting
// six of seven fields does not get a correction, it gets a dead login and a
// user who has to fetch a fresh code. So a step with a field this build cannot
// collect is not submittable, says which field and why, and offers the way out
// rather than a button that spends the user's code.
//
// # Nothing here keeps a value
//
// The parsed answer is returned to the caller and referenced nowhere else. No
// module-level state, no store, no storage — an answer is a network credential
// in flight (ADR 0011).

import { missing, parseCookies, type CookieJar } from './cookies';
import type { InputField } from './login-view';

/**
 * The bridgev2 field types this Companion draws one control for, and how.
 *
 * `autocomplete` matters for more than convenience: naming a field
 * `one-time-code` is what lets a phone offer the code out of the message that
 * just arrived, which is the difference between a two-step login the user
 * finishes and one they abandon.
 */
const ENTRY_TYPES = {
	username: { control: 'text', autocomplete: 'username', secret: false },
	email: { control: 'email', autocomplete: 'email', secret: false },
	phone_number: { control: 'tel', autocomplete: 'tel', secret: false },
	password: { control: 'password', autocomplete: 'current-password', secret: true },
	'2fa_code': { control: 'text', autocomplete: 'one-time-code', secret: false },
	token: { control: 'password', autocomplete: 'off', secret: true },
	url: { control: 'url', autocomplete: 'url', secret: false },
	domain: { control: 'text', autocomplete: 'off', secret: false }
} as const;

export type EntryType = keyof typeof ENTRY_TYPES;

/**
 * The `autocomplete` token one entry gets. A union and not a `string`, because
 * the DOM's own attribute type is a union: a typo here would otherwise reach the
 * browser as a token it ignores, silently costing the phone's offer to fill in
 * the code it just received.
 */
export type AutocompleteHint = (typeof ENTRY_TYPES)[EntryType]['autocomplete'];

/**
 * The field types a bridge sends **grouped**, collected through one control.
 *
 * Exactly one, deliberately. bridgev2's cookie step is the shape this project
 * has seen and the one #57's screen is written for; other grouped types exist
 * in mautrix for networks nothing here bridges, and adding them from the
 * documentation rather than from a capture is how this repository bought three
 * bugs in one ticket (#106). An ungrouped type is refused and named, which is
 * a screen that says what is missing — the safe direction.
 */
const JAR_TYPES = ['cookie'] as const;

export type JarType = (typeof JAR_TYPES)[number];

/** The member of the submitted `data` object a jar of a given type goes in. */
const JAR_MEMBER: Record<JarType, string> = { cookie: 'cookies' };

/** One control on screen, and the fields it answers for. */
export type Control =
	/** One ordinary field: one input. */
	| {
			readonly kind: 'entry';
			/** Stable across re-renders and unique in the step: the field's own id. */
			readonly id: string;
			readonly field: InputField;
			readonly type: EntryType;
			readonly control: (typeof ENTRY_TYPES)[EntryType]['control'];
			readonly autocomplete: AutocompleteHint;
			/** A value that must never be echoed back to the screen. */
			readonly secret: boolean;
	  }
	/** Several fields of one grouped type: one paste, one parser. */
	| {
			readonly kind: 'jar';
			readonly id: string;
			readonly type: JarType;
			/** The domain the bridge says they share, when it says one. */
			readonly domain: string | null;
			readonly fields: readonly InputField[];
			/** The names the jar must contain, in the bridge's own order. */
			readonly names: readonly string[];
	  }
	/** A field this build will not draw, and which the step therefore cannot answer. */
	| {
			readonly kind: 'refused';
			readonly id: string;
			readonly field: InputField;
			readonly because: 'no_type' | 'unknown_type';
	  };

/**
 * A step's fields as the controls that collect them, in the bridge's order.
 *
 * A jar takes the position of the first of its fields, so a step that mixes a
 * jar with ordinary fields draws them where the bridge put them.
 */
export function controlsOf(fields: readonly InputField[]): Control[] {
	const controls: Control[] = [];
	const jars = new Map<JarType, number>();
	for (const field of fields) {
		if (field.type === null) {
			controls.push({ kind: 'refused', id: field.id, field, because: 'no_type' });
			continue;
		}
		const jarType = JAR_TYPES.find((known) => known === field.type);
		if (jarType !== undefined) {
			const at = jars.get(jarType);
			if (at === undefined) {
				jars.set(jarType, controls.length);
				controls.push({
					kind: 'jar',
					id: `jar:${jarType}`,
					type: jarType,
					domain: field.cookieDomain,
					fields: [field],
					names: [field.id]
				});
				continue;
			}
			const jar = controls[at];
			if (jar?.kind === 'jar') {
				controls[at] = {
					...jar,
					// The first domain stated wins; a jar spanning two of them is
					// a shape no capture shows, and the names are what the paste
					// is checked against either way.
					domain: jar.domain ?? field.cookieDomain,
					fields: [...jar.fields, field],
					names: [...jar.names, field.id]
				};
			}
			continue;
		}
		const entry = ENTRY_TYPES[field.type as EntryType];
		if (entry === undefined) {
			controls.push({ kind: 'refused', id: field.id, field, because: 'unknown_type' });
			continue;
		}
		controls.push({
			kind: 'entry',
			id: field.id,
			field,
			type: field.type as EntryType,
			control: entry.control,
			autocomplete: entry.autocomplete,
			secret: entry.secret
		});
	}
	return controls;
}

/** Whether every control of a step can be answered at all. */
export function answerable(controls: readonly Control[]): boolean {
	return controls.length > 0 && !controls.some((control) => control.kind === 'refused');
}

/** Why one control's value will not do. */
export type Problem =
	/** Nothing was typed. */
	| { readonly controlId: string; readonly because: 'empty' }
	/** The bridge's own pattern for the field does not match. */
	| { readonly controlId: string; readonly because: 'pattern' }
	/** A paste that could not be read as the jar it is meant to be. */
	| { readonly controlId: string; readonly because: 'unreadable' }
	/** A jar that is missing some of the names the bridge asked for. */
	| { readonly controlId: string; readonly because: 'incomplete'; readonly names: readonly string[] };

export type Answer =
	| { readonly ok: true; readonly data: Record<string, unknown> }
	| { readonly ok: false; readonly problems: readonly Problem[] };

/**
 * The step's answer, in the shape the bridge takes, or every reason it is not
 * ready.
 *
 * `values` is keyed on [`Control.id`]. An ordinary field goes in under its own
 * id — `{"phone_number": "+33…"}`, which is what bridgev2 reads — and a jar
 * goes in under the member its type names: `{"cookies": {…}}`.
 *
 * The bridge's own `pattern` is checked here rather than left to the network,
 * because the network's refusal destroys the login process: a phone number this
 * side can see is too short costs nothing to catch, and costs the user a fresh
 * code to get wrong.
 */
export function answerOf(
	controls: readonly Control[],
	values: Readonly<Record<string, string>>
): Answer {
	const problems: Problem[] = [];
	const data: Record<string, unknown> = {};
	for (const control of controls) {
		if (control.kind === 'refused') {
			// Not a problem with a value: the step is not answerable at all,
			// which `answerable` is what says so.
			continue;
		}
		const typed = (values[control.id] ?? '').trim();
		if (typed === '') {
			problems.push({ controlId: control.id, because: 'empty' });
			continue;
		}
		if (control.kind === 'jar') {
			const parsed = parseJar(values[control.id] ?? '');
			if (parsed === null) {
				problems.push({ controlId: control.id, because: 'unreadable' });
				continue;
			}
			const absent = missing(control.names, parsed);
			if (absent.length > 0) {
				problems.push({ controlId: control.id, because: 'incomplete', names: absent });
				continue;
			}
			data[JAR_MEMBER[control.type]] = parsed;
			continue;
		}
		// The raw value, not the trimmed one: a credential's own whitespace is
		// the credential's. Only the emptiness check works on the trim.
		const value = values[control.id] ?? '';
		if (!matches(control.field.pattern, value)) {
			problems.push({ controlId: control.id, because: 'pattern' });
			continue;
		}
		data[control.field.id] = value;
	}
	return problems.length > 0 ? { ok: false, problems } : { ok: true, data };
}

function parseJar(pasted: string): CookieJar | null {
	const parsed = parseCookies(pasted);
	return parsed.ok ? parsed.cookies : null;
}

/**
 * Whether a value satisfies the bridge's pattern for its field.
 *
 * A pattern this browser cannot compile is not a failed value: an unusable
 * pattern must never be the reason a correct answer is refused, so it is
 * ignored and the network decides.
 */
function matches(pattern: string | null, value: string): boolean {
	if (pattern === null || pattern === '') {
		return true;
	}
	try {
		return new RegExp(pattern, 'u').test(value);
	} catch {
		return true;
	}
}
