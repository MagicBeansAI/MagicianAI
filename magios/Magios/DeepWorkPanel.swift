import SwiftUI
import UIKit
import MarkdownUI

struct DeepWorkTaskDetailProjection: Equatable {
    let description: String
    let agentId: String?
    let status: String
    let activeRootExecutionId: String?

    init(task: [String: Any]) throws {
        let manifest = task["manifest"] as? [String: Any] ?? task
        let state = task["state"] as? [String: Any] ?? task
        guard let status = state["status"] as? String else {
            throw URLError(.cannotParseResponse)
        }
        self.description = manifest["description"] as? String ?? ""
        self.agentId = manifest["agent_id"] as? String
        self.status = status
        self.activeRootExecutionId = TaskStatusModel.resolveActiveExecutionId(
            status: status,
            activeRootExecutionId: state["active_root_execution_id"] as? String
        )
    }

    init(description: String, agentId: String?, status: String, activeRootExecutionId: String?) {
        self.description = description
        self.agentId = agentId
        self.status = status
        self.activeRootExecutionId = TaskStatusModel.resolveActiveExecutionId(
            status: status,
            activeRootExecutionId: activeRootExecutionId
        )
    }
}

@MainActor
final class DeepWorkTaskDetailModel: ObservableObject {
    typealias Fetch = (String) async throws -> DeepWorkTaskDetailProjection

    @Published private(set) var projection: DeepWorkTaskDetailProjection
    private let fetch: Fetch
    private var requestGeneration: UInt64 = 0

    init(initial: DeepWorkTaskDetailProjection, fetch: @escaping Fetch) {
        self.projection = initial
        self.fetch = fetch
    }

    convenience init(task: TaskStatusModel, session: URLSession = .shared) {
        let initial = DeepWorkTaskDetailProjection(
            description: "",
            agentId: nil,
            status: task.status,
            activeRootExecutionId: task.activeExecutionIdForControls
        )
        self.init(initial: initial) { taskId in
            let pathCharacters = CharacterSet.alphanumerics.union(
                CharacterSet(charactersIn: "-._~")
            )
            guard let encodedTaskId = taskId.addingPercentEncoding(
                withAllowedCharacters: pathCharacters
            ), let url = URL(
                string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v3/tasks/\(encodedTaskId)"
            ) else { throw URLError(.badURL) }
            var request = URLRequest(url: url)
            MagicianAccess.authorize(&request)
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse,
                  (200..<300).contains(http.statusCode),
                  let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let task = object["task"] as? [String: Any] else {
                throw URLError(.badServerResponse)
            }
            return try DeepWorkTaskDetailProjection(task: task)
        }
    }

    func refresh(taskId: String) {
        requestGeneration &+= 1
        let generation = requestGeneration
        Task {
            guard let refreshed = try? await fetch(taskId),
                  generation == requestGeneration else { return }
            projection = refreshed
        }
    }
}

enum TaskDetailTab: String, CaseIterable, Identifiable {
    case overview, run, output, plan, history

    var id: String { rawValue }
    var title: String { rawValue.capitalized }
    var icon: String {
        switch self {
        case .overview: return "rectangle.grid.1x2"
        case .run: return "bolt.horizontal.circle"
        case .output: return "shippingbox"
        case .plan: return "list.bullet.clipboard"
        case .history: return "clock.arrow.circlepath"
        }
    }
}

/// An explicit place to open the task detail at, chosen by the entry point
/// (a card's Result button) rather than inferred from the verdict.
enum TaskDetailFocus: Equatable {
    /// The Output tab, scrolled to the Result card.
    case result

    var tab: TaskDetailTab {
        switch self {
        case .result: return .output
        }
    }

    /// Scroll anchor inside the tab.
    var anchor: String {
        switch self {
        case .result: return "task-detail-result"
        }
    }
}

extension ActId {
    /// The tab this act renders as. An exhaustive `switch` with no `default`,
    /// so a fourth act cannot silently fail to reach the strip: it fails to
    /// build until it has somewhere to go.
    var detailTab: TaskDetailTab {
        switch self {
        case .plan: return .plan
        case .run: return .run
        case .output: return .output
        }
    }
}

/// The same task-backed data powers two intentionally different destinations:
/// a durable task inspector and the execution-focused sheet opened from a
/// chat turn's Steps strip. Keeping this explicit prevents either affordance
/// from silently drifting back to the other's navigation semantics.
enum TaskDetailPresentation: Equatable {
    case taskDetails
    case runInspection

    var navigationTitle: String {
        switch self {
        case .taskDetails: return "Task"
        case .runInspection: return "Run activity"
        }
    }

    var showsTaskTabs: Bool { self == .taskDetails }
}

enum TaskDetailAction {
    case run, plan, reset, markComplete, markNotDone, cancelTask
    case approvePlan, rejectPlan, replan, retrySynthesis, delete
}

struct TaskDetailSeed: Equatable {
    let taskId: String
    let title: String
    let description: String
    let status: String
    let agentId: String?
    let threadId: String?
    let priority: String?
    let dueDate: String?
    let tags: [TaskTag]
    let lifecycle: String?
    let createdBy: String?
    let chatSessionId: String?
    let activeRootExecutionId: String?
    let latestRootExecutionId: String?
    let synthesisPending: Bool
    let synthesisFailedExecutionId: String?
    let hasPlan: Bool
    let planStatus: String?
    let planId: String?
    let createdAt: String?
    let updatedAt: String?
    let fallbackSteps: [String]
    let fallbackTerminalLines: [String]
    /// Cron expression from the list row, for the header's recurring line.
    var scheduleCron: String? = nil
    /// The list row already knew this task recurs (cron, tag or internal
    /// recurring schedule).
    var isRecurring = false

    init(_ task: TaskStatusModel) {
        taskId = task.taskId
        title = task.title
        description = ""
        status = task.status
        agentId = nil
        threadId = nil
        priority = nil
        dueDate = nil
        tags = []
        lifecycle = nil
        createdBy = nil
        chatSessionId = nil
        activeRootExecutionId = task.activeExecutionIdForControls
        // Task-status chat cards carry the concrete execution being described.
        // Preserve it as the inspection target even after the run is terminal;
        // activeRootExecutionId intentionally disappears at that point.
        latestRootExecutionId = task.executionId
        synthesisPending = false
        synthesisFailedExecutionId = nil
        hasPlan = false
        planStatus = nil
        planId = nil
        createdAt = nil
        updatedAt = nil
        fallbackSteps = task.steps
        fallbackTerminalLines = task.terminalLines
    }

    init(_ task: TaskV3) {
        taskId = task.id
        title = task.title
        description = task.description
        status = task.status
        agentId = task.agentId
        threadId = task.uiThreadId
        priority = task.priorityLabel
        dueDate = task.dueDate
        tags = task.tags
        lifecycle = task.lifecycle
        createdBy = task.createdBy
        chatSessionId = task.chatSessionId
        activeRootExecutionId = task.activeExecutionIdForControls
        latestRootExecutionId = task.latestRootExecutionId
        synthesisPending = task.synthesisPending
        synthesisFailedExecutionId = task.synthesisFailedExecutionId
        hasPlan = task.hasPlan
        planStatus = task.planStatus
        planId = task.latestPlanId
        createdAt = task.createdAt
        updatedAt = task.updatedAt
        fallbackSteps = []
        fallbackTerminalLines = []
        scheduleCron = task.scheduleCron
        isRecurring = task.isRecurring
    }
}

struct TaskDetailStep: Identifiable, Equatable {
    let id: String
    let number: Int
    let name: String
    let status: String
    let progress: String?
    let capability: String?
    let delegateAgentId: String?
    /// Backend-recorded duration for this step. Nil means the selected run did
    /// not report one; the UI must not infer it from neighbouring events.
    let durationMs: Double?
}

struct TaskDetailActivity: Identifiable, Equatable {
    let id: String
    let title: String
    let body: String?
    let status: String
    let kind: String
    let agentId: String?
    let timestamp: Date?
    let eventType: String?
    /// Backend-recorded latency for this event. This is the same measurement
    /// the web Run timeline renders at the trailing edge of a row.
    let latencyMs: Double?
    let model: String?
    /// Backend-recorded price in US dollars. The app never reconstructs this
    /// from a model name or a bundled pricing table.
    let costUsd: Double?
    let inputTokens: Int?
    let outputTokens: Int?
    let cacheReadTokens: Int?
    /// The execution that produced this row — a delegated child's id for its
    /// rows. Matched against `run.delegations` to build envelopes.
    var executionId: String? = nil
}

enum TaskTimelineFormatting {
    static let renderLimit = 200

    /// Keep mobile rendering bounded without changing the run-level totals.
    /// The caller still reports the full event count and discloses omissions.
    static func latest<T>(_ values: [T], limit: Int = renderLimit) -> [T] {
        Array(values.suffix(max(0, limit)))
    }

    /// Match the web task panel's two-unit duration grammar.
    static func duration(milliseconds: Double?) -> String? {
        guard let milliseconds, milliseconds.isFinite, milliseconds >= 0 else { return nil }
        let seconds = Int((milliseconds / 1_000).rounded())
        if seconds < 60 { return "\(seconds)s" }
        let minutes = seconds / 60
        if minutes < 60 {
            let remainder = seconds % 60
            return remainder == 0 ? "\(minutes)m" : "\(minutes)m \(remainder)s"
        }
        let hours = minutes / 60
        let remainder = minutes % 60
        return remainder == 0 ? "\(hours)h" : "\(hours)h \(remainder)m"
    }

    static func duration(seconds: TimeInterval?) -> String? {
        guard let seconds else { return nil }
        return duration(milliseconds: seconds * 1_000)
    }

    static func offset(event: Date?, origin: Date?) -> String? {
        guard let event, let origin else { return nil }
        let elapsed = event.timeIntervalSince(origin)
        guard elapsed >= 0, let value = duration(seconds: elapsed) else { return nil }
        return "+\(value)"
    }

    /// Local 24-hour wall clock with seconds, matching the web timeline's
    /// scan-friendly column while still using the device timezone.
    static func wallClock(_ date: Date?) -> String? {
        guard let date else { return nil }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = .current
        formatter.dateFormat = "HH:mm:ss"
        return formatter.string(from: date)
    }

    static func tokenCount(_ count: Int?) -> String? {
        guard let count, count >= 0 else { return nil }
        if count < 1_000 { return "\(count)" }
        if count < 1_000_000 {
            if count < 10_000 {
                // JavaScript's `toFixed(1)` rounds midpoint values away from
                // zero. Make that rule explicit so 1,250 stays in parity with
                // the web Run summary as 1.3k instead of printf's 1.2k.
                let tenths = (Double(count) / 100).rounded(.toNearestOrAwayFromZero)
                return String(format: "%.1fk", tenths / 10)
            }
            return "\(Int((Double(count) / 1_000).rounded()))k"
        }
        return String(format: "%.1fM", Double(count) / 1_000_000)
    }

    /// Render cents for ordinary totals while preserving sub-cent model-call
    /// precision. Missing, negative, or non-finite prices are not measurements.
    static func usd(_ amount: Double?) -> String? {
        guard let amount, amount.isFinite, amount >= 0 else { return nil }
        let raw = String(
            format: "%.8f",
            locale: Locale(identifier: "en_US_POSIX"),
            amount
        )
        let components = raw.split(separator: ".", omittingEmptySubsequences: false)
        guard components.count == 2 else { return nil }
        var fraction = String(components[1])
        while fraction.count > 2, fraction.last == "0" { fraction.removeLast() }
        return "$\(components[0]).\(fraction)"
    }
}

struct TaskRunSummaryRow: Identifiable, Equatable {
    let label: String
    let value: String
    var id: String { label }
}

/// The native projection of the web Run act's provenance rows. It consumes the
/// selected execution's complete activity list, not the bounded rows rendered
/// below it, so visual pagination cannot change totals.
enum TaskRunSummary {
    static func rows(
        executionId: String?,
        activity: [TaskDetailActivity],
        startedAt: Date? = nil,
        endedAt: Date? = nil
    ) -> [TaskRunSummaryRow] {
        var rows: [TaskRunSummaryRow] = []
        if let executionId, !executionId.isEmpty {
            rows.append(TaskRunSummaryRow(label: "Execution id", value: executionId))
        }

        if let cost = measuredDoubleSum(
            activity.filter { $0.kind.lowercased() == "llm" }.map(\.costUsd)
        ),
           let formatted = TaskTimelineFormatting.usd(cost) {
            rows.append(TaskRunSummaryRow(label: "Cost", value: formatted))
        }

        let billedCalls = activity.filter {
            $0.inputTokens != nil || $0.outputTokens != nil || $0.cacheReadTokens != nil
        }
        let input = measuredIntSum(activity.map(\.inputTokens))
        let output = measuredIntSum(activity.map(\.outputTokens))
        if let inputText = TaskTimelineFormatting.tokenCount(input),
           let outputText = TaskTimelineFormatting.tokenCount(output),
           !billedCalls.isEmpty {
            let noun = billedCalls.count == 1 ? "call" : "calls"
            rows.append(TaskRunSummaryRow(
                label: "Tokens",
                value: "\(inputText) → \(outputText) tok · \(billedCalls.count) \(noun)"
            ))
        }

        let cacheRead = measuredIntSum(activity.map(\.cacheReadTokens))
        if let input, input > 0, let cacheRead, cacheRead > 0,
           let cacheText = TaskTimelineFormatting.tokenCount(cacheRead),
           let inputText = TaskTimelineFormatting.tokenCount(input) {
            let rawPercent = min(100, (Double(cacheRead) / Double(input) * 100).rounded())
            if let percent = Int(exactly: rawPercent) {
                rows.append(TaskRunSummaryRow(
                    label: "Prompt cache",
                    value: "\(percent)% cached · \(cacheText) of \(inputText) tok"
                ))
            }
        }

        if let modelTimeMs = positiveDoubleSum(activity.map(\.latencyMs)),
           let modelTime = TaskTimelineFormatting.duration(milliseconds: modelTimeMs) {
            let timestamps = activity.compactMap(\.timestamp) + [startedAt, endedAt].compactMap { $0 }
            let observedMs: Double? = {
                guard let first = timestamps.min(), let last = timestamps.max() else { return nil }
                let span = last.timeIntervalSince(first) * 1_000
                return span.isFinite && span > 0 ? span : nil
            }()
            if let observedMs,
               let observed = TaskTimelineFormatting.duration(milliseconds: observedMs) {
                let rawPercent = (modelTimeMs / observedMs * 100).rounded()
                if let percent = Int(exactly: rawPercent) {
                    rows.append(TaskRunSummaryRow(
                        label: "Model time",
                        value: "\(modelTime) of \(observed) observed · \(percent)%"
                    ))
                } else {
                    rows.append(TaskRunSummaryRow(label: "Model time", value: modelTime))
                }
            } else {
                rows.append(TaskRunSummaryRow(label: "Model time", value: modelTime))
            }
        }

        let failures = activity.filter {
            $0.status.lowercased() == "failed" && $0.kind.lowercased() != "lifecycle"
        }.count
        if failures > 0 {
            rows.append(TaskRunSummaryRow(label: "Failed calls", value: "\(failures)"))
        }

        var modelOrder: [String] = []
        var modelCalls: [String: Int] = [:]
        for model in activity.compactMap(\.model) where !model.isEmpty {
            if modelCalls[model] == nil { modelOrder.append(model) }
            modelCalls[model, default: 0] += 1
        }
        let models = modelOrder.enumerated().sorted { left, right in
            let leftCalls = modelCalls[left.element] ?? 0
            let rightCalls = modelCalls[right.element] ?? 0
            return leftCalls == rightCalls ? left.offset < right.offset : leftCalls > rightCalls
        }.map { $0.element }
        if models.count == 1, let model = models.first {
            rows.append(TaskRunSummaryRow(label: "Model", value: model))
        } else if !models.isEmpty {
            rows.append(TaskRunSummaryRow(
                label: "Models",
                value: models.map { "\($0) (\(modelCalls[$0] ?? 0))" }.joined(separator: " · ")
            ))
        }
        return rows
    }

    private static func measuredIntSum(_ values: [Int?]) -> Int? {
        let measured = values.compactMap { $0 }
        guard !measured.isEmpty else { return nil }
        var total = 0
        for value in measured {
            let (next, overflow) = total.addingReportingOverflow(value)
            guard !overflow else { return nil }
            total = next
        }
        return total
    }

    private static func measuredDoubleSum(_ values: [Double?]) -> Double? {
        let measured = values.compactMap { value -> Double? in
            guard let value, value.isFinite, value >= 0 else { return nil }
            return value
        }
        guard !measured.isEmpty else { return nil }
        let total = measured.reduce(0, +)
        return total.isFinite ? total : nil
    }

    private static func positiveDoubleSum(_ values: [Double?]) -> Double? {
        let measured = values.compactMap { value -> Double? in
            guard let value, value.isFinite, value > 0 else { return nil }
            return value
        }
        guard !measured.isEmpty else { return nil }
        let total = measured.reduce(0, +)
        return total.isFinite ? total : nil
    }
}

struct TaskDetailQuestion: Identifiable, Equatable {
    let id: String
    let text: String
    let status: String
    let options: [String]
}

struct TaskDetailResponsibilityChild: Identifiable, Equatable {
    let id: String
    let title: String
    let owner: String
    let waitingState: String
    let isBlocking: Bool
}

struct TaskDetailResponsibility: Equatable {
    let summary: String
    let owner: String
    let waitingState: String
    let stage: String?
    let provider: String?
    let children: [TaskDetailResponsibilityChild]
}

struct TaskDetailArtifact: Identifiable, Equatable {
    let id: String
    let relativePath: String
    let mediaType: String?
    let role: String?
    let audience: String?
    let sizeBytes: Int?
    let bodySnippet: String?
    let sourceExecutionId: String?
    /// Who owns the file: task deliverable or one of the selected run's
    /// intermediate scopes. Task-level sources default to `.task`.
    var scope: TaskOutputScope = .task

    var displayName: String { (relativePath as NSString).lastPathComponent }
}

struct TaskDetailExecution: Identifiable, Equatable {
    let id: String
    let status: String
    let agentId: String?
    let startedAt: Date?
    let endedAt: Date?
    let summary: String?
    let outcome: String?
    let error: String?
    let progress: Int?
    let outputCount: Int
    let persistedArtifactCount: Int
    let outputPaths: [String]
    let persistedArtifactLabels: [String]
}

struct TaskDetailShellEntry: Identifiable, Equatable {
    let id: String
    let command: String
    let lines: [String]
    let exitCode: Int?
    let complete: Bool
}

struct TaskDetailSnapshot: Equatable {
    var title: String
    var description: String
    var status: String
    var agentId: String?
    var threadId: String?
    var priority: String?
    var dueDate: String?
    var tags: [TaskTag]
    var lifecycle: String?
    var createdBy: String?
    var chatSessionId: String?
    var activeRootExecutionId: String?
    var latestRootExecutionId: String?
    var selectedExecutionId: String?
    var synthesisPending: Bool
    var synthesisFailedExecutionId: String?
    var progress: Int?
    var currentStep: Int?
    var createdAt: Date?
    var updatedAt: Date?
    /// When the run last actually *advanced* — a step started or completed.
    /// Deliberately not `updatedAt`, which moves on any write: stall detection
    /// reading it would let a wedged run refresh its own liveness and never
    /// report stalled (design §5).
    var lastProgressAt: Date?
    var runSummary: String?
    var resultSummary: String?
    var resultOutcome: String?
    var selectedExecutionStatus: String?
    var selectedExecutionError: String?
    var selectedExecutionStartedAt: Date?
    var selectedExecutionEndedAt: Date?
    var planStatus: String?
    var planId: String?
    var planMarkdown: String?
    var steps: [TaskDetailStep]
    var activity: [TaskDetailActivity]
    var questions: [TaskDetailQuestion]
    var attentionCount: Int
    /// The ask blocking this task, when the run carries one that a human can
    /// answer. `attentionCount` counts the whole attention list — which also
    /// holds informational rows such as terminal failures — so it cannot decide
    /// this; `parseRunAttention` filters on `hitl_request`, which the backend
    /// embeds on exactly the rows that can be responded to.
    var attention: VerdictAttention?
    var responsibility: TaskDetailResponsibility?
    var artifacts: [TaskDetailArtifact]
    var history: [TaskDetailExecution]
    var shellEntries: [TaskDetailShellEntry]
    var observationCount: Int
    var linkedInputCount: Int
    var deliveryCount: Int
    var unavailableSections: [String]
    var taskHasPlan: Bool
    /// This task has a Run act. False only when the run payload failed to load
    /// *and* nothing else observed an execution — a missing act is absent, not
    /// empty, and an empty Run act asserts "nothing has happened" (design §6).
    var hasRunAct: Bool
    /// This task has an Output act — **true with zero files**, which is what
    /// lets that act say `no output`. False only when the outputs payload
    /// failed to load and no file reached us another way, because then we never
    /// observed what this task produced.
    var hasOutputAct: Bool
    /// Delegated children contributing rows to `activity` (`run.delegations`).
    var delegations: [TaskDelegationGroup] = []
    /// Persisted artifacts of the selected run that have no file to open.
    var structuredArtifacts: [TaskDetailStructuredArtifact] = []
    var scheduleCron: String? = nil
    var isRecurring = false

    var hasPlan: Bool {
        taskHasPlan || planStatus != nil || planId != nil || planMarkdown?.isEmpty == false
    }

    /// Output tab grouping: deliverables first, run-owned files and evidence
    /// behind the "Intermediate artifacts & evidence" disclosure.
    var outputGroups: TaskOutputGroups {
        TaskOutputGroups.group(artifacts, structured: structuredArtifacts)
    }

    /// Human cadence for the header, or nil when the task does not recur.
    var recurrenceDescription: String? {
        isRecurring ? TaskRecurrence.description(cron: scheduleCron) : nil
    }

    /// The header's primary button is "Reset to Ready". The overflow menu
    /// hides its own Reset then, so the action lives in exactly one place.
    /// Mirrors the branch order of `DeepWorkPanel.primaryTaskAction`.
    var headerOffersReset: Bool {
        guard activeRootExecutionId == nil else { return false }
        if ["planning", "eliciting", "draft"].contains(planStatus ?? "") { return false }
        if planStatus == "approved" && status == "ready" { return false }
        return ["paused", "failed", "cancelled", "canceled"].contains(status.lowercased())
    }

    /// The acts this task has, in lifecycle order.
    var acts: [ActId] {
        TaskCapabilities.deriveActs(ActCapabilities(
            hasPlanAct: hasPlan, hasRunAct: hasRunAct, hasOutputAct: hasOutputAct
        ))
    }

    /// The tab strip: the acts the task has, in lifecycle order, wrapped by the
    /// two tabs that are not acts. Overview is the task itself rather than a
    /// stage of it, and History is every run rather than this one — neither has
    /// a position in the lifecycle, so neither goes through `deriveActs`.
    var visibleTabs: [TaskDetailTab] {
        [.overview] + acts.map(\.detailTab) + [.history]
    }

    /// How long the selected run took. `nil` rather than a guess when either
    /// end is missing — a live run has no end, and an approximated duration is
    /// the thing this whole layer refuses to render.
    var runElapsed: TimeInterval? {
        guard let started = selectedExecutionStartedAt,
              let ended = selectedExecutionEndedAt else { return nil }
        return ended.timeIntervalSince(started)
    }

    /// The honest origin for per-event `+elapsed` labels: the selected run's
    /// recorded start when present, otherwise the first timed event. This is
    /// the native equivalent of the web timeline's earliest-recorded-instant
    /// rule and deliberately returns nil when the wire supplied no clock.
    var timelineOrigin: Date? {
        selectedExecutionStartedAt
            ?? activity.compactMap(\.timestamp).min()
    }

    /// A duration suitable for the Run summary. Terminal runs require their
    /// recorded end; active runs may use the current clock so the value remains
    /// live even between pushed events.
    func runDuration(at now: Date) -> TimeInterval? {
        guard let started = selectedExecutionStartedAt else { return nil }
        if let ended = selectedExecutionEndedAt {
            return max(0, ended.timeIntervalSince(started))
        }
        let running = ["running", "planning", "paused", "deferred"]
            .contains((selectedExecutionStatus ?? status).lowercased())
        return running ? max(0, now.timeIntervalSince(started)) : nil
    }

    /// Replace the live execution slice with a pushed full panel snapshot while
    /// retaining task details and downloaded outputs that are not part of the
    /// realtime contract. `ExecutionPanelDelta` is a snapshot, not a patch: an
    /// empty activity/questions array therefore clears the old value.
    func applyingRealtimePanel(
        seed: TaskDetailSeed,
        panelPayload: [String: Any]
    ) -> TaskDetailSnapshot? {
        let overview = Self.dictionary(panelPayload["overview"]) ?? [:]
        let debug = Self.dictionary(panelPayload["debug"]) ?? [:]
        let selected = Self.dictionary(debug["selected_execution"]) ?? [:]
        let incomingExecutionId = Self.string(overview["execution_id"])
            ?? Self.string(selected["execution_id"])
        guard let selectedExecutionId, incomingExecutionId == selectedExecutionId else {
            return nil
        }

        let live = Self.parse(
            seed: seed,
            taskPayload: nil,
            panelPayload: panelPayload,
            outputsPayload: nil,
            detailsPayload: nil,
            planPayload: nil
        )
        var merged = self
        merged.title = live.title
        merged.description = live.description
        merged.status = live.status
        merged.agentId = live.agentId
        merged.threadId = live.threadId
        merged.priority = live.priority
        merged.progress = live.progress
        merged.currentStep = live.currentStep
        merged.createdAt = live.createdAt ?? createdAt
        merged.updatedAt = live.updatedAt ?? updatedAt
        merged.runSummary = live.runSummary
        merged.resultSummary = live.resultSummary ?? resultSummary
        merged.resultOutcome = live.resultOutcome ?? resultOutcome
        merged.selectedExecutionStatus = live.selectedExecutionStatus
        merged.selectedExecutionError = live.selectedExecutionError
        merged.selectedExecutionStartedAt = live.selectedExecutionStartedAt
            ?? selectedExecutionStartedAt
        merged.selectedExecutionEndedAt = live.selectedExecutionEndedAt
        merged.planId = live.planId ?? planId
        merged.planMarkdown = live.planMarkdown ?? planMarkdown
        merged.steps = live.steps
        merged.activity = live.activity
        merged.questions = live.questions
        merged.attentionCount = live.attentionCount
        merged.attention = live.attention
        merged.responsibility = live.responsibility
        merged.history = Self.mergeHistory(live.history, history)
        merged.shellEntries = live.shellEntries
        merged.observationCount = live.observationCount
        merged.linkedInputCount = live.linkedInputCount
        merged.deliveryCount = live.deliveryCount
        merged.unavailableSections.removeAll { $0 == "live run" }
        merged.taskHasPlan = live.taskHasPlan
        merged.hasRunAct = true
        merged.hasOutputAct = hasOutputAct || live.hasOutputAct
        merged.delegations = live.delegations
        // Task deliverables are not part of the realtime contract; the run's
        // own outputs are, but only when this snapshot carried them.
        if Self.carriesRunOutputs(panelPayload) {
            merged.artifacts = artifacts.filter { $0.scope == .task }
                + live.artifacts.filter { $0.scope != .task }
            merged.structuredArtifacts = live.structuredArtifacts
        }
        return merged
    }

    /// What the run is on right now, named. Only the indexed match: with no
    /// index there is no step to name, and picking one by status would invent a
    /// second progress model beside `currentStep`.
    var currentStepLabel: String? {
        guard let index = currentStep else { return nil }
        return steps.first { $0.number == index }?.name
    }

    /// Everything the verdict is derived from, as one value. Split out from
    /// `verdict(now:)` so the mapping — and especially the 0-based wire step
    /// becoming the 1-based one a reader counts — is assertable on its own.
    func verdictInput(now: Date) -> VerdictInput {
        VerdictInput(
            status: status,
            attention: attention,
            error: selectedExecutionError,
            // `overview.current_step` is 0-based, as `Step \(n + 1)` elsewhere
            // in this file attests. The verdict line counts the way a person
            // does.
            currentStep: currentStep.map { $0 + 1 },
            totalSteps: steps.isEmpty ? nil : steps.count,
            currentStepLabel: currentStepLabel,
            elapsed: runElapsed,
            lastProgressAt: lastProgressAt,
            now: now
        )
    }

    func verdict(now: Date) -> Verdict {
        TaskVerdict.derive(verdictInput(now: now))
    }

    /// Which tab opens first. Derived from the verdict and the acts the task
    /// has — **the only thing that decides this**. The payload also carries a
    /// `default_tab`, and reading both would be two mechanisms doing one job
    /// with nothing pinning either.
    func defaultOpenTab(now: Date) -> TaskDetailTab {
        let act = TaskCapabilities.defaultOpenAct(
            state: verdict(now: now).state, acts: acts, attention: attention?.source
        )
        return act?.detailTab ?? .overview
    }

    /// The tab to open for an entry point. An explicit focus wins when its tab
    /// exists; otherwise the verdict-derived default applies.
    func openTab(focus: TaskDetailFocus?, now: Date) -> TaskDetailTab {
        if let focus, visibleTabs.contains(focus.tab) { return focus.tab }
        return defaultOpenTab(now: now)
    }

    static func parse(
        seed: TaskDetailSeed,
        taskPayload: [String: Any]?,
        panelPayload: [String: Any]?,
        outputsPayload: [String: Any]?,
        detailsPayload: [String: Any]?,
        planPayload: [String: Any]?,
        unavailableSections: [String] = []
    ) -> TaskDetailSnapshot {
        let taskRecord = dictionary(taskPayload?["task"]) ?? dictionary(detailsPayload?["task"]) ?? [:]
        let manifest = dictionary(taskRecord["manifest"]) ?? taskRecord
        let state = dictionary(taskRecord["state"]) ?? taskRecord
        let refs = dictionary(taskRecord["refs"]) ?? [:]
        let overview = dictionary(panelPayload?["overview"]) ?? [:]
        let run = dictionary(panelPayload?["run"]) ?? [:]
        let output = dictionary(panelPayload?["output"]) ?? [:]
        let debug = dictionary(panelPayload?["debug"]) ?? [:]
        let selectedExecution = dictionary(debug["selected_execution"]) ?? [:]
        let plan = dictionary(planPayload?["plan"]) ?? [:]

        let title = string(overview["title"]) ?? string(manifest["title"]) ?? seed.title
        let description = string(overview["description"])
            ?? string(manifest["description"]) ?? seed.description
        let status = string(overview["status"]) ?? string(state["status"]) ?? seed.status
        let selectedExecutionId = string(selectedExecution["execution_id"])
            ?? string(overview["execution_id"]) ?? seed.latestRootExecutionId
        let result = dictionary(output["result"])
        let taskplan = dictionary(debug["taskplan"])

        var steps = parseSteps(selectedExecution["step_statuses"])
        if steps.isEmpty {
            steps = seed.fallbackSteps.enumerated().map {
                TaskDetailStep(id: "fallback-\($0.offset)", number: $0.offset,
                               name: $0.element, status: "info", progress: nil,
                               capability: nil, delegateAgentId: nil, durationMs: nil)
            }
        }

        var activity = parseActivity(run["activity_log"])
        if activity.isEmpty { activity = parseActivity(run["recent_activity"]).reversed() }
        if activity.isEmpty { activity = parseTimeline(debug["timeline"]) }
        if activity.isEmpty {
            activity = seed.fallbackTerminalLines.enumerated().map {
                TaskDetailActivity(id: "terminal-\($0.offset)", title: "Terminal output",
                                   body: $0.element, status: "info", kind: "tool",
                                   agentId: nil, timestamp: nil, eventType: nil,
                                   latencyMs: nil, model: nil, costUsd: nil,
                                   inputTokens: nil, outputTokens: nil, cacheReadTokens: nil)
            }
        }

        let runQuestions = parseQuestions(run["pending_questions"])
        let planQuestions = parseQuestions(plan["pending_questions"])
        let questions = deduplicateQuestions(runQuestions + planQuestions)
        let attentionItems = array(run["needs_attention"])
        let attention = attentionItems.count
        let runAttention = parseRunAttention(attentionItems)

        var artifacts = parseArtifacts(dictionary(outputsPayload?["outputs"])?["outputs"])
        artifacts += parseArtifacts(refs["outputs"])
        if let detailTask = dictionary(detailsPayload?["task"]),
           let detailRefs = dictionary(detailTask["refs"]) {
            artifacts += parseArtifacts(detailRefs["outputs"])
        }
        // The selected run's own outputs, scoped by the array they arrive in
        // (web `selectedRunOutputs`). Both arrays are always serialized by a
        // current backend; their absence marks an older payload whose run-level
        // ownership cannot be inferred, so nothing is added then.
        var structuredArtifacts: [TaskDetailStructuredArtifact] = []
        if let runOutputs = parseRunOutputs(output) {
            artifacts += runOutputs.files
            structuredArtifacts = runOutputs.structured
        }
        artifacts = deduplicateArtifacts(artifacts)

        let panelHistory = parseRecentRuns(output["recent_runs"])
        let detailHistory = parseExecutionDetails(detailsPayload?["executions"])
        let history = mergeHistory(panelHistory, detailHistory)

        // A missing act is absent, not empty (design §6). `panelPayload == nil`
        // and `outputsPayload == nil` ARE the load failures — the caller passes
        // nil for a request that did not answer — so the joint is read here
        // rather than re-derived from the human-readable `unavailableSections`
        // strings beside it.
        let hasRunAct = panelPayload != nil
            || selectedExecutionId != nil || !steps.isEmpty || !activity.isEmpty || !history.isEmpty
        let hasOutputAct = outputsPayload != nil || !artifacts.isEmpty
            || !structuredArtifacts.isEmpty || result != nil

        return TaskDetailSnapshot(
            title: title,
            description: description,
            status: status,
            agentId: string(overview["active_agent_id"]) ?? string(overview["assigned_agent_id"])
                ?? string(manifest["agent_id"]) ?? seed.agentId,
            threadId: string(overview["ui_thread_id"]) ?? string(manifest["ui_thread_id"]) ?? seed.threadId,
            priority: string(overview["priority"]) ?? string(manifest["priority"]) ?? seed.priority,
            dueDate: string(manifest["due_date"]) ?? seed.dueDate,
            tags: parseTags(manifest["tags"], fallback: seed.tags),
            lifecycle: string(manifest["lifecycle"]) ?? seed.lifecycle,
            createdBy: string(manifest["created_by"]) ?? seed.createdBy,
            chatSessionId: string(manifest["chat_session_id"]) ?? seed.chatSessionId,
            activeRootExecutionId: TaskStatusModel.resolveActiveExecutionId(
                status: status,
                activeRootExecutionId: string(state["active_root_execution_id"])
                    ?? seed.activeRootExecutionId
            ),
            latestRootExecutionId: string(state["latest_root_execution_id"])
                ?? seed.latestRootExecutionId,
            selectedExecutionId: selectedExecutionId,
            synthesisPending: (bool(state["synthesis_pending"])
                ?? ((state["synthesis_pending_executions"] as? [Any])?.isEmpty == false))
                || seed.synthesisPending,
            synthesisFailedExecutionId: string(state["synthesis_failed_execution_id"])
                ?? seed.synthesisFailedExecutionId,
            progress: integer(overview["progress"]) ?? integer(selectedExecution["progress"]),
            currentStep: integer(overview["current_step"]) ?? integer(selectedExecution["current_step"]),
            createdAt: date(overview["created_at"]) ?? date(manifest["created_at"])
                ?? seed.createdAt.flatMap(TaskV3.parseISO),
            updatedAt: date(overview["updated_at"]) ?? date(state["updated_at"])
                ?? seed.updatedAt.flatMap(TaskV3.parseISO),
            lastProgressAt: date(state["last_progress_at"]),
            runSummary: string(run["summary"]),
            resultSummary: string(result?["summary"]),
            resultOutcome: string(result?["outcome"]),
            selectedExecutionStatus: string(selectedExecution["status"]),
            selectedExecutionError: string(selectedExecution["error_message"])
                ?? string(debug["latest_error_message"]),
            selectedExecutionStartedAt: date(selectedExecution["started_at"]),
            selectedExecutionEndedAt: date(selectedExecution["ended_at"]),
            planStatus: string(plan["status"]) ?? seed.planStatus,
            planId: string(plan["plan_id"]) ?? string(selectedExecution["plan_id"]) ?? seed.planId,
            planMarkdown: string(taskplan?["markdown"]),
            steps: steps,
            activity: activity,
            questions: questions,
            attentionCount: attention,
            attention: runAttention,
            responsibility: parseResponsibility(run["responsibility"]),
            artifacts: artifacts,
            history: history,
            shellEntries: parseShellEntries(debug["shell_entries"]),
            observationCount: array(debug["observations"]).count,
            linkedInputCount: array(selectedExecution["linked_inputs"]).count,
            deliveryCount: array(output["deliveries"]).count,
            unavailableSections: unavailableSections,
            taskHasPlan: bool(overview["has_plan"]) ?? seed.hasPlan,
            hasRunAct: hasRunAct,
            hasOutputAct: hasOutputAct,
            delegations: parseDelegations(run["delegations"]),
            structuredArtifacts: structuredArtifacts,
            scheduleCron: seed.scheduleCron ?? cronExpression(manifest["schedule"]),
            isRecurring: seed.isRecurring
                || TaskRecurrence.isRecurring(
                    cron: cronExpression(manifest["schedule"]),
                    tags: parseTags(manifest["tags"], fallback: []),
                    hasRecurringSchedule: dictionary(detailsPayload?["recurring_schedule"]) != nil
                )
        )
    }

    /// `schedule.kind.Cron.expression` from the externally-tagged schedule.
    private static func cronExpression(_ value: Any?) -> String? {
        guard let schedule = dictionary(value),
              let kind = dictionary(schedule["kind"]),
              let cron = dictionary(kind["Cron"]) else { return nil }
        return string(cron["expression"])
    }

    private static func parseDelegations(_ value: Any?) -> [TaskDelegationGroup] {
        array(value).compactMap { item in
            guard let executionId = string(item["execution_id"]) else { return nil }
            return TaskDelegationGroup(
                executionId: executionId,
                agentId: string(item["agent_id"]) ?? "agent",
                status: string(item["status"]) ?? "unknown",
                entryCount: integer(item["entry_count"]) ?? 0,
                parentExecutionId: string(item["parent_execution_id"]),
                startedAt: instant(item["started_at"]),
                completedAt: instant(item["completed_at"])
            )
        }
    }

    /// Whether this panel payload carries the selected run's output arrays at all.
    static func carriesRunOutputs(_ panelPayload: [String: Any]) -> Bool {
        let output = dictionary(panelPayload["output"]) ?? [:]
        return output["selected_execution_outputs"] is [Any] && output["selected_child_outputs"] is [Any]
    }

    private static func parseRunOutputs(
        _ output: [String: Any]
    ) -> (files: [TaskDetailArtifact], structured: [TaskDetailStructuredArtifact])? {
        guard output["selected_execution_outputs"] is [Any],
              output["selected_child_outputs"] is [Any] else { return nil }
        var files = parseArtifacts(output["selected_execution_outputs"], scope: .execution)
            + parseArtifacts(output["selected_child_outputs"], scope: .delegated)
        var structured: [TaskDetailStructuredArtifact] = []
        for (offset, item) in array(output["selected_execution_artifacts"]).enumerated() {
            let id = string(item["artifact_id"]) ?? "run-artifact-\(offset)"
            let path = string(item["relative_path"])
            let name = string(item["display_name"])
                ?? path.map { ($0 as NSString).lastPathComponent } ?? id
            if let path {
                files.append(TaskDetailArtifact(
                    id: "artifact-\(id)", relativePath: path,
                    mediaType: string(item["content_type"]), role: string(item["artifact_type"]),
                    audience: nil, sizeBytes: integer(item["size_bytes"]), bodySnippet: nil,
                    sourceExecutionId: string(item["source_execution_id"]), scope: .artifact
                ))
            } else {
                structured.append(TaskDetailStructuredArtifact(
                    id: id, name: name, artifactType: string(item["artifact_type"]),
                    contentType: string(item["content_type"]), producedAt: string(item["produced_at"])
                ))
            }
        }
        return (files, structured)
    }

    private static func dictionary(_ value: Any?) -> [String: Any]? { value as? [String: Any] }
    private static func array(_ value: Any?) -> [[String: Any]] { value as? [[String: Any]] ?? [] }
    private static func string(_ value: Any?) -> String? {
        guard let value = value as? String else { return nil }
        let clean = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return clean.isEmpty ? nil : clean
    }
    private static func bool(_ value: Any?) -> Bool? {
        if let value = value as? Bool { return value }
        if let value = value as? NSNumber { return value.boolValue }
        return nil
    }
    private static func integer(_ value: Any?) -> Int? {
        if let value = value as? Int { return value }
        if let value = value as? NSNumber { return value.intValue }
        if let value = value as? String { return Int(value) }
        return nil
    }
    private static func number(_ value: Any?) -> Double? {
        if let value = value as? Double, value.isFinite { return value }
        if let value = value as? NSNumber {
            let number = value.doubleValue
            return number.isFinite ? number : nil
        }
        if let value = value as? String, let number = Double(value), number.isFinite {
            return number
        }
        return nil
    }
    private static func date(_ value: Any?) -> Date? {
        if let value = value as? String { return TaskV3.parseISO(value) }
        guard let number = value as? NSNumber else { return nil }
        let raw = number.doubleValue
        return Date(timeIntervalSince1970: raw > 10_000_000_000 ? raw / 1_000 : raw)
    }

    private static func parseTags(_ value: Any?, fallback: [TaskTag]) -> [TaskTag] {
        let parsed = array(value).compactMap { item -> TaskTag? in
            guard let name = string(item["name"]) else { return nil }
            return TaskTag(id: string(item["id"]) ?? name, name: name, color: string(item["color"]))
        }
        return parsed.isEmpty ? fallback : parsed
    }

    private static func parseSteps(_ value: Any?) -> [TaskDetailStep] {
        array(value).enumerated().map { offset, item in
            let stepNumber = integer(item["number"]) ?? offset
            return TaskDetailStep(
                id: string(item["step_id"]) ?? "step-\(stepNumber)",
                number: stepNumber,
                name: string(item["name"]) ?? "Step \(stepNumber + 1)",
                status: string(item["status"]) ?? "pending",
                progress: string(item["progress"]),
                capability: string(item["capability"]),
                delegateAgentId: string(item["delegate_agent_id"]),
                durationMs: number(item["duration_ms"])
            )
        }
    }

    private static func parseActivity(_ value: Any?) -> [TaskDetailActivity] {
        array(value).enumerated().map { offset, item in
            let metadata = dictionary(item["metadata"]) ?? [:]
            let eventType = string(metadata["event_type"])
            return TaskDetailActivity(
                id: string(item["id"]) ?? "activity-\(offset)",
                title: activityTitle(
                    fallback: string(item["title"]) ?? "Activity",
                    eventType: eventType,
                    metadata: metadata
                ),
                body: string(item["summary"]),
                status: string(item["status"]) ?? "info",
                kind: activityKind(eventType: eventType, fallback: string(item["item_type"])),
                agentId: string(item["agent_id"]),
                timestamp: date(item["created_at"]) ?? date(item["updated_at"]),
                eventType: eventType,
                latencyMs: number(metadata["latency_ms"]),
                model: string(metadata["model"]),
                costUsd: eventType?.hasPrefix("llm.") == true
                    ? (number(metadata["cost_usd"]) ?? number(metadata["cost"]))
                    : nil,
                inputTokens: integer(metadata["input_tokens"]),
                outputTokens: integer(metadata["output_tokens"]),
                cacheReadTokens: integer(metadata["cache_read_tokens"]),
                executionId: string(metadata["execution_id"]) ?? string(item["execution_id"])
            )
        }
    }

    private static func activityKind(eventType: String?, fallback: String?) -> String {
        guard let eventType else { return fallback ?? "event" }
        if eventType.hasPrefix("llm.") { return "llm" }
        if eventType.hasPrefix("reasoning") { return "reasoning" }
        if eventType.hasPrefix("tool.") { return "tool" }
        return "event"
    }

    private static func activityTitle(
        fallback: String,
        eventType: String?,
        metadata: [String: Any]
    ) -> String {
        guard let eventType else { return fallback }
        let tool = string(metadata["target"])
            ?? string(metadata["tool_name"])
            ?? string(metadata["action_type"])
        let capability = string(metadata["capability"])
        switch eventType {
        case "tool.succeeded": return tool.map { "\($0) returned" } ?? fallback
        case "tool.failed": return tool.map { "\($0) failed" } ?? fallback
        case "tool.started", "tool.requested":
            return tool.map { "Calling \($0)" } ?? fallback
        case "llm.requested", "llm.succeeded", "llm.failed":
            return capability.map { "Thinking with \($0)" } ?? fallback
        default: return fallback
        }
    }

    private static func parseTimeline(_ value: Any?) -> [TaskDetailActivity] {
        array(value).enumerated().map { offset, item in
            TaskDetailActivity(
                id: string(item["id"]) ?? "timeline-\(offset)",
                title: string(item["title"]) ?? "Event",
                body: string(item["message"]),
                status: string(item["severity"]) ?? "info",
                kind: "event",
                agentId: string(item["agent_id"]),
                timestamp: date(item["timestamp"]),
                eventType: nil, latencyMs: nil, model: nil, costUsd: nil,
                inputTokens: nil, outputTokens: nil, cacheReadTokens: nil,
                executionId: string(item["execution_id"])
            )
        }
    }

    /// A wire instant, rejecting the zero sentinel. `0` is how an unset
    /// timestamp reaches us as a number, and reading it as 1970 would make a
    /// fresh ask look 56 years old.
    private static func instant(_ value: Any?) -> Date? {
        if let number = value as? NSNumber, number.doubleValue == 0 { return nil }
        return date(value)
    }

    /// The first attention item that is actually an ask, as a `VerdictAttention`.
    ///
    /// **The filter is `hitl_request`, and that is the whole rule.** The
    /// attention list a task carries is wider than "a human must answer": it
    /// also holds informational rows, notably terminal execution failures.
    /// Ranking one of those as `waiting` would paint a **failed** task
    /// `Waiting on you` and hide the error message. The backend embeds a
    /// canonical `hitl_request` on exactly the rows that can be responded to,
    /// so its presence is the signal.
    ///
    /// A source this client does not model is skipped rather than coerced —
    /// `HitlSource(rawValue:)` is the check — and the verdict then falls back to
    /// exactly what the task's own status earns.
    private static func parseRunAttention(_ items: [[String: Any]]) -> VerdictAttention? {
        for item in items {
            guard let request = dictionary(item["hitl_request"]),
                  let raw = string(request["source"]),
                  let source = HitlSource(rawValue: raw) else { continue }
            return VerdictAttention(
                source: source,
                // The prompt is the ask in the words the backend chose; the feed
                // item's title is a lane label ("Approval requested") and says
                // less. Falling back to it rather than to the per-source copy
                // would substitute a generic sentence for a generic sentence, so
                // nil is left for `derive` to fill from the source it models.
                summary: string(request["prompt"]) ?? string(item["summary"]),
                // The request's own instant first: `created_at` is when the feed
                // *row* was written, which is the same moment often enough to be
                // tempting and not always. Both omitted rather than approximated
                // when neither is readable.
                raisedAt: instant(request["at"]) ?? instant(item["created_at"])
            )
        }
        return nil
    }

    private static func parseQuestions(_ value: Any?) -> [TaskDetailQuestion] {
        array(value).enumerated().compactMap { offset, item in
            guard let text = string(item["question_text"])
                ?? string(item["question"]) ?? string(item["prompt"]) else { return nil }
            let options = array(item["options"]).compactMap { string($0["label"]) ?? string($0["value"]) }
            return TaskDetailQuestion(id: string(item["id"]) ?? "question-\(offset)", text: text,
                                      status: string(item["status"]) ?? "pending", options: options)
        }
    }

    private static func deduplicateQuestions(_ questions: [TaskDetailQuestion]) -> [TaskDetailQuestion] {
        var seen = Set<String>()
        return questions.filter { seen.insert($0.id).inserted }
    }

    private static func parseResponsibility(_ value: Any?) -> TaskDetailResponsibility? {
        guard let item = dictionary(value) else { return nil }
        let children = array(item["active_children"]).enumerated().map { offset, child in
            let id = string(child["execution_id"]) ?? "child-\(offset)"
            return TaskDetailResponsibilityChild(
                id: id,
                title: string(child["title"]) ?? id,
                owner: string(child["active_owner_agent_id"]) ?? "unknown",
                waitingState: string(child["waiting_state"]) ?? "unknown",
                isBlocking: bool(child["is_blocking"]) ?? false
            )
        }
        return TaskDetailResponsibility(
            summary: string(item["responsibility_summary"]) ?? "Execution responsibility",
            owner: string(item["active_owner_agent_id"]) ?? "unknown",
            waitingState: string(item["waiting_state"]) ?? "unknown",
            stage: string(item["current_stage"]), provider: string(item["current_provider"]),
            children: children
        )
    }

    /// `scope` is the source's scope: every row of the task outputs endpoint
    /// is a task deliverable (web forces `task` there too), and each run-level
    /// array names its own. A row's `scope` field is its writer scope, not a
    /// regrouping instruction, so it does not override the source.
    private static func parseArtifacts(_ value: Any?, scope: TaskOutputScope = .task) -> [TaskDetailArtifact] {
        array(value).enumerated().compactMap { offset, item in
            guard let path = string(item["relative_path"]) ?? string(item["artifact_path"]) else { return nil }
            return TaskDetailArtifact(
                id: string(item["output_id"]) ?? string(item["id"]) ?? "\(path)-\(offset)",
                relativePath: path,
                mediaType: string(item["media_type"]) ?? string(item["mime_type"]),
                role: string(item["role"]) ?? string(item["class"]),
                audience: string(item["audience"]),
                sizeBytes: integer(item["size_bytes"]),
                bodySnippet: string(item["body_snippet"]),
                sourceExecutionId: string(item["source_execution_id"]),
                scope: scope
            )
        }
    }

    private static func deduplicateArtifacts(_ artifacts: [TaskDetailArtifact]) -> [TaskDetailArtifact] {
        // Per scope: a promoted deliverable and the run file it came from are
        // two rows on the web as well.
        var seen = Set<String>()
        return artifacts.filter { seen.insert("\($0.scope.rawValue)|\($0.relativePath)").inserted }
    }

    private static func parseRecentRuns(_ value: Any?) -> [TaskDetailExecution] {
        array(value).enumerated().compactMap { offset, item in
            guard let id = string(item["execution_id"]) else { return nil }
            return TaskDetailExecution(
                id: id, status: string(item["status"]) ?? "unknown", agentId: nil,
                startedAt: date(item["started_at"]), endedAt: date(item["ended_at"]),
                summary: string(item["completion_summary"]), outcome: string(item["completion_outcome"]),
                error: string(item["error_message"]), progress: integer(item["progress"]),
                outputCount: (item["completion_artifact_names"] as? [Any])?.count ?? 0,
                persistedArtifactCount: 0,
                outputPaths: item["completion_artifact_names"] as? [String] ?? [],
                persistedArtifactLabels: []
            )
        }
    }

    private static func parseExecutionDetails(_ value: Any?) -> [TaskDetailExecution] {
        array(value).compactMap { item in
            guard let state = dictionary(item["state"]), let id = string(state["execution_id"]) else { return nil }
            let refs = dictionary(item["refs"]) ?? [:]
            let outputItems = array(refs["output_refs"]) + array(refs["child_output_refs"])
            let outputPaths = outputItems.compactMap {
                string($0["relative_path"]) ?? string($0["artifact_path"])
            }
            let persistedArtifactLabels = array(item["artifacts"]).enumerated().map { offset, artifact in
                string(artifact["relative_path"]) ?? string(artifact["artifact_path"])
                    ?? string(artifact["path"]) ?? string(artifact["name"])
                    ?? string(artifact["id"]) ?? "Artifact \(offset + 1)"
            }
            return TaskDetailExecution(
                id: id, status: string(state["status"]) ?? "unknown", agentId: string(state["agent_id"]),
                startedAt: date(state["started_at"]),
                endedAt: date(state["completed_at"]) ?? date(state["ended_at"]),
                summary: string(state["completion_summary"]), outcome: string(state["completion_outcome"]),
                error: string(state["error_message"]), progress: integer(state["progress"]),
                outputCount: outputItems.count, persistedArtifactCount: persistedArtifactLabels.count,
                outputPaths: outputPaths, persistedArtifactLabels: persistedArtifactLabels
            )
        }
    }

    private static func mergeHistory(_ primary: [TaskDetailExecution], _ details: [TaskDetailExecution]) -> [TaskDetailExecution] {
        var merged = Dictionary(uniqueKeysWithValues: details.map { ($0.id, $0) })
        for run in primary {
            if let detail = merged[run.id] {
                merged[run.id] = TaskDetailExecution(
                    id: run.id, status: run.status, agentId: detail.agentId,
                    startedAt: run.startedAt ?? detail.startedAt, endedAt: run.endedAt ?? detail.endedAt,
                    summary: run.summary ?? detail.summary, outcome: run.outcome ?? detail.outcome,
                    error: run.error ?? detail.error, progress: run.progress ?? detail.progress,
                    outputCount: max(run.outputCount, detail.outputCount),
                    persistedArtifactCount: detail.persistedArtifactCount,
                    outputPaths: detail.outputPaths.isEmpty ? run.outputPaths : detail.outputPaths,
                    persistedArtifactLabels: detail.persistedArtifactLabels
                )
            } else { merged[run.id] = run }
        }
        return merged.values.sorted { ($0.startedAt ?? .distantPast) > ($1.startedAt ?? .distantPast) }
    }

    private static func parseShellEntries(_ value: Any?) -> [TaskDetailShellEntry] {
        array(value).enumerated().map { offset, item in
            let lines = array(item["lines"]).compactMap { string($0["text"]) }
            return TaskDetailShellEntry(
                id: string(item["step_id"]) ?? "shell-\(offset)",
                command: string(item["command"]) ?? "Command",
                lines: lines, exitCode: integer(item["exit_code"]),
                complete: bool(item["is_complete"]) ?? false
            )
        }
    }
}

@MainActor
final class TaskDetailRealtime {
    private var webSocket: URLSessionWebSocketTask?
    private var session: URLSession?
    private var reconnectTask: Task<Void, Never>?
    private var reconnectAttempt = 0
    private var executionId: String?
    private var onPanelState: (([String: Any]) -> Void)?
    private(set) var active = false

    /// Stream full execution-panel snapshots for exactly one inspected run.
    /// Changing the historical-run picker only changes the filter; the global
    /// scoped socket itself does not need to be rebuilt.
    func start(executionId: String?, onPanelState: @escaping ([String: Any]) -> Void) {
        stop()
        self.executionId = executionId
        self.onPanelState = onPanelState
        active = true
        reconnectAttempt = 0
        connect()
    }

    func update(executionId: String?) {
        self.executionId = executionId
    }

    func stop() {
        active = false
        reconnectTask?.cancel()
        reconnectTask = nil
        webSocket?.cancel(with: .goingAway, reason: nil)
        webSocket = nil
        session?.invalidateAndCancel()
        session = nil
        executionId = nil
        onPanelState = nil
    }

    private func connect() {
        guard active, !isRunningUnderTests, webSocket == nil,
              let url = URL(
                string: "\(MagicianAccess.webSocketBaseURL.absoluteString)/api/magician/v2/realtime/ws"
              ) else { return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        let session = URLSession(
            configuration: .default, delegate: nil, delegateQueue: OperationQueue.main
        )
        self.session = session
        let socket = session.webSocketTask(with: request)
        webSocket = socket
        socket.resume()
        receive(on: socket)
    }

    private func receive(on socket: URLSessionWebSocketTask) {
        socket.receive { [weak self, weak socket] result in
            DispatchQueue.main.async {
                guard let self, self.active, let socket, self.webSocket === socket else { return }
                switch result {
                case .success(let message):
                    switch message {
                    case .string(let text): self.handleIncomingJSON(text)
                    case .data(let data): self.handleIncomingData(data)
                    @unknown default: break
                    }
                    self.reconnectAttempt = 0
                    self.receive(on: socket)
                case .failure(let error):
                    debugLog("[task-detail-realtime] receive error: \(error)")
                    self.webSocket = nil
                    self.session?.invalidateAndCancel()
                    self.session = nil
                    self.scheduleReconnect()
                }
            }
        }
    }

    /// Internal so exact scope/run filtering is covered without a real socket.
    @discardableResult
    func handleIncomingJSON(_ text: String) -> Bool {
        guard let data = text.data(using: .utf8) else { return false }
        return handleIncomingData(data)
    }

    @discardableResult
    private func handleIncomingData(_ data: Data) -> Bool {
        guard active, let executionId,
              let envelope = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              envelope["event_type"] as? String == "ExecutionPanelDelta",
              let payload = envelope["data"] as? [String: Any],
              payload["principal"] as? String == MagicianAccess.principal,
              payload["workspace"] as? String == MagicianAccess.workspace,
              let state = payload["state"] as? [String: Any],
              let overview = state["overview"] as? [String: Any]
        else { return false }
        let debug = state["debug"] as? [String: Any]
        let selected = debug?["selected_execution"] as? [String: Any]
        let stateExecutionId = (overview["execution_id"] as? String)
            ?? (selected?["execution_id"] as? String)
        guard stateExecutionId == executionId else { return false }
        onPanelState?(state)
        return true
    }

    private func scheduleReconnect() {
        guard active else { return }
        let delay = min(15.0, pow(2.0, Double(min(reconnectAttempt, 3))))
        reconnectAttempt += 1
        reconnectTask = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            guard let self, self.active, !Task.isCancelled else { return }
            self.connect()
        }
    }
}

@MainActor
final class TaskDetailViewModel: ObservableObject {
    typealias Fetch = (TaskDetailSeed, String?) async throws -> TaskDetailSnapshot

    @Published private(set) var snapshot: TaskDetailSnapshot?
    @Published private(set) var isLoading = false
    @Published private(set) var errorMessage: String?
    @Published private(set) var selectedExecutionId: String?

    private let seed: TaskDetailSeed
    private let fetch: Fetch
    private let realtime: TaskDetailRealtime?
    private var requestGeneration: UInt64 = 0

    init(seed: TaskDetailSeed, realtime: TaskDetailRealtime? = nil, fetch: @escaping Fetch) {
        self.seed = seed
        self.realtime = realtime
        self.fetch = fetch
        selectedExecutionId = seed.activeRootExecutionId ?? seed.latestRootExecutionId
    }

    convenience init(seed: TaskDetailSeed, session: URLSession = .shared) {
#if DEBUG
        if ProcessInfo.processInfo.arguments.contains("--tasks-ui-test-fixture") {
            self.init(seed: seed) { seed, selectedExecutionId in
                Self.uiTestSnapshot(seed: seed, selectedExecutionId: selectedExecutionId)
            }
            return
        }
#endif
        self.init(seed: seed, realtime: TaskDetailRealtime()) { seed, selectedExecutionId in
            try await Self.fetchNetwork(seed: seed, selectedExecutionId: selectedExecutionId, session: session)
        }
    }

    func startRealtime() {
        realtime?.start(executionId: selectedExecutionId) { [weak self] panel in
            self?.applyRealtimePanel(panel)
        }
    }

    func stopRealtime() {
        realtime?.stop()
    }

    func refresh() {
        requestGeneration &+= 1
        let generation = requestGeneration
        isLoading = snapshot == nil
        errorMessage = nil
        Task {
            do {
                let refreshed = try await fetch(seed, selectedExecutionId)
                guard generation == requestGeneration else { return }
                snapshot = refreshed
                if selectedExecutionId == nil { selectedExecutionId = refreshed.selectedExecutionId }
                realtime?.update(executionId: selectedExecutionId)
            } catch {
                guard generation == requestGeneration else { return }
                errorMessage = error.localizedDescription
            }
            if generation == requestGeneration { isLoading = false }
        }
    }

    func selectExecution(_ executionId: String) {
        guard executionId != selectedExecutionId else { return }
        selectedExecutionId = executionId
        realtime?.update(executionId: executionId)
        refresh()
    }

    /// Apply a pushed full snapshot in-place. Kept internal so tests can cover
    /// replacement semantics without opening a WebSocket.
    func applyRealtimePanel(_ panel: [String: Any]) {
        guard let current = snapshot,
              let updated = current.applyingRealtimePanel(seed: seed, panelPayload: panel)
        else { return }
        snapshot = updated
    }

    private static func fetchNetwork(
        seed: TaskDetailSeed,
        selectedExecutionId: String?,
        session: URLSession
    ) async throws -> TaskDetailSnapshot {
        let encoded = seed.taskId.addingPercentEncoding(
            withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-._~"))
        ) ?? seed.taskId
        let panelSuffix: String = {
            guard let execution = selectedExecutionId, !execution.isEmpty else { return "" }
            let encodedExecution = execution.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)
                ?? execution
            return "?execution_id=" + encodedExecution
        }()
        async let taskPayload = try? requestJSON("/tasks/\(encoded)", session: session)
        async let panelPayload = try? requestJSON("/tasks/\(encoded)/execution-panel\(panelSuffix)", session: session)
        async let outputsPayload = try? requestJSON("/tasks/\(encoded)/outputs", session: session)
        async let detailsPayload = try? requestJSON("/tasks/\(encoded)/details", session: session)
        async let planPayload = try? requestJSON("/tasks/\(encoded)/plan", session: session)
        let values = await (taskPayload, panelPayload, outputsPayload, detailsPayload, planPayload)
        guard values.0 != nil || values.1 != nil || values.3 != nil else {
            throw NSError(domain: "TaskDetail", code: 1,
                          userInfo: [NSLocalizedDescriptionKey: "Task details could not be loaded."])
        }
        var unavailable: [String] = []
        if values.1 == nil { unavailable.append("live run") }
        if values.2 == nil { unavailable.append("outputs") }
        if values.3 == nil { unavailable.append("execution history") }
        return TaskDetailSnapshot.parse(
            seed: seed, taskPayload: values.0, panelPayload: values.1,
            outputsPayload: values.2, detailsPayload: values.3, planPayload: values.4,
            unavailableSections: unavailable
        )
    }

    private static func requestJSON(_ path: String, session: URLSession) async throws -> [String: Any] {
        guard let url = URL(string: "\(MagicianAccess.baseURL.absoluteString)/api/magician/v3" + path) else {
            throw URLError(.badURL)
        }
        var request = URLRequest(url: url)
        request.timeoutInterval = 15
        MagicianAccess.authorize(&request)
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            throw URLError(.badServerResponse)
        }
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw URLError(.cannotParseResponse)
        }
        return object
    }

    private static func uiTestSnapshot(
        seed: TaskDetailSeed,
        selectedExecutionId: String?
    ) -> TaskDetailSnapshot {
        let isInternal = seed.taskId == "fixture-internal"
        let executionId = selectedExecutionId
            ?? (isInternal ? "exec-internal" : "exec-fixture")
        let panel: [String: Any] = [
            "default_tab": isInternal ? "output" : "run",
            "overview": [
                "title": seed.title, "description": seed.description, "status": seed.status,
                "assigned_agent_id": seed.agentId ?? "agent", "ui_thread_id": seed.threadId ?? "general",
                "has_plan": !isInternal, "progress": isInternal ? 100 : 64, "current_step": 1,
                "execution_id": executionId, "created_at": 1_752_559_200_000,
                "updated_at": 1_752_562_800_000
            ],
            "run": [
                "summary": isInternal ? "Memory compaction completed." : "Research is complete; writing is in progress.",
                "pending_questions": isInternal ? [] : [[
                    "id": "fixture-question", "question_text": "Which audience should the brief prioritize?",
                    "status": "pending", "options": [["label": "Leadership", "value": "leadership"]]
                ]],
                "needs_attention": [],
                "activity_log": [[
                    "id": "fixture-activity", "title": isInternal ? "Compaction complete" : "Research complete",
                    "summary": isInternal ? "Archived twelve stale episodes." : "Collected three authoritative sources.",
                    "status": "done", "item_type": "agent_message", "agent_id": seed.agentId ?? "agent",
                    "created_at": 1_752_562_700_000
                ]],
                "responsibility": [
                    "responsibility_summary": "The assigned agent owns this run.",
                    "active_owner_agent_id": seed.agentId ?? "agent", "waiting_state": isInternal ? "completed" : "running",
                    "active_children": []
                ]
            ],
            "output": [
                "result": [
                    "summary": isInternal ? "The memory ledger was compacted successfully." : "A draft launch brief is available.",
                    "outcome": "success", "artifact_names": [isInternal ? "memory-summary.json" : "launch-brief.md"]
                ],
                "deliveries": [],
                "recent_runs": [[
                    "execution_id": executionId, "status": isInternal ? "completed" : "running",
                    "started_at": 1_752_562_000_000, "ended_at": isInternal ? 1_752_562_060_000 : NSNull(),
                    "completion_summary": isInternal ? "Compaction complete" : NSNull(),
                    "completion_artifact_names": [isInternal ? "memory-summary.json" : "launch-brief.md"]
                ]]
            ],
            "debug": [
                "selected_execution": [
                    "execution_id": executionId, "status": isInternal ? "completed" : "running",
                    "progress": isInternal ? 100 : 64, "plan_id": isInternal ? NSNull() : "plan-fixture",
                    "step_statuses": [[
                        "number": 0, "name": isInternal ? "Scan episodes" : "Research",
                        "status": "completed", "step_id": "fixture-step-1"
                    ], [
                        "number": 1, "name": isInternal ? "Archive stale records" : "Write brief",
                        "status": isInternal ? "completed" : "running", "step_id": "fixture-step-2"
                    ]]
                ],
                "taskplan": isInternal ? (NSNull() as Any) : ([
                    "execution_id": executionId, "markdown": "# Launch plan\n- [x] Research\n- [ ] Write brief"
                ] as [String: Any]) as Any,
                "timeline": [], "observations": [["observation_id": "fixture-observation"]],
                "shell_entries": []
            ]
        ]
        let path = isInternal ? "memory-summary.json" : "launch-brief.md"
        let outputs: [String: Any] = [
            "outputs": ["outputs": [[
                "output_id": "fixture-output", "relative_path": path,
                "media_type": isInternal ? "application/json" : "text/markdown",
                "role": "deliverable", "size_bytes": 2_048
            ]]]
        ]
        let details: [String: Any] = [
            "executions": [[
                "state": [
                    "execution_id": executionId, "status": isInternal ? "completed" : "running",
                    "agent_id": seed.agentId ?? "agent", "started_at": "2026-07-15T07:00:00Z",
                    "completed_at": isInternal ? "2026-07-15T07:01:00Z" : NSNull()
                ],
                "refs": ["output_refs": [["relative_path": path]], "child_output_refs": []],
                "artifacts": isInternal ? [["path": "memory-ledger.db"]] : []
            ]]
        ]
        let plan: [String: Any]? = isInternal ? nil : [
            "plan": ["plan_id": "plan-fixture", "status": "approved", "pending_questions": []]
        ]
        return TaskDetailSnapshot.parse(
            seed: seed, taskPayload: nil, panelPayload: panel,
            outputsPayload: outputs, detailsPayload: details, planPayload: plan
        )
    }
}

struct DeepWorkPanel: View {
    private let seed: TaskDetailSeed
    private let sourceTask: TaskV3?
    private let onAction: ((TaskV3, TaskDetailAction) -> Void)?
    private let presentation: TaskDetailPresentation
    private let focus: TaskDetailFocus?

    @Environment(\.presentationMode) private var presentationMode
    @StateObject private var themeManager = ThemeManager.shared
    @StateObject private var detailModel: TaskDetailViewModel
    @StateObject private var notePublisher = TaskNotePublishViewModel()
    @State private var selectedTab: TaskDetailTab = .overview
    @State private var appliedDefaultTab = false
    @State private var openedArtifact: ArtifactRef?
    @State private var sharePayload: SharePayload?
    @State private var confirmDelete = false
    @State private var collapsedDelegations: Set<String> = []
    @AppStorage(TaskTimelineMode.storageKey) private var timelineModeRaw = TaskTimelineMode.grouped.rawValue

    init(
        task: TaskStatusModel,
        presentation: TaskDetailPresentation = .taskDetails,
        session: URLSession = .shared
    ) {
        let seed = TaskDetailSeed(task)
        self.seed = seed
        sourceTask = nil
        onAction = nil
        self.presentation = presentation
        focus = nil
        _detailModel = StateObject(wrappedValue: TaskDetailViewModel(seed: seed, session: session))
    }

    init(
        task: TaskV3,
        focus: TaskDetailFocus? = nil,
        session: URLSession = .shared,
        onAction: ((TaskV3, TaskDetailAction) -> Void)? = nil
    ) {
        let seed = TaskDetailSeed(task)
        self.seed = seed
        sourceTask = task
        self.onAction = onAction
        self.presentation = .taskDetails
        self.focus = focus
        _detailModel = StateObject(wrappedValue: TaskDetailViewModel(seed: seed, session: session))
    }

    var body: some View {
        NavigationView {
            ScrollViewReader { scrollProxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 14) {
                        if let message = notePublisher.successMessage {
                            notePublishSuccess(message)
                        }
                        if let snapshot = detailModel.snapshot {
                            header(snapshot)
                            if !snapshot.unavailableSections.isEmpty {
                                partialDataNotice(snapshot.unavailableSections)
                            }
                            if presentation.showsTaskTabs {
                                tabPicker(snapshot)
                                tabContent(snapshot)
                            } else {
                                runTab(snapshot)
                            }
                        } else if detailModel.isLoading {
                            loadingState
                        } else {
                            errorState(detailModel.errorMessage ?? "Task details are unavailable.")
                        }
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
                }
                .onReceive(detailModel.$snapshot) { snapshot in
                    guard !appliedDefaultTab, let snapshot = snapshot else { return }
                    // `openTab` only ever names a tab the task has, so no
                    // membership check is needed here — one that looked like a
                    // safety net would be a second guard preventing the same thing,
                    // and it would hide a real regression in the first.
                    let tab = presentation == .runInspection
                        ? .run
                        : snapshot.openTab(focus: focus, now: Date())
                    selectedTab = tab
                    appliedDefaultTab = true
                    if let focus, tab == focus.tab {
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) {
                            withAnimation { scrollProxy.scrollTo(focus.anchor, anchor: .top) }
                        }
                    }
                }
            }
            .refreshable { detailModel.refresh() }
            .background(themeManager.backgroundColor.ignoresSafeArea())
            .navigationTitle(presentation.navigationTitle)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .navigationBarLeading) {
                    Button("Done") { presentationMode.wrappedValue.dismiss() }
                }
                ToolbarItemGroup(placement: .navigationBarTrailing) {
                    if presentation == .taskDetails,
                       TaskNotePublishEligibility.allows(
                           status: detailModel.snapshot?.status ?? seed.status
                       ) {
                        Button {
                            Task { await notePublisher.publish(taskID: seed.taskId) }
                        } label: {
                            if notePublisher.publishingTaskID == seed.taskId {
                                ProgressView().controlSize(.small)
                            } else {
                                Image(systemName: "note.text.badge.plus")
                            }
                        }
                        .disabled(notePublisher.publishingTaskID != nil)
                        .accessibilityLabel("Publish to Notes")
                        .accessibilityIdentifier("task-detail-publish-notes")
                    }
                    Button { detailModel.refresh() } label: { Image(systemName: "arrow.clockwise") }
                    if sourceTask != nil, onAction != nil {
                        taskActionsMenu.disabled(notePublisher.publishingTaskID != nil)
                    }
                }
            }
            .onAppear {
                detailModel.refresh()
                detailModel.startRealtime()
            }
            .onDisappear { detailModel.stopRealtime() }
            .sheet(item: $openedArtifact) { ArtifactViewer(artifact: $0) }
            .sheet(item: $sharePayload) { ShareSheet(items: $0.items) }
            .confirmationDialog("Delete this task?", isPresented: $confirmDelete, titleVisibility: .visible) {
                Button("Delete", role: .destructive) { perform(.delete, dismissAfter: true) }
                Button("Keep task", role: .cancel) {}
            } message: {
                Text("This removes the task record and cannot be undone.")
            }
            .alert("Publish to Notes failed", isPresented: Binding(
                get: { notePublisher.errorMessage != nil },
                set: { if !$0 { notePublisher.errorMessage = nil } }
            )) {
                Button("OK", role: .cancel) { notePublisher.errorMessage = nil }
            } message: {
                Text(notePublisher.errorMessage ?? "The task page could not be published.")
            }
        }
    }

    private func notePublishSuccess(_ message: String) -> some View {
        HStack(spacing: 9) {
            Label(message, systemImage: "checkmark.circle.fill")
                .font(.themed(13, weight: .semibold))
                .foregroundColor(themeManager.successColor)
            Spacer(minLength: 8)
            Button { notePublisher.clearSuccess() } label: {
                Image(systemName: "xmark")
            }
            .buttonStyle(.plain)
            .foregroundColor(themeManager.secondaryTextColor)
            .accessibilityLabel("Dismiss publication confirmation")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(themeManager.successColor.opacity(0.12))
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .accessibilityIdentifier("task-detail-publish-notes-success")
    }

    private func header(_ snapshot: TaskDetailSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: statusIcon(snapshot.status))
                    .font(.system(size: 22, weight: .semibold))
                    .foregroundColor(statusColor(snapshot.status))
                    .frame(width: 28)
                VStack(alignment: .leading, spacing: 5) {
                    Text(snapshot.title.isEmpty ? "Untitled task" : snapshot.title)
                        .font(.themed(20, weight: .bold))
                        .foregroundColor(themeManager.textColor)
                        .fixedSize(horizontal: false, vertical: true)
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 6) {
                            chip(snapshot.status.replacingOccurrences(of: "_", with: " ").capitalized,
                                 icon: "circle.fill", tint: statusColor(snapshot.status))
                            if let agent = snapshot.agentId { chip(agent, icon: "person.fill") }
                            if let origin = originLabel(snapshot) { chip(origin, icon: "point.3.connected.trianglepath.dotted") }
                        }
                    }
                    if let recurrence = snapshot.recurrenceDescription {
                        Label("Recurring · \(recurrence)", systemImage: "repeat")
                            .font(.themed(11, weight: .medium))
                            .foregroundColor(themeManager.infoColor)
                            .fixedSize(horizontal: false, vertical: true)
                            .accessibilityIdentifier("task-detail-recurrence")
                    }
                }
                Spacer(minLength: 0)
            }

            // L0. One sentence answering "is this okay?", above everything the
            // panel would otherwise make the reader assemble: the chip says
            // `Running` where this says *stalled*, and the progress bar says
            // `64%` where this says *waiting on you, 4m*.
            verdictBlock(snapshot.verdict(now: Date()))

            if let progress = snapshot.progress {
                HStack(spacing: 8) {
                    ProgressView(value: Double(max(0, min(100, progress))), total: 100)
                        .tint(themeManager.accentColor)
                    Text("\(progress)%")
                        .font(.themed(12, weight: .semibold))
                        .foregroundColor(themeManager.secondaryTextColor)
                }
            }

            if let executionId = snapshot.activeRootExecutionId {
                ExecutionControlsView(
                    executionId: executionId,
                    refreshToken: "\(snapshot.status)|\(executionId)",
                    onChanged: detailModel.refresh
                )
                .id(executionId)
            } else if let task = sourceTask, onAction != nil {
                primaryTaskAction(task, snapshot: snapshot)
            }
        }
        .padding(14)
        .background(themeManager.surfaceColor)
        .clipShape(RoundedRectangle(cornerRadius: 16))
    }

    private func verdictBlock(_ verdict: Verdict) -> some View {
        let tint = verdictTint(verdict.state.severity)
        return HStack(alignment: .top, spacing: 9) {
            // The glyph carries the identity — all seven are distinct — because
            // the wash below is deliberately shared between states and carries
            // nothing at all in greyscale.
            Image(systemName: verdict.state.marker)
                .font(.system(size: 15, weight: .semibold))
                .foregroundColor(tint)
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 2) {
                Text(verdict.headline)
                    .font(.themed(15, weight: .bold))
                    .foregroundColor(themeManager.textColor)
                    .fixedSize(horizontal: false, vertical: true)
                // Rendered even when empty (the `finished` row, whose second
                // line the output summary owns) so the block does not resize
                // under the reader.
                Text(verdict.detail)
                    .font(.themed(12))
                    .foregroundColor(themeManager.secondaryTextColor)
                    .frame(minHeight: 15, alignment: .leading)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tint.opacity(0.1))
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("task-detail-verdict")
        .accessibilityLabel(
            verdict.detail.isEmpty ? verdict.headline : "\(verdict.headline). \(verdict.detail)"
        )
    }

    /// Severity band → theme colour. Exhaustive with no `default`, so a new band
    /// fails to build rather than falling through to a quiet neutral.
    ///
    /// Deliberately NOT keyed off `statusColor` below: that maps *task
    /// statuses*, and three verdict states are not statuses — `stalled` and
    /// `finished` would fall through to neutral and `waiting` would take the
    /// paused colour, rendering the loudest state in the union as one of the
    /// quietest.
    private func verdictTint(_ severity: VerdictSeverity) -> Color {
        switch severity {
        case .attention: return themeManager.warningColor
        case .failure: return themeManager.dangerColor
        case .progress: return themeManager.accentColor
        case .neutral: return themeManager.secondaryTextColor
        case .success: return themeManager.successColor
        }
    }

    private func tabPicker(_ snapshot: TaskDetailSnapshot) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 7) {
                ForEach(snapshot.visibleTabs) { tab in
                    Button { selectedTab = tab } label: {
                        Label(tab.title, systemImage: tab.icon)
                            .font(.themed(12, weight: .semibold))
                            .padding(.horizontal, 11).padding(.vertical, 8)
                            .background(selectedTab == tab ? themeManager.accentColor : themeManager.surfaceColor)
                            .foregroundColor(selectedTab == tab ? themeManager.onAccentColor : themeManager.textColor)
                            .clipShape(Capsule())
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("task-detail-tab-\(tab.rawValue)")
                }
            }
        }
    }

    @ViewBuilder
    private func tabContent(_ snapshot: TaskDetailSnapshot) -> some View {
        switch selectedTab {
        case .overview: overviewTab(snapshot)
        case .run: runTab(snapshot)
        case .output: outputTab(snapshot)
        case .plan: planTab(snapshot)
        case .history: historyTab(snapshot)
        }
    }

    private func overviewTab(_ snapshot: TaskDetailSnapshot) -> some View {
        VStack(spacing: 12) {
            if !snapshot.description.isEmpty {
                detailCard("Description") {
                    Markdown(snapshot.description)
                        .markdownTextStyle { ForegroundColor(themeManager.textColor) }
                        .textSelection(.enabled)
                }
            }

            if snapshot.attentionCount > 0 || !snapshot.questions.isEmpty {
                attentionCard(snapshot)
            }

            detailCard("Task details") {
                LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: 12) {
                    metric("Status", snapshot.status.capitalized)
                    metric("Agent", snapshot.agentId ?? "—")
                    metric("Thread", snapshot.threadId.map { "#\($0)" } ?? "—")
                    metric("Priority", snapshot.priority ?? "—")
                    metric("Due", snapshot.dueDate ?? "—")
                    metric("Updated", formatted(snapshot.updatedAt))
                    metric("Created", formatted(snapshot.createdAt))
                    metric("Created by", snapshot.createdBy ?? "—")
                }
                if !snapshot.tags.isEmpty {
                    Divider().opacity(0.4)
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack { ForEach(snapshot.tags, id: \.id) { chip("#\($0.name)", icon: "tag") } }
                    }
                }
                if let chat = snapshot.chatSessionId {
                    Divider().opacity(0.4)
                    metadataLine("Chat session", chat)
                }
                metadataLine("Task ID", seed.taskId, monospaced: true)
                if let execution = snapshot.activeRootExecutionId {
                    metadataLine("Active execution", execution, monospaced: true)
                } else if let execution = snapshot.latestRootExecutionId {
                    metadataLine("Latest execution", execution, monospaced: true)
                }
            }
        }
    }

    private func runTab(_ snapshot: TaskDetailSnapshot) -> some View {
        let visibleActivity = TaskTimelineFormatting.latest(snapshot.activity)
        let hiddenActivityCount = snapshot.activity.count - visibleActivity.count
        let summaryRows = TaskRunSummary.rows(
            executionId: snapshot.selectedExecutionId,
            activity: snapshot.activity,
            startedAt: snapshot.selectedExecutionStartedAt,
            endedAt: snapshot.selectedExecutionEndedAt
        )
        return VStack(spacing: 12) {
            detailCard("Run", subtitle: snapshot.selectedExecutionId ?? "No execution selected") {
                if !snapshot.history.isEmpty { executionSelector(snapshot) }
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: 12) {
                        metric("Status", snapshot.selectedExecutionStatus?.capitalized ?? snapshot.status.capitalized)
                        metric("Progress", snapshot.progress.map { "\($0)%" } ?? "—")
                        metric("Current step", snapshot.currentStep.map { "Step \($0 + 1)" } ?? "—")
                        metric("Duration", TaskTimelineFormatting.duration(
                            seconds: snapshot.runDuration(at: context.date)
                        ) ?? "—")
                        metric("Started", formatted(snapshot.selectedExecutionStartedAt))
                        metric("Ended", formatted(snapshot.selectedExecutionEndedAt))
                    }
                }
                if let summary = snapshot.runSummary {
                    Divider().opacity(0.4)
                    Markdown(summary).textSelection(.enabled)
                }
                if let error = snapshot.selectedExecutionError {
                    inlineNotice(error, tint: themeManager.dangerColor, icon: "exclamationmark.triangle.fill")
                }
            }

            if !summaryRows.isEmpty {
                detailCard("Run summary") {
                    ForEach(summaryRows) { row in
                        metadataLine(
                            row.label,
                            row.value,
                            monospaced: row.label == "Execution id"
                        )
                        if row.id != summaryRows.last?.id { Divider().opacity(0.28) }
                    }
                }
            }

            if let responsibility = snapshot.responsibility {
                responsibilityCard(responsibility)
            }

            detailCard("Plan steps", subtitle: snapshot.steps.isEmpty ? "No step snapshot" : "\(snapshot.steps.count) steps") {
                if snapshot.steps.isEmpty {
                    emptyMessage("No plan step snapshot is available for this execution.")
                } else {
                    ForEach(snapshot.steps) { stepRow($0) }
                }
            }

            detailCard(
                "Activity",
                subtitle: snapshot.activity.isEmpty
                    ? "No activity yet"
                    : "\(snapshot.activity.count) execution events"
            ) {
                if snapshot.activity.isEmpty { emptyMessage("No activity has been recorded.") }
                else {
                    if hiddenActivityCount > 0 {
                        Text("Showing the latest \(visibleActivity.count) of \(snapshot.activity.count) events")
                            .font(.themed(11, weight: .medium))
                            .foregroundColor(themeManager.secondaryTextColor)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    if !snapshot.delegations.isEmpty {
                        timelineModePicker
                    }
                    if timelineMode == .grouped && !snapshot.delegations.isEmpty {
                        ForEach(TaskDelegationTimeline.group(visibleActivity, delegations: snapshot.delegations)) { segment in
                            switch segment {
                            case .row(let activity):
                                activityRow(activity, origin: snapshot.timelineOrigin)
                            case .delegation(let group, let entries):
                                delegationBlock(group, entries: entries, origin: snapshot.timelineOrigin)
                            }
                        }
                    } else {
                        ForEach(visibleActivity) {
                            activityRow(
                                $0,
                                origin: snapshot.timelineOrigin,
                                delegatedAgent: TaskDelegationTimeline.delegatedAgent(
                                    for: $0, delegations: snapshot.delegations
                                )
                            )
                        }
                    }
                }
            }

            if !snapshot.shellEntries.isEmpty || snapshot.observationCount > 0
                || snapshot.linkedInputCount > 0 {
                detailCard("Run internals") {
                    if snapshot.observationCount > 0 {
                        metadataLine("Observations", "\(snapshot.observationCount)")
                    }
                    if snapshot.linkedInputCount > 0 {
                        metadataLine("Linked inputs", "\(snapshot.linkedInputCount)")
                    }
                    ForEach(snapshot.shellEntries) { shellEntry($0) }
                }
            }
        }
    }

    private func outputTab(_ snapshot: TaskDetailSnapshot) -> some View {
        let groups = snapshot.outputGroups
        return VStack(spacing: 12) {
            detailCard("Result", subtitle: snapshot.resultOutcome?.replacingOccurrences(of: "_", with: " ").capitalized) {
                if let summary = snapshot.resultSummary {
                    Markdown(summary).textSelection(.enabled)
                } else {
                    emptyMessage(snapshot.synthesisPending
                                 ? "The run finished and its output is still being synthesized."
                                 : "No completion result has been recorded yet.")
                }
                if snapshot.synthesisPending {
                    inlineNotice("Output synthesis is running.", tint: themeManager.accentColor,
                                 icon: "wand.and.stars")
                } else if snapshot.synthesisFailedExecutionId != nil {
                    inlineNotice("Output synthesis needs attention.", tint: themeManager.dangerColor,
                                 icon: "exclamationmark.arrow.triangle.2.circlepath")
                    if sourceTask != nil, onAction != nil {
                        Button { perform(.retrySynthesis) } label: {
                            Label("Retry synthesis", systemImage: "arrow.clockwise")
                                .frame(maxWidth: .infinity)
                        }.buttonStyle(.bordered)
                    }
                }
            }
            .id(TaskDetailFocus.result.anchor)

            detailCard(
                "Deliverables",
                subtitle: "\(groups.deliverables.count) \(groups.deliverables.count == 1 ? "deliverable" : "deliverables")"
            ) {
                if groups.deliverables.isEmpty {
                    emptyMessage("No task-level deliverables were recorded.")
                } else {
                    ForEach(groups.deliverables) { artifactRow($0) }
                }
            }

            if groups.intermediateCount > 0 {
                intermediatesCard(groups)
            }

            if snapshot.deliveryCount > 0 {
                detailCard("Deliveries") {
                    metadataLine("Published deliveries", "\(snapshot.deliveryCount)")
                }
            }
        }
    }

    /// Run-owned files and evidence, collapsed by default so the deliverables
    /// stay the headline (web "Intermediate artifacts & evidence" details).
    private func intermediatesCard(_ groups: TaskOutputGroups) -> some View {
        let count = groups.intermediateCount
        return DisclosureGroup {
            VStack(alignment: .leading, spacing: 12) {
                ForEach(groups.intermediates) { section in
                    VStack(alignment: .leading, spacing: 6) {
                        Text(section.scope.title)
                            .font(.themed(13, weight: .bold))
                            .foregroundColor(themeManager.textColor)
                        Text(section.files.isEmpty && !section.structured.isEmpty
                             ? "Structured execution evidence with no file to open."
                             : section.scope.description)
                            .font(.themed(11))
                            .foregroundColor(themeManager.secondaryTextColor)
                            .fixedSize(horizontal: false, vertical: true)
                        ForEach(section.files) { artifactRow($0) }
                        ForEach(section.structured) { structuredArtifactRow($0) }
                    }
                    .accessibilityIdentifier("task-detail-output-scope-\(section.scope.rawValue)")
                }
            }
            .padding(.top, 8)
        } label: {
            VStack(alignment: .leading, spacing: 2) {
                Text("Intermediate artifacts & evidence (\(count) \(count == 1 ? "item" : "items"))")
                    .font(.themed(14, weight: .bold))
                    .foregroundColor(themeManager.textColor)
                    .fixedSize(horizontal: false, vertical: true)
                Text("Outputs and evidence belonging only to execution runs")
                    .font(.themed(11))
                    .foregroundColor(themeManager.secondaryTextColor)
            }
        }
        .tint(themeManager.secondaryTextColor)
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(themeManager.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: 14))
        .overlay {
            RoundedRectangle(cornerRadius: 14)
                .stroke(themeManager.cardBorderColor, lineWidth: 1)
        }
        .accessibilityIdentifier("task-detail-output-intermediates")
    }

    private func structuredArtifactRow(_ artifact: TaskDetailStructuredArtifact) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "archivebox")
                .foregroundColor(themeManager.secondaryTextColor)
                .frame(width: 28)
            VStack(alignment: .leading, spacing: 2) {
                Text(artifact.name).font(.themed(13, weight: .semibold))
                    .foregroundColor(themeManager.textColor).lineLimit(2)
                let meta = [artifact.artifactType, artifact.contentType, artifact.producedAt]
                    .compactMap { $0 }.joined(separator: " · ")
                if !meta.isEmpty {
                    Text(meta).font(.themed(11)).foregroundColor(themeManager.secondaryTextColor)
                        .lineLimit(2)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.vertical, 4)
    }

    private func planTab(_ snapshot: TaskDetailSnapshot) -> some View {
        VStack(spacing: 12) {
            detailCard("Plan status") {
                LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: 12) {
                    metric("Status", snapshot.planStatus?.capitalized ?? "None")
                    metric("Questions", "\(snapshot.questions.count)")
                }
                if let planId = snapshot.planId { metadataLine("Plan ID", planId, monospaced: true) }
                if let task = sourceTask, onAction != nil { planActions(task, snapshot: snapshot) }
            }

            if !snapshot.questions.isEmpty { attentionCard(snapshot) }

            detailCard("Stepwise plan") {
                if snapshot.steps.isEmpty { emptyMessage("No plan step snapshot is available yet.") }
                else { ForEach(snapshot.steps) { stepRow($0) } }
            }

            if let markdown = snapshot.planMarkdown, !markdown.isEmpty {
                detailCard("Plan document") {
                    Markdown(markdown).textSelection(.enabled)
                }
            }
        }
    }

    private func historyTab(_ snapshot: TaskDetailSnapshot) -> some View {
        VStack(spacing: 12) {
            if snapshot.history.isEmpty {
                detailCard("Execution history") { emptyMessage("No executions have been recorded.") }
            } else {
                ForEach(snapshot.history) { run in
                    detailCard("Execution", subtitle: run.id) {
                        HStack {
                            chip(run.status.capitalized, icon: "circle.fill", tint: statusColor(run.status))
                            if snapshot.activeRootExecutionId == run.id {
                                chip("Active", icon: "bolt.fill", tint: themeManager.accentColor)
                            }
                            Spacer()
                            Button("Inspect") {
                                selectedTab = .run
                                detailModel.selectExecution(run.id)
                            }
                            .font(.themed(12, weight: .semibold))
                            .buttonStyle(.bordered)
                        }
                        if let span = TaskHistoryFormatting.runSpan(
                            startedAt: run.startedAt, endedAt: run.endedAt, clock: { shortTime($0) }
                        ) {
                            Text(span)
                                .font(.themed(12, weight: .medium))
                                .foregroundColor(themeManager.secondaryTextColor)
                                .monospacedDigit()
                                .fixedSize(horizontal: false, vertical: true)
                                .accessibilityIdentifier("task-detail-history-span-\(run.id)")
                        }
                        LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: 10) {
                            metric("Started", formatted(run.startedAt))
                            metric("Ended", formatted(run.endedAt))
                            metric("Outputs", "\(run.outputCount)")
                            metric("Artifacts", "\(run.persistedArtifactCount)")
                        }
                        if let summary = run.summary { Markdown(summary).textSelection(.enabled) }
                        if let error = run.error {
                            inlineNotice(error, tint: themeManager.dangerColor, icon: "exclamationmark.triangle.fill")
                        }
                        if !run.outputPaths.isEmpty {
                            DisclosureGroup("Execution outputs") {
                                ForEach(run.outputPaths, id: \.self) { path in
                                    let ref = ArtifactRef.taskOutput(
                                        taskId: seed.taskId, relativePath: path, mime: nil
                                    )
                                    Button { openedArtifact = ref } label: {
                                        HStack {
                                            Image(systemName: ref.kind.systemIcon)
                                            Text((path as NSString).lastPathComponent).lineLimit(2)
                                            Spacer()
                                            Image(systemName: "arrow.up.forward.square")
                                        }
                                        .font(.themed(12)).foregroundColor(themeManager.textColor)
                                        .padding(.vertical, 4)
                                    }.buttonStyle(.plain)
                                }
                            }
                            .font(.themed(12, weight: .semibold))
                            .foregroundColor(themeManager.textColor)
                        }
                        if !run.persistedArtifactLabels.isEmpty {
                            DisclosureGroup("Persisted artifacts") {
                                ForEach(run.persistedArtifactLabels, id: \.self) { label in
                                    Label(label, systemImage: "archivebox")
                                        .font(.themedMono(11))
                                        .foregroundColor(themeManager.secondaryTextColor)
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                        .padding(.vertical, 2)
                                }
                            }
                            .font(.themed(12, weight: .semibold))
                            .foregroundColor(themeManager.textColor)
                        }
                    }
                }
            }
        }
    }

    private func attentionCard(_ snapshot: TaskDetailSnapshot) -> some View {
        detailCard("Needs your attention", subtitle: "\(snapshot.questions.count + snapshot.attentionCount) waiting") {
            ForEach(snapshot.questions) { question in
                VStack(alignment: .leading, spacing: 5) {
                    Text(question.text).font(.themed(14, weight: .semibold)).foregroundColor(themeManager.textColor)
                    if !question.options.isEmpty {
                        Text(question.options.joined(separator: " · "))
                            .font(.themed(12)).foregroundColor(themeManager.secondaryTextColor)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            Button {
                presentationMode.wrappedValue.dismiss()
                AppActions.shared.requestAttention()
            } label: {
                Label("Review in Attention", systemImage: "bell.badge.fill")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(themeManager.accentColor)
            .foregroundColor(themeManager.onAccentColor)
        }
    }

    private func responsibilityCard(_ responsibility: TaskDetailResponsibility) -> some View {
        detailCard("Responsibility", subtitle: responsibility.summary) {
            LazyVGrid(columns: [GridItem(.flexible()), GridItem(.flexible())], spacing: 10) {
                metric("Owner", responsibility.owner)
                metric("State", responsibility.waitingState.replacingOccurrences(of: "_", with: " ").capitalized)
                if let stage = responsibility.stage { metric("Stage", stage) }
                if let provider = responsibility.provider { metric("Provider", provider) }
            }
            ForEach(responsibility.children) { child in
                HStack(alignment: .top, spacing: 9) {
                    Image(systemName: child.isBlocking ? "hourglass.circle.fill" : "arrow.turn.down.right")
                        .foregroundColor(child.isBlocking ? themeManager.warningColor : themeManager.accentColor)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(child.title).font(.themed(13, weight: .semibold)).foregroundColor(themeManager.textColor)
                        Text("\(child.owner) · \(child.waitingState.replacingOccurrences(of: "_", with: " "))")
                            .font(.themed(11)).foregroundColor(themeManager.secondaryTextColor)
                    }
                }
            }
        }
    }

    private func executionSelector(_ snapshot: TaskDetailSnapshot) -> some View {
        Menu {
            ForEach(snapshot.history) { run in
                Button { detailModel.selectExecution(run.id) } label: {
                    Label(shortExecution(run.id), systemImage: run.id == snapshot.selectedExecutionId
                          ? "checkmark.circle.fill" : "circle")
                }
            }
        } label: {
            HStack {
                Label("Selected run", systemImage: "clock.arrow.circlepath")
                Spacer()
                Text(shortExecution(snapshot.selectedExecutionId ?? "Latest"))
                Image(systemName: "chevron.up.chevron.down")
            }
            .font(.themed(12, weight: .semibold))
            .foregroundColor(themeManager.textColor)
            .padding(10)
            .background(themeManager.backgroundColor.opacity(0.55))
            .clipShape(RoundedRectangle(cornerRadius: 10))
        }
    }

    private func stepRow(_ step: TaskDetailStep) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: stepIcon(step.status))
                .foregroundColor(statusColor(step.status))
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 3) {
                Text("\(step.number + 1). \(step.name)")
                    .font(.themed(14, weight: .semibold)).foregroundColor(themeManager.textColor)
                HStack(spacing: 5) {
                    if let capability = step.capability { Text(capability) }
                    if let delegate = step.delegateAgentId { Text("→ \(delegate)") }
                    if let progress = step.progress { Text(progress) }
                }
                .font(.themed(11)).foregroundColor(themeManager.secondaryTextColor)
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 3) {
                Text(step.status.replacingOccurrences(of: "_", with: " ").capitalized)
                    .font(.themed(10, weight: .semibold)).foregroundColor(statusColor(step.status))
                if let duration = TaskTimelineFormatting.duration(milliseconds: step.durationMs) {
                    Text(duration).font(.themed(10)).foregroundColor(themeManager.secondaryTextColor)
                        .monospacedDigit()
                }
            }
        }
        .padding(.vertical, 4)
    }

    private var timelineMode: TaskTimelineMode {
        TaskTimelineMode(rawValue: timelineModeRaw) ?? .grouped
    }

    private var timelineModePicker: some View {
        Picker("Timeline", selection: Binding(
            get: { timelineMode },
            set: { timelineModeRaw = $0.rawValue }
        )) {
            ForEach(TaskTimelineMode.allCases) { Text($0.title).tag($0) }
        }
        .pickerStyle(.segmented)
        .controlSize(.small)
        .accessibilityIdentifier("task-detail-timeline-mode")
    }

    private func delegationTint(_ status: String) -> Color {
        switch status.lowercased() {
        case "running", "executing", "planning", "queued": return themeManager.infoColor
        case "done", "completed", "complete", "succeeded": return themeManager.successColor
        case "failed", "error", "cancelled", "canceled": return themeManager.dangerColor
        case "waiting", "paused", "needs_action", "blocked": return themeManager.warningColor
        default: return themeManager.secondaryTextColor
        }
    }

    /// One bounded envelope per delegated child, at its first row: agent +
    /// status + span, the child's rows collapsible beneath.
    private func delegationBlock(
        _ group: TaskDelegationGroup,
        entries: [TaskDetailActivity],
        origin: Date?
    ) -> some View {
        let tint = delegationTint(group.status)
        let span = TaskDelegationTimeline.span(entries: entries, group: group)
        let expanded = !collapsedDelegations.contains(group.executionId)
        let noun = entries.count == 1 ? "step" : "steps"
        let meta = ["\(entries.count) \(noun)", group.status.replacingOccurrences(of: "_", with: " "), span.summary]
            .compactMap { $0 }.joined(separator: " · ")
        return HStack(alignment: .top, spacing: 0) {
            Rectangle().fill(tint).frame(width: 3)
            VStack(alignment: .leading, spacing: 8) {
                Button {
                    if expanded { collapsedDelegations.insert(group.executionId) }
                    else { collapsedDelegations.remove(group.executionId) }
                } label: {
                    HStack(alignment: .top, spacing: 8) {
                        Image(systemName: expanded ? "chevron.down" : "chevron.right")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundColor(themeManager.secondaryTextColor)
                            .frame(width: 14)
                            .padding(.top, 2)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Delegated to \(group.agentId)")
                                .font(.themed(13, weight: .semibold))
                                .foregroundColor(themeManager.textColor)
                                .lineLimit(2)
                            Text(meta)
                                .font(.themed(11))
                                .foregroundColor(tint)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        Spacer(minLength: 0)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Delegated to \(group.agentId), \(meta)")
                .accessibilityHint(expanded ? "Collapse" : "Expand")
                if expanded {
                    ForEach(entries) { activityRow($0, origin: origin) }
                }
            }
            .padding(10)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tint.opacity(0.06))
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .accessibilityIdentifier("task-detail-delegation-\(group.executionId)")
    }

    private func activityRow(
        _ activity: TaskDetailActivity,
        origin: Date?,
        delegatedAgent: String? = nil
    ) -> some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(spacing: 0) {
                Circle().fill(statusColor(activity.status)).frame(width: 9, height: 9).padding(.top, 4)
                Rectangle().fill(themeManager.secondaryTextColor.opacity(0.18)).frame(width: 1, height: 42)
            }
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline) {
                    Text(delegatedAgent.map { "\(activity.title) · \($0)" } ?? activity.title)
                        .font(.themed(13, weight: .semibold)).foregroundColor(themeManager.textColor)
                    Spacer()
                    if let latency = TaskTimelineFormatting.duration(milliseconds: activity.latencyMs) {
                        Text(latency).font(.themed(10, weight: .medium))
                            .foregroundColor(themeManager.secondaryTextColor).monospacedDigit()
                    }
                }
                HStack(spacing: 6) {
                    if let clock = TaskTimelineFormatting.wallClock(activity.timestamp) {
                        Text(clock).monospacedDigit()
                    }
                    if let offset = TaskTimelineFormatting.offset(
                        event: activity.timestamp, origin: origin
                    ) {
                        Text(offset).monospacedDigit()
                    }
                }
                .font(.themed(10, weight: .medium))
                .foregroundColor(themeManager.secondaryTextColor)
                if let body = activity.body, !body.isEmpty {
                    Text(body).font(.themed(12)).foregroundColor(themeManager.secondaryTextColor)
                        .fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
                }
                HStack(spacing: 5) {
                    Text(activity.kind.replacingOccurrences(of: "_", with: " "))
                    if let agent = activity.agentId { Text("· \(agent)") }
                    if let model = activity.model { Text("· \(model)").lineLimit(1) }
                    if activity.inputTokens != nil || activity.outputTokens != nil {
                        let input = TaskTimelineFormatting.tokenCount(activity.inputTokens) ?? "—"
                        let output = TaskTimelineFormatting.tokenCount(activity.outputTokens) ?? "—"
                        Text("· \(input) → \(output) tok")
                    }
                    if let cached = TaskTimelineFormatting.tokenCount(activity.cacheReadTokens) {
                        Text("· \(cached) cached")
                    }
                }.font(.themed(10)).foregroundColor(themeManager.secondaryTextColor.opacity(0.8))
                    .lineLimit(2)
            }
        }
    }

    private func shellEntry(_ entry: TaskDetailShellEntry) -> some View {
        DisclosureGroup {
            ScrollView(.horizontal) {
                Text(entry.lines.joined(separator: "\n"))
                    .font(.themedMono(.caption)).foregroundColor(themeManager.textColor)
                    .frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
            }
            .padding(10).background(themeManager.controlColor).clipShape(RoundedRectangle(cornerRadius: 8))
        } label: {
            HStack {
                Image(systemName: "terminal")
                Text(entry.command).lineLimit(1)
                Spacer()
                Text(entry.exitCode.map { "exit \($0)" } ?? (entry.complete ? "done" : "running"))
                    .font(.themed(10)).foregroundColor(themeManager.secondaryTextColor)
            }
            .font(.themed(12, weight: .semibold)).foregroundColor(themeManager.textColor)
        }
    }

    private func artifactRow(_ artifact: TaskDetailArtifact) -> some View {
        let ref = ArtifactRef.taskOutput(taskId: seed.taskId, relativePath: artifact.relativePath,
                                         mime: artifact.mediaType)
        return Button { openedArtifact = ref } label: {
            HStack(spacing: 10) {
                Image(systemName: ref.kind.systemIcon).font(.title3).foregroundColor(themeManager.accentColor)
                    .frame(width: 28)
                VStack(alignment: .leading, spacing: 3) {
                    Text(artifact.displayName).font(.themed(14, weight: .semibold))
                        .foregroundColor(themeManager.textColor).lineLimit(2)
                    HStack(spacing: 4) {
                        Text(ref.kind.label)
                        if let role = artifact.role { Text("· \(role)") }
                        if let size = artifact.sizeBytes { Text("· \(formatBytes(size))") }
                    }.font(.themed(11)).foregroundColor(themeManager.secondaryTextColor)
                    if let snippet = artifact.bodySnippet {
                        Text(snippet).font(.themed(11)).foregroundColor(themeManager.secondaryTextColor)
                            .lineLimit(3)
                    }
                }
                Spacer()
                Image(systemName: "chevron.right").font(.caption).foregroundColor(themeManager.secondaryTextColor)
            }
            .padding(.vertical, 5)
        }
        .buttonStyle(.plain)
        .contextMenu {
            Button { openedArtifact = ref } label: {
                Label("Open preview", systemImage: "doc.text.magnifyingglass")
            }
            if !ref.isMagicianOwned, let url = ref.url {
                Button { UIApplication.shared.open(url) } label: { Label("Open in Browser", systemImage: "safari") }
                Button { UIPasteboard.general.string = url.absoluteString } label: { Label("Copy Link", systemImage: "link") }
                Button { sharePayload = SharePayload(items: [url]) } label: { Label("Share…", systemImage: "square.and.arrow.up") }
            }
        }
    }

    private func detailCard<Content: View>(
        _ title: String,
        subtitle: String? = nil,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 11) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.themed(16, weight: .bold)).foregroundColor(themeManager.textColor)
                if let subtitle = subtitle, !subtitle.isEmpty {
                    Text(subtitle).font(.themed(11)).foregroundColor(themeManager.secondaryTextColor)
                        .lineLimit(2)
                }
            }
            content()
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(themeManager.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: 14))
        .overlay {
            RoundedRectangle(cornerRadius: 14)
                .stroke(themeManager.cardBorderColor, lineWidth: 1)
        }
    }

    private func metric(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label.uppercased()).font(.themed(9, weight: .bold)).foregroundColor(themeManager.secondaryTextColor)
            Text(value).font(.themed(13, weight: .semibold)).foregroundColor(themeManager.textColor)
                .lineLimit(2).minimumScaleFactor(0.75)
        }.frame(maxWidth: .infinity, alignment: .leading)
    }

    private func metadataLine(_ label: String, _ value: String, monospaced: Bool = false) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Text(label).foregroundColor(themeManager.secondaryTextColor)
            Spacer(minLength: 8)
            Text(value)
                .font(monospaced ? .themedMono(11) : .themed(12, weight: .medium))
                .foregroundColor(themeManager.textColor).multilineTextAlignment(.trailing).textSelection(.enabled)
        }.font(.themed(12))
    }

    private func chip(_ text: String, icon: String, tint: Color? = nil) -> some View {
        HStack(spacing: 4) {
            Image(systemName: icon).font(.system(size: 8))
            Text(text).lineLimit(1)
        }
        .font(.themed(10, weight: .semibold))
        .padding(.horizontal, 7).padding(.vertical, 4)
        .background((tint ?? themeManager.secondaryTextColor).opacity(0.13))
        .foregroundColor(tint ?? themeManager.secondaryTextColor)
        .clipShape(Capsule())
    }

    private func partialDataNotice(_ sections: [String]) -> some View {
        inlineNotice("Some \(sections.joined(separator: ", ")) details are temporarily unavailable. Pull to retry.",
                     tint: themeManager.warningColor, icon: "wifi.exclamationmark")
    }

    private func inlineNotice(_ text: String, tint: Color, icon: String) -> some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: icon).foregroundColor(tint)
            Text(text).font(.themed(12)).foregroundColor(themeManager.textColor)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(10).background(tint.opacity(0.1)).clipShape(RoundedRectangle(cornerRadius: 10))
    }

    private func emptyMessage(_ text: String) -> some View {
        Text(text).font(.themed(13)).foregroundColor(themeManager.secondaryTextColor)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var loadingState: some View {
        VStack(spacing: 12) {
            ProgressView()
            Text("Loading the task workspace…").font(.themed(14)).foregroundColor(themeManager.secondaryTextColor)
        }.frame(maxWidth: .infinity).padding(.top, 100)
    }

    private func errorState(_ message: String) -> some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle.fill").font(.largeTitle).foregroundColor(themeManager.warningColor)
            Text(message).font(.themed(14)).foregroundColor(themeManager.secondaryTextColor).multilineTextAlignment(.center)
            Button("Try again") { detailModel.refresh() }
                .buttonStyle(.borderedProminent).tint(themeManager.accentColor)
                .foregroundColor(themeManager.onAccentColor)
        }.frame(maxWidth: .infinity).padding(.top, 80)
    }

    @ViewBuilder
    private func primaryTaskAction(_ task: TaskV3, snapshot: TaskDetailSnapshot) -> some View {
        Group {
            if snapshot.planStatus == "planning" {
                Button { selectedTab = .plan } label: {
                    Label("View Plan", systemImage: "list.bullet.clipboard").frame(maxWidth: .infinity)
                }
            } else if snapshot.planStatus == "eliciting" {
                Button { selectedTab = .plan } label: {
                    Label(task.pendingQuestion == nil ? "View Plan" : "Answer Question",
                          systemImage: "questionmark.bubble").frame(maxWidth: .infinity)
                }
            } else if snapshot.planStatus == "draft" {
                Button { selectedTab = .plan } label: {
                    Label("Review Plan", systemImage: "checklist").frame(maxWidth: .infinity)
                }
            } else if snapshot.planStatus == "approved" && snapshot.status == "ready" {
                Button { perform(.run) } label: {
                    Label("Run Plan", systemImage: "play.fill").frame(maxWidth: .infinity)
                }
            } else {
                switch snapshot.status.lowercased() {
                case "pending":
                    HStack(spacing: 8) {
                        Button { perform(.plan) } label: {
                            Label("PrePlan", systemImage: "sparkles").frame(maxWidth: .infinity)
                        }
                        Button { perform(.run) } label: {
                            Label("Run Now", systemImage: "play.fill").frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.bordered)
                    }
                case "ready":
                    Button { perform(.run) } label: {
                        Label("Run Now", systemImage: "play.fill").frame(maxWidth: .infinity)
                    }
                case "paused", "failed", "cancelled", "canceled":
                    Button { perform(.reset) } label: {
                        Label("Reset to Ready", systemImage: "arrow.uturn.backward").frame(maxWidth: .infinity)
                    }
                default: EmptyView()
                }
            }
        }
        .buttonStyle(.borderedProminent)
        .tint(themeManager.accentColor)
        .foregroundColor(themeManager.onAccentColor)
    }

    private func planActions(_ task: TaskV3, snapshot: TaskDetailSnapshot) -> some View {
        HStack(spacing: 8) {
            if snapshot.planStatus == "draft" || snapshot.planStatus == "eliciting" {
                Button("Approve") { perform(.approvePlan) }
                    .buttonStyle(.borderedProminent).tint(themeManager.accentColor)
                    .foregroundColor(themeManager.onAccentColor)
                Button("Reject", role: .destructive) { perform(.rejectPlan) }.buttonStyle(.bordered)
            }
            Button(snapshot.hasPlan ? "Replan" : "Plan") {
                perform(snapshot.hasPlan ? .replan : .plan)
            }.buttonStyle(.bordered)
        }
    }

    private var taskActionsMenu: some View {
        Menu {
            if let snapshot = detailModel.snapshot {
                if snapshot.status == "completed" {
                    Button { perform(.markNotDone) } label: { Label("Mark not done", systemImage: "arrow.uturn.backward") }
                } else if !["running", "planning", "paused", "executing", "queued"].contains(snapshot.status) {
                    Button { perform(.markComplete) } label: { Label("Mark complete", systemImage: "checkmark.circle") }
                }
                if snapshot.status == "pending" {
                    Button { perform(.plan) } label: { Label("Plan", systemImage: "sparkles") }
                }
                // One Reset only: the header's primary button owns it whenever
                // it shows; the menu keeps it for the case the header shows
                // live run controls instead (paused with an active execution).
                if ["paused", "failed", "cancelled", "canceled"].contains(snapshot.status),
                   !snapshot.headerOffersReset {
                    Button { perform(.reset) } label: {
                        Label("Reset to Ready", systemImage: "arrow.uturn.backward")
                    }
                }
                if snapshot.hasPlan {
                    Button { selectedTab = .plan } label: { Label("Open plan", systemImage: "list.bullet.clipboard") }
                    Button { perform(.replan) } label: { Label("Replan", systemImage: "arrow.triangle.2.circlepath") }
                }
                if snapshot.status != "completed" && snapshot.status != "cancelled" {
                    Button { perform(.cancelTask) } label: { Label("Cancel task", systemImage: "xmark.circle") }
                }
            }
            Divider()
            Button(role: .destructive) { confirmDelete = true } label: { Label("Delete", systemImage: "trash") }
        } label: { Image(systemName: "ellipsis.circle") }
    }

    private func perform(_ action: TaskDetailAction, dismissAfter: Bool = false) {
        guard let task = sourceTask else { return }
        onAction?(task, action)
        if dismissAfter {
            presentationMode.wrappedValue.dismiss()
        } else {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.55) { detailModel.refresh() }
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.8) { detailModel.refresh() }
        }
    }

    private func originLabel(_ snapshot: TaskDetailSnapshot) -> String? {
        let internalLifecycle = ["internal", "ephemeral_owned_by_chat", "internal_debug"].contains(snapshot.lifecycle ?? "")
        guard internalLifecycle else { return snapshot.lifecycle == nil ? nil : "persistent" }
        if snapshot.createdBy == "__system__" { return "debug" }
        if snapshot.chatSessionId?.isEmpty == false { return "chat" }
        return "internal"
    }

    private func statusColor(_ status: String) -> Color {
        switch status.lowercased() {
        case "complete", "completed", "done": return themeManager.successColor
        case "failed", "error", "cancelled", "canceled", "critical": return themeManager.dangerColor
        case "paused", "waiting", "warning", "needs_action": return themeManager.warningColor
        case "running", "planning", "executing", "queued": return themeManager.accentColor
        default: return themeManager.secondaryTextColor
        }
    }

    private func statusIcon(_ status: String) -> String {
        switch status.lowercased() {
        case "complete", "completed", "done": return "checkmark.circle.fill"
        case "failed", "error", "cancelled", "canceled": return "exclamationmark.triangle.fill"
        case "paused", "waiting": return "pause.circle.fill"
        case "pending", "ready": return "circle.dotted"
        default: return "arrow.triangle.2.circlepath.circle.fill"
        }
    }

    private func stepIcon(_ status: String) -> String {
        switch status.lowercased() {
        case "complete", "completed", "done", "succeeded": return "checkmark.circle.fill"
        case "failed", "error": return "xmark.circle.fill"
        case "running", "ongoing", "in_progress": return "play.circle.fill"
        default: return "circle"
        }
    }

    private func formatted(_ date: Date?) -> String {
        guard let date = date else { return "—" }
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }

    private func shortTime(_ date: Date?) -> String {
        guard let date = date else { return "" }
        let formatter = DateFormatter()
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }

    private func shortExecution(_ id: String) -> String {
        id.count > 14 ? String(id.prefix(12)) + "…" : id
    }

    private func formatBytes(_ bytes: Int) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(bytes), countStyle: .file)
    }
}

struct ExecutionControlsView: View {
    @StateObject private var viewModel: ExecutionControlViewModel
    @ObservedObject private var coordinator = ExecutionControlCoordinator.shared
    @StateObject private var themeManager = ThemeManager.shared
    @State private var showSteer = false
    @State private var confirmStop = false
    private let refreshToken: String

    init(executionId: String, refreshToken: String = "", onChanged: @escaping () -> Void = {}) {
        self.refreshToken = refreshToken
        _viewModel = StateObject(
            wrappedValue: ExecutionControlViewModel(executionId: executionId, onChanged: onChanged)
        )
    }

    private var coordination: ExecutionControlCoordinationSnapshot {
        coordinator.snapshot(for: viewModel.executionId)
    }

    private var refreshIdentity: String {
        "\(refreshToken)|\(coordination.revision)"
    }

    var body: some View {
        Group {
            if viewModel.isLoading
                || !viewModel.isCurrent(hostToken: refreshToken, coordination: coordination) {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("Loading run controls")
                        .font(.themed(12))
                        .foregroundColor(themeManager.secondaryTextColor)
                }
            } else if viewModel.loadErrorMessage != nil {
                Button { viewModel.load() } label: {
                    Label("Retry run controls", systemImage: "arrow.clockwise")
                        .font(.themed(12, weight: .semibold))
                        .foregroundColor(themeManager.secondaryTextColor)
                }
                .buttonStyle(.plain)
            } else if let state = viewModel.state, state.hasAvailableAction {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        if state.canPause {
                            controlButton("Pause", systemImage: "pause.fill", action: .pause)
                        }
                        if state.canResume {
                            controlButton("Resume", systemImage: "play.fill", action: .resume)
                        }
                        if state.canSteer {
                            Button { showSteer = true } label: {
                                controlLabel("Steer", systemImage: "arrow.triangle.turn.up.right.diamond.fill",
                                             tint: themeManager.accentColor, action: .steer)
                            }
                            .buttonStyle(.plain)
                            .disabled(viewModel.busyAction != nil)
                        }
                        if state.canCancel {
                            Button { confirmStop = true } label: {
                                controlLabel("Stop", systemImage: "stop.fill", tint: themeManager.dangerColor, action: .cancel)
                            }
                            .buttonStyle(.plain)
                            .disabled(viewModel.busyAction != nil)
                        }
                    }
                }
            }
        }
        .task(id: refreshIdentity) {
            viewModel.refresh(hostToken: refreshToken, coordination: coordination)
        }
        .sheet(isPresented: $showSteer) {
            ExecutionSteerSheet(
                isBusy: coordination.busyAction != nil,
                onSend: { message in viewModel.perform(.steer, message: message) }
            )
        }
        .confirmationDialog("Stop this run?", isPresented: $confirmStop, titleVisibility: .visible) {
            Button("Stop run", role: .destructive) { viewModel.perform(.cancel) }
                .disabled(coordination.busyAction != nil)
            Button("Keep running", role: .cancel) {}
        } message: {
            Text("The active execution and its running children will be cancelled.")
        }
        .alert("Run control failed", isPresented: Binding(
            get: { viewModel.errorMessage != nil },
            set: { if !$0 { viewModel.errorMessage = nil } }
        )) {
            Button("OK", role: .cancel) { viewModel.errorMessage = nil }
        } message: {
            Text(viewModel.errorMessage ?? "The run could not be updated.")
        }
    }

    private func controlButton(_ title: String, systemImage: String, action: ExecutionControlAction,
                               tint: Color? = nil) -> some View {
        Button { viewModel.perform(action) } label: {
            controlLabel(title, systemImage: systemImage, tint: tint ?? themeManager.accentColor,
                         action: action)
        }
        .buttonStyle(.plain)
        .disabled(viewModel.busyAction != nil)
    }

    private func controlLabel(_ title: String, systemImage: String, tint: Color,
                              action: ExecutionControlAction) -> some View {
        HStack(spacing: 6) {
            if viewModel.busyAction == action {
                ProgressView().controlSize(.small)
            } else {
                Image(systemName: systemImage)
            }
            Text(title)
        }
        .font(.themed(12, weight: .semibold))
        .padding(.horizontal, 11)
        .padding(.vertical, 7)
        .foregroundColor(tint)
        .background(tint.opacity(0.12))
        .clipShape(Capsule())
        .overlay(Capsule().stroke(tint.opacity(0.25), lineWidth: 1))
    }
}

private struct ExecutionSteerSheet: View {
    @Environment(\.dismiss) private var dismiss
    @StateObject private var themeManager = ThemeManager.shared
    @State private var message = ""
    let isBusy: Bool
    let onSend: (String) -> Bool

    private var trimmed: String { message.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var byteCount: Int { ExecutionControlViewModel.steerByteCount(message) }
    private var isValid: Bool {
        !trimmed.isEmpty && byteCount <= ExecutionControlViewModel.maximumSteerBytes
    }

    var body: some View {
        NavigationView {
            VStack(alignment: .leading, spacing: 12) {
                Text("Adjust the active run without restarting it.")
                    .font(.themed(14))
                    .foregroundColor(themeManager.secondaryTextColor)
                TextEditor(text: $message)
                    .font(.themed(16))
                    .foregroundColor(themeManager.textColor)
                    .scrollContentBackground(.hidden)
                    .padding(10)
                    .frame(minHeight: 180)
                    .background(themeManager.surfaceColor)
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                    .overlay(
                        RoundedRectangle(cornerRadius: 10)
                            .stroke(themeManager.secondaryTextColor.opacity(0.2), lineWidth: 1)
                    )
                Text("\(byteCount) / \(ExecutionControlViewModel.maximumSteerBytes) bytes")
                    .font(.themed(12))
                    .foregroundColor(byteCount > ExecutionControlViewModel.maximumSteerBytes
                                     ? themeManager.dangerColor : themeManager.secondaryTextColor)
                Spacer()
            }
            .padding()
            .background(themeManager.backgroundColor.ignoresSafeArea())
            .navigationTitle("Steer Run")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Send") {
                        if onSend(trimmed) {
                            dismiss()
                        }
                    }
                    .disabled(!isValid || isBusy)
                }
            }
        }
    }
}

struct ShareSheet: UIViewControllerRepresentable {
    let items: [Any]
    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }
    func updateUIViewController(_ uiViewController: UIActivityViewController, context: Context) {}
}
