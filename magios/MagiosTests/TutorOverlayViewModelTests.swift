import XCTest
import UIKit
@testable import Magician

/// Verifies the source-free (blackboard) vs screen_overlay send paths: blackboard must
/// issue NO attachment upload and OMIT `attachment_ids`; screen_overlay must upload
/// first and include the returned id. Both keep the tutor text + source_surface.
@MainActor
final class TutorOverlayViewModelTests: XCTestCase {
    private final class Capture {
        var messageBody: [String: Any]?
        var messageBodies: [[String: Any]] = []
        var sawAttachmentsRequest = false
    }

    override func tearDown() {
        MockURLProtocol.handler = nil
        super.tearDown()
    }

    private func solidImage() -> UIImage {
        let rect = CGRect(x: 0, y: 0, width: 4, height: 4)
        UIGraphicsBeginImageContext(rect.size)
        UIColor.blue.setFill(); UIRectFill(rect)
        let image = UIGraphicsGetImageFromCurrentImageContext()!
        UIGraphicsEndImageContext()
        return image
    }

    private func runStart(screenshot: UIImage?, canvasMode: TutorCanvasMode) -> Capture {
        let capture = Capture()
        let sent = expectation(description: "messages POST issued")
        MockURLProtocol.handler = { req in
            let path = req.url?.path ?? ""
            if path.hasSuffix("/chat/new") {
                return (response(for: req), jsonData(["session": ["id": "s1"]]))
            }
            if path.hasSuffix("/attachments") {
                capture.sawAttachmentsRequest = true
                return (response(for: req), jsonData(["attachment_id": "att1"]))
            }
            if path.hasSuffix("/messages") {
                if let body = requestBody(req) {
                    capture.messageBody = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
                    if let messageBody = capture.messageBody {
                        capture.messageBodies.append(messageBody)
                    }
                }
                sent.fulfill()
                return (response(for: req), jsonData([:]))
            }
            return (response(for: req), jsonData([:]))
        }
        let vm = TutorOverlayViewModel(
            screenshot: screenshot, canvasMode: canvasMode,
            networkSession: makeMockSession(), connectsRealtime: false)
        vm.start(question: "explain recursion")
        wait(for: [sent], timeout: 4)
        return capture
    }

    func testBlackboardOmitsAttachmentAndSkipsUpload() {
        let capture = runStart(screenshot: nil, canvasMode: .blackboard)
        XCTAssertEqual(capture.messageBody?["text"] as? String, "@tutor explain recursion")
        XCTAssertEqual(capture.messageBody?["source_surface"] as? String, "ios_tutor_overlay")
        XCTAssertNil(capture.messageBody?["attachment_ids"], "blackboard must not send attachment_ids")
        XCTAssertFalse(capture.sawAttachmentsRequest, "blackboard must not upload")
    }

    func testScreenOverlayUploadsAndIncludesAttachment() {
        let capture = runStart(screenshot: solidImage(), canvasMode: .screenOverlay)
        XCTAssertEqual(capture.messageBody?["text"] as? String, "@tutor explain recursion")
        XCTAssertEqual(capture.messageBody?["attachment_ids"] as? [String], ["att1"])
        XCTAssertTrue(capture.sawAttachmentsRequest, "screen_overlay must upload the image")
    }

    func testExplainDeeperSendsOneTutorPrefixAndStartsAFreshCorrelatedTurn() async throws {
        let capture = Capture()
        let initialSent = expectation(description: "initial Tutor message sent")
        let deeperSent = expectation(description: "Explain Deeper message sent")
        MockURLProtocol.handler = { req in
            let path = req.url?.path ?? ""
            if path.hasSuffix("/chat/new") {
                return (response(for: req), jsonData(["session": ["id": "s1"]]))
            }
            if path.hasSuffix("/messages") {
                let body = try XCTUnwrap(requestBody(req))
                let object = try XCTUnwrap(
                    try JSONSerialization.jsonObject(with: body) as? [String: Any]
                )
                capture.messageBodies.append(object)
                if capture.messageBodies.count == 1 {
                    initialSent.fulfill()
                } else if capture.messageBodies.count == 2 {
                    deeperSent.fulfill()
                }
                return (response(for: req), jsonData([:]))
            }
            return (response(for: req), jsonData([:]))
        }

        let session = makeMockSession()
        let viewModel = TutorOverlayViewModel(
            screenshot: nil,
            canvasMode: .blackboard,
            networkSession: session,
            connectsRealtime: false,
            expirySeconds: 0,
            replayInterStepMilliseconds: 60_000,
            sleepMilliseconds: { _ in }
        )
        viewModel.start(question: "explain recursion")
        await fulfillment(of: [initialSent], timeout: 4)

        let initialBody = try XCTUnwrap(capture.messageBodies.first)
        let initialTurnId = try XCTUnwrap(initialBody["chat_turn_id"] as? String)
        viewModel.handleRealtimeEvent(event(
            "tutor.draw.shape",
            payload: [
                "shape_json": [
                    "type": "rect",
                    "step_label": "Base case",
                    "narration": "The base case stops the recursion.",
                    "animate": false,
                ],
            ],
            chatTurnId: initialTurnId
        ))
        XCTAssertFalse(
            viewModel.canExplainDeeper,
            "the correction channel must stay closed while the new shape is queued"
        )
        await drainTasks()
        XCTAssertEqual(viewModel.phase, .teaching)
        XCTAssertTrue(
            viewModel.canExplainDeeper,
            "a fully settled live step should expose the correction channel"
        )
        viewModel.handleRealtimeEvent(event(
            "tutor.draw.shape",
            payload: [
                "shape_json": [
                    "type": "arrow",
                    "step_label": "Recursive case",
                    "narration": "The recursive call reduces the input.",
                    "animate": false,
                ],
            ],
            chatTurnId: initialTurnId
        ))
        XCTAssertFalse(viewModel.canExplainDeeper)
        await drainTasks()
        XCTAssertTrue(viewModel.canExplainDeeper)
        viewModel.handleRealtimeEvent(event(
            "tutor.run.completed", payload: [:], chatTurnId: initialTurnId
        ))
        await drainTasks()
        XCTAssertTrue(viewModel.canExplainDeeper)

        viewModel.replay()
        await drainTasks()
        XCTAssertEqual(viewModel.phase, .teaching)
        XCTAssertTrue(
            viewModel.canExplainDeeper,
            "a settled replay step should expose the same correction channel"
        )

        viewModel.explainDeeper()
        await fulfillment(of: [deeperSent], timeout: 4)

        let deeperBody = capture.messageBodies[1]
        let deeperText = try XCTUnwrap(deeperBody["text"] as? String)
        let deeperTurnId = try XCTUnwrap(deeperBody["chat_turn_id"] as? String)
        XCTAssertTrue(deeperText.hasPrefix("@tutor go deeper"))
        XCTAssertEqual(deeperText.components(separatedBy: "@tutor").count - 1, 1)
        XCTAssertNotEqual(deeperTurnId, initialTurnId)

        viewModel.handleRealtimeEvent(event(
            "tutor.draw.shape",
            payload: ["shape_json": ["type": "rect", "step_label": "Stale step"]],
            chatTurnId: initialTurnId
        ))
        await drainTasks()
        XCTAssertEqual(viewModel.stepLabel, "Base case")

        viewModel.handleRealtimeEvent(event(
            "tutor.draw.shape",
            payload: ["shape_json": ["type": "rect", "step_label": "Deeper step"]],
            chatTurnId: deeperTurnId
        ))
        await drainTasks()
        XCTAssertEqual(viewModel.stepLabel, "Deeper step")
        session.invalidateAndCancel()
    }

    private func event(
        _ type: String,
        payload: [String: Any],
        chatTurnId: String
    ) -> String {
        var scopedPayload = payload
        scopedPayload["chat_turn_id"] = chatTurnId
        let object: [String: Any] = [
            "event_type": "AgentEvent",
            "data": ["event": ["event_type": type, "payload": scopedPayload]],
        ]
        return String(data: jsonData(object), encoding: .utf8)!
    }

    private func drainTasks() async {
        for _ in 0..<8 { await Task.yield() }
    }
}
