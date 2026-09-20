// The schedule a transient "unknown" is re-asked on (#148). Fake timers and a
// fake window: what is under test is when the read runs, not what it reads.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { rereadDelayMs, rereadWhileUnsettled, REREAD_DELAYS_MS, type RereadEnvironment } from './reread';

class Target {
	private readonly listeners = new Map<string, Set<() => void>>();
	visibilityState: DocumentVisibilityState = 'visible';
	addEventListener(type: string, listener: EventListenerOrEventListenerObject | null) {
		if (typeof listener === 'function') {
			const set = this.listeners.get(type) ?? new Set();
			set.add(listener as () => void);
			this.listeners.set(type, set);
		}
	}
	removeEventListener(type: string, listener: EventListenerOrEventListenerObject | null) {
		if (typeof listener === 'function') {
			this.listeners.get(type)?.delete(listener as () => void);
		}
	}
	fire(type: string) {
		for (const listener of this.listeners.get(type) ?? []) {
			listener();
		}
	}
	count(type: string) {
		return this.listeners.get(type)?.size ?? 0;
	}
}

function environment(): RereadEnvironment & { doc: Target; win: Target } {
	const doc = new Target();
	const win = new Target();
	return { document: doc, window: win, doc, win };
}

/** Lets the microtasks a `read().then(...)` chain needs run. */
async function settle() {
	for (let i = 0; i < 4; i += 1) {
		await Promise.resolve();
	}
}

describe('the re-read schedule', () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('backs off, then repeats the last delay', () => {
		expect(rereadDelayMs(1)).toBe(REREAD_DELAYS_MS[0]);
		expect(rereadDelayMs(2)).toBe(REREAD_DELAYS_MS[1]);
		expect(rereadDelayMs(4)).toBe(REREAD_DELAYS_MS[3]);
		expect(rereadDelayMs(40)).toBe(REREAD_DELAYS_MS[3]);
		expect(rereadDelayMs(0)).toBe(REREAD_DELAYS_MS[0]);
	});

	it('reads once and stops when the answer is known', async () => {
		const env = environment();
		const read = vi.fn(async () => {});
		const rereader = rereadWhileUnsettled(read, () => false, env);
		await settle();
		expect(read).toHaveBeenCalledTimes(1);
		await vi.advanceTimersByTimeAsync(120_000);
		expect(read).toHaveBeenCalledTimes(1);
		rereader.stop();
	});

	it('re-asks a transient answer on the backoff until it is known', async () => {
		const env = environment();
		let known = false;
		const read = vi.fn(async () => {});
		rereadWhileUnsettled(read, () => !known, env);
		await settle();
		expect(read).toHaveBeenCalledTimes(1);

		// 2 s: the second read. Still unknown.
		await vi.advanceTimersByTimeAsync(2_000);
		expect(read).toHaveBeenCalledTimes(2);
		// 5 s more: the third. Known now.
		known = true;
		await vi.advanceTimersByTimeAsync(5_000);
		expect(read).toHaveBeenCalledTimes(3);
		// Nothing after that.
		await vi.advanceTimersByTimeAsync(120_000);
		expect(read).toHaveBeenCalledTimes(3);
	});

	it('re-asks at once when the tab comes back into view or the browser comes online', async () => {
		const env = environment();
		const read = vi.fn(async () => {});
		rereadWhileUnsettled(read, () => true, env);
		await settle();
		expect(read).toHaveBeenCalledTimes(1);

		env.doc.fire('visibilitychange');
		await settle();
		expect(read).toHaveBeenCalledTimes(2);

		env.win.fire('online');
		await settle();
		expect(read).toHaveBeenCalledTimes(3);

		// A tab going *hidden* is not a reason to read.
		env.doc.visibilityState = 'hidden';
		env.doc.fire('visibilitychange');
		await settle();
		expect(read).toHaveBeenCalledTimes(3);
	});

	it('does not run two reads at once, and never after stop', async () => {
		const env = environment();
		let release: () => void = () => {};
		const read = vi.fn(
			() =>
				new Promise<void>((resolve) => {
					release = resolve;
				})
		);
		const rereader = rereadWhileUnsettled(read, () => true, env);
		await settle();
		expect(read).toHaveBeenCalledTimes(1);
		// Two events while the first read is still out: joined, not doubled.
		env.win.fire('online');
		env.doc.fire('visibilitychange');
		await settle();
		expect(read).toHaveBeenCalledTimes(1);
		release();
		await settle();

		rereader.stop();
		await vi.advanceTimersByTimeAsync(120_000);
		env.win.fire('online');
		await settle();
		expect(read).toHaveBeenCalledTimes(1);
		expect(env.doc.count('visibilitychange')).toBe(0);
		expect(env.win.count('online')).toBe(0);
	});
});
