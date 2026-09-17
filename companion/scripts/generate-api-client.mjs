#!/usr/bin/env node
// Generates the Companion's typed view of the Gateway from
// `companion-gateway/openapi.yaml` — the Gateway's own source of truth, which
// the running binary embeds and serves at `/openapi.yaml` (ticket #63).
//
// Two files come out, and neither is ever edited by hand:
//
//   src/lib/api/schema.d.ts       every path, operation, parameter, request
//                                 body and response of the Gateway, as
//                                 TypeScript. `openapi-fetch` turns it into a
//                                 client where `client.GET('/api/devices')` is
//                                 checked against the description: a path that
//                                 does not exist, or a response member nobody
//                                 described, is a compile error rather than a
//                                 runtime surprise.
//
//   src/lib/api/gateway-version.ts  the description's `info.version`. The
//                                 Gateway's conformance test asserts it equals
//                                 what `GET /health` reports, so this is the
//                                 client half of the version handshake: the
//                                 version of the Gateway this build was
//                                 generated against.
//
// Both are committed, because the image's Node stage builds `companion/` alone
// (`deploy/docker-compose/companion-gateway.Dockerfile` copies no sibling
// directory) and so cannot regenerate them. Committed generated code drifts,
// which is what `--check` is for: it regenerates into memory and fails when
// the result differs from what is on disk. `npm test` runs it first.
//
// Usage:
//   node scripts/generate-api-client.mjs            write the files
//   node scripts/generate-api-client.mjs --check    fail if they are stale

import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { relative } from 'node:path';
import process from 'node:process';

import openapiTS, { astToString } from 'openapi-typescript';
import { parse as parseYaml } from 'yaml';

const here = new URL('.', import.meta.url);
const description = new URL('../../companion-gateway/openapi.yaml', here);
const schemaFile = new URL('../src/lib/api/schema.d.ts', here);
const versionFile = new URL('../src/lib/api/gateway-version.ts', here);

const banner = (script) => `// Generated from companion-gateway/openapi.yaml by ${script}.
// Do not edit: run \`npm run api:generate\`. \`npm run api:check\` fails when
// this file no longer matches the description.
`;

async function schema() {
	const ast = await openapiTS(description, {
		// The Gateway answers `additionalProperties: false` objects, and a
		// member it does not describe is a bug on its side (its conformance
		// test says so). Reading the description exactly is the point.
		alphabetize: true,
		emptyObjectsUnknown: true
	});
	return `${banner('scripts/generate-api-client.mjs')}\n${astToString(ast)}`;
}

async function version() {
	const document = parseYaml(await readFile(description, 'utf8'));
	const value = document?.info?.version;
	if (typeof value !== 'string' || value.length === 0) {
		throw new Error('companion-gateway/openapi.yaml has no info.version');
	}
	return `${banner('scripts/generate-api-client.mjs')}
/**
 * The Gateway version this build of the Companion was generated against.
 *
 * \`GET /health\` reports the Gateway's own version, and the Gateway's
 * conformance test asserts it equals the description's \`info.version\` — the
 * value below. So a difference between the two at runtime means exactly one
 * thing: this shell outlived a Gateway upgrade, which is what an installed PWA
 * with a service worker makes possible. See \`src/lib/version/handshake.ts\`.
 */
export const EXPECTED_GATEWAY_VERSION = ${JSON.stringify(value)};
`;
}

const wanted = new Map([
	[schemaFile, await schema()],
	[versionFile, await version()]
]);

const checking = process.argv.includes('--check');
const stale = [];

for (const [file, contents] of wanted) {
	const path = relative(process.cwd(), fileURLToPath(file));
	const current = await readFile(file, 'utf8').catch(() => null);
	if (current === contents) {
		continue;
	}
	if (checking) {
		stale.push(path);
		continue;
	}
	await writeFile(file, contents);
	console.log(`${current === null ? 'wrote' : 'updated'} ${path}`);
}

if (stale.length > 0) {
	console.error(
		`The generated Gateway client is stale:\n` +
			stale.map((path) => `  ${path}`).join('\n') +
			`\n\ncompanion-gateway/openapi.yaml has moved since these were generated.\n` +
			`Run \`npm run api:generate\` in companion/ and commit the result.\n`
	);
	process.exit(1);
}

if (checking) {
	console.log('The generated Gateway client matches companion-gateway/openapi.yaml.');
} else if (wanted.size > 0) {
	console.log('Done.');
}
