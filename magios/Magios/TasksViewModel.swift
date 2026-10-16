import SwiftUI
import Combine

/// The lanes shown as tabs: user-visible tasks, recurring monitors (§9.3.1 —
/// the same canonical scheduled tasks, listed through the paginated
/// `/v3/monitors` API and rendered by `MonitorsLaneView`), and runtime/
/// chat-spawned internal tasks (the separate `/tasks/internal` feed).
enum TaskLane: String, CaseIterable, Identifiable, Hashable {
    case tasks, monitors, internalTasks
    var id: String { rawValue }
    var title: String {
        switch self {
        case .tasks: return "Tasks"
        case .monitors: return "Monitors"
        case .internalTasks: return "Internal"
        }
    }
}

enum TaskListLoadState: Equatable {
    case idle
    case loading
    case loaded
    case failed
}

struct TaskNavigationTarget: Equatable {
    let task: TaskV3
    let lane: TaskLane
}

/// The preset filters, mirroring the web `/tasks` toolbar (All / Inbox / Today /
/// Overdue / Running / Completed) with the same predicates.
enum TaskFilter: String, CaseIterable, Identifiable {
    case all, inbox, today, overdue, running, completed
    var id: String { rawValue }
    var title: String {
        switch self {
        case .all: return "All"
        case .inbox: return "Inbox"
        case .today: return "Today"
        case .overdue: return "Overdue"
        case .running: return "Running"
        case .completed: return "Completed"
        }
    }

    func matches(_ t: TaskV3, todayISO: String) -> Bool {
        switch self {
        case .all: return t.status != "completed"
        case .inbox: return t.tags.isEmpty && t.status == "pending"
        case .today: return (t.dueDate ?? "").hasPrefix(todayISO)
        case .overdue:
            guard let d = t.dueDate, !d.isEmpty else { return false }
            return d < todayISO && t.status != "completed"
        case .running: return t.status == "running" || t.status == "paused"
        case .completed: return t.status == "completed"
        }
    }
}

/// Sort field for the task list (web `InternalTaskSortField`: updated/created/title/agent/status).
enum TaskSortField: String, CaseIterable, Identifiable {
    case updatedAt, createdAt, title, agent, status
    var id: String { rawValue }
    var wire: String { switch self { case .updatedAt: return "updated_at"; case .createdAt: return "created_at"; case .title: return "title"; case .agent: return "agent_id"; case .status: return "status" } }
    var title: String {
        switch self {
        case .updatedAt: return "Updated"
        case .createdAt: return "Created"
        case .title: return "Title"
        case .agent: return "Agent"
        case .status: return "Status"
        }
    }
}

/// A pickable agent for the create form (`agent_id` is required at creation).
struct AgentOption: Identifiable, Equatable {
    let id: String
    let name: String
    var isPrimary = false
    static func decodeCrew(_ record: [String: Any]) -> AgentOption? {
        let item = record["definition"] as? [String: Any] ?? record
        guard let id = (item["agent_id"] ?? item["id"]) as? String, !id.isEmpty else { return nil }
        return AgentOption(id: id, name: item["name"] as? String ?? id)
    }
    static func decode(_ record: [String: Any]) -> AgentOption? {
        let item = record["definition"] as? [String: Any] ?? record
        guard let id = (item["agent_id"] ?? item["id"]) as? String, !id.isEmpty,
              (item["disabled"] as? Bool) != true,
              item["kind"] == nil || (item["kind"] as? String)?.lowercased() == "personal" else { return nil }
        return AgentOption(id: id, name: item["name"] as? String ?? id, isPrimary: item["is_primary"] as? Bool ?? false)
    }
}

enum ExecutionControlAction: String, Equatable {
    case pause, resume, steer, cancel
}

struct ExecutionControlCoordinationSnapshot: Equatable {
    var busyAction: ExecutionControlAction?
    var revision: UInt64
    var invalidatedBy: UUID?

    static let idle = ExecutionControlCoordinationSnapshot(
        busyAction: nil,
        revision: 0,
        invalidatedBy: nil
    )
}

@MainActor
final class ExecutionControlCoordinator: ObservableObject {
    static let shared = ExecutionControlCoordinator()

    @Published private var snapshots: [String: ExecutionControlCoordinationSnapshot] = [:]

    func snapshot(for executionId: String) -> ExecutionControlCoordinationSnapshot {
        snapshots[executionId] ?? .idle
    }

    @discardableResult
    func begin(_ action: ExecutionControlAction, for executionId: String) -> Bool {
        var snapshot = snapshot(for: executionId)
        guard snapshot.busyAction == nil else { return false }
        snapshot.busyAction = action
        snapshots[executionId] = snapshot
        return true
    }

    func finish(for executionId: String, invalidatedBy source: UUID) {
        var snapshot = snapshot(for: executionId)
        snapshot.busyAction = nil
        snapshot.revision &+= 1
        snapshot.invalidatedBy = source
        snapshots[executionId] = snapshot
    }

    func invalidate(_ executionId: String, source: UUID? = nil) {
        var snapshot = snapshot(for: executionId)
        snapshot.revision &+= 1
        snapshot.invalidatedBy = source
        snapshots[executionId] = snapshot
    }
}

struct ExecutionControlState: Codable, Equatable {
    let executionId: String
    let waitingState: String
    let pausedFromState: String?
    let pauseKind: String?
    let active: Bool
    let canPause: Bool
    let canResume: Bool
    let canSteer: Bool
    let canCancel: Bool

    enum CodingKeys: String, CodingKey {
        case executionId = "execution_id"
        case waitingState = "waiting_state"
        case pausedFromState = "paused_from_state"
        case pauseKind = "pause_kind"
        case active
        case canPause = "can_pause"
        case canResume = "can_resume"
        case canSteer = "can_steer"
        case canCancel = "can_cancel"
    }

    var hasAvailableAction: Bool { canPause || canResume || canSteer || canCancel }
}

@MainActor
final class ExecutionControlViewModel: ObservableObject {
    static let maximumSteerBytes = 4 * 1024

    @Published private(set) var state: ExecutionControlState?
    @Published private(set) var isLoading = false
    @Published private(set) var loadErrorMessage: String?
    @Published var errorMessage: String?

    let executionId: String
    private let session: URLSession
    private let onChanged: () -> Void
    private let coordinator: ExecutionControlCoordinator
    private let instanceId = UUID()
    private var requestGeneration: UInt64 = 0
    private var lastHostRefreshToken: String?
    private var lastCoordinatorRevision: UInt64?
    private var hasPendingRefresh = false

    var busyAction: ExecutionControlAction? {
        coordinator.snapshot(for: executionId).busyAction
    }

    convenience init(executionId: String, session: URLSession = .shared,
                     onChanged: @escaping () -> Void = {}) {
        self.init(
            executionId: executionId,
            session: session,
            coordinator: .shared,
            onChanged: onChanged
        )
    }

    init(executionId: String, session: URLSession,
         coordinator: ExecutionControlCoordinator,
         onChanged: @escaping () -> Void = {}) {
        self.executionId = executionId
        self.session = session
        self.coordinator = coordinator
        self.onChanged = onChanged
    }

    func refresh(hostToken: String, coordination: ExecutionControlCoordinationSnapshot) {
        let hostChanged = lastHostRefreshToken != hostToken
        let coordinatorChanged = lastCoordinatorRevision != coordination.revision
        lastHostRefreshToken = hostToken
        lastCoordinatorRevision = coordination.revision

        guard hostChanged || coordinatorChanged else { return }
        if !hostChanged, coordinatorChanged, coordination.invalidatedBy == instanceId,
           !hasPendingRefresh {
            return
        }
        if isLoading || busyAction != nil {
            hasPendingRefresh = true
            return
        }
        hasPendingRefresh = false
        load()
    }

    func isCurrent(hostToken: String, coordination: ExecutionControlCoordinationSnapshot) -> Bool {
        lastHostRefreshToken == hostToken && lastCoordinatorRevision == coordination.revision
    }

    func load() {
        guard !isLoading, busyAction == nil else { return }
        requestGeneration &+= 1
        let generation = requestGeneration
        isLoading = true
        loadErrorMessage = nil
        Task {
            do {
                let refreshedState = try await requestState()
                guard generation == requestGeneration else { return }
                state = refreshedState
            } catch {
                guard generation == requestGeneration else { return }
                loadErrorMessage = error.localizedDescription
            }
            if generation == requestGeneration {
                isLoading = false
                if hasPendingRefresh {
                    hasPendingRefresh = false
                    load()
                }
            }
        }
    }

    @discardableResult
    func perform(_ action: ExecutionControlAction, message: String? = nil) -> Bool {
        if action == .steer {
            let guidance = message?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            guard !guidance.isEmpty else {
                errorMessage = "Steer guidance cannot be empty."
                return false
            }
            guard Self.steerByteCount(guidance) <= Self.maximumSteerBytes else {
                errorMessage = "Steer guidance must be 4,096 bytes or less."
                return false
            }
        }
        guard coordinator.begin(action, for: executionId) else {
            errorMessage = "Another action is already updating this run. Try again when it finishes."
            return false
        }
        requestGeneration &+= 1
        isLoading = false
        errorMessage = nil
        loadErrorMessage = nil
        Task {
            defer { coordinator.finish(for: executionId, invalidatedBy: instanceId) }
            var actionError: Error?
            do {
                try await send(action, message: message)
                onChanged()
            } catch {
                actionError = error
            }

            do {
                state = try await requestState()
                loadErrorMessage = nil
            } catch {
                loadErrorMessage = error.localizedDescription
            }

            errorMessage = actionError?.localizedDescription
        }
        return true
    }

    static func steerByteCount(_ message: String) -> Int {
        message.lengthOfBytes(using: .utf8)
    }

    static func timeoutInterval(for action: ExecutionControlAction) -> TimeInterval {
        action == .pause ? 45 : 15
    }

    private func requestState() async throws -> ExecutionControlState {
        var request = try makeRequest(suffix: "control-state", method: "GET")
        request.timeoutInterval = 10
        let (data, response) = try await session.data(for: request)
        try validate(response: response, data: data, action: "load execution controls")
        return try JSONDecoder().decode(ExecutionControlState.self, from: data)
    }

    private func send(_ action: ExecutionControlAction, message: String?) async throws {
        var request = try makeRequest(suffix: action.rawValue, method: "POST")
        request.timeoutInterval = Self.timeoutInterval(for: action)
        if action == .steer {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: ["message": message ?? ""])
        }
        let (data, response) = try await session.data(for: request)
        try validate(response: response, data: data, action: action.rawValue)
    }

    private func makeRequest(suffix: String, method: String) throws -> URLRequest {
        let pathSegmentCharacters = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))
        guard let encodedExecutionId = executionId.addingPercentEncoding(
            withAllowedCharacters: pathSegmentCharacters
        ) else {
            throw URLError(.badURL)
        }
        let urlString = "\(MagicianAccess.baseURL.absoluteString)/api/magician/v2/executions/\(encodedExecutionId)/\(suffix)"
        guard let url = URL(string: urlString) else {
            throw URLError(.badURL)
        }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        request.httpMethod = method
        return request
    }

    private func validate(response: URLResponse, data: Data, action: String) throws {
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        guard (200..<300).contains(http.statusCode) else {
            let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            let message = (object?["error"] as? String)
                ?? (object?["message"] as? String)
                ?? "Could not \(action) (HTTP \(http.statusCode))."
            throw NSError(domain: "ExecutionControls", code: http.statusCode,
                          userInfo: [NSLocalizedDescriptionKey: message])
        }
    }
}

struct InternalTaskQuery {
    var offset = 0
    var limit = 50
    var sort: TaskSortField = .updatedAt
    var ascending = false
    var agent: String? = nil
    var status: String? = nil
    var search = ""
    var path: String {
        var pairs = [("limit", String(min(max(limit, 1), 500))), ("offset", String(max(offset, 0))), ("sort", sort.wire), ("order", ascending ? "asc" : "desc")]
        for (key, raw) in [("agent_id", agent ?? ""), ("status", status ?? ""), ("query", search)] {
            let value = raw.trimmingCharacters(in: .whitespacesAndNewlines); if !value.isEmpty { pairs.append((key, value)) }
        }
        return "/tasks/internal?" + pairs.map { $0.0 + "=" + WorkAPIClient.query($0.1) }.joined(separator: "&")
    }
}

struct TaskThreadChoice: Identifiable, Equatable {
    let id: String
    let name: String
    let archived: Bool
    init?(_ row: [String: Any]) { guard let id = row["id"] as? String, !id.isEmpty else { return nil }; self.id = id; name = row["name"] as? String ?? id; archived = row["archived"] as? Bool ?? false }
}

enum TaskReferenceComposer {
    static func description(_ text: String, ids: [String]) -> String {
        let missing = ids.filter { !text.components(separatedBy: .whitespacesAndNewlines).contains("task:" + $0) }
        return text + (missing.isEmpty ? "" : (text.isEmpty ? "" : "\n\n") + missing.map { "task:" + $0 }.joined(separator: " "))
    }
    static func dependencies(_ manual: [String], references: [String]) -> [String] {
        var seen = Set<String>()
        return (manual + references).map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty && seen.insert($0).inserted }
    }
}

final class TasksViewModel: ObservableObject {
    @Published private(set) var completedReferences: [TaskV3] = []
    @Published private(set) var referencesLoading = false
    @Published private(set) var referencesError: String?
    @Published private(set) var referencesHasMore = false
    private var referenceOffset = 0
    private var referenceGeneration: UInt64 = 0
    func loadReferences(append: Bool = false) {
        guard connectionCurrent, !append || (!referencesLoading && referencesHasMore) else { return }
#if DEBUG
        if isUITestLaunch { completedReferences = tasks.filter { $0.status == "completed" }; referencesHasMore = false; return }
#endif
        referenceGeneration &+= 1; let ticket = referenceGeneration
        let offset = append ? referenceOffset : 0
        referencesLoading = true; referencesError = nil
        guard var components = URLComponents(string: "\(base)/tasks") else { referencesLoading = false; return }
        components.queryItems = [URLQueryItem(name: "limit", value: "50"), URLQueryItem(name: "offset", value: String(offset)), URLQueryItem(name: "view", value: "completed"), URLQueryItem(name: "today", value: todayISO)]
        guard let url = components.url else { referencesLoading = false; return }
        var request = URLRequest(url: url); guard authorize(&request) else { referencesLoading = false; return }
        session.dataTask(with: request) { [weak self] data, response, error in
            let ok = (response as? HTTPURLResponse).map { (200..<300).contains($0.statusCode) } == true
            let page = error == nil && ok ? data.flatMap { try? JSONDecoder().decode(TaskListResponse.self, from: $0) } : nil
            DispatchQueue.main.async {
                guard let self, self.connectionCurrent, self.referenceGeneration == ticket else { return }
                self.referencesLoading = false
                guard let page else { self.referencesError = "Completed tasks could not be loaded. Retry."; return }
                var seen = Set<String>()
                self.completedReferences = ((append ? self.completedReferences : []) + page.tasks.filter { $0.status == "completed" }).filter { seen.insert($0.id).inserted }
                self.referenceOffset = offset + page.tasks.count
                self.referencesHasMore = page.pagination?.hasMore == true && !page.tasks.isEmpty
            }
        }.resume()
    }

    @Published var tasks: [TaskV3] = []
    @Published var internalTasks: [TaskV3] = []
    @Published var agents: [AgentOption] = []
    @Published private(set) var internalCrewChoices: [AgentOption] = []
    @Published private(set) var agentsLoading = false
    @Published private(set) var agentsError: String?
    private var agentRequestGeneration: UInt64 = 0
    @Published var lane: TaskLane = .tasks
    @Published var filter: TaskFilter = .all
    /// Mutually exclusive with `filter` (web parity): when set, the preset is ignored.
    @Published var selectedTag: String?
    /// Full-text search over title + tag names (web parity), applied on top of the
    /// active tag/preset filter.
    @Published var searchQuery = "" { didSet { if searchQuery != oldValue { if lane == .internalTasks { refetchInternalLane() } else if lane == .tasks { refetchTaskLane() } } } }
    @Published private(set) var taskLoadState: TaskListLoadState = .idle
    @Published private(set) var internalTaskLoadState: TaskListLoadState = .idle
    /// Task-level mutations are serialized and surfaced instead of silently
    /// reloading an unchanged list when the backend rejects an action.
    @Published private(set) var mutatingTaskID: String?
    @Published var actionErrorMessage: String?
    @Published private(set) var actionNoticeMessage: String?
    /// The task whose output-synthesis retry is currently in flight (disables its pill).
    @Published var retryingSynthesisId: String?
    /// List sort (web parity: default updated / descending).
    @Published var sortField: TaskSortField = .updatedAt
    @Published var sortAscending = false
    /// Internal-lane-only filters (web parity: status + agent dropdowns). nil/"" = any.
    @Published var internalStatusFilter: String? { didSet { if internalStatusFilter != oldValue { refetchInternalLane() } } }
    @Published var internalAgentFilter: String? { didSet { if internalAgentFilter != oldValue { refetchInternalLane() } } }
    @Published var internalPageSize = 50 { didSet { if internalPageSize != oldValue { refetchInternalLane() } } }
    @Published var selectingInternal = false
    @Published private(set) var selectedInternalIDs = Set<String>()
    @Published private(set) var cleanupProgress: String?
    @Published private(set) var cleanupFailures: [String: String] = [:]
    func toggleInternalSelection(_ task: TaskV3) {
        guard task.canCleanUpInternal, mutatingTaskID == nil else { return }
        if !selectedInternalIDs.insert(task.id).inserted { selectedInternalIDs.remove(task.id) }
    }
    func selectLoadedInternal() { guard mutatingTaskID == nil else { return }; selectedInternalIDs = Set(visibleTasks.filter(\.canCleanUpInternal).map(\.id)) }
    func clearInternalSelection() { guard mutatingTaskID == nil else { return }; selectedInternalIDs = [] }
    func deleteSelectedInternal() {
        guard mutatingTaskID == nil, connectionCurrent else { return }
        let targets = internalTasks.filter { selectedInternalIDs.contains($0.id) && $0.canCleanUpInternal }
        guard !targets.isEmpty else { return }
        mutatingTaskID = "internal-cleanup"; cleanupFailures = [:]; actionErrorMessage = nil
        Task { @MainActor [weak self] in
            guard let self else { return }; var deleted = 0
            for (index, task) in targets.enumerated() {
                guard self.connectionCurrent else { return }
                self.cleanupProgress = "Deleting \(index + 1) of \(targets.count)…"
                do {
                    guard let url = URL(string: self.base + "/tasks/internal/" + WorkAPIClient.query(task.id)) else { throw URLError(.badURL) }
                    var request = URLRequest(url: url); guard self.authorize(&request) else { return }; request.httpMethod = "DELETE"; request.timeoutInterval = 30
                    let (data, response) = try await self.session.data(for: request)
                    guard self.connectionCurrent else { return }
                    let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                    guard (200..<300).contains(status) else { throw NSError(domain: "InternalCleanup", code: status, userInfo: [NSLocalizedDescriptionKey: Self.taskMutationError(data: data, status: status, fallback: "Delete failed")]) }
                    deleted += 1; self.selectedInternalIDs.remove(task.id); self.internalTasks.removeAll { $0.id == task.id }
                } catch { guard self.connectionCurrent else { return }; self.cleanupFailures[task.id] = error.localizedDescription }
            }
            self.mutatingTaskID = nil; self.cleanupProgress = "Deleted \(deleted) of \(targets.count). \(self.cleanupFailures.count) failed."
            self.load()
        }
    }
    private var seenInternalAgents = Set<String>()
    func refetchInternalLane() {
        guard mutatingTaskID != "internal-cleanup", !isUITestLaunch else { return }
        selectedInternalIDs = []; cleanupFailures = [:]; cleanupProgress = nil
        internalLoadedSpan = internalPageSize; internalTasks = []; internalHasMore = false
        fetchPage(lane: .internalTasks, offset: 0, limit: internalPageSize)
    }

    private let origin = MagicianAccess.baseURL
    private let headers = MagicianAccess.headers()
    private var invalidated = false
    private var connectionObserver: AnyCancellable?
    private var connectionCurrent: Bool { !invalidated && origin == MagicianAccess.baseURL && headers == MagicianAccess.headers() }
    private var base: String { "\(origin.absoluteString)/api/magician/v3" }
    @Published private(set) var threadContext: String?

    private func authorize(_ request: inout URLRequest) -> Bool {
        guard connectionCurrent else { actionErrorMessage = "Connection changed. Reopen this Work view."; return false }
        headers.forEach { request.setValue($0.value, forHTTPHeaderField: $0.key) }
        return true
    }

    func setThreadContext(_ id: String?) {
        let normalized = id?.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let next = normalized?.isEmpty == false ? normalized : nil
        guard threadContext != next else { return }
        threadContext = next; lane = .tasks
        listRequestGenerations[.tasks] = (listRequestGenerations[.tasks] ?? 0) &+ 1
        tasks = []; gracedRows = []; gracedIds = []; lastStatus = [:]
        graceTimers.values.forEach { $0.cancel() }; graceTimers = [:]
        laneCounts = nil; loadedView = nil; tasksHasMore = false; tasksTotal = 0
        selectedTag = nil; tasksLoadedSpan = Self.listPageSize
        refetchTaskLane()
    }
    private let session: URLSession
    private var listRequestGenerations: [TaskLane: UInt64] = [:]

    init(session: URLSession = .shared) {
        self.session = session
        connectionObserver = NotificationCenter.default.publisher(for: .magicianMobileConnectionDidChange)
            .receive(on: RunLoop.main).sink { [weak self] _ in
                guard let self else { return }
                self.invalidated = true; self.disconnectRealtime(); self.reloadWork?.cancel()
                self.listRequestGenerations = self.listRequestGenerations.mapValues { $0 &+ 1 }
                self.tasks = []; self.internalTasks = []; self.agents = []; self.internalCrewChoices = []; self.agentsLoading = false; self.agentsError = "Connection changed. Reopen Work."; self.agentRequestGeneration &+= 1; self.gracedRows = []
                self.selectedInternalIDs = []; self.cleanupFailures = [:]; self.cleanupProgress = nil; self.mutatingTaskID = nil
                self.completedReferences = []; self.referencesLoading = false; self.referencesHasMore = false; self.referenceGeneration &+= 1
                self.actionErrorMessage = "Connection changed. Reopen this Work view."
            }
#if DEBUG
        if ProcessInfo.processInfo.arguments.contains("--tasks-ui-test-fixture") {
            let fixture: [String: Any] = ["tasks": [
                [
                    "id": "fixture-active", "title": "Fixture active task",
                    "description": "Build a concise launch brief for the mobile detail workspace.",
                    "status": "running", "agent_id": "researcher", "ui_thread_id": "launch",
                    "priority": "p1", "has_plan": true, "plan_status": "approved",
                    "latest_plan_id": "plan-fixture", "latest_root_execution_id": "exec-fixture",
                    "tags": [["id": "launch", "name": "launch"]],
                    "created_at": "2026-07-15T06:00:00Z", "updated_at": "2026-07-15T07:00:00Z"
                ],
                [
                    "id": "fixture-internal", "title": "Fixture internal task",
                    "description": "Compact the internal memory ledger.", "status": "failed",
                    "agent_id": "memory-agent", "ui_thread_id": "system", "lifecycle": "internal",
                    "created_by": "__system__", "latest_root_execution_id": "exec-internal",
                    "created_at": "2026-07-15T01:00:00Z", "updated_at": "2026-07-15T01:01:00Z"
                ],
                [
                    "id": "fixture-completed", "title": "Fixture completed task",
                    "description": "A completed task ready for a durable Notes page.",
                    "status": "completed", "agent_id": "researcher", "ui_thread_id": "launch",
                    "completion_summary": "The launch brief is ready.",
                    "created_at": "2026-07-14T06:00:00Z", "updated_at": "2026-07-15T08:00:00Z"
                ]
            ]]
            if let data = try? JSONSerialization.data(withJSONObject: fixture),
               let decoded = try? JSONDecoder().decode(TaskListResponse.self, from: data) {
                tasks = decoded.tasks.filter { $0.lifecycle != "internal" }
                internalTasks = decoded.tasks.filter { $0.lifecycle == "internal" }
                taskLoadState = .loaded
                internalTaskLoadState = .loaded
            }
        } else if isUITestLaunch {
            taskLoadState = .loaded
            internalTaskLoadState = .loaded
        }
#endif
    }

    var activeLoadState: TaskListLoadState {
        switch lane {
        case .tasks: return taskLoadState
        case .internalTasks: return internalTaskLoadState
        case .monitors: return .loaded
        }
    }

    /// Treat the pre-request idle state as loading so the first rendered frame
    /// cannot claim the lane is empty before `onAppear` starts its request.
    var isLoading: Bool {
        activeLoadState == .idle || activeLoadState == .loading
    }

    /// A calendar date rendered as `yyyy-MM-dd` in an explicit zone.
    ///
    /// The zone is spelled out rather than left to the formatter's default
    /// because this one string now does two jobs: it is what the local
    /// today/overdue predicates compare against, and it is the `today=` the
    /// server computes those same lanes from. Rendering a local instant in
    /// UTC would name the previous day for the whole day everywhere east of
    /// Greenwich — a wrong answer that looks entirely right, and one the web
    /// client was shipping until this plan's Phase 0.
    static func localDateISO(_ date: Date, in timeZone: TimeZone) -> String {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.locale = Locale(identifier: "en_US_POSIX")
        f.timeZone = timeZone
        f.dateFormat = "yyyy-MM-dd"
        return f.string(from: date)
    }

    /// The READER's local date — never the server's, never UTC.
    var todayISO: String { Self.localDateISO(Date(), in: .current) }

    var activeTasks: [TaskV3] {
        switch lane {
        case .tasks: return tasks
        case .internalTasks: return internalTasks
        // The monitors lane renders `MonitorsLaneView` rows, not TaskV3 cards.
        case .monitors: return []
        }
    }

    /// Tags present in the active lane (web derives the tag list from the tasks).
    var availableTags: [String] {
        Array(Set(activeTasks.flatMap { $0.tags.map(\.name) }.filter { !$0.isEmpty })).sorted()
    }

    /// Distinct agent ids in the internal lane — the internal-lane agent filter's options.
    var availableInternalAgents: [String] {
        Array(seenInternalAgents.union(internalCrewChoices.map(\.id)).union(internalTasks.map(\.agentId)).union(internalAgentFilter.map { [$0] } ?? []).filter { !$0.isEmpty }).sorted()
    }

    /// The active lane's tasks after its own filters and search, then sorted by
    /// the chosen field/order. Regular task presets/tags do not constrain the
    /// Internal lane; its Web counterpart defaults to every status and exposes
    /// dedicated status and agent filters.
    ///
    /// The tasks lane's preset is not decided here once `/v3/tasks?view=` has
    /// answered it over the whole scoped pool. Re-filtering the page was the
    /// defect — it made tapping Overdue a search of the first 50 rows, so a
    /// matching task at position 60 stayed invisible until the reader tapped
    /// Load more twice. The local predicate is consulted only where there is
    /// no answer to consult: a selected tag, a binary that does not compute
    /// lanes, and the round trip a lane change costs.
    var visibleTasks: [TaskV3] {
        let today = todayISO
        var filtered: [TaskV3]
        if lane == .internalTasks {
            filtered = activeTasks
        } else if let tag = selectedTag {
            // No server lane exists for a tag, so the request carries no
            // `view=` while one is selected and the pool arrives unfiltered.
            filtered = activeTasks.filter { $0.tags.contains { $0.name == tag } }
        } else if lane == .tasks {
            filtered = activeTasks
            if loadedView != filter {
                // The rows on hand answer the lane the reader just left. Stand
                // in with the local predicate for the round trip the change
                // costs, so the list does not show the old lane under the new
                // chip. Once the selected lane's answer lands this is skipped
                // entirely — the local predicate never gets to narrow, and so
                // never gets to contradict, a server answer.
                filtered = filtered.filter { filter.matches($0, todayISO: today) }
            }
            // Grace period: keep just-completed tasks in non-completed views
            // briefly. The server answers from stored state and must not model
            // this, so a ticked task is simply absent from the lane it was
            // ticked in; `gracedRows` is what holds it there.
            if filter != .completed { filtered += gracedRows }
        } else {
            filtered = []
        }
        if lane == .internalTasks {
            if let st = internalStatusFilter, !st.isEmpty { filtered = filtered.filter { $0.status == st } }
            if let ag = internalAgentFilter, !ag.isEmpty { filtered = filtered.filter { $0.agentId == ag } }
        }
        let q = searchQuery.trimmingCharacters(in: .whitespaces).lowercased()
        if !q.isEmpty {
            filtered = filtered.filter { t in
                t.title.lowercased().contains(q) || t.tags.contains { $0.name.lowercased().contains(q) }
            }
        }
        return sortTasks(filtered)
    }

    /// Sort by the active field/order (web parity). ISO date strings compare
    /// lexicographically = chronologically.
    private func sortTasks(_ tasks: [TaskV3]) -> [TaskV3] {
        let sorted = tasks.sorted { a, b in
            switch sortField {
            case .updatedAt: return a.updatedAt < b.updatedAt
            case .createdAt: return a.createdAt < b.createdAt
            case .title: return a.title.localizedCaseInsensitiveCompare(b.title) == .orderedAscending
            case .agent: return a.agentId.localizedCaseInsensitiveCompare(b.agentId) == .orderedAscending
            case .status: return a.status < b.status
            }
        }
        return sortAscending ? sorted : sorted.reversed()
    }

    /// Cycle sort: same field toggles order, a new field resets to descending (web `setSort`).
    func setSort(field: TaskSortField) {
        if sortField == field { sortAscending.toggle() }
        else { sortField = field; sortAscending = false }
        if lane == .internalTasks { refetchInternalLane() } else if lane == .tasks { refetchTaskLane() }
    }

    /// Per-status counts for the active lane (the web's read-only status ledger).
    func count(status: String) -> Int { activeTasks.filter { $0.status == status }.count }

    /// The badge for a preset chip: that lane's total over the WHOLE scoped
    /// pool, as counted by the server before any lane filter.
    ///
    /// `nil` means the server did not report a count, and the chip must then
    /// render no badge at all. A `0` fabricated for an unreported lane is the
    /// worst of both — it looks like an answer and is really "we don't know".
    /// A `0` the server did report is a real answer and does earn a badge.
    ///
    /// Only the tasks lane has counts: they describe the `/v3/tasks` pool, and
    /// showing them over the internal lane's rows would caption one list with
    /// another's totals.
    func laneCount(_ f: TaskFilter) -> Int? {
        guard lane == .tasks else { return nil }
        if let threadContext {
            return tasks.filter { $0.uiThreadId.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() == threadContext && f.matches($0, todayISO: todayISO) }.count + gracePeriodDelta(for: f)
        }
        guard let total = laneCounts?[f.rawValue] else { return nil }
        return total + gracePeriodDelta(for: f)
    }

    /// Rows the completion grace period is holding are the client's own
    /// overlay; the server counted stored state and knows nothing about them.
    /// The badge is moved by exactly the rows the list adds, through this one
    /// function, so the number cannot come to disagree with what is under it.
    /// The other five lanes are reported as counted — their rows are not on
    /// screen, so there is nothing to reconcile against.
    private func gracePeriodDelta(for f: TaskFilter) -> Int {
        guard selectedTag == nil, f == filter, f != .completed else { return 0 }
        return gracedRows.count
    }

    func setFilter(_ f: TaskFilter) {
        guard f != filter || selectedTag != nil else { return }
        filter = f
        selectedTag = nil
        refetchTaskLane()
    }

    func toggleTag(_ name: String) {
        selectedTag = (selectedTag == name) ? nil : name
        refetchTaskLane()
    }

    /// A lane change is a new query, not a re-read of what is already held:
    /// restart at the first page so the answer covers the pool rather than
    /// whatever pages happen to be loaded.
    private func refetchTaskLane() {
        guard !isUITestLaunch else { return }
        tasksLoadedSpan = taskPageSize
        fetchPage(lane: .tasks, offset: 0, limit: taskPageSize)
    }

    /// Resolve cross-surface task links against both lanes. Internal tasks are
    /// preferred because backend path resolution also treats their canonical
    /// storage as authoritative when a stale duplicate id exists.
    func navigationTarget(for id: String) -> TaskNavigationTarget? {
        if let task = internalTasks.first(where: { $0.id == id }) {
            return TaskNavigationTarget(task: task, lane: .internalTasks)
        }
        if let task = tasks.first(where: { $0.id == id }) {
            return TaskNavigationTarget(task: task, lane: .tasks)
        }
        return nil
    }

    /// A cross-surface link can name an internal task outside the newest page
    /// retained by the lane. Use the backend's filtered query contract to fetch
    /// that row without loading the complete internal-task history.
    func resolveNavigationTarget(
        for id: String,
        completion: @escaping (TaskNavigationTarget?) -> Void
    ) {
        let targetID = id.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !targetID.isEmpty else {
            DispatchQueue.main.async { completion(nil) }
            return
        }
        if let cached = navigationTarget(for: targetID) {
            completion(cached)
            return
        }
        guard var components = URLComponents(string: "\(base)/tasks/internal") else {
            DispatchQueue.main.async { completion(nil) }
            return
        }
        components.queryItems = (components.queryItems ?? []) + [
            URLQueryItem(name: "limit", value: "25"),
            URLQueryItem(name: "sort", value: "updated_at"),
            URLQueryItem(name: "order", value: "desc"),
            URLQueryItem(name: "query", value: targetID)
        ]
        guard let url = components.url else {
            DispatchQueue.main.async { completion(nil) }
            return
        }
        var request = URLRequest(url: url)
        guard authorize(&request) else { return }
        request.httpMethod = "GET"
        session.dataTask(with: request) { [weak self] data, response, _ in
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let list = (200..<300).contains(status)
                ? data.flatMap { try? JSONDecoder().decode(TaskListResponse.self, from: $0) }
                : nil
            let fetched = list?.tasks.first(where: { $0.id == targetID })
            DispatchQueue.main.async {
                guard let self, self.connectionCurrent else { completion(nil); return }
                if let fetched {
                    if let index = self.internalTasks.firstIndex(where: { $0.id == fetched.id }) {
                        self.internalTasks[index] = fetched
                    } else {
                        self.internalTasks.insert(fetched, at: 0)
                    }
                }
                // A regular-lane load may have completed while the targeted
                // internal lookup was in flight, so resolve both caches again.
                completion(self.navigationTarget(for: targetID))
            }
        }.resume()
    }

    /// Clear stale list constraints before presenting a linked task so closing
    /// the detail workspace leaves the destination visible in the selected lane.
    func prepareForNavigation(to target: TaskNavigationTarget) {
        lane = target.lane
        selectedTag = nil
        searchQuery = ""
        internalStatusFilter = nil
        internalAgentFilter = nil
        filter = target.task.status == "completed" ? .completed : .all
        // The preset is a server query now, so moving it has to re-ask; the
        // page already held answers a lane the reader has just left.
        if target.lane == .internalTasks {
            refetchInternalLane()
        } else {
            refetchTaskLane()
        }
    }

    // ── server pagination state (load-more appends; see fetchPage) ──
    @Published var taskPageSize = 50 { didSet { if taskPageSize != oldValue { refetchTaskLane() } } }
    @Published private(set) var tasksTotal = 0
    @Published private(set) var tasksHasMore = false
    @Published private(set) var internalTotal = 0
    @Published private(set) var internalHasMore = false
    /// Rows the reader has asked for in each lane — one page after a lane
    /// change, one page more per Load more.
    ///
    /// This list ACCUMULATES, so "the page you are on" names nothing: a
    /// refetch re-requests this whole span from the top instead of page one.
    /// Re-asking for fifty rows would throw away every page the reader loaded
    /// to reach the row they just touched.
    private var tasksLoadedSpan = TasksViewModel.listPageSize
    private var internalLoadedSpan = TasksViewModel.listPageSize
    /// Every lane's total over the whole scoped pool, as reported beside
    /// `pagination`, keyed by wire name. `nil` until a response carries one,
    /// and `nil` is a real state: a binary that reports no counts did not
    /// compute lanes either, so the rows it sent answer no lane.
    @Published private(set) var laneCounts: [String: Int]?
    /// The lane the rows currently held were answered for — `nil` while they
    /// answer no lane at all (a tag request, or a legacy binary). Read only by
    /// `visibleTasks`, which changes in the same tick as the `@Published`
    /// `tasks`/`filter` it sits beside, so it needs no publisher of its own.
    private var loadedView: TaskFilter?

    /// Tasks that just completed — kept visible in non-completed views for a short
    /// grace period (web parity) so they don't vanish mid-glance.
    @Published var gracedIds: Set<String> = []
    /// The rows those ids name, held BESIDE the page rather than inside it.
    ///
    /// The server's lane drops a completed task the moment it is ticked, which
    /// is precisely what the grace period exists to prevent, so the row has to
    /// survive the refetch somewhere. Keeping it out of `tasks` means `tasks`
    /// stays exactly the page the server answered, and the grace's expiry can
    /// drop the row without waiting for another request.
    @Published private(set) var gracedRows: [TaskV3] = []
    private var lastStatus: [String: String] = [:]
    private var graceTimers: [String: DispatchWorkItem] = [:]

    /// Server page size. Load-more appends the next page (monitors idiom).
    static let listPageSize = 50
    /// The widest `limit` the task endpoints honour — both clamp to 500. A
    /// span refetch that asked for more would be answered with 500 rows
    /// anyway, so asking for exactly what can be served keeps the offset walk
    /// and the returned row count in agreement.
    static let maxRequestPageSize = 500

    /// Re-read the lanes the reader is holding. Every mutation funnels through
    /// here, so this asks for the span already loaded rather than page one —
    /// a tick on the fourth page must not collapse the list back to the first.
    func load() {
        guard !isUITestLaunch else { return }   // UI tests: no real backend fetch (keeps launch idle)
        fetchAgents()
        fetchPage(lane: .tasks, offset: 0, limit: tasksLoadedSpan)
        fetchPage(lane: .internalTasks, offset: 0, limit: internalLoadedSpan)
    }

    /// Append the next server page for a lane (the list footer's Load more).
    func loadMore(lane: TaskLane) {
        switch lane {
        case .tasks:
            guard tasksHasMore, taskLoadState != .loading else { return }
            fetchPage(lane: .tasks, offset: tasksLoadedSpan, limit: taskPageSize)
        case .internalTasks:
            guard internalHasMore, internalTaskLoadState != .loading else { return }
            fetchPage(lane: .internalTasks, offset: internalLoadedSpan, limit: internalPageSize)
        case .monitors:
            break // monitors paginate through MonitorsViewModel's cursor.
        }
    }

    private func fetchPage(lane: TaskLane, offset: Int, limit: Int = TasksViewModel.listPageSize) {
        if lane == .tasks && (threadContext != nil || !searchQuery.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || selectedTag != nil || sortField != .updatedAt || sortAscending) { fetchThreadPool(threadContext); return }
        let limit = min(max(limit, 1), Self.maxRequestPageSize)
        var paging = "limit=\(limit)&offset=\(offset)&sort=updated_at&order=desc"
        // A selected tag ignores the preset (web parity) and the server has no
        // tag lane, so no `view=` goes with it and the tag is matched over the
        // returned pool. Captured here, not read again in the completion: the
        // reader may have moved on by the time the answer lands.
        let requestedView: TaskFilter? = selectedTag == nil ? filter : nil
        if lane == .tasks {
            // `today=` always travels, even for the four lanes that don't need
            // it: it is what makes the server report `counts` at all, and the
            // two date lanes 400 without it rather than guessing a timezone.
            paging += "&today=\(todayISO)"
            if let requestedView { paging += "&view=\(requestedView.rawValue)" }
        }
        let path = lane == .internalTasks
            ? InternalTaskQuery(offset: offset, limit: limit, sort: sortField, ascending: sortAscending, agent: internalAgentFilter, status: internalStatusFilter, search: searchQuery).path
            : "/tasks?\(paging)"
        fetch(path: path, lane: lane) { [weak self] response in
            guard let self, self.connectionCurrent else { return }
            let append = offset > 0
            self.apply(response.tasks, lane: lane, append: append)
            switch lane {
            case .tasks:
                // Where the next page starts, read off what the server
                // ACTUALLY returned rather than off what was asked for: a pool
                // that shrank under the reader must not leave a walk that
                // steps over rows.
                self.tasksLoadedSpan = max(self.taskPageSize, offset + response.tasks.count)
                // Absent stays absent: a legacy binary and a request without a
                // today= both report nothing, and nothing is not six zeros.
                self.laneCounts = response.counts
                // `counts` and `view=` arrived on the same branch of the same
                // endpoint, so counts are the proof the lane was honoured. A
                // binary that answered without them ignored `view=` too, and
                // the rows it sent answer no lane — claiming otherwise would
                // hand the reader the unfiltered pool under a lane's name.
                self.loadedView = response.counts == nil ? nil : requestedView
                if let page = response.pagination {
                    self.tasksTotal = page.total
                    self.tasksHasMore = page.hasMore
                } else {
                    // Legacy binary: the whole pool arrived in one response.
                    self.tasksTotal = self.tasks.count
                    self.tasksHasMore = false
                }
            case .internalTasks:
                self.internalLoadedSpan = max(self.internalPageSize, offset + response.tasks.count)
                if let page = response.pagination {
                    self.internalTotal = page.total
                    self.internalHasMore = page.hasMore
                } else {
                    self.internalTotal = self.internalTasks.count
                    self.internalHasMore = false
                }
            case .monitors:
                break
            }
        }
    }

    /// The regular task endpoint has no thread predicate. Walk its unfiltered
    /// pages once and publish the complete scoped pool, with scoped counts.
    private func fetchThreadPool(_ threadID: String?) {
        guard connectionCurrent else { return }
        let generation = (listRequestGenerations[.tasks] ?? 0) &+ 1
        listRequestGenerations[.tasks] = generation
        setLoadState(.loading, lane: .tasks)
        fetchThreadPage(threadID, offset: 0, rows: [], generation: generation)
    }

    private func fetchThreadPage(_ threadID: String?, offset: Int, rows: [TaskV3], generation: UInt64) {
        var laneQuery = "&today=\(todayISO)"
        if threadID == nil && selectedTag == nil { laneQuery += "&view=\(filter.rawValue)" }
        guard connectionCurrent, listRequestGenerations[.tasks] == generation,
              let url = URL(string: "\(base)/tasks?limit=\(Self.maxRequestPageSize)&offset=\(offset)&sort=updated_at&order=desc\(laneQuery)") else { return }
        var request = URLRequest(url: url)
        guard authorize(&request) else { return }
        session.dataTask(with: request) { [weak self] data, response, error in
            let success = (response as? HTTPURLResponse).map { (200..<300).contains($0.statusCode) } == true
            let page = error == nil && success ? data.flatMap { try? JSONDecoder().decode(TaskListResponse.self, from: $0) } : nil
            DispatchQueue.main.async {
                guard let self, self.connectionCurrent, self.threadContext == threadID,
                      self.listRequestGenerations[.tasks] == generation else { return }
                guard let page else { self.setLoadState(.failed, lane: .tasks); return }
                var seen = Set(rows.map(\.id))
                let accumulated = rows + page.tasks.filter { (threadID == nil || $0.uiThreadId.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() == threadID) && seen.insert($0.id).inserted }
                if page.pagination?.hasMore == true, !page.tasks.isEmpty {
                    self.fetchThreadPage(threadID, offset: offset + page.tasks.count, rows: accumulated, generation: generation)
                } else {
                    self.apply(accumulated, lane: .tasks)
                    self.laneCounts = nil; self.loadedView = nil; self.tasksTotal = accumulated.count; self.tasksHasMore = false
                    self.setLoadState(.loaded, lane: .tasks)
                }
            }
        }.resume()
    }

    private func apply(_ new: [TaskV3], lane: TaskLane, append: Bool = false) {
        if lane == .internalTasks { seenInternalAgents.formUnion(new.map(\.agentId)) }
        for t in new {
            if t.status == "completed", let prev = lastStatus[t.id], prev != "completed" { grace(t.id) }
            lastStatus[t.id] = t.status
        }
        if append {
            // Load-more: append, deduped by id (a task can move between pages
            // while the pool shifts under us).
            var seen = Set((lane == .tasks ? tasks : internalTasks).map(\.id))
            let fresh = new.filter { seen.insert($0.id).inserted }
            if lane == .tasks { tasks += fresh } else { internalTasks += fresh }
        } else if lane == .tasks {
            // Carry the grace period's rows across the refetch. The server's
            // lane no longer contains them, so without this the just-ticked
            // task vanishes on the very reload its tick triggered.
            //
            // `tasks` here is the reader's WHOLE loaded span, so a graced row
            // is found wherever it was loaded: the grace period and the pages
            // under it are carried by the same refetch rather than one of them
            // outliving the other.
            let arrived = Set(new.map(\.id))
            var seen = Set<String>()
            gracedRows = (tasks + gracedRows)
                .filter { gracedIds.contains($0.id) && !arrived.contains($0.id) }
                .filter { seen.insert($0.id).inserted }
            tasks = new
        } else {
            internalTasks = new
        }
    }

    private func grace(_ id: String) {
        gracedIds.insert(id)
        graceTimers[id]?.cancel()
        let work = DispatchWorkItem { [weak self] in
            self?.gracedIds.remove(id)
            self?.gracedRows.removeAll { $0.id == id }
            self?.graceTimers[id] = nil
        }
        graceTimers[id] = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 5, execute: work)
    }

    /// Populate the agent picker (GET /v2/agents). Tolerant of `{agents:[…]}` or a
    /// bare array, and of `agent_id`/`id` keys.
    func reloadAgents() { fetchAgents() }
    private func fetchAgents() {
        guard connectionCurrent else { return }
#if DEBUG
        if ProcessInfo.processInfo.arguments.contains("--work-ui-test-fixture") || ProcessInfo.processInfo.arguments.contains("--tasks-ui-test-fixture") {
            agents = [AgentOption(id: "fixture-crew", name: "Fixture crew", isPrimary: true)]; agentsLoading = false; agentsError = nil; return
        }
#endif
        agentRequestGeneration &+= 1
        let ticket = agentRequestGeneration
        agentsLoading = true; agentsError = nil
        guard let url = URL(string: "\(origin.absoluteString)/api/magician/v2/agents") else { return }
        var request = URLRequest(url: url)
        guard authorize(&request) else { return }
        request.httpMethod = "GET"
        session.dataTask(with: request) { [weak self] data, response, error in
            var opts: [AgentOption] = []; var crew: [AgentOption] = []
            var readable = false
            let valid = (200..<300).contains((response as? HTTPURLResponse)?.statusCode ?? 0)
            if valid, let data, let json = try? JSONSerialization.jsonObject(with: data) {
                let arr = (json as? [String: Any]).flatMap { $0["agents"] as? [[String: Any]] } ?? (json as? [[String: Any]])
                readable = arr != nil
                opts = (arr ?? []).compactMap(AgentOption.decode); crew = (arr ?? []).compactMap(AgentOption.decodeCrew)
            }
            DispatchQueue.main.async {
                guard let self, self.connectionCurrent, ticket == self.agentRequestGeneration else { return }
                self.agents = opts; self.internalCrewChoices = crew; self.agentsLoading = false
                self.agentsError = valid && readable ? nil : (error?.localizedDescription ?? "Crew could not be loaded. Retry to choose a task owner.")
            }
        }.resume()
    }

    /// Create a task (POST /tasks). `agent_id` is required by the backend.
    func createTask(title: String, description: String, agentId: String, threadId: String,
                    priority: String?, dueDate: String?, tagNames: [String],
                    outputMode: String = "accumulate",
                    dependsOn: [String] = [],
                    schedule: [String: Any]? = nil,
                    completion: @escaping () -> Void) {
        var body: [String: Any] = [
            "title": title,
            "description": description,
            "agent_id": agentId,
            "ui_thread_id": threadId.isEmpty ? "general" : threadId,
            "created_by": "user",
            "output_mode": outputMode
        ]
        if let p = priority { body["priority"] = p }
        if let d = dueDate { body["due_date"] = d }
        if !tagNames.isEmpty { body["tags"] = tagNames.map { ["id": $0, "name": $0] as [String: Any] } }
        if !dependsOn.isEmpty { body["depends_on"] = dependsOn }
        if let schedule = schedule { body["schedule"] = schedule }

        mutate("POST", "/tasks", body: body, action: "create task",
               successNotice: "Task created.", onSuccess: completion)
    }

    // MARK: - Actions (web parity)

    /// Run the task now (POST /execute — plan-gated or direct per backend logic).
    func execute(_ t: TaskV3) {
        guard !t.isAppManaged else { actionErrorMessage = "This run is managed by its App. Open Apps to manage its lifecycle."; return }
        mutate("POST", "/tasks/\(WorkAPIClient.query(t.id))/execute", body: [:], taskID: t.id, action: "run task")
    }

    /// Ask the agent to draft a plan without running (POST /plan).
    func preplan(_ t: TaskV3) {
        guard !t.isAppManaged else { actionErrorMessage = "This run is managed by its App. Open Apps to manage its lifecycle."; return }
        mutate("POST", "/tasks/\(WorkAPIClient.query(t.id))/plan", body: [:], taskID: t.id, action: "plan task")
    }

    /// Force a task's status (mark complete / uncomplete / cancel / reset to ready).
    func setStatus(_ t: TaskV3, _ status: String) {
        guard !t.isAppManaged else { actionErrorMessage = "This run is managed by its App. Open Apps to manage its lifecycle."; return }
        guard status != "completed" || t.canMarkCompleteManually else { return }
        // The grace period used to start by spotting a running→completed row in
        // the next response. The server's lane no longer returns that row, so
        // the tick itself is what starts it — which is also what the web client
        // means by a pending completion: a task THIS reader just ticked.
        if status == "completed" { grace(t.id) }
        let action: String
        let notice: String
        switch status {
        case "ready":
            action = "reset task"
            notice = "Task reset to ready."
        case "completed":
            action = "complete task"
            notice = "Task marked complete."
        case "cancelled":
            action = "cancel task"
            notice = "Task cancelled."
        default:
            action = "update task"
            notice = "Task updated."
        }
        mutate(
            "PUT",
            "/tasks/\(WorkAPIClient.query(t.id))/status",
            body: ["status": status],
            taskID: t.id,
            action: action,
            successNotice: notice
        )
    }

    /// Stop a running task by cancelling its backend-declared active execution.
    /// Historical execution ids are inspection-only and must never be mutated.
    @MainActor
    func cancelExecution(_ t: TaskV3) {
        guard let exec = t.activeExecutionIdForControls else { return }
        let pathSegmentCharacters = CharacterSet.alphanumerics.union(
            CharacterSet(charactersIn: "-._~")
        )
        guard let encodedExecutionId = exec.addingPercentEncoding(
            withAllowedCharacters: pathSegmentCharacters
        ) else { return }
        let coordinator = ExecutionControlCoordinator.shared
        let source = UUID()
        guard coordinator.begin(.cancel, for: exec) else { return }
        mutateV2("/executions/\(encodedExecutionId)/cancel") {
            coordinator.finish(for: exec, invalidatedBy: source)
        }
    }

    func deleteTask(_ t: TaskV3, removeFiles: Bool = false) {
        guard !t.isAppManaged else { actionErrorMessage = "This run is managed by its App. Open Apps to manage its lifecycle."; return }
        let isInternal = ["internal", "ephemeral_owned_by_chat", "internal_debug"]
            .contains(t.lifecycle ?? "")
        let path = isInternal
            ? "/tasks/internal/\(WorkAPIClient.query(t.id))"
            : "/tasks/\(WorkAPIClient.query(t.id))?remove_files=\(removeFiles)"
        mutate("DELETE", path, body: nil, taskID: t.id, action: "delete task",
               successNotice: "Task deleted.")
    }

    func updateTitle(_ task: TaskV3, title: String) {
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty else { actionErrorMessage = "Task title is required."; return }
        updateTask(task, fields: ["title": title])
    }
    func moveTask(_ task: TaskV3, to threadID: String, completion: @escaping () -> Void = {}) {
        guard !task.isAppManaged, mutatingTaskID == nil, connectionCurrent else { return }
        guard let url = URL(string: origin.absoluteString + "/api/magician/v2/ui-threads/" + WorkAPIClient.query(threadID)) else { return }
        var request = URLRequest(url: url); guard authorize(&request) else { return }; request.timeoutInterval = 15
        mutatingTaskID = task.id; actionErrorMessage = nil
        session.dataTask(with: request) { [weak self] data, response, error in
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let root = data.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }
            let target = (root?["thread"] as? [String: Any] ?? root).flatMap(TaskThreadChoice.init)
            DispatchQueue.main.async {
                guard let self, self.connectionCurrent else { return }; self.mutatingTaskID = nil
                guard error == nil, (200..<300).contains(status), let target, target.id == threadID, !target.archived else { self.actionErrorMessage = "The destination thread is unavailable or archived. Refresh threads before moving this task."; return }
                self.mutate("PUT", "/tasks/\(WorkAPIClient.query(task.id))", body: ["ui_thread_id": target.id], taskID: task.id, action: "move task", successNotice: "Task moved.", onSuccess: completion)
            }
        }.resume()
    }

    /// Partial update (PUT /tasks/{id}) — priority / due date / tags. `nil` values
    /// clear the field (sent as JSON null).
    func updateTask(_ t: TaskV3, fields: [String: Any?]) {
        guard !t.isAppManaged else { actionErrorMessage = "This run is managed by its App. Open Apps to manage its lifecycle."; return }
        var body: [String: Any] = [:]
        for (k, v) in fields { body[k] = v ?? NSNull() }
        mutate("PUT", "/tasks/\(WorkAPIClient.query(t.id))", body: body, taskID: t.id, action: "update task",
               successNotice: "Task updated.")
    }

    func setSchedulePaused(_ task: TaskV3, paused: Bool) {
        writeSchedule(task, changes: ["paused": .bool(paused)], requireCron: true,
                      notice: paused ? "Schedule paused." : "Schedule resumed.")
    }

    /// Read the current schedule before replacing it, preserving pause, limits
    /// and unknown fields with exact integer fidelity.
    private func writeSchedule(_ task: TaskV3, changes: [String: WorkJSON], remove: Set<String> = [], requireCron: Bool = false, notice: String) {
        guard !task.isAppManaged, mutatingTaskID == nil, connectionCurrent else { return }
        mutatingTaskID = task.id; actionErrorMessage = nil
        Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                guard let url = URL(string: self.base + "/tasks/" + WorkAPIClient.query(task.id)) else { throw URLError(.badURL) }
                var request = URLRequest(url: url); guard self.authorize(&request) else { return }; request.timeoutInterval = 15
                let (data, response) = try await self.session.data(for: request)
                guard self.connectionCurrent else { return }
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                guard (200..<300).contains(status) else { throw NSError(domain: "TaskSchedule", code: status, userInfo: [NSLocalizedDescriptionKey: Self.taskMutationError(data: data, status: status, fallback: "The current schedule could not be loaded.")]) }
                let root = try JSONDecoder().decode(WorkJSON.self, from: data)
                guard let manifest = root.object?["task"]?.object?["manifest"]?.object, manifest["task_id"]?.text == task.id else { throw NSError(domain: "TaskSchedule", code: 0, userInfo: [NSLocalizedDescriptionKey: "The current task could not be loaded."]) }
                var schedule = manifest["schedule"]?.object ?? [:]
                if requireCron && schedule["kind"]?.object?["Cron"]?.object == nil { throw NSError(domain: "TaskSchedule", code: 0, userInfo: [NSLocalizedDescriptionKey: "The task no longer has a recurring schedule. Refresh before trying again."]) }
                remove.forEach { schedule[$0] = nil }; changes.forEach { schedule[$0.key] = $0.value }
                let body = try JSONEncoder().encode(WorkJSON.object(["schedule": .object(schedule)]))
                self.mutatingTaskID = nil
                self.mutate("PUT", "/tasks/" + WorkAPIClient.query(task.id), body: nil, taskID: task.id, action: "update schedule", successNotice: notice, rawBody: body)
            } catch {
                guard self.connectionCurrent else { return }; self.mutatingTaskID = nil; self.actionErrorMessage = error.localizedDescription
            }
        }
    }

    /// Update or remove a recurring task schedule using the same canonical
    /// externally-tagged Cron payload as the web Tasks workspace.
    func updateSchedule(
        _ t: TaskV3,
        cron: String,
        timezone: String,
        maxRecords: String,
        maxDays: String
    ) {
        let expression = cron.trimmingCharacters(in: .whitespacesAndNewlines)
        if expression.isEmpty {
            updateTask(t, fields: ["schedule": nil])
            return
        }
        guard expression.split(whereSeparator: { $0.isWhitespace }).count == 5 else {
            actionErrorMessage = "Schedule must be a five-field cron expression, for example 0 9 * * *."
            return
        }
        let zone = timezone.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            ? TimeZone.current.identifier
            : timezone.trimmingCharacters(in: .whitespacesAndNewlines)

        var retention: [String: Any] = [:]
        if !maxRecords.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            guard let value = Int(maxRecords), value > 0 else {
                actionErrorMessage = "Maximum saved runs must be a positive number."
                return
            }
            retention["max_records"] = value
        }
        if !maxDays.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            guard let value = Int(maxDays), value > 0 else {
                actionErrorMessage = "Maximum history days must be a positive number."
                return
            }
            retention["max_age_days"] = value
        }

        var schedule: [String: Any] = [
            "kind": ["Cron": ["expression": expression, "timezone": zone]],
            "timezone": zone
        ]
        if !retention.isEmpty { schedule["execution_history_retention"] = retention }
        guard let data = try? JSONSerialization.data(withJSONObject: schedule), let changes = try? JSONDecoder().decode(WorkJSON.self, from: data).object else { actionErrorMessage = "The schedule could not be encoded."; return }
        writeSchedule(t, changes: changes, remove: ["execution_history_retention"], notice: "Schedule saved.")
    }

    // Plan review (web parity).
    func approvePlan(_ t: TaskV3) {
        guard !t.isAppManaged else { actionErrorMessage = "This run is managed by its App. Open Apps to manage its lifecycle."; return }
        guard let p = t.latestPlanId else { return }
        mutate("POST", "/tasks/\(WorkAPIClient.query(t.id))/plan/approve?plan_id=\(p)", body: [:],
               taskID: t.id, action: "approve plan", successNotice: "Plan approved.")
    }
    func rejectPlan(_ t: TaskV3) {
        guard !t.isAppManaged else { actionErrorMessage = "This run is managed by its App. Open Apps to manage its lifecycle."; return }
        guard let p = t.latestPlanId else { return }
        mutate("POST", "/tasks/\(WorkAPIClient.query(t.id))/plan/reject?plan_id=\(p)", body: [:],
               taskID: t.id, action: "reject plan", successNotice: "Plan rejected.")
    }
    func replan(_ t: TaskV3) {
        guard !t.isAppManaged else { actionErrorMessage = "This run is managed by its App. Open Apps to manage its lifecycle."; return }
        mutate("POST", "/tasks/\(WorkAPIClient.query(t.id))/plan/replan", body: [:], taskID: t.id,
               action: "replan task", successNotice: "Replanning started.")
    }

    /// Retry output synthesis for a task whose synthesis exhausted its retries
    /// (POST /tasks/{id}/executions/{execId}/retry-synthesis) — web parity.
    func retrySynthesis(_ t: TaskV3) {
        guard !t.isAppManaged, mutatingTaskID == nil, retryingSynthesisId == nil,
              let execution = t.synthesisFailedExecutionId?.trimmingCharacters(in: .whitespacesAndNewlines), !execution.isEmpty else { return }
        retryingSynthesisId = t.id
        mutate("POST", "/tasks/\(WorkAPIClient.query(t.id))/executions/\(WorkAPIClient.query(execution))/retry-synthesis", body: nil, taskID: t.id, action: "retry output synthesis", successNotice: "Retrying output synthesis.")
    }

    // Inline tag editing (web parity).
    func addTag(_ t: TaskV3, name: String) {
        let clean = name.trimmingCharacters(in: .whitespaces)
        guard !clean.isEmpty, !t.tags.contains(where: { $0.name == clean }) else { return }
        let tags = t.tags.map { ["id": $0.id, "name": $0.name] as [String: Any] } + [["id": clean, "name": clean]]
        updateTask(t, fields: ["tags": tags])
    }
    func removeTag(_ t: TaskV3, name: String) {
        let tags = t.tags.filter { $0.name != name }.map { ["id": $0.id, "name": $0.name] as [String: Any] }
        updateTask(t, fields: ["tags": tags])
    }

    /// An ISO `yyyy-MM-dd` string `days` from today (for the due-date shortcuts).
    func isoDate(offset days: Int) -> String {
        let d = Calendar(identifier: .gregorian).date(byAdding: .day, value: days, to: Date()) ?? Date()
        return Self.localDateISO(d, in: .current)
    }

    private func mutate(
        _ method: String,
        _ path: String,
        body: [String: Any]?,
        taskID: String? = nil,
        action: String = "update task",
        successNotice: String? = nil,
        rawBody: Data? = nil,
        onSuccess: (() -> Void)? = nil
    ) {
        guard mutatingTaskID == nil else {
            actionErrorMessage = "Another task action is still finishing. Try again in a moment."
            return
        }
        guard let url = URL(string: base + path) else { return }
        mutatingTaskID = taskID ?? path
        actionErrorMessage = nil
        actionNoticeMessage = nil
        var request = URLRequest(url: url)
        guard authorize(&request) else { return }
        request.httpMethod = method
        if let rawBody {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = rawBody
        } else if let body = body {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try? JSONSerialization.data(withJSONObject: body)
        }
        session.dataTask(with: request) { [weak self] data, response, error in
            DispatchQueue.main.async {
                guard let self, self.connectionCurrent else { return }
                self.mutatingTaskID = nil
                if self.retryingSynthesisId == taskID { self.retryingSynthesisId = nil }
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                guard error == nil, (200..<300).contains(status) else {
                    self.actionErrorMessage = Self.taskMutationError(
                        data: data,
                        status: status,
                        fallback: "Could not \(action)."
                    )
                    self.load()
                    return
                }
                self.actionNoticeMessage = successNotice
                self.load()
                onSuccess?()
                // A second refresh catches async status transitions (running →
                // completed) until realtime WS updates land in a later slice.
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.6) { [weak self] in self?.load() }
                if successNotice != nil {
                    DispatchQueue.main.asyncAfter(deadline: .now() + 2.6) { [weak self] in
                        if self?.actionNoticeMessage == successNotice {
                            self?.actionNoticeMessage = nil
                        }
                    }
                }
            }
        }.resume()
    }

    private static func taskMutationError(data: Data?, status: Int, fallback: String) -> String {
        if let data, !data.isEmpty {
            if let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                if let message = (object["error"] ?? object["message"] ?? object["detail"]) as? String,
                   !message.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    return message
                }
            }
            if let rawText = String(data: data, encoding: .utf8) {
                let text = rawText.trimmingCharacters(in: .whitespacesAndNewlines)
                if !text.isEmpty { return text }
            }
        }
        return status > 0 ? "\(fallback) (HTTP \(status))" : fallback
    }

    private func mutateV2(_ path: String, completion: @escaping () -> Void) {
        guard let url = URL(string: "\(origin.absoluteString)/api/magician/v2" + path) else {
            DispatchQueue.main.async(execute: completion)
            return
        }
        var request = URLRequest(url: url)
        guard authorize(&request) else { return }
        request.httpMethod = "POST"
        session.dataTask(with: request) { [weak self] _, _, _ in
            DispatchQueue.main.async {
                guard self?.connectionCurrent == true else { return }
                completion()
                self?.load()
            }
        }.resume()
    }

    // MARK: - Realtime (web parity: live list on task events)

    private var realtimeSubscription: RealtimeHubSubscription?
    private var reloadWork: DispatchWorkItem?

    /// Subscribe to the app's realtime hub; reload (debounced) on task /
    /// planning / execution events so the list stays live, and reload at once
    /// when the hub reports that events may have been missed.
    func connectRealtime() {
        guard !isRunningUnderTests else { return }   // no realtime in unit tests
        guard realtimeSubscription == nil else { return }
        realtimeSubscription = RealtimeHub.shared.subscribe(
            onEvent: { [weak self] event in
                guard let self, self.isTaskEvent(event.text) else { return }
                self.scheduleReload()
            },
            onResync: { [weak self] in self?.load() }
        )
    }

    func disconnectRealtime() {
        realtimeSubscription?.cancel()
        realtimeSubscription = nil
    }

    func isTaskEvent(_ text: String) -> Bool {
        guard let data = text.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let type = obj["event_type"] as? String else { return false }
        return type.contains("Task") || type.contains("Planning") || type.contains("Execution")
    }

    private func scheduleReload() {
        reloadWork?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.load() }
        reloadWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5, execute: work)
    }

    private func fetch(path: String, lane: TaskLane, assign: @escaping (TaskListResponse) -> Void) {
        guard let url = URL(string: base + path) else {
            setLoadState(.failed, lane: lane)
            return
        }
        let generation = (listRequestGenerations[lane] ?? 0) &+ 1
        listRequestGenerations[lane] = generation
        setLoadState(.loading, lane: lane)
        var request = URLRequest(url: url)
        guard authorize(&request) else { return }
        request.httpMethod = "GET"
        session.dataTask(with: request) { [weak self] data, response, error in
            let statusOK = (response as? HTTPURLResponse).map { (200..<300).contains($0.statusCode) } == true
            let list = error == nil && statusOK
                ? data.flatMap { try? JSONDecoder().decode(TaskListResponse.self, from: $0) }
                : nil
            DispatchQueue.main.async {
                guard let self, self.connectionCurrent, self.listRequestGenerations[lane] == generation else { return }
                if let list {
                    assign(list)
                    self.setLoadState(.loaded, lane: lane)
                } else {
                    self.setLoadState(.failed, lane: lane)
                }
            }
        }.resume()
    }

    private func setLoadState(_ state: TaskListLoadState, lane: TaskLane) {
        switch lane {
        case .tasks: taskLoadState = state
        case .internalTasks: internalTaskLoadState = state
        case .monitors: break
        }
    }
}
