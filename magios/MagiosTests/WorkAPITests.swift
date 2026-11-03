import XCTest
@testable import Magician

private final class WorkMockURLProtocol: URLProtocol {
    static var handle: ((URLRequest) throws -> (Int, String, [String: String]))?
    static var stream: ((WorkMockURLProtocol) -> Void)?
    static var onStop: (() -> Void)?
    static func body(_ request: URLRequest) -> Data {
        if let data = request.httpBody { return data }
        guard let stream = request.httpBodyStream else { return Data() }
        stream.open(); defer { stream.close() }
        var data = Data(); var buffer = [UInt8](repeating: 0, count: 4096)
        while true {
            let count = stream.read(&buffer, maxLength: buffer.count)
            guard count > 0 else { break }
            data.append(contentsOf: buffer.prefix(count))
        }
        return data
    }
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        if let stream = Self.stream { stream(self); return }
        do {
            // A request that lands after tearDown (a model from the previous
            // test still loading) fails instead of trapping on a nil handler.
            guard let handle = Self.handle else { throw URLError(.cancelled) }
            let (status, body, headers) = try handle(request)
            let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: headers)!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: Data(body.utf8))
            client?.urlProtocolDidFinishLoading(self)
        } catch { client?.urlProtocol(self, didFailWithError: error) }
    }
    override func stopLoading() { Self.onStop?() }
}

/// Polls until `condition` is false, for up to `timeout`. The old fixed budget
/// (100 polls of 2 ms) ran out on a loaded host and asserted mid-load.
@MainActor
private func waitWhile(timeout: TimeInterval = 5, _ condition: () -> Bool) async throws {
    let deadline = Date().addingTimeInterval(timeout)
    while condition(), Date() < deadline { try await Task.sleep(nanoseconds: 2_000_000) }
}

final class WorkAPITests: XCTestCase {
    @MainActor
    func testChatOutputActionCannotSwitchConnectionsAfterOpeningItsContext() async throws {
        var current = true, requests = 0
        WorkMockURLProtocol.handle = { _ in requests += 1; return (200, "{}", [:]) }
        let context = ChatContextSheet(sessionID: "chat-a", threadID: "thread-a", page: .files, client: client(currentCheck: { current }))
        current = false
        do {
            try await ChatSessionAccounting.openOnHost("Pages/report.md", session: context.sessionID, client: context.client!)
            XCTFail("A stale chat context must not open a file on another connection")
        } catch { XCTAssertEqual((error as? URLError)?.code, .cancelled) }
        XCTAssertEqual(requests, 0)
    }

    @MainActor
    func testPendingModeSaveRetainsItsConfirmedConnection() async throws {
        let confirmed = try MobileConnectionProfile(publicOrigin: URL(string: "https://confirmed.test")!,
            principal: "owner", workspace: "private", deviceID: "device-a", deviceToken: "owner-a")
        var active = confirmed
        let api = ExecutionModeAPI(session: mockSession(), connection: { active })
        let pending = api.frozen()
        active = try MobileConnectionProfile(publicOrigin: URL(string: "https://other.test")!,
            principal: "other", workspace: "other", deviceID: "device-b", deviceToken: "owner-b")
        WorkMockURLProtocol.handle = { request in
            XCTAssertEqual(request.url?.host, "confirmed.test")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer owner-a")
            return (200, #"{"active":true,"enabled":true,"until_ms":null,"remaining_secs":null}"#, [:])
        }
        _ = try await pending.saveSuper(.hour)
    }

    @MainActor
    func testComposerModesKeepChatOverridesSeparateFromBackendSuperMode() async throws {
        let profile = try MobileConnectionProfile(publicOrigin: URL(string: "https://modes.test")!,
            principal: "owner", workspace: "private", deviceID: "device", deviceToken: "test-owner")
        var writes: [[String: Any]] = []
        WorkMockURLProtocol.handle = { request in
            XCTAssertEqual(request.httpMethod, "PUT")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer test-owner")
            XCTAssertNil(URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.query)
            let body = try JSONSerialization.jsonObject(with: WorkMockURLProtocol.body(request)) as! [String: Any]
            writes.append(body)
            switch request.url!.path {
            case "/api/magician/v2/chat/sessions/chat-1/god-mode":
                return (200, #"{"enabled":true,"override":null,"default":true}"#, [:])
            case "/api/magician/v2/settings/super-god-mode":
                return (200, #"{"active":false,"enabled":false,"until_ms":null,"remaining_secs":null}"#, [:])
            default: XCTFail("Mode settings must not launch work"); return (404, "{}", [:])
            }
        }
        let api = ExecutionModeAPI(session: mockSession(), connection: { profile })
        _ = try await api.saveChat("chat-1", enabled: true)
        _ = try await api.saveChat("chat-1", enabled: nil)
        _ = try await api.saveSuper(.hour)
        _ = try await api.saveSuper(.untilOff)
        _ = try await api.saveSuper(nil)
        XCTAssertEqual(writes[0]["enabled"] as? Bool, true)
        XCTAssertTrue(writes[1]["enabled"] is NSNull)
        XCTAssertEqual(writes[2]["duration"] as? String, "1h")
        XCTAssertEqual(writes[3]["duration"] as? String, "forever")
        XCTAssertEqual(writes[4]["enabled"] as? Bool, false)
        WorkMockURLProtocol.handle = { _ in (403, #"{"message":"Owner authentication required"}"#, [:]) }
        do { _ = try await api.saveSuper(.hour); XCTFail("A refused mode must not appear enabled") }
        catch { XCTAssertTrue(error.localizedDescription.contains("Owner authentication required")) }
    }

    @MainActor
    func testSchedulePauseAndEditPreserveCurrentLimitsAndExactUnknownFields() async throws {
        var schedule = try JSONDecoder().decode(WorkJSON.self, from: Data(#"{"kind":{"Cron":{"expression":"0 9 * * *","timezone":"UTC"}},"timezone":"UTC","max_runs":3,"future_limit":18446744073709551615}"#.utf8))
        var writes: [WorkJSON] = []; let lock = NSLock()
        WorkMockURLProtocol.handle = { request in
            lock.lock(); defer { lock.unlock() }
            if request.httpMethod == "PUT" {
                let body = try JSONDecoder().decode(WorkJSON.self, from: WorkMockURLProtocol.body(request)); writes.append(body); schedule = body.object!["schedule"]!
                return (200, "{}", [:])
            }
            if request.url!.path.hasSuffix("/tasks/task_scheduled") {
                let body = WorkJSON.object(["task": .object(["manifest": .object(["task_id": .string("task_scheduled"), "schedule": schedule])])])
                return (200, String(data: try JSONEncoder().encode(body), encoding: .utf8)!, [:])
            }
            return (200, request.url!.path.hasSuffix("/agents") ? #"{"agents":[]}"# : #"{"tasks":[]}"#, [:])
        }
        let task = try JSONDecoder().decode(TaskV3.self, from: Data(#"{"id":"task_scheduled","title":"Scheduled","status":"pending","schedule":{"kind":{"Cron":{"expression":"0 9 * * *","timezone":"UTC"}}}}"#.utf8))
        let model = TasksViewModel(session: mockSession()); model.setSchedulePaused(task, paused: true)
        for _ in 0..<500 where model.mutatingTaskID != nil { try await Task.sleep(nanoseconds: 2_000_000) }
        XCTAssertNil(model.actionErrorMessage); XCTAssertEqual(writes.count, 1)
        XCTAssertEqual(schedule.object?["paused"], .bool(true)); XCTAssertEqual(schedule.object?["future_limit"], .unsigned(UInt64.max))
        model.updateSchedule(task, cron: "0 10 * * *", timezone: "UTC", maxRecords: "10", maxDays: "")
        for _ in 0..<500 where model.mutatingTaskID != nil { try await Task.sleep(nanoseconds: 2_000_000) }
        XCTAssertNil(model.actionErrorMessage); XCTAssertEqual(writes.count, 2)
        XCTAssertEqual(schedule.object?["paused"], .bool(true)); XCTAssertEqual(schedule.object?["max_runs"], .integer(3)); XCTAssertEqual(schedule.object?["future_limit"], .unsigned(UInt64.max))
        XCTAssertEqual(schedule.object?["kind"]?.object?["Cron"]?.object?["expression"], .string("0 10 * * *"))
        XCTAssertEqual(writes.last?.object?.count, 1)
    }
    @MainActor
    func testTaskTitleAndThreadMoveValidateDestinationAndUpdateOnlyRequestedFields() async throws {
        var writes: [[String: Any]] = []; var reads: [String] = []; let lock = NSLock()
        WorkMockURLProtocol.handle = { request in
            lock.lock(); defer { lock.unlock() }
            if request.httpMethod == "PUT" { writes.append(try JSONSerialization.jsonObject(with: WorkMockURLProtocol.body(request)) as! [String: Any]); return (200, "{}", [:]) }
            let path = request.url!.path; reads.append(path)
            if path.contains("/ui-threads/") { let archived = path.hasSuffix("/archived"); return (200, archived ? #"{"thread":{"id":"archived","name":"Archived","archived":true}}"# : #"{"thread":{"id":"launch","name":"Launch","archived":false}}"#, [:]) }
            return (200, path.hasSuffix("/agents") ? #"{"agents":[]}"# : #"{"tasks":[]}"#, [:])
        }
        let task = try JSONDecoder().decode(TaskV3.self, from: Data(#"{"id":"task_one","title":"Original","status":"pending","ui_thread_id":"general"}"#.utf8))
        let model = TasksViewModel(session: mockSession()); model.updateTitle(task, title: "  Renamed  ")
        for _ in 0..<500 where model.mutatingTaskID != nil { try await Task.sleep(nanoseconds: 2_000_000) }
        XCTAssertNil(model.actionErrorMessage)
        XCTAssertEqual(writes.first?["title"] as? String, "Renamed"); XCTAssertEqual(writes.first?.count, 1)
        model.moveTask(task, to: "archived")
        for _ in 0..<500 where model.mutatingTaskID != nil { try await Task.sleep(nanoseconds: 2_000_000) }
        XCTAssertEqual(writes.count, 1); XCTAssertNotNil(model.actionErrorMessage)
        var completed = false; model.moveTask(task, to: "launch") { completed = true }
        for _ in 0..<500 where model.mutatingTaskID != nil { try await Task.sleep(nanoseconds: 2_000_000) }
        XCTAssertTrue(completed); XCTAssertEqual(writes.count, 2); XCTAssertEqual(writes.last?["ui_thread_id"] as? String, "launch"); XCTAssertEqual(writes.last?.count, 1)
        XCTAssertTrue(reads.contains { $0.hasSuffix("/ui-threads/launch") })
    }
    @MainActor
    func testOrdinarySearchAndTagWalkBeyondLoadedPagesAndSynthesisReportsRejection() async throws {
        var offsets: [String] = []; let lock = NSLock()
        WorkMockURLProtocol.handle = { request in
            lock.lock(); defer { lock.unlock() }
            if request.httpMethod == "POST" { return (409, #"{"error":"synthesis busy"}"#, [:]) }
            if request.url!.path.hasSuffix("/agents") { return (200, #"{"agents":[]}"#, [:]) }
            if request.url!.path.hasSuffix("/tasks/internal") { return (200, #"{"tasks":[]}"#, [:]) }
            let offset = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "offset" }?.value ?? "0"; offsets.append(offset)
            return (200, offset == "1" ? #"{"tasks":[{"id":"task_tagged","title":"Beyond loaded page","status":"pending","tags":[{"id":"only-tag","name":"only-tag"}]}],"pagination":{"total":2,"limit":500,"offset":1,"has_more":false}}"# : #"{"tasks":[{"id":"task_first","title":"First","status":"pending"}],"pagination":{"total":2,"limit":500,"offset":0,"has_more":true}}"#, [:])
        }
        let model = TasksViewModel(session: mockSession()); model.searchQuery = "only-tag"
        try await waitWhile { model.taskLoadState != .loaded }
        XCTAssertEqual(model.visibleTasks.map(\.id), ["task_tagged"]); XCTAssertTrue(offsets.contains("1")); XCTAssertFalse(model.tasksHasMore)
        model.searchQuery = ""; model.toggleTag("only-tag")
        try await waitWhile { model.taskLoadState != .loaded }
        XCTAssertEqual(model.visibleTasks.map(\.id), ["task_tagged"])
        let task = try JSONDecoder().decode(TaskV3.self, from: Data(#"{"id":"task_failed","title":"Failed","status":"failed","synthesis_failed_execution_id":"run/+one"}"#.utf8))
        model.retrySynthesis(task)
        try await waitWhile { model.mutatingTaskID != nil }
        XCTAssertEqual(model.actionErrorMessage, "synthesis busy"); XCTAssertNil(model.retryingSynthesisId)
    }
    @MainActor
    func testActualEnrichedHistoryPagesSurviveRunSelectionAndKeepAppSchedule() async throws {
        var paths: [String] = []
        let appID = "task_app_" + String(repeating: "a", count: 64)
        let model = TaskDetailScreenModel(seed: TaskDetailRoute(taskId: appID).seed) { path in
            paths.append(path)
            if path.contains("/details") {
                if path.contains("cursor=") { return ["executions": [["state": ["execution_id": "older", "status": "completed"], "refs": [:], "artifacts": []]]] }
                return ["task": ["manifest": ["ui_thread_id": "app:install.one", "created_by": "app_action", "lifecycle": "internal"]], "executions": [["state": ["execution_id": "latest", "status": "completed"], "refs": [:], "artifacts": []]], "next_execution_cursor": "cursor/+&", "recurring_schedule": ["behavior_id": "behavior", "interval_seconds": 60]]
            }
            return [:]
        }
        await model.refresh(); XCTAssertTrue(model.historyHasMore); XCTAssertEqual(model.appInstallationID, "install.one")
        await model.loadOlderExecutions(); XCTAssertEqual(model.snapshot?.history.map(\.id).sorted(), ["latest", "older"]); XCTAssertFalse(model.historyHasMore)
        XCTAssertTrue(paths.contains { $0.hasSuffix("details?cursor=cursor%2F%2B%26") }); XCTAssertEqual(model.recurringSchedule?["interval_seconds"] as? Int, 60)
        await model.refresh(); XCTAssertEqual(model.snapshot?.history.count, 2); XCTAssertFalse(model.historyHasMore)
    }
    @MainActor
    func testInternalCorpusReferencesAndPartialCleanupUseRealScopedEndpoints() async throws {
        var deletes: [String] = []
        var paths: [URL] = []; let lock = NSLock()
        let appID = "task_app_" + String(repeating: "a", count: 64)
        WorkMockURLProtocol.handle = { request in
            lock.lock(); defer { lock.unlock() }; let url = request.url!; paths.append(url)
            if request.httpMethod == "DELETE" { deletes.append(url.lastPathComponent); return (url.lastPathComponent == "bad" ? 409 : 200, url.lastPathComponent == "bad" ? #"{"error":"record busy"}"# : "{}", [:]) }
            if url.path.hasSuffix("/agents") { return (200, #"{"agents":[]}"#, [:]) }
            if url.path.hasSuffix("/tasks/internal") { return (200, "{\"tasks\":[{\"id\":\"good\",\"title\":\"Good\",\"status\":\"completed\",\"agent_id\":\"crew/+one\",\"lifecycle\":\"internal\"},{\"id\":\"bad\",\"title\":\"Bad\",\"status\":\"completed\",\"agent_id\":\"crew/+one\",\"lifecycle\":\"internal\"},{\"id\":\"\(appID)\",\"title\":\"App\",\"status\":\"completed\",\"lifecycle\":\"internal\"}]}", [:]) }
            let offset = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first(where: { $0.name == "offset" })?.value
            return (200, offset == "1" ? #"{"tasks":[{"id":"task_two","title":"Two","status":"completed"}],"pagination":{"total":2,"limit":50,"offset":1,"has_more":false}}"# : #"{"tasks":[{"id":"task_one","title":"One","status":"completed"}],"pagination":{"total":2,"limit":50,"offset":0,"has_more":true}}"#, [:])
        }
        let model = TasksViewModel(session: mockSession()); model.lane = .internalTasks
        model.internalAgentFilter = "crew/+one"; model.internalStatusFilter = "completed"; model.searchQuery = "notes & quotes"; model.setSort(field: .agent); model.internalPageSize = 100
        try await waitWhile { model.internalTaskLoadState != .loaded }
        XCTAssertEqual(model.internalTasks.count, 3)
        let query = URLComponents(url: paths.last(where: { $0.path.hasSuffix("/tasks/internal") })!, resolvingAgainstBaseURL: false)!.queryItems!
        XCTAssertEqual(query.first { $0.name == "agent_id" }?.value, "crew/+one"); XCTAssertEqual(query.first { $0.name == "query" }?.value, "notes & quotes"); XCTAssertEqual(query.first { $0.name == "sort" }?.value, "agent_id"); XCTAssertEqual(query.first { $0.name == "limit" }?.value, "100")
        model.searchQuery = ""
        try await waitWhile { model.internalTaskLoadState != .loaded }
        model.selectLoadedInternal(); XCTAssertEqual(model.selectedInternalIDs, ["good", "bad"])
        model.deleteSelectedInternal()
        try await waitWhile { model.mutatingTaskID != nil }
        XCTAssertEqual(deletes, ["good", "bad"]); XCTAssertEqual(model.selectedInternalIDs, ["bad"]); XCTAssertEqual(model.cleanupFailures["bad"], "record busy")
        model.loadReferences()
        try await waitWhile { model.referencesLoading }
        model.loadReferences(append: true)
        try await waitWhile { model.referencesLoading }
        XCTAssertEqual(model.completedReferences.map(\.id), ["task_one", "task_two"]); XCTAssertFalse(model.referencesHasMore)
    }
    @MainActor
    func testTaskReferencesAppProvenanceAndHistoryRetainExactContext() {
        XCTAssertEqual(TaskReferenceComposer.description("Use task:task_one_long", ids: ["task_one", "task_two"]), "Use task:task_one_long\n\ntask:task_one task:task_two")
        XCTAssertEqual(TaskReferenceComposer.dependencies([" manual ", "task_one"], references: ["task_one", "task_two"]), ["manual", "task_one", "task_two"])
        let id = "task_app_" + String(repeating: "a", count: 64)
        XCTAssertEqual(TaskWorkOwnership.appInstallation(taskID: id, threadID: "app:install.one", createdBy: "app_action"), "install.one")
        XCTAssertNil(TaskWorkOwnership.appInstallation(taskID: id, threadID: "app:../escape", createdBy: "app_action"))
        XCTAssertNil(TaskDetailActionRequest.make(.reset, taskId: id, planId: nil, synthesisExecutionId: nil, lifecycle: "internal"))
        XCTAssertNil(TaskDetailActionRequest.make(.delete, taskId: id, planId: nil, synthesisExecutionId: nil, lifecycle: "internal"))
        XCTAssertNil(TaskDetailActionRequest.make(.retrySynthesis, taskId: id, planId: nil, synthesisExecutionId: "run", lifecycle: "internal"))
        let url = WorkWebContext(task: id, execution: "run/+", internalWork: true, appInstallation: "install.one").url(base: URL(string: "https://console.example")!, destination: .apps)!
        XCTAssertEqual(url.path, "/apps/install.one"); XCTAssertTrue(url.absoluteString.contains("from="))
        let merged = TaskDetailScreenModel.mergeHistory(["executions": [["execution_id": "one"], ["execution_id": "two"]]], ["executions": [["execution_id": "two"], ["execution_id": "three"]]])
        XCTAssertEqual((merged["executions"] as? [[String: Any]])?.count, 3); XCTAssertNil(merged["next_execution_cursor"])
    }

    func testGuidedCrewEditPreservesNestedFieldsAndLargeBudgets() throws {
        let original = try WorkJSON.parseObject(#"{"agent_id":"writer","name":"Writer","persona":"Help","trust_level":"local","kind":"Personal","budget":18446744073709551615,"harness":{"program_section":"Old","provider":{"mode":"keep"}},"autonomous_config":{"schedule":"0 9 * * *","future":true,"focus_areas":[{"name":"Launch","description":"Verify","priority":"high","evidence":{"revision":18446744073709551615}}]}}"#)
        let changed = original.setting(["harness", "program_section"], to: .string("Engineering"))
        XCTAssertEqual(changed["budget"], .unsigned(UInt64.max))
        XCTAssertEqual(changed.at(["harness", "provider"]), original.at(["harness", "provider"]))
        XCTAssertEqual(changed["autonomous_config"], original["autonomous_config"])
        XCTAssertNil(changed.crewValidation(editingID: "writer"))
        XCTAssertNotNil(changed.crewValidation(editingID: "other"))
        XCTAssertEqual(try WorkJSON.parseObject(changed.formatted), changed)
        let removed = changed.setting(["harness", "provider"], to: nil).mergePatch(from: original)
        XCTAssertEqual(removed.at(["harness", "provider"]), .null)
        XCTAssertEqual(removed.at(["harness", "program_section"]), .string("Engineering"))
        XCTAssertNil(removed.object?["budget"])
    }
    func testGuidedCrewCreationRequiresUsableIdentityAndCycles() {
        XCTAssertNotNil(WorkJSON.newCrewDefinition.crewValidation())
        let ready = WorkJSON.newCrewDefinition.setting(["name"], to: .string("Writer"))
        XCTAssertNil(ready.crewValidation())
        XCTAssertNotNil(ready.setting(["agent_id"], to: .string("Bad ID")).crewValidation())
        XCTAssertNotNil(ready.setting(["constraints", "coordination", "max_delegation_depth"], to: .integer(50)).crewValidation())
        XCTAssertNotNil(ready.setting(["autonomous_config"], to: .object([:])).crewValidation())
        // The server's definition has no top-level `coordination` and stores
        // `kind` lowercase; drafts must match it to save.
        XCTAssertNil(WorkJSON.newCrewDefinition["coordination"].object)
        XCTAssertEqual(WorkJSON.newCrewDefinition["kind"].text, "worker")
    }
    func testCrewKindIsReadCaseInsensitively() throws {
        let stored = try WorkJSON.parseObject(#"{"agent_id":"writer","name":"Writer","persona":"Help","trust_level":"local","kind":"personal"}"#)
        XCTAssertTrue(stored.isPersonalCrew)
        XCTAssertNil(stored.crewValidation())
        XCTAssertTrue(try WorkJSON.parseObject(#"{"kind":"Personal"}"#).isPersonalCrew)
        XCTAssertFalse(try WorkJSON.parseObject(#"{"kind":"worker"}"#).isPersonalCrew)
    }

    func testRetiredTaskPathsKeepExactRecordExecutionAndThread() {
        let task = WorkNavigationTarget.parse("/tasks/task%2Fone+two?selected=wrong&type=internal&execution_id=exec%2Fone&plan=open")
        XCTAssertEqual(task?.kind, .task)
        XCTAssertEqual(task?.recordID, "task/one+two")
        XCTAssertEqual(task?.executionID, "exec/one")
        XCTAssertEqual(task?.internalWork, true)
        XCTAssertEqual(task?.plan, true)
        XCTAssertEqual(WorkNavigationTarget.parse("https://magican.example/tasks/task%252Fone")?.recordID, "task%2Fone")
        for source in ["/t/Launch/tasks/task-one?selected=wrong&execution=run-one", "/t/Launch/tasks?selected=task-one&execution=run-one"] {
            let target = WorkNavigationTarget.parse(source)
            XCTAssertEqual(target?.kind, .task)
            XCTAssertEqual(target?.recordID, "task-one")
            XCTAssertEqual(target?.executionID, "run-one")
            XCTAssertEqual(target?.threadID, "launch")
        }
        XCTAssertNil(WorkNavigationTarget.parse("/api/magician/v3/tasks/task-one"))
        XCTAssertNil(WorkNavigationTarget.parse("/tasks/task-one/outputs/report.pdf"))
        XCTAssertNil(WorkNavigationTarget.parse("javascript:/tasks/task-one"))
    }

    func testWorkLinksKeepEncodedMemberAndExactExecutionContext() {
        let memory = WorkNavigationTarget.parse("/work/crew/member%2Fone/memory/goal%2Ftier?goal_id=goal%2Fone")
        XCTAssertEqual(memory?.kind, .memory); XCTAssertEqual(memory?.recordID, "member/one")
        XCTAssertEqual(memory?.tier, "goal/tier"); XCTAssertEqual(memory?.goalID, "goal/one")
        let task = WorkNavigationTarget.parse("/work?view=running&type=internal&selected=task%2Fone&execution_id=run%2Ftwo&plan=open&thread=launch")
        XCTAssertEqual(task?.kind, .task); XCTAssertEqual(task?.executionID, "run/two")
        XCTAssertEqual(task?.recordID, "task/one"); XCTAssertEqual(task?.internalWork, true)
        XCTAssertEqual(task?.plan, true); XCTAssertEqual(task?.threadID, "launch")
        XCTAssertEqual(WorkNavigationTarget.parse("/crew/new?edit=writer")?.kind, .editMember)
        XCTAssertEqual(WorkNavigationTarget.parse("magican://work?view=quality&run=run-1")?.kind, .qualityRun)
        XCTAssertEqual(WorkNavigationTarget.parse("/work?view=quality&campaign=capture%2Fone")?.kind, .qualityCampaign)
        XCTAssertEqual(WorkNavigationTarget.parse("/work?view=quality&campaign=capture%2Fone")?.recordID, "capture/one")
        XCTAssertEqual(WorkNavigationTarget.parse("/t/launch/tasks")?.kind, .thread)
        XCTAssertNil(WorkNavigationTarget.parse("/work?view=unknown&selected=one"))
        XCTAssertNil(WorkNavigationTarget.parse("javascript:/work?view=crew"))
    }

    @MainActor
    func testSimpleTaskOwnerUsesWrappedEnabledPersonalCrewAndRefreshesPrimary() async throws {
        XCTAssertEqual(AgentOption.decodeCrew(["definition": ["agent_id": "worker", "kind": "Worker"]])?.id, "worker")
        XCTAssertNil(AgentOption.decode(["definition": ["agent_id": "worker", "kind": "Worker"]]))
        XCTAssertNil(AgentOption.decode(["definition": ["agent_id": "disabled", "kind": "Personal", "disabled": true]]))
        XCTAssertEqual(AgentOption.decode(["agent_id": "legacy", "name": "Legacy"])?.id, "legacy")
        WorkMockURLProtocol.handle = { request in
            if request.url?.path.hasSuffix("/agents") == true { return (200, #"{"agents":[{"definition":{"agent_id":"custom-owner","name":"My primary","kind":"Personal","is_primary":true}},{"definition":{"agent_id":"worker","kind":"Worker"}}]}"#, [:]) }
            return (200, #"{"tasks":[]}"#, [:])
        }
        let model = TasksViewModel(session: mockSession()); model.reloadAgents()
        try await waitWhile { model.agentsLoading }
        XCTAssertEqual(model.agents.map(\.id), ["custom-owner"]); XCTAssertEqual(model.agents.first?.isPrimary, true)
        WorkMockURLProtocol.handle = { _ in (200, #"{"agents":[]}"#, [:]) }
        model.reloadAgents()
        try await waitWhile { model.agentsLoading }
        XCTAssertTrue(model.agents.isEmpty); XCTAssertNil(model.agentsError)
        WorkMockURLProtocol.handle = { _ in (503, #"{"error":"offline"}"#, [:]) }
        model.reloadAgents()
        try await waitWhile { model.agentsLoading }
        XCTAssertTrue(model.agents.isEmpty); XCTAssertNotNil(model.agentsError)
    }

    @MainActor
    func testProgramRosterBoundsConcurrentOverviewRequestsAndKeepsEveryResult() async throws {
        final class Activity: @unchecked Sendable {
            private let lock = NSLock()
            private var active = 0
            private var maximum = 0
            private var count = 0
            func start() { lock.lock(); defer { lock.unlock() }; active += 1; count += 1; maximum = max(maximum, active) }
            func end() { lock.lock(); defer { lock.unlock() }; active -= 1 }
            func read() -> (Int, Int) { lock.lock(); defer { lock.unlock() }; return (maximum, count) }
        }
        let activity = Activity()
        WorkMockURLProtocol.stream = { protocolInstance in
            activity.start()
            DispatchQueue.global().asyncAfter(deadline: .now() + 0.025) {
                let response = HTTPURLResponse(url: protocolInstance.request.url!, statusCode: 200, httpVersion: nil, headerFields: [:])!
                protocolInstance.client?.urlProtocol(protocolInstance, didReceive: response, cacheStoragePolicy: .notAllowed)
                protocolInstance.client?.urlProtocol(protocolInstance, didLoad: Data(#"{"context":{"program":{"relative_path":"program.md"}}}"#.utf8))
                activity.end()
                protocolInstance.client?.urlProtocolDidFinishLoading(protocolInstance)
            }
        }
        let resources = (0..<12).map { _ in WorkResource(clientFactory: { self.client(currentCheck: { true }) }) }
        for (index, resource) in resources.enumerated() { resource.load("/api/magician/v2/agents/member-\(index)/harness-overview") }
        for _ in 0..<500 where resources.contains(where: { $0.loading }) { try await Task.sleep(nanoseconds: 2_000_000) }
        XCTAssertTrue(resources.allSatisfy { $0.value?["context"]["program"]["relative_path"].text == "program.md" })
        XCTAssertEqual(activity.read().0, 4); XCTAssertEqual(activity.read().1, 12)
    }

    @MainActor
    func testCrewCycleUsesOnlyExactExecutionPanelAndCannotMutateATask() async throws {
        let value = try WorkJSON.parseObject(#"{"definition":{"agent_id":"writer"},"current_cycle_id":"cycle-1","current_execution_id":"run/+&one"}"#)
        let member = try WorkCrewMember(value)
        XCTAssertEqual(member.cycleTaskID, "agent-cycle:writer:cycle-1")
        XCTAssertNil(try WorkCrewMember(value.setting(["current_execution_id"], to: nil)).cycleTaskID)
        var requests: [String] = []
        let route = TaskDetailRoute(taskId: member.cycleTaskID!, executionId: member.executionID, inspectRun: true)
        let model = TaskDetailScreenModel(seed: route.seed, executionId: route.executionId) { path in
            requests.append(path)
            return ["overview": ["status": "running", "title": "Writer cycle", "execution_id": "run/+&one", "has_plan": true],
                    "run": ["summary": "Actual cycle activity"], "output": ["result": ["summary": "Cycle evidence"]],
                    "debug": ["selected_execution": ["execution_id": "run/+&one"]]]
        }
        await model.refresh()
        await model.pollPanel()
        XCTAssertEqual(requests, Array(repeating: "/executions/run%2F%2B%26one/execution-panel", count: 2))
        let snapshot = try XCTUnwrap(model.snapshot)
        XCTAssertEqual(snapshot.activeRootExecutionId, "run/+&one")
        XCTAssertTrue(snapshot.isExecutionOnly); XCTAssertFalse(snapshot.hasPlan); XCTAssertFalse(snapshot.hasOutputAct)
        XCTAssertTrue(TaskDetailMenu.actions(snapshot).isEmpty)
        XCTAssertNil(TaskDetailActionRequest.make(.delete, taskId: route.taskId, planId: nil, synthesisExecutionId: nil, lifecycle: snapshot.lifecycle))
        let deleted = await model.perform(.delete); XCTAssertFalse(deleted)
        XCTAssertEqual(requests.count, 2)
        let ordinary = TaskDetailScreenModel(seed: TaskDetailRoute(taskId: "task/one", executionId: "run/+&one").seed, executionId: "run/+&one") { path in
            if path.contains("execution-panel") { XCTAssertEqual(path, "/tasks/task%2Fone/execution-panel?execution_id=run%2F%2B%26one") }
            return [:]
        }
        await ordinary.refresh()
        let artifactURL = ArtifactRef.taskOutput(taskId: "task/one", relativePath: "outputs/file?+&.md", mime: "text/plain").url!
        XCTAssertEqual(URLComponents(url: artifactURL, resolvingAgainstBaseURL: false)?.percentEncodedPath, "/api/magician/v3/tasks/task%2Fone/outputs/file%3F%2B%26.md")
        XCTAssertTrue(WorkWebContext(section: .crew, agent: "writer/+one").workURL(base: URL(string: "https://runtime.example")!)!.absoluteString.contains("view=crew&agent=writer/"))
    }

    @MainActor
    func testHeaderOnlyCrewVersionIsAvailableForReviewedEdits() async throws {
        WorkMockURLProtocol.handle = { _ in (200, #"{"definition":{"agent_id":"writer","name":"Writer"}}"#, ["ETag": "\"header-v1\""]) }
        let frozen = client(currentCheck: { true }), resource = WorkResource(clientFactory: { frozen })
        resource.load("/api/magician/v2/agents/writer")
        try await waitWhile { resource.loading }
        XCTAssertEqual(resource.value?["etag"].text, "\"header-v1\"")
    }

    @MainActor
    func testThreadPoolWalksAllPagesAndCountsOnlyMatchingTasks() async throws {
        var offsets: [String] = []; let lock = NSLock()
        WorkMockURLProtocol.handle = { request in
            lock.lock(); defer { lock.unlock() }
            if request.url?.path.hasSuffix("/agents") == true { return (200, #"{"agents":[]}"#, [:]) }
            if request.url?.path.hasSuffix("/internal") == true { return (200, #"{"tasks":[]}"#, [:]) }
            let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
            XCTAssertNil(query.first { $0.name == "view" }, "A thread pool must include every status before applying scoped filters")
            let offset = query.first { $0.name == "offset" }?.value ?? ""; offsets.append(offset)
            if offset == "0" { return (200, #"{"tasks":[{"id":"other","title":"Other","status":"running","ui_thread_id":"general"}],"pagination":{"total":2,"limit":500,"offset":0,"has_more":true}}"#, [:]) }
            return (200, #"{"tasks":[{"id":"scoped","title":"Scoped","status":"running","ui_thread_id":"Launch"}],"pagination":{"total":2,"limit":500,"offset":1,"has_more":false}}"#, [:])
        }
        let model = TasksViewModel(session: mockSession())
        model.setThreadContext(" Launch ")
        try await waitWhile { model.isLoading }
        XCTAssertEqual(lock.withLock { offsets }, ["0", "1"])
        XCTAssertEqual(model.visibleTasks.map(\.id), ["scoped"])
        XCTAssertEqual(model.laneCount(.running), 1); XCTAssertEqual(model.laneCount(.completed), 0)
        XCTAssertFalse(model.tasksHasMore)
        NotificationCenter.default.post(name: .magicianMobileConnectionDidChange, object: nil)
        try await Task.sleep(nanoseconds: 10_000_000)
        XCTAssertTrue(model.tasks.isEmpty)
        var created = false
        model.createTask(title: "Stale", description: "", agentId: "writer", threadId: "launch", priority: nil, dueDate: nil, tagNames: []) { created = true }
        XCTAssertFalse(created); XCTAssertNotNil(model.actionErrorMessage)
    }

    private func mockSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [WorkMockURLProtocol.self]
        return URLSession(configuration: configuration)
    }
    private func client(headers: [String: String] = [:], currentCheck: (() -> Bool)? = nil) -> WorkAPIClient {
        WorkAPIClient(baseURL: URL(string: "https://work.test")!, headers: headers, session: mockSession(), currentCheck: currentCheck)
    }
    override func tearDown() { WorkMockURLProtocol.handle = nil; WorkMockURLProtocol.stream = nil; WorkMockURLProtocol.onStop = nil; super.tearDown() }

    @MainActor
    func testOversizedFilesStopBeforeEOFWithAndWithoutDeclaredLength() async {
        for declared in [true, false] {
            let rejected = expectation(description: "Oversized stream rejected \(declared)")
            let stopped = expectation(description: "Oversized transfer cancelled \(declared)")
            WorkMockURLProtocol.onStop = { stopped.fulfill() }
            WorkMockURLProtocol.stream = { transport in
                // Foundation coalesces tiny URLProtocol prefixes. Send a full
                // transport chunk while leaving the response open.
                let response = HTTPURLResponse(url: transport.request.url!, statusCode: 200, httpVersion: nil,
                                               headerFields: declared ? ["Content-Length": "65536"] : [:])!
                transport.client?.urlProtocol(transport, didReceive: response, cacheStoragePolicy: .notAllowed)
                transport.client?.urlProtocol(transport, didLoad: Data(repeating: 49, count: 65536))
                // Keep the transfer open: rejection must precede EOF.
            }
            let frozen = client()
            let download = Task {
                do { _ = try await frozen.file("/api/magician/v2/relentless/runs/run/artifacts/hash/file", maximumBytes: 8); XCTFail("Oversized file must fail") }
                catch { XCTAssertTrue(error.localizedDescription.contains("large")); rejected.fulfill() }
            }
            await fulfillment(of: [rejected], timeout: 1)
            download.cancel()
            await download.value
            await fulfillment(of: [stopped], timeout: 1)
        }
    }

    func testExactSizeFileRetainsBytesAndFrozenHeaders() async throws {
        WorkMockURLProtocol.handle = { request in
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer original")
            return (200, "12345678", ["Content-Length": "8"])
        }
        let data = try await client(headers: ["Authorization": "Bearer original"]).file("/api/magician/v2/relentless/runs/run/artifacts/hash/file", maximumBytes: 8)
        XCTAssertEqual(data, Data("12345678".utf8))
    }

    @MainActor
    func testCancellingFileDownloadStopsTheOpenTransfer() async {
        let started = expectation(description: "Stream opened")
        let stopped = expectation(description: "Cancelled stream stopped")
        WorkMockURLProtocol.onStop = { stopped.fulfill() }
        WorkMockURLProtocol.stream = { transport in
            let response = HTTPURLResponse(url: transport.request.url!, statusCode: 200, httpVersion: nil, headerFields: [:])!
            transport.client?.urlProtocol(transport, didReceive: response, cacheStoragePolicy: .notAllowed)
            transport.client?.urlProtocol(transport, didLoad: Data("1234".utf8)); started.fulfill()
        }
        let frozen = client()
        let download = Task {
            do { _ = try await frozen.file("/api/magician/v2/relentless/runs/run/artifacts/hash/file", maximumBytes: 8); XCTFail("Cancelled download must fail") }
            catch { XCTAssertTrue(error is CancellationError || (error as? URLError)?.code == .cancelled) }
        }
        await fulfillment(of: [started], timeout: 1)
        download.cancel(); await download.value
        await fulfillment(of: [stopped], timeout: 1)
    }

    func testDefinitionPreservesLargeIntegerAndEncodedIdentityWithVersionCheck() async throws {
        let definition = try JSONDecoder().decode(WorkJSON.self, from: Data("{\"unknown_budget\":9223372036854775807}".utf8))
        XCTAssertEqual(definition["unknown_budget"], .integer(Int64.max))
        WorkMockURLProtocol.handle = { request in
            XCTAssertEqual(request.url?.absoluteString, "https://work.test/api/magician/v2/agents/crew%2Fone%3F%23%25")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer first")
            XCTAssertEqual(request.value(forHTTPHeaderField: "If-Match"), "\"v1\"")
            return (200, "{\"definition\":{\"agent_id\":\"crew/one?#%\",\"unknown_budget\":9223372036854775807},\"etag\":\"v2\"}", [:])
        }
        let record = try await client(headers: ["Authorization": "Bearer first"]).patchMember("crew/one?#%", patch: definition.object!, etag: "\"v1\"")
        XCTAssertEqual(record.definition["unknown_budget"], .integer(Int64.max))
        XCTAssertEqual(record.etag, "v2")
    }
    func testMissingVersionNeverSendsAnEdit() async {
        WorkMockURLProtocol.handle = { _ in XCTFail("No request is allowed without a version"); return (200, "{}", [:]) }
        do { _ = try await client().patchMember("crew", patch: ["name": .string("New name")], etag: ""); XCTFail("Missing version must fail") }
        catch { XCTAssertTrue(error.localizedDescription.contains("Refresh")) }
    }
    func testConcurrentEditRequiresRefresh() async {
        WorkMockURLProtocol.handle = { _ in (412, "{\"error\":\"stale\"}", [:]) }
        do { _ = try await client().patchMember("crew", patch: ["name": .string("New name")], etag: "old"); XCTFail("Stale edit must fail") }
        catch { XCTAssertTrue(error.localizedDescription.contains("Refresh")) }
    }
    func testTriggerUsesJSONBody() async throws {
        WorkMockURLProtocol.handle = { request in
            XCTAssertEqual(request.httpMethod, "POST")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")
            XCTAssertEqual(request.url?.path, "/api/magician/v2/agents/crew/trigger")
            return (200, "{}", [:])
        }
        try await client().actOnMember("crew", action: .trigger)
    }
    func testSpecialistHandoffKeepsExactWorkReturnAndBrowserSessionCustody() throws {
        XCTAssertEqual(WorkNavigationTarget.parse("/work")?.kind, .all)
        XCTAssertEqual(WorkNavigationTarget.parse("/work?view=needs-you")?.kind, .needsYou)
        let context = WorkWebContext(agent: "owner + / café", task: "task/+one", execution: "run + / two", internalWork: true)
        let base = URL(string: "https://paired.example")!
        let events = try XCTUnwrap(context.url(base: base, destination: .events))
        let query = Dictionary(uniqueKeysWithValues: URLComponents(url: events, resolvingAgainstBaseURL: false)!.queryItems!.map { ($0.name, $0.value ?? "") })
        XCTAssertEqual(query["agent_id"], context.agent)
        XCTAssertEqual(query["task_id"], context.task)
        XCTAssertEqual(query["execution_id"], context.execution)
        let target = try XCTUnwrap(WorkNavigationTarget.parse(try XCTUnwrap(query["from"])))
        XCTAssertEqual(target.recordID, context.task); XCTAssertEqual(target.executionID, context.execution); XCTAssertTrue(target.internalWork)
        XCTAssertTrue(events.absoluteString.contains("%2B"), "Browser query decoding must preserve a literal plus")
        let budget = try XCTUnwrap(context.url(base: base, destination: .budget))
        XCTAssertEqual(URLComponents(url: budget, resolvingAgainstBaseURL: false)!.queryItems!.map(\.name), ["from"])
        XCTAssertEqual(context.url(base: base, destination: .analytics)?.fragment, "usage")
        for raw in ["javascript:alert(1)", "https://token@paired.example", "https://paired.example?bearer=secret", "https://paired.example#secret"] {
            XCTAssertNil(context.url(base: URL(string: raw)!, destination: .events))
        }
        for origin in [WorkWebContext(section: .crew, agent: "forge"), WorkWebContext(section: .programs, program: "planning/+launch.md"), WorkWebContext(section: .quality, run: "run/+one"), WorkWebContext(section: .operations)] {
            for destination in WorkSpecialist.allCases {
                let url = try XCTUnwrap(origin.url(base: base, destination: destination))
                XCTAssertEqual(URLComponents(url: url, resolvingAgainstBaseURL: false)!.queryItems?.first { $0.name == "from" }?.value, origin.returnPath)
                XCTAssertNil(url.user)
                XCTAssertNotNil(WorkNavigationTarget.parse(origin.returnPath))
            }
        }
    }

    func testHierarchyReportsLongCyclesAndStopsAtMissingTargets() throws {
        func member(_ id: String, _ targets: [String]) throws -> WorkCrewMember {
            try WorkCrewMember(.object(["agent_id": .string(id), "delegation_targets": .array(targets.map(WorkJSON.string))]))
        }
        let members = try [member("a", ["b", "missing"]), member("b", ["c"]), member("c", ["a"]), member("d", ["*"])]
        XCTAssertEqual(workDelegationCycle(origin: "a", target: "b", members: members), ["a", "b", "c", "a"])
        XCTAssertNil(workDelegationCycle(origin: "a", target: "missing", members: members))
        XCTAssertNil(workDelegationCycle(origin: "d", target: "*", members: members))
    }

    func testReferencesFreezeFullContentAndPreserveSeparateProviderIdentity() {
        var draft = WorkQualityDraft()
        draft.references = [WorkSourceReference(id: "notes:one", title: "Launch", origin: "provider-one/launch.md", content: "Exact source\nwith full text"), WorkSourceReference(id: "notes:two", title: "Launch", origin: "provider-two/launch.md", content: "Different evidence")]
        let sealed = draft.proposal()["sources"]
        draft.references[0] = WorkSourceReference(id: "notes:one", title: "Changed", origin: "other", content: "Changed text")
        XCTAssertEqual(sealed["notes:one"].text, "Reference: Launch\nOrigin: provider-one/launch.md\n\nExact source\nwith full text")
        XCTAssertEqual(sealed["notes:two"].text, "Reference: Launch\nOrigin: provider-two/launch.md\n\nDifferent evidence")
    }

    func testQualityContractKeepsLocalHandoffReviewQuorumAndIntegerBudget() {
        var draft = WorkQualityDraft(); draft.goal = "Read a local brief"; draft.source = "Only supplied facts"
        draft.rubric = "Clear and complete"; draft.actor = "actor"; draft.critic = "critic"
        let proposal = draft.proposal()
        XCTAssertEqual(proposal["handoff_kind"], .string("internal_draft"))
        XCTAssertEqual(proposal["human_review"], .string("required"))
        XCTAssertEqual(proposal["critic_profiles"].array?.count, 3)
        XCTAssertEqual(proposal["budget"]["max_cost_microusd"], .integer(40_000_000))
        XCTAssertEqual(proposal["budget"]["max_wall_clock_secs"], .integer(1800))
        XCTAssertEqual(proposal["review"]["score_quorum"], .integer(3))
        XCTAssertEqual(proposal["sources"]["brief"], .string("Only supplied facts"))
    }
    func testQualityModelControlsPreserveDefaultsAndBoundedIndependentRoutes() throws {
        let fixture = try WorkJSON.parseObject(#"""
        {"quality":{"profiles":[{"name":"actor-auto","family":"a"},{"name":"actor-high","family":"a"},{"name":"review-auto","family":"b"},{"name":"operation/actor","family":"a"}]},
         "catalog":{"profiles":[{"name":"operation/actor","model_selection":{"model_id":"a","thinking":"high"}}],"models":[
          {"id":"a","provider":"openai","connection":"a","model":"a","label":"Draft","thinking":{"mode":"effort","levels":["low","high"],"default":"high"},"choices":{"auto":"chat-a"},"operation_choices":{"auto":"actor-auto","low":"unbounded","high":"actor-high"}},
          {"id":"b","provider":"gemini","connection":"b","model":"b","label":"Review","thinking":{"mode":"effort","levels":["low","high"],"default":"low"},"choices":{"auto":"chat-b"},"operation_choices":{"auto":"review-auto","high":"unbounded-review"}}
        ]}}
        """#)
        let catalog = WorkQualityModels(quality: fixture["quality"], catalog: fixture["catalog"])
        let models = catalog.models()
        XCTAssertEqual(models.first?.choices, ["auto": "actor-auto", "high": "actor-high"])
        XCTAssertEqual(models.first?.thinking.levels, ["high"])
        XCTAssertEqual(catalog.selection("operation/actor", models: models)?.thinking, "high")
        XCTAssertTrue(catalog.valid("operation/actor", models: models))
        let reviewers = catalog.models(excluding: catalog.family("operation/actor"))
        XCTAssertEqual(reviewers.map(\.id), ["b"])
        XCTAssertFalse(catalog.valid("actor-auto", models: reviewers))
        XCTAssertFalse(catalog.valid("unbounded-review", models: reviewers))
        XCTAssertTrue(catalog.valid("review-auto", models: reviewers))
        XCTAssertFalse(catalog.valid("retired-model", models: models))
        let unavailable = ChatModelChoice(id: "b", provider: "gemini", connection: "b", model: "b", label: "Review", thinking: models[1].thinking, choices: models[1].choices, installed: false, limits: nil)
        XCTAssertFalse(catalog.valid("review-auto", models: [unavailable]))
        let workload = WorkQualityModels(quality: try WorkJSON.parseObject(#"{"profiles":[{"name":"operation/actor","family":"a"}]}"#), catalog: fixture["catalog"])
        XCTAssertEqual(workload.models().first?.choices, ["auto": "operation/actor", "high": "operation/actor"])
        XCTAssertEqual(workload.models().first?.thinking.defaultLevel, "high")
        XCTAssertEqual(workload.selection("operation/actor", models: workload.models())?.thinking, "auto")
        let noJSON = WorkQualityModels(quality: try WorkJSON.parseObject(#"{"profiles":[{"name":"review-auto","family":"b","supports_json":false}]}"#), catalog: fixture["catalog"])
        XCTAssertEqual(noJSON.models().map(\.id), ["b"])
        XCTAssertTrue(noJSON.models(excluding: "a").isEmpty)
        var draft = WorkQualityDraft(); draft.actor = models[0].choices["high"]!; draft.critic = reviewers[0].choices["auto"]!
        XCTAssertEqual(draft.proposal()["actor_profile"].text, "actor-high")
        XCTAssertEqual(draft.proposal()["critic_profiles"].array, Array(repeating: .string("review-auto"), count: 3))
    }
    @MainActor
    func testConnectionSwitchClearsRecordAndRejectsPendingConfirmation() async throws {
        var current = true, writes = 0
        WorkMockURLProtocol.handle = { request in
            if request.httpMethod == "POST" { writes += 1 }
            return (200, "{\"name\":\"First workspace\"}", [:])
        }
        let frozen = client(currentCheck: { current })
        let resource = WorkResource(clientFactory: { frozen })
        resource.load("/api/magician/v2/agents")
        try await waitWhile { resource.loading }
        XCTAssertEqual(resource.value?["name"], .string("First workspace"))
        current = false
        NotificationCenter.default.post(name: .magicianMobileConnectionDidChange, object: nil)
        try await Task.sleep(nanoseconds: 10_000_000)
        XCTAssertNil(resource.value)
        let result = await resource.perform("/api/magician/v2/agents/crew/pause")
        XCTAssertNil(result); XCTAssertEqual(writes, 0)
        current = true
        resource.refresh()
        XCTAssertNil(resource.value, "An invalidated screen cannot reconnect its old fields to a new workspace")
    }
    func testPolicyTemplatesKeepFiniteCapsAndExplicitCaptureReview() {
        let capture = WorkPolicyKind.capture.initialSpec
        XCTAssertEqual(capture["maximum_acts"], .integer(1))
        XCTAssertEqual(capture["approval"]["always_review"], .bool(true))
        XCTAssertEqual(capture["approval"]["allow_automatic_capture"], .bool(false))
        let learning = WorkPolicyKind.learning.initialSpec
        XCTAssertEqual(learning["maximum_adopted_runs"], .integer(1))
        XCTAssertEqual(learning["unknown_monitor_disables_adoption"], .bool(true))
    }

    @MainActor
    func testLatestFilterLoadsAfterAnInFlightWriteSucceedsOrFails() async throws {
        for status in [200, 409] {
            let started = expectation(description: "Write started \(status)")
            let release = DispatchSemaphore(value: 0)
            WorkMockURLProtocol.handle = { request in
                if request.httpMethod == "POST" {
                    started.fulfill()
                    guard release.wait(timeout: .now() + 5) == .success else { throw WorkAPIError.invalidResponse }
                    return (status, "{}", [:])
                }
                let filter = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "q" }?.value ?? ""
                return (200, "{\"filter\":\"\(filter)\"}", [:])
            }
            let frozen = client(currentCheck: { true })
            let resource = WorkResource(clientFactory: { frozen })
            resource.load("/api/magician/v2/relentless/catalog?q=original")
            try await waitWhile { resource.loading }
            XCTAssertEqual(resource.value?["filter"].text, "original")
            let write = Task { await resource.perform("/api/magician/v2/relentless/runs/run/cancel") }
            await fulfillment(of: [started], timeout: 3)
            XCTAssertTrue(resource.busy)
            resource.load("/api/magician/v2/relentless/catalog?q=intermediate")
            resource.load("/api/magician/v2/relentless/catalog?q=latest")
            resource.refresh()
            release.signal()
            let result = await write.value
            XCTAssertEqual(result != nil, status == 200)
            try await waitWhile { resource.loading }
            XCTAssertEqual(resource.path, "/api/magician/v2/relentless/catalog?q=latest")
            XCTAssertEqual(resource.value?["filter"].text, "latest")
            XCTAssertFalse(resource.busy)
            XCTAssertEqual(resource.error != nil, status != 200, "Changing the filter must not hide a failed write")
        }
    }

    @MainActor
    func testQuickTaskCreationKeepsFailureOpenAndCompletesOnlyAfterSuccessfulRetry() async throws {
        let model = TasksViewModel(session: mockSession())
        var accept = false, completed = false
        WorkMockURLProtocol.handle = { request in
            if request.httpMethod == "POST" {
                return accept ? (201, "{\"task\":{\"id\":\"created\"}}", [:]) : (422, "{\"error\":\"Choose an available crew member.\"}", [:])
            }
            return (200, "{\"tasks\":[],\"agents\":[]}", [:])
        }
        func create() {
            model.createTask(title: "Keep this draft", description: "Owner instructions", agentId: "crew", threadId: "general",
                             priority: nil, dueDate: nil, tagNames: []) { completed = true }
        }
        create()
        try await waitWhile { model.mutatingTaskID != nil }
        XCTAssertFalse(completed, "A rejected creation must not dismiss the draft form")
        XCTAssertEqual(model.actionErrorMessage, "Choose an available crew member.")
        XCTAssertNil(model.mutatingTaskID)
        accept = true; create()
        try await waitWhile { model.mutatingTaskID != nil }
        XCTAssertTrue(completed)
        XCTAssertNil(model.actionErrorMessage)
        XCTAssertEqual(model.actionNoticeMessage, "Task created.")
    }


    func testProgramDocumentReadsContentHashAndToleratesOlderRuntimes() throws {
        let current = try JSONDecoder().decode(WorkProgramDocument.self, from: Data(##"{"name":"positioning.md","title":"Positioning","content":"# Pitch","missions_section":null,"sha256":"abc123","history":[{"snapshot":"s1","modified":"2026-10-17"}]}"##.utf8))
        XCTAssertEqual(current.sha256, "abc123"); XCTAssertEqual(current.history.map(\.id), ["s1"])
        let older = try JSONDecoder().decode(WorkProgramDocument.self, from: Data(#"{"name":"facts.md","content":"Facts","history":"odd"}"#.utf8))
        XCTAssertEqual(older.sha256, ""); XCTAssertEqual(older.title, "facts.md"); XCTAssertTrue(older.history.isEmpty)
    }

    func testProgramSaveSendsDraftWithOpenedHashAndReadsTheNewHash() async throws {
        WorkMockURLProtocol.handle = { request in
            XCTAssertEqual(request.httpMethod, "PUT")
            XCTAssertEqual(request.url?.path, "/api/magician/v2/programs/plans/+launch.md")
            XCTAssertEqual(request.url?.absoluteString, "https://work.test/api/magician/v2/programs/plans%2F+launch.md")
            let body = try JSONDecoder().decode(WorkJSON.self, from: WorkMockURLProtocol.body(request))
            XCTAssertEqual(body, .object(["content": .string("# New\n"), "base_sha256": .string("base-hash")]))
            return (200, #"{"saved":true,"program":"plans/+launch.md","created":false,"unchanged":false,"history_snapshot":"snap-1","sha256":"next-hash"}"#, [:])
        }
        let saved = try await client().saveProgram(name: "plans/+launch.md", content: "# New\n", baseSHA256: "base-hash")
        XCTAssertEqual(saved.sha256, "next-hash"); XCTAssertEqual(saved.historySnapshot, "snap-1"); XCTAssertFalse(saved.unchanged)
    }

    func testProgramSaveConflictKeepsServerMessageAndCurrentHash() async {
        WorkMockURLProtocol.handle = { _ in (409, #"{"error":"program_changed","message":"positioning.md changed after it was opened.","current_sha256":"theirs"}"#, [:]) }
        do { _ = try await client().saveProgram(name: "positioning.md", content: "Mine", baseSHA256: "mine"); XCTFail("A changed document must not save") }
        catch { XCTAssertEqual(error as? WorkProgramSaveError, .changed(message: "positioning.md changed after it was opened.", currentSHA256: "theirs")) }
    }

    func testProgramSaveRefusalSurfacesTheServerTextAndAMissingBaseSendsNothing() async {
        WorkMockURLProtocol.handle = { _ in (401, #"{"error":"grant_cannot_manage","message":"Only a signed-in owner session can make this change."}"#, [:]) }
        do { _ = try await client().saveProgram(name: "positioning.md", content: "Mine", baseSHA256: "mine"); XCTFail("A refused save must fail") }
        catch { XCTAssertEqual(error.localizedDescription, "Only a signed-in owner session can make this change.") }
        WorkMockURLProtocol.handle = { _ in (400, #"{"error":"save failed: invalid program"}"#, [:]) }
        do { _ = try await client().saveProgram(name: "positioning.md", content: "Mine", baseSHA256: "mine"); XCTFail("A rejected save must fail") }
        catch { XCTAssertEqual(error.localizedDescription, "save failed: invalid program") }
        WorkMockURLProtocol.handle = { _ in XCTFail("No save without the opened version"); return (200, "{}", [:]) }
        do { _ = try await client().saveProgram(name: "positioning.md", content: "Mine", baseSHA256: ""); XCTFail("A missing base must fail") }
        catch { XCTAssertEqual(error as? WorkProgramSaveError, .missingBase) }
    }

    func testFocusAreaSharedProgramsKeepThreeDistinctStatesAndOtherFields() throws {
        let area = try WorkJSON.parseObject(#"{"name":"Inbound","description":"Sweep","program":"outward_charter.md","priority":"high","evidence":{"revision":18446744073709551615}}"#)
        XCTAssertNil(area.sharedPrograms); XCTAssertEqual(area.sharedProgramsMode(), .global)
        let none = area.settingSharedPrograms(.none)
        XCTAssertEqual(none["shared_programs"], .array([])); XCTAssertEqual(none.sharedProgramsMode(), .none)
        XCTAssertEqual(none.sharedProgramsMode(choosing: true), .choose, "Choose with nothing checked also stores []")
        let chosen = none.togglingSharedProgram("facts.md", on: true).togglingSharedProgram("positioning.md", on: true)
        XCTAssertEqual(chosen["shared_programs"], .array([.string("facts.md"), .string("positioning.md")]))
        XCTAssertEqual(chosen.sharedProgramsMode(), .choose)
        XCTAssertEqual(chosen.togglingSharedProgram("facts.md", on: false)["shared_programs"], .array([.string("positioning.md")]))
        XCTAssertEqual(chosen.togglingSharedProgram("facts.md", on: false).togglingSharedProgram("positioning.md", on: false)["shared_programs"], .array([]))
        let global = chosen.settingSharedPrograms(.global)
        XCTAssertNil(global.object?["shared_programs"], "Use global list removes the key")
        XCTAssertEqual(global, area, "every other field survives the round trip")
        XCTAssertEqual(try WorkJSON.parseObject(chosen.formatted), chosen)
        XCTAssertEqual(try WorkJSON.parseObject(#"{"shared_programs":null}"#).sharedProgramsMode(), .global)
        XCTAssertEqual(try WorkJSON.parseObject(#"{"shared_programs":[" facts.md ","facts.md",""]}"#).sharedPrograms, ["facts.md"])
        // The patch carries the whole focus-area list, so [] and an absent key stay distinct on the wire.
        let definition = WorkJSON.object(["autonomous_config": .object(["focus_areas": .array([area])])])
        let patch = definition.setting(["autonomous_config", "focus_areas"], to: .array([none])).mergePatch(from: definition)
        XCTAssertEqual(patch.at(["autonomous_config", "focus_areas"]).array?.first?["shared_programs"], .array([]))
    }
}
