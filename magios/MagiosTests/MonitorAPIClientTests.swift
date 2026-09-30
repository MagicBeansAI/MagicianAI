//  MonitorAPIClientTests.swift
//  Recurring Monitors (Phase 5, iOS) — `Monitors.APIClient` verification via
//  the `LTMAPIClientTests` MockTransport pattern: a mock transport records
//  the request and replies with canned `(Data, status)`. For each endpoint we
//  assert the REQUEST (method / path / query / scope headers / body), the
//  RESPONSE decoding, and the stable-reason error mapping.

import XCTest
@testable import Magician

/// Records the request it was handed and replies with a pre-seeded body.
///
/// **`send` is LOCKED, and that is a crash fix rather than defensive tidying.**
/// `MonitorDetailViewModel.load()` issues four `async let` fetches that all
/// reach this one instance concurrently, so `requestCount += 1` and
/// `script.removeFirst()` were being executed from four tasks at once with no
/// synchronisation — an unsynchronised mutation of an `Array`'s storage, which
/// is undefined behaviour and was observed as `signal segv` in
/// `testDetailLoadErrorSurfaces` under full-suite load. It reproduced in two
/// consecutive full runs and never once in three isolated runs of
/// `MonitorsViewModelTests`, which is the signature of exactly this race.
///
/// The `@unchecked Sendable` above is the reason the compiler had nothing to
/// say: it is an assertion by the author that this type is safe to share, and
/// until now it was not true. The lock is what makes it true.
final class MonitorMockTransport: Monitors.Transport, @unchecked Sendable {
    private let lock = NSLock()

    private var _lastRequest: URLRequest?
    private var _requestCount = 0
    private var _nextBody = Data("{}".utf8)
    private var _nextStatus = 200
    private var _script: [(Data, Int)] = []

    var lastRequest: URLRequest? { lock.withLock { _lastRequest } }
    var requestCount: Int { lock.withLock { _requestCount } }
    var nextBody: Data {
        get { lock.withLock { _nextBody } }
        set { lock.withLock { _nextBody = newValue } }
    }
    var nextStatus: Int {
        get { lock.withLock { _nextStatus } }
        set { lock.withLock { _nextStatus = newValue } }
    }
    /// Optional per-call script (consumed front-first); falls back to
    /// `nextBody`/`nextStatus` when empty.
    var script: [(Data, Int)] {
        get { lock.withLock { _script } }
        set { lock.withLock { _script = newValue } }
    }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        // One critical section for the whole read-modify-write, not one per
        // property: a per-property lock would still let two concurrent sends
        // take the same scripted reply, which is a wrong-answer bug rather than
        // a crash and therefore harder to notice.
        let (body, status): (Data, Int) = lock.withLock {
            _lastRequest = request
            _requestCount += 1
            return _script.isEmpty ? (_nextBody, _nextStatus) : _script.removeFirst()
        }
        let http = HTTPURLResponse(
            url: request.url!, statusCode: status, httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"])!
        return (body, http)
    }

    func seed(_ json: String, status: Int = 200) {
        lock.withLock {
            _nextBody = Data(json.utf8)
            _nextStatus = status
        }
    }
}

final class MonitorAPIClientTests: XCTestCase {

    private let baseURL = URL(string: "https://ios.example.test")!
    private var transport: MonitorMockTransport!
    private var client: Monitors.APIClient!

    override func setUp() {
        super.setUp()
        transport = MonitorMockTransport()
        client = Monitors.APIClient(
            baseURL: baseURL,
            scope: Monitors.Scope(principal: "anonymous", workspace: "default"),
            transport: transport)
    }

    // MARK: - Shared fixtures

    static let listPageJSON = """
    {"items":[{"task_id":"task_1","title":"A","objective":"watch a","state":"active",
      "cadence_summary":"Cron 0 6 * * 1 (America/Los_Angeles)","monitor_revision":1,
      "last_run_status":"never_ran","health":"ok"}],
     "next_cursor":"cur_task_1","limit":50}
    """

    static let specJSON = """
    {"schema_version":1,"objective":"Watch pricing","query_seeds":[],
     "sources":{"urls":["https://a.example/p"],"domains":[],"authenticated_sources":[]},
     "include_rules":[],"exclude_rules":[],"match_mode":"balanced",
     "notification_policy":"material_changes","notify_initial_baseline":false}
    """

    private func spec() throws -> Monitors.SpecV1 {
        try JSONDecoder().decode(Monitors.SpecV1.self, from: Data(Self.specJSON.utf8))
    }

    // MARK: - Request assertion helpers

    private func assertScopeHeaders(
        _ request: URLRequest, file: StaticString = #filePath, line: UInt = #line
    ) {
        XCTAssertNil(request.value(forHTTPHeaderField: "X-Principal"), file: file, line: line)
        XCTAssertNil(request.value(forHTTPHeaderField: "X-Workspace"), file: file, line: line)
    }

    private func queryValue(_ request: URLRequest, _ name: String) -> String? {
        guard let url = request.url,
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
        else { return nil }
        return components.queryItems?.first { $0.name == name }?.value
    }

    private func bodyObject(_ request: URLRequest) -> [String: Any] {
        guard let body = request.httpBody,
              let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any]
        else { return [:] }
        return object
    }

    // MARK: - list

    func testListSendsQueryAndDecodesEnvelope() async throws {
        transport.seed(Self.listPageJSON)
        let page = try await client.list(limit: 25, cursor: "cur_task_0", state: "active")
        let request = try XCTUnwrap(transport.lastRequest)
        XCTAssertEqual(request.httpMethod, "GET")
        XCTAssertEqual(request.url?.path, "/api/magician/v3/monitors")
        assertScopeHeaders(request)
        XCTAssertEqual(queryValue(request, "limit"), "25")
        XCTAssertEqual(queryValue(request, "cursor"), "cur_task_0")
        XCTAssertEqual(queryValue(request, "state"), "active")
        XCTAssertEqual(page.items.count, 1)
        XCTAssertEqual(page.items.first?.taskID, "task_1")
        XCTAssertEqual(page.nextCursor, "cur_task_1")
    }

    func testListOmitsAbsentCursorAndState() async throws {
        transport.seed("""
        {"items":[],"next_cursor":null,"limit":50}
        """)
        let page = try await client.list()
        let request = try XCTUnwrap(transport.lastRequest)
        XCTAssertNil(queryValue(request, "cursor"))
        XCTAssertNil(queryValue(request, "state"))
        XCTAssertNil(page.nextCursor, "null cursor = last page")
    }

    // MARK: - detail

    func testDetailPathAndNotFoundMapping() async throws {
        transport.seed("""
        {"error":"monitor_not_found","task_id":"task_plain"}
        """, status: 404)
        do {
            _ = try await client.detail("task_plain")
            XCTFail("plain task must map to .notFound")
        } catch let error as Monitors.APIError {
            XCTAssertEqual(error, .notFound)
        }
        let request = try XCTUnwrap(transport.lastRequest)
        XCTAssertEqual(request.url?.path, "/api/magician/v3/monitors/task_plain")
        assertScopeHeaders(request)
    }

    // MARK: - create / update

    func testCreatePostsBodyAndAccepts201() async throws {
        transport.seed("""
        {"task_id":"task_new","monitor_revision":1}
        """, status: 201)
        let schedule = Monitors.ScheduleWire(
            kind: .cron(expression: "0 9 * * *", timezone: "America/Los_Angeles"))
        let response = try await client.create(title: "Pricing", spec: try spec(),
                                               schedule: schedule)
        XCTAssertEqual(response.taskID, "task_new")
        XCTAssertEqual(response.monitorRevision, 1)
        let request = try XCTUnwrap(transport.lastRequest)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path, "/api/magician/v3/monitors")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")
        let body = bodyObject(request)
        XCTAssertEqual(body["title"] as? String, "Pricing")
        let specBody = try XCTUnwrap(body["spec"] as? [String: Any])
        XCTAssertEqual(specBody["schema_version"] as? Int, 1)
        XCTAssertEqual(specBody["notification_policy"] as? String, "material_changes")
        // Externally-tagged schedule kind on the wire.
        let kind = try XCTUnwrap((body["schedule"] as? [String: Any])?["kind"] as? [String: Any])
        let cron = try XCTUnwrap(kind["Cron"] as? [String: Any])
        XCTAssertEqual(cron["expression"] as? String, "0 9 * * *")
        XCTAssertEqual(cron["timezone"] as? String, "America/Los_Angeles")
    }

    func testCreateOmitsNilTitleAndSchedule() async throws {
        transport.seed("""
        {"task_id":"task_new","monitor_revision":1}
        """, status: 201)
        _ = try await client.create(spec: try spec())
        let body = bodyObject(try XCTUnwrap(transport.lastRequest))
        XCTAssertNil(body["title"])
        XCTAssertNil(body["schedule"])
        XCTAssertNotNil(body["spec"])
    }

    func testCreateMapsAdmissionRejection() async throws {
        transport.seed("""
        {"error":"monitor_sources_required"}
        """, status: 400)
        do {
            _ = try await client.create(spec: try spec())
            XCTFail("400 must map to .validation")
        } catch let error as Monitors.APIError {
            XCTAssertEqual(error, .validation(reason: "monitor_sources_required"))
        }
    }

    func testUpdatePatchesProvidedFieldsOnly() async throws {
        transport.seed("""
        {"task_id":"task_1","monitor_revision":3}
        """)
        let response = try await client.update("task_1", spec: try spec())
        XCTAssertEqual(response.monitorRevision, 3)
        let request = try XCTUnwrap(transport.lastRequest)
        XCTAssertEqual(request.httpMethod, "PATCH")
        XCTAssertEqual(request.url?.path, "/api/magician/v3/monitors/task_1")
        let body = bodyObject(request)
        XCTAssertNil(body["title"], "absent fields stay off the wire (PATCH semantics)")
        XCTAssertNil(body["schedule"])
        XCTAssertNotNil(body["spec"])
    }

    func testUpdateWithOnlyTitleEncodesOnlyTitle() async throws {
        transport.seed("""
        {"task_id":"task_1","monitor_revision":2}
        """)
        _ = try await client.update("task_1", title: "Renamed")
        let body = bodyObject(try XCTUnwrap(transport.lastRequest))
        XCTAssertEqual(Set(body.keys), ["title"],
                       "a title-only PATCH must carry NO spec/schedule keys")
        XCTAssertEqual(body["title"] as? String, "Renamed")
    }

    // MARK: - convert (Phase 7)

    func testConvertPostsSpecAndTitleOnlyAndDecodes() async throws {
        transport.seed("""
        {"task_id":"task_1","monitor_revision":1,"converted":true}
        """)
        let response = try await client.convert("task_1", spec: try spec(),
                                                title: "Pricing monitor")
        XCTAssertEqual(response.taskID, "task_1")
        XCTAssertEqual(response.monitorRevision, 1)
        XCTAssertTrue(response.converted)
        let request = try XCTUnwrap(transport.lastRequest)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path, "/api/magician/v3/monitors/task_1/convert")
        assertScopeHeaders(request)
        let body = bodyObject(request)
        XCTAssertEqual(Set(body.keys), ["spec", "title"],
                       "the convert body is {spec, title?} — NEVER a schedule key")
    }

    func testConvertOmitsNilTitle() async throws {
        transport.seed("""
        {"task_id":"task_1","monitor_revision":1,"converted":true}
        """)
        _ = try await client.convert("task_1", spec: try spec())
        let body = bodyObject(try XCTUnwrap(transport.lastRequest))
        XCTAssertEqual(Set(body.keys), ["spec"],
                       "no title → the task keeps its own; nothing else rides along")
    }

    func testConvertConflictReasonsMapPrecisely() async throws {
        transport.seed("""
        {"error":"monitor_already_exists","task_id":"task_1"}
        """, status: 409)
        do {
            _ = try await client.convert("task_1", spec: try spec())
            XCTFail("expected conflict")
        } catch let error as Monitors.APIError {
            XCTAssertEqual(error, .conflict(reason: "monitor_already_exists"))
            XCTAssertEqual(error.userMessage, "This task is already a monitor.")
        }

        transport.seed("""
        {"error":"task_not_eligible_for_monitor","task_id":"task_2"}
        """, status: 409)
        do {
            _ = try await client.convert("task_2", spec: try spec())
            XCTFail("expected conflict")
        } catch let error as Monitors.APIError {
            XCTAssertEqual(error, .conflict(reason: "task_not_eligible_for_monitor"))
            XCTAssertEqual(error.userMessage, "This task can't be converted to a monitor.")
        }
    }

    func testConvertMissingTaskMapsNotFoundAndBadSpecMapsValidation() async throws {
        transport.seed("""
        {"error":"task_not_found","task_id":"task_missing"}
        """, status: 404)
        do {
            _ = try await client.convert("task_missing", spec: try spec())
            XCTFail("expected notFound")
        } catch let error as Monitors.APIError {
            XCTAssertEqual(error, .notFound)
        }

        transport.seed("""
        {"error":"monitor_sources_required"}
        """, status: 400)
        do {
            _ = try await client.convert("task_1", spec: try spec())
            XCTFail("expected validation")
        } catch let error as Monitors.APIError {
            XCTAssertEqual(error, .validation(reason: "monitor_sources_required"))
        }
    }

    // MARK: - path encoding

    func testPathSegmentPercentEncodesSpaceAndSlashExactlyOnce() async throws {
        transport.seed("""
        {"error":"monitor_not_found","task_id":"weird"}
        """, status: 404)
        _ = try? await client.detail("task 7/beta")
        let url = try XCTUnwrap(transport.lastRequest?.url)
        let components = try XCTUnwrap(
            URLComponents(url: url, resolvingAgainstBaseURL: false))
        // Exactly ONE round of encoding: a space is %20 (not %2520) and the
        // slash stays inside the segment as %2F (not a path separator).
        XCTAssertEqual(components.percentEncodedPath,
                       "/api/magician/v3/monitors/task%207%2Fbeta")
    }

    // MARK: - delete / pause / resume / run

    func testDeleteSendsRemoveFilesFlag() async throws {
        transport.seed("""
        {"ok":true,"task_id":"task_1","files_removed":false}
        """)
        let response = try await client.delete("task_1")
        XCTAssertTrue(response.ok)
        XCTAssertFalse(response.filesRemoved)
        let request = try XCTUnwrap(transport.lastRequest)
        XCTAssertEqual(request.httpMethod, "DELETE")
        XCTAssertEqual(request.url?.path, "/api/magician/v3/monitors/task_1")
        XCTAssertEqual(queryValue(request, "remove_files"), "false")
    }

    func testPauseAndResumeDecodeState() async throws {
        transport.seed("""
        {"task_id":"task_1","state":"paused"}
        """)
        let paused = try await client.pause("task_1")
        XCTAssertEqual(paused.state, "paused")
        XCTAssertEqual(transport.lastRequest?.url?.path,
                       "/api/magician/v3/monitors/task_1/pause")
        XCTAssertEqual(transport.lastRequest?.httpMethod, "POST")

        transport.seed("""
        {"task_id":"task_1","state":"active"}
        """)
        let resumed = try await client.resume("task_1")
        XCTAssertEqual(resumed.state, "active")
        XCTAssertEqual(transport.lastRequest?.url?.path,
                       "/api/magician/v3/monitors/task_1/resume")
    }

    func testPauseUnscheduledMapsConflict() async throws {
        transport.seed("""
        {"error":"monitor_unscheduled","task_id":"task_1"}
        """, status: 409)
        do {
            _ = try await client.pause("task_1")
            XCTFail("409 monitor_unscheduled must map to .unscheduled")
        } catch let error as Monitors.APIError {
            XCTAssertEqual(error, .unscheduled)
        }
    }

    func testRunNowAccepts202() async throws {
        transport.seed("""
        {"task":{},"execution":{}}
        """, status: 202)
        try await client.runNow("task_1")
        let request = try XCTUnwrap(transport.lastRequest)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path, "/api/magician/v3/monitors/task_1/run")
        assertScopeHeaders(request)
    }

    // MARK: - runs / updates / scope updates

    func testRunsDecodesSimpleEnvelope() async throws {
        let fixturesDir = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("magician/tests/fixtures/monitors", isDirectory: true)
        let run = try String(contentsOf: fixturesDir
            .appendingPathComponent("monitor_run_result_v1_changed.json"), encoding: .utf8)
        transport.seed("""
        {"items":[\(run)],"next_cursor":null,"limit":10}
        """)
        let page = try await client.runs("task_monitor_fixture_001", limit: 10)
        let request = try XCTUnwrap(transport.lastRequest)
        XCTAssertEqual(request.url?.path,
                       "/api/magician/v3/monitors/task_monitor_fixture_001/runs")
        XCTAssertEqual(queryValue(request, "limit"), "10")
        XCTAssertEqual(page.items.count, 1)
        XCTAssertEqual(page.items.first?.status, .changed)
    }

    func testUpdatesAndScopeUpdatesPaths() async throws {
        transport.seed("""
        {"items":[],"next_cursor":null,"limit":50}
        """)
        _ = try await client.updates("task_1")
        XCTAssertEqual(transport.lastRequest?.url?.path,
                       "/api/magician/v3/monitors/task_1/updates")

        transport.seed("""
        {"items":[],"next_cursor":null,"limit":50}
        """)
        _ = try await client.scopeUpdates(limit: 20)
        XCTAssertEqual(transport.lastRequest?.url?.path,
                       "/api/magician/v3/monitor-updates")
        XCTAssertEqual(queryValue(transport.lastRequest!, "limit"), "20")
    }

    // MARK: - feedback (Phase 6, plan §10)

    func testSubmitFeedbackPostsBodyAndDecodes() async throws {
        transport.seed("""
        {"task_id":"task_1","update_id":"mu_1","verdict":"useful",
         "recorded":true,"feedback_id":"mf_0001"}
        """)
        let response = try await client.submitFeedback(
            "task_1", updateID: "mu_1", verdict: .useful)
        XCTAssertEqual(response.verdict, .useful)
        XCTAssertTrue(response.recorded)
        XCTAssertEqual(response.feedbackID, "mf_0001")
        let request = try XCTUnwrap(transport.lastRequest)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path,
                       "/api/magician/v3/monitors/task_1/updates/mu_1/feedback")
        assertScopeHeaders(request)
        let body = bodyObject(request)
        XCTAssertEqual(body["verdict"] as? String, "useful")
        XCTAssertNil(body["note"], "an absent note stays off the wire")
    }

    func testSubmitFeedbackEncodesNoteAndSnakeCaseVerdict() async throws {
        transport.seed("""
        {"task_id":"task_1","update_id":"mu_1","verdict":"not_relevant",
         "recorded":true,"feedback_id":"mf_0002"}
        """)
        _ = try await client.submitFeedback(
            "task_1", updateID: "mu_1", verdict: .notRelevant, note: "wrong product line")
        let body = bodyObject(try XCTUnwrap(transport.lastRequest))
        XCTAssertEqual(body["verdict"] as? String, "not_relevant",
                       "the verdict rides the wire snake_case")
        XCTAssertEqual(body["note"] as? String, "wrong product line")
    }

    func testSubmitFeedbackIdempotentReplayDecodesRecordedFalse() async throws {
        transport.seed("""
        {"task_id":"task_1","update_id":"mu_1","verdict":"useful",
         "recorded":false,"feedback_id":"mf_0001"}
        """)
        let response = try await client.submitFeedback(
            "task_1", updateID: "mu_1", verdict: .useful)
        XCTAssertFalse(response.recorded,
                       "recorded:false = idempotent replay of the same verdict")
        XCTAssertEqual(response.verdict, .useful)
        XCTAssertEqual(response.feedbackID, "mf_0001",
                       "the replay still carries the stored feedback id")
    }

    func testSubmitFeedbackVerdictInvalidMapsValidation() async throws {
        transport.seed("""
        {"error":"monitor_feedback_verdict_invalid"}
        """, status: 400)
        do {
            _ = try await client.submitFeedback("task_1", updateID: "mu_1", verdict: .useful)
            XCTFail("400 must map to .validation")
        } catch let error as Monitors.APIError {
            XCTAssertEqual(error, .validation(reason: "monitor_feedback_verdict_invalid"))
        }
    }

    func testSubmitFeedbackUpdateNotFoundMapsNotFound() async throws {
        transport.seed("""
        {"error":"update_not_found"}
        """, status: 404)
        do {
            _ = try await client.submitFeedback("task_1", updateID: "mu_gone", verdict: .useful)
            XCTFail("404 update_not_found must map to .notFound")
        } catch let error as Monitors.APIError {
            XCTAssertEqual(error, .notFound)
        }
    }

    func testFeedbackListSendsLimitAndDecodesOptionalNote() async throws {
        transport.seed("""
        {"items":[
          {"feedback_id":"mf_1","update_id":"mu_1","verdict":"useful",
           "recorded_at":"2026-07-22T10:00:00Z"},
          {"feedback_id":"mf_2","update_id":"mu_2","verdict":"not_relevant",
           "note":"n","recorded_at":"2026-07-22T11:00:00Z"}
        ],"next_cursor":null,"limit":10}
        """)
        let page = try await client.feedback("task_1", limit: 10)
        let request = try XCTUnwrap(transport.lastRequest)
        XCTAssertEqual(request.httpMethod, "GET")
        XCTAssertEqual(request.url?.path, "/api/magician/v3/monitors/task_1/feedback")
        assertScopeHeaders(request)
        XCTAssertEqual(queryValue(request, "limit"), "10")
        XCTAssertEqual(page.items.count, 2)
        XCTAssertNil(page.items[0].note, "note is optional on the wire")
        XCTAssertEqual(page.items[1].note, "n")
        XCTAssertEqual(page.items[1].verdict, .notRelevant)
        XCTAssertNil(page.nextCursor)
    }

    // MARK: - error mapping edges

    func testUnknownErrorFallsBackToHTTP() async throws {
        transport.seed("""
        {"error":"weird_reason"}
        """, status: 500)
        do {
            _ = try await client.detail("task_1")
            XCTFail("500 must throw")
        } catch let error as Monitors.APIError {
            XCTAssertEqual(error, .http(status: 500, code: "weird_reason"))
        }
    }

    func testMissingScopeMapping() async throws {
        transport.seed("""
        {"error":"missing_scope"}
        """, status: 400)
        do {
            _ = try await client.list()
            XCTFail("400 missing_scope must map")
        } catch let error as Monitors.APIError {
            XCTAssertEqual(error, .missingScope)
        }
    }

    func testGarbledSuccessBodyMapsToDecoding() async throws {
        transport.seed("not json at all")
        do {
            _ = try await client.list()
            XCTFail("bad body must throw .decoding")
        } catch let error as Monitors.APIError {
            guard case .decoding = error else {
                return XCTFail("expected .decoding, got \(error)")
            }
        }
    }
}
