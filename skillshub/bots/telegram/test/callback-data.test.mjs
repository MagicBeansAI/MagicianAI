import assert from "node:assert/strict";
import test from "node:test";

import { chunkTelegramText, isRejectedButtonUrl } from "../dist/callback-data.js";

test("chunkTelegramText preserves content while splitting long messages", () => {
  const text = `line one\n${"x".repeat(4_500)} tail`;
  const chunks = chunkTelegramText(text, 1000);

  assert.ok(chunks.length > 1);
  assert.ok(chunks.every((chunk) => chunk.length <= 1000));
  assert.equal(chunks.join(""), text);
});

test("isRejectedButtonUrl separates a refused button URL from a refused message", () => {
  // Telegram's real wording, from a delivery that failed outright because the
  // configured link origin was not public.
  assert.equal(
    isRejectedButtonUrl(
      new Error(
        "400: Bad Request: inline keyboard button URL 'http://localhost:5173/attention?attention=1' is invalid: Wrong HTTP URL",
      ),
    ),
    true,
  );
  assert.equal(isRejectedButtonUrl("Wrong HTTP URL"), true);

  // Everything else must still throw, so the runtime records the provider's
  // real answer instead of reporting a send that never happened.
  for (const other of [
    new Error("403: Forbidden: bot was blocked by the user"),
    new Error("400: Bad Request: chat not found"),
    new Error("429: Too Many Requests: retry after 30"),
    new Error("socket hang up"),
  ]) {
    assert.equal(isRejectedButtonUrl(other), false, other.message);
  }
});
