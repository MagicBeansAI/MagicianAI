import SwiftUI

/// A crew with the project it belongs to.
struct VibeDevProjectCrew: Identifiable {
    let project: VibeDevProject
    let crew: VibeDevCrew
    var id: String { crew.crewId }
}

/// What a swipe on a live run row offers.
enum VibeDevRowAction: String, Equatable, CaseIterable {
    case steer, stop
}

/// The home screen: Now (what needs the owner, what is going, what just
/// finished) and Projects. Reads `/vibedev/runs/live` and `/vibedev/projects`
/// (which carries each project's crews); keeps the last good data when a
/// read fails.
@MainActor
final class VibeDevNowModel: ObservableObject {
    @Published private(set) var liveRuns: [VibeDevLiveRun] = []
    @Published private(set) var history: [VibeDevLiveRun] = []
    @Published private(set) var nextCursor: String?
    @Published private(set) var loadingMore = false
    @Published private(set) var projects: [VibeDevProject] = []
    @Published private(set) var loaded = false
    @Published private(set) var lastUpdated: Date?
    @Published private(set) var refreshFailed = false
    @Published private(set) var errorMessage: String?
    /// Live task ids whose stop is waiting out its undo window.
    @Published private(set) var stopping: Set<String> = []
    /// Needs-you items answered here and waiting (undo) or in flight.
    @Published private(set) var hiddenItems: Set<String> = []

    let context: VibeDevContext
    private(set) lazy var actions = VibeDevActions(context: context) { [weak self] in
        Task { await self?.refresh() }
    }
    private var live: VibeDevLiveUpdates?
    private var refreshing = false
    /// A refresh asked for while one runs: it runs once more after, so a
    /// change that landed mid-read is not lost.
    private var refreshQueued = false

    init(context: VibeDevContext) {
        self.context = context
    }

    // MARK: Derived lists

    var needsYou: [VibeDevNeedsYouItem] {
        VibeDevQuestions.needsYou(projects: projects, liveRuns: liveRuns)
            .filter { !hiddenItems.contains($0.id) }
    }

    var working: [VibeDevLiveRun] { liveRuns.filter { $0.state.isLive } }
    var justFinished: [VibeDevLiveRun] { liveRuns.filter { $0.state.isEnded } }

    /// Crews whose lanes are going (their questions show under Needs you).
    var runningCrews: [VibeDevProjectCrew] {
        visibleProjects.flatMap { project in
            project.crews.filter { $0.status == .running || $0.status == .ready }
                .map { VibeDevProjectCrew(project: project, crew: $0) }
        }
    }

    var visibleProjects: [VibeDevProject] { projects.filter { !$0.archived } }

    func liveCount(for projectID: String) -> Int {
        liveRuns.filter { $0.projectId == projectID && ($0.state.isLive || $0.state == .needsYou) }.count
    }

    func project(_ id: String) -> VibeDevProject? { projects.first { $0.projectId == id } }

    func rowActions(for run: VibeDevLiveRun) -> [VibeDevRowAction] {
        guard run.state.isLive, !stopping.contains(run.liveTaskId) else { return [] }
        return [.steer, .stop]
    }

    func isStopping(_ run: VibeDevLiveRun) -> Bool { stopping.contains(run.liveTaskId) }

    // MARK: Reads

    func refresh() async {
        guard !refreshing else {
            refreshQueued = true
            return
        }
        refreshing = true
        defer { refreshing = false }
        repeat {
            refreshQueued = false
            await read()
        } while refreshQueued
    }

    private func read() async {
        // A cold start shows the last data at once; the read below replaces it.
        if !loaded, liveRuns.isEmpty, projects.isEmpty, let cached = context.cache?.loadNow() {
            liveRuns = cached.0.live.runs
            history = cached.0.history?.runs ?? cached.0.live.runs
            nextCursor = cached.0.history?.nextCursor
            projects = cached.0.projects
            context.remember(projects)
            lastUpdated = cached.1
        }
        async let runs = context.api.liveRuns()
        async let list = context.api.projects()
        async let page = context.api.runHistory(limit: 20, cursor: nil)
        do {
            let (runsResponse, projectsResponse, firstPage) = try await (runs, list, page)
            let firstRead = history.isEmpty
            let freshIDs = Set(firstPage.runs.map(\.runId))
            let outranLoadedRows = !history.contains { freshIDs.contains($0.runId) }
            history = (firstPage.runs + history.filter { !freshIDs.contains($0.runId) })
                .sorted { ($0.updatedAt, $0.runId) > ($1.updatedAt, $1.runId) }
            if firstRead || !firstPage.hasMore || outranLoadedRows { nextCursor = firstPage.nextCursor }
            liveRuns = runsResponse.runs
            projects = projectsResponse.projects
            context.remember(projects)
            // Forget answers the server has acted on; keep the ones still pending.
            let present = Set(VibeDevQuestions.needsYou(projects: projects, liveRuns: liveRuns).map(\.id))
            hiddenItems.formIntersection(present)
            stopping = stopping.filter { id in liveRuns.contains { $0.liveTaskId == id && $0.state.isLive } }
            lastUpdated = Date()
            refreshFailed = false
            errorMessage = nil
            context.cache?.saveNow(VibeDevCache.NowSnapshot(live: runsResponse, projects: projectsResponse.projects, history: firstPage))
            VibeDevBadge.shared.update(
                runsNeedingYou: liveRuns.filter { $0.state == .needsYou }.count,
                crewQuestions: needsYou.filter { $0.kind != .run }.count)
        } catch {
            refreshFailed = true
            if liveRuns.isEmpty && projects.isEmpty { errorMessage = VibeDevActions.describe(error) }
        }
        loaded = true
    }

    func loadMore() async {
        guard !loadingMore, !refreshing, let cursor = nextCursor else { return }
        loadingMore = true
        defer { loadingMore = false }
        do {
            let page = try await context.api.runHistory(limit: 20, cursor: cursor)
            let held = Set(history.map(\.runId))
            history.append(contentsOf: page.runs.filter { !held.contains($0.runId) })
            history.sort { ($0.updatedAt, $0.runId) > ($1.updatedAt, $1.runId) }
            nextCursor = page.nextCursor
            errorMessage = nil
        } catch { errorMessage = VibeDevActions.describe(error) }
    }

    func startLive() {
        if live == nil {
            live = VibeDevLiveUpdates { [weak self] in
                Task { await self?.refresh() }
            }
        }
        live?.start()
    }

    func stopLive() { live?.stop() }

    // MARK: Acts

    func perform(_ option: VibeDevQuestionOption, of item: VibeDevNeedsYouItem,
                 open: @escaping (VibeDevRoute) -> Void) {
        actions.perform(option, of: item, hide: { [weak self] hidden in
            guard let self else { return }
            if hidden { self.hiddenItems.insert(item.id) } else { self.hiddenItems.remove(item.id) }
        }, open: open)
    }

    func stop(_ run: VibeDevLiveRun) {
        actions.stop(liveTaskID: run.liveTaskId, name: run.shortName) { [weak self] pending in
            guard let self else { return }
            if pending { self.stopping.insert(run.liveTaskId) } else { self.stopping.remove(run.liveTaskId) }
        }
    }
}
