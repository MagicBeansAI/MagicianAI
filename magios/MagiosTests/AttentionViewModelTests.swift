import XCTest
@testable import Magician

final class AttentionViewModelTests: XCTestCase {
    @MainActor func testAlreadyClosedAttentionApprovalCannotStartARun() throws {
        final class Client: PlanApprovalClient {
            var starts = 0
            func respond(_ request: PlanApprovalRequest, confirmed: Bool) async -> PlanApprovalPost { .ok }
            func startRun(taskId: String) async throws { starts += 1 }
        }
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let target = try JSONDecoder().decode(AttentionItem.self, from: jsonData([
            "id": "plan", "title": "Approve?", "item_type": "approval", "status": "needs_action",
            "metadata": ["source": "plan_approval", "input_type": "confirmation", "correlation_id": "plan", "task_id": "task"]
        ]))
        vm.lanes["approvals"] = [target]
        MockURLProtocol.handler = { req in
            if req.httpMethod == "POST" { return (response(for: req, status: 409), jsonData(["reason": "already_resolved"])) }
            return (response(for: req), jsonData(["requests": []]))
        }
        let client = Client()
        vm.submitPlanApproval(target, choice: .approveAndRun, client: client)
        waitUntil { vm.bulkNotice != nil }
        XCTAssertEqual(client.starts, 0)
        XCTAssertTrue(vm.items("approvals").isEmpty)
        XCTAssertNil(vm.mutationError)
    }
    func testMissingResponseAcknowledgementRestoresTheQuestion() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let target = item(correlationId: "corr-1", inputType: "choice")
        vm.lanes["requests"] = [target]
        MockURLProtocol.handler = { request in
            if request.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: request), jsonData(["requests": []]))
            }
            if request.httpMethod == "POST" { return (response(for: request), jsonData([:])) }
            return (response(for: request, status: 503), Data())
        }
        vm.submitChoice(target, optionId: "yes")
        waitUntil { vm.pendingCardMutationKeys.isEmpty && !vm.isLoading }
        XCTAssertEqual(vm.items("requests").map(\.id), [target.id])
        XCTAssertNotNil(vm.mutationError)
    }
    func testExactRequestLookupOpensAQuestionWithoutLoadingInboxPages() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let done = expectation(description: "exact request")
        var gets = 0
        MockURLProtocol.handler = { req in
            gets += 1
            XCTAssertTrue(req.url!.absoluteString.contains("/feed/attention/pause%2Fa%3A1"))
            return (response(for: req), jsonData(["id": "exact", "title": "Older form", "item_type": "request", "status": "needs_action",
                "metadata": ["input_type": "form", "pause_state_id": "pause/a:1", "input_schema": ["questions": [["id": "account", "prompt": "Which account?"]]]]]))
        }
        vm.pendingItem("pause/a:1") { result in
            guard case .success(let row) = result, let row else { return XCTFail("exact question must load") }
            XCTAssertEqual(row.formQuestions.first?.id, "account")
            XCTAssertFalse(vm.isAnsweredElsewhere(row), "Absence from a paged inbox does not resolve an exact request")
            done.fulfill()
        }
        wait(for: [done], timeout: 2)
        XCTAssertEqual(gets, 1)
    }

    func testExactRequestFallsBackToTheUnprojectedUserRequestLedger() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let done = expectation(description: "ledger fallback")
        MockURLProtocol.handler = { req in
            if req.url!.path.contains("/feed/attention/") { return (response(for: req, status: 404), jsonData([:])) }
            return (response(for: req), jsonData(["requests": [self.pendingQuestion("not-yet-projected")]]))
        }
        vm.pendingItem("not-yet-projected") { result in
            guard case .success(let row) = result, let row else { return XCTFail("ledger question must open") }
            XCTAssertEqual(row.correlationId, "not-yet-projected")
            done.fulfill()
        }
        wait(for: [done], timeout: 2)
    }
    func testComposerFetchIncludesQuestionsBeyondTheInitialAttentionPage() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        var pages = 0
        var ledgerReads = 0
        MockURLProtocol.handler = { req in
            if req.url!.path.hasSuffix("/user-requests") { ledgerReads += 1; return (response(for: req), jsonData(["requests": []])) }
            if req.url!.path.hasSuffix("/feed/attention") {
                pages += 1
                let cursor = URLComponents(url: req.url!, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "requests_cursor" }?.value
                let id = cursor == nil ? "newer" : "older"
                return (response(for: req), jsonData([
                    "requests": [["id": id, "title": "Question \(id)", "status": "needs_action", "item_type": "request"]],
                    "pages": ["requests": cursor == nil ? ["next_cursor": "next/+", "has_more": true] : ["has_more": false]]
                ]))
            }
            return (response(for: req), jsonData(["items": []]))
        }
        vm.fetchPendingAsks()
        waitUntil { !vm.isLoading }
        XCTAssertEqual(pages, 2)
        XCTAssertEqual(ledgerReads, 1)
        XCTAssertEqual(Set(vm.items("requests").map(\.id)), ["newer", "older"])
    }

    func testPendingQuestionFetchRefusesRepeatedCursorWithoutLooping() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        var pages = 0
        MockURLProtocol.handler = { req in
            if req.url!.path.hasSuffix("/feed/attention") {
                pages += 1
                return (response(for: req), jsonData(["pages": ["requests": ["next_cursor": "same", "has_more": true]]]))
            }
            return (response(for: req), jsonData(["items": []]))
        }
        vm.fetchPendingAsks()
        waitUntil { !vm.isLoading }
        XCTAssertEqual(pages, 2)
        XCTAssertTrue(vm.loadError?.contains("repeated") == true)
    }

    func testApprovalFromAnOldConnectionIsRetainedWithoutPostingToTheNewMachine() {
        var current = true
        let vm = AttentionViewModel(networkSession: makeMockSession(), currentConnectionCheck: { current })
        var writes = 0
        MockURLProtocol.handler = { req in
            if req.httpMethod == "POST" { writes += 1; return (response(for: req), jsonData(["accepted": true])) }
            if req.url!.path.hasSuffix("/user-requests") { return (response(for: req), jsonData(["requests": []])) }
            return (response(for: req), jsonData(["requests": [["id": "approval", "title": "Allow?", "item_type": "request", "status": "needs_action",
                "metadata": ["source": "agentic", "input_type": "confirmation", "pause_state_id": "ask"]]]]))
        }
        vm.fetch()
        waitUntil { !vm.isLoading }
        let ask = vm.items("requests")[0]
        current = false
        vm.submitConfirmation(ask, confirmed: true)
        waitUntil { vm.pendingCardMutationKeys.isEmpty }
        XCTAssertEqual(writes, 0)
        XCTAssertEqual(vm.items("requests").first?.id, "approval")
        XCTAssertTrue(vm.mutationError?.contains("Connection changed") == true)
    }

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
                return (response(for: request), jsonData(["accepted": true]))
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

    // MARK: - Answered elsewhere (I1: an open sheet whose ask settled on another surface)

    private func loadedPages() -> AttentionPages {
        AttentionPages(
            requests: LanePage(total: 1, nextCursor: nil, hasMore: false),
            approvals: nil, escalations: nil, failed: nil, running: nil
        )
    }

    func testAnsweredElsewhereWhenARefetchNoLongerCarriesTheAsk() {
        let open = item(correlationId: "corr-1")
        let other = try! JSONDecoder().decode(AttentionItem.self, from: jsonData([
            "id": "item-2", "title": "Other", "item_type": "agentic", "status": "pending",
            "metadata": ["input_type": "text", "pause_state_id": "corr-2"],
        ]))
        func check(_ lanes: [String: [AttentionItem]], loaded: Bool = true,
                   resolved: Set<String> = [], settled: Set<String> = []) -> Bool {
            AttentionViewModel.answeredElsewhere(open, lanes: lanes, hasLoaded: loaded,
                                                 remotelyResolved: resolved, locallySettled: settled)
        }
        XCTAssertFalse(check(["requests": [open]]), "still pending")
        XCTAssertFalse(check(["requests": [], "approvals": [open]]), "a row that moved lanes is still pending")
        XCTAssertTrue(check(["requests": [other]]), "gone after a refetch: answered elsewhere")
        XCTAssertFalse(check(["requests": [other]], loaded: false), "absence before any load proves nothing")
        XCTAssertTrue(check(["requests": [open]], resolved: ["corr-1"]),
                      "a realtime resolution closes the sheet before the refetch lands")
        XCTAssertFalse(check(["requests": []], resolved: ["corr-1"], settled: ["corr-1"]),
                       "this client's own answer is never 'answered elsewhere'")
        XCTAssertFalse(check(["requests": []], settled: ["item-1"]), "nor is its own dismiss/cancel")
    }

    func testResolutionFramesYieldTheirCorrelationId() {
        XCTAssertEqual(AttentionViewModel.resolvedCorrelationId(
            fromEventText: #"{"event_type":"HitlResolved","data":{"correlation_id":"c1"}}"#), "c1")
        XCTAssertEqual(AttentionViewModel.resolvedCorrelationId(
            fromEventText: #"{"event_type":"UserRequestResolved","data":{"request":{"id":"u1"}}}"#), "u1")
        XCTAssertEqual(AttentionViewModel.resolvedCorrelationId(
            fromEventText: #"{"event_type":"AttentionExpired","data":{"correlation_id":"c2"}}"#), "c2")
        XCTAssertEqual(AttentionViewModel.resolvedCorrelationId(
            fromEventText: #"{"event_type":"hitl.responded","payload":{"correlation_id":"c3"}}"#), "c3")
        XCTAssertNil(AttentionViewModel.resolvedCorrelationId(
            fromEventText: #"{"event_type":"HitlRequested","data":{"correlation_id":"c1"}}"#))
        XCTAssertEqual(AttentionViewModel.requestedCorrelationId(
            fromEventText: #"{"event_type":"HitlRequested","data":{"correlation_id":"c1"}}"#), "c1")
        XCTAssertNil(AttentionViewModel.resolvedCorrelationId(fromEventText: "not-json"))
    }

    func testASubmitAfterARemoteResolutionIsRefusedAndNotPosted() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let target = item(correlationId: "corr-1", inputType: "choice")
        vm.pages = loadedPages()
        vm.lanes["requests"] = [target]
        var posted = false
        MockURLProtocol.handler = { request in
            if request.httpMethod == "POST" { posted = true }
            return (response(for: request), jsonData([:]))
        }

        vm.noteRemoteResolution(correlationId: "corr-1")
        XCTAssertTrue(vm.isAnsweredElsewhere(target))
        vm.submitChoice(target, optionId: "yes")

        XCTAssertTrue(vm.pendingCardMutationKeys.isEmpty, "no optimistic transaction starts")
        XCTAssertEqual(vm.items("requests").map(\.id), ["item-1"], "the card is not removed locally")
        XCTAssertEqual(vm.bulkNotice, AttentionViewModel.answeredElsewhereNotice)
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        XCTAssertFalse(posted, "a stale answer must never reach /respond")

        // A fresh ask under the same correlation id is answerable again.
        vm.noteRequested(correlationId: "corr-1")
        XCTAssertFalse(vm.isAnsweredElsewhere(target))
    }

    func testASubmitForAnAskARefetchDroppedIsRefused() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let target = item(correlationId: "corr-1", inputType: "text")
        vm.pages = loadedPages()
        vm.lanes["requests"] = []
        var posted = false
        MockURLProtocol.handler = { request in
            if request.httpMethod == "POST" { posted = true }
            return (response(for: request), jsonData([:]))
        }
        vm.submitText(target, "late")
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        XCTAssertFalse(posted)
        XCTAssertEqual(vm.bulkNotice, AttentionViewModel.answeredElsewhereNotice)
    }

    func testOwnAnswerIsNotReportedAsAnsweredElsewhere() {
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let target = item(correlationId: "corr-1", inputType: "choice")
        vm.pages = loadedPages()
        vm.lanes["requests"] = [target]
        MockURLProtocol.handler = { request in (response(for: request), jsonData([:])) }

        vm.submitChoice(target, optionId: "yes")
        XCTAssertTrue(vm.items("requests").isEmpty, "the optimistic removal ran")
        XCTAssertFalse(vm.isAnsweredElsewhere(target), "the row left because this client answered it")
        // The echo of this client's own answer on the socket is not a remote one.
        vm.noteRemoteResolution(correlationId: "corr-1")
        XCTAssertTrue(vm.remotelyResolved.isEmpty)
        XCTAssertNotEqual(vm.bulkNotice, AttentionViewModel.answeredElsewhereNotice)
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

    /// An outward-send review answers with the tool-authorization choice
    /// shape — `{type: choice, selected_id}` and nothing else — never a bare
    /// confirm, so Discard and the remembered grant are distinct answers.
    func testSubmitOutwardReviewPostsTheChoiceShape() {
        for id in ["send", "discard", "always_allow_recipient"] {
            assertRespond(expectedValue: ["type": "choice", "selected_id": id]) {
                $0.submitOutwardReview(self.item(inputType: "confirmation"), optionId: id)
            }
        }
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let exp = expectation(description: "respond")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "POST", req.url!.absoluteString.contains("/hitl/corr-1/respond"),
               let body = requestBody(req),
               let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
               let value = json["value"] as? [String: Any] {
                XCTAssertEqual(Set(value.keys), ["type", "selected_id"], "no other_value rides a review")
                exp.fulfill()
            }
            return (response(for: req), jsonData(["requests": []]))
        }
        vm.submitOutwardReview(item(inputType: "confirmation"), optionId: "send")
        wait(for: [exp], timeout: 2)
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
                return (response(for: req), jsonData(["accepted": true]))
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

    // MARK: - HITL surfacing fixes (C2–C4)

    private func decode(_ dict: [String: Any]) -> AttentionItem {
        try! JSONDecoder().decode(AttentionItem.self, from: JSONSerialization.data(withJSONObject: dict))
    }

    func testDelegatedChildAnswerCarriesTheChildExecutionId() {
        let child = decode([
            "id": "item-1", "title": "Q?", "item_type": "request", "status": "needs_action",
            "task_id": "parent-task",
            "metadata": ["delegation_child": true, "delegated_agent_id": "researcher",
                         "hitl_request": ["correlation_id": "corr-1", "source": "agentic",
                                          "execution_id": "child-exec", "input_type": "text"]]
        ])
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let exp = expectation(description: "respond")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "POST", req.url!.absoluteString.contains("/hitl/corr-1/respond"),
               let body = requestBody(req),
               let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                XCTAssertEqual(json["execution_id"] as? String, "child-exec")
                XCTAssertEqual(json["source"] as? String, "agentic")
                XCTAssertNil(json["task_id"], "task_id rides only planning sources")
                exp.fulfill()
            }
            return (response(for: req), jsonData([:]))
        }
        vm.submitText(child, "Ada")
        wait(for: [exp], timeout: 2)
    }

    func testMcpOAuthAnswersExternalActionCompleted() {
        let oauth = decode([
            "id": "item-1", "title": "Connect", "item_type": "request", "status": "needs_action",
            "metadata": ["source": "mcp_oauth", "pause_state_id": "corr-1",
                         "input_schema": ["url": "https://auth.example.com/a"]]
        ])
        let vm = AttentionViewModel(networkSession: makeMockSession())
        let exp = expectation(description: "respond")
        MockURLProtocol.handler = { req in
            if req.httpMethod == "POST", req.url!.absoluteString.contains("/hitl/corr-1/respond"),
               let body = requestBody(req),
               let json = try? JSONSerialization.jsonObject(with: body) as? [String: Any] {
                XCTAssertEqual(json["source"] as? String, "mcp_oauth")
                XCTAssertEqual((json["value"] as? [String: Any])?["type"] as? String, "external_action_completed")
                exp.fulfill()
            }
            return (response(for: req), jsonData([:]))
        }
        vm.submitExternalDone(oauth, guidance: nil)
        wait(for: [exp], timeout: 2)
    }

    func testStalledWaitCancelPostsToExecutionCancelAndRemovesCard() {
        let stalled = decode([
            "id": "stall-1", "title": "Waiting on you, but no question was delivered",
            "item_type": "escalation", "status": "needs_action",
            "metadata": ["attention_reason": "waiting_without_question",
                         "execution_id": "exec-7", "task_id": "task-7",
                         "actions": ["cancel", "open_task"]]
        ])
        let vm = AttentionViewModel(networkSession: makeMockSession())
        vm.lanes["escalations"] = [stalled]
        let exp = expectation(description: "cancel")
        MockURLProtocol.handler = { req in
            if req.url?.path.hasSuffix("/user-requests") == true {
                return (response(for: req), jsonData(["requests": []]))
            }
            if req.httpMethod == "POST", req.url?.path == "/api/magician/v2/executions/exec-7/cancel" {
                exp.fulfill()
            }
            XCTAssertFalse(req.url!.path.contains("/hitl/"), "a stalled wait has no ask to answer")
            return (response(for: req), jsonData([:]))
        }
        vm.cancelStalledExecution(stalled)
        XCTAssertTrue(vm.items("escalations").isEmpty, "the card leaves before the API returns")
        wait(for: [exp], timeout: 2)
    }

    func testFormSubmitPostsAnswersArray() {
        assertRespond(expectedValue: ["type": "form"]) {
            $0.submitForm(self.item(inputType: "form"),
                          answers: [["id": "q1", "value": "x"], ["id": "q2", "selected_ids": ["a"]],
                                    ["id": "q3", "skipped": true]])
        }
    }
}

/// The HITL respond-handle contract: `magician/tests/fixtures/hitl/attention_rows.json`
/// holds one Attention row per HITL kind exactly as the server serves it
/// (pinned by the Rust test `feed::hitl_respond_fixtures`; the web and Android
/// tests read the same file). Every row must post the call `expect` names —
/// through its `metadata.respond` handle, and, with the handle stripped (an
/// older server), through the per-source derivation.
final class HitlRespondFixtureTests: XCTestCase {
    private static let fixtureURL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()  // MagiosTests/
        .deletingLastPathComponent()  // magios/
        .deletingLastPathComponent()  // repo root
        .appendingPathComponent("magician/tests/fixtures/hitl/attention_rows.json")

    private func rows() throws -> [[String: Any]] {
        let data = try Data(contentsOf: Self.fixtureURL)
        let root = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        return try XCTUnwrap(root["rows"] as? [[String: Any]])
    }

    private func check(_ fixture: [String: Any], stripHandle: Bool) throws {
        let kind = fixture["kind"] as? String ?? "?"
        var row = try XCTUnwrap(fixture["row"] as? [String: Any], kind)
        if stripHandle, var metadata = row["metadata"] as? [String: Any] {
            metadata.removeValue(forKey: "respond")
            if var hitl = metadata["hitl_request"] as? [String: Any] {
                hitl.removeValue(forKey: "respond")
                metadata["hitl_request"] = hitl
            }
            row["metadata"] = metadata
        }
        let item = try JSONDecoder().decode(AttentionItem.self,
                                            from: JSONSerialization.data(withJSONObject: row))
        if stripHandle {
            XCTAssertNil(item.respondHandle, kind)
        } else {
            XCTAssertNotNil(item.respondHandle, "\(kind): the handle decodes")
        }
        let answer = try XCTUnwrap(fixture["answer"] as? [String: Any], kind)
        let expect = try XCTUnwrap(fixture["expect"] as? [String: Any], kind)
        let body = try XCTUnwrap(expect["body"] as? [String: Any], kind)
        let call = item.respondCall(value: answer)
        XCTAssertEqual(call.path, expect["url"] as? String, "\(kind): url")
        for key in ["source", "input_type", "execution_id", "task_id"] {
            XCTAssertEqual(call.body[key] as? String, body[key] as? String, "\(kind): \(key)")
        }
        XCTAssertEqual(call.body["channel"] as? String, "ios", kind)
        XCTAssertNotNil(call.body["value"], kind)
    }

    func testEveryRowPostsTheCallItsHandleNames() throws {
        let fixtures = try rows()
        XCTAssertGreaterThanOrEqual(fixtures.count, 10)
        for fixture in fixtures { try check(fixture, stripHandle: false) }
    }

    func testRowsFromAnOlderServerResolveTheSameCall() throws {
        for fixture in try rows() { try check(fixture, stripHandle: true) }
    }

    func testAnUnusableHandleFallsBackToTheDerivation() throws {
        let item = try JSONDecoder().decode(AttentionItem.self, from: JSONSerialization.data(withJSONObject: [
            "id": "i", "title": "Q", "item_type": "escalation", "status": "needs_action",
            "metadata": [
                "source": "user_request", "input_type": "choice", "pause_state_id": "ur-1",
                "respond": ["url": "https://elsewhere.example/x", "correlation_id": "ur-1",
                            "source": "user_request", "input_type": "choice"]
            ]
        ] as [String: Any]))
        XCTAssertNil(item.respondHandle)
        XCTAssertEqual(item.respondCall(value: ["type": "choice", "selected_id": "a"]).path,
                       "/api/magician/v2/hitl/ur-1/respond")
    }
}
