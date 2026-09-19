// What build the suite is about to test, as one string (#185).
//
// # Why a build has to have a name
//
// The Companion's e2e suite runs against `build/`, served by
// `tests/serve-like-gateway.mjs` on a fixed port. Playwright reuses a server
// that is already listening there, and on 2026-09-19 that server was one a run
// started **twenty-eight hours earlier**: every spec ran against the previous
// day's export, thirty-six of them failed, and the failures read exactly like a
// regression — `serving.spec.ts` reported a prerendered page as 404. The cause
// was not in the diff and the diff is where anybody would look.
//
// The fix is not a habit of killing servers. It is that the suite must be able
// to tell *its own* server from somebody else's, and a server is this suite's
// only if it is serving the build this run just made. So the build gets a name,
// computed here, and the server publishes the name it is serving
// (`/__twalk_test__/serving`). A server that cannot say the right one is not
// this suite's server, whatever is listening on the port.
//
// # Why the content and not a timestamp
//
// SvelteKit already writes `build/_app/version.json` with a build timestamp, and
// it would be a shorter fingerprint. It answers the wrong question. What the
// suite needs to know is whether the bytes on that port are the bytes it just
// built — so this hashes every file of the export, by path and by content. Two
// builds of the same tree get the same name, which is what makes reuse
// legitimate when a developer runs `npm run test:e2e` twice; one byte different
// anywhere gets a different name, which is what makes the stale server
// impossible to adopt.
//
// Synchronous on purpose: `playwright.config.ts` needs this value while it is
// still deciding what to run, before any server exists. Twelve megabytes over
// three hundred files, so it costs tens of milliseconds.

import { createHash } from 'node:crypto';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import process from 'node:process';

const here = fileURLToPath(new URL('.', import.meta.url));

/**
 * The export the suite serves, and which `TWALK_TEST_STATIC_DIR` can move.
 *
 * @returns {string}
 */
export function buildDir() {
	return resolve(process.env.TWALK_TEST_STATIC_DIR ?? join(here, '..', 'build'));
}

/**
 * The name of the build in `root`, or `null` when there is no build there.
 *
 * `null` rather than a throw: `npm run test:e2e` before `npm run build` is a
 * mistake worth a sentence of its own, and the caller is the one that can say
 * it. Every file's path and bytes go in, sorted, so the name is the export's
 * content and nothing about when or where it was produced.
 *
 * @param {string} [root]
 * @returns {string | null}
 */
export function buildId(root = buildDir()) {
	let files;
	try {
		files = filesUnder(root).sort();
	} catch {
		return null;
	}
	if (files.length === 0) {
		return null;
	}
	const digest = createHash('sha256');
	for (const file of files) {
		// The path is hashed too: a file renamed is a different export, and two
		// files whose contents were swapped must not agree with themselves.
		digest.update(relative(root, file).split(sep).join('/'));
		digest.update('\0');
		digest.update(readFileSync(file));
		digest.update('\0');
	}
	// Short enough to read in a failure message, long enough not to collide.
	return digest.digest('hex').slice(0, 16);
}

/**
 * @param {string} root
 * @returns {string[]}
 */
function filesUnder(root) {
	/** @type {string[]} */
	const found = [];
	for (const entry of readdirSync(root, { withFileTypes: true })) {
		const path = join(root, entry.name);
		if (entry.isDirectory()) {
			found.push(...filesUnder(path));
		} else if (entry.isFile()) {
			found.push(path);
		} else if (entry.isSymbolicLink() && statSync(path).isFile()) {
			found.push(path);
		}
	}
	return found;
}

/** The path a server publishes its identity at. Not part of the Gateway's surface. */
export const SERVING_PATH = '/__twalk_test__/serving';
