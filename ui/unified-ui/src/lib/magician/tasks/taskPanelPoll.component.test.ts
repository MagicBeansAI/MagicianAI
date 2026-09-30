/**
 * The panel's poll: when it reads, what it reads, and what it does when a read
 * fails.
 *
 * **A `.component.test.ts` for a module with no component**, and the reason is
 * the environment rather than the subject: `sharedPoll`'s readable returns
 * immediately unless `browser` is true, so in the node lane — where
 * `$app/environment` says `false` — this poller never starts and every assertion
 * below would pass by never running. jsdom is where `browser` is true, and the
 * component lane is the one that has it.
 *
 * Fake timers throughout. The cadence is the subject, so waiting for it would
 * make these tests slow and the failures indistinguishable from flakes.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
	createTaskPanelPoll,
	panelPollCadence,
	samePanelPollTarget,
	type PanelPollTarget
} from './taskPanelPoll';

const IDLE_MS = 1_000;
const FAST_MS = 100;

interface Harness {
	poll: ReturnType<typeof createTaskPanelPoll<string>>;
	reads: PanelPollTarget[];
	snapshots: Array<{ target: PanelPollTarget; value: string; at: number }>;
	failures: Array<{ target: PanelPollTarget; message: string }>;
	fail(reason: string | null): void;
}

function harness(): Harness {
	const reads: PanelPollTarget[] = [];
	const snapshots: Harness['snapshots'] = [];
	const failures: Harness['failures'] = [];
	let failure: string | null = null;

	const poll = createTaskPanelPoll<string>({
		read: async (target) => {
			reads.push({ ...target });
			if (failure !== null) throw new Error(failure);
			return `${target.taskId}/${target.executionId ?? 'current'}#${reads.length}`;
		},
		onSnapshot: (target, value, at) => snapshots.push({ target, value, at }),
		onFailure: (target, message) => failures.push({ target, message }),
		idleMs: IDLE_MS,
		fastMs: FAST_MS,
		maxBackoffMs: 4_000
	});

	return {
		poll,
		reads,
		snapshots,
		failures,
		fail: (reason) => {
			failure = reason;
		}
	};
}

/** Let the scheduled read fire *and* its promise settle. */
async function tick(ms: number): Promise<void> {
	await vi.advanceTimersByTimeAsync(ms);
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe('panelPollCadence', () => {
	/**
	 * The whole point of the `off` row: a finished run's events cannot change, so
	 * every request made about one is waste — and on a panel left open it is waste
	 * that shows up as steady traffic against an idle backend.
	 */
	it('stops for terminal and archived states', () => {
		expect(panelPollCadence('completed')).toBe('off');
		expect(panelPollCadence('failed')).toBe('off');
		expect(panelPollCadence('cancelled')).toBe('off');
		expect(panelPollCadence('archived')).toBe('off');
	});

	it('runs fast for the two states that are doing work right now', () => {
		expect(panelPollCadence('running')).toBe('fast');
		expect(panelPollCadence('planning')).toBe('fast');
	});

	/**
	 * Not settled and not streaming. A paused run is suspended rather than
	 * finished — the ask that unblocks it can arrive at any moment — and a pending
	 * one can be started from another surface or by a schedule.
	 */
	it('keeps a slow watch on the states that can still change without the reader', () => {
		expect(panelPollCadence('paused')).toBe('idle');
		expect(panelPollCadence('pending')).toBe('idle');
		expect(panelPollCadence('ready')).toBe('idle');
		expect(panelPollCadence('deferred')).toBe('idle');
	});

	it('treats an absent status as no reason to poll', () => {
		expect(panelPollCadence(null)).toBe('off');
		expect(panelPollCadence(undefined)).toBe('off');
	});
});

describe('samePanelPollTarget', () => {
	it('separates two runs of one task, which is the pair the Run act renders', () => {
		expect(samePanelPollTarget({ taskId: 't', executionId: 'a' }, { taskId: 't', executionId: 'a' })).toBe(true);
		expect(samePanelPollTarget({ taskId: 't', executionId: 'a' }, { taskId: 't', executionId: 'b' })).toBe(false);
		expect(samePanelPollTarget({ taskId: 't', executionId: null }, { taskId: 't', executionId: 'a' })).toBe(false);
		expect(samePanelPollTarget(null, { taskId: 't', executionId: null })).toBe(false);
		expect(samePanelPollTarget(null, null)).toBe(true);
	});
});

describe('createTaskPanelPoll', () => {
	/**
	 * `off` means "this cannot change", not "this is already on screen". A reader
	 * opening a finished task's panel has aimed at something nobody has fetched, so
	 * the first read still happens — and then nothing else does, however long the
	 * panel stays open. Not polling and never reading are different things, and the
	 * cadence decides only the first.
	 */
	it('reads a settled target once and then never again', async () => {
		const h = harness();

		h.poll.aim({ taskId: 'done', executionId: null }, 'off');
		await tick(0);
		expect(h.reads).toEqual([{ taskId: 'done', executionId: null }]);
		expect(h.snapshots).toHaveLength(1);

		await tick(60_000);
		expect(h.reads).toHaveLength(1);
	});

	/**
	 * The run picker is at its most useful on a task that has stopped — a run that
	 * failed and was retried is the reason a task has more than one. A poll that had
	 * parked would leave the Run act describing the previous choice.
	 */
	it('reads the run the reader picked on a settled task', async () => {
		const h = harness();

		h.poll.aim({ taskId: 'retried', executionId: null }, 'off');
		await tick(0);
		h.poll.aim({ taskId: 'retried', executionId: 'ex_1' }, 'off');
		await tick(0);

		expect(h.reads.at(-1)).toEqual({ taskId: 'retried', executionId: 'ex_1' });
		expect(h.snapshots.at(-1)?.target.executionId).toBe('ex_1');
	});

	it('reads nothing at all once it is aimed at nothing', async () => {
		const h = harness();

		h.poll.aim(null, 'off');
		await tick(60_000);

		expect(h.reads).toHaveLength(0);
		expect(h.snapshots).toHaveLength(0);
	});

	it('reads the moment it is aimed, and keeps reading on the live cadence', async () => {
		const h = harness();

		h.poll.aim({ taskId: 'live', executionId: null }, 'fast');
		await tick(0);
		expect(h.reads).toHaveLength(1);

		await tick(FAST_MS * 3);
		expect(h.reads.length).toBeGreaterThanOrEqual(4);
		// Every read landed as a snapshot, tagged with what it was about.
		expect(h.snapshots).toHaveLength(h.reads.length);
		expect(h.snapshots.every((s) => s.target.taskId === 'live')).toBe(true);
	});

	it('reads a not-streaming task far more slowly than a live one', async () => {
		const h = harness();

		h.poll.aim({ taskId: 'queued', executionId: null }, 'idle');
		await tick(FAST_MS * 4);

		// One read, from being aimed — nothing on the fast cadence.
		expect(h.reads).toHaveLength(1);
		await tick(IDLE_MS);
		expect(h.reads).toHaveLength(2);
	});

	/**
	 * Design's second constraint, and the one a poll is most likely to break: a
	 * reader studying attempt 2 of 3 must not have the Run act snapped back to the
	 * current run under their cursor. The run is part of the target, so it is asked
	 * for **by name** on every tick rather than re-derived from the last reply.
	 */
	it('keeps asking for the run the reader chose, tick after tick', async () => {
		const h = harness();

		h.poll.aim({ taskId: 'retried', executionId: null }, 'fast');
		await tick(0);
		expect(h.reads[0].executionId).toBeNull();

		h.poll.aim({ taskId: 'retried', executionId: 'ex_2' }, 'fast');
		await tick(0);
		// Re-aiming reads immediately: the control the reader clicked has already
		// moved, and the act under it has to catch up.
		expect(h.reads.at(-1)?.executionId).toBe('ex_2');

		await tick(FAST_MS * 3);
		const afterChoice = h.reads.slice(1);
		expect(afterChoice.length).toBeGreaterThanOrEqual(4);
		expect(afterChoice.every((target) => target.executionId === 'ex_2')).toBe(true);
	});

	it('leaves no read running for the task the reader left', async () => {
		const h = harness();

		h.poll.aim({ taskId: 'first', executionId: null }, 'fast');
		await tick(FAST_MS * 2);
		const firstReads = h.reads.length;
		expect(firstReads).toBeGreaterThan(0);

		h.poll.aim({ taskId: 'second', executionId: null }, 'fast');
		await tick(FAST_MS * 4);

		expect(h.reads.slice(firstReads).every((target) => target.taskId === 'second')).toBe(true);
		expect(h.snapshots.slice(-1)[0].target.taskId).toBe('second');
	});

	it('parks when the task it is watching settles', async () => {
		const h = harness();

		h.poll.aim({ taskId: 'finishing', executionId: null }, 'fast');
		await tick(FAST_MS * 2);
		const whileLive = h.reads.length;

		h.poll.aim({ taskId: 'finishing', executionId: null }, 'off');
		await tick(60_000);

		expect(h.reads).toHaveLength(whileLive);
	});

	/**
	 * Design §6: never render stale state as current. `sharedPoll` keeps its last
	 * good value and says nothing, which is exactly the silence the panel must not
	 * have — so a failed read is reported, and the surface turns that into the
	 * staleness line.
	 */
	it('reports a failed read rather than going quiet, and takes it back on the next good one', async () => {
		const h = harness();

		h.poll.aim({ taskId: 'flaky', executionId: null }, 'fast');
		await tick(0);
		expect(h.snapshots).toHaveLength(1);

		h.fail('the network went away');
		await tick(FAST_MS * 2);
		expect(h.failures.length).toBeGreaterThan(0);
		expect(h.failures[0].message).toBe('the network went away');
		expect(h.failures[0].target.taskId).toBe('flaky');
		// The last good value is not replaced by the failure — nothing new landed.
		expect(h.snapshots).toHaveLength(1);

		h.fail(null);
		await tick(10_000);
		expect(h.snapshots.length).toBeGreaterThan(1);
	});

	/**
	 * The backoff is `sharedPoll`'s and it only runs if the failure is re-thrown
	 * into it. Reporting a failure and swallowing it would leave a dead backend
	 * being hammered at the live cadence.
	 */
	it('slows down while reads keep failing', async () => {
		const healthy = harness();
		healthy.poll.aim({ taskId: 'up', executionId: null }, 'fast');
		await tick(FAST_MS * 20);

		const broken = harness();
		broken.fail('down');
		broken.poll.aim({ taskId: 'gone', executionId: null }, 'fast');
		await tick(FAST_MS * 20);

		// Same window, same cadence asked for: a backend that is answering is read
		// on the live cadence, and one that is not costs a fraction of it.
		expect(healthy.reads.length).toBeGreaterThan(15);
		expect(broken.reads.length).toBeLessThan(healthy.reads.length / 4);
		// And every one of those failures was reported rather than swallowed.
		expect(broken.failures).toHaveLength(broken.reads.length);
	});

	/**
	 * The states that park the poll are exactly the ones whose reader reaches for
	 * Retry, so a refresh that only worked while polling would do nothing in the
	 * one case the control exists for.
	 */
	it('refreshes on demand while parked', async () => {
		const h = harness();

		h.poll.aim({ taskId: 'settled', executionId: 'ex_1' }, 'off');
		await tick(0);
		const onAim = h.reads.length;

		h.poll.refreshNow();
		await tick(0);

		expect(h.reads).toHaveLength(onAim + 1);
		expect(h.reads.at(-1)).toEqual({ taskId: 'settled', executionId: 'ex_1' });
		expect(h.snapshots.at(-1)?.target.executionId).toBe('ex_1');

		// Still parked afterwards: a read was asked for, not a cadence.
		await tick(60_000);
		expect(h.reads).toHaveLength(onAim + 1);
	});

	it('reads nothing on demand when it is aimed at nothing', async () => {
		const h = harness();

		h.poll.aim(null, 'fast');
		h.poll.refreshNow();
		await tick(1_000);

		expect(h.reads).toHaveLength(0);
	});

	it('stops for good', async () => {
		const h = harness();

		h.poll.aim({ taskId: 'live', executionId: null }, 'fast');
		await tick(FAST_MS);
		const before = h.reads.length;

		h.poll.stop();
		await tick(60_000);
		h.poll.refreshNow();
		await tick(1_000);

		expect(h.reads).toHaveLength(before);
	});
});
