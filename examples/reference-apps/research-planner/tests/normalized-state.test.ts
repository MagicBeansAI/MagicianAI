import assert from "node:assert/strict";
import test from "node:test";

import { NormalizedEntityState } from "../src/normalized-state.js";

test("optimistic create is reconciled only by a correlated receipt", () => {
  const state = new NormalizedEntityState();
  state.stageCreate("research_topic", "temporary_topic", { title: "Draft" }, "owner-request:fixture");
  state.commitCreate("owner-request:fixture", {
    receipt_id: "receipt:fixture",
    installation_id: "install_fixture",
    origin: { kind: "owner_api", session_ref: "session:fixture", request_ref: "owner-request:fixture" },
    mutation_key: `blake3:${"1".repeat(64)}`,
    batch_digest: `blake3:${"2".repeat(64)}`,
    committed_record_revisions: [{ entity: "research_topic", record_id: "topic_1", revision: 1 }],
    change_seq_range: { first: 1, last: 1 },
    committed_at: "2026-08-23T09:00:00Z",
  });
  assert.deepEqual(state.snapshot().map((record) => [record.recordId, record.revision]), [["topic_1", 1]]);
});

test("definite pre-dispatch failure can roll back without touching committed records", () => {
  const state = new NormalizedEntityState();
  state.stageCreate("research_topic", "temporary_topic", { title: "Draft" }, "owner-request:fixture");
  state.rollback("owner-request:fixture");
  assert.deepEqual(state.snapshot(), []);
});

test("a substituted owner request receipt cannot settle optimistic state", () => {
  const state = new NormalizedEntityState();
  state.stageCreate("research_topic", "temporary_topic", { title: "Draft" }, "owner-request:fixture");
  assert.throws(() => state.commitCreate("owner-request:fixture", {
    receipt_id: "receipt:substituted",
    installation_id: "install_fixture",
    origin: { kind: "owner_api", session_ref: "session:fixture", request_ref: "owner-request:other" },
    mutation_key: `blake3:${"1".repeat(64)}`,
    batch_digest: `blake3:${"2".repeat(64)}`,
    committed_record_revisions: [{ entity: "research_topic", record_id: "topic_1", revision: 1 }],
    change_seq_range: { first: 1, last: 1 },
    committed_at: "2026-08-23T09:00:00Z",
  }), /does not correlate/);
  assert.equal(state.snapshot()[0]?.pendingKey, "owner-request:fixture");
});
