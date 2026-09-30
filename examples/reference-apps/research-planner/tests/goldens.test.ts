import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import type { AppActionLaunchResponse, AppEntityChangeBatch, AppRunSnapshot } from "@magician/apps";
import { BUILD_PLAN_FORM, validateActionFormInput } from "../src/action-forms.js";
import {
  assertBuildPlanLaunchIdentity,
  assertBuildPlanSnapshotIdentity,
  assertContiguousChangeBatch,
} from "../src/client.js";
import type { BuildPlanOutput } from "../src/contracts.js";
import { REFUSED_FEATURES, REFUSED_RECIPE_NODE_KINDS, refuseRecipeNode } from "../src/refusals.js";
import { supportedRecipeFixtures } from "../src/recipe-fixtures.js";

async function fixture(path: string): Promise<unknown> {
  return JSON.parse(await readFile(new URL(`../../fixtures/${path}`, import.meta.url), "utf8"));
}

test("support matrix and refusal fixtures stay closed", async () => {
  const matrix = await fixture("golden/supported-scenarios.json") as {
    readonly supported: readonly string[];
    readonly recipe_ready_nodes: readonly string[];
  };
  assert.deepEqual(matrix.recipe_ready_nodes, ["query", "get", "map", "validate", "emit_value", "sequence", "parallel", "switch"]);
  assert.equal(matrix.supported.includes("agent_as_tool"), true);
  assert.equal(matrix.supported.includes("browser_observe_snapshot"), true);
  assert.equal(matrix.supported.includes("browser_reviewed_origin_navigate"), true);
  assert.equal(matrix.supported.includes("browser_fresh_observation_scroll"), true);
  assert.equal(matrix.supported.includes("browser_fresh_observation_click"), true);
  assert.equal(matrix.supported.includes("cross_app_composition"), true);
  assert.equal(matrix.supported.includes("bounded_composition_chain"), true);
  assert.equal(matrix.supported.includes("composition_cursor_subscription"), true);
  assert.equal(Object.keys(REFUSED_FEATURES).includes("agent_as_tool"), false);
  assert.equal(Object.keys(REFUSED_FEATURES).includes("cross_app_composition"), false);
  assert.equal(Object.keys(REFUSED_FEATURES).includes("p6_memory"), false);
  assert.equal(Object.keys(REFUSED_FEATURES).includes("update_disable_export"), false);
  assert.equal(Object.keys(REFUSED_FEATURES).includes("attention_destination"), true);
  assert.equal(Object.keys(REFUSED_FEATURES).includes("task_plan_destination"), true);
});

test("Query fixture carries the exact required own-store parameters", () => {
  const query = supportedRecipeFixtures().query;
  assert.notEqual(query, undefined);
  const text = JSON.stringify(query);
  assert.match(text, /\"entity\"/);
  assert.match(text, /\"limit\"/);
  assert.match(text, /\"select\"/);
  assert.doesNotMatch(text, /\"source_installation_id\"|\"purpose\"/);
});

test("malicious fixtures execute the closed consumer refusals", async () => {
  const action = await fixture("malicious/cross-installation-action-result.json") as AppActionLaunchResponse<BuildPlanOutput>;
  assert.throws(() => assertBuildPlanLaunchIdentity(action, "install_fixture"), /another installation/);
  const recovered: AppRunSnapshot<BuildPlanOutput> = {
    protocol_version: "1",
    run_handle: action.run_handle,
    status: "waiting",
    terminal: false,
    result_withheld: false,
  };
  assert.throws(
    () => assertBuildPlanSnapshotIdentity(recovered, "install_fixture", action.run_handle.run_ref),
    /another installation/,
  );

  const gap = await fixture("malicious/change-gap.json") as AppEntityChangeBatch;
  assert.throws(() => assertContiguousChangeBatch(gap, 7), /sequence gap/);

  const unknown = await fixture("malicious/unknown-action-field.json") as never;
  assert.throws(() => validateActionFormInput(BUILD_PLAN_FORM, unknown), /unknown action field/);

  const recipe = await fixture("malicious/recipe-unsupported-nodes.json") as {
    readonly refused: readonly string[];
    readonly must_not_fallback: boolean;
  };
  assert.equal(recipe.must_not_fallback, true);
  assert.deepEqual(recipe.refused, REFUSED_RECIPE_NODE_KINDS);
  for (const kind of REFUSED_RECIPE_NODE_KINDS) {
    assert.throws(() => refuseRecipeNode(kind), new RegExp(`unsupported_recipe_node:${kind}`));
  }
});
