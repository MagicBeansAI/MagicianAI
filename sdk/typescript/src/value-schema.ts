import type { AppName, JsonValue } from "./types.js";

export type AppGeneratedValueSchemaNode =
  | { readonly kind: "unit" }
  | { readonly kind: "boolean" }
  | { readonly kind: "integer" }
  | { readonly kind: "decimal" }
  | { readonly kind: "text"; readonly max_bytes: number }
  | { readonly kind: "markdown"; readonly max_bytes: number }
  | { readonly kind: "enum"; readonly values: readonly AppName[] }
  | { readonly kind: "timestamp" }
  | { readonly kind: "entity_reference"; readonly entity: AppName }
  | { readonly kind: "opaque_reference" }
  | { readonly kind: "entity_projection_ref"; readonly entity: AppName; readonly value_schema_ref: string }
  | {
      readonly kind: "artifact_ref";
      readonly value_schema_ref: string;
      readonly max_bytes: number;
      readonly media_types: readonly string[];
    }
  | { readonly kind: "receipt_ref"; readonly receipt_kind: "mutation" | "external_effect" }
  | { readonly kind: "resource_ref"; readonly resource_kind: AppName }
  | { readonly kind: "nullable"; readonly value_type: number }
  | { readonly kind: "array"; readonly items: number; readonly min_items: number; readonly max_items: number }
  | {
      readonly kind: "record";
      readonly fields: Readonly<Record<AppName, { readonly value_type: number; readonly required: boolean }>>;
    }
  | {
      readonly kind: "tagged_union";
      readonly discriminator: AppName;
      readonly variants: Readonly<Record<AppName, number>>;
    };

export interface AppGeneratedValueSchema {
  readonly version: "v1";
  readonly root: number;
  readonly handling_floor: {
    readonly classification: "public" | "ordinary" | "personal" | "sensitive" | "secret";
    readonly model_processing: "none" | "local_only" | "remote_allowed";
  };
  readonly nodes: readonly AppGeneratedValueSchemaNode[];
}

export interface AppValueCodec<T> {
  readonly schemaRef: string;
  readonly schemaDigest: string;
  parse(value: JsonValue): T;
  is(value: unknown): value is T;
}

export type AppValueCarrier = "input" | "result";

const MAX_SCHEMA_NODES = 256;
const MAX_SCHEMA_DEPTH = 32;
const MAX_VALUE_NODES = 8_000;
const NAME = /^[A-Za-z][A-Za-z0-9_-]{0,127}$/;
const ISO_TIMESTAMP = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/;

function safeIndex(value: number, count: number): boolean {
  return Number.isSafeInteger(value) && value >= 0 && value < count;
}

function children(node: AppGeneratedValueSchemaNode): readonly number[] {
  if (node.kind === "nullable") return [node.value_type];
  if (node.kind === "array") return [node.items];
  if (node.kind === "record") return Object.values(node.fields).map((field) => field.value_type);
  if (node.kind === "tagged_union") return Object.values(node.variants);
  return [];
}

function validateSchema(schema: AppGeneratedValueSchema): void {
  if (schema.version !== "v1"
    || schema.nodes.length < 1
    || schema.nodes.length > MAX_SCHEMA_NODES
    || !safeIndex(schema.root, schema.nodes.length)) {
    throw new TypeError("generated workflow schema is not bounded V1");
  }
  const state = new Uint8Array(schema.nodes.length);
  const stack: Array<{ index: number; depth: number; exit: boolean }> = [
    { index: schema.root, depth: 0, exit: false },
  ];
  while (stack.length > 0) {
    const current = stack.pop();
    if (current === undefined) break;
    if (current.depth > MAX_SCHEMA_DEPTH || !safeIndex(current.index, schema.nodes.length)) {
      throw new TypeError("generated workflow schema exceeds its graph bounds");
    }
    if (current.exit) {
      state[current.index] = 2;
      continue;
    }
    if (state[current.index] === 1) throw new TypeError("generated workflow schema is recursive");
    if (state[current.index] === 2) continue;
    state[current.index] = 1;
    stack.push({ ...current, exit: true });
    const node = schema.nodes[current.index];
    if (node === undefined) throw new TypeError("generated workflow schema references an unknown node");
    if ((node.kind === "text" || node.kind === "markdown") && (!Number.isSafeInteger(node.max_bytes) || node.max_bytes < 1 || node.max_bytes > 262_144)) {
      throw new TypeError("generated text schema has an invalid byte ceiling");
    }
    if (node.kind === "enum" && (node.values.length < 1 || node.values.length > 32 || new Set(node.values).size !== node.values.length)) {
      throw new TypeError("generated enum schema is not closed and bounded");
    }
    if (node.kind === "array" && (!Number.isSafeInteger(node.min_items)
      || !Number.isSafeInteger(node.max_items)
      || node.min_items < 0
      || node.min_items > node.max_items
      || node.max_items > 256)) {
      throw new TypeError("generated array schema has invalid item bounds");
    }
    if (node.kind === "record") {
      const names = Object.keys(node.fields);
      if (names.length < 1 || names.length > 64 || names.some((name) => !NAME.test(name))) {
        throw new TypeError("generated record schema is not named and bounded");
      }
    }
    if (node.kind === "tagged_union") {
      const variants = Object.keys(node.variants);
      if (!NAME.test(node.discriminator)
        || node.discriminator === "value"
        || variants.length < 1
        || variants.length > 32
        || variants.includes(node.discriminator)) {
        throw new TypeError("generated tagged union is not closed");
      }
    }
    for (const child of children(node)) {
      stack.push({ index: child, depth: current.depth + 1, exit: false });
    }
  }
  if (state.some((entry) => entry !== 2)) throw new TypeError("generated workflow schema has unreachable definitions");
}

function resourcePrefix(node: AppGeneratedValueSchemaNode): string | undefined {
  if (node.kind === "entity_projection_ref") return "entity-handle:";
  if (node.kind === "artifact_ref") return "artifact-handle:";
  if (node.kind === "receipt_ref") return "receipt-handle:";
  if (node.kind === "resource_ref") return "resource-handle:";
  return undefined;
}

function validateValue(schema: AppGeneratedValueSchema, carrier: AppValueCarrier, root: JsonValue): boolean {
  const stack: Array<{ value: JsonValue; type: number; depth: number }> = [
    { value: root, type: schema.root, depth: 0 },
  ];
  let visited = 0;
  while (stack.length > 0) {
    const current = stack.pop();
    if (current === undefined) break;
    visited += 1;
    if (visited > MAX_VALUE_NODES || current.depth > MAX_SCHEMA_DEPTH) return false;
    const node = schema.nodes[current.type];
    if (node === undefined) return false;
    if (node.kind === "nullable") {
      if (current.value !== null) stack.push({ value: current.value, type: node.value_type, depth: current.depth + 1 });
      continue;
    }
    if (node.kind === "unit") {
      if (current.value !== null) return false;
      continue;
    }
    if (node.kind === "boolean") {
      if (typeof current.value !== "boolean") return false;
      continue;
    }
    if (node.kind === "integer") {
      if (typeof current.value !== "number" || !Number.isSafeInteger(current.value)) return false;
      continue;
    }
    if (node.kind === "decimal") {
      if (typeof current.value !== "number" || !Number.isFinite(current.value)) return false;
      continue;
    }
    if (node.kind === "text" || node.kind === "markdown") {
      if (typeof current.value !== "string" || new TextEncoder().encode(current.value).length > node.max_bytes) return false;
      continue;
    }
    if (node.kind === "enum") {
      if (typeof current.value !== "string" || !node.values.includes(current.value)) return false;
      continue;
    }
    if (node.kind === "timestamp") {
      if (typeof current.value !== "string" || !ISO_TIMESTAMP.test(current.value) || Number.isNaN(Date.parse(current.value))) return false;
      continue;
    }
    if (node.kind === "entity_reference") {
      if (typeof current.value !== "string" || current.value.length < 1 || current.value.length > 256) return false;
      continue;
    }
    if (node.kind === "opaque_reference") {
      if (typeof current.value !== "string" || !current.value.startsWith("ref:") || current.value.includes("..")) return false;
      continue;
    }
    const prefix = resourcePrefix(node);
    if (prefix !== undefined) {
      if (carrier !== "result"
        || typeof current.value !== "string"
        || !current.value.startsWith(prefix)
        || current.value.includes("task")
        || current.value.includes("execution")) return false;
      continue;
    }
    if (node.kind === "array") {
      if (!Array.isArray(current.value) || current.value.length < node.min_items || current.value.length > node.max_items) return false;
      for (let index = current.value.length - 1; index >= 0; index -= 1) {
        stack.push({ value: current.value[index] as JsonValue, type: node.items, depth: current.depth + 1 });
      }
      continue;
    }
    if (node.kind === "record") {
      if (current.value === null || Array.isArray(current.value) || typeof current.value !== "object") return false;
      const object = current.value as Readonly<Record<string, JsonValue>>;
      if (Object.keys(object).some((name) => !(name in node.fields))) return false;
      for (const [name, field] of Object.entries(node.fields)) {
        if (!(name in object)) {
          if (field.required) return false;
          continue;
        }
        stack.push({ value: object[name] as JsonValue, type: field.value_type, depth: current.depth + 1 });
      }
      continue;
    }
    if (node.kind === "tagged_union") {
      if (current.value === null || Array.isArray(current.value) || typeof current.value !== "object") return false;
      const object = current.value as Readonly<Record<string, JsonValue>>;
      const keys = Object.keys(object);
      const tag = object[node.discriminator];
      if (keys.length !== 2 || typeof tag !== "string" || !(tag in node.variants) || !("value" in object)) return false;
      stack.push({ value: object.value as JsonValue, type: node.variants[tag] as number, depth: current.depth + 1 });
      continue;
    }
    return false;
  }
  return true;
}

export function defineAppValueCodec<T>(
  schemaRef: string,
  schemaDigest: string,
  schema: AppGeneratedValueSchema,
  carrier: AppValueCarrier,
): AppValueCodec<T> {
  if (!/^workflow-schema:blake3:[0-9a-f]{64}$/.test(schemaRef)
    || !/^blake3:[0-9a-f]{64}$/.test(schemaDigest)
    || !schemaRef.endsWith(schemaDigest)) {
    throw new TypeError("generated workflow schema identity is inconsistent");
  }
  validateSchema(schema);
  return Object.freeze({
    schemaRef,
    schemaDigest,
    parse(value: JsonValue): T {
      if (!validateValue(schema, carrier, value)) throw new TypeError("value does not match its exact generated app schema");
      return value as T;
    },
    is(value: unknown): value is T {
      return validateValue(schema, carrier, value as JsonValue);
    },
  });
}
