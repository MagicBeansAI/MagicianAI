import XCTest
import Combine
@testable import Magician

final class TodayViewModelTests: XCTestCase {
    private var cancellables: Set<AnyCancellable> = []
    private var sessions: [URLSession] = []

    override func setUp() {
        super.setUp()
        MockURLProtocol.handler = nil
        DeferredMockURLProtocol.handler = nil
        cancellables.removeAll()
    }

    override func tearDown() {
        sessions.forEach { $0.invalidateAndCancel() }
        sessions.removeAll()
        MockURLProtocol.handler = nil
        DeferredMockURLProtocol.handler = nil
        cancellables.removeAll()
        super.tearDown()
    }

    func testPublishedBriefingMuijParsesRepresentativeDashboardWithoutStringifyingJSON() throws {
        let raw: JSONValue = .object([
            "muij_version": .string("1.0"),
            "agent_id": .string("surface-briefing"),
            "generated_at": .string("2026-08-10T00:00:00Z"),
            "layout": .array([.object([
                "id": .string("root"), "component_type": .string("Stack"), "label": .string("Briefing"),
                "props": .object([:]), "children": .array([
                    .object([
                        "id": .string("metrics"), "component_type": .string("Grid"), "label": .string("Metrics"),
                        "props": .object(["columns": .number(2)]), "children": .array([
                            .object([
                                "id": .string("revenue"), "component_type": .string("MetricCard"), "label": .string("Revenue"),
                                "props": .object(["value": .string("$12.4k"), "trend": .string("up")]),
                            ])
                        ])
                    ]),
                    .object([
                        "id": .string("records"), "component_type": .string("Table"), "label": .string("Campaigns"),
                        "props": .object([
                            "columns": .array([.object(["key": .string("name"), "label": .string("Campaign")])]),
                            "rows": .array([.object(["name": .string("Search")])]),
                        ]),
                    ]),
                ]),
            ])]),
        ])

        let document = try XCTUnwrap(try? MuijDocumentModel.parse(raw).get())
        XCTAssertEqual(document.layout.first?.children.first?.children.first?.metricValue, "$12.4k")
        XCTAssertEqual(document.layout.first?.children.last?.tableColumns.first?.label, "Campaign")
        XCTAssertEqual(document.layout.first?.children.last?.tableRows.first?["name"]?.compactDisplay, "Search")
    }

    func testPublishedBriefingMuijRejectsDuplicateIdsAndExcessiveDepth() {
        func component(_ id: String, children: [JSONValue] = []) -> JSONValue {
            .object([
                "id": .string(id), "component_type": .string("Stack"), "label": .string(id),
                "props": .object([:]), "children": .array(children),
            ])
        }
        let duplicate: JSONValue = .object([
            "muij_version": .string("1.0"), "agent_id": .string("surface"),
            "layout": .array([component("same"), component("same")]),
        ])
        XCTAssertEqual(MuijDocumentModel.parse(duplicate), .failure(.duplicateID("same")))

        var nested = component("leaf")
        for depth in 0..<MuijDocumentModel.maximumDepth { nested = component("node-\(depth)", children: [nested]) }
        let tooDeep: JSONValue = .object([
            "muij_version": .string("1.0"), "agent_id": .string("surface"), "layout": .array([nested]),
        ])
        XCTAssertEqual(MuijDocumentModel.parse(tooDeep), .failure(.nestingTooDeep))
    }

    func testPublishedBriefingMuijKeepsUnknownDisplayTypesForwardCompatible() throws {
        let raw: JSONValue = .object([
            "muij_version": .string("1.0"), "agent_id": .string("surface"),
            "layout": .array([.object([
                "id": .string("future"), "component_type": .string("FutureDisplay"),
                "label": .string("A future component"), "props": .object([:]),
            ])]),
        ])
        let document = try MuijDocumentModel.parse(raw).get()
        XCTAssertEqual(document.layout.count, 1)
        XCTAssertEqual(document.layout.first?.type, "FutureDisplay")
        XCTAssertEqual(document.layout.first?.displayLabel, "A future component")
    }

    func testPublishedBriefingMuijRejectsMalformedComponentShape() {
        let malformed: JSONValue = .object([
            "muij_version": .string("1.0"), "agent_id": .string("surface"),
            "layout": .array([.object([
                "id": .string("broken"), "component_type": .string("Stack"),
                "label": .string("Broken"), "props": .object([:]),
                "children": .object(["not": .string("an array")]),
            ])]),
        ])
        XCTAssertEqual(MuijDocumentModel.parse(malformed), .failure(.invalidComponent("broken")))
    }

    @MainActor
    func testFetchPublishesAllMobileTodaySurfaces() {
        let todaySeen = expectation(description: "Today endpoint")
        MockURLProtocol.handler = { request in
            switch request.url!.path {
            case "/api/magician/v2/today":
                todaySeen.fulfill()
                XCTAssertEqual(request.httpMethod, "GET")
                XCTAssertTrue(request.url!.query!.contains("per_section=8"))
                return (response(for: request), self.todayPayload())
            case "/api/magician/v2/today/visibility":
                return (response(for: request), jsonData(["items": [[
                    "item_id": "hidden", "hidden_kind": "dismissed", "record": ["snapshot": [
                        "title": "Hidden", "reason": "Later", "section": "changed",
                        "source_kind": "memory", "source_id": "m1", "space_ids": [], "item_updated_at": 1
                    ]]
                ]]]))
            case "/api/magician/v2/channel-assist/resurfacing/today":
                return (response(for: request), jsonData(["cards": [[
                    "candidate_id": "r1", "line": "Remember this", "why_now": "Useful now",
                    "source_title": "Memory", "summary": "A useful detail", "source_kind": "memory"
                ]], "total": 1]))
            case "/api/magician/v2/channel-assist/follow-ups":
                return (response(for: request), jsonData(["items": [[
                    "annotation_id": "f1", "provider": "gmail", "lane": "user_assist",
                    "label": "needs_reply", "subject": "Reply", "received_at": 1
                ]], "total": 1]))
            case "/api/magician/v2/feed":
                return (response(for: request), jsonData(["items": [
                    ["id": "a1", "item_type": "agent_learning", "title": "Learned", "status": "info", "updated_at": 1],
                    ["id": "noise", "item_type": "approval", "title": "Internal", "status": "needs_action", "updated_at": 1]
                ]]))
            case "/api/magician/v3/published-surfaces/projections":
                return (response(for: request), jsonData(["surfaces": [[
                    "surface": ["surface_id": "b1", "route": "/briefing", "title": "Morning brief", "published_at": "2026-07-13T00:00:00Z"]
                ]]]))
            case "/api/magician/v2/analytics/llm_calls/query":
                XCTAssertEqual(request.httpMethod, "POST")
                XCTAssertNil(request.value(forHTTPHeaderField: "X-Principal"))
                XCTAssertNil(request.value(forHTTPHeaderField: "X-Workspace"))
                return (response(for: request), jsonData([
                    "columns": ["section", "k", "model", "v1", "v2"],
                    "rows": [
                        ["today_total", "all", NSNull(), 1.25, 4],
                        ["yesterday_total", "all", NSNull(), 2.5, 8],
                        ["today_provider", "openai", "gpt-test", 1.25, 4]
                    ]
                ]))
            case "/api/magician/v2/analytics/query":
                XCTAssertNil(request.value(forHTTPHeaderField: "X-Principal"))
                XCTAssertNil(request.value(forHTTPHeaderField: "X-Workspace"))
                return (response(for: request), jsonData(["columns": ["section", "n"], "rows": [["coding_runs_today", 3]]]))
            case "/api/magician/v2/analytics/memory_events/query":
                XCTAssertNil(request.value(forHTTPHeaderField: "X-Principal"))
                XCTAssertNil(request.value(forHTTPHeaderField: "X-Workspace"))
                return (response(for: request), jsonData([
                    "columns": ["section", "n", "passes"],
                    "rows": [["memories_today", 2, 0], ["evals_today", 5, 4]]
                ]))
            case "/api/magician/v3/tasks":
                return (response(for: request), jsonData(["tasks": []]))
            case "/api/magician/v2/agents":
                return (response(for: request), jsonData(["agents": []]))
            case "/api/magician/v2/agents/updates":
                return (response(for: request), jsonData(["events": []]))
            default:
                XCTFail("Unexpected Today request: \(request.url!.absoluteString)")
                return (response(for: request, status: 404), Data())
            }
        }

        let sut = TodayViewModel(networkSession: mockSession(), baseURL: URL(string: "https://example.com")!)
        let loaded = expectation(description: "Today loaded")
        sut.$isLoading.dropFirst().filter { !$0 }.sink { _ in loaded.fulfill() }.store(in: &cancellables)
        sut.fetch()
        wait(for: [todaySeen, loaded], timeout: 3)

        XCTAssertEqual(sut.counts.needsYou, 1)
        XCTAssertEqual(sut.items(for: .needsYou).first?.taskID, "task-1")
        XCTAssertEqual(sut.hiddenItems.first?.record.snapshot?.title, "Hidden")
        XCTAssertEqual(sut.resurfacingCards.first?.id, "r1")
        XCTAssertEqual(sut.messageFollowUps.first?.id, "f1")
        XCTAssertEqual(sut.activityItems.map(\.id), ["a1"])
        XCTAssertEqual(sut.briefings.first?.surface.title, "Morning brief")
        XCTAssertEqual(sut.pulse?.spendToday, 1.25)
        XCTAssertEqual(sut.pulse?.topModel?.model, "gpt-test")
        XCTAssertEqual(sut.pulse?.codingRunsToday, 3)
        XCTAssertEqual(sut.pulse?.evalPassesToday, 4)
        XCTAssertEqual(sut.count(for: .followups), 3)
        XCTAssertEqual(sut.availableSections.first, .needsYou)
        XCTAssertNil(sut.error)
    }

    @MainActor
    func testPrimaryTodayFailureIsVisibleWithoutCrashingSecondarySurfaces() {
        let secondaryRequestsFinished = expectation(description: "Secondary Today requests finished")
        // Visibility, resurfacing, follow-ups, feed, briefings, four pulse
        // analytics/task reads, agents, the crew's llm_calls + tasks reads,
        // agent updates and the wire's 24h count.
        secondaryRequestsFinished.expectedFulfillmentCount = 14
        MockURLProtocol.handler = { request in
            if request.url!.path == "/api/magician/v2/today" {
                return (response(for: request, status: 503), Data("offline".utf8))
            }
            secondaryRequestsFinished.fulfill()
            switch request.url!.path {
            case "/api/magician/v2/today/visibility":
                return (response(for: request), jsonData(["items": []]))
            case "/api/magician/v2/channel-assist/resurfacing/today":
                return (response(for: request), jsonData(["cards": [[
                    "candidate_id": "secondary-r1", "line": "Secondary Worth item",
                    "why_now": "Still available", "source_title": "Memory", "summary": "A detail",
                    "source_kind": "memory"
                ]], "total": 1]))
            case "/api/magician/v2/channel-assist/follow-ups":
                return (response(for: request), jsonData(["items": [[
                    "annotation_id": "secondary-f1", "provider": "gmail", "lane": "user_assist",
                    "label": "needs_reply", "subject": "Secondary follow-up", "received_at": 1
                ]], "total": 1]))
            case "/api/magician/v2/feed":
                return (response(for: request), jsonData(["items": [[
                    "id": "secondary-a1", "item_type": "agent_learning", "title": "Secondary activity",
                    "status": "info", "updated_at": 1
                ]]]))
            case "/api/magician/v3/published-surfaces/projections":
                return (response(for: request), jsonData(["surfaces": [["surface": [
                    "surface_id": "secondary-b1", "route": "/briefing", "title": "Secondary briefing",
                    "published_at": "2026-07-13T00:00:00Z"
                ]]]]))
            case "/api/magician/v2/analytics/llm_calls/query":
                return (response(for: request), jsonData([
                    "columns": ["section", "k", "model", "v1", "v2"], "rows": []
                ]))
            case "/api/magician/v2/analytics/query":
                return (response(for: request), jsonData(["columns": ["section", "n"], "rows": []]))
            case "/api/magician/v2/analytics/memory_events/query":
                return (response(for: request), jsonData([
                    "columns": ["section", "n", "passes"], "rows": []
                ]))
            case "/api/magician/v3/tasks":
                return (response(for: request), jsonData(["tasks": []]))
            case "/api/magician/v2/agents":
                return (response(for: request), jsonData(["agents": []]))
            case "/api/magician/v2/agents/updates":
                return (response(for: request), jsonData(["events": []]))
            default:
                XCTFail("Unexpected secondary Today request: \(request.url!.absoluteString)")
                return (response(for: request, status: 404), Data())
            }
        }
        let sut = TodayViewModel(networkSession: mockSession(), baseURL: URL(string: "https://example.com")!)
        let loaded = expectation(description: "Today failed")
        sut.$isLoading.dropFirst().filter { !$0 }.sink { _ in loaded.fulfill() }.store(in: &cancellables)
        sut.fetch()
        wait(for: [secondaryRequestsFinished, loaded], timeout: 3)
        XCTAssertNotNil(sut.error)
        XCTAssertNil(sut.payload)
        XCTAssertEqual(sut.resurfacingCards.map(\.id), ["secondary-r1"])
        XCTAssertEqual(sut.messageFollowUps.map(\.id), ["secondary-f1"])
        XCTAssertEqual(sut.activityItems.map(\.id), ["secondary-a1"])
        XCTAssertEqual(sut.briefings.map(\.id), ["secondary-b1"])
        XCTAssertNotNil(sut.pulse)
    }

    @MainActor
    func testGreetingAndRelativeTimeAreDeterministic() {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(secondsFromGMT: 0)!
        let date = calendar.date(from: DateComponents(year: 2026, month: 7, day: 13, hour: 8))!
        XCTAssertEqual(TodayViewModel.greeting(for: date, calendar: calendar), "Good morning")
        XCTAssertEqual(TodayViewModel.relativeTime(1_000, now: Date(timeIntervalSince1970: 3_601)), "1h ago")
        let lateAfternoon = calendar.date(from: DateComponents(year: 2026, month: 7, day: 13, hour: 17, minute: 59))!
        let evening = calendar.date(from: DateComponents(year: 2026, month: 7, day: 13, hour: 18))!
        XCTAssertEqual(TodayViewModel.greeting(for: lateAfternoon, calendar: calendar), "Good afternoon")
        XCTAssertEqual(TodayViewModel.greeting(for: evening, calendar: calendar), "Good evening")
    }

    func testAttentionFeedbackAttributionRequiresExactSelectedServedBinding() throws {
        func followUp(decision: [String: Any]?) throws -> ChannelFollowUp {
            var value: [String: Any] = [
                "candidate_id": "follow-1", "source_revision": "distill:3",
                "annotation_id": "follow-1", "provider": "gmail", "account_alias": "work",
                "thread_id": "thread-1", "lane": "user_assist", "created_at": 1
            ]
            if let decision { value["decision_item"] = decision }
            return try JSONDecoder().decode(ChannelFollowUp.self, from: jsonData(value))
        }

        let valid = try followUp(decision: [
            "decision_id": "decision-1", "candidate_id": "follow-1",
            "source_revision": "distill:3", "served_route": "follow_up", "selected": true
        ])
        XCTAssertEqual(valid.feedbackAttribution?.decisionID, "decision-1")

        let notSelected = try followUp(decision: [
            "decision_id": "decision-2", "candidate_id": "follow-1",
            "source_revision": "distill:3", "served_route": "follow_up", "selected": false
        ])
        XCTAssertNil(notSelected.feedbackAttribution)

        let wrongLane = try followUp(decision: [
            "decision_id": "decision-3", "candidate_id": "follow-1",
            "source_revision": "distill:3", "served_route": "worth_a_look", "selected": true
        ])
        XCTAssertNil(wrongLane.feedbackAttribution)

        let staleRevision = try followUp(decision: [
            "decision_id": "decision-4", "candidate_id": "follow-1",
            "source_revision": "distill:2", "served_route": "follow_up", "selected": true
        ])
        XCTAssertNil(staleRevision.feedbackAttribution)
        XCTAssertNil(try followUp(decision: nil).feedbackAttribution)
    }

    func testCanonicalDeliveryLoaderProducesExactFollowUpBinding() async throws {
        let reference = CanonicalAttentionProjectionReference(
            projectionID: "projection-ios-1",
            universeDigest: "universe-ios-1",
            status: "succeeded"
        )
        let expiresAt = Int64(Date().timeIntervalSince1970 * 1_000) + 60_000
        MockURLProtocol.handler = { request in
            XCTAssertEqual(
                request.url?.path,
                "/api/magician/v2/channel-assist/attention-learning/canonical-deliveries/follow_up"
            )
            XCTAssertTrue(request.url?.query?.contains("page_size=1") == true)
            return (response(for: request), jsonData([
                "schema_version": 1,
                "root_decision": [
                    "decision_id": "root-ios-1", "lane": "follow_up",
                    "projection_id": "projection-ios-1", "universe_digest": "universe-ios-1",
                    "expires_at": expiresAt
                ],
                "page": [
                    "delivery_id": "delivery-ios-1", "page_index": 0,
                    "page_start": 0, "page_size": 1, "next_cursor": NSNull(),
                    "expires_at": expiresAt
                ],
                "items": [[
                    "position": 1, "candidate_id": "follow_up:annotation-ios-1",
                    "source_revision": "distill:9", "root_policy_propensity": 0.75,
                    "conditional_delivery_propensity": 1.0, "exposure_token": "exposure-ios-1",
                    "item": [
                        "canonical_id": "follow_up:annotation-ios-1",
                        "source_revision": "distill:9", "served_lane": "follow_up",
                        "origin": ["kind": "follow_up", "annotation_id": "annotation-ios-1"]
                    ]
                ]],
                "impression_policy": [
                    "min_visible_ms": 750, "visibility_rule_version": "delivery-visible-v1"
                ]
            ]))
        }

        let bindings = try await AttentionDeliveryLoader.load(
            surface: "follow_up",
            reference: reference,
            pageSize: 1,
            principal: "person-1",
            workspace: "space-1",
            networkSession: mockSession(),
            baseURL: URL(string: "https://example.com")!
        )

        let binding = try XCTUnwrap(bindings.first)
        XCTAssertEqual(binding.rawItemID, "annotation-ios-1")
        XCTAssertEqual(binding.position, 1)
        XCTAssertEqual(binding.feedbackAttribution.decisionID, "root-ios-1")
        XCTAssertEqual(binding.feedbackAttribution.candidateID, "follow_up:annotation-ios-1")
        XCTAssertEqual(binding.feedbackAttribution.deliveryID, "delivery-ios-1")
        XCTAssertNil(binding.feedbackAttribution.impressionID)
        let raw = try JSONDecoder().decode(ChannelFollowUp.self, from: jsonData([
            "candidate_id": "annotation-ios-1", "source_revision": "distill:9",
            "annotation_id": "annotation-ios-1", "provider": "gmail",
            "account_alias": "work", "thread_id": "thread-ios-1",
            "lane": "user_assist", "created_at": 1
        ]))
        // `applyingAttentionDelivery` returns nil when the bindings do not
        // describe these rows, so the chain has to travel — unwrapping here is
        // the assertion that they DID.
        let delivered = try XCTUnwrap(applyingAttentionDelivery(bindings, to: [raw])?.first)
        XCTAssertEqual(delivered.feedbackAttribution?.candidateID, "follow_up:annotation-ios-1")
        XCTAssertEqual(delivered.feedbackAttribution?.deliveryID, "delivery-ios-1")

        do {
            _ = try await AttentionDeliveryLoader.load(
                surface: "follow_up",
                reference: CanonicalAttentionProjectionReference(
                    projectionID: "stale-projection", universeDigest: reference.universeDigest,
                    status: reference.status
                ),
                pageSize: 1,
                principal: "person-1",
                workspace: "space-1",
                networkSession: mockSession(),
                baseURL: URL(string: "https://example.com")!
            )
            XCTFail("A stale projection must not authorize visibility attribution")
        } catch {
            // Expected: projection identity is part of the frozen delivery contract.
        }
    }

    func testVerifiedIOSImpressionUsesFrozenDeliveryAndRetainsReceipt() async throws {
        let binding = AttentionDeliveryBinding(
            principal: "person-2", workspace: "space-2", rawItemID: "annotation-ios-2",
            originKind: "follow_up", decisionID: "root-ios-2", deliveryID: "delivery-ios-2",
            pageIndex: 0, position: 1, exposureToken: "exposure-ios-2",
            candidateID: "follow_up:annotation-ios-2", sourceRevision: "distill:10",
            surface: "follow_up", minVisibleMS: 10,
            visibilityRuleVersion: "delivery-visible-v1", rootPolicyPropensity: 0.6,
            expiresAt: Int64(Date().timeIntervalSince1970 * 1_000) + 60_000
        )
        MockURLProtocol.handler = { request in
            XCTAssertEqual(
                request.url?.path,
                "/api/magician/v2/channel-assist/attention-learning/impressions"
            )
            let body = try XCTUnwrap(requestBody(request))
            let value = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
            XCTAssertEqual(value["client_type"] as? String, "ios")
            XCTAssertEqual(value["delivery_id"] as? String, binding.deliveryID)
            XCTAssertEqual(value["position"] as? Int, binding.position)
            XCTAssertEqual(value["exposure_token"] as? String, binding.exposureToken)
            let eventID = try XCTUnwrap(value["event_id"] as? String)
            let visibleMS = try XCTUnwrap(value["visible_ms"] as? Int)
            return (response(for: request), jsonData([
                "impression_id": "impression-ios-2", "event_id": eventID,
                "decision_id": binding.decisionID, "delivery_id": binding.deliveryID,
                "page_index": binding.pageIndex, "position": binding.position,
                "exposure_token": binding.exposureToken, "candidate_id": binding.candidateID,
                "source_revision": binding.sourceRevision!, "surface": binding.surface,
                "accumulated_visible_ms": visibleMS, "min_visible_ms": binding.minVisibleMS,
                "visibility_rule_version": binding.visibilityRuleVersion,
                "root_policy_propensity": binding.rootPolicyPropensity,
                "conditional_delivery_propensity": 1.0,
                "verified": true, "deduplicated": false
            ]))
        }

        await AttentionImpressionRecorder.shared.record(
            binding,
            visibleMS: 25,
            networkSession: mockSession(),
            baseURL: URL(string: "https://example.com")!
        )

        XCTAssertEqual(
            AttentionImpressionLedger.shared.receipt(for: binding.identity)?.impressionID,
            "impression-ios-2"
        )
        XCTAssertEqual(binding.feedbackAttribution.impressionID, "impression-ios-2")
    }

    func testSnoozeTargetsUseExactLocalCalendarBoundaries() {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "Asia/Kolkata")!
        func date(_ day: Int, _ hour: Int, _ minute: Int = 0) -> Date {
            calendar.date(from: DateComponents(year: 2026, month: 7, day: day, hour: hour, minute: minute))!
        }
        XCTAssertEqual(TodayViewModel.snoozeMinutes(for: .tonight, now: date(1, 15), calendar: calendar), 180)
        XCTAssertEqual(TodayViewModel.snoozeMinutes(for: .tonight, now: date(1, 17, 30), calendar: calendar), 30)
        XCTAssertEqual(TodayViewModel.snoozeMinutes(for: .tonight, now: date(1, 18), calendar: calendar), 180)
        XCTAssertEqual(TodayViewModel.snoozeMinutes(for: .tomorrowMorning, now: date(1, 23, 30), calendar: calendar), 510)
        XCTAssertEqual(TodayViewModel.snoozeMinutes(for: .tomorrowMorning, now: date(1, 7), calendar: calendar), 60)
        XCTAssertEqual(TodayViewModel.snoozeMinutes(for: .tomorrowMorning, now: date(1, 8), calendar: calendar), 1_440)
        XCTAssertEqual(TodayViewModel.snoozeMinutes(for: .nextWeek, now: date(6, 9), calendar: calendar), 7 * 1_440 - 60)
    }

    func testFollowUpSnoozeChoicesExposeTheFullUserFacingContract() {
        XCTAssertEqual(TodayViewModel.SnoozeOption.allCases.map(\.title), [
            "Until tonight",
            "Tomorrow morning",
            "Next week"
        ])
        XCTAssertEqual(TodayViewModel.SnoozeOption.allCases.map(\.systemImage), [
            "clock",
            "clock",
            "clock"
        ])
    }

    func testTodayItemPreservesActionsEvidenceMetadataAndAttentionTarget() throws {
        let data = jsonData([
            "id": "today:needs_you:feed-1", "section": "needs_you", "priority": 3,
            "title": "Approve", "reason": "Waiting", "source_kind": "approval", "source_id": "feed-1",
            "space_ids": ["client_alpha"], "status": "needs_action", "created_at": 1, "updated_at": 2,
            "actions": [["id": "approve", "label": "Approve", "action_type": "hitl", "payload": ["ok": true]]],
            "evidence_refs": [["kind": "message", "id": "m1"]],
            "metadata": [
                "pause_state_id": "pause-1",
                "learned_items": [["id": "l1", "title": "Preference", "summary": "Likes concise replies", "updated_at": 3]]
            ]
        ])
        let item = try JSONDecoder().decode(TodayItem.self, from: data)
        XCTAssertEqual(item.actions.first?.actionType, "hitl")
        XCTAssertEqual(item.evidenceRefs.count, 1)
        XCTAssertEqual(item.attentionItemID, "pause-1")
        XCTAssertEqual(item.learnedItems.first?.title, "Preference")
        XCTAssertEqual(item.learnedItems.first?.summary, "Likes concise replies")
    }

    func testGenericTodaySourceActionUsesTheAdapterProvidedEndpoint() throws {
        let action = try JSONDecoder().decode(TodayAction.self, from: jsonData([
            "id": "delegate", "label": "Delegate", "action_type": "today_source_action",
            "payload": [
                "method": "POST",
                "endpoint": "/api/magician/v2/today/items/source%3A42/actions/delegate",
                "icon": "person.2.fill"
            ]
        ]))
        let unsafeMethod = try JSONDecoder().decode(TodayAction.self, from: jsonData([
            "id": "delegate", "label": "Delegate", "action_type": "today_source_action",
            "payload": ["method": "DELETE", "endpoint": "/api/magician/v2/today/items/42/actions/delegate"]
        ]))
        let unsafeHost = try JSONDecoder().decode(TodayAction.self, from: jsonData([
            "id": "delegate", "label": "Delegate", "action_type": "today_source_action",
            "payload": ["method": "POST", "endpoint": "https://untrusted.example/actions/delegate"]
        ]))

        XCTAssertEqual(action.executionEndpoint,
                       "/api/magician/v2/today/items/source%3A42/actions/delegate")
        XCTAssertNil(unsafeMethod.executionEndpoint)
        XCTAssertNil(unsafeHost.executionEndpoint)

        let item = try JSONDecoder().decode(TodayItem.self, from: jsonData([
            "id": "generic-actions", "section": "followups", "priority": 1,
            "title": "Adapter actions", "reason": "Source adapter supplied actions",
            "source_kind": "future_source", "source_id": "source:42", "status": "needs_action",
            "created_at": 1, "updated_at": 1,
            "actions": [
                ["id": "delegate", "label": "Delegate", "action_type": "today_source_action",
                 "payload": ["method": "POST", "endpoint": action.executionEndpoint!]],
                ["id": "approve", "label": "Approve", "action_type": "hitl",
                 "payload": ["method": "POST", "endpoint": "/api/magician/v2/today/items/42/actions/approve"]],
                ["id": "unsafe", "label": "Unsafe", "action_type": "today_source_action",
                 "payload": ["method": "DELETE", "endpoint": "/api/magician/v2/today/items/42/actions/unsafe"]]
            ]
        ]))
        XCTAssertEqual(item.executableTodayActions.map(\.id), ["delegate"])
    }

    func testMeetingActionAlwaysRoutesToNativeDetailInsteadOfExternalMeet() throws {
        let withoutThread = try JSONDecoder().decode(TodayItem.self, from: jsonData([
            "id": "meeting-action-no-thread", "section": "followups", "priority": 730,
            "title": "Meeting action: Send the launch notes",
            "summary": "Send **the full notes** to the launch team.",
            "reason": "Captured from a meeting", "source_kind": "meeting_action",
            "source_id": "meeting:removed:action:0", "source_url": "https://meet.google.com/abc-defg-hij",
            "status": "needs_action", "created_at": 1, "updated_at": 1
        ]))
        let withThread = try JSONDecoder().decode(TodayItem.self, from: jsonData([
            "id": "meeting-action-with-thread", "section": "followups", "priority": 730,
            "title": "Meeting action: Publish the decision log", "reason": "Captured from a meeting",
            "source_kind": "meeting_action", "source_id": "meeting:current:action:0",
            "source_url": "https://meet.google.com/current-room", "thread_id": "meeting-thread-current",
            "status": "needs_action", "created_at": 1, "updated_at": 1
        ]))
        let metadataIdentified = try JSONDecoder().decode(TodayItem.self, from: jsonData([
            "id": "legacy-meeting-action", "section": "followups", "priority": 730,
            "title": "Meeting action: File the review", "reason": "Captured from a meeting",
            "source_kind": "memory", "source_id": "meeting:legacy:action:0",
            "source_url": "https://meet.google.com/legacy-room",
            "metadata": ["followup_kind": "meeting_action_item"],
            "status": "needs_action", "created_at": 1, "updated_at": 1
        ]))

        XCTAssertEqual(withoutThread.todayCardRoutingPreference, .nativeDetail)
        XCTAssertEqual(withThread.todayCardRoutingPreference, .nativeDetail)
        XCTAssertEqual(metadataIdentified.todayCardRoutingPreference, .nativeDetail)
        XCTAssertNil(withoutThread.externalSourceURL)
        XCTAssertNil(withThread.externalSourceURL)
        XCTAssertNil(metadataIdentified.externalSourceURL)
    }

    func testStaleMeetingLinkStaysNativeWhileCanonicalTaskRowRoutesToTask() throws {
        let staleMetadataLink = try JSONDecoder().decode(TodayItem.self, from: jsonData([
            "id": "meeting-action-linked", "section": "followups", "priority": 730,
            "title": "Meeting action: Send notes", "reason": "Captured from a meeting",
            "source_kind": "meeting_action", "source_id": "meeting:weekly:action:0",
            "source_url": "https://meet.google.com/weekly-room",
            "metadata": ["linked_task_id": "task_meeting_action_weekly"],
            "status": "needs_action", "created_at": 1, "updated_at": 1
        ]))
        let canonicalTask = try JSONDecoder().decode(TodayItem.self, from: jsonData([
            "id": "followup-task", "section": "followups", "priority": 730,
            "title": "Publish the meeting decision", "reason": "Due today",
            "source_kind": "task", "source_id": "task_decision_log",
            "source_url": "/tasks/task_decision_log", "task_id": "task_decision_log",
            "status": "pending", "created_at": 1, "updated_at": 1
        ]))

        XCTAssertEqual(staleMetadataLink.todayCardRoutingPreference, .nativeDetail)
        XCTAssertEqual(canonicalTask.todayCardRoutingPreference, .task("task_decision_log"))
    }

    func testMeetingActionDetailPreservesCompleteMarkdownAndExpansionPolicy() throws {
        let supportingContext = String(repeating: "Supporting context must remain visible. ", count: 8)
        let longMarkdown = """
        ## Action details

        **Owner:** Sam

        - [ ] Share the complete launch notes
        - [ ] Include the [decision log](https://example.com/decision-log)
        - [ ] Preserve `inline code` and every final detail

        \(supportingContext)
        """
        let fallbackPayload: [String: Any] = [
            "id": "meeting-action-markdown", "section": "followups", "priority": 730,
            "title": "Meeting action: Share notes", "summary": "Short card summary",
            "reason": "Captured from a meeting", "source_kind": "meeting_action",
            "source_id": "meeting:markdown:action:0", "status": "needs_action",
            "metadata": ["description": longMarkdown, "action_item": "Share notes"],
            "created_at": 1, "updated_at": 1
        ]
        let itemData = jsonData(fallbackPayload)
        let item = try JSONDecoder().decode(TodayItem.self, from: itemData)
        let expectedMarkdown = longMarkdown.trimmingCharacters(in: .whitespacesAndNewlines)

        let detail = try XCTUnwrap(item.todayDetailMarkdownSource)
        XCTAssertTrue(detail.hasPrefix(expectedMarkdown))
        XCTAssertTrue(detail.contains("**Owner:** Sam"))
        XCTAssertTrue(detail.contains("[decision log](https://example.com/decision-log)"))
        XCTAssertTrue(detail.contains("`inline code`"))
        XCTAssertTrue(detail.hasSuffix("Short card summary"))
        XCTAssertTrue(TodayDescriptionPresentation.isExpandable(detail))
        XCTAssertFalse(TodayDescriptionPresentation.isExpandable("**Short** description"))

        let completePayload: [String: Any] = [
            "id": "meeting-action-complete-markdown", "section": "followups", "priority": 730,
            "title": "Meeting action: Use canonical body", "summary": "A lossy card summary",
            "reason": "Captured from a meeting", "source_kind": "meeting_action",
            "source_id": "meeting:complete:action:0", "status": "needs_action",
            "metadata": ["detail_markdown": longMarkdown, "description": "Legacy fallback"],
            "created_at": 1, "updated_at": 1
        ]
        let completeData = jsonData(completePayload)
        let complete = try JSONDecoder().decode(TodayItem.self, from: completeData)
        XCTAssertEqual(complete.todayDetailMarkdownSource, expectedMarkdown)
    }

    func testSpaceGroupingLabelsOtherAndFilesItLast() {
        func item(_ id: String, _ spaces: [String]) -> TodayItem {
            TodayItem(id: id, section: "changed", priority: 0, title: id, summary: nil, reason: "",
                      sourceKind: "memory", sourceID: id, sourceURL: nil, spaceIDs: spaces,
                      threadID: nil, taskID: nil, agentID: nil, status: "info", createdAt: 1, updatedAt: 1)
        }
        let groups = TodayViewModel.spaceGroups([item("none", []), item("beta", ["beta_space"]), item("alpha", ["alpha-space"])])
        XCTAssertEqual(groups.map(\.id), ["alpha-space", "beta_space", "unfiled"])
        XCTAssertEqual(groups.map(\.label), ["Alpha Space", "Beta Space", "Other"])
    }

    func testRichResurfacingAndChannelContractsDecode() throws {
        let card = try JSONDecoder().decode(ResurfacingCard.self, from: jsonData([
            "candidate_id": "r1", "line": "Review policy", "why_now": "Effective tomorrow",
            "source_title": "Policy", "summary": "A limit changed", "source_kind": "memory", "source_ref": "m:1",
            "detail_label": "Review", "content_revision": "rev-2", "source_updated": true,
            "brief_status": "v2", "brief": ["schema_version": 2, "key_facts": ["Limit is 10"],
                "changes": [["aspect": "Limit", "before": "5", "after": "10"]],
                "temporal_facts": [["kind": "effective", "text": "Tomorrow", "at_ms": 100]],
                "detail_status": "complete", "missing_details": []],
            "recommended_action": ["kind": "create_task", "label": "Create task", "rationale": "Act now",
                "confidence": 0.9, "content_revision": "rev-2", "source": "curator"],
            "actions": [["kind": "create_task", "label": "Create task", "requires_input": true, "side_effect": "creates_task"]]
        ]))
        XCTAssertEqual(card.brief?.changes.first?.after, "10")
        XCTAssertEqual(card.recommendedAction?.kind, .createTask)
        XCTAssertTrue(card.sourceUpdated)

        let followUp = try JSONDecoder().decode(ChannelFollowUp.self, from: jsonData([
            "annotation_id": "f1", "provider": "gmail", "account_alias": "work", "thread_id": "t1",
            "lane": "user_assist", "created_at": 1,
            "proposed_action": ["follow_up_kind": "reply", "action_owner": "me", "due_text": "Tuesday",
                                "urgency": "high", "key_details": ["Confirm time", "Send agenda"]]
        ]))
        XCTAssertEqual(followUp.actionSummary, "Reply · Owner: Me · Due: Tuesday · High · Details: Confirm time · Send agenda")
    }

    func testChannelActionAdapterDescriptorsDecodeAndBuildCanonicalEndpoints() throws {
        let followUp = try JSONDecoder().decode(ChannelFollowUp.self, from: jsonData([
            "annotation_id": "follow/up 42", "provider": "imessage", "account_alias": "messages",
            "thread_id": "chat-42", "lane": "user_assist", "created_at": 1,
            "state": "needs_approval", "review_required": true, "source_family": "comms_ingest",
            "available_actions": [
                ["id": "reply", "label": "Reply", "needs_compose": true,
                 "confirm": true, "icon": "reply"],
                ["id": "like", "label": "Like", "needs_compose": false,
                 "confirm": false, "icon": "heart"]
            ]
        ]))

        XCTAssertEqual(followUp.state, "needs_approval")
        XCTAssertTrue(followUp.reviewRequired)
        XCTAssertFalse(followUp.canAcknowledge)
        XCTAssertEqual(followUp.sourceFamily, "comms_ingest")
        XCTAssertEqual(followUp.availableActions.map(\.id), ["reply", "like"])

        let reply = try XCTUnwrap(followUp.availableActions.first)
        XCTAssertTrue(reply.needsCompose)
        XCTAssertTrue(reply.confirm)
        XCTAssertEqual(reply.systemImage, "arrowshape.turn.up.left.fill")
        XCTAssertEqual(reply.composeEndpoint(annotationID: followUp.id),
                       "/api/magician/v2/channel-assist/annotations/follow%2Fup%2042/action/reply/compose")
        XCTAssertEqual(reply.commitEndpoint(annotationID: followUp.id),
                       "/api/magician/v2/channel-assist/annotations/follow%2Fup%2042/action/reply/commit")

        let direct = try XCTUnwrap(followUp.availableActions.last)
        XCTAssertFalse(direct.needsCompose)
        XCTAssertFalse(direct.confirm)
        XCTAssertEqual(direct.systemImage, "heart.fill")
        XCTAssertEqual(direct.commitEndpoint(annotationID: followUp.id),
                       "/api/magician/v2/channel-assist/annotations/follow%2Fup%2042/action/like/commit")
    }

    @MainActor
    func testChannelActionClientComposesRedraftsCommitsEditedTextAndSupportsDirectActions() async throws {
        let item = try JSONDecoder().decode(ChannelFollowUp.self, from: jsonData([
            "candidate_id": "annotation-42", "source_revision": "distill:7",
            "annotation_id": "annotation-42", "provider": "imessage", "account_alias": "messages",
            "thread_id": "chat-42", "lane": "user_assist", "created_at": 1,
            "decision_item": [
                "decision_id": "decision-follow-42", "candidate_id": "annotation-42",
                "source_revision": "distill:7", "served_route": "follow_up", "selected": true
            ],
            "available_actions": [
                ["id": "reply", "label": "Reply", "needs_compose": true,
                 "confirm": true, "icon": "reply"],
                ["id": "like", "label": "Like", "needs_compose": false,
                 "confirm": false, "icon": "heart"]
            ]
        ]))
        let reply = try XCTUnwrap(item.availableActions.first)
        let direct = try XCTUnwrap(item.availableActions.last)
        let requested = expectation(description: "All generic channel action requests")
        requested.expectedFulfillmentCount = 8
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.httpMethod, "POST")
            XCTAssertFalse(request.url?.query?.contains("principal=") ?? false)
            XCTAssertFalse(request.url?.query?.contains("workspace=") ?? false)
            let body = requestBody(request).flatMap {
                try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
            } ?? [:]
            let assertFeedbackAttribution = {
                XCTAssertFalse((body["event_id"] as? String ?? "").isEmpty)
                let attribution = body["attribution"] as? [String: Any]
                XCTAssertEqual(attribution?["decision_id"] as? String, "decision-follow-42")
                XCTAssertEqual(attribution?["candidate_id"] as? String, "annotation-42")
                XCTAssertEqual(attribution?["source_revision"] as? String, "distill:7")
                XCTAssertNil(attribution?["impression_id"])
                XCTAssertNil(attribution?["delivery_id"])
            }
            requested.fulfill()
            switch request.url?.path {
            case "/api/magician/v2/channel-assist/annotations/annotation-42/action/reply/compose":
                XCTAssertEqual(body["hint"] as? String, "Make it warmer")
                return (response(for: request), jsonData(["compose_id": "compose-1", "text": "Draft reply"]))
            case "/api/magician/v2/channel-assist/annotations/annotation-42/action/reply/commit":
                XCTAssertEqual(body["body"] as? String, "Edited owner-approved reply")
                XCTAssertEqual(body["compose_id"] as? String, "compose-1")
                assertFeedbackAttribution()
                return (response(for: request), jsonData([
                    "feedback_receipt": [
                        "outcome_id": "channel-reply-outcome", "outcome": "action_completed",
                        "surface": "follow_up", "feedback_recorded": true,
                        "affected_candidates": 3, "rescore_status": "completed",
                        "posterior_update": [
                            "status": "updated", "attribution_quality": "decision_only",
                            "posterior_version_before": 4, "posterior_version_after": 5,
                            "rescore_scheduled": true
                        ]
                    ]
                ]))
            case "/api/magician/v2/channel-assist/annotations/annotation-42/action/like/commit":
                assertFeedbackAttribution()
                XCTAssertNil(body["body"])
                XCTAssertNil(body["compose_id"])
                return (response(for: request), jsonData([:]))
            case "/api/magician/v2/channel-assist/annotations/annotation-42/snooze":
                XCTAssertTrue(body.isEmpty)
                return (response(for: request), jsonData([:]))
            case "/api/magician/v2/channel-assist/annotations/annotation-42/dismiss":
                assertFeedbackAttribution()
                if let reason = body["reason"] as? String {
                    XCTAssertEqual(reason, "already_handled")
                }
                return (response(for: request), jsonData([:]))
            case "/api/magician/v2/channel-assist/annotations/annotation-42/acknowledge":
                assertFeedbackAttribution()
                return (response(for: request), jsonData([:]))
            case "/api/magician/v2/channel-assist/annotations/annotation-42/useful":
                assertFeedbackAttribution()
                return (response(for: request), jsonData([:]))
            default:
                XCTFail("Unexpected channel action request: \(request.url?.absoluteString ?? "nil")")
                return (response(for: request, status: 404), Data())
            }
        }

        let client = ChannelFollowUpActionClient(networkSession: mockSession(),
                                                 baseURL: URL(string: "https://example.com")!,
                                                 principal: "person-1", workspace: "space-1")
        let draft = await client.compose(reply, for: item, hint: "  Make it warmer  ")
        XCTAssertEqual(draft, ChannelActionDraft(composeID: "compose-1", text: "Draft reply"))
        let editedCommitted = await client.commit(reply, for: item,
                                                  body: "  Edited owner-approved reply  ",
                                                  composeID: "compose-1")
        let directCommitted = await client.commit(direct, for: item)
        let snoozed = await client.resolve(item, action: "snooze")
        let dismissed = await client.resolve(item, action: "dismiss")
        let dismissedWithReason = await client.resolve(item, action: "dismiss", reason: "already_handled")
        let acknowledged = await client.resolve(item, action: "acknowledge")
        let markedUseful = await client.resolve(item, action: "useful")
        XCTAssertTrue(editedCommitted)
        XCTAssertTrue(directCommitted)
        XCTAssertTrue(snoozed)
        XCTAssertTrue(dismissed)
        XCTAssertTrue(dismissedWithReason)
        XCTAssertTrue(acknowledged)
        XCTAssertTrue(markedUseful)
        XCTAssertTrue(item.canAcknowledge)
        XCTAssertEqual(client.feedbackReceipts[item.id]?.outcomeID, "channel-reply-outcome")
        XCTAssertEqual(client.feedbackReceipts[item.id]?.posteriorUpdate?.status, "updated")
        await fulfillment(of: [requested], timeout: 2)
        XCTAssertNil(client.error)
        XCTAssertNil(client.busyKey)
    }

    func testChannelFollowUpDismissOptionsMatchWebContractWithNoReasonFirst() {
        XCTAssertEqual(ChannelFollowUpDismissOption.all.map(\.code), [
            nil, "spam", "already_handled", "duplicate", "delegated",
            "not_relevant", "wrong_classification",
        ] as [String?])
        XCTAssertEqual(ChannelFollowUpDismissOption.all.map(\.label), [
            "No reason", "Spam / junk", "Already taken care of", "Duplicate request",
            "Someone else handles this", "Not relevant to me", "Shouldn't have been flagged",
        ])
    }

    @MainActor
    func testChannelActionClientFailureReturnsFalseAndKeepsTheFollowUpOwnedByTheCaller() async throws {
        let item = try JSONDecoder().decode(ChannelFollowUp.self, from: jsonData([
            "annotation_id": "annotation-failed", "provider": "imessage", "account_alias": "messages",
            "thread_id": "chat-failed", "lane": "user_assist", "created_at": 1,
            "available_actions": [["id": "like", "label": "Like", "needs_compose": false,
                                    "confirm": false, "icon": "heart"]]
        ]))
        let action = try XCTUnwrap(item.availableActions.first)
        var visibleItems = [item]
        MockURLProtocol.handler = { request in
            (response(for: request, status: 503), jsonData(["error": "Channel gateway unavailable"]))
        }
        let client = ChannelFollowUpActionClient(networkSession: mockSession(),
                                                 baseURL: URL(string: "https://example.com")!)

        let succeeded = await client.commit(action, for: item)
        if succeeded { visibleItems.removeAll { $0.id == item.id } }

        XCTAssertFalse(succeeded)
        XCTAssertEqual(visibleItems.map(\.id), ["annotation-failed"])
        XCTAssertTrue(client.error?.contains("Channel gateway unavailable") == true)
        XCTAssertNil(client.busyKey)
    }

    @MainActor
    func testChannelWritingPreferencesFetchLearnAndUpdateUseCanonicalEndpoints() async throws {
        let item = try JSONDecoder().decode(ChannelFollowUp.self, from: jsonData([
            "annotation_id": "annotation-77", "provider": "gmail", "account_alias": "work",
            "thread_id": "chat-77", "lane": "user_assist", "created_at": 1, "sender": "Ann"
        ]))
        MockURLProtocol.handler = { request in
            let body = requestBody(request).flatMap {
                try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
            } ?? [:]
            switch (request.httpMethod, request.url?.path) {
            case ("GET", "/api/magician/v2/channel-assist/annotations/annotation-77/writing-preferences"):
                return (response(for: request), jsonData(["items": [[
                    "id": "wp1", "scope_kind": "sender", "scope_value": "ann@example.com",
                    "statement": "Keep replies concise", "status": "candidate", "evidence_count": 2
                ]]]))
            case ("POST", "/api/magician/v2/channel-assist/annotations/annotation-77/writing-preferences"):
                XCTAssertEqual(body["scope"] as? String, "sender")
                XCTAssertEqual(body["statement"] as? String, "Use a friendly greeting")
                XCTAssertEqual(body["promote"] as? Bool, true)
                return (response(for: request), jsonData(["items": []]))
            case ("POST", "/api/magician/v2/channel-assist/writing-preferences/wp1/promote"):
                return (response(for: request), jsonData([:]))
            default:
                XCTFail("Unexpected writing-preference request: \(request.url?.absoluteString ?? "nil")")
                return (response(for: request, status: 404), Data())
            }
        }
        let client = ChannelFollowUpActionClient(networkSession: mockSession(),
                                                 baseURL: URL(string: "https://example.com")!,
                                                 principal: "person-1", workspace: "space-1")
        let prefs = await client.fetchWritingPreferences(for: item)
        XCTAssertEqual(prefs.map(\.id), ["wp1"])
        XCTAssertEqual(prefs.first?.status, "candidate")
        let learned = await client.learnWritingPreference(for: item, scope: "sender",
                                                          statement: "  Use a friendly greeting  ", promote: true)
        XCTAssertTrue(learned)
        let promoted = await client.updateWritingPreference(id: "wp1", action: "promote")
        XCTAssertTrue(promoted)
        XCTAssertNil(client.error)
        XCTAssertNil(client.busyKey)
    }

    @MainActor
    func testParityActionsUseCanonicalEndpointsAndPayloads() throws {
        let seen = expectation(description: "mark seen")
        let approved = expectation(description: "follow-up approve")
        let contextual = expectation(description: "resurfacing action")
        let telemetry = expectation(description: "recommendation telemetry")
        telemetry.expectedFulfillmentCount = 2
        let followUpJSON: [String: Any] = [
            "annotation_id": "f1", "provider": "gmail", "account_alias": "work", "thread_id": "t1",
            "lane": "user_assist", "created_at": 1
        ]
        let sut = loadedSUT(followUps: ["items": [followUpJSON], "total": 1])
        let followUp = try XCTUnwrap(sut.messageFollowUps.first)
        MockURLProtocol.handler = { request in
            let body = requestBody(request).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] } ?? [:]
            switch request.url!.path {
            case "/api/magician/v2/today/items/n1/visibility":
                XCTAssertEqual(body["action"] as? String, "mark_seen"); seen.fulfill()
                return (response(for: request), jsonData([:]))
            case "/api/magician/v2/channel-assist/annotations/f1/approve":
                XCTAssertEqual(body["hint"] as? String, "Draft a concise reply"); approved.fulfill()
                return (response(for: request), jsonData(["task_id": "task-1"]))
            case "/api/magician/v2/channel-assist/resurfacing/r1/actions":
                XCTAssertEqual(body["kind"] as? String, "create_task")
                XCTAssertEqual(body["content_revision"] as? String, "rev-2")
                XCTAssertFalse((body["idempotency_key"] as? String ?? "").isEmpty)
                XCTAssertEqual((body["input"] as? [String: Any])?["title"] as? String, "Review policy")
                contextual.fulfill()
                return (response(for: request), jsonData([
                    "candidate_id": "r1", "action": "create_task", "result_ref": "task-2", "replayed": false,
                    "result": ["kind": "task", "task_id": "task-2", "route": "/tasks/task-2"]
                ]))
            case "/api/magician/v2/channel-assist/resurfacing/r1/recommendation-event":
                XCTAssertTrue(["selected", "completed"].contains(body["event"] as? String ?? "")); telemetry.fulfill()
                return (response(for: request), jsonData(["recorded": true]))
            default:
                XCTFail("Unexpected action request: \(request.url!.absoluteString)")
                return (response(for: request, status: 404), Data())
            }
        }
        let todayItem = TodayItem(id: "n1", section: "needs_you", priority: 1, title: "Approve", summary: nil,
                                  reason: "", sourceKind: "approval", sourceID: "a1", sourceURL: nil,
                                  spaceIDs: [], threadID: nil, taskID: nil, agentID: nil, status: "needs_action",
                                  createdAt: 1, updatedAt: 1)
        let card = try JSONDecoder().decode(ResurfacingCard.self, from: jsonData([
            "candidate_id": "r1", "line": "Review policy", "why_now": "Now", "source_title": "Policy",
            "summary": "Review it", "source_kind": "memory", "content_revision": "rev-2",
            "recommended_action": ["kind": "create_task", "label": "Create task", "rationale": "Act",
                                     "confidence": 0.9, "content_revision": "rev-2", "source": "curator"]
        ]))
        sut.markSeen(todayItem)
        sut.resolveFollowUp(followUp, action: "approve", hint: "Draft a concise reply")
        sut.performResurfacingAction(card, kind: .createTask, input: ["title": "Review policy", "instruction": "Review it"])
        wait(for: [seen, approved, contextual, telemetry], timeout: 3)
        XCTAssertEqual(sut.resurfacingActionResults["r1"]?.result?.objectValue?["task_id"]?.stringValue, "task-2")
    }

    @MainActor
    func testNativeReminderReceiptUsesEventKitDeliveryAndStableIdempotencyKey() throws {
        let posted = expectation(description: "native reminder receipt")
        let completed = expectation(description: "native reminder completion")
        let sut = loadedSUT()
        let card = try JSONDecoder().decode(ResurfacingCard.self, from: jsonData([
            "candidate_id": "r1", "line": "Review policy", "why_now": "Now",
            "source_title": "Policy", "summary": "Review it", "source_kind": "memory",
            "content_revision": "rev-2"
        ]))
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.url?.path, "/api/magician/v2/channel-assist/resurfacing/r1/actions")
            let body = requestBody(request).flatMap {
                try? JSONSerialization.jsonObject(with: $0) as? [String: Any]
            } ?? [:]
            XCTAssertEqual(body["kind"] as? String, "create_reminder")
            XCTAssertEqual(body["idempotency_key"] as? String, "native-reminder-operation-1")
            let input = body["input"] as? [String: Any]
            XCTAssertEqual(input?["delivery"] as? String, "client_apple_eventkit")
            XCTAssertEqual(input?["external_id"] as? String, "eventkit-reminder-1")
            posted.fulfill()
            return (response(for: request), jsonData([
                "candidate_id": "r1", "action": "create_reminder", "result_ref": "receipt-1",
                "replayed": false,
                "result": [
                    "kind": "reminder", "reminder_id": "eventkit-reminder-1",
                    "provider": "apple_eventkit_ios",
                    "at": "2099-07-20T03:30:00Z", "timezone": "Asia/Kolkata"
                ]
            ]))
        }

        sut.performResurfacingAction(
            card,
            kind: .createReminder,
            input: [
                "title": "Review policy", "instruction": "Review it",
                "at": "2099-07-20T03:30:00Z", "timezone": "Asia/Kolkata",
                "delivery": "client_apple_eventkit", "external_id": "eventkit-reminder-1"
            ],
            idempotencyKey: "native-reminder-operation-1"
        ) { result in
            if case .failure(let error) = result { XCTFail("Unexpected reminder failure: \(error)") }
            completed.fulfill()
        }

        wait(for: [posted, completed], timeout: 3)
        XCTAssertEqual(
            sut.resurfacingActionResults["r1"]?.result?.objectValue?["reminder_id"]?.stringValue,
            "eventkit-reminder-1"
        )
    }

    @MainActor
    func testPendingAppleReminderReceiptSurvivesViewRecreationAndClearsAfterAcknowledgement() {
        let suite = "AppleReminderPendingReceiptStoreTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let operationKey = AppleReminderPendingReceiptStore.operationKey(candidateID: "candidate-1")
        let receipt = PendingAppleReminderReceipt(
            idempotencyKey: "native-reminder-operation-1",
            identifier: "eventkit-reminder-1",
            title: "Review policy",
            notes: "Review it",
            dueAt: Date(timeIntervalSince1970: 4_070_908_800),
            timeZoneIdentifier: "Asia/Kolkata",
            createdAt: Date(timeIntervalSince1970: 1_785_571_200)
        )

        AppleReminderPendingReceiptStore(defaults: defaults).save(receipt, for: operationKey)
        let relaunched = AppleReminderPendingReceiptStore(defaults: defaults)
        XCTAssertEqual(relaunched.receipt(for: operationKey), receipt)

        relaunched.remove(for: operationKey)
        XCTAssertNil(AppleReminderPendingReceiptStore(defaults: defaults).receipt(for: operationKey))
    }

    @MainActor
    func testCoreLanePaginationBootstrapsCursorDeduplicatesAndMergesNextPage() {
        let existing = todayItemJSON("changed-1")
        let sut = loadedSUT(today: todayPayload(changed: [existing]))
        var requests = 0
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.url!.path, "/api/magician/v2/today")
            XCTAssertTrue(request.url!.query!.contains("section=changed"))
            requests += 1
            if requests == 1 {
                XCTAssertFalse(request.url!.query!.contains("cursor="))
                return (response(for: request), self.todayPayload(changed: [existing], sectionPage: [
                    "section": "changed", "total": 2, "limit": 8, "next_cursor": "cursor-1", "has_more": true
                ]))
            }
            XCTAssertTrue(request.url!.query!.contains("cursor=cursor-1"))
            return (response(for: request), self.todayPayload(changed: [self.todayItemJSON("changed-2")], sectionPage: [
                "section": "changed", "total": 2, "limit": 8, "cursor": "cursor-1", "has_more": false
            ]))
        }

        performAndWaitForIdle(sut) { sut.loadRemaining(.changed) }

        XCTAssertEqual(requests, 2)
        XCTAssertEqual(sut.items(for: .changed).map(\.id), ["changed-1", "changed-2"])
        XCTAssertNil(sut.sectionErrors[TodaySection.changed.rawValue])
    }

    @MainActor
    func testResurfacingAndMessagePaginationUseOpaqueCursorsAndDeduplicate() throws {
        let resurfacing: [String: Any] = ["cards": [[
            "candidate_id": "r1", "line": "One", "why_now": "Now", "source_title": "Source",
            "summary": "First", "source_kind": "memory"
        ]], "total": 2, "next_cursor": ["surfaced_at": 123, "score": 0.8, "candidate_id": "r1"]]
        let followUps: [String: Any] = ["items": [[
            "annotation_id": "f1", "provider": "gmail", "account_alias": "work", "thread_id": "t1",
            "lane": "user_assist", "created_at": 1
        ]], "total": 2, "next_cursor": "follow-cursor"]
        let sut = loadedSUT(resurfacing: resurfacing, followUps: followUps)

        MockURLProtocol.handler = { request in
            switch request.url!.path {
            case "/api/magician/v2/channel-assist/resurfacing/today":
                let query = request.url!.query ?? ""
                XCTAssertTrue(query.contains("cursor_surfaced_at=123"))
                XCTAssertTrue(query.contains("cursor_candidate_id=r1"))
                return (response(for: request), jsonData(["cards": [
                    ["candidate_id": "r1", "line": "Duplicate", "why_now": "Now", "source_title": "Source", "summary": "Duplicate", "source_kind": "memory"],
                    ["candidate_id": "r2", "line": "Two", "why_now": "Soon", "source_title": "Source", "summary": "Second", "source_kind": "memory"]
                ], "total": 2]))
            case "/api/magician/v2/channel-assist/follow-ups":
                XCTAssertTrue(request.url!.query!.contains("cursor=follow-cursor"))
                return (response(for: request), jsonData(["items": [
                    ["annotation_id": "f1", "provider": "gmail", "account_alias": "work", "thread_id": "t1", "lane": "user_assist", "created_at": 1],
                    ["annotation_id": "f2", "provider": "slack", "account_alias": "team", "thread_id": "t2", "lane": "user_assist", "created_at": 2]
                ], "total": 2]))
            default:
                XCTFail("Unexpected pagination request: \(request.url!.absoluteString)")
                return (response(for: request, status: 404), Data())
            }
        }

        performAndWaitForIdle(sut) { sut.loadRemainingResurfacing() }
        XCTAssertEqual(sut.resurfacingCards.map(\.id), ["r1", "r2"])
        performAndWaitForIdle(sut) { sut.loadRemainingMessageFollowUps() }
        XCTAssertEqual(sut.messageFollowUps.map(\.id), ["f1", "f2"])
    }

    @MainActor
    func testHideFailureRollsBackOptimisticStateAndPreservesSnapshotPayload() {
        let sut = loadedSUT()
        let item = sut.items(for: .needsYou)[0]
        let attempted = expectation(description: "hide attempted")
        MockURLProtocol.handler = { request in
            let body = requestBody(request).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] } ?? [:]
            XCTAssertEqual(request.url!.path, "/api/magician/v2/today/items/n1/visibility")
            XCTAssertEqual(body["action"] as? String, "snooze")
            XCTAssertEqual(body["snooze_minutes"] as? Int, 45)
            XCTAssertEqual((body["snapshot"] as? [String: Any])?["title"] as? String, "Approve")
            attempted.fulfill()
            return (response(for: request, status: 503), Data("cannot hide".utf8))
        }

        performAndWaitForCardMutations(sut) { sut.hide(item, action: "snooze", snoozeMinutes: 45) }

        wait(for: [attempted], timeout: 1)
        XCTAssertEqual(sut.items(for: .needsYou).map(\.id), ["n1"])
        XCTAssertTrue(sut.hiddenItems.isEmpty)
        XCTAssertNil(sut.lastHiddenItem)
        XCTAssertEqual(sut.counts.needsYou, 1)
        XCTAssertTrue(sut.error?.contains("cannot hide") == true)
    }

    @MainActor
    func testHideFailureRestoresOriginalTodayRowPosition() {
        let rows = [
            todayItemJSON("n0", section: "needs_you"),
            todayItemJSON("n1", section: "needs_you"),
            todayItemJSON("n2", section: "needs_you"),
        ]
        let sut = loadedSUT(today: todayPayload(needsYou: rows))
        let middle = sut.items(for: .needsYou)[1]
        MockURLProtocol.handler = { request in
            (response(for: request, status: 503), Data("cannot hide".utf8))
        }

        performAndWaitForCardMutations(sut) {
            sut.hide(middle, action: "dismiss")
        }

        XCTAssertEqual(sut.items(for: .needsYou).map(\.id), ["n0", "n1", "n2"])
        XCTAssertEqual(sut.counts.needsYou, 3)
    }

    @MainActor
    func testUndoWaitsForPendingHideThenRestoresInOrder() {
        let sut = loadedSUT()
        let item = sut.items(for: .needsYou)[0]
        let restored = expectation(description: "restore completed")
        let fallback = initialHandler(today: todayPayload())
        let lock = NSLock()
        var visibilityActions: [String] = []
        MockURLProtocol.handler = { request in
            if request.url!.path == "/api/magician/v2/today/items/n1/visibility", request.httpMethod == "POST" {
                let body = requestBody(request).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] } ?? [:]
                let action = body["action"] as? String ?? ""
                lock.lock(); visibilityActions.append(action); lock.unlock()
                if action == "dismiss" { Thread.sleep(forTimeInterval: 0.05) }
                if action == "restore" { restored.fulfill() }
                return (response(for: request), jsonData([:]))
            }
            return try fallback(request)
        }

        sut.hide(item, action: "dismiss")
        XCTAssertEqual(sut.hiddenItems.first?.id, "n1")
        XCTAssertTrue(sut.items(for: .needsYou).isEmpty)
        sut.undoLastHidden()
        wait(for: [restored], timeout: 3)
        waitForMainQueue()

        lock.lock(); let actions = visibilityActions; lock.unlock()
        XCTAssertEqual(Array(actions.prefix(2)), ["dismiss", "restore"])
        XCTAssertNil(sut.lastHiddenItem)
    }

    @MainActor
    func testFollowUpResolutionRemovesImmediatelyThenRollsBackOnFailure() {
        let followUps: [String: Any] = ["items": [[
            "candidate_id": "f1", "source_revision": "distill:1",
            "annotation_id": "f1", "provider": "gmail", "account_alias": "work", "thread_id": "t1",
            "lane": "user_assist", "created_at": 1,
            "decision_item": [
                "decision_id": "decision-follow-1", "candidate_id": "f1",
                "source_revision": "distill:1", "served_route": "follow_up", "selected": true
            ]
        ], [
            "annotation_id": "f2", "provider": "slack", "account_alias": "team", "thread_id": "t2",
            "lane": "user_assist", "created_at": 2
        ]], "total": 2]
        let sut = loadedSUT(followUps: followUps)
        let first = sut.messageFollowUps[0]
        let requestStarted = expectation(description: "follow-up request started")
        let requestMayFinish = DispatchSemaphore(value: 0)
        let firstMutationFinished = expectation(description: "first mutation finished")
        sut.$pendingCardMutationKeys.dropFirst().filter(\.isEmpty).prefix(1)
            .sink { _ in firstMutationFinished.fulfill() }.store(in: &cancellables)
        MockURLProtocol.handler = { request in
            XCTAssertTrue(request.url!.path.hasSuffix("/f1/dismiss"))
            let body = requestBody(request).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] } ?? [:]
            XCTAssertEqual(body["reason"] as? String, "already_handled")
            XCTAssertFalse((body["event_id"] as? String ?? "").isEmpty)
            let attribution = body["attribution"] as? [String: Any]
            XCTAssertEqual(attribution?["decision_id"] as? String, "decision-follow-1")
            XCTAssertEqual(attribution?["candidate_id"] as? String, "f1")
            XCTAssertEqual(attribution?["source_revision"] as? String, "distill:1")
            requestStarted.fulfill()
            _ = requestMayFinish.wait(timeout: .now() + 2)
            return (response(for: request), jsonData([:]))
        }
        sut.resolveFollowUp(first, action: "dismiss", reason: "already_handled")
        XCTAssertEqual(sut.messageFollowUps.map(\.id), ["f2"], "the card must leave before the request finishes")
        XCTAssertEqual(sut.messageFollowUpTotal, 1)
        wait(for: [requestStarted], timeout: 1)
        requestMayFinish.signal()
        wait(for: [firstMutationFinished], timeout: 2)
        XCTAssertEqual(sut.messageFollowUps.map(\.id), ["f2"])
        XCTAssertEqual(sut.messageFollowUpTotal, 1)

        let second = sut.messageFollowUps[0]
        MockURLProtocol.handler = { request in (response(for: request, status: 500), Data("resolve failed".utf8)) }
        performAndWaitForCardMutations(sut) { sut.resolveFollowUp(second, action: "acknowledge") }
        XCTAssertEqual(sut.messageFollowUps.map(\.id), ["f2"])
        XCTAssertEqual(sut.messageFollowUpTotal, 1)
        XCTAssertTrue(sut.error?.contains("resolve failed") == true)
    }

    @MainActor
    func testFollowUpPaginationCannotUndoPendingOptimisticTotal() {
        let followUps: [String: Any] = ["items": [
            ["annotation_id": "f1", "provider": "gmail", "account_alias": "work",
             "thread_id": "t1", "lane": "user_assist", "created_at": 1],
            ["annotation_id": "f2", "provider": "gmail", "account_alias": "work",
             "thread_id": "t2", "lane": "user_assist", "created_at": 2],
        ], "total": 2, "next_cursor": "next"]
        let sut = loadedSUT(followUps: followUps)
        let target = sut.messageFollowUps[0]
        let postStarted = expectation(description: "resolution started")
        let postMayFinish = DispatchSemaphore(value: 0)
        MockURLProtocol.handler = { request in
            if request.httpMethod == "POST" {
                postStarted.fulfill()
                _ = postMayFinish.wait(timeout: .now() + 2)
                return (response(for: request), jsonData([:]))
            }
            return (response(for: request), jsonData([
                "items": [
                    ["annotation_id": "f1", "provider": "gmail", "account_alias": "work",
                     "thread_id": "t1", "lane": "user_assist", "created_at": 1],
                    ["annotation_id": "f2", "provider": "gmail", "account_alias": "work",
                     "thread_id": "t2", "lane": "user_assist", "created_at": 2],
                ],
                "total": 2,
            ]))
        }

        sut.resolveFollowUp(target, action: "dismiss")
        XCTAssertEqual(sut.visibleMessageFollowUpTotal, 1)
        wait(for: [postStarted], timeout: 1)
        performAndWaitForIdle(sut) { sut.loadRemainingMessageFollowUps() }
        XCTAssertEqual(sut.visibleMessageFollowUpTotal, 1,
                       "a stale page total must not re-inflate the optimistic projection")
        postMayFinish.signal()
        waitUntil { sut.pendingCardMutationKeys.isEmpty }
    }

    /// THE BUG, on the tab Today opens on. Resolving a follow-up rewound the
    /// cursor to `nil`, so the next Load more re-asked for page one, `merging`
    /// deduped every row away, and the button did nothing.
    ///
    /// The cursor is a KEYSET position (`created_at`, `annotation_id`) carried
    /// whole in the cursor — the server never looks the row up again — so
    /// resolving a follow-up does not move it and there is nothing to rewind.
    @MainActor
    func testResolvingAFollowUpLeavesLoadMoreAbleToFetchTheNextPage() {
        func row(_ index: Int) -> [String: Any] {
            ["annotation_id": "f\(index)", "provider": "gmail", "account_alias": "work",
             "thread_id": "t\(index)", "lane": "user_assist", "created_at": index]
        }
        let sut = loadedSUT(followUps: ["items": [row(1), row(2)], "total": 4,
                                        "next_cursor": "page2"])
        XCTAssertEqual(sut.messageFollowUps.map(\.id), ["f1", "f2"])

        MockURLProtocol.handler = { request in
            if request.httpMethod == "POST" { return (response(for: request), jsonData([:])) }
            let items = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
            guard items.first(where: { $0.name == "cursor" })?.value == "page2" else {
                // Where a rewind lands: page one again, whose rows dedupe to
                // nothing new. That is the dead click, expressed as a fixture.
                return (response(for: request), jsonData(["items": [row(1), row(2)], "total": 4]))
            }
            return (response(for: request), jsonData(["items": [row(3), row(4)], "total": 4]))
        }

        // f2 is the LAST loaded row — the one the cursor was minted from, and
        // so the worst case for keeping it.
        performAndWaitForCardMutations(sut) {
            sut.resolveFollowUp(sut.messageFollowUps[1], action: "acknowledge")
        }
        XCTAssertEqual(sut.visibleMessageFollowUps.map(\.id), ["f1"])

        performAndWaitForIdle(sut) { sut.loadRemainingMessageFollowUps() }

        XCTAssertEqual(sut.visibleMessageFollowUps.map(\.id), ["f1", "f3", "f4"],
                       "Load more fetched the next page instead of re-serving the first")
    }

    @MainActor
    func testTodayLanePaginationCannotUndoPendingOptimisticDismissal() {
        let first = todayItemJSON("n1", section: "needs_you")
        let second = todayItemJSON("n2", section: "needs_you")
        let stalePage = todayPayload(needsYou: [first, second], sectionPage: [
            "section": "needs_you", "total": 2, "limit": 8, "has_more": false,
        ])
        let fixtureHandler = initialHandler(today: stalePage)
        let postStarted = expectation(description: "Today dismissal started")
        var finishPost: (() -> Void)?
        DeferredMockURLProtocol.handler = { request, completion in
            if request.httpMethod == "POST",
               request.url?.path == "/api/magician/v2/today/items/n1/visibility" {
                let response = response(for: request)
                DispatchQueue.main.async {
                    finishPost = { completion(.success((response, jsonData([:])))) }
                    postStarted.fulfill()
                }
                return
            }
            do {
                completion(.success(try fixtureHandler(request)))
            } catch {
                completion(.failure(error))
            }
        }
        let deferredSession = makeDeferredMockSession()
        sessions.append(deferredSession)
        let sut = loadedSUT(today: stalePage, networkSession: deferredSession)
        let target = sut.items(for: .needsYou)[0]
        let originalTotal = sut.counts.total

        sut.hide(target, action: "dismiss")
        XCTAssertEqual(sut.items(for: .needsYou).map(\.id), ["n2"])
        XCTAssertEqual(sut.counts.needsYou, 1)
        XCTAssertEqual(sut.counts.total, originalTotal - 1)
        wait(for: [postStarted], timeout: 1)

        performAndWaitForIdle(sut) { sut.loadRemaining(.needsYou) }
        XCTAssertEqual(sut.items(for: .needsYou).map(\.id), ["n2"])
        XCTAssertEqual(sut.counts.needsYou, 1,
                       "a stale lane page must not re-inflate the optimistic count")
        XCTAssertEqual(sut.counts.total, originalTotal - 1)

        XCTAssertNotNil(finishPost)
        finishPost?()
        waitUntil { sut.pendingCardMutationKeys.isEmpty }
    }

    func testStableRollbackAnchorsPreserveOrderAcrossConcurrentFailures() {
        var ledger = OptimisticCardOrderLedger()
        let first = ledger.begin(listKey: "lane", itemID: "a", currentIDs: ["a", "b", "c"])
        let second = ledger.begin(listKey: "lane", itemID: "b", currentIDs: ["b", "c"])

        func restored(_ id: String, with anchor: OptimisticCardListAnchor,
                      into values: inout [String]) {
            values.insert(id, at: anchor.insertionIndex(in: values))
        }

        var firstFailureOrder = ["c"]
        restored("a", with: first, into: &firstFailureOrder)
        restored("b", with: second, into: &firstFailureOrder)
        XCTAssertEqual(firstFailureOrder, ["a", "b", "c"])

        var secondFailureOrder = ["c"]
        restored("b", with: second, into: &secondFailureOrder)
        restored("a", with: first, into: &secondFailureOrder)
        XCTAssertEqual(secondFailureOrder, ["a", "b", "c"])
    }

    func testStableRollbackLedgerPreservesOrderWhenWholeLaneFailsInOrder() {
        var ledger = OptimisticCardOrderLedger()
        let first = ledger.begin(listKey: "lane", itemID: "a", currentIDs: ["a", "b", "c"])
        let second = ledger.begin(listKey: "lane", itemID: "b", currentIDs: ["b", "c"])
        let third = ledger.begin(listKey: "lane", itemID: "c", currentIDs: ["c"])
        var values: [String] = []

        for (id, anchor) in [("a", first), ("b", second), ("c", third)] {
            values.insert(id, at: anchor.insertionIndex(in: values))
        }

        XCTAssertEqual(values, ["a", "b", "c"])
    }

    func testCommittedTombstoneExpiresForLegitimateReissue() throws {
        let coordinator = CardMutationCoordinator()
        let key = OptimisticCardKey.channelFollowUp("same-id")
        let ticket = try XCTUnwrap(coordinator.begin(key))
        coordinator.succeed(ticket, suppressFor: -1)

        XCTAssertFalse(coordinator.isSuppressed(key))
        XCTAssertNotNil(coordinator.begin(key))
    }

    @MainActor
    func testSharedMessageTombstoneSuppressesThenRollsBack() throws {
        // Attention no longer carries follow-ups (live-only surface since
        // 2026-09-11), so the shared CardMutationCoordinator tombstone is
        // exercised through Today alone: suppress on mutation start, restore
        // on failure.
        let followUps: [String: Any] = ["items": [[
            "annotation_id": "shared-1", "provider": "gmail", "account_alias": "work",
            "thread_id": "thread-1", "lane": "user_assist", "created_at": 1
        ]], "total": 1]
        let coordinator = CardMutationCoordinator()
        let today = loadedSUT(followUps: followUps, mutationCoordinator: coordinator)
        let item = try XCTUnwrap(today.messageFollowUps.first)
        let requestStarted = expectation(description: "shared mutation started")
        let requestMayFinish = DispatchSemaphore(value: 0)
        let mutationFinished = expectation(description: "shared mutation rolled back")
        today.$pendingCardMutationKeys.dropFirst().filter(\.isEmpty).prefix(1)
            .sink { _ in mutationFinished.fulfill() }.store(in: &cancellables)
        MockURLProtocol.handler = { request in
            requestStarted.fulfill()
            _ = requestMayFinish.wait(timeout: .now() + 2)
            return (response(for: request, status: 503), Data("offline".utf8))
        }

        today.resolveFollowUp(item, action: "dismiss")
        XCTAssertTrue(today.visibleMessageFollowUps.isEmpty,
                      "the tombstone must suppress the mutating copy immediately")
        wait(for: [requestStarted], timeout: 1)
        requestMayFinish.signal()
        wait(for: [mutationFinished], timeout: 2)

        XCTAssertEqual(today.visibleMessageFollowUps.map(\.id), ["shared-1"])
    }

    @MainActor
    func testChannelMessageFetchPublishesEvidenceAndSectionScopedFailure() throws {
        let followUps: [String: Any] = ["items": [[
            "annotation_id": "f1", "provider": "gmail", "account_alias": "work", "thread_id": "t1",
            "lane": "user_assist", "created_at": 1
        ]], "total": 1]
        let sut = loadedSUT(followUps: followUps)
        let item = sut.messageFollowUps[0]
        MockURLProtocol.handler = { request in
            (response(for: request), jsonData([
                "body": "Latest body", "summary": "Summary", "subject": "Subject", "has_newer": true,
                "evidence_messages": [["message_id": "m1", "body": "Evidence", "subject": "Earlier", "received_at": 10]]
            ]))
        }
        performAndWaitForIdle(sut) { sut.fetchChannelMessage(item) }
        XCTAssertEqual(sut.channelMessages["f1"]?.evidenceMessages.first?.body, "Evidence")
        XCTAssertEqual(sut.channelMessages["f1"]?.hasNewer, true)

        MockURLProtocol.handler = { request in (response(for: request, status: 404), Data("message unavailable".utf8)) }
        performAndWaitForIdle(sut) { sut.fetchChannelMessage(item) }
        XCTAssertTrue(sut.sectionErrors["message:f1"]?.contains("message unavailable") == true)
        XCTAssertNil(sut.error)
    }

    @MainActor
    func testResurfacingDetailOriginalDismissAndErrorPaths() throws {
        let resurfacing: [String: Any] = ["cards": [[
            "candidate_id": "r1", "line": "Review", "why_now": "Now", "source_title": "Policy",
            "summary": "Changed", "source_kind": "memory", "content_revision": "rev-1",
            "recommended_action": ["kind": "create_task", "label": "Create task", "rationale": "Act",
                                     "confidence": 0.9, "content_revision": "rev-1", "source": "curator"]
        ]], "total": 1]
        let sut = loadedSUT(resurfacing: resurfacing)
        let card = sut.resurfacingCards[0]
        let presented = expectation(description: "recommendation presented")
        MockURLProtocol.handler = { request in
            if request.url!.path.hasSuffix("/detail") {
                return (response(for: request), jsonData([
                    "candidate_id": "r1", "source_kind": "memory", "status": "available", "title": "Policy",
                    "summary": "Current", "content_revision": "rev-2", "source_updated": true, "has_newer": true,
                    "actions": [["kind": "create_task", "label": "Create task", "requires_input": true, "side_effect": "creates_task"]]
                ]))
            }
            if request.url!.path.hasSuffix("/recommendation-event") {
                let body = requestBody(request).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] } ?? [:]
                XCTAssertEqual(body["event"] as? String, "presented")
                presented.fulfill()
                return (response(for: request), jsonData([:]))
            }
            XCTFail("Unexpected detail request: \(request.url!.absoluteString)")
            return (response(for: request, status: 404), Data())
        }
        performAndWaitForIdle(sut) { sut.fetchResurfacingDetail(card) }
        wait(for: [presented], timeout: 1)
        XCTAssertEqual(sut.resurfacingDetails["r1"]?.contentRevision, "rev-2")
        XCTAssertEqual(sut.resurfacingDetails["r1"]?.hasNewer, true)

        MockURLProtocol.handler = { request in
            XCTAssertTrue(request.url!.path.hasSuffix("/original"))
            return (response(for: request), jsonData([
                "candidate_id": "r1", "source_kind": "memory", "status": "available", "source_updated": false,
                "has_newer": false, "actions": [], "original": ["body": "Original content"]
            ]))
        }
        performAndWaitForIdle(sut) { sut.fetchResurfacingDetail(card, original: true) }
        XCTAssertEqual(sut.resurfacingDetails["r1"]?.original?.objectValue?["body"]?.stringValue, "Original content")

        MockURLProtocol.handler = { request in (response(for: request, status: 409), Data("stale revision".utf8)) }
        performAndWaitForIdle(sut) { sut.performResurfacingAction(card, kind: .createTask) }
        XCTAssertTrue(sut.sectionErrors["resurfacing-action:r1:create_task"]?.contains("stale revision") == true)

        MockURLProtocol.handler = { request in (response(for: request), jsonData([:])) }
        performAndWaitForCardMutations(sut) { sut.dismissResurfacing(card) }
        XCTAssertTrue(sut.resurfacingCards.isEmpty)
        XCTAssertEqual(sut.resurfacingTotal, 0)
    }

    @MainActor
    func testResurfacingFeedbackActionsUseCanonicalPayloadAndResolveTheCard() throws {
        let resurfacing: [String: Any] = ["cards": [[
            "candidate_id": "r1", "line": "Review", "why_now": "Now",
            "source_title": "Policy", "summary": "Changed", "source_kind": "memory",
            "source_revision": "revision-r1",
            "decision_item": [
                "decision_id": "decision-worth-r1", "candidate_id": "r1",
                "source_revision": "revision-r1", "served_route": "worth_a_look", "selected": true
            ]
        ]], "total": 1]

        XCTAssertEqual(ResurfacingFeedbackAction.allCases.map(\.label), [
            "Mark useful", "Acknowledge", "Dismiss"
        ])

        for action in ResurfacingFeedbackAction.allCases {
            let sut = loadedSUT(resurfacing: resurfacing)
            let card = try XCTUnwrap(sut.resurfacingCards.first)
            MockURLProtocol.handler = { request in
                XCTAssertEqual(request.httpMethod, "POST")
                XCTAssertEqual(
                    request.url?.path,
                    "/api/magician/v2/channel-assist/resurfacing/r1/action"
                )
                let body = try XCTUnwrap(requestBody(request))
                let object = try XCTUnwrap(
                    JSONSerialization.jsonObject(with: body) as? [String: Any]
                )
                XCTAssertEqual(object["action"] as? String, action.rawValue)
                XCTAssertFalse((object["event_id"] as? String ?? "").isEmpty)
                let attribution = try XCTUnwrap(object["attribution"] as? [String: Any])
                XCTAssertEqual(attribution["decision_id"] as? String, "decision-worth-r1")
                XCTAssertEqual(attribution["candidate_id"] as? String, "r1")
                XCTAssertEqual(attribution["source_revision"] as? String, "revision-r1")
                XCTAssertNil(attribution["impression_id"])
                XCTAssertNil(attribution["delivery_id"])
                return (response(for: request), jsonData([:]))
            }

            performAndWaitForCardMutations(sut) {
                sut.resolveResurfacing(card, action: action)
            }

            XCTAssertTrue(sut.resurfacingCards.isEmpty)
            XCTAssertEqual(sut.resurfacingTotal, 0)
        }
    }

    @MainActor
    func testFailedResurfacingFeedbackKeepsTheCardVisible() throws {
        let resurfacing: [String: Any] = ["cards": [[
            "candidate_id": "r1", "line": "Review", "why_now": "Now",
            "source_title": "Policy", "summary": "Changed", "source_kind": "memory"
        ]], "total": 1]
        let sut = loadedSUT(resurfacing: resurfacing)
        let card = try XCTUnwrap(sut.resurfacingCards.first)
        MockURLProtocol.handler = { request in
            (response(for: request, status: 503), Data("feedback unavailable".utf8))
        }

        performAndWaitForCardMutations(sut) {
            sut.resolveResurfacing(card, action: .acknowledge)
        }

        XCTAssertEqual(sut.resurfacingCards.map(\.id), ["r1"])
        XCTAssertEqual(sut.resurfacingTotal, 1)
        XCTAssertTrue(sut.error?.contains("feedback unavailable") == true)
    }

    @MainActor
    func testResurfacingFeedbackDisappearsBeforeTheRequestCompletes() throws {
        let resurfacing: [String: Any] = ["cards": [[
            "candidate_id": "r1", "line": "Review", "why_now": "Now",
            "source_title": "Policy", "summary": "Changed", "source_kind": "memory"
        ]], "total": 1]
        let sut = loadedSUT(resurfacing: resurfacing)
        let card = try XCTUnwrap(sut.resurfacingCards.first)
        let requestStarted = expectation(description: "resurfacing request started")
        let requestMayFinish = DispatchSemaphore(value: 0)
        let mutationFinished = expectation(description: "resurfacing mutation finished")
        sut.$pendingCardMutationKeys.dropFirst().filter(\.isEmpty).prefix(1)
            .sink { _ in mutationFinished.fulfill() }.store(in: &cancellables)
        MockURLProtocol.handler = { request in
            requestStarted.fulfill()
            _ = requestMayFinish.wait(timeout: .now() + 2)
            return (response(for: request), jsonData([:]))
        }

        sut.resolveResurfacing(card, action: .dismiss)
        XCTAssertTrue(sut.visibleResurfacingCards.isEmpty)
        XCTAssertEqual(sut.visibleResurfacingTotal, 0)
        wait(for: [requestStarted], timeout: 1)
        requestMayFinish.signal()
        wait(for: [mutationFinished], timeout: 2)
        XCTAssertTrue(sut.visibleResurfacingCards.isEmpty)
    }

    @MainActor
    func testDigestPagingPreservesLanesClampsOffsetAndScopesErrors() {
        let sut = loadedSUT()
        let originalNeedsYou = sut.items(for: .needsYou)
        MockURLProtocol.handler = { request in
            XCTAssertTrue(request.url!.query!.contains("digest_offset=7"))
            return (response(for: request), self.todayPayload(digest: [
                "generated_at": 2_000, "total": 9, "limit": 7, "offset": 7,
                "bullets": [["id": "d8", "text": "Later change", "source_kind": "task", "source_id": "t8", "space_ids": [], "updated_at": 2]]
            ]))
        }
        performAndWaitForDigest(sut) { sut.loadDigest(offset: 7) }
        XCTAssertEqual(sut.digestOffset, 7)
        XCTAssertEqual(sut.digest.bullets.map(\.id), ["d8"])
        XCTAssertEqual(sut.items(for: .needsYou), originalNeedsYou)

        MockURLProtocol.handler = { request in (response(for: request, status: 503), Data("digest offline".utf8)) }
        performAndWaitForDigest(sut) { sut.loadDigest(offset: -20) }
        XCTAssertEqual(sut.digestOffset, 7)
        XCTAssertTrue(sut.sectionErrors["digest"]?.contains("digest offline") == true)
        XCTAssertNil(sut.error)
    }

    /// A reader east of UTC, at an hour where their calendar date and the UTC
    /// date name different days. Every `/today` predicate is a date comparison,
    /// so asking with the server's date is a wrong answer that looks entirely
    /// right — and only for part of every day.
    @MainActor
    func testTodayRequestCarriesTheReaderLocalDateNotItsUTCRendering() {
        var utc = Calendar(identifier: .gregorian)
        utc.timeZone = TimeZone(secondsFromGMT: 0)!
        // 01:30 on the 31st in IST. In UTC the same instant is still the 30th.
        let instant = utc.date(from: DateComponents(year: 2026, month: 7, day: 30, hour: 20))!
        let ist = TimeZone(identifier: "Asia/Kolkata")!
        // The fixture must actually straddle the boundary; if these two ever
        // named the same day the assertion below would agree for no reason.
        XCTAssertEqual(TasksViewModel.localDateISO(instant, in: ist), "2026-07-31")
        XCTAssertEqual(TasksViewModel.localDateISO(instant, in: TimeZone(secondsFromGMT: 0)!), "2026-07-30")

        let lock = NSLock()
        var sentDates: [String?] = []
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.url!.path, "/api/magician/v2/today")
            let sent = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?
                .queryItems?.first(where: { $0.name == "today" })?.value
            lock.lock(); sentDates.append(sent); lock.unlock()
            return (response(for: request), self.todayPayload())
        }

        let sut = TodayViewModel(networkSession: mockSession(), baseURL: URL(string: "https://example.com")!,
                                 principal: "person-1", workspace: "space-1", refreshesAfterActions: false)
        sut.readerLocalDate = { TasksViewModel.localDateISO(instant, in: ist) }
        performAndWaitForDigest(sut) { sut.loadDigest(offset: 0) }

        lock.lock(); let observed = sentDates; lock.unlock()
        // The value, not the parameter's presence. A UTC rendering of this very
        // instant is "2026-07-30", so a test that merely checked `today` was
        // set would pass against the exact defect this closes.
        XCTAssertEqual(observed, ["2026-07-31"])
    }

    @MainActor
    func testBriefingRenderAndViewAllHaveIndependentSuccessAndFailureState() throws {
        let initialBriefings: [String: Any] = ["surfaces": [["surface": [
            "surface_id": "b1", "route": "/briefing", "title": "Morning", "published_at": "2026-07-13T00:00:00Z"
        ]]]]
        let sut = loadedSUT(briefings: initialBriefings)
        let briefing = sut.briefings[0]
        MockURLProtocol.handler = { request in
            XCTAssertTrue(request.url!.path.hasSuffix("/b1/render"))
            XCTAssertFalse(request.url?.query?.contains("principal=") ?? false)
            XCTAssertFalse(request.url?.query?.contains("workspace=") ?? false)
            return (response(for: request), jsonData(["render": ["text_content": "Rendered briefing", "source_agent_id": "presto"]]))
        }
        performAndWaitForIdle(sut) { sut.fetchBriefingRender(briefing) }
        XCTAssertEqual(sut.briefingRenders["b1"]?.textContent, "Rendered briefing")

        MockURLProtocol.handler = { request in
            XCTAssertTrue(request.url!.query!.contains("limit=50"))
            return (response(for: request), jsonData(["surfaces": [
                ["surface": ["surface_id": "b1", "route": "/briefing", "title": "Morning", "published_at": "2026-07-13T00:00:00Z"]],
                ["surface": ["surface_id": "b2", "route": "/briefing", "title": "Evening", "published_at": "2026-07-13T12:00:00Z"]]
            ]]))
        }
        performAndWaitForIdle(sut) { sut.loadAllBriefings() }
        XCTAssertEqual(sut.briefings.map(\.id), ["b1", "b2"])

        MockURLProtocol.handler = { request in (response(for: request, status: 500), Data("briefings failed".utf8)) }
        performAndWaitForIdle(sut) { sut.loadAllBriefings() }
        XCTAssertEqual(sut.briefings.map(\.id), ["b1", "b2"])
        XCTAssertTrue(sut.sectionErrors["briefings"]?.contains("briefings failed") == true)
    }

    @MainActor
    func testActivityDurabilityRemovalAndPartialClearFailure() throws {
        let activities: [String: Any] = ["items": [
            ["id": "learning", "item_type": "agent_learning", "title": "Learned", "status": "info", "updated_at": 1],
            ["id": "delivery", "item_type": "data_delivery", "title": "Delivered", "status": "done", "updated_at": 1],
            ["id": "failed", "item_type": "task", "title": "Failed", "status": "failed", "updated_at": 1],
            ["id": "outcome", "item_type": "task", "title": "Done", "summary": "Completed successfully", "status": "done", "updated_at": 1],
            ["id": "artifact", "item_type": "task", "title": "Artifact", "status": "done", "updated_at": 1,
             "metadata": ["completion_artifact_names": ["report.pdf"]]],
            ["id": "noise", "item_type": "approval", "title": "Approval", "status": "needs_action", "updated_at": 1],
            ["id": "empty-task", "item_type": "task", "title": "Empty", "status": "done", "updated_at": 1]
        ]]
        let sut = loadedSUT(activity: activities)
        XCTAssertEqual(sut.activityItems.map(\.id), ["learning", "delivery", "failed", "outcome", "artifact"])

        let removed = expectation(description: "single activity removed")
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.httpMethod, "DELETE")
            XCTAssertFalse(request.url?.query?.contains("principal=") ?? false)
            XCTAssertFalse(request.url?.query?.contains("workspace=") ?? false)
            removed.fulfill()
            return (response(for: request), Data())
        }
        sut.removeActivity(sut.activityItems[0])
        wait(for: [removed], timeout: 2)
        XCTAssertFalse(sut.activityItems.contains { $0.id == "learning" })

        let clearFinished = expectation(description: "partial clear finished")
        clearFinished.expectedFulfillmentCount = 4
        MockURLProtocol.handler = { request in
            clearFinished.fulfill()
            if request.url!.path.hasSuffix("/failed") { return (response(for: request, status: 500), Data("delete failed".utf8)) }
            return (response(for: request), Data())
        }
        sut.clearActivity()
        XCTAssertTrue(sut.activityItems.isEmpty)
        wait(for: [clearFinished], timeout: 3)
        waitForMainQueue()
        XCTAssertEqual(sut.activityItems.map(\.id), ["failed"])
        XCTAssertTrue(sut.error?.contains("delete failed") == true)
    }

    func testRealtimeEventClassifierRejectsNoiseAndAcceptsParityFamilies() {
        XCTAssertTrue(TodayViewModel.isRelevantRealtimeEvent(#"{"event_type":"task.completed"}"#))
        XCTAssertTrue(TodayViewModel.isRelevantRealtimeEvent(#"{"type":"channel.follow_up.updated"}"#))
        XCTAssertTrue(TodayViewModel.isRelevantRealtimeEvent(#"{"event_type":"published.surface"}"#))
        XCTAssertFalse(TodayViewModel.isRelevantRealtimeEvent(#"{"event_type":"heartbeat"}"#))
        XCTAssertFalse(TodayViewModel.isRelevantRealtimeEvent("not-json"))
    }

    func testJSONRoundTripAndLocalTimeFormattingEdgeCases() throws {
        let value: JSONValue = .object(["string": .string("text"), "number": .number(2.5),
                                        "bool": .bool(true), "array": .array([.null, .string("x")])])
        XCTAssertEqual(try JSONDecoder().decode(JSONValue.self, from: JSONEncoder().encode(value)), value)
        XCTAssertEqual(TodayViewModel.relativeTime(0), "just now")
        XCTAssertEqual(TodayViewModel.relativeTime(3_000, now: Date(timeIntervalSince1970: 63)), "1m ago")
        XCTAssertEqual(TodayViewModel.relativeTime(1_000, now: Date(timeIntervalSince1970: 172_801)), "2d ago")
        XCTAssertEqual(TodayViewModel.futureDistance(nil), "until later")
        XCTAssertEqual(TodayViewModel.futureDistance(1_000, now: Date(timeIntervalSince1970: 2)), "until now")
        XCTAssertEqual(TodayViewModel.futureDistance(3_601_000, now: Date(timeIntervalSince1970: 1)), "for 1h")
        XCTAssertEqual(Set(ResurfacingActionKind.allCases.map(\.systemImage)).count, ResurfacingActionKind.allCases.count)
    }

    @MainActor
    func testMeetingActionCreatesTaskRemovesSourceCardAndReturnsNavigation() {
        let itemID = "today:followups:meeting_action:weekly"
        let endpoint = "/api/magician/v2/today/items/\(itemID)/actions/create_task"
        let today = jsonData([
            "generated_at": 1_000, "headline": "One follow-up", "digest": ["total": 0, "bullets": []],
            "sections": [
                "needs_you": [],
                "followups": [[
                    "id": itemID, "section": "followups", "priority": 730,
                    "title": "Meeting action: Send notes", "reason": "Captured from a meeting",
                    "source_kind": "meeting_action", "source_id": "meeting:weekly:action:0",
                    "status": "needs_action", "created_at": 1, "updated_at": 1,
                    "actions": [[
                        "id": "create_task", "label": "Create task",
                        "action_type": "today_source_action",
                        "payload": ["method": "POST", "endpoint": endpoint, "icon": "checklist"]
                    ]]
                ]],
                "active_work": [], "delivered": [], "changed": []
            ],
            "counts": ["needs_you": 0, "followups": 1, "active_work": 0, "delivered": 0, "changed": 0, "total": 1]
        ])
        let sut = loadedSUT(today: today)
        let item = sut.items(for: .followups)[0]
        XCTAssertEqual(item.actions.first?.executionEndpoint, endpoint)
        XCTAssertEqual(item.actions.first?.systemImage, "checklist")

        let requested = expectation(description: "Today source action request")
        let navigated = expectation(description: "Task navigation returned")
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.httpMethod, "POST")
            XCTAssertEqual(request.url?.path, endpoint)
            XCTAssertFalse(request.url?.query?.contains("principal=") ?? false)
            XCTAssertFalse(request.url?.query?.contains("workspace=") ?? false)
            requested.fulfill()
            return (response(for: request), jsonData([
                "task_id": "task_meeting_action_weekly",
                "navigate_to": ["kind": "task", "task_id": "task_meeting_action_weekly"]
            ]))
        }
        performAndWaitForIdle(sut) {
            sut.performTodayAction(item.actions[0], for: item) { taskID in
                XCTAssertEqual(taskID, "task_meeting_action_weekly")
                navigated.fulfill()
            }
        }

        wait(for: [requested, navigated], timeout: 1)
        XCTAssertTrue(sut.items(for: .followups).isEmpty)
        XCTAssertEqual(sut.counts.followups, 0)
        XCTAssertNil(sut.error)
    }

    private func todayPayload(changed: [[String: Any]] = [],
                              needsYou: [[String: Any]]? = nil,
                              sectionPage: [String: Any]? = nil,
                              digest: [String: Any]? = nil) -> Data {
        let needsYouRows = needsYou ?? [[
            "id": "n1", "section": "needs_you", "priority": 10, "title": "Approve",
            "reason": "Blocked", "source_kind": "task", "source_id": "task-1", "task_id": "task-1",
            "status": "needs_action", "created_at": 1, "updated_at": 1
        ]]
        var value: [String: Any] = [
            "generated_at": 1_000,
            "headline": "One thing needs you.",
            "digest": digest ?? ["total": 1, "bullets": [[
                "id": "d1", "text": "Memory changed", "source_kind": "memory", "space_ids": [], "updated_at": 1
            ]]],
            "sections": [
                "needs_you": needsYouRows,
                "followups": [[
                    "id": "fu1", "section": "followups", "priority": 1, "title": "Follow up",
                    "reason": "Due", "source_kind": "task", "source_id": "task-2",
                    "status": "info", "created_at": 1, "updated_at": 1
                ]],
                "active_work": [], "delivered": [], "changed": changed
            ],
            "counts": ["needs_you": needsYouRows.count, "followups": 2, "active_work": 0, "delivered": 0,
                       "changed": max(1, changed.count), "total": 3 + needsYouRows.count + changed.count]
        ]
        if let sectionPage { value["section_page"] = sectionPage }
        return jsonData(value)
    }

    private func todayItemJSON(_ id: String, section: String = "changed", title: String? = nil) -> [String: Any] {
        ["id": id, "section": section, "priority": 1, "title": title ?? id, "reason": "Changed",
         "source_kind": "memory", "source_id": id, "space_ids": [], "status": "info",
         "created_at": 1, "updated_at": 1]
    }

    @MainActor
    private func loadedSUT(today: Data? = nil,
                           hidden: Any = ["items": []],
                           resurfacing: Any = ["cards": [], "total": 0],
                           followUps: Any = ["items": [], "total": 0],
                           activity: Any = ["items": []],
                           briefings: Any = ["surfaces": []],
                           mutationCoordinator: CardMutationCoordinator? = nil,
                           networkSession: URLSession? = nil) -> TodayViewModel {
        let todayData = today ?? todayPayload()
        MockURLProtocol.handler = initialHandler(today: todayData, hidden: hidden, resurfacing: resurfacing,
                                                 followUps: followUps, activity: activity, briefings: briefings)
        let sut = TodayViewModel(networkSession: networkSession ?? mockSession(), baseURL: URL(string: "https://example.com")!,
                                 principal: "person-1", workspace: "space-1", refreshesAfterActions: false,
                                 mutationCoordinator: mutationCoordinator)
        let loaded = expectation(description: "Today fixture loaded")
        sut.$isLoading.dropFirst().filter { !$0 }.prefix(1).sink { _ in loaded.fulfill() }.store(in: &cancellables)
        sut.fetch()
        wait(for: [loaded], timeout: 3)
        return sut
    }

    private func initialHandler(today: Data,
                                hidden: Any = ["items": []],
                                resurfacing: Any = ["cards": [], "total": 0],
                                followUps: Any = ["items": [], "total": 0],
                                activity: Any = ["items": []],
                                briefings: Any = ["surfaces": []]) -> (URLRequest) throws -> (HTTPURLResponse, Data) {
        { request in
            switch request.url!.path {
            case "/api/magician/v2/today": return (response(for: request), today)
            case "/api/magician/v2/today/visibility": return (response(for: request), jsonData(hidden))
            case "/api/magician/v2/channel-assist/resurfacing/today": return (response(for: request), jsonData(resurfacing))
            case "/api/magician/v2/channel-assist/follow-ups": return (response(for: request), jsonData(followUps))
            case "/api/magician/v2/feed": return (response(for: request), jsonData(activity))
            case "/api/magician/v3/published-surfaces/projections": return (response(for: request), jsonData(briefings))
            case "/api/magician/v2/analytics/llm_calls/query":
                return (response(for: request), jsonData(["columns": ["section", "k", "model", "v1", "v2"], "rows": []]))
            case "/api/magician/v2/analytics/query":
                return (response(for: request), jsonData(["columns": ["section", "n"], "rows": []]))
            case "/api/magician/v2/analytics/memory_events/query":
                return (response(for: request), jsonData(["columns": ["section", "n", "passes"], "rows": []]))
            case "/api/magician/v3/tasks": return (response(for: request), jsonData(["tasks": []]))
            case "/api/magician/v2/agents": return (response(for: request), jsonData(["agents": []]))
            case "/api/magician/v2/agents/updates": return (response(for: request), jsonData(["events": []]))
            default:
                XCTFail("Unexpected fixture request: \(request.httpMethod ?? "") \(request.url!.absoluteString)")
                return (response(for: request, status: 404), Data())
            }
        }
    }

    @MainActor
    private func performAndWaitForIdle(_ sut: TodayViewModel, action: () -> Void,
                                       file: StaticString = #filePath, line: UInt = #line) {
        let idle = expectation(description: "Today action completed")
        sut.$actionItemID.dropFirst().filter { $0 == nil }.prefix(1).sink { _ in idle.fulfill() }.store(in: &cancellables)
        action()
        wait(for: [idle], timeout: 3)
    }

    @MainActor
    private func performAndWaitForCardMutations(_ sut: TodayViewModel, action: () -> Void,
                                                file: StaticString = #filePath, line: UInt = #line) {
        let idle = expectation(description: "Optimistic card mutation completed")
        sut.$pendingCardMutationKeys.dropFirst().filter(\.isEmpty).prefix(1)
            .sink { _ in idle.fulfill() }.store(in: &cancellables)
        action()
        wait(for: [idle], timeout: 3)
    }

    @MainActor
    private func performAndWaitForDigest(_ sut: TodayViewModel, action: () -> Void) {
        let idle = expectation(description: "Digest action completed")
        sut.$isDigestLoading.dropFirst().filter { !$0 }.prefix(1).sink { _ in idle.fulfill() }.store(in: &cancellables)
        action()
        wait(for: [idle], timeout: 3)
    }

    private func mockSession() -> URLSession {
        let session = makeMockSession()
        sessions.append(session)
        return session
    }
}
