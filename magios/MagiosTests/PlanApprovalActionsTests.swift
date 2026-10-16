import XCTest
@testable import Magician

/// Plan approval: the four choices and Approve & Run's two-call order (web
/// `promptFor.test.ts` "posts Approve & Run as an approval (the run starts
/// after it lands)" and `respondToHitl.ts` `startRunAfterPlanApproval`).
final class PlanApprovalActionsTests: XCTestCase {
    /// Records every call in order; each answer is scripted.
    private final class StubClient: PlanApprovalClient {
        var respondResult: PlanApprovalPost = .ok
        var startError: Error?
        private(set) var calls: [String] = []
        private(set) var postedConfirmed: [Bool] = []

        func respond(_ request: PlanApprovalRequest, confirmed: Bool) async -> PlanApprovalPost {
            calls.append("respond:\(request.correlationId)")
            postedConfirmed.append(confirmed)
            return respondResult
        }

        func startRun(taskId: String) async throws {
            calls.append("execute:\(taskId)")
            if let startError { throw startError }
        }
    }

    private let request = PlanApprovalRequest(correlationId: "plan_1", taskId: "task_42")

    // MARK: - Choices

    func testOffersReviewRejectApproveAndApproveAndRunInThatOrder() {
        XCTAssertEqual(PlanApprovalActions.choices(hasTask: true), [.review, .reject, .approve, .approveAndRun])
        XCTAssertEqual(
            PlanApprovalActions.choices(hasTask: true).map { $0.label() },
            ["Review plan", "Reject", "Approve", "Approve & Run"]
        )
    }

    func testDropsReviewWhenThereIsNoTaskToOpen() {
        XCTAssertEqual(PlanApprovalActions.choices(hasTask: false), [.reject, .approve, .approveAndRun])
    }

    func testRejectAndApproveTakeTheBackendLabelsButApproveAndRunKeepsItsOwn() {
        XCTAssertEqual(PlanApprovalChoice.reject.label(denyLabel: "Discard"), "Discard")
        XCTAssertEqual(PlanApprovalChoice.approve.label(confirmLabel: "Accept"), "Accept")
        XCTAssertEqual(PlanApprovalChoice.approve.label(confirmLabel: "  "), "Approve")
        XCTAssertEqual(PlanApprovalChoice.approveAndRun.label(confirmLabel: "Accept"), "Approve & Run")
    }

    func testOnlyAPlanApprovalConfirmationIsAPlanReview() {
        XCTAssertTrue(PlanApprovalActions.isPlanApproval(source: "plan_approval", inputType: "confirmation"))
        XCTAssertFalse(PlanApprovalActions.isPlanApproval(source: "plan_approval", inputType: "text"))
        XCTAssertFalse(PlanApprovalActions.isPlanApproval(source: "agentic", inputType: "confirmation"))
    }

    func testApproveAndRunPostsTheSameConfirmationAsApprove() {
        XCTAssertEqual(PlanApprovalChoice.approveAndRun.confirmed, true)
        XCTAssertEqual(PlanApprovalChoice.approve.confirmed, true)
        XCTAssertEqual(PlanApprovalChoice.reject.confirmed, false)
        XCTAssertNil(PlanApprovalChoice.review.confirmed)
    }

    func testTheRespondBodyNamesTheTaskAndTheConfirmation() {
        var withRun = request
        withRun.executionId = "planexec_1"
        let body = PlanApprovalActions.respondPayload(withRun, confirmed: true)
        XCTAssertEqual(body["source"] as? String, "plan_approval")
        XCTAssertEqual(body["task_id"] as? String, "task_42")
        XCTAssertEqual(body["execution_id"] as? String, "planexec_1")
        XCTAssertEqual(body["correlation_id"] as? String, "plan_1")
        let value = body["value"] as? [String: Any]
        XCTAssertEqual(value?["type"] as? String, "confirmation")
        XCTAssertEqual(value?["confirmed"] as? Bool, true)
    }

    // MARK: - Two-call sequence

    func testApproveAndRunApprovesFirstThenStartsTheRun() async {
        let client = StubClient()
        let outcome = await PlanApprovalActions.approveAndRun(request, client: client)
        XCTAssertEqual(outcome, .approvedAndStarted)
        XCTAssertEqual(client.calls, ["respond:plan_1", "execute:task_42"])
        XCTAssertEqual(client.postedConfirmed, [true])
    }

    func testAFailedStartKeepsTheApprovalAndSaysTheRunDidNotStart() async {
        let client = StubClient()
        client.startError = NSError(domain: "t", code: 409, userInfo: [NSLocalizedDescriptionKey: "Busy."])
        let outcome = await PlanApprovalActions.approveAndRun(request, client: client)
        XCTAssertEqual(outcome, .approvedRunNotStarted("Busy."))
        XCTAssertTrue(outcome.isApproved)
        XCTAssertTrue(outcome.message.hasPrefix("Approved — the run did not start"))
        XCTAssertEqual(client.calls, ["respond:plan_1", "execute:task_42"])
    }

    func testAFailedApprovalNeverStartsARun() async {
        let client = StubClient()
        client.respondResult = .failed("Gone.")
        let outcome = await PlanApprovalActions.approveAndRun(request, client: client)
        XCTAssertEqual(outcome, .failed("Gone."))
        XCTAssertFalse(outcome.isApproved)
        XCTAssertEqual(client.calls, ["respond:plan_1"])
    }

    func testApproveAndRunWithoutATaskApprovesAndSaysTheRunDidNotStart() async {
        let client = StubClient()
        let outcome = await PlanApprovalActions.approveAndRun(
            PlanApprovalRequest(correlationId: "plan_1", taskId: nil), client: client)
        guard case .approvedRunNotStarted = outcome else { return XCTFail("\(outcome)") }
        XCTAssertEqual(client.calls, ["respond:plan_1"])
    }

    func testApproveAloneStartsNothingAndSaysToRunItWhenReady() async {
        let client = StubClient()
        let outcome = await PlanApprovalActions.approve(request, client: client)
        XCTAssertEqual(outcome, .approved)
        XCTAssertEqual(client.calls, ["respond:plan_1"])
        XCTAssertTrue(outcome.message.contains("Run it when you're ready"))
    }

    func testRejectPostsFalseAndReviewPostsNothing() async {
        let client = StubClient()
        let rejected = await PlanApprovalActions.perform(.reject, request, client: client)
        XCTAssertEqual(rejected, .rejected)
        XCTAssertEqual(client.postedConfirmed, [false])
        let reviewed = await PlanApprovalActions.perform(.review, request, client: client)
        XCTAssertNil(reviewed)
        XCTAssertEqual(client.calls.count, 1)
    }
}
