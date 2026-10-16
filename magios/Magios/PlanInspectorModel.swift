import Foundation
import SwiftUI

/// Opens the Plan Inspector for a task. `tab` picks the first view (Review
/// plan opens the Graph with the approval bar).
struct PlanInspectorRoute: Identifiable, Hashable {
    let taskId: String
    var title: String? = nil
    var tab: PlanInspectorTab = .graph
    /// The plan's markdown document, when the caller has it: a secondary
    /// view in the More menu.
    var planDocument: String? = nil

    var id: String { taskId }
}

enum PlanInspectorTab: String, CaseIterable, Identifiable, Hashable {
    case graph, steps, history
    var id: String { rawValue }
    var title: String {
        switch self {
        case .graph: return "Graph"
        case .steps: return "Steps"
        case .history: return "History"
        }
    }
}

/// The More menu's panes.
enum PlanInspectorPane: String, CaseIterable, Identifiable {
    case analysis, slots, attempts, clarifications
    var id: String { rawValue }
    var title: String {
        switch self {
        case .analysis: return "Analysis"
        case .slots: return "Slots"
        case .attempts: return "Attempts"
        case .clarifications: return "Clarifications"
        }
    }
    var icon: String {
        switch self {
        case .analysis: return "text.magnifyingglass"
        case .slots: return "square.grid.2x2"
        case .attempts: return "arrow.triangle.branch"
        case .clarifications: return "questionmark.bubble"
        }
    }
}

/// A lazily loaded pane's state.
enum PlanLoadable<Value: Equatable>: Equatable {
    case idle, loading, loaded(Value), failed(String)

    var value: Value? { if case .loaded(let value) = self { return value } else { return nil } }
}

/// Draft plan decisions on the approval bar.
enum PlanDecision: Equatable {
    case reject, approve, approveAndRun
}

/// The Plan Inspector's state and actions. Port of the state machine in the
/// web `ExecutionPlanInspector.svelte` (stale responses dropped by request
/// generation; edit, save, history, restore, clarifications, manual resume).
@MainActor
final class PlanInspectorModel: ObservableObject {
    let route: PlanInspectorRoute
    let client: PlanInspectorClient

    @Published var tab: PlanInspectorTab
    @Published private(set) var record: PlanRecord?
    @Published private(set) var hasLoaded = false
    @Published private(set) var isLoading = false
    @Published private(set) var errorMessage: String?
    /// One transient line (saved, restored, approved…).
    @Published var notice: String?

    // Editing
    @Published private(set) var isEditing = false
    @Published var draft: PlanGraph?
    @Published private(set) var draftBaseline: PlanGraph?
    @Published private(set) var isSaving = false

    // History
    @Published private(set) var versions: [PlanVersionSummary] = []
    @Published private(set) var hasMoreVersions = false
    @Published private(set) var versionsLoading = false
    @Published private(set) var versionsError: String?
    @Published private(set) var preview: PlanVersionSummary?
    @Published private(set) var previewGraph: PlanLoadable<PlanGraph> = .idle
    @Published private(set) var isRestoring = false

    // More panes
    @Published private(set) var analysis: PlanLoadable<PlanJSON?> = .idle
    @Published private(set) var slots: PlanLoadable<PlanSlotSnapshot?> = .idle
    @Published private(set) var attempts: PlanLoadable<[PlanStrategyAttempt]> = .idle
    @Published private(set) var clarifications: PlanLoadable<[PlanClarificationEntry]> = .idle

    // Clarifications
    @Published var answers: [String: String] = [:]
    @Published private(set) var answering: Set<String> = []
    @Published private(set) var answerErrors: [String: String] = [:]
    @Published var resumeForm = PlanManualResume()
    @Published private(set) var isResuming = false
    @Published private(set) var resumeError: String?

    // Approval
    @Published private(set) var deciding: PlanDecision?

    // Live run marks (from `/execution-panel`)
    @Published private(set) var runMarks: [String: PlanStepMark] = [:]
    @Published private(set) var runActive = false

    private var planGeneration: UInt64 = 0
    private var versionsGeneration: UInt64 = 0
    private var previewGeneration: UInt64 = 0
    private var liveTask: Task<Void, Never>?
    /// Seconds between live polls while a run is active.
    var livePollInterval: UInt64 = 4

    init(route: PlanInspectorRoute, client: PlanInspectorClient) {
        self.route = route
        self.client = client
        tab = route.tab
    }

    convenience init(route: PlanInspectorRoute) {
#if DEBUG
        if let transport = PlanInspectorFixtures.transport(for: route.taskId) {
            self.init(route: route, client: PlanInspectorClient(taskId: route.taskId, transport: transport))
            return
        }
#endif
        self.init(route: route, client: PlanInspectorClient(taskId: route.taskId))
    }

    // MARK: Derived state

    var status: PlanStatus? { record?.status }
    var graph: PlanGraph? { record?.graph }
    /// What the Graph and Steps views draw: the draft while editing.
    var shownGraph: PlanGraph? { isEditing ? draft : graph }
    var canEdit: Bool { Self.canEdit(status: status, hasGraph: graph != nil) }
    var isDirty: Bool { Self.isDirty(editing: isEditing, draft: draft, baseline: draftBaseline) }
    var showsApprovalBar: Bool {
        Self.showsApprovalBar(status: status, planId: record?.planId, editing: isEditing)
    }
    var pendingQuestions: [PlanQuestion] { record?.pendingQuestions ?? [] }
    /// The manual resume form is for a planner still waiting on inputs.
    var canResume: Bool { status == .planning || status == .eliciting }
    /// Replan restarts planning for a plan that is not approved or running.
    var canReplan: Bool { status == .draft || status == .rejected || status == .failed }

    static func canEdit(status: PlanStatus?, hasGraph: Bool) -> Bool { hasGraph && (status?.canEdit ?? false) }

    static func isDirty(editing: Bool, draft: PlanGraph?, baseline: PlanGraph?) -> Bool {
        editing && draft != nil && draft != baseline
    }

    static func showsApprovalBar(status: PlanStatus?, planId: String?, editing: Bool) -> Bool {
        status == .draft && !(planId ?? "").isEmpty && !editing
    }

    // MARK: Loading

    func load() async {
        planGeneration &+= 1
        let generation = planGeneration
        isLoading = true
        errorMessage = nil
        do {
            let loaded = try await client.loadPlan()
            guard generation == planGeneration else { return }
            record = loaded
            hasLoaded = true
        } catch {
            guard generation == planGeneration else { return }
            errorMessage = error.localizedDescription
            hasLoaded = true
        }
        isLoading = false
    }

    /// The toolbar's Refresh: the plan, history, and any pane already opened.
    func refresh() async {
        async let plan: Void = load()
        async let history: Void = loadVersions()
        _ = await (plan, history)
        for pane in PlanInspectorPane.allCases where paneState(pane) != .idle { await loadPane(pane) }
        await refreshLiveMarks()
    }

    func loadVersions() async {
        versionsGeneration &+= 1
        let generation = versionsGeneration
        versionsLoading = true
        versionsError = nil
        do {
            let page = try await client.versions(limit: 20)
            guard generation == versionsGeneration else { return }
            versions = page.versions
            hasMoreVersions = page.hasMore
        } catch {
            guard generation == versionsGeneration else { return }
            versionsError = error.localizedDescription
        }
        versionsLoading = false
    }

    // MARK: Editing

    func beginEditing() {
        guard canEdit, let graph else { return }
        var sorted = graph
        sorted.sortByWaterfall()
        draft = sorted
        draftBaseline = sorted
        isEditing = true
    }

    /// Leave edit mode, dropping the draft.
    func discardEdits() {
        isEditing = false
        draft = nil
        draftBaseline = nil
    }

    func editDraft(_ change: (inout PlanGraph) -> Void) {
        guard isEditing, var graph = draft else { return }
        change(&graph)
        draft = graph
    }

    /// Change a dependency, then re-sort into waterfall order as the web does.
    @discardableResult
    func setDependency(_ stepId: String, on dep: String, _ enabled: Bool) -> Bool {
        var changed = false
        editDraft { graph in
            changed = graph.setDependency(stepId, on: dep, enabled)
            if changed { graph.sortByWaterfall() }
        }
        return changed
    }

    func makeStart(_ stepId: String) {
        editDraft { graph in
            graph.makeStart(stepId)
            graph.sortByWaterfall()
        }
    }

    @discardableResult
    func addStep() -> String? {
        var id: String?
        editDraft { id = $0.addStep() }
        return id
    }

    /// `PUT /plan` with the draft; on success leave edit mode and reload.
    @discardableResult
    func save(label: String?) async -> Bool {
        guard isEditing, let draft, !isSaving else { return false }
        isSaving = true
        errorMessage = nil
        defer { isSaving = false }
        do {
            let saved = try await client.savePlan(draft, label: label)
            record = PlanRecord.merging(saved, fallback: record, graph: draft)
            discardEdits()
            notice = "Plan saved."
            async let plan: Void = load()
            async let history: Void = loadVersions()
            _ = await (plan, history)
            return true
        } catch {
            errorMessage = error.localizedDescription
            return false
        }
    }

    // MARK: History

    func previewVersion(_ version: PlanVersionSummary) async {
        previewGeneration &+= 1
        let generation = previewGeneration
        preview = version
        previewGraph = .loading
        do {
            let graph = try await client.version(version.epochMs)
            guard generation == previewGeneration else { return }
            previewGraph = graph.map { .loaded($0) } ?? .failed("This version has no plan graph.")
        } catch {
            guard generation == previewGeneration else { return }
            previewGraph = .failed(error.localizedDescription)
        }
    }

    func clearPreview() {
        previewGeneration &+= 1
        preview = nil
        previewGraph = .idle
    }

    /// Restore the previewed version (the screen confirms first), refetch,
    /// and show Steps.
    @discardableResult
    func restorePreview() async -> Bool {
        guard let preview, !isRestoring else { return false }
        isRestoring = true
        defer { isRestoring = false }
        do {
            let restored = try await client.restore(preview.epochMs)
            record = PlanRecord.merging(restored, fallback: record, graph: nil)
            discardEdits()
            async let plan: Void = load()
            async let history: Void = loadVersions()
            _ = await (plan, history)
            clearPreview()
            tab = .steps
            notice = "Plan version restored."
            return true
        } catch {
            previewGraph = .failed(error.localizedDescription)
            return false
        }
    }

    // MARK: More panes

    func paneState(_ pane: PlanInspectorPane) -> PlanLoadable<Bool> {
        func shape<T>(_ state: PlanLoadable<T>) -> PlanLoadable<Bool> {
            switch state {
            case .idle: return .idle
            case .loading: return .loading
            case .loaded: return .loaded(true)
            case .failed(let message): return .failed(message)
            }
        }
        switch pane {
        case .analysis: return shape(analysis)
        case .slots: return shape(slots)
        case .attempts: return shape(attempts)
        case .clarifications: return shape(clarifications)
        }
    }

    func loadPane(_ pane: PlanInspectorPane) async {
        switch pane {
        case .analysis:
            analysis = .loading
            do { analysis = .loaded(try await client.analysis()) } catch { analysis = .failed(error.localizedDescription) }
        case .slots:
            slots = .loading
            do { slots = .loaded(try await client.slots()) } catch { slots = .failed(error.localizedDescription) }
        case .attempts:
            attempts = .loading
            do { attempts = .loaded(try await client.attempts()) } catch { attempts = .failed(error.localizedDescription) }
        case .clarifications:
            clarifications = .loading
            do { clarifications = .loaded(try await client.clarifications()) } catch {
                clarifications = .failed(error.localizedDescription)
            }
        }
    }

    // MARK: Clarifications

    /// Answer a pending question (an option's value or the typed text). One
    /// request per question at a time.
    func answer(_ question: PlanQuestion, with value: String? = nil) async {
        let text = (value ?? answers[question.id] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, !answering.contains(question.id) else { return }
        answering.insert(question.id)
        answerErrors[question.id] = nil
        defer { answering.remove(question.id) }
        do {
            try await client.answer(questionId: question.id, text: text)
            answers[question.id] = nil
            notice = "Response sent."
            await load()
            await loadPane(.clarifications)
        } catch {
            answerErrors[question.id] = error.localizedDescription
        }
    }

    func submitResume() async {
        guard !isResuming else { return }
        resumeError = nil
        isResuming = true
        defer { isResuming = false }
        do {
            let resumed = try await client.resume(resumeForm)
            resumeForm = PlanManualResume()
            notice = resumed ? "Planning resumed."
                : "Manual resume recorded. Planning is still waiting on remaining inputs."
            await load()
            for pane in PlanInspectorPane.allCases { await loadPane(pane) }
        } catch {
            resumeError = error.localizedDescription
        }
    }

    // MARK: Approval

    func decide(_ decision: PlanDecision) async {
        guard deciding == nil, let planId = record?.planId else { return }
        deciding = decision
        defer { deciding = nil }
        let outcome: PlanApprovalOutcome
        switch decision {
        case .reject:
            do { try await client.reject(planId: planId); outcome = .rejected } catch { outcome = .failed(error.localizedDescription) }
        case .approve:
            do { try await client.approve(planId: planId); outcome = .approved } catch { outcome = .failed(error.localizedDescription) }
        case .approveAndRun:
            let client = self.client
            outcome = await PlanApprovalActions.approveAndRun(
                taskId: route.taskId,
                approve: {
                    do { try await client.approve(planId: planId); return .ok } catch { return .failed(error.localizedDescription) }
                },
                startRun: { _ in try await client.startRun() })
        }
        notice = outcome.message
        await load()
        if outcome == .approvedAndStarted { startLive() }
    }

    func replan() async {
        do {
            try await client.replan()
            discardEdits()
            notice = "Planning restarted."
            await load()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    // MARK: Live run marks

    /// Fetch the run's step marks once.
    func refreshLiveMarks() async {
        guard let panel = try? await client.executionPanel() else { return }
        apply(panel: panel)
    }

    func apply(panel: [String: Any]) {
        let selected = (panel["debug"] as? [String: Any])?["selected_execution"] as? [String: Any] ?? [:]
        let hasRun = !selected.isEmpty
        let ended = TaskDetailSnapshot.runHasEnded(
            status: selected["status"] as? String,
            endedAt: (selected["ended_at"] as? String).flatMap(TaskV3.parseISO)
                ?? (selected["ended_at"] as? NSNumber).map { Date(timeIntervalSince1970: $0.doubleValue / 1000) })
        let rows = PlanSteps.rows(fromPanel: panel, runEnded: ended)
        runActive = hasRun && !ended
        runMarks = graph.map { Self.marks(rows: rows, graph: $0) } ?? [:]
    }

    /// Map run rows onto plan steps: by step id, else by position.
    static func marks(rows: [PlanStepRow], graph: PlanGraph) -> [String: PlanStepMark] {
        var marks: [String: PlanStepMark] = [:]
        let ids = Set(graph.steps.map(\.id))
        for row in rows {
            if let stepId = row.stepId, ids.contains(stepId) {
                marks[stepId] = row.mark
            } else if graph.steps.indices.contains(row.number - 1) {
                let id = graph.steps[row.number - 1].id
                if marks[id] == nil { marks[id] = row.mark }
            }
        }
        return marks
    }

    /// Poll every few seconds while a run is active and the screen is
    /// visible; one fetch otherwise.
    func startLive() {
        liveTask?.cancel()
        liveTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                await self.refreshLiveMarks()
                guard self.runActive, !Task.isCancelled else { return }
                try? await Task.sleep(nanoseconds: self.livePollInterval * 1_000_000_000)
            }
        }
    }

    func stopLive() {
        liveTask?.cancel()
        liveTask = nil
    }
}

extension PlanRecord {
    /// A mutation's response, with the previous record (or the saved draft)
    /// filling in whatever the response left out — web `applyCurrentPlanRecord`.
    static func merging(_ response: PlanRecord, fallback: PlanRecord?, graph: PlanGraph?) -> PlanRecord {
        PlanRecord(
            planId: response.planId ?? fallback?.planId,
            status: response.status ?? fallback?.status,
            rawStatus: response.rawStatus ?? fallback?.rawStatus,
            graph: response.graph ?? graph ?? fallback?.graph,
            pendingQuestions: response.planId == nil ? (fallback?.pendingQuestions ?? []) : response.pendingQuestions,
            error: response.error)
    }

    init(planId: String?, status: PlanStatus?, rawStatus: String?, graph: PlanGraph?,
         pendingQuestions: [PlanQuestion], error: String?) {
        self.planId = planId
        self.status = status
        self.rawStatus = rawStatus
        self.graph = graph
        self.pendingQuestions = pendingQuestions
        self.error = error
    }
}
