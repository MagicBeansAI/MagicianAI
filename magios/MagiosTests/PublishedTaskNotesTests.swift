import Foundation
import XCTest
@testable import Magician

@MainActor
final class PublishedTaskNotesTests: XCTestCase {
    private var sessions: [URLSession] = []

    override func setUp() {
        super.setUp()
        MockURLProtocol.handler = nil
        MagicianAccess.clearCredentials()
    }

    override func tearDown() {
        sessions.forEach { $0.invalidateAndCancel() }
        sessions.removeAll()
        MockURLProtocol.handler = nil
        MagicianAccess.clearCredentials()
        super.tearDown()
    }

    func testPublishedNotesUseScopedServerPagesWithPreviousAndNext() async {
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.httpMethod, "GET")
            XCTAssertEqual(request.url?.path, "/api/magician/v2/notes/published-tasks")
            XCTAssertNil(request.value(forHTTPHeaderField: "X-Principal"))
            XCTAssertNil(request.value(forHTTPHeaderField: "X-Workspace"))
            let query = Self.query(request)
            XCTAssertNil(query["principal"])
            XCTAssertNil(query["workspace"])
            let offset = Int(query["offset"] ?? "") ?? 0
            let limit = Int(query["limit"] ?? "") ?? 5
            return (response(for: request), Self.page(offset: offset, limit: limit, total: 12))
        }
        let model = makeModel()

        await model.loadIfNeeded()
        XCTAssertEqual(model.currentPage, 1)
        XCTAssertEqual(model.pageCount, 3)
        XCTAssertEqual(model.pageStart, 1)
        XCTAssertEqual(model.pageEnd, 5)
        XCTAssertTrue(model.canLoadNext)
        XCTAssertFalse(model.canLoadPrevious)

        await model.loadNextPage()
        XCTAssertEqual(model.currentPage, 2)
        XCTAssertEqual(model.pageStart, 6)
        XCTAssertEqual(model.pageEnd, 10)
        XCTAssertTrue(model.canLoadPrevious)

        await model.loadNextPage()
        XCTAssertEqual(model.currentPage, 3)
        XCTAssertEqual(model.pageStart, 11)
        XCTAssertEqual(model.pageEnd, 12)
        XCTAssertFalse(model.canLoadNext)

        await model.loadPreviousPage()
        XCTAssertEqual(model.currentPage, 2)
    }

    func testSearchAndPageSizeResetToTheFirstServerPage() async {
        var requests: [[String: String]] = []
        MockURLProtocol.handler = { request in
            let query = Self.query(request)
            requests.append(query)
            return (response(for: request), Self.page(offset: 0, limit: Int(query["limit"] ?? "") ?? 5, total: 1))
        }
        let model = makeModel()
        await model.loadIfNeeded()

        model.searchText = "  launch brief  "
        await model.submitSearch()
        XCTAssertEqual(requests.last?["q"], "launch brief")
        XCTAssertEqual(requests.last?["offset"], "0")
        XCTAssertEqual(model.currentPage, 1)

        await model.setPageSize(10)
        XCTAssertEqual(requests.last?["limit"], "10")
        XCTAssertEqual(requests.last?["q"], "launch brief")
        XCTAssertEqual(requests.last?["offset"], "0")
        XCTAssertEqual(model.pageSize, 10)

        await model.clearSearch()
        XCTAssertNil(requests.last?["q"])
        XCTAssertEqual(model.activeSearch, "")
    }

    func testShrunkenCorpusCorrectsAnOutOfRangePageWithOneBoundedRefetch() async {
        var shrunk = false
        var offsets: [Int] = []
        MockURLProtocol.handler = { request in
            let query = Self.query(request)
            let offset = Int(query["offset"] ?? "") ?? 0
            let limit = Int(query["limit"] ?? "") ?? 5
            offsets.append(offset)
            return (
                response(for: request),
                Self.page(offset: offset, limit: limit, total: shrunk ? 2 : 12)
            )
        }
        let model = makeModel()
        await model.loadIfNeeded()
        await model.loadNextPage()
        await model.loadNextPage()
        XCTAssertEqual(model.currentPage, 3)

        shrunk = true
        await model.reload()

        XCTAssertEqual(Array(offsets.suffix(2)), [10, 0])
        XCTAssertEqual(model.currentPage, 1)
        XCTAssertEqual(model.total, 2)
        XCTAssertEqual(model.items.count, 2)
    }

    func testPromotionAndBackfillSendScopedReviewGatedRequests() async throws {
        let note = try JSONDecoder().decode(PublishedTaskNote.self, from: jsonData(Self.note(index: 1)))
        var promotionBody: [String: Any]?
        var backfillBody: [String: Any]?
        MockURLProtocol.handler = { request in
            switch (request.httpMethod, request.url?.path) {
            case ("POST", "/api/magician/v2/notes/published-tasks/task-1/promote-memory"):
                promotionBody = try JSONSerialization.jsonObject(with: requestBody(request) ?? Data()) as? [String: Any]
                return (response(for: request), jsonData([
                    "candidate": ["id": "candidate-1", "state": "pending_review"]
                ]))
            case ("POST", "/api/magician/v2/notes/publish/tasks/backfill"):
                backfillBody = try JSONSerialization.jsonObject(with: requestBody(request) ?? Data()) as? [String: Any]
                return (response(for: request), jsonData([
                    "published": [],
                    "skipped_task_ids": [],
                    "errors": [],
                    "pagination": ["has_more": false]
                ]))
            case ("GET", "/api/magician/v2/notes/published-tasks"):
                return (response(for: request), Self.page(offset: 0, limit: 5, total: 0))
            default:
                XCTFail("Unexpected Published Notes request: \(request)")
                return (response(for: request, status: 500), Data())
            }
        }
        let model = makeModel()

        await model.promote(note)
        XCTAssertNil(promotionBody?["principal"])
        XCTAssertNil(promotionBody?["workspace"])
        XCTAssertTrue(model.successMessage?.contains("pending_review") == true)

        await model.backfillNextBatch()
        XCTAssertNil(backfillBody?["principal"])
        XCTAssertNil(backfillBody?["workspace"])
        XCTAssertEqual(backfillBody?["limit"] as? Int, 25)
        XCTAssertEqual(backfillBody?["only_unpublished"] as? Bool, true)
        XCTAssertEqual(model.successMessage, "Completed tasks are already published.")
    }

    func testManualTaskPublishUsesScopedEndpointAndServerOwnedDefaults() async throws {
        var body: [String: Any]?
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.httpMethod, "POST")
            XCTAssertEqual(request.url?.path, "/api/magician/v2/notes/publish/task/task-1")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")
            XCTAssertNil(request.value(forHTTPHeaderField: "X-Principal"))
            XCTAssertNil(request.value(forHTTPHeaderField: "X-Workspace"))
            body = try JSONSerialization.jsonObject(with: requestBody(request) ?? Data()) as? [String: Any]
            return (response(for: request), jsonData(Self.note(index: 1)))
        }
        let model = TaskNotePublishViewModel(client: makeClient())

        await model.publish(taskID: "task-1")

        XCTAssertEqual(model.publishedNote?.taskID, "task-1")
        XCTAssertEqual(model.successMessage, "Published to Notes.")
        XCTAssertNil(model.errorMessage)
        XCTAssertNil(body?["principal"])
        XCTAssertNil(body?["workspace"])
        XCTAssertNil(body?["mode"])
        XCTAssertNil(body?["include_assets"])
    }

    func testManualPublishFailureIsVisibleAndReleasesSingleFlightState() async {
        MockURLProtocol.handler = { request in
            XCTAssertEqual(request.url?.path, "/api/magician/v2/notes/publish/task/task-1")
            return (
                response(for: request, status: 422),
                jsonData(["message": "Only terminal tasks can be published"])
            )
        }
        let model = TaskNotePublishViewModel(client: makeClient())

        await model.publish(taskID: "task-1")

        XCTAssertNil(model.publishingTaskID)
        XCTAssertNil(model.publishedNote)
        XCTAssertNil(model.successMessage)
        XCTAssertEqual(model.errorMessage, "Only terminal tasks can be published")
    }

    func testReloadFailureKeepsTheLastGoodPageVisible() async {
        var shouldFail = false
        MockURLProtocol.handler = { request in
            if shouldFail {
                return (response(for: request, status: 503), jsonData(["message": "Notes index is unavailable"] ))
            }
            return (response(for: request), Self.page(offset: 0, limit: 5, total: 1))
        }
        let model = makeModel()
        await model.loadIfNeeded()
        XCTAssertEqual(model.items.count, 1)

        shouldFail = true
        await model.reload()

        XCTAssertEqual(model.items.count, 1)
        XCTAssertTrue(model.errorMessage?.contains("Notes index is unavailable") == true)
    }

    func testPublishedNoteOpeningRequiresCredentialFreeHTTPS() throws {
        var insecure = Self.note(index: 1)
        insecure["open_url"] = "http://127.0.0.1:3021/Tasks/task-1"
        let insecureNote = try JSONDecoder().decode(PublishedTaskNote.self, from: jsonData(insecure))
        XCTAssertNil(insecureNote.destinationURL)

        var credentialed = Self.note(index: 2)
        credentialed["open_url"] = "https://user:secret@notes.example.test/Tasks/task-2"
        let credentialedNote = try JSONDecoder().decode(PublishedTaskNote.self, from: jsonData(credentialed))
        XCTAssertNil(credentialedNote.destinationURL)

        let protectedNote = try JSONDecoder().decode(
            PublishedTaskNote.self,
            from: jsonData(Self.note(index: 3))
        )
        XCTAssertEqual(protectedNote.destinationURL?.host, "notes.example.test")
    }

    func testPromotionRejectsPathBreakingTaskIDBeforeNetwork() async {
        MockURLProtocol.handler = { request in
            XCTFail("Invalid task id must not reach the network: \(request)")
            return (response(for: request, status: 500), Data())
        }
        let session = makeMockSession()
        sessions.append(session)
        let client = PublishedTaskNotesClient(
            session: session,
            baseURL: URL(string: "https://ios.example.test")!,
            timeout: 2
        )
        do {
            _ = try await client.promote(taskID: "task/escape")
            XCTFail("Expected invalid task id rejection")
        } catch {
            XCTAssertEqual(error as? PublishedTaskNotesClientError, .invalidURL)
        }
    }

    func testManualPublishRejectsPathBreakingTaskIDBeforeNetwork() async {
        MockURLProtocol.handler = { request in
            XCTFail("Invalid task id must not reach the network: \(request)")
            return (response(for: request, status: 500), Data())
        }
        let client = makeClient()

        do {
            _ = try await client.publish(taskID: "task/escape")
            XCTFail("Expected invalid task id rejection")
        } catch {
            XCTAssertEqual(error as? PublishedTaskNotesClientError, .invalidURL)
        }
    }

    private func makeModel() -> PublishedTaskNotesViewModel {
        PublishedTaskNotesViewModel(client: makeClient())
    }

    private func makeClient() -> PublishedTaskNotesClient {
        let session = makeMockSession()
        sessions.append(session)
        return PublishedTaskNotesClient(
            session: session,
            baseURL: URL(string: "https://ios.example.test")!,
            timeout: 2
        )
    }

    nonisolated private static func query(_ request: URLRequest) -> [String: String] {
        Dictionary(uniqueKeysWithValues: (URLComponents(
            url: request.url!,
            resolvingAgainstBaseURL: false
        )?.queryItems ?? []).compactMap { item in
            item.value.map { (item.name, $0) }
        })
    }

    nonisolated private static func page(offset: Int, limit: Int, total: Int) -> Data {
        let count = max(0, min(limit, total - offset))
        return jsonData([
            "items": (0..<count).map { note(index: offset + $0) },
            "offset": offset,
            "limit": limit,
            "total": total,
            "has_more": offset + count < total
        ])
    }

    nonisolated private static func note(index: Int) -> [String: Any] {
        [
            "projection_id": "projection-\(index)",
            "task_id": "task-\(index)",
            "title": "Published task \(index)",
            "status": "completed",
            "agent_id": "researcher",
            "mode": "standard",
            "task_completed_at": "2026-08-03T10:00:00Z",
            "source_updated_at": "2026-08-03T10:00:00Z",
            "published_at": "2026-08-03T10:01:00Z",
            "tags": ["magician/task", "date/2026-08-03"],
            "note_path": "Tasks/2026-08-03/task-\(index).md",
            "open_url": "https://notes.example.test/Tasks/2026-08-03/task-\(index)",
            "assets": []
        ]
    }
}
