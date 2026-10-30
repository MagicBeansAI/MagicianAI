import Foundation

// Pure logic behind the Work summaries: the goal tree's collapse state, the
// goal and quality counts, and what the All work landing shows. Ports of the
// web's `GoalsWorkspace` / `GoalRow` (collapse, counts, compact list) and the
// Quality masthead counts; no networking and no SwiftUI.

/// Which goal nodes are folded, by note path, like the web's `collapsedPaths`.
/// A node only folds when it has sub-goals; a path survives a reload.
struct WorkGoalCollapse: Equatable, Sendable {
    private(set) var collapsed: Set<String> = []

    func isCollapsed(_ node: WorkGoalNode) -> Bool { !node.children.isEmpty && collapsed.contains(node.goal.path) }
    mutating func toggle(_ path: String) {
        if collapsed.contains(path) { collapsed.remove(path) } else { collapsed.insert(path) }
    }
    mutating func expandAll() { collapsed = [] }
    /// Folds every node with sub-goals, in the main trees and the "Not linked" ones.
    mutating func collapseAll(_ trees: [WorkGoalTree]) { collapsed = Self.collapsible(trees) }

    static func collapsible(_ trees: [WorkGoalTree]) -> Set<String> {
        var paths = Set<String>()
        func visit(_ nodes: [WorkGoalNode]) {
            for node in nodes where !node.children.isEmpty { paths.insert(node.goal.path); visit(node.children) }
        }
        for tree in trees { visit(tree.roots); visit(tree.orphans.map(\.node)) }
        return paths
    }
}

/// Which goals the Goals screen lists: the open trees or the Closed list. Remembered per
/// device, and set by the All work landing's links into the screen.
enum WorkGoalList: String, CaseIterable, Sendable {
    case open, closed
    static let storageKey = "work.goals.list"
}

extension WorkGoalCopy {
    static func goalCount(_ count: Int) -> String { plural(count, "goal", "goals") }
    /// The web counts a node's direct sub-goals (`node.children.length`).
    static func subGoals(_ count: Int) -> String { plural(count, "sub-goal", "sub-goals") }
    static func subGoals(of node: WorkGoalNode) -> String { subGoals(node.children.count) }
    static func expandLabel(_ title: String) -> String { "Expand sub-goals of \(title)" }
    static func collapseLabel(_ title: String) -> String { "Collapse sub-goals of \(title)" }
    static func ownerLabel(_ owner: String) -> String { owner == "user" ? "You" : owner }
}

/// The Quality catalog's `counts`: what needs the owner (briefs, reviews and
/// attention, as the web's "N needing review") and the active runs.
struct WorkQualityCounts: Equatable, Sendable {
    var needsReview: Int
    var active: Int

    /// `nil` when the response carries no `counts` object.
    init?(catalog: WorkJSON?) {
        guard let counts = catalog?["counts"].object else { return nil }
        func number(_ key: String) -> Int {
            switch counts[key] {
            case .integer(let value)?: return Int(value)
            case .unsigned(let value)?: return Int(clamping: value)
            case .number(let value)?: return value.isFinite ? Int(value) : 0
            default: return 0
            }
        }
        needsReview = number("brief") + number("review") + number("attention")
        active = number("active")
    }

    static func inboxLabel(_ counts: Self?) -> String { counts.map { "Needs review · \($0.needsReview)" } ?? "Needs review" }
    static func runsLabel(_ counts: Self?) -> String { counts.map { "Runs · \($0.active) active" } ?? "Runs" }
}

enum WorkPlaybookCopy {
    static let empty = "No playbook documents in this workspace."
    static func title(_ count: Int?) -> String { count.map { "Playbooks · \($0)" } ?? "Playbooks" }
}

/// What the All work landing summary reads and shows.
enum WorkLanding {
    static let goalLimit = 3
    static let playbookLimit = 4
    /// Main-goal folders only, and a few notes each: enough for the first goals
    /// and their sub-goal counts without the Goals screen's full walk. A closed
    /// main goal still takes a folder and its notes, so this stays above the
    /// web's compact read (6 folders, 21 notes).
    static let goalReadLimits = WorkGoalReadLimits(folderDepth: 1, folders: 8, notes: 32)
    /// Coming back to the landing within this long (closing a task) keeps the summary.
    static let reloadAfter: TimeInterval = 60

    /// The web's compact list: main goals then "Not linked" heads, or every goal
    /// flat when no workspace has a main goal; the first `limit`.
    static func goals(_ spaces: [(goals: [WorkGoal], tree: WorkGoalTree)], limit: Int = goalLimit) -> [WorkGoalNode] {
        let roots = spaces.flatMap { $0.tree.roots }
        let nodes = roots.isEmpty
            ? spaces.flatMap { $0.goals.map { WorkGoalNode(goal: $0, children: []) } }
            : roots + spaces.flatMap { $0.tree.orphans.map(\.node) }
        return Array(nodes.prefix(max(0, limit)))
    }

    /// Each workspace's open goals and its tree without closed goals: what the landing lists and counts.
    static func openGoals(_ spaces: [WorkGoalWorkspace]) -> [(goals: [WorkGoal], tree: WorkGoalTree)] {
        spaces.map { space in (goals: space.goals.filter { !$0.isClosed }, tree: workOpenGoalTree(workBuildGoalTree(space.goals))) }
    }

    static func playbooks(_ programs: [WorkProgramSummary], limit: Int = playbookLimit) -> [WorkProgramSummary] {
        Array(programs.prefix(max(0, limit)))
    }

    /// A count read under the landing's limits is a floor when notes or folders were left unread.
    static func countLabel(_ count: Int, partial: Bool) -> String { partial ? "\(count)+" : "\(count)" }

    static func needsLoad(lastLoad: Date?, now: Date, reloadAfter: TimeInterval = reloadAfter) -> Bool {
        guard let lastLoad else { return true }
        return now.timeIntervalSince(lastLoad) >= reloadAfter
    }
}
