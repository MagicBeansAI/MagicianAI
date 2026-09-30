import Foundation
import SwiftUI

struct PublishedTaskNoteAsset: Decodable, Equatable {
    let path: String
}

struct PublishedTaskNote: Decodable, Identifiable, Equatable {
    let projectionID: String
    let taskID: String
    let title: String
    let status: String
    let agentID: String
    let mode: String
    let taskCompletedAt: String?
    let sourceUpdatedAt: String
    let publishedAt: String
    let tags: [String]
    let notePath: String
    let openURL: String?
    let assets: [PublishedTaskNoteAsset]

    var id: String { projectionID }

    var destinationURL: URL? {
        guard let openURL,
              let url = URL(string: openURL),
              let scheme = url.scheme?.lowercased(),
              scheme == "https",
              url.host?.isEmpty == false,
              url.user == nil,
              url.password == nil else { return nil }
        return url
    }

    enum CodingKeys: String, CodingKey {
        case projectionID = "projection_id"
        case taskID = "task_id"
        case title, status
        case agentID = "agent_id"
        case mode
        case taskCompletedAt = "task_completed_at"
        case sourceUpdatedAt = "source_updated_at"
        case publishedAt = "published_at"
        case tags
        case notePath = "note_path"
        case openURL = "open_url"
        case assets
    }
}

struct PublishedTaskNotePage: Decodable, Equatable {
    let items: [PublishedTaskNote]
    let offset: Int
    let limit: Int
    let total: Int
    let hasMore: Bool

    enum CodingKeys: String, CodingKey {
        case items, offset, limit, total
        case hasMore = "has_more"
    }
}

struct PublishedTaskNotesBackfillReceipt: Decodable, Equatable {
    struct Failure: Decodable, Equatable {
        let taskID: String
        let error: String

        enum CodingKeys: String, CodingKey {
            case taskID = "task_id"
            case error
        }
    }

    struct Pagination: Decodable, Equatable {
        let hasMore: Bool

        enum CodingKeys: String, CodingKey {
            case hasMore = "has_more"
        }
    }

    let published: [PublishedTaskNote]
    let errors: [Failure]
    let pagination: Pagination
}

struct PublishedTaskNotePromotionReceipt: Decodable, Equatable {
    struct Candidate: Decodable, Equatable {
        let id: String
        let state: String
    }

    let candidate: Candidate
}

enum PublishedTaskNotesClientError: LocalizedError, Equatable {
    case invalidURL
    case server(status: Int, message: String?)

    var errorDescription: String? {
        switch self {
        case .invalidURL:
            return "The Published Notes request could not be created."
        case .server(let status, let message):
            return message ?? "Published Notes returned HTTP \(status)."
        }
    }
}

enum TaskNotePublishEligibility {
    static func allows(status: String) -> Bool {
        ["completed", "failed", "cancelled"].contains(status)
    }
}

struct PublishedTaskNotesClient {
    var session: URLSession = .shared
    var baseURL: URL = MagicianAccess.baseURL
    var timeout: TimeInterval = 15

    private var notesBaseURL: URL {
        ["api", "magician", "v2", "notes"].reduce(baseURL) {
            $0.appendingPathComponent($1)
        }
    }

    func fetch(offset: Int, limit: Int, query: String) async throws -> PublishedTaskNotePage {
        var components = URLComponents(
            url: notesBaseURL.appendingPathComponent("published-tasks"),
            resolvingAgainstBaseURL: false
        )
        var queryItems = [
            URLQueryItem(name: "offset", value: String(max(0, offset))),
            URLQueryItem(name: "limit", value: String(limit))
        ]
        if !query.isEmpty { queryItems.append(URLQueryItem(name: "q", value: query)) }
        components?.queryItems = queryItems
        guard let url = components?.url else { throw PublishedTaskNotesClientError.invalidURL }
        var request = URLRequest(url: url, timeoutInterval: timeout)
        MagicianAccess.authorize(&request)
        let data = try await perform(request)
        return try JSONDecoder().decode(PublishedTaskNotePage.self, from: data)
    }

    func promote(taskID: String) async throws -> PublishedTaskNotePromotionReceipt {
        try validateTaskID(taskID)
        let url = notesBaseURL
            .appendingPathComponent("published-tasks")
            .appendingPathComponent(taskID)
            .appendingPathComponent("promote-memory")
        var request = URLRequest(url: url, timeoutInterval: timeout)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = Data("{}".utf8)
        let data = try await perform(request)
        return try JSONDecoder().decode(PublishedTaskNotePromotionReceipt.self, from: data)
    }

    /// Manually project one terminal task through the same scoped Notes
    /// provider selection used by web. Omitting projection controls delegates
    /// mode and asset selection to the user's server-owned Notes settings.
    func publish(taskID: String) async throws -> PublishedTaskNote {
        try validateTaskID(taskID)
        let url = notesBaseURL
            .appendingPathComponent("publish")
            .appendingPathComponent("task")
            .appendingPathComponent(taskID)
        var request = URLRequest(url: url, timeoutInterval: timeout)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = Data("{}".utf8)
        let data = try await perform(request)
        return try JSONDecoder().decode(PublishedTaskNote.self, from: data)
    }

    func backfill(limit: Int = 25) async throws -> PublishedTaskNotesBackfillReceipt {
        let url = notesBaseURL
            .appendingPathComponent("publish")
            .appendingPathComponent("tasks")
            .appendingPathComponent("backfill")
        var request = URLRequest(url: url, timeoutInterval: timeout)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = try JSONEncoder().encode(BackfillRequest(limit: limit))
        let data = try await perform(request)
        return try JSONDecoder().decode(PublishedTaskNotesBackfillReceipt.self, from: data)
    }

    private func perform(_ request: URLRequest) async throws -> Data {
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode) else {
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let payload = try? JSONDecoder().decode(APIErrorPayload.self, from: data)
            throw PublishedTaskNotesClientError.server(
                status: status,
                message: payload?.message ?? payload?.error
            )
        }
        return data
    }

    private func validateTaskID(_ taskID: String) throws {
        guard !taskID.isEmpty,
              taskID.count <= 512,
              taskID.rangeOfCharacter(from: CharacterSet(charactersIn: "/\\?#")) == nil else {
            throw PublishedTaskNotesClientError.invalidURL
        }
    }

    private struct BackfillRequest: Encodable {
        let limit: Int
        let onlyUnpublished = true

        enum CodingKeys: String, CodingKey {
            case limit
            case onlyUnpublished = "only_unpublished"
        }
    }

    private struct APIErrorPayload: Decodable {
        let message: String?
        let error: String?
    }
}

/// Shared, single-flight state for manual task publication. The controller is
/// deliberately independent of task-list mutations: publishing a projection
/// must not rewrite or reload the canonical task merely to report success.
@MainActor
final class TaskNotePublishViewModel: ObservableObject {
    @Published private(set) var publishingTaskID: String?
    @Published private(set) var publishedNote: PublishedTaskNote?
    @Published private(set) var successMessage: String?
    @Published var errorMessage: String?

    private let client: PublishedTaskNotesClient

    init(client: PublishedTaskNotesClient = PublishedTaskNotesClient()) {
        self.client = client
    }

    func publish(taskID: String) async {
        guard publishingTaskID == nil else { return }
        publishingTaskID = taskID
        publishedNote = nil
        successMessage = nil
        errorMessage = nil
        defer { publishingTaskID = nil }

        do {
            publishedNote = try await client.publish(taskID: taskID)
            successMessage = "Published to Notes."
        } catch let error as URLError where error.code == .cancelled {
            return
        } catch is CancellationError {
            return
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func clearSuccess() {
        publishedNote = nil
        successMessage = nil
    }
}

@MainActor
final class PublishedTaskNotesViewModel: ObservableObject {
    static let pageSizeOptions = [5, 10, 20, 50]

    @Published private(set) var items: [PublishedTaskNote] = []
    @Published private(set) var currentPage = 1
    @Published private(set) var pageSize = 5
    @Published private(set) var offset = 0
    @Published private(set) var total = 0
    @Published private(set) var hasMore = false
    @Published private(set) var isLoading = false
    @Published private(set) var isBackfilling = false
    @Published private(set) var promotingTaskID: String?
    @Published var searchText = ""
    @Published var errorMessage: String?
    @Published var successMessage: String?

    private let client: PublishedTaskNotesClient
    private var appliedSearch = ""
    private var fetchGeneration = 0
    private var hasLoaded = false

    var pageCount: Int { max(1, Int(ceil(Double(total) / Double(pageSize)))) }
    var pageStart: Int { total == 0 ? 0 : offset + 1 }
    var pageEnd: Int { total == 0 ? 0 : min(total, offset + items.count) }
    var canLoadPrevious: Bool { currentPage > 1 && !isLoading }
    var canLoadNext: Bool { currentPage < pageCount && !isLoading && hasMore }
    var activeSearch: String { appliedSearch }

    init(client: PublishedTaskNotesClient = PublishedTaskNotesClient()) {
        self.client = client
    }

    func loadIfNeeded() async {
        guard !hasLoaded, !isLoading else { return }
        await loadCurrentPage()
    }

    func reload() async {
        successMessage = nil
        await loadCurrentPage()
    }

    func submitSearch() async {
        successMessage = nil
        searchText = String(searchText.prefix(120))
        appliedSearch = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        currentPage = 1
        await loadCurrentPage()
    }

    func clearSearch() async {
        successMessage = nil
        searchText = ""
        appliedSearch = ""
        currentPage = 1
        await loadCurrentPage()
    }

    func setPageSize(_ size: Int) async {
        guard Self.pageSizeOptions.contains(size), size != pageSize else { return }
        successMessage = nil
        pageSize = size
        currentPage = 1
        await loadCurrentPage()
    }

    func loadPreviousPage() async {
        guard canLoadPrevious else { return }
        successMessage = nil
        currentPage -= 1
        await loadCurrentPage()
    }

    func loadNextPage() async {
        guard canLoadNext else { return }
        successMessage = nil
        currentPage += 1
        await loadCurrentPage()
    }

    func promote(_ note: PublishedTaskNote) async {
        guard promotingTaskID == nil else { return }
        successMessage = nil
        promotingTaskID = note.taskID
        defer { promotingTaskID = nil }
        do {
            let receipt = try await client.promote(taskID: note.taskID)
            successMessage = "Memory candidate created for review (\(receipt.candidate.state))."
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func backfillNextBatch() async {
        guard !isBackfilling else { return }
        successMessage = nil
        errorMessage = nil
        isBackfilling = true
        defer { isBackfilling = false }
        do {
            let receipt = try await client.backfill()
            let success: String?
            let failure: String?
            if !receipt.published.isEmpty {
                success = "Published \(receipt.published.count) completed task\(receipt.published.count == 1 ? "" : "s")"
                    + (receipt.pagination.hasMore ? "; more remain." : ".")
                failure = nil
            } else if !receipt.errors.isEmpty {
                success = nil
                failure = "\(receipt.errors.count) task page\(receipt.errors.count == 1 ? "" : "s") could not be published."
            } else {
                success = "Completed tasks are already published."
                failure = nil
            }
            currentPage = 1
            await loadCurrentPage()
            if errorMessage == nil {
                successMessage = success
                errorMessage = failure
            }
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func loadCurrentPage() async {
        fetchGeneration += 1
        let generation = fetchGeneration
        let requestedPageSize = pageSize
        let query = appliedSearch
        var targetPage = currentPage
        var mayCorrectOutOfRangePage = true
        isLoading = true
        errorMessage = nil
        defer {
            if generation == fetchGeneration { isLoading = false }
        }

        do {
            while true {
                let result = try await client.fetch(
                    offset: (targetPage - 1) * requestedPageSize,
                    limit: requestedPageSize,
                    query: query
                )
                guard generation == fetchGeneration else { return }
                let resultPageCount = max(
                    1,
                    Int(ceil(Double(result.total) / Double(requestedPageSize)))
                )
                if targetPage > resultPageCount && mayCorrectOutOfRangePage {
                    targetPage = resultPageCount
                    currentPage = targetPage
                    mayCorrectOutOfRangePage = false
                    continue
                }
                items = result.items
                currentPage = targetPage
                offset = result.offset
                total = result.total
                hasMore = result.hasMore
                hasLoaded = true
                return
            }
        } catch is CancellationError {
            return
        } catch let error as URLError where error.code == .cancelled {
            return
        } catch {
            guard generation == fetchGeneration else { return }
            errorMessage = "Published Notes could not be loaded: \(error.localizedDescription)"
        }
    }
}

struct PublishedTaskNotesSection: View {
    @ObservedObject var model: PublishedTaskNotesViewModel
    @ObservedObject private var theme = ThemeManager.shared
    @State private var selectedNote: PublishedTaskNote?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            header
            Text("Completed task pages you can search, reopen, or deliberately hand to memory review.")
                .font(.subheadline)
                .foregroundColor(theme.secondaryTextColor)

            searchBar
            statusMessages
            content
            pager
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.cardColor)
        .clipShape(RoundedRectangle(cornerRadius: 16))
        .overlay {
            RoundedRectangle(cornerRadius: 16)
                .stroke(theme.cardBorderColor, lineWidth: 1)
        }
        // `.contain` keeps the section a container, so its identifier does not
        // overwrite the search field's / pager buttons' own identifiers.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("published-notes-section")
        .task { await model.loadIfNeeded() }
        .fullScreenCover(item: $selectedNote) { note in
            NotesBrowserView(initialPath: note.notePath)
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Image(systemName: "book.pages.fill")
                    .foregroundColor(theme.accentColor)
                VStack(alignment: .leading, spacing: 2) {
                    Text("OBSERVED KNOWLEDGE")
                        .font(.caption2.weight(.bold))
                        .tracking(0.7)
                        .foregroundColor(theme.accentColor)
                    Text("Published Notes")
                        .font(.headline)
                        .foregroundColor(theme.textColor)
                }
                Spacer()
                if model.isLoading { ProgressView().scaleEffect(0.8) }
                Button {
                    Task { await model.reload() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .disabled(model.isLoading)
                .accessibilityLabel("Refresh Published Notes")
            }

            HStack {
                Menu {
                    ForEach(PublishedTaskNotesViewModel.pageSizeOptions, id: \.self) { size in
                        Button {
                            Task { await model.setPageSize(size) }
                        } label: {
                            if size == model.pageSize {
                                Label("\(size) per page", systemImage: "checkmark")
                            } else {
                                Text("\(size) per page")
                            }
                        }
                    }
                } label: {
                    Label("\(model.pageSize) per page", systemImage: "list.number")
                        .font(.caption)
                }
                .disabled(model.isLoading)

                Spacer()

                Button {
                    Task { await model.backfillNextBatch() }
                } label: {
                    Label(model.isBackfilling ? "Publishing…" : "Publish next 25", systemImage: "square.and.arrow.down")
                        .font(.caption)
                }
                .disabled(model.isBackfilling || model.isLoading)
            }
        }
    }

    private var searchBar: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .foregroundColor(theme.secondaryTextColor)
            TextField("Search title, task, agent, or tag", text: $model.searchText)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(.search)
                .onSubmit { Task { await model.submitSearch() } }
                .accessibilityIdentifier("published-notes-search")
            if !model.searchText.isEmpty {
                Button {
                    Task { await model.clearSearch() }
                } label: {
                    Image(systemName: "xmark.circle.fill")
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Clear Published Notes search")
            }
            Button {
                Task { await model.submitSearch() }
            } label: {
                Image(systemName: "arrow.right.circle.fill")
            }
            .buttonStyle(.plain)
            .disabled(model.isLoading)
            .accessibilityLabel("Search Published Notes")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(theme.surfaceColor)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .overlay {
            RoundedRectangle(cornerRadius: 12)
                .stroke(theme.cardBorderColor, lineWidth: 1)
        }
    }

    @ViewBuilder
    private var statusMessages: some View {
        if let success = model.successMessage {
            Text(success)
                .font(.caption)
                .foregroundColor(theme.accentColor)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        if let error = model.errorMessage {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                Text(error).font(.caption)
                Spacer(minLength: 0)
                Button("Retry") { Task { await model.reload() } }
                    .font(.caption.weight(.semibold))
            }
            .foregroundColor(theme.dangerColor)
            .padding(10)
            .background(theme.dangerColor.opacity(0.10))
            .clipShape(RoundedRectangle(cornerRadius: 10))
        }
    }

    @ViewBuilder
    private var content: some View {
        if model.isLoading && model.items.isEmpty {
            HStack {
                Spacer()
                ProgressView("Loading Published Notes…")
                Spacer()
            }
            .padding(.vertical, 24)
        } else if model.items.isEmpty {
            ContentUnavailableView {
                Label("No Published Notes", systemImage: "book.closed")
            } description: {
                Text(model.activeSearch.isEmpty
                    ? "No completed task pages have been published yet."
                    : "No published task pages match this search.")
            }
            .frame(maxWidth: .infinity)
        } else {
            LazyVStack(spacing: 10) {
                ForEach(model.items) { noteCard($0) }
            }
            .opacity(model.isLoading ? 0.58 : 1)
        }
    }

    private func noteCard(_ note: PublishedTaskNote) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(note.title)
                    .font(.subheadline.weight(.semibold))
                    .foregroundColor(theme.textColor)
                    .lineLimit(2)
                Spacer(minLength: 4)
                Text(note.mode.uppercased())
                    .font(.system(size: 8, weight: .bold))
                    .foregroundColor(theme.accentColor)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 3)
                    .background(theme.accentColor.opacity(0.10))
                    .clipShape(Capsule())
            }

            Text("\(note.status) · \(note.agentID) · \(sourceDateLabel(note))")
                .font(.caption)
                .foregroundColor(theme.secondaryTextColor)
                .lineLimit(2)

            Text("Published \(dateLabel(note.publishedAt)) · \(note.notePath)"
                + (note.assets.isEmpty ? "" : " · \(note.assets.count) assets"))
                .font(.caption2)
                .foregroundColor(theme.secondaryTextColor)
                .lineLimit(3)

            if !note.tags.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 5) {
                        ForEach(Array(note.tags.prefix(6).enumerated()), id: \.offset) { _, tag in
                            Text(tag)
                                .font(.caption2)
                                .foregroundColor(theme.secondaryTextColor)
                                .padding(.horizontal, 7)
                                .padding(.vertical, 3)
                                .background(theme.backgroundColor.opacity(0.65))
                                .clipShape(Capsule())
                        }
                    }
                }
            }

            VStack(spacing: 8) {
                if !note.notePath.isEmpty {
                    Button {
                        selectedNote = note
                    } label: {
                        Label("Open in Notes", systemImage: "book.pages")
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .foregroundColor(theme.onAccentColor)
                    .frame(maxWidth: .infinity)
                }

                Button {
                    Task { await model.promote(note) }
                } label: {
                    Label(
                        model.promotingTaskID == note.taskID ? "Creating…" : "Promote to memory",
                        systemImage: "brain.head.profile"
                    )
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.promotingTaskID != nil)
                .frame(maxWidth: .infinity)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(theme.surfaceColor)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .overlay {
            RoundedRectangle(cornerRadius: 12)
                .stroke(theme.cardBorderColor, lineWidth: 1)
        }
        .accessibilityIdentifier("published-note-\(note.taskID)")
    }

    private var pager: some View {
        HStack(spacing: 12) {
            Button {
                Task { await model.loadPreviousPage() }
            } label: {
                Label("Previous", systemImage: "chevron.left")
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(!model.canLoadPrevious)
            .accessibilityIdentifier("published-notes-previous")

            Spacer(minLength: 0)
            VStack(spacing: 1) {
                Text("Page \(model.currentPage) of \(model.pageCount)")
                    .font(.caption.weight(.semibold))
                Text("\(model.pageStart)–\(model.pageEnd) of \(model.total)")
                    .font(.caption2)
                    .foregroundColor(theme.secondaryTextColor)
            }
            Spacer(minLength: 0)

            Button {
                Task { await model.loadNextPage() }
            } label: {
                Label("Next", systemImage: "chevron.right")
                    .labelStyle(.titleAndIcon)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(!model.canLoadNext)
            .accessibilityIdentifier("published-notes-next")
        }
    }

    private func sourceDateLabel(_ note: PublishedTaskNote) -> String {
        if let completed = note.taskCompletedAt { return "completed \(dateLabel(completed))" }
        return "updated \(dateLabel(note.sourceUpdatedAt))"
    }

    private func dateLabel(_ value: String) -> String {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let standard = ISO8601DateFormatter()
        standard.formatOptions = [.withInternetDateTime]
        guard let date = fractional.date(from: value) ?? standard.date(from: value) else { return value }
        return date.formatted(date: .abbreviated, time: .shortened)
    }
}
