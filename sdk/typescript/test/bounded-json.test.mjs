import assert from "node:assert/strict";
import test from "node:test";
import { encodeBoundedJson } from "../dist/bounded-json.js";
import { MagicianAppsError } from "../dist/index.js";

function rejects(value, maxBytes = 1_024) {
  assert.throws(
    () => encodeBoundedJson("mutate_data", value, maxBytes, { maxDepth: 8, maxNodes: 64 }),
    (error) => error instanceof MagicianAppsError,
  );
}

test("preflights exact escaped, surrogate, and multibyte JSON byte lengths", () => {
  assert.equal(encodeBoundedJson("mutate_data", "é", 4, { maxDepth: 2, maxNodes: 2 }).byteLength, 4);
  rejects("é", 3);
  rejects("\u0000\u0000", 13);
  rejects("\ud800", 7);
});

test("rejects sparse, accessor, nonplain, and cyclic input before stringify", () => {
  const sparse = new Array(1);
  rejects(sparse);

  const accessor = {};
  Object.defineProperty(accessor, "secret", { enumerable: true, get: () => "not-read" });
  rejects(accessor);

  rejects(new Date("2026-08-22T00:00:00Z"));

  const cyclic = {};
  cyclic.self = cyclic;
  rejects(cyclic);
});

test("rejects unsafe JSON integers instead of silently rounding", () => {
  rejects(Number.MAX_SAFE_INTEGER + 1);
  assert.equal(
    encodeBoundedJson("mutate_data", Number.MAX_SAFE_INTEGER, 64, { maxDepth: 2, maxNodes: 2 }).byteLength,
    String(Number.MAX_SAFE_INTEGER).length,
  );
});
