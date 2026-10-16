import Foundation

// MARK: - Composer @-mention catalog
//
// Swift port of ui/unified-ui/src/lib/magician/chat/composerMentions.ts. The
// picker logic is pure (group -> needle -> filter -> cap) so it matches the web
// byte-for-byte: selecting a mention inserts the SAME serialized token the web
// chip emits (`agent:foo`, `skill:bar via agent:baz`, `personality:baz`,
// `task:<id>`, or a feature command such as `@tutor` / `@brainstorm` / `@vibedev`),
// which is what the backend parses inline. A feature lane is offered only when iOS can
// either execute it natively (tutor, brainstorm) or the server dispatches it from the
// marker alone (`@vibedev`) — never one that needs a surface iOS does not have.

public enum ComposerMentionKind: String, Codable, Equatable {
    case agent, tool, personality, task, feature
}

public struct ComposerMentionItem: Identifiable, Equatable {
    public let id: String
    public let label: String
    public let detail: String?
    public let kind: ComposerMentionKind
    /// Text inserted into the user turn (the backend routing form).
    public let insertText: String
    public let searchText: String?
    /// For task/feature mentions: the human title shown inside the chip while
    /// `insertText` keeps the precise id/slug.
    public let chipLabel: String?

    public init(id: String, label: String, detail: String? = nil, kind: ComposerMentionKind,
                insertText: String, searchText: String? = nil, chipLabel: String? = nil) {
        self.id = id; self.label = label; self.detail = detail; self.kind = kind
        self.insertText = insertText; self.searchText = searchText; self.chipLabel = chipLabel
    }

    /// The exact text to splice into the composer when this mention is picked —
    /// identical to the web chip serialization so the backend routes the same.
    public var serialized: String {
        switch id {
        case "feature:tutor": return "@tutor"
        case "feature:tutor_quick": return "@tutor #quick"
        case "feature:brainstorm": return "@brainstorm"
        case "feature:vibedev": return "@vibedev"
        // The ASCII space is load-bearing: `#` is a token character in the
        // backend tokenizer, so a glued `@vibedev#discuss` is one unknown token
        // and matches no marker at all.
        case "feature:vibedev_discuss": return "@vibedev #discuss"
        default: return insertText
        }
    }
}

public enum MentionGroup: Equatable {
    case kind(ComposerMentionKind)
    case all
}

// MARK: - State machine (pure)

public func mentionGroupForQuery(_ query: String) -> MentionGroup {
    let n = query.lowercased()
    if n == "agent" || n.hasPrefix("agent:") { return .kind(.agent) }
    if n == "agents" || n.hasPrefix("agents:") { return .kind(.agent) }
    if n == "skill" || n.hasPrefix("skill:") { return .kind(.tool) }
    if n == "skills" || n.hasPrefix("skills:") { return .kind(.tool) }
    if n == "tool" || n.hasPrefix("tool:") { return .kind(.tool) }
    if n == "tools" || n.hasPrefix("tools:") { return .kind(.tool) }
    if n == "personality" || n.hasPrefix("personality:") { return .kind(.personality) }
    if n == "task" || n.hasPrefix("task:") { return .kind(.task) }
    if n == "tasks" || n.hasPrefix("tasks:") { return .kind(.task) }
    if n == "feature" || n.hasPrefix("feature:") { return .kind(.feature) }
    if n == "features" || n.hasPrefix("features:") { return .kind(.feature) }
    return .all
}

public func mentionNeedleForQuery(_ query: String, group: MentionGroup) -> String {
    if group == .all { return query.lowercased() }
    if let idx = query.firstIndex(of: ":") {
        return String(query[query.index(after: idx)...]).lowercased()
    }
    return ""
}

public func filterMentionItems(_ items: [ComposerMentionItem], group: MentionGroup, needle: String) -> [ComposerMentionItem] {
    items.filter { item in
        if case let .kind(k) = group, item.kind != k { return false }
        if needle.isEmpty { return true }
        let haystack = item.searchText ?? "\(item.label) \(item.detail ?? "") \(item.id)"
        return haystack.lowercased().contains(needle)
    }
}

/// group -> needle -> filter -> cap. Single entry point turning a raw `@…` query
/// into ranked matches.
public func mentionMatchesFor(_ items: [ComposerMentionItem], query: String, limit: Int = 9) -> [ComposerMentionItem] {
    let group = mentionGroupForQuery(query)
    let needle = mentionNeedleForQuery(query, group: group)
    return Array(filterMentionItems(items, group: group, needle: needle).prefix(limit))
}

public struct MentionTrigger: Equatable {
    public let consume: Int
    public let query: String
}

/// Detect a trailing `@…` trigger in the text before the caret. iOS composers
/// treat the whole text as "before caret" (caret at end — the common
/// type-to-filter case). Mirrors detectMentionTrigger (chat leaves allowSpaces
/// OFF so prose after a mention closes the picker).
public func detectMentionTrigger(_ beforeCursor: String) -> MentionTrigger? {
    // Command form: `@agent foo`, `@skill bar`, etc.
    let cmd = "(^|\\s)@(agent|agents|skill|skills|tool|tools|personality|task|tasks|feature|features)\\s+([A-Za-z0-9_:-]*)$"
    if let m = firstMatch(cmd, in: beforeCursor, caseInsensitive: true),
       let leadR = Range(m.range(at: 1), in: beforeCursor),
       let kindR = Range(m.range(at: 2), in: beforeCursor) {
        let leadLen = beforeCursor.distance(from: leadR.lowerBound, to: leadR.upperBound)
        let kind = String(beforeCursor[kindR]).lowercased()
        let tail = m.range(at: 3).location != NSNotFound ? (Range(m.range(at: 3), in: beforeCursor).map { String(beforeCursor[$0]) } ?? "") : ""
        let consume = beforeCursor.count - (matchStartOffset(m, in: beforeCursor) + leadLen)
        return MentionTrigger(consume: consume, query: "\(kind):\(tail)")
    }
    // Bare form (no spaces): `@que`
    let bare = "(^|\\s)@([A-Za-z0-9_:-]*)$"
    if let m = firstMatch(bare, in: beforeCursor, caseInsensitive: false) {
        let q = Range(m.range(at: 2), in: beforeCursor).map { String(beforeCursor[$0]) } ?? ""
        return MentionTrigger(consume: q.count + 1, query: q)
    }
    return nil
}

public func mentionKindLabel(_ kind: ComposerMentionKind) -> String {
    switch kind {
    case .agent: return "Agent"
    case .personality: return "Personality"
    case .task: return "Task"
    case .feature: return "Feature"
    case .tool: return "Tool"
    }
}

private func firstMatch(_ pattern: String, in text: String, caseInsensitive: Bool) -> NSTextCheckingResult? {
    var opts: NSRegularExpression.Options = []
    if caseInsensitive { opts.insert(.caseInsensitive) }
    guard let re = try? NSRegularExpression(pattern: pattern, options: opts) else { return nil }
    let range = NSRange(text.startIndex..<text.endIndex, in: text)
    return re.firstMatch(in: text, options: [], range: range)
}

private func matchStartOffset(_ m: NSTextCheckingResult, in text: String) -> Int {
    guard let r = Range(m.range, in: text) else { return 0 }
    return text.distance(from: text.startIndex, to: r.lowerBound)
}

// MARK: - Builders (feature + task)

/// Feature lanes offered by the iOS composer.
public func buildFeatureMentionItems() -> [ComposerMentionItem] {
    [
        ComposerMentionItem(
            id: "feature:tutor", label: "@tutor",
            detail: "Tutor — explains on a blackboard", kind: .feature,
            insertText: "feature:tutor",
            searchText: "tutor @tutor blackboard explain feature", chipLabel: "@tutor"),
        ComposerMentionItem(
            id: "feature:tutor_quick", label: "@tutor_quick",
            detail: "Tutor, quick — fastest first overlay", kind: .feature,
            insertText: "feature:tutor_quick",
            searchText: "tutor quick tutor_quick @tutor #quick fast feature", chipLabel: "@tutor_quick"),
        ComposerMentionItem(
            id: "feature:brainstorm", label: "@brainstorm",
            detail: "Thinking Map — grow an idea on a live canvas", kind: .feature,
            insertText: "feature:brainstorm",
            searchText: "brainstorm brainstrom ideas thinking map canvas weave feature", chipLabel: "@brainstorm"),
        // Unlike the lanes above, nothing on the client intercepts these: the
        // marker rides along in the message text and the server recognises it,
        // so any surface whose text reaches chat gets the rail without growing
        // its own copy of the trigger.
        ComposerMentionItem(
            id: "feature:vibedev", label: "@vibedev",
            detail: "VibeDev — starts a build in your project", kind: .feature,
            insertText: "feature:vibedev",
            searchText: "vibedev @vibedev build implement ship develop feature",
            chipLabel: "@vibedev"),
        ComposerMentionItem(
            id: "feature:vibedev_discuss", label: "@vibedev_discuss",
            detail: "VibeDev, discuss — plans the build without writing it", kind: .feature,
            insertText: "feature:vibedev_discuss",
            searchText: "vibedev discuss vibedev_discuss @vibedev #discuss plan spec design feature",
            chipLabel: "@vibedev_discuss")
    ]
}

// MARK: - Brainstorm lane invocation

/// Composer invocation and seed extraction for the native Thinking Map lane.
/// The common `brainstrom` transposition is accepted, but the chip always emits
/// the canonical `@brainstorm` spelling.
enum BrainstormInvoke {
    private static let pattern = "^\\s*@brain(?:storm|strom)(?=$|[\\s:,])"

    static func isBrainstormInvoke(_ text: String) -> Bool {
        text.range(of: pattern, options: [.regularExpression, .caseInsensitive]) != nil
    }

    /// Strip only a leading invocation. Remaining text becomes the first thought
    /// on the map; an empty result intentionally opens the zero-ceremony capture.
    static func strip(_ text: String) -> String {
        let leading = "^\\s*@brain(?:storm|strom)(?=$|[\\s:,])[\\s,:]*"
        let stripped = text.replacingOccurrences(
            of: leading, with: "", options: [.regularExpression, .caseInsensitive])
        return stripped.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// Task mentions: chip DISPLAYS the title while `insertText` keeps `task:<id>`.
public func buildTaskMentionItems(_ tasks: [(id: String?, title: String?)]) -> [ComposerMentionItem] {
    var items: [ComposerMentionItem] = []
    for task in tasks {
        guard let id = task.id?.trimmingCharacters(in: .whitespaces), !id.isEmpty,
              let title = task.title?.trimmingCharacters(in: .whitespaces), !title.isEmpty else { continue }
        items.append(ComposerMentionItem(
            id: "task:\(id)", label: title, detail: id, kind: .task,
            insertText: "task:\(id)", searchText: "\(title) \(id)", chipLabel: title))
    }
    return items
}

// MARK: - Reference-catalog fetch models (GET .../reference-catalog)

public struct ComposerReferenceAgentEntry: Codable, Equatable {
    public let agentId: String
    public let name: String?
    public let description: String?
    public let route: String?
    enum CodingKeys: String, CodingKey {
        case agentId = "agent_id"
        case name, description, route
    }
}

public struct ComposerSkillEntry: Codable, Equatable {
    public let name: String
    public let description: String?
    public let kind: String?
    public let layer: String?
    public let ownerAgentId: String?
    public let ownerAgentName: String?
    public let route: String?
    enum CodingKeys: String, CodingKey {
        case name, description, kind, layer, route
        case ownerAgentId = "owner_agent_id"
        case ownerAgentName = "owner_agent_name"
    }
}

public struct ReferenceCatalogResponse: Codable, Equatable {
    public let agents: [ComposerReferenceAgentEntry]?
    public let skills: [ComposerSkillEntry]?
}

private func truncateReferenceDetail(_ s: String, limit: Int = 80) -> String {
    if s.count <= limit { return s }
    return String(s.prefix(limit - 1)) + "…"
}

/// Assemble the full composer mention list from a fetched catalog + tasks —
/// features first (so a bare `@` surfaces them), then delegate-scoped agents,
/// then skills/personalities, then task references. Mirrors
/// buildComposerMentionItems in ChatPanel.svelte.
public func buildComposerMentionItems(
    agents: [ComposerReferenceAgentEntry],
    skills: [ComposerSkillEntry],
    tasks: [(id: String?, title: String?)] = []
) -> [ComposerMentionItem] {
    var items: [ComposerMentionItem] = []
    items.append(contentsOf: buildFeatureMentionItems())

    var seen = Set<String>()
    for agent in agents {
        let agentId = agent.agentId.trimmingCharacters(in: .whitespaces)
        if agentId.isEmpty || seen.contains(agentId) { continue }
        seen.insert(agentId)
        let name = agent.name?.trimmingCharacters(in: .whitespaces)
        let routeLabel = agent.route == "self" ? "Current agent" : "Delegate target"
        let detail = truncateReferenceDetail([routeLabel, agent.description].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · "))
        items.append(ComposerMentionItem(
            id: "agent:\(agentId)",
            label: (name?.isEmpty == false) ? "\(name!) · \(agentId)" : agentId,
            detail: detail, kind: .agent, insertText: "agent:\(agentId)",
            searchText: [agentId, name, agent.description, routeLabel].compactMap { $0 }.joined(separator: " ")))
    }

    for skill in skills {
        let name = skill.name.trimmingCharacters(in: .whitespaces)
        if name.isEmpty { continue }
        let isPersonality = skill.kind == "personality-mode"
        let ownerAgentId = skill.ownerAgentId?.trimmingCharacters(in: .whitespaces)
        let ownerAgentName = skill.ownerAgentName?.trimmingCharacters(in: .whitespaces)
        let isDelegated = !isPersonality && (ownerAgentId?.isEmpty == false) && skill.route == "delegate"
        let routeLabel: String? = isDelegated ? "Via \(ownerAgentName?.isEmpty == false ? ownerAgentName! : ownerAgentId!)" : nil
        let fallbackDetail = isPersonality ? "Personality mode" : (skill.kind == "compiled" ? "Compiled tool" : "Procedure skill")
        let itemId = isDelegated ? "tool:\(name):via:\(ownerAgentId!)" : "\(isPersonality ? "personality" : "tool"):\(name)"
        let insert = isPersonality ? "personality:\(name)" : (isDelegated ? "skill:\(name) via agent:\(ownerAgentId!)" : "skill:\(name)")
        items.append(ComposerMentionItem(
            id: itemId, label: name,
            detail: truncateReferenceDetail([routeLabel, skill.description ?? fallbackDetail].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")),
            kind: isPersonality ? .personality : .tool, insertText: insert,
            searchText: [name, skill.description, skill.kind, skill.layer, ownerAgentId, ownerAgentName, routeLabel].compactMap { $0 }.joined(separator: " ")))
    }

    items.append(contentsOf: buildTaskMentionItems(tasks))
    return items
}
