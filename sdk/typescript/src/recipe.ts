import type { JsonValue } from "./types.js";
import type { ContextualRoundDeclaration, StoreTransactionDeclaration } from "./contextual-round.js";

const NAME = /^[A-Za-z][A-Za-z0-9_-]{0,127}$/;
const REFERENCE = /^[A-Za-z][A-Za-z0-9_-]*:[^\\/\s][^\\\s]*$/;
const MAX_RECIPE_NODES = 256;

export type SupportedRecipeNodeKind =
  | "query"
  | "get"
  | "map"
  | "validate"
  | "emit_value"
  | "sequence"
  | "parallel"
  | "switch"
  | "reconcile"
  | "store_transaction"
  | "contextual_round";

export interface RecipeOutput {
  readonly kind: "typed_value" | "entity_projection";
  readonly schema_ref: string;
  readonly authority: "authoritative" | "derived";
}

export interface RecipeNodeResources {
  readonly max_active_millis: number;
  readonly max_input_bytes: number;
  readonly max_output_bytes: number;
  readonly max_cost_microusd: number;
  readonly max_tool_calls: number;
  readonly max_parallelism: number;
}

export interface RecipeNodeOptions {
  readonly maxActiveMillis?: number;
  readonly maxInputBytes?: number;
  readonly maxOutputBytes?: number;
  readonly cancellationAcknowledgementMillis?: number;
}

export type RecipeMappingOperation =
  | { readonly kind: "select"; readonly source: string; readonly target: string }
  | { readonly kind: "constant"; readonly target: string; readonly value: JsonValue }
  | {
      readonly kind: "convert";
      readonly source: string;
      readonly target: string;
      readonly conversion: "integer_to_decimal";
    }
  | {
      readonly kind: "map_enum";
      readonly source: string;
      readonly target: string;
      readonly values: Readonly<Record<string, string>>;
    };

export type ReconciliationValue =
  | { readonly kind: "literal"; readonly value: string | number | boolean | null }
  | { readonly kind: "source"; readonly field: string; readonly fallback: string | number | boolean | null; readonly allow_null?: boolean }
  | { readonly kind: "timestamp" }
  | { readonly kind: "source_document"; readonly max_bytes: number };

export interface ReconciliationSource {
  readonly tool: string;
  readonly action: string;
  readonly primitive_ref: string;
  readonly action_ref: string;
  readonly parameters: Readonly<Record<string, string | number | boolean | null>>;
  readonly input_parameters: Readonly<Record<string, string>>;
  readonly when_input_present: string | null;
  readonly rows: { readonly kind: "document" } | {
    readonly kind: "page";
    readonly rows_field: string;
    readonly next_cursor_field: string;
    readonly truncated_field: string;
  };
}

export interface ReconciliationDeclaration {
  readonly tool: string;
  readonly action: string;
  readonly parameters: Readonly<Record<string, string | number | boolean | null>>;
  readonly input_parameters?: Readonly<Record<string, string>>;
  readonly sources?: Readonly<Record<string, ReconciliationSource>>;
  readonly rows_field: string;
  readonly next_cursor_field: string;
  readonly truncated_field: string;
  readonly max_source_rows: number;
  readonly max_existing_rows: number;
  readonly targets: readonly {
    readonly entity: string;
    readonly source?: string;
    readonly key_field: string;
    readonly source_key: string | null;
    readonly fields: Readonly<Record<string, ReconciliationValue>>;
    readonly update_fields: readonly string[];
    readonly seeds: readonly {
      readonly key: string;
      readonly record_id: string;
      readonly fields: Readonly<Record<string, ReconciliationValue>>;
    }[];
    readonly retirement: {
      readonly discriminator: string;
      readonly value: string | number | boolean | null;
      readonly patch: Readonly<Record<string, string | number | boolean | null>>;
    } | null;
  }[];
}

interface RecipeNodeBase {
  readonly input_schema_ref: string;
  readonly output: RecipeOutput;
  readonly effect:
    | { readonly class: "none"; readonly idempotency: { readonly kind: "not_applicable" }; readonly uncertainty: "impossible" }
    | { readonly class: "read_only"; readonly idempotency: { readonly kind: "intrinsic" }; readonly uncertainty: "impossible" }
    | { readonly class: "internal_mutation"; readonly idempotency: { readonly kind: "intrinsic" }; readonly uncertainty: "receipt_bound" };
  readonly authority: {
    readonly required_grant_refs?: readonly string[];
    readonly primitive_ref?: string;
    readonly action_ref?: string;
  };
  readonly resources: RecipeNodeResources;
  readonly retry: { readonly kind: "none" };
  readonly cancellation: { readonly mode: "propagate"; readonly acknowledgement_timeout_millis: number };
  readonly provenance_join: "preserve";
}

export type SupportedRecipeNode = RecipeNodeBase & {
  readonly node:
    | { readonly kind: "query"; readonly entity: string }
    | { readonly kind: "get"; readonly entity: string }
    | {
        readonly kind: "map";
        readonly mapping_digest: string;
        readonly operations: readonly RecipeMappingOperation[];
      }
    | { readonly kind: "validate" }
    | { readonly kind: "reconcile"; readonly declaration: ReconciliationDeclaration }
    | ({ readonly kind: "store_transaction" } & StoreTransactionDeclaration)
    | ({ readonly kind: "contextual_round" } & ContextualRoundDeclaration)
    | { readonly kind: "emit_value" }
    | { readonly kind: "sequence"; readonly steps: readonly string[] }
    | { readonly kind: "parallel"; readonly branches: Readonly<Record<string, string>> }
    | { readonly kind: "switch"; readonly discriminator: string; readonly cases: Readonly<Record<string, string>> };
};

export interface RecipeGraphCeiling {
  readonly max_nodes: number;
  readonly max_edges: number;
  readonly max_depth: number;
  readonly max_fan_out: number;
  readonly max_parallelism: number;
  readonly max_payload_bytes: number;
  readonly max_active_millis: number;
  readonly max_cost_microusd: number;
  readonly max_tool_calls: number;
}

export interface SupportedRecipeDefinition {
  readonly version: "v1";
  readonly input_schema_ref: string;
  readonly output: RecipeOutput;
  readonly root: string;
  readonly nodes: Readonly<Record<string, SupportedRecipeNode>>;
  readonly ceilings: RecipeGraphCeiling;
  readonly evolution: {
    readonly topology_revision: number;
    readonly migration: "recompile_required";
    readonly predecessor_recipe_ref?: string;
  };
}

export interface SupportedRecipeBundle {
  readonly version: "v1";
  readonly schemas: Readonly<Record<string, JsonValue>>;
  readonly recipe: SupportedRecipeDefinition;
}

function positive(value: number, field: string): number {
  if (!Number.isSafeInteger(value) || value < 1) throw new TypeError(`${field} must be a positive safe integer`);
  return value;
}

function name(value: string, field: string): string {
  if (!NAME.test(value)) throw new TypeError(`${field} is not a bounded Recipe name`);
  return value;
}

function reference(value: string, field: string): string {
  if (!REFERENCE.test(value) || value.includes("..") || value.includes("//")) {
    throw new TypeError(`${field} is not an exact logical reference`);
  }
  return value;
}

function digest(value: string, field: string): string {
  if (!/^blake3:[0-9a-f]{64}$/.test(value)) {
    throw new TypeError(`${field} must be an exact BLAKE3 digest`);
  }
  return value;
}

function fieldPath(value: string, field: string): string {
  if (!/^[A-Za-z][A-Za-z0-9_-]{0,127}$/.test(value)) {
    throw new TypeError(`${field} must be one flat bounded field path`);
  }
  return value;
}

function exactZero(value: number, field: string): void {
  if (value !== 0) throw new TypeError(`${field} must remain zero in the supported Recipe vertical`);
}

function resources(options: RecipeNodeOptions = {}, maxParallelism = 1): RecipeNodeResources {
  return {
    max_active_millis: positive(options.maxActiveMillis ?? 1_000, "maxActiveMillis"),
    max_input_bytes: positive(options.maxInputBytes ?? 4_096, "maxInputBytes"),
    max_output_bytes: positive(options.maxOutputBytes ?? 4_096, "maxOutputBytes"),
    max_cost_microusd: 0,
    max_tool_calls: 0,
    max_parallelism: positive(maxParallelism, "maxParallelism"),
  };
}

function base(
  inputSchemaRef: string,
  output: RecipeOutput,
  options: RecipeNodeOptions,
  maxParallelism = 1,
): Omit<RecipeNodeBase, "effect" | "authority"> {
  reference(inputSchemaRef, "inputSchemaRef");
  reference(output.schema_ref, "output.schema_ref");
  return {
    input_schema_ref: inputSchemaRef,
    output: { ...output },
    resources: resources(options, maxParallelism),
    retry: { kind: "none" },
    cancellation: {
      mode: "propagate",
      acknowledgement_timeout_millis: positive(
        options.cancellationAcknowledgementMillis ?? 100,
        "cancellationAcknowledgementMillis",
      ),
    },
    provenance_join: "preserve",
  };
}

function pureNode(
  node: SupportedRecipeNode["node"],
  inputSchemaRef: string,
  output: RecipeOutput,
  options: RecipeNodeOptions = {},
  maxParallelism = 1,
): SupportedRecipeNode {
  if (output.authority !== "derived") throw new TypeError(`${node.kind} output must be derived`);
  return {
    ...base(inputSchemaRef, output, options, maxParallelism),
    node,
    effect: { class: "none", idempotency: { kind: "not_applicable" }, uncertainty: "impossible" },
    authority: {},
  };
}

export const recipeNode = Object.freeze({
  storeTransaction(
    declaration: StoreTransactionDeclaration, inputSchemaRef: string, outputSchemaRef: string,
    requiredGrantRefs: readonly string[], options: RecipeNodeOptions = {},
  ): SupportedRecipeNode {
    const { program, sources } = declaration;
    if (requiredGrantRefs.length === 0 || Object.keys(sources).length > 64
      || program.values.expressions.length === 0 || program.values.expressions.length > 1024
      || program.queries.length > 64 || program.guards.length > 64 || program.mutations.length > 256
      || positive(program.max_mutations, "transaction max_mutations") > 256
      || positive(program.max_query_pages, "transaction max_query_pages") > 65_535
      || program.queries.some((query) => query.per_participant)
      || program.mutations.some((rule) => rule.semantic_result)
      || program.values.expressions.some((expression) => expression.kind === "participant_id"
        || (expression.kind === "read" && ["model", "participant"].includes(expression.source)))) {
      throw new TypeError("store transaction requires fixed own-store authority and no semantic dependencies");
    }
    requiredGrantRefs.forEach((grant) => reference(grant, "transaction required grant"));
    for (const [sourceName, source] of Object.entries(sources)) {
      name(sourceName, "transaction source name");
      name(source.tool, "transaction source tool");
      name(source.action, "transaction source action");
      reference(source.primitive_ref, "transaction primitive_ref");
      reference(source.action_ref, "transaction action_ref");
      if (!source.primitive_ref.startsWith("primitive:") || !source.action_ref.startsWith("primitive-action:")) {
        throw new TypeError("transaction source requires exact primitive/action references");
      }
    }
    const common = base(inputSchemaRef, { kind: "typed_value", schema_ref: outputSchemaRef, authority: "derived" }, options);
    return {
      ...common, resources: { ...common.resources, max_tool_calls: Object.keys(sources).length },
      node: { ...declaration, kind: "store_transaction" },
      effect: { class: "internal_mutation", idempotency: { kind: "intrinsic" }, uncertainty: "receipt_bound" },
      authority: { required_grant_refs: [...new Set(requiredGrantRefs)].sort() },
    };
  },
  contextualRound(
    declaration: ContextualRoundDeclaration, inputSchemaRef: string, outputSchemaRef: string,
    requiredGrantRefs: readonly string[], options: RecipeNodeOptions = {},
  ): SupportedRecipeNode {
    const { source, program } = declaration;
    name(source.tool, "round source tool");
    name(source.action, "round source action");
    name(declaration.cursor_parameter, "round cursor parameter");
    name(program.semantic_step, "round semantic step");
    reference(source.primitive_ref, "round primitive_ref");
    reference(source.action_ref, "round action_ref");
    const limits = program.limits;
    positive(declaration.max_source_pages, "round max_source_pages");
    positive(limits.max_participants, "round max_participants");
    positive(limits.max_concurrent, "round max_concurrent");
    positive(limits.max_attempts_per_participant, "round max_attempts");
    positive(limits.aggregate.tokens, "round aggregate tokens");
    positive(limits.per_participant.tokens, "round participant tokens");
    positive(program.max_output_tokens, "round max_output_tokens");
    for (const usage of [limits.aggregate, limits.per_participant]) {
      if (!Number.isSafeInteger(usage.micro_usd) || usage.micro_usd < 0) {
        throw new TypeError("round cost must be nonnegative integer micro-USD");
      }
    }
    if (source.rows.kind !== "page" || !source.primitive_ref.startsWith("primitive:")
      || !source.action_ref.startsWith("primitive-action:") || requiredGrantRefs.length === 0
      || limits.max_concurrent > limits.max_participants
      || limits.per_participant.tokens > limits.aggregate.tokens
      || limits.per_participant.micro_usd > limits.aggregate.micro_usd
      || program.max_output_tokens > limits.per_participant.tokens
      || program.values.expressions.length === 0 || program.values.expressions.length > 1024
      || program.queries.length > 64 || program.eligibility.length > 64
      || program.mutations.length > 256 || program.final_mutations.length > 256) {
      throw new TypeError("round requires reviewed source authority and consistent participant budgets");
    }
    positive(program.max_mutations_per_participant, "round max_mutations");
    requiredGrantRefs.forEach((grant) => reference(grant, "round required grant"));
    const common = base(inputSchemaRef, { kind: "typed_value", schema_ref: outputSchemaRef, authority: "derived" }, options, limits.max_concurrent);
    return {
      ...common,
      resources: { ...common.resources, max_tool_calls: declaration.max_source_pages, max_cost_microusd: limits.aggregate.micro_usd },
      node: { ...declaration, kind: "contextual_round" },
      effect: { class: "internal_mutation", idempotency: { kind: "intrinsic" }, uncertainty: "receipt_bound" },
      authority: { primitive_ref: source.primitive_ref, action_ref: source.action_ref,
        required_grant_refs: [...new Set(requiredGrantRefs)].sort() },
    };
  },
  reconcile(
    declaration: ReconciliationDeclaration, inputSchemaRef: string, outputSchemaRef: string,
    authority: { readonly primitive_ref: string; readonly action_ref: string; readonly required_grant_refs: readonly string[] },
    options: RecipeNodeOptions = {},
  ): SupportedRecipeNode {
    name(declaration.tool, "reconciliation tool");
    name(declaration.action, "reconciliation action");
    reference(authority.primitive_ref, "primitive_ref");
    reference(authority.action_ref, "action_ref");
    if (!authority.primitive_ref.startsWith("primitive:") || !authority.action_ref.startsWith("primitive-action:")
      || authority.required_grant_refs.length === 0 || declaration.targets.length < 1 || declaration.targets.length > 8
      || positive(declaration.max_source_rows, "max_source_rows") > 200
      || positive(declaration.max_existing_rows, "max_existing_rows") > 65_535
      || Object.keys(declaration.sources ?? {}).length > 3) {
      throw new TypeError("reconciliation requires exact reviewed authority and bounded rows/targets");
    }
    authority.required_grant_refs.forEach((grant) => reference(grant, "required_grant_ref"));
    for (const [sourceName, source] of Object.entries(declaration.sources ?? {})) {
      name(sourceName, "source name");
      name(source.tool, "source tool");
      name(source.action, "source action");
      reference(source.primitive_ref, "source primitive_ref");
      reference(source.action_ref, "source action_ref");
      if (!source.primitive_ref.startsWith("primitive:") || !source.action_ref.startsWith("primitive-action:")) {
        throw new TypeError("source requires exact reviewed primitive/action references");
      }
    }
    const common = base(inputSchemaRef, { kind: "typed_value", schema_ref: outputSchemaRef, authority: "derived" }, options);
    return {
      ...common, resources: { ...common.resources, max_tool_calls: 1 + Object.keys(declaration.sources ?? {}).length },
      node: { kind: "reconcile", declaration },
      effect: { class: "internal_mutation", idempotency: { kind: "intrinsic" }, uncertainty: "receipt_bound" },
      authority: { ...authority, required_grant_refs: [...new Set(authority.required_grant_refs)].sort() },
    };
  },
  validate(schemaRef: string, options: RecipeNodeOptions = {}): SupportedRecipeNode {
    return pureNode({ kind: "validate" }, schemaRef, { kind: "typed_value", schema_ref: schemaRef, authority: "derived" }, options);
  },
  emitValue(schemaRef: string, options: RecipeNodeOptions = {}): SupportedRecipeNode {
    return pureNode({ kind: "emit_value" }, schemaRef, { kind: "typed_value", schema_ref: schemaRef, authority: "derived" }, options);
  },
  query(
    entity: string,
    inputSchemaRef: string,
    outputSchemaRef: string,
    requiredGrantRefs: readonly string[],
    options: RecipeNodeOptions = {},
  ): SupportedRecipeNode {
    name(entity, "entity");
    if (requiredGrantRefs.length === 0) throw new TypeError("query requires at least one reviewed grant reference");
    requiredGrantRefs.forEach((grant, index) => reference(grant, `requiredGrantRefs[${index}]`));
    return {
      ...base(inputSchemaRef, { kind: "entity_projection", schema_ref: outputSchemaRef, authority: "authoritative" }, options),
      node: { kind: "query", entity },
      effect: { class: "read_only", idempotency: { kind: "intrinsic" }, uncertainty: "impossible" },
      authority: { required_grant_refs: [...new Set(requiredGrantRefs)].sort() },
    };
  },
  get(
    entity: string,
    projectionSchemaRef: string,
    requiredGrantRefs: readonly string[],
    options: RecipeNodeOptions = {},
  ): SupportedRecipeNode {
    name(entity, "entity");
    if (requiredGrantRefs.length === 0) throw new TypeError("get requires at least one reviewed grant reference");
    requiredGrantRefs.forEach((grant, index) => reference(grant, `requiredGrantRefs[${index}]`));
    return {
      ...base(
        projectionSchemaRef,
        { kind: "entity_projection", schema_ref: projectionSchemaRef, authority: "authoritative" },
        options,
      ),
      node: { kind: "get", entity },
      effect: { class: "read_only", idempotency: { kind: "intrinsic" }, uncertainty: "impossible" },
      authority: { required_grant_refs: [...new Set(requiredGrantRefs)].sort() },
    };
  },
  map(
    inputSchemaRef: string,
    outputSchemaRef: string,
    mappingDigest: string,
    operations: readonly RecipeMappingOperation[],
    options: RecipeNodeOptions = {},
  ): SupportedRecipeNode {
    digest(mappingDigest, "mappingDigest");
    if (operations.length === 0 || operations.length > 256) {
      throw new TypeError("map requires a bounded immutable mapping body");
    }
    const targets = new Set<string>();
    const normalized = operations.map((operation, index) => {
      const target = fieldPath(operation.target, `operations[${index}].target`);
      if (targets.has(target)) throw new TypeError("map operation targets must be unique");
      targets.add(target);
      if (operation.kind === "constant") return { ...operation, target };
      if (operation.kind === "select") {
        return {
          ...operation,
          source: fieldPath(operation.source, `operations[${index}].source`),
          target,
        };
      }
      if (operation.kind === "convert" && operation.conversion === "integer_to_decimal") {
        return {
          ...operation,
          source: fieldPath(operation.source, `operations[${index}].source`),
          target,
        };
      }
      if (operation.kind === "map_enum") {
        const values = Object.entries(operation.values)
          .map(([source, destination]) => [name(source, "map_enum source"), name(destination, "map_enum destination")] as const)
          .sort(([left], [right]) => left.localeCompare(right));
        if (values.length === 0 || values.length > 32 || new Set(values.map(([source]) => source)).size !== values.length) {
          throw new TypeError(`operations[${index}] map_enum must be a bounded total declaration`);
        }
        return {
          ...operation,
          source: fieldPath(operation.source, `operations[${index}].source`),
          target,
          values: Object.fromEntries(values),
        };
      }
      throw new TypeError(`operations[${index}] uses an unsupported or partial mapping`);
    });
    return pureNode(
      { kind: "map", mapping_digest: mappingDigest, operations: normalized },
      inputSchemaRef,
      { kind: "typed_value", schema_ref: outputSchemaRef, authority: "derived" },
      options,
    );
  },
  sequence(
    steps: readonly string[],
    inputSchemaRef: string,
    output: RecipeOutput,
    options: RecipeNodeOptions = {},
  ): SupportedRecipeNode {
    if (steps.length === 0) throw new TypeError("sequence requires at least one child");
    steps.forEach((step, index) => name(step, `steps[${index}]`));
    return pureNode({ kind: "sequence", steps: [...steps] }, inputSchemaRef, output, options);
  },
  parallel(
    branches: Readonly<Record<string, string>>,
    inputSchemaRef: string,
    output: RecipeOutput,
    options: RecipeNodeOptions = {},
  ): SupportedRecipeNode {
    const entries = Object.entries(branches).sort(([left], [right]) => left.localeCompare(right));
    if (entries.length < 2) throw new TypeError("parallel requires at least two branches");
    for (const [branch, child] of entries) {
      name(branch, "parallel branch");
      name(child, `parallel branch ${branch}`);
    }
    return pureNode({ kind: "parallel", branches: Object.fromEntries(entries) }, inputSchemaRef, output, options, entries.length);
  },
  switch(
    discriminator: string,
    cases: Readonly<Record<string, string>>,
    inputSchemaRef: string,
    output: RecipeOutput,
    options: RecipeNodeOptions = {},
  ): SupportedRecipeNode {
    name(discriminator, "discriminator");
    const entries = Object.entries(cases).sort(([left], [right]) => left.localeCompare(right));
    if (entries.length === 0) throw new TypeError("switch requires at least one exact case");
    for (const [tag, child] of entries) {
      name(tag, "switch tag");
      name(child, `switch case ${tag}`);
    }
    return pureNode({ kind: "switch", discriminator, cases: Object.fromEntries(entries) }, inputSchemaRef, output, options);
  },
});

export function defineRecipeBundle(input: SupportedRecipeBundle): SupportedRecipeBundle {
  const nodes = Object.entries(input.recipe.nodes).sort(([left], [right]) => left.localeCompare(right));
  if (input.version !== "v1" || input.recipe.version !== "v1" || nodes.length < 1 || nodes.length > MAX_RECIPE_NODES) {
    throw new TypeError("Recipe bundle must be a bounded v1 graph");
  }
  name(input.recipe.root, "recipe.root");
  reference(input.recipe.input_schema_ref, "recipe.input_schema_ref");
  reference(input.recipe.output.schema_ref, "recipe.output.schema_ref");
  const nodeMap = new Map(nodes);
  const childrenByNode = new Map<string, readonly string[]>();
  let aggregateActiveMillis = 0;
  let aggregatePayloadBytes = 0;
  let requiredParallelism = 1;
  for (const [nodeId, definition] of nodes) {
    name(nodeId, "recipe node");
    reference(definition.input_schema_ref, `recipe node ${nodeId} input_schema_ref`);
    reference(definition.output.schema_ref, `recipe node ${nodeId} output.schema_ref`);
    const nodeResources = definition.resources;
    positive(nodeResources.max_active_millis, `recipe node ${nodeId} max_active_millis`);
    positive(nodeResources.max_input_bytes, `recipe node ${nodeId} max_input_bytes`);
    positive(nodeResources.max_output_bytes, `recipe node ${nodeId} max_output_bytes`);
    positive(nodeResources.max_parallelism, `recipe node ${nodeId} max_parallelism`);
    if (definition.node.kind === "contextual_round") {
      if (!Number.isSafeInteger(nodeResources.max_cost_microusd)
        || nodeResources.max_cost_microusd < definition.node.program.limits.aggregate.micro_usd) {
        throw new TypeError("round node cost ceiling is smaller than its participant aggregate");
      }
    } else exactZero(nodeResources.max_cost_microusd, `recipe node ${nodeId} max_cost_microusd`);
    if (nodeResources.max_tool_calls !== (definition.node.kind === "reconcile" ? 1 + Object.keys(definition.node.declaration.sources ?? {}).length : definition.node.kind === "contextual_round" ? definition.node.max_source_pages : definition.node.kind === "store_transaction" ? Object.keys(definition.node.sources).length : 0)) {
      throw new TypeError(`recipe node ${nodeId} has an unsupported tool-call ceiling`);
    }
    if (definition.retry.kind !== "none"
      || definition.cancellation.mode !== "propagate"
      || positive(
        definition.cancellation.acknowledgement_timeout_millis,
        `recipe node ${nodeId} cancellation acknowledgement`,
      ) > nodeResources.max_active_millis
      || definition.provenance_join !== "preserve") {
      throw new TypeError(`recipe node ${nodeId} exceeds the supported lifecycle contract`);
    }
    const isRead = definition.node.kind === "query" || definition.node.kind === "get";
    if (isRead) {
      if (definition.effect.class !== "read_only"
        || definition.effect.idempotency.kind !== "intrinsic"
        || definition.effect.uncertainty !== "impossible"
        || definition.output.kind !== "entity_projection"
        || definition.output.authority !== "authoritative"
        || definition.authority.required_grant_refs === undefined
        || definition.authority.required_grant_refs.length === 0) {
        throw new TypeError(`recipe read node ${nodeId} does not match the exact read-only authority contract`);
      }
      name(definition.node.entity, `recipe node ${nodeId} entity`);
      if (definition.node.kind === "get"
        && definition.input_schema_ref !== definition.output.schema_ref) {
        throw new TypeError(`recipe Get node ${nodeId} must preserve its exact projection schema`);
      }
      definition.authority.required_grant_refs.forEach((grant, index) => {
        reference(grant, `recipe node ${nodeId} required_grant_refs[${index}]`);
      });
    } else if (definition.node.kind === "contextual_round") {
      if (nodes.length !== 1 || nodeId !== input.recipe.root
        || definition.effect.class !== "internal_mutation" || definition.effect.uncertainty !== "receipt_bound"
        || definition.effect.idempotency.kind !== "intrinsic" || definition.output.kind !== "typed_value"
        || definition.output.authority !== "derived"
        || definition.authority.primitive_ref !== definition.node.source.primitive_ref
        || definition.authority.action_ref !== definition.node.source.action_ref) {
        throw new TypeError("contextual round must be one terminal node with its exact reviewed source authority");
      }
      recipeNode.contextualRound(definition.node, definition.input_schema_ref, definition.output.schema_ref,
        definition.authority.required_grant_refs ?? []);
      if (nodeResources.max_parallelism < definition.node.program.limits.max_concurrent) {
        throw new TypeError("round concurrency exceeds its node ceiling");
      }
    } else if (definition.node.kind === "store_transaction") {
      if (nodes.length !== 1 || nodeId !== input.recipe.root
        || definition.effect.class !== "internal_mutation" || definition.effect.uncertainty !== "receipt_bound"
        || definition.effect.idempotency.kind !== "intrinsic" || definition.output.kind !== "typed_value"
        || definition.output.authority !== "derived" || definition.authority.primitive_ref !== undefined
        || definition.authority.action_ref !== undefined || "target_app_ref" in definition.authority) {
        throw new TypeError("store transaction must be one terminal node with fixed own-store authority");
      }
      recipeNode.storeTransaction(definition.node, definition.input_schema_ref, definition.output.schema_ref,
        definition.authority.required_grant_refs ?? []);
    } else if (definition.node.kind === "reconcile") {
      if (nodes.length !== 1 || nodeId !== input.recipe.root
        || definition.effect.class !== "internal_mutation" || definition.effect.uncertainty !== "receipt_bound"
        || definition.effect.idempotency.kind !== "intrinsic" || definition.output.kind !== "typed_value"
        || definition.output.authority !== "derived") {
        throw new TypeError("reconciliation must be one terminal, receipt-bound recipe node");
      }
      recipeNode.reconcile(definition.node.declaration, definition.input_schema_ref, definition.output.schema_ref, {
        primitive_ref: definition.authority.primitive_ref ?? "",
        action_ref: definition.authority.action_ref ?? "",
        required_grant_refs: definition.authority.required_grant_refs ?? [],
      });
    } else if (definition.effect.class !== "none"
      || definition.effect.idempotency.kind !== "not_applicable"
      || definition.effect.uncertainty !== "impossible"
      || definition.output.authority !== "derived"
      || (definition.authority.required_grant_refs?.length ?? 0) !== 0) {
      throw new TypeError(`recipe node ${nodeId} does not match the exact pure derived contract`);
    }
    if (definition.node.kind === "map") {
      digest(definition.node.mapping_digest, `recipe node ${nodeId} mapping_digest`);
      if (!Array.isArray(definition.node.operations)
        || definition.node.operations.length === 0
        || definition.node.operations.length > 256) {
        throw new TypeError(`recipe Map node ${nodeId} requires a bounded immutable body`);
      }
      const targets = new Set<string>();
      definition.node.operations.forEach((operation, index) => {
        const target = fieldPath(operation.target, `recipe node ${nodeId} operations[${index}].target`);
        if (targets.has(target)) throw new TypeError(`recipe Map node ${nodeId} repeats target ${target}`);
        targets.add(target);
        if (operation.kind === "select") {
          fieldPath(operation.source, `recipe node ${nodeId} operations[${index}].source`);
        } else if (operation.kind === "convert") {
          fieldPath(operation.source, `recipe node ${nodeId} operations[${index}].source`);
          if (operation.conversion !== "integer_to_decimal") {
            throw new TypeError(`recipe Map node ${nodeId} uses an unsupported conversion`);
          }
        } else if (operation.kind !== "constant") {
          throw new TypeError(`recipe Map node ${nodeId} uses an unsupported operation`);
        }
      });
    }
    const children = definition.node.kind === "sequence" ? definition.node.steps
      : definition.node.kind === "parallel" ? Object.values(definition.node.branches)
        : definition.node.kind === "switch" ? Object.values(definition.node.cases)
          : definition.node.kind === "query" || definition.node.kind === "get"
            || definition.node.kind === "map" || definition.node.kind === "validate"
            || definition.node.kind === "emit_value" || definition.node.kind === "reconcile" || definition.node.kind === "contextual_round" || definition.node.kind === "store_transaction" ? []
          : (() => { throw new TypeError(`unsupported Recipe node kind: ${String((definition.node as { kind?: unknown }).kind)}`); })();
    if (definition.node.kind === "parallel") {
      if (children.length < 2 || children.length > nodeResources.max_parallelism) {
        throw new TypeError(`recipe parallel node ${nodeId} exceeds its declared execution width`);
      }
      requiredParallelism = Math.max(requiredParallelism, children.length);
    } else if (definition.node.kind === "contextual_round") {
      requiredParallelism = Math.max(requiredParallelism, nodeResources.max_parallelism);
    } else if (nodeResources.max_parallelism !== 1) {
      throw new TypeError(`non-Parallel recipe node ${nodeId} must declare max_parallelism 1`);
    }
    aggregateActiveMillis += nodeResources.max_active_millis;
    aggregatePayloadBytes += nodeResources.max_input_bytes + nodeResources.max_output_bytes;
    if (!Number.isSafeInteger(aggregateActiveMillis) || !Number.isSafeInteger(aggregatePayloadBytes)) {
      throw new TypeError("recipe resource aggregation exceeds safe integer bounds");
    }
    childrenByNode.set(nodeId, children);
    for (const child of children) {
      if (!nodeMap.has(child)) throw new TypeError(`recipe node ${nodeId} references unknown child ${child}`);
    }
  }
  if (!nodeMap.has(input.recipe.root)) throw new TypeError("recipe.root is not present in nodes");
  const root = nodeMap.get(input.recipe.root);
  if (root === undefined
    || root.input_schema_ref !== input.recipe.input_schema_ref
    || root.output.kind !== input.recipe.output.kind
    || root.output.schema_ref !== input.recipe.output.schema_ref
    || root.output.authority !== input.recipe.output.authority) {
    throw new TypeError("recipe root does not match the exact input/output boundary");
  }
  const state = new Map<string, 0 | 1 | 2>();
  const indegree = new Map<string, number>();
  let edgeCount = 0;
  let maxDepth = 0;
  const stack: Array<{ readonly node: string; readonly depth: number; readonly exiting: boolean; readonly parallelAncestor: boolean }> = [
    { node: input.recipe.root, depth: 1, exiting: false, parallelAncestor: false },
  ];
  while (stack.length > 0) {
    const current = stack.pop();
    if (current === undefined) break;
    if (current.exiting) {
      state.set(current.node, 2);
      continue;
    }
    if (state.get(current.node) === 1) throw new TypeError(`recipe cycle through ${current.node}`);
    if (state.get(current.node) === 2) continue;
    state.set(current.node, 1);
    maxDepth = Math.max(maxDepth, current.depth);
    const definition = nodeMap.get(current.node);
    if (definition === undefined) throw new TypeError(`unknown recipe node ${current.node}`);
    if (current.parallelAncestor && definition.node.kind === "parallel") {
      throw new TypeError("nested Parallel exceeds the implementation-ready execution-width owner");
    }
    const children = childrenByNode.get(current.node) ?? [];
    edgeCount += children.length;
    stack.push({ ...current, exiting: true });
    for (let index = children.length - 1; index >= 0; index -= 1) {
      const child = children[index];
      if (child === undefined) continue;
      indegree.set(child, (indegree.get(child) ?? 0) + 1);
      stack.push({
        node: child,
        depth: current.depth + 1,
        exiting: false,
        parallelAncestor: current.parallelAncestor || definition.node.kind === "parallel",
      });
    }
  }
  if (state.size !== nodes.length) throw new TypeError("recipe contains unreachable nodes");
  for (const [nodeId, degree] of indegree) {
    if (nodeId !== input.recipe.root && degree !== 1) throw new TypeError(`recipe node ${nodeId} has shared control ownership`);
  }
  positive(input.recipe.ceilings.max_nodes, "ceilings.max_nodes");
  positive(input.recipe.ceilings.max_edges, "ceilings.max_edges");
  positive(input.recipe.ceilings.max_depth, "ceilings.max_depth");
  positive(input.recipe.ceilings.max_fan_out, "ceilings.max_fan_out");
  positive(input.recipe.ceilings.max_parallelism, "ceilings.max_parallelism");
  positive(input.recipe.ceilings.max_payload_bytes, "ceilings.max_payload_bytes");
  positive(input.recipe.ceilings.max_active_millis, "ceilings.max_active_millis");
  if (root.node.kind === "contextual_round") {
    if (!Number.isSafeInteger(input.recipe.ceilings.max_cost_microusd)
      || input.recipe.ceilings.max_cost_microusd < root.resources.max_cost_microusd) {
      throw new TypeError("round cost exceeds its graph ceiling");
    }
  } else exactZero(input.recipe.ceilings.max_cost_microusd, "ceilings.max_cost_microusd");
  if (input.recipe.ceilings.max_tool_calls !== (root.node.kind === "reconcile" ? 1 + Object.keys(root.node.declaration.sources ?? {}).length : root.node.kind === "contextual_round" ? root.node.max_source_pages : root.node.kind === "store_transaction" ? Object.keys(root.node.sources).length : 0)) {
    throw new TypeError("recipe tool-call ceiling does not match its admitted owner");
  }
  if (nodes.length > input.recipe.ceilings.max_nodes
    || edgeCount > input.recipe.ceilings.max_edges
    || maxDepth > input.recipe.ceilings.max_depth
    || [...childrenByNode.values()].some((children) => children.length > input.recipe.ceilings.max_fan_out)
    || requiredParallelism > input.recipe.ceilings.max_parallelism
    || aggregatePayloadBytes > input.recipe.ceilings.max_payload_bytes
    || aggregateActiveMillis > input.recipe.ceilings.max_active_millis) {
    throw new TypeError("recipe exceeds its declared graph ceilings");
  }
  positive(input.recipe.evolution.topology_revision, "evolution.topology_revision");
  if (input.recipe.evolution.topology_revision === 1 && input.recipe.evolution.predecessor_recipe_ref !== undefined) {
    throw new TypeError("topology revision 1 cannot declare a predecessor");
  }
  return {
    version: "v1",
    schemas: Object.fromEntries(Object.entries(input.schemas).sort(([left], [right]) => left.localeCompare(right))),
    recipe: { ...input.recipe, nodes: Object.fromEntries(nodes) },
  };
}
