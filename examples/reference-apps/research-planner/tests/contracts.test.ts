import assert from "node:assert/strict";
import test from "node:test";

import type { AppRecordProjection } from "@magician/apps";
import {
  isCalendarDate,
  isResearchProjection,
  isRfc3339,
} from "../src/contracts.js";

function plan(fields: AppRecordProjection["fields"]): AppRecordProjection {
  return {
    entity: "research_plan",
    record_id: "plan_1",
    record_revision: 1,
    fields,
  };
}

test("research projections preserve optional versus explicit-null fields", () => {
  assert.equal(isResearchProjection(plan({
    topic_id: "topic_1",
    body: "Bounded plan",
    status: "draft",
  })), true);
  assert.equal(isResearchProjection(plan({
    topic_id: "topic_1",
    body: "Bounded plan",
    status: "draft",
    revision_note: null,
  })), true);
  assert.equal(isResearchProjection(plan({
    topic_id: "topic_1",
    body: "Bounded plan",
    status: "draft",
    substituted: "hidden",
  })), false);
});

test("calendar and timestamp validators reject normalized-looking invalid dates", () => {
  assert.equal(isCalendarDate("2026-02-28"), true);
  assert.equal(isCalendarDate("2026-02-30"), false);
  assert.equal(isRfc3339("2026-08-23T12:30:45Z"), true);
  assert.equal(isRfc3339("2026-02-30T12:30:45Z"), false);
  assert.equal(isRfc3339("2026-08-23 12:30:45Z"), false);
});
