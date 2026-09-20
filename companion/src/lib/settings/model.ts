// The settings screen's pure half (ticket #101): what the Gateway's answers
// mean for the form, and what the form sends back.
//
// This screen is where a non-technical user meets three sovereignty decisions
// in a row — which model reads their contacts' messages, which language their
// assistant falls back to, and (once #99 lands) where its traces go. The copy
// says what leaves the machine when each control is used; this module says
// what each control *is*, so the rules below are tested against values rather
// than against pixels.
//
// # Four things the model form has to get right
//
// - **The credential is write-only.** `GET /api/settings/model` never carries
//   it: it describes it (`configured`, `source`, `hint`). So the form cannot
//   round-trip it, and the request it sends says one of three things —
//   *absent* keeps what is stored (a form that only renamed the model), `null`
//   forgets the browser's value, a string replaces it. A member the shape does
//   not have is refused by the Gateway rather than ignored, which is why the
//   request is built here and not assembled ad hoc.
// - **A file wins.** When the operator supplied `GATEWAY_LLM_API_KEY_FILE`,
//   `credential.source === 'file'` and whatever the browser stored is ignored
//   (#98's precedence, ADR 0015). The screen says the file is in force and
//   offers no field to overwrite it: a control that looked like it worked and
//   did nothing would be the defect this project keeps closing.
// - **`configured: false` is a state, not an error.** A new deployment answers
//   `200` with every model member `null`. The form renders empty and says that
//   no model means no persona starts.
// - **The probe spends money.** `POST /api/settings/model/probe` sends one
//   real one-token completion — the only call in Twalk that bills the operator
//   without a message arriving first — so the button says so, and its four
//   answers stay four sentences: nothing configured, nothing answered, refused,
//   answered something that is not a completion.
//
// # The language
//
// `PUT /api/settings/language` takes one of the five the Companion ships, or
// `null` for no preference — which is **not** English. The setting has two
// effects, said where it is set: it changes this interface (the same switch
// the diagnostics page offers), and it is the language a persona falls back
// to when it cannot tell what language a message was written in (ADR 0016).
// It does not decide the language a suggestion is written in.
//
// # The disclosure (#121, ADR 0019, ADR 0031)
//
// Every reply a persona drafted reaches the contact with one sentence after
// it, on a line of its own, in the language the reply was written in — and
// this screen holds the one control over that, which is global and recorded.
// `GET /api/settings/disclosure` answers on or off and, once somebody has
// decided, since when and by whom: an append-only journal in the Gateway, not
// a preference, because turning the sentence off is "a deliberate act with a
// timestamp" (ADR 0019). [`disclosureRecord`] turns that answer into the one
// line the card reads back, and it keeps three states apart rather than two:
// on because nobody ever decided, on since a dated decision, and off since
// one. The date is formatted in the interface's locale by [`disclosureDate`],
// the same instant the Gateway stamped, so the record the user reads and the
// row the journal holds cannot disagree on when.
//
// The sentence the card shows as an example is the catalogue's copy of the
// contract's (`contracts/disclosure/v1/sentences.json`), one per interface
// language — pinned to the contract file by `$lib/i18n/i18n.test.ts`, the way
// the SDK pins its own copy. The Companion cannot read the contract at build
// time: the Gateway image's Node stage copies `companion/` and nothing else.
//
// # What is not here
//
// Tracing. ADR 0017 makes it OTLP and opt-in with content as a second switch,
// and #101 asks for both controls — but the Gateway has no settings surface
// for either (#99 is open), and a control this screen cannot store would be a
// promise. The screen says so, once, rather than drawing it.

import type { components } from '$lib/api/schema';
import { LOCALE_NAMES, type MessageKey } from '$lib/i18n';

export type ModelConfiguration = components['schemas']['ModelConfiguration'];
export type ModelRequest = components['schemas']['ModelConfigurationRequest'];
export type LanguagePreference = components['schemas']['LanguagePreference'];
export type Language = NonNullable<LanguagePreference['language']>;
export type Probe = components['schemas']['ModelProbe'];
export type DisclosureState = components['schemas']['DisclosureState'];

/** The five languages, each named in itself — the Companion's own list, so the two cannot drift. */
export const LANGUAGE_NAMES: Record<Language, string> = LOCALE_NAMES;

/** What the form holds while the user edits it. Strings, as inputs are. */
export interface ModelForm {
	baseUrl: string;
	model: string;
	/** The credential field: empty means "leave what is stored" or "none", never "". */
	credential: string;
	/** The advanced field: the provider parameters as the user typed them. */
	params: string;
}

/** How the credential is to be rendered: three states, no fourth. */
export type CredentialState =
	/** An operator's file is in force; the browser's value, if any, is ignored. */
	| { kind: 'file'; path: string; companionStored: boolean }
	/** The browser set one, and the Gateway shows its last characters. */
	| { kind: 'companion'; hint: string | null }
	/** None is configured. */
	| { kind: 'none' };

export function credentialState(configuration: ModelConfiguration): CredentialState {
	const credential = configuration.credential;
	if (credential.source === 'file') {
		return {
			kind: 'file',
			path: credential.file ?? '',
			companionStored: credential.companion_credential_stored
		};
	}
	if (credential.source === 'companion' && credential.configured) {
		return { kind: 'companion', hint: credential.hint };
	}
	return { kind: 'none' };
}

/** The form as the Gateway's answer fills it. The credential is never in it. */
export function formOf(configuration: ModelConfiguration): ModelForm {
	return {
		baseUrl: configuration.base_url ?? '',
		model: configuration.model ?? '',
		credential: '',
		params: configuration.params === null ? '' : JSON.stringify(configuration.params, null, 2)
	};
}

/** Why a form cannot be sent, by field, before the Gateway is asked. */
export type FormProblem =
	| { field: 'baseUrl'; because: 'empty' | 'not_a_url' }
	| { field: 'model'; because: 'empty' }
	| { field: 'params'; because: 'not_an_object' };

/**
 * The request for a form, or the problems that keep it from being one.
 *
 * `forgetCredential` is the explicit act of removing the browser's credential
 * (`null`); an empty field otherwise means *keep what is stored*, because a
 * user who changed only the model must not lose the credential they typed
 * last week. What the Gateway checks is checked here first only where the
 * answer is local — a URL that is not one, a parameters field that is not a
 * JSON object — so the form can point at the field; everything else is the
 * Gateway's to refuse.
 */
export function requestOf(
	form: ModelForm,
	options: { forgetCredential: boolean } = { forgetCredential: false }
): { ok: true; request: ModelRequest } | { ok: false; problems: FormProblem[] } {
	const problems: FormProblem[] = [];
	const baseUrl = form.baseUrl.trim();
	if (baseUrl === '') {
		problems.push({ field: 'baseUrl', because: 'empty' });
	} else if (!looksLikeHttpUrl(baseUrl)) {
		problems.push({ field: 'baseUrl', because: 'not_a_url' });
	}
	const model = form.model.trim();
	if (model === '') {
		problems.push({ field: 'model', because: 'empty' });
	}
	let params: Record<string, unknown> | null = null;
	if (form.params.trim() !== '') {
		const parsed = parseObject(form.params);
		if (parsed === null) {
			problems.push({ field: 'params', because: 'not_an_object' });
		} else {
			params = parsed;
		}
	}
	if (problems.length > 0) {
		return { ok: false, problems };
	}
	const request: ModelRequest = { base_url: baseUrl, model, params };
	if (options.forgetCredential) {
		request.credential = null;
	} else if (form.credential.trim() !== '') {
		request.credential = form.credential.trim();
	}
	return { ok: true, request };
}

function looksLikeHttpUrl(value: string): boolean {
	try {
		const url = new URL(value);
		return (url.protocol === 'http:' || url.protocol === 'https:') && url.host !== '';
	} catch {
		return false;
	}
}

function parseObject(text: string): Record<string, unknown> | null {
	try {
		const parsed: unknown = JSON.parse(text);
		if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
			return null;
		}
		return parsed as Record<string, unknown>;
	} catch {
		return null;
	}
}

/** The record line's three shapes, and the values its sentence interpolates. */
export type DisclosureRecord =
	/** On, and nobody has ever decided: the default state, which is a record and not a gap. */
	| { kind: 'on'; key: 'settings.disclosure.record.on'; values: Record<string, never> }
	/** On again, since a dated, attributed decision. */
	| {
			kind: 'on';
			key: 'settings.disclosure.record.onSince';
			values: { date: string; actor: string };
	  }
	/** Off, since a dated, attributed decision. */
	| { kind: 'off'; key: 'settings.disclosure.record.off'; values: { date: string; actor: string } };

/**
 * An instant the Gateway stamped, as a date and time in the interface's
 * locale — "20 septembre 2026 à 10:05" for a French interface, the same instant
 * in English words for an English one.
 *
 * An instant that does not parse is rendered as the string the Gateway sent
 * rather than as nothing: the record is a fact about a decision, and a blank
 * where its date should be would read as "never".
 */
export function disclosureDate(iso: string, locale: string): string {
	const at = Date.parse(iso);
	if (Number.isNaN(at)) {
		return iso;
	}
	return new Intl.DateTimeFormat(locale, { dateStyle: 'long', timeStyle: 'short' }).format(at);
}

/**
 * The one line the disclosure card reads back: on, or "off since <date> by
 * <actor>".
 *
 * `enabled` alone does not decide the sentence. The Gateway answers `since`
 * and `actor` as `null` while nobody has decided, and that is a state the
 * card says in its own words — the default was never chosen by anyone — so
 * "on since 20 September by you" is reserved for a switch that was turned
 * back on. Off is always dated and attributed, because a journal row wrote
 * it; a `false` with no date would be a Gateway this build does not know, and
 * it is rendered as off with the instant it did not give rather than as on.
 */
export function disclosureRecord(state: DisclosureState, locale: string): DisclosureRecord {
	if (state.enabled && (state.since === null || state.actor === null)) {
		return { kind: 'on', key: 'settings.disclosure.record.on', values: {} };
	}
	const values = {
		date: state.since === null ? '' : disclosureDate(state.since, locale),
		actor: state.actor ?? ''
	};
	return state.enabled
		? { kind: 'on', key: 'settings.disclosure.record.onSince', values }
		: { kind: 'off', key: 'settings.disclosure.record.off', values };
}

/**
 * The Gateway's refusal codes for the two writes and the probe, each a
 * sentence the user can act on — the ones #98 handed over in #101's comment.
 * A code this table does not know renders the Gateway's own `detail`, so an
 * unknown refusal is a sentence rather than a blank.
 */
export const REFUSAL_COPY: Record<string, MessageKey> = {
	invalid_base_url: 'settings.model.refused.invalidBaseUrl',
	invalid_model: 'settings.model.refused.invalidModel',
	invalid_params: 'settings.model.refused.invalidParams',
	invalid_credential: 'settings.model.refused.invalidCredential',
	malformed_request: 'settings.model.refused.malformed',
	unsupported_language: 'settings.language.refused.unsupported',
	settings_not_configured: 'settings.refused.notConfigured',
	// The disclosure journal lives in the consent store, so a Gateway with no
	// bus has no switch to read — the consent routes' own code (#121).
	consent_not_configured: 'settings.disclosure.refused.notConfigured',
	store_unavailable: 'settings.refused.storeUnavailable'
};

/** The probe's four non-answers, each a different fix. */
export const PROBE_FAILURE_COPY: Record<string, MessageKey> = {
	model_not_configured: 'settings.probe.notConfigured',
	endpoint_unreachable: 'settings.probe.unreachable',
	endpoint_refused: 'settings.probe.refused',
	endpoint_not_compatible: 'settings.probe.notCompatible'
};

export function refusalCopy(code: string | null): MessageKey | null {
	return code === null ? null : (REFUSAL_COPY[code] ?? null);
}

export function probeFailureCopy(code: string | null): MessageKey | null {
	return code === null ? null : (PROBE_FAILURE_COPY[code] ?? null);
}
