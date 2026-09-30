import Foundation

// Pure, view-free rules the Tasks surface shares with the web task UI. Each
// helper names the web function it mirrors so a change on one side has an
// obvious twin to update:
//
// - `TaskResultRule`      ← `hasFinalResult` (NativeTasksSurface.svelte) /
//                           `internalTaskHasResult` (InternalTasksWorkspace.svelte)
// - `TaskRecurrence`      ← `isRecurring` / `isRecurringTask`
// - `TaskCronDescription` ← `cronToHumanReadable` (lib/utils/cron.ts)
// - `TaskOutputScope`     ← `outputScope` / `outputScopeTitle` /
//                           `outputScopeDescription` (UnifiedTaskPanel.svelte)
// - `TaskDelegationTimeline` ← `groupTimelineByDelegation` / `delegationSpan`
//                           (taskTimeline.ts)

// MARK: - Result

enum TaskResultRule {
    /// Outcome words that make a completed task's outcome a non-result.
    static let failureWords = ["failed", "cancelled", "canceled", "stopped", "error"]

    /// A task has a result to show when it named artifacts, wrote a summary, or
    /// completed with an outcome that does not read as a failure.
    static func hasResult(
        status: String,
        completionSummary: String?,
        completionOutcome: String?,
        artifactNames: [String]
    ) -> Bool {
        if !artifactNames.isEmpty { return true }
        if let summary = completionSummary,
           !summary.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return true
        }
        guard status.lowercased() == "completed",
              let outcome = completionOutcome?
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .lowercased(),
              !outcome.isEmpty else { return false }
        return !failureWords.contains { outcome.contains($0) }
    }
}

// MARK: - Recurrence

enum TaskRecurrence {
    static let tagNames: Set<String> = ["recurring", "app_recurring"]

    static func isRecurringTag(_ tag: TaskTag) -> Bool {
        tagNames.contains(tag.name.lowercased()) || tagNames.contains(tag.id.lowercased())
    }

    static func isRecurring(cron: String?, tags: [TaskTag], hasRecurringSchedule: Bool = false) -> Bool {
        if let cron, !cron.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return true }
        if tags.contains(where: isRecurringTag) { return true }
        return hasRecurringSchedule
    }

    /// Secondary text for the ↻ chip: the human cron description when there is
    /// a cron, otherwise a plain statement (the tag carries no cadence).
    static func description(cron: String?) -> String {
        guard let cron, !cron.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return "Recurring task"
        }
        return TaskCronDescription.describe(cron)
    }
}

// MARK: - Cron description

/// A small port of the cases `cronToHumanReadable` renders most: every N
/// minutes/hours, daily, weekdays, weekly and monthly at a wall-clock time.
/// Anything else falls back to the raw expression rather than guessing.
enum TaskCronDescription {
    private static let dayNames = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"]
    private static let dayAbbreviations = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"]

    static func describe(_ raw: String) -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return "" }
        let fields = trimmed.split(whereSeparator: \.isWhitespace).map(String.init)
        guard fields.count == 5 else { return trimmed }
        let minute = fields[0], hour = fields[1], dayOfMonth = fields[2], month = fields[3], dayOfWeek = fields[4]
        guard month == "*" else { return trimmed }

        if hour == "*", dayOfMonth == "*", dayOfWeek == "*" {
            if minute == "*" { return "Every minute" }
            if let n = step(minute) { return n == 1 ? "Every minute" : "Every \(n) minutes" }
            if let m = number(minute, in: 0...59) {
                return m == 0 ? "Every hour" : "Every hour at :\(pad(m))"
            }
            return trimmed
        }

        if dayOfMonth == "*", dayOfWeek == "*", let n = step(hour),
           let m = number(minute, in: 0...59) {
            let base = n == 1 ? "Every hour" : "Every \(n) hours"
            return m == 0 ? base : "\(base) at :\(pad(m))"
        }

        guard let m = number(minute, in: 0...59), let h = number(hour, in: 0...23) else {
            return trimmed
        }
        let at = "\(pad(h)):\(pad(m))"
        if dayOfMonth == "*", dayOfWeek == "*" { return "Daily at \(at)" }
        if dayOfMonth == "*" {
            if dayOfWeek == "1-5" || dayOfWeek.uppercased() == "MON-FRI" { return "Weekdays at \(at)" }
            if let days = weekdays(dayOfWeek) { return "Weekly on \(days.joined(separator: ", ")) at \(at)" }
            return trimmed
        }
        if dayOfWeek == "*", let day = number(dayOfMonth, in: 1...31) {
            return "Monthly on day \(day) at \(at)"
        }
        return trimmed
    }

    private static func step(_ field: String) -> Int? {
        guard field.hasPrefix("*/"), let n = Int(field.dropFirst(2)), n > 0 else { return nil }
        return n
    }

    private static func number(_ field: String, in range: ClosedRange<Int>) -> Int? {
        guard let n = Int(field), range.contains(n) else { return nil }
        return n
    }

    private static func weekdays(_ field: String) -> [String]? {
        let parts = field.split(separator: ",").map(String.init)
        guard !parts.isEmpty else { return nil }
        var names: [String] = []
        for part in parts {
            if let n = Int(part), (0...7).contains(n) {
                names.append(dayNames[n % 7])
            } else if let index = dayAbbreviations.firstIndex(of: part.uppercased()) {
                names.append(dayNames[index])
            } else {
                return nil
            }
        }
        return names
    }

    private static func pad(_ n: Int) -> String { n < 10 ? "0\(n)" : "\(n)" }
}

// MARK: - Output scopes

/// Who a task output belongs to. Task deliverables are the stable, promoted
/// files; the other three belong only to the selected run.
enum TaskOutputScope: String, CaseIterable, Equatable {
    case task, execution, delegated, artifact

    /// Tolerant wire decode: unknown or absent scopes are nil so the caller
    /// picks the default for the source it read the row from.
    init?(wire: String?) {
        guard let raw = wire?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased(),
              let scope = TaskOutputScope(rawValue: raw) else { return nil }
        self = scope
    }

    var title: String {
        switch self {
        case .task: return "Task deliverables"
        case .execution: return "Direct outputs"
        case .delegated: return "Delegated outputs"
        case .artifact: return "Persisted artifacts"
        }
    }

    var description: String {
        switch self {
        case .task:
            return "Stable task-level deliverables. These can be promoted or replaced across runs."
        case .execution:
            return "Files written directly by the selected execution."
        case .delegated:
            return "Files returned by work delegated from the selected execution."
        case .artifact:
            return "File-backed evidence persisted during the selected execution."
        }
    }
}

/// An artifact persisted by the selected run with no file behind it — it has
/// an identity to show but nothing to open.
struct TaskDetailStructuredArtifact: Identifiable, Equatable {
    let id: String
    let name: String
    let artifactType: String?
    let contentType: String?
    let producedAt: String?
}

struct TaskOutputSection: Identifiable, Equatable {
    let scope: TaskOutputScope
    let files: [TaskDetailArtifact]
    let structured: [TaskDetailStructuredArtifact]
    var id: String { scope.rawValue }
}

struct TaskOutputGroups: Equatable {
    /// Task-scoped files: the Output act's headline list.
    let deliverables: [TaskDetailArtifact]
    /// Run-owned groups in fixed order (direct, delegated, persisted), empty
    /// groups omitted.
    let intermediates: [TaskOutputSection]

    var intermediateCount: Int {
        intermediates.reduce(0) { $0 + $1.files.count + $1.structured.count }
    }

    static func group(
        _ files: [TaskDetailArtifact],
        structured: [TaskDetailStructuredArtifact] = []
    ) -> TaskOutputGroups {
        let deliverables = files.filter { $0.scope == .task }
        let intermediates = [TaskOutputScope.execution, .delegated, .artifact].compactMap { scope -> TaskOutputSection? in
            let scoped = files.filter { $0.scope == scope }
            let extra = scope == .artifact ? structured : []
            guard !scoped.isEmpty || !extra.isEmpty else { return nil }
            return TaskOutputSection(scope: scope, files: scoped, structured: extra)
        }
        return TaskOutputGroups(deliverables: deliverables, intermediates: intermediates)
    }
}

// MARK: - Delegation envelopes

/// A delegated child execution whose events appear in the parent's activity
/// log (`run.delegations` on the execution panel payload). Rows are matched by
/// the child's execution id, never by agent: one agent can be delegated to
/// more than once in a run.
struct TaskDelegationGroup: Identifiable, Equatable {
    let executionId: String
    let agentId: String
    /// The child's own status, not the parent's.
    let status: String
    let entryCount: Int
    let parentExecutionId: String?
    let startedAt: Date?
    let completedAt: Date?

    var id: String { executionId }
}

enum TaskTimelineSegment: Identifiable, Equatable {
    case row(TaskDetailActivity)
    case delegation(TaskDelegationGroup, [TaskDetailActivity])

    var id: String {
        switch self {
        case .row(let activity): return activity.id
        case .delegation(let group, _): return "delegation:\(group.executionId)"
        }
    }
}

enum TaskTimelineMode: String, CaseIterable, Identifiable {
    case grouped, chronological

    /// Remembered per device, like the web toggle's local preference.
    static let storageKey = "magios.tasks.timelineMode"

    var id: String { rawValue }
    var title: String {
        switch self {
        case .grouped: return "Grouped"
        case .chronological: return "Chronological"
        }
    }
}

struct TaskDelegationSpan: Equatable {
    let startClock: String?
    let endClock: String?
    let duration: String?
    let summary: String?
}

enum TaskDelegationTimeline {
    /// One block per delegated child, placed where its first row landed; later
    /// rows join that block. Rows whose execution has no delegation entry stay
    /// plain rows rather than vanishing.
    static func group(
        _ activity: [TaskDetailActivity],
        delegations: [TaskDelegationGroup]
    ) -> [TaskTimelineSegment] {
        var groups: [String: TaskDelegationGroup] = [:]
        for delegation in delegations where !delegation.executionId.isEmpty {
            groups[delegation.executionId] = delegation
        }
        guard !groups.isEmpty else { return activity.map { .row($0) } }

        enum Slot { case row(TaskDetailActivity), block(String) }
        var slots: [Slot] = []
        var buckets: [String: [TaskDetailActivity]] = [:]
        for entry in activity {
            guard let executionId = entry.executionId, groups[executionId] != nil else {
                slots.append(.row(entry))
                continue
            }
            if buckets[executionId] == nil {
                buckets[executionId] = []
                slots.append(.block(executionId))
            }
            buckets[executionId, default: []].append(entry)
        }
        return slots.compactMap { slot in
            switch slot {
            case .row(let entry): return .row(entry)
            case .block(let executionId):
                guard let group = groups[executionId] else { return nil }
                return .delegation(group, buckets[executionId] ?? [])
            }
        }
    }

    /// The agent a row was delegated to, for the chronological view's suffix.
    static func delegatedAgent(
        for activity: TaskDetailActivity,
        delegations: [TaskDelegationGroup]
    ) -> String? {
        guard let executionId = activity.executionId else { return nil }
        return delegations.first { $0.executionId == executionId }?.agentId
    }

    /// Start/end clocks, duration and a one-line summary for a child block:
    /// `10:02:00 – 10:07:00 (5m)`, or `started 10:02:00` while it runs. The
    /// recorded start/completion win; otherwise the child's own rows bound it.
    static func span(
        entries: [TaskDetailActivity],
        group: TaskDelegationGroup?,
        clock: (Date?) -> String? = TaskTimelineFormatting.wallClock
    ) -> TaskDelegationSpan {
        var start = group?.startedAt
        var end = group?.completedAt
        let timed = entries.compactMap(\.timestamp)
        if let first = timed.first, start == nil { start = first }
        if end == nil, group?.status.lowercased() != "running", let last = timed.last { end = last }

        let startClock = clock(start)
        let endClock = clock(end)
        let duration: String? = {
            guard let start, let end, end >= start else { return nil }
            return TaskTimelineFormatting.duration(seconds: end.timeIntervalSince(start))
        }()

        var summary: String?
        if let startClock, let endClock, startClock != endClock {
            summary = duration.map { "\(startClock) – \(endClock) (\($0))" } ?? "\(startClock) – \(endClock)"
        } else if let startClock {
            summary = group?.status.lowercased() == "running" ? "started \(startClock)" : startClock
        }
        return TaskDelegationSpan(startClock: startClock, endClock: endClock, duration: duration, summary: summary)
    }
}

// MARK: - History

enum TaskHistoryFormatting {
    /// `started → ended · 5m` for one root execution; `started …` while it has
    /// no recorded end; nil when nothing was recorded.
    static func runSpan(
        startedAt: Date?,
        endedAt: Date?,
        clock: (Date) -> String
    ) -> String? {
        guard let startedAt else { return nil }
        guard let endedAt else { return "started \(clock(startedAt))" }
        let base = "\(clock(startedAt)) → \(clock(endedAt))"
        guard endedAt >= startedAt,
              let duration = TaskTimelineFormatting.duration(seconds: endedAt.timeIntervalSince(startedAt))
        else { return base }
        return "\(base) · \(duration)"
    }
}
