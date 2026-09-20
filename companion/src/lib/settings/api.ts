// The settings screen's calls (ticket #101). Every one returns a discriminated
// result rather than throwing, and none has a value that means "still
// working": what comes back is the answer, or a refusal with the Gateway's
// stable code and its own `detail`, so the screen has a sentence for every
// outcome and a spinner for none (#111, #135, #139).

import { gateway } from '$lib/api/client';
import { troubleOf, type ApiTrouble } from '$lib/api/trouble';
import type {
	DisclosureState,
	Language,
	LanguagePreference,
	ModelConfiguration,
	ModelRequest,
	Probe
} from './model';

/** A refusal: which kind of trouble, the Gateway's code when it gave one, and its own words. */
export interface Refused {
	ok: false;
	trouble: ApiTrouble;
	code: string | null;
	detail: string | null;
	/** The endpoint's own HTTP status, on a probe the endpoint answered. */
	endpointStatus: number | null;
}

function refused(answer: { error?: unknown; response: Response } | null): Refused {
	if (answer === null) {
		return { ok: false, trouble: 'unreachable', code: null, detail: null, endpointStatus: null };
	}
	const error = answer.error as
		| { error?: unknown; detail?: unknown; endpoint_status?: unknown }
		| undefined;
	return {
		ok: false,
		trouble: troubleOf(answer),
		code: typeof error?.error === 'string' ? error.error : null,
		detail: typeof error?.detail === 'string' ? error.detail : null,
		endpointStatus: typeof error?.endpoint_status === 'number' ? error.endpoint_status : null
	};
}

export type ModelAnswer = { ok: true; configuration: ModelConfiguration } | Refused;
export type LanguageAnswer = { ok: true; preference: LanguagePreference } | Refused;
export type ProbeAnswer = { ok: true; probe: Probe } | Refused;
export type DisclosureAnswer = { ok: true; state: DisclosureState } | Refused;

export async function loadModel(): Promise<ModelAnswer> {
	const answer = await gateway.GET('/api/settings/model').catch(() => null);
	if (answer?.data !== undefined) {
		return { ok: true, configuration: answer.data };
	}
	return refused(answer);
}

export async function saveModel(request: ModelRequest): Promise<ModelAnswer> {
	const answer = await gateway.PUT('/api/settings/model', { body: request }).catch(() => null);
	if (answer?.data !== undefined) {
		return { ok: true, configuration: answer.data };
	}
	return refused(answer);
}

/** Forgets the model, and the credential the browser set with it. */
export async function forgetModel(): Promise<{ ok: true } | Refused> {
	const answer = await gateway.DELETE('/api/settings/model').catch(() => null);
	if (answer !== null && answer.error === undefined) {
		return { ok: true };
	}
	return refused(answer);
}

/** One real one-token completion, billed to the operator. The screen says so. */
export async function probeModel(): Promise<ProbeAnswer> {
	const answer = await gateway.POST('/api/settings/model/probe').catch(() => null);
	if (answer?.data !== undefined) {
		return { ok: true, probe: answer.data };
	}
	return refused(answer);
}

export async function loadLanguage(): Promise<LanguageAnswer> {
	const answer = await gateway.GET('/api/settings/language').catch(() => null);
	if (answer?.data !== undefined) {
		return { ok: true, preference: answer.data };
	}
	return refused(answer);
}

/** `null` is "no preference", which is not English. */
export async function saveLanguage(language: Language | null): Promise<LanguageAnswer> {
	const answer = await gateway
		.PUT('/api/settings/language', { body: { language } })
		.catch(() => null);
	if (answer?.data !== undefined) {
		return { ok: true, preference: answer.data };
	}
	return refused(answer);
}

/** The disclosure switch as the Gateway's journal answers it (#121). */
export async function loadDisclosure(): Promise<DisclosureAnswer> {
	const answer = await gateway.GET('/api/settings/disclosure').catch(() => null);
	if (answer?.data !== undefined) {
		return { ok: true, state: answer.data };
	}
	return refused(answer);
}

/**
 * One decision about the disclosure: off for every reply approved from now
 * on, or back on. Appended to the Gateway's journal with the owner as actor
 * and the instant it was taken; `reason` is the user's own note, kept with
 * the decision and read back on the card. Never per message (ADR 0019).
 */
export async function saveDisclosure(enabled: boolean, reason?: string): Promise<DisclosureAnswer> {
	const note = reason?.trim() ?? '';
	const answer = await gateway
		.PUT('/api/settings/disclosure', {
			body: note === '' ? { enabled } : { enabled, reason: note }
		})
		.catch(() => null);
	if (answer?.data !== undefined) {
		return { ok: true, state: answer.data };
	}
	return refused(answer);
}
