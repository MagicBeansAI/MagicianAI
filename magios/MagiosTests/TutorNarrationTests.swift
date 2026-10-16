import XCTest
import UIKit
@testable import Magician

@MainActor
final class TutorNarrationTests: XCTestCase {
    private final class FakeNarrator: TutorNarrating {
        var spoken: [String] = []
        var cancelCount = 0
        var result: TutorNarrationResult = .completed
        var startsPlayback = true

        func speak(text: String, stepId: String, onStart: @escaping () -> Void) async -> TutorNarrationResult {
            spoken.append(text)
            if startsPlayback { onStart() }
            return result
        }

        func cancel() { cancelCount += 1 }
    }

    func testNarratedPlaybackRevealsAtSpeechStartAndSerializesSteps() async throws {
        let narrator = FakeNarrator()
        var sleeps: [Double] = []
        let coordinator = TutorPlaybackCoordinator(
            narrator: narrator,
            sleepMilliseconds: { sleeps.append($0) }
        )
        let first = try decode(#"{"type":"rect","narration":"First instruction","duration_ms":200}"#)
        let second = try decode(#"{"type":"arrow","step_label":"Second instruction","wait_for_voice":true,"delay_ms":300,"duration_ms":100}"#)
        let items = [
            TutorRevealItem(shape: first, delayMs: 0, durationMs: 200),
            TutorRevealItem(shape: second, delayMs: 300, durationMs: 100),
        ]
        var revealed: [String] = []

        await coordinator.play(
            items: items,
            fallbackCaption: nil,
            fallbackNarration: nil,
            fallbackWaitForVoice: false
        ) { item, caption in
            revealed.append(item.shape.type)
            if item.shape.type == "arrow" { XCTAssertEqual(caption, "Second instruction") }
        }

        XCTAssertEqual(narrator.spoken, ["First instruction", "Second instruction"])
        XCTAssertEqual(revealed, ["rect", "arrow"])
        XCTAssertTrue(sleeps.contains { $0 >= 299 })
    }

    func testNarrationFailureFallsBackToVisibleDrawing() async throws {
        let narrator = FakeNarrator()
        narrator.startsPlayback = false
        narrator.result = .failed
        let coordinator = TutorPlaybackCoordinator(narrator: narrator, sleepMilliseconds: { _ in })
        let shape = try decode(#"{"type":"circle","narration":"Look here","animate":false}"#)
        var revealed = false

        await coordinator.play(
            items: [TutorRevealItem(shape: shape, delayMs: 0, durationMs: 0)],
            fallbackCaption: nil,
            fallbackNarration: nil,
            fallbackWaitForVoice: false
        ) { _, _ in revealed = true }

        XCTAssertTrue(revealed)
        XCTAssertEqual(narrator.spoken, ["Look here"])
    }

    func testTutorAudioFocusRemainsUntilEveryOwnerReleases() {
        let first = TutorAudioFocus.shared.acquire()
        let second = TutorAudioFocus.shared.acquire()
        XCTAssertTrue(TutorAudioFocus.shared.isActive)
        TutorAudioFocus.shared.release(first)
        XCTAssertTrue(TutorAudioFocus.shared.isActive)
        TutorAudioFocus.shared.release(second)
        XCTAssertFalse(TutorAudioFocus.shared.isActive)
    }

    func testRealtimeCompletionWaitsForNarrationAndReplaySpeaksAgain() async {
        let narrator = FakeNarrator()
        let sut = TutorOverlayViewModel(
            screenshot: testImage(),
            narrator: narrator,
            connectsRealtime: false,
            expirySeconds: 10,
            replayInterStepMilliseconds: 0,
            sleepMilliseconds: { _ in }
        )
        sut.handleRealtimeEvent(event("tutor.draw.shape", payload: [
            "shape_json": [
                "type": "rect", "x": 1, "y": 2, "w": 3, "h": 4,
                "narration": "Tap this control", "animate": false,
            ],
        ]))
        sut.handleRealtimeEvent(event("tutor.run.completed", payload: [:]))
        await drainTasks()

        XCTAssertEqual(sut.phase, .completed)
        XCTAssertEqual(sut.visibleShapes.count, 1)
        XCTAssertEqual(narrator.spoken, ["Tap this control"])

        sut.replay()
        await drainTasks()
        XCTAssertEqual(sut.phase, .completed)
        XCTAssertEqual(narrator.spoken, ["Tap this control", "Tap this control"])
    }

    func testKeepShowingSuppressesAutomaticExpiryAndAskAgainResets() async {
        let narrator = FakeNarrator()
        let sut = TutorOverlayViewModel(
            screenshot: testImage(),
            narrator: narrator,
            connectsRealtime: false,
            expirySeconds: 0.08,
            replayInterStepMilliseconds: 0,
            sleepMilliseconds: { _ in }
        )
        sut.handleRealtimeEvent(event("tutor.draw.shape", payload: [
            "shape_json": ["type": "rect", "animate": false],
        ]))
        sut.handleRealtimeEvent(event("tutor.run.completed", payload: [:]))
        await drainTasks()
        sut.keepShowing()
        try? await Task.sleep(nanoseconds: 150_000_000)

        XCTAssertFalse(sut.dismissalRequested)
        XCTAssertTrue(sut.isKeptShowing)
        sut.askAgain()
        XCTAssertEqual(sut.phase, .ready)
        XCTAssertFalse(sut.canReplay)
        XCTAssertFalse(sut.isKeptShowing)
    }

    func testCompletedGuideRequestsDismissalAfterExpiry() async {
        let sut = TutorOverlayViewModel(
            screenshot: testImage(),
            narrator: FakeNarrator(),
            connectsRealtime: false,
            expirySeconds: 0.03,
            replayInterStepMilliseconds: 0,
            sleepMilliseconds: { _ in }
        )
        sut.handleRealtimeEvent(event("tutor.draw.shape", payload: [
            "shape_json": ["type": "rect", "animate": false],
        ]))
        sut.handleRealtimeEvent(event("tutor.run.completed", payload: [:]))
        await drainTasks()
        try? await Task.sleep(nanoseconds: 80_000_000)

        XCTAssertTrue(sut.dismissalRequested)
    }

    func testRealtimeEventsFromAnotherTurnAreIgnored() async {
        let narrator = FakeNarrator()
        let sut = TutorOverlayViewModel(
            screenshot: testImage(),
            narrator: narrator,
            connectsRealtime: false,
            expirySeconds: 0,
            sleepMilliseconds: { _ in }
        )
        sut.handleRealtimeEvent(event(
            "tutor.draw.shape",
            payload: ["shape_json": ["type": "rect", "narration": "Wrong turn"]],
            chatTurnId: "different-turn"
        ))
        await drainTasks()

        XCTAssertFalse(sut.canReplay)
        XCTAssertTrue(narrator.spoken.isEmpty)
    }

    func testDismissCancelsWholeActiveTutorTurn() {
        let messageSeen = expectation(description: "Tutor message sent")
        let cancelSeen = expectation(description: "Tutor cancellation sent")
        let session = makeMockSession()
        MockURLProtocol.handler = { request in
            switch request.url!.path {
            case "/api/magician/v2/chat/new":
                return (response(for: request), jsonData(["session": ["id": "tutor-session"]]))
            case "/api/magician/v2/chat/sessions/tutor-session/attachments":
                XCTAssertTrue(request.value(forHTTPHeaderField: "Content-Type")?.contains("multipart/form-data") == true)
                return (response(for: request), jsonData(["attachment_id": "attachment-1"]))
            case "/api/magician/v2/chat/sessions/tutor-session/messages":
                let body = try JSONSerialization.jsonObject(with: requestBody(request)!) as! [String: Any]
                XCTAssertEqual(body["source_surface"] as? String, "ios_tutor_overlay")
                messageSeen.fulfill()
                return (response(for: request), Data("{}".utf8))
            case "/api/magician/v2/chat/sessions/tutor-session/run":
                XCTAssertEqual(request.httpMethod, "DELETE")
                XCTAssertFalse(request.url?.query?.contains("principal=") ?? false)
                XCTAssertFalse(request.url?.query?.contains("workspace=") ?? false)
                XCTAssertNil(request.httpBody)
                cancelSeen.fulfill()
                return (response(for: request), jsonData(["accepted": true, "cancelled": true]))
            default:
                XCTFail("Unexpected Tutor request: \(request)")
                return (response(for: request, status: 404), Data())
            }
        }
        let sut = TutorOverlayViewModel(
            screenshot: testImage(),
            networkSession: session,
            narrator: FakeNarrator(),
            connectsRealtime: false,
            expirySeconds: 0,
            sleepMilliseconds: { _ in }
        )

        sut.start(question: "Where should I tap?")
        wait(for: [messageSeen], timeout: 2)
        sut.dismissTutor()
        wait(for: [cancelSeen], timeout: 2)
        session.invalidateAndCancel()
    }

    private func decode(_ json: String) throws -> TutorShape {
        try JSONDecoder().decode(TutorShape.self, from: Data(json.utf8))
    }

    private func event(_ type: String, payload: [String: Any], chatTurnId: String = "") -> String {
        var scopedPayload = payload
        scopedPayload["chat_turn_id"] = chatTurnId
        let object: [String: Any] = [
            "event_type": "AgentEvent",
            "data": ["event": ["event_type": type, "payload": scopedPayload]],
        ]
        return String(data: jsonData(object), encoding: .utf8)!
    }

    private func testImage() -> UIImage {
        UIGraphicsImageRenderer(size: CGSize(width: 4, height: 4)).image { context in
            UIColor.black.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 4, height: 4))
        }
    }

    private func drainTasks() async {
        for _ in 0..<8 { await Task.yield() }
    }
}
