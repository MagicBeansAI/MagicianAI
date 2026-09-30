import {
  assertBoundedJsonValue,
  boundedJsonMetrics,
  decodeBoundedJsonResponse,
  encodeBoundedJson,
} from "./bounded-json.js";
import { MagicianAppsError } from "./errors.js";
import { blake3 } from "@noble/hashes/blake3";
import { bytesToHex } from "@noble/hashes/utils";
import {
  APP_CONTRACT_CAPABILITIES_SCHEMA_VERSION,
  APP_DATA_PLANE_PROTOCOL_VERSION,
  APP_JSON_SCHEMA_DIALECT,
  APP_SUPPORTED_PUBLIC_API_PREFIX,
  APP_SUPPORTED_PUBLIC_CONTRACT_VERSION,
  APP_SUPPORTED_PUBLIC_DEPRECATIONS,
  APP_SUPPORTED_PUBLIC_OPERATIONS,
  APP_SUPPORTED_MANIFEST_FEATURES,
  APP_SUPPORTED_MANIFEST_SCHEMA_VERSIONS,
  type AppPublicOperationId,
  type SupportedPublicErrorReason,
} from "./generated/public-contract.js";
import type { AppContractCapabilities, AppPublicOperation } from "./public-contract.js";
import type {
  AppActionCancellationReceipt,
  AppActionCancellationRequest,
  AppActionCompositionRequest,
  AppActionResultComposition,
  AppActionLaunchResponse,
  AppDirectActionRequest,
  AppEntityChangeBatch,
  AppMutationCommand,
  AppMutationReceipt,
  AppQueryPage,
  AppQueryRequest,
  AppRunSnapshot,
  AppOutputValidator,
  IterateEntityChangesOptions,
  JsonValue,
  ReadEntityChangesOptions,
  WaitForRunOptions,
} from "./types.js";
import {
  validateActionCancellationReceipt,
  validateActionComposition,
  validateActionLaunch,
  validateErrorEnvelope,
  validateEntityChangeBatch,
  validateMutationReceipt,
  validateQueryPage,
  validateRunSnapshot,
  type ResponseValidationLimits,
} from "./validators.js";

const RUN_REF_PREFIX = "run:app-action:";
const ERROR_BODY_MAX_BYTES = 65_536;
const APP_CONTRACT_CAPABILITIES_MAX_BYTES = 262_144;
const APP_SDK_HARD_MAX_DOCUMENT_BYTES = 1_048_576;
const APP_SDK_HARD_MAX_JSON_DEPTH = 32;
const APP_SDK_HARD_MAX_JSON_NODES = 20_000;
const APP_SDK_HARD_MAX_COLLECTION_ITEMS = 256;
const APP_SDK_HARD_MAX_VALUE_BYTES = 262_144;
const APP_SDK_HARD_MAX_VALUE_NODES = 8_000;
const APP_SDK_HARD_MAX_PREDICATE_NODES = 128;
const APP_SDK_HARD_MAX_PREDICATE_DEPTH = 16;
const APP_SDK_HARD_MAX_PAGE_ROWS = 200;
const APP_SDK_HARD_MAX_ENTITY_CHANGE_PAGE_ROWS = 128;
const APP_SDK_HARD_MAX_ITERATION_PAGES = 200;
const APP_SDK_DEFAULT_DEADLINE_MS = 15_000;
const CAPABILITIES_SHAPE_LIMITS = { maxDepth: 32, maxNodes: 20_000 } as const;
const SDK_HARD_RESPONSE_VALIDATION_LIMITS: ResponseValidationLimits = {
  maxCollectionItems: APP_SDK_HARD_MAX_COLLECTION_ITEMS,
  maxPageRows: APP_SDK_HARD_MAX_PAGE_ROWS,
  maxEntityChangePageRows: APP_SDK_HARD_MAX_ENTITY_CHANGE_PAGE_ROWS,
  maxJsonDepth: APP_SDK_HARD_MAX_JSON_DEPTH,
  maxValueBytes: APP_SDK_HARD_MAX_VALUE_BYTES,
  maxValueNodes: APP_SDK_HARD_MAX_VALUE_NODES,
};

export interface MagicianAppsClientOptions {
  /** Absolute Magician origin. A path, query, fragment, or embedded credential is rejected. */
  readonly origin: string | URL;
  /** Injection seam for an authenticated same-origin fetch implementation. */
  readonly fetch?: typeof globalThis.fetch;
  readonly defaultDeadlineMs?: number;
}

export interface AppRequestOptions {
  readonly signal?: AbortSignal;
  readonly deadlineMs?: number;
}

interface InternalRequestOptions extends AppRequestOptions {
  /** Absolute end-to-end expiry retained across negotiation and request encoding. */
  readonly absoluteExpiryMs?: number;
}

export interface AppOutputRequestOptions<Output extends JsonValue> extends AppRequestOptions {
  readonly outputValidator: AppOutputValidator<Output>;
}

type ObjectValue = Record<string, JsonValue>;
type ResponseValidator<T> = (value: JsonValue) => T;

interface OperationDeadline {
  readonly capabilities: AppContractCapabilities;
  readonly expiresAt: number;
  readonly signal?: AbortSignal;
}

function freezeTree<T>(value: T): T {
  if (value === null || typeof value !== "object" || Object.isFrozen(value)) return value;
  for (const child of Object.values(
    value as unknown as Readonly<Record<string, unknown>>,
  )) freezeTree(child);
  Object.freeze(value);
  return value;
}

const SDK_PUBLIC_OPERATIONS = freezeTree(
  JSON.parse(JSON.stringify(APP_SUPPORTED_PUBLIC_OPERATIONS)) as AppPublicOperation[],
) as readonly AppPublicOperation[];
const SDK_PUBLIC_DEPRECATIONS = freezeTree(
  JSON.parse(JSON.stringify(APP_SUPPORTED_PUBLIC_DEPRECATIONS)) as JsonValue[],
) as readonly JsonValue[];

function isObject(value: unknown): value is ObjectValue {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function requiredString(object: ObjectValue, key: string, operationId: AppPublicOperationId): string {
  const value = object[key];
  if (typeof value !== "string" || value.length === 0) {
    throw new MagicianAppsError("decode", operationId, `Response field ${key} must be a non-empty string`);
  }
  return value;
}

function requiredSafeInteger(object: ObjectValue, key: string, operationId: AppPublicOperationId): number {
  const value = object[key];
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    throw new MagicianAppsError("decode", operationId, `Response field ${key} must be a non-negative safe integer`);
  }
  return value as number;
}

function arraysEqual(left: readonly unknown[], right: readonly unknown[]): boolean {
  return left.length === right.length && left.every((value, index) => deepEqual(value, right[index]));
}

function deepEqual(left: unknown, right: unknown): boolean {
  if (left === right) return true;
  if (Array.isArray(left) && Array.isArray(right)) return arraysEqual(left, right);
  if (!isObject(left) || !isObject(right)) return false;
  const leftKeys = Object.keys(left).sort();
  const rightKeys = Object.keys(right).sort();
  return arraysEqual(leftKeys, rightKeys) && leftKeys.every((key) => deepEqual(left[key], right[key]));
}

function assertExactObjectKeys(
  object: ObjectValue,
  required: readonly string[],
  operationId: AppPublicOperationId,
  label: string,
): void {
  const keys = Object.keys(object).sort();
  const expected = [...required].sort();
  if (!arraysEqual(keys, expected)) {
    throw new MagicianAppsError("contract_mismatch", operationId, `${label} has missing or unknown fields`);
  }
}

function assertExactCapabilities(value: JsonValue): AppContractCapabilities {
  const operationId = "contract_capabilities";
  if (!isObject(value)) {
    throw new MagicianAppsError("contract_mismatch", operationId, "Capabilities response must be an object");
  }
  assertExactObjectKeys(value, [
    "schema_version", "contract_version", "supported_protocol_versions",
    "supported_manifest_schema_versions", "supported_manifest_features", "json_schema_dialect",
    "limits", "sdk_compatibility", "deprecations", "operation_inventory_digest", "operations",
  ], operationId, "Capabilities response");
  if (requiredSafeInteger(value, "schema_version", operationId) !== APP_CONTRACT_CAPABILITIES_SCHEMA_VERSION) {
    throw new MagicianAppsError("contract_mismatch", operationId, "Unsupported capabilities schema version");
  }
  if (requiredString(value, "contract_version", operationId) !== APP_SUPPORTED_PUBLIC_CONTRACT_VERSION) {
    throw new MagicianAppsError("contract_mismatch", operationId, "Unsupported supported-public contract version");
  }
  if (requiredString(value, "json_schema_dialect", operationId) !== APP_JSON_SCHEMA_DIALECT) {
    throw new MagicianAppsError("contract_mismatch", operationId, "Unsupported JSON Schema dialect");
  }
  const protocols = value.supported_protocol_versions;
  if (!Array.isArray(protocols) || !arraysEqual(protocols, [APP_DATA_PLANE_PROTOCOL_VERSION])) {
    throw new MagicianAppsError("contract_mismatch", operationId, "Unsupported data-plane protocol set");
  }
  if (!Array.isArray(value.supported_manifest_schema_versions)
    || !arraysEqual(value.supported_manifest_schema_versions, APP_SUPPORTED_MANIFEST_SCHEMA_VERSIONS)
    || !Array.isArray(value.supported_manifest_features)
    || !arraysEqual(value.supported_manifest_features, APP_SUPPORTED_MANIFEST_FEATURES)) {
    throw new MagicianAppsError("contract_mismatch", operationId, "Unsupported manifest schema or feature set");
  }
  const compatibility = value.sdk_compatibility;
  if (!isObject(compatibility)
    || compatibility.policy !== "current_contract_only"
    || compatibility.generated_by_is_authority !== false
    || !Array.isArray(compatibility.supported_contract_versions)
    || !arraysEqual(compatibility.supported_contract_versions, [APP_SUPPORTED_PUBLIC_CONTRACT_VERSION])) {
    throw new MagicianAppsError("contract_mismatch", operationId, "Unsupported SDK compatibility window");
  }
  assertExactObjectKeys(compatibility, [
    "policy", "supported_contract_versions", "generated_by_is_authority",
  ], operationId, "SDK compatibility window");
  if (!Array.isArray(value.deprecations)
    || !deepEqual(value.deprecations, SDK_PUBLIC_DEPRECATIONS)) {
    throw new MagicianAppsError("contract_mismatch", operationId, "Deprecation inventory differs from this SDK");
  }
  const digest = requiredString(value, "operation_inventory_digest", operationId);
  if (!/^blake3:[0-9a-f]{64}$/.test(digest)) {
    throw new MagicianAppsError("contract_mismatch", operationId, "Operation inventory digest is malformed");
  }
  if (!Array.isArray(value.operations) || !deepEqual(value.operations, SDK_PUBLIC_OPERATIONS)) {
    throw new MagicianAppsError("contract_mismatch", operationId, "Operation inventory differs from this SDK");
  }
  const computedDigest = `blake3:${bytesToHex(blake3(new TextEncoder().encode(JSON.stringify(value.operations))))}`;
  if (computedDigest !== digest) {
    throw new MagicianAppsError("contract_mismatch", operationId, "Operation inventory digest does not match its payload");
  }
  const limits = value.limits;
  if (!isObject(limits)) {
    throw new MagicianAppsError("contract_mismatch", operationId, "Capabilities limits are missing");
  }
  const limitKeys = [
    "max_document_bytes", "max_json_depth", "max_json_nodes", "max_value_bytes", "max_value_nodes",
    "max_collection_items", "max_predicate_nodes", "max_predicate_depth", "max_page_rows",
    "max_entity_change_page_rows",
  ] as const;
  assertExactObjectKeys(limits, limitKeys, operationId, "Capabilities limits");
  for (const key of limitKeys) {
    if (requiredSafeInteger(limits, key, operationId) < 1) {
      throw new MagicianAppsError("contract_mismatch", operationId, `Capability limit ${key} must be positive`);
    }
  }
  return freezeTree(value) as unknown as AppContractCapabilities;
}

function normalizeOrigin(value: string | URL): URL {
  const origin = new URL(value.toString());
  if ((origin.protocol !== "http:" && origin.protocol !== "https:")
    || origin.username !== ""
    || origin.password !== ""
    || (origin.pathname !== "/" && origin.pathname !== "")
    || origin.search !== ""
    || origin.hash !== "") {
    throw new TypeError("origin must be an absolute HTTP(S) origin without credentials, path, query, or fragment");
  }
  return origin;
}

function boundedPositiveInteger(value: number | undefined, fallback: number, label: string): number {
  const candidate = value ?? fallback;
  if (!Number.isSafeInteger(candidate) || candidate < 1 || candidate > 120_000) {
    throw new TypeError(`${label} must be a positive integer no greater than 120000`);
  }
  return candidate;
}

function segment(value: string, label: string, maxBytes: number, extended: boolean): string {
  const pattern = extended
    ? /^[A-Za-z0-9][A-Za-z0-9_.:/@#-]*$/
    : /^[A-Za-z0-9][A-Za-z0-9_.-]*$/;
  if (new TextEncoder().encode(value).length > maxBytes || !pattern.test(value)) {
    throw new TypeError(`${label} is not a bounded canonical identifier`);
  }
  return encodeURIComponent(value);
}

function installationSegment(value: string): string {
  return segment(value, "installationId", 128, false);
}

function actionSegment(value: string): string {
  if (new TextEncoder().encode(value).length > 64 || !/^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(value)) {
    throw new TypeError("actionId is not a bounded canonical app name");
  }
  return encodeURIComponent(value);
}

function runSegment(value: string): string {
  return segment(value, "runRef", 192, true);
}

function operationDescriptor(id: AppPublicOperationId): AppPublicOperation {
  const operation = SDK_PUBLIC_OPERATIONS.find((candidate) => candidate.operation_id === id);
  if (operation === undefined) throw new Error(`SDK operation inventory is incomplete: ${id}`);
  return operation;
}

function createDeadlineSignal(
  operationId: AppPublicOperationId,
  callerSignal: AbortSignal | undefined,
  deadlineMs: number,
): { readonly signal: AbortSignal; readonly cleanup: () => void; readonly timedOut: () => boolean } {
  const controller = new AbortController();
  let timeoutFired = false;
  const abortFromCaller = (): void => controller.abort(callerSignal?.reason);
  if (callerSignal?.aborted === true) abortFromCaller();
  else callerSignal?.addEventListener("abort", abortFromCaller, { once: true });
  const timer = setTimeout(() => {
    timeoutFired = true;
    controller.abort(new Error(`${operationId} deadline elapsed`));
  }, deadlineMs);
  return {
    signal: controller.signal,
    cleanup: () => {
      clearTimeout(timer);
      callerSignal?.removeEventListener("abort", abortFromCaller);
    },
    timedOut: () => timeoutFired,
  };
}

function signalIsAborted(signal: AbortSignal | undefined): boolean {
  return signal?.aborted === true;
}

function operationErrorCodes(operationId: AppPublicOperationId): ReadonlySet<string> {
  return new Set(operationDescriptor(operationId).errors);
}

function responseValidationLimits(capabilities: AppContractCapabilities): ResponseValidationLimits {
  return {
    maxCollectionItems: Math.min(capabilities.limits.max_collection_items, APP_SDK_HARD_MAX_COLLECTION_ITEMS),
    maxPageRows: Math.min(capabilities.limits.max_page_rows, APP_SDK_HARD_MAX_PAGE_ROWS),
    maxEntityChangePageRows: Math.min(
      capabilities.limits.max_entity_change_page_rows,
      APP_SDK_HARD_MAX_ENTITY_CHANGE_PAGE_ROWS,
    ),
    maxJsonDepth: Math.min(capabilities.limits.max_json_depth, APP_SDK_HARD_MAX_JSON_DEPTH),
    maxValueBytes: Math.min(capabilities.limits.max_value_bytes, APP_SDK_HARD_MAX_VALUE_BYTES),
    maxValueNodes: Math.min(capabilities.limits.max_value_nodes, APP_SDK_HARD_MAX_VALUE_NODES),
  };
}

interface EffectiveRequestLimits {
  readonly maxDocumentBytes: number;
  readonly maxJsonDepth: number;
  readonly maxJsonNodes: number;
  readonly maxValueBytes: number;
  readonly maxValueNodes: number;
  readonly maxCollectionItems: number;
  readonly maxPredicateNodes: number;
  readonly maxPredicateDepth: number;
  readonly maxPageRows: number;
}

function effectiveRequestLimits(capabilities: AppContractCapabilities): EffectiveRequestLimits {
  return {
    maxDocumentBytes: Math.min(capabilities.limits.max_document_bytes, APP_SDK_HARD_MAX_DOCUMENT_BYTES),
    maxJsonDepth: Math.min(capabilities.limits.max_json_depth, APP_SDK_HARD_MAX_JSON_DEPTH),
    maxJsonNodes: Math.min(capabilities.limits.max_json_nodes, APP_SDK_HARD_MAX_JSON_NODES),
    maxValueBytes: Math.min(capabilities.limits.max_value_bytes, APP_SDK_HARD_MAX_VALUE_BYTES),
    maxValueNodes: Math.min(capabilities.limits.max_value_nodes, APP_SDK_HARD_MAX_VALUE_NODES),
    maxCollectionItems: Math.min(capabilities.limits.max_collection_items, APP_SDK_HARD_MAX_COLLECTION_ITEMS),
    maxPredicateNodes: Math.min(capabilities.limits.max_predicate_nodes, APP_SDK_HARD_MAX_PREDICATE_NODES),
    maxPredicateDepth: Math.min(capabilities.limits.max_predicate_depth, APP_SDK_HARD_MAX_PREDICATE_DEPTH),
    maxPageRows: Math.min(capabilities.limits.max_page_rows, APP_SDK_HARD_MAX_PAGE_ROWS),
  };
}

function inputObject(
  value: unknown,
  operationId: AppPublicOperationId,
  required: readonly string[],
  optional: readonly string[],
  label: string,
): Readonly<Record<string, unknown>> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new MagicianAppsError("contract_mismatch", operationId, `${label} must be an object`);
  }
  const keys = Object.keys(value);
  const allowed = new Set([...required, ...optional]);
  if (keys.some((key) => !allowed.has(key)) || required.some((key) => !Object.hasOwn(value, key))) {
    throw new MagicianAppsError("contract_mismatch", operationId, `${label} has missing or unknown fields`);
  }
  return value as Readonly<Record<string, unknown>>;
}

function inputRecord(
  value: unknown,
  operationId: AppPublicOperationId,
  label: string,
): Readonly<Record<string, unknown>> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new MagicianAppsError("contract_mismatch", operationId, `${label} must be an object`);
  }
  return value as Readonly<Record<string, unknown>>;
}

function inputArray(
  value: unknown,
  operationId: AppPublicOperationId,
  label: string,
  max: number,
  allowEmpty = true,
): readonly unknown[] {
  if (!Array.isArray(value) || value.length > max || (!allowEmpty && value.length === 0)) {
    throw new MagicianAppsError("limit_exceeded", operationId, `${label} is not a bounded array`);
  }
  return value;
}

function inputString(value: unknown, operationId: AppPublicOperationId, label: string, maxBytes: number): string {
  if (typeof value !== "string" || value.length === 0 || new TextEncoder().encode(value).length > maxBytes) {
    throw new MagicianAppsError("contract_mismatch", operationId, `${label} is not a bounded string`);
  }
  return value;
}

function inputReference(value: unknown, operationId: AppPublicOperationId, label: string): string {
  const result = inputString(value, operationId, label, 192);
  if (!/^[A-Za-z0-9][A-Za-z0-9_.:/@#-]*$/.test(result)) {
    throw new MagicianAppsError("contract_mismatch", operationId, `${label} is not a canonical reference`);
  }
  return result;
}

function inputOpaqueId(value: unknown, operationId: AppPublicOperationId, label: string): string {
  const result = inputString(value, operationId, label, 128);
  if (!/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(result)) {
    throw new MagicianAppsError("contract_mismatch", operationId, `${label} is not a canonical opaque id`);
  }
  return result;
}

function inputName(value: unknown, operationId: AppPublicOperationId, label: string): string {
  const result = inputString(value, operationId, label, 64);
  if (!/^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(result)) {
    throw new MagicianAppsError("contract_mismatch", operationId, `${label} is not a canonical app name`);
  }
  return result;
}

function inputFieldPath(value: unknown, operationId: AppPublicOperationId, label: string): string {
  const result = inputString(value, operationId, label, 256);
  const parts = result.split(".");
  if (parts.length > 16 || parts.some((part) => part.length > 64 || !/^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(part))) {
    throw new MagicianAppsError("contract_mismatch", operationId, `${label} is not a canonical field path`);
  }
  return result;
}

function inputSafeInteger(value: unknown, operationId: AppPublicOperationId, label: string, minimum: number): number {
  if (!Number.isSafeInteger(value) || (value as number) < minimum) {
    throw new MagicianAppsError("contract_mismatch", operationId, `${label} must be a safe integer >= ${minimum}`);
  }
  return value as number;
}

function assertUnique(values: readonly string[], operationId: AppPublicOperationId, label: string): void {
  if (new Set(values).size !== values.length) {
    throw new MagicianAppsError("contract_mismatch", operationId, `${label} contains duplicates`);
  }
}

function validatePredicateInput(value: unknown, limits: EffectiveRequestLimits): void {
  const operationId = "query_data";
  const predicate = inputObject(value, operationId, ["root", "nodes"], [], "predicate");
  const nodes = inputArray(predicate.nodes, operationId, "predicate.nodes", limits.maxPredicateNodes, false);
  const root = inputSafeInteger(predicate.root, operationId, "predicate.root", 0);
  if (root >= nodes.length) throw new MagicianAppsError("contract_mismatch", operationId, "predicate.root references a missing node");
  const childIndexes: number[][] = [];
  for (const [index, rawNode] of nodes.entries()) {
    const tagged = inputObject(rawNode, operationId, ["kind"], ["children", "child", "field", "operator", "value", "values", "negated"], `predicate.nodes[${index}]`);
    const kind = tagged.kind;
    const children: number[] = [];
    if (kind === "all" || kind === "any") {
      inputObject(rawNode, operationId, ["kind", "children"], [], `predicate.nodes[${index}]`);
      for (const child of inputArray(tagged.children, operationId, "predicate.children", limits.maxCollectionItems, false)) {
        children.push(inputSafeInteger(child, operationId, "predicate child", 0));
      }
      if (new Set(children).size !== children.length) throw new MagicianAppsError("contract_mismatch", operationId, "predicate children contain duplicates");
    } else if (kind === "not") {
      inputObject(rawNode, operationId, ["kind", "child"], [], `predicate.nodes[${index}]`);
      children.push(inputSafeInteger(tagged.child, operationId, "predicate child", 0));
    } else if (kind === "compare") {
      inputObject(rawNode, operationId, ["kind", "field", "operator", "value"], [], `predicate.nodes[${index}]`);
      inputFieldPath(tagged.field, operationId, "predicate field");
      if (typeof tagged.operator !== "string"
        || !["equal", "not_equal", "less_than", "less_than_or_equal", "greater_than", "greater_than_or_equal", "contains", "starts_with"].includes(tagged.operator)) {
        throw new MagicianAppsError("contract_mismatch", operationId, "predicate operator is unsupported");
      }
      if (tagged.value === null || typeof tagged.value === "object" || !["string", "number", "boolean"].includes(typeof tagged.value)) {
        throw new MagicianAppsError("contract_mismatch", operationId, "predicate compare value must be a non-null scalar");
      }
      if (typeof tagged.value === "string" && new TextEncoder().encode(tagged.value).length > 4_096) {
        throw new MagicianAppsError("limit_exceeded", operationId, "predicate string exceeds 4096 bytes");
      }
      assertBoundedJsonValue(operationId, tagged.value, limits.maxValueBytes, { maxDepth: limits.maxJsonDepth, maxNodes: limits.maxValueNodes });
    } else if (kind === "in") {
      inputObject(rawNode, operationId, ["kind", "field", "values"], [], `predicate.nodes[${index}]`);
      inputFieldPath(tagged.field, operationId, "predicate field");
      for (const scalar of inputArray(tagged.values, operationId, "predicate values", limits.maxCollectionItems, false)) {
        if (scalar === null || typeof scalar === "object" || !["string", "number", "boolean"].includes(typeof scalar)) {
          throw new MagicianAppsError("contract_mismatch", operationId, "predicate in value must be a non-null scalar");
        }
        if (typeof scalar === "string" && new TextEncoder().encode(scalar).length > 4_096) {
          throw new MagicianAppsError("limit_exceeded", operationId, "predicate string exceeds 4096 bytes");
        }
        assertBoundedJsonValue(operationId, scalar, limits.maxValueBytes, { maxDepth: limits.maxJsonDepth, maxNodes: limits.maxValueNodes });
      }
    } else if (kind === "is_null") {
      inputObject(rawNode, operationId, ["kind", "field"], ["negated"], `predicate.nodes[${index}]`);
      inputFieldPath(tagged.field, operationId, "predicate field");
      if (tagged.negated !== undefined && typeof tagged.negated !== "boolean") {
        throw new MagicianAppsError("contract_mismatch", operationId, "predicate negated must be boolean");
      }
    } else {
      throw new MagicianAppsError("contract_mismatch", operationId, "predicate kind is unsupported");
    }
    if (children.some((child) => child >= nodes.length)) {
      throw new MagicianAppsError("contract_mismatch", operationId, "predicate references a missing node");
    }
    childIndexes.push(children);
  }
  const colors = new Array<number>(nodes.length).fill(0);
  const depths = new Array<number>(nodes.length).fill(0);
  colors[root] = 1;
  const stack: Array<readonly [number, number]> = [[root, 0]];
  while (stack.length > 0) {
    const frame = stack.pop();
    if (frame === undefined) break;
    const [index, nextChild] = frame;
    const children = childIndexes[index];
    if (children === undefined) throw new MagicianAppsError("contract_mismatch", operationId, "predicate references a missing node");
    if (nextChild >= children.length) {
      let maxChildDepth = 0;
      for (const child of children) maxChildDepth = Math.max(maxChildDepth, depths[child] ?? 0);
      const depth = maxChildDepth + 1;
      if (depth > limits.maxPredicateDepth) throw new MagicianAppsError("limit_exceeded", operationId, "predicate depth exceeds negotiated capability");
      depths[index] = depth;
      colors[index] = 2;
      continue;
    }
    const child = children[nextChild];
    if (child === undefined) throw new MagicianAppsError("contract_mismatch", operationId, "predicate references a missing node");
    stack.push([index, nextChild + 1]);
    if (colors[child] === 0) {
      colors[child] = 1;
      stack.push([child, 0]);
    } else if (colors[child] === 1) {
      throw new MagicianAppsError("contract_mismatch", operationId, "predicate contains a cycle");
    }
  }
  if (colors.some((color) => color === 0)) throw new MagicianAppsError("contract_mismatch", operationId, "predicate contains unreachable nodes");
}

function validateQueryInput(request: AppQueryRequest, installationId: string, limits: EffectiveRequestLimits): void {
  const operationId = "query_data";
  assertBoundedJsonValue(operationId, request, limits.maxDocumentBytes, {
    maxDepth: limits.maxJsonDepth,
    maxNodes: limits.maxJsonNodes,
  });
  const object = inputObject(request, operationId, ["protocol_version", "source_installation_id", "entity", "select", "limit", "purpose"], ["predicate", "order", "cursor", "relation_expansions", "pagination"], "query request");
  if (object.protocol_version !== APP_DATA_PLANE_PROTOCOL_VERSION || object.source_installation_id !== installationId) {
    throw new MagicianAppsError("contract_mismatch", operationId, "Query route, source installation, or protocol differs");
  }
  if (object.pagination !== undefined && object.pagination !== "snapshot" && object.pagination !== "keyset") throw new MagicianAppsError("contract_mismatch", operationId, "unsupported pagination mode");
  inputName(object.entity, operationId, "query entity");
  inputName(object.purpose, operationId, "query purpose");
  const select = inputArray(object.select, operationId, "query select", limits.maxCollectionItems, false)
    .map((field) => inputFieldPath(field, operationId, "query select field"));
  assertUnique(select, operationId, "query select");
  const orderFields = inputArray(object.order ?? [], operationId, "query order", limits.maxCollectionItems)
    .map((entry, index) => {
      const order = inputObject(entry, operationId, ["field", "direction"], [], `query order[${index}]`);
      if (order.direction !== "ascending" && order.direction !== "descending") throw new MagicianAppsError("contract_mismatch", operationId, "query order direction is unsupported");
      return inputFieldPath(order.field, operationId, "query order field");
    });
  assertUnique(orderFields, operationId, "query order fields");
  const relationNames = inputArray(object.relation_expansions ?? [], operationId, "relation expansions", limits.maxCollectionItems)
    .map((entry, index) => {
      const expansion = inputObject(entry, operationId, ["relation", "max_depth", "max_rows"], ["select"], `relation expansion[${index}]`);
      const relation = inputName(expansion.relation, operationId, "relation expansion relation");
      const fields = inputArray(expansion.select ?? [], operationId, "relation expansion select", limits.maxCollectionItems, false)
        .map((field) => inputFieldPath(field, operationId, "relation expansion field"));
      assertUnique(fields, operationId, "relation expansion select");
      if (inputSafeInteger(expansion.max_depth, operationId, "relation max_depth", 1) > limits.maxPredicateDepth
        || inputSafeInteger(expansion.max_rows, operationId, "relation max_rows", 1) > limits.maxPageRows) {
        throw new MagicianAppsError("limit_exceeded", operationId, "relation expansion exceeds negotiated capability");
      }
      return relation;
    });
  assertUnique(relationNames, operationId, "relation expansions");
  if (object.predicate !== undefined) validatePredicateInput(object.predicate, limits);
  if (object.cursor !== undefined) inputReference(object.cursor, operationId, "query cursor");
  const limit = inputSafeInteger(object.limit, operationId, "query limit", 1);
  if (limit > limits.maxPageRows) throw new MagicianAppsError("limit_exceeded", operationId, "Query limit exceeds negotiated capability");
}

function validateMutationInput(command: AppMutationCommand, limits: EffectiveRequestLimits): void {
  const operationId = "mutate_data";
  assertBoundedJsonValue(operationId, command, limits.maxDocumentBytes, {
    maxDepth: limits.maxJsonDepth,
    maxNodes: limits.maxJsonNodes,
  });
  const object = inputObject(command, operationId, ["protocol_version", "idempotency_key", "atomicity", "expected_schema_revision", "operations"], ["expected_record_revisions"], "mutation command");
  if (object.protocol_version !== APP_DATA_PLANE_PROTOCOL_VERSION || object.atomicity !== "all_or_nothing") {
    throw new MagicianAppsError("contract_mismatch", operationId, "Mutation protocol or atomicity differs");
  }
  inputReference(object.idempotency_key, operationId, "mutation idempotency_key");
  inputSafeInteger(object.expected_schema_revision, operationId, "expected_schema_revision", 1);
  const expected = inputArray(object.expected_record_revisions ?? [], operationId, "expected_record_revisions", limits.maxCollectionItems);
  const expectedIdentities = new Set<string>();
  for (const [index, entry] of expected.entries()) {
    const row = inputObject(entry, operationId, ["entity", "record_id", "revision"], [], `expected_record_revisions[${index}]`);
    const identity = `${inputName(row.entity, operationId, "expected entity")}\0${inputOpaqueId(row.record_id, operationId, "expected record_id")}`;
    inputSafeInteger(row.revision, operationId, "expected revision", 1);
    if (expectedIdentities.has(identity)) throw new MagicianAppsError("contract_mismatch", operationId, "expected_record_revisions contains duplicates");
    expectedIdentities.add(identity);
  }
  const operations = inputArray(object.operations, operationId, "mutation operations", limits.maxCollectionItems, false);
  const temporaryIds = new Set<string>();
  const touchedRecords = new Set<string>();
  const touchedRelations = new Set<string>();
  let aggregateValueBytes = 0;
  let aggregateValueNodes = 0;
  for (const [index, entry] of operations.entries()) {
    const tagged = inputObject(entry, operationId, ["kind"], ["entity", "temporary_id", "payload", "record_id", "patch", "relation", "from_record_id", "to_record_id", "expected_from_revision", "expected_to_revision"], `mutation operation[${index}]`);
    if (tagged.kind === "create") {
      inputObject(entry, operationId, ["kind", "entity", "temporary_id", "payload"], [], `mutation operation[${index}]`);
      inputName(tagged.entity, operationId, "create entity");
      const temporaryId = inputName(tagged.temporary_id, operationId, "create temporary_id");
      if (temporaryIds.has(temporaryId)) throw new MagicianAppsError("contract_mismatch", operationId, "create temporary_id is duplicated");
      temporaryIds.add(temporaryId);
      if (tagged.payload === null || typeof tagged.payload !== "object" || Array.isArray(tagged.payload)) throw new MagicianAppsError("contract_mismatch", operationId, "create payload must be an object");
      const metrics = boundedJsonMetrics(operationId, tagged.payload, limits.maxValueBytes, { maxDepth: limits.maxJsonDepth, maxNodes: limits.maxValueNodes });
      aggregateValueBytes += metrics.bytes;
      aggregateValueNodes += metrics.nodes;
    } else if (tagged.kind === "update") {
      inputObject(entry, operationId, ["kind", "entity", "record_id", "patch"], [], `mutation operation[${index}]`);
      const identity = `${inputName(tagged.entity, operationId, "update entity")}\0${inputOpaqueId(tagged.record_id, operationId, "update record_id")}`;
      if (touchedRecords.has(identity)) throw new MagicianAppsError("contract_mismatch", operationId, "existing record is mutated more than once");
      touchedRecords.add(identity);
      if (tagged.patch === null || typeof tagged.patch !== "object" || Array.isArray(tagged.patch) || Object.keys(tagged.patch).length === 0) throw new MagicianAppsError("contract_mismatch", operationId, "update patch must be a non-empty object");
      const metrics = boundedJsonMetrics(operationId, tagged.patch, limits.maxValueBytes, { maxDepth: limits.maxJsonDepth, maxNodes: limits.maxValueNodes });
      aggregateValueBytes += metrics.bytes;
      aggregateValueNodes += metrics.nodes;
    } else if (tagged.kind === "delete" || tagged.kind === "restore") {
      inputObject(entry, operationId, ["kind", "entity", "record_id"], [], `mutation operation[${index}]`);
      const identity = `${inputName(tagged.entity, operationId, "mutation entity")}\0${inputOpaqueId(tagged.record_id, operationId, "mutation record_id")}`;
      if (touchedRecords.has(identity)) throw new MagicianAppsError("contract_mismatch", operationId, "existing record is mutated more than once");
      touchedRecords.add(identity);
    } else if (tagged.kind === "create_relation" || tagged.kind === "delete_relation") {
      inputObject(entry, operationId, ["kind", "relation", "from_record_id", "to_record_id", "expected_from_revision", "expected_to_revision"], [], `mutation operation[${index}]`);
      const relation = inputName(tagged.relation, operationId, "relation name");
      const from = inputOpaqueId(tagged.from_record_id, operationId, "relation from_record_id");
      const to = inputOpaqueId(tagged.to_record_id, operationId, "relation to_record_id");
      inputSafeInteger(tagged.expected_from_revision, operationId, "relation expected_from_revision", 1);
      inputSafeInteger(tagged.expected_to_revision, operationId, "relation expected_to_revision", 1);
      if (from === to) throw new MagicianAppsError("contract_mismatch", operationId, "relation endpoints must differ");
      const identity = `${relation}\0${from}\0${to}`;
      if (touchedRelations.has(identity)) throw new MagicianAppsError("contract_mismatch", operationId, "relation edge is mutated more than once");
      touchedRelations.add(identity);
    } else {
      throw new MagicianAppsError("contract_mismatch", operationId, "mutation operation kind is unsupported");
    }
  }
  if (aggregateValueBytes > limits.maxValueBytes || aggregateValueNodes > limits.maxValueNodes) {
    throw new MagicianAppsError("limit_exceeded", operationId, "mutation payloads exceed the aggregate value ceiling");
  }
  if (expectedIdentities.size !== touchedRecords.size || [...touchedRecords].some((identity) => !expectedIdentities.has(identity))) {
    throw new MagicianAppsError("contract_mismatch", operationId, "expected_record_revisions must exactly cover existing-record mutations");
  }
}

function validateActionInput(request: AppDirectActionRequest, limits: EffectiveRequestLimits): void {
  const operationId = "launch_action";
  assertBoundedJsonValue(operationId, request, limits.maxDocumentBytes, {
    maxDepth: limits.maxJsonDepth,
    maxNodes: limits.maxJsonNodes,
  });
  const object = inputObject(request, operationId, ["idempotency_key", "input"], [], "action request");
  inputReference(object.idempotency_key, operationId, "action idempotency_key");
  assertBoundedJsonValue(operationId, object.input, limits.maxValueBytes, {
    maxDepth: limits.maxJsonDepth,
    maxNodes: limits.maxValueNodes,
  });
}

function validateActionCancellationInput(request: AppActionCancellationRequest): void {
  const operationId = "cancel_action_run";
  const object = inputObject(
    request,
    operationId,
    ["expected_generation", "idempotency_key"],
    [],
    "action cancellation request",
  );
  inputSafeInteger(object.expected_generation, operationId, "expected_generation", 0);
  inputReference(object.idempotency_key, operationId, "idempotency_key");
}

function validateActionCompositionInput(
  request: AppActionCompositionRequest,
  limits: EffectiveRequestLimits,
): void {
  const operationId = "compose_action_run";
  assertBoundedJsonValue(operationId, request, limits.maxDocumentBytes, {
    maxDepth: limits.maxJsonDepth,
    maxNodes: limits.maxJsonNodes,
  });
  const object = inputObject(request, operationId, [
    "destination_installation_id", "destination_action_id", "mapping", "idempotency_key",
  ], ["chain", "subscription"], "action composition request");
  inputOpaqueId(object.destination_installation_id, operationId, "destination installation_id");
  inputName(object.destination_action_id, operationId, "destination action_id");
  inputReference(object.idempotency_key, operationId, "composition idempotency_key");
  const mappings = inputArray(object.mapping, operationId, "composition mapping", limits.maxCollectionItems, false);
  const targets = new Set<string>();
  let aggregateValueBytes = 0;
  let aggregateValueNodes = 0;
  for (const [index, entry] of mappings.entries()) {
    const tagged = inputObject(entry, operationId, ["kind", "target"], ["source", "value", "conversion", "values"], `composition mapping[${index}]`);
    const target = inputFieldPath(tagged.target, operationId, "composition target");
    if (targets.has(target)) throw new MagicianAppsError("contract_mismatch", operationId, "composition mapping targets must be unique");
    targets.add(target);
    if (tagged.kind === "select") {
      inputObject(entry, operationId, ["kind", "source", "target"], [], `composition mapping[${index}]`);
      inputFieldPath(tagged.source, operationId, "composition source");
    } else if (tagged.kind === "constant") {
      inputObject(entry, operationId, ["kind", "target", "value"], [], `composition mapping[${index}]`);
      const metrics = boundedJsonMetrics(operationId, tagged.value, limits.maxValueBytes, {
        maxDepth: limits.maxJsonDepth,
        maxNodes: limits.maxValueNodes,
      });
      aggregateValueBytes += metrics.bytes;
      aggregateValueNodes += metrics.nodes;
    } else if (tagged.kind === "convert") {
      inputObject(entry, operationId, ["kind", "source", "target", "conversion"], [], `composition mapping[${index}]`);
      inputFieldPath(tagged.source, operationId, "composition source");
      if (!["integer_to_decimal", "text_to_markdown", "text_to_timestamp"].includes(String(tagged.conversion))) {
        throw new MagicianAppsError("contract_mismatch", operationId, "composition conversion is unsupported");
      }
    } else if (tagged.kind === "map_enum") {
      inputObject(entry, operationId, ["kind", "source", "target", "values"], [], `composition mapping[${index}]`);
      inputFieldPath(tagged.source, operationId, "composition source");
      const values = inputRecord(tagged.values, operationId, "composition enum map");
      const entries = Object.entries(values);
      if (entries.length === 0 || entries.length > limits.maxCollectionItems) {
        throw new MagicianAppsError("limit_exceeded", operationId, "composition enum map exceeds the collection limit");
      }
      for (const [source, destination] of entries) {
        inputName(source, operationId, "composition enum source");
        inputName(destination, operationId, "composition enum destination");
      }
    } else {
      throw new MagicianAppsError("contract_mismatch", operationId, "composition mapping kind is unsupported");
    }
  }
  if (aggregateValueBytes > limits.maxValueBytes || aggregateValueNodes > limits.maxValueNodes) {
    throw new MagicianAppsError("limit_exceeded", operationId, "composition constants exceed the aggregate value ceiling");
  }
  const idempotencyKeys = new Set([String(object.idempotency_key)]);
  const destinationInstallations = new Set([String(object.destination_installation_id)]);
  if (object.chain !== undefined) {
    const chain = inputArray(object.chain, operationId, "composition chain", 2, true);
    for (const [index, hop] of chain.entries()) {
      const parsed = inputObject(hop, operationId, [
        "destination_installation_id", "destination_action_id", "mapping", "idempotency_key",
      ], [], `composition chain[${index}]`);
      const key = inputReference(parsed.idempotency_key, operationId, `composition chain[${index}] idempotency_key`);
      if (idempotencyKeys.has(key)) {
        throw new MagicianAppsError("contract_mismatch", operationId, "composition hop idempotency keys must be unique");
      }
      idempotencyKeys.add(key);
      const destination = String(parsed.destination_installation_id);
      if (destinationInstallations.has(destination)) {
        throw new MagicianAppsError("contract_mismatch", operationId, "composition destination installations must be unique");
      }
      destinationInstallations.add(destination);
      validateActionCompositionInput(parsed as unknown as AppActionCompositionRequest, limits);
    }
  }
  if (object.subscription !== undefined) {
    const subscription = inputObject(object.subscription, operationId, [], ["cursor", "limit"], "composition subscription");
    if (subscription.cursor !== undefined) inputReference(subscription.cursor, operationId, "composition subscription cursor");
    if (subscription.limit !== undefined
      && inputSafeInteger(subscription.limit, operationId, "composition subscription limit", 1) > 8) {
      throw new MagicianAppsError("limit_exceeded", operationId, "composition subscription limit exceeds 8");
    }
  }
}

function assertObjectResponse(value: JsonValue, operationId: AppPublicOperationId): ObjectValue {
  if (!isObject(value)) throw new MagicianAppsError("decode", operationId, "Response must be a JSON object");
  return value;
}

export class MagicianAppsClient {
  readonly #fetch: typeof globalThis.fetch;
  readonly #apiBase: URL;
  readonly #defaultDeadlineMs: number;
  #capabilities: AppContractCapabilities | undefined;

  constructor(options: MagicianAppsClientOptions) {
    const origin = normalizeOrigin(options.origin);
    this.#apiBase = new URL(APP_SUPPORTED_PUBLIC_API_PREFIX.replace(/^\//, ""), origin);
    this.#fetch = options.fetch ?? globalThis.fetch;
    if (typeof this.#fetch !== "function") throw new TypeError("A fetch implementation is required");
    this.#defaultDeadlineMs = boundedPositiveInteger(options.defaultDeadlineMs, APP_SDK_DEFAULT_DEADLINE_MS, "defaultDeadlineMs");
  }

  async connect(options: AppRequestOptions = {}): Promise<AppContractCapabilities> {
    if (this.#capabilities !== undefined) return this.#capabilities;
    // Pre-success callers negotiate independently so one caller's cancellation
    // or deadline can never cancel or silently replace another caller's fence.
    const capabilities = await this.#loadCapabilities(options);
    this.#capabilities ??= capabilities;
    return this.#capabilities;
  }

  async queryData(installationId: string, request: AppQueryRequest, options: AppRequestOptions = {}): Promise<AppQueryPage> {
    const deadline = await this.#negotiateFor("query_data", options);
    const limits = effectiveRequestLimits(deadline.capabilities);
    validateQueryInput(request, installationId, limits);
    return this.#request(
      "query_data",
      `/installations/${installationSegment(installationId)}/data/query`,
      request,
      this.#remainingOptions("query_data", deadline),
      deadline.capabilities,
      (value) => validateQueryPage(
        value,
        installationId,
        request,
        responseValidationLimits(deadline.capabilities),
      ),
    );
  }

  async mutateData(installationId: string, command: AppMutationCommand, options: AppRequestOptions = {}): Promise<AppMutationReceipt> {
    const deadline = await this.#negotiateFor("mutate_data", options);
    validateMutationInput(command, effectiveRequestLimits(deadline.capabilities));
    return this.#request(
      "mutate_data",
      `/installations/${installationSegment(installationId)}/data/mutations`,
      command,
      this.#remainingOptions("mutate_data", deadline),
      deadline.capabilities,
      (value) => validateMutationReceipt(
        value,
        installationId,
        command.idempotency_key,
        responseValidationLimits(deadline.capabilities),
      ),
    );
  }

  launchAction<Input extends JsonValue, Output extends JsonValue>(
    installationId: string,
    actionId: string,
    request: AppDirectActionRequest<Input>,
    options: AppOutputRequestOptions<Output>,
  ): Promise<AppActionLaunchResponse<Output>>;
  launchAction<Input extends JsonValue = JsonValue>(
    installationId: string,
    actionId: string,
    request: AppDirectActionRequest<Input>,
    options?: AppRequestOptions,
  ): Promise<AppActionLaunchResponse<JsonValue>>;
  async launchAction<Input extends JsonValue = JsonValue>(
    installationId: string,
    actionId: string,
    request: AppDirectActionRequest<Input>,
    options: AppRequestOptions & { readonly outputValidator?: AppOutputValidator<JsonValue> } = {},
  ): Promise<AppActionLaunchResponse<JsonValue>> {
    const deadline = await this.#negotiateFor("launch_action", options);
    validateActionInput(request, effectiveRequestLimits(deadline.capabilities));
    const outputValidator = options.outputValidator ?? ((value: JsonValue): value is JsonValue => true);
    return this.#request(
      "launch_action",
      `/installations/${installationSegment(installationId)}/actions/${actionSegment(actionId)}/runs`,
      request,
      this.#remainingOptions("launch_action", deadline),
      deadline.capabilities,
      (value) => validateActionLaunch(
        value,
        installationId,
        actionId,
        responseValidationLimits(deadline.capabilities),
        outputValidator,
      ),
    );
  }

  getActionRun<Output extends JsonValue>(runRef: string, options: AppOutputRequestOptions<Output>): Promise<AppRunSnapshot<Output>>;
  getActionRun(runRef: string, options?: AppRequestOptions): Promise<AppRunSnapshot<JsonValue>>;
  async getActionRun(
    runRef: string,
    options: AppRequestOptions & { readonly outputValidator?: AppOutputValidator<JsonValue> } = {},
  ): Promise<AppRunSnapshot<JsonValue>> {
    if (!runRef.startsWith(RUN_REF_PREFIX) || runRef.length === RUN_REF_PREFIX.length) {
      throw new TypeError("runRef must use the canonical run:app-action: namespace");
    }
    const deadline = await this.#negotiateFor("get_action_run", options);
    const outputValidator = options.outputValidator ?? ((value: JsonValue): value is JsonValue => true);
    return this.#request(
      "get_action_run",
      `/action-runs/${runSegment(runRef)}`,
      undefined,
      this.#remainingOptions("get_action_run", deadline),
      deadline.capabilities,
      (value) => validateRunSnapshot(
        value,
        runRef,
        responseValidationLimits(deadline.capabilities),
        outputValidator,
      ),
    );
  }

  composeActionRun<Output extends JsonValue>(
    sourceRunRef: string,
    request: AppActionCompositionRequest,
    options: AppOutputRequestOptions<Output>,
  ): Promise<AppActionResultComposition<Output>>;
  composeActionRun(
    sourceRunRef: string,
    request: AppActionCompositionRequest,
    options?: AppRequestOptions,
  ): Promise<AppActionResultComposition<JsonValue>>;
  async composeActionRun(
    sourceRunRef: string,
    request: AppActionCompositionRequest,
    options: AppRequestOptions & { readonly outputValidator?: AppOutputValidator<JsonValue> } = {},
  ): Promise<AppActionResultComposition<JsonValue>> {
    if (!sourceRunRef.startsWith(RUN_REF_PREFIX) || sourceRunRef.length === RUN_REF_PREFIX.length) {
      throw new TypeError("sourceRunRef must use the canonical run:app-action: namespace");
    }
    const deadline = await this.#negotiateFor("compose_action_run", options);
    validateActionCompositionInput(request, effectiveRequestLimits(deadline.capabilities));
    const outputValidator = options.outputValidator ?? ((value: JsonValue): value is JsonValue => true);
    return this.#request(
      "compose_action_run",
      `/action-runs/${runSegment(sourceRunRef)}/compositions`,
      request,
      this.#remainingOptions("compose_action_run", deadline),
      deadline.capabilities,
      (value) => validateActionComposition(
        value,
        sourceRunRef,
        request,
        responseValidationLimits(deadline.capabilities),
        outputValidator,
      ),
    );
  }

  async cancelActionRun(
    runRef: string,
    request: AppActionCancellationRequest,
    options: AppRequestOptions = {},
  ): Promise<AppActionCancellationReceipt> {
    if (!runRef.startsWith(RUN_REF_PREFIX) || runRef.length === RUN_REF_PREFIX.length) {
      throw new TypeError("runRef must use the canonical run:app-action: namespace");
    }
    validateActionCancellationInput(request);
    const deadline = await this.#negotiateFor("cancel_action_run", options);
    return this.#request(
      "cancel_action_run",
      `/action-runs/${runSegment(runRef)}/cancel`,
      request,
      this.#remainingOptions("cancel_action_run", deadline),
      deadline.capabilities,
      (value) => validateActionCancellationReceipt(value, runRef, request.idempotency_key),
    );
  }

  waitForRun<Output extends JsonValue>(
    runRef: string,
    options: WaitForRunOptions & { readonly outputValidator: AppOutputValidator<Output> },
  ): Promise<AppRunSnapshot<Output>>;
  waitForRun(runRef: string, options?: WaitForRunOptions): Promise<AppRunSnapshot<JsonValue>>;
  async waitForRun(
    runRef: string,
    options: WaitForRunOptions & { readonly outputValidator?: AppOutputValidator<JsonValue> } = {},
  ): Promise<AppRunSnapshot<JsonValue>> {
    const deadlineMs = boundedPositiveInteger(options.deadlineMs, this.#defaultDeadlineMs, "deadlineMs");
    const pollIntervalMs = boundedPositiveInteger(options.pollIntervalMs, 250, "pollIntervalMs");
    const startedAt = Date.now();
    for (;;) {
      const remaining = deadlineMs - (Date.now() - startedAt);
      if (remaining <= 0) throw new MagicianAppsError("timeout", "get_action_run", "Run wait deadline elapsed");
      const requestOptions: AppRequestOptions = {
        deadlineMs: remaining,
        ...(options.signal === undefined ? {} : { signal: options.signal }),
      };
      const snapshot = options.outputValidator === undefined
        ? await this.getActionRun(runRef, requestOptions)
        : await this.getActionRun(runRef, {
            ...requestOptions,
            outputValidator: options.outputValidator,
          });
      if (snapshot.terminal) return snapshot;
      const afterRequestRemaining = deadlineMs - (Date.now() - startedAt);
      if (afterRequestRemaining <= 0) {
        throw new MagicianAppsError("timeout", "get_action_run", "Run wait deadline elapsed");
      }
      await new Promise<void>((resolve, reject) => {
        const finish = (): void => {
          options.signal?.removeEventListener("abort", abort);
          resolve();
        };
        const timer = setTimeout(finish, Math.min(pollIntervalMs, afterRequestRemaining));
        const abort = (): void => {
          clearTimeout(timer);
          options.signal?.removeEventListener("abort", abort);
          reject(new MagicianAppsError("cancelled", "get_action_run", "Run wait was cancelled"));
        };
        if (options.signal?.aborted === true) abort();
        else options.signal?.addEventListener("abort", abort, { once: true });
      });
    }
  }

  async readEntityChanges(installationId: string, options: ReadEntityChangesOptions): Promise<AppEntityChangeBatch> {
    const deadline = await this.#negotiateFor("read_entity_changes", options);
    const capabilities = deadline.capabilities;
    const after = options.afterChangeSequence ?? 0;
    const limit = options.limit ?? Math.min(64, capabilities.limits.max_entity_change_page_rows, APP_SDK_HARD_MAX_ENTITY_CHANGE_PAGE_ROWS);
    if (!Number.isSafeInteger(options.surfaceRevision) || options.surfaceRevision < 1
      || !Number.isSafeInteger(after) || after < 0
      || !Number.isSafeInteger(limit) || limit < 1
      || limit > Math.min(capabilities.limits.max_entity_change_page_rows, APP_SDK_HARD_MAX_ENTITY_CHANGE_PAGE_ROWS)) {
      throw new MagicianAppsError("limit_exceeded", "read_entity_changes", "Entity-change cursor, revision, or limit is invalid");
    }
    const query = new URLSearchParams({
      after_change_sequence: String(after),
      surface_revision: String(options.surfaceRevision),
      limit: String(limit),
    });
    return this.#request(
      "read_entity_changes",
      `/installations/${installationSegment(installationId)}/entity-changes?${query.toString()}`,
      undefined,
      this.#remainingOptions("read_entity_changes", deadline),
      capabilities,
      (value) => validateEntityChangeBatch(
        value,
        installationId,
        options.surfaceRevision,
        after,
        responseValidationLimits(capabilities),
      ),
    );
  }

  async *iterateEntityChanges(installationId: string, options: IterateEntityChangesOptions): AsyncGenerator<AppEntityChangeBatch, void, void> {
    const maxPages = options.maxPages ?? APP_SDK_HARD_MAX_ITERATION_PAGES;
    if (!Number.isSafeInteger(maxPages) || maxPages < 1 || maxPages > APP_SDK_HARD_MAX_ITERATION_PAGES) {
      throw new TypeError(`maxPages must be a positive integer no greater than ${APP_SDK_HARD_MAX_ITERATION_PAGES}`);
    }
    const deadlineMs = boundedPositiveInteger(options.deadlineMs, this.#defaultDeadlineMs, "deadlineMs");
    const startedAt = Date.now();
    let after = options.afterChangeSequence ?? 0;
    for (let page = 0; page < maxPages; page += 1) {
      const remaining = deadlineMs - (Date.now() - startedAt);
      if (remaining <= 0) throw new MagicianAppsError("timeout", "read_entity_changes", "Entity-change iteration deadline elapsed");
      const batch = await this.readEntityChanges(installationId, {
        surfaceRevision: options.surfaceRevision,
        afterChangeSequence: after,
        ...(options.limit === undefined ? {} : { limit: options.limit }),
        ...(options.signal === undefined ? {} : { signal: options.signal }),
        deadlineMs: remaining,
      });
      yield batch;
      if (!batch.has_more || batch.reset_required) return;
      if (batch.through_change_sequence <= after) {
        throw new MagicianAppsError("decode", "read_entity_changes", "Entity-change pagination made no progress");
      }
      after = batch.through_change_sequence;
    }
    throw new MagicianAppsError("limit_exceeded", "read_entity_changes", "Entity-change page ceiling exceeded");
  }

  async #loadCapabilities(options: AppRequestOptions): Promise<AppContractCapabilities> {
    return this.#fetchJson(
      "contract_capabilities",
      "/contract-capabilities",
      undefined,
      options,
      APP_CONTRACT_CAPABILITIES_MAX_BYTES,
      CAPABILITIES_SHAPE_LIMITS,
      SDK_HARD_RESPONSE_VALIDATION_LIMITS,
      assertExactCapabilities,
    );
  }

  async #negotiateFor(
    operationId: Exclude<AppPublicOperationId, "contract_capabilities">,
    options: AppRequestOptions,
  ): Promise<OperationDeadline> {
    const deadlineMs = boundedPositiveInteger(options.deadlineMs, this.#defaultDeadlineMs, "deadlineMs");
    const expiresAt = Date.now() + deadlineMs;
    const capabilities = await this.connect({
      deadlineMs,
      ...(options.signal === undefined ? {} : { signal: options.signal }),
    });
    if (Date.now() >= expiresAt) {
      throw new MagicianAppsError("timeout", operationId, "End-to-end request deadline elapsed during contract negotiation");
    }
    return {
      capabilities,
      expiresAt,
      ...(options.signal === undefined ? {} : { signal: options.signal }),
    };
  }

  #remainingOptions(operationId: AppPublicOperationId, deadline: OperationDeadline): InternalRequestOptions {
    const remaining = deadline.expiresAt - Date.now();
    if (remaining <= 0) throw new MagicianAppsError("timeout", operationId, "End-to-end request deadline elapsed");
    return {
      deadlineMs: remaining,
      absoluteExpiryMs: deadline.expiresAt,
      ...(deadline.signal === undefined ? {} : { signal: deadline.signal }),
    };
  }

  async #request<T = JsonValue>(
    operationId: Exclude<AppPublicOperationId, "contract_capabilities">,
    path: string,
    body: unknown,
    options: InternalRequestOptions,
    capabilities: AppContractCapabilities,
    validator?: ResponseValidator<T>,
  ): Promise<T> {
    const limits = effectiveRequestLimits(capabilities);
    return this.#fetchJson(operationId, path, body, options, limits.maxDocumentBytes, {
      maxDepth: limits.maxJsonDepth,
      maxNodes: limits.maxJsonNodes,
    }, responseValidationLimits(capabilities), validator);
  }

  async #fetchJson<T = JsonValue>(
    operationId: AppPublicOperationId,
    path: string,
    body: unknown,
    options: InternalRequestOptions,
    maxBytes: number,
    shapeLimits: { readonly maxDepth: number; readonly maxNodes: number },
    errorValidationLimits: ResponseValidationLimits,
    validator?: ResponseValidator<T>,
  ): Promise<T> {
    const operation = operationDescriptor(operationId);
    const deadlineMs = boundedPositiveInteger(options.deadlineMs, this.#defaultDeadlineMs, "deadlineMs");
    const absoluteExpiryMs = options.absoluteExpiryMs ?? Date.now() + deadlineMs;
    const url = new URL(`${APP_SUPPORTED_PUBLIC_API_PREFIX}${path}`, this.#apiBase.origin);
    const requestBody = body === undefined ? undefined : encodeBoundedJson(operationId, body, maxBytes, shapeLimits);
    const remainingAfterEncode = absoluteExpiryMs - Date.now();
    if (remainingAfterEncode <= 0) {
      throw new MagicianAppsError("timeout", operationId, "End-to-end request deadline elapsed before dispatch");
    }
    if (signalIsAborted(options.signal)) {
      throw new MagicianAppsError("cancelled", operationId, "Request was cancelled before dispatch");
    }
    const deadline = createDeadlineSignal(
      operationId,
      options.signal,
      Math.min(deadlineMs, remainingAfterEncode),
    );
    let requestMayHaveStarted = false;
    try {
      if (signalIsAborted(options.signal)) {
        throw new MagicianAppsError("cancelled", operationId, "Request was cancelled before dispatch");
      }
      if (Date.now() >= absoluteExpiryMs) {
        throw new MagicianAppsError("timeout", operationId, "End-to-end request deadline elapsed before dispatch");
      }
      requestMayHaveStarted = true;
      const response = await this.#fetch(url, {
        method: operation.method,
        headers: requestBody === undefined
          ? { accept: "application/json" }
          : { accept: "application/json", "content-type": "application/json" },
        ...(requestBody === undefined ? {} : { body: new Uint8Array(requestBody).buffer }),
        signal: deadline.signal,
        credentials: "same-origin",
        redirect: "error",
        referrerPolicy: "no-referrer",
        cache: "no-store",
        mode: "same-origin",
      });
      if (deadline.timedOut()) throw new MagicianAppsError("timeout", operationId, "Request deadline elapsed");
      if (signalIsAborted(options.signal)) throw new MagicianAppsError("cancelled", operationId, "Request was cancelled");
      if (!operation.success_statuses.includes(response.status)) {
        const value = await decodeBoundedJsonResponse(operationId, response, Math.min(ERROR_BODY_MAX_BYTES, maxBytes), shapeLimits);
        const envelope = validateErrorEnvelope(value, operationId, errorValidationLimits);
        if (envelope.details?.operation_id !== operationId) {
          throw new MagicianAppsError("decode", operationId, "Error envelope is missing its exact operation correlation");
        }
        const reason = envelope.details?.reason;
        const reasonCode = typeof reason === "string" && operationErrorCodes(operationId).has(reason)
          ? reason as SupportedPublicErrorReason
          : undefined;
        if (reason !== undefined && reasonCode === undefined) {
          throw new MagicianAppsError("decode", operationId, "Error envelope reason is not declared for this operation");
        }
        throw new MagicianAppsError(
          envelope.disposition === "outcome_uncertain" ? "outcome_uncertain" : "http",
          operationId,
          "Supported-public operation was rejected",
          {
            httpStatus: response.status,
            errorCode: envelope.code,
            errorDisposition: envelope.disposition,
            ...(envelope.details === undefined ? {} : { errorDetails: envelope.details }),
            ...(envelope.retry_after_ms === undefined ? {} : { retryAfterMs: envelope.retry_after_ms }),
            ...(reasonCode === undefined ? {} : { reasonCode }),
            ...(envelope.disposition === "retry_same_input" ? { retryDisposition: "retry_identical_input" as const } : {}),
          });
      }
      const decoded = await decodeBoundedJsonResponse(operationId, response, maxBytes, shapeLimits);
      const validated = validator === undefined ? decoded as T : validator(decoded);
      if (deadline.timedOut() || Date.now() >= absoluteExpiryMs) {
        throw new MagicianAppsError("timeout", operationId, "End-to-end request deadline elapsed");
      }
      if (signalIsAborted(options.signal)) {
        throw new MagicianAppsError("cancelled", operationId, "Request was cancelled");
      }
      return validated;
    } catch (error) {
      if (error instanceof MagicianAppsError && error.httpStatus !== undefined) throw error;
      if (requestMayHaveStarted && operation.idempotency === "client_keyed") {
        throw new MagicianAppsError(
          "outcome_uncertain",
          operationId,
          "The server may have accepted this client-keyed request; retry only with byte-identical input and the same idempotency key",
          { retryDisposition: "retry_identical_input" },
        );
      }
      if (error instanceof MagicianAppsError) throw error;
      if (deadline.timedOut()) throw new MagicianAppsError("timeout", operationId, "Request deadline elapsed");
      if (signalIsAborted(options.signal)) throw new MagicianAppsError("cancelled", operationId, "Request was cancelled");
      throw new MagicianAppsError("transport", operationId, "Supported-public request failed before a response");
    } finally {
      deadline.cleanup();
    }
  }
}
