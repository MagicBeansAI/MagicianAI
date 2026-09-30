import assert from "node:assert/strict";
import test from "node:test";

import {
  exerciseReviewedBuildPlanDependencies,
  exerciseSealedAuthoringLeaves,
} from "../src/dependency-fixtures.js";

const INPUT = {
  topic_id: "topic_1",
  query: "battery recycling",
  start_date: "2026-08-01",
  end_date: "2026-08-23",
} as const;

test("ToolSkill, time_math, and vendored procedure ports execute in their reviewed order", async () => {
  const calls: string[] = [];
  const result = await exerciseReviewedBuildPlanDependencies(INPUT, {
    timeMathDateRange(input) {
      calls.push("time_math.date_range");
      assert.deepEqual(input, { start_date: "2026-08-01", end_date: "2026-08-23" });
      return { ...input, inclusive_days: 23 };
    },
    researchOutlineNormalize(input) {
      calls.push("research_outline.normalize");
      assert.deepEqual(input, { objective: "battery recycling", questions: ["battery recycling"] });
      return { objective: input.objective, questions: ["materials", "policy"] };
    },
    planBrief(input) {
      calls.push("skill:plan-brief");
      assert.equal(input.date_range.inclusive_days, 23);
      assert.deepEqual(input.outline.questions, ["materials", "policy"]);
      return { body: "Review materials and policy evidence." };
    },
  });
  assert.deepEqual(calls, result.trace);
  assert.equal(result.body, "Review materials and policy evidence.");
});

test("dependency qualification refuses substituted or unbounded owner results", async () => {
  await assert.rejects(exerciseReviewedBuildPlanDependencies(INPUT, {
    timeMathDateRange(input) {
      return { ...input, inclusive_days: 23 };
    },
    researchOutlineNormalize(input) {
      return { ...input, questions: Array.from({ length: 17 }, (_, index) => `question ${index}`) };
    },
    planBrief() {
      return { body: "must not run" };
    },
  }), /research_outline\.normalize fixture returned an invalid bounded result/);
});

test("sealed agent and all four Browser dependencies expose only exact reviewed leaves", async () => {
  const calls: string[] = [];
  let snapshots = 0;
  const result = await exerciseSealedAuthoringLeaves("Summarize the open page", {
    researchPlannerWorkerAgentAsTool(input) {
      calls.push("research-planner-worker.agent_as_tool");
      assert.deepEqual(input, { request: "Summarize the open page" });
      assert.equal(Object.keys(input).some((key) => /delegate|agent_id|task_id/.test(key)), false);
      return { answer: "No broader delegation authority was supplied." };
    },
    browserSnapshot(input) {
      calls.push("browser.snapshot");
      assert.deepEqual(input, { action: "observe" });
      assert.equal(
        Object.keys(input).some((key) => /navigate|click|type|session|tab|selector|cdp|profile|argv/.test(key)),
        false,
      );
      snapshots += 1;
      return {
        kind: "app_browser_interactive",
        success: true,
        origin: snapshots === 1 ? null : "https://example.com",
        content_digest: `blake3:${String(snapshots).repeat(64)}`,
        content: snapshots === 3 ? "after-scroll" : "bounded observation",
        elements: [`browser-element:fixture-${snapshots}`],
        observation_ref: `interactive-observation:fixture-${snapshots}`,
      };
    },
    browserNavigate(input) {
      calls.push("browser.navigate");
      assert.deepEqual(input, { action: "navigate", url: "https://example.com/reference" });
      assert.equal(Object.keys(input).some((key) => /session|tab|selector|script|cdp|file|download|argv/.test(key)), false);
      return browserActionResult("https://example.com", "navigated");
    },
    browserScroll(input) {
      calls.push("browser.scroll");
      assert.deepEqual(input, {
        action: "scroll",
        observation_ref: "interactive-observation:fixture-2",
        direction: "down",
        pixels: 640,
      });
      return browserActionResult("https://example.com", "scrolled");
    },
    browserClick(input) {
      calls.push("browser.click");
      assert.deepEqual(input, {
        action: "click",
        observation_ref: "interactive-observation:fixture-3",
        element_ref: "browser-element:fixture-3",
      });
      return browserActionResult("https://example.com", "clicked");
    },
  });
  assert.deepEqual(calls, result.trace);
  assert.equal(result.observation.content, "after-scroll");
  assert.equal(result.navigation.content, "navigated");
  assert.equal(result.scroll.content, "scrolled");
  assert.equal(result.click.content, "clicked");
});

test("sealed authoring leaf qualification refuses substituted result vocabulary", async () => {
  await assert.rejects(exerciseSealedAuthoringLeaves("Observe", {
    researchPlannerWorkerAgentAsTool() {
      return { answer: "bounded" };
    },
    browserSnapshot() {
      return {
        kind: "app_browser_interactive",
        success: true,
        origin: null,
        content_digest: null,
        content: null,
        elements: [],
        observation_ref: "interactive-observation:fixture",
        tab_id: "raw-tab",
      } as never;
    },
    browserNavigate() {
      throw new Error("must not run");
    },
    browserScroll() {
      throw new Error("must not run");
    },
    browserClick() {
      throw new Error("must not run");
    },
  }), /browser\.snapshot fixture returned an invalid bounded observation/);
});

test("Browser interaction fixture refuses replayed observations before physical input", async () => {
  let scrollCalled = false;
  await assert.rejects(exerciseSealedAuthoringLeaves("Observe", {
    researchPlannerWorkerAgentAsTool() {
      return { answer: "bounded" };
    },
    browserSnapshot() {
      return {
        kind: "app_browser_interactive",
        success: true,
        origin: "https://example.com",
        content_digest: `blake3:${"b".repeat(64)}`,
        content: "bounded",
        elements: ["browser-element:fixture"],
        observation_ref: "interactive-observation:replayed",
      };
    },
    browserNavigate() {
      return browserActionResult("https://example.com", "navigated");
    },
    browserScroll() {
      scrollCalled = true;
      return browserActionResult("https://example.com", "must not run");
    },
    browserClick() {
      throw new Error("must not run");
    },
  }), /refused a replayed observation/);
  assert.equal(scrollCalled, false);
});

function browserActionResult(origin: string, content: string) {
  return {
    kind: "app_browser_interactive" as const,
    success: true,
    origin,
    content_digest: `blake3:${"a".repeat(64)}`,
    content,
    elements: [],
  };
}
