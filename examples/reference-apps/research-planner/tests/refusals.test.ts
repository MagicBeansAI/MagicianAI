import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { REFUSED_FEATURES, refuse } from "../src/refusals.js";

test("deferred features remain explicit refusals rather than approximation paths", () => {
  for (const feature of Object.keys(REFUSED_FEATURES) as (keyof typeof REFUSED_FEATURES)[]) {
    assert.throws(() => refuse(feature), new RegExp(`unsupported_public_contract:${feature}`));
  }
});

test("unsupported Recipe and unavailable physical owners stay closed", async () => {
  const fixture = JSON.parse(await readFile(
    new URL("../../fixtures/refused/unsupported-public-paths.json", import.meta.url),
    "utf8",
  )) as { readonly features: readonly (keyof typeof REFUSED_FEATURES)[]; readonly must_not_fallback: boolean };
  assert.equal(fixture.must_not_fallback, true);
  assert.deepEqual(fixture.features, [
    "recipe_retry",
    "recipe_effects",
    "unavailable_physical_owner",
    "attention_destination",
    "task_plan_destination",
  ]);
  for (const feature of fixture.features) {
    assert.throws(() => refuse(feature), new RegExp(`unsupported_public_contract:${feature}`));
  }
});
