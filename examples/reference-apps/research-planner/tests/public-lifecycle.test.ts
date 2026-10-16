import assert from "node:assert/strict";
import { chmod, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

const lifecycleScript = new URL(
  import.meta.url.includes("/dist/tests/")
    ? "../../scripts/public-lifecycle.mjs"
    : "../scripts/public-lifecycle.mjs",
  import.meta.url,
);

test("initial archive publication remains inert until exact review and approval", async (context) => {
  const fixture = await fakeMagician(context);
  const invocation = spawnSync(process.execPath, [
    lifecycleScript.pathname, "publish-reviewed",
    "--archive", "/fixtures/research-planner-0.1.1.app.zip",
    "--request-id", "reference:initial-1",
    "--principal", "owner",
    "--workspace", "default",
  ], {
    encoding: "utf8",
    env: { ...process.env, MAGICIAN_BIN: fixture.binary, FAKE_MAGICIAN_LOG: fixture.log },
  });
  assert.equal(invocation.status, 0, invocation.stderr);
  assert.deepEqual((await fixture.calls()).map((call) => call[2]), [
    "candidate-publish", "review", "approve",
  ]);
});

test("reviewed destructive update executes the exact public publication, plan, backup, review and approval owners", async (context) => {
  const fixture = await fakeMagician(context);
  const invocation = spawnSync(process.execPath, [
    lifecycleScript.pathname, "update-reviewed",
    "--archive", "/fixtures/research-planner-0.2.0.app.zip",
    "--installation-id", "install_research",
    "--expected-generation", "7",
    "--request-id", "reference:update-1",
    "--operations-file", "/fixtures/v0.1.1-to-v0.2.0.json",
    "--passphrase-file", "/fixtures/archive.passphrase",
    "--principal", "owner",
    "--workspace", "default",
  ], {
    encoding: "utf8",
    env: { ...process.env, MAGICIAN_BIN: fixture.binary, FAKE_MAGICIAN_LOG: fixture.log },
  });
  assert.equal(invocation.status, 0, invocation.stderr);
  const result = JSON.parse(invocation.stdout) as { readonly mode: string; readonly result: { readonly plan: { readonly destructive: boolean } } };
  assert.equal(result.mode, "update-reviewed");
  assert.equal(result.result.plan.destructive, true);

  const calls = await fixture.calls();
  assert.deepEqual(calls.map((call) => call[2]), [
    "update-begin", "candidate-publish", "update-plan", "update-backup", "review", "approve",
  ]);
  assert.deepEqual(calls[0], [
    "app", "--json", "update-begin", "install_research",
    "--expected-generation", "7", "--request-id", "reference:update-1:begin",
    "--principal", "owner", "--workspace", "default",
  ]);
  assert.deepEqual(calls[1], [
    "app", "--json", "candidate-publish", "/fixtures/research-planner-0.2.0.app.zip",
    "--request-id", "reference:update-1:candidate",
    "--installation-id", "install_research", "--attempt-kind", "update",
    "--principal", "owner", "--workspace", "default",
  ]);
  assert.ok(calls[2]?.includes("/fixtures/v0.1.1-to-v0.2.0.json"));
  assert.ok(calls[3]?.includes("/fixtures/archive.passphrase"));
  assert.ok(calls[5]?.includes("--confirm-destructive-migration"));
  assert.ok(calls[5]?.includes("migration:update-1"));
  assert.ok(calls[5]?.includes(`blake3:${"7".repeat(64)}`));
});

test("reviewed reinstall reuses retained lifecycle state without inventing update parking", async (context) => {
  const fixture = await fakeMagician(context);
  const invocation = spawnSync(process.execPath, [
    lifecycleScript.pathname, "reinstall-reviewed",
    "--archive", "/fixtures/research-planner-0.2.0.app.zip",
    "--installation-id", "install_research",
    "--expected-generation", "7",
    "--request-id", "reference:reinstall-1",
    "--operations-file", "/fixtures/v0.1.1-to-v0.2.0.json",
    "--passphrase-file", "/fixtures/archive.passphrase",
    "--principal", "owner",
    "--workspace", "default",
  ], {
    encoding: "utf8",
    env: { ...process.env, MAGICIAN_BIN: fixture.binary, FAKE_MAGICIAN_LOG: fixture.log },
  });
  assert.equal(invocation.status, 0, invocation.stderr);
  const calls = await fixture.calls();
  assert.deepEqual(calls.map((call) => call[2]), [
    "candidate-publish", "update-plan", "update-backup", "review", "approve",
  ]);
  assert.ok(calls[0]?.includes("reinstall"));
  assert.equal(calls.some((call) => call[2] === "update-begin"), false);
});

test("encrypted combined portability executes preview, approval and commit against the explicit destination", async (context) => {
  const fixture = await fakeMagician(context);
  const invocation = spawnSync(process.execPath, [
    lifecycleScript.pathname, "portability-roundtrip",
    "--source-installation-id", "install_source",
    "--destination-installation-id", "install_destination",
    "--kind", "combined",
    "--archive", "/fixtures/research-planner.appdata",
    "--passphrase-file", "/fixtures/archive.passphrase",
    "--request-id", "reference:portable-1",
    "--principal", "owner",
    "--workspace", "default",
  ], {
    encoding: "utf8",
    env: { ...process.env, MAGICIAN_BIN: fixture.binary, FAKE_MAGICIAN_LOG: fixture.log },
  });
  assert.equal(invocation.status, 0, invocation.stderr);
  const calls = await fixture.calls();
  assert.deepEqual(calls.map((call) => call[2]), [
    "data-export", "data-import-preview", "data-import-approve", "data-import-commit",
  ]);
  assert.deepEqual(calls[0]?.slice(2, 7), [
    "data-export", "install_source", "--kind", "combined", "--request-id",
  ]);
  assert.equal(calls[1]?.[3], "install_destination");
  assert.ok(calls[2]?.includes(`blake3:${"8".repeat(64)}`));
  assert.ok(calls[3]?.includes("approval:data-import:fixture"));
});

test("substituted review correlation stops before approval", async (context) => {
  const fixture = await fakeMagician(context, { substituteReviewAttempt: true });
  const invocation = spawnSync(process.execPath, [
    lifecycleScript.pathname, "update-reviewed",
    "--archive", "/fixtures/research-planner-0.2.0.app.zip",
    "--installation-id", "install_research",
    "--expected-generation", "7",
    "--request-id", "reference:update-hostile",
    "--operations-file", "/fixtures/v0.1.1-to-v0.2.0.json",
    "--passphrase-file", "/fixtures/archive.passphrase",
    "--principal", "owner",
    "--workspace", "default",
  ], {
    encoding: "utf8",
    env: { ...process.env, MAGICIAN_BIN: fixture.binary, FAKE_MAGICIAN_LOG: fixture.log },
  });
  assert.notEqual(invocation.status, 0);
  assert.match(invocation.stderr, /attempt_id did not match/);
  assert.deepEqual((await fixture.calls()).map((call) => call[2]), [
    "update-begin", "candidate-publish", "update-plan", "update-backup", "review",
  ]);
});

async function fakeMagician(
  context: { after(callback: () => void | Promise<void>): void },
  options: { readonly substituteReviewAttempt?: boolean } = {},
): Promise<{ readonly binary: string; readonly log: string; calls(): Promise<readonly (readonly string[])[]> }> {
  const root = await mkdtemp(join(tmpdir(), "magician-reference-lifecycle-"));
  const binary = join(root, "magician-fixture.mjs");
  const log = join(root, "calls.jsonl");
  await writeFile(binary, `#!/usr/bin/env node
import { appendFileSync } from "node:fs";
const args = process.argv.slice(2);
appendFileSync(process.env.FAKE_MAGICIAN_LOG, JSON.stringify(args) + "\\n");
const command = args[2];
const value = (name) => { const index = args.indexOf(name); return index < 0 ? undefined : args[index + 1]; };
const installation = command === "candidate-publish" && value("--installation-id") === undefined
  ? "install_initial"
  : value("--installation-id") ?? args[3] ?? "install_initial";
let result;
if (command === "update-begin") result = { installation_id: installation, generation: 8, status: "update_pending" };
else if (command === "candidate-publish") result = { installation_id: installation, attempt_id: "attempt:update-1", state: value("--attempt-kind") === "reinstall" ? "uninstalled_retained" : value("--attempt-kind") === "update" ? "update_pending" : "ready_for_review", activation_authority_granted: false };
else if (command === "update-plan") result = { installation_id: installation, attempt_id: value("--attempt-id"), migration_run_id: "migration:update-1", update_plan_digest: "blake3:${"7".repeat(64)}", dry_run_examined: 3, dry_run_representable: 3, destructive: true, backup_required: true };
else if (command === "update-backup") result = { migration_run_id: args[3], encrypted: true, update_plan_digest: "blake3:${"7".repeat(64)}" };
else if (command === "review") result = { installation_id: installation, attempt_id: ${options.substituteReviewAttempt === true ? '"attempt:substituted"' : '"attempt:update-1"'}, workflow_material_digest: "blake3:${"9".repeat(64)}" };
else if (command === "approve") result = { installation_id: installation, status: "enabled" };
else if (command === "data-export") result = { request_id: value("--request-id"), kind: value("--kind"), encrypted: true };
else if (command === "data-import-preview") result = { installation_id: installation, preview_digest: "blake3:${"8".repeat(64)}" };
else if (command === "data-import-approve") result = { installation_id: installation, approval_ref: "approval:data-import:fixture" };
else if (command === "data-import-commit") result = { installation_id: installation, receipt_id: "receipt:data-import:fixture" };
else throw new Error("unexpected fake command " + command);
process.stdout.write(JSON.stringify({ schema_version: 1, ok: true, result }) + "\\n");
`, { mode: 0o700 });
  await chmod(binary, 0o700);
  context.after(async () => {
    const { rm } = await import("node:fs/promises");
    await rm(root, { recursive: true, force: true });
  });
  return {
    binary,
    log,
    async calls() {
      const contents = await readFile(log, "utf8");
      return contents.trim().split("\n").filter(Boolean).map((line) => JSON.parse(line) as readonly string[]);
    },
  };
}
