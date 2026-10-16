// Assemble the deployable site in showcase/anatomy/dist/ (gitignored):
//   - the explorer as the front page (index.html; /explorer and the old page
//     names redirect to /)
//   - media/ (music bed and narration clips the manifest names)
//   - three.js and the addons the explorer imports, vendored under vendor/,
//     with the explorer's import map pointed at them (no third-party CDN at
//     runtime; jsdelivr is only read here, once, and cached in .cache/)
//   - _headers for Cloudflare Pages: long cache for vendor/, a day for media,
//     short for pages
// node explorer/site.mjs [--no-media] [--no-narration]
//   --no-media       no audio at all (tours still run, captions only)
//   --no-narration   the music bed but no voice clips
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { writePublicAssets, markPreview } from "../../../scripts/public-site-assets.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const OUT = join(ROOT, "dist");
const CACHE = join(ROOT, ".cache", "vendor");
const THREE = "three@0.160.0";
const CDN = `https://cdn.jsdelivr.net/npm/${THREE}/`;
const withMedia = !process.argv.includes("--no-media");
const withVoice = withMedia && !process.argv.includes("--no-narration");

rmSync(OUT, { recursive: true, force: true });
mkdirSync(OUT, { recursive: true });


// three.js: fetch every module the explorer reaches (relative imports followed), cached
async function fetchModule(path) {
  const cached = join(CACHE, THREE, path);
  if (existsSync(cached)) return readFileSync(cached, "utf8");
  const res = await fetch(CDN + path);
  if (!res.ok) throw new Error(`${res.status} fetching ${CDN + path}`);
  const text = await res.text();
  mkdirSync(dirname(cached), { recursive: true });
  writeFileSync(cached, text);
  return text;
}
const explorer = readFileSync(join(ROOT, "explorer.html"), "utf8");
const entry = ["build/three.module.js", ...[...explorer.matchAll(/from\s+"three\/addons\/([^"]+)"/g)].map((m) => `examples/jsm/${m[1]}`)];
const seen = new Set();
const queue = [...entry];
while (queue.length) {
  const path = queue.shift();
  if (seen.has(path)) continue;
  seen.add(path);
  const text = await fetchModule(path);
  mkdirSync(dirname(join(OUT, "vendor", THREE, path)), { recursive: true });
  writeFileSync(join(OUT, "vendor", THREE, path), text);
  for (const m of text.matchAll(/(?:from|import)\s*["'](\.{1,2}\/[^"']+)["']/g)) queue.push(join(dirname(path), m[1]).replace(/\\/g, "/"));
}
const map = `{"imports":{"three":"./vendor/${THREE}/build/three.module.js","three/addons/":"./vendor/${THREE}/examples/jsm/"}}`;
const rewritten = explorer.replace(/<script type="importmap">[^<]*<\/script>/, `<script type="importmap">${map}</script>\n<link rel="modulepreload" href="./vendor/${THREE}/build/three.module.js">`);
if (rewritten === explorer) throw new Error("import map not found in explorer.html");
writeFileSync(join(OUT, "index.html"), rewritten);
// links shared before the explorer became the front page keep working (query kept)
writeFileSync(
  join(OUT, "_redirects"),
  ["/explorer", "/explorer.html", "/index.html", "/01-case", "/01-case-light", "/02-magician", "/02-magician-light"].map((from) => `${from} / 301`).join("\n") + "\n",
);

// media: the music bed and only the clips the manifest names
let clips = 0;
if (withMedia) {
  const media = join(ROOT, "media");
  if (existsSync(join(media, "tour-music.m4a"))) cpSync(join(media, "tour-music.m4a"), join(OUT, "media", "tour-music.m4a"));
  const manifest = join(media, "narration", "manifest.json");
  if (withVoice && existsSync(manifest)) {
    for (const { file } of Object.values(JSON.parse(readFileSync(manifest, "utf8")))) {
      const src = join(media, "narration", file);
      if (!existsSync(src) || readFileSync(src).subarray(0, 40).toString().startsWith("version https://git-lfs")) continue; // an un-fetched LFS pointer is not audio
      cpSync(src, join(OUT, "media", "narration", file));
      clips++;
    }
  }
}

writeFileSync(
  join(OUT, "_headers"),
  `/vendor/*
  Cache-Control: public, max-age=31536000, immutable
/media/*
  Cache-Control: public, max-age=86400
/
  Cache-Control: public, max-age=300
`,
);
console.log(`wrote dist/ · ${seen.size} three.js modules · ${clips} narration clips${withMedia ? (withVoice ? "" : " (narration skipped)") : " (media skipped)"}`);

writePublicAssets(OUT, "explain");
if (process.argv.includes("--preview")) markPreview(OUT);
