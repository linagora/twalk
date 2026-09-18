// The perimeter of an activation, which is the whole of ADR 0013 in one
// function: what the assistant may read is what the user connected and ticked,
// and nothing that arrives afterwards.

import { describe, expect, it } from 'vitest';

import { defaultSelection, scopeFor, scopeOptions, type BridgeRow } from './scope';

function bridge(network: string, state: string | null): BridgeRow {
	return {
		bridge_id: `mautrix-${network}`,
		network,
		login: state === null ? null : { state }
	};
}

describe('scopeOptions', () => {
	it('offers the connected networks ticked, and Matrix unticked', () => {
		const options = scopeOptions([
			bridge('whatsapp', 'complete'),
			bridge('signal', 'awaiting_remote'),
			bridge('sms', null)
		]);
		expect(options).toEqual([
			{ network: 'whatsapp', proven: true, preselected: true },
			// Nothing the Gateway records says the Sensor was invited into any
			// room, so Matrix is offered and never assumed.
			{ network: 'matrix', proven: false, preselected: false }
		]);
	});

	it('preselects only what a bridge proves', () => {
		const options = scopeOptions([bridge('whatsapp', 'complete'), bridge('signal', 'complete')]);
		expect(defaultSelection(options)).toEqual(['whatsapp', 'signal']);
	});

	it('offers Matrix even on a deployment with no bridge at all', () => {
		expect(scopeOptions([])).toEqual([{ network: 'matrix', proven: false, preselected: false }]);
	});

	it('does not list one network twice when two bridges serve it', () => {
		const options = scopeOptions([bridge('sms', 'complete'), bridge('sms', 'complete')]);
		expect(options.filter((option) => option.network === 'sms')).toHaveLength(1);
	});
});

describe('scopeFor', () => {
	it('sorts and deduplicates, as the event id recipe does', () => {
		expect(scopeFor(['whatsapp', 'signal', 'whatsapp'])).toEqual(['signal', 'whatsapp']);
	});

	it('is empty for an empty selection, which is not a decision', () => {
		expect(scopeFor([])).toEqual([]);
	});
});
