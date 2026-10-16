import assert from "node:assert/strict";
import test from "node:test";

import {
  APP_SUPPORTED_PUBLIC_OPERATIONS,
  MagicianAppsClient,
} from "@magician/apps";

const INSTALLATION_ID = "install_1";

test("a packed external consumer reaches the real authenticated Apps route and entity owners", async () => {
  const origin = process.env.MAGICIAN_P4_REAL_SERVER_ORIGIN;
  assert.ok(origin, "MAGICIAN_P4_REAL_SERVER_ORIGIN must name the real fixture server");

  const client = new MagicianAppsClient({ origin });
  const capabilities = await client.connect();
  assert.deepEqual(capabilities.operations, APP_SUPPORTED_PUBLIC_OPERATIONS);

  const initial = await client.queryData(INSTALLATION_ID, {
    protocol_version: "1",
    source_installation_id: INSTALLATION_ID,
    entity: "item",
    select: ["title", "status"],
    order: [{ field: "status", direction: "ascending" }],
    limit: 100,
    purpose: "real_server_canary",
  });
  assert.equal(initial.envelope.installation_id, INSTALLATION_ID);
  assert.equal(initial.envelope.source, "app_store");
  assert.ok(initial.envelope.value.some((record) => record.record_id === "record_a"));

  // The fixture seeds its initial entity heads directly through the canonical
  // test owner, so its preexisting rows intentionally have no replayable
  // outbox history. Adopt the authoritative head/reset cursor before testing
  // that a subsequent live mutation is present in the durable change stream.
  const baselineChanges = await client.readEntityChanges(INSTALLATION_ID, {
    surfaceRevision: 1,
    afterChangeSequence: 0,
    limit: 64,
  });
  assert.equal(baselineChanges.installation_id, INSTALLATION_ID);

  const mutation = await client.mutateData(INSTALLATION_ID, {
    protocol_version: "1",
    idempotency_key: "mutation:p4-real-server-canary",
    atomicity: "all_or_nothing",
    expected_schema_revision: 1,
    operations: [{
      kind: "create",
      entity: "item",
      temporary_id: "p4_real_server_canary",
      payload: { title: "Created by the packed SDK", status: "open" },
    }],
  });
  assert.equal(mutation.installation_id, INSTALLATION_ID);
  if (mutation.origin.kind !== "owner_api") {
    assert.fail(`real owner mutation returned ${mutation.origin.kind} provenance`);
  }
  assert.equal(mutation.origin.request_ref, "mutation:p4-real-server-canary");
  const committed = mutation.committed_record_revisions[0];
  assert.ok(committed, "the real mutation owner must commit one record");

  const projected = await client.queryData(INSTALLATION_ID, {
    protocol_version: "1",
    source_installation_id: INSTALLATION_ID,
    entity: "item",
    select: ["title", "status"],
    limit: 100,
    purpose: "real_server_canary_requery",
  });
  assert.ok(projected.envelope.value.some((record) => (
    record.record_id === committed.record_id
    && record.fields.title === "Created by the packed SDK"
  )));

  const changes = await client.readEntityChanges(INSTALLATION_ID, {
    surfaceRevision: 1,
    afterChangeSequence: baselineChanges.current_change_sequence,
    limit: 64,
  });
  assert.equal(changes.installation_id, INSTALLATION_ID);
  assert.ok(changes.changes.some((change) => (
    change.entity === "item" && change.record_id === committed.record_id
  )));
});
