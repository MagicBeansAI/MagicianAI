//  MonitorFormTests.swift
//  Recurring Monitors (Phase 5, iOS) — the admission-mirror parity tests
//  (twin of the web `specForm.test.ts`): same stable snake_case reasons,
//  same normalization, schedule building, and `cadenceSummary` parity
//  strings against the canonical fixture spec.

import XCTest
@testable import Magician

final class MonitorFormTests: XCTestCase {

    private static let fixturesDir = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .appendingPathComponent("magician/tests/fixtures/monitors", isDirectory: true)

    private func fixtureSpec() throws -> Monitors.SpecV1 {
        let data = try Data(contentsOf: Self.fixturesDir
            .appendingPathComponent("monitor_spec_v1.json"))
        return try JSONDecoder().decode(Monitors.SpecV1.self, from: data)
    }

    private func rejectionReason(_ spec: Monitors.SpecV1) -> String? {
        if case .rejected(let reason) = MonitorForm.validateAndNormalize(spec) {
            return reason
        }
        return nil
    }

    // MARK: - Admission mirror (stable reasons)

    func testCanonicalFixtureSpecIsAdmittedUnchanged() throws {
        let spec = try fixtureSpec()
        guard case .ok(let normalized) = MonitorForm.validateAndNormalize(spec) else {
            return XCTFail("the canonical fixture spec must pass the mirror")
        }
        XCTAssertEqual(normalized, spec, "the fixture is already normalized — idempotent")
    }

    func testStableRejectionReasons() throws {
        var spec = try fixtureSpec()
        spec.schemaVersion = 2
        XCTAssertEqual(rejectionReason(spec), "monitor_schema_version_unsupported")

        spec = try fixtureSpec()
        spec.objective = "   "
        XCTAssertEqual(rejectionReason(spec), "monitor_objective_required")

        spec = try fixtureSpec()
        spec.objective = String(repeating: "x", count: MonitorForm.maxObjectiveChars + 1)
        XCTAssertEqual(rejectionReason(spec), "monitor_objective_too_long")

        spec = try fixtureSpec()
        spec.sources = Monitors.Sources()
        spec.querySeeds = []
        XCTAssertEqual(rejectionReason(spec), "monitor_sources_required")

        spec = try fixtureSpec()
        spec.sources.urls = ["ftp://files.example/pricing"]
        XCTAssertEqual(rejectionReason(spec), "monitor_source_url_scheme_unsupported")

        spec = try fixtureSpec()
        spec.sources.urls = ["not a url"]
        XCTAssertEqual(rejectionReason(spec), "monitor_source_url_invalid")

        spec = try fixtureSpec()
        spec.sources.urls = (0...MonitorForm.maxSourceURLs).map {
            "https://example.com/page-\($0)"
        }
        XCTAssertEqual(rejectionReason(spec), "monitor_source_urls_too_many")

        spec = try fixtureSpec()
        spec.includeRules = [String(repeating: "r", count: MonitorForm.maxListEntryChars + 1)]
        XCTAssertEqual(rejectionReason(spec), "monitor_include_rules_entry_too_long")

        spec = try fixtureSpec()
        spec.querySeeds = (0...MonitorForm.maxListEntries).map { "seed \($0)" }
        XCTAssertEqual(rejectionReason(spec), "monitor_query_seeds_too_many_entries")
    }

    func testAuthenticatedSourcesAloneDoNotSatisfySourcesRequired() throws {
        // Backend parity: authenticated_sources alone cannot drive a scan —
        // at least one URL, domain, or query seed is still required.
        var spec = try fixtureSpec()
        spec.sources = Monitors.Sources(
            authenticatedSources: ["analytics.example dashboard"])
        spec.querySeeds = []
        XCTAssertEqual(rejectionReason(spec), "monitor_sources_required")
    }

    func testSourceURLDedupComparesRawStrings() throws {
        // DOCUMENTED DIVERGENCE (parity with the web mirror): client-side
        // URL dedup compares the RAW string, so trailing-slash and case
        // variants both pass validation as TWO entries — canonical URL
        // dedup happens server-side.
        var spec = try fixtureSpec()
        spec.sources.urls = ["https://a.example/pricing",
                             "https://a.example/pricing/"]
        guard case .ok(let slashVariants) = MonitorForm.validateAndNormalize(spec) else {
            return XCTFail("trailing-slash variants must both pass client validation")
        }
        XCTAssertEqual(slashVariants.sources.urls.count, 2)

        spec.sources.urls = ["https://a.example/pricing",
                             "https://A.EXAMPLE/pricing"]
        guard case .ok(let caseVariants) = MonitorForm.validateAndNormalize(spec) else {
            return XCTFail("case variants must both pass client validation")
        }
        XCTAssertEqual(caseVariants.sources.urls.count, 2)
    }

    func testNormalizationTrimsDedupesAndDropsEmpties() throws {
        var spec = try fixtureSpec()
        spec.objective = "  Watch the page  "
        spec.querySeeds = [" alpha ", "", "alpha", "beta"]
        spec.sources.urls = ["https://a.example/x", "https://a.example/x", "  "]
        guard case .ok(let normalized) = MonitorForm.validateAndNormalize(spec) else {
            return XCTFail("spec must be admitted")
        }
        XCTAssertEqual(normalized.objective, "Watch the page")
        XCTAssertEqual(normalized.querySeeds, ["alpha", "beta"])
        XCTAssertEqual(normalized.sources.urls, ["https://a.example/x"])
    }

    // MARK: - Schedule building

    private func builtSchedule(_ form: MonitorForm) throws -> Monitors.ScheduleWire? {
        switch form.buildSchedule() {
        case .ok(let schedule): return schedule
        case .rejected(let reason):
            XCTFail("unexpected rejection: \(reason)")
            return nil
        }
    }

    func testPresetCadenceBuildsCron() throws {
        var form = MonitorForm()
        form.cadence = "weekly-mon-9"
        form.timezone = "America/Los_Angeles"
        let schedule = try XCTUnwrap(builtSchedule(form))
        XCTAssertEqual(schedule.kind,
                       .cron(expression: "0 9 * * 1", timezone: "America/Los_Angeles"))
        XCTAssertEqual(Monitors.cadenceSummary(schedule),
                       "Cron 0 9 * * 1 (America/Los_Angeles)")
    }

    func testCustomAndNoneAndInvalidCadence() throws {
        var form = MonitorForm()
        form.cadence = "none"
        XCTAssertNil(try builtSchedule(form), "none = run on demand only")

        form.cadence = "custom"
        form.cronExpression = "0 6 * * 1"
        let custom = try XCTUnwrap(builtSchedule(form))
        XCTAssertEqual(custom.kind, .cron(expression: "0 6 * * 1", timezone: nil))
        XCTAssertEqual(Monitors.cadenceSummary(custom), "Cron 0 6 * * 1")

        form.cronExpression = ""
        guard case .rejected(let emptyReason) = form.buildSchedule() else {
            return XCTFail("empty custom cron must reject")
        }
        XCTAssertEqual(emptyReason, "monitor_schedule_required")

        form.cronExpression = "9am daily"
        guard case .rejected(let invalidReason) = form.buildSchedule() else {
            return XCTFail("non-5-field cron must reject")
        }
        XCTAssertEqual(invalidReason, "monitor_schedule_invalid")
    }

    // MARK: - Cadence summary parity (backend `cadence_summary` strings)

    func testCadenceSummaryParityStrings() {
        XCTAssertEqual(
            Monitors.cadenceSummary(Monitors.ScheduleWire(
                kind: .cron(expression: "0 6 * * 1", timezone: "America/Los_Angeles"))),
            "Cron 0 6 * * 1 (America/Los_Angeles)")
        XCTAssertEqual(
            Monitors.cadenceSummary(Monitors.ScheduleWire(
                kind: .interval(seconds: 3600, jitterSeconds: nil))),
            "Every 3600s")
        XCTAssertEqual(Monitors.cadenceSummary(nil), "unscheduled")
        // Kind-level timezone wins; the schedule-level one is the fallback.
        XCTAssertEqual(
            Monitors.cadenceSummary(Monitors.ScheduleWire(
                kind: .cron(expression: "0 9 * * *", timezone: nil),
                timezone: "Europe/Berlin")),
            "Cron 0 9 * * * (Europe/Berlin)")
    }

    // MARK: - Edit round trip

    func testFormFromDetailRoundTripsSpecAndCadence() throws {
        let spec = try fixtureSpec()
        let detail = Monitors.DetailV1(
            taskID: "task_1", title: "Acme pricing", spec: spec, monitorRevision: 2,
            schedule: Monitors.ScheduleWire(
                kind: .cron(expression: "0 9 * * *", timezone: "America/Los_Angeles")),
            state: .init(status: "pending", scheduleFireCount: 0),
            createdAt: "c", updatedAt: "u", tags: ["system:monitor"])
        let form = MonitorForm(detail: detail)
        XCTAssertEqual(form.title, "Acme pricing")
        XCTAssertEqual(form.cadence, "daily-9", "a known cron maps back to its preset")
        XCTAssertEqual(form.timezone, "America/Los_Angeles")
        XCTAssertEqual(form.notificationPolicy, .materialChanges)

        guard case .ok(let rebuilt) = MonitorForm.validateAndNormalize(form.buildSpec()) else {
            return XCTFail("rebuilt spec must be admitted")
        }
        XCTAssertEqual(rebuilt, spec, "detail → form → spec is lossless")

        // An unknown cron lands on `custom`; no schedule lands on `none`.
        var custom = detail
        custom.schedule = Monitors.ScheduleWire(
            kind: .cron(expression: "13 3 * * 2", timezone: nil))
        XCTAssertEqual(MonitorForm(detail: custom).cadence, "custom")
        var unscheduled = detail
        unscheduled.schedule = nil
        XCTAssertEqual(MonitorForm(detail: unscheduled).cadence, "none")
    }

    func testNormalizedTitleAndReasonLabels() {
        var form = MonitorForm()
        form.title = "   "
        XCTAssertNil(form.normalizedTitle, "blank title → server derives from objective")
        form.title = " Pricing watch "
        XCTAssertEqual(form.normalizedTitle, "Pricing watch")

        XCTAssertEqual(MonitorForm.reasonLabel("monitor_objective_required"),
                       "Describe what to monitor.")
        XCTAssertEqual(MonitorForm.reasonLabel("monitor_domains_entry_too_long"),
                       "One entry is longer than \(MonitorForm.maxListEntryChars) characters.")
        XCTAssertEqual(MonitorForm.reasonLabel("some_unknown_reason"), "some_unknown_reason")
    }

    // MARK: - Convert prefill (Phase 7)

    func testConvertPrefillSeedsObjectiveFromDescriptionAndPinsCadenceNone() {
        let form = MonitorForm.convertPrefill(
            taskTitle: "  Acme pricing check  ",
            taskDescription: "  Check the Acme pricing page  ")
        XCTAssertEqual(form.title, "Acme pricing check")
        XCTAssertEqual(form.objective, "Check the Acme pricing page")
        XCTAssertEqual(form.cadence, "none",
                       "conversion never authors a schedule — the task keeps its own")
        XCTAssertEqual(form.cronExpression, "")
        // Everything else stays at composer defaults.
        let defaults = MonitorForm()
        XCTAssertEqual(form.urlsText, defaults.urlsText)
        XCTAssertEqual(form.matchMode, defaults.matchMode)
        XCTAssertEqual(form.notificationPolicy, defaults.notificationPolicy)
        XCTAssertEqual(form.notifyInitialBaseline, defaults.notifyInitialBaseline)
    }

    func testConvertPrefillFallsBackToTitleWhenDescriptionBlank() {
        let form = MonitorForm.convertPrefill(taskTitle: "Watch it", taskDescription: "   ")
        XCTAssertEqual(form.objective, "Watch it")
    }
}
