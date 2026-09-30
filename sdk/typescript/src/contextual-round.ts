import type { JsonValue } from "./types.js";
import type { ReconciliationSource } from "./recipe.js";

/** Flat data expressions. The server compiles dependencies and validates authority. */
export type RoundValueExpression =
  | { readonly kind: "literal"; readonly value: JsonValue }
  | { readonly kind: "read"; readonly source: "input" | "participant" | "context" | "model" | "item"; readonly pointer: string }
  | { readonly kind: "timestamp" | "run_id" | "participant_id" }
  | { readonly kind: "object"; readonly fields: Readonly<Record<string, number>> }
  | { readonly kind: "array"; readonly values: readonly number[] }
  | { readonly kind: "canonical_json"; readonly value: number }
  | { readonly kind: "join"; readonly value: number; readonly separator: string }
  | { readonly kind: "byte_length"; readonly value: number }
  | { readonly kind: "linked_text_rows"; readonly value: number; readonly schema: LinkedTextRowsSchema }
  | { readonly kind: "split"; readonly value: number; readonly separator: string }
  | { readonly kind: "slice"; readonly value: number; readonly limit: number }
  | { readonly kind: "project"; readonly rows: number; readonly pointer: string; readonly skip_missing?: boolean }
  | { readonly kind: "coalesce" | "all" | "any"; readonly values: readonly number[] }
  | { readonly kind: "equal" | "less_than" | "add" | "subtract" | "multiply"; readonly left: number; readonly right: number }
  | { readonly kind: "not" | "present" | "length" | "trim"; readonly value: number }
  | { readonly kind: "if"; readonly condition: number; readonly then_value: number; readonly else_value: number }
  | { readonly kind: "clamp"; readonly value: number; readonly minimum: number; readonly maximum: number }
  | { readonly kind: "truncate"; readonly value: number; readonly max_chars: number }
  | { readonly kind: "contains"; readonly value: number; readonly member: number }
  | { readonly kind: "elapsed_seconds"; readonly timestamp: number }
  | { readonly kind: "lookup"; readonly rows: number; readonly key_pointer: string; readonly key: number; readonly value_pointer: string }
  | { readonly kind: "stable_id"; readonly prefix: string; readonly parts: readonly number[] };

export interface RoundUsage {
  readonly tokens: number;
  readonly micro_usd: number;
}

export interface RoundContextQuery {
  readonly name: string;
  /** Refresh changing shared data for each progressive turn; participant queries are always fresh. */
  readonly refresh_before_dispatch?: boolean;
  readonly entity: string;
  readonly per_participant: boolean;
  readonly parameters: Readonly<Record<string, JsonValue>>;
  readonly bindings: Readonly<Record<string, number>>;
}

export interface RoundMutationRule {
  readonly entity: string;
  readonly for_each: number | null;
  readonly when: number | null;
  readonly semantic_result: boolean;
  readonly change:
    | { readonly kind: "create"; readonly record_id: number; readonly fields: Readonly<Record<string, number>> }
    | { readonly kind: "update"; readonly record_id: number; readonly revision: number; readonly fields: Readonly<Record<string, number>> }
    | { readonly kind: "delete"; readonly record_id: number; readonly revision: number };
}

/** Closed JSON input with explicit dictionary links, without inferred attribution. */
export interface LinkedTextRowsSchema {
  readonly dictionary_field: string;
  readonly rows_field: string;
  readonly link_field: string;
  readonly text_field: string;
  readonly max_keys: number;
  readonly max_rows: number;
  readonly max_bytes: number;
}

/** Mechanical App-owned changes, executed by the normal store/receipt owners. */
export interface StoreTransactionProgram {
  readonly values: { readonly expressions: readonly RoundValueExpression[] };
  readonly queries: readonly RoundContextQuery[];
  readonly scan_queries?: readonly string[];
  readonly query_when?: Readonly<Record<string, number>>;
  readonly source_parameters?: Readonly<Record<string, Readonly<Record<string, number>>>>;
  readonly summary?: number;
  readonly guards: readonly { readonly require: number; readonly reason: string }[];
  readonly mutations: readonly RoundMutationRule[];
  readonly max_mutations: number;
  readonly max_query_pages: number;
}

export interface StoreTransactionDeclaration {
  readonly sources: Readonly<Record<string, ReconciliationSource>>;
  readonly program: StoreTransactionProgram;
}

export interface ContextualRoundProgram {
  readonly values: { readonly expressions: readonly RoundValueExpression[] };
  readonly participant_id_pointer: string;
  readonly resume_after?: number | null;
  /** Progressive rounds use max_concurrent: 1 and refresh store context between committed turns. */
  readonly context_mode?: 'snapshot' | 'progressive';
  readonly queries: readonly RoundContextQuery[];
  readonly eligibility: readonly { readonly require: number; readonly reason: string }[];
  readonly semantic_step: string;
  readonly max_output_tokens: number;
  readonly limits: {
    readonly max_participants: number;
    readonly max_concurrent: number;
    readonly max_attempts_per_participant: number;
    readonly aggregate: RoundUsage;
    readonly per_participant: RoundUsage;
  };
  readonly draft_when: number;
  readonly quiet_when: number;
  readonly quiet_reason: number;
  readonly mutations: readonly RoundMutationRule[];
  readonly final_mutations: readonly RoundMutationRule[];
  readonly max_mutations_per_participant: number;
}

export interface ContextualRoundDeclaration {
  readonly source: ReconciliationSource & {
    readonly rows: Extract<ReconciliationSource["rows"], { readonly kind: "page" }>;
  };
  readonly cursor_parameter: string;
  readonly max_source_pages: number;
  readonly program: ContextualRoundProgram;
}
