//
//  MonitorContractFixtureTests.swift
//  MagiosTests
//
//  Recurring Monitors Phase 0 — iOS side of the cross-platform contract
//  check. Reads the CANONICAL fixtures from magician/tests/fixtures/monitors/
//  in the repo (single source of truth — the Rust and web tests read the
//  same files; simulator tests share the host filesystem, and #filePath
//  anchors the repo location) and asserts the plan's structural contract +
//  semantic invariants (§7.3 removal safety, §7.4 dedupe key).
//
//  Phase 5 adds the TYPED decodes below the structural checks: every fixture
//  must decode through the `Monitors` wire models (§9.3 rule 6 — an
//  incompatible backend contract change fails this build together with the
//  Rust and vitest checks).
//

import XCTest
@testable import Magician

final class MonitorContractFixtureTests: XCTestCase {

    private static let fixturesDir = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()  // MagiosTests/
        .deletingLastPathComponent()  // magios/
        .deletingLastPathComponent()  // repo root
        .appendingPathComponent("magician/tests/fixtures/monitors", isDirectory: true)

    private func fixture(_ name: String) throws -> [String: Any] {
        let url = Self.fixturesDir.appendingPathComponent(name)
        let data = try Data(contentsOf: url)
        let json = try JSONSerialization.jsonObject(with: data)
        return try XCTUnwrap(json as? [String: Any], "\(name) is not a JSON object")
    }

    private func requireKeys(
        _ object: [String: Any], _ keys: [String], _ ctx: String,
        file: StaticString = #filePath, line: UInt = #line
    ) {
        for key in keys {
            XCTAssertNotNil(object[key], "\(ctx): required key `\(key)` missing",
                            file: file, line: line)
        }
    }

    func testMonitorSpecV1Shape() throws {
        let spec = try fixture("monitor_spec_v1.json")
        requireKeys(spec, [
            "schema_version", "objective", "query_seeds", "sources",
            "include_rules", "exclude_rules", "match_mode",
            "notification_policy", "notify_initial_baseline",
        ], "MonitorSpecV1")
        XCTAssertEqual(spec["schema_version"] as? Int, 1)
        let sources = try XCTUnwrap(spec["sources"] as? [String: Any])
        requireKeys(sources, ["urls", "domains", "authenticated_sources"], "sources")
        XCTAssertTrue(["strict", "balanced", "broad"]
            .contains(spec["match_mode"] as? String ?? ""))
        XCTAssertTrue(["material_changes", "every_run", "never"]
            .contains(spec["notification_policy"] as? String ?? ""))
        for raw in try XCTUnwrap(sources["urls"] as? [String]) {
            XCTAssertNotNil(URL(string: raw), "source url must parse: \(raw)")
        }
    }

    private func assertRunResultShape(_ run: [String: Any], _ ctx: String) throws {
        requireKeys(run, [
            "monitor_task_id", "execution_id", "monitor_revision",
            "started_at", "completed_at", "status", "complete_scan",
            "source_outcomes", "counts", "findings", "run_fingerprint",
        ], ctx)
        XCTAssertTrue(["baseline", "changed", "unchanged", "degraded", "failed"]
            .contains(run["status"] as? String ?? ""), "\(ctx): status enum")
        let counts = try XCTUnwrap(run["counts"] as? [String: Any])
        requireKeys(counts, ["scanned", "new", "updated", "unchanged", "possibly_removed"],
                    "\(ctx).counts")
        for case let finding as [String: Any] in try XCTUnwrap(run["findings"] as? [Any]) {
            requireKeys(finding, [
                "stable_key", "title", "source", "observed_at", "summary",
                "why_it_matters", "entities", "evidence",
                "content_fingerprint", "classification",
            ], "\(ctx).finding")
            XCTAssertTrue(["new", "updated", "unchanged", "possibly_removed"]
                .contains(finding["classification"] as? String ?? ""))
        }
    }

    func testChangedRunCarriesChangeFingerprint() throws {
        let run = try fixture("monitor_run_result_v1_changed.json")
        try assertRunResultShape(run, "changed run")
        XCTAssertEqual(run["status"] as? String, "changed")
        XCTAssertEqual(run["complete_scan"] as? Bool, true)
        XCTAssertNotNil(run["change_fingerprint"] as? String)
        XCTAssertFalse(try XCTUnwrap(run["findings"] as? [Any]).isEmpty)
    }

    func testUnchangedRunHasNoChangeFingerprint() throws {
        let run = try fixture("monitor_run_result_v1_unchanged.json")
        try assertRunResultShape(run, "unchanged run")
        XCTAssertEqual(run["status"] as? String, "unchanged")
        XCTAssertNil(run["change_fingerprint"],
                     "an unchanged run must not carry a change fingerprint")
    }

    func testDegradedRunRespectsRemovalSafety() throws {
        let run = try fixture("monitor_run_result_v1_degraded.json")
        try assertRunResultShape(run, "degraded run")
        XCTAssertEqual(run["status"] as? String, "degraded")
        // §7.3: an incomplete/auth-failed scan can never mark items removed.
        XCTAssertEqual(run["complete_scan"] as? Bool, false)
        let counts = try XCTUnwrap(run["counts"] as? [String: Any])
        XCTAssertEqual(counts["possibly_removed"] as? Int, 0)
        let outcomes = try XCTUnwrap(run["source_outcomes"] as? [[String: Any]])
        XCTAssertTrue(outcomes.contains {
            $0["status"] as? String == "auth_failed" && $0["complete"] as? Bool == false
        })
        let problem = try XCTUnwrap(run["access_problem"] as? [String: Any])
        requireKeys(problem, ["source", "kind", "message", "since"], "access_problem")
    }

    func testListPageUsesCursorEnvelope() throws {
        let page = try fixture("monitor_list_page_v1.json")
        requireKeys(page, ["items", "next_cursor", "limit"], "monitor list page")
        for item in try XCTUnwrap(page["items"] as? [[String: Any]]) {
            requireKeys(item, [
                "task_id", "title", "objective", "state", "cadence_summary",
                "monitor_revision", "last_run_status", "health",
            ], "monitor list item")
            XCTAssertTrue(["active", "paused"].contains(item["state"] as? String ?? ""))
        }
    }

    func testUpdateDetailDedupeKeyComposition() throws {
        let update = try fixture("monitor_update_detail_v1.json")
        requireKeys(update, [
            "update_id", "monitor_task_id", "monitor_revision", "execution_id",
            "occurred_at", "status", "change_fingerprint", "headline",
            "summary", "findings", "notification",
        ], "monitor update detail")
        let notification = try XCTUnwrap(update["notification"] as? [String: Any])
        requireKeys(notification, ["policy", "emitted", "channel", "dedupe_key"],
                    "notification")
        // §7.4: (scope, monitor_task_id, monitor_revision, change_fingerprint,
        // channel) — every component must appear in the dedupe key.
        let key = try XCTUnwrap(notification["dedupe_key"] as? String)
        let components = [
            try XCTUnwrap(update["monitor_task_id"] as? String),
            String(try XCTUnwrap(update["monitor_revision"] as? Int)),
            try XCTUnwrap(update["change_fingerprint"] as? String),
            try XCTUnwrap(notification["channel"] as? String),
        ]
        for component in components {
            XCTAssertTrue(key.contains(component),
                          "dedupe key missing `\(component)`: \(key)")
        }
        // The update shares the producing run's change fingerprint.
        let producing = try fixture("monitor_run_result_v1_changed.json")
        XCTAssertEqual(update["change_fingerprint"] as? String,
                       producing["change_fingerprint"] as? String)
    }

    // MARK: - Phase 5: typed decodes through the `Monitors` wire models

    private func fixtureData(_ name: String) throws -> Data {
        try Data(contentsOf: Self.fixturesDir.appendingPathComponent(name))
    }

    private func decodeFixture<T: Decodable>(_ type: T.Type, _ name: String) throws -> T {
        try JSONDecoder().decode(T.self, from: try fixtureData(name))
    }

    func testTypedSpecDecodesAndRoundTrips() throws {
        let spec = try decodeFixture(Monitors.SpecV1.self, "monitor_spec_v1.json")
        XCTAssertEqual(spec.schemaVersion, 1)
        XCTAssertEqual(spec.matchMode, .balanced)
        XCTAssertEqual(spec.notificationPolicy, .materialChanges)
        XCTAssertFalse(spec.notifyInitialBaseline)
        XCTAssertEqual(spec.sources.urls, ["https://acme-robotics.example/pricing"])
        XCTAssertEqual(spec.sources.domains, ["acme-robotics.example"])
        XCTAssertEqual(spec.sources.authenticatedSources, [])
        XCTAssertEqual(spec.querySeeds.count, 2)
        XCTAssertEqual(spec.includeRules.count, 3)
        // Round-trip: encode → decode → identical (snake_case keys stable).
        let encoded = try JSONEncoder().encode(spec)
        let decoded = try JSONDecoder().decode(Monitors.SpecV1.self, from: encoded)
        XCTAssertEqual(decoded, spec)
    }

    func testTypedChangedRunDecodes() throws {
        let run = try decodeFixture(Monitors.RunResultV1.self,
                                    "monitor_run_result_v1_changed.json")
        XCTAssertEqual(run.status, .changed)
        XCTAssertTrue(run.completeScan)
        XCTAssertNotNil(run.changeFingerprint)
        XCTAssertNil(run.accessProblem)
        XCTAssertEqual(run.counts.scanned, 12)
        XCTAssertEqual(run.counts.new, 1)
        XCTAssertEqual(run.findings.count, 2)
        XCTAssertEqual(run.findings[0].classification, .updated)
        XCTAssertEqual(run.findings[1].classification, .new)
        XCTAssertNil(run.findings[0].publishedAt, "fixture pins published_at: null")
        XCTAssertEqual(run.findings[0].evidence.first?.kind, "quote")
        XCTAssertEqual(run.sourceOutcomes.first?.status, .ok)
    }

    func testTypedUnchangedRunDecodesWithoutOptionals() throws {
        // Old/quiet records legitimately omit change_fingerprint and
        // access_problem — the models must stay tolerant (§9.3 rule 6).
        let run = try decodeFixture(Monitors.RunResultV1.self,
                                    "monitor_run_result_v1_unchanged.json")
        XCTAssertEqual(run.status, .unchanged)
        XCTAssertNil(run.changeFingerprint)
        XCTAssertNil(run.accessProblem)
        XCTAssertTrue(run.findings.isEmpty)
    }

    func testTypedDegradedRunDecodesAccessProblem() throws {
        let run = try decodeFixture(Monitors.RunResultV1.self,
                                    "monitor_run_result_v1_degraded.json")
        XCTAssertEqual(run.status, .degraded)
        XCTAssertFalse(run.completeScan)
        XCTAssertEqual(run.counts.possiblyRemoved, 0, "§7.3 removal safety")
        let problem = try XCTUnwrap(run.accessProblem)
        XCTAssertEqual(problem.kind, "auth_failed")
        XCTAssertTrue(run.sourceOutcomes.contains {
            $0.status == .authFailed && !$0.complete
        })
        XCTAssertEqual(run.sourceOutcomes.first?.note?.isEmpty, false)
    }

    func testTypedListPageDecodes() throws {
        let page = try decodeFixture(Monitors.ListPageV1.self, "monitor_list_page_v1.json")
        XCTAssertEqual(page.items.count, 2)
        XCTAssertEqual(page.nextCursor, "cur_task_monitor_fixture_002")
        XCTAssertEqual(page.limit, 50)
        // `total` counts the CORPUS, not the page — the fixture's 3 against 2
        // rows is what makes the difference provable, and what a pager needs
        // to know there is another page without following the cursor.
        XCTAssertEqual(page.total, 3)
        // `offset` is where the cursor RESOLVED to, not a number we sent.
        XCTAssertEqual(page.offset, 0)
        let first = try XCTUnwrap(page.items.first)
        XCTAssertEqual(first.taskID, "task_monitor_fixture_001")
        XCTAssertEqual(first.state, "active")
        // The server cadence string is shown VERBATIM — pin the exact format.
        XCTAssertEqual(first.cadenceSummary, "Cron 0 6 * * 1 (America/Los_Angeles)")
        XCTAssertEqual(first.lastRunStatus, "unchanged")
        XCTAssertEqual(first.health, "ok")
        XCTAssertEqual(page.items[1].health, "needs_attention")
        // The fixture is Phase-2-AHEAD by design: the live Phase 1 list
        // derives last_run_status from TaskState alone and only emits
        // never_ran | unchanged | failed — `degraded` here pins the future
        // run-ledger-backed shape the models must already tolerate.
        XCTAssertEqual(page.items[1].lastRunStatus, "degraded")
    }

    func testTypedListItemToleratesMissingOptionalFields() throws {
        // A monitor that never ran (Phase 1 live shape: no last_run_at, no
        // reserved next_run_at) must decode.
        let json = """
        {"task_id":"task_x","title":"T","objective":"O","state":"paused",
         "cadence_summary":"unscheduled","monitor_revision":1,
         "last_run_status":"never_ran","health":"ok"}
        """
        let item = try JSONDecoder().decode(Monitors.ListItemV1.self, from: Data(json.utf8))
        XCTAssertNil(item.lastRunAt)
        XCTAssertNil(item.nextRunAt)
        XCTAssertEqual(item.lastRunStatus, "never_ran")
    }

    func testTypedListPageDecodesNullCursor() throws {
        // Deliberately the PRE-`total` envelope: this is the exact body an
        // older server still sends, so it must keep decoding untouched. That
        // is why `total`/`offset` are optional on the model.
        let json = """
        {"items":[],"next_cursor":null,"limit":50}
        """
        let page = try JSONDecoder().decode(Monitors.ListPageV1.self, from: Data(json.utf8))
        XCTAssertNil(page.nextCursor)
        // Absent is "no page count, cursor paging only" — never zero pages.
        XCTAssertNil(page.total, "an absent total is unknown, not 0")
        XCTAssertNil(page.offset)
    }

    func testTypedUpdateDetailDecodes() throws {
        let update = try decodeFixture(Monitors.UpdateDetailV1.self,
                                       "monitor_update_detail_v1.json")
        XCTAssertEqual(update.updateID, "mu_fixture_0001")
        XCTAssertEqual(update.status, .changed)
        XCTAssertEqual(update.findings.count, 2)
        XCTAssertEqual(update.notification.policy, .materialChanges)
        XCTAssertTrue(update.notification.emitted)
        XCTAssertEqual(update.notification.channel, "today_changed")
        // §7.4 key composition, now through the typed model.
        for component in [update.monitorTaskID, String(update.monitorRevision),
                          try XCTUnwrap(update.changeFingerprint),
                          update.notification.channel] {
            XCTAssertTrue(update.notification.dedupeKey.contains(component))
        }
        // The producing run's fingerprint matches, typed end to end.
        let producing = try decodeFixture(Monitors.RunResultV1.self,
                                          "monitor_run_result_v1_changed.json")
        XCTAssertEqual(update.changeFingerprint, producing.changeFingerprint)
    }

    func testScheduleWireExternalTagging() throws {
        // The externally-tagged Cron shape the backend serializes.
        let cronJSON = """
        {"kind":{"Cron":{"expression":"0 6 * * 1","timezone":"America/Los_Angeles"}},
         "timezone":"America/Los_Angeles","paused":false}
        """
        let cron = try JSONDecoder().decode(Monitors.ScheduleWire.self, from: Data(cronJSON.utf8))
        XCTAssertEqual(cron.kind, .cron(expression: "0 6 * * 1",
                                        timezone: "America/Los_Angeles"))
        XCTAssertEqual(cron.paused, false)
        XCTAssertEqual(Monitors.cadenceSummary(cron), "Cron 0 6 * * 1 (America/Los_Angeles)")

        let intervalJSON = """
        {"kind":{"Interval":{"seconds":3600}}}
        """
        let interval = try JSONDecoder().decode(Monitors.ScheduleWire.self,
                                                from: Data(intervalJSON.utf8))
        XCTAssertEqual(interval.kind, .interval(seconds: 3600, jitterSeconds: nil))
        XCTAssertEqual(Monitors.cadenceSummary(interval), "Every 3600s")

        // Unknown kinds (Once/OnEvent/future) ride `.other` untouched.
        let onceJSON = """
        {"kind":{"Once":{"at":"2026-08-01T09:00:00Z"}}}
        """
        let once = try JSONDecoder().decode(Monitors.ScheduleWire.self, from: Data(onceJSON.utf8))
        guard case .other = once.kind else {
            return XCTFail("Once must decode into the tolerant .other arm")
        }
        XCTAssertEqual(Monitors.cadenceSummary(once), "Once at 2026-08-01T09:00:00Z")
        XCTAssertEqual(Monitors.cadenceSummary(nil), "unscheduled")

        // Encoding round-trips the external tagging.
        let encoded = try JSONEncoder().encode(cron)
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: encoded) as? [String: Any])
        let kind = try XCTUnwrap(object["kind"] as? [String: Any])
        let payload = try XCTUnwrap(kind["Cron"] as? [String: Any])
        XCTAssertEqual(payload["expression"] as? String, "0 6 * * 1")
    }

    /// Adversarial-review C1: KNOWN schedule variants must decode STRICTLY.
    /// A malformed `Cron`/`Interval` payload throws instead of silently
    /// demoting to `.other` (which would render "unscheduled", misread the
    /// edit form, and mask a backend regression). UNKNOWN variant keys keep
    /// riding the tolerant `.other` arm.
    func testScheduleKindKnownVariantsDecodeStrictly() throws {
        // {"Cron":{}} — the variant key exists but the payload is malformed
        // (missing `expression`) → the decode must THROW.
        XCTAssertThrowsError(try JSONDecoder().decode(
            Monitors.ScheduleWire.self,
            from: Data(#"{"kind":{"Cron":{}}}"#.utf8)),
            "a malformed Cron payload must throw, not demote to .other")

        // {"Interval":{"seconds":"not-a-number"}} → THROW.
        XCTAssertThrowsError(try JSONDecoder().decode(
            Monitors.ScheduleWire.self,
            from: Data(#"{"kind":{"Interval":{"seconds":"not-a-number"}}}"#.utf8)),
            "a malformed Interval payload must throw, not demote to .other")

        // UNKNOWN variants stay tolerant: Once still decodes as `.other`.
        let once = try JSONDecoder().decode(
            Monitors.ScheduleWire.self,
            from: Data(#"{"kind":{"Once":{"at":"2026-08-01T09:00:00Z"}}}"#.utf8))
        guard case .other = once.kind else {
            return XCTFail("Once must keep decoding into the tolerant .other arm")
        }

        // Valid Cron round-trips the exact externally-tagged byte shape:
        // {"kind":{"Cron":{"expression":…,"timezone":…}}}.
        let cron = Monitors.ScheduleWire(
            kind: .cron(expression: "0 6 * * 1", timezone: "America/Los_Angeles"))
        let cronObject = try XCTUnwrap(JSONSerialization.jsonObject(
            with: JSONEncoder().encode(cron)) as? [String: Any])
        let cronKind = try XCTUnwrap(cronObject["kind"] as? [String: Any])
        XCTAssertEqual(Set(cronKind.keys), ["Cron"], "ONE variant key")
        let cronPayload = try XCTUnwrap(cronKind["Cron"] as? [String: Any])
        XCTAssertEqual(cronPayload["expression"] as? String, "0 6 * * 1")
        XCTAssertEqual(cronPayload["timezone"] as? String, "America/Los_Angeles")
        XCTAssertEqual(
            try JSONDecoder().decode(Monitors.ScheduleWire.self,
                                     from: JSONEncoder().encode(cron)).kind,
            cron.kind, "Cron encode → decode is lossless")

        // Valid Interval round-trips too ({"Interval":{"seconds":…,
        // "jitter_seconds":…}} — snake_case payload key).
        let interval = Monitors.ScheduleWire(
            kind: .interval(seconds: 3600, jitterSeconds: 30))
        let intervalObject = try XCTUnwrap(JSONSerialization.jsonObject(
            with: JSONEncoder().encode(interval)) as? [String: Any])
        let intervalKind = try XCTUnwrap(intervalObject["kind"] as? [String: Any])
        XCTAssertEqual(Set(intervalKind.keys), ["Interval"])
        let intervalPayload = try XCTUnwrap(intervalKind["Interval"] as? [String: Any])
        XCTAssertEqual(intervalPayload["seconds"] as? Int, 3600)
        XCTAssertEqual(intervalPayload["jitter_seconds"] as? Int, 30)
        XCTAssertEqual(
            try JSONDecoder().decode(Monitors.ScheduleWire.self,
                                     from: JSONEncoder().encode(interval)).kind,
            interval.kind, "Interval encode → decode is lossless")
    }

    func testTypedMonitorDetailDecodes() throws {
        // Shape from monitors_api::get_monitor_v3_handler (no canonical
        // fixture exists for the detail; this pins the handler's JSON).
        let specJSON = try XCTUnwrap(
            String(data: try fixtureData("monitor_spec_v1.json"), encoding: .utf8))
        let json = """
        {"task_id":"task_monitor_fixture_001","title":"Acme Robotics pricing",
         "spec":\(specJSON),
         "monitor_revision":2,
         "schedule":{"kind":{"Cron":{"expression":"0 6 * * 1","timezone":"America/Los_Angeles"}}},
         "state":{"status":"pending","schedule_fire_count":4},
         "created_at":"2026-07-20T00:00:00Z","updated_at":"2026-07-23T06:01:05Z",
         "tags":["system:monitor"]}
        """
        let detail = try JSONDecoder().decode(Monitors.DetailV1.self, from: Data(json.utf8))
        XCTAssertEqual(detail.taskID, "task_monitor_fixture_001")
        XCTAssertEqual(detail.monitorRevision, 2)
        XCTAssertEqual(detail.state.scheduleFireCount, 4)
        XCTAssertTrue(detail.tags.contains("system:monitor"))
        XCTAssertEqual(Monitors.cadenceSummary(detail.schedule),
                       "Cron 0 6 * * 1 (America/Los_Angeles)")
        // An old/unscheduled monitor detail (no schedule key) must decode.
        let bare = """
        {"task_id":"t","title":"x","spec":{"schema_version":1,"objective":"o",
         "query_seeds":[],"sources":{"urls":["https://a.example"],"domains":[],
         "authenticated_sources":[]},"include_rules":[],"exclude_rules":[],
         "match_mode":"strict","notification_policy":"never",
         "notify_initial_baseline":true},"monitor_revision":1,"schedule":null,
         "state":{"status":"pending","schedule_fire_count":0},
         "created_at":"c","updated_at":"u","tags":[]}
        """
        let unscheduled = try JSONDecoder().decode(Monitors.DetailV1.self, from: Data(bare.utf8))
        XCTAssertNil(unscheduled.schedule)
        XCTAssertEqual(unscheduled.spec.notificationPolicy, .never)
    }
}
