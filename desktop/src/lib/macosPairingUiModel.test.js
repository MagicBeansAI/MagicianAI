import assert from "node:assert/strict";
import test from "node:test";

import {
  canApproveMacosPairing,
  macosPairingPhaseLabel,
  readBoundedResponseText,
  validMacosBundleId,
} from "./macosPairingUiModel.js";

test("approval requires the exact displayed scope-target material digest", () => {
  const status = {
    ownerApprovalRequired: true,
    setupId: "setup:1",
    generation: 2,
    scopeBindingRef: "scope:workspace-a",
    requestedTargets: [{ targetRef: "target:1", bundleId: "com.example.Editor" }],
    reviewMaterialDigest: "blake3:review-a",
  };
  assert.equal(canApproveMacosPairing(status, "blake3:review-a"), true);
  assert.equal(canApproveMacosPairing(status, "blake3:review-b"), false);
  assert.equal(canApproveMacosPairing({ ...status, scopeBindingRef: null }, "blake3:review-a"), false);
});

test("bundle and lifecycle rendering stay closed and bounded", () => {
  assert.equal(validMacosBundleId("com.example.Editor"), true);
  assert.equal(validMacosBundleId("com..example"), false);
  assert.equal(validMacosBundleId(`com.${"x".repeat(300)}`), false);
  assert.equal(macosPairingPhaseLabel("unexpected"), "Unavailable");
});

test("chunked pairing responses are rejected before crossing the UI byte ceiling", async () => {
  const valid = new ReadableStream({
    start(controller) {
      controller.enqueue(new TextEncoder().encode('{"phase":"active"}'));
      controller.close();
    },
  });
  assert.equal(await readBoundedResponseText(valid, 64), '{"phase":"active"}');

  const oversized = new ReadableStream({
    start(controller) {
      controller.enqueue(new Uint8Array(40));
      controller.enqueue(new Uint8Array(40));
      controller.close();
    },
  });
  await assert.rejects(() => readBoundedResponseText(oversized, 64), /oversized response/);
});
