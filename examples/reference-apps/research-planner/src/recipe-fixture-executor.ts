import type { AppRecordProjection, JsonValue } from "@magician/apps";

import { supportedRecipeFixtures } from "./recipe-fixtures.js";

const FIELD = /^[A-Za-z][A-Za-z0-9_]{0,127}$/;
const RECORD_ID = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/;
const MAX_FIXTURE_BYTES = 32_768;
const MAX_VALUE_BYTES = 4_096;
const MAX_QUERY_LIMIT = 100;

export type SupportedRecipeFixtureName = "query" | "get" | "map" | "sequence" | "parallel" | "switch";
export type SupportedRecipeNodeKind = "query" | "get" | "map" | "validate" | "emit_value" | "sequence" | "parallel" | "switch";

export interface SupportedRecipeQueryInput {
  readonly entity: "research_topic";
  readonly limit: number;
  readonly select: readonly string[];
}

export interface SupportedRecipeFixtureOwners {
  queryOwnEntity(input: SupportedRecipeQueryInput):
    | readonly AppRecordProjection[]
    | Promise<readonly AppRecordProjection[]>;
}

export interface SupportedRecipeFixtureExecution {
  readonly value: JsonValue;
  readonly executedNodeKinds: readonly SupportedRecipeNodeKind[];
}

/**
 * Execute only this package's three provider-free qualification fixtures.
 * This deliberately is not an arbitrary Recipe interpreter or validator.
 */
export async function executeSupportedRecipeFixture(
  name: SupportedRecipeFixtureName,
  input: JsonValue,
  owners: SupportedRecipeFixtureOwners,
): Promise<SupportedRecipeFixtureExecution> {
  const fixture = supportedRecipeFixtures()[name];
  if (fixture === undefined) throw new Error(`unsupported_recipe_fixture:${name}`);
  if (new TextEncoder().encode(JSON.stringify(fixture)).byteLength > MAX_FIXTURE_BYTES) {
    throw new Error("recipe_fixture_contract_drift:fixture exceeds its byte ceiling");
  }
  const recipe = requireRecord(fixture.recipe, "recipe");
  const nodes = requireRecord(recipe.nodes, "recipe.nodes");
  if (recipe.root !== "root") throw new Error("recipe_fixture_contract_drift:root");
  const trace: SupportedRecipeNodeKind[] = [];

  if (name === "query") {
    assertExactNodeSet(nodes, ["root"]);
    const root = requireNode(nodes, "root", "query");
    assertExactKeys(root, ["entity", "kind"], "query node");
    if (root.entity !== "research_topic") throw new Error("recipe_fixture_contract_drift:query entity");
    const query = validateQueryInput(input);
    trace.push("query");
    const records = await owners.queryOwnEntity(query);
    return { value: validateQueryResult(records, query), executedNodeKinds: trace };
  }

  if (name === "get") {
    assertExactNodeSet(nodes, ["get", "query", "root"]);
    const root = requireNode(nodes, "root", "sequence");
    assertExactKeys(root, ["kind", "steps"], "get sequence node");
    if (!Array.isArray(root.steps) || root.steps.join(",") !== "query,get") {
      throw new Error("recipe_fixture_contract_drift:get sequence");
    }
    const query = validateQueryInput(input);
    trace.push("sequence");
    requireNode(nodes, "query", "query");
    trace.push("query");
    const sealed = validateQueryResult(await owners.queryOwnEntity(query), query);
    requireNode(nodes, "get", "get");
    trace.push("get");
    const replayed = validateQueryResult(await owners.queryOwnEntity(query), query);
    if (JSON.stringify(replayed) !== JSON.stringify(sealed)) {
      throw new Error("recipe_get_stale_record_projection");
    }
    return { value: replayed, executedNodeKinds: trace };
  }

  if (name === "map") {
    assertExactNodeSet(nodes, ["root"]);
    const root = requireNode(nodes, "root", "map");
    assertExactKeys(root, ["kind", "mapping_digest", "operations"], "map node");
    if (typeof root.mapping_digest !== "string"
      || !/^blake3:[0-9a-f]{64}$/.test(root.mapping_digest)
      || !Array.isArray(root.operations)
      || root.operations.length !== 1) {
      throw new Error("recipe_fixture_contract_drift:map identity");
    }
    trace.push("map");
    return { value: validateValue(input), executedNodeKinds: trace };
  }

  if (name === "sequence") {
    assertExactNodeSet(nodes, ["emit", "root", "validate"]);
    const root = requireNode(nodes, "root", "sequence");
    assertExactKeys(root, ["kind", "steps"], "sequence node");
    if (!Array.isArray(root.steps)
      || root.steps.length !== 2
      || root.steps[0] !== "validate"
      || root.steps[1] !== "emit") {
      throw new Error("recipe_fixture_contract_drift:sequence steps");
    }
    trace.push("sequence");
    let value = validateValue(input);
    requireLeaf(nodes, "validate", "validate");
    trace.push("validate");
    value = validateValue(value);
    requireLeaf(nodes, "emit", "emit_value");
    trace.push("emit_value");
    return { value, executedNodeKinds: trace };
  }

  if (name === "parallel") {
    assertExactNodeSet(nodes, ["left", "right", "root"]);
    const root = requireNode(nodes, "root", "parallel");
    assertExactKeys(root, ["branches", "kind"], "parallel node");
    const branches = requireRecord(root.branches, "parallel branches");
    assertExactKeys(branches, ["alpha", "omega"], "parallel branches");
    if (branches.alpha !== "left" || branches.omega !== "right") {
      throw new Error("recipe_fixture_contract_drift:parallel branches");
    }
    trace.push("parallel");
    const values = await Promise.all([
      Promise.resolve().then(() => {
        requireLeaf(nodes, "left", "validate");
        return validateValue(input);
      }),
      Promise.resolve().then(() => {
        requireLeaf(nodes, "right", "emit_value");
        return validateValue(input);
      }),
    ]);
    trace.push("validate", "emit_value");
    return { value: values, executedNodeKinds: trace };
  }

  assertExactNodeSet(nodes, ["active", "draft", "root"]);
  const root = requireNode(nodes, "root", "switch");
  assertExactKeys(root, ["cases", "discriminator", "kind"], "switch node");
  const cases = requireRecord(root.cases, "switch cases");
  assertExactKeys(cases, ["active", "draft"], "switch cases");
  if (root.discriminator !== "status" || cases.active !== "active" || cases.draft !== "draft") {
    throw new Error("recipe_fixture_contract_drift:switch cases");
  }
  const tagged = validateTaggedValue(input);
  trace.push("switch");
  const branch = cases[tagged.status];
  const expectedKind = tagged.status === "active" ? "validate" : "emit_value";
  requireLeaf(nodes, String(branch), expectedKind);
  trace.push(expectedKind);
  return { value: validateValue({ value: tagged.value }), executedNodeKinds: trace };
}

function validateQueryInput(value: JsonValue): SupportedRecipeQueryInput {
  const input = requireRecord(value, "query input");
  assertExactKeys(input, ["entity", "limit", "select"], "query input");
  if (input.entity !== "research_topic"
    || typeof input.limit !== "number"
    || !Number.isSafeInteger(input.limit)
    || input.limit < 1
    || input.limit > MAX_QUERY_LIMIT
    || !Array.isArray(input.select)
    || input.select.length < 1
    || input.select.length > 16
    || input.select.some((field) => typeof field !== "string" || !FIELD.test(field))
    || new Set(input.select).size !== input.select.length) {
    throw new TypeError("query fixture input is not its exact bounded own-store request");
  }
  return {
    entity: "research_topic",
    limit: input.limit,
    select: input.select as string[],
  };
}

function validateQueryResult(
  value: readonly AppRecordProjection[],
  query: SupportedRecipeQueryInput,
): JsonValue {
  if (!Array.isArray(value) || value.length > query.limit) {
    throw new Error("query fixture owner exceeded the requested row ceiling");
  }
  const selected = [...query.select].sort();
  const seen = new Set<string>();
  for (const record of value) {
    if (!isRecord(record)
      || record.entity !== query.entity
      || typeof record.record_id !== "string"
      || !RECORD_ID.test(record.record_id)
      || seen.has(record.record_id)
      || typeof record.record_revision !== "number"
      || !Number.isSafeInteger(record.record_revision)
      || record.record_revision < 1
      || !isRecord(record.fields)
      || !sameKeys(record.fields, selected)) {
      throw new Error("query fixture owner returned a substituted projection");
    }
    seen.add(record.record_id);
  }
  return value as unknown as JsonValue;
}

function validateTaggedValue(value: JsonValue): { readonly status: "active" | "draft"; readonly value: string } {
  const tagged = requireRecord(value, "switch input");
  assertExactKeys(tagged, ["status", "value"], "switch input");
  if ((tagged.status !== "active" && tagged.status !== "draft") || !boundedValue(tagged.value)) {
    throw new TypeError("switch fixture input is not its exact tagged value");
  }
  return { status: tagged.status, value: tagged.value };
}

function validateValue(value: JsonValue): { readonly value: string } {
  const record = requireRecord(value, "value input");
  assertExactKeys(record, ["value"], "value input");
  if (!boundedValue(record.value)) throw new TypeError("value fixture input exceeds its schema");
  return { value: record.value };
}

function boundedValue(value: unknown): value is string {
  return typeof value === "string"
    && new TextEncoder().encode(value).byteLength <= MAX_VALUE_BYTES;
}

function requireLeaf(
  nodes: Readonly<Record<string, unknown>>,
  id: string,
  kind: "validate" | "emit_value",
): void {
  const node = requireNode(nodes, id, kind);
  assertExactKeys(node, ["kind"], `${kind} node`);
}

function requireNode(
  nodes: Readonly<Record<string, unknown>>,
  id: string,
  kind: SupportedRecipeNodeKind,
): Readonly<Record<string, unknown>> {
  const envelope = requireRecord(nodes[id], `recipe node ${id}`);
  const node = requireRecord(envelope.node, `recipe node ${id}.node`);
  if (node.kind !== kind) throw new Error(`recipe_fixture_contract_drift:${id} must be ${kind}`);
  return node;
}

function assertExactNodeSet(nodes: Readonly<Record<string, unknown>>, expected: readonly string[]): void {
  if (!sameKeys(nodes, [...expected].sort())) throw new Error("recipe_fixture_contract_drift:node set");
}

function assertExactKeys(value: Readonly<Record<string, unknown>>, expected: readonly string[], label: string): void {
  if (!sameKeys(value, [...expected].sort())) throw new Error(`recipe_fixture_contract_drift:${label}`);
}

function sameKeys(value: Readonly<Record<string, unknown>>, sortedExpected: readonly string[]): boolean {
  const observed = Object.keys(value).sort();
  return observed.length === sortedExpected.length
    && observed.every((key, index) => key === sortedExpected[index]);
}

function requireRecord(value: unknown, label: string): Readonly<Record<string, unknown>> {
  if (!isRecord(value)) throw new Error(`recipe_fixture_contract_drift:${label}`);
  return value;
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
