import Foundation

enum ChatSessionResumeResult: Equatable {
    case restored(String)
    case missing
    case unavailable
}

class ThreadViewModel: ObservableObject {
    static let pageSize = 15

    @Published var threads: [UiThreadRecord] = []
    @Published var sessions: [ChatSession] = []
    @Published var searchResults: [HistorySearchItem] = []
    @Published private(set) var selectedSessionSnapshot: ChatSession? = nil
    @Published private(set) var selectedThreadSnapshot: UiThreadRecord? = nil
    @Published var activeThreadId: String = "general"
    @Published var activeSessionId: String? = nil
    @Published var isLoading: Bool = false
    @Published var activeTab: String = "sessions"
    @Published var historyLane: ChatHistoryLane = .personal
    @Published var searchText: String = ""
    @Published private(set) var total: Int = 0
    @Published private(set) var offset: Int = 0
    @Published private(set) var isSearchActive: Bool = false
    @Published private(set) var errorMessage: String? = nil

    private let principal = MagicianAccess.principal
    private let workspace = MagicianAccess.workspace
    private let networkSession: URLSession
    private var appliedSearch = ""
    private var searchWorkItem: DispatchWorkItem?
    private var fetchGeneration = 0
    private var threadOpenGeneration = 0

    private var base: String { "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2" }

    var pageStart: Int { total == 0 ? 0 : offset + 1 }
    var pageEnd: Int { min(offset + Self.pageSize, total) }
    var canLoadPrevious: Bool { offset > 0 && !isLoading }
    var canLoadNext: Bool { offset + Self.pageSize < total && !isLoading }

    init(networkSession: URLSession = .shared) {
        self.networkSession = networkSession
    }

    deinit {
        searchWorkItem?.cancel()
    }

    func fetchData() {
        fetchCurrentPage()
    }

    func selectTab(_ tab: String) {
        guard tab == "sessions" || tab == "threads", activeTab != tab else { return }
        activeTab = tab
        resetPageAndFetch()
    }

    func selectHistoryLane(_ lane: ChatHistoryLane) {
        guard historyLane != lane else { return }
        historyLane = lane
        resetPageAndFetch()
    }

    func updateSearchText(_ value: String, debounce: Bool = true) {
        var bounded = ""
        for scalar in value.unicodeScalars.prefix(120) {
            bounded.unicodeScalars.append(scalar)
        }
        searchText = bounded
        searchWorkItem?.cancel()
        let workItem = DispatchWorkItem { [weak self] in
            self?.applySearch()
        }
        searchWorkItem = workItem
        if debounce {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: workItem)
        } else {
            workItem.perform()
        }
    }

    func submitSearch() {
        searchWorkItem?.cancel()
        updateSearchText(searchText, debounce: false)
    }

    func clearSearch() {
        searchWorkItem?.cancel()
        searchText = ""
        applySearch()
    }

    func dismissError() {
        errorMessage = nil
    }

    func loadPreviousPage() {
        guard canLoadPrevious else { return }
        offset = max(0, offset - Self.pageSize)
        fetchCurrentPage()
    }

    func loadNextPage() {
        guard canLoadNext else { return }
        offset += Self.pageSize
        fetchCurrentPage()
    }

    private func applySearch() {
        let normalized = searchText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard normalized != appliedSearch else { return }
        appliedSearch = normalized
        isSearchActive = !normalized.isEmpty
        resetPageAndFetch()
    }

    private func resetPageAndFetch() {
        offset = 0
        total = 0
        sessions = []
        threads = []
        searchResults = []
        fetchCurrentPage()
    }

    private func fetchCurrentPage() {
        fetchGeneration += 1
        let generation = fetchGeneration
        isLoading = true
        errorMessage = nil
        if isSearchActive {
            fetchHistorySearch(generation: generation)
        } else if activeTab == "threads" {
            fetchThreads(generation: generation)
        } else {
            fetchSessions(generation: generation)
        }
    }

    private func historyURL(path: String) -> URL? {
        guard var components = URLComponents(string: "\(base)/\(path)") else { return nil }
        let query = [
            URLQueryItem(name: "history_lane", value: historyLane.rawValue),
            URLQueryItem(name: "limit", value: String(Self.pageSize)),
            URLQueryItem(name: "offset", value: String(offset)),
        ]
        components.queryItems = query
        return components.url
    }

    private func globalSearchURL() -> URL? {
        guard var components = URLComponents(string: "\(base)/history/search") else { return nil }
        components.queryItems = [
            URLQueryItem(name: "q", value: appliedSearch),
            URLQueryItem(name: "limit", value: String(Self.pageSize)),
            URLQueryItem(name: "offset", value: String(offset)),
        ]
        return components.url
    }

    private func fetchHistorySearch(generation: Int) {
        guard let url = globalSearchURL() else {
            finishFailure("Could not build the history-search request.", generation: generation)
            return
        }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            guard let self else { return }
            guard error == nil, let data, Self.isSuccessful(response) else {
                self.finishFailure(error?.localizedDescription ?? "History search could not be loaded.", generation: generation)
                return
            }
            do {
                let decoded = try JSONDecoder().decode(HistorySearchResponse.self, from: data)
                DispatchQueue.main.async {
                    guard generation == self.fetchGeneration else { return }
                    self.searchResults = decoded.items
                    self.sessions = []
                    self.threads = []
                    self.total = decoded.total
                    self.refreshSelectedSnapshots()
                    self.finishLoad(generation: generation)
                }
            } catch {
                self.finishFailure("History search response was invalid.", generation: generation)
            }
        }.resume()
    }

    private func fetchThreads(generation: Int) {
        guard let url = historyURL(path: "ui-threads") else {
            finishFailure("Could not build the thread-history request.", generation: generation)
            return
        }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            guard let self else { return }
            guard error == nil, let data, Self.isSuccessful(response) else {
                self.finishFailure(error?.localizedDescription ?? "Thread history could not be loaded.", generation: generation)
                return
            }
            do {
                let decoded = try JSONDecoder().decode(UiThreadListResponse.self, from: data)
                DispatchQueue.main.async {
                    guard generation == self.fetchGeneration else { return }
                    self.threads = decoded.threads
                    self.total = decoded.total ?? decoded.threads.count
                    self.refreshSelectedSnapshots()
                    self.finishLoad(generation: generation)
                }
            } catch {
                self.finishFailure("Thread history response was invalid.", generation: generation)
            }
        }.resume()
    }

    private func fetchSessions(generation: Int) {
        guard let url = historyURL(path: "chat/sessions") else {
            finishFailure("Could not build the session-history request.", generation: generation)
            return
        }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            guard let self else { return }
            guard error == nil, let data, Self.isSuccessful(response) else {
                self.finishFailure(error?.localizedDescription ?? "Session history could not be loaded.", generation: generation)
                return
            }

            let page = try? JSONDecoder().decode(ChatSessionListResponse.self, from: data)
            let bareSessions = page == nil ? try? JSONDecoder().decode([ChatSession].self, from: data) : nil
            guard let decodedSessions = page?.sessions ?? bareSessions else {
                self.finishFailure("Session history response was invalid.", generation: generation)
                return
            }
            DispatchQueue.main.async {
                guard generation == self.fetchGeneration else { return }
                self.sessions = decodedSessions
                self.total = page?.total ?? decodedSessions.count
                self.refreshSelectedSnapshots()
                self.finishLoad(generation: generation)
            }
        }.resume()
    }

    private func finishLoad(generation: Int) {
        guard generation == fetchGeneration else { return }
        if offset > 0 && offset >= total {
            offset = total == 0 ? 0 : ((total - 1) / Self.pageSize) * Self.pageSize
            fetchCurrentPage()
            return
        }
        isLoading = false
    }

    private func finishFailure(_ message: String, generation: Int) {
        DispatchQueue.main.async {
            guard generation == self.fetchGeneration else { return }
            self.errorMessage = message
            self.isLoading = false
        }
    }

    private static func isSuccessful(_ response: URLResponse?) -> Bool {
        guard let response = response as? HTTPURLResponse else { return false }
        return (200..<300).contains(response.statusCode)
    }

    func selectThread(_ id: String) {
        threadOpenGeneration += 1
        activeThreadId = id
        if let thread = threadMetadata(for: id) {
            selectedThreadSnapshot = thread
        }
    }

    func selectSession(_ id: String) {
        threadOpenGeneration += 1
        activeSessionId = id
        if let session = sessionMetadata(for: id) {
            applySelectedSession(session)
        }
    }

    func sessionMetadata(for id: String) -> ChatSession? {
        if selectedSessionSnapshot?.id == id { return selectedSessionSnapshot }
        if let session = sessions.first(where: { $0.id == id }) { return session }
        return searchResults.lazy.compactMap(\.session).first(where: { $0.id == id })
    }

    func threadMetadata(for id: String) -> UiThreadRecord? {
        if selectedThreadSnapshot?.id == id { return selectedThreadSnapshot }
        if let thread = threads.first(where: { $0.id == id }) { return thread }
        return searchResults.lazy.compactMap(\.thread).first(where: { $0.id == id })
    }

    private func applySelectedSession(_ session: ChatSession) {
        activeSessionId = session.id
        activeThreadId = session.uiThreadId
        selectedSessionSnapshot = session
        if let thread = threadMetadata(for: session.uiThreadId) {
            selectedThreadSnapshot = thread
        }
    }

    private func refreshSelectedSnapshots() {
        if let activeSessionId,
           let session = sessions.first(where: { $0.id == activeSessionId })
                ?? searchResults.lazy.compactMap(\.session).first(where: { $0.id == activeSessionId }) {
            applySelectedSession(session)
        }
        if let thread = threads.first(where: { $0.id == activeThreadId })
            ?? searchResults.lazy.compactMap(\.thread).first(where: { $0.id == activeThreadId }) {
            selectedThreadSnapshot = thread
        }
    }

    func openThread(_ thread: UiThreadRecord, completion: @escaping (String?) -> Void) {
        threadOpenGeneration += 1
        openThreadPage(
            thread,
            offset: 0,
            archivedFallback: nil,
            generation: threadOpenGeneration,
            completion: completion
        )
    }

    private func openThreadPage(
        _ thread: UiThreadRecord,
        offset: Int,
        archivedFallback: ChatSession?,
        generation: Int,
        completion: @escaping (String?) -> Void
    ) {
        guard var components = URLComponents(string: "\(base)/chat/sessions") else {
            completion(nil)
            return
        }
        components.queryItems = [
            URLQueryItem(name: "ui_thread_id", value: thread.id),
            URLQueryItem(name: "limit", value: String(Self.pageSize)),
            URLQueryItem(name: "offset", value: String(offset)),
        ]
        guard let url = components.url else {
            completion(nil)
            return
        }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            guard let self else { return }
            guard generation == self.threadOpenGeneration else {
                DispatchQueue.main.async { completion(nil) }
                return
            }
            guard error == nil, Self.isSuccessful(response), let data else {
                self.finishThreadOpenFailure(
                    error?.localizedDescription ?? "Thread history could not be loaded.",
                    completion: completion
                )
                return
            }
            let page = try? JSONDecoder().decode(ChatSessionListResponse.self, from: data)
            let bareSessions = page == nil ? try? JSONDecoder().decode([ChatSession].self, from: data) : nil
            guard let candidates = page?.sessions ?? bareSessions else {
                self.finishThreadOpenFailure(
                    "Thread history response was invalid.",
                    completion: completion
                )
                return
            }
            if let session = candidates.first(where: { $0.status == "active" }) {
                DispatchQueue.main.async {
                    self.applySelectedSession(session)
                    self.selectedThreadSnapshot = thread
                    completion(session.id)
                }
                return
            }
            let fallback = archivedFallback ?? candidates.first
            let nextOffset = offset + candidates.count
            if let total = page?.total, !candidates.isEmpty, nextOffset < total {
                self.openThreadPage(
                    thread,
                    offset: nextOffset,
                    archivedFallback: fallback,
                    generation: generation,
                    completion: completion
                )
                return
            }
            if let fallback {
                DispatchQueue.main.async {
                    self.applySelectedSession(fallback)
                    self.selectedThreadSnapshot = thread
                    completion(fallback.id)
                }
                return
            }
            self.createSession(
                uiThreadId: thread.id,
                historyLane: ChatHistoryLane(rawValue: thread.historyLane ?? "") ?? .personal,
                selectionGeneration: generation
            ) { [weak self] sessionId in
                if sessionId != nil {
                    self?.selectedThreadSnapshot = thread
                }
                completion(sessionId)
            }
        }.resume()
    }

    private func finishThreadOpenFailure(
        _ message: String,
        completion: @escaping (String?) -> Void
    ) {
        DispatchQueue.main.async {
            self.errorMessage = message
            completion(nil)
        }
    }

    static func pickResumeSessionId(from sessions: [ChatSession]) -> String? {
        let active = sessions.filter { $0.status == "active" }.sorted { $0.updatedAt > $1.updatedAt }
        return active.first?.id
    }

    /// Reopen the exact session this device last displayed. Only a confirmed
    /// 404 is allowed to fall through to session creation; an offline or
    /// malformed response must not manufacture a duplicate conversation.
    func resumeLastSession(
        preferredSessionId: String?,
        completion: @escaping (ChatSessionResumeResult) -> Void
    ) {
        threadOpenGeneration += 1
        let generation = threadOpenGeneration
        let preferred = preferredSessionId?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if let preferred, !preferred.isEmpty {
            resumeExactSession(preferred, generation: generation, completion: completion)
        } else {
            resumeMostRecentSessionPage(
                offset: 0,
                generation: generation,
                seenSessionIds: [],
                pageCount: 0,
                completion: completion
            )
        }
    }

    private func resumeExactSession(
        _ sessionId: String,
        generation: Int,
        completion: @escaping (ChatSessionResumeResult) -> Void
    ) {
        guard let url = mutationURL(path: "chat/sessions/\(sessionId)") else {
            completion(.unavailable)
            return
        }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            guard let self else { return }
            let status = (response as? HTTPURLResponse)?.statusCode
            let session = data.flatMap { try? JSONDecoder().decode(ChatSessionDetailResponse.self, from: $0).session }
            DispatchQueue.main.async {
                guard generation == self.threadOpenGeneration else {
                    completion(.unavailable)
                    return
                }
                if error == nil, status.map({ (200..<300).contains($0) }) == true, let session {
                    self.applySelectedSession(session)
                    completion(.restored(session.id))
                } else if status == 404 {
                    completion(.missing)
                } else {
                    completion(.unavailable)
                }
            }
        }.resume()
    }

    func resumeDefaultSession(completion: @escaping (String?) -> Void) {
        resumeLastSession(preferredSessionId: nil) { result in
            if case .restored(let id) = result {
                completion(id)
            } else {
                completion(nil)
            }
        }
    }

    private func resumeMostRecentSessionPage(
        offset: Int,
        generation: Int,
        seenSessionIds: Set<String>,
        pageCount: Int,
        completion: @escaping (ChatSessionResumeResult) -> Void
    ) {
        guard pageCount < 100 else {
            completion(.unavailable)
            return
        }
        guard var components = URLComponents(string: "\(base)/chat/sessions") else {
            completion(.unavailable)
            return
        }
        components.queryItems = [
            URLQueryItem(name: "history_lane", value: ChatHistoryLane.personal.rawValue),
            URLQueryItem(name: "limit", value: String(Self.pageSize)),
            URLQueryItem(name: "offset", value: String(offset)),
        ]
        guard let url = components.url else {
            completion(.unavailable)
            return
        }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        networkSession.dataTask(with: request) { [weak self] data, response, _ in
            guard let self else { return }
            guard Self.isSuccessful(response), let data else {
                DispatchQueue.main.async { completion(.unavailable) }
                return
            }
            let page = try? JSONDecoder().decode(ChatSessionListResponse.self, from: data)
            let bareSessions = page == nil ? try? JSONDecoder().decode([ChatSession].self, from: data) : nil
            guard let sessions = page?.sessions ?? bareSessions else {
                DispatchQueue.main.async { completion(.unavailable) }
                return
            }
            if let pick = Self.pickResumeSessionId(from: sessions) {
                DispatchQueue.main.async {
                    guard generation == self.threadOpenGeneration else {
                        completion(.unavailable)
                        return
                    }
                    if let session = sessions.first(where: { $0.id == pick }) {
                        self.applySelectedSession(session)
                    }
                    completion(.restored(pick))
                }
                return
            }
            let pageIds = Set(sessions.map(\.id).filter { !$0.isEmpty })
            guard sessions.isEmpty || !pageIds.isSubset(of: seenSessionIds) else {
                DispatchQueue.main.async { completion(.unavailable) }
                return
            }
            let nextSeenSessionIds = seenSessionIds.union(pageIds)
            let nextOffset = offset + sessions.count
            let serverHasNextPage = page?.total.map { nextOffset < $0 } ?? false
            let legacyFullPageMayHaveMore = page == nil && sessions.count == Self.pageSize
            if !sessions.isEmpty, serverHasNextPage || legacyFullPageMayHaveMore {
                self.resumeMostRecentSessionPage(
                    offset: nextOffset,
                    generation: generation,
                    seenSessionIds: nextSeenSessionIds,
                    pageCount: pageCount + 1,
                    completion: completion
                )
            } else {
                DispatchQueue.main.async {
                    guard generation == self.threadOpenGeneration else {
                        completion(.unavailable)
                        return
                    }
                    completion(.missing)
                }
            }
        }.resume()
    }

    func archiveSession(_ id: String) {
        guard !isDefaultSession(id) else { return }
        patchSession(id, status: "archived")
    }

    func unarchiveSession(_ id: String) {
        patchSession(id, status: "active")
    }

    private func isDefaultSession(_ id: String) -> Bool {
        sessions.first(where: { $0.id == id })?.isDefaultSession == true
            || searchResults.first(where: { $0.session?.id == id })?.session?.isDefaultSession == true
    }

    private func patchSession(_ id: String, status: String) {
        guard let url = mutationURL(path: "chat/sessions/\(id)") else { return }
        var request = URLRequest(url: url)
        request.httpMethod = "PATCH"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = try? JSONSerialization.data(withJSONObject: ["status": status])
        sendMutation(request) { [weak self] in
            if status == "archived", self?.activeSessionId == id {
                self?.activeSessionId = nil
                self?.selectedSessionSnapshot = nil
            }
            self?.fetchCurrentPage()
        }
    }

    func deleteSession(_ id: String) {
        guard !isDefaultSession(id), let url = mutationURL(path: "chat/sessions/\(id)") else { return }
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        MagicianAccess.authorize(&request)
        sendMutation(request) { [weak self] in
            if self?.activeSessionId == id {
                self?.activeSessionId = nil
                self?.selectedSessionSnapshot = nil
            }
            self?.fetchCurrentPage()
        }
    }

    func archiveThread(_ id: String) {
        guard id != "general" else { return }
        patchThread(id, archived: true)
    }

    func unarchiveThread(_ id: String) {
        patchThread(id, archived: false)
    }

    private func patchThread(_ id: String, archived: Bool) {
        guard id != "general", let url = mutationURL(path: "ui-threads/\(id)") else { return }
        var request = URLRequest(url: url)
        request.httpMethod = "PATCH"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = try? JSONSerialization.data(withJSONObject: ["archived": archived])
        sendMutation(request) { [weak self] in
            if archived, self?.activeThreadId == id { self?.activeThreadId = "general" }
            self?.fetchCurrentPage()
        }
    }

    func deleteThread(_ id: String) {
        guard id != "general", let url = mutationURL(path: "ui-threads/\(id)") else { return }
        var request = URLRequest(url: url)
        request.httpMethod = "DELETE"
        MagicianAccess.authorize(&request)
        sendMutation(request) { [weak self] in
            if self?.activeThreadId == id { self?.activeThreadId = "general" }
            self?.fetchCurrentPage()
        }
    }

    private func mutationURL(path: String, extraQueryItems: [URLQueryItem] = []) -> URL? {
        guard var components = URLComponents(string: "\(base)/\(path)") else { return nil }
        components.queryItems = extraQueryItems.isEmpty ? nil : extraQueryItems
        return components.url
    }

    private func sendMutation(_ request: URLRequest, onSuccess: @escaping () -> Void) {
        errorMessage = nil
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            DispatchQueue.main.async {
                guard let self else { return }
                guard error == nil, Self.isSuccessful(response) else {
                    let serverMessage = data.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }?["error"] as? String
                    self.errorMessage = serverMessage ?? error?.localizedDescription ?? "The history action failed."
                    return
                }
                onSuccess()
            }
        }.resume()
    }

    func newThread(name: String, completion: @escaping (String?) -> Void) {
        guard let url = mutationURL(path: "ui-threads") else {
            completion(nil)
            return
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        request.httpBody = try? JSONSerialization.data(
            withJSONObject: ["id": NSNull(), "name": trimmed.isEmpty ? "New thread" : trimmed]
        )
        networkSession.dataTask(with: request) { [weak self] data, response, _ in
            var threadId: String? = nil
            if Self.isSuccessful(response), let data,
               let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                threadId = object["id"] as? String
            }
            DispatchQueue.main.async {
                guard let self else { return }
                if let threadId {
                    self.activeThreadId = threadId
                    self.activeTab = "threads"
                    self.historyLane = .personal
                    self.offset = 0
                    self.fetchCurrentPage()
                }
                completion(threadId)
            }
        }.resume()
    }

    func newSession(
        uiThreadId: String = "general",
        historyLane: ChatHistoryLane = .personal,
        completion: @escaping (String?) -> Void
    ) {
        threadOpenGeneration += 1
        createSession(
            uiThreadId: uiThreadId,
            historyLane: historyLane,
            selectionGeneration: nil,
            completion: completion
        )
    }

    /// Create the launch replacement only while the selection state that
    /// requested it is still current. Unlike an explicit New Session action,
    /// this must never overwrite a conversation the owner opened while the
    /// replacement request was in flight.
    func newSessionIfNothingSelected(
        uiThreadId: String = "general",
        historyLane: ChatHistoryLane = .personal,
        completion: @escaping (String?) -> Void
    ) {
        guard activeSessionId == nil else {
            completion(nil)
            return
        }
        createSession(
            uiThreadId: uiThreadId,
            historyLane: historyLane,
            selectionGeneration: threadOpenGeneration,
            completion: completion
        )
    }

    private func createSession(
        uiThreadId: String,
        historyLane: ChatHistoryLane,
        selectionGeneration: Int?,
        completion: @escaping (String?) -> Void
    ) {
        guard let url = mutationURL(
            path: "chat/new",
            extraQueryItems: [
                URLQueryItem(name: "ui_thread_id", value: uiThreadId),
                URLQueryItem(name: "history_lane", value: historyLane.rawValue),
            ]
        ) else {
            completion(nil)
            return
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        MagicianAccess.authorize(&request)
        request.httpBody = "{}".data(using: .utf8)
        networkSession.dataTask(with: request) { [weak self] data, response, error in
            let envelope = data.flatMap { try? JSONDecoder().decode(ChatSessionEnvelope.self, from: $0) }
            DispatchQueue.main.async {
                guard let self else { return }
                if let selectionGeneration,
                   selectionGeneration != self.threadOpenGeneration {
                    completion(nil)
                    return
                }
                if Self.isSuccessful(response), let session = envelope?.session {
                    self.applySelectedSession(session)
                    self.activeTab = "sessions"
                    self.historyLane = historyLane
                    self.offset = 0
                    self.fetchCurrentPage()
                    completion(session.id)
                } else {
                    self.errorMessage = error?.localizedDescription ?? "A new session could not be created."
                    completion(nil)
                }
            }
        }.resume()
    }
}
