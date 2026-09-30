#!/usr/bin/env node
// Names the packages whose native addons THIS Node cannot load because they
// were compiled for another Node ABI (NODE_MODULE_VERSION). One package per
// line on stdout; nothing when the tree is clean. Exit status is 0 either
// way — the caller decides what to rebuild.
//
// Why the runtime is asked instead of a file being inspected: the lockfile
// does not move when `.node/` is swapped for a new major, so a stamp keyed
// on it reads a stale tree as current, and the ABI an addon was built for is
// not reliably readable from the binary. `process.dlopen` is the exact
// check `require` performs, so what fails here is what fails in a bot.
// N-API addons (fsevents, sharp) load across majors and are never named;
// a compile-time ABI addon (better-sqlite3) is. Load errors that are not an
// ABI mismatch are left alone: they are not what a Node bump broke.
//
// Usage: node scripts/native_addons_needing_rebuild.mjs [node_modules]

import { readdirSync } from "node:fs";
import { join, resolve, sep } from "node:path";
import process from "node:process";

const root = resolve(process.argv[2] ?? "node_modules");

function* addonFiles(dir) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === ".cache") continue;
      yield* addonFiles(path);
    } else if (entry.isFile() && entry.name.endsWith(".node")) {
      yield path;
    }
  }
}

// The innermost `node_modules/<name>` or `node_modules/@scope/<name>` above
// the addon owns it — nested trees resolve to the nested package.
function owningPackage(file) {
  const parts = file.split(sep);
  const at = parts.lastIndexOf("node_modules");
  if (at < 0 || at + 1 >= parts.length) return null;
  const first = parts[at + 1];
  if (first.startsWith("@")) {
    return at + 2 < parts.length ? `${first}/${parts[at + 2]}` : null;
  }
  return first;
}

function isAbiMismatch(error) {
  return (
    error?.code === "ERR_DLOPEN_FAILED" &&
    /NODE_MODULE_VERSION/.test(String(error.message))
  );
}

const stale = new Set();
for (const file of addonFiles(root)) {
  try {
    process.dlopen({ exports: {} }, file);
  } catch (error) {
    if (!isAbiMismatch(error)) continue;
    const pkg = owningPackage(file);
    if (pkg) stale.add(pkg);
  }
}

for (const pkg of [...stale].sort()) process.stdout.write(`${pkg}\n`);
