// Whether a persona runtime is present on this deployment, as the screens
// say it (ticket #177).
//
// Two screens used to tell the user that no agent runtime was deployed here,
// from a copy key that was true the day it was written and false the day the
// reference deployment started running one. The fact is now readable:
// `GET /api/runtime` (#189) projects the bus's consumer list — the one trace a
// runtime leaves — into three words the Gateway can stand behind. This module
// turns the answer, or its absence, into the five states a screen renders,
// and the sentence for each. It reads nothing itself beyond that one call.
//
// Five, not three, because a screen that cannot read the fact must say so
// rather than pick a side (#139, #145): a bus that is not configured is a
// deployment no runtime can run against, and a read that failed is neither
// "present" nor "absent".

import { gateway } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import type { MessageKey } from '$lib/i18n';

export type RuntimeReading = components['schemas']['RuntimePresence'];

/** What a screen can say about the runtime. */
export type RuntimeState =
	/** At least one persona consumer is live: a runtime is hosting personas. */
	| 'present'
	/** Consumers exist and none is live: a runtime was here and is not now. */
	| 'gone'
	/** No persona consumer at all: no runtime has ever hosted one here. */
	| 'never'
	/** The Gateway has no bus, so no runtime can run against this deployment. */
	| 'no-bus'
	/** The read did not answer, or answered something this build cannot read. */
	| 'unknown';

export interface Runtime {
	state: RuntimeState;
	/** How many personas a present runtime is hosting live; 0 otherwise. */
	hosting: number;
}

export const UNKNOWN: Runtime = { state: 'unknown', hosting: 0 };

/** The Gateway's stable code, or `null` when the body carried none. */
function codeOf(error: unknown): string | null {
	const code = (error as { error?: unknown } | undefined)?.error;
	return typeof code === 'string' ? code : null;
}

/**
 * The state from one answer of `GET /api/runtime`. Pure, so the five
 * renderings are pinned against values rather than against a running bus.
 */
export function runtimeOf(answer: { data?: RuntimeReading; error?: unknown } | null): Runtime {
	if (answer === null) {
		return UNKNOWN;
	}
	if (answer.data !== undefined) {
		const reading = answer.data;
		const hosting = reading.personas.filter((persona) => persona.liveness === 'live').length;
		switch (reading.presence) {
			case 'present':
				return { state: 'present', hosting };
			case 'gone':
				return { state: 'gone', hosting: 0 };
			case 'never':
				return { state: 'never', hosting: 0 };
			default:
				return UNKNOWN;
		}
	}
	return codeOf(answer.error) === 'runtime_not_configured'
		? { state: 'no-bus', hosting: 0 }
		: UNKNOWN;
}

/** One read. Never throws: a screen that cannot read the fact says so. */
export async function readRuntime(): Promise<Runtime> {
	const answer = await gateway.GET('/api/runtime').catch(() => null);
	return runtimeOf(answer);
}

/** Whether, in this state, an activated assistant can produce anything now. */
export function runtimeRuns(runtime: Runtime): boolean {
	return runtime.state === 'present';
}

/**
 * The sentence the dashboard and the personas screen say about the runtime,
 * as a catalogue key. One key per state, so the catalogue test's parity check
 * covers every rendering and a state cannot fall through to another's words.
 */
export function runtimeCopyKey(state: RuntimeState): MessageKey {
	switch (state) {
		case 'present':
			return 'runtime.present';
		case 'gone':
			return 'runtime.gone';
		case 'never':
			return 'runtime.never';
		case 'no-bus':
			return 'runtime.noBus';
		case 'unknown':
			return 'runtime.unknown';
	}
}

/**
 * What the approvals screen's empty state says, given the runtime and whether
 * the user has activated any persona (#177, criterion 3). Three situations,
 * three sentences: no runtime to propose anything; a runtime with nothing
 * activated; an active assistant that has proposed nothing in this window —
 * the last being the common case, and the one the old copy buried under the
 * other two.
 */
export function emptyApprovalsKey(runtime: Runtime, anyPersonaActive: boolean | null): MessageKey {
	if (runtime.state === 'never' || runtime.state === 'gone' || runtime.state === 'no-bus') {
		return 'approvals.empty.noRuntime';
	}
	if (anyPersonaActive === false) {
		return 'approvals.empty.notActivated';
	}
	if (runtime.state === 'present' && anyPersonaActive === true) {
		return 'approvals.empty.nothingProposed';
	}
	// Unknown runtime, or unknown activation: the one sentence that claims
	// neither.
	return 'approvals.empty.unknown';
}
