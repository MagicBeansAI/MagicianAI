import assert from "node:assert/strict";
import test from "node:test";

import { renderRun } from "../src/rendering.js";

test("uncertain terminal runs never render result bytes", () => {
  const panel = renderRun({
    protocol_version: "1",
    run_handle: {
      protocol_version: "1",
      run_ref: "run:fixture",
      installation_id: "install_fixture",
      action_id: "build_plan",
    },
    status: "uncertain",
    terminal: true,
    result_withheld: true,
    result: {
      protocol_version: "1",
      action_id: "build_plan",
      run_ref: "run:fixture",
      status: "uncertain",
    },
  });
  assert.equal(panel.kind, "uncertain");
  assert.doesNotMatch(JSON.stringify(panel), /plan_id|output/);
});
