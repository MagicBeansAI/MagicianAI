import SwiftUI
import UIKit

/// What every VibeDev screen shares: the client, the Attention approval path,
/// the undo toast, and the last project list (so a pushed screen can draw its
/// header before its own read returns).
final class VibeDevContext: ObservableObject {
    let api: VibeDevServing
    let approvals: VibeDevApprovals
    let undo: VibeDevUndoCenter
    /// The last data, for a cold start offline (nil when no machine is paired).
    let cache: VibeDevCache?
    var knownProjects: [String: VibeDevProject] = [:]
    /// The project list in the server's order, for pickers.
    private(set) var projectList: [VibeDevProject] = []
    var profiles: [VibeDevCodingProfile]?
    /// Health action `git_init`: run it, then re-read the projects so every
    /// banner updates. Returns the server's message on failure, nil on success.
    @MainActor
    func gitInit(projectID: String) async -> String? {
        do {
            let response = try await api.gitInit(projectID: projectID)
            if let list = try? await api.projects() {
                remember(list.projects)
            } else if var project = knownProjects[projectID] {
                project.health = response.health
                remember(project: project)
            }
            return nil
        } catch {
            return VibeDevActions.describe(error)
        }
    }

    @MainActor
    init(api: VibeDevServing = VibeDevAPI(),
         approvals: VibeDevApprovals = AttentionVibeDevApprovals(),
         undo: VibeDevUndoCenter? = nil,
         cache: VibeDevCache? = VibeDevCache.forActiveMachine()) {
        self.api = api
        self.approvals = approvals
        self.undo = undo ?? VibeDevUndoCenter()
        self.cache = cache
    }

    /// Keep the newest full project list.
    func remember(_ projects: [VibeDevProject]) {
        for project in projects { knownProjects[project.projectId] = project }
        projectList = projects
    }

    func remember(project: VibeDevProject) {
        knownProjects[project.projectId] = project
        if let index = projectList.firstIndex(where: { $0.projectId == project.projectId }) {
            projectList[index] = project
        } else {
            projectList.insert(project, at: 0)
        }
    }
}

/// Screens a VibeDev stack can push.
enum VibeDevRoute: Hashable {
    case project(String)
    /// Any task id of the run, and the tab to open on.
    case run(String, VibeDevRunTab)
    case diff(VibeDevDiffTarget)
}

enum VibeDevRunTab: String, Hashable, CaseIterable {
    case turns, changes
}

struct VibeDevDiffTarget: Hashable {
    let runID: String
    let turn: String?
    let files: [VibeDevChangedFile]
    let index: Int
}

// MARK: - Badge

/// The left-menu badge: runs waiting on the owner (kept current by the live
/// runs poll the Live Activity already makes) plus open crew questions (known
/// once VibeDev has loaded the projects).
final class VibeDevBadge: ObservableObject {
    static let shared = VibeDevBadge()

    @Published private(set) var count = 0
    private var runsNeedingYou = 0
    private var crewQuestions = 0

    func update(runsNeedingYou: Int? = nil, crewQuestions: Int? = nil) {
        let apply = {
            if let runsNeedingYou { self.runsNeedingYou = runsNeedingYou }
            if let crewQuestions { self.crewQuestions = crewQuestions }
            let total = self.runsNeedingYou + self.crewQuestions
            if total != self.count { self.count = total }
        }
        if Thread.isMainThread { apply() } else { DispatchQueue.main.async(execute: apply) }
    }
}

// MARK: - Undo toast

/// One toast at a time. A destructive action waits `delay` seconds with Undo
/// before it is sent; starting another action sends the waiting one at once.
@MainActor
final class VibeDevUndoCenter: ObservableObject {
    struct Toast: Identifiable, Equatable {
        let id = UUID()
        let message: String
        let undoable: Bool
        let isError: Bool
        let duration: TimeInterval
    }

    @Published private(set) var toast: Toast?

    let delay: TimeInterval
    private let haptics: Bool
    private var pendingCommit: (() async -> Void)?
    private var pendingUndo: (() -> Void)?
    private var timer: Task<Void, Never>?

    init(delay: TimeInterval = 2, haptics: Bool = !isRunningUnderTests) {
        self.delay = delay
        self.haptics = haptics
    }

    var hasPending: Bool { pendingCommit != nil }

    /// Show `message` with Undo; after the delay run `commit`. Undo runs `undo`
    /// instead and nothing is sent.
    func schedule(_ message: String, undo: (() -> Void)? = nil, commit: @escaping () async -> Void) {
        flush()
        pendingCommit = commit
        pendingUndo = undo
        toast = Toast(message: message, undoable: true, isError: false, duration: delay)
        feedback(.warning)
        timer = Task { [weak self, delay] in
            try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
            guard !Task.isCancelled else { return }
            await self?.fire()
        }
    }

    /// A plain notice that hides itself. While an action waits on its undo
    /// window the notice is dropped: it must neither hide Undo nor send the
    /// waiting action early.
    func show(_ message: String, isError: Bool = false) {
        guard pendingCommit == nil else { return }
        timer?.cancel()
        let toast = Toast(message: message, undoable: false, isError: isError, duration: 2.4)
        self.toast = toast
        feedback(isError ? .error : .success)
        timer = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 2_400_000_000)
            guard !Task.isCancelled else { return }
            if self?.toast?.id == toast.id { self?.toast = nil }
        }
    }

    func undo() {
        timer?.cancel()
        timer = nil
        let undo = pendingUndo
        pendingCommit = nil
        pendingUndo = nil
        toast = nil
        undo?()
    }

    /// Send a waiting action now (another action started, or the screen closed).
    func flush() {
        timer?.cancel()
        timer = nil
        guard let commit = pendingCommit else { return }
        pendingCommit = nil
        pendingUndo = nil
        toast = nil
        Task { await commit() }
    }

    func dismiss() {
        if pendingCommit != nil { flush() } else { timer?.cancel(); toast = nil }
    }

    private func fire() async {
        let commit = pendingCommit
        pendingCommit = nil
        pendingUndo = nil
        timer = nil
        toast = nil
        await commit?()
    }

    private func feedback(_ type: UINotificationFeedbackGenerator.FeedbackType) {
        guard haptics else { return }
        UINotificationFeedbackGenerator().notificationOccurred(type)
    }
}

// MARK: - Live updates

/// Re-reads a screen when the realtime hub says something it shows changed:
/// task, HITL and execution events and the `coding.*` lifecycle (not the
/// streaming deltas), debounced; and on resync and returning to the
/// foreground. Mirrors the web live-runs store's filter.
final class VibeDevLiveUpdates {
    static let eventPrefixes = ["Task", "task.", "hitl.", "execution.", "delegation.results_ready"]
    static let codingLifecycle: Set<String> = [
        "coding.queued", "coding.started", "coding.agent_started", "coding.agent_ended",
        "coding.agent_settled", "coding.turn.started", "coding.turn.finished", "coding.completed",
        "coding.failed", "coding.budget_exhausted", "coding.approval_requested",
        "coding.sandbox_write_blocked",
    ]

    static func isRelevant(_ eventType: String) -> Bool {
        if codingLifecycle.contains(eventType) { return true }
        return eventPrefixes.contains { eventType.hasPrefix($0) }
    }

    /// Task ids whose streaming `coding.*` steps also count (an open timeline).
    var followedTaskIDs: Set<String> = []

    private let debounce: TimeInterval
    private let onRefresh: () -> Void
    private var subscription: RealtimeHubSubscription?
    private var observers: [NSObjectProtocol] = []
    private var pending: DispatchWorkItem?

    init(debounce: TimeInterval = 1, onRefresh: @escaping () -> Void) {
        self.debounce = debounce
        self.onRefresh = onRefresh
    }

    deinit {
        subscription?.cancel()
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        pending?.cancel()
    }

    func matches(_ event: RealtimeHubEvent) -> Bool {
        let type = event.effectiveEventType
        if Self.isRelevant(type) { return true }
        if type.hasPrefix("coding."), let task = event.refs.taskId, followedTaskIDs.contains(task) { return true }
        return false
    }

    func start() {
        guard subscription == nil else { return }
        subscription = RealtimeHub.shared.subscribe(
            where: { [weak self] event in self?.matches(event) ?? false },
            onEvent: { [weak self] _ in self?.poke() },
            onResync: { [weak self] in self?.poke(immediate: true) }
        )
        observers.append(NotificationCenter.default.addObserver(
            forName: UIApplication.willEnterForegroundNotification, object: nil, queue: .main
        ) { [weak self] _ in self?.poke(immediate: true) })
    }

    func stop() {
        subscription?.cancel()
        subscription = nil
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
        observers.removeAll()
        pending?.cancel()
        pending = nil
    }

    func poke(immediate: Bool = false) {
        pending?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.onRefresh() }
        pending = work
        DispatchQueue.main.asyncAfter(deadline: .now() + (immediate ? 0.05 : debounce), execute: work)
    }
}

// MARK: - Sandboxed diff approval

enum VibeDevApprovalOutcome: Equatable {
    case sent
    /// No diff approval for this run in Attention (it may be another kind of ask).
    case notFound
    case failed(String)
}

protocol VibeDevApprovals {
    @MainActor func decide(taskIDs: [String], apply: Bool) async -> VibeDevApprovalOutcome
}

/// Answers a run's pending diff approval through the same call the Attention
/// screen makes (`AttentionViewModel.submitDiffApproval`), and waits for the
/// server so a refused or failed answer is reported rather than "approved".
final class AttentionVibeDevApprovals: VibeDevApprovals {
    /// Kept so an in-flight answer finishes after the screen moves on.
    private var models: [AttentionViewModel] = []

    @MainActor
    func decide(taskIDs: [String], apply: Bool) async -> VibeDevApprovalOutcome {
        let model = AttentionViewModel()
        models = Array((models + [model]).suffix(3))
        await model.refresh()
        let wanted = Set(taskIDs.filter { !$0.isEmpty })
        let item = model.items("all").first { item in
            item.inputType == "diff_approval" && item.owningTaskId.map(wanted.contains) == true
        }
        guard let item else { return .notFound }
        if let failure = await model.answerDiffApproval(item, apply: apply) { return .failed(failure) }
        return .sent
    }
}
