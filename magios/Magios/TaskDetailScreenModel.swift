import Foundation
import Combine

/// The task detail screen's data and its pure rules (design §3,
/// `docs/archive/plans/2026-10-01-mobile-lively-chat-and-task-detail-design.md`).
/// The view (`TaskDetailScreen`) reads only what is here plus the shared
/// models (`TaskActs`, `ActSummaries`, `PlanSteps`, `PlanProgress`,
/// `TaskOutputGroups`, `TaskVerdict`).

// MARK: - Route

/// Where a task detail screen opens: every entry point (Tasks list, chat card,
/// activity Steps strip, notifications, Attention's Review plan) pushes one of
/// these. Hashable by the identity fields only, so a `TaskV3` riding along for
/// the seed does not change which screen it is.
struct TaskDetailRoute: Identifiable, Hashable {
    let taskId: String
    /// The run to inspect (a chat turn's run, a history pick). Nil: the task's
    /// active run, else its latest.
    var executionId: String? = nil
    /// The act to open instead of the state's default.
    var focus: TaskDetailFocus? = nil
    /// Opened from a run's Steps strip: the Run act opens.
    var inspectRun = false
    /// Shown until the task loads.
    var titleHint: String? = nil
    var statusHint: String? = nil
    /// The list row the screen was opened from, when there is one.
    var task: TaskV3? = nil

    var id: String { "\(taskId)|\(executionId ?? "")" }

    init(taskId: String, executionId: String? = nil, focus: TaskDetailFocus? = nil,
         inspectRun: Bool = false, titleHint: String? = nil, statusHint: String? = nil) {
        self.taskId = taskId
        self.executionId = executionId
        self.focus = focus
        self.inspectRun = inspectRun
        self.titleHint = titleHint
        self.statusHint = statusHint
    }

    init(task: TaskV3, focus: TaskDetailFocus? = nil) {
        self.init(taskId: task.id, focus: focus, titleHint: task.title, statusHint: task.status)
        self.task = task
    }

    /// A chat task card's model (the same shape `DeepWorkPanel(task:)` took).
    init(statusModel: TaskStatusModel, inspectRun: Bool = false) {
        self.init(taskId: statusModel.taskId, executionId: statusModel.executionId,
                  inspectRun: inspectRun, titleHint: statusModel.title, statusHint: statusModel.status)
        self.statusModel = statusModel
    }

    fileprivate(set) var statusModel: TaskStatusModel? = nil

    var seed: TaskDetailSeed {
        if let task { return TaskDetailSeed(task) }
        var model = statusModel ?? TaskStatusModel(
            taskId: taskId, title: titleHint ?? "", status: statusHint ?? "", steps: [])
        if executionId != nil { model.executionId = executionId }
        return TaskDetailSeed(model)
    }

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.taskId == rhs.taskId && lhs.executionId == rhs.executionId
            && lhs.focus == rhs.focus && lhs.inspectRun == rhs.inspectRun
    }

    func hash(into hasher: inout Hasher) {
        hasher.combine(taskId)
        hasher.combine(executionId)
        hasher.combine(focus)
        hasher.combine(inspectRun)
    }
}

// MARK: - Pure layout rules

enum TaskDetailLayout {
    /// The acts in the order the screen renders them: result first when
    /// finished, run first while running (`TaskActs.actOrder`).
    static func actOrder(_ snapshot: TaskDetailSnapshot, now: Date) -> [ActId] {
        TaskActs.actOrder(state: snapshot.verdict(now: now).state, acts: snapshot.acts)
    }

    /// The one act open when the screen appears. An entry point's focus wins
    /// when its act exists; a run inspection opens Run; else the state's
    /// default (`TaskActs.defaultOpenAct`).
    static func initialOpenAct(_ snapshot: TaskDetailSnapshot, focus: TaskDetailFocus?,
                               inspectRun: Bool, now: Date) -> ActId? {
        let acts = snapshot.acts
        if let wanted = focus.map(act(for:)), acts.contains(wanted) { return wanted }
        if inspectRun, acts.contains(.run) { return .run }
        return TaskActs.defaultOpenAct(
            state: snapshot.verdict(now: now).state, acts: acts, attention: snapshot.attention?.source)
    }

    static func act(for focus: TaskDetailFocus) -> ActId {
        switch focus {
        case .result: return .output
        case .plan: return .plan
        }
    }

    /// The Output act: deliverables only.
    static func outputFiles(_ snapshot: TaskDetailSnapshot) -> [TaskDetailArtifact] {
        snapshot.outputGroups.deliverables
    }

    /// Technical details: run-owned files, persisted evidence and the task's
    /// other files ("Other task files"), never a deliverable.
    static func technicalSections(_ snapshot: TaskDetailSnapshot) -> [TaskOutputSection] {
        snapshot.outputGroups.intermediates
    }

    /// The deliverable previewed in place by default: the first one whose kind
    /// reads inline (text, markdown, JSON, image). The previewed file keeps
    /// its row.
    static func defaultPreview(_ files: [TaskDetailArtifact]) -> TaskDetailArtifact? {
        files.first { previewable($0) }
    }

    static func previewable(_ artifact: TaskDetailArtifact) -> Bool {
        let kind = ActSummaries.outputKind(mediaType: artifact.mediaType, path: artifact.relativePath)
        if kind == .image { return true }
        let ext = (artifact.relativePath as NSString).pathExtension.lowercased()
        let mime = (artifact.mediaType ?? "").lowercased()
        return ["md", "markdown", "txt", "json", "csv", "log", "yaml", "yml"].contains(ext)
            || mime.hasPrefix("text/") || mime == "application/json"
    }

    /// The one line a closed act card carries.
    static func summary(for act: ActId, _ snapshot: TaskDetailSnapshot, now: Date) -> String {
        switch act {
        case .plan:
            return ActSummaries.planSummary(PlanSummaryInput(
                status: TaskPlanStatus(wire: snapshot.planStatus),
                approvedAt: snapshot.planUpdatedAt,
                questions: snapshot.questions.map(\.text),
                now: now))
        case .run:
            // The run's calls, tokens and cost close the line when reported.
            return TaskUsageFormat.runSummary(runSummary(snapshot, now: now), run: snapshot.usage?.run)
        case .output:
            return ActSummaries.outputSummary(artifacts: outputFiles(snapshot))
        }
    }

    private static func runSummary(_ snapshot: TaskDetailSnapshot, now: Date) -> String {
        if !snapshot.planStepRows.isEmpty, isLive(snapshot) {
            return PlanSteps.summary(snapshot.planStepRows)
        }
        let live = isLive(snapshot)
        return ActSummaries.runSummary(RunSummaryInput(
            steps: snapshot.steps.count,
            retries: 0,
            elapsed: snapshot.runDuration(at: now),
            currentStep: live ? snapshot.currentStep.map { $0 + 1 } : nil,
            totalSteps: snapshot.steps.isEmpty ? nil : snapshot.steps.count,
            events: snapshot.activity.count))
    }

    /// The task's total usage (run + planning) for the header, or nil.
    static func usageTotal(_ snapshot: TaskDetailSnapshot) -> TaskUsageTotal? {
        TaskUsageModel.total(live: nil, snapshot: snapshot.usage)
    }

    /// The Plan act keeps its summary while open (web `summaryWhenOpen`): its
    /// body lists steps, never the plan's state.
    static func keepsSummaryWhenOpen(_ act: ActId) -> Bool { act == .plan }

    /// The run is live: planning, running or executing.
    static func isLive(_ snapshot: TaskDetailSnapshot) -> Bool {
        isActive(status: snapshot.selectedExecutionStatus ?? snapshot.status)
    }

    static func isActive(status: String) -> Bool {
        ["planning", "running", "executing", "in_progress"].contains(status.lowercased())
    }

    /// Poll `/execution-panel` only while the screen is visible (and the app
    /// in the foreground) and the task is planning or running (design §3
    /// "Live updates").
    static func shouldPoll(visible: Bool, status: String, planStatus: String? = nil) -> Bool {
        guard visible else { return false }
        return isActive(status: status) || planStatus?.lowercased() == "planning"
    }

    static let pollInterval: TimeInterval = 4

    /// The compact title bar shows once the header card's bottom edge has
    /// scrolled above the top of the scroll view.
    static func compactTitleVisible(headerBottom: CGFloat) -> Bool {
        headerBottom <= 0
    }

    /// Planning activity is a live list while planning, then folds.
    static func planningActivityIsLive(_ snapshot: TaskDetailSnapshot) -> Bool {
        snapshot.planStatus?.lowercased() == "planning"
    }

    /// The Plan act's steps: the plan graph's, else the selected run's.
    static func planSteps(_ snapshot: TaskDetailSnapshot) -> [String] {
        if !snapshot.planGraphSteps.isEmpty { return snapshot.planGraphSteps }
        if !snapshot.planStepRows.isEmpty { return snapshot.planStepRows.map(\.name) }
        return snapshot.steps.map(\.name)
    }

    /// `Markdown · 2 KB`: line 1's trailing meta for an output row.
    static func fileMeta(_ artifact: TaskDetailArtifact) -> String {
        let ref = ArtifactRef.taskOutput(taskId: "", relativePath: artifact.relativePath, mime: artifact.mediaType)
        let size = artifact.sizeBytes.map {
            ByteCountFormatter.string(fromByteCount: Int64($0), countStyle: .file)
        }
        return [ref.kind.label, size].compactMap { $0 }.joined(separator: " · ")
    }

    /// What `Copy path` puts on the pasteboard: the absolute path when the
    /// server sent one, else the task-relative path.
    static func copyPath(_ artifact: TaskDetailArtifact) -> String {
        artifact.absolutePath ?? artifact.relativePath
    }
}

// MARK: - Lifecycle actions

/// One lifecycle action as an HTTP call under `/api/magician/v3`. Pure, so
/// the menu's wiring is testable; the screen sends it.
struct TaskDetailActionRequest: Equatable {
    let method: String
    let path: String
    let body: [String: String]?
    let notice: String

    static func make(_ action: TaskDetailAction, taskId: String, planId: String?,
                     synthesisExecutionId: String?, lifecycle: String?) -> TaskDetailActionRequest? {
        guard !taskId.hasPrefix("agent-cycle:") else { return nil }
        if TaskWorkOwnership.isAppManaged(taskId) { return nil }
        let id = encode(taskId)
        switch action {
        case .run: return .init(method: "POST", path: "/tasks/\(id)/execute", body: [:], notice: "Run started.")
        case .plan: return .init(method: "POST", path: "/tasks/\(id)/plan", body: [:], notice: "Planning started.")
        case .reset, .markNotDone:
            return .init(method: "PUT", path: "/tasks/\(id)/status", body: ["status": "ready"], notice: "Task reset to ready.")
        case .markComplete:
            return .init(method: "PUT", path: "/tasks/\(id)/status", body: ["status": "completed"], notice: "Task marked complete.")
        case .cancelTask:
            return .init(method: "PUT", path: "/tasks/\(id)/status", body: ["status": "cancelled"], notice: "Task cancelled.")
        case .approvePlan:
            guard let planId, !planId.isEmpty else { return nil }
            return .init(method: "POST", path: "/tasks/\(id)/plan/approve?plan_id=\(encode(planId))", body: [:], notice: "Plan approved.")
        case .rejectPlan:
            guard let planId, !planId.isEmpty else { return nil }
            return .init(method: "POST", path: "/tasks/\(id)/plan/reject?plan_id=\(encode(planId))", body: [:], notice: "Plan rejected.")
        case .replan:
            return .init(method: "POST", path: "/tasks/\(id)/plan/replan", body: [:], notice: "Replanning started.")
        case .retrySynthesis:
            guard let exec = synthesisExecutionId?.trimmingCharacters(in: .whitespaces), !exec.isEmpty else { return nil }
            return .init(method: "POST", path: "/tasks/\(id)/executions/\(encode(exec))/retry-synthesis", body: nil,
                         notice: "Retrying output synthesis.")
        case .delete:
            let isInternal = ["internal", "ephemeral_owned_by_chat", "internal_debug"].contains(lifecycle ?? "")
            return .init(method: "DELETE",
                         path: isInternal ? "/tasks/internal/\(id)" : "/tasks/\(id)?remove_files=false",
                         body: nil, notice: "Task deleted.")
        }
    }

    private static func encode(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-._~")))
            ?? value
    }
}

/// Which lifecycle actions the ⋯ menu offers for a snapshot. Mirrors
/// `DeepWorkPanel`'s header button, plan actions and overflow menu, so moving
/// to this screen loses none of them.
enum TaskDetailMenu {
    static func actions(_ snapshot: TaskDetailSnapshot) -> [TaskDetailAction] {
        guard !snapshot.isExecutionOnly else { return [] }
        let status = snapshot.status.lowercased()
        let plan = snapshot.planStatus?.lowercased()
        let live = snapshot.activeRootExecutionId != nil
        var actions: [TaskDetailAction] = []
        if !live {
            if plan == "approved" && status == "ready" { actions.append(.run) }
            else if ["pending", "ready"].contains(status), !["planning", "eliciting", "draft"].contains(plan ?? "") {
                actions.append(.run)
            }
            if status == "pending", !snapshot.hasPlan { actions.append(.plan) }
        }
        if plan == "draft" || plan == "eliciting" { actions += [.approvePlan, .rejectPlan] }
        if snapshot.hasPlan, plan != "planning" { actions.append(.replan) }
        if snapshot.synthesisFailedExecutionId != nil, !snapshot.synthesisPending { actions.append(.retrySynthesis) }
        if ["paused", "failed", "cancelled", "canceled"].contains(status), !live { actions.append(.reset) }
        if status == "completed" {
            actions.append(.markNotDone)
        } else if !["running", "planning", "paused", "executing", "queued"].contains(status) {
            actions.append(.markComplete)
        }
        if !["completed", "cancelled", "canceled"].contains(status) { actions.append(.cancelTask) }
        actions.append(.delete)
        return actions
    }

    static func title(_ action: TaskDetailAction) -> String {
        switch action {
        case .run: return "Run now"
        case .plan: return "Plan"
        case .reset: return "Reset to Ready"
        case .markComplete: return "Mark complete"
        case .markNotDone: return "Mark not done"
        case .cancelTask: return "Cancel task"
        case .approvePlan: return "Approve plan"
        case .rejectPlan: return "Reject plan"
        case .replan: return "Replan"
        case .retrySynthesis: return "Retry output synthesis"
        case .delete: return "Delete task"
        }
    }

    static func icon(_ action: TaskDetailAction) -> String {
        switch action {
        case .run: return "play.fill"
        case .plan: return "sparkles"
        case .reset, .markNotDone: return "arrow.uturn.backward"
        case .markComplete: return "checkmark.circle"
        case .cancelTask: return "xmark.circle"
        case .approvePlan: return "checkmark.seal"
        case .rejectPlan: return "hand.thumbsdown"
        case .replan: return "arrow.triangle.2.circlepath"
        case .retrySynthesis: return "wand.and.stars"
        case .delete: return "trash"
        }
    }
}

// MARK: - View model

/// Loads the five task endpoints, keeps them, and rebuilds the snapshot with
/// `TaskDetailSnapshot.parse` (the same parsing `DeepWorkPanel` uses). The
/// 4 s poll refetches only the panel and the plan; the realtime hub
/// applies pushed panels in place (`applyingRealtimePanel`) and a hub resync
/// refetches everything.
@MainActor
final class TaskDetailScreenModel: ObservableObject {
    typealias Load = (String) async throws -> [String: Any]

    @Published private(set) var snapshot: TaskDetailSnapshot?
    @Published private(set) var isLoading = false
    @Published private(set) var errorMessage: String?
    @Published private(set) var selectedExecutionId: String?
    @Published var notice: String?
    @Published var actionError: String?
    @Published private(set) var historyLoading = false
    @Published private(set) var historyError: String?
    private var historyExpanded = false
    var recurringSchedule: [String: Any]? { details?["recurring_schedule"] as? [String: Any] }
    var historyHasMore: Bool { (details?["next_execution_cursor"] as? String)?.isEmpty == false }
    var appInstallationID: String? {
        let record = details?["task"] as? [String: Any] ?? task?["task"] as? [String: Any] ?? task ?? [:]
        let manifest = record["manifest"] as? [String: Any] ?? record
        return TaskWorkOwnership.appInstallation(taskID: seed.taskId, threadID: manifest["ui_thread_id"] as? String ?? snapshot?.threadId, createdBy: manifest["created_by"] as? String ?? snapshot?.createdBy)
    }
    static func mergeHistory(_ previous: [String: Any]?, _ page: [String: Any]) -> [String: Any] {
        var result = previous ?? [:]; page.forEach { result[$0.key] = $0.value }
        result["next_execution_cursor"] = page["next_execution_cursor"]
        func id(_ row: [String: Any]) -> String? { ((row["state"] as? [String: Any]) ?? row)["execution_id"] as? String }
        let fresh = page["executions"] as? [[String: Any]] ?? []
        var updates: [String: [String: Any]] = [:]; fresh.forEach { if let key = id($0) { updates[key] = $0 } }
        let rows = (previous?["executions"] as? [[String: Any]] ?? []).map { row in id(row).flatMap { updates[$0] } ?? row } + fresh
        var seen = Set<String>()
        result["executions"] = rows.filter { row in guard let key = id(row) else { return false }; return seen.insert(key).inserted }
        return result
    }
    func loadOlderExecutions() async {
        guard connectionCurrent, !historyLoading, let cursor = details?["next_execution_cursor"] as? String, !cursor.isEmpty else { return }
        let current = generation; historyLoading = true; historyError = nil
        defer { if current == generation { historyLoading = false } }
        do {
            let page = try await load("/tasks/\(encodedTaskId)/details?cursor=\(WorkAPIClient.query(cursor))")
            guard connectionCurrent, current == generation else { return }
            details = Self.mergeHistory(details, page); historyExpanded = true
            if page["next_execution_cursor"] as? String == cursor { details?["next_execution_cursor"] = nil; historyError = "The server repeated its history cursor. Refresh to retry." }
            rebuild()
        } catch { if current == generation && connectionCurrent { historyError = "Older runs could not be loaded. Retry." } }
    }
    @Published private(set) var busyAction: TaskDetailAction?

    let seed: TaskDetailSeed
    private let load: Load
    private let realtime: TaskDetailRealtime?
    private var generation: UInt64 = 0
    private let origin = MagicianAccess.baseURL
    private let headers = MagicianAccess.headers()
    private var connectionObserver: AnyCancellable?
    private var connectionCurrent: Bool { origin == MagicianAccess.baseURL && headers == MagicianAccess.headers() }
    private var task: [String: Any]?
    private var panel: [String: Any]?
    private var outputs: [String: Any]?
    private var details: [String: Any]?
    private var plan: [String: Any]?

    init(seed: TaskDetailSeed, executionId: String? = nil, realtime: TaskDetailRealtime? = nil, load: @escaping Load) {
        self.seed = seed
        self.load = load
        self.realtime = realtime
        selectedExecutionId = executionId ?? seed.activeRootExecutionId ?? seed.latestRootExecutionId
        connectionObserver = NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)
            .receive(on: RunLoop.main).sink { [weak self] _ in
                guard let self else { return }
                generation &+= 1; stopRealtime(); snapshot = nil; task = nil; panel = nil; outputs = nil; details = nil; plan = nil
                isLoading = false; errorMessage = "Connection changed. Reopen this Work item."; actionError = errorMessage
            }
    }

    convenience init(route: TaskDetailRoute, session: URLSession = .shared) {
#if DEBUG
        if ProcessInfo.processInfo.arguments.contains("--work-ui-test-fixture"), route.taskId.hasPrefix("agent-cycle:") {
            self.init(seed: route.seed, executionId: route.executionId) { path in
                TaskDetailFixtures.payload(path: path, taskId: "fixture-detail-completed")
            }
            return
        }
        if ProcessInfo.processInfo.arguments.contains("--tasks-ui-test-fixture")
            || TaskDetailFixtures.isFixture(route.taskId) {
            self.init(seed: route.seed, executionId: route.executionId) { path in
                TaskDetailFixtures.payload(path: path, taskId: route.taskId)
            }
            return
        }
#endif
        let origin = MagicianAccess.baseURL, headers = MagicianAccess.headers()
        self.init(seed: route.seed, executionId: route.executionId, realtime: TaskDetailRealtime()) { path in
            guard origin == MagicianAccess.baseURL, headers == MagicianAccess.headers(),
                  let url = URL(string: origin.absoluteString + "/api/magician/v3" + path) else { throw URLError(.cancelled) }
            var request = URLRequest(url: url); request.timeoutInterval = 15
            headers.forEach { request.setValue($0.value, forHTTPHeaderField: $0.key) }
            let (data, response) = try await session.data(for: request)
            guard origin == MagicianAccess.baseURL, headers == MagicianAccess.headers(),
                  let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode),
                  let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else { throw URLError(.badServerResponse) }
            return object
        }
    }

    private var encodedTaskId: String {
        seed.taskId.addingPercentEncoding(
            withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-._~"))) ?? seed.taskId
    }

    private var panelPath: String {
        if seed.taskId.hasPrefix("agent-cycle:"), let execution = selectedExecutionId, !execution.isEmpty {
            return "/executions/\(WorkAPIClient.query(execution))/execution-panel"
        }
        guard let execution = selectedExecutionId, !execution.isEmpty else {
            return "/tasks/\(encodedTaskId)/execution-panel"
        }
        let encoded = WorkAPIClient.query(execution)
        return "/tasks/\(encodedTaskId)/execution-panel?execution_id=\(encoded)"
    }

    /// Everything: task, panel, outputs, details, plan.
    func refresh() async {
        guard connectionCurrent else { snapshot = nil; errorMessage = "Connection changed. Reopen this Work item."; return }
        generation &+= 1; historyLoading = false; historyError = nil
        let current = generation
        isLoading = snapshot == nil
        errorMessage = nil
        let id = encodedTaskId
        let panelPath = self.panelPath
        let load = self.load
        if seed.taskId.hasPrefix("agent-cycle:") {
            guard selectedExecutionId?.isEmpty == false else { isLoading = false; errorMessage = "This cycle has no execution to inspect."; return }
            let fresh = try? await load(panelPath)
            guard current == generation, connectionCurrent else { return }
            isLoading = false
            guard let fresh else { errorMessage = "Cycle execution could not be loaded."; return }
            panel = fresh; rebuild(); return
        }
        async let taskPayload = try? load("/tasks/\(id)")
        async let panelPayload = try? load(panelPath)
        async let outputsPayload = try? load("/tasks/\(id)/outputs")
        async let detailsPayload = try? load("/tasks/\(id)/details")
        async let planPayload = try? load("/tasks/\(id)/plan")
        let values = await (taskPayload, panelPayload, outputsPayload, detailsPayload, planPayload)
        guard current == generation else { return }
        isLoading = false
        guard values.0 != nil || values.1 != nil || values.3 != nil else {
            if snapshot == nil { errorMessage = "Task details could not be loaded." }
            return
        }
        let previousDetails = details
        (task, panel, outputs, details, plan) = values
        if historyExpanded, let fresh = details {
            details = Self.mergeHistory(previousDetails, fresh)
            details?["next_execution_cursor"] = previousDetails?["next_execution_cursor"]
        }
        rebuild()
    }

    /// The live poll: the panel and the plan only. A status that changed
    /// (a run ended, a plan finished) triggers a full refresh so outputs and
    /// history follow.
    func pollPanel() async {
        guard connectionCurrent else { return }
        let before = (snapshot?.status, snapshot?.planStatus)
        let current = generation
        let load = self.load
        let panelPath = self.panelPath
        if seed.taskId.hasPrefix("agent-cycle:") {
            if let fresh = try? await load(panelPath), current == generation, connectionCurrent { panel = fresh; rebuild() }
            return
        }
        let planPath = "/tasks/\(encodedTaskId)/plan"
        async let panelPayload = try? load(panelPath)
        async let planPayload = try? load(planPath)
        let values = await (panelPayload, planPayload)
        guard current == generation else { return }
        if let fresh = values.0 { panel = fresh }
        if let fresh = values.1 { plan = fresh }
        rebuild()
        if (snapshot?.status, snapshot?.planStatus) != before { await refresh() }
    }

    private func rebuild() {
        var unavailable: [String] = []
        if panel == nil { unavailable.append("live run") }
        if !seed.taskId.hasPrefix("agent-cycle:") {
            if outputs == nil { unavailable.append("outputs") }
            if details == nil { unavailable.append("execution history") }
        }
        var parsed = TaskDetailSnapshot.parse(
            seed: seed, taskPayload: task, panelPayload: panel, outputsPayload: outputs,
            detailsPayload: details, planPayload: plan, unavailableSections: unavailable)
        if parsed.progressExecutionId != nil, parsed.progressExecutionId == snapshot?.progressExecutionId {
            parsed.lastProgressAt = TaskRunEvidence.latest(snapshot?.lastProgressAt, parsed.lastProgressAt)
        }
        snapshot = parsed
        if selectedExecutionId == nil { selectedExecutionId = parsed.selectedExecutionId }
        realtime?.update(executionId: selectedExecutionId)
    }

    func selectExecution(_ executionId: String) {
        guard executionId != selectedExecutionId else { return }
        selectedExecutionId = executionId
        realtime?.update(executionId: executionId)
        Task { await refresh() }
    }

    func startRealtime() {
        realtime?.start(
            executionId: selectedExecutionId,
            onPanelState: { [weak self] panel in self?.applyRealtimePanel(panel) },
            onProgress: { [weak self] root, at in
                guard let self, self.connectionCurrent, self.snapshot?.progressExecutionId == root else { return }
                guard at.timeIntervalSince(self.snapshot?.lastProgressAt ?? .distantPast) >= 1 else { return }
                self.snapshot?.lastProgressAt = at
            },
            onResync: { [weak self] in Task { await self?.refresh() } }
        )
    }

    func stopRealtime() { realtime?.stop() }

    func applyRealtimePanel(_ pushed: [String: Any]) {
        guard connectionCurrent, let current = snapshot,
              let updated = current.applyingRealtimePanel(seed: seed, panelPayload: pushed) else { return }
        panel = pushed
        snapshot = updated
    }

    /// Send a lifecycle action. True when the server accepted it.
    @discardableResult
    func perform(_ action: TaskDetailAction, session: URLSession = .shared) async -> Bool {
        guard connectionCurrent else { actionError = "Connection changed. Reopen this Work item."; return false }
        guard busyAction == nil, let snapshot,
              let request = TaskDetailActionRequest.make(
                action, taskId: seed.taskId, planId: snapshot.planId,
                synthesisExecutionId: snapshot.synthesisFailedExecutionId, lifecycle: snapshot.lifecycle),
              let url = URL(string: "\(origin.absoluteString)/api/magician/v3\(request.path)")
        else { return false }
        busyAction = action
        defer { busyAction = nil }
        var urlRequest = URLRequest(url: url)
        urlRequest.httpMethod = request.method
        headers.forEach { urlRequest.setValue($0.value, forHTTPHeaderField: $0.key) }
        if let body = request.body {
            urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
            urlRequest.httpBody = try? JSONSerialization.data(withJSONObject: body)
        }
        do {
            let (data, response) = try await session.data(for: urlRequest)
            guard connectionCurrent else { return false }
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            guard (200..<300).contains(status) else {
                let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
                actionError = (body?["message"] as? String) ?? (body?["error"] as? String)
                    ?? "\(TaskDetailMenu.title(action)) failed (HTTP \(status))."
                return false
            }
            notice = request.notice
            if action != .delete { await refresh() }
            return true
        } catch {
            actionError = error.localizedDescription
            return false
        }
    }
}
