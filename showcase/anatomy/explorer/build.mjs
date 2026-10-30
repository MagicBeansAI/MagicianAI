// Builds showcase/anatomy/explorer.html: one 3D canvas holding the whole
// system, from the sealed case down to single source files. Every number is
// read from data/magician.json (cargo metadata + source scan).
// node explorer/build.mjs
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { buildTours } from "./tours.mjs";
import { site, metadataHead } from "../../../scripts/public-site-assets.mjs";
import { createAmbientBackground } from "./ambient.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const TRUTH = JSON.parse(readFileSync(join(ROOT, "data", "magician.json"), "utf8"));
const ROLES = JSON.parse(readFileSync(join(ROOT, "data", "roles.json"), "utf8"));
const C = TRUTH.crates;

function closure(name, seen = new Set([name])) {
  for (const dep of C[name].deps) if (!seen.has(dep)) (seen.add(dep), closure(dep, seen));
  return seen;
}
const reduced = (names) => {
  const set = new Set(names);
  const edges = [];
  for (const a of names)
    for (const b of C[a].deps)
      if (set.has(b) && !C[a].deps.some((c) => c !== b && closure(c).has(b) && c !== a)) edges.push([a, b]);
  return edges;
};

const STATS = ["files", "lines", "structs", "enums", "traits", "fns", "tests"];
const pick = (src) => Object.fromEntries(STATS.map((k) => [k, src[k] ?? 0]));

// module tree → nodes: directories hold modules, files hold their types
const TYPE_INDEX = new Map(); // `${crate}:${name}` → [type node]
const IMPLS = []; // [crate, trait, type]
const MAX_LINKS = 40;
function linksFor(owner, map) {
  return Object.entries(map ?? {})
    .sort((a, b) => b[1] - a[1])
    .slice(0, MAX_LINKS)
    .map(([key, count]) => {
      const [a, b] = key.split(">");
      return [`${owner}/${a}`.replace("//", "/"), `${owner}/${b}`.replace("//", "/"), "dep", `${count} references`];
    });
}
function moduleNodes(crate, tree, prefix) {
  return Object.entries(tree)
    .map(([name, node]) => {
      const id = `${crate}::${prefix}/${name}`.replace("//", "/");
      const sub = moduleNodes(crate, node.children, `${prefix}/${name}`);
      const isDir = sub.length > 0;
      const path = `${C[crate].path}/src${prefix}/${name}${isDir ? "/" : ".rs"}`.replace("//", "/");
      const types = (node.types ?? []).map(([kind, tname, line, span, members, test]) => {
        const t = { id: `${id}#${tname}@${line}`, name: tname, kind, line, members, ...(test ? { test: true } : {}), path: `${isDir ? `${path}mod.rs` : path}:${line}`, lines: span, files: 0, structs: 0, enums: 0, traits: 0, fns: 0, tests: 0, children: [] };
        const key = `${crate}:${tname}`;
        TYPE_INDEX.set(key, [...(TYPE_INDEX.get(key) ?? []), t]);
        return t;
      });
      for (const [trait, type] of node.impls ?? []) IMPLS.push([crate, trait, type]);
      return {
        id,
        name: isDir ? `${name}/` : `${name}.rs`,
        kind: isDir ? "module" : "file",
        path,
        ...pick(node),
        links: linksFor(id, node.links),
        children: [...sub, ...types.sort((x, y) => y.lines - x.lines)],
      };
    })
    .sort((a, b) => b.lines - a.lines);
}
function crateNode(name) {
  const c = C[name];
  return {
    id: name,
    name,
    kind: "crate",
    path: c.path,
    role: ROLES[name] ?? "",
    deps: c.deps,
    ...pick(c),
    links: linksFor(`${name}::`, c.moduleLinks),
    // [from module path, target crate, target module path, references]
    xlinks: (c.crossLinks ?? []).slice(0, 160),
    children: moduleNodes(name, c.modules, ""),
  };
}
function binaryNode(id, bin, extra) {
  const names = [...closure(bin)].sort((a, b) => C[b].lines - C[a].lines);
  const kids = names.map(crateNode);
  const sum = (k) => kids.reduce((s, n) => s + n[k], 0);
  return { id, kind: "process", ...Object.fromEntries(STATS.map((k) => [k, sum(k)])), crates: names.length, links: reduced(names), children: kids, ...extra };
}

const configText = readFileSync(join(ROOT, "..", "..", "magician-config.yaml"), "utf8");
const configKeys = [...configText.matchAll(/^([a-z_][a-z0-9_]*):/gm)].map((m) => m[1]);
const keyLines = (key) => {
  const start = configText.search(new RegExp(`^${key}:`, "m"));
  const rest = configText.slice(start + 1);
  const next = rest.search(/^[a-z_][a-z0-9_]*:/m);
  return (next < 0 ? rest : rest.slice(0, next)).split("\n").length;
};
const fileLines = (rel) => {
  try {
    return readFileSync(join(ROOT, "..", "..", rel), "utf8").split("\n").length;
  } catch {
    return 0;
  }
};
const leaf = (id, name, kind, lines, extra = {}) => ({ id, name, kind, lines, files: 0, structs: 0, enums: 0, traits: 0, fns: 0, tests: 0, children: [], ...extra });

// Layout of the processes on the case floor (world units, x right, y toward viewer).
const PLAN = {
  magicutor: { x: -136, y: -88, w: 60, d: 30, h: 26 },
  supervisor: { x: -60, y: -88, w: 84, d: 30, h: 20 },
  decision: { x: 40, y: -88, w: 50, d: 30, h: 28 },
  config: { x: 106, y: -88, w: 32, d: 30, h: 12 },
  magician: { x: -92, y: -30, w: 154, d: 70, h: 40 },
  ollama: { x: -142, y: 58, w: 62, d: 26, h: 18 },
  bots: { x: 18, y: 58, w: 76, d: 26, h: 16 },
  tools: { x: -70, y: 58, w: 80, d: 26, h: 12 },
};
const BOTS = ["telegram", "telegram-self", "whatsapp", "gmail", "kapso", "agentmail"];
const processes = [
  binaryNode("magician", "magician-bin", { name: "magician", role: "HTTP + WebSocket server on :3002; orchestration, agentic execution, chat, agents, apps", port: ":3002" }),
  binaryNode("supervisor", "magic-supervisor", { name: "magic-supervisor", role: "starts, health-checks and restarts magician, magicutor and decision-engine; control on :8081", port: ":8081" }),
  binaryNode("magicutor", "magicutor", { name: "magicutor", role: "browser sidecar: CDP proxy, extension bridge, per-thread tab ownership", port: ":3003" }),
  binaryNode("decision", "decision-engine", { name: "decision-engine", role: "typed Choice / Score / Noul decisions and tool-action verdicts over a unix socket", port: "unix socket" }),
  {
    id: "config",
    name: "config",
    kind: "process",
    role: "runtime configuration read at start-up",
    ...pick({}),
    lines: ["magician-config.yaml", "llm-router.yaml", "tool-runtime-config.yaml", "decision-engine.yaml"].reduce((s, f) => s + fileLines(f), 0),
    children: [
      { ...leaf("cfg:magician-config.yaml", "magician-config.yaml", "config", fileLines("magician-config.yaml"), { role: `${configKeys.length} top-level keys` }), children: configKeys.map((key) => leaf(`cfg:key:${key}`, key, "key", keyLines(key))) },
      leaf("cfg:llm-router.yaml", "llm-router.yaml", "config", fileLines("llm-router.yaml"), { role: "provider connections, model capabilities, workload assignments and typed fallbacks, spliced into the main config" }),
      leaf("cfg:tool-runtime-config.yaml", "tool-runtime-config.yaml", "config", fileLines("tool-runtime-config.yaml"), { role: "skill registry roots and semantic search, passed as --config" }),
      leaf("cfg:decision-engine.yaml", "decision-engine.yaml", "config", fileLines("decision-engine.yaml"), { role: "decision-engine models, tiers and operations" }),
    ],
  },
  { id: "ollama", name: "ollama", kind: "process", role: "local models started by run-supervisor.sh", ...pick({}), lines: 0, children: [leaf("ollama:gen", "generate :11434", "daemon", 1), leaf("ollama:embed", "embed :11435", "daemon", 1)] },
  // workspace crates no server process links: command-line tools run by hand
  (() => {
    const placed = new Set(["magician-bin", "magic-supervisor", "magicutor", "decision-engine"].filter((b) => C[b]).flatMap((b) => [...closure(b)]));
    const names = Object.keys(C).filter((c) => !placed.has(c)).sort((a, b) => C[b].lines - C[a].lines);
    const kids = names.map(crateNode);
    return { id: "tools", name: "tools", kind: "process", role: "command-line tools run by hand: the setup wizard, converters and storage experiments; no server process links them", ...Object.fromEntries(STATS.map((k) => [k, kids.reduce((s, n) => s + n[k], 0)])), crates: names.length, links: reduced(names), children: kids };
  })(),
  { id: "bots", name: "bots", kind: "process", role: "channel daemons spawned by magician (magician-learning BotManager)", ...pick({}), lines: 0, children: BOTS.map((b) => leaf(`bot:${b}`, b, "daemon", 1, { path: `skillshub/bots/${b}` })) },
];
for (const p of processes) {
  p.plan = PLAN[p.id];
  p.id = `p:${p.id}`;
}

const LINKS = [
  ["supervisor", "magicutor", "power", "spawn + health"],
  ["supervisor", "decision", "power", "spawn + health"],
  ["supervisor", "magician", "power", "spawn + health"],
  ["magician", "magicutor", "data", "HTTP :3003"],
  ["magician", "decision", "data", "unix socket"],
  ["config", "magician", "data", "--config"],
  ["config", "decision", "data", "decision-engine.yaml"],
  ["magician", "ollama", "data", "HTTP :11434 / :11435"],
  ["magician", "bots", "data", "spawn + HTTP/WS"],
  ["tools", "config", "data", "setup writes config + env"],
].map(([a, b, kind, say]) => [`p:${a}`, `p:${b}`, kind, say]);

const server = {
  id: "server",
  name: "Magician server",
  kind: "server",
  role: "what make run-supervisor brings up on one machine",
  ...Object.fromEntries(STATS.map((k) => [k, TRUTH.totals[k] ?? 0])),
  lines: TRUTH.totals.lines,
  files: TRUTH.totals.files,
  links: LINKS,
  children: processes,
};
// trait ↔ implementor, matched by name inside the crate first, then anywhere
const byName = (crate, name) => TYPE_INDEX.get(`${crate}:${name}`) ?? [...TYPE_INDEX.entries()].filter(([k]) => k.endsWith(`:${name}`)).flatMap(([, v]) => v);
let implCount = 0;
for (const [crate, traitName, typeName] of IMPLS) {
  const trait = byName(crate, traitName).find((t) => t.kind === "trait");
  const type = byName(crate, typeName).find((t) => t.kind !== "trait");
  if (!trait || !type) continue;
  (trait.implementedBy ??= []).includes(type.id) || trait.implementedBy.push(type.id);
  (type.implements ??= []).includes(trait.id) || type.implements.push(trait.id);
  implCount++;
}
// ---------- the whole product: Magican = the server plus every client around it ----------
const WORLD = JSON.parse(readFileSync(join(ROOT, "data", "world.json"), "utf8"));
const CLIENT_CODE = JSON.parse(readFileSync(join(ROOT, "data", "clients.json"), "utf8"));
const cStats = (t) => ({ files: t.files, lines: t.lines, types: t.types, fns: t.fns, tests: t.tests, structs: 0, enums: 0, traits: 0 });
function clientTree(prefix, children) {
  return Object.entries(children)
    .map(([name, t]) => {
      const kids = clientTree(`${prefix}/${name}`, t.children);
      return { id: `${prefix}/${name}`, name: kids.length ? `${name}/` : name, kind: kids.length ? "module" : "file", path: t.file ?? `${prefix.split(":").pop()}/${name}`.replace(/^\//, ""), ...cStats(t), children: kids };
    })
    .sort((a, b) => b.lines - a.lines);
}
const clientInfo = Object.fromEntries(WORLD.clients.map((c) => [c.id, c]));
const DEVICE = { "skills-hub": "rack", codegraph: "graph", desktop: "laptop", "web-ui": "browser", ios: "phone", android: "phone", esp32: "board", kindle: "ereader", "chrome-extension": "puzzle", "apps-sdk": "kit", "setup-wizard": "kit", helpers: "helpers" };
// placement around the server case (world units; y here is depth toward the viewer)
const CLIENT_PLAN = {
  // left side, front row and right side: never behind the server case from the camera
  desktop: { x: -318, y: -180, w: 120, d: 86 },
  helpers: { x: -300, y: -74, w: 72, d: 62 },
  "chrome-extension": { x: -292, y: 8, w: 56, d: 52 },
  "web-ui": { x: -300, y: 132, w: 150, d: 96 },
  ios: { x: -120, y: 134, w: 52, d: 92 },
  android: { x: -50, y: 134, w: 52, d: 92 },
  esp32: { x: 30, y: 150, w: 72, d: 62 },
  "setup-wizard": { x: 200, y: -170, w: 60, d: 52 },
  "apps-sdk": { x: 200, y: -92, w: 60, d: 52 },
  kindle: { x: 200, y: -12, w: 62, d: 84 },
  "skills-hub": { x: 124, y: 132, w: 96, d: 92 },
  codegraph: { x: 272, y: -170, w: 56, d: 52 },
};
const plannedH = (lines) => 10 + 9 * Math.log10(lines / 1000 + 1);
function clientNode(id) {
  const info = clientInfo[id];
  const code = CLIENT_CODE[id];
  return {
    id: `client:${id}`,
    name: info.name.replace(/\s*\(.*\)$/, ""),
    kind: "client",
    device: DEVICE[id] ?? "kit",
    role: info.role,
    languages: Object.keys(code.languages),
    connects: info.connects,
    ...cStats(code),
    plan: { ...CLIENT_PLAN[id], h: plannedH(code.lines) },
    children: clientTree(`client:${id}:`, code.children),
  };
}
const helperIds = ["macos-audio-engine", "macos-speech-helper", "macos-meet-audio"];
const helpers = helperIds.map(clientNode).map((n) => ({ ...n, plan: undefined }));
const helperGroup = {
  id: "client:helpers",
  name: "macOS helpers",
  kind: "client",
  device: "helpers",
  role: "small Mac programs the server and the desktop app start for on-device speech, voices and meeting audio",
  ...["files", "lines", "types", "fns", "tests"].reduce((acc, k) => ({ ...acc, [k]: helpers.reduce((s, h) => s + h[k], 0) }), { structs: 0, enums: 0, traits: 0 }),
  plan: { ...CLIENT_PLAN.helpers, h: plannedH(helpers.reduce((s, h) => s + h.lines, 0)) },
  children: helpers,
};
const clientIds = ["desktop", "web-ui", "ios", "android", "esp32", "kindle", "chrome-extension", "apps-sdk", "setup-wizard", "skills-hub", "codegraph"];
const clients = [...clientIds.map(clientNode), helperGroup];
// channel bots live inside the server's bots unit: each daemon holds its own source
const botCode = CLIENT_CODE["channel-bots"].children;
const botsUnit = server.children.find((c) => c.id === "p:bots");
for (const d of botsUnit.children) {
  const name = d.id.replace("bot:", "");
  const src = botCode[`skillshub/bots/${name}/src`];
  if (src) Object.assign(d, cStats(src), { children: clientTree(`client:bots:${name}`, src.children), path: `skillshub/bots/${name}/src` });
}
const sdk = botCode["skillshub/bots/sdk/src"];
if (sdk) botsUnit.children.push({ id: "bot:sdk", name: "bot sdk", kind: "module", path: "skillshub/bots/sdk/src", ...cStats(sdk), children: clientTree("client:bots:sdk", sdk.children) });
// what talks to the server, and how (from the clients research)
const via = (id) => (clientInfo[id]?.connects ?? []).filter((c) => /magician|magicutor/.test(c.to)).map((c) => `${c.protocol} ${c.route}`.slice(0, 90));
// The optional fifth entry names where a cable really ends inside a box (child ids,
// outermost first), so it continues over the rim when that box is open: the HTTP
// API lives in the magician-api crate of the magician process.
const API = { b: ["p:magician", "magician-api"] };
const WORLD_LINKS = [
  ...["desktop", "web-ui", "ios", "android", "esp32", "apps-sdk"].map((id) => [`client:${id}`, "server", "data", via(id).slice(0, 2).join(" · ") || "HTTP", API]),
  ["client:chrome-extension", "server", "data", "WebSocket native bridge ws://127.0.0.1:3003/bridge/native", { b: ["p:magicutor"] }],
  ["client:chrome-extension", "server", "data", "HTTP REST :3002 /api/magician/v2 and /health", API],
  ["server", "client:desktop", "host", "host gateway :3017 (screen, AppleScript, speech, iMessage)", { a: ["p:magician"] }],
  ["server", "client:helpers", "power", "spawned on demand: on-device speech and voices", { a: ["p:magician"] }],
  ["server", "client:skills-hub", "power", "installs skills per scope and runs them as governed tool processes", { a: ["p:magician"] }],
  ["client:kindle", "server", "planned", "design only: paired-device HTTPS", API],
];
const sum = (k) => server[k] + clients.reduce((s, c) => s + (c[k] ?? 0), 0);
server.plan = { x: -157, y: -102, w: 314, d: 204, h: 94 };
const tree = {
  id: "root",
  name: "Magican",
  kind: "system",
  role: "the whole product: the Magician server and every app and device that talks to it",
  ...Object.fromEntries(STATS.map((k) => [k, sum(k)])),
  types: clients.reduce((s, c) => s + (c.types ?? 0), 0),
  links: WORLD_LINKS,
  children: [server, ...clients],
};
const count = (n) => 1 + n.children.reduce((s, c) => s + count(c), 0);
const VOICE = process.env.VOICE_PROFILE || "84d4ac28-d249-4011-8244-b2368a477564";
const { graph, features, warn } = buildTours({ tree, crates: C, here: HERE, voice: VOICE });
for (const w of warn) console.warn("tour:", w);
// the star mascot for the Magican lid (docs/assets/brand: light version on light lids, gold companion on dark)
const svgData = (f) => `data:image/svg+xml;base64,${readFileSync(join(ROOT, "..", "..", "docs", "assets", "brand", f)).toString("base64")}`;
const brand = { mascotLight: svgData("mascot-hi-light.svg"), mascotDark: svgData("mascot-hi-dark.svg") };
// WHAT EACH BOX WAS BUILT FROM. One pass over git history gives every tracked
// file its newest commit (hash + time). Every box then carries the newest
// commit among the files under it (`lc`, an index into `changes`), and `dirty`
// when any of those files has uncommitted edits — so the hover card can say
// when that part of the code last moved, and whether this build includes work
// not yet committed. Crates also carry their Cargo version.
const REPO = join(ROOT, "..", "..");
const gitText = (args) => execFileSync("git", args, { cwd: REPO, maxBuffer: 256 * 1024 * 1024 }).toString();
const lastChange = new Map(); // repo-relative file → [hash, iso time]
{
  let current = null;
  for (const line of gitText(["log", "--format=@%h%x09%cI", "--name-only", "--no-renames", "HEAD"]).split("\n")) {
    if (line.startsWith("@")) current = line.slice(1).split("\t");
    else if (line && current && !lastChange.has(line)) lastChange.set(line, current);
  }
}
const dirtyFiles = new Set(
  gitText(["status", "--porcelain", "--untracked-files=all"])
    .split("\n")
    .filter(Boolean)
    .map((l) => l.slice(3).replace(/^.* -> /, "")),
);
const changes = [];
const changeIndex = new Map();
const rank = (entry) => {
  if (!changeIndex.has(entry[0])) (changeIndex.set(entry[0], changes.length), changes.push(entry));
  return changeIndex.get(entry[0]);
};
const newer = (a, b) => (a === null ? b : b === null ? a : changes[a][1] >= changes[b][1] ? a : b);
function stamp(node) {
  const file = node.path && !node.path.endsWith("/") ? node.path.replace(/:\d+$/, "") : null;
  let best = file && lastChange.has(file) ? rank(lastChange.get(file)) : null;
  let dirty = Boolean(file && dirtyFiles.has(file));
  for (const child of node.children) {
    const [b, d] = stamp(child);
    best = newer(best, b);
    dirty ||= d;
  }
  if (best !== null) node.lc = best;
  if (dirty) node.dirty = true;
  return [best, dirty];
}
stamp(tree);
for (const n of (function walk(x, out = []) { out.push(x); x.children.forEach((c) => walk(c, out)); return out; })(tree)) if (n.kind === "crate" && C[n.name]) n.version = C[n.name].version;
const builtAt = new Date().toISOString();
const builtFrom = gitText(["rev-parse", "--short", "HEAD"]).trim() + (dirtyFiles.size ? " + uncommitted changes" : "");

const DATA = { brand, commit: TRUTH.commit, generatedAt: TRUTH.generatedAt, builtAt, builtFrom, changes, totals: TRUTH.totals, tree, graph, features };

const css = readFileSync(join(HERE, "style.css"), "utf8");
const app = `${createAmbientBackground.toString()}\n${readFileSync(join(HERE, "app.js"), "utf8")}`;
const homeMascots = ["light", "dark"].map(tone => {
  const image = readFileSync(join(ROOT, `../../ui/unified-ui/static/brand/star-${tone}.png`)).toString("base64");
  return `<img class="home-${tone}" src="data:image/png;base64,${image}" width="36" height="36" alt="">`;
}).join("");
const searchIcon = '<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true"><circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 4.5 4.5"/></svg>';
const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<meta name="theme-color" content="#f4f4f1">
${metadataHead({ title: site.explain.title, description: site.explain.description, canonical: site.explain.origin + "/" })}
<link rel="icon" href="/favicon.svg" type="image/svg+xml">
<style>${css}</style>
<script type="importmap">{"imports":{"three":"https://cdn.jsdelivr.net/npm/three@0.160.0/build/three.module.js","three/addons/":"https://cdn.jsdelivr.net/npm/three@0.160.0/examples/jsm/"}}</script>
</head>
<body data-skin="paper">
<noscript><main class="text-fallback"><h1>How Magican works</h1><p>${site.explain.description}</p><p>The interactive model needs JavaScript and WebGL. <a href="/overview">Read the architecture and feature directory without JavaScript</a>, or <a href="${site.origin}/">visit Magican</a>.</p></main></noscript>
<div id="stage" aria-label="Magician anatomy: a 3D model of the system you can open level by level"></div>
<div class="splash" id="splash"><b>Magican</b><span>opening the box…</span></div>
<header class="bar">
  <a class="site-home" href="https://magican.ai" aria-label="Back to Magican" title="Back to Magican">${homeMascots}</a>
  <div class="crumbs" id="crumbs"></div>
  <a class="github" href="https://github.com/MagicBeansAI/MagicianAI" target="_blank" rel="noopener noreferrer" aria-label="Magican on GitHub (opens in a new tab)" title="Magican on GitHub"><svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" aria-hidden="true" fill="currentColor" viewBox="0 0 16 16"><path d="M6.766 11.328c-2.063-.25-3.516-1.734-3.516-3.656 0-.781.281-1.625.75-2.188-.203-.515-.172-1.609.063-2.062.625-.078 1.468.25 1.968.703.594-.187 1.219-.281 1.985-.281.765 0 1.39.094 1.953.265.484-.437 1.344-.765 1.969-.687.218.422.25 1.515.046 2.047.5.593.766 1.39.766 2.203 0 1.922-1.453 3.375-3.547 3.64.531.344.89 1.094.89 1.954v1.625c0 .468.391.734.86.547C13.781 14.359 16 11.53 16 8.03 16 3.61 12.406 0 7.984 0 3.563 0 0 3.61 0 8.031a7.88 7.88 0 0 0 5.172 7.422c.422.156.828-.125.828-.547v-1.25c-.219.094-.5.156-.75.156-1.031 0-1.64-.562-2.078-1.609-.172-.422-.36-.672-.719-.719-.187-.015-.25-.093-.25-.187 0-.188.313-.328.625-.328.453 0 .844.281 1.25.86.313.452.64.655 1.031.655s.641-.14 1-.5c.266-.265.47-.5.657-.656"/></svg></a>
  <button id="searchToggle" class="search-toggle" type="button" aria-label="Search code" title="Search code (/)" aria-expanded="false" aria-controls="headerSearch">${searchIcon}</button>
  <button id="menu" class="menu" type="button" aria-expanded="false" aria-controls="tools">Menu</button>
  <div class="tools" id="tools">
    <a class="overview-link" href="/overview">Overview</a>
    <button id="explore" type="button" aria-haspopup="dialog" aria-expanded="false" aria-controls="exploreMenu">Explore <span aria-hidden="true">▾</span></button>
    <div id="exploreMenu" class="explore-menu" role="dialog" aria-label="Explore Magican" hidden>
      <div class="browse-tabs" role="tablist" aria-label="Browse showcase">
        <button id="features" type="button" role="tab" aria-selected="true" aria-controls="featureList" data-browse="features">Features</button>
        <button id="tour" type="button" role="tab" aria-selected="false" aria-controls="tours" tabindex="-1" data-browse="tour">Tour</button>
      </div>
      <div id="featureList" class="tours features" role="tabpanel" aria-labelledby="features"></div>
      <div id="tours" class="tours" role="tabpanel" aria-labelledby="tour" hidden></div>
    </div>
    <button id="mute" class="sound-toggle" type="button" title="Mute sound (M)" aria-label="Mute sound (M)" aria-pressed="false"></button>
    <button id="expand" type="button" title="Open every level down to crates and their top modules">Expand all</button>
    <button id="home" type="button" title="Reset the camera">Reset view</button>
    <select id="skin" aria-label="Skin"><option value="paper">Paper</option><option value="sorbet">Sorbet</option><option value="night">Night</option><option value="blueprint">Blueprint</option></select>
  </div>
  <div id="headerSearch" class="header-search" role="search" aria-label="Search the codebase" hidden>
    ${searchIcon}
    <input id="search" type="search" placeholder="Find crate, module or file…" aria-label="Find crate, module or file" aria-controls="results" autocomplete="off" spellcheck="false">
    <button id="searchCancel" type="button">Cancel</button>
    <div id="results" class="results" role="region" aria-label="Search results" hidden></div>
  </div>
</header>
<div class="info" id="panel" aria-live="polite"></div>
<div class="caption" id="caption" aria-live="polite"></div>
<div class="tip" id="tip" hidden></div>
<div class="wirekey" id="wirekey" hidden></div>
<footer class="hint" id="hint" data-commit="${TRUTH.commit}">Click a box to break it open · drag to orbit · right-drag or shift-drag to pan · scroll to zoom · Esc goes up a level · ? for keys · from source at ${TRUTH.commit}</footer>
<div class="help" id="help" hidden role="dialog" aria-label="Keyboard shortcuts"><b>Keys</b><dl><dt>Click</dt><dd>open a box, or close the focused one</dd><dt>Esc</dt><dd>up a level (stops a tour)</dd><dt>Backspace</dt><dd>close the focused box</dd><dt>Home</dt><dd>seal everything</dd><dt>/</dt><dd>search</dd><dt>Space</dt><dd>pause or resume the tour</dd><dt>← →</dt><dd>previous step · next step (or previous/next feature)</dd><dt>1–9</dt><dd>pick a fork option</dd><dt>M</dt><dd>all sound on or off</dd><dt>V</dt><dd>narration voice on or off</dd><dt>B</dt><dd>background music on or off</dd><dt>?</dt><dd>this panel</dd></dl></div>
<script id="data" type="application/json">${JSON.stringify(DATA).replace(/</g, "\\u003c")}</script>
<script type="module">${app}</script>
</body>
</html>`;
writeFileSync(join(ROOT, "explorer.html"), html);
console.log(`wrote explorer.html · ${(html.length / 1024).toFixed(0)} KB · ${count(tree)} nodes · ${implCount} impl links`);
