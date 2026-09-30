import assert from "node:assert/strict";
import test from "node:test";

import type { RecoveryTextOwner } from "../src/recovery-store.js";
import { JsonRunRecoveryStore } from "../src/recovery-store.js";

function durableOwner() {
  let retained: string | undefined;
  const owner: RecoveryTextOwner = {
    read: () => retained,
    write: (value) => { retained = value; },
  };
  return { owner, replace: (value: string | undefined) => { retained = value; } };
}

test("a new process instance recovers the exact pre-dispatch intent and opaque run reference", () => {
  const durable = durableOwner();
  const input = {
    topic_id: "topic_1",
    query: "battery recycling",
    start_date: "2026-08-01",
    end_date: "2026-08-23",
  } as const;
  new JsonRunRecoveryStore(durable.owner).save({
    version: 1,
    installationId: "install_fixture",
    actionId: "build_plan",
    idempotencyKey: "action:fixture",
    input,
  });

  const afterRestart = new JsonRunRecoveryStore(durable.owner);
  const pending = afterRestart.load();
  assert.deepEqual(pending?.input, input);
  assert.equal(pending?.idempotencyKey, "action:fixture");
  if (pending === undefined) throw new Error("pending intent was not recovered");
  afterRestart.save({ ...pending, runRef: "run:app-action:fixture" });

  const afterResponseLoss = new JsonRunRecoveryStore(durable.owner);
  assert.equal(afterResponseLoss.load()?.runRef, "run:app-action:fixture");
  afterResponseLoss.clear();
  assert.equal(new JsonRunRecoveryStore(durable.owner).load(), undefined);
});

test("restart recovery refuses extra fields and identity substitution", () => {
  const durable = durableOwner();
  durable.replace(JSON.stringify({
    version: 1,
    installationId: "install_other",
    actionId: "build_plan",
    idempotencyKey: "action:fixture",
    input: {
      topic_id: "topic_1",
      query: "battery recycling",
      start_date: "2026-08-01",
      end_date: "2026-08-23",
    },
    private_route: "/internal/recover",
  }));
  assert.throws(() => new JsonRunRecoveryStore(durable.owner).load(), /malformed or substituted/);
});
