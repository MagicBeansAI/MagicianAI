// Voices the tour with the local Voicebox server (127.0.0.1:17493). One clip
// per spoken line, named after its step:
//   media/narration/<part>/<step id>--<title>.mp3
// Only new or changed lines are generated (the manifest keeps the exact spoken
// text and voice of every clip).
//
//   node explorer/narrate.mjs                    voice whatever is new or changed
//   node explorer/narrate.mjs --redo chat.engine voice again every line whose id starts with this (repeatable)
//   node explorer/narrate.mjs --prune            also delete clips no step uses any more
//   node explorer/narrate.mjs --list             print id → file → text and exit
//   VOICE_PROFILE=<voicebox profile id>          pick another voice
// Then rebuild: node explorer/build.mjs
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { buildTours, speakable } from "./tours.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const OUT = join(HERE, "..", "media", "narration");
const API = process.env.VOICEBOX_URL || "http://127.0.0.1:17493";
const VOICE = process.env.VOICE_PROFILE || "84d4ac28-d249-4011-8244-b2368a477564";
const args = process.argv.slice(2);
const redo = args.flatMap((a, i) => (a === "--redo" ? [args[i + 1]] : []));

const html = readFileSync(join(HERE, "..", "explorer.html"), "utf8");
const DATA = JSON.parse(html.match(/<script id="data" type="application\/json">([\s\S]*?)<\/script>/)[1]);
const TRUTH = JSON.parse(readFileSync(join(HERE, "..", "data", "magician.json"), "utf8"));
const { lines } = buildTours({ tree: DATA.tree, crates: TRUTH.crates, here: HERE, voice: VOICE });

mkdirSync(OUT, { recursive: true });
const manifestPath = join(OUT, "manifest.json");
const manifest = existsSync(manifestPath) ? JSON.parse(readFileSync(manifestPath, "utf8")) : {};
const save = () => writeFileSync(manifestPath, JSON.stringify(Object.fromEntries(Object.entries(manifest).sort()), null, 1));

if (args.includes("--list")) {
  for (const l of lines) console.log(`${l.id}\t${manifest[l.id] ? l.file : "(not voiced)"}\t${l.text}`);
  process.exit(0);
}
if (args.includes("--prune")) {
  const live = new Set(lines.map((l) => l.id));
  for (const [id, m] of Object.entries(manifest)) {
    if (live.has(id)) continue;
    rmSync(join(OUT, m.file), { force: true });
    delete manifest[id];
    console.log(`pruned ${m.file}`);
  }
  save();
}
const stale = (l) => {
  const m = manifest[l.id];
  return redo.some((p) => l.id.startsWith(p)) || !m || m.spoken !== speakable(l.text) || m.voice !== VOICE || !existsSync(join(OUT, m.file));
};
// most-travelled paths first, nerd-only detail last
const ORDER = ["entry", "chat", "llm", "decision", "task", "quality", "browser", "voice", "apps", "setup", "feature"];
const rank = (l) => (l.nerd ? 100 : 0) + (ORDER.indexOf(l.id.split(".")[0]) + 1 || 50);
const todo = lines.filter(stale).sort((a, b) => rank(a) - rank(b));
console.log(`${todo.length} clips to voice, ${lines.length - todo.length} up to date`);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function healthy() {
  try {
    return (await fetch(`${API}/health`, { signal: AbortSignal.timeout(4000) })).ok;
  } catch {
    return false;
  }
}
async function ensureServer() {
  if (await healthy()) return;
  console.log("voicebox is down; relaunching");
  try {
    execFileSync("open", ["-a", "Voicebox"]);
  } catch {}
  for (let t = 0; t < 80; t++) {
    await sleep(3000);
    if (await healthy()) return;
  }
  throw new Error("voicebox did not come back");
}
async function generate(text) {
  for (let attempt = 1; attempt <= 4; attempt++) {
    try {
      await ensureServer();
      const res = await fetch(`${API}/generate`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ profile_id: VOICE, text: speakable(text), language: "en" }) });
      if (!res.ok) throw new Error(`voicebox ${res.status}: ${await res.text()}`);
      const { id } = await res.json();
      for (let t = 0; t < 400; t++) {
        await sleep(1500);
        const h = await (await fetch(`${API}/history/${id}`)).json();
        if (h.status === "completed") return Buffer.from(await (await fetch(`${API}/history/${id}/export-audio`)).arrayBuffer());
        if (h.status === "failed" || h.error) throw new Error(`generation failed: ${h.error}`);
      }
      throw new Error("generation timed out");
    } catch (error) {
      console.log(`  attempt ${attempt} failed: ${error.cause?.code ?? error.message}`);
      await sleep(4000);
    }
  }
  throw new Error(`could not voice: ${text}`);
}

for (const [n, l] of todo.entries()) {
  const started = Date.now();
  const target = join(OUT, l.file);
  mkdirSync(dirname(target), { recursive: true });
  const wav = `${target}.wav`;
  writeFileSync(wav, await generate(l.text));
  execFileSync("ffmpeg", ["-loglevel", "error", "-y", "-i", wav, "-af", "silenceremove=start_periods=1:start_threshold=-50dB,loudnorm=I=-16:TP=-1.5", "-ac", "1", "-b:a", "96k", target]);
  rmSync(wav, { force: true });
  const old = manifest[l.id];
  if (old && old.file !== l.file) rmSync(join(OUT, old.file), { force: true });
  const ms = Math.round(Number(execFileSync("ffprobe", ["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", target]).toString()) * 1000);
  manifest[l.id] = { file: l.file, ms, text: l.text, spoken: speakable(l.text), voice: VOICE };
  save();
  console.log(`[${n + 1}/${todo.length}] ${l.file} · ${(ms / 1000).toFixed(1)}s in ${((Date.now() - started) / 1000).toFixed(0)}s`);
}
console.log("done");
