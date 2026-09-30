import { mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const referenceAppsRoot = resolve(projectRoot, "..");
const outputRoot = resolve(projectRoot, "dist", "packages");
const magicianBin = process.env.MAGICIAN_BIN ?? "magician";
mkdirSync(outputRoot, { recursive: true, mode: 0o700 });

const packages = [
  {
    root: resolve(projectRoot, "app"),
    output: resolve(outputRoot, "research-planner-0.1.1.app.zip"),
    ownerArgs: [
      "--skills-dir", resolve(projectRoot, "owner-fixtures", "skills"),
      "--templates-dir", resolve(projectRoot, "owner-fixtures", "agents"),
    ],
  },
  {
    root: resolve(referenceAppsRoot, "research-planner-composition-destination", "app"),
    output: resolve(outputRoot, "research-planner-composition-destination-0.1.0.app.zip"),
    ownerArgs: [
      "--skills-dir", resolve(projectRoot, "owner-fixtures", "skills"),
      "--templates-dir", resolve(projectRoot, "owner-fixtures", "agents"),
    ],
  },
  {
    root: resolve(referenceAppsRoot, "research-planner-update-v0.2.0", "app"),
    output: resolve(outputRoot, "research-planner-0.2.0.app.zip"),
    ownerArgs: [
      "--skills-dir", resolve(projectRoot, "owner-fixtures", "skills"),
      "--templates-dir", resolve(projectRoot, "owner-fixtures", "agents"),
    ],
  },
];

for (const candidate of packages) {
  run(["app", "check", candidate.root, "--write-generated"]);
  run(["app", "test", candidate.root]);
  run(["app", "pack", candidate.root, ...candidate.ownerArgs, "--output", candidate.output]);
}

process.stdout.write(`${JSON.stringify({
  schema_version: 1,
  status: "packed",
  outputs: packages.map((candidate) => candidate.output),
})}\n`);

function run(args) {
  const invocation = spawnSync(magicianBin, args, {
    encoding: "utf8",
    maxBuffer: 1_048_576,
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (invocation.error) throw invocation.error;
  if (invocation.status !== 0) {
    throw new Error(`public ${args.slice(0, 3).join(" ")} failed with exit ${invocation.status}`);
  }
}
