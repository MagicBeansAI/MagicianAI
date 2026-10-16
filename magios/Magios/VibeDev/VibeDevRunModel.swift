import SwiftUI

/// One run: its turns (each expanding into a timeline), its changes, and the
/// actions its state allows.
@MainActor
final class VibeDevRunModel: ObservableObject {
    /// What the bottom bar offers.
    enum Bar: Equatable {
        case live
        case needsYou
        case ended(trusted: Bool)
        case none
    }

    let requestedID: String
    @Published var tab: VibeDevRunTab
    @Published private(set) var turns: VibeDevRunTurns?
    @Published private(set) var live: VibeDevLiveRun?
    @Published private(set) var expanded: Set<String> = []
    @Published private(set) var timelines: [String: VibeDevTimeline] = [:]
    @Published private(set) var timelineErrors: [String: String] = [:]
    @Published private(set) var loadingTimelines: Set<String> = []
    @Published private(set) var changes: VibeDevRunChanges?
    @Published private(set) var changesTurn: String?
    @Published private(set) var changesError: String?
    @Published private(set) var loadingChanges = false
    @Published private(set) var loaded = false
    @Published private(set) var lastUpdated: Date?
    @Published private(set) var refreshFailed = false
    @Published private(set) var errorMessage: String?
    @Published private(set) var stopping = false
    @Published private(set) var busy = false

    let context: VibeDevContext
    private(set) lazy var actions = VibeDevActions(context: context) { [weak self] in
        Task { await self?.refresh() }
    }
    private var live_: VibeDevLiveUpdates?
    private var seededExpansion = false
    /// The model's own read loop: callers ask for a read and wait for it, but
    /// neither run the loop nor cancel it.
    private var drain: Task<Void, Never>?
    /// A refresh asked for while a read runs: one more read follows, so a
    /// change that landed mid-read is not lost. Reads never overlap, so an
    /// older one cannot overwrite a newer one, and events that keep arriving
    /// cannot keep superseding every read before it lands.
    private var refreshQueued = false
    /// Reads begun so far, numbered from 1.
    private var readsStarted = 0
    /// Callers waiting for read number `target` to land.
    private var readWaiters: [(target: Int, done: CheckedContinuation<Void, Never>)] = []
    /// Bumped by each change load, so an older load that returns late cannot
    /// overwrite a newer one.
    private var changesGeneration = 0

    init(runID: String, tab: VibeDevRunTab = .turns, context: VibeDevContext) {
        self.requestedID = runID
        self.tab = tab
        self.context = context
    }

    // MARK: Derived

    var rootID: String { turns?.runId ?? requestedID }
    var liveTaskID: String { live?.liveTaskId ?? turns?.latestTurnId ?? requestedID }
    var isTrusted: Bool { turns?.trusted ?? false }
    var project: VibeDevProject? { turns.flatMap { context.knownProjects[$0.projectId] } }
    var title: String { turns?.displayTitle ?? live?.displayTitle ?? "Run" }
    var name: String { turns?.lane.map { "\($0) lane" } ?? VibeDevFormat.short(title) }

    var state: VibeDevRunState {
        if let live { return live.state }
        guard let latest = turns?.turns.last else { return .working }
        return latest.state
    }

    var bar: Bar {
        guard turns != nil else { return .none }
        switch state {
        case .working, .paused: return stopping ? .none : .live
        case .needsYou: return .needsYou
        case .done, .failed, .stopped: return .ended(trusted: isTrusted)
        }
    }

    // MARK: Reads

    /// Read the run again, and return once the first read that begins after
    /// this call has landed: a read already in flight may predate the change
    /// asked about, and reads queued after it are other callers' to wait for.
    /// Cancelling the caller does not cancel the read.
    func refresh() async {
        let target = readsStarted + 1
        if drain == nil {
            drain = Task { await self.drainReads() }
        } else {
            refreshQueued = true
        }
        await withCheckedContinuation { readWaiters.append((target, $0)) }
    }

    private func drainReads() async {
        repeat {
            refreshQueued = false
            readsStarted += 1
            let number = readsStarted
            await read()
            let landed = readWaiters.filter { $0.target <= number }
            readWaiters.removeAll { $0.target <= number }
            for waiter in landed { waiter.done.resume() }
        } while refreshQueued
        drain = nil
    }

    private func read() async {
        if turns == nil, let cached = context.cache?.loadTurns(for: requestedID) {
            turns = cached.0
            lastUpdated = cached.1
        }
        do {
            let detail = try await context.api.runTurns(runID: requestedID)
            turns = detail
            context.cache?.saveTurns(detail, for: requestedID)
            if let liveList = try? await context.api.liveRuns() {
                live = liveList.runs.first { $0.runId == detail.runId }
            }
            if !seededExpansion {
                seededExpansion = true
                if let last = detail.turns.last, last.state.isLive || last.state == .needsYou {
                    expanded.insert(last.taskId)
                }
            }
            if !stopping || !state.isLive { stopping = false }
            lastUpdated = Date()
            refreshFailed = false
            errorMessage = nil
            live_?.followedTaskIDs = expanded
            // Re-read open timelines whose turn is still moving.
            for turn in detail.turns where expanded.contains(turn.taskId) &&
                (timelines[turn.taskId] == nil || !turn.state.isEnded) {
                await loadTimeline(turn.taskId)
            }
            if tab == .changes || changes != nil { await loadChanges() }
        } catch {
            refreshFailed = true
            if turns == nil { errorMessage = VibeDevActions.describe(error) }
        }
        loaded = true
    }

    func toggle(_ turn: VibeDevRunTurn) {
        if expanded.contains(turn.taskId) {
            expanded.remove(turn.taskId)
        } else {
            expanded.insert(turn.taskId)
            if timelines[turn.taskId] == nil { Task { await loadTimeline(turn.taskId) } }
        }
        live_?.followedTaskIDs = expanded
    }

    func loadTimeline(_ turnID: String) async {
        loadingTimelines.insert(turnID)
        defer { loadingTimelines.remove(turnID) }
        do {
            timelines[turnID] = try await context.api.timeline(turnID: turnID, limit: 200)
            timelineErrors[turnID] = nil
        } catch {
            if timelines[turnID] == nil { timelineErrors[turnID] = VibeDevActions.describe(error) }
        }
    }

    func selectTab(_ tab: VibeDevRunTab) {
        self.tab = tab
        if tab == .changes && changes == nil { Task { await loadChanges() } }
    }

    func filterChanges(turn: String?) {
        guard turn != changesTurn else { return }
        changesTurn = turn
        changes = nil
        Task { await loadChanges() }
    }

    /// Read the changes for the turn filter as it is now. A load that returns
    /// after the filter moved on, or after a newer load began, is dropped.
    func loadChanges() async {
        changesGeneration += 1
        let mine = changesGeneration
        let turn = changesTurn
        loadingChanges = true
        defer { if mine == changesGeneration { loadingChanges = false } }
        do {
            let result = try await context.api.changes(runID: rootID, turn: turn, path: nil)
            guard mine == changesGeneration, turn == changesTurn else { return }
            changes = result
            changesError = nil
        } catch {
            guard mine == changesGeneration, turn == changesTurn else { return }
            if changes == nil { changesError = VibeDevActions.describe(error) }
        }
    }

    func startLive() {
        if live_ == nil {
            live_ = VibeDevLiveUpdates { [weak self] in Task { await self?.refresh() } }
        }
        live_?.followedTaskIDs = expanded
        live_?.start()
    }

    func stopLive() { live_?.stop() }

    // MARK: Acts

    func stop() {
        actions.stop(liveTaskID: liveTaskID, name: name) { [weak self] pending in
            self?.stopping = pending
        }
    }

    func decide(apply: Bool) {
        let ids = [liveTaskID, rootID] + (turns?.turns.map(\.taskId) ?? [])
        busy = true
        Task {
            await actions.decide(approvalID: live?.approvalId, taskIDs: ids, apply: apply)
            busy = false
        }
    }

    func merge() {
        landing("Merged into \(project?.baseBranch ?? "the base branch")") {
            try await self.context.api.merge(rootID: self.rootID)
        }
    }

    func openPullRequest() {
        let title = self.title
        let body = turns?.turns.last(where: { $0.summary != nil })?.summary ?? ""
        landing("Pull request opened") {
            _ = try await self.context.api.openPullRequest(rootID: self.rootID, title: title, body: body)
        }
    }

    /// Delete the run's branch and clone, after the undo window. The commit
    /// holds the client and the undo center, not this screen: leaving inside
    /// the window still discards; only the re-read needs the screen.
    func discard() {
        let root = rootID
        let api = context.api, undo = context.undo
        undo.schedule("Discarding \(name)…") { [weak self] in
            do {
                try await api.discard(rootID: root)
                undo.show("Discarded")
            } catch {
                undo.show(VibeDevActions.describe(error), isError: true)
            }
            await self?.refresh()
        }
    }

    private func landing(_ done: String, _ call: @escaping () async throws -> Void) {
        busy = true
        Task {
            do {
                try await call()
                context.undo.show(done)
            } catch {
                context.undo.show(VibeDevActions.describe(error), isError: true)
            }
            busy = false
            await refresh()
        }
    }
}

/// One changed file's diff, with previous/next across the run's files.
@MainActor
final class VibeDevDiffModel: ObservableObject {
    let runID: String
    let turn: String?
    let files: [VibeDevChangedFile]
    @Published private(set) var index: Int
    @Published private(set) var patch: VibeDevFilePatch?
    @Published private(set) var diff: UnifiedDiff?
    @Published private(set) var loading = false
    @Published private(set) var errorMessage: String?

    private let context: VibeDevContext
    private var cache: [String: (VibeDevFilePatch, UnifiedDiff)] = [:]

    init(target: VibeDevDiffTarget, context: VibeDevContext) {
        runID = target.runID
        turn = target.turn
        files = target.files
        index = min(max(0, target.index), max(0, target.files.count - 1))
        self.context = context
    }

    var file: VibeDevChangedFile? { files.indices.contains(index) ? files[index] : nil }
    var canGoBack: Bool { index > 0 }
    var canGoForward: Bool { index < files.count - 1 }
    /// A big diff opens on its hunk list.
    var startCollapsed: Bool { (diff?.lineCount ?? 0) > 400 }

    func move(by step: Int) {
        let next = index + step
        guard files.indices.contains(next) else { return }
        select(next)
    }

    func select(_ next: Int) {
        guard files.indices.contains(next) else { return }
        index = next
        Task { await load() }
    }

    func load() async {
        guard let file else { return }
        if let cached = cache[file.path] {
            patch = cached.0
            diff = cached.1
            errorMessage = nil
            return
        }
        patch = nil
        diff = nil
        loading = true
        defer { loading = false }
        do {
            let changes = try await context.api.changes(runID: runID, turn: turn, path: file.path)
            guard self.file?.path == file.path else { return }
            if let patch = changes.patch {
                let parsed = UnifiedDiffParser.parse(patch.unifiedDiff)
                cache[file.path] = (patch, parsed)
                self.patch = patch
                self.diff = parsed
                errorMessage = nil
            } else {
                errorMessage = "No diff for this file."
            }
        } catch {
            errorMessage = VibeDevActions.describe(error)
        }
    }
}
