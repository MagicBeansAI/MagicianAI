export const REFUSED_FEATURES = Object.freeze({
  recipe_retry: "Recipe v1 Retry is declared but not in the implementation-ready lowering subset.",
  recipe_effects: "Mutation, tool/action dispatch, procedures, agent calls, artifacts, receipts, and other effectful Recipe nodes are not lowered in v1.",
  unavailable_physical_owner: "A missing reviewed physical owner is typed unavailable; this app does not approximate it with raw provider access.",
  attention_destination: "Attention, Today, Worth a Look, and notification adapters remain destination-specific deferred work.",
  task_plan_destination: "Canonical task and plan proposal destinations remain deferred and are not approximated with app-owned writes.",
} as const);

export type RefusedFeature = keyof typeof REFUSED_FEATURES;

export const REFUSED_RECIPE_NODE_KINDS = Object.freeze([
  "retry", "call_tool", "invoke_action", "mutate",
  "run_procedure", "agent_as_tool", "emit_artifact", "emit_receipt",
  "mark_uncertain", "deferred",
] as const);

export type RefusedRecipeNodeKind = (typeof REFUSED_RECIPE_NODE_KINDS)[number];

export function refuse(feature: RefusedFeature): never {
  throw new Error(`unsupported_public_contract:${feature}:${REFUSED_FEATURES[feature]}`);
}

export function refuseRecipeNode(kind: RefusedRecipeNodeKind): never {
  throw new Error(`unsupported_recipe_node:${kind}`);
}
