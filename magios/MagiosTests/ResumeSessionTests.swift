import XCTest
@testable import Magician

/// The pure launch-resume picker used only when an older installation has no
/// remembered device selection. The newest active Personal session wins even
/// when it lives outside #general.
final class ResumeSessionTests: XCTestCase {
    private func session(_ id: String, thread: String, status: String = "active", updated: Int) -> ChatSession {
        ChatSession(id: id, principal: "anonymous", workspace: "default", agentId: "a",
                    uiThreadId: thread, title: nil, status: status, createdAt: 0, updatedAt: updated)
    }

    func testPicksMostRecentActiveAcrossThreads() {
        let sessions = [
            session("a", thread: "other", updated: 100),
            session("b", thread: "general", updated: 50),
            session("c", thread: "general", updated: 80),
        ]
        XCTAssertEqual(ThreadViewModel.pickResumeSessionId(from: sessions), "a")
    }

    func testFallsBackToMostRecentActiveOverallWhenNoGeneral() {
        let sessions = [
            session("a", thread: "other", updated: 100),
            session("b", thread: "work", updated: 90),
        ]
        XCTAssertEqual(ThreadViewModel.pickResumeSessionId(from: sessions), "a")
    }

    func testIgnoresArchivedSessions() {
        let sessions = [
            session("a", thread: "general", status: "archived", updated: 100),
            session("b", thread: "general", status: "active", updated: 40),
        ]
        XCTAssertEqual(ThreadViewModel.pickResumeSessionId(from: sessions), "b")
    }

    func testEmptyOrAllArchivedReturnsNil() {
        XCTAssertNil(ThreadViewModel.pickResumeSessionId(from: []))
        XCTAssertNil(ThreadViewModel.pickResumeSessionId(
            from: [session("a", thread: "general", status: "archived", updated: 1)]))
    }

    func testDeviceSelectionStoreRetainsOnlyTheMatchingSession() throws {
        let suite = "ResumeSessionTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = ChatSessionSelectionStore(defaults: defaults)

        XCTAssertNil(store.rememberedSessionId())
        store.remember(" session-one ")
        XCTAssertEqual(store.rememberedSessionId(), "session-one")

        store.forget("another-session")
        XCTAssertEqual(store.rememberedSessionId(), "session-one")
        store.forget("session-one")
        XCTAssertNil(store.rememberedSessionId())
    }
}
