import { spawnSync } from "node:child_process";

const MAX_OUTPUT_BYTES = 1_048_576;
const magicianBin = process.env.MAGICIAN_BIN ?? "magician";
const [mode, ...rawArgs] = process.argv.slice(2);
const options = parseOptions(rawArgs);

let result;
if (mode === "publish-reviewed") result = publishReviewed(options);
else if (mode === "update-reviewed") result = updateReviewed(options, "update");
else if (mode === "reinstall-reviewed") result = updateReviewed(options, "reinstall");
else if (mode === "portability-roundtrip") result = portabilityRoundTrip(options);
else fail("mode must be publish-reviewed, update-reviewed, reinstall-reviewed, or portability-roundtrip");

process.stdout.write(`${JSON.stringify({ schema_version: 1, mode, result })}\n`);

function publishReviewed(values) {
  requireOnly(values, ["api-base", "archive", "principal", "request-id", "workspace"]);
  const scope = liveScope(values);
  const publication = runApp([
    "candidate-publish", required(values, "archive"),
    "--request-id", required(values, "request-id"), ...scope,
  ]);
  const installationId = requiredResultString(publication, "installation_id");
  if (publication.state !== "ready_for_review" || publication.activation_authority_granted !== false) {
    fail("candidate publication did not return inert ready_for_review state");
  }
  const attemptId = requiredResultString(publication, "attempt_id");
  const review = runApp(["review", installationId, ...scope]);
  expectString(review, "installation_id", installationId);
  expectString(review, "attempt_id", attemptId);
  const reviewDigest = requiredResultString(review, "workflow_material_digest");
  const approval = runApp([
    "approve", installationId, "--review-digest", reviewDigest, ...scope,
  ]);
  expectString(approval, "installation_id", installationId);
  return { publication, review, approval };
}

function updateReviewed(values, attemptKind) {
  const common = [
    "api-base", "archive", "expected-generation", "installation-id", "operations-file",
    "passphrase-file", "principal", "request-id", "workspace",
  ];
  requireOnly(values, common);
  const scope = liveScope(values);
  const installationId = required(values, "installation-id");
  const expectedGeneration = positiveInteger(required(values, "expected-generation"), "expected-generation");
  const requestId = required(values, "request-id");

  let parkedGeneration = expectedGeneration;
  let parking;
  if (attemptKind === "update") {
    parking = runApp([
      "update-begin", installationId,
      "--expected-generation", String(expectedGeneration),
      "--request-id", `${requestId}:begin`, ...scope,
    ]);
    expectString(parking, "installation_id", installationId);
    parkedGeneration = positiveInteger(parking.generation, "update-begin generation");
    if (parkedGeneration !== expectedGeneration + 1 || parking.status !== "update_pending") {
      fail("update-begin did not return the exact parked generation");
    }
  }

  const publication = runApp([
    "candidate-publish", required(values, "archive"),
    "--request-id", `${requestId}:candidate`,
    "--installation-id", installationId,
    "--attempt-kind", attemptKind, ...scope,
  ]);
  expectString(publication, "installation_id", installationId);
  if (publication.activation_authority_granted !== false
    || publication.state !== (attemptKind === "update" ? "update_pending" : "uninstalled_retained")) {
    fail("existing-installation publication widened lifecycle authority");
  }
  const attemptId = requiredResultString(publication, "attempt_id");

  const plan = runApp([
    "update-plan", installationId,
    "--attempt-id", attemptId,
    "--expected-generation", String(parkedGeneration),
    "--operations-file", required(values, "operations-file"), ...scope,
  ]);
  expectString(plan, "installation_id", installationId);
  expectString(plan, "attempt_id", attemptId);
  const migrationRunId = requiredResultString(plan, "migration_run_id");
  const updatePlanDigest = requiredResultString(plan, "update_plan_digest");
  if (plan.dry_run_examined !== plan.dry_run_representable) {
    fail("migration dry-run did not represent every examined record");
  }

  let backup;
  if (plan.backup_required === true) {
    backup = runApp([
      "update-backup", migrationRunId,
      "--passphrase-file", required(values, "passphrase-file"), ...scope,
    ]);
    expectString(backup, "migration_run_id", migrationRunId);
    if (backup.encrypted !== true || backup.update_plan_digest !== updatePlanDigest) {
      fail("destructive update backup is absent or belongs to another plan");
    }
  } else if (plan.destructive === true) {
    fail("destructive update did not require a backup");
  }

  const review = runApp(["review", installationId, ...scope]);
  expectString(review, "installation_id", installationId);
  expectString(review, "attempt_id", attemptId);
  const reviewDigest = requiredResultString(review, "workflow_material_digest");
  const approveArgs = [
    "approve", installationId,
    "--review-digest", reviewDigest,
    "--migration-run-id", migrationRunId,
    "--update-plan-digest", updatePlanDigest,
  ];
  if (plan.destructive === true) approveArgs.push("--confirm-destructive-migration");
  approveArgs.push(...scope);
  const approval = runApp(approveArgs);
  expectString(approval, "installation_id", installationId);
  return { ...(parking === undefined ? {} : { parking }), publication, plan, ...(backup === undefined ? {} : { backup }), review, approval };
}

function portabilityRoundTrip(values) {
  requireOnly(values, [
    "api-base", "archive", "destination-installation-id", "kind", "passphrase-file",
    "principal", "request-id", "source-installation-id", "workspace",
  ]);
  const scope = liveScope(values);
  const source = required(values, "source-installation-id");
  const destination = required(values, "destination-installation-id");
  const archive = required(values, "archive");
  const passphrase = required(values, "passphrase-file");
  const requestId = required(values, "request-id");
  const kind = required(values, "kind");
  if (kind !== "data" && kind !== "combined") fail("kind must be data or combined");
  const exported = runApp([
    "data-export", source, "--kind", kind,
    "--request-id", `${requestId}:export`,
    "--passphrase-file", passphrase,
    "--output", archive, ...scope,
  ]);
  expectString(exported, "request_id", `${requestId}:export`);
  const preview = runApp([
    "data-import-preview", destination, archive,
    "--request-id", `${requestId}:preview`,
    "--passphrase-file", passphrase, ...scope,
  ]);
  expectString(preview, "installation_id", destination);
  const previewDigest = requiredResultString(preview, "preview_digest");
  const approval = runApp([
    "data-import-approve", destination,
    "--preview-digest", previewDigest,
    "--request-id", `${requestId}:approve`, ...scope,
  ]);
  expectString(approval, "installation_id", destination);
  const approvalRef = requiredResultString(approval, "approval_ref");
  const commit = runApp([
    "data-import-commit", destination,
    "--preview-digest", previewDigest,
    "--approval-ref", approvalRef,
    "--request-id", `${requestId}:commit`, ...scope,
  ]);
  expectString(commit, "installation_id", destination);
  return { exported, preview, approval, commit };
}

function runApp(args) {
  const invocation = spawnSync(magicianBin, ["app", "--json", ...args], {
    encoding: "utf8",
    maxBuffer: MAX_OUTPUT_BYTES,
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (invocation.error) fail(`unable to execute magician: ${invocation.error.message}`);
  if (invocation.status !== 0) fail(`magician app ${args[0]} failed with exit ${invocation.status}`);
  let envelope;
  try {
    envelope = JSON.parse(invocation.stdout);
  } catch {
    fail(`magician app ${args[0]} returned invalid JSON`);
  }
  if (!isObject(envelope) || envelope.ok !== true || !isObject(envelope.result)) {
    fail(`magician app ${args[0]} returned a non-success envelope`);
  }
  return envelope.result;
}

function liveScope(values) {
  const scope = [
    "--principal", required(values, "principal"),
    "--workspace", required(values, "workspace"),
  ];
  if (values["api-base"] !== undefined) scope.push("--api-base", values["api-base"]);
  return scope;
}

function parseOptions(args) {
  const parsed = Object.create(null);
  for (let index = 0; index < args.length; index += 2) {
    const key = args[index];
    const value = args[index + 1];
    if (typeof key !== "string" || !key.startsWith("--") || value === undefined || value.startsWith("--")) {
      fail("every lifecycle option must be one --name value pair");
    }
    const name = key.slice(2);
    if (parsed[name] !== undefined) fail(`duplicate option --${name}`);
    parsed[name] = value;
  }
  return parsed;
}

function requireOnly(values, allowed) {
  for (const key of Object.keys(values)) {
    if (!allowed.includes(key)) fail(`unsupported option --${key}`);
  }
}

function required(values, key) {
  const value = values[key];
  if (typeof value !== "string" || value.length === 0) fail(`missing --${key}`);
  return value;
}

function requiredResultString(value, key) {
  const result = value[key];
  if (typeof result !== "string" || result.length === 0) fail(`live result omitted ${key}`);
  return result;
}

function expectString(value, key, expected) {
  if (value[key] !== expected) fail(`live result ${key} did not match the retained request`);
}

function positiveInteger(value, label) {
  const parsed = typeof value === "number" ? value : Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 1) fail(`${label} must be a positive safe integer`);
  return parsed;
}

function isObject(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function fail(message) {
  throw new Error(message);
}
