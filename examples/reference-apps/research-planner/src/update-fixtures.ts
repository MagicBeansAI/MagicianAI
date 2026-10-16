import type { JsonValue } from "@magician/apps";

export interface ResearchPlanV011 {
  readonly topic_id: string;
  readonly body: string;
  readonly status: "draft" | "approved";
  readonly revision_note: string | null;
}

export interface ResearchPlanV020 {
  readonly topic_id: string;
  readonly body: string;
  readonly status: "draft" | "approved";
  readonly reviewed_at: string | null;
}

type UpdateOperation =
  | {
      readonly kind: "add_field";
      readonly entity: "research_plan";
      readonly field: "reviewed_at";
      readonly scalar: "timestamp";
      readonly nullable: true;
    }
  | {
      readonly kind: "retire_field";
      readonly entity: "research_plan";
      readonly field: "revision_note";
    };

/**
 * Provider-free proof for the exact checked-in v0.1.1 -> v0.2.0 migration
 * input. The production migration coordinator remains the only owner allowed
 * to dry-run, back up, switch, or rewind installed records.
 */
export function applyReviewedUpdateFixture(
  input: ResearchPlanV011,
  operations: JsonValue,
): ResearchPlanV020 {
  const parsed = exactOperations(operations);
  assertPlanV011(input);
  let working: Record<string, JsonValue> = { ...input };
  for (const operation of parsed) {
    if (operation.kind === "add_field") {
      if (Object.hasOwn(working, operation.field)) throw new Error("migration add_field would overwrite an existing value");
      working = { ...working, [operation.field]: null };
    } else {
      if (!Object.hasOwn(working, operation.field)) throw new Error("migration retire_field source is absent");
      const { [operation.field]: retired, ...remaining } = working;
      if (retired !== null && typeof retired !== "string") throw new Error("migration retire_field source was substituted");
      working = remaining;
    }
  }
  assertPlanV020(working);
  return working;
}

function exactOperations(value: JsonValue): readonly UpdateOperation[] {
  if (!Array.isArray(value) || value.length !== 2) throw new Error("update migration requires its exact two-operation plan");
  const [add, retire] = value;
  if (!isObject(add)
    || !sameKeys(add, ["entity", "field", "kind", "nullable", "scalar"])
    || add.kind !== "add_field"
    || add.entity !== "research_plan"
    || add.field !== "reviewed_at"
    || add.scalar !== "timestamp"
    || add.nullable !== true) {
    throw new Error("update migration add_field operation was substituted");
  }
  if (!isObject(retire)
    || !sameKeys(retire, ["entity", "field", "kind"])
    || retire.kind !== "retire_field"
    || retire.entity !== "research_plan"
    || retire.field !== "revision_note") {
    throw new Error("update migration retire_field operation was substituted");
  }
  return [add as unknown as UpdateOperation, retire as unknown as UpdateOperation];
}

function assertPlanV011(value: unknown): asserts value is ResearchPlanV011 {
  if (!isObject(value)
    || !sameKeys(value, ["body", "revision_note", "status", "topic_id"])
    || typeof value.topic_id !== "string"
    || typeof value.body !== "string"
    || (value.status !== "draft" && value.status !== "approved")
    || (value.revision_note !== null && typeof value.revision_note !== "string")) {
    throw new Error("v0.1.1 research-plan fixture was substituted");
  }
}

function assertPlanV020(value: unknown): asserts value is ResearchPlanV020 {
  if (!isObject(value)
    || !sameKeys(value, ["body", "reviewed_at", "status", "topic_id"])
    || typeof value.topic_id !== "string"
    || typeof value.body !== "string"
    || (value.status !== "draft" && value.status !== "approved")
    || (value.reviewed_at !== null && typeof value.reviewed_at !== "string")) {
    throw new Error("v0.2.0 research-plan fixture is invalid");
  }
}

function sameKeys(value: Readonly<Record<string, unknown>>, expected: readonly string[]): boolean {
  const observed = Object.keys(value).sort();
  return observed.length === expected.length
    && observed.every((key, index) => key === [...expected].sort()[index]);
}

function isObject(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
