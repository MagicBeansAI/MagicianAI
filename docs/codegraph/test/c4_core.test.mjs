import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const C4Core = require("../c4_core.js");

const { bandForScale, clampScale, screenToWorld, worldToScreen, boxesIntersect, cullRects, encodeUrlState, decodeUrlState, fuzzyScore } = C4Core;

test("bandForScale follows the semantic ladder", () => {
  assert.equal(bandForScale(0.05), "runtime");
  assert.equal(bandForScale(0.2), "runtime");
  assert.equal(bandForScale(0.34), "runtime");
  assert.equal(bandForScale(0.5), "areas");
  assert.equal(bandForScale(1.0), "areas");
  assert.equal(bandForScale(1.5), "code");
  assert.equal(bandForScale(2.9), "code");
  assert.equal(bandForScale(3.5), "detail");
  assert.equal(bandForScale(0), "runtime");
});

test("clampScale keeps camera in range", () => {
  assert.equal(clampScale(0.001, 0.02, 8), 0.02);
  assert.equal(clampScale(100, 0.02, 8), 8);
  assert.equal(clampScale(1, 0.02, 8), 1);
});

test("screen/world transforms round-trip", () => {
  const cam = { x: 120, y: -40, z: 0.8 };
  const view = { w: 1000, h: 700 };
  const wx = 555, wy = -123;
  const [sx, sy] = worldToScreen(wx, wy, cam, view);
  const [bx, by] = screenToWorld(sx, sy, cam, view);
  assert.ok(Math.abs(bx - wx) < 1e-9);
  assert.ok(Math.abs(by - wy) < 1e-9);
});

test("worldToScreen centers the camera", () => {
  const cam = { x: 0, y: 0, z: 1 };
  const view = { w: 1000, h: 700 };
  const [sx, sy] = worldToScreen(0, 0, cam, view);
  assert.equal(sx, 500);
  assert.equal(sy, 350);
});

test("boxesIntersect with margin", () => {
  const vp = { x: 0, y: 0, w: 100, h: 100 };
  assert.equal(boxesIntersect(vp, { x: 150, y: 150, w: 10, h: 10 }), false);
  assert.equal(boxesIntersect(vp, { x: 90, y: 90, w: 40, h: 40 }), true);
  assert.equal(boxesIntersect(vp, { x: 110, y: 0, w: 10, h: 10 }, 20), true);
});

test("cullRects keeps intersecting rects only", () => {
  const rects = [
    { id: "a", x: 0, y: 0, w: 10, h: 10 },
    { id: "b", x: 500, y: 500, w: 10, h: 10 },
    { id: "c", x: 95, y: 95, w: 10, h: 10 },
  ];
  const kept = cullRects(rects, { x: 0, y: 0, w: 100, h: 100 }, 0);
  assert.deepEqual(kept.map((r) => r.id), ["a", "c"]);
});

test("url state encode/decode round-trips", () => {
  const state = { x: 123.456, y: -78.9, z: 0.125, sel: "runtime:magician", exp: ["runtime:magician", "area:skills"] };
  const encoded = encodeUrlState(state);
  const decoded = decodeUrlState(encoded);
  assert.equal(decoded.sel, "runtime:magician");
  assert.deepEqual(decoded.exp, ["runtime:magician", "area:skills"]);
  assert.ok(Math.abs(decoded.x - 123.456) < 0.01);
  assert.ok(Math.abs(decoded.z - 0.125) < 1e-6);
});

test("decodeUrlState applies defaults", () => {
  const decoded = decodeUrlState("");
  assert.equal(decoded.sel, "");
  assert.deepEqual(decoded.exp, []);
  assert.equal(typeof decoded.x, "number");
  assert.ok(decoded.z > 0);
});

test("fuzzyScore matches subsequences and rejects the rest", () => {
  assert.equal(fuzzyScore("zzz", "magician"), null);
  const score = fuzzyScore("mag", "magician");
  assert.ok(score > 0);
  // Consecutive match beats scattered match.
  const consecutive = fuzzyScore("mag", "magician");
  const scattered = fuzzyScore("mag", "memory-aggregate-grid");
  assert.ok(consecutive > scattered);
});

test("fuzzyScore is case-insensitive on labels", () => {
  assert.ok(fuzzyScore("ui", "Unified UI") !== null);
});

const { routeOrthogonal, labelAnchor } = C4Core;

function segmentHitsRect(a, b, r) {
  // Orthogonal segment vs open rectangle interior.
  const minX = Math.min(a[0], b[0]), maxX = Math.max(a[0], b[0]);
  const minY = Math.min(a[1], b[1]), maxY = Math.max(a[1], b[1]);
  return maxX > r.x && minX < r.x + r.w && maxY > r.y && minY < r.y + r.h;
}

function assertOrthogonal(points) {
  for (let i = 0; i + 1 < points.length; i++) {
    const [a, b] = [points[i], points[i + 1]];
    assert.ok(a[0] === b[0] || a[1] === b[1], `segment ${i} is diagonal`);
  }
}

test("routeOrthogonal goes straight when nothing is in the way", () => {
  const from = { x: 0, y: 0, w: 100, h: 100 };
  const to = { x: 400, y: 0, w: 100, h: 100 };
  const pts = routeOrthogonal(from, to, []);
  assert.deepEqual(pts, [[100, 50], [400, 50]]);
});

test("routeOrthogonal detours around a blocking box", () => {
  const from = { x: 0, y: 0, w: 100, h: 100 };
  const to = { x: 800, y: 0, w: 100, h: 100 };
  const wall = { x: 300, y: -200, w: 200, h: 500 };
  const pts = routeOrthogonal(from, to, [wall]);
  assert.ok(pts && pts.length >= 4);
  assertOrthogonal(pts);
  for (let i = 0; i + 1 < pts.length; i++) {
    assert.ok(!segmentHitsRect(pts[i], pts[i + 1], wall), "route crosses the wall");
  }
  // Starts on the source border and ends on the target border.
  const [sx, sy] = pts[0];
  assert.ok(sx === 100 || sx === 0 || sy === 0 || sy === 100);
  const [ex, ey] = pts[pts.length - 1];
  assert.ok(ex === 800 || ex === 900 || ey === 0 || ey === 100);
});

test("routeOrthogonal offsets parallel edges into separate lanes", () => {
  const from = { x: 0, y: 0, w: 100, h: 200 };
  const to = { x: 400, y: 0, w: 100, h: 200 };
  const a = routeOrthogonal(from, to, [], { offset: -30 });
  const b = routeOrthogonal(from, to, [], { offset: 30 });
  assert.notEqual(a[0][1], b[0][1]);
});

test("labelAnchor sits on the longest segment", () => {
  assert.deepEqual(labelAnchor([[0, 0], [10, 0], [10, 400], [20, 400]]), [10, 200]);
});
