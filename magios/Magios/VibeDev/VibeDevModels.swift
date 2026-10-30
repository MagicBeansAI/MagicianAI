import Foundation

// Wire models for the VibeDev mobile companion. Every shape mirrors what the
// server serializes (`magician/tests/fixtures/vibedev/*.json`); keys are
// spelled out so a server rename fails the fixture tests instead of decoding
// to nothing. Optional fields the server omits (`skip_serializing_if`) decode
// as nil, and enums keep unknown values instead of failing the whole list.

// MARK: - Shared decoding helpers

/// A string enum that keeps a value it does not know.
protocol VibeDevOpenEnum: RawRepresentable, Codable, Hashable where RawValue == String {
    static var fallback: Self { get }
}

extension VibeDevOpenEnum {
    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? Self.fallback
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }
}

enum VibeDevTime {
    private static let fractional: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()
    private static let plain: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter
    }()

    static func parse(_ text: String?) -> Date? {
        guard let text, !text.isEmpty else { return nil }
        return fractional.date(from: text) ?? plain.date(from: text)
    }

    static func date(ms: Int64) -> Date { Date(timeIntervalSince1970: TimeInterval(ms) / 1000) }

    /// "now", "4m", "2h", "3d" — the compact age used on rows.
    static func age(since date: Date?, now: Date = Date()) -> String {
        guard let date else { return "" }
        let seconds = max(0, now.timeIntervalSince(date))
        if seconds < 60 { return "now" }
        let minutes = Int(seconds / 60)
        if minutes < 60 { return "\(minutes)m" }
        let hours = minutes / 60
        if hours < 48 { return "\(hours)h" }
        return "\(hours / 24)d"
    }

    /// "4m 10s" style duration between two instants.
    static func duration(from start: Date?, to end: Date?) -> String {
        guard let start, let end else { return "" }
        let seconds = Int(max(0, end.timeIntervalSince(start)))
        if seconds < 60 { return "\(seconds)s" }
        let minutes = seconds / 60
        if minutes < 60 { return "\(minutes)m" }
        return "\(minutes / 60)h \(minutes % 60)m"
    }
}

enum VibeDevFormat {
    static func usd(_ value: Double?) -> String {
        guard let value else { return "" }
        return String(format: "$%.2f", value)
    }

    /// Run titles arrive as "VibeDev · <prompt>"; the prefix says nothing here.
    static func title(_ raw: String) -> String {
        for prefix in ["VibeDev · ", "VibeDev: ", "VibeDev - "] where raw.hasPrefix(prefix) {
            return String(raw.dropFirst(prefix.count))
        }
        return raw
    }

    static func short(_ text: String, _ limit: Int = 28) -> String {
        text.count > limit ? String(text.prefix(limit - 1)) + "…" : text
    }

    /// The engine id the server reports, as a person would say it.
    static func engine(_ raw: String?) -> String {
        switch raw?.lowercased() {
        case nil, "": return ""
        case "pi": return "Pi"
        case "codex", "codex_app_server": return "Codex"
        case "claude", "claude_code", "claude-code": return "Claude Code"
        case "grok", "grok_acp": return "Grok"
        case "agy", "agy_cli": return "Antigravity"
        default: return raw ?? ""
        }
    }
}

// MARK: - Completion kind on VibeDev wire shapes

/// `completion_kind` that never fails its parent: unknown kinds and other
/// shapes read as nil (see `TaskCompletion`).
struct VibeDevCompletionKind: Codable, Equatable {
    let value: String?
    init(_ value: String?) { self.value = TaskCompletion.kind(value) }
    init(from decoder: Decoder) throws {
        value = TaskCompletion.kind(try? decoder.singleValueContainer().decode(String.self))
    }
    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        try c.encode(value)
    }
}

/// `open_items` that never fails its parent: non-strings are dropped.
struct VibeDevOpenItems: Codable, Equatable {
    let items: [String]
    init(_ items: [String]) { self.items = TaskCompletion.openItems(items) }
    init(from decoder: Decoder) throws { items = TaskCompletion.decodeOpenItems(decoder) }
    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        try c.encode(items)
    }
}

// MARK: - Live runs (`GET /vibedev/runs/live`)

enum VibeDevRunState: String, VibeDevOpenEnum, CaseIterable {
    case needsYou = "needs_you", working, paused, failed, stopped, done
    static var fallback: VibeDevRunState { .working }

    var isLive: Bool { self == .working || self == .paused }
    var isEnded: Bool { self == .failed || self == .stopped || self == .done }

    var label: String {
        switch self {
        case .needsYou: return "Needs you"
        case .working: return "Working"
        case .paused: return "Paused"
        case .failed: return "Failed"
        case .stopped: return "Stopped"
        case .done: return "Done"
        }
    }

    /// A task status (`running`, `completed`, …) as a run state.
    static func fromTaskStatus(_ status: String) -> VibeDevRunState {
        switch status.lowercased() {
        case "completed", "succeeded", "done": return .done
        case "failed", "error": return .failed
        case "cancelled", "canceled", "archived", "stopped": return .stopped
        case "paused": return .paused
        case "waiting_for_input", "needs_input", "blocked", "awaiting_approval": return .needsYou
        default: return .working
        }
    }
}

/// What a run waits on the owner for (`waiting_kind`). Only a diff review or
/// a decision makes a run `needs_you`; a notice (an owner briefing) rides a
/// run in whatever state it is.
enum VibeDevWaitingKind: String, VibeDevOpenEnum {
    case diffReview = "diff_review", decision, notice, unknown
    static var fallback: VibeDevWaitingKind { .unknown }
}

/// An acknowledge-only owner briefing on a run (`notice`). Read leniently: a
/// malformed one decodes empty instead of failing the run.
struct VibeDevRunNotice: Codable, Equatable {
    var requestId: String = ""
    var optionId: String = "acknowledge"
    var title: String? = nil
    var message: String = ""
    var severity: String? = nil

    enum CodingKeys: String, CodingKey {
        case requestId = "request_id"
        case optionId = "option_id"
        case title, message, severity
    }

    init(requestId: String, optionId: String = "acknowledge", title: String? = nil,
         message: String, severity: String? = nil) {
        self.requestId = requestId
        self.optionId = optionId
        self.title = title
        self.message = message
        self.severity = severity
    }

    init(from decoder: Decoder) throws {
        guard let c = try? decoder.container(keyedBy: CodingKeys.self) else { return }
        requestId = (try? c.decodeIfPresent(String.self, forKey: .requestId)) ?? ""
        optionId = (try? c.decodeIfPresent(String.self, forKey: .optionId)) ?? "acknowledge"
        title = try? c.decodeIfPresent(String.self, forKey: .title)
        message = (try? c.decodeIfPresent(String.self, forKey: .message)) ?? ""
        severity = try? c.decodeIfPresent(String.self, forKey: .severity)
    }
}

struct VibeDevLiveRun: Codable, Identifiable, Equatable {
    let runId: String
    let liveTaskId: String
    let projectId: String
    let projectName: String
    let title: String
    let latestTurnTitle: String?
    let state: VibeDevRunState
    let status: String
    let step: String?
    let summary: String?
    let turns: Int
    /// The run root's creation time (absent on older servers).
    var startedAt: String? = nil
    /// Spend so far, present only when the run is metered.
    var costUsd: Double? = nil
    /// A needs-you run waiting on a diff approval: the proposal id the
    /// approval answers (`ccp-…`).
    var approvalId: String? = nil
    /// `diff_review` / `decision` on a needs-you run, `notice` when only a
    /// briefing waits; absent from older servers.
    var waitingKind: VibeDevWaitingKind? = nil
    var notice: VibeDevRunNotice? = nil
    let crewId: String?
    let lane: String?
    let updatedAt: String
    let lastProgressAt: String?
    /// Ended runs only: `partial` when requirements were left open.
    var completionKind: VibeDevCompletionKind? = nil
    var openItemsWire: VibeDevOpenItems? = nil
    var needsYouCount: Int? = nil
    var workLocation: String? = nil
    var repoPath: String? = nil

    var id: String { runId }
    /// Ended done with requirements still open.
    var isPartlyDone: Bool { state == .done && completionKind?.value == "partial" }
    var openItems: [String] { openItemsWire?.items ?? [] }
    /// "Partly done" instead of "Done" when requirements are open.
    var stateLabel: String { isPartlyDone ? TaskCompletion.partlyDoneLabel : state.label }
    var openItemLine: String? { isPartlyDone ? TaskCompletion.openLine(openItems) : nil }
    var startedDate: Date? { VibeDevTime.parse(startedAt) }
    /// Working rows show how long the run has gone; others when it last moved.
    var rowTime: String {
        if state.isLive || state == .needsYou, let started = startedDate {
            return VibeDevTime.age(since: started).replacingOccurrences(of: "now", with: "0m")
        }
        if state.isEnded, let costUsd { return VibeDevFormat.usd(costUsd) }
        return VibeDevTime.age(since: updatedDate)
    }
    var displayTitle: String { VibeDevFormat.title(title) }
    var updatedDate: Date? { VibeDevTime.parse(lastProgressAt) ?? VibeDevTime.parse(updatedAt) }
    /// "API lane" for a crew lane, else the short title.
    var shortName: String { lane.map { "\($0) lane" } ?? VibeDevFormat.short(displayTitle) }

    enum CodingKeys: String, CodingKey {
        case runId = "run_id"
        case liveTaskId = "live_task_id"
        case projectId = "project_id"
        case projectName = "project_name"
        case title
        case latestTurnTitle = "latest_turn_title"
        case state, status, step, summary, turns
        case startedAt = "started_at"
        case costUsd = "cost_usd"
        case approvalId = "approval_id"
        case waitingKind = "waiting_kind"
        case notice
        case crewId = "crew_id"
        case lane
        case updatedAt = "updated_at"
        case lastProgressAt = "last_progress_at"
        case completionKind = "completion_kind"
        case openItemsWire = "open_items"
        case needsYouCount = "needs_you_count"
        case workLocation = "work_location"
        case repoPath = "repo_path"
    }
}

struct VibeDevRunHistory: Codable, Equatable {
    let runs: [VibeDevLiveRun]
    let hasMore: Bool
    let nextCursor: String?
    enum CodingKeys: String, CodingKey {
        case runs
        case hasMore = "has_more"
        case nextCursor = "next_cursor"
    }
}

struct VibeDevLiveRunsResponse: Codable, Equatable {
    let runs: [VibeDevLiveRun]
    let counts: [String: Int]

    enum CodingKeys: String, CodingKey { case runs, counts }

    init(runs: [VibeDevLiveRun], counts: [String: Int] = [:]) {
        self.runs = runs
        self.counts = counts
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        runs = try c.decode([VibeDevLiveRun].self, forKey: .runs)
        counts = try c.decodeIfPresent([String: Int].self, forKey: .counts) ?? [:]
    }
}

// MARK: - Crews

enum VibeDevCrewStatus: String, VibeDevOpenEnum {
    case proposed, running, choosing, ready, integrated, conflict, failed, cancelled
    static var fallback: VibeDevCrewStatus { .running }

    var label: String {
        switch self {
        case .proposed: return "Waiting to start"
        case .running: return "Running"
        case .choosing: return "Pick an attempt"
        case .ready: return "Merging"
        case .integrated: return "Merged"
        case .conflict: return "Lanes conflict"
        case .failed: return "Failed"
        case .cancelled: return "Stopped"
        }
    }
}

enum VibeDevCrewMode: String, VibeDevOpenEnum {
    case split, race
    static var fallback: VibeDevCrewMode { .split }
}

enum VibeDevLaneStatus: String, VibeDevOpenEnum {
    case pending, running, done, failed, stopped
    static var fallback: VibeDevLaneStatus { .running }

    var label: String {
        switch self {
        case .pending: return "Waiting"
        case .running: return "Running"
        case .done: return "Done"
        case .failed: return "Failed"
        case .stopped: return "Stopped"
        }
    }
}

enum VibeDevLaneRole: String, VibeDevOpenEnum {
    case work, integrate, review
    static var fallback: VibeDevLaneRole { .work }

    var label: String? {
        switch self {
        case .work: return nil
        case .integrate: return "Integrator"
        case .review: return "Reviewer"
        }
    }
}

struct VibeDevCrewLane: Codable, Equatable {
    let lane: String
    let brief: String
    let owns: [String]
    let codingProfileId: String?
    let runTaskId: String?
    let status: VibeDevLaneStatus
    let role: VibeDevLaneRole

    enum CodingKeys: String, CodingKey {
        case lane, brief, owns
        case codingProfileId = "coding_profile_id"
        case runTaskId = "run_task_id"
        case status, role
    }

    init(lane: String, brief: String = "", owns: [String] = [], codingProfileId: String? = nil,
         runTaskId: String? = nil, status: VibeDevLaneStatus = .pending, role: VibeDevLaneRole = .work) {
        self.lane = lane; self.brief = brief; self.owns = owns; self.codingProfileId = codingProfileId
        self.runTaskId = runTaskId; self.status = status; self.role = role
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        lane = try c.decode(String.self, forKey: .lane)
        brief = try c.decodeIfPresent(String.self, forKey: .brief) ?? ""
        owns = try c.decodeIfPresent([String].self, forKey: .owns) ?? []
        codingProfileId = try c.decodeIfPresent(String.self, forKey: .codingProfileId)
        runTaskId = try c.decodeIfPresent(String.self, forKey: .runTaskId)
        status = try c.decodeIfPresent(VibeDevLaneStatus.self, forKey: .status) ?? .pending
        role = try c.decodeIfPresent(VibeDevLaneRole.self, forKey: .role) ?? .work
    }
}

struct VibeDevCrewConflict: Codable, Equatable {
    let lane: String
    let files: [String]
}

struct VibeDevCrew: Codable, Identifiable, Equatable {
    let crewId: String
    let goal: String
    let status: VibeDevCrewStatus
    let mode: VibeDevCrewMode
    let lanes: [VibeDevCrewLane]
    let review: Bool
    let winner: Int?
    let budgetUsd: Double?
    let spentUsd: Double?
    let startRequestId: String?
    let chooseRequestId: String?
    let resolveRequestId: String?
    let pullRequestRequestId: String?
    let branch: String?
    let conflict: VibeDevCrewConflict?
    let pullRequestUrl: String?
    let note: String?
    let createdAtMs: Int64
    let updatedAtMs: Int64

    var id: String { crewId }

    /// Nothing more happens without the owner, and nothing is asked.
    var isFinished: Bool {
        switch status {
        case .cancelled, .failed: return true
        case .integrated: return pullRequestRequestId == nil
        case .conflict: return resolveRequestId == nil
        default: return false
        }
    }

    enum CodingKeys: String, CodingKey {
        case crewId = "crew_id"
        case goal, status, mode, lanes, review, winner
        case budgetUsd = "budget_usd"
        case spentUsd = "spent_usd"
        case startRequestId = "start_request_id"
        case chooseRequestId = "choose_request_id"
        case resolveRequestId = "resolve_request_id"
        case pullRequestRequestId = "pull_request_request_id"
        case branch, conflict
        case pullRequestUrl = "pull_request_url"
        case note
        case createdAtMs = "created_at_ms"
        case updatedAtMs = "updated_at_ms"
    }

    init(crewId: String, goal: String, status: VibeDevCrewStatus, mode: VibeDevCrewMode = .split,
         lanes: [VibeDevCrewLane] = [], review: Bool = false, winner: Int? = nil,
         budgetUsd: Double? = nil, spentUsd: Double? = nil, startRequestId: String? = nil,
         chooseRequestId: String? = nil, resolveRequestId: String? = nil,
         pullRequestRequestId: String? = nil, branch: String? = nil,
         conflict: VibeDevCrewConflict? = nil, pullRequestUrl: String? = nil, note: String? = nil,
         createdAtMs: Int64 = 0, updatedAtMs: Int64 = 0) {
        self.crewId = crewId; self.goal = goal; self.status = status; self.mode = mode
        self.lanes = lanes; self.review = review; self.winner = winner; self.budgetUsd = budgetUsd
        self.spentUsd = spentUsd; self.startRequestId = startRequestId
        self.chooseRequestId = chooseRequestId; self.resolveRequestId = resolveRequestId
        self.pullRequestRequestId = pullRequestRequestId; self.branch = branch
        self.conflict = conflict; self.pullRequestUrl = pullRequestUrl; self.note = note
        self.createdAtMs = createdAtMs; self.updatedAtMs = updatedAtMs
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        crewId = try c.decode(String.self, forKey: .crewId)
        goal = try c.decode(String.self, forKey: .goal)
        status = try c.decodeIfPresent(VibeDevCrewStatus.self, forKey: .status) ?? .proposed
        mode = try c.decodeIfPresent(VibeDevCrewMode.self, forKey: .mode) ?? .split
        lanes = try c.decode([VibeDevCrewLane].self, forKey: .lanes)
        review = try c.decodeIfPresent(Bool.self, forKey: .review) ?? false
        winner = try c.decodeIfPresent(Int.self, forKey: .winner)
        budgetUsd = try c.decodeIfPresent(Double.self, forKey: .budgetUsd)
        spentUsd = try c.decodeIfPresent(Double.self, forKey: .spentUsd)
        startRequestId = try c.decodeIfPresent(String.self, forKey: .startRequestId)
        chooseRequestId = try c.decodeIfPresent(String.self, forKey: .chooseRequestId)
        resolveRequestId = try c.decodeIfPresent(String.self, forKey: .resolveRequestId)
        pullRequestRequestId = try c.decodeIfPresent(String.self, forKey: .pullRequestRequestId)
        branch = try c.decodeIfPresent(String.self, forKey: .branch)
        conflict = try c.decodeIfPresent(VibeDevCrewConflict.self, forKey: .conflict)
        pullRequestUrl = try c.decodeIfPresent(String.self, forKey: .pullRequestUrl)
        note = try c.decodeIfPresent(String.self, forKey: .note)
        createdAtMs = try c.decode(Int64.self, forKey: .createdAtMs)
        updatedAtMs = try c.decode(Int64.self, forKey: .updatedAtMs)
    }
}

struct VibeDevCrewsResponse: Codable, Equatable {
    let crews: [VibeDevCrew]
}

struct VibeDevCrewEnvelope: Codable, Equatable {
    let crew: VibeDevCrew
}

// MARK: - Projects (`GET /vibedev/projects`)

enum VibeDevTrust: String, VibeDevOpenEnum {
    case sandboxed, trusted, original
    static var fallback: VibeDevTrust { .sandboxed }
}

struct VibeDevRunLink: Codable, Equatable {
    let parentTaskId: String?
    let crewId: String?
    let lane: String?

    enum CodingKeys: String, CodingKey {
        case parentTaskId = "parent_task_id"
        case crewId = "crew_id"
        case lane
    }
}

// MARK: - Project health

/// A fix the server offers for a health issue (`git_init` today). Kinds the
/// app does not know get no button.
struct VibeDevHealthAction: Codable, Equatable {
    static let gitInitKind = "git_init"
    let kind: String
    let label: String

    var isGitInit: Bool { kind == Self.gitInitKind }
}

/// One problem with a project (`folder_missing`, `not_a_git_repository`, …).
/// The code is opaque; `title` (when present) and `message` are what the
/// owner reads.
struct VibeDevProjectHealthIssue: Codable, Equatable {
    let code: String
    /// `warning` | `error`; anything else reads as a warning.
    let severity: String
    /// Short headline ("Folder is missing"); absent on older servers.
    var title: String? = nil
    let message: String
    var action: VibeDevHealthAction? = nil

    init(code: String, severity: String, title: String? = nil, message: String, action: VibeDevHealthAction? = nil) {
        self.code = code; self.severity = severity; self.title = title; self.message = message; self.action = action
    }

    var isError: Bool { severity.lowercased() == "error" }

    /// The `git_init` action, when this issue offers it.
    var gitInitAction: VibeDevHealthAction? { action?.isGitInit == true ? action : nil }
}

/// `POST /vibedev/projects/{id}/git-init` answers `{ project_id, health }`.
struct VibeDevGitInitResponse: Decodable, Equatable {
    let projectId: String
    let health: VibeDevProjectHealth

    enum CodingKeys: String, CodingKey { case projectId = "project_id", health }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        projectId = ((try? c.decodeIfPresent(String.self, forKey: .projectId)) ?? nil) ?? ""
        health = ((try? c.decodeIfPresent(VibeDevProjectHealth.self, forKey: .health)) ?? nil) ?? .ok
    }
}

/// The confirmation before `git init` (web wording).
enum VibeDevGitInitCopy {
    static let title = "Make this folder a git repository?"
    static let message = "Magician runs git init in the project folder and commits its current files as a first commit on the base branch (main if none is set). Branch runs, merge, pull requests and rewind then work for this trusted project."
    static let confirm = "Make it a git repository"
}

/// `project.health`: absent or unreadable is `ok`. A project with an error
/// issue cannot start runs (the server answers 409
/// `vibedev_project_unhealthy`).
struct VibeDevProjectHealth: Codable, Equatable {
    enum Level: String { case ok, warning, error }

    let level: Level
    let issues: [VibeDevProjectHealthIssue]

    static let ok = VibeDevProjectHealth(level: .ok, issues: [])

    init(level: Level, issues: [VibeDevProjectHealthIssue] = []) {
        self.level = level; self.issues = issues
    }

    /// The wire `status`, or (unknown/missing) the worst issue severity.
    init(status: String?, issues: [VibeDevProjectHealthIssue]) {
        self.issues = issues
        if let known = status.flatMap({ Level(rawValue: $0.trimmingCharacters(in: .whitespaces).lowercased()) }) {
            level = known
        } else if issues.contains(where: \.isError) {
            level = .error
        } else {
            level = issues.isEmpty ? .ok : .warning
        }
    }

    private enum CodingKeys: String, CodingKey { case status, issues }
    private struct LenientIssue: Decodable {
        let issue: VibeDevProjectHealthIssue?
        private enum CodingKeys: String, CodingKey { case code, severity, title, message, action }
        private enum ActionKeys: String, CodingKey { case kind, label }
        init(from decoder: Decoder) throws {
            let c = try? decoder.container(keyedBy: CodingKeys.self)
            func text(_ key: CodingKeys) -> String? {
                let value = ((try? c?.decodeIfPresent(String.self, forKey: key)) ?? nil)?
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                return value?.isEmpty == false ? value : nil
            }
            var action: VibeDevHealthAction?
            if let a = try? c?.nestedContainer(keyedBy: ActionKeys.self, forKey: .action),
               let kind = ((try? a.decodeIfPresent(String.self, forKey: .kind)) ?? nil)?
                   .trimmingCharacters(in: .whitespacesAndNewlines), !kind.isEmpty,
               let label = ((try? a.decodeIfPresent(String.self, forKey: .label)) ?? nil)?
                   .trimmingCharacters(in: .whitespacesAndNewlines), !label.isEmpty {
                action = VibeDevHealthAction(kind: kind, label: label)
            }
            let title = text(.title)
            // An issue needs something to show: its message, else its title.
            guard let message = text(.message) ?? title else { issue = nil; return }
            issue = VibeDevProjectHealthIssue(
                code: text(.code) ?? "", severity: text(.severity) ?? "warning",
                title: title, message: message, action: action)
        }
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let status = (try? c.decodeIfPresent(String.self, forKey: .status)) ?? nil
        let issues = ((try? c.decodeIfPresent([LenientIssue].self, forKey: .issues)) ?? nil)?.compactMap(\.issue) ?? []
        self.init(status: status, issues: issues)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(level.rawValue, forKey: .status)
        if !issues.isEmpty { try c.encode(issues, forKey: .issues) }
    }

    var needsAttention: Bool { level != .ok }

    /// Web wording (`StudioRail.svelte`); nil when the project is fine.
    var title: String? {
        switch level {
        case .ok: return nil
        case .warning: return "Project needs attention."
        case .error: return "Project needs attention: runs cannot start."
        }
    }

    var messages: [String] { issues.map(\.message) }
}

struct VibeDevProject: Codable, Identifiable, Equatable {
    let projectId: String
    let name: String
    let repoPath: String?
    /// Absent on the wire means sandboxed.
    let trust: VibeDevTrust
    let baseBranch: String?
    let runTaskIds: [String]
    let runLinks: [String: VibeDevRunLink]
    let crews: [VibeDevCrew]
    let archived: Bool
    let repoDisplayPath: String
    let repoAbsolutePath: String
    let chatSessionStatus: String
    var chatSessionId: String? = nil
    let createdAtMs: Int64
    let updatedAtMs: Int64
    /// Absent on the wire (older servers) means ok.
    var health: VibeDevProjectHealth

    var id: String { projectId }
    var isTrusted: Bool { trust == .trusted }
    var repoLabel: String {
        if !repoDisplayPath.isEmpty { return repoDisplayPath }
        return repoPath ?? ""
    }

    enum CodingKeys: String, CodingKey {
        case projectId = "project_id"
        case name
        case repoPath = "repo_path"
        case trust
        case baseBranch = "base_branch"
        case runTaskIds = "run_task_ids"
        case runLinks = "run_links"
        case crews, archived
        case repoDisplayPath = "repo_display_path"
        case repoAbsolutePath = "repo_absolute_path"
        case chatSessionId = "chat_session_id"
        case chatSessionStatus = "chat_session_status"
        case createdAtMs = "created_at_ms"
        case updatedAtMs = "updated_at_ms"
        case health
    }

    init(projectId: String, name: String, repoPath: String? = nil, trust: VibeDevTrust = .sandboxed,
         baseBranch: String? = nil, runTaskIds: [String] = [], runLinks: [String: VibeDevRunLink] = [:],
         crews: [VibeDevCrew] = [], archived: Bool = false, repoDisplayPath: String = "",
         repoAbsolutePath: String = "", chatSessionStatus: String = "open",
         createdAtMs: Int64 = 0, updatedAtMs: Int64 = 0, health: VibeDevProjectHealth = .ok) {
        self.health = health
        self.projectId = projectId; self.name = name; self.repoPath = repoPath; self.trust = trust
        self.baseBranch = baseBranch; self.runTaskIds = runTaskIds; self.runLinks = runLinks
        self.crews = crews; self.archived = archived; self.repoDisplayPath = repoDisplayPath
        self.repoAbsolutePath = repoAbsolutePath; self.chatSessionStatus = chatSessionStatus
        self.createdAtMs = createdAtMs; self.updatedAtMs = updatedAtMs
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        projectId = try c.decode(String.self, forKey: .projectId)
        name = try c.decode(String.self, forKey: .name)
        repoPath = try c.decodeIfPresent(String.self, forKey: .repoPath)
        trust = try c.decodeIfPresent(VibeDevTrust.self, forKey: .trust) ?? .sandboxed
        baseBranch = try c.decodeIfPresent(String.self, forKey: .baseBranch)
        runTaskIds = try c.decodeIfPresent([String].self, forKey: .runTaskIds) ?? []
        runLinks = try c.decodeIfPresent([String: VibeDevRunLink].self, forKey: .runLinks) ?? [:]
        crews = try c.decodeIfPresent([VibeDevCrew].self, forKey: .crews) ?? []
        archived = try c.decodeIfPresent(Bool.self, forKey: .archived) ?? false
        repoDisplayPath = try c.decodeIfPresent(String.self, forKey: .repoDisplayPath) ?? ""
        repoAbsolutePath = try c.decodeIfPresent(String.self, forKey: .repoAbsolutePath) ?? ""
        chatSessionId = try c.decodeIfPresent(String.self, forKey: .chatSessionId)
        chatSessionStatus = try c.decodeIfPresent(String.self, forKey: .chatSessionStatus) ?? ""
        createdAtMs = try c.decodeIfPresent(Int64.self, forKey: .createdAtMs) ?? 0
        updatedAtMs = try c.decodeIfPresent(Int64.self, forKey: .updatedAtMs) ?? 0
        health = ((try? c.decodeIfPresent(VibeDevProjectHealth.self, forKey: .health)) ?? nil) ?? .ok
    }
}

struct VibeDevProjectsResponse: Codable, Equatable {
    let projects: [VibeDevProject]
    let activeProjectId: String?

    enum CodingKeys: String, CodingKey {
        case projects
        case activeProjectId = "active_project_id"
    }
}

/// `POST /vibedev/projects` and `PATCH /vibedev/projects/{id}` answer `{project, session}`.
struct VibeDevProjectMutation: Decodable {
    let project: VibeDevProject
}

// MARK: - Runs page (`GET /vibedev/projects/{id}/runs`)

struct VibeDevTaskTag: Codable, Equatable {
    let name: String
}

/// The fields of `TaskListItemV3` the run list reads.
struct VibeDevTaskItem: Codable, Identifiable, Equatable {
    let id: String
    let title: String
    let description: String
    let status: String
    let tags: [VibeDevTaskTag]
    let createdAt: String
    let updatedAt: String
    let completionSummary: String?
    /// `full` / `partial` once completed; unknown values read as absent.
    let completionKind: String?
    /// What a partial completion left undone.
    let openItems: [String]

    var isThreaded: Bool { tags.contains { $0.name == "vibedev-threaded" } }
    /// Completed with requirements still open.
    var isPartlyDone: Bool { TaskCompletion.isPartlyDone(status: status, kind: completionKind) }

    /// `Parent task: <id>` from the server-written description.
    var parentFromDescription: String? {
        for line in description.split(separator: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("Parent task:") {
                let value = trimmed.dropFirst("Parent task:".count).trimmingCharacters(in: .whitespaces)
                return value.isEmpty ? nil : value
            }
        }
        return nil
    }

    enum CodingKeys: String, CodingKey {
        case id, title, description, status, tags
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case completionSummary = "completion_summary"
        case completionKind = "completion_kind"
        case openItems = "open_items"
    }

    init(id: String, title: String, description: String = "", status: String, tags: [String] = [],
         createdAt: String = "", updatedAt: String = "", completionSummary: String? = nil,
         completionKind: String? = nil, openItems: [String] = []) {
        self.id = id; self.title = title; self.description = description; self.status = status
        self.tags = tags.map(VibeDevTaskTag.init(name:)); self.createdAt = createdAt
        self.updatedAt = updatedAt; self.completionSummary = completionSummary
        self.completionKind = TaskCompletion.kind(completionKind)
        self.openItems = TaskCompletion.openItems(openItems)
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        description = try c.decodeIfPresent(String.self, forKey: .description) ?? ""
        status = try c.decode(String.self, forKey: .status)
        tags = try c.decodeIfPresent([VibeDevTaskTag].self, forKey: .tags) ?? []
        createdAt = try c.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        updatedAt = try c.decodeIfPresent(String.self, forKey: .updatedAt) ?? ""
        completionSummary = try c.decodeIfPresent(String.self, forKey: .completionSummary)
        let completion = TaskCompletion.decode(c, kind: .completionKind, items: .openItems)
        completionKind = completion.kind
        openItems = completion.openItems
    }
}

struct VibeDevRunsPagination: Codable, Equatable {
    let runs: Int
    let totalRuns: Int
    let offset: Int
    let hasMore: Bool
    let nextCursor: String?

    enum CodingKeys: String, CodingKey {
        case runs
        case totalRuns = "total_runs"
        case offset
        case hasMore = "has_more"
        case nextCursor = "next_cursor"
    }
}

/// One run on the project list: its root task and how many turns it has.
struct VibeDevRunRow: Identifiable, Equatable {
    let root: VibeDevTaskItem
    let turns: [VibeDevTaskItem]
    let link: VibeDevRunLink?

    var id: String { root.id }
    var turnCount: Int { turns.count + 1 }
    var latest: VibeDevTaskItem { turns.last ?? root }
    var state: VibeDevRunState { VibeDevRunState.fromTaskStatus(latest.status) }
    /// The latest turn completed with requirements still open.
    var isPartlyDone: Bool { state == .done && latest.isPartlyDone }
    /// The state word for the row: "Partly done" instead of "Done" when open
    /// requirements remain.
    var stateLabel: String { isPartlyDone ? TaskCompletion.partlyDoneLabel : state.label }
    /// "Open: <first open item>" for a partly-done run.
    var openItemLine: String? { isPartlyDone ? TaskCompletion.openLine(latest.openItems) : nil }
    var title: String { VibeDevFormat.title(root.title) }
    var updatedDate: Date? { VibeDevTime.parse(latest.updatedAt) ?? VibeDevTime.parse(root.updatedAt) }
}

struct VibeDevRunsPage: Codable, Equatable {
    let tasks: [VibeDevTaskItem]
    let links: [String: VibeDevRunLink]
    let pagination: VibeDevRunsPagination

    enum CodingKeys: String, CodingKey { case tasks, links, pagination }

    init(tasks: [VibeDevTaskItem], links: [String: VibeDevRunLink] = [:], pagination: VibeDevRunsPagination) {
        self.tasks = tasks; self.links = links; self.pagination = pagination
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        tasks = try c.decode([VibeDevTaskItem].self, forKey: .tasks)
        links = try c.decodeIfPresent([String: VibeDevRunLink].self, forKey: .links) ?? [:]
        pagination = try c.decode(VibeDevRunsPagination.self, forKey: .pagination)
    }

    /// Roots as rows, each with the threaded turns folded into it (the
    /// server sends each root followed by its turns; a turn's parent is its
    /// typed link, else its `Parent task:` line).
    var rows: [VibeDevRunRow] {
        let byID = Dictionary(tasks.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        func parent(of task: VibeDevTaskItem) -> String? {
            links[task.id]?.parentTaskId ?? task.parentFromDescription
        }
        func root(of task: VibeDevTaskItem) -> String {
            var current = task
            var seen: Set<String> = []
            while current.isThreaded, seen.insert(current.id).inserted,
                  let parentID = parent(of: current), let next = byID[parentID] {
                current = next
            }
            return current.id
        }
        var order: [String] = []
        var turns: [String: [VibeDevTaskItem]] = [:]
        for task in tasks {
            let rootID = root(of: task)
            if rootID == task.id {
                if turns[rootID] == nil { order.append(rootID); turns[rootID] = [] }
            } else {
                if turns[rootID] == nil { order.append(rootID); turns[rootID] = [] }
                turns[rootID, default: []].append(task)
            }
        }
        return order.compactMap { id in
            guard let rootTask = byID[id] else { return nil }
            return VibeDevRunRow(root: rootTask, turns: turns[id] ?? [], link: links[id])
        }
    }
}

// MARK: - Run turns (`GET /vibedev/runs/{id}/turns`)

enum VibeDevTurnMode: String, VibeDevOpenEnum {
    case build, discuss, autopilot
    static var fallback: VibeDevTurnMode { .build }
}

struct VibeDevRunTurn: Codable, Identifiable, Equatable {
    let taskId: String
    let prompt: String
    let mode: VibeDevTurnMode
    let status: String
    let createdAt: String
    let updatedAt: String
    let summary: String?
    let engine: String?
    var codingRouteId: String? = nil
    let costUsd: Double?
    let commit: String?
    let filesChanged: Int?
    /// `partial` when the turn completed with requirements left open.
    var completionKind: VibeDevCompletionKind? = nil
    var openItemsWire: VibeDevOpenItems? = nil

    var id: String { taskId }
    var state: VibeDevRunState { VibeDevRunState.fromTaskStatus(status) }
    var isPartlyDone: Bool { TaskCompletion.isPartlyDone(status: status, kind: completionKind?.value) }
    var openItems: [String] { openItemsWire?.items ?? [] }
    var stateLabel: String { isPartlyDone ? TaskCompletion.partlyDoneLabel : state.label }
    var duration: String {
        VibeDevTime.duration(from: VibeDevTime.parse(createdAt),
                             to: state.isEnded ? VibeDevTime.parse(updatedAt) : Date())
    }

    enum CodingKeys: String, CodingKey {
        case taskId = "task_id"
        case prompt, mode, status
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case summary, engine
        case codingRouteId = "coding_route_id"
        case costUsd = "cost_usd"
        case commit
        case filesChanged = "files_changed"
        case completionKind = "completion_kind"
        case openItemsWire = "open_items"
    }
}

struct VibeDevRunTurns: Codable, Equatable {
    let runId: String
    let projectId: String
    let projectName: String
    let title: String
    let status: String
    var workLocation: String? = nil
    let trusted: Bool
    let branch: String?
    let crewId: String?
    let lane: String?
    let turns: [VibeDevRunTurn]

    var displayTitle: String { VibeDevFormat.title(title) }
    var totalCost: Double? {
        let costs = turns.compactMap(\.costUsd)
        return costs.isEmpty ? nil : costs.reduce(0, +)
    }
    var engine: String { VibeDevFormat.engine(turns.last(where: { $0.engine != nil })?.engine) }
    /// The newest turn: a follow-up threads onto it.
    var latestTurnId: String { turns.last?.taskId ?? runId }

    enum CodingKeys: String, CodingKey {
        case runId = "run_id"
        case projectId = "project_id"
        case projectName = "project_name"
        case workLocation = "work_location"
        case title, status, trusted, branch
        case crewId = "crew_id"
        case lane, turns
    }
}

// MARK: - Turn timeline (`GET /vibedev/runs/{turn}/timeline`)

enum VibeDevTimelineKind: String, VibeDevOpenEnum {
    case message, reasoning, action, error, completed, notice
    static var fallback: VibeDevTimelineKind { .notice }
}

struct VibeDevTimelineCard: Codable, Identifiable, Equatable {
    let index: Int
    let kind: VibeDevTimelineKind
    let status: String?
    let title: String
    let text: String?
    let atMs: Int64

    var id: Int { index }

    enum CodingKeys: String, CodingKey {
        case index, kind, status, title, text
        case atMs = "at_ms"
    }
}

struct VibeDevTimeline: Codable, Equatable {
    let taskId: String
    let cards: [VibeDevTimelineCard]
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case taskId = "task_id"
        case cards, truncated
    }
}

// MARK: - Changes (`GET /vibedev/runs/{id}/changes`)

struct VibeDevChangedFile: Codable, Identifiable, Equatable, Hashable {
    let path: String
    /// Git's letter: `A`, `M`, `D`, `R`.
    let status: String
    let additions: Int
    let deletions: Int

    var id: String { path }
    var fileName: String { path.split(separator: "/").last.map(String.init) ?? path }
}

struct VibeDevFilePatch: Codable, Equatable {
    let path: String
    let unifiedDiff: String
    let truncated: Bool

    enum CodingKeys: String, CodingKey {
        case path
        case unifiedDiff = "unified_diff"
        case truncated
    }
}

struct VibeDevRunChanges: Codable, Equatable {
    let runId: String
    let turn: String?
    /// `branch` (trusted) or `proposals` (sandboxed).
    let source: String
    let files: [VibeDevChangedFile]
    let patch: VibeDevFilePatch?

    enum CodingKeys: String, CodingKey {
        case runId = "run_id"
        case turn, source, files, patch
    }
}

// MARK: - Repo suggestions, coding profiles

struct VibeDevRepoSuggestion: Codable, Identifiable, Equatable {
    let path: String
    let name: String
    let project: String?
    var id: String { path }
}

struct VibeDevRepoSuggestions: Codable, Equatable {
    let repos: [VibeDevRepoSuggestion]
}

struct VibeDevCodingProfile: Codable, Identifiable, Equatable {
    let id: String
    let label: String
    let engine: String?
    let selectable: Bool
    let isDefault: Bool
    let readiness: String?
    var modelSelection: ChatModelSelection? = nil
    var modelChoice: ChatModelChoice? = nil
    /// The engine's `credentials` (`login`, `magician_key`, `none`).
    var credentials: String? = nil

    enum CodingKeys: String, CodingKey {
        case id, label, engine, selectable, readiness, credentials
        case isDefault = "is_default"
        case modelSelection = "model_selection"
        case modelChoice = "model_choice"
    }

    init(id: String, label: String, engine: String? = nil, selectable: Bool = true,
         isDefault: Bool = false, readiness: String? = nil) {
        self.id = id; self.label = label; self.engine = engine; self.selectable = selectable
        self.isDefault = isDefault; self.readiness = readiness
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        label = try c.decodeIfPresent(String.self, forKey: .label) ?? id
        engine = try c.decodeIfPresent(String.self, forKey: .engine)
        selectable = try c.decodeIfPresent(Bool.self, forKey: .selectable) ?? true
        isDefault = try c.decodeIfPresent(Bool.self, forKey: .isDefault) ?? false
        readiness = try c.decodeIfPresent(String.self, forKey: .readiness)
        modelSelection = try c.decodeIfPresent(ChatModelSelection.self, forKey: .modelSelection)
        modelChoice = try c.decodeIfPresent(ChatModelChoice.self, forKey: .modelChoice)
        credentials = try? c.decodeIfPresent(String.self, forKey: .credentials)
    }
}

struct VibeDevCodingSelection: Codable, Equatable {
    let kind: String
    let engine: String?
    let modelId: String?
    let thinking: String?
    enum CodingKeys: String, CodingKey {
        case kind, engine, thinking
        case modelId = "model_id"
    }
    init(profile: VibeDevCodingProfile) {
        kind = "model"
        engine = profile.engine
        modelId = profile.modelSelection?.modelId
        thinking = profile.modelSelection?.thinking
    }
    var wireValue: [String: String] {
        var value = ["kind": kind]
        if let engine { value["engine"] = engine }
        if let modelId { value["model_id"] = modelId }
        if let thinking { value["thinking"] = thinking }
        return value
    }
}

struct VibeDevModelCatalog: Decodable {
    struct Model: Decodable {
        let engine: String
        let choice: ChatModelChoice
        enum CodingKeys: String, CodingKey { case engine }
        init(from decoder: Decoder) throws {
            engine = try decoder.container(keyedBy: CodingKeys.self).decode(String.self, forKey: .engine)
            choice = try ChatModelChoice(from: decoder)
        }
    }
    struct Engine: Decodable {
        let id: String
        let selectable: Bool
        let readiness: String?
        /// `login` / `magician_key` / `none`; read leniently.
        let credentials: String?
        enum CodingKeys: String, CodingKey { case id, selectable, readiness, credentials }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            id = try c.decode(String.self, forKey: .id)
            selectable = try c.decode(Bool.self, forKey: .selectable)
            readiness = try c.decodeIfPresent(String.self, forKey: .readiness)
            credentials = try? c.decodeIfPresent(String.self, forKey: .credentials)
        }
    }
    let models: [Model]
    let engines: [Engine]
    let defaultSelection: VibeDevCodingSelection?
    enum CodingKeys: String, CodingKey {
        case models, engines
        case defaultSelection = "default_selection"
    }
    var rows: [VibeDevCodingProfile] {
        models.flatMap { model -> [VibeDevCodingProfile] in
            guard let engine = engines.first(where: { $0.id == model.engine }), engine.selectable else { return [] }
            return model.choice.choices.sorted(by: { $0.key < $1.key }).map { thinking, route in
                var row = VibeDevCodingProfile(id: route, label: model.choice.label, engine: model.engine,
                    isDefault: defaultSelection?.kind == "model" && defaultSelection?.engine == model.engine
                        && defaultSelection?.modelId == model.choice.id && defaultSelection?.thinking == thinking,
                    readiness: engine.readiness)
                row.modelSelection = ChatModelSelection(modelId: model.choice.id, thinking: thinking)
                row.modelChoice = thinking == "auto" ? model.choice : nil
                row.credentials = engine.credentials
                return row
            }
        } + engines.filter { !$0.selectable }.map {
            var row = VibeDevCodingProfile(id: "unavailable/\($0.id)", label: $0.id, engine: $0.id,
                selectable: false, readiness: $0.readiness)
            row.credentials = $0.credentials
            return row
        }
    }
}

// MARK: - Writes

struct VibeDevStartRunRequest: Equatable {
    var prompt: String
    var projectId: String
    var mode: VibeDevTurnMode = .build
    var clientRunId: String = UUID().uuidString.lowercased()
    var parentTaskId: String?
    var threaded: Bool = false
    var codingChoice: VibeDevCodingSelection?
    var costBudgetUsd: Double?
    var attachments: [StagedAttachment] = []
    var attachmentSessionId: String?
    var nightly = false

    var body: [String: Any] {
        var body: [String: Any] = [
            "prompt": prompt, "project_id": projectId, "mode": mode.rawValue,
            "client_run_id": clientRunId,
        ]
        if let parentTaskId {
            body["parent_task_id"] = parentTaskId
            body["threaded"] = threaded
        }
        if let codingChoice { body["coding_choice"] = codingChoice.wireValue }
        if let costBudgetUsd { body["cost_budget_usd"] = costBudgetUsd }
        body["auto_apply"] = false
        if mode == .discuss { body["agent_id"] = "cto" }
        if !attachments.isEmpty {
            body["attachments"] = attachments.compactMap { attachment -> [String: Any]? in
                guard let id = attachment.remoteId else { return nil }
                return ["attachment_id": id, "filename": attachment.filename, "mime_type": attachment.mime]
            }
            body["attachment_session_id"] = attachmentSessionId
        }
        if nightly && mode == .autopilot && parentTaskId == nil {
            body["schedule"] = ["kind": ["Cron": ["expression": "0 2 * * *", "timezone": "UTC"]],
                                "timezone": "UTC", "missed_fire_policy": "skip", "concurrent_execution_policy": "skip"]
        }
        return body
    }
}

struct VibeDevStartRunResponse: Codable, Equatable {
    let taskId: String
    let executionId: String?
    let isFollowUp: Bool
    let scheduled: Bool
    let replayed: Bool

    enum CodingKeys: String, CodingKey {
        case taskId = "task_id"
        case executionId = "execution_id"
        case isFollowUp = "is_follow_up"
        case scheduled, replayed
    }

    init(taskId: String, executionId: String? = nil, isFollowUp: Bool = false,
         scheduled: Bool = false, replayed: Bool = false) {
        self.taskId = taskId; self.executionId = executionId; self.isFollowUp = isFollowUp
        self.scheduled = scheduled; self.replayed = replayed
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        taskId = try c.decode(String.self, forKey: .taskId)
        executionId = try c.decodeIfPresent(String.self, forKey: .executionId)
        isFollowUp = try c.decodeIfPresent(Bool.self, forKey: .isFollowUp) ?? false
        scheduled = try c.decodeIfPresent(Bool.self, forKey: .scheduled) ?? false
        replayed = try c.decodeIfPresent(Bool.self, forKey: .replayed) ?? false
    }
}

struct VibeDevCreateCrewRequest: Equatable {
    var goal: String
    var mode: VibeDevCrewMode = .split
    var racers: Int?
    var review: Bool = true
    var budgetUsd: Double?

    var body: [String: Any] {
        var body: [String: Any] = [
            "goal": goal, "mode": mode.rawValue, "review": review, "start": false,
        ]
        if mode == .race, let racers { body["racers"] = racers }
        if let budgetUsd { body["budget_usd"] = budgetUsd }
        return body
    }
}

enum VibeDevCrewAction: String {
    case start, stop, resolve, integrate
    case pullRequest = "pull-request"
}
