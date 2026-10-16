import assert from "node:assert/strict";
import test from "node:test";
import { blake3 } from "@noble/hashes/blake3";
import { bytesToHex } from "@noble/hashes/utils";
import {
  APP_SUPPORTED_PUBLIC_OPERATIONS,
  APP_SUPPORTED_PUBLIC_DEPRECATIONS,
  MagicianAppsClient,
  MagicianAppsError,
} from "../dist/index.js";

const encoder = new TextEncoder();

function jsonResponse(value, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function capabilities(overrides = {}) {
  const operations = structuredClone(APP_SUPPORTED_PUBLIC_OPERATIONS);
  const digest = `blake3:${bytesToHex(blake3(encoder.encode(JSON.stringify(operations))))}`;
  return {
    schema_version: 1,
    contract_version: "1.5.0",
    supported_protocol_versions: ["1"],
    supported_manifest_schema_versions: ["1.0"],
    supported_manifest_features: [
      "typed_entities_v1",
      "declarative_views_v1",
      "governed_actions_v1",
      "immutable_dependencies_v1",
      "owner_data_plane_v1",
      "durable_action_runs_v1",
      "contribution_ports_v1",
      "attention_lanes_v1",
      "llm_operations_v1",
      "custom_surfaces_v1",
      "app_widgets_v1",
      "app_behaviors_v1",
      "app_event_behaviors_v1",
      "app_owner_notifications_v1",
    ],
    json_schema_dialect: "https://json-schema.org/draft/2020-12/schema",
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
      supported_contract_versions: ["1.5.0"],
      generated_by_is_authority: false,
    },
    deprecations: structuredClone(APP_SUPPORTED_PUBLIC_DEPRECATIONS),
    operation_inventory_digest: digest,
    operations,
    ...overrides,
  };
}

function mutationReceipt(origin) {
  return {
    receipt_id: "receipt:fixture",
    installation_id: "install_fixture",
    origin,
    mutation_key: `blake3:${"1".repeat(64)}`,
    batch_digest: `blake3:${"2".repeat(64)}`,
    committed_record_revisions: [{ entity: "item", record_id: "item_1", revision: 2 }],
    change_seq_range: { first: 3, last: 3 },
    committed_at: "2026-08-22T00:00:00Z",
  };
}

function entityChangeBatch(overrides = {}) {
  return {
    installation_id: "install_fixture",
    surface_revision: 1,
    after_change_sequence: 0,
    through_change_sequence: 1,
    current_change_sequence: 1,
    changes: [{ entity: "item", record_id: "item_1", record_revision: 1, change_sequence: 1 }],
    has_more: false,
    reset_required: false,
    ...overrides,
  };
}

function queryPage(records = []) {
  return {
    envelope: {
      protocol_version: "1",
      source: "app_store",
      scope_binding_ref: "scope_fixture",
      installation_id: "install_fixture",
      package_revision_ref: "package:fixture:1",
      schema_revision: 1,
      grant_revision: 1,
      value_schema_ref: "schema:fixture",
      value: records,
      handling_labels: {
        classification: "ordinary",
        model_processing: "none",
        policy_digest: `blake3:${"1".repeat(64)}`,
        provenance_digest: `blake3:${"2".repeat(64)}`,
      },
      content_digest: `blake3:${"3".repeat(64)}`,
      produced_at: "2026-08-22T00:00:00Z",
    },
    result_schema_ref: "schema:fixture",
  };
}

test("negotiates exact inventory before a fixed query route", async () => {
  const requests = [];
  const fetch = async (input, init) => {
    requests.push({ url: input.toString(), init });
    if (requests.length === 1) return jsonResponse(capabilities());
    return jsonResponse({
      envelope: {
        protocol_version: "1",
        source: "app_store",
        scope_binding_ref: "scope_fixture",
        installation_id: "install_fixture",
        package_revision_ref: "package:fixture:1",
        schema_revision: 1,
        grant_revision: 1,
        value_schema_ref: "schema:fixture",
        value: [],
        handling_labels: {
          classification: "ordinary",
          model_processing: "none",
          policy_digest: `blake3:${"1".repeat(64)}`,
          provenance_digest: `blake3:${"2".repeat(64)}`,
        },
        content_digest: `blake3:${"3".repeat(64)}`,
        produced_at: "2026-08-22T00:00:00Z",
      },
      result_schema_ref: "schema:fixture",
    });
  };
  const client = new MagicianAppsClient({ origin: "http://127.0.0.1:3002", fetch });
  await client.queryData("install_fixture", {
    protocol_version: "1",
    source_installation_id: "install_fixture",
    entity: "item",
    select: ["title"],
    limit: 16,
    purpose: "surface_hydration",
  });
  assert.equal(requests.length, 2);
  assert.equal(requests[0].url, "http://127.0.0.1:3002/api/magician/v2/apps/contract-capabilities");
  assert.equal(requests[1].url, "http://127.0.0.1:3002/api/magician/v2/apps/installations/install_fixture/data/query");
  assert.equal(requests[1].init.method, "POST");
  assert.equal(requests[1].init.credentials, "same-origin");
  assert.equal(requests[1].init.redirect, "error");
  assert.equal(requests[1].init.mode, "same-origin");
  assert.equal(client.request, undefined);
});

test("query results cannot substitute the requested entity or projected fields", async () => {
  for (const record of [
    { entity: "other", record_id: "item_1", record_revision: 1, fields: { title: "ok" } },
    { entity: "item", record_id: "item_1", record_revision: 1, fields: { secret: "not selected" } },
  ]) {
    let calls = 0;
    const client = new MagicianAppsClient({
      origin: "https://magician.example.test",
      fetch: async () => {
        calls += 1;
        return calls === 1 ? jsonResponse(capabilities()) : jsonResponse(queryPage([record]));
      },
    });
    await assert.rejects(client.queryData("install_fixture", {
      protocol_version: "1",
      source_installation_id: "install_fixture",
      entity: "item",
      select: ["title"],
      relation_expansions: [{ relation: "owner", select: ["name"], max_depth: 1, max_rows: 1 }],
      limit: 16,
      purpose: "surface_hydration",
    }), (error) => {
      assert.ok(error instanceof MagicianAppsError);
      assert.equal(error.kind, "decode");
      return true;
    });
  }
});

test("refuses operation substitution before a non-capability request", async () => {
  const forged = capabilities();
  forged.operations[1].path = "/internal/raw-query";
  forged.operation_inventory_digest = `blake3:${bytesToHex(blake3(encoder.encode(JSON.stringify(forged.operations))))}`;
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      return jsonResponse(forged);
    },
  });
  await assert.rejects(client.connect(), (error) => {
    assert(error instanceof MagicianAppsError);
    assert.equal(error.kind, "contract_mismatch");
    return true;
  });
  assert.equal(calls, 1);
});

test("refuses missing, substituted, and additive capability metadata", async () => {
  for (const mutate of [
    (value) => { delete value.deprecations; },
    (value) => { value.deprecations[0].field = "internal.task_id"; },
    (value) => { value.sdk_compatibility.future = true; },
    (value) => { value.limits.future = 1; },
    (value) => { value.future = true; },
  ]) {
    const value = capabilities();
    mutate(value);
    const client = new MagicianAppsClient({
      origin: "https://magician.example.test",
      fetch: async () => jsonResponse(value),
    });
    await assert.rejects(client.connect(), (error) => {
      assert.ok(error instanceof MagicianAppsError);
      assert.equal(error.kind, "contract_mismatch");
      return true;
    });
  }
});

test("accepts the 1.5.0 handshake with custom_surfaces_v1 and rejects older servers", async () => {
  let calls = 0;
  const current = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      return jsonResponse(capabilities());
    },
  });
  const accepted = await current.connect();
  assert.equal(accepted.contract_version, "1.5.0");
  assert.ok(accepted.supported_manifest_features.includes("custom_surfaces_v1"));
  assert.equal(calls, 1);
  for (const stale of [
    { contract_version: "1.3.0" },
    {
      sdk_compatibility: {
        ...capabilities().sdk_compatibility,
        supported_contract_versions: ["1.3.0"],
      },
    },
    {
      supported_manifest_features: capabilities().supported_manifest_features
        .filter((feature) => feature !== "custom_surfaces_v1"),
    },
  ]) {
    const client = new MagicianAppsClient({
      origin: "https://magician.example.test",
      fetch: async () => jsonResponse(capabilities(stale)),
    });
    await assert.rejects(client.connect(), (error) => {
      assert.ok(error instanceof MagicianAppsError);
      assert.equal(error.kind, "contract_mismatch");
      return true;
    });
  }
});

test("advertised limits never widen local traversal and page ceilings", async () => {
  const value = capabilities();
  for (const key of Object.keys(value.limits)) value.limits[key] = Number.MAX_SAFE_INTEGER;
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      return jsonResponse(value);
    },
  });
  await assert.rejects(
    client.queryData("install_fixture", {
      protocol_version: "1",
      source_installation_id: "install_fixture",
      entity: "item",
      select: ["title"],
      limit: 201,
      purpose: "surface_hydration",
    }),
    (error) => {
      assert.ok(error instanceof MagicianAppsError);
      assert.equal(error.kind, "limit_exceeded");
      return true;
    },
  );
  assert.equal(calls, 1);
});

test("advertised limits cannot widen local collection, predicate, or value ceilings", async () => {
  for (const request of [
    {
      protocol_version: "1",
      source_installation_id: "install_fixture",
      entity: "item",
      select: Array.from({ length: 257 }, (_, index) => `field_${index}`),
      limit: 1,
      purpose: "surface_hydration",
    },
    {
      protocol_version: "1",
      source_installation_id: "install_fixture",
      entity: "item",
      select: ["title"],
      predicate: {
        root: 0,
        nodes: Array.from({ length: 129 }, () => ({ kind: "is_null", field: "title" })),
      },
      limit: 1,
      purpose: "surface_hydration",
    },
  ]) {
    const value = capabilities();
    for (const key of Object.keys(value.limits)) value.limits[key] = Number.MAX_SAFE_INTEGER;
    let calls = 0;
    const client = new MagicianAppsClient({
      origin: "https://magician.example.test",
      fetch: async () => {
        calls += 1;
        return jsonResponse(value);
      },
    });
    await assert.rejects(client.queryData("install_fixture", request), (error) => {
      assert.ok(error instanceof MagicianAppsError);
      assert.equal(error.kind, "limit_exceeded");
      return true;
    });
    assert.equal(calls, 1);
  }
});

test("entity-change iteration rejects more than 200 pages before fetching", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      return jsonResponse(capabilities());
    },
  });
  const iterator = client.iterateEntityChanges("install_fixture", {
    surfaceRevision: 1,
    maxPages: 201,
  });
  await assert.rejects(iterator.next(), TypeError);
  assert.equal(calls, 0);
});

test("oversized escaped request input is rejected before a non-capability fetch", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      return jsonResponse(capabilities());
    },
  });
  await assert.rejects(client.mutateData("install_fixture", {
    protocol_version: "1",
    idempotency_key: "mutation:fixture",
    atomicity: "all_or_nothing",
    expected_schema_revision: 1,
    operations: [{
      kind: "create",
      entity: "item",
      temporary_id: "tmp",
      payload: { title: "\u0000".repeat(200_000) },
    }],
  }), (error) => {
    assert.ok(error instanceof MagicianAppsError);
    assert.equal(error.kind, "limit_exceeded");
    return true;
  });
  assert.equal(calls, 1);
});

test("one concurrent caller cannot cancel another caller's initial handshake", async () => {
  const controller = new AbortController();
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async (_input, init) => {
      calls += 1;
      if (calls === 2) return jsonResponse(capabilities());
      return await new Promise((_resolve, reject) => {
        init.signal.addEventListener("abort", () => reject(init.signal.reason), { once: true });
      });
    },
  });
  const cancelled = client.connect({ signal: controller.signal });
  const independent = client.connect();
  controller.abort(new Error("caller A stopped"));
  await assert.rejects(cancelled, (error) => {
    assert.ok(error instanceof MagicianAppsError);
    assert.equal(error.kind, "cancelled");
    return true;
  });
  assert.equal((await independent).contract_version, "1.5.0");
  assert.equal(calls, 2);
});

test("one concurrent handshake deadline cannot shorten another caller's deadline", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async (_input, init) => {
      calls += 1;
      if (calls === 2) return jsonResponse(capabilities());
      return await new Promise((_resolve, reject) => {
        init.signal.addEventListener("abort", () => reject(init.signal.reason), { once: true });
      });
    },
  });
  // Keep the deadlines far enough apart that host timer quantization cannot
  // turn this isolation assertion into a scheduler-speed test.
  const short = client.connect({ deadlineMs: 25 });
  const long = client.connect({ deadlineMs: 5_000 });
  await assert.rejects(short, (error) => {
    assert.ok(error instanceof MagicianAppsError);
    assert.equal(error.kind, "timeout");
    return true;
  });
  assert.equal((await long).contract_version, "1.5.0");
  assert.equal(calls, 2);
});

test("one end-to-end deadline includes capability negotiation", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      await new Promise((resolve) => setTimeout(resolve, 5));
      return jsonResponse(capabilities());
    },
  });
  await assert.rejects(client.queryData("install_fixture", {
    protocol_version: "1",
    source_installation_id: "install_fixture",
    entity: "item",
    select: ["title"],
    limit: 1,
    purpose: "surface_hydration",
  }, { deadlineMs: 1 }), (error) => {
    assert.ok(error instanceof MagicianAppsError);
    assert.equal(error.kind, "timeout");
    return true;
  });
  assert.equal(calls, 1);
});

test("absolute operation expiry is re-sampled after request encoding before dispatch", async () => {
  const originalNow = Date.now;
  let clockSamples = 0;
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      return jsonResponse(capabilities());
    },
  });
  await client.connect();
  Date.now = () => {
    clockSamples += 1;
    return clockSamples >= 4 ? 20 : 0;
  };
  try {
    await assert.rejects(client.queryData("install_fixture", {
      protocol_version: "1",
      source_installation_id: "install_fixture",
      entity: "item",
      select: ["title"],
      limit: 1,
      purpose: "surface_hydration",
    }, { deadlineMs: 10 }), (error) => {
      assert.ok(error instanceof MagicianAppsError);
      assert.equal(error.kind, "timeout");
      return true;
    });
    assert.equal(calls, 1, "expired encoded request must not be dispatched");
  } finally {
    Date.now = originalNow;
  }
});

test("post-validation expiry of a dispatched keyed operation is outcome uncertain", async () => {
  const originalNow = Date.now;
  let clockSamples = 0;
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      return calls === 1
        ? jsonResponse(capabilities())
        : jsonResponse(mutationReceipt({
            kind: "owner_api",
            session_ref: "session:fixture",
            request_ref: "mutation:fixture",
          }));
    },
  });
  await client.connect();
  Date.now = () => {
    clockSamples += 1;
    return clockSamples >= 6 ? 20 : 0;
  };
  try {
    await assert.rejects(client.mutateData("install_fixture", {
      protocol_version: "1",
      idempotency_key: "mutation:fixture",
      atomicity: "all_or_nothing",
      expected_schema_revision: 1,
      operations: [{
        kind: "update",
        entity: "item",
        record_id: "item_1",
        patch: { title: "updated" },
      }],
      expected_record_revisions: [{ entity: "item", record_id: "item_1", revision: 1 }],
    }, { deadlineMs: 10 }), (error) => {
      assert.ok(error instanceof MagicianAppsError);
      assert.equal(error.kind, "outcome_uncertain");
      assert.equal(error.retryDisposition, "retry_identical_input");
      return true;
    });
    assert.equal(calls, 2);
  } finally {
    Date.now = originalNow;
  }
});

test("streams under the negotiated response byte ceiling", async () => {
  const tiny = capabilities({ limits: { ...capabilities().limits, max_document_bytes: 256 } });
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      if (calls === 1) return jsonResponse(tiny);
      return jsonResponse({ installation_id: "install_fixture", padding: "x".repeat(512) });
    },
  });
  await assert.rejects(client.mutateData("install_fixture", {
    protocol_version: "1",
    idempotency_key: "mutation:fixture",
    atomicity: "all_or_nothing",
    expected_schema_revision: 1,
    operations: [{ kind: "create", entity: "item", temporary_id: "tmp", payload: {} }],
  }), (error) => {
    assert(error instanceof MagicianAppsError);
    assert.equal(error.kind, "outcome_uncertain");
    return true;
  });
});

test("rejects a one-field nested run-handle substitution", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      if (calls === 1) return jsonResponse(capabilities());
      return jsonResponse({
        protocol_version: "1",
        run_handle: {
          protocol_version: "2",
          run_ref: "run:app-action:fixture",
          installation_id: "install_fixture",
          action_id: "refresh",
        },
        status: "running",
        terminal: false,
        result_withheld: false,
      }, 202);
    },
  });
  await assert.rejects(client.getActionRun("run:app-action:fixture"), (error) => {
    assert(error instanceof MagicianAppsError);
    assert.equal(error.kind, "decode");
    return true;
  });
});

test("composes one opaque source run into an exact destination action", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async (input, init) => {
      calls += 1;
      if (calls === 1) return jsonResponse(capabilities());
      assert.equal(new URL(String(input)).pathname, "/api/magician/v2/apps/action-runs/run%3Aapp-action%3Asource/compositions");
      assert.deepEqual(JSON.parse(new TextDecoder().decode(init.body)), {
        destination_installation_id: "install_destination",
        destination_action_id: "consume_plan",
        mapping: [{ kind: "select", source: "plan_id", target: "source_plan_id" }],
        idempotency_key: "composition:fixture",
      });
      return jsonResponse({
        status: "launched",
        source_run: {
          protocol_version: "1",
          run_ref: "run:app-action:source",
          installation_id: "install_source",
          action_id: "build_plan",
        },
        chain: {
          origin_source_run_ref: "run:app-action:source",
          active_source_run_ref: "run:app-action:source",
          active_destination_installation_id: "install_destination",
          active_destination_action_id: "consume_plan",
          hop_index: 0,
          hop_count: 1,
        },
        launch: {
          run_handle: {
            protocol_version: "1",
            run_ref: "run:app-action:destination",
            installation_id: "install_destination",
            action_id: "consume_plan",
          },
        },
        result_withheld_by_policy: true,
      }, 202);
    },
  });
  const result = await client.composeActionRun("run:app-action:source", {
    destination_installation_id: "install_destination",
    destination_action_id: "consume_plan",
    mapping: [{ kind: "select", source: "plan_id", target: "source_plan_id" }],
    idempotency_key: "composition:fixture",
  });
  assert.equal(result.status, "launched");
  assert.equal(result.launch.run_handle.run_ref, "run:app-action:destination");
  assert.equal(result.result_withheld_by_policy, true);
});

test("composition response substitution is outcome uncertain and retry-identical", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      if (calls === 1) return jsonResponse(capabilities());
      return jsonResponse({
        status: "waiting",
        source_run: {
          protocol_version: "1",
          run_ref: "run:app-action:substituted",
          installation_id: "install_source",
          action_id: "build_plan",
        },
        chain: {
          origin_source_run_ref: "run:app-action:source",
          active_source_run_ref: "run:app-action:substituted",
          active_destination_installation_id: "install_destination",
          active_destination_action_id: "consume_plan",
          hop_index: 0,
          hop_count: 1,
        },
      }, 202);
    },
  });
  await assert.rejects(client.composeActionRun("run:app-action:source", {
    destination_installation_id: "install_destination",
    destination_action_id: "consume_plan",
    mapping: [{ kind: "select", source: "plan_id", target: "source_plan_id" }],
    idempotency_key: "composition:fixture",
  }), (error) => {
    assert.ok(error instanceof MagicianAppsError);
    assert.equal(error.kind, "outcome_uncertain");
    assert.equal(error.operationId, "compose_action_run");
    assert.equal(error.retryDisposition, "retry_identical_input");
    return true;
  });
});

test("bounded composition chains validate active-hop and subscription correlation", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async (_input, init) => {
      calls += 1;
      if (calls === 1) return jsonResponse(capabilities());
      const body = JSON.parse(new TextDecoder().decode(init.body));
      assert.equal(body.chain.length, 1);
      assert.equal(body.subscription.cursor, "composition-cursor:4:9999999999999:fixture");
      return jsonResponse({
        status: "waiting",
        source_run: {
          protocol_version: "1",
          run_ref: "run:app-action:intermediate",
          installation_id: "install_middle",
          action_id: "normalize_plan",
        },
        chain: {
          origin_source_run_ref: "run:app-action:source",
          active_source_run_ref: "run:app-action:intermediate",
          active_destination_installation_id: "install_final",
          active_destination_action_id: "publish_plan",
          hop_index: 1,
          hop_count: 2,
        },
        subscription: {
          after_sequence: 4,
          through_sequence: 5,
          current_sequence: 5,
          updates: [{
            sequence: 5,
            source_run_ref: "run:app-action:intermediate",
            destination_installation_id: "install_final",
            destination_action_id: "publish_plan",
            status: "waiting",
            observed_at: "2026-08-23T12:00:00Z",
          }],
          has_more: false,
          reset_required: false,
          next_cursor: "composition-cursor:5:9999999999999:fixture",
          expires_at: "2286-11-20T17:46:39Z",
        },
      }, 202);
    },
  });
  const result = await client.composeActionRun("run:app-action:source", {
    destination_installation_id: "install_middle",
    destination_action_id: "normalize_plan",
    mapping: [{ kind: "select", source: "plan_id", target: "source_plan_id" }],
    idempotency_key: "composition:first",
    chain: [{
      destination_installation_id: "install_final",
      destination_action_id: "publish_plan",
      mapping: [{ kind: "select", source: "normalized_plan_id", target: "source_plan_id" }],
      idempotency_key: "composition:second",
    }],
    subscription: { cursor: "composition-cursor:4:9999999999999:fixture", limit: 8 },
  });
  assert.equal(result.chain.hop_index, 1);
  assert.equal(result.subscription.current_sequence, 5);
});

test("composition chain cycles are rejected before operation dispatch", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      if (calls === 1) return jsonResponse(capabilities());
      throw new Error("composition operation must not dispatch");
    },
  });
  await assert.rejects(client.composeActionRun("run:app-action:source", {
    destination_installation_id: "install_middle",
    destination_action_id: "normalize_plan",
    mapping: [{ kind: "select", source: "plan_id", target: "source_plan_id" }],
    idempotency_key: "composition:first",
    chain: [{
      destination_installation_id: "install_middle",
      destination_action_id: "publish_plan",
      mapping: [{ kind: "select", source: "normalized_plan_id", target: "source_plan_id" }],
      idempotency_key: "composition:second",
    }],
  }), (error) => error instanceof MagicianAppsError && error.kind === "contract_mismatch");
  assert.equal(calls, 1);
});

test("composition subscription cursor-response substitution is outcome uncertain", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      if (calls === 1) return jsonResponse(capabilities());
      return jsonResponse({
        status: "waiting",
        source_run: {
          protocol_version: "1",
          run_ref: "run:app-action:source",
          installation_id: "install_source",
          action_id: "build_plan",
        },
        chain: {
          origin_source_run_ref: "run:app-action:source",
          active_source_run_ref: "run:app-action:source",
          active_destination_installation_id: "install_destination",
          active_destination_action_id: "consume_plan",
          hop_index: 0,
          hop_count: 1,
        },
        subscription: {
          after_sequence: 0,
          through_sequence: 1,
          current_sequence: 1,
          updates: [{
            sequence: 1,
            source_run_ref: "run:app-action:another",
            destination_installation_id: "install_destination",
            destination_action_id: "consume_plan",
            status: "waiting",
            observed_at: "2026-08-23T12:00:00Z",
          }],
          has_more: false,
          reset_required: false,
          next_cursor: "composition-cursor:1:9999999999999:fixture",
          expires_at: "2286-11-20T17:46:39Z",
        },
      }, 202);
    },
  });
  await assert.rejects(client.composeActionRun("run:app-action:source", {
    destination_installation_id: "install_destination",
    destination_action_id: "consume_plan",
    mapping: [{ kind: "select", source: "plan_id", target: "source_plan_id" }],
    idempotency_key: "composition:fixture",
    subscription: { limit: 8 },
  }), (error) => error instanceof MagicianAppsError && error.kind === "outcome_uncertain");
});

test("caller-selected output typing requires and executes a bounded validator", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      if (calls === 1) return jsonResponse(capabilities());
      return jsonResponse({
        protocol_version: "1",
        run_handle: {
          protocol_version: "1",
          run_ref: "run:app-action:fixture",
          installation_id: "install_fixture",
          action_id: "refresh",
        },
        status: "completed",
        terminal: true,
        result_withheld: false,
        result: {
          protocol_version: "1",
          action_id: "refresh",
          run_ref: "run:app-action:fixture",
          status: "completed",
          output: {
            protocol_version: "1",
            source: "app_action",
            scope_binding_ref: "scope_fixture",
            installation_id: "install_fixture",
            package_revision_ref: "package:fixture:1",
            schema_revision: 1,
            grant_revision: 1,
            value_schema_ref: "schema:refresh:v1",
            value: { kind: 7 },
            handling_labels: {
              classification: "ordinary",
              model_processing: "none",
              policy_digest: `blake3:${"1".repeat(64)}`,
              provenance_digest: `blake3:${"2".repeat(64)}`,
            },
            content_digest: `blake3:${"3".repeat(64)}`,
            produced_at: "2026-08-22T00:00:00Z",
          },
        },
      });
    },
  });
  await assert.rejects(client.getActionRun("run:app-action:fixture", {
    outputValidator: (value) => typeof value === "object"
      && value !== null
      && !Array.isArray(value)
      && value.kind === "expected",
  }), (error) => {
    assert(error instanceof MagicianAppsError);
    assert.equal(error.kind, "decode");
    return true;
  });
});

test("rejects a cross-installation action output envelope", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      if (calls === 1) return jsonResponse(capabilities());
      return jsonResponse({
        protocol_version: "1",
        run_handle: {
          protocol_version: "1",
          run_ref: "run:app-action:fixture",
          installation_id: "install_fixture",
          action_id: "refresh",
        },
        status: "completed",
        terminal: true,
        result_withheld: false,
        result: {
          protocol_version: "1",
          action_id: "refresh",
          run_ref: "run:app-action:fixture",
          status: "completed",
          output: {
            protocol_version: "1",
            source: "app_action",
            scope_binding_ref: "scope_fixture",
            installation_id: "install_substituted",
            package_revision_ref: "package:fixture:1",
            schema_revision: 1,
            grant_revision: 1,
            value_schema_ref: "schema:refresh:v1",
            value: { kind: "expected" },
            handling_labels: {
              classification: "ordinary",
              model_processing: "none",
              policy_digest: `blake3:${"1".repeat(64)}`,
              provenance_digest: `blake3:${"2".repeat(64)}`,
            },
            content_digest: `blake3:${"3".repeat(64)}`,
            produced_at: "2026-08-22T00:00:00Z",
          },
        },
      });
    },
  });
  await assert.rejects(client.getActionRun("run:app-action:fixture"), (error) => {
    assert.ok(error instanceof MagicianAppsError);
    assert.equal(error.kind, "decode");
    return true;
  });
});

test("mutation receipt must be the exact owner request origin", async () => {
  for (const origin of [
    { kind: "owner_api", session_ref: "session:fixture", request_ref: "mutation:substituted" },
    { kind: "workflow", execution_id: "execution:fixture", output_revision: 1 },
  ]) {
    let calls = 0;
    const client = new MagicianAppsClient({
      origin: "https://magician.example.test",
      fetch: async () => {
        calls += 1;
        return calls === 1 ? jsonResponse(capabilities()) : jsonResponse(mutationReceipt(origin));
      },
    });
    await assert.rejects(client.mutateData("install_fixture", {
      protocol_version: "1",
      idempotency_key: "mutation:fixture",
      atomicity: "all_or_nothing",
      expected_schema_revision: 1,
      operations: [{
        kind: "update",
        entity: "item",
        record_id: "item_1",
        patch: { title: "updated" },
      }],
      expected_record_revisions: [{ entity: "item", record_id: "item_1", revision: 1 }],
    }), (error) => {
      assert.ok(error instanceof MagicianAppsError);
      assert.equal(error.kind, "outcome_uncertain");
      assert.equal(error.retryDisposition, "retry_identical_input");
      return true;
    });
  }
});

test("mutation receipt range must exactly cover every committed revision", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      if (calls === 1) return jsonResponse(capabilities());
      const receipt = mutationReceipt({ kind: "owner_api", session_ref: "session:fixture", request_ref: "mutation:fixture" });
      receipt.change_seq_range = { first: 3, last: 4 };
      return jsonResponse(receipt);
    },
  });
  await assert.rejects(client.mutateData("install_fixture", {
    protocol_version: "1",
    idempotency_key: "mutation:fixture",
    atomicity: "all_or_nothing",
    expected_schema_revision: 1,
    operations: [{ kind: "create", entity: "item", temporary_id: "tmp", payload: {} }],
  }), (error) => {
    assert.ok(error instanceof MagicianAppsError);
    assert.equal(error.kind, "outcome_uncertain");
    assert.equal(error.retryDisposition, "retry_identical_input");
    return true;
  });
});

test("client-keyed transport failure reports retry-identical outcome uncertainty", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      if (calls === 1) return jsonResponse(capabilities());
      throw new TypeError("connection reset after request write");
    },
  });
  await assert.rejects(client.mutateData("install_fixture", {
    protocol_version: "1",
    idempotency_key: "mutation:fixture",
    atomicity: "all_or_nothing",
    expected_schema_revision: 1,
    operations: [{ kind: "create", entity: "item", temporary_id: "tmp", payload: {} }],
  }), (error) => {
    assert.ok(error instanceof MagicianAppsError);
    assert.deepEqual(error.toJSON(), {
      kind: "outcome_uncertain",
      operation_id: "mutate_data",
      message: "The server may have accepted this client-keyed request; retry only with byte-identical input and the same idempotency key",
      retry_disposition: "retry_identical_input",
    });
    return true;
  });
});

test("a pre-dispatch cancellation remains proven safe for a cached handshake", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      return jsonResponse(capabilities());
    },
  });
  await client.connect();
  const controller = new AbortController();
  controller.abort(new Error("stopped before dispatch"));
  await assert.rejects(client.mutateData("install_fixture", {
    protocol_version: "1",
    idempotency_key: "mutation:fixture",
    atomicity: "all_or_nothing",
    expected_schema_revision: 1,
    operations: [{ kind: "create", entity: "item", temporary_id: "tmp", payload: {} }],
  }, { signal: controller.signal }), (error) => {
    assert.ok(error instanceof MagicianAppsError);
    assert.equal(error.kind, "cancelled");
    return true;
  });
  assert.equal(calls, 1);
});

test("unsafe response integers are rejected after parse", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      if (calls === 1) return jsonResponse(capabilities());
      return jsonResponse(queryPage([{
        entity: "item",
        record_id: "item_1",
        record_revision: Number.MAX_SAFE_INTEGER + 1,
        fields: { title: "unsafe" },
      }]));
    },
  });
  await assert.rejects(client.queryData("install_fixture", {
    protocol_version: "1",
    source_installation_id: "install_fixture",
    entity: "item",
    select: ["title"],
    limit: 1,
    purpose: "surface_hydration",
  }), (error) => {
    assert.ok(error instanceof MagicianAppsError);
    assert.equal(error.kind, "decode");
    return true;
  });
});

test("unsafe request integers are rejected before a client-keyed dispatch", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      return jsonResponse(capabilities());
    },
  });
  await assert.rejects(client.mutateData("install_fixture", {
    protocol_version: "1",
    idempotency_key: "mutation:fixture",
    atomicity: "all_or_nothing",
    expected_schema_revision: 1,
    operations: [{
      kind: "create",
      entity: "item",
      temporary_id: "tmp",
      payload: { count: Number.MAX_SAFE_INTEGER + 1 },
    }],
  }), (error) => {
    assert.ok(error instanceof MagicianAppsError);
    assert.equal(error.kind, "decode");
    return true;
  });
  assert.equal(calls, 1);
});

test("entity-change batches cannot omit sequences or substitute head flags", async () => {
  for (const batch of [
    entityChangeBatch({
      through_change_sequence: 2,
      current_change_sequence: 2,
      changes: [{ entity: "item", record_id: "item_2", record_revision: 1, change_sequence: 2 }],
    }),
    entityChangeBatch({ through_change_sequence: 2, current_change_sequence: 2 }),
    entityChangeBatch({ current_change_sequence: 2, has_more: false }),
    entityChangeBatch({ reset_required: true, has_more: false }),
  ]) {
    let calls = 0;
    const client = new MagicianAppsClient({
      origin: "https://magician.example.test",
      fetch: async () => {
        calls += 1;
        return calls === 1 ? jsonResponse(capabilities()) : jsonResponse(batch);
      },
    });
    await assert.rejects(client.readEntityChanges("install_fixture", {
      surfaceRevision: 1,
      afterChangeSequence: 0,
    }), (error) => {
      assert.ok(error instanceof MagicianAppsError);
      assert.equal(error.kind, "decode");
      return true;
    });
  }
});

test("parses canonical HTTP error envelopes and preserves the inventoried reason", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      if (calls === 1) return jsonResponse(capabilities());
      return jsonResponse({
        code: "conflict",
        disposition: "terminal",
        message: "Revision changed.",
        details: { operation_id: "mutate_data", reason: "app_data_conflict" },
      }, 409);
    },
  });
  await assert.rejects(client.mutateData("install_fixture", {
    protocol_version: "1",
    idempotency_key: "mutation:fixture",
    atomicity: "all_or_nothing",
    expected_schema_revision: 1,
    operations: [{ kind: "create", entity: "item", temporary_id: "tmp", payload: {} }],
  }), (error) => {
    assert(error instanceof MagicianAppsError);
    assert.equal(error.kind, "http");
    assert.equal(error.httpStatus, 409);
    assert.equal(error.errorCode, "conflict");
    assert.equal(error.errorDisposition, "terminal");
    assert.equal(error.reasonCode, "app_data_conflict");
    assert.deepEqual(error.errorDetails, { operation_id: "mutate_data", reason: "app_data_conflict" });
    assert.deepEqual(error.toJSON(), {
      kind: "http",
      operation_id: "mutate_data",
      message: "Supported-public operation was rejected",
      http_status: 409,
      error_code: "conflict",
      error_disposition: "terminal",
      error_details: { operation_id: "mutate_data", reason: "app_data_conflict" },
      reason_code: "app_data_conflict",
    });
    return true;
  });
});

test("uses canonical retry disposition and delay from a proved public rejection", async () => {
  let calls = 0;
  const client = new MagicianAppsClient({
    origin: "https://magician.example.test",
    fetch: async () => {
      calls += 1;
      if (calls === 1) return jsonResponse(capabilities());
      return jsonResponse({
        code: "rate_limited",
        disposition: "retry_same_input",
        message: "The bounded app data lane is busy.",
        details: { operation_id: "mutate_data", reason: "app_data_overloaded" },
        retry_after_ms: 2_000,
      }, 429);
    },
  });
  await assert.rejects(client.mutateData("install_fixture", {
    protocol_version: "1",
    idempotency_key: "mutation:fixture",
    atomicity: "all_or_nothing",
    expected_schema_revision: 1,
    operations: [{ kind: "create", entity: "item", temporary_id: "tmp", payload: {} }],
  }), (error) => {
    assert(error instanceof MagicianAppsError);
    assert.equal(error.kind, "http");
    assert.equal(error.errorCode, "rate_limited");
    assert.equal(error.errorDisposition, "retry_same_input");
    assert.equal(error.retryAfterMs, 2_000);
    assert.equal(error.retryDisposition, "retry_identical_input");
    return true;
  });
});

test("rejects malformed or cross-operation canonical HTTP error envelopes", async () => {
  for (const envelope of [
    {
      code: "conflict",
      disposition: "terminal",
      message: "Missing operation correlation.",
      details: { reason: "app_workflow_stale" },
    },
    {
      code: "external_outcome_uncertain",
      disposition: "terminal",
      message: "Inconsistent uncertainty.",
      details: { operation_id: "get_action_run", reason: "app_workflow_failed" },
    },
    {
      code: "conflict",
      disposition: "terminal",
      message: "Wrong operation.",
      details: { operation_id: "mutate_data", reason: "app_workflow_stale" },
    },
    {
      code: "conflict",
      disposition: "terminal",
      message: "Unlisted reason.",
      details: { operation_id: "get_action_run", reason: "app_data_conflict" },
    },
    {
      code: "conflict",
      disposition: "terminal",
      message: "Illegal retry delay.",
      details: { operation_id: "get_action_run", reason: "app_workflow_stale" },
      retry_after_ms: 1_000,
    },
    {
      code: "conflict",
      disposition: "terminal",
      message: "Unknown field.",
      details: { operation_id: "get_action_run", reason: "app_workflow_stale" },
      invented: true,
    },
  ]) {
    let calls = 0;
    const client = new MagicianAppsClient({
      origin: "https://magician.example.test",
      fetch: async () => {
        calls += 1;
        return calls === 1 ? jsonResponse(capabilities()) : jsonResponse(envelope, 409);
      },
    });
    await assert.rejects(client.getActionRun("run:app-action:fixture"), (error) => {
      assert(error instanceof MagicianAppsError);
      assert.equal(error.kind, "decode");
      return true;
    });
  }
});
