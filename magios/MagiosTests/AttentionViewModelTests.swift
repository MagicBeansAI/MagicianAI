import XCTest
@testable import Magician

final class AttentionViewModelTests: XCTestCase {
    override func setUp() {
        super.setUp()
        PendingHitlTracker.shared.reset()
    }

    override func tearDown() {
        MockURLProtocol.handler = { req in (response(for: req), jsonData([:])) }
        DeferredMockURLProtocol.handler = nil
        PendingHitlTracker.shared.reset()
        super.tearDown()
    }

    private func item(correlationId: String = "corr-1", inputType: String = "text") -> AttentionItem {
        let dict: [String: Any] = [
            "id": "item-1", "title": "Q?", "item_type": "agentic", "status": "pending",
            "metadata": ["input_type": inputType, "pause_state_id": correlationId, "question": "Q?"]
        ]
        return try! JSONDecoder().decode(AttentionItem.self, from: JSONSerialization.data(withJSONObject: dict))
    }

    // MARK: - parseResolved (pure NDJSON pairing)

    private func pendingQuestion(_ id: String = "ledger-choice") -> [String: Any] {
        ["id": id, "question": "Choose a color", "created_at": 1234,
         "options": [["id": "blue", "label": "Blue"]],
         "context": ["input_type": "choice", "input_schema": ["allow_other": true]]]
    }

    func testLedgerQuestionUsesExistingChoiceResponseAndDisappearsAfterSuccess() throws {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        var answered = false
        var ledgerReads = 0
        MockURLProtocol.handler = { request in
            if request.httpMethod == "POST" {
                XCTAssertEqual(request.url?.path, "/api/magician/v2/hitl/ledger-choice/respond")
                let body = try XCTUnwrap(requestBody(request))
                let payload = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
                XCTAssertEqual(payload["source"] as? String, "user_request")
                XCTAssertEqual(payload["channel"] as? String, "ios")
                XCTAssertEqual(payload["correlation_id"] as? String, "ledger-choice")
                XCTAssertEqual((payload["value"] as? [String: Any])?["selected_id"] as? String, "blue")
                answered = true
                return (response(for: request), jsonData([:]))
            }
            if request.url?.path.hasSuffix("/user-requests") == true {
                ledgerReads += 1
                XCTAssertEqual(request.httpMethod, "GET")
                XCTAssertNil(request.value(forHTTPHeaderField: "X-Principal"))
                XCTAssertNil(request.value(forHTTPHeaderField: "X-Workspace"))
                return (response(for: request), jsonData(["requests": answered ? [] : [self.pendingQuestion()]]))
            }
            return (response(for: request), jsonData(["requests": []]))
        }
        vm.fetch()
        waitUntil { !vm.isLoading }
        let question = try XCTUnwrap(vm.items("requests").first)
        XCTAssertEqual(question.id, "user-request:ledger-choice")
        XCTAssertEqual(question.inputType, "choice")
        XCTAssertEqual(question.options.first?.id, "blue")
        XCTAssertEqual(question.metadata?.inputSchema?.allowOther, true)
        XCTAssertEqual(question.updatedAt, 1234)
        XCTAssertEqual(vm.badgeCount, 1)
        XCTAssertNil(vm.loadError)
        vm.submitChoice(question, optionId: "blue", otherValue: nil)
        waitUntil { answered && ledgerReads >= 2 && !vm.isLoading && vm.pendingCardMutationKeys.isEmpty }
        XCTAssertTrue(vm.items("requests").isEmpty)
        XCTAssertEqual(vm.badgeCount, 0)
    }

    func testPendingLedgerDeduplicatesFeedCorrelationsAndRepeatedLedgerIDs() throws {
        let feed = try JSONDecoder().decode(FeedAttentionResponse.self, from: jsonData([
            "approvals": [["id": "feed-choice", "title": "Existing", "item_type": "request",
                           "status": "needs_action", "metadata": ["source": "user_request",
                           "hitl_request": ["identifiers": ["correlation_id": "already-projected"]]]]],
            "counts": ["needs_action": 1, "approvals": 1],
            "pages": ["requests": ["total": 0, "next_cursor": "cursor", "has_more": true]]
        ]))
        let pending = try JSONDecoder().decode(PendingUserRequestsResponse.self, from: jsonData([
            "requests": [pendingQuestion("already-projected"), pendingQuestion(), pendingQuestion()]
        ]))
        let result = feed.withPendingUserRequests(pending.requests)
        XCTAssertEqual(result.requests.map(\.correlationId), ["ledger-choice"])
        XCTAssertEqual(result.approvals.map(\.id), ["feed-choice"])
        XCTAssertEqual(result.counts?.needsAction, 2)
        XCTAssertEqual(result.pages?.requests?.total, 1)
        XCTAssertEqual(result.pages?.requests?.nextCursor, "cursor")
    }

    func testLedgerFailureRetainsLastSnapshotAndSuccessfulRefreshClearsIt() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        var phase = 0
        MockURLProtocol.handler = { request in
            if request.url?.path.hasSuffix("/user-requests") == true {
                if phase == 1 { return (response(for: request, status: 503), jsonData(["requests": []])) }
                return (response(for: request), jsonData(["requests": phase == 0 ? [self.pendingQuestion()] : []]))
            }
            return (response(for: request), jsonData(["requests": []]))
        }
        vm.fetch()
        waitUntil { !vm.isLoading }
        XCTAssertEqual(vm.items("requests").count, 1)
        phase = 1
        vm.fetch()
        waitUntil { !vm.isLoading }
        XCTAssertEqual(vm.items("requests").count, 1)
        XCTAssertNotNil(vm.loadError)
        phase = 2
        vm.fetch()
        waitUntil { !vm.isLoading }
        XCTAssertTrue(vm.items("requests").isEmpty)
        XCTAssertNil(vm.loadError)
    }

    func testMalformedLedgerIsAnErrorInsteadOfAnEmptyInbox() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        MockURLProtocol.handler = { request in
            let body: [String: Any] = request.url?.path.hasSuffix("/user-requests") == true
                ? ["requests": [["id": "broken-missing-question"]]] : ["requests": []]
            return (response(for: request), jsonData(body))
        }
        vm.fetch()
        waitUntil { !vm.isLoading }
        XCTAssertNotNil(vm.loadError)
        XCTAssertTrue(vm.items("requests").isEmpty)
    }

    func testFeedPageReplacesLedgerRowUsingTheResponseCorrelation() throws {
        let pending = try JSONDecoder().decode(PendingUserRequest.self, from: jsonData(pendingQuestion()))
        let projected = AttentionItem(pending: pending)
        let feed = try JSONDecoder().decode(AttentionItem.self, from: jsonData([
            "id": "persisted-feed-id", "title": "Choose a color", "item_type": "request", "status": "needs_action",
            "metadata": ["source": "user_request", "pause_state_id": pending.id]
        ]))
        XCTAssertEqual(AttentionViewModel.mergedAttentionItems([projected], [feed]).map(\.id), [feed.id])
        XCTAssertEqual(AttentionViewModel.mergedAttentionItems([feed], [projected]).map(\.id), [feed.id])
        XCTAssertEqual(AttentionViewModel.mergedAttentionItems([projected], [projected]).count, 1)
    }

    func testPagedFeedReconcilesLedgerProjectionAndBadgeWithoutDuplicatingQuestion() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        MockURLProtocol.handler = { request in
            if request.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: request), jsonData(["requests": [self.pendingQuestion()]]))
            }
            if request.url?.query?.contains("requests_cursor") == true {
                return (response(for: request), jsonData([
                    "requests": [["id": "feed-id", "title": "Choose a color", "item_type": "request", "status": "needs_action",
                                   "metadata": ["source": "user_request", "pause_state_id": "ledger-choice"]]],
                    "pages": ["requests": ["total": 1, "has_more": false]],
                    "counts": ["requests": 1, "needs_action": 1]
                ]))
            }
            return (response(for: request), jsonData([
                "requests": [], "pages": ["requests": ["total": 1, "has_more": true, "next_cursor": "next"]],
                "counts": ["requests": 1, "needs_action": 1]
            ]))
        }
        vm.fetch()
        waitUntil { !vm.isLoading }
        XCTAssertEqual(vm.items("requests").map(\.id), ["user-request:ledger-choice"])
        vm.loadMore("requests")
        waitUntil { !vm.isLoadingPage("requests") }
        XCTAssertEqual(vm.items("requests").map(\.id), ["feed-id"])
        XCTAssertEqual(vm.total("requests"), 1)
        XCTAssertEqual(vm.badgeCount, 1)
    }

    func testParseResolvedPairsByCorrelationId() {
        let ndjson = """
        {"event_type":"HitlRequested","timestamp_ms":1000,"data":{"correlation_id":"c1","question":"Q1"}}
        {"event_type":"HitlResolved","timestamp_ms":2000,"data":{"correlation_id":"c1","outcome":"approved","decision":"yes"}}
        """
        let rows = AttentionViewModel.parseResolved(ndjson)
        XCTAssertEqual(rows.count, 1)
        XCTAssertEqual(rows[0].correlationId, "c1")
        XCTAssertEqual(rows[0].prompt, "Q1")
        XCTAssertEqual(rows[0].outcome, "approved")
        XCTAssertEqual(rows[0].decision, "yes")
        XCTAssertEqual(rows[0].resolvedAt, 2000)
    }

    func testParseResolvedSkipsResolvedWithoutRequest() {
        let ndjson = #"{"event_type":"HitlResolved","timestamp_ms":2000,"data":{"correlation_id":"orphan","outcome":"x"}}"#
        XCTAssertTrue(AttentionViewModel.parseResolved(ndjson).isEmpty)
    }

    func testParseResolvedSortsNewestFirst() {
        let ndjson = """
        {"event_type":"HitlRequested","timestamp_ms":10,"data":{"correlation_id":"a","question":"A"}}
        {"event_type":"HitlResolved","timestamp_ms":100,"data":{"correlation_id":"a","outcome":"o"}}
        {"event_type":"HitlRequested","timestamp_ms":20,"data":{"correlation_id":"b","question":"B"}}
        {"event_type":"HitlResolved","timestamp_ms":300,"data":{"correlation_id":"b","outcome":"o"}}
        """
        XCTAssertEqual(AttentionViewModel.parseResolved(ndjson).map(\.correlationId), ["b", "a"])
    }

    func testParseResolvedNestedIdentifiers() {
        let ndjson = """
        {"event_type":"HitlRequested","timestamp_ms":1,"data":{"hitl_request":{"identifiers":{"correlation_id":"nested"}},"question":"N"}}
        {"event_type":"HitlResolved","timestamp_ms":2,"data":{"correlation_id":"nested","outcome":"done"}}
        """
        let rows = AttentionViewModel.parseResolved(ndjson)
        XCTAssertEqual(rows.map(\.correlationId), ["nested"])
        XCTAssertEqual(rows.first?.prompt, "N")
    }

    func testParseResolvedIgnoresBlankAndControlLines() {
        let ndjson = """

        {"event_type":"__events_start"}
        {"event_type":"HitlRequested","timestamp_ms":1,"data":{"correlation_id":"c","question":"Q"}}
        not-json
        {"event_type":"HitlResolved","timestamp_ms":2,"data":{"correlation_id":"c","outcome":"ok"}}
        """
        XCTAssertEqual(AttentionViewModel.parseResolved(ndjson).count, 1)
    }

    func testCorrelationIdHelperPrecedence() {
        XCTAssertEqual(AttentionViewModel.correlationId(from: ["correlation_id": "c", "pause_state_id": "p"]), "c")
        XCTAssertEqual(AttentionViewModel.correlationId(from: ["pause_state_id": "p"]), "p")
        XCTAssertEqual(AttentionViewModel.correlationId(from: ["approval_id": "ap"]), "ap")
        XCTAssertNil(AttentionViewModel.correlationId(from: ["something": "else"]))
    }

    // MARK: - Response payloads

    func testFetchPopulatesLanes() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: req), jsonData(["requests": []]))
            }
            return (response(for: req), jsonData([
                "requests": [["id": "r1", "title": "R", "item_type": "agentic", "status": "pending"]],
                "approvals": [], "escalations": [], "failed": [], "running": []
            ]))
        }
        vm.fetch()
        waitUntil { !vm.items("requests").isEmpty }
        XCTAssertEqual(vm.items("requests").map(\.id), ["r1"])
    }

    /// Wire contract for the row stamp: the feed sends epoch millis as
    /// `updated_at` (web reads the same field as millis) and rows without it
    /// must still decode.
    func testFetchDecodesUpdatedAtMillis() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: req), jsonData(["requests": []]))
            }
            return (response(for: req), jsonData([
                "failed": [
                    ["id": "f1", "title": "F", "item_type": "task", "status": "failed",
                     "updated_at": 1_762_000_000_123.0],
                    ["id": "f2", "title": "F2", "item_type": "task", "status": "failed"]
                ],
                "requests": [], "approvals": [], "escalations": [], "running": []
            ]))
        }
        vm.fetch()
        waitUntil { vm.items("failed").count == 2 }
        XCTAssertEqual(vm.items("failed")[0].updatedAt!, 1_762_000_000_123.0, accuracy: 1)
        XCTAssertNil(vm.items("failed")[1].updatedAt)
    }

    func testServerTotalsAndAllLaneLoadMoreAdvanceFeedCursors() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: req), jsonData(["requests": []]))
            }
            let components = URLComponents(url: req.url!, resolvingAgainstBaseURL: false)
            let query = Dictionary(uniqueKeysWithValues: (components?.queryItems ?? []).map { ($0.name, $0.value ?? "") })
            if query["requests_cursor"] == "request+/next=" {
                return (response(for: req), jsonData([
                    "requests": [["id": "r2", "title": "R2", "item_type": "agentic", "status": "pending"]],
                    "approvals": [], "escalations": [], "failed": [], "running": [],
                    "pages": ["requests": ["total": 41, "has_more": false]],
                ]))
            }
            return (response(for: req), jsonData([
                "requests": [["id": "r1", "title": "R1", "item_type": "agentic", "status": "pending"]],
                "approvals": [], "escalations": [], "failed": [], "running": [],
                "pages": [
                    "requests": ["total": 41, "next_cursor": "request+/next=", "has_more": true],
                    "approvals": ["total": 2, "has_more": false],
                    "escalations": ["total": 0, "has_more": false],
                    "failed": ["total": 3, "has_more": false],
                ],
                "counts": ["needs_action": 43, "failed": 3, "requests": 41, "approvals": 2],
            ]))
        }

        vm.fetch()
        waitUntil { !vm.isLoading }
        XCTAssertEqual(vm.total("requests"), 41)
        // `all` owns the four HITL lanes ONLY (41 + 2 + 0 + 3) so that the tab
        // total equals the badge and the list it opens.
        XCTAssertEqual(vm.total("all"), 46)
        XCTAssertTrue(vm.hasMore("all"))

        vm.loadMore("all")
        waitUntil { vm.items("requests").count == 2 && !vm.isLoadingPage("all") }
        XCTAssertEqual(vm.items("requests").map(\.id), ["r1", "r2"])
        XCTAssertEqual(vm.total("all"), 46)
        XCTAssertFalse(vm.hasMore("all"))
    }

    // Badge count uses the SAME formula as the web
    // (ui/unified-ui/src/lib/attention/attentionBadgeCount.test.ts).
    func testBadgeFormulaMatchesWeb() {
        XCTAssertEqual(AttentionViewModel.resolveAttentionBadgeCount(pendingHitl: 4, needsAction: 5, failed: 0), 5)
        XCTAssertEqual(AttentionViewModel.resolveAttentionBadgeCount(pendingHitl: 7, needsAction: 5, failed: 2), 9)
        XCTAssertEqual(AttentionViewModel.resolveAttentionBadgeCount(pendingHitl: -1, needsAction: -2, failed: -3), 0)
        // iOS path (no realtime pending-HITL) == needs_action + failed.
        XCTAssertEqual(AttentionViewModel.resolveAttentionBadgeCount(pendingHitl: 0, needsAction: 3, failed: 2), 5)
    }

    // The badge reads the backend `counts` from the feed response.
    func testFetchComputesBadgeFromBackendCounts() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: req), jsonData(["requests": []]))
            }
            return (response(for: req), jsonData([
                "requests": [], "approvals": [], "escalations": [], "failed": [], "running": [],
                "counts": ["needs_action": 3, "failed": 2, "approvals": 1],
            ]))
        }
        vm.fetch()
        waitUntil { vm.badgeCount == 5 }   // max(0,0,3) + max(0,2) = 5
        XCTAssertEqual(vm.counts?.needsAction, 3)
        XCTAssertEqual(vm.counts?.failed, 2)
        XCTAssertEqual(vm.badgeCount, 5)
    }

    func testSubmitChoicePostsToRespond() {
        assertRespond(expectedValue: ["type": "choice", "selected_id": "opt-1"]) {
            $0.submitChoice(self.item(), optionId: "opt-1")
        }
    }

    func testSubmitConfirmationPayload() {
        assertRespond(expectedValue: ["type": "confirmation", "confirmed": true]) {
            $0.submitConfirmation(self.item(inputType: "confirmation"), confirmed: true)
        }
    }

    func testSubmitPasswordPayload() {
        assertRespond(expectedValue: ["type": "password", "value": "hunter2"]) {
            $0.submitPassword(self.item(inputType: "password"), "hunter2")
        }
    }

    func testHitlResponseRemovesCardAndBadgeBeforeNetworkThenRollsBack() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let target = item(correlationId: "corr-1", inputType: "choice")
        vm.lanes["requests"] = [target]
        var counts = FeedAttentionCounts()
        counts.requests = 1
        counts.needsAction = 1
        vm.counts = counts
        PendingHitlTracker.shared.seed(correlationIds: ["corr-1"])
        vm.badgeCount = 1
        let requestStarted = expectation(description: "response request started")
        let requestMayFinish = DispatchSemaphore(value: 0)
        MockURLProtocol.handler = { request in
            if request.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: request), jsonData(["requests": []]))
            }
            requestStarted.fulfill()
            _ = requestMayFinish.wait(timeout: .now() + 2)
            return (response(for: request, status: 503), Data("unavailable".utf8))
        }

        vm.submitChoice(target, optionId: "yes")
        XCTAssertTrue(vm.items("requests").isEmpty,
                      "the HITL card must leave before the response API finishes")
        XCTAssertEqual(vm.total("requests"), 0)
        XCTAssertEqual(vm.badgeCount, 0)
        wait(for: [requestStarted], timeout: 2)
        requestMayFinish.signal()
        waitUntil { vm.pendingCardMutationKeys.isEmpty }

        XCTAssertEqual(vm.items("requests").map(\.id), ["item-1"])
        XCTAssertEqual(vm.total("requests"), 1)
        XCTAssertEqual(vm.badgeCount, 1)
        XCTAssertNotNil(vm.mutationError)
    }

    func testHitlReaskRestoresLiveCardInsteadOfCommittingTombstone() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let target = item(correlationId: "corr-1", inputType: "text")
        vm.lanes["requests"] = [target]
        var counts = FeedAttentionCounts()
        counts.requests = 1
        counts.needsAction = 1
        vm.counts = counts
        PendingHitlTracker.shared.seed(correlationIds: ["corr-1"])
        vm.badgeCount = 1
        MockURLProtocol.handler = { request in
            if request.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: request), jsonData(["requests": []]))
            }
            if request.httpMethod == "POST" {
                return (response(for: request), jsonData([
                    "resumed": false,
                    "status": "reask_required",
                    "message": "Please provide a full name",
                ]))
            }
            return (response(for: request), jsonData([
                "requests": [[
                    "id": "item-1", "title": "Q?", "item_type": "agentic", "status": "pending",
                    "metadata": ["input_type": "text", "pause_state_id": "corr-1", "question": "Q?"],
                ]],
                "approvals": [], "escalations": [], "failed": [], "running": [],
                "counts": ["requests": 1, "needs_action": 1],
            ]))
        }

        vm.submitText(target, "Ada")
        XCTAssertTrue(vm.items("requests").isEmpty)
        waitUntil { vm.pendingCardMutationKeys.isEmpty && !vm.isLoading }

        XCTAssertEqual(vm.items("requests").map(\.id), ["item-1"])
        XCTAssertEqual(vm.badgeCount, 1)
        XCTAssertTrue(vm.mutationError?.contains("Please provide a full name") == true)
    }

    func testOptimisticRefreshCountsUnrelatedNewAttentionWork() {
        let coordinator = CardMutationCoordinator()
        let vm = AttentionViewModel(networkSession: makeMockSession(), mutationCoordinator: coordinator)
        let target = item(correlationId: "corr-1", inputType: "choice")
        vm.lanes["requests"] = [target]
        var counts = FeedAttentionCounts()
        counts.requests = 1
        counts.needsAction = 1
        vm.counts = counts
        MockURLProtocol.handler = { request in
            if request.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: request), jsonData(["requests": []]))
            }
            if request.httpMethod == "POST" {
                return (response(for: request), jsonData(["resumed": true]))
            }
            return (response(for: request), jsonData([
                "requests": [
                    ["id": "item-1", "title": "Old", "item_type": "agentic", "status": "pending",
                     "metadata": ["input_type": "choice", "pause_state_id": "corr-1"]],
                    ["id": "item-2", "title": "New", "item_type": "agentic", "status": "pending",
                     "metadata": ["input_type": "text", "pause_state_id": "corr-2"]],
                ],
                "approvals": [], "escalations": [], "failed": [], "running": [],
                "pages": ["requests": ["total": 2, "has_more": false]],
                "counts": ["requests": 2, "needs_action": 2],
            ]))
        }

        vm.submitChoice(target, optionId: "yes")
        waitUntil { vm.pendingCardMutationKeys.isEmpty && !vm.isLoading }

        XCTAssertEqual(vm.items("requests").map(\.id), ["item-2"])
        XCTAssertEqual(vm.total("requests"), 1)
        XCTAssertEqual(vm.counts?.needsAction, 1)
        XCTAssertEqual(vm.badgeCount, 1)
    }

    func testExpiredTombstoneRevealsSameIdentityAlreadyFetchedDuringGrace() {
        let coordinator = CardMutationCoordinator()
        let vm = AttentionViewModel(networkSession: makeMockSession(), mutationCoordinator: coordinator)
        let reissued = item(correlationId: "corr-1")
        vm.counts = FeedAttentionCounts()
        vm.pages = AttentionPages(
            requests: LanePage(total: 0, nextCursor: nil, hasMore: false),
            approvals: nil, escalations: nil, failed: nil, running: nil
        )
        coordinator.commit(.hitl("corr-1"), suppressFor: 0.01)
        vm.lanes["requests"] = [reissued]

        XCTAssertTrue(vm.items("requests").isEmpty)
        waitUntil { vm.items("requests").map(\.id) == ["item-1"] }
        XCTAssertEqual(vm.counts?.requests, 1)
        XCTAssertEqual(vm.counts?.needsAction, 1)
        XCTAssertEqual(vm.total("requests"), 1)
        XCTAssertEqual(vm.badgeCount, 1)
    }

    func testPaginatedReissueIsRetainedBehindTombstoneUntilExpiry() {
        let coordinator = CardMutationCoordinator()
        let vm = AttentionViewModel(networkSession: makeMockSession(), mutationCoordinator: coordinator)
        let existing = try! JSONDecoder().decode(AttentionItem.self, from: jsonData([
            "id": "item-2", "title": "Existing", "item_type": "agentic", "status": "pending",
            "metadata": ["input_type": "text", "pause_state_id": "corr-2"],
        ]))
        vm.lanes["requests"] = [existing]
        vm.pages = AttentionPages(
            requests: LanePage(total: 1, nextCursor: "next", hasMore: true),
            approvals: nil, escalations: nil, failed: nil, running: nil
        )
        var counts = FeedAttentionCounts()
        counts.requests = 2
        counts.needsAction = 2
        vm.counts = counts
        vm.badgeCount = 2
        coordinator.commit(.hitl("corr-1"), suppressFor: 0.05)
        MockURLProtocol.handler = { request in
            if request.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: request), jsonData(["requests": []]))
            }
            return (response(for: request), jsonData([
                "requests": [[
                    "id": "item-1", "title": "Reissued", "item_type": "agentic", "status": "pending",
                    "metadata": ["input_type": "text", "pause_state_id": "corr-1"],
                ]],
                "approvals": [], "escalations": [], "failed": [], "running": [],
                "pages": ["requests": ["total": 2, "has_more": false]],
            ]))
        }

        vm.loadMore("requests")
        waitUntil { !vm.isLoadingPage("requests") && vm.lanes["requests"]?.count == 2 }
        XCTAssertEqual(vm.items("requests").map(\.id), ["item-2"])
        XCTAssertEqual(vm.total("requests"), 1)
        XCTAssertEqual(vm.counts?.requests, 1)
        XCTAssertEqual(vm.badgeCount, 1)
        waitUntil { vm.items("requests").map(\.id) == ["item-2", "item-1"] }
        XCTAssertEqual(vm.total("requests"), 2)
        XCTAssertEqual(vm.counts?.requests, 2)
        XCTAssertEqual(vm.badgeCount, 2)
    }

    func testDismissPostsItemId() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let dismissedItem = item()
        vm.lanes["failed"] = [dismissedItem]
        var counts = FeedAttentionCounts()
        counts.failed = 1
        vm.counts = counts
        vm.badgeCount = 1
        let exp = expectation(description: "dismiss")
        let requestMayFinish = DispatchSemaphore(value: 0)
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: req), jsonData(["requests": []]))
            }
            if req.httpMethod == "POST", req.url!.absoluteString.contains("/feed/attention/dismiss"),
               let body = requestBody(req),
               let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                XCTAssertEqual(json["item_id"] as? String, "item-1")
                exp.fulfill()
                _ = requestMayFinish.wait(timeout: .now() + 2)
            }
            return (response(for: req), jsonData([:]))
        }
        vm.dismiss(dismissedItem)
        XCTAssertTrue(vm.items("failed").isEmpty, "dismissal must be visible before the API returns")
        XCTAssertEqual(vm.total("failed"), 0)
        XCTAssertEqual(vm.badgeCount, 0)
        wait(for: [exp], timeout: 2)
        requestMayFinish.signal()
    }

    func testDismissFailureRestoresTheExactAttentionProjection() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let dismissedItem = item()
        vm.lanes["failed"] = [dismissedItem]
        var counts = FeedAttentionCounts()
        counts.failed = 1
        vm.counts = counts
        vm.badgeCount = 1
        let attempted = expectation(description: "dismiss failed")
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: req), jsonData(["requests": []]))
            }
            attempted.fulfill()
            return (response(for: req, status: 503), Data("unavailable".utf8))
        }

        vm.dismiss(dismissedItem)
        XCTAssertTrue(vm.items("failed").isEmpty)
        wait(for: [attempted], timeout: 2)
        waitUntil { vm.items("failed").map(\.id) == ["item-1"] }

        XCTAssertEqual(vm.items("failed").map(\.id), ["item-1"])
        XCTAssertEqual(vm.total("failed"), 1)
        XCTAssertEqual(vm.badgeCount, 1)
        XCTAssertNotNil(vm.mutationError)
    }

    // MARK: - Accumulating lists: a mutation must not cost the reader a page

    /// Query values a fixture saw, recorded off the loading thread.
    private final class QueryLog {
        private let lock = NSLock()
        private var storage: [String] = []
        func record(_ value: String) {
            lock.lock(); defer { lock.unlock() }
            storage.append(value)
        }
        var all: [String] {
            lock.lock(); defer { lock.unlock() }
            return storage
        }
    }

    /// THE BUG. `fetch()` restarted every lane at its first page, so dismissing
    /// a card the reader had paged in to find took the rest of that page down
    /// with it. This list ACCUMULATES — the refetch has to ask for the span the
    /// reader is holding, not for page one.
    func testDismissRefetchesTheLoadedSpanRatherThanTheFirstPage() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let pool = (1...60).map { "f\($0)" }
        let limits = QueryLog()
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: req), jsonData(["requests": []]))
            }
            if req.httpMethod == "POST" { return (response(for: req), jsonData([:])) }
            let items = URLComponents(url: req.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
            func query(_ name: String) -> String? { items.first { $0.name == name }?.value }
            let limit = Int(query("limit") ?? "") ?? 25
            let start = Int(query("failed_cursor") ?? "") ?? 0
            limits.record(String(limit))
            let page = Array(pool.dropFirst(start).prefix(limit))
            let next = start + page.count
            return (response(for: req), jsonData([
                "requests": [], "approvals": [], "escalations": [], "running": [],
                "failed": page.map {
                    ["id": $0, "title": $0, "item_type": "agentic", "status": "pending"]
                },
                "pages": ["failed": ["total": pool.count, "next_cursor": String(next),
                                     "has_more": next < pool.count]],
                "counts": ["needs_action": 0, "failed": pool.count],
            ]))
        }

        vm.fetch()
        waitUntil { vm.items("failed").count == 25 }
        vm.loadMore("failed")
        waitUntil { vm.items("failed").count == 50 }
        let target = vm.items("failed")[40]   // a card only the second page carries

        vm.dismiss(target)

        waitUntil { limits.all.count == 3 }
        XCTAssertEqual(limits.all, ["25", "25", "50"],
                       "the dismiss re-reads the two pages the reader loaded")
        waitUntil { vm.items("failed").count == 49 }
        XCTAssertFalse(vm.items("failed").contains { $0.id == target.id })
        XCTAssertTrue(vm.items("failed").contains { $0.id == "f50" },
                      "the second page is still here after the dismiss")
    }

    // MARK: - Bulk apply ("Approve all")

    private func diffItem(id: String, correlationId: String) -> AttentionItem {
        let dict: [String: Any] = [
            "id": id, "title": "Apply?", "item_type": "agentic", "status": "pending",
            "metadata": ["input_type": "diff_approval", "attention_kind": "diff_approval",
                         "pause_state_id": correlationId, "question": "Apply?"]
        ]
        return try! JSONDecoder().decode(AttentionItem.self, from: JSONSerialization.data(withJSONObject: dict))
    }

    func testDiffApprovalItemsFilter() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        vm.lanes["approvals"] = [diffItem(id: "d1", correlationId: "c1"), item(), diffItem(id: "d2", correlationId: "c2")]
        XCTAssertEqual(vm.diffApprovalItems(in: "approvals").map(\.id), ["d1", "d2"])   // text item excluded
        XCTAssertTrue(vm.diffApprovalItems(in: "escalations").isEmpty)
    }

    func testApproveAllDiffApprovalsPostsApplyForEachThenNotice() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        vm.lanes["approvals"] = [diffItem(id: "d1", correlationId: "c1"), diffItem(id: "d2", correlationId: "c2")]
        let exp = expectation(description: "two applies")
        exp.expectedFulfillmentCount = 2
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: req), jsonData(["requests": []]))
            }
            if req.httpMethod == "POST", req.url!.absoluteString.contains("/respond"),
               let body = requestBody(req),
               let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
               let value = json["value"] as? [String: Any],
               value["selected_id"] as? String == "apply" {
                exp.fulfill()
            }
            return (response(for: req), jsonData([:]))
        }
        vm.approveAllDiffApprovals(in: "approvals")
        XCTAssertTrue(vm.approvingAllDiffs)               // flips true synchronously
        XCTAssertTrue(vm.items("approvals").isEmpty,      // every card leaves before either POST completes
                      "bulk apply should use the same optimistic card contract")
        wait(for: [exp], timeout: 3)
        waitUntil { !vm.approvingAllDiffs && vm.bulkNotice != nil }
        XCTAssertEqual(vm.bulkNotice, "Applied 2 code change sets.")
    }

    func testApproveAllDiffApprovalsNoopWhenNoneAndWhenBusy() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        vm.approveAllDiffApprovals(in: "approvals")       // empty lane → no-op
        XCTAssertFalse(vm.approvingAllDiffs)
        XCTAssertNil(vm.bulkNotice)
        // Re-entrancy guard: already in flight → second call is ignored.
        vm.lanes["approvals"] = [diffItem(id: "d1", correlationId: "c1")]
        vm.approvingAllDiffs = true
        vm.approveAllDiffApprovals(in: "approvals")
        XCTAssertNil(vm.bulkNotice)
    }

    // Fire a respond action, assert the POST hits /hitl/{cid}/respond with the value dict.
    private func assertRespond(expectedValue: [String: Any], _ action: (AttentionViewModel) -> Void) {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let exp = expectation(description: "respond")
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: req), jsonData(["requests": []]))
            }
            if req.httpMethod == "POST", req.url!.absoluteString.contains("/hitl/corr-1/respond"),
               let body = requestBody(req),
               let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
               let value = json["value"] as? [String: Any] {
                for (k, v) in expectedValue { XCTAssertEqual(value[k] as? NSObject, v as? NSObject) }
                exp.fulfill()
            }
            return (response(for: req), jsonData([:]))
        }
        action(vm)
        wait(for: [exp], timeout: 2)
    }
}
