import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

import { defineRecipeBundle, recipeNode } from "../dist/index.js";

const valueRef = "workflow-schema:blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const arrayRef = "workflow-schema:blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

test("mechanical sync recipes retain typed input bindings and conditionally bounded source reads", () => {
  const learning = JSON.parse(readFileSync(new URL("../../../magician_data_v3/system/learning/app/recipes/sync-queue.json", import.meta.url), "utf8"));
  const queue = defineRecipeBundle(learning).recipe.nodes.root.node.declaration;
  assert.deepEqual(queue.input_parameters, { limit: "limit", state: "state" });
  assert.equal(queue.targets[0].fields.source_agent_id.allow_null, true);
  const maps = JSON.parse(readFileSync(new URL("../../../magician_data_v3/system/thinking_map/app/recipes/sync-maps.json", import.meta.url), "utf8"));
  const bundle = defineRecipeBundle(maps);
  const root = bundle.recipe.nodes.root;
  assert.equal(root.resources.max_tool_calls, 2);
  assert.equal(root.node.declaration.sources.snapshot.when_input_present, "map_id");
  assert.deepEqual(root.node.declaration.sources.snapshot.input_parameters, { map_id: "map_id" });
  assert.deepEqual(root.node.declaration.targets[1].fields.snapshot, { kind: "source_document", max_bytes: 262144 });
  const undercounted = structuredClone(maps);
  undercounted.recipe.ceilings.max_tool_calls = 1;
  assert.throws(() => defineRecipeBundle(undercounted), /tool-call ceiling/);
});

test("Town Square uses the shared bounded reconciliation builder without a model node", () => {
  const shipped = JSON.parse(readFileSync(new URL("../../../magician_data_v3/system/town_square/app/recipes/sync-roster.json", import.meta.url), "utf8"));
  const bundle = defineRecipeBundle(shipped);
  assert.equal(bundle.recipe.nodes.root.node.kind, "reconcile");
  assert.equal(bundle.recipe.nodes.root.effect.uncertainty, "receipt_bound");
  const declaration = bundle.recipe.nodes.root.node.declaration;
  assert.deepEqual(declaration.targets.map((target) => target.entity), ["member", "self_state", "turn_cursor"]);
  assert.deepEqual(declaration.targets[1].update_fields, []);
  assert.equal(declaration.targets[2].seeds[0].record_id, "singleton");
  const widened = structuredClone(shipped);
  widened.recipe.nodes.extra = structuredClone(widened.recipe.nodes.root);
  assert.throws(() => defineRecipeBundle(widened), /one terminal/);
  const unbound = structuredClone(shipped);
  delete unbound.recipe.nodes.root.authority.primitive_ref;
  assert.throws(() => defineRecipeBundle(unbound), /reference/);
  const multipleStorePages = structuredClone(shipped);
  multipleStorePages.recipe.nodes.root.node.declaration.max_existing_rows = 1000;
  assert.equal(defineRecipeBundle(multipleStorePages).recipe.nodes.root.node.declaration.max_existing_rows, 1000);
  const oversizedRunBudget = structuredClone(shipped);
  oversizedRunBudget.recipe.nodes.root.node.declaration.max_existing_rows = 65_536;
  assert.throws(() => defineRecipeBundle(oversizedRunBudget), /bounded rows/);
});

function singleNodeBundle(node, inputSchemaRef, output) {
  return {
    version: "v1",
    schemas: { value: { version: "v1" } },
    recipe: {
      version: "v1",
      input_schema_ref: inputSchemaRef,
      output,
      root: "root",
      nodes: { root: node },
      ceilings: {
        max_nodes: 1, max_edges: 1, max_depth: 1, max_fan_out: 1,
        max_parallelism: 1, max_payload_bytes: 8_192, max_active_millis: 1_000,
        max_cost_microusd: 0, max_tool_calls: 0,
      },
      evolution: { topology_revision: 1, migration: "recompile_required" },
    },
  };
}

test("supported Recipe builders emit the exact closed Parallel shape", () => {
  const left = recipeNode.validate(valueRef);
  const right = recipeNode.emitValue(valueRef);
  const root = recipeNode.parallel(
    { omega: "right", alpha: "left" },
    valueRef,
    { kind: "typed_value", schema_ref: arrayRef, authority: "derived" },
  );
  const bundle = defineRecipeBundle({
    version: "v1",
    schemas: { output: { version: "v1" }, value: { version: "v1" } },
    recipe: {
      version: "v1",
      input_schema_ref: valueRef,
      output: { kind: "typed_value", schema_ref: arrayRef, authority: "derived" },
      root: "root",
      nodes: { right, root, left },
      ceilings: {
        max_nodes: 3,
        max_edges: 2,
        max_depth: 2,
        max_fan_out: 2,
        max_parallelism: 2,
        max_payload_bytes: 32_768,
        max_active_millis: 3_000,
        max_cost_microusd: 0,
        max_tool_calls: 0,
      },
      evolution: { topology_revision: 1, migration: "recompile_required" },
    },
  });

  assert.deepEqual(Object.keys(bundle.recipe.nodes), ["left", "right", "root"]);
  assert.deepEqual(bundle.recipe.nodes.root.node.branches, { alpha: "left", omega: "right" });
  assert.equal(bundle.recipe.nodes.root.resources.max_parallelism, 2);
});

test("Recipe builders expose only owned nodes and keep caller authority out of the API", () => {
  assert.deepEqual(Object.keys(recipeNode).sort(), ["contextualRound", "emitValue", "get", "map", "parallel", "query", "reconcile", "sequence", "storeTransaction", "switch", "validate"]);
  assert.throws(
    () => recipeNode.parallel(
      { only: "child" },
      valueRef,
      { kind: "typed_value", schema_ref: arrayRef, authority: "derived" },
    ),
    /at least two/,
  );
  assert.throws(
    () => recipeNode.query("entry", valueRef, arrayRef, []),
    /reviewed grant/,
  );
  assert.throws(
    () => recipeNode.get("entry", arrayRef, []),
    /reviewed grant/,
  );
  assert.throws(
    () => recipeNode.map(valueRef, valueRef, "blake3:bad", [], {}),
    /BLAKE3 digest/,
  );
  assert.throws(
    () => recipeNode.map(
      valueRef,
      valueRef,
      `blake3:${"a".repeat(64)}`,
      [{ kind: "convert", source: "value", target: "value", conversion: "text_to_timestamp" }],
    ),
    /unsupported or partial mapping/,
  );

  const get = recipeNode.get("entry", arrayRef, ["grant:records"]);
  assert.equal(get.input_schema_ref, get.output.schema_ref);
  assert.equal(get.node.kind, "get");
  const map = recipeNode.map(
    valueRef,
    valueRef,
    `blake3:${"a".repeat(64)}`,
    [{ kind: "select", source: "value", target: "value" }],
  );
  assert.deepEqual(map.node, {
    kind: "map",
    mapping_digest: `blake3:${"a".repeat(64)}`,
    operations: [{ kind: "select", source: "value", target: "value" }],
  });
  const enumMap = recipeNode.map(
    valueRef,
    valueRef,
    `blake3:${"b".repeat(64)}`,
    [{ kind: "map_enum", source: "status", target: "status", values: { open: "active", done: "closed" } }],
  );
  assert.deepEqual(enumMap.node.operations[0].values, { done: "closed", open: "active" });
  assert.equal(
    defineRecipeBundle(singleNodeBundle(get, arrayRef, get.output)).recipe.nodes.root.node.kind,
    "get",
  );
  assert.equal(
    defineRecipeBundle(singleNodeBundle(map, valueRef, map.output)).recipe.nodes.root.node.kind,
    "map",
  );
  const substitutedMap = structuredClone(map);
  substitutedMap.node.operations = [];
  assert.throws(
    () => defineRecipeBundle(singleNodeBundle(substitutedMap, valueRef, substitutedMap.output)),
    /bounded immutable body/,
  );

  const leaf = recipeNode.validate(valueRef);
  const inner = recipeNode.parallel(
    { left: "a", right: "b" },
    valueRef,
    { kind: "typed_value", schema_ref: arrayRef, authority: "derived" },
  );
  const outer = recipeNode.parallel(
    { inner: "inner", leaf: "c" },
    valueRef,
    { kind: "typed_value", schema_ref: arrayRef, authority: "derived" },
  );
  assert.throws(() => defineRecipeBundle({
    version: "v1",
    schemas: { output: { version: "v1" }, value: { version: "v1" } },
    recipe: {
      version: "v1",
      input_schema_ref: valueRef,
      output: { kind: "typed_value", schema_ref: arrayRef, authority: "derived" },
      root: "root",
      nodes: { root: outer, inner, a: leaf, b: leaf, c: leaf },
      ceilings: {
        max_nodes: 5, max_edges: 4, max_depth: 3, max_fan_out: 2,
        max_parallelism: 2, max_payload_bytes: 32_768, max_active_millis: 5_000,
        max_cost_microusd: 0, max_tool_calls: 0,
      },
      evolution: { topology_revision: 1, migration: "recompile_required" },
    },
  }), /nested Parallel/);
});

test("contextual round builder admits the shipped multi-agent recipe with aggregate budgets", () => {
  const shipped = JSON.parse(readFileSync(new URL("../../../magician_data_v3/system/town_square/app/recipes/take-ambient-turn.json", import.meta.url), "utf8"));
  const bundle = defineRecipeBundle(shipped);
  const root = bundle.recipe.nodes.root;
  const declaration = root.node;
  const built = recipeNode.contextualRound(declaration, root.input_schema_ref, root.output.schema_ref,
    root.authority.required_grant_refs, { maxActiveMillis: root.resources.max_active_millis,
      maxInputBytes: root.resources.max_input_bytes, maxOutputBytes: root.resources.max_output_bytes,
      cancellationAcknowledgementMillis: root.cancellation.acknowledgement_timeout_millis });
  assert.deepEqual(built, root);
  assert.equal(built.node.program.limits.max_participants, 32);
  assert.equal(built.node.program.context_mode, "progressive");
  assert.equal(built.resources.max_parallelism, 1);
  assert.equal(built.resources.max_cost_microusd, 4_000_000);
  assert.equal(built.effect.uncertainty, "receipt_bound");
});

test("contextual round cannot detach source authority or understate concurrency and spend", () => {
  const shipped = JSON.parse(readFileSync(new URL("../../../magician_data_v3/system/town_square/app/recipes/take-ambient-turn.json", import.meta.url), "utf8"));
  for (const change of [
    (bundle) => { bundle.recipe.nodes.root.authority.action_ref = "primitive-action:other"; },
    (bundle) => { bundle.recipe.nodes.root.resources.max_parallelism = 0; },
    (bundle) => { bundle.recipe.nodes.root.resources.max_cost_microusd = 0; },
    (bundle) => { bundle.recipe.ceilings.max_cost_microusd = 0; },
    (bundle) => { bundle.recipe.ceilings.max_parallelism = 0; },
    (bundle) => { bundle.recipe.nodes.root.node.program.limits.per_participant.tokens = 3_000_000; },
    (bundle) => { bundle.recipe.nodes.root.node.program.limits.aggregate.micro_usd = -1; },
    (bundle) => { bundle.recipe.nodes.root.node.source.rows.kind = "document"; },
  ]) {
    const invalid = structuredClone(shipped);
    change(invalid);
    assert.throws(() => defineRecipeBundle(invalid));
  }
});


test("shared store builder admits mechanical ledger recipes across Apps without model authority", () => {
  for (const [app, workflow] of [["learning", "approve-candidate"], ["meetings", "listen"], ["claims_review", "confirm-claim"], ["town_square", "set-policy"], ["town_square", "publish-post"], ["town_square", "create-group"], ["town_square", "react-to-post"], ["meetings", "read-transcript"], ["meetings", "search-meetings"], ["meetings", "sync-sessions"], ["meetings", "sync-takeaways"], ["meetings", "sync-threads"], ["meetings", "sync-upcoming"], ["claims_review", "stage-ingest"], ["claims_review", "sync-claims"], ["claims_review", "sync-context"]]) {
    const source = JSON.parse(readFileSync(new URL(`../../../magician_data_v3/system/${app}/app/recipes/${workflow}.json`, import.meta.url), "utf8"));
    const bundle = defineRecipeBundle(source);
    const root = bundle.recipe.nodes.root;
    const node = recipeNode.storeTransaction(root.node, root.input_schema_ref, root.output.schema_ref, root.authority.required_grant_refs);
    assert.equal(node.node.kind, "store_transaction");
    assert.equal(node.resources.max_cost_microusd, 0);
    assert.equal(node.resources.max_tool_calls, Object.keys(root.node.sources).length);
    assert.equal(node.authority.primitive_ref, undefined);
    const invalid = structuredClone(source);
    invalid.recipe.nodes.root.node.program.values.expressions.push({ kind: "read", source: "model", pointer: "/unused" });
    assert.throws(() => defineRecipeBundle(invalid), /semantic dependencies/);
  }
});
