import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

test("the public manifest exercises the bounded detail/form/section/list/table surface", async () => {
  const manifest = await readFile(new URL("../../app/SKILL.md", import.meta.url), "utf8");
  for (const kind of ["section", "detail", "form", "list", "table"]) {
    assert.match(manifest, new RegExp(`\\n\\s+- component: ${kind}\\n`));
  }
  assert.match(manifest, /id: current_topic\n\s+fields: \[title, query, status, priority, updated_at\]/);
  assert.match(manifest, /id: create_topic\n\s+fields: \[title, query, status, priority, updated_at\]/);
  assert.match(manifest, /id: topic_table\n\s+columns: \[title, status, priority, updated_at\]/);
  for (const forbidden of ["raw_html:", "script:", "custom_transport:", "provider_url:"]) {
    assert.equal(manifest.includes(forbidden), false, forbidden);
  }
});

test("base and update manifests select the exact D8 Browser roster as singleton grants", async () => {
  const manifests = await Promise.all([
    readFile(new URL("../../app/SKILL.md", import.meta.url), "utf8"),
    readFile(new URL(
      import.meta.url.includes("/dist/tests/")
        ? "../../../research-planner-update-v0.2.0/app/SKILL.md"
        : "../../research-planner-update-v0.2.0/app/SKILL.md",
      import.meta.url,
    ), "utf8"),
  ]);
  const expected = [
    ["snapshot", "observe", "524288"],
    ["navigate", "navigate_or_launch", "32768"],
    ["scroll", "interact", "32768"],
    ["click", "outward_commit", "32768"],
  ] as const;
  for (const manifest of manifests) {
    for (const [action, actionClass, outputBytes] of expected) {
      const block = dependencyBlock(manifest, `browser__${action}`);
      assert.doesNotMatch(block, /primitive_ref:/);
      assert.match(block, new RegExp(`actions: \\[${action}\\]`));
      assert.match(block, new RegExp(`action_classes: \\[${actionClass}\\]`));
      assert.match(block, /allowed_origins: \[about:blank, "https:\/\/example\.com"\]/);
      assert.match(block, /target_profile_class: installation_ephemeral_headless/);
      assert.match(block, /background: direct_owner/);
      assert.match(block, /capture: structured_evidence_only/);
      assert.match(block, /transfer: denied/);
      assert.match(block, new RegExp(`max_output_bytes: ${outputBytes}`));
      assert.match(block, /session: run_bound/);
      assert.doesNotMatch(block, /actions: \[[^\]]+,/);
      assert.doesNotMatch(block, /raw_selector|script|session_id|tab_id|cdp|argv|download/);
    }
  }
});

test("base and update declare only the implemented bounded P6 memory proposal", async () => {
  const manifests = await Promise.all([
    readFile(new URL("../../app/SKILL.md", import.meta.url), "utf8"),
    readFile(new URL(
      import.meta.url.includes("/dist/tests/")
        ? "../../../research-planner-update-v0.2.0/app/SKILL.md"
        : "../../research-planner-update-v0.2.0/app/SKILL.md",
      import.meta.url,
    ), "utf8"),
  ]);
  for (const manifest of manifests) {
    assert.match(manifest, /plan_memory:\n\s+source:\n\s+kind: mutation_backed_entity_projection/);
    assert.match(manifest, /entity: research_plan\n\s+selected_fields: \[topic_id, body, status\]/);
    assert.match(manifest, /destination: memory/);
    assert.match(manifest, /evidence_classes: \[hypothesis\]/);
    assert.match(manifest, /frequency: \{ max_proposals: 4, window_seconds: 3600 \}/);
    assert.match(manifest, /maximum_retention_seconds: 604800/);
    assert.doesNotMatch(manifest, /destination: (attention|notification|task|plan)/);
  }
});

function dependencyBlock(manifest: string, name: string): string {
  const start = manifest.indexOf(`      - name: ${name}\n`);
  assert.notEqual(start, -1, `missing dependency ${name}`);
  const next = manifest.indexOf("      - name: ", start + 1);
  return manifest.slice(start, next === -1 ? manifest.length : next);
}
