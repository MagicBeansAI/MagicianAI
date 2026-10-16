import XCTest
@testable import Magician

/// The coding-runs Live Activity is moved by server pushes whose
/// `content-state` keys are the Swift property names; renaming one silently
/// breaks lock-screen updates, so the wire keys are pinned here.
final class VibeDevPushTests: XCTestCase {

    @MainActor
    func testRetireWaitsForQueuedRouteWorkOnlySoLong() async {
        // Route work still retrying: the retire stops waiting at its limit,
        // so the DELETEs still go out.
        let stuck = Task<Void, Never> { try? await Task.sleep(for: .seconds(30)) }
        let started = ContinuousClock.now
        await CodingRunsActivity.wait(for: stuck, atMost: .milliseconds(100))
        XCTAssertLessThan(ContinuousClock.now - started, .seconds(5))
        stuck.cancel()

        // Work that ends first ends the wait.
        let quick = Task<Void, Never> {}
        let quickStart = ContinuousClock.now
        await CodingRunsActivity.wait(for: quick, atMost: .seconds(30))
        XCTAssertLessThan(ContinuousClock.now - quickStart, .seconds(5))
    }

    func testServerContentStateDecodes() throws {
        let json = """
        {"working": 2, "needsYou": 1, "done": 3, "failed": 0,
         "headline": "API lane · Shop", "detail": "Needs you"}
        """
        let state = try JSONDecoder().decode(CodingRunsActivityAttributes.ContentState.self, from: Data(json.utf8))
        XCTAssertEqual(state, CodingRunsActivityAttributes.ContentState(
            working: 2, needsYou: 1, done: 3, failed: 0, headline: "API lane · Shop", detail: "Needs you"))
        XCTAssertTrue(state.isActive)
    }

    func testContentStateEncodesTheKeysTheServerSends() throws {
        let state = CodingRunsActivityAttributes.ContentState(working: 1, needsYou: 0)
        let object = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: JSONEncoder().encode(state)) as? [String: Any])
        XCTAssertEqual(Set(object.keys), ["working", "needsYou", "done", "failed", "headline", "detail"])
    }

    func testAttributesCarryTheTitle() throws {
        let attributes = try JSONDecoder().decode(CodingRunsActivityAttributes.self,
                                                  from: Data(#"{"title":"Coding runs"}"#.utf8))
        XCTAssertEqual(attributes.title, "Coding runs")
    }

    func testCodingRunsRegistrationKinds() {
        XCTAssertEqual(MobilePushRegistrationKind.codingRuns.rawValue, "coding_runs")
        XCTAssertEqual(MobilePushRegistrationKind.codingRunsStart.rawValue, "coding_runs_start")
    }

    // MARK: What a poll may do

    func testAStalePollCannotEndAnActivityTrackedSinceItWasSent() {
        // Sent at 5; the server push-started an activity, adopted at 6, while
        // it was out: "nothing running" leaves it for the next poll.
        XCTAssertFalse(CodingRunsPoll.mayEnd(trackedAt: 6, sentAt: 5))
        // Tracked before the poll was sent: the answer is newer, it ends.
        XCTAssertTrue(CodingRunsPoll.mayEnd(trackedAt: 4, sentAt: 5))
        // Nothing tracked: nothing to keep.
        XCTAssertTrue(CodingRunsPoll.mayEnd(trackedAt: nil, sentAt: 5))
    }

    func testAnOlderPollAnswerIsDropped() {
        XCTAssertTrue(CodingRunsPoll.isCurrent(sentAt: 7, newestApplied: 5))
        XCTAssertFalse(CodingRunsPoll.isCurrent(sentAt: 5, newestApplied: 7))
        XCTAssertFalse(CodingRunsPoll.isCurrent(sentAt: 7, newestApplied: 7))
        XCTAssertTrue(CodingRunsPoll.isCurrent(sentAt: 1, newestApplied: 0), "the first answer applies")
    }

    // MARK: Which activity is kept

    func testTheNewestRunningActivityIsKept() {
        // A server-started newcomer beats the tracked one, wherever iOS lists it.
        XCTAssertEqual(CodingRunsAdoption.survivor(tracked: "a", running: ["a", "b"], retired: []), "b")
        XCTAssertEqual(CodingRunsAdoption.survivor(tracked: "a", running: ["b", "a"], retired: []), "b")
        XCTAssertEqual(CodingRunsAdoption.survivor(tracked: "a", running: ["a"], retired: []), "a")
        // At launch nothing is tracked: the last listed.
        XCTAssertEqual(CodingRunsAdoption.survivor(tracked: nil, running: ["a", "b"], retired: []), "b")
        XCTAssertNil(CodingRunsAdoption.survivor(tracked: nil, running: [], retired: []))
    }

    func testARetiredActivityIsNeverKept() {
        // Retired here but still ending: the tracked one stays, and the
        // retiree is not taken for a newcomer.
        XCTAssertEqual(CodingRunsAdoption.survivor(tracked: "b", running: ["a", "b"], retired: ["a"]), "b")
        // The one being ended is not adopted back once nothing is tracked.
        XCTAssertNil(CodingRunsAdoption.survivor(tracked: nil, running: ["a"], retired: ["a"]))
        XCTAssertEqual(CodingRunsAdoption.survivor(tracked: nil, running: ["a", "c"], retired: ["a"]), "c")
    }

    func testTheRuleAgreesWithItselfAfterAnAdoption() {
        // Once "b" won over "a" (and "a" was retired), asking again with
        // "b" tracked keeps "b": the two activities never both end.
        let first = CodingRunsAdoption.survivor(tracked: "a", running: ["a", "b"], retired: [])
        XCTAssertEqual(first, "b")
        XCTAssertEqual(CodingRunsAdoption.survivor(tracked: first, running: ["a", "b"], retired: ["a"]), "b")
    }

    // MARK: Magician at work

    func testSummaryContentStateDecodesTheNewFields() throws {
        let json = """
        {"working": 2, "needsYou": 1, "done": 0, "failed": 0,
         "headline": "Add clearCompleted · Shop", "detail": "Needs you",
         "featuredKind": "coding_run", "featuredState": "needs_you",
         "featuredTitle": "Add clearCompleted", "featuredContext": "Shop",
         "featuredStep": "Waiting for review", "actionKind": "approve_diff", "actionId": "ap_1",
         "deepLink": "magican://vibe?project=p&run=r", "more": 3,
         "next": [{"id": "crew:c1", "kind": "crew", "state": "working", "title": "Split the API"}]}
        """
        let state = try JSONDecoder().decode(CodingRunsActivityAttributes.ContentState.self, from: Data(json.utf8))
        XCTAssertTrue(state.hasFeatured)
        XCTAssertEqual(state.featuredKind, "coding_run")
        XCTAssertEqual(state.actionKind, "approve_diff")
        XCTAssertEqual(state.actionId, "ap_1")
        XCTAssertEqual(state.more, 3)
        XCTAssertEqual(state.next, [CodingRunsActivityAttributes.ContentState.NextItem(id: "crew:c1", kind: "crew", state: "working", title: "Split the API")])
        XCTAssertEqual(state.relevanceScore, 80)
    }

    func testAnOlderPayloadHasNoFeaturedItem() throws {
        let json = #"{"working": 1, "needsYou": 0, "done": 0, "failed": 0, "headline": "API lane", "detail": "Tests"}"#
        let state = try JSONDecoder().decode(CodingRunsActivityAttributes.ContentState.self, from: Data(json.utf8))
        XCTAssertFalse(state.hasFeatured)
        XCTAssertNil(state.next)
        XCTAssertEqual(state.relevanceScore, 40)
    }

    func testWorkLiveBuildsTheSummary() throws {
        let json = """
        {"featured": {"id": "run:r1", "kind": "coding_run", "title": "Add clearCompleted",
                      "context": "Shop", "state": "needs_you", "step": null,
                      "action": {"kind": "approve_diff", "id": "ap_1"},
                      "deep_link": "magican://vibe?project=p&run=r1"},
         "next": [{"id": "crew:c1", "kind": "crew", "title": "Split the API", "context": "Shop", "state": "working"},
                  {"id": "task:t1", "kind": "work_task", "title": "Weekly report", "context": "", "state": "working"}],
         "counts": {"needs_you": 1, "working": 4, "failed": 0, "done": 0},
         "items": [], "followed": ["crew:c1"], "generated_at": "2026-10-15T07:00:00Z"}
        """
        let live = try XCTUnwrap(WorkLiveSummary.parse(Data(json.utf8)))
        XCTAssertEqual(live.followed, ["crew:c1"])
        let state = WorkLiveSummary.state(for: live)
        XCTAssertEqual(state.working, 4)
        XCTAssertEqual(state.needsYou, 1)
        XCTAssertEqual(state.headline, "Add clearCompleted · Shop")
        XCTAssertEqual(state.detail, "Needs you")
        XCTAssertEqual(state.actionId, "ap_1")
        XCTAssertEqual(state.next?.map(\.id), ["crew:c1", "task:t1"])
        // Five counted, three shown.
        XCTAssertEqual(state.more, 2)
    }

    func testABodyWithoutCountsIsNotAWorkSummary() {
        // An older server's fallback answer: the poll then reads the old route.
        XCTAssertNil(WorkLiveSummary.parse(Data(#"{"runs": []}"#.utf8)))
    }

    func testFollowedItemContentStateDecodes() throws {
        let json = """
        {"state": "working", "title": "Split the API", "context": "Shop", "step": "Lane 2 of 3",
         "spentUsd": 0.04, "deepLink": "magican://vibe?crew=c1", "updatedAt": 1791961200}
        """
        let state = try JSONDecoder().decode(FollowedItemActivityAttributes.ContentState.self, from: Data(json.utf8))
        XCTAssertEqual(state.title, "Split the API")
        XCTAssertEqual(state.spentUsd, 0.04)
        XCTAssertEqual(state.updatedAt, 1791961200)
        XCTAssertNil(state.actionKind)
        XCTAssertFalse(state.isEnded)
        let attributes = try JSONDecoder().decode(FollowedItemActivityAttributes.self,
                                                  from: Data(#"{"itemId":"crew:c1","kind":"crew"}"#.utf8))
        XCTAssertEqual(attributes.itemId, "crew:c1")
        XCTAssertEqual(MobilePushRegistrationKind.workItem.rawValue, "work_item")
    }

    func testAWorkTaskLinkOpensTheTask() {
        let target = WorkNavigationTarget.parse("magican://work?task=task_1")
        XCTAssertEqual(target?.kind, .task)
        XCTAssertEqual(target?.recordID, "task_1")
    }
}
