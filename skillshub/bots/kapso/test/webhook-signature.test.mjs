import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import test from "node:test";

import { verifyKapsoWebhookSignature } from "../dist/webhook-signature.js";

const SECRET = "wh_sec_test_only";
const RAW_BODY = Buffer.from(
  '{"message":{"type":"text","text":{"body":"hello"}}}',
);

function sign(body, secret = SECRET) {
  return createHmac("sha256", secret).update(body).digest("hex");
}

test("accepts a valid Kapso HMAC over the exact raw body", () => {
  assert.equal(
    verifyKapsoWebhookSignature(RAW_BODY, sign(RAW_BODY), SECRET),
    true,
  );
});

test("rejects the same JSON when its raw representation changes", () => {
  const reformatted = Buffer.from(
    '{"message": {"type":"text","text":{"body":"hello"}}}',
  );
  assert.equal(
    verifyKapsoWebhookSignature(reformatted, sign(RAW_BODY), SECRET),
    false,
  );
});

test("rejects a signature produced with another secret", () => {
  assert.equal(
    verifyKapsoWebhookSignature(RAW_BODY, sign(RAW_BODY, "wrong-secret"), SECRET),
    false,
  );
});

test("rejects missing and malformed signatures without throwing", () => {
  for (const signature of [
    undefined,
    "",
    "not-hex",
    "ab".repeat(31),
    "gg".repeat(32),
  ]) {
    assert.equal(
      verifyKapsoWebhookSignature(RAW_BODY, signature, SECRET),
      false,
    );
  }
});

test("accepts hexadecimal signatures independent of letter case", () => {
  assert.equal(
    verifyKapsoWebhookSignature(RAW_BODY, sign(RAW_BODY).toUpperCase(), SECRET),
    true,
  );
});
