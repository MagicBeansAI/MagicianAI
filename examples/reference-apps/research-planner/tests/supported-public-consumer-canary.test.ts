import assert from "node:assert/strict";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import test from "node:test";

import { blake3 } from "@noble/hashes/blake3";
import { bytesToHex } from "@noble/hashes/utils";
import {
  APP_CONTRACT_CAPABILITIES_SCHEMA_VERSION,
  APP_DATA_PLANE_PROTOCOL_VERSION,
  APP_JSON_SCHEMA_DIALECT,
  APP_SUPPORTED_MANIFEST_FEATURES,
  APP_SUPPORTED_MANIFEST_SCHEMA_VERSIONS,
  APP_SUPPORTED_PUBLIC_CONTRACT_VERSION,
  APP_SUPPORTED_PUBLIC_DEPRECATIONS,
  APP_SUPPORTED_PUBLIC_OPERATIONS,
  MagicianAppsClient,
  type AppDirectActionRequest,
  type AppMutationCommand,
  type AppQueryRequest,
  type JsonValue,
} from "@magician/apps";
import { ResearchPlannerClient } from "../src/client.js";

const INSTALLATION_ID = "install_consumer_canary";
const ACTION_ID = "build_plan";
const COMPOSED_ACTION_ID = "accept_plan";
const DESTINATION_INSTALLATION_ID = "install_consumer_destination_a";
const SECOND_DESTINATION_INSTALLATION_ID = "install_consumer_destination_b";
const THIRD_DESTINATION_INSTALLATION_ID = "install_consumer_destination_c";
const RUN_REF = "run:app-action:consumer_canary";
const SECOND_DESTINATION_RUN_REF = "run:app-action:consumer_composition_b";
const DESTINATION_RUN_REF = "run:app-action:consumer_composition_c";
const COMPOSITION_CURSOR = "composition-cursor:consumer-canary:1";
const DIGEST_ONE = `blake3:${"1".repeat(64)}`;
const DIGEST_TWO = `blake3:${"2".repeat(64)}`;
const DIGEST_THREE = `blake3:${"3".repeat(64)}`;

interface ObservedRequest {
  readonly method: string;
  readonly path: string;
  readonly query: string;
  readonly body: unknown;
}

function capabilities() {
  const operations = structuredClone(APP_SUPPORTED_PUBLIC_OPERATIONS);
  const inventoryDigest = `blake3:${bytesToHex(blake3(new TextEncoder().encode(JSON.stringify(operations))))}`;
  return {
    schema_version: APP_CONTRACT_CAPABILITIES_SCHEMA_VERSION,
    contract_version: APP_SUPPORTED_PUBLIC_CONTRACT_VERSION,
    supported_protocol_versions: [APP_DATA_PLANE_PROTOCOL_VERSION],
    supported_manifest_schema_versions: [...APP_SUPPORTED_MANIFEST_SCHEMA_VERSIONS],
    supported_manifest_features: [...APP_SUPPORTED_MANIFEST_FEATURES],
    json_schema_dialect: APP_JSON_SCHEMA_DIALECT,
    limits: {
      max_document_bytes: 1_048_576,
      max_json_depth: 32,
      max_json_nodes: 20_000,
      max_value_bytes: 262_144,
      max_value_nodes: 8_000,
      max_collection_items: 256,
      max_predicate_nodes: 128,
      max_predicate_depth: 16,
      max_page_rows: 200,
      max_entity_change_page_rows: 128,
    },
    sdk_compatibility: {
      policy: "current_contract_only",
      supported_contract_versions: [APP_SUPPORTED_PUBLIC_CONTRACT_VERSION],
      generated_by_is_authority: false,
    },
    deprecations: structuredClone(APP_SUPPORTED_PUBLIC_DEPRECATIONS),
    operation_inventory_digest: inventoryDigest,
    operations,
  };
}

async function requestBody(request: IncomingMessage): Promise<unknown> {
  const chunks: Buffer[] = [];
  for await (const chunk of request) chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  if (chunks.length === 0) return undefined;
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function sendJson(response: ServerResponse, status: number, value: unknown): void {
  const body = JSON.stringify(value);
  response.writeHead(status, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(body),
    "cache-control": "private, no-store",
  });
  response.end(body);
}

function envelope(source: "app_store" | "app_action", value: JsonValue) {
  return {
    protocol_version: "1",
    source,
    scope_binding_ref: "scope_consumer_canary",
    installation_id: INSTALLATION_ID,
    package_revision_ref: "package:consumer-canary:1",
    schema_revision: 1,
    grant_revision: 1,
    value_schema_ref: source === "app_store" ? "schema:research_topic:v1" : "schema:build_plan:v1",
    value,
    handling_labels: {
      classification: "ordinary",
      model_processing: "none",
      policy_digest: DIGEST_ONE,
      provenance_digest: DIGEST_TWO,
    },
    content_digest: DIGEST_THREE,
    produced_at: "2026-08-23T12:00:00Z",
  };
}

test("a packed external TypeScript consumer exercises exactly the supported-public eight", async (context) => {
  const observed: ObservedRequest[] = [];
  let runReads = 0;
  let compositionReads = 0;
  const server = createServer((request, response) => {
    void (async () => {
      const requestUrl = new URL(request.url ?? "/", "http://127.0.0.1");
      const body = await requestBody(request);
      observed.push({
        method: request.method ?? "",
        path: decodeURIComponent(requestUrl.pathname),
        query: requestUrl.search,
        body,
      });

      if (request.method === "GET" && requestUrl.pathname === "/api/magician/v2/apps/contract-capabilities") {
        sendJson(response, 200, capabilities());
        return;
      }
      if (request.method === "POST" && requestUrl.pathname === `/api/magician/v2/apps/installations/${INSTALLATION_ID}/data/query`) {
        const query = body as AppQueryRequest;
        assert.equal(query.source_installation_id, INSTALLATION_ID);
        assert.equal(query.entity, "research_topic");
        sendJson(response, 200, {
          envelope: envelope("app_store", [{
            entity: "research_topic",
            record_id: "topic_canary",
            record_revision: 1,
            fields: { title: "Canary topic" },
          }]),
          result_schema_ref: "schema:research_topic:v1",
        });
        return;
      }
      if (request.method === "POST" && requestUrl.pathname === `/api/magician/v2/apps/installations/${INSTALLATION_ID}/data/mutations`) {
        const mutation = body as AppMutationCommand;
        sendJson(response, 200, {
          receipt_id: "receipt:consumer-canary",
          installation_id: INSTALLATION_ID,
          origin: {
            kind: "owner_api",
            session_ref: "session:consumer-canary",
            request_ref: mutation.idempotency_key,
          },
          mutation_key: DIGEST_ONE,
          batch_digest: DIGEST_TWO,
          committed_record_revisions: [{ entity: "research_topic", record_id: "topic_created", revision: 1 }],
          change_seq_range: { first: 1, last: 1 },
          committed_at: "2026-08-23T12:00:01Z",
        });
        return;
      }
      if (request.method === "POST" && requestUrl.pathname === `/api/magician/v2/apps/installations/${INSTALLATION_ID}/actions/${ACTION_ID}/runs`) {
        const action = body as AppDirectActionRequest;
        assert.equal(action.idempotency_key, "action:consumer-canary");
        sendJson(response, 202, {
          run_handle: {
            protocol_version: "1",
            run_ref: RUN_REF,
            installation_id: INSTALLATION_ID,
            action_id: ACTION_ID,
          },
          execution_id: "execution_consumer_canary",
        });
        return;
      }
      if (request.method === "GET" && decodeURIComponent(requestUrl.pathname) === `/api/magician/v2/apps/action-runs/${RUN_REF}`) {
        runReads += 1;
        const handle = {
          protocol_version: "1",
          run_ref: RUN_REF,
          installation_id: INSTALLATION_ID,
          action_id: ACTION_ID,
        };
        if (runReads === 1) {
          sendJson(response, 202, {
            protocol_version: "1",
            run_handle: handle,
            execution_id: "execution_consumer_canary",
            status: "running",
            terminal: false,
            result_withheld: false,
          });
        } else {
          sendJson(response, 200, {
            protocol_version: "1",
            run_handle: handle,
            execution_id: "execution_consumer_canary",
            status: "completed",
            terminal: true,
            result_withheld: false,
            result: {
              protocol_version: "1",
              action_id: ACTION_ID,
              run_ref: RUN_REF,
              status: "completed",
              output: envelope("app_action", { plan_id: "plan_canary", status: "draft" }),
            },
          });
        }
        return;
      }
      if (request.method === "POST" && decodeURIComponent(requestUrl.pathname) === `/api/magician/v2/apps/action-runs/${RUN_REF}/cancel`) {
        assert.deepEqual(body, {
          expected_generation: 0,
          idempotency_key: "cancel:consumer-canary",
        });
        sendJson(response, 202, {
          protocol_version: "1",
          run_ref: RUN_REF,
          generation: 1,
          idempotency_key: "cancel:consumer-canary",
          status: "cancelling",
          requested_at: "2026-08-23T12:00:02Z",
        });
        return;
      }
      if (request.method === "POST" && decodeURIComponent(requestUrl.pathname) === `/api/magician/v2/apps/action-runs/${RUN_REF}/compositions`) {
        compositionReads += 1;
        assert.deepEqual(body, {
          destination_installation_id: DESTINATION_INSTALLATION_ID,
          destination_action_id: COMPOSED_ACTION_ID,
          mapping: [{ kind: "select", source: "plan_id", target: "source_plan_id" }],
          idempotency_key: "composition:consumer-canary",
          chain: [
            {
              destination_installation_id: SECOND_DESTINATION_INSTALLATION_ID,
              destination_action_id: COMPOSED_ACTION_ID,
              mapping: [{ kind: "select", source: "forward_plan_id", target: "source_plan_id" }],
              idempotency_key: "composition:consumer-canary:b",
            },
            {
              destination_installation_id: THIRD_DESTINATION_INSTALLATION_ID,
              destination_action_id: COMPOSED_ACTION_ID,
              mapping: [{ kind: "select", source: "forward_plan_id", target: "source_plan_id" }],
              idempotency_key: "composition:consumer-canary:c",
            },
          ],
          subscription: compositionReads === 1
            ? { limit: 8 }
            : { cursor: COMPOSITION_CURSOR, limit: 8 },
        });
        sendJson(response, 202, {
          status: "launched",
          source_run: {
            protocol_version: "1",
            run_ref: SECOND_DESTINATION_RUN_REF,
            installation_id: SECOND_DESTINATION_INSTALLATION_ID,
            action_id: COMPOSED_ACTION_ID,
          },
          chain: {
            origin_source_run_ref: RUN_REF,
            active_source_run_ref: SECOND_DESTINATION_RUN_REF,
            active_destination_installation_id: THIRD_DESTINATION_INSTALLATION_ID,
            active_destination_action_id: COMPOSED_ACTION_ID,
            hop_index: 2,
            hop_count: 3,
          },
          launch: {
            run_handle: {
              protocol_version: "1",
              run_ref: DESTINATION_RUN_REF,
              installation_id: THIRD_DESTINATION_INSTALLATION_ID,
              action_id: COMPOSED_ACTION_ID,
            },
          },
          result_withheld_by_policy: true,
          subscription: {
            after_sequence: compositionReads === 1 ? 0 : 1,
            through_sequence: 1,
            current_sequence: 1,
            updates: compositionReads === 1 ? [{
              sequence: 1,
              source_run_ref: SECOND_DESTINATION_RUN_REF,
              destination_installation_id: THIRD_DESTINATION_INSTALLATION_ID,
              destination_action_id: COMPOSED_ACTION_ID,
              status: "launched",
              destination_run_ref: DESTINATION_RUN_REF,
              observed_at: "2026-08-23T12:00:03Z",
            }] : [],
            has_more: false,
            reset_required: false,
            next_cursor: COMPOSITION_CURSOR,
            expires_at: "2026-08-23T12:10:03Z",
          },
        });
        return;
      }
      if (request.method === "GET" && requestUrl.pathname === `/api/magician/v2/apps/installations/${INSTALLATION_ID}/entity-changes`) {
        assert.equal(requestUrl.searchParams.get("surface_revision"), "1");
        assert.equal(requestUrl.searchParams.get("after_change_sequence"), "0");
        sendJson(response, 200, {
          installation_id: INSTALLATION_ID,
          surface_revision: 1,
          after_change_sequence: 0,
          through_change_sequence: 1,
          current_change_sequence: 1,
          changes: [{
            entity: "research_topic",
            record_id: "topic_created",
            record_revision: 1,
            change_sequence: 1,
          }],
          has_more: false,
          reset_required: false,
        });
        return;
      }
      sendJson(response, 404, { error: "not_found", message: "canary route is outside the supported set" });
    })().catch((error: unknown) => {
      response.destroy(error instanceof Error ? error : new Error(String(error)));
    });
  });
  server.listen(0, "127.0.0.1");
  await new Promise<void>((resolve, reject) => {
    server.once("listening", resolve);
    server.once("error", reject);
  });
  context.after(() => new Promise<void>((resolve, reject) => {
    server.close((error) => error === undefined ? resolve() : reject(error));
  }));
  const address = server.address();
  assert.notEqual(address, null);
  assert.equal(typeof address, "object");
  if (address === null || typeof address === "string") throw new Error("canary server has no TCP address");

  const client = new MagicianAppsClient({ origin: `http://127.0.0.1:${address.port}` });
  const negotiated = await client.connect();
  assert.equal(negotiated.operations.length, 8);

  const query = await client.queryData(INSTALLATION_ID, {
    protocol_version: "1",
    source_installation_id: INSTALLATION_ID,
    entity: "research_topic",
    select: ["title"],
    limit: 10,
    purpose: "surface_refresh",
  });
  assert.equal(query.envelope.value[0]?.record_id, "topic_canary");

  const mutation = await client.mutateData(INSTALLATION_ID, {
    protocol_version: "1",
    idempotency_key: "mutation:consumer-canary",
    atomicity: "all_or_nothing",
    expected_schema_revision: 1,
    operations: [{
      kind: "create",
      entity: "research_topic",
      temporary_id: "topic_temporary",
      payload: {
        title: "Canary topic",
        query: "canary",
        status: "draft",
        priority: 1,
        updated_at: "2026-08-23T12:00:00Z",
      },
    }],
  });
  assert.equal(mutation.committed_record_revisions[0]?.record_id, "topic_created");

  const outputValidator = (value: JsonValue): value is { readonly plan_id: string; readonly status: "draft" | "approved" } => (
    typeof value === "object"
    && value !== null
    && !Array.isArray(value)
    && Object.keys(value).length === 2
    && typeof value.plan_id === "string"
    && (value.status === "draft" || value.status === "approved")
  );
  const launched = await client.launchAction(INSTALLATION_ID, ACTION_ID, {
    idempotency_key: "action:consumer-canary",
    input: {
      topic_id: "topic_created",
      query: "canary",
      start_date: "2026-08-01",
      end_date: "2026-08-23",
    },
  }, { outputValidator });
  assert.equal(launched.run_handle.run_ref, RUN_REF);
  const running = await client.getActionRun(RUN_REF, { outputValidator });
  assert.equal(running.terminal, false);
  const cancellation = await client.cancelActionRun(RUN_REF, {
    expected_generation: 0,
    idempotency_key: "cancel:consumer-canary",
  });
  assert.equal(cancellation.status, "cancelling");
  const referenceClient = new ResearchPlannerClient({
    origin: `http://127.0.0.1:${address.port}`,
    installationId: INSTALLATION_ID,
    surfaceRevision: 1,
  });
  const composition = await referenceClient.composeBuildPlanInto(RUN_REF, {
    destinationInstallationId: DESTINATION_INSTALLATION_ID,
    destinationActionId: COMPOSED_ACTION_ID,
    idempotencyKey: "composition:consumer-canary",
    chain: [
      {
        destinationInstallationId: SECOND_DESTINATION_INSTALLATION_ID,
        destinationActionId: COMPOSED_ACTION_ID,
        mapping: [{ kind: "select", source: "forward_plan_id", target: "source_plan_id" }],
        idempotencyKey: "composition:consumer-canary:b",
      },
      {
        destinationInstallationId: THIRD_DESTINATION_INSTALLATION_ID,
        destinationActionId: COMPOSED_ACTION_ID,
        mapping: [{ kind: "select", source: "forward_plan_id", target: "source_plan_id" }],
        idempotencyKey: "composition:consumer-canary:c",
      },
    ],
    subscriptionLimit: 8,
  });
  assert.equal(composition.status, "launched");
  assert.equal(composition.launch.run_handle.run_ref, DESTINATION_RUN_REF);
  assert.equal(composition.result_withheld_by_policy, true);
  assert.equal(composition.chain.hop_index, 2);
  assert.equal(composition.subscription?.updates[0]?.destination_run_ref, DESTINATION_RUN_REF);
  const subscriptionCursor = composition.subscription?.next_cursor;
  assert.ok(subscriptionCursor);
  const resumedComposition = await referenceClient.composeBuildPlanInto(RUN_REF, {
    destinationInstallationId: DESTINATION_INSTALLATION_ID,
    destinationActionId: COMPOSED_ACTION_ID,
    idempotencyKey: "composition:consumer-canary",
    chain: [
      {
        destinationInstallationId: SECOND_DESTINATION_INSTALLATION_ID,
        destinationActionId: COMPOSED_ACTION_ID,
        mapping: [{ kind: "select", source: "forward_plan_id", target: "source_plan_id" }],
        idempotencyKey: "composition:consumer-canary:b",
      },
      {
        destinationInstallationId: THIRD_DESTINATION_INSTALLATION_ID,
        destinationActionId: COMPOSED_ACTION_ID,
        mapping: [{ kind: "select", source: "forward_plan_id", target: "source_plan_id" }],
        idempotencyKey: "composition:consumer-canary:c",
      },
    ],
    subscriptionCursor,
    subscriptionLimit: 8,
  });
  assert.equal(resumedComposition.subscription?.after_sequence, 1);
  assert.deepEqual(resumedComposition.subscription?.updates, []);
  const terminal = await client.waitForRun(RUN_REF, {
    outputValidator,
    pollIntervalMs: 1,
    deadlineMs: 1_000,
  });
  assert.equal(terminal.result?.output?.value.plan_id, "plan_canary");

  const changes = await client.readEntityChanges(INSTALLATION_ID, {
    surfaceRevision: 1,
    afterChangeSequence: 0,
    limit: 64,
  });
  assert.equal(changes.changes[0]?.record_id, "topic_created");

  assert.deepEqual(observed.map(({ method, path }) => ({ method, path })), [
    { method: "GET", path: "/api/magician/v2/apps/contract-capabilities" },
    { method: "POST", path: `/api/magician/v2/apps/installations/${INSTALLATION_ID}/data/query` },
    { method: "POST", path: `/api/magician/v2/apps/installations/${INSTALLATION_ID}/data/mutations` },
    { method: "POST", path: `/api/magician/v2/apps/installations/${INSTALLATION_ID}/actions/${ACTION_ID}/runs` },
    { method: "GET", path: `/api/magician/v2/apps/action-runs/${RUN_REF}` },
    { method: "POST", path: `/api/magician/v2/apps/action-runs/${RUN_REF}/cancel` },
    { method: "GET", path: "/api/magician/v2/apps/contract-capabilities" },
    { method: "POST", path: `/api/magician/v2/apps/action-runs/${RUN_REF}/compositions` },
    { method: "POST", path: `/api/magician/v2/apps/action-runs/${RUN_REF}/compositions` },
    { method: "GET", path: `/api/magician/v2/apps/action-runs/${RUN_REF}` },
    { method: "GET", path: `/api/magician/v2/apps/installations/${INSTALLATION_ID}/entity-changes` },
  ]);
  const serialized = JSON.stringify(observed);
  for (const forbidden of ["/launch", "custom-surface", "task_id", "provider", "mcp"]) {
    assert.equal(serialized.includes(forbidden), false, forbidden);
  }
});
