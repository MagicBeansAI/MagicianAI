//
//  VibeDevFixtureTests.swift
//  MagiosTests
//
//  The iOS side of the VibeDev wire contract: every canonical fixture in
//  magician/tests/fixtures/vibedev/ (written by the Rust tests from the same
//  types the endpoints serialize, and read by the magdroid and web tests too)
//  must decode through the VibeDev models. A server shape change fails here
//  together with Rust and Android.
//
//  Also: the token-usage check — VibeDev screens draw only with theme tokens.
//

import XCTest
@testable import Magician

final class VibeDevFixtureTests: XCTestCase {

    private static let repoRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()  // MagiosTests/
        .deletingLastPathComponent()  // magios/
        .deletingLastPathComponent()  // repo root

    private static let fixturesDir = repoRoot
        .appendingPathComponent("magician/tests/fixtures/vibedev", isDirectory: true)

    private func decode<T: Decodable>(_ type: T.Type, _ name: String,
                                      file: StaticString = #filePath, line: UInt = #line) throws -> T {
        let data = try Data(contentsOf: Self.fixturesDir.appendingPathComponent(name))
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            XCTFail("\(name) does not decode as \(T.self): \(error)", file: file, line: line)
            throw error
        }
    }

    func testLiveRunsFixture() throws {
        let response = try decode(VibeDevLiveRunsResponse.self, "live_runs.json")
        XCTAssertFalse(response.runs.isEmpty)
        for run in response.runs {
            XCTAssertFalse(run.runId.isEmpty)
            XCTAssertFalse(run.liveTaskId.isEmpty, "steer/stop need live_task_id")
            XCTAssertFalse(run.projectId.isEmpty)
            XCTAssertNotNil(VibeDevTime.parse(run.updatedAt), "updated_at is RFC 3339")
        }
        let states = Set(response.runs.map(\.state))
        XCTAssertTrue(states.contains(.needsYou))
        XCTAssertTrue(states.contains(.working))
        XCTAssertTrue(states.contains(.done))
        let lane = try XCTUnwrap(response.runs.first { $0.lane != nil })
        XCTAssertNotNil(lane.crewId, "a lane run names its crew")
        XCTAssertEqual(lane.shortName, "\(lane.lane!) lane")
        XCTAssertNotNil(response.runs.first { $0.state == .working }?.step)
        if let working = response.runs.first(where: { $0.state == .working }), working.startedAt != nil {
            XCTAssertNotNil(working.startedDate, "started_at is RFC 3339")
        }
        let done = try XCTUnwrap(response.runs.first { $0.state == .done })
        XCTAssertNotNil(done.summary)
        // A metered finished run shows its spend; without it, its age.
        let cost = try XCTUnwrap(done.costUsd, "the finished run is metered in the fixture")
        XCTAssertEqual(done.rowTime, VibeDevFormat.usd(cost))
        // Counts are keyed by the same state names.
        for (key, value) in response.counts {
            XCTAssertNotNil(VibeDevRunState(rawValue: key), "unknown count key \(key)")
            XCTAssertEqual(value, response.runs.filter { $0.state.rawValue == key }.count)
        }
        // A run waiting on a diff approval names the approval to answer.
        let waiting = try XCTUnwrap(response.runs.first { $0.state == .needsYou })
        XCTAssertEqual(waiting.approvalId?.hasPrefix("ccp-"), true)
        XCTAssertTrue(response.runs.filter { $0.state != .needsYou }.allSatisfy { $0.approvalId == nil })
        // A needs-you run becomes a Needs-you card with Review and Approve.
        let items = VibeDevQuestions.needsYou(projects: [], liveRuns: response.runs)
        XCTAssertEqual(items.map(\.kind), [.run])
        XCTAssertEqual(items.first?.options.map(\.action), [.reviewRun, .approveRun])
    }

    func testProjectsFixture() throws {
        let response = try decode(VibeDevProjectsResponse.self, "projects.json")
        let project = try XCTUnwrap(response.projects.first)
        XCTAssertFalse(project.projectId.isEmpty)
        XCTAssertFalse(project.name.isEmpty)
        XCTAssertFalse(project.repoDisplayPath.isEmpty)
        XCTAssertFalse(project.repoAbsolutePath.isEmpty)
        XCTAssertFalse(project.chatSessionStatus.isEmpty)
        XCTAssertFalse(project.runTaskIds.isEmpty)
        XCTAssertTrue(project.isTrusted)
        XCTAssertEqual(project.baseBranch, "main")
        XCTAssertEqual(project.repoLabel, project.repoDisplayPath)
        XCTAssertEqual(response.activeProjectId, project.projectId)
    }

    func testRunsPageFixtureFoldsTurnsIntoRuns() throws {
        let page = try decode(VibeDevRunsPage.self, "runs_page.json")
        XCTAssertFalse(page.tasks.isEmpty)
        let rows = page.rows
        XCTAssertEqual(rows.count, page.pagination.runs, "one row per root run on the page")
        XCTAssertGreaterThanOrEqual(page.pagination.totalRuns, rows.count)
        // Every threaded task is folded into a row, never a row of its own.
        for task in page.tasks where task.isThreaded {
            XCTAssertFalse(rows.contains { $0.id == task.id }, "\(task.id) is a turn, not a run")
            XCTAssertTrue(rows.contains { $0.turns.contains(task) })
        }
        let folded = try XCTUnwrap(rows.first { $0.turnCount > 1 })
        XCTAssertEqual(folded.latest.id, folded.turns.last?.id)
        // A lane run carries its typed link.
        if let laneRow = rows.first(where: { $0.link?.lane != nil }) {
            XCTAssertNotNil(laneRow.link?.crewId)
        }
    }

    func testCrewsFixture() throws {
        let response = try decode(VibeDevCrewsResponse.self, "crews.json")
        XCTAssertFalse(response.crews.isEmpty)
        for crew in response.crews {
            XCTAssertFalse(crew.crewId.isEmpty)
            XCTAssertFalse(crew.lanes.isEmpty)
            XCTAssertGreaterThan(crew.createdAtMs, 0)
        }
        let split = try XCTUnwrap(response.crews.first { $0.mode == .split })
        XCTAssertNotNil(split.budgetUsd)
        XCTAssertTrue(split.lanes.contains { !$0.owns.isEmpty }, "split lanes own paths")

        let race = try XCTUnwrap(response.crews.first { $0.status == .choosing })
        XCTAssertEqual(race.mode, .race)
        let question = try XCTUnwrap(VibeDevQuestions.question(for: race, projectID: "p", projectName: "Shop"))
        XCTAssertEqual(question.kind, .crewChoose)
        let keeps = question.options.compactMap { option -> Int? in
            if case .keepLane(let index, _) = option.action { return index }
            return nil
        }
        let done = race.lanes.enumerated().filter { $0.element.status == .done && $0.element.role == .work }.map(\.offset)
        XCTAssertEqual(keeps, done, "keep options name the finished attempts by lane index")
        XCTAssertEqual(question.options.last?.action, .keepNone)
    }

    func testRunTurnsFixture() throws {
        let detail = try decode(VibeDevRunTurns.self, "run_turns.json")
        XCTAssertFalse(detail.runId.isEmpty)
        XCTAssertFalse(detail.projectId.isEmpty)
        XCTAssertGreaterThanOrEqual(detail.turns.count, 2)
        XCTAssertEqual(detail.turns.first?.taskId, detail.runId, "the root is the first turn")
        XCTAssertEqual(detail.latestTurnId, detail.turns.last?.taskId)
        if detail.trusted { XCTAssertNotNil(detail.branch) }
        for turn in detail.turns {
            XCTAssertFalse(turn.prompt.isEmpty)
            XCTAssertNotNil(VibeDevTime.parse(turn.createdAt))
        }
        XCTAssertNotNil(detail.totalCost)
        XCTAssertFalse(detail.engine.isEmpty)
    }

    func testTurnTimelineFixture() throws {
        let timeline = try decode(VibeDevTimeline.self, "turn_timeline.json")
        XCTAssertFalse(timeline.taskId.isEmpty)
        XCTAssertFalse(timeline.cards.isEmpty)
        XCTAssertEqual(timeline.cards.map(\.index), timeline.cards.map(\.index).sorted(), "cards come in order")
        let data = try Data(contentsOf: Self.fixturesDir.appendingPathComponent("turn_timeline.json"))
        let raw = try XCTUnwrap(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        let rawKinds = (raw["cards"] as? [[String: Any]] ?? []).compactMap { $0["kind"] as? String }
        for kind in rawKinds {
            XCTAssertNotNil(VibeDevTimelineKind(rawValue: kind), "unknown timeline kind \(kind)")
        }
        let actions = timeline.cards.filter { $0.kind == .action }
        XCTAssertFalse(actions.isEmpty)
        for action in actions {
            XCTAssertTrue(["running", "ok", "error"].contains(action.status ?? ""), "action status \(action.status ?? "nil")")
        }
        XCTAssertTrue(actions.contains { $0.status == "error" })
    }

    func testRunChangesFixtureAndItsPatchParses() throws {
        let changes = try decode(VibeDevRunChanges.self, "run_changes.json")
        XCTAssertFalse(changes.files.isEmpty)
        XCTAssertTrue(["branch", "proposals"].contains(changes.source))
        let patch = try XCTUnwrap(changes.patch)
        XCTAssertTrue(changes.files.contains { $0.path == patch.path })
        let diff = UnifiedDiffParser.parse(patch.unifiedDiff)
        XCTAssertEqual(diff.newPath, patch.path)
        XCTAssertFalse(diff.hunks.isEmpty)
        XCTAssertGreaterThan(diff.additions, 0)
        XCTAssertGreaterThan(diff.deletions, 0)
        let first = try XCTUnwrap(diff.hunks.first?.lines.first)
        XCTAssertEqual(first.kind, .context)
        XCTAssertEqual(first.oldNumber, diff.hunks.first?.oldStart)
        XCTAssertEqual(first.newNumber, diff.hunks.first?.newStart)
    }

    func testRepoSuggestionsFixture() throws {
        let response = try decode(VibeDevRepoSuggestions.self, "repos_suggest.json")
        XCTAssertFalse(response.repos.isEmpty)
        for repo in response.repos {
            XCTAssertFalse(repo.path.isEmpty)
            XCTAssertFalse(repo.name.isEmpty)
        }
        XCTAssertTrue(response.repos.contains { $0.project != nil }, "a repo a project uses names it")
    }

    func testUnknownEnumValuesDoNotFailTheList() throws {
        let json = """
        {"runs":[{"run_id":"r","live_task_id":"r","project_id":"p","project_name":"P","title":"T",
        "state":"teleporting","status":"running","turns":1,"updated_at":"2026-10-13T09:41:00Z"}],"counts":{}}
        """
        let response = try JSONDecoder().decode(VibeDevLiveRunsResponse.self, from: Data(json.utf8))
        XCTAssertEqual(response.runs.first?.state, .working)
    }

    // MARK: Theming parity

    /// VibeDev screens read only semantic theme tokens, so every web theme
    /// renders the same on iPhone (plan §6).
    func testVibeDevSourcesUseOnlyThemeTokens() throws {
        let dir = Self.repoRoot.appendingPathComponent("magios/Magios/VibeDev", isDirectory: true)
        let files = try FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "swift" }
        XCTAssertFalse(files.isEmpty)
        let forbidden = [
            #"\bColor\("#,
            #"\bColor\.[a-z]"#,
            #"\bUIColor\b"#,
            #"[(\s,]\.(white|black|gray|grey|red|green|blue|orange|yellow|pink|purple|cyan|mint|teal|indigo|brown)\b"#,
            #"Color\(hex:"#,
        ].map { try! NSRegularExpression(pattern: $0) }
        for file in files {
            let text = try String(contentsOf: file, encoding: .utf8)
            for (number, line) in text.components(separatedBy: "\n").enumerated() {
                let trimmed = line.trimmingCharacters(in: .whitespaces)
                if trimmed.hasPrefix("//") { continue }
                let range = NSRange(line.startIndex..., in: line)
                for pattern in forbidden where pattern.firstMatch(in: line, range: range) != nil {
                    XCTFail("\(file.lastPathComponent):\(number + 1) uses a raw color: \(trimmed)")
                }
            }
        }
    }
}
