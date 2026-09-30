import assert from "node:assert/strict";
import test from "node:test";

import { accessibleActionFields, BUILD_PLAN_FORM, validateActionFormInput } from "../src/action-forms.js";

test("closed companion action form accepts only its current reviewed fields", () => {
  validateActionFormInput(BUILD_PLAN_FORM, {
    topic_id: "topic_1",
    query: "battery recycling",
    start_date: "2026-08-01",
    end_date: "2026-08-23",
  });
  assert.throws(() => validateActionFormInput(BUILD_PLAN_FORM, {
    topic_id: "topic_1",
    query: "battery recycling",
    start_date: "2026-08-01",
    end_date: "2026-08-23",
    raw_provider_url: "https://internal.example.test",
  }), /unknown action field/);
});

test("bounded strings and exact dates fail before SDK dispatch", () => {
  assert.throws(() => validateActionFormInput(BUILD_PLAN_FORM, {
    topic_id: "topic_1",
    query: "x".repeat(4097),
    start_date: "08/01/2026",
    end_date: "2026-08-23",
  }));
  assert.throws(() => validateActionFormInput(BUILD_PLAN_FORM, {
    topic_id: "topic_1",
    query: "battery recycling",
    start_date: "2026-02-30",
    end_date: "2026-03-01",
  }), /YYYY-MM-DD/);
  assert.throws(() => validateActionFormInput(BUILD_PLAN_FORM, {
    topic_id: "topic id with spaces",
    query: "battery recycling",
    start_date: "2026-08-01",
    end_date: "2026-08-23",
  }), /canonical record id/);
});

test("form projection retains accessible labels and exact reference selector identity", () => {
  const fields = accessibleActionFields(BUILD_PLAN_FORM);
  const reference = fields.find((field) => field.referenceEntity !== undefined);
  assert.deepEqual(reference, {
    inputId: "action-build_plan-topic_id",
    label: "Research topic",
    describedBy: "action-build_plan-topic_id-help",
    autocomplete: "off",
    required: true,
    referenceEntity: "research_topic",
  });
});
