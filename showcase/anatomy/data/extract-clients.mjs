// Counts the code of every client (desktop, web, iOS, Android, ESP32, Kindle,
// helpers, extension, bots, SDK) from the source roots in data/world.json,
// skipping generated, vendored and build output. Writes data/clients.json.
// node data/extract-clients.mjs
import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, extname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..", "..", "..");
const WORLD = JSON.parse(readFileSync(join(HERE, "world.json"), "utf8"));

const LANG = {
  ".rs": "Rust", ".swift": "Swift", ".kt": "Kotlin", ".kts": "Kotlin", ".java": "Java", ".c": "C", ".h": "C", ".cpp": "C++", ".cc": "C++", ".hpp": "C++",
  ".ts": "TypeScript", ".tsx": "TypeScript", ".js": "JavaScript", ".mjs": "JavaScript", ".cjs": "JavaScript", ".svelte": "Svelte",
  ".py": "Python", ".proto": "Protobuf", ".sh": "Shell", ".go": "Go", ".mts": "TypeScript", ".cts": "TypeScript",
};
// rough, per-language: declared types, functions, tests
const RULES = {
  Rust: { types: /^\s*(pub(\([^)]*\))?\s+)?(struct|enum|trait)\s+\w/gm, fns: /^\s*(pub(\([^)]*\))?\s+)?(async\s+)?fn\s+\w/gm, tests: /#\[(tokio::)?test\b/gm },
  Swift: { types: /^\s*(?:(?:public|private|internal|open|fileprivate|final|@\w+)\s+)*(struct|class|enum|protocol|actor)\s+\w/gm, fns: /\bfunc\s+\w/g, tests: /\bfunc\s+test\w*\s*\(/g },
  Kotlin: { types: /^\s*(?:(?:public|private|internal|open|abstract|sealed|data|enum|inner|value|annotation)\s+)*(class|interface|object)\s+\w/gm, fns: /\bfun\s+[\w<.]/g, tests: /@Test\b/g },
  Java: { types: /\b(class|interface|enum|record)\s+[A-Z]\w*/g, fns: /^\s*(public|private|protected|static|\s)+[\w<>\[\]]+\s+\w+\s*\([^;]*\)\s*\{/gm, tests: /@Test\b/g },
  C: { types: /\b(typedef\s+struct|struct\s+\w+\s*\{|typedef\s+enum|enum\s+\w+\s*\{)/g, fns: /^[A-Za-z_][\w\s\*]*\s\**[A-Za-z_]\w*\s*\([^;{]*\)\s*\{/gm, tests: /\bTEST(_CASE)?\s*\(/g },
  "C++": { types: /\b(class|struct)\s+[A-Za-z_]\w*\s*[:{]/g, fns: /^[A-Za-z_][\w:<>\s\*&]*\s[\*&]?[A-Za-z_][\w:]*\s*\([^;{]*\)\s*(const\s*)?\{/gm, tests: /\bTEST(_F)?\s*\(/g },
  TypeScript: { types: /\b(class|interface)\s+[A-Za-z_]\w*|\btype\s+[A-Z]\w*\s*(<[^>]*>)?\s*=/g, fns: /\bfunction\s+\w+|\b(const|let)\s+\w+\s*=\s*(async\s*)?\([^)]*\)\s*(:\s*[^=]+)?=>/g, tests: /\b(it|test)\s*\(\s*["'`]/g },
  JavaScript: { types: /\bclass\s+[A-Za-z_]\w*/g, fns: /\bfunction\s+\w+|\b(const|let)\s+\w+\s*=\s*(async\s*)?\([^)]*\)\s*=>/g, tests: /\b(it|test)\s*\(\s*["'`]/g },
  Svelte: { types: /\b(interface)\s+[A-Za-z_]\w*|\btype\s+[A-Z]\w*\s*=/g, fns: /\bfunction\s+\w+|\b(const|let)\s+\w+\s*=\s*(async\s*)?\([^)]*\)\s*=>/g, tests: /$^/g },
  Shell: { types: /$^/g, fns: /^\s*(function\s+)?[A-Za-z_][\w-]*\s*\(\)\s*\{/gm, tests: /$^/g },
  Python: { types: /^\s*class\s+\w/gm, fns: /^\s*(async\s+)?def\s+\w/gm, tests: /^\s*def\s+test_\w/gm },
  Go: { types: /^type\s+\w+\s+(struct|interface)/gm, fns: /^func\s/gm, tests: /^func\s+Test\w/gm },
};

const globRe = (g) => new RegExp(`^${g.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*\*\//g, "(.*/)?").replace(/\*\*/g, ".*").replace(/\*/g, "[^/]*")}$`);
const ALWAYS = ["**/node_modules/**", "**/.build/**", "**/build/**", "**/dist/**", "**/.svelte-kit/**", "**/target/**", "**/DerivedData/**", "**/*.min.js", "**/*.generated.*"];

function excluded(rel, excludes) {
  for (const e of [...excludes, ...ALWAYS]) {
    if (!e.includes("*")) {
      if (rel === e || rel.startsWith(`${e}/`)) return true;
    } else if (globRe(e).test(rel) || globRe(`${e}/**`).test(rel)) return true;
  }
  return false;
}
function shebang(abs) {
  try {
    const head = readFileSync(abs, "utf8").slice(0, 80);
    if (!head.startsWith("#!")) return null;
    if (/python/.test(head)) return "Python";
    if (/node|deno|bun/.test(head)) return "JavaScript";
    if (/\b(ba|z)?sh\b/.test(head)) return "Shell";
  } catch {}
  return null;
}
const langOf = (abs) => LANG[extname(abs)] ?? (extname(abs) === "" ? shebang(abs) : null);
function walk(abs, excludes, out) {
  let st;
  try {
    st = statSync(abs);
  } catch {
    return out;
  }
  const rel = relative(ROOT, abs);
  if (excluded(rel, excludes)) return out;
  if (st.isDirectory()) for (const name of readdirSync(abs)) {
    if (name.startsWith(".")) continue;
    walk(join(abs, name), excludes, out);
  }
  else if (langOf(abs)) out.push(abs);
  return out;
}
const blank = () => ({ files: 0, lines: 0, types: 0, fns: 0, tests: 0, children: {} });
const add = (a, c) => {
  for (const k of ["files", "lines", "types", "fns", "tests"]) a[k] += c[k];
};

const out = {};
for (const client of WORLD.clients) {
  const roots = client.roots ?? [];
  const total = blank();
  const langs = {};
  for (const root of roots) {
    // tracked-only clients skip build output and vendored downloads git never saw
    const tracked = client.tracked ? new Set(execFileSync("git", ["ls-files", root], { cwd: ROOT, maxBuffer: 64 << 20 }).toString().split("\n").filter(Boolean).map((f) => join(ROOT, f))) : null;
    for (const file of walk(join(ROOT, root), client.exclude ?? [], [])) {
      if (tracked && !tracked.has(file)) continue;
      const text = readFileSync(file, "utf8");
      const lang = langOf(file);
      const r = RULES[lang] ?? {};
      const c = { files: 1, lines: text.split("\n").length, types: (text.match(r.types ?? /$^/g) || []).length, fns: (text.match(r.fns ?? /$^/g) || []).length, tests: (text.match(r.tests ?? /$^/g) || []).length };
      langs[lang] = (langs[lang] ?? 0) + c.lines;
      add(total, c);
      // tree: one top node per root, then directories, then the file
      const rootRel = relative(ROOT, join(ROOT, root));
      const inside = relative(join(ROOT, root), file);
      const parts = [rootRel, ...(inside ? inside.split("/") : [])];
      let node = total;
      for (const [i, part] of parts.entries()) {
        node = node.children[part] ??= blank();
        add(node, c);
        if (i === parts.length - 1) node.file = relative(ROOT, file);
      }
    }
  }
  out[client.id] = { ...total, languages: Object.fromEntries(Object.entries(langs).sort((a, b) => b[1] - a[1])) };
  console.log(`${client.id.padEnd(20)} ${String(total.files).padStart(5)} files ${String(total.lines).padStart(8)} lines  ${Object.keys(out[client.id].languages).join(", ")}`);
}
writeFileSync(join(HERE, "clients.json"), JSON.stringify(out));
