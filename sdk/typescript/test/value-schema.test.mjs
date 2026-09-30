import assert from "node:assert/strict";
import test from "node:test";

import { defineAppValueCodec } from "../dist/index.js";

const digest = `blake3:${"a".repeat(64)}`;
const schemaRef = `workflow-schema:${digest}`;
const floor = { classification: "personal", model_processing: "local_only" };

test("generated codecs enforce arrays and closed tagged unions", () => {
  const schema = {
    version: "v1",
    root: 0,
    handling_floor: floor,
    nodes: [
      { kind: "record", fields: { items: { value_type: 1, required: true } } },
      { kind: "array", items: 2, min_items: 1, max_items: 1 },
      { kind: "tagged_union", discriminator: "kind", variants: { text: 3 } },
      { kind: "text", max_bytes: 8 },
    ],
  };
  const codec = defineAppValueCodec(schemaRef, digest, schema, "input");
  assert.deepEqual(codec.parse({ items: [{ kind: "text", value: "ok" }] }), {
    items: [{ kind: "text", value: "ok" }],
  });
  assert.equal(codec.is({ items: [{ kind: "unknown", value: "x" }] }), false);
  assert.equal(codec.is({ items: [{ kind: "text", value: "x" }, { kind: "text", value: "y" }] }), false);
  assert.equal(codec.is({ items: [{ kind: "text", value: "too-long-value" }] }), false);
});

test("resource codecs reject input minting and internal-id substitution", () => {
  const schema = {
    version: "v1",
    root: 0,
    handling_floor: floor,
    nodes: [{ kind: "artifact_ref", value_schema_ref: schemaRef, max_bytes: 1024, media_types: ["application/json"] }],
  };
  const input = defineAppValueCodec(schemaRef, digest, schema, "input");
  const result = defineAppValueCodec(schemaRef, digest, schema, "result");
  assert.equal(input.is("artifact-handle:opaque-one"), false);
  assert.equal(result.is("artifact-handle:opaque-one"), true);
  assert.equal(result.is("artifact:task_123"), false);
  assert.equal(result.is("artifact-handle:execution_123"), false);
});

test("schema construction rejects recursion and identity drift", () => {
  const recursive = {
    version: "v1",
    root: 0,
    handling_floor: floor,
    nodes: [{ kind: "nullable", value_type: 0 }],
  };
  assert.throws(() => defineAppValueCodec(schemaRef, digest, recursive, "input"), /recursive/);
  assert.throws(
    () => defineAppValueCodec(schemaRef, `blake3:${"b".repeat(64)}`, {
      ...recursive,
      nodes: [{ kind: "text", max_bytes: 8 }],
    }, "input"),
    /identity/,
  );
});
