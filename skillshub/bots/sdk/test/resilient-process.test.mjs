import assert from "node:assert/strict";
import test from "node:test";

import { ResilientProcess } from "../dist/index.js";

const MINUTE = 60_000;
const DAY = 24 * 60 * MINUTE;

/** A process whose clock the test drives; records each re-auth decision. */
function harness(t, options = {}) {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  let now = 1_000_000;
  const purges = [];
  const resilient = new ResilientProcess({
    label: "[test-bot]",
    authFailureCodes: [2],
    now: () => now,
    onReauth: async (shouldPurge) => {
      purges.push(shouldPurge);
    },
    ...options,
  });
  return {
    resilient,
    purges,
    /** Run for `upMs`, exit with `code`, then let the scheduled re-auth fire. */
    async runThenExit(upMs, code) {
      resilient.recordStart();
      now += upMs;
      await resilient.handleExit(code, null, () => {});
      t.mock.timers.tick(60_000);
      await new Promise((resolve) => setImmediate(resolve));
    },
  };
}

test("auth failures days apart after healthy runs are never consecutive, so they never purge", async (t) => {
  const h = harness(t);
  for (let i = 0; i < 5; i++) {
    await h.runThenExit(2 * DAY, 2);
  }
  assert.deepEqual(h.purges, [false, false, false, false, false]);
  assert.equal(h.resilient.consecutiveFailures, 1);
});

test("rapid auth failures still escalate to a purge on the third", async (t) => {
  const h = harness(t);
  for (let i = 0; i < 3; i++) {
    await h.runThenExit(10_000, 2);
  }
  assert.deepEqual(h.purges, [false, false, true]);
});

test("a healthy run resets the streak, a short one does not", async (t) => {
  const h = harness(t, { healthyRunMs: 5 * MINUTE });
  await h.runThenExit(10_000, 2);
  await h.runThenExit(10_000, 2);
  await h.runThenExit(5 * MINUTE, 2);
  await h.runThenExit(10_000, 2);
  assert.deepEqual(h.purges, [false, false, false, false]);
  assert.equal(h.resilient.consecutiveFailures, 2);
});

/** A process driven exit by exit, observing exactly when each restart fires. */
function quickExitHarness(t, options = {}) {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  let now = 1_000_000;
  let restarts = 0;
  const escalations = [];
  const resilient = new ResilientProcess({
    label: "[test-bot]",
    authFailureCodes: [2],
    now: () => now,
    onPersistentFailure: (detail) => {
      escalations.push(detail);
    },
    ...options,
  });
  return {
    resilient,
    escalations,
    restarts: () => restarts,
    async exitAfter(upMs, code) {
      resilient.recordStart();
      now += upMs;
      await resilient.handleExit(code, null, () => {
        restarts++;
      });
    },
    /** The restart scheduled by the last exit fires after exactly `ms`. */
    assertRestartAfter(ms) {
      const before = restarts;
      t.mock.timers.tick(ms - 1);
      assert.equal(restarts, before, `restarted before ${ms}ms`);
      t.mock.timers.tick(1);
      assert.equal(restarts, before + 1, `no restart at ${ms}ms`);
    },
  };
}

test("quick non-auth exits back off exponentially up to the cap", async (t) => {
  const h = quickExitHarness(t, { maxRestartDelayMs: 20_000, maxQuickExits: 10 });
  for (const delay of [3_000, 6_000, 12_000, 20_000, 20_000]) {
    await h.exitAfter(10_000, 5);
    h.assertRestartAfter(delay);
  }
  assert.deepEqual(h.escalations, []);
});

test("quick non-auth exits escalate instead of restarting forever", async (t) => {
  const h = quickExitHarness(t, { maxQuickExits: 3 });
  await h.exitAfter(10_000, 5);
  h.assertRestartAfter(3_000);
  await h.exitAfter(10_000, 5);
  h.assertRestartAfter(6_000);
  await h.exitAfter(10_000, 5);
  assert.equal(h.escalations.length, 1);
  assert.match(h.escalations[0], /3 exits in a row/);
  assert.match(h.escalations[0], /code=5/);
  const restarts = h.restarts();
  t.mock.timers.tick(10 * MINUTE);
  assert.equal(h.restarts(), restarts, "an escalated failure is not restarted");
});

test("a healthy run resets the quick-exit backoff and its escalation", async (t) => {
  const h = quickExitHarness(t, { maxQuickExits: 3, healthyRunMs: 5 * MINUTE });
  await h.exitAfter(10_000, 5);
  h.assertRestartAfter(3_000);
  await h.exitAfter(10_000, 5);
  h.assertRestartAfter(6_000);
  await h.exitAfter(5 * MINUTE, 5);
  h.assertRestartAfter(3_000);
  await h.exitAfter(10_000, 5);
  h.assertRestartAfter(6_000);
  assert.deepEqual(h.escalations, []);
});

test("without an escalation hook, quick exits keep restarting at the capped backoff", async (t) => {
  const h = quickExitHarness(t, { maxQuickExits: 2, onPersistentFailure: undefined });
  for (const delay of [3_000, 6_000, 12_000]) {
    await h.exitAfter(10_000, 5);
    h.assertRestartAfter(delay);
  }
});
