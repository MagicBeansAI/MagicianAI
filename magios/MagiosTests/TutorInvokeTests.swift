import XCTest
import UIKit
@testable import Magician

/// Mirrors the web `isTutorInvokeText` rule (ChatPanel.svelte) so composer @tutor
/// detection stays at parity. Spoken commands use the broader deterministic
/// grammar, while keyboard `@copilot` remains outside this native lane.
final class TutorInvokeTests: XCTestCase {
    func testIsTutorInvokePositives() {
        for t in ["@tutor explain recursion", "@Tutor x", "hey tutor teach me",
                  "hey, tutur x", "@tutur typo tolerant", "@tutor: explain recursion"] {
            XCTAssertTrue(TutorInvoke.isTutorInvoke(t), "should match: \(t)")
        }
    }

    func testIsTutorInvokeNegatives() {
        for t in ["tutor x", "@task do it", "@copilot open mail", "just plain text",
                  "email@tutor.com", "@tutor@example.com sent this", "hey tutor@example.com wrote",
                  "@tutoring session", "please @tutor help"] {
            XCTAssertFalse(TutorInvoke.isTutorInvoke(t), "should NOT match: \(t)")
        }
    }

    func testStripRemovesLeadingInvokeToken() {
        XCTAssertEqual(TutorInvoke.strip("@tutor explain recursion"), "explain recursion")
        XCTAssertEqual(TutorInvoke.strip("hey tutor, explain recursion"), "explain recursion")
        XCTAssertEqual(TutorInvoke.strip("@tutor: explain recursion"), "explain recursion")
        XCTAssertEqual(TutorInvoke.strip("  @Tutor   big O  "), "big O")
        // No leading token → returned trimmed, unchanged.
        XCTAssertEqual(TutorInvoke.strip("explain recursion"), "explain recursion")
    }

    func testModeFromImagePresence() {
        XCTAssertEqual(TutorInvoke.mode(hasImage: true), .screenOverlay)
        XCTAssertEqual(TutorInvoke.mode(hasImage: false), .blackboard)
    }

    func testVoiceGrammarNormalizesTutorAndTutorQuickBlackboard() {
        XCTAssertEqual(
            TutorInvoke.parseVoiceGuidedFlow("Tutor explain recursion"),
            .init(
                feature: .tutor,
                canvasMode: .blackboard,
                quick: false,
                normalizedText: "@tutor explain recursion"
            )
        )
        XCTAssertEqual(
            TutorInvoke.parseVoiceGuidedFlow("Start Tutor Quick blackboard explain recursion"),
            .init(
                feature: .tutor,
                canvasMode: .blackboard,
                quick: true,
                normalizedText: "@tutor #quick blackboard explain recursion"
            )
        )
    }

    func testVoiceGrammarClassifiesUnsupportedScreenAndAppCopilotRequests() {
        XCTAssertEqual(
            TutorInvoke.parseVoiceGuidedFlow("Tutor screen explain this graph")?.canvasMode,
            .screenOverlay
        )
        let copilot = TutorInvoke.parseVoiceGuidedFlow("App Copilot show me how to create a note")
        XCTAssertEqual(copilot?.feature, .appCopilot)
        XCTAssertEqual(copilot?.canvasMode, .screenOverlay)
    }

    func testVoiceGrammarDoesNotTakeOverIncidentalMentions() {
        for text in [
            "Can you compare tutor products?",
            "I mentioned app copilot later in this sentence",
            "Please ask hey tutor to explain this"
        ] {
            XCTAssertNil(TutorInvoke.parseVoiceGuidedFlow(text), "should not match: \(text)")
        }
    }

    func testExplicitBlackboardOverridesIncidentalScreenWording() {
        let invocation = TutorInvoke.parseVoiceGuidedFlow(
            "Tutor blackboard explain what a screen reader does"
        )
        XCTAssertEqual(invocation?.canvasMode, .blackboard)
    }

    func testQuestionWordingNeverImplicitlyAuthorizesScreenCapture() {
        for text in [
            "Tutor explain my app",
            "Tutor explain the current window",
            "Tutor explain why the screenshot is blurry"
        ] {
            let invocation = TutorInvoke.parseVoiceGuidedFlow(text)
            XCTAssertEqual(invocation?.canvasMode, .blackboard, text)
            XCTAssertFalse(invocation?.requiresScreenCapture ?? true, text)
        }
    }

    func testCommandPrefixSourceRemainsAuthoritative() {
        XCTAssertEqual(
            TutorInvoke.parseVoiceGuidedFlow(
                "Tutor screen explain the blackboard controls"
            )?.canvasMode,
            .screenOverlay
        )
        XCTAssertEqual(
            TutorInvoke.parseVoiceGuidedFlow(
                "Tutor blackboard explain how screenshots work"
            )?.canvasMode,
            .blackboard
        )
    }

    func testLockedScreenSpeechIsFeatureSpecific() {
        XCTAssertEqual(
            DeviceScreenLock.message(for: .tutor),
            "Please unlock your screen to use Tutor."
        )
        XCTAssertEqual(
            DeviceScreenLock.message(for: .appCopilot),
            "Please unlock your screen to use App Copilot."
        )
    }

    @MainActor
    func testRouterInvalidatesQueuedVoicePresentationAcrossLockUnlock() {
        let center = NotificationCenter()
        var locked = false
        var spoken: [String] = []
        let router = TutorOverlayRouter(
            notificationCenter: center,
            screenIsLocked: { locked },
            speaker: { spoken.append($0) },
            ambientRail: AmbientRail(windowIsLive: { false }, yield: { _ in })
        )

        router.present(question: "explain recursion", image: nil, autoStart: true)
        let admissionID = router.request?.voiceAdmissionID
        XCTAssertNotNil(admissionID)

        locked = true
        center.post(
            name: UIApplication.protectedDataWillBecomeUnavailableNotification,
            object: nil
        )

        XCTAssertNil(router.request)
        XCTAssertEqual(spoken, ["Please unlock your screen to use Tutor."])
        XCTAssertEqual(router.consumeVoiceAdmission(admissionID), .invalidated)
        locked = false
    }

    @MainActor
    func testRouterRechecksLockWhenViewConsumesVoiceAdmission() {
        var locked = false
        var spoken: [String] = []
        let router = TutorOverlayRouter(
            notificationCenter: NotificationCenter(),
            screenIsLocked: { locked },
            speaker: { spoken.append($0) },
            ambientRail: AmbientRail(windowIsLive: { false }, yield: { _ in })
        )

        router.present(question: "explain recursion", image: nil, autoStart: true)
        let admissionID = router.request?.voiceAdmissionID
        locked = true

        XCTAssertEqual(router.consumeVoiceAdmission(admissionID), .locked)
        XCTAssertTrue(spoken.isEmpty, "the view owns speech for a consume-time rejection")
        XCTAssertEqual(router.consumeVoiceAdmission(admissionID), .invalidated)
    }

    @MainActor
    func testRouterRejectsAlreadyLockedVoicePresentationImmediately() {
        var spoken: [String] = []
        let router = TutorOverlayRouter(
            notificationCenter: NotificationCenter(),
            screenIsLocked: { true },
            speaker: { spoken.append($0) },
            ambientRail: AmbientRail(windowIsLive: { false }, yield: { _ in })
        )

        router.present(question: "explain recursion", image: nil, autoStart: true)

        XCTAssertNil(router.request)
        XCTAssertEqual(spoken, ["Please unlock your screen to use Tutor."])
    }

    @MainActor
    func testAdmittedVoiceTutorYieldsAmbientWindowBeforePresentation() {
        var yielded: [String] = []
        let router = TutorOverlayRouter(
            notificationCenter: NotificationCenter(),
            screenIsLocked: { false },
            speaker: { _ in },
            ambientRail: AmbientRail(
                windowIsLive: { true },
                yield: { yielded.append($0) }
            )
        )

        router.present(question: "explain recursion", image: nil, autoStart: true)

        XCTAssertEqual(yielded, [AmbientYieldReason.tutorStarted])
        XCTAssertEqual(router.request?.question, "explain recursion")
    }

    @MainActor
    func testLockedVoiceTutorDoesNotEndAmbientWindow() {
        var yielded: [String] = []
        let router = TutorOverlayRouter(
            notificationCenter: NotificationCenter(),
            screenIsLocked: { true },
            speaker: { _ in },
            ambientRail: AmbientRail(
                windowIsLive: { true },
                yield: { yielded.append($0) }
            )
        )

        router.present(question: "explain recursion", image: nil, autoStart: true)

        XCTAssertTrue(yielded.isEmpty)
        XCTAssertNil(router.request)
    }
}
