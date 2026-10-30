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
    var tokensReported = true

    static let zero = TaskUsage(calls: 0, tokens: 0, costUsd: nil, tokensReported: false)

    /// `self + other`; the cost stays `nil` only when neither side was priced.
    func adding(_ other: TaskUsage) -> TaskUsage {
        TaskUsage(
            calls: calls + other.calls,
            tokens: tokens + other.tokens,
            costUsd: costUsd == nil && other.costUsd == nil
                ? nil : (costUsd ?? 0) + (other.costUsd ?? 0),
            tokensReported: tokensReported || other.tokensReported)
    }
}

/// What one `/execution-panel` payload states: its run and its planning.
struct TaskUsageSnapshot: Equatable {
    var run: TaskUsage?
    var planning: TaskUsage?
    var executionId: String? = nil
    var calls: [String: TaskCallUsage] = [:]
    var activityIds: Set<String>? = nil
    var updatedAt: Double = 0
}

struct TaskCallUsage: Equatable {
    var tokens: Int?
    var costUsd: Double?
    var executionId: String? = nil

    func enriched(by next: Self) -> Self {
        Self(tokens: next.tokens ?? tokens, costUsd: next.costUsd ?? costUsd,
             executionId: next.executionId ?? executionId)
    }
}

/// The figure a task shows: run + planning, and the two parts for its
/// accessibility label.
struct TaskUsageTotal: Equatable {
    let usage: TaskUsage
    let run: TaskUsage?
    let planning: TaskUsage?
    var executionId: String? = nil
    var activityCount: Int? = nil
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
        let executionId = TaskRunEvidence.executionId(panel)
        let run = panel["run"] as? [String: Any] ?? [:]
        let full = run["activity_log"] as? [[String: Any]] ?? []
        let activity = full.isEmpty ? (run["recent_activity"] as? [[String: Any]] ?? []) : full
        var calls: [String: TaskCallUsage] = [:]
        var ids = Set<String>()
        for item in activity {
            let meta = item["metadata"] as? [String: Any] ?? [:]
            let correlation = meta["correlation"] as? [String: Any] ?? [:]
            let type = meta["event_type"] as? String ?? ""
            let rawId = meta["llm_call_id"] as? String ?? correlation["llm_call_id"] as? String
            let itemId = item["id"] as? String
            if let id = rawId ?? itemId { ids.insert(rawId.map { "llm-call:\($0)" } ?? id) }
            guard type.hasPrefix("llm.") || type == "LLMResponseReceived" else { continue }
            if number(meta["provider_attempt_count"] ?? correlation["provider_attempt_count"]) == 0 { continue }
            let availability = meta["usage_availability"] as? [String: Any] ?? [:]
            guard meta["usage_reported"] as? Bool != false else { continue }
            let input = availability["tokens"] as? Bool == false ? nil : number(meta["input_tokens"])
            let output = availability["tokens"] as? Bool == false ? nil : number(meta["output_tokens"])
            let cost = availability["cost"] as? Bool == false ? nil : number(meta["cost_usd"]) ?? number(meta["cost"])
            let hasCache = number(meta["cache_read_tokens"]) != nil || number(meta["cache_creation_tokens"]) != nil
            guard input != nil || output != nil || cost != nil || hasCache, let id = rawId ?? itemId else { continue }
            let call = TaskCallUsage(tokens: input == nil && output == nil ? nil : Int((input ?? 0) + (output ?? 0)),
                                     costUsd: cost.flatMap { $0 >= 0 ? $0 : nil }, executionId: executionId)
            calls[id] = calls[id]?.enriched(by: call) ?? call
        }
        return TaskUsageSnapshot(run: sumCalls(calls), planning: planningUsage(fromPanel: panel),
            executionId: executionId, calls: calls, activityIds: ids,
            updatedAt: number((panel["overview"] as? [String: Any])?["updated_at"]) ?? 0)
    }

    static func runUsage(fromPanel panel: [String: Any]) -> TaskUsage? { usageSnapshot(fromPanel: panel).run }

    static func sumCalls(_ calls: [String: TaskCallUsage]) -> TaskUsage? {
        calls.values.reduce(nil) { total, call in
            add(total, TaskUsage(calls: 1, tokens: call.tokens ?? 0, costUsd: call.costUsd,
                                 tokensReported: call.tokens != nil))
        }
    }

    static func mergeCalls(_ base: [String: TaskCallUsage], _ next: [String: TaskCallUsage]) -> [String: TaskCallUsage] {
        var result = base
        for (id, call) in next { result[id] = result[id]?.enriched(by: call) ?? call }
        return result
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
            planning = add(planning, TaskUsage(calls: calls, tokens: Int(tokens ?? 0), costUsd: priced, tokensReported: tokens != nil))
        }
        return planning
    }

    /// Aggregate-only compatibility: retain the stored baseline. Production
    /// totals merge individual call IDs in the ledger before reaching here.
    static func total(live: TaskUsage?, snapshot: TaskUsageSnapshot?) -> TaskUsageTotal? {
        let run: TaskUsage? = {
            guard let stored = snapshot?.run else { return live }
            guard let live else { return stored }
            return TaskUsage(calls: max(stored.calls, live.calls), tokens: max(stored.tokens, live.tokens),
                             costUsd: [stored.costUsd, live.costUsd].compactMap { $0 }.max(),
                             tokensReported: stored.tokensReported || live.tokensReported)
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
    private var liveCalls: [String: [String: TaskCallUsage]] = [:]
    private var retired: [String: Set<String>] = [:]
    private(set) var snapshots: [String: TaskUsageSnapshot] = [:]
    var live: [String: TaskUsage] { liveCalls.compactMapValues(TaskUsageModel.sumCalls) }

    @discardableResult
    mutating func record(taskId: String, callKey: String, tokens: Int?, costUsd: Double?, executionId: String? = nil) -> Bool {
        guard tokens != nil || costUsd != nil else { return false }
        if let executionId, retired[taskId]?.contains(executionId) == true { return false }
        let call = TaskCallUsage(tokens: tokens, costUsd: costUsd, executionId: executionId)
        let old = liveCalls[taskId]?[callKey]
        let next = old?.enriched(by: call) ?? call
        guard old != next else { return false }
        liveCalls[taskId, default: [:]][callKey] = next
        return true
    }

    @discardableResult
    mutating func setSnapshot(_ snapshot: TaskUsageSnapshot, for taskId: String) -> Bool {
        var next = snapshot
        if let previous = snapshots[taskId] {
            if next.executionId == nil, previous.executionId != nil { return false }
            if let id = next.executionId, retired[taskId]?.contains(id) == true { return false }
            if previous.executionId != next.executionId {
                guard next.updatedAt >= previous.updatedAt else { return false }
                if let id = previous.executionId { retired[taskId, default: []].insert(id) }
                liveCalls[taskId] = liveCalls[taskId]?.filter { $0.value.executionId == next.executionId }
            } else {
                next.calls = next.updatedAt < previous.updatedAt
                    ? TaskUsageModel.mergeCalls(next.calls, previous.calls)
                    : TaskUsageModel.mergeCalls(previous.calls, next.calls)
                next.run = TaskUsageModel.sumCalls(next.calls) ?? next.run ?? previous.run
                next.planning = next.planning ?? previous.planning
                next.activityIds = previous.activityIds.map { $0.union(next.activityIds ?? []) } ?? next.activityIds
                next.updatedAt = max(previous.updatedAt, next.updatedAt)
            }
        }
        guard snapshots[taskId] != next else { return false }
        snapshots[taskId] = next
        return true
    }

    func total(for taskId: String) -> TaskUsageTotal? {
        let snapshot = snapshots[taskId]
        let live = (liveCalls[taskId] ?? [:]).filter { snapshot?.executionId == nil || $0.value.executionId == nil || $0.value.executionId == snapshot?.executionId }
        let run = TaskUsageModel.sumCalls(TaskUsageModel.mergeCalls(live, snapshot?.calls ?? [:]))
        let total = TaskUsageModel.total(live: run, snapshot: snapshot)
        return total.map { TaskUsageTotal(usage: $0.usage, run: $0.run, planning: $0.planning,
            executionId: snapshot?.executionId, activityCount: snapshot?.activityIds?.count) }
    }
}

/// Shared evidence rules for detail panels and the per-task usage ledger.
/// Only real work advances freshness; a heartbeat never hides a stall.
enum TaskRunEvidence {
    static let progressEvents: Set<String> = [
        "llm.requested", "llm.first_token", "llm.succeeded", "llm.failed",
        "reasoning.start", "reasoning.content", "reasoning.end", "tool.started", "tool.requested",
        "tool.succeeded", "tool.failed", "tool.call.started", "tool.call.finished", "tool.call.failed",
        "step.started", "step.completed", "step.failed", "step.skipped", "step.delegated",
        "child.spawned", "child.completed", "child.failed", "child.output_available",
        "sub_goal.requested", "sub_goal.outcome", "execution.started", "execution.resumed",
        "LLMRequestSent", "LLMResponseReceived", "AgenticActionStarted", "AgenticActionExecuted"
    ]
    static func isProgress(_ type: String, _ meta: [String: Any] = [:]) -> Bool {
        guard progressEvents.contains(type) else { return false }
        let correlation = meta["correlation"] as? [String: Any] ?? [:]
        return !(["llm.succeeded", "llm.failed", "LLMResponseReceived"].contains(type)
            && TaskUsageModel.number(meta["provider_attempt_count"] ?? correlation["provider_attempt_count"]) == 0
            && meta["response_kind"] as? String != "harness_aggregate")
    }
    static func executionId(_ panel: [String: Any]) -> String? {
        let overview = panel["overview"] as? [String: Any] ?? [:]
        let debug = panel["debug"] as? [String: Any] ?? [:]
        let selected = debug["selected_execution"] as? [String: Any] ?? [:]
        return overview["execution_id"] as? String ?? selected["execution_id"] as? String
            ?? (selected["state"] as? [String: Any])?["execution_id"] as? String
    }
    static func date(_ value: Any?) -> Date? {
        if let millis = TaskUsageModel.number(value), millis > 0 { return Date(timeIntervalSince1970: millis / 1000) }
        guard let value = value as? String else { return nil }
        let format = ISO8601DateFormatter(); format.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let date = format.date(from: value) ?? ISO8601DateFormatter().date(from: value)
        return date.flatMap { $0.timeIntervalSince1970 > 0 ? $0 : nil }
    }
    static func latest(_ dates: Date?...) -> Date? { dates.compactMap { $0 }.filter { $0.timeIntervalSince1970 > 0 }.max() }
    static func progressAt(_ panel: [String: Any]?, executionId: String?) -> Date? {
        guard let panel, let executionId, self.executionId(panel) == executionId else { return nil }
        let run = panel["run"] as? [String: Any] ?? [:]
        var latest: Date?
        for row in (run["activity_log"] as? [[String: Any]] ?? []) + (run["recent_activity"] as? [[String: Any]] ?? []) {
            let meta = row["metadata"] as? [String: Any] ?? [:]
            if isProgress(meta["event_type"] as? String ?? "", meta) { latest = self.latest(latest, date(row["created_at"])) }
        }
        for row in (panel["debug"] as? [String: Any])?["timeline"] as? [[String: Any]] ?? [] {
            let type = (row["title"] as? String ?? "").lowercased().replacingOccurrences(of: "[ _]+", with: ".", options: .regularExpression)
            if isProgress(type) { latest = self.latest(latest, date(row["timestamp"])) }
        }
        return latest
    }
    static func progressEvent(_ envelope: [String: Any], principal: String, workspace: String) -> (String, Date)? {
        let data = envelope["data"] as? [String: Any] ?? [:]
        let inner = envelope["event_type"] as? String == "AgentEvent" ? data["event"] as? [String: Any] : nil
        let payload = inner?["payload"] as? [String: Any] ?? data
        let correlation = payload["correlation"] as? [String: Any] ?? [:]
        guard (payload["principal"] ?? inner?["principal"] ?? data["principal"]) as? String == principal,
              (payload["workspace"] ?? inner?["workspace"] ?? data["workspace"]) as? String == workspace,
              isProgress(inner?["event_type"] as? String ?? envelope["event_type"] as? String ?? "", payload),
              let root = (payload["root_execution_id"] ?? correlation["root_execution_id"] ?? payload["execution_id"] ?? correlation["execution_id"]) as? String,
              let at = date(payload["timestamp"] ?? inner?["timestamp"] ?? data["timestamp"] ?? envelope["timestamp"])
        else { return nil }
        return (root, at)
    }
    static func history(_ body: [String: Any], executionId: String) -> [String: TaskCallUsage] {
        let columns = body["columns"] as? [String] ?? []
        var calls: [String: TaskCallUsage] = [:]
        for row in body["rows"] as? [[Any]] ?? [] {
            func value(_ key: String) -> Any? { columns.firstIndex(of: key).flatMap { $0 < row.count ? row[$0] : nil } }
            guard let id = value("llm_call_id") as? String, !id.isEmpty else { continue }
            let input = TaskUsageModel.number(value("input_tokens")), output = TaskUsageModel.number(value("output_tokens"))
            calls[id] = TaskCallUsage(tokens: input == nil && output == nil ? nil : Int((input ?? 0) + (output ?? 0)),
                                     costUsd: TaskUsageModel.number(value("cost_usd")), executionId: executionId)
        }
        return calls
    }
}

/// The app's one ledger: the live task card's `/execution-panel` poll writes
/// it, a turn's chip reads the tasks the turn started from it, and a task's
/// stored usage is loaded once per phase (web `loadTaskUsageSnapshot`).
@MainActor
final class TaskUsageStore: ObservableObject {
    static let shared = TaskUsageStore()
    @Published private(set) var ledger = TaskUsageLedger()
    private var loaded = Set<String>()
    private var inFlight: [String: Task<Void, Never>] = [:]
    private var connection = TaskUsageStore.connectionKey()
    private var subscription: RealtimeHubSubscription?
    private var readTail: Task<Void, Never>?

    init() {
        subscription = RealtimeHub.shared.subscribe(RealtimeHubFilter(), onEvent: { [weak self] event in
            let expected = Self.connectionKey()
            Task { @MainActor [weak self] in
                guard Self.connectionKey() == expected else { return }
                self?.ingest(event.object)
            }
        })
    }

    nonisolated static func connectionKey() -> String {
        MagicianAccess.baseURL.absoluteString + "|" + MagicianAccess.principal + "|" + MagicianAccess.workspace + "|" + MagicianAccess.headers().sorted { $0.key < $1.key }.map { "\($0.key)=\($0.value)" }.joined(separator: "|")
    }

    @discardableResult private func ensureConnection() -> String {
        let current = Self.connectionKey()
        if connection != current {
            connection = current
            inFlight.values.forEach { $0.cancel() }; inFlight.removeAll(); loaded.removeAll(); readTail = nil
            ledger = TaskUsageLedger()
        }
        return current
    }

    func total(for taskId: String) -> TaskUsageTotal? {
        guard connection == Self.connectionKey() else { return nil }
        return ledger.total(for: taskId)
    }

    func record(taskId: String, callKey: String, tokens: Int?, costUsd: Double?, executionId: String? = nil) {
        ensureConnection()
        var next = ledger
        if next.record(taskId: taskId, callKey: callKey, tokens: tokens, costUsd: costUsd, executionId: executionId) { ledger = next }
    }

    func apply(panel: [String: Any], taskId: String, history: [String: TaskCallUsage] = [:]) {
        ensureConnection()
        var snapshot = TaskUsageModel.usageSnapshot(fromPanel: panel)
        snapshot.calls = TaskUsageModel.mergeCalls(snapshot.calls, history)
        snapshot.run = TaskUsageModel.sumCalls(snapshot.calls)
        var next = ledger
        if next.setSnapshot(snapshot, for: taskId) { ledger = next }
    }

    /// Both typed/enveloped model responses and full panel deltas use the one hub.
    func ingest(_ event: [String: Any]) {
        ensureConnection()
        let data = event["data"] as? [String: Any] ?? [:]
        let inner = event["event_type"] as? String == "AgentEvent" ? data["event"] as? [String: Any] : nil
        let payload = inner?["payload"] as? [String: Any] ?? data
        guard (payload["principal"] ?? inner?["principal"] ?? data["principal"]) as? String == MagicianAccess.principal,
              (payload["workspace"] ?? inner?["workspace"] ?? data["workspace"]) as? String == MagicianAccess.workspace else { return }
        if event["event_type"] as? String == "ExecutionPanelDelta", let taskId = data["task_id"] as? String,
           let panel = data["state"] as? [String: Any] { apply(panel: panel, taskId: taskId); return }
        let type = inner?["event_type"] as? String ?? event["event_type"] as? String ?? ""
        guard type == "llm.succeeded" || type == "LLMResponseReceived" else { return }
        let correlation = payload["correlation"] as? [String: Any] ?? [:]
        guard TaskUsageModel.number(payload["provider_attempt_count"] ?? correlation["provider_attempt_count"]) != 0,
              let callId = (payload["llm_call_id"] ?? correlation["llm_call_id"]) as? String else { return }
        let execution = (payload["root_execution_id"] ?? correlation["root_execution_id"] ?? payload["execution_id"] ?? correlation["execution_id"]) as? String
        var tasks = Set(ledger.snapshots.filter { execution != nil && $0.value.executionId == execution }.map { $0.key })
        if let task = (payload["task_id"] ?? correlation["task_id"]) as? String { tasks.insert(task) }
        let available = (payload["usage_availability"] ?? correlation["usage_availability"]) as? [String: Any] ?? [:]
        guard payload["usage_reported"] as? Bool != false else { return }
        let input = available["tokens"] as? Bool == false ? nil : TaskUsageModel.number(payload["input_tokens"])
        let output = available["tokens"] as? Bool == false ? nil : TaskUsageModel.number(payload["output_tokens"])
        let cost = available["cost"] as? Bool == false ? nil : TaskUsageModel.number(payload["cost_usd"] ?? payload["cost"])
        for task in tasks { record(taskId: task, callKey: callId, tokens: input == nil && output == nil ? nil : Int((input ?? 0) + (output ?? 0)), costUsd: cost, executionId: execution) }
    }

    /// Shared, serial history reads with one request per task/phase; failures may retry. A connection
    /// switch invalidates in-flight results before they can enter the ledger.
    func loadSnapshot(taskId: String, phase: String) {
        let expected = ensureConnection(), key = "\(taskId)::\(phase)"
        guard !loaded.contains(key), inFlight[taskId] == nil else { return }
        loaded.insert(key)
        let previousRead = readTail
        inFlight[taskId] = Task { [weak self] in
            defer { if let self, self.connection == expected { self.inFlight.removeValue(forKey: taskId) } }
            await previousRead?.value
            guard !Task.isCancelled, Self.connectionKey() == expected else { return }
            guard let panel = await LiveTaskCardClient.json("/tasks/\(LiveTaskCardClient.segment(taskId))/execution-panel"),
                  let self, Self.connectionKey() == expected, !Task.isCancelled else {
                if let self, self.connection == expected { self.loaded.remove(key) }; return
            }
            self.apply(panel: panel, taskId: taskId)
            if let executionId = TaskRunEvidence.executionId(panel), let body = await Self.readHistory(executionId),
               Self.connectionKey() == expected, !Task.isCancelled {
                self.apply(panel: panel, taskId: taskId, history: TaskRunEvidence.history(body, executionId: executionId))
            }
        }
        readTail = inFlight[taskId]
    }

    private static func readHistory(_ executionId: String) async -> [String: Any]? {
        let expected = connectionKey(), origin = MagicianAccess.baseURL, headers = MagicianAccess.headers()
        var url = URLComponents(url: origin, resolvingAgainstBaseURL: false)
        url?.path = "/api/magician/v2/analytics/llm_calls/query"
        url?.queryItems = [URLQueryItem(name: "principal", value: MagicianAccess.principal), URLQueryItem(name: "workspace", value: MagicianAccess.workspace)]
        guard let target = url?.url else { return nil }
        let id = "'" + executionId.replacingOccurrences(of: "'", with: "''") + "'"
        let sql = "SELECT llm_call_id, input_tokens, output_tokens, cost_usd FROM llm_calls WHERE (root_execution_id = \(id) OR execution_id = \(id)) AND COALESCE(provider_attempt_count, 1) <> 0"
        var request = URLRequest(url: target); request.httpMethod = "POST"; request.timeoutInterval = 15
        headers.forEach { request.setValue($0.value, forHTTPHeaderField: $0.key) }
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: ["sql": sql])
        guard let (data, response) = try? await URLSession.shared.data(for: request), connectionKey() == expected,
              let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else { return nil }
        return try? JSONSerialization.jsonObject(with: data) as? [String: Any]
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
    var tokensReported = true

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
            costUsd: cost, spawnedTasks: 0, tokensReported: reported > 0)
    }

    /// The turn's own usage plus that of the tasks it started. Task tokens
    /// are not split into input and output; they count as output.
    static func summary(rows: [ActivityRow], taskUsage: (String) -> TaskUsageTotal?) -> TurnUsageSummary? {
        let base = own(rows)
        let taskIds = spawnedTaskIds(rows)
        guard !taskIds.isEmpty else { return base }
        var summary = base ?? TurnUsageSummary(calls: 0, inputTokens: 0, outputTokens: 0, costUsd: nil, spawnedTasks: 0, tokensReported: false)
        for usage in taskIds.compactMap(taskUsage).map(\.usage) {
            summary.tokensReported = summary.tokensReported || usage.tokensReported
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
        return ([calls(usage.calls), usage.tokensReported ? "\(compactTokens(usage.tokens)) tok" : "tokens unavailable", usd(usage.costUsd)] as [String?])
            .compactMap { $0 }
            .joined(separator: " · ")
    }

    /// The live card's spoken figure, split into Planning and Run.
    static func taskAccessibilityLabel(_ total: TaskUsageTotal) -> String {
        let usage = total.usage
        var parts = ["\(usage.calls) model call\(usage.calls == 1 ? "" : "s"), " + (usage.tokensReported ? "\(usage.tokens) tokens" : "tokens unavailable")
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
        return ([calls(summary.calls), summary.tokensReported ? "\(tokenCount(summary.tokens)) tok" : "tokens unavailable", usd(summary.costUsd), tasks] as [String?])
            .compactMap { $0 }
            .joined(separator: " · ")
    }

    /// The turn chip's short form for a narrow header: `12k tok · $0.04`.
    static func turnShortLine(_ summary: TurnUsageSummary) -> String? {
        guard summary.calls > 0 || summary.costUsd != nil else { return nil }
        return ([summary.tokensReported ? "\(tokenCount(summary.tokens)) tok" : "tokens unavailable", usd(summary.costUsd)] as [String?])
            .compactMap { $0 }
            .joined(separator: " · ")
    }

    static func turnAccessibilityLabel(_ summary: TurnUsageSummary) -> String {
        var label = "\(summary.calls) model call\(summary.calls == 1 ? "" : "s"), " + (summary.tokensReported ? "\(summary.tokens) tokens" : "tokens unavailable")
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
