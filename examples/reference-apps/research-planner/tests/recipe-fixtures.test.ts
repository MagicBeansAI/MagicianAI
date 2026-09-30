import assert from "node:assert/strict";
import test from "node:test";

import { executeSupportedRecipeFixture } from "../src/recipe-fixture-executor.js";
import { supportedRecipeFixtures } from "../src/recipe-fixtures.js";

test("external recipe fixtures contain only the eight implementation-ready node kinds", () => {
  const fixtures = supportedRecipeFixtures();
  const kinds = new Set<string>();
  for (const bundle of Object.values(fixtures)) {
    const recipe = bundle.recipe as { readonly nodes: Readonly<Record<string, { readonly node: { readonly kind: string } }>> };
    for (const node of Object.values(recipe.nodes)) kinds.add(node.node.kind);
  }
  assert.deepEqual([...kinds].sort(), ["emit_value", "get", "map", "parallel", "query", "sequence", "switch", "validate"]);
});

test("recipe schema references are content addressed and mutation/tool nodes are absent", () => {
  const text = JSON.stringify(supportedRecipeFixtures());
  assert.match(text, /workflow-schema:blake3:[0-9a-f]{64}/);
  for (const denied of ["retry", "call_tool", "invoke_action", "mutate", "agent_as_tool"]) {
    assert.doesNotMatch(text, new RegExp(`\\"kind\\":\\"${denied}\\"`));
  }
});

test("the exact provider-free fixtures execute all eight supported Recipe node kinds", async () => {
  const query = await executeSupportedRecipeFixture("query", {
    entity: "research_topic",
    limit: 2,
    select: ["title", "status"],
  }, {
    queryOwnEntity(input) {
      assert.deepEqual(input, {
        entity: "research_topic",
        limit: 2,
        select: ["title", "status"],
      });
      return [{
        entity: "research_topic",
        record_id: "topic_1",
        record_revision: 1,
        fields: { title: "Battery recycling", status: "active" },
      }];
    },
  });
  const exactProjection = [{
    entity: "research_topic",
    record_id: "topic_get_1",
    record_revision: 4,
    fields: { title: "Sealed topic" },
  }] as const;
  const get = await executeSupportedRecipeFixture("get", {
    entity: "research_topic",
    limit: 1,
    select: ["title"],
  }, {
    queryOwnEntity() { return exactProjection; },
  });
  const map = await executeSupportedRecipeFixture("map", { value: "mapped" }, {
    queryOwnEntity() { throw new Error("map must not query"); },
  });
  const sequence = await executeSupportedRecipeFixture("sequence", { value: "validated" }, {
    queryOwnEntity() { throw new Error("sequence must not query"); },
  });
  const parallel = await executeSupportedRecipeFixture("parallel", { value: "joined" }, {
    queryOwnEntity() { throw new Error("parallel must not query"); },
  });
  const active = await executeSupportedRecipeFixture("switch", { status: "active", value: "selected" }, {
    queryOwnEntity() { throw new Error("switch must not query"); },
  });
  const draft = await executeSupportedRecipeFixture("switch", { status: "draft", value: "emitted" }, {
    queryOwnEntity() { throw new Error("switch must not query"); },
  });
  assert.equal((query.value as readonly unknown[]).length, 1);
  assert.deepEqual(get.value, exactProjection);
  assert.deepEqual(map.value, { value: "mapped" });
  assert.deepEqual(sequence.value, { value: "validated" });
  assert.deepEqual(parallel.value, [{ value: "joined" }, { value: "joined" }]);
  assert.deepEqual(active.value, { value: "selected" });
  assert.deepEqual(draft.value, { value: "emitted" });
  const executed = new Set([
    ...query.executedNodeKinds,
    ...get.executedNodeKinds,
    ...map.executedNodeKinds,
    ...sequence.executedNodeKinds,
    ...parallel.executedNodeKinds,
    ...active.executedNodeKinds,
    ...draft.executedNodeKinds,
  ]);
  assert.deepEqual([...executed].sort(), ["emit_value", "get", "map", "parallel", "query", "sequence", "switch", "validate"]);
});

test("fixture execution refuses unknown input fields and substituted projections", async () => {
  await assert.rejects(executeSupportedRecipeFixture("sequence", {
    value: "valid",
    private_fallback: true,
  }, { queryOwnEntity() { return []; } }), /recipe_fixture_contract_drift:value input|exact/);
  await assert.rejects(executeSupportedRecipeFixture("query", {
    entity: "research_topic",
    limit: 1,
    select: ["title"],
  }, {
    queryOwnEntity() {
      return [{
        entity: "research_source",
        record_id: "source_1",
        record_revision: 1,
        fields: { title: "substituted" },
      }];
    },
  }), /substituted projection/);
});
