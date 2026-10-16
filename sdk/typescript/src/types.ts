import type { AppJsonSafeInteger } from "./generated/public-contract.js";

/** JSON numbers are finite; any integer must also be a JavaScript safe integer. */
export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonValue[] | { readonly [key: string]: JsonValue };
export type AppOutputValidator<T extends JsonValue> = (value: JsonValue) => value is T;

export type AppProtocolVersion = "1";
export type AppReference = string;
export type AppInstallationId = string;
export type AppName = string;
export type AppFieldPath = string;
export type AppRecordId = string;
export type AppDigest = string;
/** Positive JavaScript safe integer on every admitted request and response. */
export type AppRevision = AppJsonSafeInteger;
export type IsoTimestamp = string;

declare const appOpaqueHandleBrand: unique symbol;
export type AppOpaqueHandle<Kind extends string> = string & {
  readonly [appOpaqueHandleBrand]: Kind;
};
export type AppOpaqueReference = AppOpaqueHandle<"reference">;
export type AppRunReference = AppOpaqueHandle<"run">;
export type AppSessionHandle = AppOpaqueHandle<"session">;
export type AppObservationHandle = AppOpaqueHandle<"observation">;
export type AppArtifactHandle = AppOpaqueHandle<"artifact">;
export type AppReceiptHandle<Kind extends string = string> = AppOpaqueHandle<`receipt:${Kind}`>;
export type AppResourceHandle<Kind extends string = string> = AppOpaqueHandle<`resource:${Kind}`>;
export type AppEntityHandle<Entity extends string = string> = AppOpaqueHandle<`entity:${Entity}`>;

export type AppDataSource =
  | "user_input"
  | "app_store"
  | "app_action"
  | "artifact_projection"
  | "external_adapter"
  | "import"
  | "brokered_transfer";

export type AppDataClassification = "public" | "ordinary" | "personal" | "sensitive" | "secret";
export type AppModelProcessing = "none" | "local_only" | "remote_allowed";
export type AppSourceRefKind =
  | "entity_record"
  | "entity_field"
  | "artifact"
  | "external_receipt"
  | "mutation_receipt";

export interface AppHandlingLabels {
  readonly classification: AppDataClassification;
  readonly model_processing: AppModelProcessing;
  readonly policy_digest: AppDigest;
  readonly provenance_digest: AppDigest;
}

export interface AppSourceRef {
  readonly kind: AppSourceRefKind;
  readonly reference: AppReference;
  readonly revision?: AppRevision;
  readonly fields?: readonly AppFieldPath[];
}

export interface AppDataEnvelope<T> {
  readonly protocol_version: AppProtocolVersion;
  readonly source: AppDataSource;
  readonly scope_binding_ref: AppReference;
  readonly installation_id: AppInstallationId;
  readonly package_revision_ref: AppReference;
  readonly schema_revision: AppRevision;
  readonly grant_revision: AppRevision;
  readonly value_schema_ref: AppReference;
  readonly value: T;
  readonly source_refs?: readonly AppSourceRef[];
  readonly handling_labels: AppHandlingLabels;
  readonly content_digest: AppDigest;
  readonly produced_at: IsoTimestamp;
  readonly expires_at?: IsoTimestamp;
}

export type AppComparisonOperator =
  | "equal"
  | "not_equal"
  | "less_than"
  | "less_than_or_equal"
  | "greater_than"
  | "greater_than_or_equal"
  | "contains"
  | "starts_with";

export type AppPredicateNode =
  | { readonly kind: "all"; readonly children: readonly AppJsonSafeInteger[] }
  | { readonly kind: "any"; readonly children: readonly AppJsonSafeInteger[] }
  | { readonly kind: "not"; readonly child: AppJsonSafeInteger }
  | {
      readonly kind: "compare";
      readonly field: AppFieldPath;
      readonly operator: AppComparisonOperator;
      readonly value: JsonValue;
    }
  | { readonly kind: "in"; readonly field: AppFieldPath; readonly values: readonly JsonValue[] }
  | { readonly kind: "is_null"; readonly field: AppFieldPath; readonly negated?: boolean };

export interface AppPredicate {
  readonly root: AppJsonSafeInteger;
  readonly nodes: readonly AppPredicateNode[];
}

export interface AppQueryOrder {
  readonly field: AppFieldPath;
  readonly direction: "ascending" | "descending";
}

export interface AppRelationExpansion {
  readonly relation: AppName;
  readonly select?: readonly AppFieldPath[];
  readonly max_depth: AppJsonSafeInteger;
  readonly max_rows: AppJsonSafeInteger;
}

export interface AppQueryRequest {
  readonly pagination?: "snapshot" | "keyset";
  readonly protocol_version: AppProtocolVersion;
  readonly source_installation_id: AppInstallationId;
  readonly entity: AppName;
  readonly select: readonly AppFieldPath[];
  readonly predicate?: AppPredicate;
  readonly order?: readonly AppQueryOrder[];
  readonly cursor?: AppReference;
  readonly limit: AppJsonSafeInteger;
  readonly relation_expansions?: readonly AppRelationExpansion[];
  readonly purpose: AppName;
}

export interface AppRecordProjection {
  readonly entity: AppName;
  readonly record_id: AppRecordId;
  readonly record_revision: AppRevision;
  readonly fields: Readonly<Record<AppFieldPath, JsonValue>>;
}

export interface AppQueryPage {
  readonly envelope: AppDataEnvelope<AppRecordProjection[]>;
  readonly next_cursor?: AppReference;
  readonly result_schema_ref: AppReference;
}

export interface AppExpectedRecordRevision {
  readonly entity: AppName;
  readonly record_id: AppRecordId;
  readonly revision: AppRevision;
}

export type AppMutationOperation =
  | { readonly kind: "create"; readonly entity: AppName; readonly temporary_id: AppName; readonly payload: JsonValue }
  | { readonly kind: "update"; readonly entity: AppName; readonly record_id: AppRecordId; readonly patch: JsonValue }
  | { readonly kind: "delete"; readonly entity: AppName; readonly record_id: AppRecordId }
  | { readonly kind: "restore"; readonly entity: AppName; readonly record_id: AppRecordId }
  | {
      readonly kind: "create_relation";
      readonly relation: AppName;
      readonly from_record_id: AppRecordId;
      readonly to_record_id: AppRecordId;
      readonly expected_from_revision: AppRevision;
      readonly expected_to_revision: AppRevision;
    }
  | {
      readonly kind: "delete_relation";
      readonly relation: AppName;
      readonly from_record_id: AppRecordId;
      readonly to_record_id: AppRecordId;
      readonly expected_from_revision: AppRevision;
      readonly expected_to_revision: AppRevision;
    };

export interface AppMutationCommand {
  readonly protocol_version: AppProtocolVersion;
  readonly idempotency_key: AppReference;
  readonly atomicity: "all_or_nothing";
  readonly expected_schema_revision: AppRevision;
  readonly operations: readonly AppMutationOperation[];
  readonly expected_record_revisions?: readonly AppExpectedRecordRevision[];
}

export type AppMutationOrigin =
  | { readonly kind: "owner_api"; readonly session_ref: AppReference; readonly request_ref: AppReference }
  | {
      readonly kind: "workflow";
      /** Server-returned mutation provenance only; never accepted as client authority or correlation. */
      readonly execution_id: AppReference;
      readonly output_revision: AppRevision;
      readonly source_artifact_refs?: readonly AppReference[];
    }
  | { readonly kind: "surface"; readonly surface_session_id: AppReference; readonly client_mutation_id: AppReference }
  | { readonly kind: "migration"; readonly migration_run_id: AppReference; readonly migration_batch: AppJsonSafeInteger };

export interface AppCommittedRecordRevision {
  readonly entity: AppName;
  readonly record_id: AppRecordId;
  readonly revision: AppRevision;
}

export interface AppMutationReceipt {
  readonly receipt_id: AppReference;
  readonly installation_id: AppInstallationId;
  readonly origin: AppMutationOrigin;
  readonly mutation_key: AppDigest;
  /** Server-owned canonical command digest; the SDK does not approximate Rust canonical-number encoding. */
  readonly batch_digest: AppDigest;
  readonly committed_record_revisions: readonly AppCommittedRecordRevision[];
  readonly change_seq_range: { readonly first: AppJsonSafeInteger; readonly last: AppJsonSafeInteger };
  readonly committed_at: IsoTimestamp;
}

export interface AppDirectActionRequest<T extends JsonValue = JsonValue> {
  readonly idempotency_key: AppReference;
  readonly input: T;
}

export interface AppRunHandle {
  readonly protocol_version: AppProtocolVersion;
  readonly run_ref: AppReference;
  readonly installation_id: AppInstallationId;
  readonly action_id: AppName;
}

export type AppCustomBridgeRunControlRequest =
  | { readonly method: "get_run"; readonly action: AppName; readonly run_ref: AppRunReference }
  | {
      readonly method: "wait_run";
      readonly action: AppName;
      readonly run_ref: AppRunReference;
      readonly max_polls: AppJsonSafeInteger;
      readonly poll_interval_ms: AppJsonSafeInteger;
    }
  | {
      readonly method: "cancel_run";
      readonly action: AppName;
      readonly run_ref: AppRunReference;
      readonly expected_generation: AppJsonSafeInteger;
      readonly idempotency_key: AppReference;
    };

export interface AppCustomBridgeRequest<Payload extends JsonValue = JsonValue> {
  readonly session_ref: AppSessionHandle;
  readonly request_id: AppOpaqueHandle<"bridge-request">;
  readonly sequence: AppJsonSafeInteger;
  readonly action: AppName;
  readonly payload: Payload;
}

export interface AppCustomBridgeResult<Result extends JsonValue = JsonValue> {
  readonly request_id: AppOpaqueHandle<"bridge-request">;
  readonly sequence: AppJsonSafeInteger;
  readonly ok: boolean;
  readonly result?: Result;
  readonly error?: AppErrorEnvelope;
}

export type AppErrorCode =
  | "invalid_request"
  | "not_authorized"
  | "not_found"
  | "conflict"
  | "stale_revision"
  | "schema_mismatch"
  | "policy_denied"
  | "resource_exhausted"
  | "rate_limited"
  | "unavailable"
  | "timeout"
  | "canceled"
  | "external_outcome_uncertain"
  | "internal";

export type AppErrorDisposition =
  | "terminal"
  | "retry_same_input"
  | "refresh_and_retry"
  | "reauthorize"
  | "user_action_required"
  | "outcome_uncertain";

export interface AppErrorEnvelope {
  readonly code: AppErrorCode;
  readonly disposition: AppErrorDisposition;
  readonly message: string;
  readonly details?: Readonly<Record<AppName, JsonValue>>;
  readonly retry_after_ms?: AppJsonSafeInteger;
}

export type AppActionStatus = "completed" | "waiting" | "failed" | "uncertain";
export interface AppActionResult<T = JsonValue> {
  readonly protocol_version: AppProtocolVersion;
  readonly action_id: AppName;
  readonly run_ref: AppReference;
  readonly status: AppActionStatus;
  readonly output?: AppDataEnvelope<T>;
  readonly mutation_receipt_refs?: readonly AppReference[];
  readonly external_effect_receipt_refs?: readonly AppReference[];
  readonly error?: AppErrorEnvelope;
}

export interface AppActionLaunchResponse<T = JsonValue> {
  readonly run_handle: AppRunHandle;
  /** Optional server diagnostic for the current execution attempt; never correlation or authority. */
  readonly execution_id?: string;
  readonly result?: AppActionResult<T>;
}

export type AppRunStatus =
  | "queued"
  | "planning"
  | "running"
  | "paused"
  | "deferred"
  | "waiting"
  | "blocked"
  | "cancelling"
  | "completed"
  | "failed"
  | "cancelled"
  | "archived"
  | "uncertain";

export interface AppRunSnapshot<T = JsonValue> {
  readonly protocol_version: AppProtocolVersion;
  readonly run_handle: AppRunHandle;
  /** Optional server diagnostic for the current execution attempt; never correlation or authority. */
  readonly execution_id?: string;
  readonly status: AppRunStatus;
  readonly terminal: boolean;
  /** Current durable cancellation-control generation, when a request exists. */
  readonly cancellation_generation?: AppJsonSafeInteger;
  readonly result_withheld: boolean;
  readonly result?: AppActionResult<T>;
}

export interface AppActionCancellationRequest {
  readonly expected_generation: AppJsonSafeInteger;
  readonly idempotency_key: AppReference;
}

export interface AppActionCancellationReceipt {
  readonly protocol_version: AppProtocolVersion;
  readonly run_ref: AppReference;
  readonly generation: AppJsonSafeInteger;
  readonly idempotency_key: AppReference;
  readonly status: "cancelling" | "cancelled";
  readonly requested_at: IsoTimestamp;
}

export type AppRegisteredScalarConversion =
  | "integer_to_decimal"
  | "text_to_markdown"
  | "text_to_timestamp";

export type AppValueMappingOperation =
  | { readonly kind: "select"; readonly source: AppFieldPath; readonly target: AppFieldPath }
  | { readonly kind: "constant"; readonly target: AppFieldPath; readonly value: JsonValue }
  | {
      readonly kind: "convert";
      readonly source: AppFieldPath;
      readonly target: AppFieldPath;
      readonly conversion: AppRegisteredScalarConversion;
    }
  | {
      readonly kind: "map_enum";
      readonly source: AppFieldPath;
      readonly target: AppFieldPath;
      readonly values: Readonly<Record<AppName, AppName>>;
    };

export interface AppActionCompositionRequest {
  readonly destination_installation_id: AppInstallationId;
  readonly destination_action_id: AppName;
  readonly mapping: readonly AppValueMappingOperation[];
  readonly idempotency_key: AppReference;
  readonly chain?: readonly AppActionCompositionHop[];
  readonly subscription?: AppActionCompositionSubscriptionRequest;
}

export interface AppActionCompositionHop {
  readonly destination_installation_id: AppInstallationId;
  readonly destination_action_id: AppName;
  readonly mapping: readonly AppValueMappingOperation[];
  readonly idempotency_key: AppReference;
}

export interface AppActionCompositionSubscriptionRequest {
  readonly cursor?: AppReference;
  readonly limit?: AppJsonSafeInteger;
}

export interface AppModelActionResult<T extends JsonValue = JsonValue> {
  readonly protocol_version: AppProtocolVersion;
  readonly action_id: AppName;
  readonly run_ref: AppReference;
  readonly status: AppActionStatus;
  readonly output?: T;
  readonly effect_committed: boolean;
}

export interface AppCompositionWorkflowLaunch<T extends JsonValue = JsonValue> {
  readonly run_handle: AppRunHandle;
  readonly result?: AppModelActionResult<T>;
}

export type AppCompositionRetryClass = "permanent" | "transient" | "effect_uncertain" | "cancelled";
export type AppActionCompositionErrorCode = "outcome_unavailable" | "cancelled";

export type AppActionResultCompositionOutcome<T extends JsonValue = JsonValue> =
  | { readonly status: "waiting"; readonly source_run: AppRunHandle }
  | {
      readonly status: "launched";
      readonly source_run: AppRunHandle;
      readonly launch: AppCompositionWorkflowLaunch<T>;
      readonly result_withheld_by_policy: boolean;
    }
  | {
      readonly status: "source_terminal";
      readonly source_run: AppRunHandle;
      readonly source_status: AppRunStatus;
    }
  | {
      readonly status: "unavailable";
      readonly source_run: AppRunHandle;
      readonly error_code: AppActionCompositionErrorCode;
      readonly retry_class: AppCompositionRetryClass;
      readonly retryable: boolean;
      readonly effect_uncertain: boolean;
    };

export interface AppActionCompositionChainProgress {
  readonly origin_source_run_ref: AppReference;
  readonly active_source_run_ref: AppReference;
  readonly active_destination_installation_id: AppInstallationId;
  readonly active_destination_action_id: AppName;
  readonly hop_index: AppJsonSafeInteger;
  readonly hop_count: AppJsonSafeInteger;
}

export type AppActionCompositionUpdateStatus = "waiting" | "launched" | "source_terminal" | "unavailable";

export interface AppActionCompositionUpdate {
  readonly sequence: AppJsonSafeInteger;
  readonly source_run_ref: AppReference;
  readonly destination_installation_id: AppInstallationId;
  readonly destination_action_id: AppName;
  readonly status: AppActionCompositionUpdateStatus;
  readonly destination_run_ref?: AppReference;
  readonly observed_at: IsoTimestamp;
}

export interface AppActionCompositionSubscriptionPage {
  readonly after_sequence: AppJsonSafeInteger;
  readonly through_sequence: AppJsonSafeInteger;
  readonly current_sequence: AppJsonSafeInteger;
  readonly updates: readonly AppActionCompositionUpdate[];
  readonly has_more: boolean;
  readonly reset_required: boolean;
  readonly next_cursor: AppReference;
  readonly expires_at: IsoTimestamp;
}

export type AppActionResultComposition<T extends JsonValue = JsonValue> =
  AppActionResultCompositionOutcome<T> & {
    readonly chain: AppActionCompositionChainProgress;
    readonly subscription?: AppActionCompositionSubscriptionPage;
  };

export interface AppEntityChange {
  readonly entity: AppName;
  readonly record_id: AppRecordId;
  readonly record_revision: AppRevision;
  readonly change_sequence: AppJsonSafeInteger;
}

export interface AppEntityChangeBatch {
  readonly installation_id: AppInstallationId;
  readonly surface_revision: AppRevision;
  readonly after_change_sequence: AppJsonSafeInteger;
  readonly through_change_sequence: AppJsonSafeInteger;
  readonly current_change_sequence: AppJsonSafeInteger;
  readonly changes: readonly AppEntityChange[];
  readonly has_more: boolean;
  readonly reset_required: boolean;
}

export interface ReadEntityChangesOptions {
  readonly surfaceRevision: number;
  readonly afterChangeSequence?: number;
  readonly limit?: number;
  readonly signal?: AbortSignal;
  readonly deadlineMs?: number;
}

export interface IterateEntityChangesOptions extends ReadEntityChangesOptions {
  readonly maxPages?: number;
}

export interface WaitForRunOptions {
  readonly signal?: AbortSignal;
  readonly deadlineMs?: number;
  readonly pollIntervalMs?: number;
}
