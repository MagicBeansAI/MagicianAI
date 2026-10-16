// Brings the explorer up to date with the code, then says what still needs a
// person. `make showcase-refresh` runs it.
//
//   1. re-reads the code: data/extract.mjs (Rust crates, modules, types and
//      references) and data/extract-clients.mjs (apps and devices)
//   2. rebuilds explorer.html, stamping every box with its last commit
//   3. checks the hand-written meaning against the code it points at:
//        - the tour graph (explorer/graph/validate.mjs)
//        - every feature's locations and tour steps (file exists, fn defined,
//          type declared)
//   4. reports drift:
//        - modules that appeared since the last refresh
//        - the biggest modules no feature or tour step mentions
//        - narration lines that are new or changed and not yet voiced
//
// node showcase/anatomy/refresh.mjs [--narrate] [--min-lines N]
//   --narrate      voice new or changed lines with Voicebox, then rebuild
//   --min-lines N  size threshold for the uncovered-modules list (default 1500)
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = join(HERE, "..", "..");
const args = process.argv.slice(2);
const narrate = args.includes("--narrate");
const minLines = Number(args[args.indexOf("--min-lines") + 1]) || 1500;

const run = (script, extra = []) => {
  process.stdout.write(`\n▸ ${script} ${extra.join(" ")}\n`);
  return execFileSync(process.execPath, [join(HERE, script), ...extra], { cwd: REPO, stdio: ["ignore", "pipe", "inherit"], maxBuffer: 64 * 1024 * 1024 }).toString();
};
const show = (text) => process.stdout.write(text.trim().split("\n").slice(-6).join("\n") + "\n");

// module paths before, to report what is new
const modulePaths = (truth) => {
  const out = new Map();
  for (const [name, c] of Object.entries(truth.crates)) {
    const walk = (tree, prefix) => {
      for (const [seg, node] of Object.entries(tree ?? {})) {
        const path = `${c.path}/src${prefix}/${seg}`;
        out.set(path, { crate: name, lines: node.lines, dir: Object.keys(node.children ?? {}).length > 0 });
        walk(node.children, `${prefix}/${seg}`);
      }
    };
    walk(c.modules, "");
  }
  return out;
};
const truthPath = join(HERE, "data", "magician.json");
const before = existsSync(truthPath) ? modulePaths(JSON.parse(readFileSync(truthPath, "utf8"))) : new Map();

show(run("data/extract.mjs"));
show(run("data/extract-clients.mjs"));
// tour steps whose function still exists but moved get their line updated
// (the old line is kept in the step's evidence); only what cannot be found is
// left as a finding
const graphReport = run("explorer/graph/validate.mjs", ["--fix"]);
show(graphReport);

// features: every location and tour step must still point at real code; a
// pointer whose function or type moved is updated to the new definition line
const cache = new Map();
const lines = (file) => {
  if (!cache.has(file)) cache.set(file, existsSync(join(REPO, file)) ? readFileSync(join(REPO, file), "utf8").split("\n") : null);
  return cache.get(file);
};
const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
// a definition: fn / func / fun / function, an arrow or method assigned to the
// name, or a class method (`private async name(` / `name<T>(`)
const fnDef = (fn) =>
  new RegExp(`\\b(fn|func|fun|function)\\s+${esc(fn)}\\b|\\b${esc(fn)}\\s*(=|:)\\s*(async\\s*)?\\(|^\\s*(?:(?:public|private|protected|static|async|override|readonly|export)\\s+)*${esc(fn)}\\s*(<[^>]*>)?\\(`);
const typeDef = (type) => new RegExp(`\\b(struct|enum|trait|class|object|protocol|interface|type)\\s+${esc(type)}\\b`);
const featureFiles = readdirSync(join(HERE, "data")).filter((f) => /^features-.*\.json$/.test(f)).sort().map((f) => join(HERE, "data", f));
const featureDocs = featureFiles.map((f) => JSON.parse(readFileSync(f, "utf8")));
const features = featureDocs.flatMap((d) => d.features);
const featureProblems = [];
const covered = new Set();
let moved = 0;
for (const f of features) {
  for (const [kind, list] of [["location", f.locations ?? []], ["tour", f.tour ?? []]]) {
    for (const loc of list) {
      covered.add(loc.file);
      const text = lines(loc.file);
      if (!text) {
        featureProblems.push(`${f.id} ${kind}: missing file ${loc.file}`);
        continue;
      }
      const at = (re) => text.flatMap((l, i) => (re.test(l) ? [i + 1] : []));
      const fnLines = loc.fn ? at(fnDef(loc.fn)) : [];
      const typeLines = loc.type ? at(typeDef(loc.type)) : [];
      if (loc.fn && !fnLines.length) featureProblems.push(`${f.id} ${kind}: ${loc.fn} not defined in ${loc.file}`);
      if (loc.type && !typeLines.length) featureProblems.push(`${f.id} ${kind}: ${loc.type} not declared in ${loc.file}`);
      const defs = fnLines.length ? fnLines : typeLines;
      if (defs.length && loc.line && !defs.includes(loc.line)) {
        const above = defs.filter((d) => d <= loc.line);
        loc.line = above.length ? above[above.length - 1] : defs[0];
        moved++;
      }
    }
  }
  if (f.uiSurface?.path) covered.add(f.uiSurface.path);
}
if (moved) featureFiles.forEach((file, i) => writeFileSync(file, `${JSON.stringify(featureDocs[i], null, 1)}\n`));

if (narrate) show(run("explorer/narrate.mjs"));
show(run("explorer/build.mjs"));
for (const part of ["chat", "brain", "task", "voice", "apps"]) {
  const file = join(HERE, "explorer", "graph", `${part}.json`);
  if (existsSync(file)) for (const n of JSON.parse(readFileSync(file, "utf8")).nodes) if (n.file) covered.add(n.file);
}

// drift
const after = modulePaths(JSON.parse(readFileSync(truthPath, "utf8")));
// a Rust module is `name.rs` and/or `name/`; a mention of either covers it
const isCovered = (path) => [...covered].some((c) => c === `${path}.rs` || c.startsWith(`${path}/`));
const fresh = [...after].filter(([p]) => before.size && !before.has(p)).sort((a, b) => b[1].lines - a[1].lines);
// only the outermost uncovered module of each branch, biggest first
// test modules (`tests`, `foo_tests`) are not features
const isTest = (path) => /(^|\/|_)tests$/.test(path);
const uncovered = [...after]
  .filter(([p, m]) => m.lines >= minLines && !isTest(p) && !isCovered(p))
  .filter(([p], _, all) => !all.some(([q, m]) => q !== p && p.startsWith(`${q}/`) && m.lines >= minLines))
  .sort((a, b) => b[1].lines - a[1].lines);
let unvoiced = 0;
try {
  unvoiced = (run("explorer/narrate.mjs", ["--list"]).match(/\(not voiced\)/g) ?? []).length;
} catch {
  unvoiced = -1;
}

const out = [];
out.push("\n── drift report ─────────────────────────────────────────────");
out.push(`tour graph: ${graphReport.trim().split("\n").pop()}`);
if (moved) out.push(`features: ${moved} pointer(s) followed their code to a new line`);
out.push(featureProblems.length ? `features: ${featureProblems.length} pointer(s) no longer match the code` : `features: all ${features.length} features point at real code`);
for (const p of featureProblems.slice(0, 20)) out.push(`  · ${p}`);
out.push(fresh.length ? `new modules since the last refresh: ${fresh.length}` : before.size ? "new modules since the last refresh: none" : "new modules: (no earlier snapshot)");
for (const [p, m] of fresh.slice(0, 15)) out.push(`  + ${p} (${m.lines.toLocaleString("en-US")} lines)${isCovered(p) ? "" : " · no feature yet"}`);
out.push(`modules over ${minLines.toLocaleString("en-US")} lines that no feature or tour step mentions: ${uncovered.length}`);
for (const [p, m] of uncovered.slice(0, 15)) out.push(`  ? ${p} (${m.lines.toLocaleString("en-US")} lines)`);
out.push(unvoiced < 0 ? "narration: could not list (is node available?)" : unvoiced ? `narration: ${unvoiced} line(s) not voiced yet · run with --narrate (Voicebox must be running)` : "narration: every line is voiced");
out.push("next: add or fix features in data/features-*.json, then refresh again; publish with make publish-showcase-preview");
console.log(out.join("\n"));
