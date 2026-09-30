import { blake3 } from "@noble/hashes/blake3";
import { bytesToHex } from "@noble/hashes/utils";

type FixtureJson = FixtureObject | readonly FixtureJson[] | string | number | boolean | null;
interface FixtureObject { readonly [key: string]: FixtureJson }

const FLOOR = { classification: "personal", model_processing: "local_only" } as const;

function canonicalJson(value: FixtureJson): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const object = value as FixtureObject;
    return `{${Object.keys(object).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(object[key]!)}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function schemaRef(source: FixtureObject): string {
  return `workflow-schema:blake3:${bytesToHex(blake3(new TextEncoder().encode(JSON.stringify(source))))}`;
}

function output(schema: string, kind: "typed_value" | "entity_projection" = "typed_value", authority: "derived" | "authoritative" = "derived") {
  return { kind, schema_ref: schema, authority } as const;
}

function resources() {
  return {
    max_active_millis: 1000,
    max_input_bytes: 4096,
    max_output_bytes: 4096,
    max_cost_microusd: 0,
    max_tool_calls: 0,
    max_parallelism: 1,
  } as const;
}

function pureNode(kind: FixtureObject, input: string, result: string) {
  return {
    input_schema_ref: input,
    output: output(result),
    node: kind,
    effect: { class: "none", idempotency: { kind: "not_applicable" }, uncertainty: "impossible" },
    authority: {},
    resources: resources(),
    retry: { kind: "none" },
    cancellation: { mode: "propagate", acknowledgement_timeout_millis: 100 },
    provenance_join: "preserve",
  } as const;
}

function ceilings(nodes: number, maxParallelism = 1) {
  return {
    max_nodes: nodes,
    max_edges: nodes,
    max_depth: nodes,
    max_fan_out: nodes,
    max_parallelism: maxParallelism,
    max_payload_bytes: 32768,
    max_active_millis: nodes * 1000,
    max_cost_microusd: 0,
    max_tool_calls: 0,
  } as const;
}

function evolution() {
  return { topology_revision: 1, migration: "recompile_required" } as const;
}

export function supportedRecipeFixtures(): Readonly<Record<string, FixtureObject>> {
  const valueSchema = {
    version: "v1",
    root: 0,
    handling_floor: FLOOR,
    nodes: [
      { kind: "record", fields: { value: { value_type: 1, required: true } } },
      { kind: "text", max_bytes: 4096 },
    ],
  } as const;
  const valueRef = schemaRef(valueSchema);
  const sequenceNodes = {
    emit: pureNode({ kind: "emit_value" }, valueRef, valueRef),
    root: pureNode({ kind: "sequence", steps: ["validate", "emit"] }, valueRef, valueRef),
    validate: pureNode({ kind: "validate" }, valueRef, valueRef),
  } as const;

  const parallelSchema = {
    version: "v1",
    root: 0,
    handling_floor: FLOOR,
    nodes: [
      { kind: "array", items: 1, min_items: 2, max_items: 2 },
      { kind: "record", fields: { value: { value_type: 2, required: true } } },
      { kind: "text", max_bytes: 4096 },
    ],
  } as const;
  const parallelRef = schemaRef(parallelSchema);
  const parallelNodes = {
    left: pureNode({ kind: "validate" }, valueRef, valueRef),
    right: pureNode({ kind: "emit_value" }, valueRef, valueRef),
    root: {
      ...pureNode({ kind: "parallel", branches: { alpha: "left", omega: "right" } }, valueRef, parallelRef),
      resources: { ...resources(), max_parallelism: 2 },
    },
  } as const;

  const taggedSchema = {
    version: "v1",
    root: 0,
    handling_floor: FLOOR,
    nodes: [
      { kind: "tagged_union", discriminator: "status", variants: { active: 1, draft: 3 } },
      { kind: "record", fields: { value: { value_type: 2, required: true } } },
      { kind: "text", max_bytes: 4096 },
      { kind: "record", fields: { value: { value_type: 4, required: true } } },
      { kind: "text", max_bytes: 4096 },
    ],
  } as const;
  const taggedRef = schemaRef(taggedSchema);
  const switchNodes = {
    active: pureNode({ kind: "validate" }, valueRef, valueRef),
    draft: pureNode({ kind: "emit_value" }, valueRef, valueRef),
    root: pureNode({ kind: "switch", discriminator: "status", cases: { active: "active", draft: "draft" } }, taggedRef, valueRef),
  } as const;

  const queryInputSchema = {
    version: "v1",
    root: 0,
    handling_floor: FLOOR,
    nodes: [
      { kind: "record", fields: {
        entity: { value_type: 1, required: true },
        limit: { value_type: 2, required: true },
        select: { value_type: 3, required: true },
      } },
      { kind: "text", max_bytes: 64 },
      { kind: "integer" },
      { kind: "array", items: 4, min_items: 1, max_items: 16 },
      { kind: "text", max_bytes: 128 },
    ],
  } as const;
  const queryInputRef = schemaRef(queryInputSchema);
  const projectionSchema = {
    version: "v1",
    root: 0,
    handling_floor: FLOOR,
    nodes: [{ kind: "entity_projection_ref", entity: "research_topic", value_schema_ref: queryInputRef }],
  } as const;
  const projectionRef = schemaRef(projectionSchema);
  const queryNode = {
    input_schema_ref: queryInputRef,
    output: output(projectionRef, "entity_projection", "authoritative"),
    node: { kind: "query", entity: "research_topic" },
    effect: { class: "read_only", idempotency: { kind: "intrinsic" }, uncertainty: "impossible" },
    authority: { required_grant_refs: ["grant:entity-read"] },
    resources: resources(),
    retry: { kind: "none" },
    cancellation: { mode: "propagate", acknowledgement_timeout_millis: 100 },
    provenance_join: "preserve",
  } as const;
  const getNode = {
    input_schema_ref: projectionRef,
    output: output(projectionRef, "entity_projection", "authoritative"),
    node: { kind: "get", entity: "research_topic" },
    effect: { class: "read_only", idempotency: { kind: "intrinsic" }, uncertainty: "impossible" },
    authority: { required_grant_refs: ["grant:entity-read"] },
    resources: resources(),
    retry: { kind: "none" },
    cancellation: { mode: "propagate", acknowledgement_timeout_millis: 100 },
    provenance_join: "preserve",
  } as const;
  const queryGetRoot = {
    ...pureNode({ kind: "sequence", steps: ["query", "get"] }, queryInputRef, projectionRef),
    output: output(projectionRef, "entity_projection", "authoritative"),
  } as const;
  const mappingOperations = [{ kind: "select", source: "value", target: "value" }] as const;
  const mappingDigest = `blake3:${bytesToHex(blake3(new TextEncoder().encode(canonicalJson({
    source_schema_ref: valueRef,
    target_schema_ref: valueRef,
    operations: mappingOperations,
  }))))}`;
  const mapNode = pureNode({
    kind: "map",
    mapping_digest: mappingDigest,
    operations: mappingOperations,
  }, valueRef, valueRef);

  return {
    query: {
      version: "v1",
      schemas: { input: queryInputSchema, projection: projectionSchema },
      recipe: {
        version: "v1",
        input_schema_ref: queryInputRef,
        output: output(projectionRef, "entity_projection", "authoritative"),
        root: "root",
        nodes: { root: queryNode },
        ceilings: ceilings(1),
        evolution: evolution(),
      },
    },
    get: {
      version: "v1",
      schemas: { input: queryInputSchema, projection: projectionSchema },
      recipe: {
        version: "v1",
        input_schema_ref: queryInputRef,
        output: output(projectionRef, "entity_projection", "authoritative"),
        root: "root",
        nodes: { get: getNode, query: queryNode, root: queryGetRoot },
        ceilings: ceilings(3),
        evolution: evolution(),
      },
    },
    map: {
      version: "v1",
      schemas: { value: valueSchema },
      recipe: {
        version: "v1",
        input_schema_ref: valueRef,
        output: output(valueRef),
        root: "root",
        nodes: { root: mapNode },
        ceilings: ceilings(1),
        evolution: evolution(),
      },
    },
    sequence: {
      version: "v1",
      schemas: { value: valueSchema },
      recipe: {
        version: "v1",
        input_schema_ref: valueRef,
        output: output(valueRef),
        root: "root",
        nodes: sequenceNodes,
        ceilings: ceilings(3),
        evolution: evolution(),
      },
    },
    parallel: {
      version: "v1",
      schemas: { output: parallelSchema, value: valueSchema },
      recipe: {
        version: "v1",
        input_schema_ref: valueRef,
        output: output(parallelRef),
        root: "root",
        nodes: parallelNodes,
        ceilings: ceilings(3, 2),
        evolution: evolution(),
      },
    },
    switch: {
      version: "v1",
      schemas: { tagged: taggedSchema, value: valueSchema },
      recipe: {
        version: "v1",
        input_schema_ref: taggedRef,
        output: output(valueRef),
        root: "root",
        nodes: switchNodes,
        ceilings: ceilings(3),
        evolution: evolution(),
      },
    },
  };
}
