import { BUILD_PLAN_FORM, validateActionFormInput } from "./action-forms.js";
import type { BuildPlanLaunchRecovery, RunRecoveryStore } from "./client.js";

const RUN_REF = /^run:app-action:[A-Za-z0-9_.:@#/-]{1,160}$/;

export interface RecoveryTextOwner {
  read(): string | undefined;
  write(value: string | undefined): void;
}

/** Strict restart-safe adapter over caller-owned durable text storage. */
export class JsonRunRecoveryStore implements RunRecoveryStore {
  readonly #owner: RecoveryTextOwner;

  constructor(owner: RecoveryTextOwner) {
    this.#owner = owner;
  }

  save(value: BuildPlanLaunchRecovery): void {
    assertRecoveryDocument(value);
    this.#owner.write(JSON.stringify(value));
  }

  load(): BuildPlanLaunchRecovery | undefined {
    const text = this.#owner.read();
    if (text === undefined) return undefined;
    if (new TextEncoder().encode(text).byteLength > 16_384) {
      throw new Error("retained build-plan recovery exceeds its byte ceiling");
    }
    let value: unknown;
    try {
      value = JSON.parse(text);
    } catch {
      throw new Error("retained build-plan recovery is malformed");
    }
    assertRecoveryDocument(value);
    return value;
  }

  clear(): void {
    this.#owner.write(undefined);
  }
}

function assertRecoveryDocument(value: unknown): asserts value is BuildPlanLaunchRecovery {
  if (!isRecord(value)) throw new Error("retained build-plan recovery is malformed");
  const expected = value.runRef === undefined
    ? ["actionId", "idempotencyKey", "input", "installationId", "version"]
    : ["actionId", "idempotencyKey", "input", "installationId", "runRef", "version"];
  if (!hasExactKeys(value, expected)
    || value.version !== 1
    || value.actionId !== "build_plan"
    || !boundedIdentity(value.installationId, 128)
    || !boundedIdentity(value.idempotencyKey, 192)
    || value.runRef !== undefined && (typeof value.runRef !== "string" || !RUN_REF.test(value.runRef))) {
    throw new Error("retained build-plan recovery is malformed or substituted");
  }
  validateActionFormInput(BUILD_PLAN_FORM, value.input as never);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasExactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  const observed = Object.keys(value).sort();
  const sorted = [...expected].sort();
  return observed.length === sorted.length
    && observed.every((key, index) => key === sorted[index]);
}

function boundedIdentity(value: unknown, maximum: number): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= maximum
    && /^[A-Za-z0-9][A-Za-z0-9_.:@#/-]*$/.test(value);
}
