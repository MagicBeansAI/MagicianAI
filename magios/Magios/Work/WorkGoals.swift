import Foundation

// Goal notes: Markdown under `Workspaces/<name>/Goals/` with YAML frontmatter.
// A port of the web's `ui/unified-ui/src/lib/work/goals.ts`: the same fields,
// defaults, tree rules, read limits and messages. Only the flat fields the
// goal template writes are read; anything else is ignored.

enum WorkGoalKind: String, Equatable, Sendable { case main, sub, peer }
enum WorkGoalVisibility: String, Equatable, Sendable {
    case `private`, `internal`, `public`
    /// What the value allows, shown with the visibility chip.
    var meaning: String {
        switch self {
        case .private: return "Shown only to you; never in an outward draft."
        case .internal: return "Agents may use it; never stated outward."
        case .public: return "May be stated outward."
        }
    }
}

struct WorkGoal: Equatable, Sendable {
    var name: String
    var title: String
    var path: String
    var workspace: String?
    var kind: WorkGoalKind
    /// The parent's note name with link brackets removed. May be empty, which
    /// the tree reads as no parent, as the web does.
    var parent: String?
    var peers: [String]
    var owner: String?
    var status: String
    var target: String?
    var visibility: WorkGoalVisibility
    var program: String?
    var goalID: String?
    var metrics: [String]
    /// `closed:`, the date (`YYYY-MM-DD`) the goal was marked done or dropped, when recorded.
    var closed: String? = nil
    /// Content revision of the note as read (`GET /notes/file`). An edit sends it back so a
    /// change made since the read (by an agent or another device) is refused, not overwritten.
    var revision: String? = nil

    /// `done` and `dropped` goals are closed: the goal screens list them only under Closed.
    static let closedStatuses: Set<String> = ["done", "dropped"]
    var isClosed: Bool { Self.closedStatuses.contains(status) }
}

/// Why a goal heads a "Not linked" tree instead of hanging under a main goal.
enum WorkGoalOrphanReason: String, Equatable, Sendable {
    case missingParent = "missing-parent", selfParent = "self-parent", parentLoop = "parent-loop"
}

/// `closedParent` names the closed goal an open goal was moved up from (`workOpenGoalTree`).
struct WorkGoalNode: Equatable, Sendable {
    var goal: WorkGoal
    var children: [WorkGoalNode]
    var closedParent: String? = nil
}

struct WorkGoalOrphan: Equatable, Sendable {
    var node: WorkGoalNode
    var reason: WorkGoalOrphanReason
    var goal: WorkGoal { node.goal }
}

/// Every goal appears exactly once: under a main goal or under one orphan root.
/// `byKey` resolves links by `workGoalKey`; the first goal listed wins a shared key.
struct WorkGoalTree: Equatable, Sendable {
    var roots: [WorkGoalNode]
    var orphans: [WorkGoalOrphan]
    var byKey: [String: WorkGoal]
}

// MARK: - Regex helpers (JS semantics on UTF-16, like the web)

private func goalMatch(_ pattern: String, _ text: String, options: NSRegularExpression.Options = []) -> [String?]? {
    guard let regex = try? NSRegularExpression(pattern: pattern, options: options) else { return nil }
    let ns = text as NSString
    guard let match = regex.firstMatch(in: text, range: NSRange(location: 0, length: ns.length)) else { return nil }
    return (0..<match.numberOfRanges).map { index in
        let range = match.range(at: index)
        return range.location == NSNotFound ? nil : ns.substring(with: range)
    }
}

/// `text.split(/\r?\n/)`.
private func goalLines(_ text: String) -> [String] {
    text.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n")
}

/// JS `String.prototype.trim` (whitespace and line terminators).
private func goalTrim(_ text: String) -> String { text.trimmingCharacters(in: .whitespacesAndNewlines) }

private func goalDroppingMD(_ name: String) -> String {
    name.range(of: "\\.md$", options: [.regularExpression, .caseInsensitive]).map { String(name[..<$0.lowerBound]) } ?? name
}

/// `[[Name|alias]]` → `Name`; anything else is returned trimmed.
func workGoalLinkTarget(_ value: String) -> String {
    let trimmed = goalTrim(value)
    if let match = goalMatch("^\\[\\[([^\\]|]+)(?:\\|[^\\]]*)?\\]\\]\\z", trimmed), let inner = match[1] { return goalTrim(inner) }
    return trimmed
}

/// The note a link names: `[[Goals/Name#part|alias]]`, `Name.md` and `name` all name the note `Name`.
func workGoalKey(_ link: String) -> String {
    let target = workGoalLinkTarget(link)
    let beforeHash = target.components(separatedBy: "#").first ?? ""
    let last = beforeHash.components(separatedBy: "/").last ?? ""
    return goalTrim(goalDroppingMD(last)).lowercased()
}

private func goalUnquote(_ value: String) -> String {
    let v = goalTrim(value)
    let chars = Array(v.utf16)
    guard chars.count > 1 else { return v }
    let quote: UInt16? = chars.first == 34 && chars.last == 34 ? 34 : (chars.first == 39 && chars.last == 39 ? 39 : nil)
    guard quote != nil else { return v }
    return String(decoding: chars[1..<(chars.count - 1)], as: UTF16.self)
}

private func goalIsSpace(_ c: Character) -> Bool { c.isWhitespace }

/// Splits at `separator` outside quotes and brackets and drops a ` #` comment.
/// A quote only opens at the start of a value, so `Founder's goal` stays plain.
private func goalScan(_ text: String, separator: Character? = nil) -> [String] {
    let chars = Array(text)
    var parts: [String] = []
    var quote: Character? = nil
    var depth = 0, start = 0
    var fresh = true
    var i = 0
    while i < chars.count {
        let c = chars[i]
        defer { i += 1 }
        if let open = quote { if c == open { quote = nil }; continue }
        if (c == "\"" || c == "'") && fresh { quote = c }
        else if c == "#" && (i == 0 || goalIsSpace(chars[i - 1])) { return parts + [String(chars[start..<i])] }
        else if c == "[" { depth += 1 }
        else if c == "]" { depth = max(0, depth - 1) }
        else if let separator, c == separator && depth == 0 { parts.append(String(chars[start..<i])); start = i + 1; fresh = true; continue }
        if !goalIsSpace(c) { fresh = c == "[" || (c == "," && depth > 0) }
    }
    return parts + [String(chars[start..<chars.count])]
}

private enum GoalField: Equatable { case text(String), list([String]) }

private func goalFrontmatter(_ markdown: String) -> (fields: [String: GoalField], body: String)? {
    let source = markdown.hasPrefix("\u{FEFF}") ? String(markdown.dropFirst()) : markdown
    guard let match = goalMatch("^---[ \\t]*\\r?\\n([\\s\\S]*?)\\r?\\n---[ \\t]*(?:\\r?\\n|\\z)", source), let whole = match[0], let block = match[1] else { return nil }
    var fields: [String: GoalField] = [:]
    var listKey: String? = nil
    for line in goalLines(block) {
        if let item = goalMatch("^\\s*-\\s+(.*)\\z", line) {
            let value = goalUnquote(goalScan(item[1] ?? "")[0])
            if let key = listKey, !value.isEmpty, case .list(let values)? = fields[key] { fields[key] = .list(values + [value]) }
            continue
        }
        guard let pair = goalMatch("^([A-Za-z_][A-Za-z0-9_-]*):\\s*(.*)\\z", line), let key = pair[1] else { continue }
        let value = goalTrim(goalScan(pair[2] ?? "")[0])
        listKey = nil
        if value.isEmpty { fields[key] = .list([]); listKey = key }
        // A flow list, unless the whole value is one bare `[[link]]`.
        else if value.hasPrefix("[") && value.hasSuffix("]") && goalMatch("^\\[\\[[^\\[\\]]*\\]\\]\\z", value) == nil {
            let inner = String(value.dropFirst().dropLast())
            fields[key] = .list(goalScan(inner, separator: ",").map(goalUnquote).filter { !$0.isEmpty })
        } else { fields[key] = .text(goalUnquote(value)) }
    }
    let body = ((source as NSString).substring(from: (whole as NSString).length))
    return (fields, body)
}

private func goalText(_ field: GoalField?) -> String? {
    guard case .text(let v)? = field, !v.isEmpty, v != "~", v != "null" else { return nil }
    return v
}

private func goalList(_ field: GoalField?) -> [String] {
    if case .list(let values)? = field { return values }
    return goalText(field).map { [$0] } ?? []
}

func workParseGoalNote(_ markdown: String, path: String) -> WorkGoal? {
    guard let note = goalFrontmatter(markdown), note.fields["magician_kind"] == .text("goal") else { return nil }
    let fields = note.fields
    let name = goalDroppingMD(path.components(separatedBy: "/").last ?? "")
    // Only an H1 on the first non-blank line is the title; otherwise the file name is.
    let firstLine = goalLines(note.body).first { !goalTrim($0).isEmpty }
    let heading = firstLine.flatMap { goalMatch("^#[ \\t]+(.+)\\z", $0)?[1] }.map(goalTrim)
    let parent = goalText(fields["parent"])
    let kind = goalText(fields["kind"]).flatMap { WorkGoalKind(rawValue: $0.lowercased()) } ?? (parent != nil ? .sub : .main)
    let visibility = goalText(fields["visibility"]).flatMap { WorkGoalVisibility(rawValue: $0.lowercased()) } ?? .private
    return WorkGoal(
        name: name, title: (heading?.isEmpty == false ? heading! : name), path: path, workspace: goalText(fields["workspace"]), kind: kind,
        parent: parent.map(workGoalLinkTarget),
        peers: goalList(fields["peers"]).map(workGoalLinkTarget), owner: goalText(fields["owner"]),
        status: goalText(fields["status"])?.lowercased() ?? "proposed", target: goalText(fields["target"]), visibility: visibility,
        program: goalText(fields["program"]), goalID: goalText(fields["goal_id"]), metrics: goalList(fields["metrics"]),
        closed: goalText(fields["closed"]))
}

// MARK: - Tree

private final class GoalTreeNode {
    let goal: WorkGoal
    let index: Int
    var children: [GoalTreeNode] = []
    init(_ goal: WorkGoal, _ index: Int) { self.goal = goal; self.index = index }
}

func workBuildGoalTree(_ goals: [WorkGoal]) -> WorkGoalTree {
    let nodes = goals.enumerated().map { GoalTreeNode($0.element, $0.offset) }
    var byKey: [String: GoalTreeNode] = [:]
    for node in nodes where byKey[workGoalKey(node.goal.name)] == nil { byKey[workGoalKey(node.goal.name)] = node }
    var parentOf: [ObjectIdentifier: GoalTreeNode] = [:]
    var mains: [GoalTreeNode] = []
    var heads: [(GoalTreeNode, WorkGoalOrphanReason)] = []
    for node in nodes {
        guard let link = node.goal.parent, !link.isEmpty else { mains.append(node); continue }
        guard let parent = byKey[workGoalKey(link)] else { heads.append((node, .missingParent)); continue }
        if parent === node { heads.append((node, .selfParent)) }
        else { parent.children.append(node); parentOf[ObjectIdentifier(node)] = parent }
    }
    // Copies each subtree once; a link back into a placed goal (a closed loop) is dropped.
    var placed = Set<ObjectIdentifier>()
    func place(_ node: GoalTreeNode) -> WorkGoalNode {
        placed.insert(ObjectIdentifier(node))
        // Children are collected first, then copied bottom-up, keeping their order.
        func copy(_ from: GoalTreeNode) -> WorkGoalNode {
            var kept: [GoalTreeNode] = []
            for child in from.children where !placed.contains(ObjectIdentifier(child)) { placed.insert(ObjectIdentifier(child)); kept.append(child) }
            return WorkGoalNode(goal: from.goal, children: kept.map(copy))
        }
        return copy(node)
    }
    let roots = mains.map(place)
    var orphans: [(index: Int, orphan: WorkGoalOrphan)] = heads.map { ($0.0.index, WorkGoalOrphan(node: place($0.0), reason: $0.1)) }
    // What is left hangs off a parent loop. Each loop is headed by its first member in goal-key order.
    for node in nodes where !placed.contains(ObjectIdentifier(node)) {
        var walked = Set<ObjectIdentifier>()
        var at = node
        while !walked.contains(ObjectIdentifier(at)) { walked.insert(ObjectIdentifier(at)); at = parentOf[ObjectIdentifier(at)]! }
        var loop = [at]
        var next = parentOf[ObjectIdentifier(at)]!
        while next !== at { loop.append(next); next = parentOf[ObjectIdentifier(next)]! }
        let head = loop.dropFirst().reduce(loop[0]) { best, member in
            let a = workGoalKey(member.goal.name), b = workGoalKey(best.goal.name)
            let earlier = a.utf16.lexicographicallyPrecedes(b.utf16) || (a == b && member.goal.path.utf16.lexicographicallyPrecedes(best.goal.path.utf16))
            return earlier ? member : best
        }
        orphans.append((head.index, WorkGoalOrphan(node: place(head), reason: .parentLoop)))
    }
    orphans.sort { $0.index < $1.index }
    return WorkGoalTree(roots: roots, orphans: orphans.map(\.orphan), byKey: byKey.mapValues(\.goal))
}

/// The tree without closed goals. An open goal under a closed one moves up to its nearest open
/// ancestor, or heads a tree of its own, and names the closed goal it sat under.
func workOpenGoalTree(_ tree: WorkGoalTree) -> WorkGoalTree {
    func prune(_ nodes: [WorkGoalNode], closedParent: String? = nil) -> [WorkGoalNode] {
        nodes.flatMap { node -> [WorkGoalNode] in
            node.goal.isClosed
                ? prune(node.children, closedParent: node.goal.title)
                : [WorkGoalNode(goal: node.goal, children: prune(node.children), closedParent: closedParent)]
        }
    }
    return WorkGoalTree(
        roots: prune(tree.roots),
        orphans: tree.orphans.flatMap { orphan in prune([orphan.node]).map { WorkGoalOrphan(node: $0, reason: orphan.reason) } },
        byKey: tree.byKey)
}

/// Closed goals, most recently closed first; those without a `closed:` date last, by title.
func workClosedGoals(_ goals: [WorkGoal]) -> [WorkGoal] {
    goals.filter(\.isClosed).sorted { a, b in
        let (left, right) = (a.closed ?? "", b.closed ?? "")
        if left != right { return left > right }
        return a.title.localizedCompare(b.title) == .orderedAscending
    }
}

/// The title of the goal a closed goal sat under, resolved by note name like the peer links;
/// the raw link when that goal was not read.
func workGoalParentTitle(_ goal: WorkGoal, in tree: WorkGoalTree) -> String? {
    guard let parent = goal.parent, !parent.isEmpty else { return nil }
    return tree.byKey[workGoalKey(parent)]?.title ?? parent
}

func workGoalOrphanNote(_ orphan: WorkGoalOrphan) -> String {
    switch orphan.reason {
    case .missingParent: return "Parent goal not found: \(orphan.goal.parent ?? "")"
    case .selfParent: return "Names itself as its parent."
    case .parentLoop: return "Part of a parent loop: its parent goals lead back to it."
    }
}

/// Program documents are bare `*.md` names; a goal may name one without the extension.
func workGoalProgramName(_ program: String) -> String {
    program.range(of: "\\.md$", options: [.regularExpression, .caseInsensitive]) != nil ? program : program + ".md"
}

// MARK: - Reading the notes

/// How much of one workspace's `Goals/` the screen reads. Each main goal has its own folder
/// (`Goals/<Main goal>/`), so a folder is one listing request and a note one read request.
struct WorkGoalReadLimits: Equatable, Sendable {
    var folderDepth: Int
    var folders: Int
    var notes: Int
    static let standard = WorkGoalReadLimits(folderDepth: 3, folders: 1000, notes: 2000)
}

/// `problems` are notes or folders in this workspace that could not be read; the rest still show.
/// `notShown` counts notes past the note limit; `foldersNotRead` counts folders past the depth or folder limit.
struct WorkGoalWorkspace: Equatable, Sendable {
    var name: String
    var path: String
    var goals: [WorkGoal]
    var problems: [String]
    var notShown: Int
    var foldersNotRead: Int

    /// Read only in part, or with notes that could not be read: goals may be missing, so both
    /// lists keep the workspace to show why.
    private var readInPart: Bool { !problems.isEmpty || notShown > 0 || foldersNotRead > 0 }
    /// A workspace whose goals are all closed is listed only under Closed. One with no goals yet
    /// stays in the open list, so it can be filled.
    var showsInOpenList: Bool { readInPart || goals.isEmpty || goals.contains { !$0.isClosed } }
    var showsInClosedList: Bool { readInPart || goals.contains(where: \.isClosed) }
}

/// One GET's status and decoded JSON body (nil when the body is not JSON).
typealias WorkGoalFetch = @Sendable (String) async throws -> (status: Int, value: WorkJSON?)

struct WorkGoalReadError: LocalizedError, Equatable {
    let message: String
    var errorDescription: String? { message }
}

private struct GoalNoteEntry: Sendable {
    let name: String
    let relativePath: String
    let kind: String
    let hasChildren: Bool?
    init?(_ value: WorkJSON) {
        guard let name = value["name"].text, let path = value["relative_path"].text, let kind = value["kind"].text else { return nil }
        self.name = name; relativePath = path; self.kind = kind; hasChildren = value["has_children"].flag
    }
}

enum WorkGoalReader {
    static let notesAPI = "/api/magician/v2/notes"
    static let workspacesFolder = "Workspaces"

    private static func failure(_ status: Int, _ value: WorkJSON?, _ what: String) -> WorkGoalReadError {
        let message = value?["message"].text ?? ""
        return WorkGoalReadError(message: message.isEmpty ? "Could not \(what) (\(status))." : message)
    }

    /// `nil` when the folder does not exist.
    private static func noteTree(_ path: String, fetch: WorkGoalFetch) async throws -> [GoalNoteEntry]? {
        let (status, value) = try await fetch("\(notesAPI)/tree?path=\(WorkAPIClient.query(path))")
        if status == 404 { return nil }
        guard (200..<300).contains(status) else { throw failure(status, value, "list \(path)") }
        return (value?["entries"].array ?? []).compactMap(GoalNoteEntry.init)
    }

    /// A folder that is missing (removed since it was listed) is empty.
    private static func listFolder(_ path: String, fetch: WorkGoalFetch) async throws -> FolderListing {
        do { return FolderListing(entries: (try await noteTree(path, fetch: fetch)) ?? [], problem: nil) }
        catch {
            if Task.isCancelled || error is CancellationError { throw error }
            return FolderListing(entries: [], problem: error.localizedDescription)
        }
    }

    private static func readGoalNote(_ path: String, fetch: WorkGoalFetch) async throws -> NoteRead {
        do {
            let (status, value) = try await fetch("\(notesAPI)/file?path=\(WorkAPIClient.query(path))")
            // Removed between the listing and the read.
            if status == 404 { return NoteRead(goal: nil, problem: nil) }
            guard (200..<300).contains(status) else { throw failure(status, value, "read \(path)") }
            guard let markdown = value?["markdown"].text else { throw WorkGoalReadError(message: "Could not read \(path).") }
            var goal = workParseGoalNote(markdown, path: path)
            goal?.revision = value?["revision"].text
            return NoteRead(goal: goal, problem: nil)
        } catch {
            if Task.isCancelled || error is CancellationError { throw error }
            return NoteRead(goal: nil, problem: error.localizedDescription)
        }
    }

    /// At most four notes requests at once, results in input order.
    private static func inPool<T: Sendable, R: Sendable>(_ items: [T], _ run: @escaping @Sendable (T) async throws -> R) async throws -> [R] {
        guard !items.isEmpty else { return [] }
        return try await withThrowingTaskGroup(of: (Int, R).self) { group in
            var results = [R?](repeating: nil, count: items.count)
            var next = 0
            while next < min(4, items.count) { let index = next; group.addTask { (index, try await run(items[index])) }; next += 1 }
            while let (index, result) = try await group.next() {
                results[index] = result
                if next < items.count { let index = next; group.addTask { (index, try await run(items[index])) }; next += 1 }
            }
            return results.map { $0! }
        }
    }

    private struct FolderListing: Sendable { let entries: [GoalNoteEntry]; let problem: String? }
    private struct NoteRead: Sendable { let goal: WorkGoal?; let problem: String? }

    private struct Walk {
        let root: String
        var listed: [String: [GoalNoteEntry]] = [:]
        var problems: [String] = []
        var folders = 0
        var foldersNotRead = 0
    }

    private struct Folder: Sendable { let space: Int; let path: String; let depth: Int }

    /// Lists every workspace's `Goals/` folder tree, one depth level at a time across all workspaces,
    /// so a level's listings share one request pool. A folder the API reports as empty is not listed.
    private static func listGoalFolders(_ spaces: [GoalNoteEntry], limits: WorkGoalReadLimits, fetch: @escaping WorkGoalFetch) async throws -> [Walk] {
        var walks = spaces.map { Walk(root: "\($0.relativePath)/Goals") }
        var level = walks.enumerated().map { Folder(space: $0.offset, path: $0.element.root, depth: 0) }
        while !level.isEmpty {
            let results = try await inPool(level) { folder in try await listFolder(folder.path, fetch: fetch) }
            var next: [Folder] = []
            for (i, folder) in level.enumerated() {
                let entries = results[i].entries
                if let problem = results[i].problem { walks[folder.space].problems.append(problem) }
                walks[folder.space].listed[folder.path] = entries
                for entry in entries {
                    if entry.kind != "dir" || entry.hasChildren == false { continue }
                    if folder.depth + 1 > limits.folderDepth || walks[folder.space].folders >= limits.folders { walks[folder.space].foldersNotRead += 1; continue }
                    walks[folder.space].folders += 1
                    next.append(Folder(space: folder.space, path: entry.relativePath, depth: folder.depth + 1))
                }
            }
            level = next
        }
        return walks
    }

    /// Markdown notes in reading order: the main-goal folders in `Goals/` first, then legacy notes
    /// directly in `Goals/`. Inside a folder its notes come before its subfolders, and the note named
    /// after the folder (the main goal) leads.
    private static func goalNotePaths(_ walk: Walk) -> [String] {
        var paths: [String] = []
        func visit(_ path: String) {
            let entries = walk.listed[path] ?? []
            let folder = (path.components(separatedBy: "/").last ?? "").lowercased()
            let own = { (entry: GoalNoteEntry) in goalDroppingMD(entry.name).lowercased() == folder }
            let notes = entries.filter { $0.kind == "file" && $0.name.range(of: "\\.md$", options: [.regularExpression, .caseInsensitive]) != nil }
            let subfolders = { for entry in entries where entry.kind == "dir" && walk.listed[entry.relativePath] != nil { visit(entry.relativePath) } }
            if path == walk.root { subfolders() }
            paths.append(contentsOf: (notes.filter(own) + notes.filter { !own($0) }).map(\.relativePath))
            if path != walk.root { subfolders() }
        }
        visit(walk.root)
        return paths
    }

    /// Every folder under `Workspaces/` with the goal notes under its `Goals/` folder: one folder per
    /// main goal (`Goals/<Main goal>/`, its sub-goals flat beside it) and legacy notes directly in `Goals/`.
    /// The `parent:` field, not the folder, places a goal in the tree. No `Workspaces/` folder is no workspaces.
    static func load(limits: WorkGoalReadLimits = .standard, fetch: @escaping WorkGoalFetch) async throws -> [WorkGoalWorkspace] {
        let spaces = ((try await noteTree(workspacesFolder, fetch: fetch)) ?? []).filter { $0.kind == "dir" }
        let walks = try await listGoalFolders(spaces, limits: limits, fetch: fetch)
        let listed = walks.map(goalNotePaths)
        let files: [(space: Int, path: String)] = listed.enumerated().flatMap { space, paths in paths.prefix(limits.notes).map { (space, $0) } }
        let notes = try await inPool(files.map(\.path)) { path in try await readGoalNote(path, fetch: fetch) }
        return spaces.enumerated().map { index, space in
            let own = notes.enumerated().filter { files[$0.offset].space == index }.map(\.element)
            return WorkGoalWorkspace(
                name: space.name, path: space.relativePath,
                goals: own.compactMap(\.goal),
                problems: walks[index].problems + own.compactMap(\.problem),
                notShown: max(0, listed[index].count - limits.notes),
                foldersNotRead: walks[index].foldersNotRead)
        }
    }
}

// MARK: - Marking a goal done

/// One request's status and decoded JSON body (nil when the body is not JSON), for the goal edits.
typealias WorkGoalSend = @Sendable (_ path: String, _ method: String, _ body: WorkJSON) async throws -> (status: Int, value: WorkJSON?)

/// Mark done closes a goal; Reopen makes a closed one active again.
enum WorkGoalStatusChange: String, Equatable, Sendable { case done, active }

enum WorkGoalEditResult: Equatable, Sendable {
    /// The note's new revision, when the server reports it.
    case saved(revision: String?)
    /// The note changed since it was read (409), or was read without a revision.
    case conflict(String)
    case failed(String)
}

/// Changes only the named front-matter keys through `PATCH /notes/front-matter`. The server keeps
/// every other line and the body as they are, and refuses with 409 when the note changed since
/// `goal.revision` was read.
enum WorkGoalWriter {
    static let frontMatterAPI = "\(WorkGoalReader.notesAPI)/front-matter"

    /// Today in the device's time zone, written `YYYY-MM-DD` on the Gregorian calendar, like the web's `localDate`.
    static func localDate(_ now: Date = Date(), timeZone: TimeZone = .current) -> String {
        var calendar = Calendar(identifier: .gregorian); calendar.timeZone = timeZone
        let day = calendar.dateComponents([.year, .month, .day], from: now)
        return String(format: "%04d-%02d-%02d", day.year ?? 0, day.month ?? 0, day.day ?? 0)
    }

    /// Done stamps `closed:` with `today` for the Closed list; reopening sets `active` and removes
    /// `closed:`. `nil` when the goal was read without a revision.
    static func statusBody(_ goal: WorkGoal, to status: WorkGoalStatusChange, today: String) -> WorkJSON? {
        guard let revision = goal.revision, !revision.isEmpty else { return nil }
        var body: [String: WorkJSON] = ["path": .string(goal.path), "expected_revision": .string(revision)]
        switch status {
        case .done: body["set"] = .object(["status": .string("done"), "closed": .string(today)])
        case .active: body["set"] = .object(["status": .string("active")]); body["unset"] = .array([.string("closed")])
        }
        return .object(body)
    }

    /// Only a cancellation is thrown; every other failure is a result the screen words.
    static func updateStatus(_ goal: WorkGoal, to status: WorkGoalStatusChange, today: String = WorkGoalWriter.localDate(), send: WorkGoalSend) async throws -> WorkGoalEditResult {
        guard let body = statusBody(goal, to: status, today: today) else {
            return .conflict("This goal has no saved revision yet. Reload the goals and try again.")
        }
        let what = status == .done ? "mark the goal done" : "reopen the goal"
        do {
            let (code, value) = try await send(frontMatterAPI, "PATCH", body)
            if code == 409 { return .conflict("This goal changed since it was loaded.") }
            guard (200..<300).contains(code) else {
                let message = value?["message"].text ?? ""
                return .failed(message.isEmpty ? "Could not \(what) (\(code))." : message)
            }
            return .saved(revision: value?["revision"].text)
        } catch {
            if Task.isCancelled || error is CancellationError { throw error }
            return .failed(error.localizedDescription)
        }
    }
}

// MARK: - Screen copy

enum WorkGoalCopy {
    static let intro = "Your open goals, the sub-goals under them and who owns each. Mark goals done here; done and dropped goals are listed under Closed. Other changes stay in chat or in the goal's note."
    static let loading = "Loading goals…"
    static let emptyLead = "No goal workspaces yet."
    static let emptyLink = "Ask Magican in chat"
    static let emptyTail = "to set one up from a template."
    static let noGoals = "No goals in this workspace yet."
    static let notLinked = "Not linked"
    static let notLinkedDetail = "These goals, and the sub-goals under them, do not lead up to a main goal."
    static let noOpenGoals = "No open goals in this workspace."
    static let noClosedGoals = "No closed goals in this workspace."
    /// The Goals screen when every workspace's goals are closed, and its Closed list when none are.
    static let noOpenGoalsAnywhere = "No open goals: every goal is done or dropped."
    static let noClosedGoalsAnywhere = "No closed goals yet. A goal marked done or dropped is listed here."
    /// The All work landing when every goal it read is closed.
    static let allClosed = "No open goals: every goal read here is done or dropped."
    static let seeClosed = "See closed goals"
    static let markDone = "Mark done"
    static let reopen = "Reopen"
    static let saving = "Saving…"
    /// Very deep chains stop at 8 levels; the rest are counted.
    static let maxDepth = 8

    static func plural(_ count: Int, _ one: String, _ many: String) -> String { "\(count) \(count == 1 ? one : many)" }
    static func notShown(_ count: Int, limits: WorkGoalReadLimits) -> String {
        "\(plural(count, "more note", "more notes")) in Goals/ not shown: this screen reads the first \(limits.notes) notes of a workspace. Open the notes to see the rest."
    }
    static func foldersNotRead(_ count: Int, limits: WorkGoalReadLimits) -> String {
        "\(plural(count, "folder", "folders")) in Goals/ not read: this screen reads folders up to \(plural(limits.folderDepth, "level", "levels")) deep, and at most \(limits.folders) of them."
    }
    static func problems(_ count: Int) -> String { count == 1 ? "One item could not be read" : "\(count) items could not be read" }
    static func openGoalCount(_ count: Int) -> String { plural(count, "open goal", "open goals") }
    static func closedParent(_ title: String) -> String { "Its parent goal “\(title)” is closed." }
    static func under(_ title: String) -> String { "Under \(title)" }
    static func closedOn(_ date: String) -> String { "Closed \(date)" }
    /// The notice after Mark done or Reopen. A save or a conflict reloads the goals; a failure does not.
    static func statusNotice(_ title: String, _ status: WorkGoalStatusChange, _ result: WorkGoalEditResult) -> String {
        let done = status == .done
        switch result {
        case .saved: return done ? "“\(title)” is marked done. It is listed under Closed." : "“\(title)” is open again."
        case .conflict:
            return "“\(title)” changed since this screen loaded, so it was not \(done ? "marked done" : "reopened"). The goals have been reloaded; try again if it is still needed."
        case .failed(let message): return "Could not \(done ? "mark" : "reopen") “\(title)”\(done ? " done" : ""): \(message)"
        }
    }
    static func deeper(_ count: Int) -> String {
        "\(count) deeper \(count == 1 ? "goal is" : "goals are") not shown here. Open the note to follow them."
    }
}
