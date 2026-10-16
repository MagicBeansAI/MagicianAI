import Combine
import Foundation

/// Model usage per task and per chat turn: calls, tokens (input + output) and
/// cost. Pure rules plus one small shared store; no SwiftUI views.
///
/// Port of `ui/unified-ui/src/lib/magician/chat/next/taskUsage.ts` (per-task
/// usage, run + planning), the `runCost` reading of
/// `ui/unified-ui/src/lib/magician/tasks/taskTimeline.ts` (which panel rows
/// carry a bill), `ui/unified-ui/src/lib/llm/tokenUsage.ts` (absence is not
/// zero), and the turn chip / step suffix of `RequestActivityCard.svelte`.
///
/// Absence and zero are different values throughout: a task nothing reported
/// usage for has no usage (`nil`), never a record of zeros, and a cost stays
/// `nil` until some call carries a backend-recorded price. A surface renders
/// nothing for `nil` — never `0 tok · $0.00`.

/// What a set of model calls cost together.
struct TaskUsage: Equatable {
    var calls: Int
    var tokens: Int
    /// `nil` until a call reports a price.
    var costUsd: Double?

    static let zero = TaskUsage(calls: 0, tokens: 0, costUsd: nil)

    /// `self + other`; the cost stays `nil` only when neither side was priced.
    func adding(_ other: TaskUsage) -> TaskUsage {
        TaskUsage(
            calls: calls + other.calls,
            tokens: tokens + other.tokens,
            costUsd: costUsd == nil && other.costUsd == nil
                ? nil : (costUsd ?? 0) + (other.costUsd ?? 0))
    }
}

/// What one `/execution-panel` payload states: its run and its planning.
struct TaskUsageSnapshot: Equatable {
    var run: TaskUsage?
    var planning: TaskUsage?
}

/// The figure a task shows: run + planning, and the two parts for its
/// accessibility label.
struct TaskUsageTotal: Equatable {
    let usage: TaskUsage
    let run: TaskUsage?
    let planning: TaskUsage?
}

enum TaskUsageModel {
    /// `a + b`, where `nil` is "nothing yet" (web `add`).
    static func add(_ a: TaskUsage?, _ b: TaskUsage) -> TaskUsage {
        (a ?? .zero).adding(b)
    }

    /// The usage an `/execution-panel` payload states (web `usageSnapshotFrom`).
    ///
    /// Run: the run's activity (`run.activity_log`, else `run.recent_activity`)
    /// summed the way web `runCost` sums the Run act's rows — a row is a call
    /// when it reported any of the four token figures, its tokens are input +
    /// output, and its price (`cost_usd`, else the older `cost`) counts only on
    /// `llm.*` rows. `nil` when no row reported tokens or a price.
    ///
    /// Planning: each `debug.planning_timeline` row with `tokens` or `cost_usd`
    /// is one call (those fields exist only on newer backends; an older
    /// payload simply has no planning usage).
    static func usageSnapshot(fromPanel panel: [String: Any]) -> TaskUsageSnapshot {
        TaskUsageSnapshot(run: runUsage(fromPanel: panel), planning: planningUsage(fromPanel: panel))
    }

    static func runUsage(fromPanel panel: [String: Any]) -> TaskUsage? {
        let run = panel["run"] as? [String: Any] ?? [:]
        let log = run["activity_log"] as? [[String: Any]] ?? []
        let activity = log.isEmpty ? (run["recent_activity"] as? [[String: Any]] ?? []) : log
        var calls = 0
        var tokens = 0
        var reportedTokens = false
        var cost: Double?
        for item in activity {
            let meta = item["metadata"] as? [String: Any] ?? [:]
            let input = number(meta["input_tokens"])
            let output = number(meta["output_tokens"])
            let cacheRead = number(meta["cache_read_tokens"])
            let cacheCreation = number(meta["cache_creation_tokens"])
            if input != nil || output != nil || cacheRead != nil || cacheCreation != nil {
                reportedTokens = true
                calls += 1
                tokens += Int((input ?? 0) + (output ?? 0))
            }
            let eventType = meta["event_type"] as? String ?? ""
            if eventType.hasPrefix("llm."),
               let price = number(meta["cost_usd"]) ?? number(meta["cost"]), price >= 0 {
                cost = (cost ?? 0) + price
            }
        }
        guard reportedTokens || cost != nil else { return nil }
        return TaskUsage(calls: calls, tokens: tokens, costUsd: cost)
    }

    static func planningUsage(fromPanel panel: [String: Any]) -> TaskUsage? {
        let debug = panel["debug"] as? [String: Any] ?? [:]
        var planning: TaskUsage?
        for entry in debug["planning_timeline"] as? [[String: Any]] ?? [] {
            let tokens = number(entry["tokens"])
            let priced = number(entry["cost_usd"])
            if tokens == nil && priced == nil { continue }
            // A backfilled summary row (`llm_calls`) stands for that many calls.
            let calls = number(entry["llm_calls"]).map { max(1, Int($0)) } ?? 1
            planning = add(planning, TaskUsage(calls: calls, tokens: Int(tokens ?? 0), costUsd: priced))
        }
        return planning
    }

    /// The task's total: the live run when calls were recorded, else the
    /// snapshot's run; planning always from the snapshot (planning calls are
    /// not on the task's event stream). `nil` when nothing reported usage.
    static func total(live: TaskUsage?, snapshot: TaskUsageSnapshot?) -> TaskUsageTotal? {
        let run: TaskUsage? = {
            if let live, live.calls > 0 { return live }
            return snapshot?.run
        }()
        let planning = snapshot?.planning
        let usage = add(run, planning ?? .zero)
        if usage.calls == 0 && usage.costUsd == nil { return nil }
        return TaskUsageTotal(usage: usage, run: run, planning: planning)
    }

    /// A finite JSON number, never a boolean.
    static func number(_ value: Any?) -> Double? {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
        let double = number.doubleValue
        return double.isFinite ? double : nil
    }
}

// MARK: - Ledger

/// Per-task live calls and stored snapshots (web `taskUsage` stores), as a
/// value so the rules are testable without a store.
struct TaskUsageLedger: Equatable {
    private var seen: Set<String> = []
    private(set) var live: [String: TaskUsage] = [:]
    private(set) var snapshots: [String: TaskUsageSnapshot] = [:]

    /// Record one finished model call of a task's run, once per `callKey`.
    /// Returns false for a replayed call.
    @discardableResult
    mutating func record(taskId: String, callKey: String, tokens: Int?, costUsd: Double?) -> Bool {
        guard seen.insert("\(taskId)::\(callKey)").inserted else { return false }
        live[taskId] = TaskUsageModel.add(live[taskId], TaskUsage(calls: 1, tokens: tokens ?? 0, costUsd: costUsd))
        return true
    }

    mutating func setSnapshot(_ snapshot: TaskUsageSnapshot, for taskId: String) {
        snapshots[taskId] = snapshot
    }

    func total(for taskId: String) -> TaskUsageTotal? {
        TaskUsageModel.total(live: live[taskId], snapshot: snapshots[taskId])
    }
}

/// The app's one ledger: the live task card's `/execution-panel` poll writes
/// it, a turn's chip reads the tasks the turn started from it, and a task's
/// stored usage is loaded once per phase (web `loadTaskUsageSnapshot`).
@MainActor
final class TaskUsageStore: ObservableObject {
    static let shared = TaskUsageStore()

    @Published private(set) var ledger = TaskUsageLedger()
    private var loaded: Set<String> = []

    func total(for taskId: String) -> TaskUsageTotal? { ledger.total(for: taskId) }

    func record(taskId: String, callKey: String, tokens: Int?, costUsd: Double?) {
        var next = ledger
        if next.record(taskId: taskId, callKey: callKey, tokens: tokens, costUsd: costUsd) { ledger = next }
    }

    /// A panel the caller already fetched (the live card's poll).
    func apply(panel: [String: Any], taskId: String) {
        let snapshot = TaskUsageModel.usageSnapshot(fromPanel: panel)
        guard ledger.snapshots[taskId] != snapshot else { return }
        var next = ledger
        next.setSnapshot(snapshot, for: taskId)
        ledger = next
    }

    /// Fetch a task's stored usage once per `phase` (the caller's status
    /// bucket, so a task is re-read when planning ends and when the run ends,
    /// not on every render).
    func loadSnapshot(taskId: String, phase: String) {
        guard loaded.insert("\(taskId)::\(phase)").inserted else { return }
        Task { [weak self] in
            guard let panel = await LiveTaskCardClient.json(
                "/tasks/\(LiveTaskCardClient.segment(taskId))/execution-panel") else { return }
            self?.apply(panel: panel, taskId: taskId)
        }
    }
}

// MARK: - A chat turn's usage

/// The turn chip's figures: the turn's own model calls plus every task the
/// turn started (web `deriveCacheSummary` + `withSpawnedTaskUsage`).
struct TurnUsageSummary: Equatable {
    var calls: Int
    var inputTokens: Int
    var outputTokens: Int
    var costUsd: Double?
    /// Tasks this turn started (`create_task` / `run_task`), priced or not yet.
    var spawnedTasks: Int

    var tokens: Int { inputTokens + outputTokens }
}

enum TurnUsage {
    /// The turn's own calls: every LLM row that finished (`llm.succeeded`, or
    /// a flat-loop `LLMResponseReceived`). A call is counted when it reported
    /// any token figure; with none reporting tokens, every finished call
    /// counts. `nil` when no call reported tokens or a price.
    static func own(_ rows: [ActivityRow]) -> TurnUsageSummary? {
        var seenCallIds: Set<String> = []
        let calls = rows.compactMap(\.callUsage).filter { call in
            // One logical call delivered by two transports counts once.
            guard let callId = call.callId else { return true }
            return seenCallIds.insert(callId).inserted
        }
        var input: Int?
        var output: Int?
        var reported = 0
        var cost: Double?
        for call in calls {
            if call.reportedTokens {
                reported += 1
                if let value = call.inputTokens { input = (input ?? 0) + value }
                if let value = call.outputTokens { output = (output ?? 0) + value }
            }
            if let price = call.costUsd { cost = (cost ?? 0) + price }
        }
        guard reported > 0 || cost != nil else { return nil }
        return TurnUsageSummary(
            calls: reported > 0 ? reported : calls.count,
            inputTokens: input ?? 0, outputTokens: output ?? 0,
            costUsd: cost, spawnedTasks: 0)
    }

    /// The turn's own usage plus that of the tasks it started. Task tokens
    /// are not split into input and output; they count as output.
    static func summary(rows: [ActivityRow], taskUsage: (String) -> TaskUsageTotal?) -> TurnUsageSummary? {
        let base = own(rows)
        let taskIds = spawnedTaskIds(rows)
        guard !taskIds.isEmpty else { return base }
        var summary = base ?? TurnUsageSummary(calls: 0, inputTokens: 0, outputTokens: 0, costUsd: nil, spawnedTasks: 0)
        for usage in taskIds.compactMap(taskUsage).map(\.usage) {
            summary.calls += usage.calls
            summary.outputTokens += usage.tokens
            if let price = usage.costUsd { summary.costUsd = (summary.costUsd ?? 0) + price }
        }
        summary.spawnedTasks = taskIds.count
        return summary
    }

    static func spawnedTaskIds(_ rows: [ActivityRow]) -> [String] {
        var ids: [String] = []
        for id in rows.flatMap(\.spawnedTaskIds) where !ids.contains(id) { ids.append(id) }
        return ids
    }
}

// MARK: - Formatting

enum TaskUsageFormat {
    /// A backend-recorded USD amount (web `formatUsd`): three decimals below
    /// a dollar, two from a dollar up, `<$0.001` below half a thousandth.
    /// `nil` for no price.
    static func usd(_ amount: Double?) -> String? { TaskTimelineFormatting.usd(amount) }

    /// `usd` for VoiceOver: `<$0.001` reads "less than a tenth of a cent".
    static func spokenUsd(_ amount: Double?) -> String? { TaskTimelineFormatting.spokenUsd(amount) }

    /// The live card's and planning activity's tokens: `12.3k` from 1000 up,
    /// else the count.
    static func compactTokens(_ value: Int) -> String {
        value >= 1000 ? String(format: "%.1fk", locale: Locale(identifier: "en_US_POSIX"), Double(value) / 1000) : String(value)
    }

    /// The turn chip's and a step's tokens (web `formatTokenCount`): `850`,
    /// `9.1k`, `42k`, `1.2M`.
    static func tokenCount(_ value: Int) -> String {
        guard value > 0 else { return "0" }
        let posix = Locale(identifier: "en_US_POSIX")
        if value < 1000 { return String(value) }
        if value < 10_000 { return String(format: "%.1fk", locale: posix, Double(value) / 1000) }
        if value < 1_000_000 { return "\(Int((Double(value) / 1000).rounded()))k" }
        return String(format: "%.1fM", locale: posix, Double(value) / 1_000_000)
    }

    /// One call's usage for its step row: `" · 9.1k tok · $0.012"`, or `""`
    /// when it reported neither (web `llmUsageSuffix`).
    static func stepSuffix(tokens: Int?, costUsd: Double?) -> String {
        [tokens.flatMap { $0 > 0 ? "\(tokenCount($0)) tok" : nil }, usd(costUsd)]
            .compactMap { $0 }
            .map { " · \($0)" }
            .joined()
    }

    /// A step detail with its usage appended; `nil` when there is neither
    /// (web `withUsage`).
    static func withUsage(_ detail: String?, suffix: String) -> String? {
        guard let detail, !detail.isEmpty else {
            return suffix.isEmpty ? nil : String(suffix.dropFirst(3))
        }
        return detail + suffix
    }

    static func calls(_ count: Int) -> String { "\(count) call\(count == 1 ? "" : "s")" }

    /// The live card's line: `3 calls · 12.3k tok · $0.04`; `nil` with no calls.
    static func taskLine(_ usage: TaskUsage) -> String? {
        guard usage.calls > 0 else { return nil }
        return ([calls(usage.calls), "\(compactTokens(usage.tokens)) tok", usd(usage.costUsd)] as [String?])
            .compactMap { $0 }
            .joined(separator: " · ")
    }

    /// The live card's spoken figure, split into Planning and Run.
    static func taskAccessibilityLabel(_ total: TaskUsageTotal) -> String {
        let usage = total.usage
        var parts = ["\(usage.calls) model call\(usage.calls == 1 ? "" : "s"), \(usage.tokens) tokens"
            + (spokenUsd(usage.costUsd).map { ", \($0)" } ?? "")]
        if let planning = total.planning {
            parts.append("Planning: \(calls(planning.calls))" + (spokenUsd(planning.costUsd).map { ", \($0)" } ?? ""))
        }
        if let run = total.run {
            parts.append("Run, including delegated work: \(calls(run.calls))" + (spokenUsd(run.costUsd).map { ", \($0)" } ?? ""))
        }
        return parts.joined(separator: ". ")
    }

    /// The turn chip: `4 calls · 12k tok · $0.04 · incl. 1 task`; `nil` when
    /// nothing reported a call or a price.
    static func turnLine(_ summary: TurnUsageSummary) -> String? {
        guard summary.calls > 0 || summary.costUsd != nil else { return nil }
        let tasks = summary.spawnedTasks > 0
            ? "incl. \(summary.spawnedTasks) task\(summary.spawnedTasks == 1 ? "" : "s")" : nil
        return ([calls(summary.calls), "\(tokenCount(summary.tokens)) tok", usd(summary.costUsd), tasks] as [String?])
            .compactMap { $0 }
            .joined(separator: " · ")
    }

    /// The turn chip's short form for a narrow header: `12k tok · $0.04`.
    static func turnShortLine(_ summary: TurnUsageSummary) -> String? {
        guard summary.calls > 0 || summary.costUsd != nil else { return nil }
        return (["\(tokenCount(summary.tokens)) tok", usd(summary.costUsd)] as [String?])
            .compactMap { $0 }
            .joined(separator: " · ")
    }

    static func turnAccessibilityLabel(_ summary: TurnUsageSummary) -> String {
        var label = "\(summary.calls) model call\(summary.calls == 1 ? "" : "s"), \(summary.tokens) tokens"
        if let cost = spokenUsd(summary.costUsd) { label += ", \(cost)" }
        if summary.spawnedTasks > 0 {
            label += ", including \(summary.spawnedTasks) task\(summary.spawnedTasks == 1 ? "" : "s") this turn started"
        }
        return label
    }

    /// The planning activity toggle's usage: `" · 36.0k tok · $0.050"` — tokens
    /// and the summed price of the planning model calls, or `""`.
    static func planningSuffix(_ entries: [PlanningEntry]) -> String {
        let tokens = entries.reduce(0) { $0 + ($1.tokens ?? 0) }
        let priced = entries.compactMap(\.costUsd)
        let cost = priced.isEmpty ? nil : usd(priced.reduce(0, +))
        return [tokens > 0 ? "\(compactTokens(tokens)) tok" : nil, cost]
            .compactMap { $0 }
            .map { " · \($0)" }
            .joined()
    }

    /// A Run act summary with the run's usage: `… · 5 calls · 9.1k tok · $0.03`.
    static func runSummary(_ summary: String, run: TaskUsage?) -> String {
        guard let run, let line = taskLine(run) else { return summary }
        return summary.isEmpty ? line : "\(summary) · \(line)"
    }
}
