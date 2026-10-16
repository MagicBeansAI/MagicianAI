// Magician anatomy explorer: one canvas, the whole system, opened in place.
// World convention: three's X right, Y up, Z toward the viewer (the anatomy
// figures' x, z, y). Boxes are { x, z, w, d, y, h } with x/z the min corner.
// Every visual is built at true size inside a group whose origin is the box's
// min corner, so a rise animation is just group.scale.y.
import * as THREE from "three";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { CSS2DRenderer, CSS2DObject } from "three/addons/renderers/CSS2DRenderer.js";
import { RoundedBoxGeometry } from "three/addons/geometries/RoundedBoxGeometry.js";
import { Line2 } from "three/addons/lines/Line2.js";
import { LineMaterial } from "three/addons/lines/LineMaterial.js";
import { LineGeometry } from "three/addons/lines/LineGeometry.js";

const DATA = JSON.parse(document.getElementById("data").textContent);
const stage = document.getElementById("stage");
const panel = document.getElementById("panel");
const crumbs = document.getElementById("crumbs");
const caption = document.getElementById("caption");
const still = matchMedia("(prefers-reduced-motion: reduce)");
// Phones and small tablets: touch first, a compact layout, and a lighter
// renderer (lower pixel density, fewer segments, no idle cable beads).
const coarse = matchMedia("(pointer: coarse)");
const LITE = coarse.matches || Math.min(screen.width, screen.height) < 600;
// compact: narrow screens, short screens (a phone on its side), or any touch screen whose short side is phone-sized
const isCompact = () => innerWidth < 760 || innerHeight < 500 || (coarse.matches && Math.min(innerWidth, innerHeight) < 600);
const applyCompact = () => document.body.classList.toggle("compact", isCompact());
applyCompact();
const QUERY = new URLSearchParams(location.search);
// Draw only while something changes. An idle page stops rendering, which
// matters on phones (battery, heat); any input, tween, tour or animation
// wakes it.
var awakeUntil = 0;
function poke(ms = 1200) {
  awakeUntil = Math.max(awakeUntil, performance.now() + ms);
}
for (const type of ["pointerdown", "pointermove", "wheel", "keydown", "touchstart"]) window.addEventListener(type, () => poke(), { passive: true, capture: true });
const fmt = (n) => Math.round(n).toLocaleString("en-US");

// ---------- clock: real time, or virtual time while recording ----------
let virtual = null;
const now = () => virtual ?? performance.now();
const timers = [];
const after = (ms, fn) => timers.push({ at: now() + ms, fn });
function runTimers() {
  const t = now();
  const due = timers.filter((x) => x.at <= t).sort((a, b) => a.at - b.at);
  for (const x of due) timers.splice(timers.indexOf(x), 1);
  for (const x of due) x.fn();
}

// ---------- skins ----------
const SKINS = {
  paper: { halo: 0x5a5a50, haloAlpha: 0.22, bg: 0xf4f4f1, solid: 0xfbfbfa, floor: 0xeeeeea, detail: 0xd9d9d4, ink: 0x26262a, ghost: 0x8a8a8f, accent: 0x111111, link: 0x5a5a60, power: 0x26262a, led: 0x30a46c, inbound: 0xb7791f, packet: 0x30a46c, packetHot: 0xffffff, tint: false },
  // Sorbet: the app's default theme (warm pastel — coral, lilac, mint), from ui/unified-ui/src/app.css
  sorbet: { halo: 0x6a4a50, haloAlpha: 0.18, bg: 0xfff9f4, solid: 0xffffff, floor: 0xfff0ea, detail: 0xf1e4dc, ink: 0x2b2230, ghost: 0xb3a8b8, accent: 0xe8505f, link: 0x2f9e78, power: 0xf2b53a, led: 0x2f9e78, inbound: 0x7a5cf0, packet: 0xffffff, packetHot: 0xffffff, tint: true },
  night: { halo: 0x000000, haloAlpha: 0.6, bg: 0x0a0a0f, solid: 0x1d1d30, floor: 0x14141f, detail: 0x2a2a42, ink: 0xd8dae6, ghost: 0x4a4b62, accent: 0x00f5c4, link: 0x00d4aa, power: 0xff6b6b, led: 0x00f5c4, inbound: 0xffb547, packet: 0xffffff, packetHot: 0xffffff, tint: true },
  blueprint: { halo: 0x020a18, haloAlpha: 0.5, bg: 0x0b2340, solid: 0x15375e, floor: 0x10304f, detail: 0x1d4776, ink: 0xe3efff, ghost: 0x4b74a6, accent: 0xffffff, link: 0x8fd3ff, power: 0xffd27a, led: 0x8fd3ff, inbound: 0xff9e80, packet: 0xffffff, packetHot: 0x0b2340, tint: false },
};
const TINTS = { "skills-hub": 0x9b7bd4, codegraph: 0x6aa6a0, desktop: 0x5b8def, "web-ui": 0x4ecdc4, ios: 0x9aa5b1, android: 0x7fb069, esp32: 0xfc9838, kindle: 0xc9b38a, "chrome-extension": 0xffc93c, helpers: 0xbda1f0, "apps-sdk": 0xff9f7f, "setup-wizard": 0xa8a8a8, magician: 0xff6b6b, magicutor: 0x4ecdc4, decision: 0xbda1f0, supervisor: 0xffc93c, config: 0xfc9838, ollama: 0x7fb069, bots: 0x5b8def };
let skin = SKINS.paper;

// ---------- three setup ----------
// the drawing buffer is kept only when a recorder reads frames back (?clean)
const RECORDING = QUERY.has("clean") || QUERY.has("record");
const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: !RECORDING, preserveDrawingBuffer: RECORDING });
renderer.setPixelRatio(Math.min(window.devicePixelRatio, LITE ? 1.5 : 2));
stage.appendChild(renderer.domElement);
const labels = new CSS2DRenderer();
labels.domElement.className = "labels";
stage.appendChild(labels.domElement);
// Factory is embedded by build.mjs so explorer.html stays self-contained.
const ambient = createAmbientBackground({ THREE,stage,still,lite:LITE,disabled:RECORDING });
const scene = new THREE.Scene();
// A long lens, not a parallel projection: an orthographic camera draws a box's
// far edge exactly as long as its near one, which the eye reads as longer, so
// lids look like rhombuses. 18° keeps the drawing flat but lets far edges
// shrink a little. ?ortho brings the parallel projection back.
const ORTHO = QUERY.has("ortho");
const LENS = 18;
const TAN_HALF = Math.tan((LENS * Math.PI) / 360);
const camera = ORTHO ? new THREE.OrthographicCamera(-1, 1, 1, -1, -20000, 20000) : new THREE.PerspectiveCamera(LENS, 1, 0.1, 50000);
let AZ = (55 * Math.PI) / 180;
let EL = (30 * Math.PI) / 180;
const VIEW = new THREE.Vector3(Math.cos(EL) * Math.cos(AZ), Math.sin(EL), Math.cos(EL) * Math.sin(AZ));
camera.position.copy(VIEW).multiplyScalar(1000);
camera.lookAt(0, 0, 0);
let relabelTimer = 0;
const controls = new OrbitControls(camera, renderer.domElement);
controls.enableDamping = true;
controls.dampingFactor = 0.12;
controls.screenSpacePanning = true;
controls.minZoom = 0.2;
controls.maxZoom = 6000;
// The view's scale: pixels per world unit at the orbit target. For the
// orthographic camera that is its zoom; the perspective camera gets the same
// scale from its distance, so framing works the same either way.
const distFor = (scale) => stage.clientHeight / (2 * scale * TAN_HALF);
function viewScale() {
  return ORTHO ? camera.zoom : stage.clientHeight / (2 * Math.max(1e-6, camera.position.distanceTo(controls.target)) * TAN_HALF);
}
// put the camera on `dir` from `target` at `scale`
function aim(target, dir, scale) {
  controls.target.copy(target);
  camera.position.copy(target).add(dir.clone().normalize().multiplyScalar(ORTHO ? 1000 : distFor(scale)));
  if (ORTHO) camera.zoom = scale;
  fitDepth();
  camera.updateProjectionMatrix();
}
// near and far follow the distance, so close-ups keep their depth precision
function fitDepth() {
  if (ORTHO) return;
  const d = camera.position.distanceTo(controls.target);
  camera.near = Math.max(0.005, d * 0.05);
  camera.far = d + 4000;
}
controls.maxPolarAngle = Math.PI * 0.49;
controls.zoomToCursor = true;
controls.addEventListener("change", () => {
  clearTimeout(relabelTimer);
  relabelTimer = setTimeout(() => updateLabels(), 90);
});
// Calibrated so the three visible faces land on Anatomy's paper tones
// (top ~100%, front ~93%, side ~84% in sRGB). three's lights are linear and
// divided by pi, hence the factors.
scene.add(new THREE.AmbientLight(0xffffff, 0.41 * Math.PI));
const sun = new THREE.DirectionalLight(0xffffff, 0.59 * Math.PI);
sun.position.set(-0.3, 1, 0.62);
const LIGHT = sun.position.clone();
const UP = new THREE.Vector3(0, 1, 0);
scene.add(sun);
const world = new THREE.Group();
scene.add(world);

let portrait = null;
function resize() {
  applyCompact();
  poke?.();
  const w = stage.clientWidth;
  const h = stage.clientHeight;
  renderer.setSize(w, h);
  ambient.resize(w,h);
  labels.setSize(w, h);
  if (ORTHO) Object.assign(camera, { left: -w / 2, right: w / 2, top: h / 2, bottom: -h / 2 });
  else {
    camera.aspect = w / Math.max(1, h);
    controls.minDistance = distFor(controls.maxZoom);
    controls.maxDistance = distFor(controls.minZoom);
  }
  camera.updateProjectionMatrix();
}
window.addEventListener("resize", () => {
  resize();
  // turning a phone: pick the view angle that fills the new shape, then refit
  const tall = innerHeight > innerWidth * 1.1;
  if (portrait !== null && tall !== portrait && typeof chooseView === "function" && !touring) {
    chooseView();
    frame(focus, 500, { azimuth: AZ_SPH, elevation: EL });
  }
  portrait = tall;
});
resize();

// ---------- tweens ----------
const tweens = new Set();
const ease = (t) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
function tween(dur, fn, done) {
  poke(dur + 400);
  if ((still.matches && virtual === null) || dur <= 0) {
    fn(1, 1);
    done?.();
    return;
  }
  tweens.add({ t0: now(), dur, fn, done });
}
function stepTweens() {
  const t = now();
  for (const tw of [...tweens]) {
    const k = Math.min(1, (t - tw.t0) / tw.dur);
    tw.fn(ease(k), k);
    if (k >= 1) {
      tweens.delete(tw);
      tw.done?.();
    }
  }
}

// ---------- tree ----------
const NODES = new Map();
(function index(node, parent, depth) {
  node.parent = parent;
  node.depth = depth;
  NODES.set(node.id, node);
  for (const child of node.children) index(child, node, depth + 1);
})(DATA.tree, null, 0);
const usedBy = {};
for (const n of NODES.values()) if (n.kind === "crate") for (const d of n.deps ?? []) (usedBy[d] ??= new Set()).add(n.id);
const processOf = (n) => {
  while (n && n.kind !== "process" && n.kind !== "client") n = n.parent;
  return n;
};
const hash = (s) => [...s].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 7);

// ---------- layout ----------
const ROOT_BOX = { x: -334, z: -200, w: 668, d: 442, y: 0, h: 120 };
const FLOOR = (box) => Math.min(box.w, box.d) * 0.03;
const weight = (n) => Math.sqrt(Math.max(1, n.lines));
function codeHeight(n, w, d, levelH, maxLog) {
  if (n.kind === "daemon") return Math.min(w, d) * 0.55;
  const share = Math.log10(Math.max(1, n.lines) + 9) / maxLog; // log lines, 1 = the biggest sibling
  // never a needle: at most three times the box's own short side
  return Math.max(Math.min(w, d) * 0.06, Math.min(levelH * share, Math.min(w, d) * 3));
}
function heightFor(n, w, d) {
  const s = Math.min(w, d);
  if (n.kind === "process") return n.plan.h;
  if (n.kind === "file") return Math.max(s * 0.06, Math.min(s * 0.16, s * 0.05 * Math.log10(n.lines + 10)));
  if (n.kind === "key") return s * 0.25;
  if (n.kind === "daemon") return s * 0.55;
  if (n.kind === "struct") return s * (0.18 + Math.min(0.2, n.members * 0.015));
  if (n.kind === "enum") return s * 0.22;
  if (n.kind === "trait") return s * 0.26;
  const g = Math.log10(Math.max(1, n.lines) / 50 + 1);
  return Math.min(s * 0.42, Math.max(s * 0.1, s * (0.08 + 0.055 * g)));
}
function squarify(items, rect) {
  const out = [];
  let { x, z, w, d } = rect;
  let rest = items.slice();
  while (rest.length) {
    const short = Math.min(w, d);
    let row = [];
    let best = Infinity;
    for (const it of rest) {
      const next = [...row, it];
      const sum = next.reduce((s, r) => s + r.a, 0);
      const max = Math.max(...next.map((r) => r.a));
      const min = Math.min(...next.map((r) => r.a));
      const worst = Math.max((short * short * max) / (sum * sum), (sum * sum) / (short * short * min));
      if (worst > best) break;
      best = worst;
      row = next;
    }
    const sum = row.reduce((s, r) => s + r.a, 0);
    if (w >= d) {
      const cw = sum / d;
      let cz = z;
      for (const r of row) out.push({ n: r.n, x, z: cz, w: cw, d: r.a / cw }), (cz += r.a / cw);
      x += cw;
      w -= cw;
    } else {
      const ch = sum / w;
      let cx = x;
      for (const r of row) out.push({ n: r.n, x: cx, z, w: r.a / ch, d: ch }), (cx += r.a / ch);
      z += ch;
      d -= ch;
    }
    rest = rest.slice(row.length);
  }
  return out;
}
function layoutChildren(node) {
  const box = node.box;
  const top = box.y + FLOOR(box);
  if (node.children.every((c) => c.plan)) {
    for (const p of node.children) {
      const { x, y: zz, w, d, h } = p.plan;
      p.box = { x, z: zz, w, d, y: top + 1.2, h };
    }
    return;
  }
  const margin = Math.min(box.w, box.d) * 0.07;
  const inner = { x: box.x + margin, z: box.z + margin, w: box.w - 2 * margin, d: box.d - 2 * margin };
  const total = node.children.reduce((s, c) => s + weight(c), 0);
  const area = inner.w * inner.d;
  // one height scale per level: the tallest sibling is the one with the most code
  const levelH = Math.min(inner.w, inner.d) * 0.32;
  const maxLog = Math.max(...node.children.map((c) => Math.log10(Math.max(1, c.lines) + 9)));
  for (const c of squarify(node.children.map((n) => ({ n, a: (weight(n) / total) * area })), inner)) {
    const gap = Math.min(c.w, c.d) * 0.1 + Math.min(inner.w, inner.d) * 0.004;
    const w = Math.max(c.w * 0.2, c.w - gap);
    const d = Math.max(c.d * 0.2, c.d - gap);
    const lift = c.n.kind === "crate" ? Math.min(w, d) * 0.03 : 0; // a crate sits on its socket
    c.n.box = { x: c.x + gap / 2, z: c.z + gap / 2, w, d, y: top + lift, h: codeHeight(c.n, w, d, levelH, maxLog) };
  }
}
DATA.tree.box = ROOT_BOX;

// ---------- visuals per kind ----------
function colorFor(n) {
  if (!skin.tint) return skin.solid;
  const p = processOf(n);
  const hue = p ? TINTS[p.id.replace(/^(p|client):/, "")] : null;
  if (hue == null) return skin.solid;
  const mix = n.kind === "process" ? 0.34 : Math.max(0.08, 0.24 - (n.depth - 2) * 0.035);
  return new THREE.Color(skin.solid).lerp(new THREE.Color(hue), mix).getHex();
}
// Anatomy's look: four flat tones by facing (top lightest, two side tones, one
// shadow tone), so faces never drift into gradients.
const TONES = (() => {
  // indexed by dot(n, light) * 0.5 + 0.5: side face lands at 3, front at 6, top at 7
  const t = new THREE.DataTexture(new Uint8Array([95, 95, 95, 110, 140, 160, 190, 255]), 8, 1, THREE.RedFormat);
  t.minFilter = t.magFilter = THREE.NearestFilter;
  t.needsUpdate = true;
  return t;
})();
const HALO = (() => {
  const c = document.createElement("canvas");
  c.width = c.height = 128;
  const g = c.getContext("2d");
  const grad = g.createRadialGradient(64, 64, 8, 64, 64, 64);
  grad.addColorStop(0, "rgba(0,0,0,1)");
  grad.addColorStop(0.55, "rgba(0,0,0,0.45)");
  grad.addColorStop(1, "rgba(0,0,0,0)");
  g.fillStyle = grad;
  g.fillRect(0, 0, 128, 128);
  return new THREE.CanvasTexture(c);
})();
const haloGeo = new THREE.PlaneGeometry(1, 1).rotateX(-Math.PI / 2).translate(0.5, 0, 0.5);
const screwGeo = new THREE.CircleGeometry(1, 14).rotateX(-Math.PI / 2);
const unitBox = new THREE.BoxGeometry(1, 1, 1).translate(0.5, 0.5, 0.5);
const unitEdges = new THREE.EdgesGeometry(unitBox);
const rounded = new Map();
function chamfered(w, h, d, r) {
  const key = `${w.toFixed(2)}:${h.toFixed(2)}:${d.toFixed(2)}:${r.toFixed(2)}`;
  if (!rounded.has(key)) {
    const g = new RoundedBoxGeometry(w, h, d, 1, r).translate(w / 2, h / 2, d / 2);
    rounded.set(key, { g, e: new THREE.EdgesGeometry(g, 25) });
  }
  return rounded.get(key);
}

// Every container is built from the same parts it opens into: a base (floor),
// four walls and a lid. Opening is one continuous motion of those parts (lid
// lifts and fades, walls sink to low rims, children rise); closing is the same
// motion played backwards, so the closed box and the open tray are one object.
function mats(color) {
  const toon = (c) => new THREE.MeshToonMaterial({ color: c, gradientMap: TONES, transparent: true, polygonOffset: true, polygonOffsetFactor: 1, polygonOffsetUnits: 1 });
  return {
    body: toon(color),
    detail: toon(skin.detail),
    floor: toon(skin.floor),
    led: new THREE.MeshBasicMaterial({ color: skin.led, transparent: true }),
    screw: new THREE.MeshBasicMaterial({ color: skin.ink, transparent: true }),
    line: new THREE.LineBasicMaterial({ color: skin.ink, transparent: true }),
    groove: new THREE.LineBasicMaterial({ color: skin.ink, transparent: true }),
  };
}
const MASCOT = {};
const DARK_SKINS = new Set(["night", "blueprint"]);
function mascotTexture() {
  const key = DARK_SKINS.has(document.body.dataset.skin) ? "mascotDark" : "mascotLight";
  if (!MASCOT[key]) {
    MASCOT[key] = new THREE.TextureLoader().load(DATA.brand[key]);
    MASCOT[key].colorSpace = THREE.SRGBColorSpace;
    MASCOT[key].anisotropy = 8;
  }
  return MASCOT[key];
}
function engraving(text) {
  const c = document.createElement("canvas");
  c.width = 1024;
  c.height = 320;
  const g = c.getContext("2d");
  g.fillStyle = "#fff";
  g.font = '600 150px ui-monospace, "SF Mono", Menlo, monospace';
  g.textBaseline = "middle";
  g.textAlign = "left";
  if ("letterSpacing" in g) g.letterSpacing = "18px";
  g.fillText(text, 60, 130);
  g.fillRect(60, 238, 700, 7);
  g.fillRect(60, 272, 470, 7);
  const t = new THREE.CanvasTexture(c);
  t.anisotropy = 8;
  t.colorSpace = THREE.SRGBColorSpace;
  return t;
}
function makeVisual(n) {
  const v = { owned: [], t: 0 };
  v.group = new THREE.Group();
  v.m = mats(colorFor(n)); // base and walls
  v.lm = mats(colorFor(n)); // lid: its own set so it can fade alone
  v.shellMat = new THREE.LineDashedMaterial({ color: skin.ghost, transparent: true, opacity: 0 });
  v.halo = new THREE.MeshBasicMaterial({ color: skin.halo ?? 0x000000, map: HALO, transparent: true, depthWrite: false });
  v.base = new THREE.Group();
  v.walls = new THREE.Group();
  v.lid = new THREE.Group();
  v.group.add(v.base, v.walls, v.lid);
  world.add(v.group);
  n.v = v;
  buildBody(n);
  place(n, 1);
  return v;
}
const own = (n, geo) => (n.v.owned.push(geo), geo);
// add a mesh (and its crease/silhouette edges) to one of the part groups
function add(n, grp, geo, mat, edges, lineMat, at = [0, 0, 0], scale = [1, 1, 1]) {
  const m = new THREE.Mesh(geo, mat);
  m.position.set(...at);
  m.scale.set(...scale);
  m.userData.node = n;
  grp.add(m);
  if (edges) {
    const e = new THREE.LineSegments(edges, lineMat);
    e.position.copy(m.position);
    e.scale.copy(m.scale);
    grp.add(e);
  }
  return m;
}
const box = (n, grp, set, mat, x, y, z, w, h, d, edges = true) => add(n, grp, unitBox, mat, edges ? unitEdges : null, set.line, [x, y, z], [Math.max(w, 1e-6), Math.max(h, 1e-6), Math.max(d, 1e-6)]);
const bevelBox = (n, grp, set, mat, x, y, z, w, h, d, frac) => {
  const r = Math.min(Math.min(w, d, h) * frac, Math.min(w, d, h) * 0.45);
  const b = chamfered(w, h, d, Math.max(r, 1e-5));
  return add(n, grp, b.g, mat, b.e, set.line, [x, y, z]);
};
// the small purposeful parts of an Anatomy solid: inset groove, corner screws
function crafted(n, grp, set, top, w, d, { screws = true, groove = true } = {}) {
  const s = Math.min(w, d);
  if (groove) {
    const i = s * 0.09;
    const y = top + s * 0.0015;
    const pts = [[i, i], [w - i, i], [w - i, d - i], [i, d - i], [i, i]].map(([x, z]) => new THREE.Vector3(x, y, z));
    grp.add(new THREE.Line(own(n, new THREE.BufferGeometry().setFromPoints(pts)), set.groove));
  }
  if (screws) {
    const r = s * 0.022;
    const k = s * 0.045;
    const inst = new THREE.InstancedMesh(screwGeo, set.screw, 4);
    const m = new THREE.Matrix4();
    [[k, k], [w - k, k], [w - k, d - k], [k, d - k]].forEach(([x, z], j) => inst.setMatrixAt(j, m.makeScale(r, 1, r).setPosition(x, top + s * 0.002, z)));
    grp.add(inst);
    n.v.owned.push({ dispose: () => inst.dispose() });
  }
}
function haloUnder(n, w, d) {
  const s = Math.min(w, d);
  const hm = new THREE.Mesh(haloGeo, n.v.halo);
  const g = s * 0.16;
  hm.position.set(-g, s * 0.001, -g * 0.6);
  hm.scale.set(w + 2 * g, 1, d + 2 * g);
  hm.renderOrder = -1;
  n.v.base.add(hm);
}

function buildBody(n) {
  const v = n.v;
  const { w, d, h } = n.box;
  const s = Math.min(w, d);
  const M = v.m;
  const L = v.lm;
  if (n.children.length) {
    // container: base + four walls + lid, all at true size
    const round = ["system", "server", "process", "crate", "client"].includes(n.kind);
    const f = FLOOR(n.box);
    const t = Math.max(s * 0.025, 1e-4);
    const lidT = Math.min(h * 0.14, s * 0.07);
    const wallH = Math.max(h - f - lidT, 1e-4);
    v.dims = { f, t, lidT, wallH, rim: Math.min(wallH * 0.28, s * 0.1) };
    v.pickOpen = box(n, v.base, M, M.floor, 0, 0, 0, w, f, d);
    v.walls.position.y = f;
    const wall = (x, z, ww, dd) => (round ? bevelBox(n, v.walls, M, M.body, x, 0, z, ww, wallH, dd, 0.3) : box(n, v.walls, M, M.body, x, 0, z, ww, wallH, dd));
    wall(0, 0, w, t);
    wall(0, d - t, w, t);
    wall(0, t, t, d - 2 * t);
    wall(w - t, t, t, d - 2 * t);
    v.lid.position.y = f + wallH;
    v.lidBody = round ? bevelBox(n, v.lid, L, L.body, 0, 0, 0, w, lidT, d, 0.35) : box(n, v.lid, L, L.body, 0, 0, 0, w, lidT, d);
    crafted(n, v.lid, L, lidT, w, d, { screws: !["system", "server", "crate", "file"].includes(n.kind) && !(n.kind === "client" && n.device !== "kit"), groove: n.kind !== "file" && !(n.kind === "client" && n.device !== "kit") });
    // an invisible full box for picking while closed
    v.pick = new THREE.Mesh(unitBox, new THREE.MeshBasicMaterial({ visible: false }));
    v.pick.scale.set(w, h, d);
    v.pick.userData.node = n;
    v.group.add(v.pick);
    const shell = new THREE.LineSegments(unitEdges, v.shellMat);
    shell.scale.set(w, h, d);
    const dash = Math.max(w, d, h) * 0.02;
    v.shellMat.dashSize = dash;
    v.shellMat.gapSize = dash;
    shell.computeLineDistances();
    v.group.add(shell);
    // kind details, each on the part that carries it
    if (n.kind === "system" || n.kind === "server") {
      const c = 11;
      for (const [cx, cz] of [[0, 0], [w - c, 0], [0, d - c], [w - c, d - c]]) {
        box(n, v.lid, L, L.detail, cx - 0.6, lidT - 0.01, cz - 0.6, c + 1.2, 1.5, c + 1.2);
        box(n, v.base, M, M.detail, cx - 0.6, -1.5, cz - 0.6, c + 1.2, 1.5, c + 1.2);
      }
      box(n, v.lid, L, L.detail, w / 2 - 46, lidT, d / 2 - 16, 92, 0.8, 32);
      // the name engraved on the label plate
      L.engrave = new THREE.MeshBasicMaterial({ map: engraving(n.kind === "server" ? "MAGICIAN" : "MAGICAN"), color: skin.ink, transparent: true, depthWrite: false });
      const plate = new THREE.Mesh(own(n, new THREE.PlaneGeometry(84, 26).rotateX(-Math.PI / 2)), L.engrave);
      plate.position.set(w / 2, lidT + 0.81, d / 2);
      plate.rotation.y = 0;
      v.lid.add(plate);
      if (n.kind === "system" && DATA.brand) {
        // the star mascot, waving, just below the name
        L.mascot = new THREE.MeshBasicMaterial({ map: mascotTexture(), transparent: true, depthWrite: false });
        const size = Math.min(w, d) * 0.28;
        const star = new THREE.Mesh(own(n, new THREE.PlaneGeometry(size, size).rotateX(-Math.PI / 2)), L.mascot);
        star.position.set(w / 2, lidT + 0.82, d / 2 + 16 + size * 0.55);
        v.lid.add(star);
      }
      for (const lx of [w * 0.18, w * 0.82]) box(n, v.walls, M, M.detail, lx - 7, wallH - 12, d, 14, 9, 1.4);
      for (const lz of [d * 0.2, d * 0.8]) box(n, v.walls, M, M.detail, w, wallH - 12, lz - 7, 1.4, 9, 14);
      box(n, v.walls, M, M.detail, w / 2 - 22, wallH * 0.55, d, 44, 10, 2.2);
      box(n, v.walls, M, M.detail, w, wallH * 0.55, d / 2 - 18, 2.2, 10, 36);
      if (n.kind === "server") {
        const wheel = own(n, new THREE.CylinderGeometry(6.6, 6.6, 6.8, 24).rotateX(Math.PI / 2));
        for (const [cx, cz] of [[20, 20], [w - 20, 20], [20, d - 20], [w - 20, d - 20]]) add(n, v.base, wheel, M.detail, null, null, [cx, -6.8, cz]);
      } else {
        // the world sits on a low plinth
        box(n, v.base, M, M.floor, -4, -5, -4, w + 8, 5, d + 8);
      }
    } else if (n.kind === "process") {
      for (let i = 0; i < 9; i++) box(n, v.walls, M, M.detail, w + 0.01, wallH * 0.25, d * (0.2 + i * 0.07), 0.3, wallH * 0.5, 0.9, false);
      const led = own(n, new THREE.SphereGeometry(0.75, 10, 8));
      for (let i = 0; i < 4; i++) add(n, v.walls, led, M.led, null, null, [w * 0.35 + i * 4.5, wallH * 0.78, d + 0.3]);
      box(n, v.walls, M, M.detail, 4, wallH * 0.55, d + 0.01, Math.min(w * 0.45, n.name.length * 2.6 + 6), wallH * 0.24, 0.25);
      haloUnder(n, w, d);
    } else if (n.kind === "crate") {
      const pitch = s * 0.07;
      const pins = [];
      for (let x = pitch; x < w - pitch / 2; x += pitch) pins.push([x - pitch * 0.2, 0, d, pitch * 0.4, pitch * 0.3], [x - pitch * 0.2, 0, -pitch * 0.3, pitch * 0.4, pitch * 0.3]);
      for (let z = pitch; z < d - pitch / 2; z += pitch) pins.push([w, 0, z - pitch * 0.2, pitch * 0.3, pitch * 0.4], [-pitch * 0.3, 0, z - pitch * 0.2, pitch * 0.3, pitch * 0.4]);
      const pin = new THREE.InstancedMesh(unitBox, M.detail, pins.length);
      const mm = new THREE.Matrix4();
      pins.forEach(([x, y, z, sx, sz], i) => pin.setMatrixAt(i, mm.makeScale(sx, (f + wallH * 0.3) * 1, sz).setPosition(x, y, z)));
      v.base.add(pin);
      v.owned.push({ dispose: () => pin.dispose() });
      add(n, v.lid, own(n, new THREE.CylinderGeometry(s * 0.05, s * 0.05, lidT * 0.1, 16)), L.detail, null, null, [s * 0.12, lidT, s * 0.12]);
      box(n, v.lid, L, L.detail, w * 0.3, lidT, d * 0.3, w * 0.4, lidT * 0.06, d * 0.4);
      const below = n.box.y - (n.parent.box.y + FLOOR(n.parent.box));
      if (below > 0) box(n, v.base, M, M.floor, -s * 0.05, -below, -s * 0.05, w + s * 0.1, below, d + s * 0.1);
    } else if (n.kind === "client") {
      const dev = n.device;
      const top = lidT;
      if (dev === "laptop") {
        // keyboard and trackpad
        const cols = 12;
        const rows = 4;
        for (let r = 0; r < rows; r++) for (let c = 0; c < cols; c++) box(n, v.lid, L, L.detail, w * 0.1 + (c * w * 0.8) / cols, top, d * 0.12 + (r * d * 0.42) / rows, (w * 0.8) / cols - 1.2, 0.6, (d * 0.42) / rows - 1.4, false);
        box(n, v.lid, L, L.detail, w * 0.36, top, d * 0.66, w * 0.28, 0.4, d * 0.22);
      } else if (dev === "browser") {
        box(n, v.lid, L, L.detail, w * 0.04, top, d * 0.05, w * 0.92, 0.5, d * 0.12);
        for (let i = 0; i < 3; i++) add(n, v.lid, own(n, new THREE.CylinderGeometry(1.6, 1.6, 0.6, 16)), L.screw, null, null, [w * 0.08 + i * 5, top + 0.5, d * 0.11]);
        box(n, v.lid, L, L.body, w * 0.3, top + 0.5, d * 0.07, w * 0.5, 0.3, d * 0.07);
        for (let i = 0; i < 5; i++) box(n, v.lid, L, L.detail, w * 0.08, top, d * (0.28 + i * 0.12), w * (0.4 + ((i * 37) % 40) / 100), 0.4, d * 0.05, false);
      } else if (dev === "phone") {
        box(n, v.lid, L, L.detail, w * 0.09, top, d * 0.07, w * 0.82, 0.5, d * 0.86);
        box(n, v.lid, L, L.body, w * 0.38, top + 0.5, d * 0.09, w * 0.24, 0.3, d * 0.025);
      } else if (dev === "board") {
        box(n, v.lid, L, L.detail, w * 0.36, top, d * 0.3, w * 0.28, 1.6, d * 0.32);
        for (let i = 0; i < 10; i++) {
          box(n, v.lid, L, L.screw, w * 0.1 + i * w * 0.08, top, d * 0.08, w * 0.04, 1.2, d * 0.05, false);
          box(n, v.lid, L, L.screw, w * 0.1 + i * w * 0.08, top, d * 0.87, w * 0.04, 1.2, d * 0.05, false);
        }
      } else if (dev === "ereader") {
        box(n, v.lid, L, L.floor, w * 0.1, top, d * 0.08, w * 0.8, 0.5, d * 0.72);
        for (let i = 0; i < 6; i++) box(n, v.lid, L, L.detail, w * 0.16, top + 0.5, d * (0.14 + i * 0.1), w * (0.5 + ((i * 29) % 20) / 100), 0.2, d * 0.03, false);
      } else if (dev === "puzzle") {
        add(n, v.lid, own(n, new THREE.CylinderGeometry(s * 0.14, s * 0.14, 2, 28)), L.detail, null, null, [w * 0.5, top + 1, d * 0.5]);
      } else if (dev === "rack") {
        // a rack of skill cartridges
        const cols = 8;
        const rows = 6;
        for (let r = 0; r < rows; r++) for (let c = 0; c < cols; c++) box(n, v.lid, L, (r + c) % 3 ? L.detail : L.body, w * 0.08 + (c * w * 0.84) / cols, top, d * 0.08 + (r * d * 0.84) / rows, (w * 0.84) / cols - 1.6, 1 + ((r * 7 + c * 3) % 4) * 0.6, (d * 0.84) / rows - 2.2);
      } else if (dev === "graph") {
        // nodes and edges of a little graph
        const pts = [[0.2, 0.25], [0.5, 0.18], [0.8, 0.3], [0.3, 0.6], [0.65, 0.55], [0.5, 0.85], [0.85, 0.8]];
        const ball = own(n, new THREE.SphereGeometry(Math.min(w, d) * 0.045, 12, 10));
        for (const [px, pz] of pts) add(n, v.lid, ball, L.detail, null, null, [w * px, top + 1, d * pz]);
        const edges = [[0, 1], [1, 2], [0, 3], [1, 4], [3, 4], [3, 5], [4, 6], [2, 4], [5, 6]];
        const geo = own(n, new THREE.BufferGeometry().setFromPoints(edges.flatMap(([a, b]) => [new THREE.Vector3(w * pts[a][0], top + 1, d * pts[a][1]), new THREE.Vector3(w * pts[b][0], top + 1, d * pts[b][1])])));
        v.lid.add(new THREE.LineSegments(geo, L.line));
      } else if (dev === "helpers") {
        for (let i = 0; i < 3; i++) box(n, v.lid, L, L.detail, w * 0.12 + i * w * 0.27, top, d * 0.3, w * 0.22, 1.2, d * 0.4);
      }
      haloUnder(n, w, d);
    } else if (n.kind === "file") {
      // a card that opens: the "text" rides on the lid
      const lines = Math.max(2, Math.min(5, Math.round(Math.log10(n.lines + 10))));
      const seed = hash(n.id);
      for (let i = 0; i < lines; i++) {
        const len = 0.35 + (((seed >> (i * 3)) & 7) / 7) * 0.5;
        box(n, v.lid, L, L.detail, w * 0.12, lidT, d * (0.2 + (i * 0.6) / Math.max(1, lines - 1)), w * 0.76 * len, lidT * 0.08, d * 0.05, false);
      }
      haloUnder(n, w, d);
    } else {
      // module / config drawer case: front panel and pull ride the front wall
      box(n, v.walls, M, M.detail, w * 0.08, wallH * 0.15, d + s * 0.0005, w * 0.84, wallH * 0.7, s * 0.012);
      box(n, v.walls, M, M.detail, w * 0.4, wallH * 0.42, d, w * 0.2, wallH * 0.14, s * 0.03);
      haloUnder(n, w, d);
    }
    setOpenness(n, 0);
    return;
  }
  // leaves are solid objects
  if (n.kind === "file") {
    v.pick = bevelBox(n, v.base, M, M.body, 0, 0, 0, w, h, d, 0.3);
    const lines = Math.max(2, Math.min(5, Math.round(Math.log10(n.lines + 10))));
    const seed = hash(n.id);
    for (let i = 0; i < lines; i++) {
      const len = 0.35 + (((seed >> (i * 3)) & 7) / 7) * 0.5;
      box(n, v.base, M, M.detail, w * 0.12, h, d * (0.2 + (i * 0.6) / Math.max(1, lines - 1)), w * 0.76 * len, h * 0.04, d * 0.05, false);
    }
    haloUnder(n, w, d);
  } else if (n.kind === "struct") {
    // a block with one slot per field
    v.pick = bevelBox(n, v.base, M, M.body, 0, 0, 0, w, h, d, 0.12);
    const k = Math.max(1, Math.min(12, n.members || 1));
    const pitch = (d * 0.76) / k;
    for (let i = 0; i < k; i++) box(n, v.base, M, M.detail, w * 0.12, h, d * 0.12 + i * pitch + pitch * 0.25, w * 0.76, h * 0.03, pitch * 0.5);
    haloUnder(n, w, d);
  } else if (n.kind === "enum") {
    // a rotary selector: one tick per variant, a pointer on the first
    const r = s * 0.46;
    const dial = own(n, new THREE.CylinderGeometry(r, r, h, 40).translate(w / 2, h / 2, d / 2));
    v.pick = add(n, v.base, dial, M.body, own(n, new THREE.EdgesGeometry(dial, 30)), M.line);
    const k = Math.max(2, Math.min(18, n.members || 2));
    const tick = own(n, new THREE.BoxGeometry(1, 1, 1).translate(0, 0.5, 0));
    for (let i = 0; i < k; i++) {
      const a = (i / k) * Math.PI * 2;
      const m = add(n, v.base, tick, M.detail, null, null, [w / 2 + Math.cos(a) * r * 0.8, h, d / 2 + Math.sin(a) * r * 0.8], [r * 0.05, h * 0.06, r * 0.16]);
      m.rotation.y = -a;
    }
    const ptr = add(n, v.base, tick, M.screw, null, null, [w / 2 + r * 0.3, h, d / 2], [r * 0.6, h * 0.08, r * 0.06]);
    haloUnder(n, w, d);
  } else if (n.kind === "trait") {
    // a socket: what implementors plug into, one pin per method
    const r = s * 0.46;
    const body = own(n, new THREE.CylinderGeometry(r, r, h, 40).translate(w / 2, h / 2, d / 2));
    v.pick = add(n, v.base, body, M.body, own(n, new THREE.EdgesGeometry(body, 30)), M.line);
    add(n, v.base, own(n, new THREE.CylinderGeometry(r * 0.55, r * 0.55, h * 0.04, 32)), M.floor, null, null, [w / 2, h, d / 2]);
    add(n, v.base, own(n, new THREE.TorusGeometry(r * 0.56, r * 0.04, 6, 32).rotateX(Math.PI / 2)), M.detail, null, null, [w / 2, h, d / 2]);
    const k = Math.max(1, Math.min(12, n.members || 1));
    const pin = own(n, new THREE.CylinderGeometry(r * 0.05, r * 0.05, h * 0.25, 10).translate(0, h * 0.125, 0));
    for (let i = 0; i < k; i++) {
      const a = (i / k) * Math.PI * 2;
      add(n, v.base, pin, M.screw, null, null, [w / 2 + Math.cos(a) * r * 0.78, h, d / 2 + Math.sin(a) * r * 0.78]);
    }
    haloUnder(n, w, d);
  } else if (n.kind === "key") {
    v.pick = bevelBox(n, v.base, M, M.body, 0, 0, 0, w, h, d, 0.15);
    box(n, v.base, M, M.detail, w * 0.3, h, d * (hash(n.id) % 2 ? 0.15 : 0.5), w * 0.4, h * 0.5, d * 0.35);
  } else if (n.kind === "daemon") {
    const r = s / 2;
    const tank = own(n, new THREE.CylinderGeometry(r, r, h, 32).translate(w / 2, h / 2, d / 2));
    v.pick = add(n, v.base, tank, M.body, own(n, new THREE.EdgesGeometry(tank, 30)), M.line);
    add(n, v.base, own(n, new THREE.TorusGeometry(r * 0.55, r * 0.06, 6, 28).rotateX(Math.PI / 2)), M.detail, null, null, [w / 2, h, d / 2]);
    haloUnder(n, w, d);
  } else {
    v.pick = bevelBox(n, v.base, M, M.body, 0, 0, 0, w, h, d, 0.12);
    crafted(n, v.base, M, h, w, d);
    haloUnder(n, w, d);
  }
}
// openness 0 = sealed box, 1 = open tray; the lid lifts and fades, walls sink to rims
function setOpenness(n, t) {
  const v = n.v;
  if (!v?.dims) return;
  v.t = t;
  const { f, wallH, rim } = v.dims;
  const k = 1 - t * (1 - rim / wallH);
  v.walls.scale.y = k;
  const lift = Math.max(n.box.w, n.box.d) * 0.55;
  v.lid.position.y = f + wallH * k + lift * t;
  v.lid.visible = t < 0.999;
  applyAlpha(n);
}
function place(n, rise) {
  const { x, z, y } = n.box;
  n.v.group.position.set(x, y, z);
  n.v.group.scale.set(1, Math.max(0.02, rise), 1);
}
function dispose(n) {
  if (!n.v) return;
  world.remove(n.v.group);
  for (const g of n.v.owned) g.dispose();
  for (const set of [n.v.m, n.v.lm]) for (const m of Object.values(set)) (m.map && !Object.values(MASCOT).includes(m.map) && m.map.dispose(), m.dispose());
  n.v.shellMat.dispose();
  n.v.halo.dispose();
  if (n.label) n.label.removeFromParent();
  n.label = null;
  n.v = null;
}

// ---------- open / close ----------
const opened = new Set();
const OPEN_MS = 850;
let focus = DATA.tree;
let hovered = null;
let selected = null;
function animateOpenness(n, to, ms, done) {
  const from = n.v.t;
  n.v.anim = (n.v.anim ?? 0) + 1;
  const token = n.v.anim;
  tween(ms, (e) => n.v && n.v.anim === token && setOpenness(n, from + (to - from) * e), () => n.v && n.v.anim === token && done?.());
}
function open(n, { animate = true } = {}) {
  if (!n.children.length || opened.has(n)) return;
  ambient.setExpanded(true);
  if (n.parent && !opened.has(n.parent)) open(n.parent, { animate });
  opened.add(n);
  if (!n.v) makeVisual(n);
  if (n.v.closing) return opened.delete(n);
  layoutChildren(n);
  if (animate) animateOpenness(n, 1, OPEN_MS);
  else setOpenness(n, 1);
  const kids = n.children;
  kids.forEach((c, i) => {
    makeVisual(c);
    if (animate) {
      place(c, 0.02);
      after(OPEN_MS * 0.35 + Math.min(600, i * (600 / Math.max(1, kids.length))), () => tween(650, (e) => c.v && !n.v?.closing && place(c, 0.02 + 0.98 * e)));
    }
  });
  rebuildLinks();
  restyle();
  syncExpand();
}
function subtree(n, out = []) {
  for (const c of n.children) if (c.v) (out.push(c), subtree(c, out));
  return out;
}
function close(n, { animate = true } = {}) {
  if (!opened.has(n)) return;
  const inner = subtree(n);
  for (const c of inner) opened.delete(c);
  opened.delete(n);
  if (selected && inner.includes(selected)) selected = null;
  if (hovered && inner.includes(hovered)) hovered = null;
  const finish = () => {
    for (const c of inner.reverse()) dispose(c);
    if (n.v) n.v.closing = false;
    if (!opened.size) ambient.setExpanded(false);
    rebuildLinks();
    restyle();
  };
  if (!animate || !n.v) {
    finish();
    if (n.v) setOpenness(n, 0);
    syncExpand();
    return;
  }
  // everything inside sinks back into its floor, then the walls rise and the lid comes down
  n.v.closing = true;
  const SINK = 520;
  inner.forEach((c, i) => {
    const from = c.v.group.scale.y;
    tween(SINK, (e) => c.v && n.v?.closing && place(c, from + (0.02 - from) * e));
  });
  after(SINK * 0.8, () => n.v?.closing && animateOpenness(n, 0, OPEN_MS, () => n.v?.closing && finish()));
  rebuildLinks();
  restyle();
  syncExpand();
}

// ---------- wires ----------
// A connection is a cable on the floor of the box that owns it. It leaves each
// end from a jack on the side facing the other end and runs along the aisles
// between siblings: an A* route over a floor grid that prefers cells other
// cables already use, so cables heading the same way merge into one bundle.
// A cable whose real end lies deeper (a client talking to the API crate, a
// module referencing another crate's module) climbs over the rim of each open
// box on the way and keeps going to the part inside.
// Direction: packets flow from the caller to the callee. Weight: thickness.
// Kind: solid for calls and references, dashed for spawn and supervise,
// dotted for planned.
const WIRE = {
  data: { say: "calls", color: () => skin.link },
  dep: { say: "uses", color: () => skin.link },
  xref: { say: "references", color: () => skin.link },
  host: { say: "hosts", color: () => 0x7c3aed },
  power: { say: "starts and supervises", color: () => skin.power, dash: [0.55, 7] },
  planned: { say: "planned", color: () => skin.ghost, dash: [0.3, 3.2], still: true },
};
function dashTexture([on, period], len, r) {
  const c = document.createElement("canvas");
  c.width = 64;
  c.height = 2;
  const g = c.getContext("2d");
  g.fillStyle = "#000";
  g.fillRect(0, 0, 64, 2);
  g.fillStyle = "#fff";
  g.fillRect(0, 0, Math.round(64 * on), 2);
  const t = new THREE.CanvasTexture(c);
  t.wrapS = THREE.RepeatWrapping;
  t.magFilter = THREE.NearestFilter;
  t.repeat.set(Math.max(1, len / (period * r)), 1);
  return t;
}

// one routing grid per open box, kept while its layout stands
const grids = new Map();
function gridOf(owner) {
  let g = grids.get(owner.id);
  if (g && g.box === owner.box) return g;
  const b = owner.box;
  const N = Math.round(Math.min(200, Math.max(80, 50 + 14 * Math.sqrt(owner.children.length))));
  const nx = Math.max(4, Math.round((b.w / Math.max(b.w, b.d)) * N));
  const nz = Math.max(4, Math.round((b.d / Math.max(b.w, b.d)) * N));
  const cx = b.w / nx;
  const cz = b.d / nz;
  const blocked = new Uint8Array(nx * nz);
  const wall = Math.max(1, Math.ceil((owner.v?.dims?.t ?? FLOOR(b)) / Math.min(cx, cz)));
  for (let j = 0; j < nz; j++) for (let i = 0; i < nx; i++) if (i < wall || j < wall || i >= nx - wall || j >= nz - wall) blocked[j * nx + i] = 1;
  for (const c of owner.children) {
    if (!c.box) continue;
    const i0 = Math.max(0, Math.ceil((c.box.x - b.x) / cx - 0.5));
    const i1 = Math.min(nx - 1, Math.floor((c.box.x + c.box.w - b.x) / cx - 0.5));
    const j0 = Math.max(0, Math.ceil((c.box.z - b.z) / cz - 0.5));
    const j1 = Math.min(nz - 1, Math.floor((c.box.z + c.box.d - b.z) / cz - 0.5));
    for (let j = j0; j <= j1; j++) for (let i = i0; i <= i1; i++) blocked[j * nx + i] = 2;
  }
  // a step that hugs a box costs more, so a cable keeps to the middle of its aisle
  const near = new Uint8Array(nx * nz);
  for (let j = 0; j < nz; j++)
    for (let i = 0; i < nx; i++) {
      const k = j * nx + i;
      if (blocked[k]) continue;
      near[k] = (i > 0 && blocked[k - 1] === 2) || (i < nx - 1 && blocked[k + 1] === 2) || (j > 0 && blocked[k - nx] === 2) || (j < nz - 1 && blocked[k + nx] === 2) ? 1 : 0;
    }
  // laid-out-by-hand levels (the world, the server) keep cables on the floor;
  // packed levels hang them in a tray above the boxes, over the same aisles
  const overhead = !owner.children.every((c) => c.plan);
  const y = b.y + FLOOR(b);
  const tallest = Math.max(0, ...owner.children.map((c) => (c.box ? c.box.y + c.box.h - y : 0)));
  g = { owner, box: b, nx, nz, cx, cz, cell: Math.min(cx, cz), blocked, near, used: new Float32Array(nx * nz), routes: new Map(), y, overhead, tray: y + tallest * 1.18 + Math.min(cx, cz) * 3, based: false };
  grids.set(owner.id, g);
  return g;
}
const cellAt = (g, x, z) => {
  const i = Math.min(g.nx - 1, Math.max(0, Math.floor((x - g.box.x) / g.cx)));
  const j = Math.min(g.nz - 1, Math.max(0, Math.floor((z - g.box.z) / g.cz)));
  return j * g.nx + i;
};
const centerOf = (g, k) => ({ x: g.box.x + ((k % g.nx) + 0.5) * g.cx, z: g.box.z + (Math.floor(k / g.nx) + 0.5) * g.cz });
function freeNear(g, k) {
  if (!g.blocked[k]) return k;
  const seen = new Set([k]);
  let ring = [k];
  for (let step = 0; step < 6 && ring.length; step++) {
    const next = [];
    for (const c of ring)
      for (const d of [1, -1, g.nx, -g.nx]) {
        const n = c + d;
        if (n < 0 || n >= g.blocked.length || seen.has(n) || (Math.abs(d) === 1 && Math.floor(n / g.nx) !== Math.floor(c / g.nx))) continue;
        if (!g.blocked[n]) return n;
        seen.add(n);
        next.push(n);
      }
    ring = next;
  }
  return -1;
}
// the side of a box facing a point; the jack slides along that side toward it
function jackOf(n, toward) {
  const b = n.box;
  const dx = (toward.x - (b.x + b.w / 2)) / (b.w / 2);
  const dz = (toward.z - (b.z + b.d / 2)) / (b.d / 2);
  const along = (v, lo, len) => Math.min(lo + len * 0.82, Math.max(lo + len * 0.18, v));
  if (Math.abs(dx) >= Math.abs(dz)) {
    const nx = Math.sign(dx) || 1;
    return { x: nx > 0 ? b.x + b.w : b.x, z: along(toward.z, b.z, b.d), nx, nz: 0, node: n };
  }
  const nz = Math.sign(dz) || 1;
  return { x: along(toward.x, b.x, b.w), z: nz > 0 ? b.z + b.d : b.z, nx: 0, nz, node: n };
}
const mid$ = (n) => ({ x: n.box.x + n.box.w / 2, z: n.box.z + n.box.d / 2 });

// A* over (cell, heading): turns cost extra, shared cells cost less
const DIRS = [[1, 0], [-1, 0], [0, 1], [0, -1]];
function astar(g, s, t) {
  const S = g.nx * g.nz * 5;
  if (!g.cost || g.cost.length !== S) (g.cost = new Float32Array(S)), (g.prev = new Int32Array(S));
  g.cost.fill(Infinity);
  const heap = [];
  const push = (f, st) => {
    heap.push([f, st]);
    let i = heap.length - 1;
    while (i > 0) {
      const p = (i - 1) >> 1;
      if (heap[p][0] <= heap[i][0]) break;
      [heap[p], heap[i]] = [heap[i], heap[p]];
      i = p;
    }
  };
  const pop = () => {
    const top = heap[0];
    const last = heap.pop();
    if (heap.length) {
      heap[0] = last;
      let i = 0;
      for (;;) {
        const l = 2 * i + 1;
        const r = l + 1;
        let m = i;
        if (l < heap.length && heap[l][0] < heap[m][0]) m = l;
        if (r < heap.length && heap[r][0] < heap[m][0]) m = r;
        if (m === i) break;
        [heap[m], heap[i]] = [heap[i], heap[m]];
        i = m;
      }
    }
    return top;
  };
  const ti = t % g.nx;
  const tj = Math.floor(t / g.nx);
  const h = (k) => (Math.abs((k % g.nx) - ti) + Math.abs(Math.floor(k / g.nx) - tj)) * 0.5;
  const start = s * 5 + 4;
  g.cost[start] = 0;
  g.prev[start] = -1;
  push(h(s), start);
  let budget = g.nx * g.nz * 6;
  while (heap.length && budget-- > 0) {
    const [f, st] = pop();
    const k = Math.floor(st / 5);
    const dir = st % 5;
    const c = g.cost[st];
    if (f - h(k) > c + 1e-6) continue;
    if (k === t) {
      const cells = [];
      for (let x = st; x !== -1; x = g.prev[x]) cells.push(Math.floor(x / 5));
      return cells.reverse();
    }
    const i = k % g.nx;
    const j = Math.floor(k / g.nx);
    for (let d = 0; d < 4; d++) {
      const ni = i + DIRS[d][0];
      const nj = j + DIRS[d][1];
      if (ni < 0 || nj < 0 || ni >= g.nx || nj >= g.nz) continue;
      const nk = nj * g.nx + ni;
      if (g.blocked[nk] === 1 && nk !== t) continue;
      if (g.blocked[nk] === 2 && nk !== t && !g.overhead) continue;
      const step = (g.blocked[nk] === 2 ? 6 : g.near[nk] ? 1.7 : 1) * (g.used[nk] ? 0.5 : 1) + (dir !== 4 && dir !== d ? 3 : 0);
      const ns = nk * 5 + d;
      if (c + step < g.cost[ns]) {
        g.cost[ns] = c + step;
        g.prev[ns] = st;
        push(c + step + h(nk), ns);
      }
    }
  }
  return null;
}
// floor route between two endpoints {x, z, nx, nz} (n = the way the cable leaves)
function routeIn(g, a, b) {
  const key = [a.x, a.z, b.x, b.z].map((v) => v.toFixed(2)).join(",");
  if (g.routes.has(key)) return g.routes.get(key);
  const out = (p) => (g.overhead ? cellAt(g, p.x, p.z) : freeNear(g, cellAt(g, p.x + p.nx * g.cx * 0.9, p.z + p.nz * g.cz * 0.9)));
  const s = out(a);
  const t = out(b);
  let pts;
  const cells = s >= 0 && t >= 0 ? astar(g, s, t) : null;
  if (!cells) pts = [a, b];
  else {
    for (const k of cells) g.used[k] += 1;
    const corners = cells.filter((k, i) => i === 0 || i === cells.length - 1 || cells[i + 1] - k !== k - cells[i - 1]).map((k) => centerOf(g, k));
    // square off the stubs: straight out of each jack, then onto the route
    const stub = (p, c) => (p.nx || (p.top && Math.abs(c.x - p.x) > Math.abs(c.z - p.z)) ? { x: c.x, z: p.z } : { x: p.x, z: c.z });
    pts = [a, stub(a, corners[0]), ...corners, stub(b, corners[corners.length - 1]), b];
  }
  g.routes.set(key, pts);
  return pts;
}

// a polyline with rounded corners
function cablePath(pts, round) {
  const clean = pts.filter((p, i) => i === 0 || p.distanceTo(pts[i - 1]) > 1e-4);
  const path = new THREE.CurvePath();
  if (clean.length < 2) clean.push(clean[0].clone().add(new THREE.Vector3(1e-3, 0, 0)));
  let prev = clean[0];
  for (let i = 1; i < clean.length - 1; i++) {
    const a = clean[i - 1];
    const c = clean[i];
    const n = clean[i + 1];
    const k = Math.min(round, a.distanceTo(c) / 2, c.distanceTo(n) / 2);
    const p1 = c.clone().add(a.clone().sub(c).setLength(k));
    const p2 = c.clone().add(n.clone().sub(c).setLength(k));
    if (prev.distanceTo(p1) > 1e-5) path.add(new THREE.LineCurve3(prev, p1));
    if (k > 1e-5) path.add(new THREE.QuadraticBezierCurve3(p1, c, p2));
    prev = k > 1e-5 ? p2 : c;
  }
  path.add(new THREE.LineCurve3(prev, clean[clean.length - 1]));
  return path;
}

// Where a cable meets a box: a jack on the side facing the other end (floor
// levels) or a socket in the lid, nudged toward the other end (tray levels).
function endOf(g, n, toward) {
  if (!g.overhead) return jackOf(n, toward);
  const b = n.box;
  const dx = toward.x - (b.x + b.w / 2);
  const dz = toward.z - (b.z + b.d / 2);
  const k = Math.hypot(dx / b.w, dz / b.d) || 1;
  return { x: b.x + b.w / 2 + (dx / k) * 0.22, z: b.z + b.d / 2 + (dz / k) * 0.22, nx: 0, nz: 0, top: true, node: n };
}
const lidOf = (n) => (opened.has(n) && n.v?.dims ? n.box.y + n.v.dims.f : n.box.y + n.box.h);
const levelY = (g, r) => (g.overhead ? g.tray : g.y + r);
// From a part deep inside open box X up to where X's own cable meets X (its
// end e): route on X's floor or tray to a port by that end, then over the rim
// (floor levels) or straight up into the tray above (tray levels).
function descend(X, chain, e, r, heavy) {
  if (!chain.length || !opened.has(X) || !X.v?.dims || !chain[0].v || !chain[0].box) return null;
  const g = gridOf(X);
  baseRoutes(X);
  // each level down runs thinner, like a trunk splitting into branches
  const rX = Math.min(r, g.cell * (heavy ? 0.6 : 0.42));
  const inset = X.v.dims.t + g.cell * 1.6;
  const port = e.top ? { x: e.x, z: e.z, nx: 0, nz: 0, top: true } : { x: e.x - e.nx * inset, z: e.z - e.nz * inset, nx: -e.nx, nz: -e.nz };
  const inner = endOf(g, chain[0], port);
  const flat = routeIn(g, inner, port);
  const deeper = descend(chain[0], chain.slice(1), inner, rX, heavy);
  const y = levelY(g, rX);
  const start = deeper ? [deeper.segs.at(-1).pts.at(-1)] : inner.top ? [new THREE.Vector3(inner.x, lidOf(chain[0]), inner.z)] : [];
  const pts = [...start, ...flat.map((p) => new THREE.Vector3(p.x, y, p.z))];
  if (!e.top) {
    const rim = X.box.y + X.v.dims.f + X.v.dims.rim + rX * 1.5;
    pts.push(new THREE.Vector3(port.x, Math.max(rim, y), port.z), new THREE.Vector3(e.x, rim, e.z));
  }
  return { segs: [...(deeper?.segs ?? []), { pts, r: rX, cell: g.cell }], end: deeper?.end ?? inner };
}
// base routes of a box are laid heaviest first, so the light ones bundle onto them
function baseRoutes(owner) {
  const g = gridOf(owner);
  if (g.based) return g;
  g.based = true;
  const weightOf = (l) => (l[2] === "dep" ? Number(String(l[3]).split(" ")[0]) || 1 : 1000);
  for (const l of [...(owner.links ?? [])].sort((a, b) => weightOf(b) - weightOf(a))) {
    const A = owner.children.find((c) => c.id === l[0]);
    const B = owner.children.find((c) => c.id === l[1]);
    if (A?.box && B?.box) routeIn(g, endOf(g, A, mid$(B)), endOf(g, B, mid$(A)));
  }
  return g;
}
const childPath = (top, ids) => {
  const out = [];
  let at = top;
  for (const id of ids ?? []) {
    at = at.children.find((c) => c.id === id);
    if (!at?.v) break;
    out.push(at);
  }
  return out;
};

let wireGroup = new THREE.Group();
world.add(wireGroup);
let wires = [];
let wireBuiltFor = null;
let hoveredWire = null;
// Cables are sized for their own level, so a cable from an outer level seen
// from deep inside would be a pipe across the screen. They are not kept
// physically true: each frame the shader pulls a cable's surface in toward its
// centre line so it is never drawn wider than WIRE_MAX_PX (radius), measured
// at its nearest point. Cables are only ever thinned, so they still fit their
// aisles; at a level's own framing (1-2.5 px) nothing changes.
const WIRE_MAX_PX = 3;
function thinnable(mat) {
  mat.userData.grow = { value: 0 };
  mat.onBeforeCompile = (shader) => {
    shader.uniforms.uGrow = mat.userData.grow;
    shader.vertexShader = shader.vertexShader.replace("#include <common>", "#include <common>\nuniform float uGrow;").replace("#include <begin_vertex>", "#include <begin_vertex>\ntransformed += normal * uGrow;");
  };
  return mat;
}
const toCam = new THREE.Vector3();
const camFwd = new THREE.Vector3();
function thinWires() {
  if (!wires.length) return;
  camera.getWorldDirection(camFwd);
  const h = stage.clientHeight;
  for (const w of wires) {
    w.meshes.forEach((m, i) => {
      const r0 = m.userData.r0;
      let depth = Infinity;
      if (!ORTHO) for (const p of m.userData.probe) depth = Math.min(depth, toCam.copy(p).sub(camera.position).dot(camFwd));
      const scale = ORTHO ? camera.zoom : h / (2 * Math.max(camera.near, depth) * TAN_HALF);
      const k = Math.min(1, WIRE_MAX_PX / Math.max(1e-9, r0 * scale));
      m.userData.k = k;
      m.material.userData.grow.value = r0 * (k - 1);
      w.xrays[i].material.userData.grow.value = r0 * (k - 1);
    });
    for (const j of w.jacks) j.scale.setScalar(w.meshes[j.userData.seg]?.userData.k ?? 1);
  }
}
function wireRadius(g, owner, kind, weight) {
  const s = Math.min(owner.box.w, owner.box.d);
  const base = Math.min(g.cell * 0.3, s * 0.0034);
  const r = kind === "dep" || kind === "xref" ? base * (0.5 + 0.32 * Math.log10(weight + 1)) : base;
  return Math.min(r, g.cell * 0.45);
}
// one cable: owner floor between children A and B, extended into open boxes
function makeWire(owner, A, B, kind, say, weight, deep = {}, extra = {}) {
  const g = baseRoutes(owner);
  const ea = endOf(g, A, mid$(B));
  const eb = endOf(g, B, mid$(A));
  // a spotlit view is framed wide and has few cables, so they run a bit heavier
  const r = wireRadius(g, owner, kind, weight) * (extra.spot ? 1.15 : 1);
  const chainA = deep.chainA ?? childPath(A, deep.a);
  const chainB = deep.chainB ?? childPath(B, deep.b);
  const head = descend(A, chainA, ea, r, extra.spot);
  const tail = descend(B, chainB, eb, r, extra.spot);
  const y = levelY(g, r);
  const flat = routeIn(g, ea, eb).map((p) => new THREE.Vector3(p.x, y, p.z));
  const cap = (e, n) => (e.top ? [new THREE.Vector3(e.x, lidOf(n), e.z)] : []);
  const tailSegs = tail ? tail.segs.slice().reverse().map((t) => ({ ...t, pts: t.pts.slice().reverse() })) : [];
  const outer = [...(head ? [head.segs.at(-1).pts.at(-1)] : cap(ea, A)), ...flat, ...(tail ? [tailSegs[0].pts[0]] : cap(eb, B))];
  const segs = [...(head?.segs ?? []), { pts: outer, r, cell: g.cell }, ...tailSegs];
  const style = WIRE[kind] ?? WIRE.dep;
  const meshes = [];
  const xrays = [];
  const samples = [];
  const sampleR = [];
  let len = 0;
  for (const seg of segs) {
    const path = cablePath(seg.pts, seg.cell * 1.4);
    const l = path.getLength();
    len += l;
    const mat = thinnable(new THREE.MeshLambertMaterial({ color: style.color(), transparent: true }));
    if (style.dash) mat.alphaMap = dashTexture(style.dash, l, seg.r);
    const geo = new THREE.TubeGeometry(path, Math.min(LITE ? 280 : 700, path.curves.length * (LITE ? 6 : 10) + Math.round(l / (seg.r * (LITE ? 4 : 2)))), seg.r, LITE ? 6 : 8, false);
    const tube = new THREE.Mesh(geo, mat);
    // where to measure how big it looks: a few points along it
    tube.userData.r0 = seg.r;
    tube.userData.probe = path.getSpacedPoints(Math.max(2, Math.min(24, Math.round(l / (seg.cell * 2)))));
    meshes.push(tube);
    // an x-ray copy shows a highlighted cable where boxes hide it
    const xray = new THREE.Mesh(geo, thinnable(new THREE.MeshBasicMaterial({ color: style.color(), transparent: true, opacity: 0.3, depthTest: false, depthWrite: false })));
    xray.renderOrder = 2;
    xray.visible = false;
    xrays.push(xray);
    const n = Math.max(6, Math.min(300, Math.round(l / (seg.r * 3))));
    for (const p of path.getSpacedPoints(n)) samples.push(p), sampleR.push(seg.r);
  }
  const mesh = meshes[0];
  const mat = mesh.material;
  const xray = xrays[0];
  const w = { owner, A, B, kind, say, weight, r, len, mesh, meshes, xray, xrays, mat, jacks: [], samples, sampleR, unit: g.cell, from: head?.end?.node ?? A, to: tail?.end?.node ?? B, ...extra };
  for (const m of meshes) m.userData.wire = w;
  const group = new THREE.Group();
  group.add(...meshes, ...xrays);
  // jacks on a closed box's side, sockets in its lid
  const jackMat = new THREE.MeshLambertMaterial({ color: skin.ink, transparent: true });
  for (const [e, jr, si] of [[head?.end ?? ea, segs[0].r, 0], [tail?.end ?? eb, segs.at(-1).r, segs.length - 1]]) {
    const n = e.node;
    const r = jr;
    if (!n) continue;
    let m;
    if (e.top) {
      m = new THREE.Mesh(new THREE.CylinderGeometry(r * 2.1, r * 2.1, r * 0.9, 14), jackMat);
      m.position.set(e.x, lidOf(n) + r * 0.45, e.z);
    } else {
      const s = r * 3.6;
      m = new THREE.Mesh(new THREE.BoxGeometry(e.nx ? r * 1.6 : s, r * 3.2, e.nz ? r * 1.6 : s), jackMat);
      m.position.set(e.x + e.nx * r * 0.8, n.box.y + r * 1.7, e.z + e.nz * r * 0.8);
    }
    m.userData.seg = si;
    group.add(m);
    w.jacks.push(m);
  }
  w.jackMat = jackMat;
  w.group = group;
  wireGroup.add(group);
  wires.push(w);
  return w;
}

// the box's direct cables, at the focus and (lighter, heaviest few) below it
function rebuildLinks() {
  world.remove(wireGroup);
  wireGroup.traverse((o) => (o.geometry?.dispose?.(), o.material?.alphaMap?.dispose?.(), o.material?.dispose?.()));
  wireGroup = new THREE.Group();
  wires = [];
  hoveredWire = null;
  for (const [id, g] of grids) if (!NODES.get(id) || !opened.has(NODES.get(id)) || g.box !== NODES.get(id).box) grids.delete(id);
  if (spot) {
    spotWires();
    wireBuiltFor = wireKey$();
    world.add(wireGroup);
    return;
  }
  for (const owner of opened) {
    if (!owner.links?.length) continue;
    const level = owner === focus ? 1 : inFocus(owner) ? 0.4 : 0;
    if (!level) continue;
    const list = level === 1 ? owner.links : owner.links.slice(0, 12);
    for (const [a, b, kind, say, deep] of list) {
      const A = owner.children.find((c) => c.id === a);
      const B = owner.children.find((c) => c.id === b);
      if (!A?.v || !B?.v) continue;
      const weight = kind === "dep" ? Number(String(say).split(" ")[0]) || 1 : 1;
      makeWire(owner, A, B, kind ?? "dep", say ?? "depends on", weight, deep ?? {}, { level });
    }
  }
  for (const w of crossWires(deepSubject())) w.level = 1;
  wireBuiltFor = wireKey$();
  world.add(wireGroup);
}

// References out of (and into) a part inside a crate reach other crates of the
// same process: each becomes one cable from the part, over the crate's rim,
// along the process floor (bundled with the crate's dependency cable) and down
// into the deepest open part of the target.
function deepSubject() {
  const n = selected ?? focus;
  if (!n || spot) return null;
  let m = n;
  while (m && m.id.includes("#")) m = m.parent;
  return m && crateOf(m) && m.kind !== "crate" ? m : null;
}
function crateOf(n) {
  while (n && n.kind !== "crate") n = n.parent;
  return n;
}
const relOf = (n, crate) => n.id.slice(crate.id.length + 2).replace(/^\//, "");
function chainTo(crate, path) {
  const out = [];
  let at = crate;
  let id = `${crate.id}::`;
  for (const seg of path ? path.split("/") : []) {
    id = `${id}/${seg}`;
    const next = at.children.find((c) => c.id === id);
    if (!next?.v) break;
    out.push(next);
    at = next;
  }
  return out;
}
function crossWires(n) {
  if (!n) return [];
  const crate = crateOf(n);
  const proc = crate?.parent;
  if (!proc || !opened.has(proc)) return [];
  const rel = relOf(n, crate);
  const under = (p) => p === rel || p.startsWith(`${rel}/`);
  const own = chainTo(crate, rel);
  const sum = (rows, keyOf) => {
    const m = new Map();
    for (const row of rows) {
      const k = keyOf(row);
      if (!k) continue;
      const e = m.get(k.key) ?? { ...k, refs: 0 };
      e.refs += row[3];
      m.set(k.key, e);
    }
    return [...m.values()].sort((a, b) => b.refs - a.refs).slice(0, 6);
  };
  const out = sum((crate.xlinks ?? []).filter((x) => under(x[0])), (x) => {
    const B = proc.children.find((c) => c.name === x[1]);
    if (!B?.v) return null;
    const chain = chainTo(B, x[2]);
    return { key: chain.at(-1)?.id ?? B.id, B, chain, to: x[1] + (x[2] ? `::${x[2].replace(/\//g, "::")}` : "") };
  });
  const inc = sum(
    proc.children.filter((c) => c !== crate && c.kind === "crate").flatMap((c) => (c.xlinks ?? []).filter((x) => x[1] === crate.name && under(x[2])).map((x) => [c, ...x])).map(([c, ...x]) => [x[0], c, x[2], x[3], c]),
    (x) => {
      const A = x[4];
      if (!A.v) return null;
      const chain = chainTo(A, x[0]);
      return { key: chain.at(-1)?.id ?? A.id, A, chain, from: A.name + (x[0] ? `::${x[0].replace(/\//g, "::")}` : "") };
    },
  );
  const made = [];
  for (const o of out) if (o.B !== crate) made.push(makeWire(proc, crate, o.B, "xref", `${o.refs} references`, o.refs, { chainA: own, chainB: o.chain }, { cross: "out", subject: n, label$: `${o.refs} refs → ${o.to}` }));
  for (const i of inc) if (i.A !== crate) made.push(makeWire(proc, i.A, crate, "xref", `${i.refs} references`, i.refs, { chainA: i.chain, chainB: own }, { cross: "in", subject: n, label$: `${i.from} → ${i.refs} refs` }));
  return made;
}

let spotSeq = 0;
const wireKey$ = () => `${focus.id}|${deepSubject()?.id ?? ""}|${opened.size}|${spot ? (spot.seq ??= ++spotSeq) : 0}`;
function syncWires() {
  if (wireBuiltFor !== wireKey$()) rebuildLinks();
}
// During a tour's path view or a feature spotlight, the cables that join the
// lit parts: any link whose two ends each hold a lit part (followed down into
// the open boxes to that part), plus cross-crate references between lit parts.
function spotWires() {
  const lit = [...spot.ids].map((id) => n$(id)).filter((n) => n?.v);
  const holds = new Map(); // node → first lit part inside it (or itself)
  for (const n of lit) for (let p = n; p; p = p.parent) if (!holds.has(p)) holds.set(p, n);
  const down = (top, n) => {
    const chain = [];
    for (let p = n; p && p !== top; p = p.parent) chain.unshift(p);
    return chain.every((c) => c.v) ? chain : [];
  };
  const made = new Set();
  for (const owner of opened) {
    for (const [a, b, kind, say] of owner.links ?? []) {
      const A = owner.children.find((c) => c.id === a);
      const B = owner.children.find((c) => c.id === b);
      if (!A?.v || !B?.v || !holds.has(A) || !holds.has(B)) continue;
      const weight = kind === "dep" ? Number(String(say).split(" ")[0]) || 1 : 1;
      made.add(`${A.id}>${B.id}`);
      makeWire(owner, A, B, kind ?? "dep", say ?? "depends on", weight, { chainA: down(A, holds.get(A)), chainB: down(B, holds.get(B)) }, { level: 1, spot: true, tint: tourLabels.get(holds.get(B).id)?.color });
    }
  }
  // cross-crate references between lit parts in different crates of one process
  const related = (x, y) => x === y || x.startsWith(`${y}/`) || y.startsWith(`${x}/`) || !x || !y;
  for (const n of lit) {
    const crate = crateOf(n);
    const proc = crate?.parent;
    if (!crate || !opened.has(proc)) continue;
    let m = n;
    while (m.id.includes("#")) m = m.parent;
    const rel = m === crate ? "" : relOf(m, crate);
    for (const other of lit) {
      const oc = crateOf(other);
      if (!oc || oc === crate || oc.parent !== proc || made.has(`${crate.id}>${oc.id}:${n.id}`)) continue;
      let o = other;
      while (o.id.includes("#")) o = o.parent;
      const orel = o === oc ? "" : relOf(o, oc);
      const refs = (crate.xlinks ?? []).filter((x) => x[1] === oc.name && related(x[0], rel) && related(x[2], orel)).reduce((t, x) => t + x[3], 0);
      if (!refs) continue;
      made.add(`${crate.id}>${oc.id}:${n.id}`);
      makeWire(proc, crate, oc, "xref", `${refs} references`, refs, { chainA: down(crate, m), chainB: down(oc, o) }, { level: 1, spot: true, tint: tourLabels.get(other.id)?.color, label$: `${m.name} → ${o.name} · ${refs} refs` });
    }
  }
}
function restyleWires() {
  syncWires();
  const hot = selected ?? hovered;
  const anyHot = Boolean(hot || hoveredWire);
  for (const w of wires) {
    const style = WIRE[w.kind] ?? WIRE.dep;
    const touching = w.cross || w === hoveredWire || (hot && (hot === w.A || hot === w.B || hot === w.from || hot === w.to));
    const out = w.cross ? w.cross === "out" : hot && (hot === w.A || hot === w.from);
    w.hot = Boolean(touching) || Boolean(w.spot);
    w.group.visible = !spot || Boolean(w.spot);
    const op = touching || w.spot ? 1 : anyHot ? w.level * 0.22 : w.level * 0.8;
    w.mat.color.setHex(touching ? (out || w === hoveredWire ? skin.accent : skin.inbound) : w.spot ? w.tint ?? skin.accent : w.level < 0.5 ? skin.ghost : style.color());
    for (const m of w.meshes) {
      if (m.material !== w.mat) m.material.color.copy(w.mat.color);
      m.material.opacity = op;
      if (style.dash) m.material.alphaTest = op * 0.5;
      m.material.depthWrite = op > 0.6;
      m.renderOrder = touching ? 3 : 1;
    }
    w.jackMat.color.setHex(touching ? w.mat.color.getHex() : skin.ink);
    w.jackMat.opacity = Math.min(1, op * 1.1);
    for (const x of w.xrays) {
      x.visible = Boolean(touching || w.spot);
      x.material.color.copy(w.mat.color);
    }
  }
  updateWireKey();
}

// packets: beads that run along cables from caller to callee
const PACKETS = new THREE.InstancedMesh(new THREE.SphereGeometry(1, 10, 8), new THREE.MeshBasicMaterial({ transparent: true, depthWrite: false }), 1600);
PACKETS.frustumCulled = false;
PACKETS.renderOrder = 4;
scene.add(PACKETS);
const _m = new THREE.Matrix4();
const _c = new THREE.Color();
function animatePackets() {
  let k = 0;
  const t = now() / 1000;
  const anyHot = wires.some((w) => w.hot);
  for (const w of wires) {
    if (!w.group.visible || (WIRE[w.kind] ?? WIRE.dep).still) continue;
    const busy = w.hot ? 1 : anyHot || LITE ? 0 : w.level === 1 ? 0.5 : 0;
    if (!busy) continue;
    const spacing = w.unit * (w.spot ? 18 : w.hot ? 7 : 14);
    const count = Math.max(1, Math.min(w.hot ? 24 : 8, Math.round(w.len / spacing)));
    const speed = w.unit * (w.hot ? 9 : 5);
    const grow = w.hot ? 1.9 : 1.6;
    _c.setHex(w.hot ? skin.packetHot : skin.packet);
    for (let i = 0; i < count && k < PACKETS.count; i++) {
      const u = (((t * speed) / w.len + i / count) % 1 + 1) % 1;
      const s = w.samples;
      const f = u * (s.length - 1);
      const a = s[Math.floor(f)];
      const b = s[Math.min(s.length - 1, Math.floor(f) + 1)];
      const size = w.sampleR[Math.floor(f)] * grow;
      _m.makeScale(size, size, size).setPosition(a.x + (b.x - a.x) * (f % 1), a.y + (b.y - a.y) * (f % 1), a.z + (b.z - a.z) * (f % 1));
      PACKETS.setMatrixAt(k, _m);
      PACKETS.setColorAt(k, _c);
      k++;
    }
  }
  PACKETS.count = k;
  PACKETS.instanceMatrix.needsUpdate = true;
  if (PACKETS.instanceColor) PACKETS.instanceColor.needsUpdate = true;
}
PACKETS.count = 0;

// the key to cable styles, drawn in the current skin
const wireKey = document.getElementById("wirekey");
function updateWireKey() {
  if (!wireKey) return;
  const show = !touring && wires.some((w) => w.group.visible);
  wireKey.hidden = !show;
  if (!show) return;
  const h = (c) => `#${new THREE.Color(c).getHexString()}`;
  const line = (c, dash) => `<svg width="34" height="8" viewBox="0 0 34 8"><line x1="1" y1="4" x2="33" y2="4" stroke="${h(c)}" stroke-width="3" stroke-linecap="round"${dash ? ` stroke-dasharray="${dash}"` : ""}/></svg>`;
  const key = `${skin.link}|${skin.power}|${skin.accent}|${skin.inbound}|${skin.packet}`;
  if (wireKey.dataset.key === key) return;
  wireKey.dataset.key = key;
  wireKey.innerHTML = [
    `<i>${line(skin.link)} calls · uses</i>`,
    `<i>${line(skin.power, "7 5")} starts · supervises</i>`,
    `<i>${line(0x7c3aed)} hosts</i>`,
    `<i>${line(skin.ghost, "2 4")} planned</i>`,
    `<i><svg width="34" height="8" viewBox="0 0 34 8"><line x1="1" y1="4" x2="33" y2="4" stroke="${h(skin.link)}" stroke-width="3" stroke-linecap="round"/>${[6, 17, 28].map((x) => `<circle cx="${x}" cy="4" r="2.4" fill="${h(skin.packet)}" stroke="${h(skin.ink)}" stroke-width="0.6"/>`).join("")}</svg> flows caller → callee</i>`,
    `<i>${line(skin.accent)} out of selection</i>`,
    `<i>${line(skin.inbound)} into selection</i>`,
    `<i class="w">thicker = more references</i>`,
  ].join("");
}

// ---------- focus styling: the focused branch is solid, the rest is wireframe ----------
let spot = null; // { feature, ids: Set } while a feature is spotlit
const inFocus = (n) => {
  if (spot) {
    if (spot.exact) return spot.ids.has(n.id); // a path view: only the hops themselves
    for (let p = n; p; p = p.parent) if (spot.ids.has(p.id)) return true;
    return false;
  }
  for (let p = n; p; p = p.parent) if (p === focus) return true;
  return false;
};
// solid in the focused branch, wireframe elsewhere; the lid also fades with openness
function applyAlpha(n) {
  const v = n.v;
  if (!v) return;
  const solid = inFocus(n);
  const hot = n === hovered || n === selected;
  const open = v.dims ? v.t : 0;
  const tune = (set, k) => {
    for (const key of ["body", "detail", "floor", "led", "screw"]) {
      set[key].opacity = (solid ? 1 : 0) * k;
      set[key].depthWrite = solid && k > 0.98;
    }
    if (set.engrave) {
      set.engrave.color.setHex(skin.ink);
      set.engrave.opacity = (solid ? 0.8 : 0.2) * k;
    }
    if (set.mascot) {
      const tex = mascotTexture();
      if (set.mascot.map !== tex) (set.mascot.map = tex), (set.mascot.needsUpdate = true);
      set.mascot.opacity = (solid ? 1 : 0.25) * k;
    }
    set.line.color.setHex(hot ? skin.accent : solid ? skin.ink : skin.ghost);
    set.line.opacity = (hot ? 1 : solid ? 0.85 : 0.26) * k;
    set.groove.opacity = (solid ? 0.3 : 0) * k;
  };
  tune(v.m, 1);
  tune(v.lm, 1 - open);
  v.halo.opacity = solid ? (skin.haloAlpha ?? 0.22) * (1 - open) : 0;
  v.shellMat.opacity = (n === focus ? 0.45 : 0.14) * open;
}
function restyle() {
  poke();
  for (const n of NODES.values()) {
    if (!n.v) continue;
    const v = n.v;
    const hot = n === hovered || n === selected;
    const base = new THREE.Color(colorFor(n));
    const body = hot ? base.clone().lerp(new THREE.Color(skin.accent), 0.2) : base;
    for (const set of [v.m, v.lm]) {
      set.body.color.copy(body);
      set.detail.color.setHex(skin.detail);
      set.floor.color.setHex(skin.floor);
      set.led.color.setHex(skin.led);
      set.screw.color.setHex(skin.ink);
      set.groove.color.setHex(skin.ink);
    }
    v.halo.color.setHex(skin.halo ?? 0x000000);
    v.shellMat.color.setHex(skin.ghost);
    applyAlpha(n);
  }
  restyleWires();
  updateLabels();
  updateCrumbs();
  updatePanel();
}

// ---------- labels ----------
// Path and feature views can put many numbered labels on a small screen.
// After the next draw, labels are kept inside the screen and any that would
// overlap an earlier one shrink to their number (the caption legend names
// the groups; hover or tap shows the rest).
let declutterDue = false;
function declutter() {
  declutterDue = false;
  if (!tourLabels.size) return hideOverlaps();
  const els = [...tourLabels.keys()].map((id) => n$(id)?.label?.element).filter((el) => el?.isConnected).sort((a, b) => Number(a.dataset.num) - Number(b.dataset.num));
  const placed = [];
  const W = innerWidth;
  const H = innerHeight;
  for (const el of els) {
    el.classList.remove("num");
    el.style.marginLeft = el.style.marginTop = "";
  }
  for (const el of els) {
    let r = el.getBoundingClientRect();
    if (r.right < 0 || r.left > W || r.bottom < 0 || r.top > H) continue;
    if (placed.some((p) => r.left < p.right && r.right > p.left && r.top < p.bottom && r.bottom > p.top)) {
      el.classList.add("num");
      r = el.getBoundingClientRect();
    }
    const dx = r.left < 6 ? 6 - r.left : r.right > W - 6 ? W - 6 - r.right : 0;
    const dy = r.top < uiTop() ? uiTop() - r.top : 0;
    if (dx || dy) {
      el.style.marginLeft = `${dx}px`;
      el.style.marginTop = `${dy}px`;
      r = { left: r.left + dx, right: r.right + dx, top: r.top + dy, bottom: r.bottom + dy };
    }
    placed.push(r);
  }
}
// Outside tours, a label that would sit on top of a bigger box's label is
// hidden; hover or tap still names that box.
function hideOverlaps() {
  const shown = [...NODES.values()].filter((n) => n.label?.element?.isConnected).sort((a, b) => b.box.w * b.box.d - a.box.w * a.box.d);
  const placed = [];
  for (const n of shown) n.label.element.style.visibility = "";
  for (const n of shown) {
    const el = n.label.element;
    const r = el.getBoundingClientRect();
    if (n === hovered || n === selected || !placed.some((p) => r.left < p.right - 2 && r.right > p.left + 2 && r.top < p.bottom - 1 && r.bottom > p.top + 1)) placed.push(r);
    else el.style.visibility = "hidden";
  }
}
function updateLabels() {
  declutterDue = true;
  poke(300);
  for (const n of NODES.values()) {
    if (!n.v) continue;
    const onPath = typeof tourLabels !== "undefined" && tourLabels.size ? tourLabels.get(n.id) : null;
    const pathMode = typeof tourLabels !== "undefined" && tourLabels.size > 0;
    const want = pathMode ? Boolean(onPath) || n === hovered : n.parent === focus || (n === hovered && n !== focus) || n === selected;
    const onScreen = Math.min(n.box.w, n.box.d) * viewScale();
    const big = pathMode || n.parent !== focus || n.parent.kind === "system" || onScreen > 34 + n.name.length * 2.2 || n.parent.children.indexOf(n) < 3;
    if (want && big) {
      if (!n.label) {
        const el = document.createElement("div");
        el.className = `tag tag-${n.kind}`;
        el.textContent = n.name;
        n.label = new CSS2DObject(el);
        world.add(n.label);
      }
      const b = n.box;
      n.label.position.set(b.x + b.w / 2, b.y + b.h, b.z + b.d / 2);
      if (onPath) {
        n.label.element.dataset.num = onPath.num;
        n.label.element.textContent = `${onPath.num} · ${n.name}`;
        n.label.element.style.setProperty("--c", `#${onPath.color.toString(16).padStart(6, "0")}`);
        n.label.element.classList.add("pathtag");
      } else if (n.label.element.classList.contains("pathtag")) {
        n.label.element.textContent = n.name;
        n.label.element.classList.remove("pathtag");
      }
      n.label.element.classList.toggle("hot", n === hovered || n === selected);
    } else if (n.label) {
      n.label.removeFromParent();
      n.label = null;
    }
  }
}

// ---------- panel and breadcrumb ----------
const KIND = { server: "Server", client: "Client", system: "System", process: "Process", crate: "Crate", module: "Module", file: "Source file", config: "Config file", key: "Config key", daemon: "Daemon", struct: "Struct", enum: "Enum", trait: "Trait" };
const MEMBER = { struct: "fields", enum: "variants", trait: "methods" };
// Two lines at the bottom: identity and counts, then role, links and the action.
const stat = (label, value) => (value ? `<span class="st"><b>${fmt(value)}</b> ${label}</span>` : "");
function updatePanel() {
  const n = selected ?? hovered ?? focus;
  const stats = [n.languages ? `<span class="st">${n.languages.join(" · ")}</span>` : "", stat("lines", n.lines), stat("types", n.types), n.kind === "file" ? "" : stat("files", n.files), stat("crates", n.crates), stat("structs", n.structs), stat("enums", n.enums), stat("traits", n.traits), stat("fns", n.fns), stat("tests", n.tests), n.children.length ? stat(n.kind === "system" ? "parts" : n.kind === "server" ? "processes" : "inside", n.children.length) : ""].join("");
  const deps = n.kind === "crate" ? n.deps : n.implements ?? [];
  const impls = n.implementedBy ?? [];
  const users = n.kind === "crate" ? [...(usedBy[n.id] ?? [])] : impls.filter((id) => !NODES.get(id)?.test);
  const doubles = n.kind === "crate" ? [] : impls.filter((id) => NODES.get(id)?.test);
  const nameOf = (id) => NODES.get(id)?.name ?? id;
  const pop = (label, list) => (list.length ? `<span class="pop"><button class="link" type="button">${label} ${list.length}</button><span class="list">${list.map((id) => `<button class="chip" data-go="${id}">${nameOf(id)}</button>`).join("")}</span></span>` : "");
  const memberStat = MEMBER[n.kind] ? stat(MEMBER[n.kind], n.members) : "";
  panel.innerHTML = `
    <div class="l1"><span class="kind">${KIND[n.kind] ?? n.kind}${n.port ? ` · ${n.port}` : ""}</span><b class="nm">${n.name}</b>${n.test ? '<span class="kind">test</span>' : ""}${n.path ? `<code>${n.path}</code>` : ""}<span class="sts">${memberStat}${stats}</span></div>
    <div class="l2">${n.role ? `<span class="role">${n.role}</span>` : ""}${pop(n.kind === "crate" ? "uses" : "implements", deps)}${pop(n.kind === "crate" ? "used by" : "implemented by", users)}${pop("test doubles", doubles)}${n.children.length ? `<button class="act" data-act="${opened.has(n) ? "close" : "open"}" data-id="${n.id}">${opened.has(n) ? "Close it up" : "Break it open"}</button>` : ""}</div>`;
}
panel.addEventListener("click", (e) => {
  const go = e.target.closest("[data-go]");
  if (go) return (stopTour(), goTo(NODES.get(go.dataset.go)));
  const act = e.target.closest("[data-act]");
  if (!act) return;
  stopTour();
  const n = NODES.get(act.dataset.id);
  if (act.dataset.act === "open") (open(n), setFocus(n));
  else (close(n), setFocus(n.parent ?? n));
});
function updateCrumbs() {
  const path = [];
  for (let p = focus; p; p = p.parent) path.unshift(p);
  crumbs.innerHTML = path.map((p, i) => `<button data-crumb="${p.id}"${i === path.length - 1 ? ' aria-current="true"' : ""}>${p.name}</button>`).join("<span>›</span>");
}
crumbs.addEventListener("click", (e) => {
  const b = e.target.closest("[data-crumb]");
  if (b) (stopTour(), setFocus(NODES.get(b.dataset.crumb)));
});

// ---------- camera ----------
function boundsOf(n) {
  const b = n.box;
  const box = new THREE.Box3(new THREE.Vector3(b.x, b.y, b.z), new THREE.Vector3(b.x + b.w, b.y + b.h, b.z + b.d));
  if (opened.has(n)) for (const c of n.children) if (c.box) box.expandByPoint(new THREE.Vector3(c.box.x, c.box.y + c.box.h * 1.6, c.box.z));
  return box;
}
let panelSpace = 0;
// Fit a box's real on-screen outline (not a bounding sphere) into the space
// between the top bar and the narration strip, from a given view direction.
// space the bar and the bottom strip actually take, measured (they wrap on phones)
const uiTop = () => Math.max(36, (document.querySelector(".bar")?.getBoundingClientRect().bottom ?? 44) + 6);
const uiBottom = () => {
  const el = document.body.classList.contains("captioned") ? caption : panel;
  const r = el?.getBoundingClientRect();
  return r && r.height ? Math.max(40, innerHeight - r.top + 6) : 76;
};
function fitView(box, dir) {
  const fwd = dir.clone().normalize().negate();
  const right = new THREE.Vector3().crossVectors(fwd, new THREE.Vector3(0, 1, 0)).normalize();
  const up = new THREE.Vector3().crossVectors(right, fwd).normalize();
  const c = box.getCenter(new THREE.Vector3());
  let minR = Infinity, maxR = -Infinity, minU = Infinity, maxU = -Infinity;
  for (const x of [box.min.x, box.max.x]) for (const y of [box.min.y, box.max.y]) for (const z of [box.min.z, box.max.z]) {
    const p = new THREE.Vector3(x, y, z).sub(c);
    const r = p.dot(right);
    const u = p.dot(up);
    minR = Math.min(minR, r); maxR = Math.max(maxR, r); minU = Math.min(minU, u); maxU = Math.max(maxU, u);
  }
  const clean = document.body.classList.contains("clean");
  const W = stage.clientWidth - 32;
  const UI_TOP = uiTop();
  const UI_BOTTOM = uiBottom();
  const H = stage.clientHeight - (clean ? 32 : UI_TOP + UI_BOTTOM + 16);
  let zoom = Math.min(W / Math.max(1e-6, maxR - minR), H / Math.max(1e-6, maxU - minU));
  // centre the outline; aim a little low so it sits between the bar and the strip
  const shift = clean ? 0 : (UI_BOTTOM - UI_TOP) / 2 / zoom;
  const center = c.clone().add(right.clone().multiplyScalar((minR + maxR) / 2)).add(up.clone().multiplyScalar((minU + maxU) / 2 - shift));
  if (ORTHO) return { center, zoom };
  // Through a lens the near corners come out bigger than the parallel outline
  // above, so measure the real projected outline and correct size and centre.
  const lift = clean ? 0 : (UI_BOTTOM - UI_TOP) / 2; // where the outline should sit, px above the middle
  const cam = new THREE.PerspectiveCamera(LENS, stage.clientWidth / Math.max(1, stage.clientHeight), 0.01, 1e6);
  const p = new THREE.Vector3();
  for (let i = 0; i < 4; i++) {
    cam.position.copy(center).add(dir.clone().normalize().multiplyScalar(distFor(zoom)));
    cam.up.set(0, 1, 0);
    cam.lookAt(center);
    cam.updateMatrixWorld();
    let x0 = Infinity, x1 = -Infinity, y0 = Infinity, y1 = -Infinity;
    for (const x of [box.min.x, box.max.x]) for (const y of [box.min.y, box.max.y]) for (const z of [box.min.z, box.max.z]) {
      p.set(x, y, z).project(cam);
      const px = (p.x * stage.clientWidth) / 2;
      const py = (p.y * stage.clientHeight) / 2;
      x0 = Math.min(x0, px); x1 = Math.max(x1, px); y0 = Math.min(y0, py); y1 = Math.max(y1, py);
    }
    // slide the target so the outline's middle lands where it should
    center.add(right.clone().multiplyScalar((x0 + x1) / 2 / zoom)).add(up.clone().multiplyScalar(((y0 + y1) / 2 - lift) / zoom));
    zoom /= Math.max((x1 - x0) / W, (y1 - y0) / H);
  }
  return { center, zoom };
}
function frame(n, dur = 1100, { azimuth = null, elevation = null } = {}) {
  const fromT = controls.target.clone();
  const fromZ = viewScale();
  const sph = new THREE.Spherical().setFromVector3(camera.position.clone().sub(fromT));
  const toSph = sph.clone();
  if (azimuth !== null) toSph.theta = azimuth;
  if (elevation !== null) toSph.phi = Math.PI / 2 - elevation;
  if (toSph.theta - sph.theta > Math.PI) toSph.theta -= 2 * Math.PI;
  if (sph.theta - toSph.theta > Math.PI) toSph.theta += 2 * Math.PI;
  const { center: toT, zoom } = fitView(boundsOf(n), new THREE.Vector3().setFromSpherical(new THREE.Spherical(1, toSph.phi, toSph.theta)));
  tween(dur, (e) => {
    const s = new THREE.Spherical(1, sph.phi + (toSph.phi - sph.phi) * e, sph.theta + (toSph.theta - sph.theta) * e);
    aim(new THREE.Vector3().lerpVectors(fromT, toT, e), new THREE.Vector3().setFromSpherical(s), fromZ * Math.pow(zoom / fromZ, e));
  }, () => updateLabels());
}
function setFocus(n, { fly = true, ...view } = {}) {
  focus = n;
  selected = null;
  restyle();
  if (fly) frame(n, 1100, view);
}
function goTo(n, view = {}) {
  if (!n) return;
  if (n.parent) open(n.parent);
  if (n.children.length) {
    open(n);
    setFocus(n, view);
  } else {
    setFocus(n.parent, view);
    selected = n;
    restyle();
  }
}

// ---------- picking ----------
const ray = new THREE.Raycaster();
const pointer = new THREE.Vector2();
let downAt = null;
function pick(e) {
  const r = renderer.domElement.getBoundingClientRect();
  pointer.set(((e.clientX - r.left) / r.width) * 2 - 1, -((e.clientY - r.top) / r.height) * 2 + 1);
  ray.setFromCamera(pointer, camera);
  const targets = [];
  for (const n of NODES.values()) if (n.v) targets.push(opened.has(n) ? n.v.pickOpen : n.v.pick);
  const hits = ray.intersectObjects(targets.filter(Boolean), false);
  const solid = hits.find((h) => inFocus(h.object.userData.node));
  const hit = solid ?? hits[0];
  pickDistance = hit?.distance ?? Infinity;
  return hit?.object.userData.node ?? null;
}
// a cable under the pointer, when it is nearer than any box (call after pick)
let pickDistance = Infinity;
function pickWire() {
  const meshes = wires.filter((w) => w.group.visible && w.mat.opacity > 0.15).flatMap((w) => w.meshes);
  const hit = meshes.length ? ray.intersectObjects(meshes, false)[0] : null;
  return hit && hit.distance <= pickDistance + hit.object.userData.wire.r * 4 ? hit.object.userData.wire : null;
}
function showWireTip(w, e) {
  if (dragging) return (tip.hidden = true);
  const style = WIRE[w.kind] ?? WIRE.dep;
  const from = w.from ?? w.A;
  const to = w.to ?? w.B;
  tip.innerHTML = `<small>Wire · ${style.say}</small><b>${from.name} → ${to.name}</b>${w.label$ ? `<p>${w.label$}</p>` : `<p>${w.say}</p>`}`;
  tip.hidden = false;
  const x = Math.min(e.clientX + 16, window.innerWidth - tip.offsetWidth - 12);
  const y = Math.min(e.clientY + 18, window.innerHeight - tip.offsetHeight - 12);
  tip.style.transform = `translate(${x}px, ${y}px)`;
}
const tip = document.getElementById("tip");
// What a box was built from: the crate's Cargo version, the newest commit under
// it (hash and local time), whether this build includes uncommitted edits there,
// and when the explorer itself was built.
const when = (iso) =>
  new Date(iso).toLocaleString(undefined, { day: "numeric", month: "short", year: "numeric", hour: "2-digit", minute: "2-digit" });
const BUILT = `Built ${when(DATA.builtAt)} from ${DATA.builtFrom}`;
function provenance(n) {
  const parts = [];
  if (n.version) parts.push(`v${n.version}`);
  const c = n.lc !== undefined ? DATA.changes[n.lc] : null;
  if (c) parts.push(`last commit ${c[0]} · ${when(c[1])}`);
  if (n.dirty) parts.push("+ uncommitted edits");
  return `<div class="meta">${parts.length ? `<span>${parts.join(" · ")}</span>` : ""}<span>${BUILT}</span></div>`;
}
function showTip(n, e) {
  if (!n || dragging) return (tip.hidden = true);
  const facts = [["lines", n.lines], ["files", n.kind === "file" ? 0 : n.files], ["types", n.types], [MEMBER?.[n.kind] ?? "", n.members], ["structs", n.structs], ["enums", n.enums], ["traits", n.traits], ["fns", n.fns], ["tests", n.tests]].filter(([k, v]) => k && v);
  tip.innerHTML = `<small>${KIND[n.kind] ?? n.kind}${n.test ? " · test" : ""}</small><b>${n.name}</b>${facts.length ? `<div>${facts.map(([k, v]) => `<span><i>${fmt(v)}</i> ${k}</span>`).join("")}</div>` : ""}${n.role ? `<p>${n.role}</p>` : ""}${provenance(n)}`;
  tip.hidden = false;
  const x = Math.min(e.clientX + 16, window.innerWidth - tip.offsetWidth - 12);
  const y = Math.min(e.clientY + 18, window.innerHeight - tip.offsetHeight - 12);
  tip.style.transform = `translate(${x}px, ${y}px)`;
}
let dragging = false;
let armed = null; // touch: the box or cable the first tap lit up
let tipTimer = 0;
renderer.domElement.addEventListener("pointerdown", () => ((dragging = true), (tip.hidden = true)));
window.addEventListener("pointerup", () => (dragging = false));
renderer.domElement.addEventListener("pointerleave", () => (tip.hidden = true));
renderer.domElement.addEventListener("pointermove", (e) => {
  if (e.pointerType === "touch") return; // touch has no hover: a first tap stands in for it
  const found = pick(e);
  const w = pickWire();
  if (w) showWireTip(w, e);
  else showTip(found, e);
  if (touring) return;
  const n = w ? null : found;
  if (n !== hovered || w !== hoveredWire) {
    hovered = n;
    hoveredWire = w;
    renderer.domElement.style.cursor = n || w ? "pointer" : "";
    restyle();
  }
});
renderer.domElement.addEventListener("pointerdown", (e) => {
  downAt = [e.clientX, e.clientY];
  if (touring) pauseTour();
});
renderer.domElement.addEventListener("pointerup", (e) => {
  if (!downAt || Math.hypot(e.clientX - downAt[0], e.clientY - downAt[1]) > 5 || e.button !== 0) return;
  const n = pick(e);
  const w = pickWire();
  if (e.pointerType === "touch") {
    const target = w ?? n;
    if (target && armed !== target) {
      armed = target;
      dragging = false;
      hovered = w ? null : n;
      hoveredWire = w ?? null;
      if (w) showWireTip(w, e);
      else showTip(n, e);
      clearTimeout(tipTimer);
      tipTimer = setTimeout(() => (tip.hidden = true), 4500);
      restyle();
      return;
    }
    armed = null;
    tip.hidden = true;
    if (!target) {
      if (hovered || hoveredWire) (hovered = null), (hoveredWire = null), restyle();
      return;
    }
  }
  if (w) {
    // a click on a cable selects the end you are not looking at
    const far = selected === (w.to ?? w.B) || focus === (w.to ?? w.B) ? (w.from ?? w.A) : (w.to ?? w.B);
    return goTo(far);
  }
  if (!n) return;
  if (n.children.length) {
    if (opened.has(n) && focus === n) {
      close(n);
      setFocus(n.parent ?? n);
    } else {
      if (!opened.has(n)) open(n);
      setFocus(n);
    }
  } else if (selected === n) {
    selected = null;
    restyle();
  } else {
    if (!inFocus(n)) setFocus(n.parent);
    selected = n;
    restyle();
  }
});
renderer.domElement.addEventListener("wheel", () => touring && pauseTour(), { passive: true });
renderer.domElement.addEventListener("dblclick", () => {
  if (focus !== DATA.tree) {
    const up = focus.parent;
    close(focus);
    setFocus(up);
  }
});
window.addEventListener("keydown", (e) => {
  if (e.target.closest?.("input, select, textarea, button, a") || !["Escape", "Backspace", "Home"].includes(e.key)) return;
  stopTour();
  if (e.key === "Escape" && focus.parent) setFocus(focus.parent);
  if (e.key === "Backspace" && focus !== DATA.tree) (close(focus), setFocus(focus.parent ?? DATA.tree));
  if (e.key === "Home") reset();
});

// ---------- tools ----------
let AZ_SPH = new THREE.Spherical().setFromVector3(VIEW).theta;
// The model is wide; on a tall screen the classic 55°/30° view leaves big
// empty bands. Try a few steeper views turned toward the long side and keep
// the first that fills the screen clearly better (12%) than the one before.
const VIEWS = [[55, 30], [36, 40], [22, 48], [12, 56]];
function chooseView() {
  const box = new THREE.Box3(new THREE.Vector3(ROOT_BOX.x, ROOT_BOX.y, ROOT_BOX.z), new THREE.Vector3(ROOT_BOX.x + ROOT_BOX.w, ROOT_BOX.y + ROOT_BOX.h, ROOT_BOX.z + ROOT_BOX.d));
  let best = null;
  for (const [az, el] of VIEWS) {
    const a = (az * Math.PI) / 180;
    const e = (el * Math.PI) / 180;
    const dir = new THREE.Vector3(Math.cos(e) * Math.cos(a), Math.sin(e), Math.cos(e) * Math.sin(a));
    const { zoom } = fitView(box, dir);
    if (!best || zoom > best.zoom * 1.12) best = { a, e, dir, zoom };
  }
  AZ = best.a;
  EL = best.e;
  VIEW.copy(best.dir);
  AZ_SPH = new THREE.Spherical().setFromVector3(VIEW).theta;
}
function reset() {
  close(DATA.tree);
  focus = DATA.tree;
  selected = null;
  restyle();
  frame(DATA.tree, 1100, { azimuth: AZ_SPH, elevation: EL });
}
document.getElementById("home").addEventListener("click", () => frame(focus, 900, { azimuth: AZ_SPH, elevation: EL }));
function expandAll() {
  open(DATA.tree);
  for (const p of DATA.tree.children) open(p);
  for (const p of DATA.tree.children) for (const c of p.children) if (c.kind === "crate") open(c, { animate: false });
  setFocus(DATA.tree, { azimuth: AZ_SPH, elevation: EL });
}
// one button: "Expand all" while sealed, "Seal" once anything is open
const expandButton = document.getElementById("expand");
function syncExpand() {
  const sealed = !opened.has(DATA.tree);
  expandButton.textContent = sealed ? "Expand all" : "Seal";
  expandButton.title = sealed ? "Open every level down to crates and their top modules" : "Close everything";
}
expandButton.addEventListener("click", () => {
  stopTour();
  if (opened.has(DATA.tree)) reset();
  else expandAll();
});
const search = document.getElementById("search");
const results = document.getElementById("results");
const searchToggle = document.getElementById("searchToggle");
const headerSearch = document.getElementById("headerSearch");
function setSearchOpen(open) {
  if (open) {
    setExploreOpen(false);
    setMenu(false);
  }
  document.body.classList.toggle("search-open", open);
  headerSearch.hidden = !open;
  searchToggle.setAttribute("aria-expanded", String(open));
  if (open) search.focus({ preventScroll: true });
  else {
    search.value = "";
    results.hidden = true;
    results.innerHTML = "";
    searchToggle.focus({ preventScroll: true });
  }
  poke();
}
searchToggle.addEventListener("click", () => setSearchOpen(true));
document.getElementById("searchCancel").addEventListener("click", () => setSearchOpen(false));
const INDEX = [...NODES.values()].filter((n) => n.id !== "root").map((n) => ({ n, key: `${n.name} ${n.path ?? ""} ${n.id}`.toLowerCase() }));
search.addEventListener("input", () => {
  const q = search.value.trim().toLowerCase();
  if (q.length < 2) return (results.innerHTML = ""), (results.hidden = true);
  const hits = INDEX.filter((e) => e.key.includes(q))
    .sort((a, b) => (a.n.name.toLowerCase().startsWith(q) ? 0 : 1) - (b.n.name.toLowerCase().startsWith(q) ? 0 : 1) || b.n.lines - a.n.lines)
    .slice(0, 14);
  results.innerHTML = hits.map(({ n }) => `<button data-go="${n.id}"><span>${n.name}</span><small>${KIND[n.kind]} · ${n.path ?? processOf(n)?.name ?? ""} · ${fmt(n.lines)} lines</small></button>`).join("") || `<div class="none">No match</div>`;
  results.hidden = false;
});
search.addEventListener("keydown", e => {
  if (e.isComposing || results.hidden) return;
  const first = results.querySelector("[data-go]");
  if (e.key === "Enter" && first) (e.preventDefault(), first.click());
  else if (e.key === "ArrowDown" && first) (e.preventDefault(), first.focus());
});
results.addEventListener("keydown", e => {
  if (!["ArrowUp", "ArrowDown"].includes(e.key)) return;
  const buttons = [...results.querySelectorAll("[data-go]")];
  const index = buttons.indexOf(e.target.closest("[data-go]"));
  if (index < 0) return;
  e.preventDefault();
  if (e.key === "ArrowUp" && index === 0) search.focus();
  else buttons[Math.min(buttons.length - 1, index + (e.key === "ArrowDown" ? 1 : -1))].focus();
});
results.addEventListener("click", (e) => {
  const b = e.target.closest("[data-go]");
  if (!b) return;
  setSearchOpen(false);
  stopTour();
  goTo(NODES.get(b.dataset.go));
});
document.getElementById("skin").addEventListener("change", (e) => applySkin(e.target.value));
function applySkin(id) {
  if (id === "brand") id = "sorbet"; // the skin's earlier name, still in some saved settings
  if (!SKINS[id]) id = "paper";
  skin = SKINS[id];
  document.getElementById("skin").value = id;
  document.body.dataset.skin = id;
  scene.background = RECORDING ? new THREE.Color(skin.bg) : null;
  ambient.setTheme(id);
  rebuildLinks();
  restyle();
  try {
    localStorage.setItem("magician-anatomy-skin", id);
  } catch {}
  document.getElementById("skin").value = id;
}

// ---------- the dynamic tour: one branching graph, walked as the viewer chooses ----------
let touring = false;
let orbiting = 0;
const T = DATA.tree;
const n$ = (id) => NODES.get(id);
const G = DATA.graph ?? { nodes: {}, start: null };
const MEDIA = "media/";
// narration docks into the bottom strip: subtitles, never a card over the scene
const say = (title, body = "", progress = "", cite = "", extra = "") => {
  caption.innerHTML = title ? `<div class="cap-top">${progress ? `<small title="${progress.replace(/"/g, "&quot;")}">${progress}</small>` : ""}${cite ? `<code title="${cite.replace(/"/g, "&quot;")}">${cite}</code>` : ""}${transportHtml()}</div><div class="cap-line"><b>${title}</b>${body ? `<span>${body}</span>` : ""}</div>${extra}` : "";
  caption.toggleAttribute("data-on", Boolean(title));
  document.body.classList.toggle("captioned", Boolean(title));
};

// audio: a music bed that ducks under the narration. Served over http(s) the
// music runs through a Web Audio gain node, because iOS Safari ignores
// HTMLMediaElement.volume (the bed would play at full level over the voice).
// From file:// browsers silence media routed into Web Audio, so there the
// element's own volume does the work.
// voiceOff / musicOff are the viewer's own choices and are remembered; muted
// (the top bar's Sound switch) silences both without forgetting them
const audioPrefs = (() => {
  try {
    return JSON.parse(localStorage.getItem("anatomy.audio") ?? "{}");
  } catch {
    return {};
  }
})();
const audio = { music: null, gain: null, ctx: null, voice: null, clip: null, muted: false, voiceOff: Boolean(audioPrefs.voiceOff), musicOff: Boolean(audioPrefs.musicOff), level: 0, target: 0 };
const saveAudioPrefs = () => {
  try {
    localStorage.setItem("anatomy.audio", JSON.stringify({ voiceOff: audio.voiceOff, musicOff: audio.musicOff }));
  } catch {}
};
const voiceOn = () => !audio.muted && !audio.voiceOff;
const AudioCtx = /^https?:$/.test(location.protocol) ? window.AudioContext ?? window.webkitAudioContext : null;
function audioStart() {
  if (!audio.music) {
    audio.music = new Audio(`${MEDIA}tour-music.m4a`);
    audio.music.loop = true;
    audio.music.preload = "auto";
    if (AudioCtx) {
      try {
        audio.ctx = new AudioCtx();
        audio.gain = audio.ctx.createGain();
        audio.gain.gain.value = 0;
        audio.ctx.createMediaElementSource(audio.music).connect(audio.gain).connect(audio.ctx.destination);
      } catch {
        audio.ctx = audio.gain = null;
      }
    }
    if (!audio.gain) audio.music.volume = 0;
  }
  audio.ctx?.resume?.().catch?.(() => {}); // called from the click that starts the tour, which iOS requires
  audio.target = 0.28;
  audio.music.play().catch(() => {});
}
function audioStop() {
  audio.target = 0;
  if (audio.voice) audio.voice.pause();
  audio.voice = null;
}
function stepAudio(dt) {
  if (!audio.music) return;
  // a paused tour lets the bed fade out too, and it ducks under the voice
  const want = audio.muted || audio.musicOff || tour?.paused ? 0 : audio.voice && !audio.voice.paused ? audio.target * 0.35 : audio.target;
  audio.level += (want - audio.level) * Math.min(1, dt / 350);
  const level = Math.max(0, Math.min(1, audio.level));
  if (audio.gain) audio.gain.gain.value = level;
  else audio.music.volume = level;
  if (audio.level < 0.002 && audio.target === 0 && !audio.music.paused) audio.music.pause();
}
function speak(clip) {
  if (audio.voice) audio.voice.pause();
  audio.voice = null;
  audio.clip = clip ?? null;
  if (!clip || !voiceOn()) return;
  audio.voice = new Audio(`${MEDIA}${clip}`);
  audio.voice.addEventListener("error", () => (audio.voice = null), { once: true });
  audio.voice.play().catch(() => (audio.voice = null));
}

// colours: one per crate (or process) the path lands in, in order of first visit
const PALETTE = [0xe5484d, 0x0d9488, 0x7c3aed, 0xd97706, 0x2563eb, 0xdb2777, 0x65a30d, 0x0891b2, 0x9333ea, 0xb45309];
const hex = (c) => `#${c.toString(16).padStart(6, "0")}`;
function groupOf(n) {
  for (let p = n; p; p = p.parent) if (p.kind === "crate" || p.kind === "process" || p.kind === "client" || p.kind === "server") return p;
  return T;
}

// trails: screen-space lines (constant pixel width at any zoom) with a fixed-size head
const trails = new THREE.Group();
world.add(trails);
const OUTSIDE = new THREE.Vector3(0, 40, 320);
const anchorOf = (n) => new THREE.Vector3(n.box.x + n.box.w / 2, n.box.y + n.box.h, n.box.z + n.box.d / 2);
const DOT = (() => {
  const c = document.createElement("canvas");
  c.width = c.height = 64;
  const g = c.getContext("2d");
  const grad = g.createRadialGradient(32, 32, 0, 32, 32, 32);
  grad.addColorStop(0, "rgba(255,255,255,1)");
  grad.addColorStop(0.45, "rgba(255,255,255,0.95)");
  grad.addColorStop(1, "rgba(255,255,255,0)");
  g.fillStyle = grad;
  g.fillRect(0, 0, 64, 64);
  return new THREE.CanvasTexture(c);
})();
const lineMats = new Set();
const trailFx = []; // one entry per hop: { pts, curve, length, mat, glow, sparkle }
const RING = (() => {
  const c = document.createElement("canvas");
  c.width = c.height = 64;
  const g = c.getContext("2d");
  g.strokeStyle = "#fff";
  g.lineWidth = 5;
  g.beginPath();
  g.arc(32, 32, 26, 0, Math.PI * 2);
  g.stroke();
  return new THREE.CanvasTexture(c);
})();
function clearTrails() {
  trailFx.length = 0;
  replay = null;
  for (const g of [...trails.children]) {
    trails.remove(g);
    g.traverse((o) => (o.geometry?.dispose?.(), o.material && (lineMats.delete(o.material), o.material.dispose())));
  }
}
function ping(at, color) {
  // a ring ripples out where the signal lands
  const geo = new THREE.BufferGeometry().setFromPoints([at]);
  const mat = new THREE.PointsMaterial({ color, size: 12, sizeAttenuation: false, map: RING, transparent: true, depthTest: false });
  const p = new THREE.Points(geo, mat);
  p.renderOrder = 12;
  world.add(p);
  tween(750, (e) => {
    mat.size = 12 + 70 * e;
    mat.opacity = 1 - e;
  }, () => {
    world.remove(p);
    geo.dispose();
    mat.dispose();
  });
}
function signal(from, to, size, color) {
  let curve;
  if (from.distanceTo(to) < size * 0.05) {
    curve = new THREE.CubicBezierCurve3(from, to.clone().add(new THREE.Vector3(size * 0.45, size * 1.2, 0)), to.clone().add(new THREE.Vector3(-size * 0.45, size * 1.2, 0)), to);
  } else {
    const span = from.distanceTo(to);
    const mid = from.clone().lerp(to, 0.5);
    mid.y += Math.max(span * 0.3, size * 2);
    curve = new THREE.QuadraticBezierCurve3(from, mid, to);
  }
  const pts = curve.getPoints(96);
  const length = curve.getLength();
  const flat = (list) => list.flatMap((p) => [p.x, p.y, p.z]);
  // soft glow under the line
  const glow = new LineMaterial({ color, linewidth: 10, transparent: true, opacity: 0.16, depthTest: false });
  glow.resolution.set(stage.clientWidth, stage.clientHeight);
  lineMats.add(glow);
  const glowLine = new Line2(new LineGeometry(), glow);
  glowLine.renderOrder = 9;
  // the line itself: dashes that flow the way the request went
  const mat = new LineMaterial({ color, linewidth: 3, transparent: true, opacity: 0.95, depthTest: false, dashed: true, dashSize: length / 16, gapSize: length / 40, dashScale: 1 });
  mat.resolution.set(stage.clientWidth, stage.clientHeight);
  lineMats.add(mat);
  const line = new Line2(new LineGeometry(), mat);
  line.renderOrder = 10;
  const headGeo = new THREE.BufferGeometry().setFromPoints([from]);
  const head = new THREE.Points(headGeo, new THREE.PointsMaterial({ color, size: 18, sizeAttenuation: false, map: DOT, transparent: true, depthTest: false }));
  head.renderOrder = 11;
  const sparkleGeo = new THREE.BufferGeometry().setFromPoints([from]);
  const sparkle = new THREE.Points(sparkleGeo, new THREE.PointsMaterial({ color: 0xffffff, size: 14, sizeAttenuation: false, map: DOT, transparent: true, depthTest: false }));
  sparkle.renderOrder = 12;
  sparkle.visible = false;
  const g = new THREE.Group();
  g.add(glowLine, line, head, sparkle);
  trails.add(g);
  const fx = { pts, curve, length, mat, glow, sparkle, sparkleGeo, head };
  trailFx.push(fx);
  const draw = (part) => {
    for (const l of [line, glowLine]) {
      l.geometry.dispose();
      l.geometry = new LineGeometry();
      l.geometry.setPositions(flat(part));
      l.computeLineDistances();
    }
  };
  draw(pts.slice(0, 2));
  tween(1100, (e) => {
    const part = pts.slice(0, Math.max(2, Math.round(e * (pts.length - 1)) + 1));
    draw(part);
    headGeo.setFromPoints([part[part.length - 1]]);
  }, () => {
    head.material.size = 10;
    mat.opacity = 0.9;
    mat.linewidth = 2.6;
    ping(to, color);
  });
}
// per frame: dashes flow; during a zoom-out a pulse replays the whole path hop by hop
let replay = null; // { t0, hop }
function animateTrails(dt) {
  const t = now();
  for (const [i, fx] of trailFx.entries()) {
    fx.mat.dashOffset -= (dt / 1000) * fx.length * 0.45;
    if (!replay) {
      fx.sparkle.visible = false;
      continue;
    }
    const cycle = trailFx.length * replay.hop + 600;
    const local = (((t - replay.t0) % cycle) - i * replay.hop) / replay.hop;
    fx.sparkle.visible = local >= 0 && local <= 1;
    if (fx.sparkle.visible) fx.sparkleGeo.setFromPoints([fx.curve.getPoint(Math.min(1, Math.max(0, local)))]);
  }
}
window.addEventListener("resize", () => lineMats.forEach((m) => m.resolution.set(stage.clientWidth, stage.clientHeight)));

// ---------- walk state ----------
let tour = null; // { at, stack, path: [{ node, num, color, group }], colors: Map, mode, auto, sinceOverview, history }
const tourLabels = new Map(); // explorer node id → { num, color }
const SPEED = Number(new URLSearchParams(location.search).get("speed")) || 1;
function stepMs(text, ms) {
  const reading = (text ?? "").split(/\s+/).length * 330;
  return Math.max(3200, (ms && voiceOn() ? ms : reading) + 800) / SPEED;
}
function hueFor(groupNode) {
  if (!tour.colors.has(groupNode.id)) tour.colors.set(groupNode.id, PALETTE[tour.colors.size % PALETTE.length]);
  return tour.colors.get(groupNode.id);
}
function legend() {
  return `<div class="legend">${[...tour.colors].map(([id, c]) => `<i style="--c:${hex(c)}">${n$(id)?.name ?? id}</i>`).join("")}</div>`;
}
function progress(extra = "") {
  return `${tour.mode === "nerd" ? "Nerd mode" : "Tour"} · step ${tour.count}${extra}`;
}
function schedule(ms, fn) {
  const token = (tour.token = {});
  clearTimeout(tour.timer);
  tour.due = Date.now() + ms;
  tour.pending = fn;
  tour.timer = setTimeout(() => tour && tour.token === token && !tour.paused && fn(), ms);
}
function rememberStep(node) {
  clearTimeout(tour.timer);
  tour.pending = null;
  tour.choosing = null;
  audio.voice?.pause();
  audio.voice = null;
  audio.clip = node.audio ?? null;
  const token = tour.stepToken = {};
  tour.history.push({ id: node.id, path: tour.path.length, stack: [...tour.stack], count: tour.count, colors: new Map(tour.colors), sinceOverview: tour.sinceOverview });
  return token;
}
function go(id) {
  if (!tour) return;
  const node = G.nodes[id];
  if (!node) return finish(`missing node ${id}`);
  tour.at = id;
  switch (node.kind) {
    case "call":
      tour.stack.push(node.next);
      return go(node.target);
    case "return": {
      const back = tour.stack.pop();
      return back ? go(back) : finish();
    }
    case "end":
      // a flow entered as a sub-tour (chat running a task) returns to its caller
      if (tour.stack.length) return go(tour.stack.pop());
      return finish(null, node);
    case "choice":
      return ask(node);
    default:
      if (node.level === "nerd" && tour.mode !== "nerd") return go(node.next);
      return visit(node);
  }
}
function visit(node) {
  const step = rememberStep(node);
  tour.count++;
  const target = node.node ? n$(node.node) : null;
  orbiting = 0;
  if (node.do === "seal") reset();
  else if (node.do === "open-root") (open(T), setFocus(T, { azimuth: AZ_SPH, elevation: EL }));
  else if (target) goTo(target);
  const before = tour.path[tour.path.length - 1];
  let crossing = false;
  if (target) {
    const group = groupOf(target);
    const color = hueFor(group);
    const num = tour.path.length + 1;
    tour.path.push({ node: target, num, color, group });
    tourLabels.set(target.id, { num, color });
    crossing = before && before.group !== group && processOf(before.node) !== processOf(target);
    const fromNode = node.from ? n$(node.from) : null;
    if (fromNode && fromNode.parent) open(fromNode.parent);
    const from = fromNode?.box ? anchorOf(fromNode) : before ? anchorOf(before.node) : OUTSIDE.clone();
    after(650, () => tour?.stepToken === step && target.v && signal(from, anchorOf(target), Math.min(target.box.w, target.box.d), color));
    tour.sinceOverview++;
    restyle();
  }
  const where = node.fn ? ` · ${node.fn}()` : "";
  say(node.title, node.say, progress(node.chapter ? ` · ${node.chapter}${where}` : where), node.evidence ?? "");
  after(450, () => tour?.stepToken === step && !tour.paused && !audio.voice && speak(node.audio));
  const wait = stepMs(node.say, node.ms);
  const next = () => (tour.sinceOverview >= (tour.mode === "nerd" ? 7 : 5) || crossing ? overview(() => go(node.next)) : go(node.next));
  schedule(wait, next);
}
function frameNodes(nodes, dur = 1100) {
  // just far enough back to hold every one of these boxes, with a little air
  const box = new THREE.Box3();
  for (const n of nodes) if (n?.box) box.expandByPoint(anchorOf(n)).expandByPoint(new THREE.Vector3(n.box.x, n.box.y, n.box.z)).expandByPoint(new THREE.Vector3(n.box.x + n.box.w, n.box.y, n.box.z + n.box.d));
  if (box.isEmpty()) return;
  box.expandByScalar(Math.max(box.getSize(new THREE.Vector3()).length() * 0.04, 1));
  const fromT = controls.target.clone();
  const fromZ = viewScale();
  const fromDir = camera.position.clone().sub(fromT).normalize();
  const dir = VIEW.clone();
  const { center, zoom } = fitView(box, dir);
  tween(dur, (e) => {
    aim(new THREE.Vector3().lerpVectors(fromT, center, e), new THREE.Vector3().lerpVectors(fromDir, dir, e), fromZ * Math.pow(zoom / fromZ, e));
  }, () => updateLabels());
}
function showPath() {
  // only the boxes on the path stay solid; the rest falls back to silhouettes
  spot = { feature: null, exact: true, ids: new Set(tour.path.map((p) => p.node.id)) };
  restyle();
  frameNodes(tour.path.map((p) => p.node));
  replay = { t0: now() + 900, hop: Math.max(380, 2200 / Math.max(1, tour.path.length)) };
}
function overview(then) {
  tour.sinceOverview = 0;
  showPath();
  say("The path so far", `${tour.path.length} hops through ${tour.colors.size} ${tour.colors.size === 1 ? "part" : "parts"} of the system`, progress(), "", legend());
  schedule(Math.max(3600, tour.path.length * 380 + 1600) / SPEED, () => {
    spot = null;
    replay = null;
    restyle();
    then();
  });
}
function ask(node) {
  const step = rememberStep(node);
  const options = node.options ?? [];
  const wait = Math.max(5200, (node.ms ?? 0) + 1800) / SPEED;
  const countdown = (o) => (tour.auto && o.default ? ` data-countdown style="--dur:${Math.round(wait)}ms"` : "");
  const note = tour.auto ? `<em class="autonote">auto-picks the highlighted path in ${Math.round(wait / 1000)} s · pick another or press Space to wait</em>` : "";
  const buttons = `<div class="choices">${options.map((o, i) => `<button type="button" data-pick="${i}"${o.default ? " data-default" : ""}${countdown(o)} title="${(o.say ?? "").replace(/"/g, "&quot;")}"><i>${i + 1}</i>${o.label}</button>`).join("")}${note}</div>`;
  say(node.title, node.say, progress(node.decidedBy ? ` · decided by ${node.decidedBy}` : ""), node.evidence ?? "", buttons);
  after(300, () => tour?.stepToken === step && !tour.paused && !audio.voice && speak(node.audio));
  const auto = options.findIndex((o) => o.default);
  if (tour.auto) schedule(wait, () => choose(node, Math.max(0, auto)));
  else clearTimeout(tour.timer);
}
function choose(node, i) {
  const o = node.options[i];
  if (!o) return;
  tour.choosing = o.next;
  tour.stepToken = {}; // the fork's delayed narration must not replace this option
  say(o.label, o.say ?? "", progress(` · ${node.title}`));
  speak(o.audio);
  schedule(o.say ? stepMs(o.say, o.ms) : 600 / SPEED, () => go(o.next));
}
caption.addEventListener("click", (e) => {
  const b = e.target.closest("[data-pick]");
  if (!b || !tour) return;
  e.stopPropagation();
  choose(G.nodes[tour.at], Number(b.dataset.pick));
});
function finish(problem = null, end = null) {
  if (!tour) return;
  tour.done = true;
  tour.pending = null;
  tour.stepToken = {};
  tour.paused = false;
  caption.removeAttribute("data-paused");
  if (tour.path.length) showPath();
  else setFocus(T, { azimuth: AZ_SPH, elevation: EL });
  const title = end?.title ?? (problem ? "Tour stopped" : "End of the path");
  say(title, problem ?? end?.say ?? "", `${tour.path.length} hops`, "", legend());
  speak(end?.audio);
  clearTimeout(tour.timer);
}
function startTour({ mode = tour?.mode ?? "core", auto = tour?.auto ?? false, from = G.start } = {}) {
  stopTour();
  spot = null;
  touring = true;
  document.body.classList.add("touring");
  tour = { at: null, stack: [], path: [], colors: new Map(), mode, auto, sinceOverview: 0, history: [], timer: 0, paused: false, count: 0 };
  tourLabels.clear();
  clearTrails();
  audioStart();
  setExploreOpen(false);
  setMenu(false);
  go(from);
}
function previousStep() {
  if (!tour || tour.history.length < (tour.done || tour.choosing ? 1 : 2)) return;
  const paused = tour.paused;
  clearTimeout(tour.timer);
  tour.token = {};
  tour.stepToken = {};
  tour.pending = null;
  audio.voice?.pause();
  if (!tour.done && !tour.choosing) tour.history.pop();
  const last = tour.history.pop();
  if (!last) return;
  tour.path.splice(last.path);
  tourLabels.clear();
  for (const step of tour.path) tourLabels.set(step.node.id, { num: step.num, color: step.color });
  clearTrails();
  tour.stack = last.stack;
  tour.colors = last.colors;
  tour.count = last.count;
  tour.sinceOverview = last.sinceOverview;
  tour.done = false;
  tour.paused = false;
  spot = null;
  caption.removeAttribute("data-paused");
  restyle();
  go(last.id);
  if (paused) pauseTour();
}
function stopTour(keepTrails = false) {
  if (tour) clearTimeout(tour.timer);
  tour = null;
  touring = false;
  orbiting = 0;
  document.body.classList.remove("touring");
  caption.removeAttribute("data-paused");
  audioStop();
  say("");
  replay = null;
  if (spot && !spot.feature) (spot = null, restyle());
  if (!keepTrails) (clearTrails(), tourLabels.clear(), restyle());
}
function pauseTour() {
  if (!tour || tour.done || tour.paused) return;
  tour.paused = true;
  tour.remaining = Math.max(0, (tour.due ?? Date.now()) - Date.now());
  clearTimeout(tour.timer);
  audio.voice?.pause();
  caption.setAttribute("data-paused", "");
  renderTransport();
  poke(400);
}
function resumeTour() {
  if (!tour || !tour.paused) return;
  tour.paused = false;
  caption.removeAttribute("data-paused");
  if (audio.voice && voiceOn()) audio.voice.play().catch(() => {});
  else if (audio.clip && voiceOn()) speak(audio.clip);
  if (tour.pending) schedule(tour.remaining ?? 0, tour.pending);
  renderTransport();
  poke(400);
}
function togglePause() {
  if (tour?.paused) resumeTour();
  else pauseTour();
}
// next step, or the usual option at a fork
function advance() {
  const node = tour && G.nodes[tour.at];
  if (!node || tour.done) return;
  if (tour.paused) (tour.paused = false, caption.removeAttribute("data-paused"));
  if (tour.choosing) return go(tour.choosing);
  if (node.kind === "choice") choose(node, Math.max(0, node.options.findIndex((o) => o.default)));
  else go(node.next);
}
function setVoice(on) {
  audio.voiceOff = !on;
  saveAudioPrefs();
  if (!voiceOn()) audio.voice?.pause();
  else if (audio.voice) tour && !tour.paused && audio.voice.play().catch(() => {});
  else if (audio.clip && tour && !tour.paused) speak(audio.clip);
  renderTransport();
  poke(400);
}
function setMusic(on) {
  audio.musicOff = !on;
  saveAudioPrefs();
  renderTransport();
  poke(800); // let the bed fade
}

// the tour's own controls, at the right of the narration strip
const ICON = {
  pause: '<path d="M4 3h3v10H4zM9 3h3v10H9z"/>',
  play: '<path d="M4 2.5v11l9-5.5z"/>',
  next: '<path d="M3 2.5v11l7.5-5.5zM11 3h2.2v10H11z"/>',
  previous: '<path d="M13 2.5v11L5.5 8zM2.8 3H5v10H2.8z"/>',
  stop: '<path d="M3.5 3.5h9v9h-9z"/>',
  voice: '<path d="M2 6h3l4-3.2v10.4L5 10H2z"/><path d="M11 5.2a4 4 0 0 1 0 5.6M12.8 3.4a6.5 6.5 0 0 1 0 9.2" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"/>',
  music: '<path d="M6 3.2 13 2v8.3a2 2 0 1 1-1.5-1.9V5L7.5 5.8v6.4A2 2 0 1 1 6 10.3z"/>',
};
const icon = (name, off = false) => `<svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true" fill="currentColor">${ICON[name]}${off ? '<path d="M2 14 14 2" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>' : ""}</svg>`;
function transportHtml() {
  if (!tour) return spot?.feature ? featureTransportHtml() : "";
  const live = !tour.done;
  const b = (tx, label, name, extra = "") => `<button type="button" data-tx="${tx}" title="${label}" aria-label="${label}"${extra}>${icon(name, extra.includes('aria-pressed="false"'))}</button>`;
  const voice = !audio.voiceOff;
  const music = !audio.musicOff;
  return `<span class="transport" role="group" aria-label="Tour controls">${
    b("previous", "Previous step (←)", "previous", tour.history.length < (tour.done || tour.choosing ? 1 : 2) ? " disabled" : "") +
    (live ? b("pause", tour.paused ? "Resume (Space)" : "Pause (Space)", tour.paused ? "play" : "pause") + b("next", "Next step (→)", "next") : "")
  }${b("stop", "Stop the tour (Esc)", "stop")}<i></i>${b("voice", voice ? "Voice on (V)" : "Voice off (V)", "voice", ` aria-pressed="${voice}"`)}${b("music", music ? "Music on (B)" : "Music off (B)", "music", ` aria-pressed="${music}"`)}</span>`;
}
function renderTransport() {
  const now = caption.querySelector(".transport");
  if (!now) return;
  now.insertAdjacentHTML("afterend", transportHtml());
  now.remove();
}
caption.addEventListener("click", (e) => {
  const b = e.target.closest("[data-tx]");
  if (!b) return;
  e.stopPropagation();
  ({ previous: previousStep, pause: togglePause, next: advance, stop: () => stopTour(), voice: () => setVoice(audio.voiceOff), music: () => setMusic(audio.musicOff), featurePrevious: () => moveFeature(-1), featureNext: () => moveFeature(1) })[b.dataset.tx]?.();
});

// ---------- features: spotlight every place a feature lives ----------
const FEATURES = DATA.features ?? [];
const featureList = document.getElementById("featureList");
const MATURITY = { shipped: "shipped", hardening: "hardening", experimental: "experimental", wip: "WIP", partial: "partial" };
// product features first, then the platform underneath; a filter narrows both
const GROUPS = ["What it does", "Under the hood"];
const featureButton = (f) => `<button type="button" data-feature="${f.id}" data-find="${`${f.name} ${f.oneLine} ${f.maturity}`.toLowerCase().replace(/"/g, "")}"><b>${f.name} <em data-m="${f.maturity}">${MATURITY[f.maturity] ?? f.maturity ?? ""}</em></b><small>${f.oneLine}</small></button>`;
featureList.innerHTML =
  `<input type="search" placeholder="Filter ${FEATURES.length} features" aria-label="Filter features" autocomplete="off" spellcheck="false">` +
  GROUPS.map((g) => {
    const items = FEATURES.filter((f) => (f.group ?? GROUPS[0]) === g);
    return items.length ? `<h4>${g} <span>${items.length}</span></h4>${items.map(featureButton).join("")}` : "";
  }).join("");
const featureFilter = featureList.querySelector("input");
featureFilter.addEventListener("input", () => {
  const words = featureFilter.value.toLowerCase().split(/\s+/).filter(Boolean);
  for (const b of featureList.querySelectorAll("[data-feature]")) b.hidden = !words.every((w) => b.dataset.find.includes(w));
  for (const h of featureList.querySelectorAll("h4")) {
    let shown = 0;
    for (let el = h.nextElementSibling; el && el.tagName === "BUTTON"; el = el.nextElementSibling) shown += el.hidden ? 0 : 1;
    h.hidden = !shown;
    h.querySelector("span").textContent = shown;
  }
});
featureFilter.addEventListener("keydown", (e) => {
  e.stopPropagation(); // typing is not a shortcut
  if (e.key === "Enter") featureList.querySelector("[data-feature]:not([hidden])")?.click();
  if (e.key === "Escape") setExploreOpen(false, true);
});
featureList.addEventListener("click", (e) => {
  const b = e.target.closest("[data-feature]");
  if (b) spotlight(FEATURES.find((f) => f.id === b.dataset.feature));
});
function spotlight(f) {
  if (!f) return;
  stopTour();
  setExploreOpen(false);
  setMenu(false);
  const nodes = f.locations.map((l) => n$(l.node)).filter(Boolean);
  for (const n of nodes) if (n.parent) open(n.parent, { animate: false });
  spot = { feature: f, ids: new Set(nodes.map((n) => n.id)) };
  tourLabels.clear();
  const colors = new Map();
  nodes.forEach((n, i) => {
    const g = groupOf(n);
    if (!colors.has(g.id)) colors.set(g.id, PALETTE[colors.size % PALETTE.length]);
    tourLabels.set(n.id, { num: i + 1, color: colors.get(g.id) });
  });
  focus = T;
  selected = null;
  restyle();
  frameNodes(nodes);
  const legendHtml = `<div class="legend">${[...colors].map(([id, c]) => `<i style="--c:${hex(c)}">${n$(id)?.name ?? id}</i>`).join("")}</div>`;
  const play = `<div class="choices">${f.start ? `<button type="button" data-play="${f.id}" title="Walk through how it works, step by step">Play its tour</button>` : ""}<button type="button" data-unspot title="Back to the whole system">Done</button></div>`;
  say(f.name, f.oneLine, `Feature · ${MATURITY[f.maturity] ?? f.maturity ?? ""} · ${nodes.length} places in the code`, (f.configKeys ?? []).length ? `config: ${f.configKeys.slice(0, 4).join(", ")}` : "", legendHtml + play);
}
function featureSequence() {
  const buttons = [...featureList.querySelectorAll("[data-feature]:not([hidden])")];
  return buttons.map(b => FEATURES.find(f => f.id === b.dataset.feature));
}
function featureTransportHtml() {
  const sequence = featureSequence();
  const index = sequence.indexOf(spot.feature);
  const button = (action, label, name, disabled) => `<button type="button" data-tx="${action}" aria-label="${label}" title="${label}"${disabled ? " disabled" : ""}>${icon(name)}</button>`;
  return `<span class="transport" role="group" aria-label="Feature controls">${button("featurePrevious", "Previous feature (←)", "previous", index <= 0)}${button("featureNext", "Next feature (→)", "next", index < 0 || index >= sequence.length - 1)}</span>`;
}
function moveFeature(delta) {
  if (!spot?.feature) return;
  const sequence = featureSequence();
  const index = sequence.indexOf(spot.feature);
  if (index >= 0) spotlight(sequence[index + delta]);
}
function unspot() {
  spot = null;
  tourLabels.clear();
  say("");
  restyle();
}
caption.addEventListener("click", (e) => {
  const playB = e.target.closest("[data-play]");
  if (playB) {
    const f = FEATURES.find((x) => x.id === playB.dataset.play);
    unspot();
    return startTour({ mode: menuMode, auto: true, from: f.start });
  }
  if (e.target.closest("[data-unspot]")) unspot();
});

// ---------- menu ----------
const menu = document.getElementById("tours");
const exploreMenu = document.getElementById("exploreMenu");
const exploreButton = document.getElementById("explore");
const browseTabs = [...exploreMenu.querySelectorAll("[data-browse]")];
function setExploreOpen(open, restoreFocus = false) {
  exploreMenu.hidden = !open;
  exploreButton.setAttribute("aria-expanded", String(open));
  if (open) browseTabs.find(tab => tab.getAttribute("aria-selected") === "true").focus({ preventScroll: true });
  else if (restoreFocus) exploreButton.focus({ preventScroll: true });
}
function selectBrowseTab(tab) {
  for (const button of browseTabs) {
    const active = button === tab;
    button.setAttribute("aria-selected", String(active));
    button.tabIndex = active ? 0 : -1;
    document.getElementById(button.getAttribute("aria-controls")).hidden = !active;
  }
}
exploreButton.addEventListener("click", () => setExploreOpen(exploreMenu.hidden));
for (const tab of browseTabs) {
  tab.addEventListener("click", () => selectBrowseTab(tab));
  tab.addEventListener("keydown", e => {
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) return;
    e.preventDefault();
    e.stopPropagation();
    const next = e.key === "Home" ? browseTabs[0] : e.key === "End" ? browseTabs.at(-1) : browseTabs[(browseTabs.indexOf(tab) + (e.key === "ArrowRight" ? 1 : -1) + browseTabs.length) % browseTabs.length];
    selectBrowseTab(next);
    next.focus();
  });
}
document.addEventListener("click", e => {
  if (!exploreMenu.hidden && !e.composedPath().includes(exploreMenu) && !e.composedPath().includes(exploreButton)) setExploreOpen(false);
});
const ENTRIES = (G.nodes[G.entryChoice]?.options ?? []).map((o, i) => ({ i, label: o.label, say: o.say }));
let menuMode = "core";
let menuAuto = true;
function renderMenu() {
  menu.innerHTML = `
    <div class="seg"><button type="button" data-mode="core"${menuMode === "core" ? " aria-pressed=true" : ""}>Standard</button><button type="button" data-mode="nerd"${menuMode === "nerd" ? " aria-pressed=true" : ""}>Nerd mode</button></div>
    <label class="auto"><input type="checkbox" data-auto${menuAuto ? " checked" : ""}> Auto-pick the usual path at each fork</label>
    <button type="button" data-start="intro"><b>From the sealed box</b><small>Open the case, then choose where a request comes from.</small></button>
    ${ENTRIES.map((e) => `<button type="button" data-start="${e.i}"><b>${e.label}</b><small>${e.say ?? ""}</small></button>`).join("")}`;
}
renderMenu();
menu.addEventListener("click", (e) => {
  const mode = e.target.closest("[data-mode]");
  if (mode) {
    menuMode = mode.dataset.mode;
    renderMenu();
    menu.querySelector(`[data-mode="${menuMode}"]`).focus({ preventScroll: true });
    return;
  }
  const auto = e.target.closest("[data-auto]");
  if (auto) return (menuAuto = auto.checked);
  const b = e.target.closest("[data-start]");
  if (!b) return;
  if (b.dataset.start === "intro") return startTour({ mode: menuMode, auto: menuAuto });
  const option = G.nodes[G.entryChoice].options[Number(b.dataset.start)];
  open(T);
  startTour({ mode: menuMode, auto: menuAuto, from: option.next });
});
const muteButton = document.getElementById("mute");
function renderSoundToggle() {
  const label = audio.muted ? "Unmute sound (M)" : "Mute sound (M)";
  muteButton.innerHTML = icon("voice", audio.muted);
  muteButton.setAttribute("aria-label", label);
  muteButton.setAttribute("title", label);
  muteButton.setAttribute("aria-pressed", String(audio.muted));
}
renderSoundToggle();
muteButton.addEventListener("click", () => {
  audio.muted = !audio.muted;
  renderSoundToggle();
  if (audio.muted) audio.voice?.pause();
  else if (voiceOn() && audio.voice && tour && !tour.paused) audio.voice.play().catch(() => {});
  renderTransport();
  poke(800);
});
window.addEventListener(
  "keydown",
  (e) => {
    if (e.key === "Escape" && !headerSearch.hidden) {
      e.preventDefault();
      e.stopImmediatePropagation();
      return setSearchOpen(false);
    }
    if (e.key === "Escape" && !exploreMenu.hidden) {
      e.preventDefault();
      e.stopImmediatePropagation();
      return setExploreOpen(false, true);
    }
    if (e.key === "Escape" && (tour || spot?.feature)) {
      e.preventDefault();
      e.stopImmediatePropagation();
      return tour ? stopTour() : unspot();
    }
    if (e.target.closest?.("input, select, textarea, button, a")) return;
    if (e.key.toLowerCase() === "m") return e.preventDefault(), e.stopImmediatePropagation(), muteButton.click();
    if (!tour) {
      if (spot?.feature && ["ArrowLeft", "ArrowRight"].includes(e.key)) {
        e.preventDefault();
        e.stopImmediatePropagation();
        moveFeature(e.key === "ArrowLeft" ? -1 : 1);
      }
      return;
    }
    const node = G.nodes[tour.at];
    if (e.key === " ") (e.preventDefault(), e.stopImmediatePropagation(), togglePause());
    else if (e.key === "ArrowRight") (e.preventDefault(), e.stopImmediatePropagation(), advance());
    else if (e.key === "ArrowLeft") (e.preventDefault(), e.stopImmediatePropagation(), previousStep());
    else if (/^[1-9]$/.test(e.key) && node?.kind === "choice") (e.stopImmediatePropagation(), choose(node, Number(e.key) - 1));
    else if (e.key.toLowerCase() === "v") (e.stopImmediatePropagation(), setVoice(audio.voiceOff));
    else if (e.key.toLowerCase() === "b") (e.stopImmediatePropagation(), setMusic(audio.musicOff));
    else if (e.key === "Escape") (e.stopImmediatePropagation(), stopTour());
  },
  true,
);

// ---------- quiet chrome: fade the bar and panel unless they are wanted ----------
const bar = document.querySelector(".bar");
const zones = [
  { el: bar, near: (x, y) => y < 90 },
  { el: panel, near: (x, y) => y > window.innerHeight - 120 },
];
const wakeTimers = new Map();
function wake(el, ms = 2600) {
  for (const target of el === bar ? [...bar.children] : [el]) target.setAttribute("data-awake", "");
  clearTimeout(wakeTimers.get(el));
  wakeTimers.set(el, setTimeout(() => {
    for (const target of el === bar ? [...bar.children] : [el]) target.removeAttribute("data-awake");
  }, ms));
}
let quietTimer = 0;
function settleQuiet() {
  clearTimeout(quietTimer);
  quietTimer = setTimeout(() => document.body.classList.add("quiet"), 1800);
}
window.addEventListener("pointermove", (e) => {
  for (const z of zones) if (z.near(e.clientX, e.clientY)) wake(z.el);
});
renderer.domElement.addEventListener("pointerdown", () => document.body.classList.add("quiet"));
renderer.domElement.addEventListener("pointerup", () => selected && wake(panel, 3200));
window.addEventListener("keydown", (e) => {
  if (e.key === "/" && !e.target.closest?.("input, select, textarea")) {
    e.preventDefault();
    setSearchOpen(true);
  }
});
settleQuiet();

// ---------- compact layout (phones): one-row bar with a menu, a one-line strip ----------
const menuButton = document.getElementById("menu");
const tools = document.getElementById("tools");
function setMenu(open) {
  document.body.classList.toggle("menu-open", open);
  menuButton.setAttribute("aria-expanded", String(open));
  menuButton.textContent = open ? "Close" : "Menu";
  poke();
}
menuButton.addEventListener("click", () => setMenu(!document.body.classList.contains("menu-open")));
// Explore's tabs and options stay inside the compact menu until a selection.
tools.addEventListener("click", (e) => {
  if (!document.body.classList.contains("compact") || e.composedPath().includes(exploreMenu)) return;
  const b = e.target.closest("button");
  if (b && b.id !== "explore") setTimeout(() => setMenu(false), 0);
});
tools.addEventListener("change", e => document.body.classList.contains("compact") && !e.target.closest("#exploreMenu") && setMenu(false));
renderer.domElement.addEventListener("pointerdown", () => document.body.classList.contains("menu-open") && setMenu(false));
// the strip shows one line; a tap on it shows the rest
panel.addEventListener("click", (e) => {
  if (!document.body.classList.contains("compact") || e.target.closest("button, a")) return;
  panel.classList.toggle("open");
  poke();
});
// the narration folds to its title and choices
caption.addEventListener("click", (e) => {
  if (!document.body.classList.contains("compact") || !e.target.closest(".cap-top") || e.target.closest("button, a, input")) return;
  caption.classList.toggle("min");
});
// the crumbs scroll to the deepest level
new MutationObserver(() => {
  crumbs.scrollLeft = crumbs.scrollWidth;
  crumbs.classList.toggle("clipped", crumbs.scrollWidth > crumbs.clientWidth + 1);
}).observe(crumbs, { childList: true });
const hint = document.getElementById("hint");
hint.textContent = hint.textContent.replace(/from source at .*$/, BUILT.replace(/^Built/, "built"));
if (coarse.matches) hint.textContent = `Tap a box to light it, tap again to open · drag to turn · pinch to zoom · two fingers to pan · ${BUILT.replace(/^Built/, "built")}`;
// keyboard help
const help = document.getElementById("help");
window.addEventListener("keydown", (e) => {
  if (e.target.closest?.("input, select")) return;
  if (e.key === "?") help.hidden = !help.hidden;
  else if (e.key === "Escape" && !help.hidden) help.hidden = true;
});
help.addEventListener("click", () => (help.hidden = true));

// ---------- start: the sealed case ----------
makeVisual(DATA.tree);
const params = new URLSearchParams(location.search);
let startSkin = "paper";
try {
  startSkin = params.get("skin") || localStorage.getItem("magician-anatomy-skin") || "paper";
} catch {}
applySkin(startSkin);
if (params.has("clean")) {
  document.body.classList.add("clean");
  panelSpace = 0;
}
chooseView();
portrait = innerHeight > innerWidth * 1.1;
camera.position.copy(VIEW).multiplyScalar(1000);
setFocus(DATA.tree, { fly: false });
frame(DATA.tree, 0);
requestAnimationFrame(() => requestAnimationFrame(() => {
  const splash = document.getElementById("splash");
  splash?.classList.add("gone");
  setTimeout(() => splash?.remove(), 500);
}));
if (params.has("open")) (open(T), setFocus(T));
if (params.has("tour")) {
  // browsers only play sound after a gesture: the first click starts the tour
  say("Click anywhere to start the tour", "");
  window.addEventListener("pointerdown", () => startTour({ auto: params.get("tour") !== "manual" }), { once: true, capture: true });
}

let lastFrame = 0;
const lastView = { pos: new THREE.Vector3(), target: new THREE.Vector3(), zoom: 1 };
let drawn = 0; // frames actually drawn (checks that an idle page sleeps)
function tick(dt, force = false) {
  runTimers();
  stepAudio(dt);
  animateTrails(dt);
  animatePackets();
  stepTweens();
  if (orbiting) {
    const off = camera.position.clone().sub(controls.target);
    off.applyAxisAngle(new THREE.Vector3(0, 1, 0), (dt / 1000) * 0.62);
    camera.position.copy(controls.target).add(off);
  }
  controls.update();
  if (!ORTHO) (fitDepth(), camera.updateProjectionMatrix());
  // controls.update() keeps reporting a change while damping decays toward
  // (never exactly) zero, so movement is measured here with a tolerance
  const moved = camera.position.distanceToSquared(lastView.pos) > 1e-6 || controls.target.distanceToSquared(lastView.target) > 1e-6 || Math.abs(camera.zoom / lastView.zoom - 1) > 1e-5;
  if (moved) {
    lastView.pos.copy(camera.position);
    lastView.target.copy(controls.target);
    lastView.zoom = camera.zoom;
    poke(400);
  }
  const busy = moved || tweens.size || timers.length || orbiting || touring || replay || PACKETS.count > 0 || performance.now() < awakeUntil;
  if (!busy && !force) return;
  // the key light turns with the camera, so the faces you see always take the
  // same three tones (otherwise orbiting to the back shows near-black faces)
  const turn = new THREE.Spherical().setFromVector3(camera.position.clone().sub(controls.target)).theta - AZ_SPH;
  sun.position.copy(LIGHT).applyAxisAngle(UP, turn);
  thinWires();
  renderer.render(scene, camera);
  labels.render(scene, camera);
  drawn++;
  if (declutterDue) declutter();
}
function loop(t) {
  if (virtual === null) tick(lastFrame ? Math.min(50, t - lastFrame) : 16);
  lastFrame = t;
  requestAnimationFrame(loop);
}
requestAnimationFrame(loop);

// Hooks for scripted capture: virtual time makes every recorded frame exact.
window.__explorer = {
  get ambient() { return ambient.state; },
  open: (id) => open(n$(id)),
  focus: (id) => setFocus(n$(id)),
  goTo: (id) => goTo(n$(id)),
  select(id) {
    selected = n$(id) ?? null;
    restyle();
  },
  expand: expandAll,
  skin: applySkin,
  tour: (from) => startTour({ auto: true, from: from ?? G.start }),
  clock() {
    virtual = performance.now();
  },
  close: (id) => close(n$(id)),
  advance(ms) {
    virtual += ms;
    tick(ms, true);
    updateLabels();
  },
  NODES,
  // on-screen cable radii (px) before thinning, for checks
  wirePx: () => wires.flatMap((w) => w.meshes.map((m) => m.userData.r0 * (m.userData.k ?? 1) * viewScale())),
  get frames() {
    return drawn;
  },
  busy: () => ({ tweens: tweens.size, timers: timers.length, orbiting, touring, replay: Boolean(replay), packets: PACKETS.count, awake: Math.max(0, awakeUntil - performance.now()) }),
};
