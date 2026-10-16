import type {
  AppOutputValidator,
  AppRecordProjection,
  JsonValue,
} from "@magician/apps";

export const APP_PROTOCOL_VERSION = "1" as const;
export const ENTITY_NAMES = ["research_topic", "research_source", "research_plan"] as const;
export type ResearchEntityName = (typeof ENTITY_NAMES)[number];

export interface ResearchTopicInput {
  readonly title: string;
  readonly query: string;
  readonly status: "draft" | "active" | "complete";
  readonly priority: number;
  readonly updated_at: string;
}

export interface BuildPlanInput extends JsonValueObject {
  readonly topic_id: string;
  readonly query: string;
  readonly start_date: string;
  readonly end_date: string;
}

export interface BuildPlanOutput extends JsonValueObject {
  readonly plan_id: string;
  readonly status: "draft" | "approved";
}

export interface JsonValueObject {
  readonly [key: string]: JsonValue;
}

const OPAQUE_ID = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/;
const RFC3339 = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(?:Z|([+-])(\d{2}):(\d{2}))$/;

function isObject(value: JsonValue): value is { readonly [key: string]: JsonValue } {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function exactKeys(value: { readonly [key: string]: JsonValue }, keys: readonly string[]): boolean {
  const observed = Object.keys(value).sort();
  const expected = [...keys].sort();
  return observed.length === expected.length && observed.every((key, index) => key === expected[index]);
}

export const isBuildPlanOutput: AppOutputValidator<BuildPlanOutput> = (
  value: JsonValue,
): value is BuildPlanOutput => {
  if (!isObject(value) || !exactKeys(value, ["plan_id", "status"])) return false;
  return typeof value.plan_id === "string"
    && value.plan_id.length > 0
    && (value.status === "draft" || value.status === "approved");
};

export function isResearchProjection(value: AppRecordProjection): boolean {
  if (!ENTITY_NAMES.includes(value.entity as ResearchEntityName)
    || !OPAQUE_ID.test(value.record_id)
    || !Number.isSafeInteger(value.record_revision)
    || value.record_revision < 1) return false;
  const fields = value.fields;
  if (value.entity === "research_topic") {
    return exactObjectKeys(fields, ["priority", "query", "status", "title", "updated_at"])
      && boundedText(fields.title, 65_536)
      && boundedText(fields.query, 65_536)
      && (fields.status === "draft" || fields.status === "active" || fields.status === "complete")
      && typeof fields.priority === "number" && Number.isSafeInteger(fields.priority)
      && isRfc3339(fields.updated_at);
  }
  if (value.entity === "research_source") {
    return exactObjectKeys(fields, ["captured_at", "status", "summary", "title", "topic_id", "url"])
      && typeof fields.topic_id === "string" && OPAQUE_ID.test(fields.topic_id)
      && boundedText(fields.title, 65_536) && boundedText(fields.url, 65_536)
      && boundedText(fields.summary, 65_536)
      && (fields.status === "captured" || fields.status === "reviewed" || fields.status === "rejected")
      && isRfc3339(fields.captured_at);
  }
  const planKeys = Object.hasOwn(fields, "revision_note")
    ? ["body", "revision_note", "status", "topic_id"]
    : ["body", "status", "topic_id"];
  return exactObjectKeys(fields, planKeys)
    && typeof fields.topic_id === "string" && OPAQUE_ID.test(fields.topic_id)
    && boundedText(fields.body, 65_536)
    && (fields.status === "draft" || fields.status === "approved")
    && (!Object.hasOwn(fields, "revision_note")
      || fields.revision_note === null
      || boundedText(fields.revision_note, 65_536));
}

export function assertResearchTopicInput(value: ResearchTopicInput): void {
  if (!exactObjectKeys(value as unknown as Readonly<Record<string, unknown>>, ["priority", "query", "status", "title", "updated_at"])
    || !boundedText(value.title, 4096)
    || !boundedText(value.query, 4096)
    || !["draft", "active", "complete"].includes(value.status)
    || !Number.isSafeInteger(value.priority)
    || value.priority < 0 || value.priority > 100
    || !isRfc3339(value.updated_at)) {
    throw new TypeError("research topic input does not match its bounded manifest schema");
  }
}

export function isOpaqueRecordId(value: unknown): value is string {
  return typeof value === "string" && OPAQUE_ID.test(value);
}

export function isCalendarDate(value: unknown): value is string {
  if (typeof value !== "string") return false;
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (match === null) return false;
  return validDateParts(Number(match[1]), Number(match[2]), Number(match[3]));
}

export function isRfc3339(value: unknown): value is string {
  if (typeof value !== "string") return false;
  const match = RFC3339.exec(value);
  if (match === null
    || !validDateParts(Number(match[1]), Number(match[2]), Number(match[3]))
    || Number(match[4]) > 23 || Number(match[5]) > 59 || Number(match[6]) > 59
    || (match[8] !== undefined && Number(match[8]) > 23)
    || (match[9] !== undefined && Number(match[9]) > 59)) return false;
  return Number.isFinite(Date.parse(value));
}

function validDateParts(year: number, month: number, day: number): boolean {
  if (year < 1 || year > 9999 || month < 1 || month > 12 || day < 1) return false;
  const date = new Date(0);
  date.setUTCHours(0, 0, 0, 0);
  date.setUTCFullYear(year, month - 1, day);
  return date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day;
}

function boundedText(value: unknown, maxBytes: number): value is string {
  return typeof value === "string" && value.length > 0
    && new TextEncoder().encode(value).byteLength <= maxBytes;
}

function exactObjectKeys(value: Readonly<Record<string, unknown>>, expected: readonly string[]): boolean {
  const observed = Object.keys(value).sort();
  const sortedExpected = [...expected].sort();
  return observed.length === sortedExpected.length
    && observed.every((key, index) => key === sortedExpected[index]);
}
