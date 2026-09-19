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
// # Where the known types come from
//
// From **bridgev2's own enumerations** — `mautrix/go`, `bridgev2/login.go`'s
// `LoginInputFieldType` and `LoginCookieFieldSourceType` — and not from the
// types this repository has happened to meet. The difference is not academic:
// the first version of this module knew one grouped type, `cookie`, because
// that is the one #57's screen was written for, and a first-party connector
// (LinkedIn) asks for three `request_header` fields. A set assembled from
// experience would have refused that legitimate login with a defect card on it.
// So when a type appears that is not here, the fix is to read that enumeration
// again rather than to add the one value in front of you.
//
// One consequence worth stating, and a known rough edge. All five grouped
// source types share **one** control, because bridgev2's own answer to a
// `cookies` step is one map keyed by field id whatever each field's source was.
// A jar that is all cookies asks for the `name=value` spelling people arrive
// with; a jar that is not asks for JSON, because a `Cookie` *header* is itself a
// list of `name=value` pairs and pasting one beside two other headers has no
// unambiguous flat reading. That is honest but not kind, and a better control
// for a mixed jar — one field per header, say — is a decision for whoever
// connects such a network rather than something to invent here.
//
// # Why refusing one field blocks the step
//
// Because a bridge drops the login process when it refuses a value: submitting
// six of seven fields does not get a correction, it gets a dead login and a
// user who has to fetch a fresh code. So a step with a **required** field this
// build cannot collect is not submittable, says which field and why, and offers
// the way out rather than a button that spends the user's code.
//
// Required, because bridgev2 says which fields are: refusing a step over a
// field the bridge itself called optional would block a login that would have
// worked. Such a field is left out of the answer and named on screen, since a
// field that vanished silently is a field the user goes looking for.
//
// # Nothing here keeps a value
//
// The parsed answer is returned to the caller and referenced nowhere else. No
// module-level state, no store, no storage — an answer is a network credential
// in flight (ADR 0011).

import { missing, parseCookies, type CookieJar } from './cookies';
import type { InputField } from './login-view';

/**
 * The field types this Companion draws one control for, and how.
 *
 * **The set is bridgev2's `LoginInputFieldType`, in full** — `mautrix/go`,
 * `bridgev2/login.go` — and not the types this repository has happened to meet.
 * That distinction is the whole difference between refusing a field nobody can
 * draw and refusing a legitimate login: the LinkedIn connector asks for three
 * fields this project had never seen, and a set assembled from experience would
 * have turned that into an unanswerable step with a defect card on it.
 *
 * So when a new type appears upstream, the fix is to read that enumeration
 * again — never to add the one value in front of you.
 *
 * `autocomplete` matters for more than convenience: naming a field
 * `one-time-code` is what lets a phone offer the code out of the message that
 * just arrived, which is the difference between a two-step login the user
 * finishes and one they abandon.
 */
const ENTRY_TYPES = {
	username: { control: 'text', autocomplete: 'username', secret: false },
	password: { control: 'password', autocomplete: 'current-password', secret: true },
	phone_number: { control: 'tel', autocomplete: 'tel', secret: false },
	email: { control: 'email', autocomplete: 'email', secret: false },
	'2fa_code': { control: 'text', autocomplete: 'one-time-code', secret: false },
	token: { control: 'password', autocomplete: 'off', secret: true },
	url: { control: 'url', autocomplete: 'url', secret: false },
	domain: { control: 'text', autocomplete: 'off', secret: false },
	/** One of `options`, so the control is a list and not a text input. */
	select: { control: 'select', autocomplete: 'off', secret: false },
	/** The answer to a challenge the network showed elsewhere. Never remembered. */
	captcha_code: { control: 'text', autocomplete: 'off', secret: false }
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
 * **The set is bridgev2's `LoginCookieFieldSourceType`, in full** —
 * `mautrix/go`, `bridgev2/login.go`. All five belong to the *same* control
 * rather than one each, and that is not a simplification: a `cookies` step's
 * answer is one map keyed by field id whatever each field's source was, so the
 * bridge itself does not separate them. A field wanting a cookie and a field
 * wanting a request header are two values fetched from one browser session in
 * one sitting, which is exactly what "grouped" means.
 *
 * Taking the set from the enumeration rather than from experience is the
 * correction #175 needed: with `cookie` alone, LinkedIn's three
 * `request_header` fields — a whole `Cookie` header and two `X-LI-*` values —
 * would each have been refused as a type nobody draws, turning a legitimate
 * login into a defect card.
 */
const JAR_TYPES = ['cookie', 'local_storage', 'request_header', 'request_body', 'special'] as const;

export type JarType = (typeof JAR_TYPES)[number];

/**
 * The member of the submitted `data` object a jar goes in.
 *
 * One for all five source types, because bridgev2 answers a `cookies` step with
 * one map — `{"cookies": {…}}` on this origin, which
 * `companion-gateway/openapi.yaml` documents and relays untouched.
 */
const JAR_MEMBER = 'cookies';

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
	/** Several fields of grouped types: one paste, one parser. */
	| {
			readonly kind: 'jar';
			readonly id: string;
			/**
			 * Whether every field in it is a `cookie`.
			 *
			 * It decides the words, and only the words: a jar of request headers
			 * must not be called a jar of cookies, and a `Cookie` header is one
			 * *value* rather than a list of them, so the spelling to ask for is
			 * not the same either.
			 */
			readonly allCookies: boolean;
			/** The domain the bridge says they share, when it says one. */
			readonly domain: string | null;
			readonly fields: readonly InputField[];
			/**
			 * What to look for, as the user will see it in their browser: each
			 * field's source name, falling back to its id.
			 *
			 * Not the same list as the ids the answer is submitted under, which is
			 * why `answerOf` maps one to the other.
			 */
			readonly names: readonly string[];
	  }
	/** A field this build will not draw, and which the step therefore cannot answer. */
	| {
			readonly kind: 'refused';
			readonly id: string;
			readonly field: InputField;
			readonly because: 'no_type' | 'unknown_type' | 'no_options';
	  };

/**
 * A step's fields as the controls that collect them, in the bridge's order.
 *
 * A jar takes the position of the first of its fields, so a step that mixes a
 * jar with ordinary fields draws them where the bridge put them.
 */
export function controlsOf(fields: readonly InputField[]): Control[] {
	const controls: Control[] = [];
	/** Where the one jar sits, once a grouped field has opened it. */
	let jarAt: number | null = null;
	for (const field of fields) {
		if (field.type === null) {
			controls.push({ kind: 'refused', id: field.id, field, because: 'no_type' });
			continue;
		}
		if (JAR_TYPES.some((known) => known === field.type)) {
			const name = field.sourceName ?? field.id;
			if (jarAt === null) {
				jarAt = controls.length;
				controls.push({
					kind: 'jar',
					id: JAR_ID,
					allCookies: field.type === 'cookie',
					domain: field.cookieDomain,
					fields: [field],
					names: [name]
				});
				continue;
			}
			const jar = controls[jarAt];
			if (jar?.kind === 'jar') {
				controls[jarAt] = {
					...jar,
					allCookies: jar.allCookies && field.type === 'cookie',
					// The first domain stated wins; the names are what the paste is
					// checked against either way.
					domain: jar.domain ?? field.cookieDomain,
					fields: [...jar.fields, field],
					names: [...jar.names, name]
				};
			}
			continue;
		}
		const entry = ENTRY_TYPES[field.type as EntryType];
		if (entry === undefined) {
			controls.push({ kind: 'refused', id: field.id, field, because: 'unknown_type' });
			continue;
		}
		if (entry.control === 'select' && field.options.length === 0) {
			// A list with nothing in it is not a control, and picking for the user
			// is worse than saying so.
			controls.push({ kind: 'refused', id: field.id, field, because: 'no_options' });
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

/** The one jar's control id, since a step has at most one. */
const JAR_ID = 'jar';

/**
 * Whether the step can be answered at all.
 *
 * A refused field stops the step only when the bridge says the login needs it.
 * Both halves matter: sending part of a *required* answer does not get a
 * correction — the network ends the login and the user pays a fresh code for it
 * — and refusing a step over a field the bridge called optional would block a
 * login that would have worked.
 */
export function answerable(controls: readonly Control[]): boolean {
	return (
		controls.some((control) => control.kind !== 'refused') &&
		!controls.some((control) => control.kind === 'refused' && control.field.required)
	);
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
 * goes in as one map under `cookies`, keyed by each field's id whatever its
 * source was.
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
			// Not a problem with a value: either the step is not answerable at
			// all — which `answerable` is what says — or the bridge called this
			// field optional and the answer simply leaves it out.
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
			// Required fields only: a bridge that calls one optional must not have
			// the paste rejected for lacking it.
			const wanted = control.fields
				.filter((field) => field.required)
				.map((field) => field.sourceName ?? field.id);
			const absent = missing(wanted, parsed);
			if (absent.length > 0) {
				problems.push({ controlId: control.id, because: 'incomplete', names: absent });
				continue;
			}
			// The paste is keyed by what the browser calls each value; the answer
			// is keyed by the **id** the bridge submits it under. They are not
			// always the same string, and only the bridge's id will do.
			const jar: Record<string, string> = {};
			for (const field of control.fields) {
				const found = parsed[field.sourceName ?? field.id];
				if (found !== undefined && found !== '') {
					jar[field.id] = found;
				}
			}
			// bridgev2's `cookies` member is a **string**, not a map — the bridge
			// parses the blob itself, which is why its own instructions offer a
			// cURL command as well as a JSON object. Submitting the map is a type
			// error the bridge answers with `cannot unmarshal object into Go
			// struct field .cookies of type string`, and the login process dies
			// with it (#221).
			//
			// What goes over is the JSON object spelling the bridge names, built
			// from the fields it asked for — not the user's paste. Relaying the
			// paste verbatim would also satisfy the bridge and would hand it the
			// user's whole Google session, which is more credential than the
			// login needs (ADR 0011) and is the property the test below holds.
			data[JAR_MEMBER] = JSON.stringify(jar);
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
