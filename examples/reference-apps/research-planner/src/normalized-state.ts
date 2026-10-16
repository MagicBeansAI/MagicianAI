import type { AppMutationReceipt, AppRecordProjection, JsonValue } from "@magician/apps";

export interface NormalizedRecord {
  readonly entity: string;
  readonly recordId: string;
  readonly revision: number;
  readonly fields: Readonly<Record<string, JsonValue>>;
  readonly pendingKey?: string;
}

export class NormalizedEntityState {
  readonly #records = new Map<string, NormalizedRecord>();

  static key(entity: string, recordId: string): string {
    return `${entity}\u0000${recordId}`;
  }

  replaceEntity(entity: string, records: readonly AppRecordProjection[]): void {
    for (const [key, record] of this.#records) {
      if (record.entity === entity && record.pendingKey === undefined) this.#records.delete(key);
    }
    for (const record of records) this.upsertProjection(record);
  }

  upsertProjection(record: AppRecordProjection): void {
    const key = NormalizedEntityState.key(record.entity, record.record_id);
    const current = this.#records.get(key);
    if (current !== undefined && current.pendingKey === undefined && current.revision > record.record_revision) return;
    this.#records.set(key, {
      entity: record.entity,
      recordId: record.record_id,
      revision: record.record_revision,
      fields: record.fields,
    });
  }

  stageCreate(entity: string, temporaryId: string, fields: Readonly<Record<string, JsonValue>>, idempotencyKey: string): void {
    this.#records.set(NormalizedEntityState.key(entity, temporaryId), {
      entity,
      recordId: temporaryId,
      revision: 0,
      fields,
      pendingKey: idempotencyKey,
    });
  }

  rollback(idempotencyKey: string): void {
    for (const [key, record] of this.#records) {
      if (record.pendingKey === idempotencyKey) this.#records.delete(key);
    }
  }

  commitCreate(idempotencyKey: string, receipt: AppMutationReceipt): void {
    if (receipt.origin.kind !== "owner_api" || receipt.origin.request_ref !== idempotencyKey) {
      throw new Error("mutation receipt does not correlate to the optimistic request");
    }
    const pending = [...this.#records.entries()].find(([, record]) => record.pendingKey === idempotencyKey);
    if (pending === undefined) throw new Error("optimistic mutation state is missing");
    const [pendingKey, record] = pending;
    const committed = receipt.committed_record_revisions.find((revision) => revision.entity === record.entity);
    if (committed === undefined) throw new Error("mutation receipt omitted the created entity revision");
    this.#records.delete(pendingKey);
    this.#records.set(NormalizedEntityState.key(committed.entity, committed.record_id), {
      entity: committed.entity,
      recordId: committed.record_id,
      revision: committed.revision,
      fields: record.fields,
    });
  }

  snapshot(entity?: string): readonly NormalizedRecord[] {
    return [...this.#records.values()]
      .filter((record) => entity === undefined || record.entity === entity)
      .sort((left, right) => left.entity.localeCompare(right.entity) || left.recordId.localeCompare(right.recordId));
  }
}
