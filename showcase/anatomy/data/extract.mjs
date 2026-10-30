// Truth extractor: every number the anatomy figures print comes from here.
// Usage: node showcase/anatomy/data/extract.mjs  →  writes showcase/anatomy/data/magician.json
import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..", "..", "..");

const meta = JSON.parse(
  execFileSync("make", ["--no-print-directory", "-s", "cargo", "ARGS=metadata --no-deps --format-version 1"], {
    cwd: ROOT,
    maxBuffer: 64 << 20,
  }).toString(),
);
const members = new Set(meta.packages.map((p) => p.name));

const PATTERNS = {
  structs: /^\s*(pub(\([^)]*\))?\s+)?struct\s+\w+/gm,
  enums: /^\s*(pub(\([^)]*\))?\s+)?enum\s+\w+/gm,
  traits: /^\s*(pub(\([^)]*\))?\s+)?(unsafe\s+)?trait\s+\w+/gm,
  impls: /^\s*impl(<[^>]*>)?\s/gm,
  fns: /^\s*(pub(\([^)]*\))?\s+)?(const\s+)?(async\s+)?(unsafe\s+)?fn\s+\w+/gm,
  asyncFns: /^\s*(pub(\([^)]*\))?\s+)?async\s+fn\s+\w+/gm,
  tests: /#\[(tokio::)?test\b/gm,
};

function walk(dir, out = []) {
  for (const name of readdirSync(dir)) {
    if (name === "target" || name === "node_modules" || name.startsWith(".")) continue;
    const path = join(dir, name);
    const info = statSync(path);
    if (info.isDirectory()) walk(path, out);
    else if (name.endsWith(".rs")) out.push(path);
  }
  return out;
}

function countIn(text) {
  const counts = {};
  for (const [key, re] of Object.entries(PATTERNS)) counts[key] = (text.match(re) || []).length;
  return counts;
}

// ---------- types: struct / enum / trait with member counts ----------
function blockAfter(text, from) {
  // returns [bodyStart, bodyEnd] of the {...} block starting at the first "{" after `from`, or null for `;` / tuple forms
  const open = text.indexOf("{", from);
  const semi = text.indexOf(";", from);
  if (open < 0 || (semi >= 0 && semi < open)) return null;
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    const c = text[i];
    if (c === "{") depth++;
    else if (c === "}" && --depth === 0) return [open + 1, i];
  }
  return null;
}
function membersOf(kind, body) {
  // count depth-1 members: fields, variants or fns
  let depth = 0;
  let count = 0;
  for (const line of body.split("\n")) {
    const t = line.trim();
    if (depth === 0 && t && !t.startsWith("//") && !t.startsWith("#")) {
      if (kind === "struct" && /^(pub(\([^)]*\))?\s+)?[a-z_][a-z0-9_]*\s*:/.test(t)) count++;
      else if (kind === "enum" && /^[A-Z][A-Za-z0-9_]*/.test(t)) count++;
      else if (kind === "trait" && /^(async\s+|unsafe\s+)*fn\s/.test(t)) count++;
    }
    for (const c of t) depth += c === "{" || c === "(" ? 1 : c === "}" || c === ")" ? -1 : 0;
    if (depth < 0) depth = 0;
  }
  return count;
}
const TYPE_RE = /^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(struct|enum|trait)\s+([A-Za-z_][A-Za-z0-9_]*)/gm;
const IMPL_RE = /^[ \t]*impl(?:<[^>{]*>)?\s+(?:[a-z_][a-z0-9_]*::)*([A-Z][A-Za-z0-9_]*)(?:<[^>{]*>)?\s+for\s+(?:[a-z_][a-z0-9_]*::)*([A-Z][A-Za-z0-9_]*)/gm;
function testRanges(text) {
  // byte ranges of #[cfg(test)] modules: types inside them are test doubles
  const out = [];
  for (const m of text.matchAll(/#\[cfg\(test\)\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{/g)) {
    const block = blockAfter(text, m.index + m[0].length - 1);
    if (block) out.push(block);
  }
  return out;
}
function typesIn(text, testFile = false) {
  const lineAt = (i) => text.slice(0, i).split("\n").length;
  const tests = testRanges(text);
  const out = [];
  for (const m of text.matchAll(TYPE_RE)) {
    const [, kind, name] = m;
    const block = blockAfter(text, m.index + m[0].length);
    const span = block ? lineAt(block[1]) - lineAt(m.index) + 1 : 1;
    const test = testFile || tests.some(([a, b]) => m.index > a && m.index < b);
    out.push([kind, name, lineAt(m.index), span, block ? membersOf(kind, text.slice(block[0], block[1])) : 0, test ? 1 : 0]);
  }
  return out;
}
const implsIn = (text) => [...text.matchAll(IMPL_RE)].map((m) => [m[1], m[2]]);

// ---------- module references: crate::, super:: and other-crate:: paths ----------
let CRATE_IDENT = new Map(); // rust ident → package name, filled once cargo metadata is read
let CRATE_PATH_RE = null;
function pathsIn(text, selfMod) {
  const refs = [];
  const read = (i, base) => {
    // read a::b::c or a::{x, y::z} starting at i, pushing full segment lists
    const seg = [];
    let j = i;
    for (;;) {
      const m = /^[A-Za-z_][A-Za-z0-9_]*/.exec(text.slice(j, j + 80));
      if (m) {
        seg.push(m[0]);
        j += m[0].length;
        if (text.startsWith("::", j)) {
          j += 2;
          if (text[j] === "{") {
            let depth = 0;
            let k = j;
            for (; k < text.length; k++) {
              if (text[k] === "{") depth++;
              else if (text[k] === "}" && --depth === 0) break;
            }
            const inner = text.slice(j + 1, k);
            let d = 0;
            let start = 0;
            for (let q = 0; q <= inner.length; q++) {
              const c = inner[q];
              if (c === "{") d++;
              else if (c === "}") d--;
              else if ((c === "," && d === 0) || q === inner.length) {
                const part = inner.slice(start, q).trim();
                if (part) refs.push([...base, ...seg, ...part.split("::").map((x) => x.replace(/[^A-Za-z0-9_].*$/, "")).filter(Boolean)]);
                start = q + 1;
              }
            }
            return;
          }
          continue;
        }
      }
      break;
    }
    if (seg.length) refs.push([...base, ...seg]);
  };
  for (const m of text.matchAll(/\bcrate::/g)) read(m.index + 7, []);
  for (const m of text.matchAll(/\bsuper::/g)) read(m.index + 7, selfMod.slice(0, -1));
  // paths into other workspace crates start with a "@crate-name" segment
  for (const m of text.matchAll(CRATE_PATH_RE ?? /$^/g)) if (m.index === 0 || !/[A-Za-z0-9_:]/.test(text[m.index - 1])) read(m.index + m[0].length, [`@${CRATE_IDENT.get(m[1])}`]);
  return refs;
}

function moduleTree(srcDir, files) {
  // Nested module tree under src/: `foo.rs` and `foo/` fold into one node `foo`,
  // each node carrying its own and its descendants' counts.
  const root = { files: 0, lines: 0, structs: 0, enums: 0, traits: 0, fns: 0, tests: 0, children: {} };
  for (const file of files) {
    const rel = relative(srcDir, file);
    if (rel.startsWith("..")) continue;
    const parts = rel.replace(/\.rs$/, "").split("/").filter((p) => p !== "mod" && p !== "lib" && p !== "main");
    const text = readFileSync(file, "utf8");
    const c = { files: 1, lines: text.split("\n").length, ...countIn(text) };
    const leafParts = rel.replace(/\.rs$/, "").split("/").filter((p) => p !== "mod");
    let node = root;
    const add = (n) => { for (const key of ["files", "lines", "structs", "enums", "traits", "fns", "tests"]) n[key] += c[key]; };
    add(node);
    for (const part of leafParts) {
      node = node.children[part] ??= { files: 0, lines: 0, structs: 0, enums: 0, traits: 0, fns: 0, tests: 0, children: {} };
      add(node);
    }
    const types = typesIn(text, /(^|\/)tests?(\/|\.rs$)|_tests?\.rs$/.test(rel));
    if (types.length) node.types = (node.types ?? []).concat(types);
    const impls = implsIn(text);
    if (impls.length) node.impls = (node.impls ?? []).concat(impls);
    node.refs = (node.refs ?? []).concat(pathsIn(text, parts));
    node.modPath = parts;
  }
  rollUpRefs(root);
  if (root.links) Object.defineProperty(root.children, "__links", { value: root.links, enumerable: false, configurable: true });
  Object.defineProperty(root.children, "__xrefs", { value: root.xrefs ?? [], enumerable: false, configurable: true });
  return root.children;
}

// Resolve every reference to the deepest existing module, then count, at each
// container, references from one child subtree into another.
function rollUpRefs(root) {
  const resolve = (segs) => {
    let node = root;
    const path = [];
    for (const s of segs) {
      if (!node.children[s]) break;
      node = node.children[s];
      path.push(s);
    }
    return path;
  };
  const walk = (node, path) => {
    for (const [name, child] of Object.entries(node.children)) walk(child, [...path, name]);
    if (!node.refs) return;
    for (const ref of node.refs) {
      if (ref[0]?.startsWith("@")) {
        // another crate: kept unresolved until every crate's tree exists
        if (path.length) (root.xrefs ??= []).push([path, ref[0].slice(1), ref.slice(1)]);
        continue;
      }
      const target = resolve(ref);
      if (!target.length) continue;
      let k = 0;
      while (k < path.length && k < target.length && path[k] === target[k]) k++;
      if (k >= path.length || k >= target.length) continue; // same subtree
      let container = root;
      for (const s of path.slice(0, k)) container = container.children[s];
      const key = `${path[k]}>${target[k]}`;
      (container.links ??= {})[key] = (container.links[key] ?? 0) + 1;
    }
    delete node.refs;
    delete node.modPath;
  };
  walk(root, []);
}

const crates = {};
const XREFS = new Map();
CRATE_IDENT = new Map(meta.packages.filter((p) => members.has(p.name)).map((p) => [p.name.replace(/-/g, "_"), p.name]));
CRATE_PATH_RE = new RegExp(`(${[...CRATE_IDENT.keys()].sort((a, b) => b.length - a.length).join("|")})::`, "g");
for (const pkg of meta.packages) {
  const dir = dirname(pkg.manifest_path);
  const files = walk(dir);
  const totals = { files: files.length, lines: 0, structs: 0, enums: 0, traits: 0, impls: 0, fns: 0, asyncFns: 0, tests: 0 };
  for (const file of files) {
    const text = readFileSync(file, "utf8");
    totals.lines += text.split("\n").length;
    const c = countIn(text);
    for (const key of Object.keys(c)) totals[key] += c[key];
  }
  crates[pkg.name] = {
    path: relative(ROOT, dir),
    version: pkg.version,
    description: pkg.description ?? null,
    targets: pkg.targets.map((t) => ({ name: t.name, kind: t.kind })),
    deps: [...new Set(pkg.dependencies.filter((d) => members.has(d.name) && !d.kind).map((d) => d.name))].sort(),
    devDeps: [...new Set(pkg.dependencies.filter((d) => members.has(d.name) && d.kind === "dev").map((d) => d.name))].sort(),
    externalDeps: pkg.dependencies.filter((d) => !members.has(d.name) && !d.kind).length,
    ...totals,
    ...(() => {
      const tree = moduleTree(join(dir, "src"), files);
      const links = tree.__links;
      const xrefs = tree.__xrefs;
      delete tree.__links;
      delete tree.__xrefs;
      XREFS.set(pkg.name, xrefs);
      return { modules: tree, moduleLinks: links ?? {} };
    })(),
  };
}

// Cross-crate references, resolved to the deepest module of the target crate and
// counted per (source module path, target crate, target module path). Only paths
// into crates the source actually depends on count, so a stray comment can't
// invent a wire. Kept to the heaviest per crate.
const MAX_XLINKS = 400;
for (const [name, list] of XREFS) {
  const counts = new Map();
  for (const [from, target, segs] of list) {
    if (target === name || !crates[target] || !crates[name].deps.includes(target)) continue;
    let node = { children: crates[target].modules };
    const to = [];
    for (const s of segs) {
      if (!node.children?.[s]) break;
      node = node.children[s];
      to.push(s);
    }
    const key = `${from.join("/")}>${target}>${to.join("/")}`;
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  crates[name].crossLinks = [...counts].sort((a, b) => b[1] - a[1]).slice(0, MAX_XLINKS).map(([key, n]) => [...key.split(">"), n]);
}

const sum = (key) => Object.values(crates).reduce((acc, c) => acc + c[key], 0);
const git = (args) => execFileSync("git", args, { cwd: ROOT }).toString().trim();
const out = {
  generatedAt: new Date().toISOString(),
  // the scan reads the working tree, so say when it differs from the commit
  commit: git(["rev-parse", "--short", "HEAD"]) + (git(["status", "--porcelain", "--untracked-files=no"]) ? " + uncommitted changes" : ""),
  branch: git(["rev-parse", "--abbrev-ref", "HEAD"]),
  totals: { crates: Object.keys(crates).length, files: sum("files"), lines: sum("lines"), structs: sum("structs"), enums: sum("enums"), traits: sum("traits"), fns: sum("fns"), tests: sum("tests") },
  crates,
};
writeFileSync(join(HERE, "magician.json"), JSON.stringify(out, null, 2));
console.log(`wrote magician.json · ${out.totals.crates} crates · ${out.totals.files} files · ${out.totals.lines.toLocaleString()} lines @ ${out.commit}`);
