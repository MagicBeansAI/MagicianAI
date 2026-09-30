import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { assertAuthorizedGeneratedArtifacts } from "../src/generated-artifacts.js";

test("generation-required sentinels fail closed until the public authoring owner replaces them", async () => {
  const artifacts = {
    derivedJson: await readFile(new URL("../../app/.magician/app-derived.json", import.meta.url), "utf8"),
    generatedTypescript: await readFile(new URL("../../app/sdk/app.generated.ts", import.meta.url), "utf8"),
  };
  if (artifacts.derivedJson.includes("generation_required")) {
    assert.throws(() => assertAuthorizedGeneratedArtifacts(artifacts), /generated_artifact_unavailable/);
  } else {
    assert.doesNotThrow(() => assertAuthorizedGeneratedArtifacts(artifacts));
  }
});

test("recognizable text without exact generated identities remains refused", () => {
  assert.throws(() => assertAuthorizedGeneratedArtifacts({
    derivedJson: JSON.stringify({
      package_name: "research-planner",
      schema_version: 1,
      manifest_digest: `blake3:${"1".repeat(64)}`,
    }),
    generatedTypescript: "// hand-written\nexport const APP_CONTRACT_VERSION = '1';",
  }), /generated_artifact_invalid/);
});
