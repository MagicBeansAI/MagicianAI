import assert from "node:assert/strict";
import test from "node:test";
import {
  defineInteractiveCapabilityRequest,
  defineInteractiveGrantSelection,
} from "../dist/index.js";

function browserRequest(overrides = {}) {
  return {
    owner: "browser",
    allowedOrigins: ["https://z.example", "about:blank", "https://a.example"],
    targetProfileClass: "installation_ephemeral_headless",
    actionClasses: ["observe"],
    background: "direct_owner",
    capture: "structured_evidence_only",
    transfer: "denied",
    resources: {
      max_sessions: 1,
      max_steps: 1,
      max_duration_seconds: 60,
      max_evidence_bytes: 4_096,
      max_evidence_nodes: 128,
      max_pixels: 0,
      max_artifact_bytes: 0,
      max_output_bytes: 4_096,
    },
    expirySession: {
      grant_lifetime_seconds: 3_600,
      max_session_seconds: 60,
      session: "invocation_bound",
    },
    ...overrides,
  };
}

test("interactive request builder admits one exact owner-supported action class", () => {
  const request = defineInteractiveCapabilityRequest(browserRequest());
  assert.deepEqual(request.allowed_origins, [
    "about:blank",
    "https://a.example",
    "https://z.example",
  ]);
  assert.deepEqual(request.action_classes, ["observe"]);
  assert(Object.isFrozen(request));
  assert(Object.isFrozen(request.resources));

  const navigate = defineInteractiveCapabilityRequest(browserRequest({
    actionClasses: ["navigate_or_launch"],
    expirySession: {
      grant_lifetime_seconds: 3_600,
      max_session_seconds: 60,
      session: "run_bound",
    },
  }));
  assert.deepEqual(navigate.action_classes, ["navigate_or_launch"]);
  assert.equal(navigate.expiry_session.session, "run_bound");
  assert.throws(() => defineInteractiveCapabilityRequest(browserRequest({
    actionClasses: ["observe", "interact"],
  })));
  assert.throws(() => defineInteractiveCapabilityRequest(browserRequest({
    actionClasses: ["capture_pixels"],
    capture: "reviewed_pixels",
  })));
  assert.throws(() => defineInteractiveCapabilityRequest(browserRequest({
    allowedOrigins: ["https://a.example", "https://a.example"],
  })));
  assert.throws(() => defineInteractiveCapabilityRequest(browserRequest({
    background: "reviewed_bounded_background",
  })));
});

test("interactive grant builder binds exact review identity without transport IDs", () => {
  const selection = defineInteractiveGrantSelection(
    "capability:browser",
    `blake3:${"a".repeat(64)}`,
    browserRequest(),
  );
  assert.equal(selection.dependency_ref, "capability:browser");
  assert.equal(selection.reviewed_request_digest, `blake3:${"a".repeat(64)}`);
  assert.equal("device_id" in selection, false);
  assert.equal("control_token" in selection, false);
  assert.throws(() => defineInteractiveGrantSelection(
    "raw-device-id",
    `blake3:${"a".repeat(64)}`,
    browserRequest(),
  ));
});
