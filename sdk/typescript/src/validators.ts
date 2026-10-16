import { MagicianAppsError } from "./errors.js";
import { boundedJsonMetrics } from "./bounded-json.js";
import type { AppPublicOperationId } from "./generated/public-contract.js";
import type {
  AppActionCancellationReceipt,
  AppActionCompositionRequest,
  AppActionResultComposition,
  AppActionLaunchResponse,
  AppActionResult,
  AppDataEnvelope,
  AppEntityChangeBatch,
  AppErrorEnvelope,
  AppMutationOrigin,
  AppMutationReceipt,
  AppQueryPage,
  AppQueryRequest,
  AppRecordProjection,
  AppRunHandle,
  AppRunSnapshot,
  AppSourceRef,
  AppOutputValidator,
  JsonValue,
} from "./types.js";

type JsonObject = { readonly [key: string]: JsonValue };

export interface ResponseValidationLimits {
  readonly maxCollectionItems: number;
  readonly maxPageRows: number;
  readonly maxEntityChangePageRows: number;
  readonly maxJsonDepth: number;
  readonly maxValueBytes: number;
  readonly maxValueNodes: number;
}

function fail(operationId: AppPublicOperationId, message: string): never {
  throw new MagicianAppsError("decode", operationId, message);
}

function object(
  value: JsonValue | undefined,
  operationId: AppPublicOperationId,
  required: readonly string[],
  optional: readonly string[] = [],
  label = "response",
): JsonObject {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return fail(operationId, `${label} must be an object`);
  }
  const keys = Object.keys(value);
  const allowed = new Set([...required, ...optional]);
  if (keys.some((key) => !allowed.has(key)) || required.some((key) => !Object.hasOwn(value, key))) {
    return fail(operationId, `${label} has missing or unknown fields`);
  }
  return value;
}

function arbitraryObject(value: JsonValue | undefined, operationId: AppPublicOperationId, label: string): JsonObject {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return fail(operationId, `${label} must be an object`);
  return value;
}

function string(value: JsonValue | undefined, operationId: AppPublicOperationId, label: string, max = 512): string {
  if (typeof value !== "string" || value.length < 1 || new TextEncoder().encode(value).length > max) {
    return fail(operationId, `${label} must be a bounded non-empty string`);
  }
  return value;
}

function reference(value: JsonValue | undefined, operationId: AppPublicOperationId, label: string): string {
  const result = string(value, operationId, label, 192);
  if (!/^[A-Za-z0-9][A-Za-z0-9_.:\/@#-]*$/.test(result)) return fail(operationId, `${label} is not a canonical reference`);
  return result;
}

function opaqueId(value: JsonValue | undefined, operationId: AppPublicOperationId, label: string): string {
  const result = string(value, operationId, label, 128);
  if (!/^[A-Za-z0-9][A-Za-z0-9_.-]*$/.test(result)) return fail(operationId, `${label} is not a canonical opaque id`);
  return result;
}

function name(value: JsonValue | undefined, operationId: AppPublicOperationId, label: string): string {
  const result = string(value, operationId, label, 64);
  if (!/^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(result)) return fail(operationId, `${label} is not a canonical app name`);
  return result;
}

function fieldPath(value: string, operationId: AppPublicOperationId, label: string): void {
  if (new TextEncoder().encode(value).length > 256
    || value.split(".").length > 16
    || value.split(".").some((part) => !/^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(part) || part.length > 64)) {
    fail(operationId, `${label} is not a canonical field path`);
  }
}

function digest(value: JsonValue | undefined, operationId: AppPublicOperationId, label: string): string {
  if (typeof value !== "string" || !/^blake3:[0-9a-f]{64}$/.test(value)) {
    return fail(operationId, `${label} is not a canonical BLAKE3 digest`);
  }
  return value;
}

function timestamp(value: JsonValue | undefined, operationId: AppPublicOperationId, label: string): string {
  const result = string(value, operationId, label, 64);
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/.test(result)
    || Number.isNaN(Date.parse(result))) {
    return fail(operationId, `${label} is not an RFC 3339 timestamp`);
  }
  return result;
}

function safeInteger(value: JsonValue | undefined, operationId: AppPublicOperationId, label: string, minimum = 0): number {
  if (!Number.isSafeInteger(value) || (value as number) < minimum) {
    return fail(operationId, `${label} must be a safe integer >= ${minimum}`);
  }
  return value as number;
}

function boolean(value: JsonValue | undefined, operationId: AppPublicOperationId, label: string): boolean {
  if (typeof value !== "boolean") return fail(operationId, `${label} must be boolean`);
  return value;
}

function array(value: JsonValue | undefined, operationId: AppPublicOperationId, label: string, max: number): readonly JsonValue[] {
  if (!Array.isArray(value) || value.length > max) return fail(operationId, `${label} is not a bounded array`);
  return value;
}

function uniqueStrings(values: readonly JsonValue[], operationId: AppPublicOperationId, label: string): readonly string[] {
  const parsed = values.map((value, index) => reference(value, operationId, `${label}[${index}]`));
  if (new Set(parsed).size !== parsed.length) fail(operationId, `${label} contains duplicate references`);
  return parsed;
}

function oneOf<T extends string>(value: JsonValue | undefined, allowed: readonly T[], operationId: AppPublicOperationId, label: string): T {
  if (typeof value !== "string" || !allowed.includes(value as T)) return fail(operationId, `${label} has an unsupported enum value`);
  return value as T;
}

function validateSourceRef(value: JsonValue, operationId: AppPublicOperationId, limits: ResponseValidationLimits): AppSourceRef {
  const source = object(value, operationId, ["kind", "reference"], ["revision", "fields"], "source_ref");
  const kind = oneOf(source.kind, ["entity_record", "entity_field", "artifact", "external_receipt", "mutation_receipt"], operationId, "source_ref.kind");
  reference(source.reference, operationId, "source_ref.reference");
  if (source.revision !== undefined) safeInteger(source.revision, operationId, "source_ref.revision", 1);
  let fields: readonly string[] = [];
  if (source.fields !== undefined) {
    fields = array(source.fields, operationId, "source_ref.fields", limits.maxCollectionItems)
      .map((field, index) => string(field, operationId, `source_ref.fields[${index}]`, 256));
    for (const [index, field] of fields.entries()) fieldPath(field, operationId, `source_ref.fields[${index}]`);
    if (new Set(fields).size !== fields.length) fail(operationId, "source_ref.fields contains duplicates");
  }
  if ((kind === "entity_record" || kind === "entity_field") && source.revision === undefined) {
    fail(operationId, "entity source_ref requires a revision");
  }
  if (kind === "entity_field" && fields.length === 0) fail(operationId, "entity_field source_ref requires fields");
  return source as unknown as AppSourceRef;
}

function validateEnvelope<T>(
  value: JsonValue,
  operationId: AppPublicOperationId,
  limits: ResponseValidationLimits,
  validateValue: (value: JsonValue) => T,
): AppDataEnvelope<T> {
  const envelope = object(value, operationId, [
    "protocol_version", "source", "scope_binding_ref", "installation_id", "package_revision_ref",
    "schema_revision", "grant_revision", "value_schema_ref", "value", "handling_labels",
    "content_digest", "produced_at",
  ], ["source_refs", "expires_at"], "data envelope");
  if (envelope.protocol_version !== "1") fail(operationId, "data envelope protocol_version must be 1");
  oneOf(envelope.source, ["user_input", "app_store", "app_action", "artifact_projection", "external_adapter", "import", "brokered_transfer"], operationId, "data envelope source");
  opaqueId(envelope.scope_binding_ref, operationId, "data envelope scope_binding_ref");
  opaqueId(envelope.installation_id, operationId, "data envelope installation_id");
  reference(envelope.package_revision_ref, operationId, "data envelope package_revision_ref");
  safeInteger(envelope.schema_revision, operationId, "data envelope schema_revision", 1);
  safeInteger(envelope.grant_revision, operationId, "data envelope grant_revision", 1);
  reference(envelope.value_schema_ref, operationId, "data envelope value_schema_ref");
  if (envelope.source_refs !== undefined) {
    const identities = new Set<string>();
    for (const entry of array(envelope.source_refs, operationId, "data envelope source_refs", limits.maxCollectionItems)) {
      const source = validateSourceRef(entry, operationId, limits);
      const identity = `${source.kind}\0${source.reference}\0${source.revision ?? ""}`;
      if (identities.has(identity)) fail(operationId, "data envelope source_refs contains a duplicate source identity");
      identities.add(identity);
    }
  }
  const labels = object(envelope.handling_labels, operationId, ["classification", "model_processing", "policy_digest", "provenance_digest"], [], "handling labels");
  oneOf(labels.classification, ["public", "ordinary", "personal", "sensitive", "secret"], operationId, "handling labels classification");
  oneOf(labels.model_processing, ["none", "local_only", "remote_allowed"], operationId, "handling labels model_processing");
  digest(labels.policy_digest, operationId, "handling labels policy_digest");
  digest(labels.provenance_digest, operationId, "handling labels provenance_digest");
  digest(envelope.content_digest, operationId, "data envelope content_digest");
  const producedAt = timestamp(envelope.produced_at, operationId, "data envelope produced_at");
  if (envelope.expires_at !== undefined) {
    const expiresAt = timestamp(envelope.expires_at, operationId, "data envelope expires_at");
    if (Date.parse(expiresAt) <= Date.parse(producedAt)) fail(operationId, "data envelope expires_at must follow produced_at");
  }
  validateValue(envelope.value as JsonValue);
  return envelope as unknown as AppDataEnvelope<T>;
}

function validateRecordProjection(
  value: JsonValue,
  operationId: AppPublicOperationId,
  request: AppQueryRequest,
  allowedFields: ReadonlySet<string>,
  limits: ResponseValidationLimits,
): AppRecordProjection {
  const record = object(value, operationId, ["entity", "record_id", "record_revision", "fields"], [], "record projection");
  if (name(record.entity, operationId, "record projection entity") !== request.entity) {
    fail(operationId, "record projection entity differs from the query");
  }
  opaqueId(record.record_id, operationId, "record projection record_id");
  safeInteger(record.record_revision, operationId, "record projection record_revision", 1);
  const fields = arbitraryObject(record.fields, operationId, "record projection fields");
  if (Object.keys(fields).length > limits.maxCollectionItems) fail(operationId, "record projection fields exceed the collection limit");
  for (const key of Object.keys(fields)) {
    fieldPath(key, operationId, "record projection field");
    if (!allowedFields.has(key)) fail(operationId, "record projection contains a field outside the query projection");
    boundedJsonMetrics(operationId, fields[key], limits.maxValueBytes, {
      maxDepth: limits.maxJsonDepth,
      maxNodes: limits.maxValueNodes,
    });
  }
  return record as unknown as AppRecordProjection;
}

export function validateQueryPage(
  value: JsonValue,
  installationId: string,
  request: AppQueryRequest,
  limits: ResponseValidationLimits,
): AppQueryPage {
  const operationId = "query_data";
  const allowedFields = new Set<string>([
    ...request.select,
    ...(request.relation_expansions ?? []).map((expansion) => expansion.relation),
  ]);
  const page = object(value, operationId, ["envelope", "result_schema_ref"], ["next_cursor"], "query page");
  const envelope = validateEnvelope(page.envelope as JsonValue, operationId, limits, (records) => {
    const entries = array(records, operationId, "query records", limits.maxPageRows);
    return entries.map((entry) => (
      validateRecordProjection(entry, operationId, request, allowedFields, limits)
    ));
  });
  if (envelope.source !== "app_store") fail(operationId, "query envelope source must be app_store");
  if (envelope.installation_id !== installationId) fail(operationId, "query envelope installation differs from the route");
  const schemaRef = reference(page.result_schema_ref, operationId, "query result_schema_ref");
  if (schemaRef !== envelope.value_schema_ref) fail(operationId, "query result schema differs from its envelope");
  if (page.next_cursor !== undefined) reference(page.next_cursor, operationId, "query next_cursor");
  return page as unknown as AppQueryPage;
}

function validateMutationOrigin(value: JsonValue, operationId: AppPublicOperationId, limits: ResponseValidationLimits): AppMutationOrigin {
  const tagged = object(value, operationId, ["kind"], [
    "session_ref", "request_ref", "execution_id", "output_revision", "source_artifact_refs",
    "surface_session_id", "client_mutation_id", "migration_run_id", "migration_batch",
  ], "mutation origin");
  const kind = oneOf(tagged.kind, ["owner_api", "workflow", "surface", "migration"], operationId, "mutation origin kind");
  const expected = kind === "owner_api" ? ["kind", "session_ref", "request_ref"]
    : kind === "workflow" ? ["kind", "execution_id", "output_revision"]
      : kind === "surface" ? ["kind", "surface_session_id", "client_mutation_id"]
        : ["kind", "migration_run_id", "migration_batch"];
  const optional = kind === "workflow" ? ["source_artifact_refs"] : [];
  object(value, operationId, expected, optional, "mutation origin");
  if (kind === "owner_api") {
    reference(tagged.session_ref, operationId, "mutation origin session_ref");
    reference(tagged.request_ref, operationId, "mutation origin request_ref");
  } else if (kind === "workflow") {
    reference(tagged.execution_id, operationId, "mutation origin execution_id");
    safeInteger(tagged.output_revision, operationId, "mutation origin output_revision", 1);
    if (tagged.source_artifact_refs !== undefined) uniqueStrings(array(tagged.source_artifact_refs, operationId, "source_artifact_refs", limits.maxCollectionItems), operationId, "source_artifact_refs");
  } else if (kind === "surface") {
    reference(tagged.surface_session_id, operationId, "mutation origin surface_session_id");
    reference(tagged.client_mutation_id, operationId, "mutation origin client_mutation_id");
  } else {
    reference(tagged.migration_run_id, operationId, "mutation origin migration_run_id");
    safeInteger(tagged.migration_batch, operationId, "mutation origin migration_batch", 1);
  }
  return tagged as unknown as AppMutationOrigin;
}

export function validateMutationReceipt(
  value: JsonValue,
  installationId: string,
  idempotencyKey: string,
  limits: ResponseValidationLimits,
): AppMutationReceipt {
  const operationId = "mutate_data";
  const receipt = object(value, operationId, [
    "receipt_id", "installation_id", "origin", "mutation_key", "batch_digest",
    "committed_record_revisions", "change_seq_range", "committed_at",
  ], [], "mutation receipt");
  reference(receipt.receipt_id, operationId, "mutation receipt_id");
  if (opaqueId(receipt.installation_id, operationId, "mutation installation_id") !== installationId) fail(operationId, "mutation receipt installation differs from the route");
  const origin = validateMutationOrigin(receipt.origin as JsonValue, operationId, limits);
  if (origin.kind !== "owner_api" || origin.request_ref !== idempotencyKey) {
    fail(operationId, "mutation receipt origin differs from the owner request");
  }
  digest(receipt.mutation_key, operationId, "mutation key");
  digest(receipt.batch_digest, operationId, "mutation batch digest");
  const revisions = array(receipt.committed_record_revisions, operationId, "committed_record_revisions", limits.maxCollectionItems);
  if (revisions.length === 0) fail(operationId, "mutation receipt has no committed records");
  const identities = new Set<string>();
  for (const revision of revisions) {
    const row = object(revision, operationId, ["entity", "record_id", "revision"], [], "committed record revision");
    const identity = `${name(row.entity, operationId, "committed entity")}\0${opaqueId(row.record_id, operationId, "committed record_id")}\0${safeInteger(row.revision, operationId, "committed revision", 1)}`;
    if (identities.has(identity)) fail(operationId, "mutation receipt contains a duplicate committed revision");
    identities.add(identity);
  }
  const range = object(receipt.change_seq_range, operationId, ["first", "last"], [], "change sequence range");
  const first = safeInteger(range.first, operationId, "change sequence first", 1);
  const last = safeInteger(range.last, operationId, "change sequence last", 1);
  if (last < first) fail(operationId, "mutation change sequence range is unordered");
  if (last - first + 1 !== revisions.length) {
    fail(operationId, "mutation change sequence range does not cover the committed revisions exactly");
  }
  timestamp(receipt.committed_at, operationId, "mutation committed_at");
  return receipt as unknown as AppMutationReceipt;
}

function validateRunHandle(value: JsonValue, operationId: AppPublicOperationId): AppRunHandle {
  const handle = object(value, operationId, ["protocol_version", "run_ref", "installation_id", "action_id"], [], "run handle");
  if (handle.protocol_version !== "1") fail(operationId, "run handle protocol_version must be 1");
  const runRef = reference(handle.run_ref, operationId, "run handle run_ref");
  if (!runRef.startsWith("run:app-action:") || runRef.length === "run:app-action:".length) fail(operationId, "run handle is outside the canonical namespace");
  opaqueId(handle.installation_id, operationId, "run handle installation_id");
  name(handle.action_id, operationId, "run handle action_id");
  return handle as unknown as AppRunHandle;
}

export function validateErrorEnvelope(value: JsonValue, operationId: AppPublicOperationId, limits: ResponseValidationLimits): AppErrorEnvelope {
  const error = object(value, operationId, ["code", "disposition", "message"], ["details", "retry_after_ms"], "error envelope");
  const code = oneOf(error.code, ["invalid_request", "not_authorized", "not_found", "conflict", "stale_revision", "schema_mismatch", "policy_denied", "resource_exhausted", "rate_limited", "unavailable", "timeout", "canceled", "external_outcome_uncertain", "internal"], operationId, "error code");
  const disposition = oneOf(error.disposition, ["terminal", "retry_same_input", "refresh_and_retry", "reauthorize", "user_action_required", "outcome_uncertain"], operationId, "error disposition");
  string(error.message, operationId, "error message", 4_096);
  if (error.details !== undefined) {
    const details = arbitraryObject(error.details, operationId, "error details");
    if (Object.keys(details).length > limits.maxCollectionItems) fail(operationId, "error details exceed the collection limit");
    let aggregateBytes = 0;
    let aggregateNodes = 0;
    for (const key of Object.keys(details)) {
      name(key, operationId, "error detail key");
      const metrics = boundedJsonMetrics(operationId, details[key], limits.maxValueBytes, {
        maxDepth: limits.maxJsonDepth,
        maxNodes: limits.maxValueNodes,
      });
      aggregateBytes += metrics.bytes;
      aggregateNodes += metrics.nodes;
    }
    if (aggregateBytes > limits.maxValueBytes || aggregateNodes > limits.maxValueNodes) {
      fail(operationId, "error details exceed the aggregate value ceiling");
    }
    if (details.operation_id !== undefined && details.operation_id !== operationId) {
      fail(operationId, "error details operation_id differs from the requested operation");
    }
    if (details.reason !== undefined) string(details.reason, operationId, "error details reason", 128);
  }
  if ((code === "external_outcome_uncertain") !== (disposition === "outcome_uncertain")) {
    fail(operationId, "external_outcome_uncertain and outcome_uncertain must be paired");
  }
  if (error.retry_after_ms !== undefined) {
    safeInteger(error.retry_after_ms, operationId, "error retry_after_ms", 1);
    if (code !== "rate_limited" && code !== "unavailable") {
      fail(operationId, "retry_after_ms is valid only for rate_limited or unavailable errors");
    }
  }
  return error as unknown as AppErrorEnvelope;
}

function validateActionResult<T extends JsonValue>(
  value: JsonValue,
  operationId: AppPublicOperationId,
  expectedInstallationId: string,
  limits: ResponseValidationLimits,
  outputValidator: AppOutputValidator<T>,
): AppActionResult<T> {
  const result = object(value, operationId, ["protocol_version", "action_id", "run_ref", "status"], [
    "output", "mutation_receipt_refs", "external_effect_receipt_refs", "error",
  ], "action result");
  if (result.protocol_version !== "1") fail(operationId, "action result protocol_version must be 1");
  name(result.action_id, operationId, "action result action_id");
  const runRef = reference(result.run_ref, operationId, "action result run_ref");
  if (!runRef.startsWith("run:app-action:") || runRef.length === "run:app-action:".length) fail(operationId, "action result run_ref is outside the canonical namespace");
  const status = oneOf(result.status, ["completed", "waiting", "failed", "uncertain"], operationId, "action result status");
  if (result.output !== undefined) {
    const output = validateEnvelope(result.output as JsonValue, operationId, limits, (output) => {
      boundedJsonMetrics(operationId, output, limits.maxValueBytes, {
        maxDepth: limits.maxJsonDepth,
        maxNodes: limits.maxValueNodes,
      });
      if (!outputValidator(output)) fail(operationId, "action output does not match the caller-supplied validator");
      return output;
    });
    if (output.source !== "app_action" || output.installation_id !== expectedInstallationId) {
      fail(operationId, "action output envelope belongs to another source or installation");
    }
  }
  const mutationRefs = result.mutation_receipt_refs === undefined ? [] : uniqueStrings(array(result.mutation_receipt_refs, operationId, "mutation_receipt_refs", limits.maxCollectionItems), operationId, "mutation_receipt_refs");
  const effectRefs = result.external_effect_receipt_refs === undefined ? [] : uniqueStrings(array(result.external_effect_receipt_refs, operationId, "external_effect_receipt_refs", limits.maxCollectionItems), operationId, "external_effect_receipt_refs");
  const error = result.error === undefined ? undefined : validateErrorEnvelope(result.error as JsonValue, operationId, limits);
  if (status === "completed" && (error !== undefined || (result.output === undefined && mutationRefs.length === 0 && effectRefs.length === 0))) fail(operationId, "completed action result is inconsistent");
  if (status === "waiting" && (error !== undefined || result.output !== undefined || mutationRefs.length > 0 || effectRefs.length > 0)) fail(operationId, "waiting action result claims terminal evidence");
  if (status === "failed" && (error === undefined || result.output !== undefined || mutationRefs.length > 0 || effectRefs.length > 0)) fail(operationId, "failed action result is inconsistent");
  if (status === "uncertain" && (error?.code !== "external_outcome_uncertain" || error.disposition !== "outcome_uncertain" || effectRefs.length === 0)) fail(operationId, "uncertain action result lacks uncertain evidence");
  return result as unknown as AppActionResult<T>;
}

export function validateActionLaunch<Output extends JsonValue>(
  value: JsonValue,
  installationId: string,
  actionId: string,
  limits: ResponseValidationLimits,
  outputValidator: AppOutputValidator<Output>,
): AppActionLaunchResponse<Output> {
  const operationId = "launch_action";
  const launch = object(value, operationId, ["run_handle"], ["execution_id", "result"], "action launch");
  const handle = validateRunHandle(launch.run_handle as JsonValue, operationId);
  if (handle.installation_id !== installationId || handle.action_id !== actionId) fail(operationId, "action launch returned a mismatched run handle");
  if (launch.execution_id !== undefined) string(launch.execution_id, operationId, "action launch execution_id", 256);
  if (launch.result !== undefined) {
    const result = validateActionResult<Output>(
      launch.result as JsonValue,
      operationId,
      handle.installation_id,
      limits,
      outputValidator,
    );
    if (result.run_ref !== handle.run_ref || result.action_id !== handle.action_id) fail(operationId, "action launch result belongs to another run");
  }
  return launch as unknown as AppActionLaunchResponse<Output>;
}

const terminalStatuses = new Set(["completed", "failed", "cancelled", "archived", "uncertain"]);
export function validateRunSnapshot<Output extends JsonValue>(
  value: JsonValue,
  runRef: string,
  limits: ResponseValidationLimits,
  outputValidator: AppOutputValidator<Output>,
): AppRunSnapshot<Output> {
  const operationId = "get_action_run";
  const snapshot = object(value, operationId, ["protocol_version", "run_handle", "status", "terminal", "result_withheld"], ["execution_id", "cancellation_generation", "result"], "run snapshot");
  if (snapshot.protocol_version !== "1") fail(operationId, "run snapshot protocol_version must be 1");
  const handle = validateRunHandle(snapshot.run_handle as JsonValue, operationId);
  if (handle.run_ref !== runRef) fail(operationId, "run snapshot belongs to another run");
  const status = oneOf(snapshot.status, ["queued", "planning", "running", "paused", "deferred", "waiting", "blocked", "cancelling", "completed", "failed", "cancelled", "archived", "uncertain"], operationId, "run status");
  const terminal = boolean(snapshot.terminal, operationId, "run terminal");
  const withheld = boolean(snapshot.result_withheld, operationId, "run result_withheld");
  if (terminal !== terminalStatuses.has(status)) fail(operationId, "run terminal flag differs from its status");
  if (snapshot.execution_id !== undefined) string(snapshot.execution_id, operationId, "run execution_id", 256);
  if (snapshot.cancellation_generation !== undefined) {
    safeInteger(snapshot.cancellation_generation, operationId, "run cancellation_generation", 1);
  }
  if (snapshot.result !== undefined) {
    if (withheld) fail(operationId, "run cannot include and withhold a result");
    const result = validateActionResult<Output>(
      snapshot.result as JsonValue,
      operationId,
      handle.installation_id,
      limits,
      outputValidator,
    );
    if (result.run_ref !== handle.run_ref || result.action_id !== handle.action_id) fail(operationId, "run result belongs to another run");
    const expected = result.status === "completed" ? "completed" : result.status === "failed" ? "failed" : result.status === "uncertain" ? "uncertain" : "waiting";
    if (status !== expected) fail(operationId, "run status differs from its typed result");
  } else if (status === "completed" && !withheld) {
    fail(operationId, "completed run lacks a result or withheld marker");
  } else if (withheld && status !== "completed") {
    fail(operationId, "result_withheld is valid only for a completed run");
  }
  return snapshot as unknown as AppRunSnapshot<Output>;
}

export function validateActionCancellationReceipt(
  value: JsonValue,
  runRef: string,
  expectedIdempotencyKey: string,
): AppActionCancellationReceipt {
  const operationId = "cancel_action_run";
  const receipt = object(value, operationId, [
    "protocol_version", "run_ref", "generation", "idempotency_key", "status", "requested_at",
  ], [], "action cancellation receipt");
  if (receipt.protocol_version !== "1") fail(operationId, "action cancellation receipt protocol_version must be 1");
  if (reference(receipt.run_ref, operationId, "action cancellation run_ref") !== runRef) {
    fail(operationId, "action cancellation receipt belongs to another run");
  }
  safeInteger(receipt.generation, operationId, "action cancellation generation", 1);
  if (reference(receipt.idempotency_key, operationId, "action cancellation idempotency_key") !== expectedIdempotencyKey) {
    fail(operationId, "action cancellation receipt has a different idempotency key");
  }
  oneOf(receipt.status, ["cancelling", "cancelled"], operationId, "action cancellation status");
  timestamp(receipt.requested_at, operationId, "action cancellation requested_at");
  return receipt as unknown as AppActionCancellationReceipt;
}

export function validateActionComposition<Output extends JsonValue>(
  value: JsonValue,
  sourceRunRef: string,
  request: AppActionCompositionRequest,
  limits: ResponseValidationLimits,
  outputValidator: AppOutputValidator<Output>,
): AppActionResultComposition<Output> {
  const operationId = "compose_action_run";
  const base = object(value, operationId, ["status", "source_run", "chain"], [
    "launch", "result_withheld_by_policy", "source_status", "error_code",
    "retry_class", "retryable", "effect_uncertain", "subscription",
  ], "action composition");
  const status = oneOf(base.status, ["waiting", "launched", "source_terminal", "unavailable"], operationId, "action composition status");
  const source = validateRunHandle(base.source_run as JsonValue, operationId);
  const chain = object(base.chain, operationId, [
    "origin_source_run_ref", "active_source_run_ref", "active_destination_installation_id",
    "active_destination_action_id", "hop_index", "hop_count",
  ], [], "action composition chain");
  const hopIndex = safeInteger(chain.hop_index, operationId, "composition hop_index");
  const hopCount = safeInteger(chain.hop_count, operationId, "composition hop_count", 1);
  const requestHops = [{
    destination_installation_id: request.destination_installation_id,
    destination_action_id: request.destination_action_id,
  }, ...(request.chain ?? [])];
  if (hopCount !== requestHops.length || hopIndex >= hopCount
    || reference(chain.origin_source_run_ref, operationId, "composition origin source") !== sourceRunRef
    || reference(chain.active_source_run_ref, operationId, "composition active source") !== source.run_ref
    || (hopIndex === 0 && source.run_ref !== sourceRunRef)) {
    fail(operationId, "composition chain correlation differs from the exact request");
  }
  const expectedHop = requestHops[hopIndex];
  if (expectedHop === undefined) fail(operationId, "composition hop index is unavailable");
  if (chain.active_destination_installation_id !== expectedHop.destination_installation_id
    || chain.active_destination_action_id !== expectedHop.destination_action_id) {
    fail(operationId, "composition active destination differs from the exact hop");
  }

  if (status === "waiting") {
    object(value, operationId, ["status", "source_run", "chain"], ["subscription"], "waiting action composition");
  } else if (status === "launched") {
    const launched = object(value, operationId, ["status", "source_run", "launch", "result_withheld_by_policy", "chain"], ["subscription"], "launched action composition");
    boolean(launched.result_withheld_by_policy, operationId, "result_withheld_by_policy");
    const launch = object(launched.launch, operationId, ["run_handle"], ["result"], "composition launch");
    const handle = validateRunHandle(launch.run_handle as JsonValue, operationId);
    if (handle.installation_id !== expectedHop.destination_installation_id || handle.action_id !== expectedHop.destination_action_id) {
      fail(operationId, "composition destination run differs from the request");
    }
    if (launch.result !== undefined) {
      const result = object(launch.result, operationId, ["protocol_version", "action_id", "run_ref", "status", "effect_committed"], ["output"], "composition result");
      if (result.protocol_version !== "1" || result.action_id !== handle.action_id || result.run_ref !== handle.run_ref) {
        fail(operationId, "composition result differs from its destination run");
      }
      oneOf(result.status, ["completed", "waiting", "failed", "uncertain"], operationId, "composition result status");
      boolean(result.effect_committed, operationId, "composition result effect_committed");
      if (result.output !== undefined) {
        boundedJsonMetrics(operationId, result.output, limits.maxValueBytes, {
          maxDepth: limits.maxJsonDepth,
          maxNodes: limits.maxValueNodes,
        });
        if (!outputValidator(result.output)) fail(operationId, "composition output does not match the caller-supplied validator");
      }
    }
  } else if (status === "source_terminal") {
    const terminal = object(value, operationId, ["status", "source_run", "source_status", "chain"], ["subscription"], "terminal-source composition");
    oneOf(terminal.source_status, ["completed", "failed", "cancelled", "archived", "uncertain"], operationId, "source terminal status");
  } else {
    const unavailable = object(value, operationId, [
      "status", "source_run", "error_code", "retry_class", "retryable", "effect_uncertain", "chain",
    ], ["subscription"], "unavailable action composition");
    const errorCode = oneOf(unavailable.error_code, ["outcome_unavailable", "cancelled"], operationId, "composition error code");
    const retryClass = oneOf(unavailable.retry_class, ["permanent", "transient", "effect_uncertain", "cancelled"], operationId, "composition retry class");
    const retryable = boolean(unavailable.retryable, operationId, "composition retryable");
    const effectUncertain = boolean(unavailable.effect_uncertain, operationId, "composition effect_uncertain");
    if (retryable !== (retryClass === "transient" || retryClass === "effect_uncertain")
      || effectUncertain !== (retryClass === "effect_uncertain")
      || (errorCode === "cancelled") !== (retryClass === "cancelled")) {
      fail(operationId, "composition retry disposition is inconsistent");
    }
  }
  if (base.subscription !== undefined) {
    validateActionCompositionSubscription(
      base.subscription,
      source.run_ref,
      expectedHop.destination_installation_id,
      expectedHop.destination_action_id,
      operationId,
    );
  }
  return value as unknown as AppActionResultComposition<Output>;
}

function validateActionCompositionSubscription(
  value: JsonValue,
  sourceRunRef: string,
  destinationInstallationId: string,
  destinationActionId: string,
  operationId: AppPublicOperationId,
): void {
  const page = object(value, operationId, [
    "after_sequence", "through_sequence", "current_sequence", "updates", "has_more",
    "reset_required", "next_cursor", "expires_at",
  ], [], "composition subscription");
  const after = safeInteger(page.after_sequence, operationId, "composition after_sequence");
  const through = safeInteger(page.through_sequence, operationId, "composition through_sequence");
  const current = safeInteger(page.current_sequence, operationId, "composition current_sequence");
  if (after > through || through > current) fail(operationId, "composition subscription sequence is not monotonic");
  if (!Array.isArray(page.updates) || page.updates.length > 8) fail(operationId, "composition subscription page is unbounded");
  let prior = after;
  for (const updateValue of page.updates) {
    const update = object(updateValue, operationId, [
      "sequence", "source_run_ref", "destination_installation_id", "destination_action_id",
      "status", "observed_at",
    ], ["destination_run_ref"], "composition update");
    const sequence = safeInteger(update.sequence, operationId, "composition update sequence", 1);
    if (sequence <= prior || sequence > through
      || reference(update.source_run_ref, operationId, "composition update source") !== sourceRunRef
      || update.destination_installation_id !== destinationInstallationId
      || update.destination_action_id !== destinationActionId) {
      fail(operationId, "composition update correlation or sequence is invalid");
    }
    prior = sequence;
    oneOf(update.status, ["waiting", "launched", "source_terminal", "unavailable"], operationId, "composition update status");
    timestamp(update.observed_at, operationId, "composition update observed_at");
    if (update.destination_run_ref !== undefined) reference(update.destination_run_ref, operationId, "composition update destination run");
  }
  if (page.updates.length > 0 && prior !== through) fail(operationId, "composition subscription through_sequence is inconsistent");
  boolean(page.has_more, operationId, "composition subscription has_more");
  boolean(page.reset_required, operationId, "composition subscription reset_required");
  reference(page.next_cursor, operationId, "composition subscription next_cursor");
  timestamp(page.expires_at, operationId, "composition subscription expires_at");
}

export function validateEntityChangeBatch(value: JsonValue, installationId: string, surfaceRevision: number, after: number, limits: ResponseValidationLimits): AppEntityChangeBatch {
  const operationId = "read_entity_changes";
  const batch = object(value, operationId, [
    "installation_id", "surface_revision", "after_change_sequence", "through_change_sequence",
    "current_change_sequence", "changes", "has_more", "reset_required",
  ], [], "entity change batch");
  if (opaqueId(batch.installation_id, operationId, "entity change installation_id") !== installationId
    || safeInteger(batch.surface_revision, operationId, "entity change surface_revision", 1) !== surfaceRevision
    || safeInteger(batch.after_change_sequence, operationId, "entity change after sequence", 0) !== after) {
    fail(operationId, "entity change response identity or cursor differs from the request");
  }
  const through = safeInteger(batch.through_change_sequence, operationId, "entity change through sequence", 0);
  const current = safeInteger(batch.current_change_sequence, operationId, "entity change current sequence", 0);
  if (through < after || current < through) fail(operationId, "entity change sequence range is invalid");
  const changes = array(batch.changes, operationId, "entity changes", limits.maxEntityChangePageRows);
  let prior = after;
  for (const change of changes) {
    const row = object(change, operationId, ["entity", "record_id", "record_revision", "change_sequence"], [], "entity change");
    name(row.entity, operationId, "entity change entity");
    opaqueId(row.record_id, operationId, "entity change record_id");
    safeInteger(row.record_revision, operationId, "entity change record_revision", 1);
    const sequence = safeInteger(row.change_sequence, operationId, "entity change sequence", 1);
    if (sequence !== prior + 1 || sequence > through) fail(operationId, "entity changes are not contiguous inside the page");
    prior = sequence;
  }
  const hasMore = boolean(batch.has_more, operationId, "entity change has_more");
  const resetRequired = boolean(batch.reset_required, operationId, "entity change reset_required");
  if (resetRequired) {
    if (changes.length !== 0 || hasMore || through !== current) {
      fail(operationId, "reset entity-change batch has inconsistent rows or sequence flags");
    }
  } else {
    if (prior !== through || (changes.length === 0 && through !== after)) {
      fail(operationId, "entity-change rows do not cover the declared through sequence");
    }
    if ((hasMore && through >= current) || (!hasMore && through !== current)) {
      fail(operationId, "entity-change continuation flag differs from the durable head");
    }
  }
  return batch as unknown as AppEntityChangeBatch;
}
