import { MagicianAppsError } from "./errors.js";
import type { AppPublicOperationId } from "./generated/public-contract.js";
import type { JsonValue } from "./types.js";

export interface JsonShapeLimits {
  readonly maxDepth: number;
  readonly maxNodes: number;
}

function jsonChildren(
  operationId: AppPublicOperationId,
  value: object,
  maxNodes: number,
): readonly unknown[] {
  if (Array.isArray(value)) {
    if (value.length > maxNodes || Object.keys(value).length !== value.length) {
      throw new MagicianAppsError("limit_exceeded", operationId, "JSON array is sparse or exceeds the node ceiling");
    }
    const children: unknown[] = [];
    for (let index = 0; index < value.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (descriptor === undefined || !("value" in descriptor)) {
        throw new MagicianAppsError("decode", operationId, "JSON arrays may not contain accessors or holes");
      }
      children.push(descriptor.value);
    }
    return children;
  }
  const descriptors = Object.getOwnPropertyDescriptors(value);
  const keys = Object.keys(descriptors);
  if (keys.length > maxNodes) {
    throw new MagicianAppsError("limit_exceeded", operationId, "JSON object exceeds the node ceiling");
  }
  const children: unknown[] = [];
  for (const key of keys) {
    const descriptor = descriptors[key];
    if (descriptor === undefined || !("value" in descriptor) || descriptor.enumerable !== true) {
      throw new MagicianAppsError("decode", operationId, "JSON objects may contain only enumerable data properties");
    }
    children.push(descriptor.value);
  }
  return children;
}

function assertPositiveLimit(value: number, label: string): void {
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new TypeError(`${label} must be a positive safe integer`);
  }
}

function inspectBoundedJsonShape(
  operationId: AppPublicOperationId,
  value: unknown,
  limits: JsonShapeLimits,
): number {
  assertPositiveLimit(limits.maxDepth, "maxDepth");
  assertPositiveLimit(limits.maxNodes, "maxNodes");
  const ancestors = new Set<object>();
  let nodes = 0;
  const stack: Array<{ readonly value: unknown; readonly depth: number; readonly exiting?: boolean }> = [
    { value, depth: 1 },
  ];
  while (stack.length > 0) {
    const current = stack.pop();
    if (current === undefined) break;
    if (current.exiting) {
      ancestors.delete(current.value as object);
      continue;
    }
    nodes += 1;
    if (nodes > limits.maxNodes) {
      throw new MagicianAppsError("limit_exceeded", operationId, "JSON node ceiling exceeded");
    }
    if (current.depth > limits.maxDepth) {
      throw new MagicianAppsError("limit_exceeded", operationId, "JSON depth ceiling exceeded");
    }
    const entry = current.value;
    if (entry === null || typeof entry === "string" || typeof entry === "boolean") continue;
    if (typeof entry === "number") {
      if (!Number.isFinite(entry)) {
        throw new MagicianAppsError("decode", operationId, "JSON contains a non-finite number");
      }
      if (Number.isInteger(entry) && !Number.isSafeInteger(entry)) {
        throw new MagicianAppsError("decode", operationId, "JSON contains an integer outside the exact JavaScript range");
      }
      continue;
    }
    if (typeof entry !== "object") {
      throw new MagicianAppsError("decode", operationId, "Value is not JSON-compatible");
    }
    if (ancestors.has(entry)) {
      throw new MagicianAppsError("decode", operationId, "JSON value contains a cycle");
    }
    const prototype = Object.getPrototypeOf(entry);
    if (!Array.isArray(entry) && prototype !== Object.prototype && prototype !== null) {
      throw new MagicianAppsError("decode", operationId, "JSON objects must be plain objects");
    }
    ancestors.add(entry);
    stack.push({ value: entry, depth: current.depth, exiting: true });
    const children = jsonChildren(operationId, entry, limits.maxNodes);
    for (let index = children.length - 1; index >= 0; index -= 1) {
      const child = children[index];
      if (child === undefined) {
        throw new MagicianAppsError("decode", operationId, "JSON arrays and objects may not contain undefined");
      }
      stack.push({ value: child, depth: current.depth + 1 });
    }
  }
  return nodes;
}

export function assertBoundedJsonShape(
  operationId: AppPublicOperationId,
  value: unknown,
  limits: JsonShapeLimits,
): asserts value is JsonValue {
  inspectBoundedJsonShape(operationId, value, limits);
}

function jsonStringByteLength(value: string): number {
  let bytes = 2;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code === 0x22 || code === 0x5c || code === 0x08 || code === 0x09
      || code === 0x0a || code === 0x0c || code === 0x0d) {
      bytes += 2;
    } else if (code < 0x20 || (code >= 0xd800 && code <= 0xdfff
      && !(code <= 0xdbff && index + 1 < value.length
        && value.charCodeAt(index + 1) >= 0xdc00 && value.charCodeAt(index + 1) <= 0xdfff))) {
      bytes += 6;
    } else if (code < 0x80) {
      bytes += 1;
    } else if (code < 0x800) {
      bytes += 2;
    } else if (code >= 0xd800 && code <= 0xdbff) {
      bytes += 4;
      index += 1;
    } else {
      bytes += 3;
    }
  }
  return bytes;
}

function exactSerializedByteLength(
  operationId: AppPublicOperationId,
  value: JsonValue,
  maxBytes: number,
  maxNodes: number,
): number {
  let bytes = 0;
  const add = (count: number): void => {
    bytes += count;
    if (bytes > maxBytes) {
      throw new MagicianAppsError("limit_exceeded", operationId, "JSON request byte ceiling exceeded");
    }
  };
  const stack: Array<
    | { readonly kind: "value"; readonly value: JsonValue }
    | { readonly kind: "bytes"; readonly count: number }
  > = [
    { kind: "value", value },
  ];
  while (stack.length > 0) {
    const current = stack.pop();
    if (current === undefined) break;
    if (current.kind === "bytes") {
      add(current.count);
      continue;
    }
    const entry = current.value;
    if (entry === null) {
      add(4);
    } else if (typeof entry === "boolean") {
      add(entry ? 4 : 5);
    } else if (typeof entry === "number") {
      add(JSON.stringify(entry).length);
    } else if (typeof entry === "string") {
      add(jsonStringByteLength(entry));
    } else if (Array.isArray(entry)) {
      const children = jsonChildren(operationId, entry, maxNodes) as readonly JsonValue[];
      add(2);
      for (let index = children.length - 1; index >= 0; index -= 1) {
        if (index < children.length - 1) stack.push({ kind: "bytes", count: 1 });
        const child = children[index];
        if (child === undefined) {
          throw new MagicianAppsError("decode", operationId, "JSON arrays may not contain undefined");
        }
        stack.push({ kind: "value", value: child });
      }
    } else {
      const descriptors = Object.getOwnPropertyDescriptors(entry);
      const keys = Object.keys(descriptors);
      add(2);
      for (let index = keys.length - 1; index >= 0; index -= 1) {
        const key = keys[index];
        if (key === undefined) continue;
        const descriptor = descriptors[key];
        if (descriptor === undefined || !("value" in descriptor)) {
          throw new MagicianAppsError("decode", operationId, "JSON objects may not contain accessors");
        }
        if (index < keys.length - 1) stack.push({ kind: "bytes", count: 1 });
        stack.push({ kind: "value", value: descriptor.value as JsonValue });
        stack.push({ kind: "bytes", count: 1 + jsonStringByteLength(key) });
      }
    }
  }
  return bytes;
}

export interface BoundedJsonMetrics {
  readonly bytes: number;
  readonly nodes: number;
}

export function boundedJsonMetrics(
  operationId: AppPublicOperationId,
  value: unknown,
  maxBytes: number,
  limits: JsonShapeLimits,
): BoundedJsonMetrics {
  assertPositiveLimit(maxBytes, "maxBytes");
  const nodes = inspectBoundedJsonShape(operationId, value, limits);
  const bytes = exactSerializedByteLength(operationId, value as JsonValue, maxBytes, limits.maxNodes);
  return { bytes, nodes };
}

export function assertBoundedJsonValue(
  operationId: AppPublicOperationId,
  value: unknown,
  maxBytes: number,
  limits: JsonShapeLimits,
): asserts value is JsonValue {
  boundedJsonMetrics(operationId, value, maxBytes, limits);
}

export function encodeBoundedJson(
  operationId: AppPublicOperationId,
  value: unknown,
  maxBytes: number,
  limits: JsonShapeLimits,
): Uint8Array {
  assertBoundedJsonValue(operationId, value, maxBytes, limits);
  const encoded = new TextEncoder().encode(JSON.stringify(value));
  if (encoded.byteLength > maxBytes) throw new Error("bounded JSON byte preflight invariant failed");
  return encoded;
}

function responseContentLength(response: Response): number | undefined {
  const raw = response.headers.get("content-length");
  if (raw === null) return undefined;
  const parsed = Number(raw);
  return Number.isSafeInteger(parsed) && parsed >= 0 ? parsed : undefined;
}

export async function decodeBoundedJsonResponse(
  operationId: AppPublicOperationId,
  response: Response,
  maxBytes: number,
  limits: JsonShapeLimits,
): Promise<JsonValue> {
  const contentType = response.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase();
  if (contentType !== "application/json") {
    throw new MagicianAppsError("decode", operationId, "Response media type is not application/json");
  }
  const declaredLength = responseContentLength(response);
  if (declaredLength !== undefined && declaredLength > maxBytes) {
    throw new MagicianAppsError("limit_exceeded", operationId, "JSON response byte ceiling exceeded");
  }
  if (response.body === null) {
    throw new MagicianAppsError("decode", operationId, "JSON response body is missing");
  }
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > maxBytes) {
        await reader.cancel();
        throw new MagicianAppsError("limit_exceeded", operationId, "JSON response byte ceiling exceeded");
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new MagicianAppsError("decode", operationId, "Response is not valid UTF-8");
  }
  let decoded: unknown;
  try {
    decoded = JSON.parse(text) as unknown;
  } catch {
    throw new MagicianAppsError("decode", operationId, "Response is not valid JSON");
  }
  assertBoundedJsonShape(operationId, decoded, limits);
  return decoded;
}
