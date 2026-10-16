// Records the explorer's guided tour to MP4, frame-exact: the page runs on a
// virtual clock (window.__explorer.advance), headless Chrome grabs each frame
// as JPEG and pipes it straight into ffmpeg, so no frames touch the disk.
// node explorer/record.mjs [--skin sorbet] [--fps 30] [--width 1920] [--height 1080] [--seconds N] [--audio file] [--out file.mp4]
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ANATOMY_HOME = process.env.ANATOMY_HOME || join(HERE, "..", "..", "..", "skillshub", "anatomy");
const { chromePort } = await import(pathToFileURL(join(ANATOMY_HOME, "scripts", "chrome-port.mjs")).href);

const arg = (name, fallback) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : fallback;
};
const skin = arg("skin", "sorbet");
const fps = Number(arg("fps", 30));
const width = Number(arg("width", 1920));
const height = Number(arg("height", 1080));
const audio = arg("audio", null);
const out = resolve(arg("out", join(HERE, "..", "media", `magician-tour-${skin}.mp4`)));
const chromeBin = process.env.CHROME_PATH || ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/Applications/Chromium.app/Contents/MacOS/Chromium", "/usr/bin/google-chrome", "/usr/bin/chromium"].find(existsSync);
if (!chromeBin) throw new Error("No Chrome found; set CHROME_PATH");

const profile = mkdtempSync(join(tmpdir(), "explorer-rec-"));
const chrome = spawn(chromeBin, ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "--no-first-run", "--hide-scrollbars", "--allow-file-access-from-files", "--enable-unsafe-swiftshader", "--ignore-gpu-blocklist", "about:blank"]);
const port = await chromePort(chrome, profile);
const page = await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: "PUT" })).json();
const socket = new WebSocket(page.webSocketDebuggerUrl);
let id = 0;
const pending = new Map();
socket.onmessage = (m) => {
  const data = JSON.parse(m.data);
  if (data.id && pending.has(data.id)) (pending.get(data.id)(data), pending.delete(data.id));
  else if (data.method === "Runtime.exceptionThrown") console.error("page exception:", data.params.exceptionDetails.exception?.description ?? data.params.exceptionDetails.text);
};
await new Promise((done) => (socket.onopen = done));
const send = (method, params = {}) => new Promise((done) => {
  const call = ++id;
  pending.set(call, done);
  socket.send(JSON.stringify({ id: call, method, params }));
});
const evaluate = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) throw new Error(r.result.exceptionDetails.exception?.description ?? r.result.exceptionDetails.text);
  return r.result?.result?.value;
};
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

await send("Runtime.enable");
await send("Page.enable");
await send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
const url = `${pathToFileURL(join(HERE, "..", "explorer.html")).href}?clean&skin=${skin}`;
await send("Page.navigate", { url });
for (let i = 0; i < 100 && !(await evaluate("Boolean(window.__explorer)")); i++) await sleep(100);
await sleep(800);
const length = Number(arg("seconds", 0)) * 1000 || (await evaluate("window.__explorer.TOUR_LENGTH"));
await evaluate("window.__explorer.record()");

const frames = Math.round((length / 1000) * fps);
const ffArgs = ["-y", "-loglevel", "error", "-f", "image2pipe", "-framerate", String(fps), "-c:v", "mjpeg", "-i", "-"];
if (audio) ffArgs.push("-i", audio, "-shortest", "-c:a", "aac", "-b:a", "192k");
ffArgs.push("-c:v", "libx264", "-preset", "slow", "-crf", "18", "-pix_fmt", "yuv420p", "-movflags", "+faststart", out);
const ff = spawn("ffmpeg", ffArgs, { stdio: ["pipe", "inherit", "inherit"] });
const started = Date.now();
for (let f = 0; f < frames; f++) {
  await evaluate(`window.__explorer.advance(${1000 / fps})`);
  const shot = await send("Page.captureScreenshot", { format: "jpeg", quality: 92, clip: { x: 0, y: 0, width, height, scale: 1 } });
  const buf = Buffer.from(shot.result.data, "base64");
  if (!ff.stdin.write(buf)) await new Promise((done) => ff.stdin.once("drain", done));
  if (f % fps === 0) process.stdout.write(`\r${(f / fps).toFixed(0)}s / ${(frames / fps).toFixed(0)}s · ${((Date.now() - started) / 1000).toFixed(0)}s elapsed`);
}
ff.stdin.end();
await new Promise((done) => ff.on("close", done));
chrome.kill("SIGKILL");
rmSync(profile, { recursive: true, force: true });
console.log(`\nwrote ${out}`);
process.exit(0);
