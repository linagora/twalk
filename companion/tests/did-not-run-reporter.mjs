// Says *why* tests did not run (#148).
//
// The real-stack projects are chained by `dependencies` — `networks` →
// `dashboard` → `approvals` → `consent` → `portals` — because they share one
// Gateway, one consent journal and one login per bridge, so they cannot run
// beside each other. The cost is that one red journey in `networks` stops the
// forty that depend on it, and Playwright's own summary says only "42 did not
// run": a reader sees a red suite and has no way to tell that the failure is
// in a project their change never touched and that the tests which would have
// judged it never executed. That is the difference between a flaky suite and
// a misleading one, and it is the tooling's copy of the defect the product
// keeps closing — two situations behind one signal.
//
// So, at the end of a run, this reporter names each project whose tests did
// not run, how many, and which dependency's failures kept them from running.
// It prints nothing when everything ran: the common case is silence.

/** @typedef {import('@playwright/test/reporter').Reporter} Reporter */

/** @implements {Reporter} */
export default class DidNotRunReporter {
	/** @type {import('@playwright/test/reporter').Suite | undefined} */
	#suite;
	/** @type {import('@playwright/test/reporter').FullConfig | undefined} */
	#config;

	/**
	 * @param {import('@playwright/test/reporter').FullConfig} config
	 * @param {import('@playwright/test/reporter').Suite} suite
	 */
	onBegin(config, suite) {
		this.#config = config;
		this.#suite = suite;
	}

	onEnd() {
		if (this.#suite === undefined || this.#config === undefined) {
			return;
		}
		/** @type {Map<string, { didNotRun: number; failed: string[] }>} */
		const byProject = new Map();
		for (const test of this.#suite.allTests()) {
			const project = test.parent.project()?.name ?? '(no project)';
			const entry = byProject.get(project) ?? { didNotRun: 0, failed: [] };
			// Playwright's own rule for "did not run" (`runner/index.js`): a
			// skipped outcome that was not asked for — no result at all, or a
			// skip on a test expected to pass. `test.skip(condition)` at the top
			// of a file sets `expectedStatus` to skipped and is a skip, not this.
			const outcome = test.outcome();
			if (
				outcome === 'skipped' &&
				!test.results.some((result) => result.status === 'interrupted') &&
				(test.results.length === 0 || test.expectedStatus !== 'skipped')
			) {
				entry.didNotRun += 1;
			} else if (outcome === 'unexpected') {
				entry.failed.push(test.titlePath().slice(2).join(' › '));
			}
			byProject.set(project, entry);
		}
		const lines = [];
		for (const project of this.#config.projects) {
			const entry = byProject.get(project.name);
			if (entry === undefined || entry.didNotRun === 0) {
				continue;
			}
			const culprits = (project.dependencies ?? [])
				.map((name) => ({ name, failed: byProject.get(name)?.failed ?? [] }))
				.filter((dependency) => dependency.failed.length > 0);
			if (culprits.length === 0) {
				lines.push(
					`  ${entry.didNotRun} tests in project "${project.name}" did not run, and no dependency of it failed: the run was stopped or the project was filtered out.`
				);
				continue;
			}
			for (const culprit of culprits) {
				lines.push(
					`  ${entry.didNotRun} tests in project "${project.name}" did not run because project "${culprit.name}" failed, in:`
				);
				for (const title of culprit.failed) {
					lines.push(`      ✘ ${title}`);
				}
			}
		}
		if (lines.length === 0) {
			return;
		}
		process.stdout.write(
			'\n' +
				'  Tests that did not run, and why (#148): a project whose dependency went red is\n' +
				'  neither green nor red about the code under change — nothing judged it.\n' +
				lines.join('\n') +
				'\n\n'
		);
	}

	printsToStdio() {
		return true;
	}
}
