// Tour definitions: the overview (generated from the data, so every number it
// says is true) and the flows in flows/*.json (each step researched against
// the code, with a file:line citation). Steps are resolved to explorer nodes.
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const words = (n) => {
  // speakable round numbers: 2,420,844 → "two point four million"
  if (n >= 1e6) return `${(n / 1e6).toFixed(1).replace(/\.0$/, "")} million`;
  if (n >= 1e4) return `${Math.round(n / 1000)} thousand`;
  if (n >= 1000) return n.toLocaleString("en-US");
  return String(n);
};
// what the voice says: code names spelled the way a person would read them
const SAY = [
  [/\bmagicllm\b/gi, "magic L L M"],
  [/\bmagicutor\b/gi, "magic-utor"],
  [/\bLLMProvider\b/g, "L L M Provider"],
  [/\bLLM\b/g, "L L M"],
  [/\bmagician_v2\b/g, "magician v2"],
  [/\bdecide\.rs\b/g, "decide dot R S"],
  [/\bWebSockets?\b/g, (m) => m.replace("WebSocket", "web socket")],
  [/\bwebsocket\b/g, "web socket"],
  [/\bOS\b/g, "O S"],
  [/\bID\b/g, "I D"],
  [/\bAPI\b/g, "A P I"],
  [/\bHTTP\b/g, "H T T P"],
  [/\bUI\b/g, "U I"],
  [/\bV3\b/g, "version three"],
  [/\bmacOS\b/g, "mac O S"],
  [/\bbase64\b/g, "base sixty-four"],
];
export const speakable = (text) => SAY.reduce((t, [re, to]) => t.replace(re, to), text);
const slug = (t) => (t ?? "").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 48);
export const clipFile = (id, label) => `${id.split(".")[0]}/${id}${label ? `--${slug(label)}` : ""}.mp3`;
export const clipKey = (text, voice) => createHash("sha1").update(`${voice}\n${speakable(text)}`).digest("hex").slice(0, 16);

export function buildTours({ tree, crates, here, voice }) {
  const byId = new Map();
  (function walk(n) {
    byId.set(n.id, n);
    n.children.forEach(walk);
  })(tree);
  const warn = [];
  const crateOf = (file) =>
    Object.entries(crates)
      .filter(([, c]) => file.startsWith(`${c.path}/src/`))
      .sort((a, b) => b[1].path.length - a[1].path.length)[0]?.[0];
  function fileNode(file) {
    const crate = crateOf(file);
    if (!crate) return null;
    let parts = file.slice(crates[crate].path.length + 5).replace(/\.rs$/, "").split("/");
    if (parts.at(-1) === "mod") parts = parts.slice(0, -1);
    while (parts.length) {
      const n = byId.get(`${crate}::/${parts.join("/")}`);
      if (n) return n;
      parts = parts.slice(0, -1);
    }
    return byId.get(crate) ?? null;
  }
  function resolve(step) {
    if (!step.file) return step.process && step.process !== "external" ? byId.get(`p:${step.process === "decision-engine" ? "decision" : step.process}`) ?? null : null;
    const f = fileNode(step.file);
    if (!f) return warn.push(`unresolved file ${step.file}`), null;
    // a step about a function lands on its file; a step about a type (or a trait's method) on the type
    if (step.type) {
      const types = f.children.filter((c) => c.name === step.type && ["struct", "enum", "trait"].includes(c.kind));
      const t = types.sort((a, b) => Math.abs(a.line - step.line) - Math.abs(b.line - step.line))[0];
      if (t && (!step.fn || t.kind === "trait")) return t;
      if (t) return f;
      warn.push(`type ${step.type} not found in ${f.id}`);
    }
    return f;
  }

  // ---------- the graph: intro + parts joined through ports ----------
  const intro = {
    part: "entry",
    nodes: [
      { id: "entry.seal", kind: "step", level: "core", do: "seal", title: "Magican", say: `This is Magican: ${words(tree.lines)} lines of code across the Magician server and the apps and devices that talk to it, sealed in one box.`, next: "entry.open" },
      { id: "entry.open", kind: "step", level: "core", do: "open-root", title: "Break it open", say: "Open it, and the Magician server sits in the middle, with the desktop app, the web app, phones, an E S P 32 and a Kindle around it.", next: "entry.server" },
      { id: "entry.server", kind: "step", level: "core", do: "goto", node: "server", title: "The Magician server", say: `The server is a sealed case of its own: ${words(byId.get("server").lines)} lines of Rust in ${Object.keys(crates).length} crates, running as seven processes.`, next: "entry.choose" },
      {
        id: "entry.choose",
        kind: "choice",
        level: "core",
        title: "Where requests come from",
        say: "Every path through Magician starts somewhere: a chat message, a task, a voice call, an app, or the way it is deployed. Pick where to begin.",
        options: [
          { label: "A chat message", say: "Someone types to Magician in the app or through a channel bot.", default: true, next: "chat.start" },
          { label: "A task", say: "Work handed to an agent: from the task A P I, a schedule, or a chat delegation.", next: "task.start" },
          { label: "A voice call", say: "Someone talks to Magician live, from the desktop tray or a device.", next: "voice.start" },
          { label: "An app", say: "An installed Magician app is set up or runs one of its actions.", next: "apps.start" },
          { label: "How it is deployed", say: "Where each piece runs: one Mac, a container, a Linux server, phones over a tunnel.", next: "setup.choose" },
        ],
      },
    ],
  };
  // ---------- setups: generated from the deployment research (data/world.json) ----------
  const world = JSON.parse(readFileSync(join(here, "..", "data", "world.json"), "utf8"));
  const entity = (name) => {
    const t = name.toLowerCase();
    if (/chrome|extension/.test(t)) return "client:chrome-extension";
    if (/desktop|host gateway|tray|gateway/.test(t)) return "client:desktop";
    if (/esp32/.test(t)) return "client:esp32";
    if (/phone|ios|iphone/.test(t)) return "client:ios";
    if (/android/.test(t)) return "client:android";
    if (/browser|unified-ui|\bui\b|vite/.test(t)) return "client:web-ui";
    if (/kapso|bot/.test(t)) return "p:bots";
    if (/ollama/.test(t)) return "p:ollama";
    if (/magicutor/.test(t)) return "p:magicutor";
    if (/magician|engine|backend|container|server/.test(t)) return "p:magician";
    return null;
  };
  const sentence = (text, max = 2) => (text.match(/[^.!?]+[.!?]+/g) ?? [text]).slice(0, max).join(" ").trim();
  const nameOf = (id) => byId.get(id)?.name ?? id;
  const setupNodes = [
    {
      id: "setup.choose",
      kind: "choice",
      level: "core",
      title: "Ways to deploy",
      say: "Magican runs in several shapes. Pick one to see where every piece lives and how they reach each other.",
      options: world.setups.map((st, i) => ({ label: st.name.replace(/\s*\(.*?\)/g, ""), say: sentence(st.summary, 1), default: i === 0, next: `setup.${st.id}` })),
    },
  ];
  for (const st of world.setups) {
    const paths = (st.paths ?? []).map((p) => ({ ...p, a: entity(p.from), b: entity(p.to) })).filter((p) => p.a && p.b);
    const placement = Object.entries(st.placement ?? {}).map(([k, v]) => `${k}: ${v}`).join(" · ");
    setupNodes.push({ id: `setup.${st.id}`, kind: "step", level: "core", do: "goto", node: "root", title: st.name, say: sentence(st.summary, 2), evidence: placement.slice(0, 400), next: paths.length ? `setup.${st.id}.0` : `setup.${st.id}.end` });
    paths.forEach((p, i) => {
      setupNodes.push({
        id: `setup.${st.id}.${i}`,
        kind: "step",
        level: "core",
        do: "goto",
        node: p.b,
        from: p.a,
        title: `${p.from} → ${p.to}`,
        // a route may say in plain words what travels on it; otherwise name the hop
        say: p.say ?? `${nameOf(p.a)} reaches ${nameOf(p.b)}: ${p.via}.`,
        spoken: p.say ?? `${nameOf(p.a)} reaches ${nameOf(p.b)} here.`,
        evidence: p.evidence,
        next: i + 1 < paths.length ? `setup.${st.id}.${i + 1}` : `setup.${st.id}.end`,
      });
    });
    setupNodes.push({ id: `setup.${st.id}.end`, kind: "end", title: st.name, say: `The desktop app's job here: ${sentence(st.desktopRole ?? "", 1)}` });
  }
  // ---------- features: data/features-*.json ----------
  const dataDir = join(here, "..", "data");
  const FEATURES = readdirSync(dataDir)
    .filter((f) => /^features-.*\.json$/.test(f))
    .flatMap((f) => JSON.parse(readFileSync(join(dataDir, f), "utf8")).features ?? []);
  const clientFile = (file) => [...byId.values()].find((n) => n.kind === "file" && n.path === file);
  const locate = (loc) => (loc.file?.includes("/src/") && crateOf(loc.file) ? resolve({ ...loc, fn: loc.fn }) : clientFile(loc.file)) ?? null;
  const featureNodes = [];
  const features = FEATURES.map((f) => {
    const locations = (f.locations ?? []).map((l) => ({ node: locate(l)?.id ?? null, why: l.why, file: l.file, line: l.line })).filter((l) => l.node);
    const steps = (f.tour ?? []).map((t) => ({ ...t, n: locate(t) })).filter((t) => t.n);
    steps.forEach((t, i) => featureNodes.push({ id: `feature.${f.id}.${i}`, kind: "step", level: "core", chapter: f.name, node: t.n.id, fn: t.fn ?? null, title: t.title, say: t.say, evidence: t.evidence, next: i + 1 < steps.length ? `feature.${f.id}.${i + 1}` : `feature.${f.id}.end` }));
    featureNodes.push({ id: `feature.${f.id}.end`, kind: "end", title: f.name, say: f.oneLine });
    const missing = (f.locations ?? []).length - locations.length;
    if (missing) warn.push(`feature ${f.id}: ${missing} location(s) not in the model`);
    return { id: f.id, name: f.name, group: f.group, oneLine: f.oneLine, maturity: f.maturity, locations, start: steps.length ? `feature.${f.id}.0` : null, configKeys: f.configKeys ?? [], routes: f.routes ?? [], uiSurface: f.uiSurface ?? null };
  });
  const dir = join(here, "graph");
  const parts = [intro, { part: "setup", nodes: setupNodes }, { part: "feature", nodes: featureNodes }, ...(existsSync(dir) ? readdirSync(dir).filter((f) => f.endsWith(".json")).map((f) => JSON.parse(readFileSync(join(dir, f), "utf8"))) : [])];
  const nodes = {};
  for (const part of parts) for (const n of part.nodes ?? []) nodes[n.id] = { ...n, part: part.part };
  // resolve steps to explorer nodes
  for (const n of Object.values(nodes)) {
    if (n.kind === "step" && !n.do && !n.node) {
      const target = resolve(n);
      if (target) n.node = target.id;
      else warn.push(`step ${n.id}: no model node for ${n.file ?? n.process}`);
    }
  }
  // every jump must land somewhere real
  const PORTS = ["chat.start", "task.start", "quality.start", "voice.start", "apps.start", "llm.start", "decision.start", "browser.start"];
  for (const n of Object.values(nodes)) {
    const targets = [n.next, n.target, ...(n.options ?? []).map((o) => o.next)].filter(Boolean);
    for (const t of targets) if (!nodes[t]) warn.push(`${n.id} → ${t} does not exist${PORTS.includes(t) ? " (port not mapped yet)" : ""}`);
    if (n.kind === "choice" && (n.options ?? []).filter((o) => o.default).length !== 1) warn.push(`${n.id}: needs exactly one default option`);
  }
  // narration clips: one readable file per spoken line, named after its step
  //   media/narration/<part>/<step id>--<title>.mp3   (options: <step id>.option-<n>--<label>.mp3)
  const manifestPath = join(here, "..", "media", "narration", "manifest.json");
  const manifest = existsSync(manifestPath) ? JSON.parse(readFileSync(manifestPath, "utf8")) : {};
  const lines = [];
  const voiceIt = (holder, id, label) => {
    const text = holder.spoken ?? holder.say;
    if (!text) return;
    const file = clipFile(id, label);
    lines.push({ id, text, file, nerd: holder.level === "nerd" });
    const m = manifest[id];
    if (m && m.spoken === speakable(text) && m.voice === voice) Object.assign(holder, { audio: `narration/${m.file}`, ms: m.ms });
  };
  for (const n of Object.values(nodes)) {
    voiceIt(n, n.id, n.title);
    (n.options ?? []).forEach((o, i) => voiceIt(o, `${n.id}.option-${i + 1}`, o.label));
  }
  return { graph: { nodes, start: "entry.seal", entryChoice: "entry.choose" }, features, lines, warn };
}
