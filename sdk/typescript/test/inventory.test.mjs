import assert from "node:assert/strict";
import test from "node:test";
import { APP_SUPPORTED_PUBLIC_OPERATIONS } from "../dist/index.js";

test("generated inventory contains only the supported-public eight", () => {
  assert.deepEqual(
    APP_SUPPORTED_PUBLIC_OPERATIONS.map(({ operation_id, method, path }) => ({ operation_id, method, path })),
    [
      { operation_id: "contract_capabilities", method: "GET", path: "/contract-capabilities" },
      { operation_id: "query_data", method: "POST", path: "/installations/{installation_id}/data/query" },
      { operation_id: "mutate_data", method: "POST", path: "/installations/{installation_id}/data/mutations" },
      { operation_id: "launch_action", method: "POST", path: "/installations/{installation_id}/actions/{action_id}/runs" },
      { operation_id: "get_action_run", method: "GET", path: "/action-runs/{run_ref}" },
      { operation_id: "compose_action_run", method: "POST", path: "/action-runs/{run_ref}/compositions" },
      { operation_id: "cancel_action_run", method: "POST", path: "/action-runs/{run_ref}/cancel" },
      { operation_id: "read_entity_changes", method: "GET", path: "/installations/{installation_id}/entity-changes" },
    ],
  );
  const serialized = JSON.stringify(APP_SUPPORTED_PUBLIC_OPERATIONS);
  for (const forbidden of ["/launch", "custom-surface", "/review", "/approve", "provider", "mcp", "task_id"]) {
    assert.equal(serialized.includes(forbidden), false, forbidden);
  }
});
