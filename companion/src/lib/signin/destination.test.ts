// The one place a value from the address bar decides where a freshly
// signed-in user lands, so what it refuses is the test.

import { describe, expect, it } from 'vitest';

import { DEFAULT_DESTINATION, destinationFrom } from './destination';

const at = (query: string): URL => new URL(`https://twalk.example/signin${query}`);

describe('destinationFrom', () => {
	it('sends the user back where they were', () => {
		expect(destinationFrom(at('?next=%2Fnetworks%2Fsignal'))).toBe('/networks/signal');
		expect(destinationFrom(at('?next=%2Fdashboard%3Ftab%3Dfeed'))).toBe('/dashboard?tab=feed');
	});

	it('falls back to the dashboard when nothing says otherwise', () => {
		expect(destinationFrom(at(''))).toBe(DEFAULT_DESTINATION);
		expect(destinationFrom(at('?next='))).toBe(DEFAULT_DESTINATION);
	});

	it('refuses anything that leaves this origin', () => {
		// The whole reason this module exists: the navigation happens with a
		// session that was just created, so an accepted absolute URL hands a
		// signed-in user to whoever wrote the link.
		for (const hostile of [
			'https://evil.example/',
			'//evil.example/',
			'/\\evil.example',
			'javascript:alert(1)',
			'data:text/html,<script>',
			'networks/signal'
		]) {
			expect(destinationFrom(at(`?next=${encodeURIComponent(hostile)}`)), hostile).toBe(
				DEFAULT_DESTINATION
			);
		}
	});
});
