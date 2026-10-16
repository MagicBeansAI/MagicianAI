import SwiftUI

/// One project: its header, its VibeDev crews, and its runs a page at a time
/// with a server-side search.
@MainActor
final class VibeDevProjectModel: ObservableObject {
    static let pageSize = 20
    /// The most runs the server returns in one page (`MAX_RUN_PAGE_SIZE`).
    static let maxPage = 50

    let projectID: String
    @Published private(set) var project: VibeDevProject?
    @Published private(set) var crews: [VibeDevCrew] = []
    @Published private(set) var rows: [VibeDevRunRow] = []
    @Published private(set) var totalRuns = 0
    @Published private(set) var hasMore = false
    @Published private(set) var loaded = false
    @Published private(set) var loadingMore = false
    @Published private(set) var lastUpdated: Date?
    @Published private(set) var refreshFailed = false
    @Published private(set) var errorMessage: String?
    @Published private(set) var hiddenItems: Set<String> = []
    /// The search the list shows; set through `search(_:)`.
    @Published private(set) var query = ""

    let context: VibeDevContext
    private(set) lazy var actions = VibeDevActions(context: context) { [weak self] in
        Task { await self?.refresh() }
    }
    private var live: VibeDevLiveUpdates?
    private var nextOffset = 0
    private var generation = 0

    init(projectID: String, context: VibeDevContext) {
        self.projectID = projectID
        self.context = context
        self.project = context.knownProjects[projectID]
        self.crews = context.knownProjects[projectID]?.crews.sorted { $0.createdAtMs > $1.createdAtMs } ?? []
    }

    var isTrusted: Bool { project?.isTrusted ?? false }

    /// Crews still going or asking something.
    var openCrews: [VibeDevCrew] { crews.filter { !$0.isFinished } }
    var finishedCrews: [VibeDevCrew] { crews.filter(\.isFinished) }

    func question(for crew: VibeDevCrew) -> VibeDevNeedsYouItem? {
        guard let item = VibeDevQuestions.question(for: crew, projectID: projectID, projectName: project?.name ?? ""),
              !hiddenItems.contains(item.id) else { return nil }
        return item
    }

    // MARK: Reads

    /// Re-read the header, crews and the runs already loaded, up to the
    /// server's page cap in one request; rows paged in past that span are
    /// kept, so paging does not reset.
    func refresh() async {
        generation += 1
        let mine = generation
        let span = min(Self.maxPage, max(Self.pageSize, rows.count))
        async let projectsRead = context.api.projects()
        async let crewsRead = context.api.crews(projectID: projectID)
        async let pageRead = context.api.runsPage(projectID: projectID, limit: span, offset: 0, query: query)
        do {
            let page = try await pageRead
            guard mine == generation else { return }
            apply(page, replacing: span)
            if let list = try? await projectsRead {
                context.remember(list.projects)
                project = list.projects.first { $0.projectId == projectID } ?? project
            }
            if let crews = try? await crewsRead {
                self.crews = crews
                let present = Set(crews.compactMap {
                    VibeDevQuestions.question(for: $0, projectID: projectID, projectName: "")?.id
                })
                hiddenItems.formIntersection(present)
            }
            lastUpdated = Date()
            refreshFailed = false
            errorMessage = nil
        } catch {
            guard mine == generation else { return }
            refreshFailed = true
            if !loaded { errorMessage = VibeDevActions.describe(error) }
        }
        loaded = true
    }

    func search(_ text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed != query else { return }
        query = trimmed
        rows = []
        nextOffset = 0
        await refresh()
    }

    func loadMore() async {
        guard hasMore, !loadingMore else { return }
        loadingMore = true
        defer { loadingMore = false }
        let mine = generation
        do {
            let page = try await context.api.runsPage(
                projectID: projectID, limit: Self.pageSize, offset: nextOffset, query: query)
            guard mine == generation else { return }
            apply(page, replacing: nil)
        } catch {
            context.undo.show(VibeDevActions.describe(error), isError: true)
        }
    }

    /// `replacing` is the span a refresh re-read from the top (nil appends a
    /// page). When more rows than that are loaded, the ones the re-read did
    /// not return stay after the fresh ones in their order (a run pushed just
    /// past the span is among them), and paging carries on from where it was.
    private func apply(_ page: VibeDevRunsPage, replacing span: Int?) {
        let incoming = page.rows
        let readTo = page.pagination.offset + page.pagination.runs
        totalRuns = page.pagination.totalRuns
        if let span {
            let fresh = Set(incoming.map(\.id))
            let tail = rows.count > span ? rows.filter { !fresh.contains($0.id) } : []
            rows = incoming + tail
            if tail.isEmpty {
                hasMore = page.pagination.hasMore
                nextOffset = readTo
            } else {
                nextOffset = max(nextOffset, readTo)
                hasMore = nextOffset < totalRuns
            }
        } else {
            let seen = Set(rows.map(\.id))
            rows += incoming.filter { !seen.contains($0.id) }
            hasMore = page.pagination.hasMore
            nextOffset = readTo
        }
    }

    func startLive() {
        if live == nil {
            live = VibeDevLiveUpdates { [weak self] in Task { await self?.refresh() } }
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
}
