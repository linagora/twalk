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
