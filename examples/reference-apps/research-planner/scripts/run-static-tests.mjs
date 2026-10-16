import { readdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const testsRoot = resolve(projectRoot, "dist", "tests");
const tests = readdirSync(testsRoot)
  .filter((name) => name.endsWith(".test.js") && !name.includes("canary"))
  .sort()
  .map((name) => resolve(testsRoot, name));

if (tests.length === 0) throw new Error("no compiled static reference tests found");
const result = spawnSync(process.execPath, ["--test", ...tests], {
  encoding: "utf8",
  maxBuffer: 4 * 1024 * 1024,
  stdio: "inherit",
});
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
