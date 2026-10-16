import test from "node:test";
import assert from "node:assert/strict";
import { isSelectedMagicianUrl } from "./magicianAuthPolicy.js";

test("container credentials stay on their exact origin, including loopback port", () => {
  const origin = "http://127.0.0.1:13002";
  assert.equal(isSelectedMagicianUrl(`${origin}/api/magician/v2/auth/session`, origin), true);
  assert.equal(isSelectedMagicianUrl("ws://127.0.0.1:13002/api/magician/v2/events", origin), true);
  for (const other of ["http://127.0.0.1:3002", "http://localhost:13002", "https://elsewhere.example", "https://127.0.0.1:13002", "http://user:password@127.0.0.1:13002"]) {
    assert.equal(isSelectedMagicianUrl(`${other}/api/magician/v2/auth/session`, origin), false);
  }
});

test("remote engine HTTPS and WSS share an origin without trusting a downgrade", () => {
  const origin = "https://engine.example";
  assert.equal(isSelectedMagicianUrl("wss://engine.example/api/magician/v2/events", origin), true);
  assert.equal(isSelectedMagicianUrl("ws://engine.example/api/magician/v2/events", origin), false);
  assert.equal(isSelectedMagicianUrl(`${origin}/unrelated`, origin), false);
});
