import XCTest
@testable import Magician

/// The web's `goals.test.ts` and the Goals screen copy, against the native port.
final class WorkGoalsTests: XCTestCase {
    private let main = """
    ---
    magician_kind: goal
    workspace: acme
    kind: main
    owner: user
    status: active
    target: 2026-12-01
    visibility: private
    ---
    # Raise a seed round
    Why it matters.
    """

    private let sub = """
    ---
    magician_kind: goal
    kind: sub
    parent: "[[Raise a seed round]]"
    peers: ["[[Find design partners]]"]
    owner: cro
    status: blocked
    visibility: internal
    program: fundraising.md
    goal_id: harness:cro:fundraising
    metrics:
      - investor_meetings
    ---
    # Apply to accelerators
    """

    private func goalNamed(_ name: String, _ parent: String?) -> WorkGoal {
        workParseGoalNote("---\nmagician_kind: goal\n\(parent.map { "parent: \"[[\($0)]]\"\n" } ?? "")---\n# \(name)", path: "G/\(name).md")!
    }
    private func names(_ nodes: [WorkGoalNode]) -> [String] { nodes.flatMap { [$0.goal.name] + names($0.children) } }
    private func goalWith(_ name: String, _ parent: String? = nil, status: String = "active", closed: String? = nil) -> WorkGoal {
        let lines = [parent.map { "parent: \"[[\($0)]]\"" }, "status: \(status)", closed.map { "closed: \($0)" }].compactMap { $0 }
        return workParseGoalNote("---\nmagician_kind: goal\n\(lines.joined(separator: "\n"))\n---\n# \(name)", path: "G/\(name).md")!
    }

    // MARK: Goal notes

    func testReadsFrontmatterFieldsAndTheTitle() throws {
        let goal = try XCTUnwrap(workParseGoalNote(main, path: "Workspaces/acme/Goals/Raise a seed round.md"))
        XCTAssertEqual(goal.name, "Raise a seed round"); XCTAssertEqual(goal.title, "Raise a seed round")
        XCTAssertEqual(goal.kind, .main); XCTAssertEqual(goal.owner, "user"); XCTAssertEqual(goal.status, "active")
        XCTAssertEqual(goal.target, "2026-12-01"); XCTAssertEqual(goal.visibility, .private)
        XCTAssertNil(goal.parent); XCTAssertEqual(goal.peers, []); XCTAssertEqual(goal.workspace, "acme")
    }

    func testReadsLinksInlineListsAndBlockLists() throws {
        let goal = try XCTUnwrap(workParseGoalNote(sub, path: "Workspaces/acme/Goals/Apply to accelerators.md"))
        XCTAssertEqual(goal.parent, "Raise a seed round"); XCTAssertEqual(goal.peers, ["Find design partners"])
        XCTAssertEqual(goal.program, "fundraising.md"); XCTAssertEqual(goal.goalID, "harness:cro:fundraising")
        XCTAssertEqual(goal.metrics, ["investor_meetings"]); XCTAssertEqual(goal.kind, .sub); XCTAssertEqual(goal.visibility, .internal)
    }

    func testIgnoresNotesThatAreNotGoals() {
        XCTAssertNil(workParseGoalNote("---\nmagician_kind: note\n---\n# x", path: "a.md"))
        XCTAssertNil(workParseGoalNote("# no frontmatter", path: "b.md"))
    }

    func testStripsLinkBracketsAndAliases() {
        XCTAssertEqual(workGoalLinkTarget("[[Raise a seed round|seed]]"), "Raise a seed round")
        XCTAssertEqual(workGoalLinkTarget("plain"), "plain")
    }

    func testDropsCommentsKeepsQuotedHashesAndReadsBareLinksAndUnindentedLists() throws {
        let goal = try XCTUnwrap(workParseGoalNote("""
        ---
        # written by the goal template
        magician_kind: goal # a goal
        parent: [[Raise a seed round]]
        peers: [[[Find design partners]], "[[Hire a designer|design]]"]
        status: "Blocked #2"
        owner: Founder's assistant
        metrics:
        - investor_meetings # weekly
        - "pipeline, qualified"
        ---
        Body without a heading.
        """, path: "Workspaces/acme/Goals/Close the round.md"))
        XCTAssertEqual(goal.title, "Close the round"); XCTAssertEqual(goal.kind, .sub); XCTAssertEqual(goal.parent, "Raise a seed round")
        XCTAssertEqual(goal.peers, ["Find design partners", "Hire a designer"]); XCTAssertEqual(goal.status, "blocked #2")
        XCTAssertEqual(goal.owner, "Founder's assistant"); XCTAssertEqual(goal.metrics, ["investor_meetings", "pipeline, qualified"])
        XCTAssertEqual(goal.visibility, .private)
    }

    func testKeepsAHashInsideALaterQuotedFlowListItem() {
        XCTAssertEqual(workParseGoalNote("---\nmagician_kind: goal\npeers: [\"a\", \"b #2\"] # peers\n---\n# x", path: "x.md")?.peers, ["a", "b #2"])
    }

    func testTitleOnlyFromAnH1OnTheFirstNonBlankLineAndAFenceWithTrailingSpaces() {
        XCTAssertEqual(workParseGoalNote("---  \nmagician_kind: goal\n---\n\n# Ship the beta\n", path: "G/beta.md")?.title, "Ship the beta")
        XCTAssertEqual(workParseGoalNote("---\nmagician_kind: goal\n---\nIntro first.\n# Later heading", path: "G/Ship the beta.md")?.title, "Ship the beta")
    }

    func testReadsCRLFNotesAndAByteOrderMark() {
        let goal = workParseGoalNote("\u{FEFF}---\r\nmagician_kind: goal\r\nparent: \"[[Plan]]\"\r\n---\r\n# Windows goal\r\n", path: "G/w.md")
        XCTAssertEqual(goal?.title, "Windows goal"); XCTAssertEqual(goal?.parent, "Plan"); XCTAssertEqual(goal?.kind, .sub)
    }

    // MARK: Tree

    func testResolvesASharedLinkKeyToTheFirstGoalListed() throws {
        let first = try XCTUnwrap(workParseGoalNote("---\nmagician_kind: goal\n---\n# First", path: "G/Plan.md"))
        let second = try XCTUnwrap(workParseGoalNote("---\nmagician_kind: goal\n---\n# Second", path: "H/plan.md"))
        XCTAssertEqual(workBuildGoalTree([first, second]).byKey["plan"], first)
    }

    func testNestsSubGoalsUnderTheirParentAndKeepsOrphansVisible() throws {
        let lost = sub.replacingOccurrences(of: "Raise a seed round", with: "Missing parent").replacingOccurrences(of: "Apply to accelerators", with: "Lost")
        let tree = workBuildGoalTree([
            try XCTUnwrap(workParseGoalNote(main, path: "G/Raise a seed round.md")),
            try XCTUnwrap(workParseGoalNote(sub, path: "G/Apply to accelerators.md")),
            try XCTUnwrap(workParseGoalNote(lost, path: "G/Lost.md")),
        ])
        XCTAssertEqual(tree.roots.map(\.goal.name), ["Raise a seed round"])
        XCTAssertEqual(tree.roots[0].children.map(\.goal.name), ["Apply to accelerators"])
        XCTAssertEqual(tree.orphans.map { [$0.goal.name, $0.reason.rawValue] }, [["Lost", "missing-parent"]])
    }

    func testHeadsANotLinkedTreeAtAMissingParentAndKeepsItsSubGoalsNested() {
        let tree = workBuildGoalTree([goalNamed("C", "B"), goalNamed("B", "Missing"), goalNamed("D", "C")])
        XCTAssertEqual(tree.roots, [])
        XCTAssertEqual(tree.orphans.map { [$0.goal.name, $0.reason.rawValue] }, [["B", "missing-parent"]])
        XCTAssertEqual(tree.orphans[0].node.children.map(\.goal.name), ["C"])
        XCTAssertEqual(tree.orphans[0].node.children[0].children.map(\.goal.name), ["D"])
        XCTAssertEqual(workGoalOrphanNote(tree.orphans[0]), "Parent goal not found: Missing")
    }

    func testHeadsASelfParentedGoalAndOneMemberOfEachParentLoopPlacingEveryGoalOnce() {
        let tree = workBuildGoalTree([goalNamed("Loop", "Loop"), goalNamed("Pong", "Ping"), goalNamed("Ping", "Pong"), goalNamed("Tail", "Pong"), goalNamed("Root", nil), goalNamed("Under loop", "Loop")])
        XCTAssertEqual(tree.roots.map(\.goal.name), ["Root"])
        XCTAssertEqual(tree.orphans.map { [$0.goal.name, $0.reason.rawValue] }, [["Loop", "self-parent"], ["Ping", "parent-loop"]])
        XCTAssertEqual(tree.orphans[0].node.children.map(\.goal.name), ["Under loop"])
        let ping = tree.orphans[1].node
        XCTAssertEqual(ping.children.map(\.goal.name), ["Pong"])
        XCTAssertEqual(ping.children[0].children.map(\.goal.name), ["Tail"])
        XCTAssertEqual(names(tree.roots + tree.orphans.map(\.node)).sorted(), ["Loop", "Ping", "Pong", "Root", "Tail", "Under loop"])
        XCTAssertEqual(tree.orphans.map(workGoalOrphanNote), ["Names itself as its parent.", "Part of a parent loop: its parent goals lead back to it."])
    }

    func testMatchesAParentLinkByNoteNameIgnoringFoldersHeadingsExtensionAndCase() {
        let tree = workBuildGoalTree([goalNamed("Raise a seed round", nil), goalNamed("Pitch deck", "Workspaces/acme/Goals/raise a seed round.md#Why")])
        XCTAssertEqual(tree.roots[0].children.map(\.goal.name), ["Pitch deck"])
        XCTAssertEqual(tree.orphans, [])
    }

    func testLinksAProgramByItsDocumentName() {
        XCTAssertEqual(workGoalProgramName("fundraising.md"), "fundraising.md")
        XCTAssertEqual(workGoalProgramName("hiring plan"), "hiring plan.md")
    }

    // MARK: Closed goals

    func testReadsTheClosedDateAndTreatsDoneAndDroppedAsClosed() throws {
        let done = try XCTUnwrap(workParseGoalNote("---\nmagician_kind: goal\nstatus: Done\nclosed: \"2026-10-20\"\n---\n# Ship", path: "G/Ship.md"))
        XCTAssertEqual(done.status, "done"); XCTAssertEqual(done.closed, "2026-10-20"); XCTAssertTrue(done.isClosed)
        XCTAssertTrue(goalWith("Gone", status: "dropped").isClosed)
        XCTAssertNil(goalWith("Gone", status: "dropped").closed)
        XCTAssertNil(workParseGoalNote("---\nmagician_kind: goal\nstatus: done\nclosed: ~\n---\n# x", path: "G/x.md")?.closed)
        for status in ["active", "proposed", "blocked", "paused"] { XCTAssertFalse(goalWith("Open", status: status).isClosed, status) }
        XCTAssertEqual(WorkGoal.closedStatuses, ["done", "dropped"])
    }

    func testOpenTreeLiftsOpenGoalsFromUnderClosedOnesAndNamesTheClosedParent() throws {
        let tree = workOpenGoalTree(workBuildGoalTree([
            goalWith("Root"), goalWith("Shipped", "Root", status: "done"), goalWith("Child", "Shipped"), goalWith("Grandchild", "Child"),
            goalWith("Finished leaf", "Root", status: "done"),
            goalWith("Closed main", status: "dropped"), goalWith("Was under main", "Closed main"),
            goalWith("Stray", "Missing"), goalWith("Stray done", "Stray", status: "done"), goalWith("Lifted", "Stray done"),
            goalWith("Gone", "Missing too", status: "done"), goalWith("Kept", "Gone"),
        ]))
        XCTAssertEqual(tree.roots.map(\.goal.name), ["Root", "Was under main"])
        XCTAssertNil(tree.roots[0].closedParent)
        XCTAssertEqual(tree.roots[1].closedParent, "Closed main", "an open goal under a closed main goal heads its own tree")
        let child = try XCTUnwrap(tree.roots[0].children.first)
        XCTAssertEqual(tree.roots[0].children.map(\.goal.name), ["Child"], "it moves up to its nearest open ancestor")
        XCTAssertEqual(child.closedParent, "Shipped")
        XCTAssertEqual(child.children.map(\.goal.name), ["Grandchild"])
        XCTAssertNil(child.children[0].closedParent, "only the goal that sat directly under a closed one names it")
        // An open "Not linked" head keeps its reason; a closed one hands it to the goals lifted from under it.
        XCTAssertEqual(tree.orphans.map { [$0.goal.name, $0.reason.rawValue] }, [["Stray", "missing-parent"], ["Kept", "missing-parent"]])
        XCTAssertNil(tree.orphans[0].node.closedParent)
        XCTAssertEqual(tree.orphans[0].node.children.map(\.goal.name), ["Lifted"])
        XCTAssertEqual(tree.orphans[0].node.children[0].closedParent, "Stray done")
        XCTAssertEqual(tree.orphans[1].node.closedParent, "Gone")
        XCTAssertFalse(names(tree.roots + tree.orphans.map(\.node)).contains { ["Shipped", "Finished leaf", "Closed main", "Stray done", "Gone"].contains($0) })
        XCTAssertNotNil(tree.byKey["shipped"], "closed goals still resolve links")
        XCTAssertEqual(WorkGoalCopy.closedParent("Shipped"), "Its parent goal “Shipped” is closed.")
    }

    func testClosedGoalsAreMostRecentlyClosedFirstThenUndatedByTitle() {
        let goals = [
            goalWith("Open goal"), goalWith("B undated", status: "done"), goalWith("Older", status: "done", closed: "2026-09-01"),
            goalWith("A undated", status: "dropped"), goalWith("Newer", status: "dropped", closed: "2026-10-20"),
            goalWith("Same day b", status: "done", closed: "2026-10-01"), goalWith("Same day a", status: "done", closed: "2026-10-01"),
        ]
        XCTAssertEqual(workClosedGoals(goals).map(\.name), ["Newer", "Same day a", "Same day b", "Older", "A undated", "B undated"])
    }

    func testAClosedGoalNamesTheGoalItSatUnderByNoteName() {
        let tree = workBuildGoalTree([goalWith("Raise a seed round"), goalWith("Pitch", "Workspaces/acme/Goals/raise a seed round.md", status: "done"), goalWith("Lost", "Missing", status: "done")])
        XCTAssertEqual(workGoalParentTitle(tree.byKey["pitch"]!, in: tree), "Raise a seed round")
        XCTAssertEqual(workGoalParentTitle(tree.byKey["lost"]!, in: tree), "Missing")
        XCTAssertNil(workGoalParentTitle(tree.byKey["raise a seed round"]!, in: tree))
        XCTAssertEqual(WorkGoalCopy.under("Raise a seed round"), "Under Raise a seed round")
        XCTAssertEqual(WorkGoalCopy.closedOn("2026-10-20"), "Closed 2026-10-20")
    }

    func testAWorkspaceWhoseGoalsAreAllClosedIsListedOnlyUnderClosed() {
        func space(_ goals: [WorkGoal], problems: [String] = [], notShown: Int = 0, foldersNotRead: Int = 0) -> WorkGoalWorkspace {
            WorkGoalWorkspace(name: "w", path: "Workspaces/w", goals: goals, problems: problems, notShown: notShown, foldersNotRead: foldersNotRead)
        }
        let allClosed = space([goalWith("Main", status: "done"), goalWith("Step", "Main", status: "dropped")])
        let mixed = space([goalWith("Main", status: "done"), goalWith("Step", "Main")])
        let open = space([goalWith("Main")])
        let empty = space([])
        XCTAssertEqual([allClosed, mixed, open, empty].map(\.showsInOpenList), [false, true, true, true])
        XCTAssertEqual([allClosed, mixed, open, empty].map(\.showsInClosedList), [true, true, false, false])
        // A partial read may have missed goals, so both lists keep the workspace and its notes.
        for read in [space([], problems: ["G/x.md could not be read"]), space([], notShown: 2), space([], foldersNotRead: 1)] {
            XCTAssertTrue(space(allClosed.goals, problems: read.problems, notShown: read.notShown, foldersNotRead: read.foldersNotRead).showsInOpenList)
            XCTAssertTrue(space(open.goals, problems: read.problems, notShown: read.notShown, foldersNotRead: read.foldersNotRead).showsInClosedList)
        }
    }

    // MARK: Marking a goal done

    private final class FakeSend: @unchecked Sendable {
        private let lock = NSLock()
        private var calls: [(path: String, method: String, body: WorkJSON)] = []
        let answer: (Int, WorkJSON?)
        init(_ status: Int, _ value: WorkJSON? = nil) { answer = (status, value) }
        var send: WorkGoalSend {
            { [self] path, method, body in record(path, method, body); return answer }
        }
        private func record(_ path: String, _ method: String, _ body: WorkJSON) { lock.lock(); defer { lock.unlock() }; calls.append((path, method, body)) }
        func sent() -> [(path: String, method: String, body: WorkJSON)] { lock.lock(); defer { lock.unlock() }; return calls }
    }

    private func revised(_ goal: WorkGoal, _ revision: String? = "rev-1") -> WorkGoal { var goal = goal; goal.revision = revision; return goal }

    func testMarkDoneAndReopenBodiesChangeOnlyStatusAndClosed() {
        let goal = revised(WorkGoal(name: "Ship", title: "Ship", path: "Workspaces/acme/Goals/Ship/Ship.md", workspace: nil, kind: .main, parent: nil, peers: [], owner: nil,
                                    status: "active", target: nil, visibility: .private, program: nil, goalID: nil, metrics: []))
        XCTAssertEqual(WorkGoalWriter.statusBody(goal, to: .done, today: "2026-10-28"), .object([
            "path": .string("Workspaces/acme/Goals/Ship/Ship.md"), "expected_revision": .string("rev-1"),
            "set": .object(["status": .string("done"), "closed": .string("2026-10-28")]),
        ]))
        XCTAssertEqual(WorkGoalWriter.statusBody(goal, to: .active, today: "2026-10-28"), .object([
            "path": .string("Workspaces/acme/Goals/Ship/Ship.md"), "expected_revision": .string("rev-1"),
            "set": .object(["status": .string("active")]), "unset": .array([.string("closed")]),
        ]))
        XCTAssertNil(WorkGoalWriter.statusBody(revised(goal, nil), to: .done, today: "2026-10-28"))
        XCTAssertNil(WorkGoalWriter.statusBody(revised(goal, ""), to: .done, today: "2026-10-28"))
    }

    func testUpdateStatusPatchesTheFrontMatterAndReadsTheAnswer() async throws {
        let goal = revised(goalWith("Ship"))
        let saved = FakeSend(200, .object(["revision": .string("rev-2")]))
        let result = try await WorkGoalWriter.updateStatus(goal, to: .done, today: "2026-10-28", send: saved.send)
        XCTAssertEqual(result, .saved(revision: "rev-2"))
        XCTAssertEqual(saved.sent().map { $0.path }, ["/api/magician/v2/notes/front-matter"])
        XCTAssertEqual(saved.sent().map { $0.method }, ["PATCH"])
        XCTAssertEqual(saved.sent().first?.body, WorkGoalWriter.statusBody(goal, to: .done, today: "2026-10-28"))

        let conflict = try await WorkGoalWriter.updateStatus(goal, to: .active, send: FakeSend(409, .object(["error": .string("revision_mismatch")])).send)
        XCTAssertEqual(conflict, .conflict("This goal changed since it was loaded."))
        let refused = try await WorkGoalWriter.updateStatus(goal, to: .done, send: FakeSend(403, .object(["message": .string("Owner sessions only.")])).send)
        XCTAssertEqual(refused, .failed("Owner sessions only."))
        let bare = try await WorkGoalWriter.updateStatus(goal, to: .active, send: FakeSend(500).send)
        XCTAssertEqual(bare, .failed("Could not reopen the goal (500)."))

        let unread = FakeSend(200)
        let stale = try await WorkGoalWriter.updateStatus(revised(goal, nil), to: .done, send: unread.send)
        XCTAssertEqual(stale, .conflict("This goal has no saved revision yet. Reload the goals and try again."))
        XCTAssertTrue(unread.sent().isEmpty, "a goal read without a revision is never written")
    }

    func testStatusNoticesMatchTheWeb() {
        XCTAssertEqual(WorkGoalCopy.statusNotice("Ship", .done, .saved(revision: nil)), "“Ship” is marked done. It is listed under Closed.")
        XCTAssertEqual(WorkGoalCopy.statusNotice("Ship", .active, .saved(revision: "r")), "“Ship” is open again.")
        XCTAssertEqual(WorkGoalCopy.statusNotice("Ship", .done, .conflict("x")), "“Ship” changed since this screen loaded, so it was not marked done. The goals have been reloaded; try again if it is still needed.")
        XCTAssertEqual(WorkGoalCopy.statusNotice("Ship", .active, .conflict("x")), "“Ship” changed since this screen loaded, so it was not reopened. The goals have been reloaded; try again if it is still needed.")
        XCTAssertEqual(WorkGoalCopy.statusNotice("Ship", .done, .failed("Offline.")), "Could not mark “Ship” done: Offline.")
        XCTAssertEqual(WorkGoalCopy.statusNotice("Ship", .active, .failed("Offline.")), "Could not reopen “Ship”: Offline.")
        XCTAssertEqual(WorkGoalCopy.openGoalCount(1), "1 open goal")
        XCTAssertEqual(WorkGoalCopy.allClosed, "No open goals: every goal read here is done or dropped.")
    }

    func testTheClosedDateIsTodayOnTheDevicesCalendarDay() {
        let evening = Date(timeIntervalSince1970: 20 * 3600) // 1970-01-01 20:00 UTC
        XCTAssertEqual(WorkGoalWriter.localDate(evening, timeZone: TimeZone(identifier: "UTC")!), "1970-01-01")
        XCTAssertEqual(WorkGoalWriter.localDate(evening, timeZone: TimeZone(identifier: "Asia/Kolkata")!), "1970-01-02")
    }

    // MARK: Workspaces

    private final class FakeNotes: @unchecked Sendable {
        typealias Handler = (_ kind: String, _ path: String) -> (Int, String)
        private let lock = NSLock()
        private var calls: [(String, String)] = []
        private var raws: [String] = []
        let handle: Handler
        init(_ handle: @escaping Handler) { self.handle = handle }
        var fetch: WorkGoalFetch {
            { [self] raw in
                let parts = URLComponents(string: raw)!
                let kind = parts.path.hasSuffix("/notes/tree") ? "tree" : "file"
                let path = parts.queryItems?.first { $0.name == "path" }?.value ?? ""
                record(kind, path, raw)
                let (status, body) = handle(kind, path)
                return (status, try? JSONDecoder().decode(WorkJSON.self, from: Data(body.utf8)))
            }
        }
        private func record(_ kind: String, _ path: String, _ raw: String) { lock.lock(); defer { lock.unlock() }; calls.append((kind, path)); raws.append(raw) }
        func listed() -> [String] { lock.lock(); defer { lock.unlock() }; return calls.filter { $0.0 == "tree" }.map(\.1) }
        func read() -> [String] { lock.lock(); defer { lock.unlock() }; return calls.filter { $0.0 == "file" }.map(\.1) }
        func raw() -> [String] { lock.lock(); defer { lock.unlock() }; return raws }
        func all() -> [(String, String)] { lock.lock(); defer { lock.unlock() }; return calls }
    }

    private static func json(_ value: Any) -> String { String(data: try! JSONSerialization.data(withJSONObject: value), encoding: .utf8)! }
    private static func entries(_ rows: [[String: Any]]) -> String { json(["entries": rows]) }
    private static func file(_ name: String, _ path: String) -> [String: Any] { ["name": name, "relative_path": path, "kind": "file"] }
    private static func dir(_ name: String, _ path: String, children: Bool? = true) -> [String: Any] {
        var row: [String: Any] = ["name": name, "relative_path": path, "kind": "dir"]
        if let children { row["has_children"] = children }
        return row
    }
    private static let notFound = (404, #"{"error":"not_found"}"#)

    /// A notes space as the tree API lists it: one level per request, folders first.
    private func notesSpace(_ paths: [String], broken: [String] = []) -> FakeNotes {
        var folders: [String: [[String: Any]]] = [:]
        for path in paths {
            let parts = path.components(separatedBy: "/")
            for i in 1..<parts.count {
                let folder = parts[..<i].joined(separator: "/"), relative = parts[...i].joined(separator: "/")
                let isFile = i == parts.count - 1 && path.hasSuffix(".md")
                var row: [String: Any] = ["name": parts[i], "relative_path": relative, "kind": isFile ? "file" : "dir"]
                if !isFile { row["has_children"] = i != parts.count - 1 }
                var rows = folders[folder] ?? []
                if !rows.contains(where: { ($0["relative_path"] as? String) == relative }) { rows.append(row) }
                folders[folder] = rows
            }
        }
        for (key, rows) in folders {
            folders[key] = rows.sorted { a, b in
                let ak = a["kind"] as! String, bk = b["kind"] as! String
                if ak != bk { return ak < bk }
                return (a["name"] as! String).lowercased() < (b["name"] as! String).lowercased()
            }
        }
        let snapshot = folders
        return FakeNotes { kind, path in
            if kind == "tree" {
                if broken.contains(path) { return (500, Self.json(["error": "io_error", "message": "Could not list \(path)."])) }
                return snapshot[path].map { (200, Self.json(["path": path, "entries": $0])) } ?? Self.notFound
            }
            let name = (path.components(separatedBy: "/").last ?? "").replacingOccurrences(of: ".md", with: "")
            return (200, Self.json(["relative_path": path, "markdown": "---\nmagician_kind: goal\n---\n# \(name)\n"]))
        }
    }

    func testReadsEveryGoalNoteInEachWorkspaceGoalsFolder() async throws {
        let mainNote = self.main, subNote = self.sub
        let notes = FakeNotes { kind, path in
            if kind == "tree" {
                if path == "Workspaces" { return (200, Self.entries([Self.dir("acme", "Workspaces/acme"), Self.file("README.md", "Workspaces/README.md")])) }
                if path == "Workspaces/acme/Goals" { return (200, Self.entries([
                    Self.file("Raise a seed round.md", "Workspaces/acme/Goals/Raise a seed round.md"),
                    Self.file("Apply to accelerators.md", "Workspaces/acme/Goals/Apply to accelerators.md"),
                    Self.dir("Archive", "Workspaces/acme/Goals/Archive"),
                    Self.file("pitch.pdf", "Workspaces/acme/Goals/pitch.pdf"),
                ])) }
                return Self.notFound
            }
            return (200, Self.json(["title": "", "relative_path": path, "markdown": path.hasSuffix("Raise a seed round.md") ? mainNote : subNote]))
        }
        let workspaces = try await WorkGoalReader.load(fetch: notes.fetch)
        XCTAssertEqual(workspaces.count, 1)
        XCTAssertEqual(workspaces[0].name, "acme"); XCTAssertEqual(workspaces[0].path, "Workspaces/acme")
        XCTAssertEqual(workspaces[0].problems, []); XCTAssertEqual(workspaces[0].notShown, 0); XCTAssertEqual(workspaces[0].foldersNotRead, 0)
        XCTAssertEqual(workspaces[0].goals.map(\.name), ["Raise a seed round", "Apply to accelerators"])
        XCTAssertEqual(workspaces[0].goals[1].path, "Workspaces/acme/Goals/Apply to accelerators.md")
        // Up to four notes are read at once, so the request order varies; the goals keep listing order.
        XCTAssertEqual(notes.read().sorted(), ["Workspaces/acme/Goals/Apply to accelerators.md", "Workspaces/acme/Goals/Raise a seed round.md"])
        XCTAssertEqual(notes.all().first?.1, "Workspaces")
    }

    func testKeepsTheRevisionEachNoteWasReadAt() async throws {
        let notes = FakeNotes { kind, path in
            if kind == "tree" { return path == "Workspaces" ? (200, Self.entries([Self.dir("acme", "Workspaces/acme")])) : (200, Self.entries([Self.file("A.md", "Workspaces/acme/Goals/A.md"), Self.file("B.md", "Workspaces/acme/Goals/B.md")])) }
            let revision: [String: Any] = path.hasSuffix("A.md") ? ["revision": "sha-a"] : [:]
            return (200, Self.json(revision.merging(["markdown": "---\nmagician_kind: goal\n---\n# x"]) { a, _ in a }))
        }
        let goals = try await WorkGoalReader.load(fetch: notes.fetch)[0].goals
        XCTAssertEqual(goals.map(\.revision), ["sha-a", nil])
    }

    func testRequestsTheNotesTreeAndFileAPIs() async throws {
        let notes = FakeNotes { kind, path in
            if kind == "tree" { return path == "Workspaces" ? (200, Self.entries([Self.dir("acme co", "Workspaces/acme co")])) : (200, Self.entries([Self.file("A b.md", "Workspaces/acme co/Goals/A b.md")])) }
            return (200, Self.json(["markdown": "---\nmagician_kind: goal\n---\n# A"]))
        }
        _ = try await WorkGoalReader.load(fetch: notes.fetch)
        XCTAssertEqual(notes.raw(), [
            "/api/magician/v2/notes/tree?path=Workspaces",
            "/api/magician/v2/notes/tree?path=Workspaces%2Facme%20co%2FGoals",
            "/api/magician/v2/notes/file?path=Workspaces%2Facme%20co%2FGoals%2FA%20b.md",
        ])
    }

    func testTreatsAMissingWorkspacesFolderAsNoWorkspaces() async throws {
        let notes = FakeNotes { _, _ in Self.notFound }
        let workspaces = try await WorkGoalReader.load(fetch: notes.fetch)
        XCTAssertEqual(workspaces, [])
        XCTAssertEqual(notes.all().count, 1)
    }

    func testFailsWhenTheWorkspacesFolderCannotBeListed() async {
        let notes = FakeNotes { _, _ in (503, #"{"error":"notes_unavailable","message":"Notes are unavailable."}"#) }
        do { _ = try await WorkGoalReader.load(fetch: notes.fetch); XCTFail("expected a failure") }
        catch { XCTAssertEqual(error.localizedDescription, "Notes are unavailable.") }
        let bare = FakeNotes { _, _ in (500, #"{"error":"io_error"}"#) }
        do { _ = try await WorkGoalReader.load(fetch: bare.fetch); XCTFail("expected a failure") }
        catch { XCTAssertEqual(error.localizedDescription, "Could not list Workspaces (500).") }
    }

    func testKeepsTheReadableGoalsAndReportsANoteThatCouldNotBeRead() async throws {
        let mainNote = self.main
        let notes = FakeNotes { kind, path in
            if kind == "tree" {
                if path == "Workspaces" { return (200, Self.entries([Self.dir("acme", "Workspaces/acme", children: nil), Self.dir("empty", "Workspaces/empty", children: nil)])) }
                if path == "Workspaces/acme/Goals" { return (200, Self.entries([
                    Self.file("Raise a seed round.md", "Workspaces/acme/Goals/Raise a seed round.md"),
                    Self.file("Broken.md", "Workspaces/acme/Goals/Broken.md"),
                ])) }
                return Self.notFound
            }
            if path.hasSuffix("Broken.md") { return (500, #"{"error":"io_error"}"#) }
            return (200, Self.json(["markdown": mainNote]))
        }
        let workspaces = try await WorkGoalReader.load(fetch: notes.fetch)
        XCTAssertEqual(workspaces.map(\.name), ["acme", "empty"])
        XCTAssertEqual(workspaces[0].goals.map(\.name), ["Raise a seed round"])
        XCTAssertEqual(workspaces[0].problems, ["Could not read Workspaces/acme/Goals/Broken.md (500)."])
        XCTAssertEqual(workspaces[1].goals, []); XCTAssertEqual(workspaces[1].problems, [])
    }

    func testReadsOneFolderPerMainGoalItsSubGoalsBesideItAndLegacyNotesDirectlyInGoals() async throws {
        let g = "Workspaces/acme/Goals"
        let space = notesSpace([
            "\(g)/Ship Picker v1/Pilot with three warehouses.md",
            "\(g)/Ship Picker v1/Ship Picker v1.md",
            "\(g)/Ship Picker v1/Drafts/Draft goal.md",
            "\(g)/Hire two field engineers/Hire two field engineers.md",
            "\(g)/Empty folder",
            "\(g)/Old flat goal.md",
        ])
        let acme = try await WorkGoalReader.load(fetch: space.fetch)[0]
        // The main goal's own note leads its folder; a folder's notes come before its subfolders.
        XCTAssertEqual(acme.goals.map(\.path), [
            "\(g)/Hire two field engineers/Hire two field engineers.md",
            "\(g)/Ship Picker v1/Ship Picker v1.md",
            "\(g)/Ship Picker v1/Pilot with three warehouses.md",
            "\(g)/Ship Picker v1/Drafts/Draft goal.md",
            "\(g)/Old flat goal.md",
        ])
        XCTAssertEqual(acme.problems, []); XCTAssertEqual(acme.notShown, 0); XCTAssertEqual(acme.foldersNotRead, 0)
        // A folder the tree API reports as empty is never listed.
        XCTAssertFalse(space.listed().contains("\(g)/Empty folder"))
        XCTAssertTrue(space.listed().contains("\(g)/Ship Picker v1/Drafts"))
    }

    func testPlacesASubGoalByItsParentFieldNotByTheFolderItSitsIn() async throws {
        let g = "Workspaces/acme/Goals", mainNote = self.main, subNote = self.sub
        let notes = FakeNotes { kind, path in
            if kind == "tree" {
                switch path {
                case "Workspaces": return (200, Self.entries([Self.dir("acme", "Workspaces/acme")]))
                case g: return (200, Self.entries([Self.dir("Raise a seed round", "\(g)/Raise a seed round"), Self.dir("Ship the beta", "\(g)/Ship the beta")]))
                case "\(g)/Raise a seed round": return (200, Self.entries([Self.file("Raise a seed round.md", "\(g)/Raise a seed round/Raise a seed round.md")]))
                case "\(g)/Ship the beta": return (200, Self.entries([
                    Self.file("Apply to accelerators.md", "\(g)/Ship the beta/Apply to accelerators.md"),
                    Self.file("Ship the beta.md", "\(g)/Ship the beta/Ship the beta.md"),
                ]))
                default: return Self.notFound
                }
            }
            if path.hasSuffix("Raise a seed round.md") { return (200, Self.json(["markdown": mainNote])) }
            if path.hasSuffix("Apply to accelerators.md") { return (200, Self.json(["markdown": subNote])) }
            return (200, Self.json(["markdown": "---\nmagician_kind: goal\nkind: main\n---\n# Ship the beta"]))
        }
        let acme = try await WorkGoalReader.load(fetch: notes.fetch)[0]
        let tree = workBuildGoalTree(acme.goals)
        XCTAssertEqual(tree.roots.map(\.goal.name), ["Raise a seed round", "Ship the beta"])
        XCTAssertEqual(tree.roots.map { $0.children.map(\.goal.name) }, [["Apply to accelerators"], []])
    }

    func testStopsAtTheFolderDepthLimitAndCountsTheFoldersItDidNotRead() async throws {
        let g = "Workspaces/acme/Goals"
        let space = notesSpace(["\(g)/Main/Main.md", "\(g)/Main/Two/Three/Deep.md", "\(g)/Main/Two/Three/Four/Too deep.md"])
        let acme = try await WorkGoalReader.load(limits: WorkGoalReadLimits(folderDepth: 3, folders: 1000, notes: 2000), fetch: space.fetch)[0]
        XCTAssertEqual(acme.goals.map(\.name), ["Main", "Deep"])
        XCTAssertEqual(acme.foldersNotRead, 1); XCTAssertEqual(acme.notShown, 0)
        XCTAssertFalse(space.listed().contains("\(g)/Main/Two/Three/Four"))
    }

    func testReadsAtMostTheNoteLimitAndTheFolderLimitPerWorkspaceAndCountsTheRest() async throws {
        let g = "Workspaces/acme/Goals"
        let space = notesSpace(["\(g)/A/A.md", "\(g)/A/A1.md", "\(g)/A/A2.md", "\(g)/B/B.md", "\(g)/C/C.md", "\(g)/Flat.md"])
        let acme = try await WorkGoalReader.load(limits: WorkGoalReadLimits(folderDepth: 3, folders: 2, notes: 3), fetch: space.fetch)[0]
        XCTAssertEqual(acme.goals.map(\.name), ["A", "A1", "A2"])
        // B.md and Flat.md are listed but past the note limit; folder C is past the folder limit.
        XCTAssertEqual(acme.notShown, 2); XCTAssertEqual(acme.foldersNotRead, 1); XCTAssertEqual(acme.problems, [])
        XCTAssertEqual(space.read().count, 3)
        XCTAssertFalse(space.listed().contains("\(g)/C"))
    }

    func testReportsAMainGoalFolderThatCouldNotBeListedAndStillShowsTheOthers() async throws {
        let g = "Workspaces/acme/Goals"
        let space = notesSpace(["\(g)/Good/Good.md", "\(g)/Bad/Bad.md", "\(g)/Flat.md"], broken: ["\(g)/Bad"])
        let acme = try await WorkGoalReader.load(fetch: space.fetch)[0]
        XCTAssertEqual(acme.goals.map(\.name), ["Good", "Flat"])
        XCTAssertEqual(acme.problems, ["Could not list \(g)/Bad."])
    }

    func testDefaultLimitsMatchTheWeb() {
        XCTAssertEqual(WorkGoalReadLimits.standard, WorkGoalReadLimits(folderDepth: 3, folders: 1000, notes: 2000))
    }

    // MARK: Screen copy and navigation

    func testScreenCopyMatchesTheWeb() {
        let limits = WorkGoalReadLimits(folderDepth: 1, folders: 10, notes: 3)
        XCTAssertEqual(WorkGoalCopy.notShown(1, limits: limits), "1 more note in Goals/ not shown: this screen reads the first 3 notes of a workspace. Open the notes to see the rest.")
        XCTAssertEqual(WorkGoalCopy.foldersNotRead(1, limits: limits), "1 folder in Goals/ not read: this screen reads folders up to 1 level deep, and at most 10 of them.")
        XCTAssertEqual(WorkGoalCopy.foldersNotRead(2, limits: .standard), "2 folders in Goals/ not read: this screen reads folders up to 3 levels deep, and at most 1000 of them.")
        XCTAssertEqual(WorkGoalCopy.problems(1), "One item could not be read")
        XCTAssertEqual(WorkGoalCopy.problems(3), "3 items could not be read")
        XCTAssertEqual(WorkGoalCopy.deeper(1), "1 deeper goal is not shown here. Open the note to follow them.")
        XCTAssertEqual(WorkGoalCopy.deeper(2), "2 deeper goals are not shown here. Open the note to follow them.")
        XCTAssertEqual("\(WorkGoalCopy.emptyLead) \(WorkGoalCopy.emptyLink) \(WorkGoalCopy.emptyTail)", "No goal workspaces yet. Ask Magican in chat to set one up from a template.")
        XCTAssertEqual(WorkGoalVisibility.internal.meaning, "Agents may use it; never stated outward.")
    }

    func testGoalsIsAWorkViewWithHelpAndARoute() {
        XCTAssertEqual(WorkSection.goals.title, "Goals")
        XCTAssertEqual(WorkSection.goals.help, .goals)
        XCTAssertEqual(WorkHelpTopic.goals.explanation, "Your goal workspaces: goals, the sub-goals under them and who owns each. Set one up from a template in chat.")
        XCTAssertEqual(WorkHelpTopic.forLabel("Goals"), .goals)
        XCTAssertEqual(WorkNavigationTarget.parse("/work?view=goals")?.section, .goals)
        XCTAssertEqual(WorkNavigationTarget.parse("/work?view=goals")?.closedGoals, false)
        XCTAssertEqual(WorkNavigationTarget.parse("/work?view=goals&goals=closed")?.closedGoals, true)
        XCTAssertEqual(WorkNavigationTarget.parse("magican://work?view=goals&goals=closed")?.closedGoals, true)
        XCTAssertEqual(WorkWebContext(section: .goals).returnPath, "/work?view=goals")
    }
}
