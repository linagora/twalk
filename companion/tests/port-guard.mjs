// Whether the ports this suite is about to use are free, or held by this
// suite's own server — and, when they are not, what holds them (#185).
//
// # The failure this exists to make impossible
//
// `tests/serve-like-gateway.mjs` listens on a fixed port and Playwright reuses
// whatever is already answering there. On 2026-09-19 that was a server a run
// had started twenty-eight hours earlier: the whole suite ran against the
// previous day's export and thirty-six tests failed as though the code were
// broken, `serving.spec.ts` included — *"a prerendered page resolves through
// its .html file: expected 200, received 404"*. Killing the process and running
// again gave 37 passed.
//
// The lesson is not "kill your servers". It is that **a failure whose cause is a
// stale server is indistinguishable from a regression**, which is the shape of
// defect this project has closed one instance of at a time: two situations
// sharing one signal. Worse here in two ways — it crosses runs, so the person
// who pays is not the person who left the server behind, and it can go the
// other way, where a stale-but-compatible build makes tests pass that should
// have failed.
//
// # What is checked, and why refusing is not the first move
//
// Three outcomes per port, and the middle one is the reason this is not simply
// "fail when the port is busy":
//
//   - **free** — Playwright will start a server. Nothing to say.
//   - **held by a server serving this very build** — legitimate reuse. The
//     server publishes the build it is serving (`/__twalk_test__/serving`,
//     `tests/build-id.mjs`), and the name is the export's own content, so
//     "serving this build" is a fact rather than a hope. Adopting it is what
//     makes `npm run test:e2e` twice in a row cheap.
//   - **held by anything else** — the suite stops, before a single assertion
//     runs, and says the port, what holds it (pid, command, when it started)
//     and what answered the probe. A stale `serve-like-gateway.mjs` names the
//     build it is serving and the run that left it; anything else is reported
//     as what it is.
//
// Refusing outright would turn one person's leftover into everybody else's hard
// stop, which the ticket warns about — so the refusal names `TWALK_TEST_PORT`,
// which moves this suite's three origins aside for a worktree that wants to run
// beside another.
//
// Run as a program, so `playwright.config.ts` can consult it synchronously,
// before the config returns and Playwright starts anything: `webServer` is
// launched **before** `globalSetup`, so a global setup would already be too
// late to stop a reuse.
//
//   node tests/port-guard.mjs <build id|-> --reuse|--exclusive <port>=<what> ...
//
// `--exclusive` is the real-stack run: each origin has a Companion Gateway and a
// Synapse of its own behind it, raised by the server this run starts, so a
// server already listening has none of that however right its build is. Reuse is
// not on offer there and the guard says which of the two reasons applies.
//
// Exit 0: every port is free or ours. Exit 1: the diagnosis is on stderr.

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import process from 'node:process';

import { SERVING_PATH } from './build-id.mjs';

/** How long a probe of an occupied port may take before it counts as no answer. */
const PROBE_TIMEOUT_MS = 2_000;

/**
 * What is on this port: `free`, `ours`, or a description of whatever else it is.
 */
async function inspect(port, expectedBuild) {
	const holder = holderOf(port);
	if (holder === null) {
		return { kind: 'free' };
	}
	const serving = await probe(port);
	const ours =
		serving !== null && serving.server === 'serve-like-gateway' && serving.build === expectedBuild;
	if (ours) {
		return { kind: 'ours', holder, serving };
	}
	return { kind: 'foreign', holder, serving };
}

/** `GET /__twalk_test__/serving`, or `null` when nothing useful came back. */
async function probe(port) {
	const abort = AbortSignal.timeout(PROBE_TIMEOUT_MS);
	try {
		const answer = await fetch(`http://127.0.0.1:${port}${SERVING_PATH}`, { signal: abort });
		if (!answer.ok) {
			return { status: answer.status };
		}
		return await answer.json();
	} catch {
		return null;
	}
}

/**
 * The process listening on `port`, as far as this host will say.
 *
 * `ss` first, `lsof` second, and `null` only when nothing is listening — a
 * holder this user cannot see is still a holder, so an unattributable listener
 * is reported as one rather than treated as a free port.
 */
function holderOf(port) {
	const listening = ss(port) ?? lsof(port);
	if (listening === null) {
		return null;
	}
	return { ...listening, startedAt: startedAt(listening.pid) };
}

function ss(port) {
	const out = run('ss', ['-ltnpH']);
	if (out === null) {
		return null;
	}
	for (const line of out.split('\n')) {
		// `LISTEN 0 511 127.0.0.1:18700 0.0.0.0:* users:(("node",pid=1234,fd=22))`
		const local = line.trim().split(/\s+/u)[3];
		if (local === undefined || !local.endsWith(`:${port}`)) {
			continue;
		}
		const named = /pid=(\d+)/u.exec(line);
		const pid = named === null ? null : Number(named[1]);
		return { pid, command: commandOf(pid), listener: local };
	}
	return null;
}

function lsof(port) {
	const out = run('lsof', ['-nP', '-iTCP:' + port, '-sTCP:LISTEN', '-Fpc']);
	if (out === null || out.trim() === '') {
		return null;
	}
	const named = /^p(\d+)/mu.exec(out);
	const pid = named === null ? null : Number(named[1]);
	return { pid, command: commandOf(pid), listener: `:${port}` };
}

function run(program, argv) {
	try {
		return execFileSync(program, argv, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] });
	} catch {
		return null;
	}
}

/** The whole command line, which is what tells a test server from a dev server. */
function commandOf(pid) {
	if (pid === null) {
		return null;
	}
	try {
		return readFileSync(`/proc/${pid}/cmdline`, 'utf8').split('\0').filter(Boolean).join(' ');
	} catch {
		const out = run('ps', ['-p', String(pid), '-o', 'args=']);
		return out === null ? null : out.trim();
	}
}

/**
 * When that process started — the fact that made #185 legible at all. *"Started
 * 2026-09-18 00:26, twenty-eight hours earlier"* is what turned thirty-six
 * mysterious failures into one sentence.
 */
function startedAt(pid) {
	if (pid === null) {
		return null;
	}
	const out = run('ps', ['-p', String(pid), '-o', 'lstart=']);
	return out === null || out.trim() === '' ? null : out.trim();
}

function describe(port, what, outcome) {
	const lines = [`  port ${port} (${what}) is held.`];
	const holder = outcome.holder;
	if (holder.pid === null) {
		lines.push('    by a process this user cannot see (no pid reported).');
	} else {
		lines.push(`    pid ${holder.pid}: ${holder.command ?? 'command unknown'}`);
		if (holder.startedAt !== null) {
			lines.push(`    started ${holder.startedAt}`);
		}
	}
	const serving = outcome.serving;
	if (serving === undefined) {
		// The caller is saying why itself; the holder's identity is all that is
		// wanted from here.
	} else if (serving === null) {
		lines.push(`    it does not answer ${SERVING_PATH}, so it is not this suite's server.`);
	} else if (serving.status !== undefined) {
		lines.push(
			`    it answers ${SERVING_PATH} with HTTP ${serving.status}, so it is not this suite's server.`
		);
	} else if (serving.server !== 'serve-like-gateway') {
		const called = serving.server === undefined ? 'nothing' : JSON.stringify(serving.server);
		lines.push(`    it calls itself ${called}, not serve-like-gateway, so it is not this`);
		lines.push("    suite's server.");
	} else {
		lines.push('    it is a serve-like-gateway, and it is serving a different build:');
		lines.push(`      it serves  ${serving.build} from ${serving.root ?? 'an unstated directory'}`);
		if (serving.startedAt !== undefined) {
			lines.push(`      started at ${serving.startedAt}`);
		}
	}
	return lines.join('\n');
}

async function main(argv) {
	const [expectedBuild, mode, ...ports] = argv;
	if (
		expectedBuild === undefined ||
		(mode !== '--reuse' && mode !== '--exclusive') ||
		ports.length === 0
	) {
		process.stderr.write(
			'usage: node tests/port-guard.mjs <build id|-> --reuse|--exclusive <port>=<what> ...\n'
		);
		return 2;
	}
	const exclusive = mode === '--exclusive';

	if (expectedBuild === '-') {
		// No export to compare against. Not this guard's refusal to make — the
		// suite serves `build/` and Playwright's own server will say so — but a
		// reused server cannot be proved either way, so say that much.
		process.stderr.write(
			'the Companion has not been built, so no server can be proved to be serving it:\n' +
				'  run `npm run build` (or `npm test`, which sequences it) before the e2e suite.\n'
		);
		return 1;
	}

	const wanted = ports.map((pair) => {
		const at = pair.indexOf('=');
		return { port: Number(pair.slice(0, at)), what: pair.slice(at + 1) };
	});

	const refusals = [];
	const reused = [];
	for (const { port, what } of wanted) {
		const outcome = await inspect(port, expectedBuild);
		if (outcome.kind === 'foreign') {
			refusals.push(describe(port, what, outcome));
		} else if (outcome.kind === 'ours' && exclusive) {
			refusals.push(
				`${describe(port, what, { holder: outcome.holder })}\n` +
					`    (it does serve build ${expectedBuild}, but this run brings up a Companion\n` +
					`    Gateway and a Synapse of its own behind each origin, and that server has\n` +
					`    none — so its answers would be a stub's where a real Gateway is expected.)`
			);
		} else if (outcome.kind === 'ours') {
			reused.push(`  port ${port} (${what}) already serves build ${expectedBuild}: reused.`);
		}
	}

	for (const line of reused) {
		// On stdout, because it is not a problem — and it is recorded, because a
		// run that reused a server should say so rather than leave a reader to
		// wonder which build the assertions met.
		process.stdout.write(`${line}\n`);
	}

	if (refusals.length === 0) {
		return 0;
	}
	process.stderr.write(
		`the Companion's e2e suite will not run: it cannot use the ports it needs.\n\n` +
			`${refusals.join('\n\n')}\n\n` +
			`This suite expects to serve build ${expectedBuild}. A server it did not start\n` +
			`is not this suite's server, and adopting one is how a run from yesterday came to\n` +
			`fail thirty-six tests as though the code were broken (#185).\n\n` +
			`Two ways out:\n` +
			`  - stop the process above, if it is a leftover;\n` +
			`  - or set TWALK_TEST_PORT to a free port, which moves all three of this suite's\n` +
			`    origins (it, +1 and +2) aside — which is what a second worktree wants.\n`
	);
	return 1;
}

process.exitCode = await main(process.argv.slice(2));
