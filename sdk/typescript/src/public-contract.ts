import type { AppJsonSafeInteger, AppPublicOperationId } from "./generated/public-contract.js";

export interface AppPublicParameter {
  readonly name: string;
  readonly location: "path" | "query";
  readonly required: boolean;
  readonly schema_type: string;
  readonly schema_format?: string;
  readonly minimum?: AppJsonSafeInteger;
  readonly maximum?: AppJsonSafeInteger;
  readonly default?: AppJsonSafeInteger;
  readonly description: string;
}

export interface AppPublicOperation {
  readonly operation_id: AppPublicOperationId;
  readonly method: "GET" | "POST";
  readonly path: string;
  readonly summary: string;
  readonly auth: "verified_session" | "verified_owner_scope";
  readonly idempotency: "read_only" | "client_keyed";
  readonly parameters: readonly AppPublicParameter[];
  readonly request_schema?: string;
  readonly success_statuses: readonly AppJsonSafeInteger[];
  readonly response_schema: string;
  readonly request_example?: string;
  readonly response_example?: string;
  readonly errors: readonly string[];
}

export interface AppContractCapabilities {
  readonly schema_version: AppJsonSafeInteger;
  readonly contract_version: string;
  readonly supported_protocol_versions: readonly string[];
  readonly supported_manifest_schema_versions: readonly string[];
  readonly supported_manifest_features: readonly string[];
  readonly json_schema_dialect: string;
  readonly limits: {
    readonly max_document_bytes: AppJsonSafeInteger;
    readonly max_json_depth: AppJsonSafeInteger;
    readonly max_json_nodes: AppJsonSafeInteger;
    readonly max_value_bytes: AppJsonSafeInteger;
    readonly max_value_nodes: AppJsonSafeInteger;
    readonly max_collection_items: AppJsonSafeInteger;
    readonly max_predicate_nodes: AppJsonSafeInteger;
    readonly max_predicate_depth: AppJsonSafeInteger;
    readonly max_page_rows: AppJsonSafeInteger;
    readonly max_entity_change_page_rows: AppJsonSafeInteger;
  };
  readonly sdk_compatibility: {
    readonly policy: "current_contract_only";
    readonly supported_contract_versions: readonly string[];
    readonly generated_by_is_authority: false;
  };
  readonly deprecations: readonly {
    readonly field: string;
    readonly replacement: string;
    readonly removal_contract_major: AppJsonSafeInteger | null;
  }[];
  readonly operation_inventory_digest: string;
  readonly operations: readonly AppPublicOperation[];
}
