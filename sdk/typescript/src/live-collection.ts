import type { MagicianAppsClient } from "./client.js";
import type { AppEntityChangeBatch, AppPredicate, AppQueryPage, AppQueryRequest, AppRecordProjection } from "./types.js";

/** Works with the HTTP client or a capability-bound query/change bridge. */
export type AppCollectionTransport = Pick<MagicianAppsClient, "queryData" | "readEntityChanges">;

/** RFC3339 ordering with sub-millisecond precision, as used by the app store. */
export function compareAppTimestamps(left: string, right: string): number {
  const precise = (value: string): [number, number] => {
    const millis = Date.parse(value);
    if (!Number.isFinite(millis)) throw new TypeError("Invalid app timestamp");
    const fraction = value.match(/\.(\d{1,9})(?:Z|[+-]\d{2}:\d{2})$/i)?.[1] ?? '';
    return [millis, Number(fraction.padEnd(9, '0').slice(3))];
  };
  const a = precise(left); const b = precise(right);
  return a[0] - b[0] || a[1] - b[1];
}

export interface AppLiveCollectionOptions {
  readonly request: Omit<AppQueryRequest, "limit" | "cursor" | "pagination">;
  readonly surfaceRevision: number;
  /** The entity's indexed identity field, whose value equals record_id. */
  readonly recordIdField: string;
  readonly pageSize?: number;
  /** Must match the query order, including the final ascending record_id tie break. */
  readonly compare: (left: AppRecordProjection, right: AppRecordProjection) => number;
}

export interface AppLiveCollectionState {
  readonly records: readonly AppRecordProjection[];
  readonly ready: boolean;
  readonly loading: boolean;
  readonly loadingMore: boolean;
  readonly hasMore: boolean;
  readonly moreChanges: boolean;
  readonly changeSequence: number;
  readonly resetReason: "history_unavailable" | "cursor_expired" | null;
  readonly error: Error | null;
}

function intersect(left: AppPredicate | undefined, right: AppPredicate): AppPredicate {
  if (!left) return right;
  const offset = left.nodes.length;
  const shifted = right.nodes.map((node) => {
    if (node.kind === "all" || node.kind === "any") return { ...node, children: node.children.map((i) => i + offset) };
    if (node.kind === "not") return { ...node, child: node.child + offset };
    return node;
  });
  return { root: offset + shifted.length, nodes: [...left.nodes, ...shifted,
    { kind: "all", children: [left.root, right.root + offset] }] };
}

/** Indexed keyset pagination plus durable, identifier-only change catch-up. No model work. */
export class AppLiveCollection {
  readonly #transport: AppCollectionTransport;
  readonly #options: AppLiveCollectionOptions;
  readonly #pageSize: number;
  readonly #abort = new AbortController();
  readonly #listeners = new Set<(state: AppLiveCollectionState) => void>();
  #state: AppLiveCollectionState = { records: [], ready: false, loading: false, loadingMore: false,
    hasMore: false, moreChanges: false, changeSequence: 0, resetReason: null, error: null };
  #cursor: string | undefined;
  #binding: string | undefined;
  #boundary: AppRecordProjection | undefined;
  #tail: Promise<void> = Promise.resolve();
  #load: Promise<void> | undefined;
  #sync: Promise<void> | undefined;

  constructor(transport: AppCollectionTransport, options: AppLiveCollectionOptions) {
    this.#pageSize = options.pageSize ?? 25;
    if (!Number.isSafeInteger(this.#pageSize) || this.#pageSize < 1 || this.#pageSize > 200
      || !Number.isSafeInteger(options.surfaceRevision) || options.surfaceRevision < 1
      || !options.request.select.includes(options.recordIdField)) {
      throw new TypeError("A collection needs a valid page size, surface revision and selected identity field");
    }
    this.#transport = transport;
    this.#options = { ...options, request: structuredClone(options.request) };
  }

  get state(): AppLiveCollectionState { return this.#state; }

  subscribe(listener: (state: AppLiveCollectionState) => void): () => void {
    this.#listeners.add(listener);
    listener(this.#state);
    return () => { this.#listeners.delete(listener); };
  }

  /** Abort outstanding reads and prevent a previous scope from publishing results. */
  dispose(): void { this.#abort.abort(); this.#listeners.clear(); }

  start(): Promise<void> {
    return this.#enqueue(async () => { if (!this.#state.ready) await this.#hydrate(null); });
  }

  loadMore(): Promise<void> {
    if (this.#load) return this.#load;
    this.#load = this.#enqueue(async () => {
      if (!this.#state.ready) { await this.#hydrate(null); return; }
      if (!this.#cursor) return;
      this.#publish({ loadingMore: true });
      try {
        const page = await this.#query({ ...this.#options.request, limit: this.#pageSize, cursor: this.#cursor });
        // Keyset pages already read current heads in one database snapshot.
        const records = page.envelope.value;
        this.#cursor = page.next_cursor;
        this.#boundary = page.envelope.value.at(-1) ?? this.#boundary;
        this.#publish({ records: this.#merge(this.#state.records, records), hasMore: Boolean(this.#cursor) });
      } catch (error) {
        const reason = (error as { reasonCode?: string })?.reasonCode;
        if (reason === "app_data_cursor_unavailable" || reason === "invalid_app_data_cursor") {
          await this.#hydrate("cursor_expired");
        } else throw error;
      } finally { this.#publish({ loadingMore: false }); }
    }).finally(() => { this.#load = undefined; });
    return this.#load;
  }

  /** One bounded change page. Call again when moreChanges; events are wake-up hints only. */
  synchronize(): Promise<void> {
    if (this.#sync) return this.#sync;
    this.#sync = this.#enqueue(async () => {
      if (!this.#state.ready) { await this.#hydrate(null); return; }
      let batch: AppEntityChangeBatch;
      try {
        batch = await this.#transport.readEntityChanges(this.#options.request.source_installation_id, {
          surfaceRevision: this.#options.surfaceRevision, afterChangeSequence: this.#state.changeSequence,
          limit: 64, signal: this.#abort.signal,
        });
      } catch (error) {
        if ((error as { reasonCode?: string })?.reasonCode === 'app_change_cursor_ahead') {
          await this.#hydrate('history_unavailable'); return;
        }
        throw error;
      }
      if (batch.reset_required) { await this.#hydrate("history_unavailable"); return; }
      const changed = [...new Set(batch.changes.filter((row) => row.entity === this.#options.request.entity)
        .map((row) => row.record_id))];
      const fresh = await this.#readRecords(changed);
      const affected = new Set(changed);
      const retainedIds = new Set(this.#state.records.map((row) => row.record_id));
      const visible = fresh.filter((row) => retainedIds.has(row.record_id) || !this.#cursor
        || !this.#boundary || this.#options.compare(row, this.#boundary) <= 0);
      // Missing rows are deleted or no longer match this collection's predicate.
      // A sequence is acknowledged only after every canonical read succeeds.
      this.#publish({ records: changed.length ? this.#merge(this.#state.records.filter((row) => !affected.has(row.record_id)), visible)
        : this.#state.records, changeSequence: batch.through_change_sequence, moreChanges: batch.has_more });
    }).finally(() => { this.#sync = undefined; });
    return this.#sync;
  }

  async #hydrate(reason: AppLiveCollectionState["resetReason"]): Promise<void> {
    this.#publish({ loading: true });
    try {
      // Capture the durable head BEFORE querying. A concurrent commit during
      // hydration is then caught by synchronize, including an initially empty feed.
      const head = await this.#transport.readEntityChanges(this.#options.request.source_installation_id, {
        surfaceRevision: this.#options.surfaceRevision, afterChangeSequence: 0, limit: 1, signal: this.#abort.signal,
      });
      const page = await this.#query({ ...this.#options.request, limit: this.#pageSize });
      this.#cursor = page.next_cursor;
      this.#boundary = page.envelope.value.at(-1);
      this.#publish({ records: this.#merge([], page.envelope.value), ready: true,
        hasMore: Boolean(this.#cursor), changeSequence: head.current_change_sequence,
        moreChanges: true, resetReason: reason });
    } finally { this.#publish({ loading: false }); }
  }

  async #query(request: AppQueryRequest): Promise<AppQueryPage> {
    const page = await this.#transport.queryData(request.source_installation_id, { ...request, pagination: "keyset" }, { signal: this.#abort.signal });
    this.#abort.signal.throwIfAborted();
    const envelope = page.envelope;
    const binding = JSON.stringify([envelope.scope_binding_ref, envelope.installation_id,
      envelope.package_revision_ref, envelope.schema_revision, envelope.grant_revision]);
    if (this.#binding !== undefined && this.#binding !== binding) {
      this.#publish({ records: [], ready: false, hasMore: false });
      throw new Error("The collection's app or workspace binding changed. Reopen the app.");
    }
    this.#binding = binding;
    for (const row of envelope.value) {
      if (row.fields[this.#options.recordIdField] !== row.record_id) {
        throw new Error("Collection identity field must equal the canonical record_id");
      }
    }
    return page;
  }

  async #readRecords(ids: readonly string[]): Promise<AppRecordProjection[]> {
    const records: AppRecordProjection[] = [];
    for (let offset = 0; offset < ids.length; offset += this.#pageSize) {
      const chunk = ids.slice(offset, offset + this.#pageSize);
      const predicate = intersect(this.#options.request.predicate, { root: 0, nodes: [
        { kind: "in", field: this.#options.recordIdField, values: chunk },
      ] });
      const page = await this.#query({ ...this.#options.request, predicate, limit: this.#pageSize });
      if (page.next_cursor) throw new Error("Collection identity lookup was not unique");
      records.push(...page.envelope.value);
    }
    return records;
  }

  #merge(retained: readonly AppRecordProjection[], incoming: readonly AppRecordProjection[]): AppRecordProjection[] {
    const rows = new Map(retained.map((row) => [row.record_id, row]));
    for (const row of incoming) {
      if ((rows.get(row.record_id)?.record_revision ?? 0) <= row.record_revision) rows.set(row.record_id, row);
    }
    return [...rows.values()].sort(this.#options.compare);
  }

  #publish(patch: Partial<AppLiveCollectionState>): void {
    if (this.#abort.signal.aborted) return;
    this.#state = { ...this.#state, ...patch };
    for (const listener of this.#listeners) listener(this.#state);
  }

  #enqueue(operation: () => Promise<void>): Promise<void> {
    const next = this.#tail.then(async () => {
      this.#abort.signal.throwIfAborted();
      this.#publish({ error: null });
      try { await operation(); }
      catch (error) {
        this.#publish({ error: error instanceof Error ? error : new Error(String(error)) });
        throw error;
      }
    });
    this.#tail = next.catch(() => undefined);
    return next;
  }
}
