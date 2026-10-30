import XCTest
@testable import Magician

/// The Work summaries against the web: goal-tree folding (`GoalsWorkspace`),
/// counts (`GoalRow`, the Quality masthead) and the All work landing's compact lists.
final class WorkSummariesTests: XCTestCase {
    private func goal(_ name: String, parent: String? = nil, status: String = "active") -> WorkGoal {
        let parentLine = parent.map { "parent: \"[[\($0)]]\"\n" } ?? ""
        return workParseGoalNote("---\nmagician_kind: goal\n\(parentLine)status: \(status)\n---\n# \(name)", path: "Workspaces/acme/Goals/\(name).md")!
    }
    private func tree(_ goals: [WorkGoal]) -> WorkGoalTree { workBuildGoalTree(goals) }
    private func path(_ name: String) -> String { "Workspaces/acme/Goals/\(name).md" }

    // MARK: Collapse

    func testTogglesOnlyNodesWithSubGoals() throws {
        let built = tree([goal("Seed"), goal("Accelerators", parent: "Seed"), goal("Hire")])
        let seed = try XCTUnwrap(built.roots.first { $0.goal.name == "Seed" })
        let hire = try XCTUnwrap(built.roots.first { $0.goal.name == "Hire" })
        var collapse = WorkGoalCollapse()
        XCTAssertFalse(collapse.isCollapsed(seed))
        collapse.toggle(seed.goal.path)
        XCTAssertTrue(collapse.isCollapsed(seed))
        collapse.toggle(hire.goal.path)
        XCTAssertFalse(collapse.isCollapsed(hire), "a leaf never reads as folded")
        collapse.toggle(seed.goal.path)
        XCTAssertFalse(collapse.isCollapsed(seed))
    }

    func testCollapseAllFoldsEveryParentInMainAndNotLinkedTreesAndExpandAllClears() {
        let built = tree([
            goal("Seed"), goal("Accelerators", parent: "Seed"), goal("YC", parent: "Accelerators"),
            goal("Stray", parent: "Missing"), goal("Stray child", parent: "Stray"), goal("Leaf")
        ])
        XCTAssertEqual(WorkGoalCollapse.collapsible([built]), [path("Seed"), path("Accelerators"), path("Stray")])
        var collapse = WorkGoalCollapse()
        collapse.collapseAll([built])
        XCTAssertEqual(collapse.collapsed, [path("Seed"), path("Accelerators"), path("Stray")])
        collapse.expandAll()
        XCTAssertTrue(collapse.collapsed.isEmpty)
    }

    func testCollapseAllCoversEveryWorkspace() {
        let one = tree([goal("Seed"), goal("Deck", parent: "Seed")])
        let two = WorkGoalTree(roots: [WorkGoalNode(goal: goal("Ship"), children: [WorkGoalNode(goal: goal("Beta"), children: [])])], orphans: [], byKey: [:])
        XCTAssertEqual(WorkGoalCollapse.collapsible([one, two]), [path("Seed"), path("Ship")])
    }

    // MARK: Counts and copy

    func testGoalAndSubGoalCountsUseDirectChildrenLikeTheWeb() throws {
        let built = tree([goal("Seed"), goal("Deck", parent: "Seed"), goal("Slides", parent: "Deck"), goal("Accelerators", parent: "Seed")])
        let seed = try XCTUnwrap(built.roots.first)
        XCTAssertEqual(WorkGoalCopy.subGoals(of: seed), "2 sub-goals")
        XCTAssertEqual(WorkGoalCopy.subGoals(1), "1 sub-goal")
        XCTAssertEqual(WorkGoalCopy.goalCount(1), "1 goal")
        XCTAssertEqual(WorkGoalCopy.goalCount(4), "4 goals")
        XCTAssertEqual(WorkGoalCopy.expandLabel("Seed"), "Expand sub-goals of Seed")
        XCTAssertEqual(WorkGoalCopy.collapseLabel("Seed"), "Collapse sub-goals of Seed")
        XCTAssertEqual(WorkGoalCopy.ownerLabel("user"), "You")
        XCTAssertEqual(WorkGoalCopy.ownerLabel("cro"), "cro")
    }

    func testQualityCountsAddBriefsReviewsAndAttentionAndReadActiveRuns() throws {
        let catalog = WorkJSON.object(["items": .array([]), "counts": .object([
            "brief": .integer(2), "review": .unsigned(3), "attention": .number(1), "active": .integer(4), "complete": .integer(9)
        ])])
        let counts = try XCTUnwrap(WorkQualityCounts(catalog: catalog))
        XCTAssertEqual(counts.needsReview, 6)
        XCTAssertEqual(counts.active, 4)
        XCTAssertEqual(WorkQualityCounts.inboxLabel(counts), "Needs review · 6")
        XCTAssertEqual(WorkQualityCounts.runsLabel(counts), "Runs · 4 active")
    }

    func testQualityCountsTreatMissingKeysAsZeroAndAMissingObjectAsUnknown() throws {
        let empty = try XCTUnwrap(WorkQualityCounts(catalog: .object(["counts": .object([:])])))
        XCTAssertEqual(empty, WorkQualityCounts(catalog: .object(["counts": .object(["active": .null])])))
        XCTAssertEqual(WorkQualityCounts.inboxLabel(empty), "Needs review · 0")
        XCTAssertEqual(WorkQualityCounts.runsLabel(empty), "Runs · 0 active")
        XCTAssertNil(WorkQualityCounts(catalog: .object(["items": .array([])])))
        XCTAssertNil(WorkQualityCounts(catalog: nil))
        XCTAssertEqual(WorkQualityCounts.inboxLabel(nil), "Needs review")
        XCTAssertEqual(WorkQualityCounts.runsLabel(nil), "Runs")
    }

    func testPlaybookCopyMatchesTheWeb() {
        XCTAssertEqual(WorkPlaybookCopy.empty, "No playbook documents in this workspace.")
        XCTAssertEqual(WorkPlaybookCopy.title(3), "Playbooks · 3")
        XCTAssertEqual(WorkPlaybookCopy.title(nil), "Playbooks")
    }

    // MARK: Landing

    func testLandingShowsMainGoalsThenNotLinkedHeadsUpToThree() {
        let first = [goal("Seed"), goal("Deck", parent: "Seed"), goal("Stray", parent: "Missing")]
        let second = [goal("Ship"), goal("Hire")]
        let shown = WorkLanding.goals([(first, tree(first)), (second, tree(second))])
        XCTAssertEqual(shown.map(\.goal.name), ["Seed", "Ship", "Hire"], "main goals of every workspace lead; Not linked heads follow")
        XCTAssertEqual(shown.first?.children.map(\.goal.name), ["Deck"])
        XCTAssertEqual(WorkLanding.goals([(first, tree(first))], limit: 5).map(\.goal.name), ["Seed", "Stray"])
    }

    func testLandingListsEveryGoalFlatWhenNoWorkspaceHasAMainGoal() {
        let loop = [goal("A", parent: "B"), goal("B", parent: "A"), goal("C", parent: "Missing")]
        let shown = WorkLanding.goals([(loop, tree(loop))])
        XCTAssertEqual(shown.map(\.goal.name), ["A", "B", "C"])
        XCTAssertTrue(shown.allSatisfy { $0.children.isEmpty })
        XCTAssertTrue(WorkLanding.goals([]).isEmpty)
    }

    func testLandingListsAndCountsOnlyOpenGoals() {
        let space = WorkGoalWorkspace(name: "acme", path: "Workspaces/acme", goals: [
            goal("Seed", status: "done"), goal("Deck", parent: "Seed"), goal("Hire"), goal("Old", parent: "Hire", status: "dropped"),
        ], problems: [], notShown: 0, foldersNotRead: 0)
        let open = WorkLanding.openGoals([space])
        XCTAssertEqual(open.flatMap { $0.goals.map(\.name) }, ["Deck", "Hire"])
        let shown = WorkLanding.goals(open)
        XCTAssertEqual(shown.map(\.goal.name), ["Deck", "Hire"], "an open goal under a closed main goal heads its own tree")
        XCTAssertEqual(shown.first?.closedParent, "Seed")
        XCTAssertTrue(shown[1].children.isEmpty, "a closed sub-goal is not counted")
        let allClosed = WorkGoalWorkspace(name: "acme", path: "Workspaces/acme", goals: [goal("Seed", status: "done")], problems: [], notShown: 0, foldersNotRead: 0)
        XCTAssertTrue(WorkLanding.goals(WorkLanding.openGoals([allClosed])).isEmpty)
    }

    func testLandingShowsAtMostFourPlaybooks() {
        let programs = (1...6).map { WorkProgramSummary(name: "p\($0).md", title: "Playbook \($0)") }
        XCTAssertEqual(WorkLanding.playbooks(programs).map(\.name), ["p1.md", "p2.md", "p3.md", "p4.md"])
        XCTAssertEqual(WorkLanding.playbooks(Array(programs.prefix(2))).count, 2)
        XCTAssertTrue(WorkLanding.playbooks(programs, limit: -1).isEmpty)
    }

    func testLandingReadsFarLessThanTheGoalsScreen() {
        let landing = WorkLanding.goalReadLimits, full = WorkGoalReadLimits.standard
        XCTAssertEqual(landing.folderDepth, 1, "main-goal folders only")
        XCTAssertLessThanOrEqual(landing.folders, 10)
        XCTAssertLessThanOrEqual(landing.notes, 40)
        XCTAssertLessThan(landing.folders, full.folders)
        XCTAssertLessThan(landing.notes, full.notes)
        XCTAssertEqual(WorkLanding.goalLimit, 3)
        XCTAssertEqual(WorkLanding.playbookLimit, 4)
    }

    func testLandingCountMarksAPartialRead() {
        XCTAssertEqual(WorkLanding.countLabel(12, partial: false), "12")
        XCTAssertEqual(WorkLanding.countLabel(32, partial: true), "32+")
    }

    func testLandingLoadsOncePerAppearanceAndKeepsAQuickReturn() {
        let start = Date(timeIntervalSince1970: 1_000)
        XCTAssertTrue(WorkLanding.needsLoad(lastLoad: nil, now: start))
        XCTAssertFalse(WorkLanding.needsLoad(lastLoad: start, now: start.addingTimeInterval(5)))
        XCTAssertFalse(WorkLanding.needsLoad(lastLoad: start, now: start.addingTimeInterval(WorkLanding.reloadAfter - 1)))
        XCTAssertTrue(WorkLanding.needsLoad(lastLoad: start, now: start.addingTimeInterval(WorkLanding.reloadAfter)))
    }

    /// The landing reads with its own limits, so a large goals folder costs a few requests.
    func testLandingReadStopsAtMainGoalFoldersAndItsNoteLimit() async throws {
        let mains = (1...12).map { "Main \($0)" }
        let recorder = RequestRecorder()
        let fetch: WorkGoalFetch = { url in
            await recorder.add(url)
            let path = URLComponents(string: url)?.queryItems?.first { $0.name == "path" }?.value ?? ""
            if url.contains("/tree?") {
                if path == "Workspaces" {
                    return (200, .object(["entries": .array([.object(["name": .string("acme"), "relative_path": .string("Workspaces/acme"), "kind": .string("dir")])])]))
                }
                if path == "Workspaces/acme/Goals" {
                    return (200, .object(["entries": .array(mains.map { .object(["name": .string($0), "relative_path": .string("Workspaces/acme/Goals/\($0)"), "kind": .string("dir"), "has_children": .bool(true)]) })]))
                }
                let main = path.components(separatedBy: "/").last ?? ""
                let notes = [main] + (1...4).map { "\(main) sub \($0)" }
                var entries: [WorkJSON] = notes.map { .object(["name": .string("\($0).md"), "relative_path": .string("\(path)/\($0).md"), "kind": .string("file")]) }
                entries.append(.object(["name": .string("Archive"), "relative_path": .string("\(path)/Archive"), "kind": .string("dir"), "has_children": .bool(true)]))
                return (200, .object(["entries": .array(entries)]))
            }
            let name = (path.components(separatedBy: "/").last ?? "").replacingOccurrences(of: ".md", with: "")
            let folder = path.components(separatedBy: "/").dropLast().last ?? ""
            let parent = name == folder ? "" : "parent: \"[[\(folder)]]\"\n"
            return (200, .object(["markdown": .string("---\nmagician_kind: goal\n\(parent)---\n# \(name)")]))
        }
        let spaces = try await WorkGoalReader.load(limits: WorkLanding.goalReadLimits, fetch: fetch)
        let requests = await recorder.urls
        let limits = WorkLanding.goalReadLimits
        XCTAssertEqual(spaces.count, 1)
        XCTAssertEqual(spaces[0].goals.count, limits.notes)
        XCTAssertGreaterThan(spaces[0].notShown + spaces[0].foldersNotRead, 0)
        XCTAssertLessThanOrEqual(requests.count, 2 + limits.folders + limits.notes)
        XCTAssertFalse(requests.contains { $0.contains("Archive") }, "folders under a main goal are not walked")
        let shown = WorkLanding.goals(spaces.map { (goals: $0.goals, tree: workBuildGoalTree($0.goals)) })
        XCTAssertEqual(shown.map(\.goal.name), ["Main 1", "Main 2", "Main 3"])
        XCTAssertEqual(shown.first.map(WorkGoalCopy.subGoals(of:)), "4 sub-goals")
    }
}

private actor RequestRecorder {
    private(set) var urls: [String] = []
    func add(_ url: String) { urls.append(url) }
}
