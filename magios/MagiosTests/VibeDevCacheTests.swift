import XCTest
@testable import Magician

@MainActor
final class VibeDevCacheTests: XCTestCase {
    private var root: URL!

    override func setUp() async throws {
        root = FileManager.default.temporaryDirectory
            .appendingPathComponent("vibedev-cache-\(UUID().uuidString)", isDirectory: true)
    }

    override func tearDown() async throws {
        try? FileManager.default.removeItem(at: root)
    }

    private func liveRun(_ id: String, _ state: VibeDevRunState) -> VibeDevLiveRun {
        VibeDevLiveRun(runId: id, liveTaskId: id, projectId: "p1", projectName: "Shop", title: "VibeDev · \(id)",
                       latestTurnTitle: nil, state: state, status: "running", step: "Editing", summary: nil,
                       turns: 1, startedAt: "2026-10-13T09:12:00Z", costUsd: 0.5, approvalId: "ccp-x",
                       crewId: nil, lane: nil, updatedAt: "2026-10-13T09:41:00Z", lastProgressAt: nil)
    }

    private var snapshot: VibeDevCache.NowSnapshot {
        let crew = VibeDevCrew(crewId: "c", goal: "g", status: .proposed,
                               lanes: [VibeDevCrewLane(lane: "API", owns: ["src"], status: .pending)],
                               budgetUsd: 6, createdAtMs: 1, updatedAtMs: 2)
        return VibeDevCache.NowSnapshot(
            live: VibeDevLiveRunsResponse(runs: [liveRun("a", .working), liveRun("b", .needsYou)],
                                          counts: ["working": 1, "needs_you": 1]),
            projects: [VibeDevProject(projectId: "p1", name: "Shop", trust: .trusted, baseBranch: "main",
                                      runTaskIds: ["a"], crews: [crew], repoDisplayPath: "~/code/shop")])
    }

    func testNowSnapshotRoundTrips() throws {
        let cache = VibeDevCache(directory: root)
        let saved = Date(timeIntervalSince1970: 1_760_346_000)
        var historySnapshot = snapshot
        var historicalRun = liveRun("old", .done)
        historicalRun.workLocation = "original"
        historySnapshot.history = VibeDevRunHistory(runs: [historicalRun], hasMore: true, nextCursor: "next")
        cache.saveNow(historySnapshot, at: saved)
        let loaded = try XCTUnwrap(cache.loadNow())
        XCTAssertEqual(loaded.0, historySnapshot)
        XCTAssertEqual(loaded.0.history?.nextCursor, "next")
        XCTAssertEqual(loaded.1, saved)
    }

    func testTurnsRoundTripPerRun() throws {
        let cache = VibeDevCache(directory: root)
        let turns = VibeDevRunTurns(
            runId: "root", projectId: "p1", projectName: "Shop", title: "t", status: "completed", trusted: true,
            branch: "vibedev/x", crewId: nil, lane: nil,
            turns: [VibeDevRunTurn(taskId: "root", prompt: "p", mode: .discuss, status: "completed",
                                   createdAt: "a", updatedAt: "b", summary: "s", engine: "pi", costUsd: 0.1,
                                   commit: "abc", filesChanged: 2)])
        cache.saveTurns(turns, for: "root/../x")
        XCTAssertEqual(cache.loadTurns(for: "root/../x")?.0, turns)
        XCTAssertNil(cache.loadTurns(for: "other"))
    }

    func testMissingOrCorruptFilesAreNoCache() throws {
        let cache = VibeDevCache(directory: root)
        XCTAssertNil(cache.loadNow())
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try Data("{not json".utf8).write(to: root.appendingPathComponent("now.json"))
        XCTAssertNil(cache.loadNow())
    }

    func testFolderNamesAreSafe() {
        XCTAssertEqual(VibeDevCache.folderName("https://mac.example:8443"), "https_3a_2f_2fmac.example_3a8443")
        XCTAssertEqual(VibeDevCache.folderName("owner-1"), "owner-1")
        // Never a name the file system reads as "here", "up" or nothing.
        for raw in ["", ".", ".."] {
            let name = VibeDevCache.folderName(raw)
            XCTAssertFalse(["", ".", ".."].contains(name), "\(raw.debugDescription) became \(name)")
            XCTAssertFalse(name.contains("/"))
        }
        XCTAssertFalse(VibeDevCache.folderName("../x").contains("/"))
    }

    func testFolderNamesDoNotCollide() {
        let inputs = ["", ".", "..", "_", "_2e", "a/b", "a_b", "a:b", "a.b", "A", "a", "é", "e\u{301}",
                      String(repeating: "x", count: 130), String(repeating: "x", count: 131),
                      String(repeating: "/", count: 200), String(repeating: "/", count: 201)]
        let names = inputs.map(VibeDevCache.folderName)
        XCTAssertEqual(Set(names).count, inputs.count, "\(names)")
        XCTAssertEqual(Set(names.map { $0.lowercased() }).count, inputs.count, "distinct without case too")
        XCTAssertTrue(names.allSatisfy { $0.count <= 120 })
        XCTAssertEqual(VibeDevCache.folderName("a/b"), VibeDevCache.folderName("a/b"), "stable across calls")
    }

    func testOddAccountNamesStayInsideTheMachineFolder() throws {
        let odd = VibeDevCache(root: root, origin: "https://mac.example", principal: "..", workspace: "")
        let plain = VibeDevCache(root: root, origin: "https://mac.example", principal: "owner", workspace: "default")
        XCTAssertTrue(odd.directory.standardizedFileURL.path.hasPrefix(root.standardizedFileURL.path + "/"))
        odd.saveNow(snapshot)
        XCTAssertNil(plain.loadNow())
        VibeDevCache.forget(origin: "https://mac.example", root: root)
        XCTAssertNil(odd.loadNow(), "forgetting the machine drops odd accounts too")
    }

    func testCacheIsPerAccountAndForgettingAMachineDropsIt() throws {
        let mine = VibeDevCache(root: root, origin: "https://mac.example:8443", principal: "owner", workspace: "default")
        let guest = VibeDevCache(root: root, origin: "https://mac.example:8443", principal: "guest", workspace: "default")
        let otherWorkspace = VibeDevCache(root: root, origin: "https://mac.example:8443",
                                          principal: "owner", workspace: "w2")
        let otherMachine = VibeDevCache(root: root, origin: "https://studio.example", principal: "owner",
                                        workspace: "default")
        mine.saveNow(snapshot)
        XCTAssertNotNil(mine.loadNow())
        XCTAssertNil(guest.loadNow(), "a re-pair as someone else starts empty")
        XCTAssertNil(otherWorkspace.loadNow())
        XCTAssertNil(otherMachine.loadNow())

        otherMachine.saveNow(snapshot)
        VibeDevCache.forget(origin: "https://Mac.Example:8443/", root: root)
        XCTAssertNil(mine.loadNow(), "forgetting the machine drops its cache")
        XCTAssertNotNil(otherMachine.loadNow(), "other machines keep theirs")
    }

    func testColdStartOfflineShowsTheLastDataAsStale() async throws {
        let cache = VibeDevCache(directory: root)
        let saved = Date().addingTimeInterval(-300)
        cache.saveNow(snapshot, at: saved)
        let api = StubVibeDevAPI()
        api.failReads = true
        let context = VibeDevContext(api: api, approvals: StubApprovals(),
                                     undo: VibeDevUndoCenter(delay: 0.1, haptics: false), cache: cache)
        let model = VibeDevNowModel(context: context)
        await model.refresh()
        XCTAssertEqual(model.working.map(\.runId), ["a"])
        XCTAssertEqual(model.history.map(\.runId), ["a", "b"])
        XCTAssertEqual(model.visibleProjects.map(\.projectId), ["p1"])
        XCTAssertTrue(model.refreshFailed, "the banner says how old the data is")
        XCTAssertEqual(model.lastUpdated?.timeIntervalSince1970 ?? 0, saved.timeIntervalSince1970, accuracy: 0.01)
        XCTAssertNil(model.errorMessage)
    }

    func testASuccessfulReadIsCached() async throws {
        let cache = VibeDevCache(directory: root)
        let api = StubVibeDevAPI()
        api.live = snapshot.live
        api.projectList = VibeDevProjectsResponse(projects: snapshot.projects, activeProjectId: nil)
        let context = VibeDevContext(api: api, approvals: StubApprovals(),
                                     undo: VibeDevUndoCenter(delay: 0.1, haptics: false), cache: cache)
        await VibeDevNowModel(context: context).refresh()
        var expected = snapshot
        expected.history = VibeDevRunHistory(runs: snapshot.live.runs, hasMore: false, nextCursor: nil)
        XCTAssertEqual(cache.loadNow()?.0, expected)
    }

    func testRunScreenFallsBackToCachedTurns() async throws {
        let cache = VibeDevCache(directory: root)
        let turns = VibeDevRunTurns(runId: "root", projectId: "p1", projectName: "Shop", title: "VibeDev · t",
                                    status: "completed", trusted: false, branch: nil, crewId: nil, lane: nil,
                                    turns: [])
        cache.saveTurns(turns, for: "root", at: Date().addingTimeInterval(-60))
        let api = StubVibeDevAPI()
        let context = VibeDevContext(api: api, approvals: StubApprovals(),
                                     undo: VibeDevUndoCenter(delay: 0.1, haptics: false), cache: cache)
        let model = VibeDevRunModel(runID: "root", context: context)
        await model.refresh()  // the stub has no turns: a 404
        XCTAssertEqual(model.turns, turns)
        XCTAssertTrue(model.refreshFailed)
        XCTAssertNil(model.errorMessage)
    }
}
