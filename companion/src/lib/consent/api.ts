// The four calls the consent screen makes, and the shapes it gets back.
//
// Every one returns a discriminated result rather than throwing, and none has a
// value that means "still working": what comes back is the answer, or an
// `Explained` refusal from `./refusal.ts` that names a cause and a next step
// (#100's rule, #111/#135/#139's incidents).
//
// # Three reads, and only one of them can empty the screen
//
// [`load`] asks three questions and keeps their failures apart, because they
// are not equally bad. The rule is: **a read that fails takes away only what it
// answers.**
//
//   - `GET /api/consent/state` is the record of what the user has decided. It is
//     the one read this screen cannot work without, so its refusal is the
//     screen's refusal.
//   - `GET /api/contacts/pending` adds the people who have written and were
//     never decided about. Its own failure is its own: `contacts_not_configured`
//     means this deployment consumes no inbound stream, and a screen that went
//     blank for it would also hide every decision the user has already taken —
//     which is still perfectly readable. So it comes back as a caveat beside a
//     shorter list.
//   - `GET /api/contacts/display-names` is a **nicety**. The Gateway reads it
//     off the tail of the bus and stores nothing, so it can answer
//     `502 bus_unreachable` while the list is perfectly readable — its own
//     description says as much: *"the Companion can show the Matrix IDs"*.
//
// That asymmetry is the whole reason this is a module and not three calls in a
// component: a screen that treated a missing display name, or an unwatched
// inbound stream, as a failed load would go blank over something it could
// simply have said.
//
// # One decision, one request
//
// [`decide`] takes one contact and one connection (ADR 0033, #272) — the
// perimeter, never its kind: two WhatsApp accounts are two decisions.
// `POST /api/consent/decisions`
// accepts one subject and one scope, and there is no batch endpoint to reach
// for: the bulk control writes N decisions with N requests, which is also what
// keeps the count on the button honest about what is about to happen.
//
// `actor` is deliberately not sent — the Gateway stamps this deployment's owner
// (ADR 0011), and the one identity field of an audit trail is not something a
// browser should be composing.
//
// # There is no "undo"
//
// The journal is append-only, so "return to undecided" is not an erasure: it is
// a decision whose answer is `pending`. `unset` is not a state a client may move
// to (the Gateway answers `unknown_value` for it) and this module offers no way
// to try. The screen has to say that, and it does.

import { gateway } from '$lib/api/client';
import { troubleOf } from '$lib/api/trouble';
import type { components } from '$lib/api/schema';
import { loadRegistry, type Connection } from '$lib/connections/registry';
import { explain, type Explained } from './refusal';
import type { DisplayName, Entry, PendingContact, State } from './model';

export type Recorded = components['schemas']['RecordedConsentDecision'];
export type PendingCount = components['schemas']['PendingContactCount'];
export type PendingConnectionCount = components['schemas']['PendingConnectionCount'];

/** How many contacts are waiting, in all, per connection and per network — the Gateway's own counts. */
export interface Waiting {
	total: number;
	connections: PendingConnectionCount[];
	networks: PendingCount[];
}

export interface Loaded {
	pending: readonly PendingContact[];
	entries: readonly Entry[];
	names: readonly DisplayName[];
	/** The registry (#272), or `[]` when it could not be read — the rows are then unlabelled. */
	connections: readonly Connection[];
	/** A refused registry read, beside rows that are still true. */
	connectionsProblem: Explained | null;
	/**
	 * The Gateway's counts, which always describe the whole list whatever a
	 * `?network=` narrowed — so a badge and the list beside it cannot disagree.
	 * `null` when the pending read was refused.
	 */
	waiting: Waiting | null;
	/**
	 * A refused pending read, beside the decisions that are still readable. The
	 * list is then short of the people who have never been decided about, which
	 * is exactly what the message says.
	 */
	waitingProblem: Explained | null;
	/** A refused display-name read, beside a list that is still true. */
	namesProblem: Explained | null;
}

export type Load = { ok: true; loaded: Loaded } | { ok: false; problem: Explained };

export type Decided = { ok: true; recorded: Recorded } | { ok: false; problem: Explained };

/** The Gateway's stable code, or `null` when the body carried none. */
function codeOf(error: unknown): string | null {
	const code = (error as { error?: unknown } | undefined)?.error;
	return typeof code === 'string' ? code : null;
}

/**
 * At most 200 contacts per display-name call — the Gateway *refuses* more
 * rather than truncating, *"so a short answer never passes for a complete
 * one"*. A real account produced eighteen conversations in a day and can
 * produce hundreds of contacts, so this bound is reached in practice.
 */
const NAMES_PER_CALL = 200;

/** Everything the screen draws, in one call. */
export async function load(): Promise<Load> {
	const [waitingAnswer, stateAnswer, registry] = await Promise.all([
		gateway.GET('/api/contacts/pending').catch(() => null),
		gateway.GET('/api/consent/state').catch(() => null),
		// The registry names an account when a kind has two (#272). A read
		// that fails takes away only what it answers: the rows still read,
		// unlabelled, and the screen says the registry could not be read.
		loadRegistry()
	]);

	if (stateAnswer === null || stateAnswer.data === undefined) {
		return {
			ok: false,
			problem:
				stateAnswer === null
					? explain('unreachable', null)
					: explain(troubleOf(stateAnswer), codeOf(stateAnswer.error))
		};
	}

	const waitingProblem =
		waitingAnswer === null
			? explain('unreachable', null)
			: waitingAnswer.data === undefined
				? explain(troubleOf(waitingAnswer), codeOf(waitingAnswer.error))
				: null;
	const pending = waitingAnswer?.data?.contacts ?? [];
	const entries = stateAnswer.data.entries;
	// Every contact the screen will draw a row for, which is the union of the
	// two lists — the same union `./model.ts` takes, computed here so the
	// name lookup does not miss a contact that has been decided about and has
	// not written since.
	const wanted = [
		...new Set([
			...pending.map((contact) => contact.contact),
			...entries
				.filter((entry) => entry.subject.type === 'contact')
				.map((entry) => entry.subject.id)
		])
	];

	const { names, problem } = await displayNames(wanted);
	const counted = waitingAnswer?.data;
	return {
		ok: true,
		loaded: {
			pending,
			entries,
			names,
			connections: registry.connections,
			connectionsProblem: registry.known ? null : explain(registry.trouble ?? 'unreachable', null),
			waiting:
				counted === undefined
					? null
					: { total: counted.total, connections: counted.connections, networks: counted.networks },
			waitingProblem,
			namesProblem: problem
		}
	};
}

/**
 * What these contacts are called, in as many calls as the Gateway's bound needs.
 *
 * A refusal of any chunk is reported once and the names that did arrive are
 * kept: a screen showing four names and two ids is more useful than one showing
 * six ids because the seventh call failed.
 */
async function displayNames(
	contacts: readonly string[]
): Promise<{ names: DisplayName[]; problem: Explained | null }> {
	const names: DisplayName[] = [];
	let problem: Explained | null = null;
	for (let at = 0; at < contacts.length; at += NAMES_PER_CALL) {
		const chunk = contacts.slice(at, at + NAMES_PER_CALL);
		const answer = await gateway
			.GET('/api/contacts/display-names', { params: { query: { contact: chunk } } })
			.catch(() => null);
		if (answer === null) {
			problem ??= explain('unreachable', null);
			continue;
		}
		if (answer.data === undefined) {
			problem ??= explain(troubleOf(answer), codeOf(answer.error));
			continue;
		}
		names.push(...answer.data.contacts);
	}
	return { names, problem };
}

/**
 * Records one decision about one contact on one connection.
 *
 * `state` is one of the three the contract has. There is no fourth argument for
 * "forget this decision": the journal is append-only, and the Gateway refuses
 * `unset` as a `new_state` because it is the absence of a decision rather than
 * one (ADR 0010). The scope is the connection's id (#270): the Gateway holds
 * consent per perimeter, and this module never names a network in its place.
 */
export async function decide(
	contact: string,
	connection: string,
	state: State,
	reason?: string
): Promise<Decided> {
	const answer = await gateway
		.POST('/api/consent/decisions', {
			body: {
				subject: { type: 'contact', id: contact },
				new_state: state,
				scope: { connections: [connection] },
				...(reason === undefined ? {} : { reason })
			}
		})
		.catch(() => null);
	if (answer === null) {
		return { ok: false, problem: explain('unreachable', null) };
	}
	if (answer.data !== undefined) {
		return { ok: true, recorded: answer.data };
	}
	return { ok: false, problem: explain(troubleOf(answer), codeOf(answer.error)) };
}
