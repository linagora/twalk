// The settings screen's rules, against values (ticket #101): what the
// credential's three states are, what the form sends and when it refuses to,
// and that every refusal the Gateway documents has a sentence.

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

import {
	credentialState,
	formOf,
	PROBE_FAILURE_COPY,
	REFUSAL_COPY,
	requestOf,
	type ModelConfiguration
} from './model';

function configuration(over: Partial<ModelConfiguration> = {}): ModelConfiguration {
	return {
		configured: true,
		base_url: 'http://127.0.0.1:4000/v1',
		model: 'qwen',
		params: null,
		personas: {},
		updated_at: '2026-09-20T08:00:00Z',
		credential: {
			configured: true,
			source: 'companion',
			hint: 'k3y9',
			file: null,
			companion_credential_stored: true
		},
		...over
	};
}

describe('the credential', () => {
	it('is file-locked when an operator supplied a file, whatever the browser stored', () => {
		expect(
			credentialState(
				configuration({
					credential: {
						configured: true,
						source: 'file',
						hint: null,
						file: '/etc/twalk/llm.key',
						companion_credential_stored: true
					}
				})
			)
		).toEqual({ kind: 'file', path: '/etc/twalk/llm.key', companionStored: true });
	});

	it('is described, never carried, when the browser set it', () => {
		expect(credentialState(configuration())).toEqual({ kind: 'companion', hint: 'k3y9' });
		// And the form it fills has no credential in it to round-trip.
		expect(formOf(configuration()).credential).toBe('');
	});

	it('is none on a fresh deployment, which is a state and not an error', () => {
		const fresh = configuration({
			configured: false,
			base_url: null,
			model: null,
			updated_at: null,
			credential: {
				configured: false,
				source: null,
				hint: null,
				file: null,
				companion_credential_stored: false
			}
		});
		expect(credentialState(fresh)).toEqual({ kind: 'none' });
		expect(formOf(fresh)).toEqual({ baseUrl: '', model: '', credential: '', params: '' });
	});
});

describe('the request the form sends', () => {
	const form = { baseUrl: 'http://127.0.0.1:4000/v1', model: 'qwen', credential: '', params: '' };

	it('leaves the credential out when the field is empty, so a stored one survives a rename', () => {
		const built = requestOf(form);
		expect(built).toEqual({
			ok: true,
			request: { base_url: 'http://127.0.0.1:4000/v1', model: 'qwen', params: null }
		});
		expect(built.ok && 'credential' in built.request).toBe(false);
	});

	it('sends null only as the explicit act of forgetting it, and a string to replace it', () => {
		expect(requestOf(form, { forgetCredential: true })).toMatchObject({
			ok: true,
			request: { credential: null }
		});
		expect(requestOf({ ...form, credential: '  sk-abc ' })).toMatchObject({
			ok: true,
			request: { credential: 'sk-abc' }
		});
	});

	it('carries the provider parameters as an object, and refuses anything else', () => {
		expect(requestOf({ ...form, params: '{"temperature": 0.2, "top_p": null}' })).toMatchObject({
			ok: true,
			request: { params: { temperature: 0.2, top_p: null } }
		});
		expect(requestOf({ ...form, params: '[1, 2]' })).toEqual({
			ok: false,
			problems: [{ field: 'params', because: 'not_an_object' }]
		});
		expect(requestOf({ ...form, params: 'not json' })).toEqual({
			ok: false,
			problems: [{ field: 'params', because: 'not_an_object' }]
		});
	});

	it('names the field that is empty or not a URL, before the Gateway is asked', () => {
		expect(requestOf({ ...form, baseUrl: '' })).toEqual({
			ok: false,
			problems: [{ field: 'baseUrl', because: 'empty' }]
		});
		expect(requestOf({ ...form, baseUrl: '127.0.0.1:4000' })).toEqual({
			ok: false,
			problems: [{ field: 'baseUrl', because: 'not_a_url' }]
		});
		expect(requestOf({ ...form, model: '   ' })).toEqual({
			ok: false,
			problems: [{ field: 'model', because: 'empty' }]
		});
	});
});

describe('every refusal the Gateway describes has a sentence', () => {
	// Read off the description itself, so a code added to the Gateway fails
	// here rather than rendering as a blank — #100's rule for the approval
	// screen, applied to the settings routes.
	const description = readFileSync(
		new URL('../../../../companion-gateway/openapi.yaml', import.meta.url),
		'utf8'
	);

	function codesUnder(path: string): string[] {
		const start = description.indexOf(`\n  ${path}:\n`);
		expect(start, path).toBeGreaterThan(-1);
		const next = description.indexOf('\n  /', start + 1);
		const section = description.slice(start, next === -1 ? undefined : next);
		const codes = new Set<string>();
		for (const match of section.matchAll(/enum:\s*\[([^\]]*)\]/g)) {
			for (const code of match[1]?.split(',') ?? []) {
				const trimmed = code.trim();
				if (/^[a-z_]+$/.test(trimmed)) {
					codes.add(trimmed);
				}
			}
		}
		for (const match of section.matchAll(/enum:\s*\n\s*\[([^\]]*)\]/g)) {
			for (const code of match[1]?.split(',') ?? []) {
				const trimmed = code.trim();
				if (/^[a-z_]+$/.test(trimmed)) {
					codes.add(trimmed);
				}
			}
		}
		return [...codes];
	}

	it('for writing a model and setting a language', () => {
		const known = new Set([...Object.keys(REFUSAL_COPY), 'unauthenticated', 'sign_in_not_configured']);
		for (const path of ['/api/settings/model', '/api/settings/language']) {
			for (const code of codesUnder(path)) {
				expect(known.has(code), `${path}: ${code}`).toBe(true);
			}
		}
	});

	it('for the probe, whose four answers are four fixes', () => {
		const known = new Set([
			...Object.keys(PROBE_FAILURE_COPY),
			...Object.keys(REFUSAL_COPY),
			'unauthenticated',
			'sign_in_not_configured'
		]);
		for (const code of codesUnder('/api/settings/model/probe')) {
			expect(known.has(code), code).toBe(true);
		}
		expect(Object.keys(PROBE_FAILURE_COPY)).toEqual([
			'model_not_configured',
			'endpoint_unreachable',
			'endpoint_refused',
			'endpoint_not_compatible'
		]);
	});
});
