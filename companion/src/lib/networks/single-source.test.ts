// One question, one answer: nothing in the Companion decides whether a network
// is connected except `connection.ts`.
//
// This is an *absence* test, which this project writes deliberately
// (CONTRIBUTING.md): the defect it guards has now appeared three times — the
// networks picker and the management screen (#108), the dashboard's rows
// (#108), and the persona screen's activation perimeter (#142) — and each time
// the shape was identical. A screen read `ConfiguredBridge.login`, the login
// **process** the Gateway holds in memory for at most thirty minutes, and
// treated it as the link. So starting a login cleared the badge, cancelling one
// cleared it, finishing one cleared it, and restarting the Gateway cleared it,
// all while the bridge held a live WhatsApp session throughout.
//
// The third instance survived the second because the fix was scoped by
// directory and the defect is defined by source of truth: the scope of "stop
// reading X, read Y" is every reader of X, which is a question grep answers.
// So this test is the grep, run by the suite rather than by whoever remembers.
//
// What it forbids is reading a login process's `state`. What it does not forbid
// is the dashboard's activity feed, which is a timeline of login processes and
// is the one place where a login *is* the subject — it is listed by name
// below, with its reason.

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

const SOURCE = fileURLToPath(new URL('../..', import.meta.url));

/**
 * Reading `login.state` — a login process's own progress — is legitimate in
 * exactly these places, and in each one the login is what the screen is about
 * rather than the answer to "is this network connected".
 */
const ABOUT_THE_LOGIN_ITSELF = [
	// The QR journey's own view model: which step of the login to draw.
	join('lib', 'networks', 'login-view.ts'),
	// The dashboard's activity feed: a timeline of things that happened, where
	// a login starting and finishing are the events.
	join('lib', 'dashboard', 'model.ts')
];

function sourceFiles(directory: string): string[] {
	const found: string[] = [];
	for (const entry of readdirSync(directory)) {
		const path = join(directory, entry);
		if (statSync(path).isDirectory()) {
			found.push(...sourceFiles(path));
			continue;
		}
		if (/\.(ts|svelte)$/.test(entry) && !/\.test\.ts$/.test(entry)) {
			found.push(path);
		}
	}
	return found;
}

describe('the single source of a network’s connected state', () => {
	it('is read by nobody through a login process', () => {
		const offenders: string[] = [];
		for (const path of sourceFiles(SOURCE)) {
			const relative = path.slice(SOURCE.length);
			if (ABOUT_THE_LOGIN_ITSELF.some((allowed) => relative.endsWith(allowed))) {
				continue;
			}
			const source = readFileSync(path, 'utf8');
			// Comments are where this defect is explained, and every module
			// that explains it names it. Only code counts.
			const code = source
				.replace(/\/\*[\s\S]*?\*\//g, '')
				.replace(/<!--[\s\S]*?-->/g, '')
				.replace(/^\s*\/\/.*$/gm, '');
			if (/\blogin\s*\??\.\s*state\b/.test(code)) {
				offenders.push(relative);
			}
		}
		expect(offenders, 'these decide from a login process, not from the bridge').toEqual([]);
	});

	it('scans the files it thinks it does', () => {
		// A traversal that silently found nothing would pass the assertion
		// above for ever.
		const files = sourceFiles(SOURCE);
		expect(files.length).toBeGreaterThan(40);
		expect(files.some((path) => path.endsWith(join('lib', 'personas', 'scope.ts')))).toBe(true);
		expect(files.some((path) => path.endsWith(join('routes', 'networks', '+page.svelte')))).toBe(
			true
		);
	});
});
