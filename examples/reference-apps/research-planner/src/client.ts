import {
  MagicianAppsClient,
  MagicianAppsError,
  type AppActionLaunchResponse,
  type AppActionCompositionHop,
  type AppActionResultComposition,
  type AppEntityChangeBatch,
  type AppMutationCommand,
  type AppMutationReceipt,
  type AppQueryPage,
  type AppRecordProjection,
  type AppRequestOptions,
  type AppRunSnapshot,
  type AppValueMappingOperation,
  type JsonValue,
} from "@magician/apps";

import { BUILD_PLAN_FORM, validateActionFormInput } from "./action-forms.js";
import {
  APP_PROTOCOL_VERSION,
  assertResearchTopicInput,
  isResearchProjection,
  isBuildPlanOutput,
  type BuildPlanInput,
  type BuildPlanOutput,
  type ResearchTopicInput,
} from "./contracts.js";
import { NormalizedEntityState } from "./normalized-state.js";

const QUERY_FIELDS = {
  research_topic: ["title", "query", "status", "priority", "updated_at"],
  research_source: ["topic_id", "title", "url", "summary", "status", "captured_at"],
  research_plan: ["topic_id", "body", "status", "revision_note"],
} as const;

const MAX_QUERY_PAGES = 100;
const MAX_ENTITY_RECORDS = 10_000;

export interface RunRecoveryStore {
  save(value: BuildPlanLaunchRecovery): void;
  load(): BuildPlanLaunchRecovery | undefined;
  clear(): void;
}

export interface BuildPlanLaunchRecovery {
  readonly version: 1;
  readonly installationId: string;
  readonly actionId: "build_plan";
  readonly idempotencyKey: string;
  readonly input: BuildPlanInput;
  readonly runRef?: string;
}

export interface ChangeConsumer {
  (batch: AppEntityChangeBatch): void | Promise<void>;
}

export interface ChangeSubscription {
  readonly signal: AbortSignal;
  stop(): void;
  readonly done: Promise<void>;
}

export class ResearchPlannerClient {
  readonly state = new NormalizedEntityState();
  readonly #sdk: MagicianAppsClient;
  readonly #installationId: string;
  readonly #surfaceRevision: number;

  constructor(options: {
    readonly origin: string | URL;
    readonly installationId: string;
    readonly surfaceRevision: number;
    readonly fetch?: typeof globalThis.fetch;
  }) {
    this.#sdk = new MagicianAppsClient({
      origin: options.origin,
      ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
    });
    if (options.installationId.length === 0 || !Number.isSafeInteger(options.surfaceRevision) || options.surfaceRevision < 1) {
      throw new TypeError("installationId and surfaceRevision must identify one current installation generation");
    }
    this.#installationId = options.installationId;
    this.#surfaceRevision = options.surfaceRevision;
  }

  connect(options: AppRequestOptions = {}) {
    return this.#sdk.connect(options);
  }

  async refreshEntity(entity: keyof typeof QUERY_FIELDS, options: AppRequestOptions = {}): Promise<readonly AppRecordProjection[]> {
    const records = await this.#queryEntity(entity, options);
    this.state.replaceEntity(entity, records);
    return records;
  }

  async refreshAll(options: AppRequestOptions = {}): Promise<void> {
    const pages = new Map<keyof typeof QUERY_FIELDS, readonly AppRecordProjection[]>();
    for (const entity of Object.keys(QUERY_FIELDS) as (keyof typeof QUERY_FIELDS)[]) {
      pages.set(entity, await this.#queryEntity(entity, options));
    }
    for (const [entity, records] of pages) this.state.replaceEntity(entity, records);
  }

  async #queryEntity(entity: keyof typeof QUERY_FIELDS, options: AppRequestOptions): Promise<readonly AppRecordProjection[]> {
    const records: AppRecordProjection[] = [];
    const seenRecords = new Set<string>();
    const seenCursors = new Set<string>();
    let cursor: string | undefined;
    let identity: string | undefined;
    for (let pageIndex = 0; pageIndex < MAX_QUERY_PAGES; pageIndex += 1) {
      const request = {
        protocol_version: APP_PROTOCOL_VERSION,
        source_installation_id: this.#installationId,
        entity,
        select: QUERY_FIELDS[entity],
        order: [{ field: QUERY_FIELDS[entity][0], direction: "ascending" }] as const,
        limit: 100,
        purpose: "surface_refresh",
        ...(cursor === undefined ? {} : { cursor }),
      };
      const page: AppQueryPage = await this.#sdk.queryData(this.#installationId, request, options);
      const pageIdentity = JSON.stringify({
        result_schema_ref: page.result_schema_ref,
        installation_id: page.envelope.installation_id,
        package_revision_ref: page.envelope.package_revision_ref,
        schema_revision: page.envelope.schema_revision,
        grant_revision: page.envelope.grant_revision,
        value_schema_ref: page.envelope.value_schema_ref,
        handling_labels: page.envelope.handling_labels,
      });
      identity ??= pageIdentity;
      if (pageIdentity !== identity) throw new Error("paged query changed reviewed schema or authority identity");
      for (const record of page.envelope.value) {
        if (record.entity !== entity || !isResearchProjection(record)) {
          throw new Error(`query returned a manifest-shape-invalid ${entity} projection`);
        }
        const recordKey = `${record.entity}\u0000${record.record_id}`;
        if (seenRecords.has(recordKey)) throw new Error("paged query repeated a record");
        seenRecords.add(recordKey);
        records.push(record);
        if (records.length > MAX_ENTITY_RECORDS) throw new Error("entity refresh exceeds its bounded record ceiling");
      }
      if (page.next_cursor === undefined) return records;
      if (seenCursors.has(page.next_cursor)) throw new Error("paged query repeated a cursor");
      seenCursors.add(page.next_cursor);
      cursor = page.next_cursor;
    }
    throw new Error("entity refresh exceeds its bounded page ceiling");
  }

  async createTopic(
    input: ResearchTopicInput,
    options: {
      readonly idempotencyKey: string;
      readonly temporaryId: string;
      readonly expectedSchemaRevision: number;
      readonly signal?: AbortSignal;
      readonly deadlineMs?: number;
    },
  ): Promise<AppMutationReceipt> {
    assertResearchTopicInput(input);
    const fields = input as unknown as Readonly<Record<string, JsonValue>>;
    this.state.stageCreate("research_topic", options.temporaryId, fields, options.idempotencyKey);
    const command: AppMutationCommand = {
      protocol_version: APP_PROTOCOL_VERSION,
      idempotency_key: options.idempotencyKey,
      atomicity: "all_or_nothing",
      expected_schema_revision: options.expectedSchemaRevision,
      operations: [{
        kind: "create",
        entity: "research_topic",
        temporary_id: options.temporaryId,
        payload: input as unknown as JsonValue,
      }],
    };
    try {
      const receipt = await this.#sdk.mutateData(this.#installationId, command, {
        ...(options.signal === undefined ? {} : { signal: options.signal }),
        ...(options.deadlineMs === undefined ? {} : { deadlineMs: options.deadlineMs }),
      });
      this.state.commitCreate(options.idempotencyKey, receipt);
      return receipt;
    } catch (error) {
      const retryIdenticalUncertain = error instanceof MagicianAppsError
        && error.kind === "outcome_uncertain"
        && error.retryDisposition === "retry_identical_input";
      if (!retryIdenticalUncertain) {
        this.state.rollback(options.idempotencyKey);
      }
      throw error;
    }
  }

  async launchBuildPlan(
    input: BuildPlanInput,
    options: {
      readonly idempotencyKey: string;
      readonly recovery: RunRecoveryStore;
      readonly signal?: AbortSignal;
      readonly deadlineMs?: number;
    },
  ): Promise<AppActionLaunchResponse<BuildPlanOutput>> {
    validateActionFormInput(BUILD_PLAN_FORM, input);
    const existing = options.recovery.load();
    const pending: BuildPlanLaunchRecovery = existing ?? {
      version: 1,
      installationId: this.#installationId,
      actionId: "build_plan",
      idempotencyKey: options.idempotencyKey,
      input,
    };
    assertRecoveryMatches(pending, this.#installationId, options.idempotencyKey, input);
    if (existing === undefined) options.recovery.save(pending);
    const launched = await this.#sdk.launchAction(this.#installationId, "build_plan", {
      idempotency_key: pending.idempotencyKey,
      input: pending.input,
    }, {
      outputValidator: isBuildPlanOutput,
      ...(options.signal === undefined ? {} : { signal: options.signal }),
      ...(options.deadlineMs === undefined ? {} : { deadlineMs: options.deadlineMs }),
    });
    assertBuildPlanLaunchIdentity(launched, this.#installationId);
    options.recovery.save({ ...pending, runRef: launched.run_handle.run_ref });
    return launched;
  }

  async recoverBuildPlan(
    recovery: RunRecoveryStore,
    options: AppRequestOptions & { readonly pollIntervalMs?: number } = {},
  ): Promise<AppRunSnapshot<BuildPlanOutput> | undefined> {
    const pending = recovery.load();
    if (pending === undefined) return undefined;
    assertRecoveryMatches(pending, this.#installationId, pending.idempotencyKey, pending.input);
    let runRef = pending.runRef;
    if (runRef === undefined) {
      const launched = await this.#sdk.launchAction(this.#installationId, "build_plan", {
        idempotency_key: pending.idempotencyKey,
        input: pending.input,
      }, {
        outputValidator: isBuildPlanOutput,
        ...(options.signal === undefined ? {} : { signal: options.signal }),
        ...(options.deadlineMs === undefined ? {} : { deadlineMs: options.deadlineMs }),
      });
      assertBuildPlanLaunchIdentity(launched, this.#installationId);
      runRef = launched.run_handle.run_ref;
      recovery.save({ ...pending, runRef });
    }
    const snapshot = await this.#sdk.waitForRun(runRef, {
      outputValidator: isBuildPlanOutput,
      ...(options.signal === undefined ? {} : { signal: options.signal }),
      ...(options.deadlineMs === undefined ? {} : { deadlineMs: options.deadlineMs }),
      ...(options.pollIntervalMs === undefined ? {} : { pollIntervalMs: options.pollIntervalMs }),
    });
    assertBuildPlanSnapshotIdentity(snapshot, this.#installationId, runRef);
    if (snapshot.terminal && !snapshot.result_withheld) recovery.clear();
    return snapshot;
  }

  async composeBuildPlanInto(
    sourceRunRef: string,
    options: {
      readonly destinationInstallationId: string;
      readonly destinationActionId: string;
      readonly idempotencyKey: string;
      readonly chain?: readonly {
        readonly destinationInstallationId: string;
        readonly destinationActionId: string;
        readonly idempotencyKey: string;
        readonly mapping: readonly AppValueMappingOperation[];
      }[];
      readonly subscriptionCursor?: string;
      readonly subscriptionLimit?: number;
      readonly signal?: AbortSignal;
      readonly deadlineMs?: number;
    },
  ): Promise<AppActionResultComposition<JsonValue>> {
    const chain: readonly AppActionCompositionHop[] = (options.chain ?? []).map((hop) => ({
      destination_installation_id: hop.destinationInstallationId,
      destination_action_id: hop.destinationActionId,
      idempotency_key: hop.idempotencyKey,
      mapping: hop.mapping,
    }));
    return this.#sdk.composeActionRun(sourceRunRef, {
      destination_installation_id: options.destinationInstallationId,
      destination_action_id: options.destinationActionId,
      mapping: [{ kind: "select", source: "plan_id", target: "source_plan_id" }],
      idempotency_key: options.idempotencyKey,
      ...(chain.length === 0 ? {} : { chain }),
      ...(options.subscriptionCursor === undefined && options.subscriptionLimit === undefined
        ? {}
        : {
            subscription: {
              ...(options.subscriptionCursor === undefined ? {} : { cursor: options.subscriptionCursor }),
              ...(options.subscriptionLimit === undefined ? {} : { limit: options.subscriptionLimit }),
            },
          }),
    }, {
      ...(options.signal === undefined ? {} : { signal: options.signal }),
      ...(options.deadlineMs === undefined ? {} : { deadlineMs: options.deadlineMs }),
    });
  }

  subscribeChanges(options: {
    readonly afterChangeSequence?: number;
    readonly pollIntervalMs?: number;
    readonly pageLimit?: number;
    readonly onBatch?: ChangeConsumer;
    readonly signal?: AbortSignal;
  } = {}): ChangeSubscription {
    assertPollInterval(options.pollIntervalMs ?? 1000);
    const local = new AbortController();
    const abort = (): void => local.abort(options.signal?.reason);
    if (options.signal?.aborted === true) abort();
    else options.signal?.addEventListener("abort", abort, { once: true });
    const done = this.#runChangeLoop({
      signal: local.signal,
      after: options.afterChangeSequence ?? 0,
      pollIntervalMs: options.pollIntervalMs ?? 1000,
      pageLimit: options.pageLimit ?? 64,
      ...(options.onBatch === undefined ? {} : { onBatch: options.onBatch }),
    }).finally(() => options.signal?.removeEventListener("abort", abort));
    return { signal: local.signal, stop: () => local.abort(), done };
  }

  async #runChangeLoop(options: {
    readonly signal: AbortSignal;
    readonly after: number;
    readonly pollIntervalMs: number;
    readonly pageLimit: number;
    readonly onBatch?: ChangeConsumer;
  }): Promise<void> {
    let after = options.after;
    for await (const batch of this.#sdk.iterateEntityChanges(this.#installationId, {
      surfaceRevision: this.#surfaceRevision,
      afterChangeSequence: after,
      limit: options.pageLimit,
      maxPages: 200,
      signal: options.signal,
    })) {
      after = await this.#consumeBatch(batch, after, options.onBatch, options.signal);
    }
    while (!options.signal.aborted) {
      const batch = await this.#sdk.readEntityChanges(this.#installationId, {
        surfaceRevision: this.#surfaceRevision,
        afterChangeSequence: after,
        limit: options.pageLimit,
        signal: options.signal,
      });
      after = await this.#consumeBatch(batch, after, options.onBatch, options.signal);
      await abortableDelay(options.pollIntervalMs, options.signal);
    }
  }

  async #consumeBatch(
    batch: AppEntityChangeBatch,
    expectedAfter: number,
    consumer: ChangeConsumer | undefined,
    signal: AbortSignal,
  ): Promise<number> {
    assertContiguousChangeBatch(batch, expectedAfter);
    if (batch.reset_required) {
      await this.refreshAll({ signal });
    } else if (batch.changes.length > 0) {
      const entities = [...new Set(batch.changes.map((change) => change.entity))];
      for (const entity of entities) {
        if (entity in QUERY_FIELDS) await this.refreshEntity(entity as keyof typeof QUERY_FIELDS, { signal });
      }
    }
    await consumer?.(batch);
    return batch.through_change_sequence;
  }
}

export function assertBuildPlanLaunchIdentity(
  launch: AppActionLaunchResponse<BuildPlanOutput>,
  installationId: string,
): void {
  if (launch.run_handle.installation_id !== installationId
    || launch.run_handle.action_id !== "build_plan"
    || launch.result !== undefined
      && (launch.result.run_ref !== launch.run_handle.run_ref || launch.result.action_id !== "build_plan")) {
    throw new Error("build-plan launch belongs to another installation or action");
  }
}

export function assertBuildPlanSnapshotIdentity(
  snapshot: AppRunSnapshot<BuildPlanOutput>,
  installationId: string,
  runRef: string,
): void {
  if (snapshot.run_handle.installation_id !== installationId
    || snapshot.run_handle.action_id !== "build_plan"
    || snapshot.run_handle.run_ref !== runRef
    || snapshot.result !== undefined
      && (snapshot.result.run_ref !== runRef || snapshot.result.action_id !== "build_plan")) {
    throw new Error("recovered build-plan run belongs to another installation or action");
  }
}

export function assertContiguousChangeBatch(batch: AppEntityChangeBatch, expectedAfter: number): void {
  if (batch.after_change_sequence !== expectedAfter) throw new Error("entity change stream lost cursor correlation");
  if (batch.reset_required) return;
  let expected = expectedAfter + 1;
  for (const change of batch.changes) {
    if (change.change_sequence !== expected) throw new Error("entity change stream contains a sequence gap");
    expected += 1;
  }
  const expectedThrough = batch.changes.length === 0 ? expectedAfter : expected - 1;
  if (batch.through_change_sequence !== expectedThrough) {
    throw new Error("entity change stream through cursor does not match its events");
  }
}

function assertRecoveryMatches(
  pending: BuildPlanLaunchRecovery,
  installationId: string,
  idempotencyKey: string,
  input: BuildPlanInput,
): void {
  validateActionFormInput(BUILD_PLAN_FORM, pending.input);
  const expectedKeys = pending.runRef === undefined
    ? ["actionId", "idempotencyKey", "input", "installationId", "version"]
    : ["actionId", "idempotencyKey", "input", "installationId", "runRef", "version"];
  const observedKeys = Object.keys(pending).sort();
  if (observedKeys.length !== expectedKeys.length
    || observedKeys.some((key, index) => key !== [...expectedKeys].sort()[index])
    || pending.version !== 1 || pending.installationId !== installationId
    || pending.actionId !== "build_plan" || pending.idempotencyKey !== idempotencyKey
    || pending.input.topic_id !== input.topic_id || pending.input.query !== input.query
    || pending.input.start_date !== input.start_date || pending.input.end_date !== input.end_date
    || pending.runRef !== undefined && !pending.runRef.startsWith("run:app-action:")) {
    throw new Error("retained build-plan launch intent was substituted");
  }
}

function abortableDelay(milliseconds: number, signal: AbortSignal): Promise<void> {
  assertPollInterval(milliseconds);
  return new Promise((resolve, reject) => {
    const finish = (): void => {
      signal.removeEventListener("abort", cancel);
      resolve();
    };
    const timer = setTimeout(finish, milliseconds);
    const cancel = (): void => {
      clearTimeout(timer);
      signal.removeEventListener("abort", cancel);
      reject(signal.reason ?? new Error("change subscription stopped"));
    };
    if (signal.aborted) cancel();
    else signal.addEventListener("abort", cancel, { once: true });
  });
}

function assertPollInterval(milliseconds: number): void {
  if (!Number.isSafeInteger(milliseconds) || milliseconds < 50 || milliseconds > 60_000) {
    throw new TypeError("pollIntervalMs must be between 50 and 60000");
  }
}
