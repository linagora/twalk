import { describe, expect, it } from 'vitest';

import { networkNameKey, relativeTime } from './format';

const NOW = Date.parse('2026-09-18T12:00:00.000Z');

describe('relativeTime', () => {
	it('scales from seconds to days, in the locale in force', () => {
		expect(relativeTime('2026-09-18T11:59:50.000Z', NOW, 'en')).toContain('second');
		expect(relativeTime('2026-09-18T11:40:00.000Z', NOW, 'en')).toContain('minute');
		expect(relativeTime('2026-09-18T09:00:00.000Z', NOW, 'en')).toContain('hour');
		expect(relativeTime('2026-09-15T12:00:00.000Z', NOW, 'en')).toContain('day');
		expect(relativeTime('2026-09-18T11:40:00.000Z', NOW, 'fr')).toContain('minutes');
	});

	it('is null for a time nobody reported, rather than the epoch', () => {
		expect(relativeTime(null, NOW, 'en')).toBeNull();
		expect(relativeTime('not a date', NOW, 'en')).toBeNull();
	});
});

describe('networkNameKey', () => {
	it('reuses screen 3’s own names', () => {
		expect(networkNameKey('whatsapp')).toBe('network.whatsapp.name');
	});

	it('has none for a network the catalogue does not know', () => {
		// The Gateway's `network` enum can grow before this table does; the
		// screen then shows the raw value rather than an empty row.
		expect(networkNameKey('mastodon')).toBeNull();
	});
});
