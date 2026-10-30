import XCTest
import Combine
@testable import Magician

final class NetworkingViewModelTests: XCTestCase {
    private var cancellables: Set<AnyCancellable> = []
    private var sessions: [URLSession] = []

    override func setUp() {
        super.setUp()
        MockURLProtocol.handler = nil
        cancellables.removeAll()
        MagicianAccess.clearCredentials()
    }

    override func tearDown() {
        sessions.forEach { $0.invalidateAndCancel() }
        sessions.removeAll()
        MockURLProtocol.handler = nil
        cancellables.removeAll()
        MagicianAccess.clearCredentials()
        super.tearDown()
    }

    func testMagicianAccessAddsBearerAndCompleteAccessCredentials() throws {
        let profile = try MobileConnectionProfile(
            publicOrigin: XCTUnwrap(URL(string: "https://ios.example.com")),
            principal: "profile-principal",
            workspace: "profile-workspace",
            deviceID: "device",
            deviceToken: "token",
            cloudflareClientID: "client",
            cloudflareClientSecret: "secret"
        )
        XCTAssertEqual(MagicianAccess.headers(profile: profile)["CF-Access-Client-Id"], "client")

        var request = URLRequest(url: URL(string: "https://ios.example.com/api/magician/v2/tasks")!)
        MagicianAccess.authorize(
            &request,
            principal: profile.principal,
            workspace: profile.workspace,
            profile: profile
        )
        XCTAssertNil(request.value(forHTTPHeaderField: "X-Principal"))
        XCTAssertNil(request.value(forHTTPHeaderField: "X-Workspace"))
        XCTAssertEqual(request.value(forHTTPHeaderField: "CF-Access-Client-Id"), "client")
        XCTAssertEqual(request.value(forHTTPHeaderField: "CF-Access-Client-Secret"), "secret")
        XCTAssertEqual(request.value(forHTTPHeaderField: "X-Magician-Device-Id"), "device")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer token")

        var external = URLRequest(url: URL(string: "https://example.com")!)
        MagicianAccess.authorize(
            &external,
            principal: profile.principal,
            workspace: profile.workspace,
            profile: profile
        )
        XCTAssertNil(external.value(forHTTPHeaderField: "CF-Access-Client-Id"))
        XCTAssertNil(external.value(forHTTPHeaderField: "X-Magician-Device-Id"))
        XCTAssertNil(external.value(forHTTPHeaderField: "Authorization"))

        let outer = MagicianAccess.cloudflareAccessHeaders(profile: profile)
        XCTAssertEqual(outer["CF-Access-Client-Id"], "client")
        XCTAssertEqual(outer["CF-Access-Client-Secret"], "secret")
        XCTAssertNil(outer["X-Magician-Device-Id"])
        XCTAssertNil(outer["Authorization"])
        XCTAssertNil(outer[MagicianAccess.appVersionHeader])
    }

    func testAppVersionHeaderTravelsOnlyWithDeviceIdentity() throws {
        let profile = try MobileConnectionProfile(
            publicOrigin: XCTUnwrap(URL(string: "https://ios.example.com")),
            principal: "profile-principal",
            workspace: "profile-workspace",
            deviceID: "device",
            deviceToken: "token"
        )
        let expected = MagicianAccess.makeAppVersionHeaderValue(info: Bundle.main.infoDictionary)
        XCTAssertEqual(MagicianAccess.headers(profile: profile)[MagicianAccess.appVersionHeader], expected)

        var request = URLRequest(url: URL(string: "https://ios.example.com/api/magician/v2/tasks")!)
        MagicianAccess.authorize(&request, principal: profile.principal, workspace: profile.workspace, profile: profile)
        XCTAssertEqual(request.value(forHTTPHeaderField: "X-Magician-App-Version"), expected)

        var external = URLRequest(url: URL(string: "https://example.com")!)
        MagicianAccess.authorize(&external, principal: profile.principal, workspace: profile.workspace, profile: profile)
        XCTAssertNil(external.value(forHTTPHeaderField: "X-Magician-App-Version"))
        XCTAssertNil(MagicianAccess.headers(profile: nil)[MagicianAccess.appVersionHeader])
    }

    func testAppVersionHeaderValueFormatsAndBoundsTheBundleVersion() {
        XCTAssertEqual(
            MagicianAccess.makeAppVersionHeaderValue(info: [
                "CFBundleShortVersionString": "0.4.11", "CFBundleVersion": "222",
            ]),
            "0.4.11 (222)"
        )
        XCTAssertEqual(MagicianAccess.makeAppVersionHeaderValue(info: ["CFBundleShortVersionString": "0.4.11"]), "0.4.11")
        XCTAssertNil(MagicianAccess.makeAppVersionHeaderValue(info: ["CFBundleShortVersionString": "  ", "CFBundleVersion": "222"]))
        XCTAssertNil(MagicianAccess.makeAppVersionHeaderValue(info: ["CFBundleVersion": "222"]))
        XCTAssertNil(MagicianAccess.makeAppVersionHeaderValue(info: nil))
        XCTAssertEqual(
            MagicianAccess.makeAppVersionHeaderValue(info: ["CFBundleShortVersionString": "0.4.11\u{2011}β", "CFBundleVersion": "2\n22"]),
            "0.4.11 (222)"
        )
        let long = MagicianAccess.makeAppVersionHeaderValue(info: [
            "CFBundleShortVersionString": String(repeating: "9", count: 60), "CFBundleVersion": "1",
        ])
        XCTAssertEqual(long?.count, 40)
    }

    func testLegacyCredentialRotationCannotInventAnUnenrolledHost() {
        MagicianAccess.setCredentials(clientId: "client", clientSecret: "secret")

        XCTAssertFalse(MagicianAccess.isConfigured)
        XCTAssertTrue(MagicianAccess.headers().isEmpty)
    }

    func testMagicianAccessMigratesLegacyAppGroupCredentialsIntoKeychain() {
        MagicianAccess.clearCredentials()
        MagicianAccess.store.set("legacy-client", forKey: MagicianAccess.clientIdKey)
        MagicianAccess.store.set("legacy-secret", forKey: MagicianAccess.clientSecretKey)

        XCTAssertEqual(MagicianAccess.clientId, "legacy-client")
        XCTAssertEqual(MagicianAccess.clientSecret, "legacy-secret")
        XCTAssertNil(MagicianAccess.store.string(forKey: MagicianAccess.clientIdKey))
        XCTAssertNil(MagicianAccess.store.string(forKey: MagicianAccess.clientSecretKey))
    }

    func testMagicianAccessNeverAddsCallerScopeHeaders() {
        var request = URLRequest(url: URL(string: "https://example.com")!)
        MagicianAccess.authorize(&request)

        XCTAssertNil(request.value(forHTTPHeaderField: "X-Principal"))
        XCTAssertNil(request.value(forHTTPHeaderField: "X-Workspace"))
    }

    func testAttentionFetchBuildsScopeAndPublishesLanes() {
        let requestSeen = expectation(description: "attention request")
        MockURLProtocol.handler = { request in
            if request.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: request), jsonData(["requests": []]))
            }
            XCTAssertEqual(request.httpMethod, "GET")
            XCTAssertEqual(request.url?.path, "/api/magician/v2/feed/attention")
            XCTAssertFalse(request.url?.query?.contains("principal=") ?? false)
            XCTAssertFalse(request.url?.query?.contains("workspace=") ?? false)
            XCTAssertTrue(request.url!.query!.contains("limit=25"))
            requestSeen.fulfill()
            return (response(for: request), jsonData([
                "requests": [["id": "r1", "title": "Question", "item_type": "request", "status": "open"]],
                "approvals": [], "escalations": [], "failed": [], "running": [],
                "pages": ["requests": ["total": 1, "next_cursor": "next", "has_more": true]]
            ]))
        }
        let sut = AttentionViewModel(networkSession: mockSession())
        let published = expectation(description: "lanes published")
        sut.$lanes.dropFirst().sink { lanes in
            if lanes["requests"]?.count == 1 { published.fulfill() }
        }.store(in: &cancellables)
        sut.fetch()
        wait(for: [requestSeen, published], timeout: 2)
        waitUntil { !sut.isLoading }
        XCTAssertFalse(sut.isLoading)
        XCTAssertEqual(sut.page("requests")?.nextCursor, "next")
        XCTAssertNil(sut.page("unknown"))
        XCTAssertEqual(sut.items("all").count, 1)              // web parity: All aggregates HITL lanes
    }

    func testAttentionSubmitChoiceSendsCanonicalHITLPayload() throws {
        let postSeen = expectation(description: "HITL POST")
        let refreshSeen = expectation(description: "attention refresh")
        var captured: [String: Any] = [:]
        MockURLProtocol.handler = { request in
            if request.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: request), jsonData(["requests": []]))
            }
            if request.httpMethod == "POST" {
                let body = try XCTUnwrap(requestBody(request))
                captured = try JSONSerialization.jsonObject(with: body) as! [String: Any]
                XCTAssertEqual(request.url?.path, "/api/magician/v2/hitl/corr/respond")
                postSeen.fulfill()
                return (response(for: request), jsonData(["accepted": true]))
            }
            refreshSeen.fulfill()
            return (response(for: request), jsonData(["requests": [], "approvals": [], "escalations": [], "failed": [], "running": []]))
        }
        let item = try JSONDecoder().decode(AttentionItem.self, from: jsonData([
            "id": "i", "title": "Choose", "item_type": "escalation", "status": "open",
            "metadata": ["input_type": "choice", "hitl_request": ["identifiers": ["correlation_id": "corr"]]]
        ]))
        let sut = AttentionViewModel(networkSession: mockSession())
        sut.submitChoice(item, optionId: "other", otherValue: "custom")
        wait(for: [postSeen, refreshSeen], timeout: 2)
        let value = captured["value"] as? [String: Any]
        XCTAssertEqual(captured["source"] as? String, "agentic")
        XCTAssertEqual(captured["correlation_id"] as? String, "corr")
        XCTAssertEqual(value?["selected_id"] as? String, "other")
        XCTAssertEqual(value?["other_value"] as? String, "custom")
    }

    func testSessionHistoryUsesServerLaneAndPaginationContract() {
        var requestedOffsets: [String] = []
        MockURLProtocol.handler = { request in
            XCTAssertTrue(request.url!.path.hasSuffix("chat/sessions"))
            let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
            XCTAssertEqual(query.first(where: { $0.name == "history_lane" })?.value, "personal")
            XCTAssertEqual(query.first(where: { $0.name == "limit" })?.value, "15")
            let offset = query.first(where: { $0.name == "offset" })?.value ?? ""
            requestedOffsets.append(offset)
            let item = offset == "15"
                ? self.sessionJSON(id: "page-two", updated: 2)
                : self.sessionJSON(id: "page-one", updated: 9, isDefault: true)
            return (response(for: request), jsonData([
                "sessions": [item], "total": 16, "limit": 15, "offset": Int(offset) ?? 0
            ]))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        let firstPage = expectation(description: "first page")
        sut.$isLoading.dropFirst().filter { !$0 }.prefix(1).sink { _ in firstPage.fulfill() }.store(in: &cancellables)
        sut.fetchData()
        wait(for: [firstPage], timeout: 2)
        XCTAssertEqual(sut.sessions.map(\.id), ["page-one"])
        XCTAssertEqual(sut.total, 16)
        XCTAssertEqual(sut.pageStart, 1)
        XCTAssertEqual(sut.pageEnd, 15)
        XCTAssertEqual(sut.sessions.first?.isDefaultSession, true)

        let secondPage = expectation(description: "second page")
        sut.$isLoading.dropFirst().filter { !$0 }.prefix(1).sink { _ in secondPage.fulfill() }.store(in: &cancellables)
        sut.loadNextPage()
        wait(for: [secondPage], timeout: 2)
        XCTAssertEqual(sut.sessions.map(\.id), ["page-two"])
        XCTAssertEqual(sut.offset, 15)
        XCTAssertEqual(sut.pageStart, 16)
        XCTAssertEqual(sut.pageEnd, 16)
        XCTAssertEqual(requestedOffsets, ["0", "15"])
    }

    func testHistorySearchIgnoresSelectedLaneAndTabAndReturnsMixedTaggedResults() {
        var seenQueries: [[URLQueryItem]] = []
        MockURLProtocol.handler = { request in
            XCTAssertTrue(request.url!.path.hasSuffix("history/search"))
            let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
            seenQueries.append(query)
            var session = self.sessionJSON(id: "screen-session", updated: 9)
            session["title"] = "Observed screen"
            session["history_lane"] = "automated"
            session["ui_thread_id"] = "screens"
            return (response(for: request), jsonData([
                "items": [
                    ["kind": "session", "history_lane": "automated", "session": session],
                    [
                        "kind": "thread", "history_lane": "personal",
                        "thread": self.threadJSON(id: "observed-notes", sort: 1, lane: "personal")
                    ],
                ],
                "total": 2, "limit": 15, "offset": 0
            ]))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        sut.activeTab = "threads"
        sut.historyLane = .automated
        sut.searchText = "observed"
        let finished = expectation(description: "global history search")
        sut.$isLoading.dropFirst().filter { !$0 }.prefix(1).sink { _ in finished.fulfill() }.store(in: &cancellables)
        sut.submitSearch()
        wait(for: [finished], timeout: 2)

        XCTAssertTrue(sut.isSearchActive)
        XCTAssertTrue(sut.sessions.isEmpty)
        XCTAssertTrue(sut.threads.isEmpty)
        XCTAssertEqual(sut.searchResults.map(\.kind), ["session", "thread"])
        XCTAssertEqual(sut.searchResults.map(\.historyLane), [.automated, .personal])
        XCTAssertEqual(seenQueries.count, 1)
        XCTAssertNil(seenQueries[0].first(where: { $0.name == "history_lane" }))
        XCTAssertEqual(seenQueries[0].first(where: { $0.name == "q" })?.value, "observed")

        sut.selectSession("screen-session")
        XCTAssertEqual(sut.activeThreadId, "screens")
        XCTAssertEqual(sut.sessionMetadata(for: "screen-session")?.title, "Observed screen")
        sut.searchResults = []
        XCTAssertEqual(sut.sessionMetadata(for: "screen-session")?.title, "Observed screen")
    }

    func testOpeningThreadResolvesItsSessionBeforeChangingTheVisibleChat() throws {
        var requestedThread: String?
        MockURLProtocol.handler = { request in
            XCTAssertTrue(request.url!.path.hasSuffix("chat/sessions"))
            let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
            requestedThread = query.first(where: { $0.name == "ui_thread_id" })?.value
            var session = self.sessionJSON(id: "travel-session", updated: 9)
            session["ui_thread_id"] = "travel"
            session["title"] = "Travel planning"
            return (response(for: request), jsonData([
                "sessions": [session], "total": 1, "limit": 15, "offset": 0
            ]))
        }
        let thread = try JSONDecoder().decode(
            UiThreadRecord.self,
            from: jsonData(threadJSON(id: "travel", sort: 1))
        )
        let sut = ThreadViewModel(networkSession: mockSession())
        let opened = expectation(description: "thread session resolved")
        var openedSessionId: String?

        sut.openThread(thread) { sessionId in
            openedSessionId = sessionId
            opened.fulfill()
        }
        wait(for: [opened], timeout: 2)

        XCTAssertEqual(requestedThread, "travel")
        XCTAssertEqual(openedSessionId, "travel-session")
        XCTAssertEqual(sut.activeThreadId, "travel")
        XCTAssertEqual(sut.activeSessionId, "travel-session")
        XCTAssertEqual(sut.sessionMetadata(for: "travel-session")?.title, "Travel planning")
        XCTAssertEqual(sut.threadMetadata(for: "travel")?.name, "travel")
    }

    func testOpeningThreadPagesPastArchivedSessionsToFindItsActiveSession() throws {
        var offsets: [Int] = []
        MockURLProtocol.handler = { request in
            let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
            let offset = Int(query.first(where: { $0.name == "offset" })?.value ?? "0") ?? 0
            offsets.append(offset)
            if offset == 0 {
                let archived = (0..<15).map { index -> [String: Any] in
                    var session = self.sessionJSON(id: "archived-\(index)", updated: 100 - index)
                    session["ui_thread_id"] = "travel"
                    session["status"] = "archived"
                    return session
                }
                return (response(for: request), jsonData([
                    "sessions": archived, "total": 16, "limit": 15, "offset": 0
                ]))
            }
            var active = self.sessionJSON(id: "active-travel", updated: 1)
            active["ui_thread_id"] = "travel"
            return (response(for: request), jsonData([
                "sessions": [active], "total": 16, "limit": 15, "offset": 15
            ]))
        }
        let thread = try JSONDecoder().decode(
            UiThreadRecord.self,
            from: jsonData(threadJSON(id: "travel", sort: 1))
        )
        let sut = ThreadViewModel(networkSession: mockSession())
        let opened = expectation(description: "active session found on later page")

        sut.openThread(thread) { sessionId in
            XCTAssertEqual(sessionId, "active-travel")
            opened.fulfill()
        }
        wait(for: [opened], timeout: 2)

        XCTAssertEqual(offsets, [0, 15])
        XCTAssertEqual(sut.activeSessionId, "active-travel")
        XCTAssertEqual(sut.activeThreadId, "travel")
    }

    func testOpeningEmptyAutomatedThreadCreatesAnAutomatedSessionInThatThread() throws {
        var requests: [URLRequest] = []
        let refreshed = expectation(description: "created automated lane refreshed")
        MockURLProtocol.handler = { request in
            requests.append(request)
            if request.httpMethod == "GET" {
                let query = URLComponents(
                    url: try XCTUnwrap(request.url),
                    resolvingAgainstBaseURL: false
                )?.queryItems ?? []
                if query.first(where: { $0.name == "history_lane" })?.value == "automated" {
                    refreshed.fulfill()
                }
                return (response(for: request), jsonData([
                    "sessions": [], "total": 0, "limit": 15, "offset": 0
                ]))
            }
            var session = self.sessionJSON(
                id: "new-screen-session",
                updated: 9,
                lane: "automated"
            )
            session["ui_thread_id"] = "screens"
            return (response(for: request), jsonData(["session": session]))
        }
        let thread = try JSONDecoder().decode(
            UiThreadRecord.self,
            from: jsonData(threadJSON(id: "screens", sort: 1, lane: "automated"))
        )
        let sut = ThreadViewModel(networkSession: mockSession())
        let opened = expectation(description: "empty automated thread session created")

        sut.openThread(thread) { sessionId in
            XCTAssertEqual(sessionId, "new-screen-session")
            opened.fulfill()
        }
        wait(for: [opened, refreshed], timeout: 2)

        let createRequest = try XCTUnwrap(requests.first(where: { $0.httpMethod == "POST" }))
        let query = URLComponents(
            url: try XCTUnwrap(createRequest.url),
            resolvingAgainstBaseURL: false
        )?.queryItems ?? []
        XCTAssertEqual(query.first(where: { $0.name == "ui_thread_id" })?.value, "screens")
        XCTAssertEqual(query.first(where: { $0.name == "history_lane" })?.value, "automated")
        let refreshRequest = try XCTUnwrap(requests.first(where: { request in
            guard request.httpMethod == "GET",
                  let url = request.url,
                  let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems
            else { return false }
            return items.first(where: { $0.name == "history_lane" })?.value == "automated"
        }))
        let refreshQuery = URLComponents(
            url: try XCTUnwrap(refreshRequest.url),
            resolvingAgainstBaseURL: false
        )?.queryItems ?? []
        XCTAssertNil(refreshQuery.first(where: { $0.name == "ui_thread_id" }))
        XCTAssertEqual(sut.activeThreadId, "screens")
        XCTAssertEqual(sut.activeSessionId, "new-screen-session")
        XCTAssertEqual(sut.historyLane, .automated)
        XCTAssertEqual(sut.threadMetadata(for: "screens")?.name, "screens")
    }

    func testFailedThreadOpenDoesNotChangeCurrentThreadSelection() throws {
        MockURLProtocol.handler = { request in
            (response(for: request, status: 500), jsonData(["error": "failed"]))
        }
        let thread = try JSONDecoder().decode(
            UiThreadRecord.self,
            from: jsonData(threadJSON(id: "travel", sort: 1))
        )
        let sut = ThreadViewModel(networkSession: mockSession())
        sut.activeThreadId = "general"
        let finished = expectation(description: "thread open failed")

        sut.openThread(thread) { sessionId in
            XCTAssertNil(sessionId)
            finished.fulfill()
        }
        wait(for: [finished], timeout: 2)

        XCTAssertEqual(sut.activeThreadId, "general")
        XCTAssertNil(sut.activeSessionId)
        XCTAssertNotNil(sut.errorMessage)
    }

    func testDefaultSessionAndGeneralThreadMutationsAreSuppressedLocally() throws {
        var requestCount = 0
        MockURLProtocol.handler = { request in
            requestCount += 1
            return (response(for: request), Data())
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        let protected = try JSONDecoder().decode(
            ChatSession.self,
            from: jsonData(sessionJSON(id: "default", updated: 1, isDefault: true))
        )
        sut.sessions = [protected]

        sut.archiveSession("default")
        sut.deleteSession("default")
        sut.archiveThread("general")
        sut.unarchiveThread("general")
        sut.deleteThread("general")

        XCTAssertEqual(requestCount, 0)
    }

    func testResumeFallbackWalksServerPagesUntilItFindsAnActiveSession() {
        var requestedOffsets: [Int] = []
        MockURLProtocol.handler = { request in
            let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
            XCTAssertNil(query.first(where: { $0.name == "ui_thread_id" }))
            XCTAssertEqual(query.first(where: { $0.name == "history_lane" })?.value, "personal")
            XCTAssertEqual(query.first(where: { $0.name == "limit" })?.value, "15")
            let offset = Int(query.first(where: { $0.name == "offset" })?.value ?? "0") ?? 0
            requestedOffsets.append(offset)
            if offset == 0 {
                let archived = (0..<15).map {
                    var item = self.sessionJSON(id: "archived-\($0)", updated: 100 - $0)
                    item["status"] = "archived"
                    return item
                }
                return (response(for: request), jsonData([
                    "sessions": archived, "total": 16, "limit": 15, "offset": 0
                ]))
            }
            return (response(for: request), jsonData([
                "sessions": [self.sessionJSON(id: "active-default", updated: 1, isDefault: true)],
                "total": 16, "limit": 15, "offset": 15
            ]))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        let resumed = expectation(description: "resumed active default session")
        sut.resumeDefaultSession { id in
            XCTAssertEqual(id, "active-default")
            resumed.fulfill()
        }
        wait(for: [resumed], timeout: 2)
        XCTAssertEqual(requestedOffsets, [0, 15])
    }

    func testResumeFallbackWalksLegacyBareFullPagesBeforeCreating() {
        var requestedOffsets: [Int] = []
        MockURLProtocol.handler = { request in
            let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
            let offset = Int(query.first(where: { $0.name == "offset" })?.value ?? "0") ?? 0
            requestedOffsets.append(offset)
            if offset == 0 {
                let archived = (0..<ThreadViewModel.pageSize).map {
                    var session = self.sessionJSON(id: "archived-\($0)", updated: $0)
                    session["status"] = "archived"
                    return session
                }
                return (response(for: request), jsonData(archived))
            }
            return (
                response(for: request),
                jsonData([self.sessionJSON(id: "active-on-page-two", updated: 100)])
            )
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        let resumed = expectation(description: "legacy second page restored")
        sut.resumeLastSession(preferredSessionId: nil) { result in
            XCTAssertEqual(result, .restored("active-on-page-two"))
            resumed.fulfill()
        }
        wait(for: [resumed], timeout: 2)
        XCTAssertEqual(requestedOffsets, [0, 15])
    }

    func testResumeLastSessionRestoresTheExactRememberedThread() {
        MockURLProtocol.handler = { request in
            XCTAssertTrue(request.url!.path.hasSuffix("chat/sessions/remembered"))
            XCTAssertEqual(request.httpMethod, "GET")
            var session = self.sessionJSON(id: "remembered", updated: 1)
            session["ui_thread_id"] = "work"
            return (response(for: request), jsonData(["session": session, "messages": []]))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        let resumed = expectation(description: "exact remembered session restored")
        sut.resumeLastSession(preferredSessionId: "remembered") { result in
            XCTAssertEqual(result, .restored("remembered"))
            resumed.fulfill()
        }
        wait(for: [resumed], timeout: 2)
        XCTAssertEqual(sut.activeSessionId, "remembered")
        XCTAssertEqual(sut.activeThreadId, "work")
    }

    func testResumeLastSessionCreatesNothingOnTransientFailure() {
        var methods: [String] = []
        MockURLProtocol.handler = { request in
            methods.append(request.httpMethod ?? "GET")
            return (response(for: request, status: 503), jsonData(["error": "offline"]))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        let settled = expectation(description: "transient failure remains unavailable")
        sut.resumeLastSession(preferredSessionId: "remembered") { result in
            XCTAssertEqual(result, .unavailable)
            settled.fulfill()
        }
        wait(for: [settled], timeout: 2)
        XCTAssertEqual(methods, ["GET"])
        XCTAssertNil(sut.activeSessionId)
    }

    func testResumeLastSessionReportsConfirmedDeletionWithoutCreatingInsideResolver() {
        var methods: [String] = []
        MockURLProtocol.handler = { request in
            methods.append(request.httpMethod ?? "GET")
            return (response(for: request, status: 404), jsonData(["error": "missing"]))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        let settled = expectation(description: "deleted session reported missing")
        sut.resumeLastSession(preferredSessionId: "deleted") { result in
            XCTAssertEqual(result, .missing)
            settled.fulfill()
        }
        wait(for: [settled], timeout: 2)
        XCTAssertEqual(methods, ["GET"])
    }

    func testResumeFallbackTreatsMalformedSessionListAsUnavailable() {
        var methods: [String] = []
        MockURLProtocol.handler = { request in
            methods.append(request.httpMethod ?? "GET")
            return (response(for: request), Data("not session json".utf8))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        let settled = expectation(description: "malformed list fails closed")
        sut.resumeLastSession(preferredSessionId: nil) { result in
            XCTAssertEqual(result, .unavailable)
            settled.fulfill()
        }
        wait(for: [settled], timeout: 2)
        XCTAssertEqual(methods, ["GET"])
        XCTAssertNil(sut.activeSessionId)
    }

    func testStartupReplacementDoesNotCreateAfterTheOwnerSelectedAnotherSession() {
        var requestCount = 0
        MockURLProtocol.handler = { request in
            requestCount += 1
            return (response(for: request), jsonData([:]))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        sut.selectSession("owner-selection")

        let settled = expectation(description: "replacement skipped")
        sut.newSessionIfNothingSelected { sessionId in
            XCTAssertNil(sessionId)
            settled.fulfill()
        }

        wait(for: [settled], timeout: 2)
        XCTAssertEqual(requestCount, 0)
        XCTAssertEqual(sut.activeSessionId, "owner-selection")
    }

    func testThreadFetchAcceptsBareSessionArray() {
        MockURLProtocol.handler = { request in
            if request.url!.path.hasSuffix("ui-threads") { return (response(for: request), jsonData(["threads": []])) }
            return (response(for: request), jsonData([self.sessionJSON(id: "bare", updated: 1)]))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        let finished = expectation(description: "fetch finished")
        sut.$isLoading.dropFirst().filter { !$0 }.sink { _ in finished.fulfill() }.store(in: &cancellables)
        sut.fetchData()
        wait(for: [finished], timeout: 2)
        XCTAssertEqual(sut.sessions.map(\.id), ["bare"])
    }

    func testNewSessionSelectsReturnedIDAndUsesPOST() {
        let created = expectation(description: "new session callback")
        MockURLProtocol.handler = { request in
            if request.url!.path.hasSuffix("chat/new") {
                XCTAssertEqual(request.httpMethod, "POST")
                XCTAssertEqual(requestBody(request).flatMap { String(data: $0, encoding: .utf8) }, "{}")
                let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
                XCTAssertEqual(query.first(where: { $0.name == "ui_thread_id" })?.value, "general")
                XCTAssertEqual(query.first(where: { $0.name == "history_lane" })?.value, "personal")
                return (response(for: request), jsonData([
                    "session": self.sessionJSON(id: "fresh", updated: 1)
                ]))
            }
            if request.url!.path.hasSuffix("ui-threads") { return (response(for: request), jsonData(["threads": []])) }
            return (response(for: request), jsonData(["sessions": []]))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        let fetchFinished = expectation(description: "post-create refresh finished")
        sut.$isLoading.dropFirst().filter { !$0 }.sink { _ in fetchFinished.fulfill() }.store(in: &cancellables)
        sut.newSession { id in
            XCTAssertEqual(id, "fresh")
            created.fulfill()
        }
        wait(for: [created, fetchFinished], timeout: 2)
        XCTAssertEqual(sut.activeSessionId, "fresh")
        sut.selectThread("work")
        sut.selectSession("selected")
        XCTAssertEqual(sut.activeThreadId, "work")
        XCTAssertEqual(sut.activeSessionId, "selected")
    }

    func testHealthParsesHealthyAliasesAndNon200SetsOffline() {
        let session = mockSession()
        var status = 200
        MockURLProtocol.handler = { request in
            (response(for: request, status: status), jsonData(["magicutor": "online", "tauri_status": "healthy"]))
        }
        let sut = HealthViewModel(networkSession: session, startsPolling: false)
        let healthy = expectation(description: "healthy")
        sut.$tauriStatus.dropFirst().filter { $0 }.sink { _ in healthy.fulfill() }.store(in: &cancellables)
        sut.checkHealth()
        wait(for: [healthy], timeout: 2)
        XCTAssertTrue(sut.magicianStatus)
        XCTAssertTrue(sut.magicutorStatus)
        XCTAssertEqual(sut.magicianState, .online)
        XCTAssertEqual(sut.magicutorState, .online)
        XCTAssertEqual(sut.tauriState, .online)

        status = 503
        let offline = expectation(description: "offline")
        sut.$magicianStatus.dropFirst().filter { !$0 }.sink { _ in offline.fulfill() }.store(in: &cancellables)
        sut.checkHealth()
        wait(for: [offline], timeout: 2)
        XCTAssertFalse(sut.tauriStatus)
        XCTAssertEqual(sut.magicianState, .offline)
        XCTAssertEqual(sut.magicutorState, .notReported)
        XCTAssertEqual(sut.tauriState, .notReported)
    }

    func testHealthVersionsPreferTheNestedCoreVersionsAndToleratePartialBodies() throws {
        let current = try XCTUnwrap(HealthVersions(data: jsonData([
            "status": "ok", "service": "magician",
            "version": "0.7.146", "api_version": "0.3.67",
            "magician": "healthy", "magicutor_status": "healthy", "tauri_status": "healthy",
            "versions": [
                "magician": "0.7.146", "magician_api": "0.3.67",
                "magicutor": "0.1.94", "desktop": "0.3.28",
            ],
        ])))
        XCTAssertEqual(current, HealthVersions(
            magician: "0.7.146", magicianAPI: "0.3.67", magicutor: "0.1.94", desktop: "0.3.28"
        ))

        // Offline components arrive as null or not at all; unknown keys are ignored.
        let partial = try XCTUnwrap(HealthVersions(data: Data("""
        {"version":"0.7.146","versions":{"magician":"0.7.150","magicutor":null,"desktop":"","future":{"x":1}},"extra":[1]}
        """.utf8)))
        XCTAssertEqual(partial.magician, "0.7.150")
        XCTAssertNil(partial.magicianAPI)
        XCTAssertNil(partial.magicutor)
        XCTAssertNil(partial.desktop)

        // Older servers send only `version`, and status keys are never read as versions.
        let older = try XCTUnwrap(HealthVersions(data: jsonData([
            "status": "ok", "version": "0.3.67", "magicutor": "online", "tauri": "healthy",
        ])))
        XCTAssertEqual(older, HealthVersions(magician: "0.3.67"))

        // A malformed `versions` value or a non-object body does not fail the rest.
        let odd = try XCTUnwrap(HealthVersions(data: Data(#"{"version":7,"api_version":"0.3.67","versions":"n/a"}"#.utf8)))
        XCTAssertEqual(odd, HealthVersions(magicianAPI: "0.3.67"))
        XCTAssertNil(HealthVersions(data: Data("[]".utf8)))
        XCTAssertNil(HealthVersions(data: Data("not json".utf8)))
    }

    func testChatViewModelLocalStateDoesNotNeedNetwork() {
        let sut = ChatViewModel(networkSession: mockSession(), connectsOnInit: false)
        XCTAssertTrue(sut.messages.isEmpty)
        sut.messages.append(ChatMessage(id: "delete-me", isUser: true, text: "Hi", type: .text))
        sut.deleteMessage("delete-me")
        XCTAssertFalse(sut.messages.contains { $0.id == "delete-me" })
        sut.messages.append(ChatMessage(id: "clear-me", isUser: true, text: "Hi", type: .text))
        sut.clearMessages()
        XCTAssertTrue(sut.messages.isEmpty)

        let attachment = StagedAttachment(id: UUID(), remoteId: nil, filename: "a.txt", mime: "text/plain", thumbnail: nil, uploading: true)
        sut.stagedAttachments = [attachment]
        sut.removeStagedAttachment(attachment.id)
        XCTAssertTrue(sut.stagedAttachments.isEmpty)
        sut.startNewSession(nil)
        XCTAssertNil(sut.currentSessionIdValue)
    }

    func testChatModelsDecodeAndSelectConfiguredDefault() {
        let keys = ["magios.chat.profile", ChatModelPreference.storageKey, "magios.chat.harnessEngine", ChatViewModel.localStorageKey(origin: MagicianAccess.baseURL.absoluteString, principal: MagicianAccess.principal, workspace: MagicianAccess.workspace, session: nil) + "|" + ChatModelPreference.storageKey]
        let original = keys.map { UserDefaults.standard.object(forKey: $0) }
        for key in keys { UserDefaults.standard.removeObject(forKey: key) }
        defer {
            for (key, value) in zip(keys, original) {
                if let value { UserDefaults.standard.set(value, forKey: key) }
                else { UserDefaults.standard.removeObject(forKey: key) }
            }
        }
        MockURLProtocol.handler = { request in
            XCTAssertTrue(request.url!.path.hasSuffix("chat/models"))
            XCTAssertEqual(URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems?.first(where: { $0.name == "engine" })?.value, "magician")
            return (response(for: request), jsonData([
                "models": [
                    ["id": "test/small", "provider": "test", "connection": "test", "model": "small", "label": "Small",
                     "capabilities": ["vision": false], "thinking": ["mode": "none", "levels": []], "choices": ["auto": "small-auto"]],
                    ["id": "test/large", "provider": "test", "connection": "test", "model": "large", "label": "Large",
                     "capabilities": ["vision": true], "thinking": ["mode": "none", "levels": []], "choices": ["auto": "large-auto"]]
                ],
                "default_selection": ["model_id": "test/large", "thinking": "auto"]
            ]))
        }
        let sut = ChatViewModel(networkSession: mockSession(), connectsOnInit: false)
        let published = expectation(description: "profiles")
        sut.$profiles.dropFirst().sink { if $0.count == 2 { published.fulfill() } }.store(in: &cancellables)
        sut.fetchProfiles()
        wait(for: [published], timeout: 2)
        XCTAssertEqual(sut.selectedProfile, "large-auto")
        XCTAssertEqual(sut.profiles.last?.modelSelection, ChatModelSelection(modelId: "test/large", thinking: "auto"))
        XCTAssertEqual(sut.profiles.first?.supportsUserImageInputs, false)
        XCTAssertNil(UserDefaults.standard.object(forKey: "magios.chat.profile"))
    }

    private func threadJSON(id: String, sort: Int, lane: String = "personal") -> [String: Any] {
        ["principal": "local", "workspace": "default", "id": id, "name": id,
         "archived": false, "sort_order": sort, "history_lane": lane,
         "created_at": 1, "updated_at": 1]
    }

    // MARK: Session delete feedback

    func testSessionDeleteShowsDeletingUntilTheServerAnswersAndBlocksASecond() throws {
        var deletes: [String] = []
        MockURLProtocol.handler = { request in
            if request.httpMethod == "DELETE" {
                deletes.append(request.url!.lastPathComponent)
                return (response(for: request, status: 204), Data())
            }
            return (response(for: request), jsonData(["sessions": [], "total": 0]))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        sut.sessions = try [sessionJSON(id: "s1", updated: 2), sessionJSON(id: "s2", updated: 1)].map {
            try JSONDecoder().decode(ChatSession.self, from: jsonData($0))
        }
        sut.activeSessionId = "s1"
        var deleted: [String] = []

        sut.deleteSession("s1") { deleted.append($0) }
        XCTAssertEqual(sut.deletingSessionId, "s1")
        XCTAssertTrue(deleted.isEmpty, "the open chat must not move before the server answers")
        sut.deleteSession("s2") { deleted.append($0) }
        XCTAssertEqual(sut.deletingSessionId, "s1")

        waitUntil { sut.deletingSessionId == nil }
        XCTAssertEqual(deletes, ["s1"])
        XCTAssertEqual(deleted, ["s1"])
        XCTAssertNil(sut.activeSessionId)
    }

    func testFailedSessionDeleteKeepsTheRowAndShowsTheError() throws {
        MockURLProtocol.handler = { request in
            if request.httpMethod == "DELETE" {
                return (response(for: request, status: 500), jsonData(["error": "Delete failed"]))
            }
            XCTFail("a failed delete must not refetch the list")
            return (response(for: request), Data())
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        sut.sessions = [try JSONDecoder().decode(ChatSession.self, from: jsonData(sessionJSON(id: "s1", updated: 2)))]
        var deleted = false

        sut.deleteSession("s1") { _ in deleted = true }
        waitUntil { sut.deletingSessionId == nil }

        XCTAssertFalse(deleted)
        XCTAssertEqual(sut.errorMessage, "Delete failed")
        XCTAssertEqual(sut.sessions.map(\.id), ["s1"])
    }

    func testThreadArchiveShowsArchivingUntilTheServerAnswers() {
        var patches = 0
        MockURLProtocol.handler = { request in
            if request.httpMethod == "PATCH" {
                patches += 1
                XCTAssertEqual(request.timeoutInterval, ThreadViewModel.threadMutationTimeout)
                return (response(for: request), Data("{}".utf8))
            }
            return (response(for: request), jsonData(["sessions": [], "total": 0]))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        sut.activeThreadId = "travel"
        sut.archiveThread("general")
        XCTAssertNil(sut.mutatingThreadId)

        sut.archiveThread("travel")
        XCTAssertEqual(sut.mutatingThreadId, "travel")
        XCTAssertEqual(sut.mutatingThreadLabel, "Archiving…")
        XCTAssertEqual(sut.activeThreadId, "travel")
        sut.deleteThread("plans")
        XCTAssertEqual(sut.mutatingThreadId, "travel")

        waitUntil { sut.mutatingThreadId == nil }
        XCTAssertEqual(patches, 1)
        XCTAssertNil(sut.mutatingThreadLabel)
        XCTAssertEqual(sut.activeThreadId, "general")

        sut.unarchiveThread("travel")
        XCTAssertEqual(sut.mutatingThreadLabel, "Restoring…")
        waitUntil { sut.mutatingThreadId == nil }
        XCTAssertEqual(patches, 2)
    }

    func testThreadDeleteShowsDeletingUntilTheServerAnswers() {
        var deletes = 0
        MockURLProtocol.handler = { request in
            if request.httpMethod == "DELETE" {
                deletes += 1
                XCTAssertEqual(request.timeoutInterval, ThreadViewModel.threadMutationTimeout)
                return (response(for: request, status: 204), Data())
            }
            return (response(for: request), jsonData(["sessions": [], "total": 0]))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        sut.activeThreadId = "travel"
        sut.deleteThread("travel")
        XCTAssertEqual(sut.mutatingThreadId, "travel")
        XCTAssertEqual(sut.mutatingThreadLabel, "Deleting…")
        XCTAssertEqual(sut.activeThreadId, "travel")

        waitUntil { sut.mutatingThreadId == nil }
        XCTAssertEqual(deletes, 1)
        XCTAssertNil(sut.mutatingThreadLabel)
        XCTAssertEqual(sut.activeThreadId, "general")
    }

    func testFailedThreadArchiveClearsTheRowAndShowsTheError() {
        MockURLProtocol.handler = { request in
            if request.httpMethod == "PATCH" {
                return (response(for: request, status: 500), jsonData(["error": "Archive failed"]))
            }
            XCTFail("a failed archive must not refetch the list")
            return (response(for: request), Data())
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        sut.archiveThread("travel")
        waitUntil { sut.mutatingThreadId == nil }
        XCTAssertNil(sut.mutatingThreadLabel)
        XCTAssertEqual(sut.errorMessage, "Archive failed")
    }

    // MARK: Live history refresh

    func testOnlyAWorkingFlipOfASessionOnBothPollsRefreshes() {
        let ids: Set<String> = ["a", "b"]
        XCTAssertFalse(ThreadViewModel.activityChangeNeedsRefresh(
            previous: nil, currentIds: ids, currentWorking: ["a"]))
        XCTAssertFalse(ThreadViewModel.activityChangeNeedsRefresh(
            previous: (ids, ["a"]), currentIds: ids, currentWorking: ["a"]))
        XCTAssertTrue(ThreadViewModel.activityChangeNeedsRefresh(
            previous: (ids, ["a"]), currentIds: ids, currentWorking: []))
        XCTAssertTrue(ThreadViewModel.activityChangeNeedsRefresh(
            previous: (ids, []), currentIds: ids, currentWorking: ["b"]))
        // "c" only scrolled onto the page: not a change.
        XCTAssertFalse(ThreadViewModel.activityChangeNeedsRefresh(
            previous: (ids, ["a"]), currentIds: ["a", "c"], currentWorking: ["a", "c"]))
    }

    func testQuietRefreshWaitsAPollCycleAfterTheLastOne() {
        let now = Date()
        XCTAssertEqual(ThreadViewModel.quietRefreshDelay(since: nil, now: now), 0.25)
        XCTAssertEqual(ThreadViewModel.quietRefreshDelay(since: now.addingTimeInterval(-1), now: now), 2, accuracy: 0.001)
        XCTAssertEqual(ThreadViewModel.quietRefreshDelay(since: now.addingTimeInterval(-10), now: now), 0.25)
    }

    @MainActor
    func testWorkingFlipRefetchesThePageWithoutBlankingIt() async throws {
        var working = ["a"]
        var listFetches = 0
        MockURLProtocol.handler = { request in
            if request.url?.path.hasSuffix("/chat/activity") == true {
                return (response(for: request), jsonData(["working_session_ids": working]))
            }
            listFetches += 1
            return (response(for: request), jsonData([
                "sessions": [self.sessionJSON(id: "a", updated: 9), self.sessionJSON(id: "b", updated: 1)],
                "total": 2
            ]))
        }
        let sut = ThreadViewModel(networkSession: mockSession())
        sut.sessions = try [sessionJSON(id: "b", updated: 5), sessionJSON(id: "a", updated: 1)].map {
            try JSONDecoder().decode(ChatSession.self, from: jsonData($0))
        }

        await sut.refreshActivity()
        XCTAssertEqual(sut.workingSessionIds, ["a"])
        working = []
        await sut.refreshActivity()

        XCTAssertFalse(sut.isLoading)
        XCTAssertEqual(sut.sessions.map(\.id), ["b", "a"], "rows stay until the refetch lands")
        let refreshed = expectation(description: "quiet refetch landed")
        func poll() {
            if sut.sessions.first?.id == "a" { refreshed.fulfill(); return }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.05, execute: poll)
        }
        poll()
        await fulfillment(of: [refreshed], timeout: 3)
        XCTAssertEqual(listFetches, 1)
        XCTAssertFalse(sut.isLoading)
        XCTAssertNil(sut.errorMessage)
    }

    private func sessionJSON(
        id: String,
        updated: Int,
        lane: String = "personal",
        isDefault: Bool = false
    ) -> [String: Any] {
        ["id": id, "principal": "local", "workspace": "default", "agent_id": "agent",
         "ui_thread_id": "general", "status": "active", "history_lane": lane,
         "is_default_session": isDefault, "created_at": 1, "updated_at": updated]
    }

    private func mockSession() -> URLSession {
        let session = makeMockSession()
        sessions.append(session)
        return session
    }
}
