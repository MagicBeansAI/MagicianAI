import type { JsonValue } from "@magician/apps";
import { isCalendarDate, isOpaqueRecordId } from "./contracts.js";

export type ActionField =
  | { readonly kind: "text"; readonly name: string; readonly label: string; readonly required: boolean; readonly maxBytes: number }
  | { readonly kind: "date"; readonly name: string; readonly label: string; readonly required: boolean }
  | { readonly kind: "reference"; readonly name: string; readonly label: string; readonly required: boolean; readonly entity: string };

export interface ActionFormSchema {
  readonly actionId: "build_plan";
  readonly fields: readonly ActionField[];
  readonly additionalProperties: false;
}

export interface AccessibleActionField {
  readonly inputId: string;
  readonly label: string;
  readonly describedBy: string;
  readonly autocomplete: "off";
  readonly required: boolean;
  readonly referenceEntity?: string;
}

export const BUILD_PLAN_FORM: ActionFormSchema = {
  actionId: "build_plan",
  additionalProperties: false,
  fields: [
    { kind: "reference", name: "topic_id", label: "Research topic", required: true, entity: "research_topic" },
    { kind: "text", name: "query", label: "Research question", required: true, maxBytes: 4096 },
    { kind: "date", name: "start_date", label: "Start date", required: true },
    { kind: "date", name: "end_date", label: "End date", required: true },
  ],
};

export function validateActionFormInput(schema: ActionFormSchema, value: JsonValue): void {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError("action input must be an object");
  }
  const names = new Set(schema.fields.map((field) => field.name));
  for (const key of Object.keys(value)) {
    if (!names.has(key)) throw new TypeError(`unknown action field: ${key}`);
  }
  for (const field of schema.fields) {
    const observed = value[field.name];
    if (observed === undefined) {
      if (field.required) throw new TypeError(`missing action field: ${field.name}`);
      continue;
    }
    if (typeof observed !== "string" || observed.length === 0) {
      throw new TypeError(`action field ${field.name} must be a non-empty string`);
    }
    if (field.kind === "text" && new TextEncoder().encode(observed).byteLength > field.maxBytes) {
      throw new TypeError(`action field ${field.name} exceeds ${field.maxBytes} UTF-8 bytes`);
    }
    if (field.kind === "date" && !isCalendarDate(observed)) {
      throw new TypeError(`action field ${field.name} must use YYYY-MM-DD`);
    }
    if (field.kind === "reference" && !isOpaqueRecordId(observed)) {
      throw new TypeError(`action field ${field.name} must be a canonical record id`);
    }
  }
  const start = value.start_date;
  const end = value.end_date;
  if (typeof start === "string" && typeof end === "string" && end < start) {
    throw new TypeError("action end_date must not precede start_date");
  }
}

export function accessibleActionFields(schema: ActionFormSchema): readonly AccessibleActionField[] {
  return schema.fields.map((field) => ({
    inputId: `action-${schema.actionId}-${field.name}`,
    label: field.label,
    describedBy: `action-${schema.actionId}-${field.name}-help`,
    autocomplete: "off",
    required: field.required,
    ...(field.kind === "reference" ? { referenceEntity: field.entity } : {}),
  }));
}
