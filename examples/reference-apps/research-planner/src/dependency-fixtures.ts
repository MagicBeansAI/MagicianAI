import type { BuildPlanInput } from "./contracts.js";
import { BUILD_PLAN_FORM, validateActionFormInput } from "./action-forms.js";

const MAX_QUESTIONS = 16;
const MAX_TEXT_BYTES = 4096;

export interface DateRangeValue {
  readonly start_date: string;
  readonly end_date: string;
  readonly inclusive_days: number;
}

export interface OutlineValue {
  readonly objective: string;
  readonly questions: readonly string[];
}

export interface PlanBriefValue {
  readonly body: string;
}

export interface ReviewedBuildPlanDependencyPorts {
  timeMathDateRange(input: { readonly start_date: string; readonly end_date: string }): DateRangeValue | Promise<DateRangeValue>;
  researchOutlineNormalize(input: OutlineValue): OutlineValue | Promise<OutlineValue>;
  planBrief(input: { readonly outline: OutlineValue; readonly date_range: DateRangeValue }): PlanBriefValue | Promise<PlanBriefValue>;
}

export interface DependencyFixtureResult {
  readonly body: string;
  readonly trace: readonly ["time_math.date_range", "research_outline.normalize", "skill:plan-brief"];
}

export interface SealedAuthoringLeafPorts {
  researchPlannerWorkerAgentAsTool(input: { readonly request: string }):
    { readonly answer: string } | Promise<{ readonly answer: string }>;
  browserSnapshot(input: { readonly action: "observe" }): BrowserObserveValue | Promise<BrowserObserveValue>;
  browserNavigate(input: { readonly action: "navigate"; readonly url: "https://example.com/reference" }):
    BrowserActionValue | Promise<BrowserActionValue>;
  browserScroll(input: {
    readonly action: "scroll";
    readonly observation_ref: string;
    readonly direction: "down";
    readonly pixels: 640;
  }): BrowserActionValue | Promise<BrowserActionValue>;
  browserClick(input: {
    readonly action: "click";
    readonly observation_ref: string;
    readonly element_ref: string;
  }): BrowserActionValue | Promise<BrowserActionValue>;
}

export interface BrowserObserveValue {
  readonly kind: "app_browser_interactive";
  readonly success: boolean;
  readonly origin: string | null;
  readonly content_digest: string | null;
  readonly content: string | null;
  readonly elements: readonly string[];
  readonly observation_ref: string | null;
}

export interface BrowserActionValue {
  readonly kind: "app_browser_interactive";
  readonly success: boolean;
  readonly origin: string | null;
  readonly content_digest: string | null;
  readonly content: string | null;
  readonly elements: readonly string[];
}

export interface SealedAuthoringLeafResult {
  readonly answer: string;
  readonly observation: BrowserObserveValue;
  readonly navigation: BrowserActionValue;
  readonly scroll: BrowserActionValue;
  readonly click: BrowserActionValue;
  readonly trace: readonly [
    "research-planner-worker.agent_as_tool",
    "browser.snapshot",
    "browser.navigate",
    "browser.snapshot",
    "browser.scroll",
    "browser.snapshot",
    "browser.click",
  ];
}

/**
 * Provider-free consumer proof for the two explicitly selected authoring
 * leaves. The ports contain neither delegation controls nor browser session,
 * tab, selector, script, type, profile, CDP, file, download, or raw argv
 * fields. Mutation leaves consume only fresh opaque observation/element refs.
 */
export async function exerciseSealedAuthoringLeaves(
  request: string,
  ports: SealedAuthoringLeafPorts,
): Promise<SealedAuthoringLeafResult> {
  const seenObservationRefs = new Set<string>();
  if (typeof request !== "string"
    || request.length === 0
    || new TextEncoder().encode(request).byteLength > 16_384) {
    throw new Error("agent_as_tool fixture input exceeds its reviewed contract");
  }
  const agent = await ports.researchPlannerWorkerAgentAsTool({ request });
  if (!hasExactKeys(agent, ["answer"])
    || typeof agent.answer !== "string"
    || agent.answer.length === 0
    || new TextEncoder().encode(agent.answer).byteLength > 32_768) {
    throw new Error("agent_as_tool fixture returned an invalid bounded result");
  }
  const initial = await ports.browserSnapshot({ action: "observe" });
  assertBrowserObservation(initial);
  rememberObservation(initial, seenObservationRefs, "browser.snapshot");
  const navigation = await ports.browserNavigate({
    action: "navigate",
    url: "https://example.com/reference",
  });
  assertBrowserAction(navigation, "browser.navigate");
  const afterNavigate = await ports.browserSnapshot({ action: "observe" });
  assertBrowserObservation(afterNavigate);
  const navigateObservation = requireFreshObservation(
    afterNavigate,
    seenObservationRefs,
    "browser.scroll",
  );
  const scroll = await ports.browserScroll({
    action: "scroll",
    observation_ref: navigateObservation.observationRef,
    direction: "down",
    pixels: 640,
  });
  assertBrowserAction(scroll, "browser.scroll");
  const afterScroll = await ports.browserSnapshot({ action: "observe" });
  assertBrowserObservation(afterScroll);
  const scrollObservation = requireFreshObservation(
    afterScroll,
    seenObservationRefs,
    "browser.click",
  );
  const click = await ports.browserClick({
    action: "click",
    observation_ref: scrollObservation.observationRef,
    element_ref: scrollObservation.elementRef,
  });
  assertBrowserAction(click, "browser.click");
  return {
    answer: agent.answer,
    observation: afterScroll,
    navigation,
    scroll,
    click,
    trace: [
      "research-planner-worker.agent_as_tool",
      "browser.snapshot",
      "browser.navigate",
      "browser.snapshot",
      "browser.scroll",
      "browser.snapshot",
      "browser.click",
    ],
  };
}

/**
 * Provider-free qualification of the exact public dependency ports declared
 * by the package. Callers supply deterministic doubles; production still uses
 * the reviewed ToolSkill, compiled transform, and vendored procedure owners.
 */
export async function exerciseReviewedBuildPlanDependencies(
  input: BuildPlanInput,
  ports: ReviewedBuildPlanDependencyPorts,
): Promise<DependencyFixtureResult> {
  validateActionFormInput(BUILD_PLAN_FORM, input);
  const dateRange = await ports.timeMathDateRange({
    start_date: input.start_date,
    end_date: input.end_date,
  });
  assertDateRange(dateRange, input);
  const outlineInput: OutlineValue = { objective: input.query, questions: [input.query] };
  const outline = await ports.researchOutlineNormalize(outlineInput);
  assertOutline(outline);
  const brief = await ports.planBrief({ outline, date_range: dateRange });
  if (!hasExactKeys(brief, ["body"]) || !boundedText(brief.body)) {
    throw new Error("plan-brief fixture returned an invalid bounded result");
  }
  return {
    body: brief.body,
    trace: ["time_math.date_range", "research_outline.normalize", "skill:plan-brief"],
  };
}

function assertDateRange(value: DateRangeValue, input: BuildPlanInput): void {
  if (!hasExactKeys(value, ["end_date", "inclusive_days", "start_date"])
    || value.start_date !== input.start_date
    || value.end_date !== input.end_date
    || !Number.isSafeInteger(value.inclusive_days)
    || value.inclusive_days < 1
    || value.inclusive_days > 3660) {
    throw new Error("time_math.date_range fixture returned an invalid bounded result");
  }
}

function assertOutline(value: OutlineValue): void {
  if (!hasExactKeys(value, ["objective", "questions"])
    || !boundedText(value.objective)
    || !Array.isArray(value.questions)
    || value.questions.length < 1
    || value.questions.length > MAX_QUESTIONS
    || value.questions.some((question) => !boundedText(question))) {
    throw new Error("research_outline.normalize fixture returned an invalid bounded result");
  }
}

function assertBrowserObservation(value: BrowserObserveValue): void {
  if (!hasExactKeys(value, [
    "content", "content_digest", "elements", "kind", "observation_ref", "origin", "success",
  ])
    || value.kind !== "app_browser_interactive"
    || typeof value.success !== "boolean"
    || !reviewedBrowserOrigin(value.origin)
    || (value.content_digest !== null
      && (typeof value.content_digest !== "string" || value.content_digest.length > 80))
    || (value.content !== null
      && (typeof value.content !== "string"
        || new TextEncoder().encode(value.content).byteLength > 393_216))
    || !Array.isArray(value.elements)
    || value.elements.length > 1_024
    || value.elements.some((element) => (
      typeof element !== "string" || element.length > 96 || !element.startsWith("browser-element:")
    ))
    || (value.observation_ref !== null
      && (typeof value.observation_ref !== "string"
        || value.observation_ref.length > 192
        || !value.observation_ref.startsWith("interactive-observation:")))) {
    throw new Error("browser.snapshot fixture returned an invalid bounded observation");
  }
}

function assertBrowserAction(value: BrowserActionValue, label: string): void {
  if (!hasExactKeys(value, ["content", "content_digest", "elements", "kind", "origin", "success"])
    || value.kind !== "app_browser_interactive"
    || typeof value.success !== "boolean"
    || !reviewedBrowserOrigin(value.origin)
    || (value.content_digest !== null
      && (typeof value.content_digest !== "string" || value.content_digest.length > 80))
    || (value.content !== null
      && (typeof value.content !== "string"
        || new TextEncoder().encode(value.content).byteLength > 524_288))
    || !Array.isArray(value.elements)
    || value.elements.length > 1_024
    || value.elements.some((element) => typeof element !== "string" || element.length > 96)) {
    throw new Error(`${label} fixture returned an invalid bounded result`);
  }
}

function requireFreshObservation(
  value: BrowserObserveValue,
  seen: Set<string>,
  label: string,
): { readonly observationRef: string; readonly elementRef: string } {
  const elementRef = value.elements[0];
  if (value.observation_ref === null || elementRef === undefined) {
    throw new Error(`${label} fixture requires a fresh opaque observation and element`);
  }
  if (seen.has(value.observation_ref)) {
    throw new Error(`${label} fixture refused a replayed observation`);
  }
  seen.add(value.observation_ref);
  return { observationRef: value.observation_ref, elementRef };
}

function rememberObservation(value: BrowserObserveValue, seen: Set<string>, label: string): void {
  if (value.observation_ref === null) return;
  if (seen.has(value.observation_ref)) throw new Error(`${label} fixture refused a replayed observation`);
  seen.add(value.observation_ref);
}

function reviewedBrowserOrigin(value: string | null): boolean {
  return value === null || value === "about:blank" || value === "https://example.com";
}

function boundedText(value: unknown): value is string {
  return typeof value === "string" && value.length > 0
    && new TextEncoder().encode(value).byteLength <= MAX_TEXT_BYTES;
}

function hasExactKeys(value: object, keys: readonly string[]): boolean {
  const observed = Object.keys(value).sort();
  const expected = [...keys].sort();
  return observed.length === expected.length
    && observed.every((key, index) => key === expected[index]);
}
