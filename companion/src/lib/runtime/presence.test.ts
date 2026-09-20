// The five things a screen can say about the runtime, from the one answer
// the Gateway gives (#177, #189). Values, not pixels: a component can stop
// rendering a sentence and start again; a state that never existed cannot be
// rendered by accident.

import { describe, expect, it } from 'vitest';

import { emptyApprovalsKey, runtimeCopyKey, runtimeOf, runtimeRuns, type RuntimeReading } from './presence';

function reading(presence: RuntimeReading['presence'], live: number, idle = 0): RuntimeReading {
	const persona = (id: string, liveness: 'live' | 'idle') => ({
		persona_id: id,
		consumer: `persona-${id}`,
		liveness,
		activation: 'active' as const,
		waiting_pulls: liveness === 'live' ? 1 : 0,
		ack_pending: 0
	});
	return {
		presence,
		personas: [
			...Array.from({ length: live }, (_, i) => persona(`live${i}`, 'live')),
			...Array.from({ length: idle }, (_, i) => persona(`idle${i}`, 'idle'))
		]
	};
}

describe('what the screens say about the runtime', () => {
	it('reads the three words the Gateway says, and counts what is hosted', () => {
		expect(runtimeOf({ data: reading('present', 2, 1) })).toEqual({ state: 'present', hosting: 2 });
		expect(runtimeOf({ data: reading('gone', 0, 1) })).toEqual({ state: 'gone', hosting: 0 });
		expect(runtimeOf({ data: reading('never', 0) })).toEqual({ state: 'never', hosting: 0 });
	});

	it('a Gateway with no bus is a deployment no runtime can run against — not "never"', () => {
		expect(runtimeOf({ error: { error: 'runtime_not_configured' } })).toEqual({
			state: 'no-bus',
			hosting: 0
		});
	});

	it('anything else is unknown, and claims neither side', () => {
		expect(runtimeOf(null).state).toBe('unknown');
		expect(runtimeOf({ error: { error: 'bus_unreachable' } }).state).toBe('unknown');
		expect(runtimeOf({ error: { error: 'unauthenticated' } }).state).toBe('unknown');
		expect(runtimeOf({ data: { presence: 'later' as never, personas: [] } }).state).toBe('unknown');
	});

	it('only a present runtime runs anything', () => {
		expect(runtimeRuns({ state: 'present', hosting: 1 })).toBe(true);
		for (const state of ['gone', 'never', 'no-bus', 'unknown'] as const) {
			expect(runtimeRuns({ state, hosting: 0 })).toBe(false);
		}
	});

	it('every state has a sentence of its own', () => {
		const keys = (['present', 'gone', 'never', 'no-bus', 'unknown'] as const).map(runtimeCopyKey);
		expect(new Set(keys).size).toBe(5);
		expect(keys).toContain('runtime.noBus');
	});
});

describe('the empty approvals screen', () => {
	it('says there is no runtime when there is none, whatever was activated', () => {
		for (const state of ['never', 'gone', 'no-bus'] as const) {
			expect(emptyApprovalsKey({ state, hosting: 0 }, true)).toBe('approvals.empty.noRuntime');
			expect(emptyApprovalsKey({ state, hosting: 0 }, false)).toBe('approvals.empty.noRuntime');
		}
	});

	it('separates "nothing activated" from "active and nothing proposed"', () => {
		expect(emptyApprovalsKey({ state: 'present', hosting: 1 }, false)).toBe(
			'approvals.empty.notActivated'
		);
		expect(emptyApprovalsKey({ state: 'present', hosting: 1 }, true)).toBe(
			'approvals.empty.nothingProposed'
		);
	});

	it('claims neither when it cannot read one of the two facts', () => {
		expect(emptyApprovalsKey({ state: 'unknown', hosting: 0 }, true)).toBe('approvals.empty.unknown');
		expect(emptyApprovalsKey({ state: 'present', hosting: 1 }, null)).toBe('approvals.empty.unknown');
		// Not activated is a fact about the user's decisions and needs no
		// runtime to be true.
		expect(emptyApprovalsKey({ state: 'unknown', hosting: 0 }, false)).toBe(
			'approvals.empty.notActivated'
		);
	});
});
