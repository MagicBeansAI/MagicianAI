import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { APP_SUPPORTED_PUBLIC_CONTRACT_VERSION, APP_SUPPORTED_PUBLIC_OPERATIONS, type JsonValue } from "@magician/apps";
import { supportedRecipeFixtures } from "../src/recipe-fixtures.js";
import { applyReviewedUpdateFixture } from "../src/update-fixtures.js";

test("every provider-free Recipe fixture is the exact package-consumed bundle", async () => {
  const fixtures = supportedRecipeFixtures();
  for (const [name, expected] of Object.entries(fixtures)) {
    const source = JSON.parse(await readFile(
      new URL(
        import.meta.url.includes("/dist/tests/")
          ? `../../app/recipes/${name}.json`
          : `../app/recipes/${name}.json`,
        import.meta.url,
      ),
      "utf8",
    )) as unknown;
    assert.deepEqual(source, expected, `${name} recipe package member drifted from its executable fixture`);
  }
  assert.deepEqual(Object.keys(fixtures).sort(), ["get", "map", "parallel", "query", "sequence", "switch"]);
});

test("the checked-in update operation file transforms the exact base shape and refuses substitution", async () => {
  const operations = JSON.parse(await readFile(
    new URL(
      import.meta.url.includes("/dist/tests/")
        ? "../../../research-planner-update-v0.2.0/migrations/v0.1.1-to-v0.2.0.json"
        : "../../research-planner-update-v0.2.0/migrations/v0.1.1-to-v0.2.0.json",
      import.meta.url,
    ),
    "utf8",
  )) as JsonValue;
  const migrated = applyReviewedUpdateFixture({
    topic_id: "topic_demo",
    body: "Compare current official requirements.",
    status: "draft",
    revision_note: "pre-update note",
  }, operations);
  assert.deepEqual(migrated, {
    topic_id: "topic_demo",
    body: "Compare current official requirements.",
    status: "draft",
    reviewed_at: null,
  });
  const substituted = structuredClone(operations) as { field?: string }[];
  if (substituted[1] !== undefined) substituted[1].field = "body";
  assert.throws(
    () => applyReviewedUpdateFixture({
      topic_id: "topic_demo",
      body: "body",
      status: "draft",
      revision_note: null,
    }, substituted as JsonValue),
    /retire_field operation was substituted/,
  );
});

test("the package boundary names the generated v1.3 public inventory exactly", async () => {
  const boundary = JSON.parse(await readFile(
    new URL("../../PACKAGE-BOUNDARY.json", import.meta.url),
    "utf8",
  )) as {
    readonly public_contract_version: string;
    readonly public_operations: readonly string[];
  };
  assert.equal(boundary.public_contract_version, APP_SUPPORTED_PUBLIC_CONTRACT_VERSION);
  assert.deepEqual(
    boundary.public_operations,
    APP_SUPPORTED_PUBLIC_OPERATIONS.map((operation) => operation.operation_id),
  );
});
