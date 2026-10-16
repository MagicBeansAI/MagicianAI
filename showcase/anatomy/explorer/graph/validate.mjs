// Checks every tour step in explorer/graph/*.json against the source:
//   - the file exists
//   - the named fn is defined in that file (and the step points at its definition)
//   - the named type is declared in that file
// node explorer/graph/validate.mjs          report
// node explorer/graph/validate.mjs --fix    move `line` to the fn/type definition, keeping the
//                                           original branch line in `evidence`
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..", "..", "..", "..");
const fix = process.argv.includes("--fix");
const cache = new Map();
const linesOf = (file) => {
  if (!cache.has(file)) cache.set(file, existsSync(join(ROOT, file)) ? readFileSync(join(ROOT, file), "utf8").split("\n") : null);
  return cache.get(file);
};
const defLines = (lines, re) => lines.flatMap((l, i) => (re.test(l) ? [i + 1] : []));
const esc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

let problems = 0;
let fixed = 0;
for (const name of readdirSync(HERE).filter((f) => f.endsWith(".json"))) {
  const path = join(HERE, name);
  const part = JSON.parse(readFileSync(path, "utf8"));
  let changed = false;
  for (const n of part.nodes) {
    if (n.kind !== "step" || !n.file) continue;
    const lines = linesOf(n.file);
    if (!lines) {
      console.log(`${n.id}: file missing ${n.file}`);
      problems++;
      continue;
    }
    if (n.type) {
      const decl = defLines(lines, new RegExp(`\\b(struct|enum|trait|class|protocol|interface)\\s+${esc(n.type)}\\b`));
      if (!decl.length) {
        console.log(`${n.id}: type ${n.type} not declared in ${n.file}`);
        problems++;
      }
    }
    if (n.fn) {
      const defs = defLines(lines, new RegExp(`\\bfn\\s+${esc(n.fn)}\\b|\\bfunc\\s+${esc(n.fn)}\\b|\\bfun\\s+${esc(n.fn)}\\b|\\bfunction\\s+${esc(n.fn)}\\b`));
      if (!defs.length) {
        console.log(`${n.id}: fn ${n.fn} not defined in ${n.file}`);
        problems++;
        continue;
      }
      if (!defs.includes(n.line)) {
        // the definition that contains the cited line (closest one above it), else the first
        const above = defs.filter((d) => d <= n.line);
        const def = above.length ? above[above.length - 1] : defs[0];
        console.log(`${n.id}: line ${n.line} is inside ${n.fn}; definition at ${def}`);
        problems++;
        if (fix) {
          const branch = `${n.file}:${n.line}`;
          if (!String(n.evidence ?? "").includes(branch)) n.evidence = `${n.evidence ? `${n.evidence}; ` : ""}branch at ${branch}`;
          n.line = def;
          changed = true;
          fixed++;
        }
      }
    }
  }
  if (changed) writeFileSync(path, `${JSON.stringify(part, null, 1)}\n`);
}
console.log(`${problems} finding(s)${fix ? `, ${fixed} line(s) moved to the definition` : ""}`);
